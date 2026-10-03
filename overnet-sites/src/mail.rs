//! `mail.ov` — почта со сквозным шифрованием в браузере.
//!
//! Ключ (ECDH P-256) создаётся и хранится в браузере; адрес = хеш публичного
//! ключа. Письмо шифруется в браузере отправителя ключом получателя (ECDH →
//! HKDF → AES-GCM), сервер хранит только шифротекст. Доступ к ящику — по токену,
//! выведенному из приватного ключа: сервер знает лишь его хеш.
//!
//! Честно о границах: сервер видит, в какой ящик и когда пришло письмо и его
//! размер. Поле «от кого» внутри письма никто не подписывает — отправитель
//! может написать что угодно. Браузер Mullvad не хранит данные между запусками,
//! поэтому ключ нужно сохранить файлом.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{load_json, random_id, save_json, Site};

const MAX_BLOB: usize = 256 * 1024;
const MAX_INBOX: usize = 500;
const KEEP_SECS: u64 = 30 * 86400;

#[derive(Serialize, Deserialize, Clone)]
struct User {
    /// base64 сырого публичного ключа P-256 (65 байт).
    public: String,
    token_hash: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Letter {
    id: String,
    ts: u64,
    blob: serde_json::Value,
}

#[derive(Serialize, Deserialize, Default)]
struct Db {
    users: HashMap<String, User>,
    inbox: HashMap<String, Vec<Letter>>,
}

pub struct Mail {
    path: PathBuf,
    db: Mutex<Db>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn sha_hex(s: &[u8]) -> String {
    hex::encode(Sha256::digest(s))
}

/// Адрес ящика из публичного ключа: base32(sha256(pub)[..10]), 16 символов.
pub fn address_of(public_raw: &[u8]) -> String {
    data_encoding::BASE32_NOPAD.encode(&Sha256::digest(public_raw)[..10]).to_ascii_lowercase()
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    data_encoding::BASE64.decode(s.as_bytes()).ok()
}

impl Mail {
    pub fn open(dir: &std::path::Path) -> Arc<Mail> {
        let path = dir.join("mail.json");
        Arc::new(Mail { db: Mutex::new(load_json(&path)), path })
    }

    async fn save(&self, db: &Db) -> Result<(), Response> {
        save_json(&self.path, db).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
    }

    fn authorized(db: &Db, addr: &str, token: &str) -> bool {
        db.users.get(addr).is_some_and(|u| u.token_hash == sha_hex(token.as_bytes()))
    }
}

#[derive(Deserialize)]
struct RegisterReq {
    addr: String,
    public: String,
    token: String,
}

#[derive(Deserialize)]
struct SendReq {
    to: String,
    blob: serde_json::Value,
}

#[derive(Deserialize)]
struct AuthReq {
    addr: String,
    token: String,
    #[serde(default)]
    id: String,
}

pub fn router(m: Arc<Mail>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/register", post(register))
        .route("/api/key/{addr}", get(key))
        .route("/api/send", post(send))
        .route("/api/inbox", post(inbox))
        .route("/api/delete", post(delete))
        .merge(crate::assets())
        .layer(DefaultBodyLimit::max(MAX_BLOB * 2))
        .with_state(m)
}

fn bad(code: StatusCode, why: &str) -> Response {
    (code, why.to_string()).into_response()
}

async fn register(State(m): State<Arc<Mail>>, Json(r): Json<RegisterReq>) -> Response {
    let Some(raw) = b64_decode(&r.public).filter(|k| k.len() == 65 && k[0] == 4) else {
        return bad(StatusCode::BAD_REQUEST, "bad key");
    };
    if address_of(&raw) != r.addr || r.token.len() < 32 {
        return bad(StatusCode::BAD_REQUEST, "address does not match the key");
    }
    let mut db = m.db.lock().await;
    let th = sha_hex(r.token.as_bytes());
    match db.users.get(&r.addr) {
        Some(u) if u.token_hash != th => return bad(StatusCode::CONFLICT, "address taken"),
        Some(_) => return StatusCode::OK.into_response(),
        None => {}
    }
    db.users.insert(r.addr, User { public: r.public, token_hash: th });
    match m.save(&db).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => e,
    }
}

async fn key(State(m): State<Arc<Mail>>, Path(addr): Path<String>) -> Response {
    let addr = addr.trim_end_matches("@mail.ov").to_ascii_lowercase();
    match m.db.lock().await.users.get(&addr) {
        Some(u) => u.public.clone().into_response(),
        None => bad(StatusCode::NOT_FOUND, "no such mailbox"),
    }
}

async fn send(State(m): State<Arc<Mail>>, Json(r): Json<SendReq>) -> Response {
    if r.blob.to_string().len() > MAX_BLOB {
        return bad(StatusCode::PAYLOAD_TOO_LARGE, "letter too large");
    }
    let to = r.to.trim_end_matches("@mail.ov").to_ascii_lowercase();
    let mut db = m.db.lock().await;
    if !db.users.contains_key(&to) {
        return bad(StatusCode::NOT_FOUND, "no such mailbox");
    }
    let t = now();
    let list = db.inbox.entry(to).or_default();
    list.retain(|l| t.saturating_sub(l.ts) < KEEP_SECS);
    if list.len() >= MAX_INBOX {
        return bad(StatusCode::INSUFFICIENT_STORAGE, "mailbox is full");
    }
    list.push(Letter { id: random_id(), ts: t, blob: r.blob });
    match m.save(&db).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => e,
    }
}

async fn inbox(State(m): State<Arc<Mail>>, Json(r): Json<AuthReq>) -> Response {
    let mut db = m.db.lock().await;
    if !Mail::authorized(&db, &r.addr, &r.token) {
        return bad(StatusCode::FORBIDDEN, "forbidden");
    }
    // Срок хранения соблюдается и для ящиков, куда давно ничего не приходило.
    let t = now();
    let list = db.inbox.get_mut(&r.addr).map(std::mem::take).unwrap_or_default();
    let (keep, old): (Vec<Letter>, Vec<Letter>) = list.into_iter().partition(|l| t.saturating_sub(l.ts) < KEEP_SECS);
    db.inbox.insert(r.addr.clone(), keep.clone());
    if !old.is_empty() {
        if let Err(e) = m.save(&db).await {
            return e;
        }
    }
    Json(keep).into_response()
}

async fn delete(State(m): State<Arc<Mail>>, Json(r): Json<AuthReq>) -> Response {
    let mut db = m.db.lock().await;
    if !Mail::authorized(&db, &r.addr, &r.token) {
        return bad(StatusCode::FORBIDDEN, "forbidden");
    }
    if let Some(list) = db.inbox.get_mut(&r.addr) {
        list.retain(|l| l.id != r.id);
    }
    match m.save(&db).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => e,
    }
}

async fn index() -> Html<String> {
    let ui = MAIL_UI
        .replace("%KEEP_DAYS%", &(KEEP_SECS / 86400).to_string())
        .replace("%MAX_INBOX%", &MAX_INBOX.to_string())
        .replace("%MAX_KB%", &(MAX_BLOB / 1024).to_string());
    Site {
        host: "mail.ov",
        name: "Mail",
        pages: &[("#welcome", "Open mailbox"), ("#inbox", "Mailbox"), ("#settings", "Settings")],
        note: "mail.ov doesn’t know who you are and keeps no logs. It stores locked letters it can’t read. Mail needs JavaScript: locking happens in your browser.",
        help: ("#how", "How mail works"),
    }
    .page("mail.ov — encrypted mail", "#welcome", &ui)
}

const MAIL_UI: &str = r##"<noscript><div class="col intro"><div role="alert" class="note err"><strong>Mail needs JavaScript.</strong><span>Letters are locked and unlocked in your browser, so this page can’t work without scripts. Turn them on for mail.ov.</span></div></div></noscript>

<section id="v-welcome" hidden class="col wide" style="gap:28px">
<div class="col intro" style="gap:14px"><h1>Mail that only the recipient can read</h1>
<p class="lede">Letters are locked in your browser before they leave it, and unlocked only in the recipient’s. mail.ov stores them, but can’t read them. No phone number, no email, no password: your mailbox is a key file that you keep.</p></div>
<div id="nocrypto" role="alert" class="note err" hidden><strong>This browser can’t encrypt here.</strong><span>To it, http://mail.ov is not a secure context, so the page gets no cryptography. Open mail in the overnet browser: run <span class="mono">overnet browser</span>.</span></div>
<div id="isopen" role="status" class="note plain row" hidden><p>Your mailbox <span class="mono" data-me></span> is open.</p><a class="btn" href="#inbox">Go to inbox</a></div>
<div class="panels" id="starts">
<section aria-labelledby="new-h" class="panel"><h2 id="new-h" style="font-size:19px">New mailbox</h2>
<p class="text-2">Creates a key in this browser and gives you an address. You’ll be asked to save the key as a file right away.</p>
<div><button type="button" class="btn" id="create">Create mailbox</button></div></section>
<section aria-labelledby="open-h" class="panel"><h2 id="open-h" style="font-size:19px">I have a key file</h2>
<p class="text-2">Its name ends in <span class="mono" style="font-size:14px">.mail.ov.key</span>. The file is read here, in the browser, and never uploaded.</p>
<div><label class="btn plain" style="position:relative">Open key file<input type="file" id="keyfile" accept=".key,application/json" class="sr"></label></div>
<div id="keyerr" role="alert" class="note err" hidden style="font-size:15px">This file isn’t a mail.ov key. Look for a file ending in <span class="mono">.mail.ov.key</span>, saved when you created the mailbox.</div></section>
</div>
<div class="qa" id="how">
<details><summary>Why do I have to save a file?</summary><div class="a"><p>The overnet browser forgets everything when you close it, on purpose. The key is the only thing that can open your mailbox, and it never leaves your computer, so nobody can reset it for you. Keep the file on a USB stick or another device you trust.</p></div></details>
<details><summary>What can mail.ov see?</summary><div class="a"><p>It sees that a letter arrived for a mailbox, when, and how large it is. It can’t see the text or the subject, and it doesn’t know who you are. The sender’s address is inside the locked letter, so the server doesn’t see it either.</p></div></details>
<details><summary>How it works</summary><div class="a"><p>Your key is a P-256 key pair made by the browser’s built-in cryptography. Your address is the first 80 bits of the SHA-256 hash of the public key, in base32. Each letter gets a fresh key pair; ECDH with the recipient’s public key, then HKDF, gives an AES-256-GCM key. The server stores only the result. Mail is the one overnet service that needs JavaScript, because the locking has to happen on your side.</p></div></details>
</div></section>

<section id="v-created" hidden class="col intro" style="max-width:640px">
<h1 style="font-size:clamp(24px,5vw,30px)">Your mailbox is ready</h1>
<div class="stack" style="gap:4px"><span class="small muted">Your address</span><span class="mono" style="font-size:clamp(16px,4.4vw,20px);word-break:break-all" data-me></span></div>
<div class="note warn" style="gap:12px;padding:18px 20px"><h2 style="font-size:18px;font-weight:600">Save your key file now</h2>
<p>When you close this browser, the key is gone from it. Without the file, nobody can open this mailbox again, including you.</p>
<p style="display:flex;flex-wrap:wrap;gap:10px;align-items:center"><button type="button" class="btn warn" data-save>Save key file</button><span class="mono" style="font-size:13px;word-break:break-all" data-keyfile></span></p></div>
<div role="status" class="note ok" data-saved hidden>Saved. Check that the file is in your downloads, then move it somewhere safe.</div>
<p><a class="btn" href="#inbox">Go to inbox</a></p></section>

<div id="v-box" hidden class="mailbox">
<aside aria-label="Mailbox"><a class="btn" href="#compose">Write a letter</a>
<nav class="folders" aria-label="Folders"><a href="#inbox" id="f-inbox"><span>Inbox</span><span class="small" id="unread"></span></a></nav>
<div class="me"><span class="small muted">Your address</span><span class="mono" style="font-size:13px;word-break:break-all" data-me></span>
<button type="button" class="btn small" style="align-self:flex-start;margin-top:6px" data-copyme>Copy address</button></div></aside>
<div class="pane">
<div role="status" class="note warn row" id="unsaved" hidden><span>Your key isn’t saved yet. If you close the browser without it, this mailbox is lost.</span><button type="button" class="btn warn" data-save>Save key file</button></div>
<div role="status" class="note ok row" id="notice" hidden><span id="notice-t"></span><button type="button" class="link-btn" style="color:inherit;text-decoration:none;font-size:18px" aria-label="Dismiss" id="notice-x">×</button></div>

<div id="v-inbox" hidden>
<div class="list-head"><h1 class="s">Inbox</h1><span class="small muted" id="inbox-sub"></span><button type="button" class="btn small" id="refresh">Check for new</button></div>
<ul class="letters" id="letters"></ul>
<div id="empty" hidden style="padding:40px 0;max-width:440px" class="stack"><p style="font-weight:500;font-size:17px">No letters yet</p><p class="text-2">Share your address so people can write to you. New letters appear when you press Check for new.</p></div>
<p class="small muted" style="margin-top:14px;font-size:13px">Letters are deleted from the server after %KEEP_DAYS% days. A mailbox holds up to %MAX_INBOX%.</p></div>

<article id="v-read" hidden class="col" style="max-width:720px">
<div style="display:flex;flex-wrap:wrap;gap:8px"><a href="#inbox" style="display:inline-flex;align-items:center;min-height:40px;padding-right:12px;text-decoration:none">← Inbox</a><span style="flex:1"></span>
<a class="btn" href="#compose" id="reply" style="min-height:40px;padding:0 14px">Reply</a><button type="button" class="btn plain" id="del" style="min-height:40px;padding:0 14px">Delete</button></div>
<h1 id="r-subj" style="font-size:clamp(22px,4.5vw,28px);line-height:1.2;text-wrap:pretty"></h1>
<dl class="facts"><dt>From</dt><dd style="display:flex;flex-wrap:wrap;gap:2px 10px;align-items:baseline"><span class="mono" style="font-size:14px" id="r-from"></span><span class="small warn-text">not verified</span></dd>
<dt>To</dt><dd class="mono" style="font-size:14px" data-me></dd><dt>Date</dt><dd id="r-date"></dd></dl>
<div class="fade-rule"></div><div class="letter-body" id="r-body"></div>
<details class="more" style="border-top:1px solid var(--line-soft);padding-top:8px"><summary>About this letter’s protection</summary><p>It was unlocked here with your key; the server only ever had scrambled text. The “From” line is written by the sender and nobody checks it. If a letter claims to be from someone you know, confirm in another way before acting on it.</p></details></article>

<form id="v-compose" hidden novalidate class="col" style="max-width:720px;gap:14px"><h1 class="s">New letter</h1>
<div class="stack"><label for="to" class="lbl">To</label><input id="to" class="field mono" autocapitalize="none" spellcheck="false" placeholder="16 letters and digits, then @mail.ov" aria-describedby="to-err">
<p id="to-err" role="alert" class="small err-text" hidden></p></div>
<div class="stack"><label for="subj" class="lbl">Subject</label><input id="subj" class="field"></div>
<div class="stack"><label for="text" class="lbl">Letter</label><textarea id="text" class="field" style="min-height:240px;line-height:1.6"></textarea></div>
<div style="display:flex;flex-wrap:wrap;align-items:center;gap:10px 16px"><button class="btn" id="send" style="padding:0 24px">Send</button><a href="#inbox" id="discard" class="text-2">Discard</a>
<span class="small muted">Locked for the recipient before it leaves this page. Up to %MAX_KB% KB.</span></div>
<div id="send-err" role="alert" class="note err" hidden></div></form>

<div id="v-settings" hidden class="col" style="max-width:680px;gap:28px"><h1 class="s">Settings</h1>
<section class="stack" style="gap:8px"><h2 class="s">Address</h2><p class="text-2">Give this to people who want to write to you. It’s safe to share.</p>
<div class="copybox"><code data-me></code><button type="button" class="btn small" data-copyme>Copy address</button></div></section>
<section class="stack" style="gap:8px"><h2 class="s">Key file</h2><p class="text-2">The key opens your mailbox. Never send it to anyone, including us; we will never ask for it.</p>
<p style="font-size:15px" id="keystatus"></p><div><button type="button" class="btn" data-save>Save key file</button></div></section>
<section class="stack" style="gap:8px"><h2 class="s">Close mailbox on this computer</h2><p class="text-2">Removes the key from this browser. Letters stay on the server, and you can open them again with your key file.</p>
<div><button type="button" class="btn danger" id="close">Close mailbox</button></div></section>
<section class="stack" style="gap:8px"><h2 class="s">What the server knows</h2><ul class="plain" style="gap:4px"><li>That your mailbox exists, and its public key.</li><li>When each letter arrived and how large it is.</li><li>Not the text, not the subject, not the sender, and not who you are.</li></ul>
<p class="small muted">Letters are kept for %KEEP_DAYS% days, up to %MAX_INBOX% per mailbox.</p></section></div>
</div></div>

<script>
(function(){
var $=function(i){return document.getElementById(i)},all=function(s){return document.querySelectorAll(s)};
var S=window.crypto&&crypto.subtle,E=new TextEncoder(),D=new TextDecoder(),P={name:'ECDH',namedCurve:'P-256'};
var b64=function(b){return btoa(String.fromCharCode.apply(null,new Uint8Array(b)))},ub64=function(s){return Uint8Array.from(atob(s),function(c){return c.charCodeAt(0)})};
var A='abcdefghijklmnopqrstuvwxyz234567';
function b32(b){var o='',v=0,n=0;for(var i=0;i<b.length;i++){v=(v<<8)|b[i];n+=8;while(n>=5){o+=A[(v>>>(n-5))&31];n-=5}}if(n>0)o+=A[(v<<(5-n))&31];return o}
function store(k,v){try{v===null?sessionStorage.removeItem(k):sessionStorage.setItem(k,v)}catch(e){}}
function load(k){try{return sessionStorage.getItem(k)}catch(e){return null}}
var K=null,saved=load('saved')==='1',letters=[],seen=new Set(JSON.parse(load('seen')||'[]')),cur=null;
var post=function(u,o){return fetch(u,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(o)})};

async function useJwk(jwk){
  var priv=await S.importKey('jwk',jwk,P,true,['deriveBits']);
  var pub=await S.importKey('jwk',{kty:jwk.kty,crv:jwk.crv,x:jwk.x,y:jwk.y},P,true,[]);var pubRaw=await S.exportKey('raw',pub);
  var addr=b32(new Uint8Array(await S.digest('SHA-256',pubRaw)).slice(0,10));
  var token=Array.from(new Uint8Array(await S.digest('SHA-256',E.encode('mail.ov token:'+jwk.d)))).map(function(x){return x.toString(16).padStart(2,'0')}).join('');
  var r=await post('/api/register',{addr:addr,public:b64(pubRaw),token:token});
  if(!r.ok)throw new Error('The server did not accept the key: '+await r.text());
  K={jwk:jwk,priv:priv,addr:addr,token:token};store('k',JSON.stringify(jwk));
  all('[data-me]').forEach(function(e){e.textContent=addr+'@mail.ov'});
  all('[data-keyfile]').forEach(function(e){e.textContent=addr+'.mail.ov.key'});
}
function me(){return K.addr+'@mail.ov'}
function saveKey(){var a=document.createElement('a');a.href=URL.createObjectURL(new Blob([JSON.stringify(K.jwk)],{type:'application/json'}));a.download=K.addr+'.mail.ov.key';a.click();
  saved=true;store('saved','1');paint()}
async function aes(shared,salt,usage){var base=await S.importKey('raw',shared,'HKDF',false,['deriveKey']);
  return S.deriveKey({name:'HKDF',hash:'SHA-256',salt:salt,info:E.encode('mail.ov v1')},base,{name:'AES-GCM',length:256},false,[usage])}
async function open1(l){var epk=ub64(l.blob.epk);var pub=await S.importKey('raw',epk,P,false,[]);
  var key=await aes(await S.deriveBits({name:'ECDH',public:pub},K.priv,256),epk,'decrypt');
  return JSON.parse(D.decode(await S.decrypt({name:'AES-GCM',iv:ub64(l.blob.iv)},key,ub64(l.blob.ct))))}

function notice(t){$('notice-t').textContent=t;$('notice').hidden=!t}
function when(ts,full){var d=new Date(ts*1000),now=new Date();
  if(full)return d.toLocaleDateString('en-GB',{day:'numeric',month:'long',year:'numeric'})+', '+d.toLocaleTimeString('en-GB',{hour:'2-digit',minute:'2-digit'});
  if(d.toDateString()===now.toDateString())return d.toLocaleTimeString('en-GB',{hour:'2-digit',minute:'2-digit'});
  return d.toLocaleDateString('en-GB',{day:'numeric',month:'short'})}

async function loadInbox(){
  $('refresh').textContent='Checking…';
  try{var r=await post('/api/inbox',{addr:K.addr,token:K.token});var list=r.ok?await r.json():[];
    var out=[];for(var i=list.length-1;i>=0;i--){var l=list[i],m;try{m=await open1(l)}catch(e){m={from:'?',subject:'(this letter can’t be unlocked)',body:''}}out.push({id:l.id,ts:l.ts,m:m})}
    letters=out}finally{$('refresh').textContent='Check for new'}
  paint()}

function paint(){
  var open=!!K,unread=letters.filter(function(l){return !seen.has(l.id)}).length;
  $('unread').textContent=unread||'';
  $('inbox-sub').textContent=letters.length?(unread?unread+' unread':'All read'):'';
  $('unsaved').hidden=!open||saved;
  all('[data-saved]').forEach(function(e){e.hidden=!saved});
  if(open)$('keystatus').textContent=saved?'Saved this session as '+K.addr+'.mail.ov.key.':'Not saved yet this session.';
  var ul=$('letters');ul.textContent='';
  letters.forEach(function(l){var li=document.createElement('li');if(!seen.has(l.id))li.className='new';
    var a=document.createElement('a');a.href='#read-'+l.id;
    var who=document.createElement('span');who.className='who';var w1=document.createElement('span');w1.textContent=l.m.from||'?';var w2=document.createElement('span');w2.textContent=when(l.ts);who.append(w1,w2);
    if(!seen.has(l.id)){var u=document.createElement('span');u.className='sr';u.textContent='Unread.';a.append(u)}
    var s=document.createElement('span');s.className='subj';s.textContent=l.m.subject||'(no subject)';
    var p=document.createElement('span');p.className='snip';p.textContent=String(l.m.body||'').replace(/\s+/g,' ');
    a.append(who,s,p);li.append(a);ul.append(li)});
  $('empty').hidden=letters.length>0}

function route(){
  var r=location.hash.slice(1)||'welcome',open=!!K;
  if(r==='how'){r='welcome';setTimeout(function(){var d=$('how').querySelector('details:last-child');d.open=true;d.scrollIntoView()},0)}
  if(!open&&r!=='welcome')r='welcome';
  var read=r.indexOf('read-')===0,box=open&&r!=='welcome'&&r!=='created';
  $('v-welcome').hidden=r!=='welcome';$('v-created').hidden=r!=='created';$('v-box').hidden=!box;
  $('isopen').hidden=!open;$('starts').hidden=open;
  ['inbox','compose','settings'].forEach(function(v){$('v-'+v).hidden=r!==v});$('v-read').hidden=!read;
  var navOn=r==='settings'?'#settings':box?'#inbox':'#welcome';
  all('.pages a').forEach(function(a){var h=a.getAttribute('href');a.hidden=open?h==='#welcome':h!=='#welcome';
    if(h===navOn)a.setAttribute('aria-current','page');else a.removeAttribute('aria-current')});
  if(r==='inbox'||read)$('f-inbox').setAttribute('aria-current','page');else $('f-inbox').removeAttribute('aria-current');
  if(read){cur=letters.find(function(l){return 'read-'+l.id===r});if(!cur){location.hash='inbox';return}
    seen.add(cur.id);store('seen',JSON.stringify(Array.from(seen)));
    $('r-subj').textContent=cur.m.subject||'(no subject)';$('r-from').textContent=cur.m.from||'?';$('r-date').textContent=when(cur.ts,true);$('r-body').textContent=cur.m.body||'';paint()}
  if(r==='compose')$('to').focus();
  window.scrollTo(0,0)}

if(!S){$('nocrypto').hidden=false;$('starts').hidden=true;all('.pages a').forEach(function(a){a.hidden=a.getAttribute('href')!=='#welcome'});$('v-welcome').hidden=false;return}

$('create').onclick=async function(){try{var kp=await S.generateKey(P,true,['deriveBits']);await useJwk(await S.exportKey('jwk',kp.privateKey));saved=false;store('saved',null);saveKey();letters=[];paint();location.hash='created'}catch(e){alert(e.message||e)}};
$('keyfile').onchange=async function(){var f=this.files[0];if(!f)return;$('keyerr').hidden=true;
  try{await useJwk(JSON.parse(await f.text()))}catch(e){$('keyerr').hidden=false;this.value='';return}
  saved=true;store('saved','1');location.hash='inbox';route();loadInbox()};
all('[data-save]').forEach(function(b){b.onclick=saveKey});
all('[data-copyme]').forEach(function(b){b.onclick=function(){ovCopy(me(),b)}});
$('notice-x').onclick=function(){notice('')};
$('refresh').onclick=loadInbox;
$('reply').onclick=function(){$('to').value=cur.m.from||'';var s=cur.m.subject||'';$('subj').value=/^re:/i.test(s)?s:'Re: '+s;$('text').value='';$('to-err').hidden=true};
$('del').onclick=async function(){await post('/api/delete',{addr:K.addr,token:K.token,id:cur.id});letters=letters.filter(function(l){return l!==cur});notice('Letter deleted from the server.');location.hash='inbox';paint()};
$('discard').onclick=function(){$('to').value=$('subj').value=$('text').value='';$('to-err').hidden=true;$('send-err').hidden=true};
$('close').onclick=function(){store('k',null);store('saved',null);store('seen',null);location.hash='';location.reload()};
$('v-compose').onsubmit=async function(e){e.preventDefault();var err=$('to-err'),serr=$('send-err'),btn=$('send');serr.hidden=true;
  var to=$('to').value.trim().toLowerCase().replace(/@mail\.ov$/,'');
  var bad=!to?'Enter the recipient’s address.':!/^[a-z2-7]{16}$/.test(to)?'This doesn’t look like a mail.ov address. Addresses are 16 letters and digits, like '+me()+'.':'';
  err.textContent=bad;err.hidden=!bad;$('to').setAttribute('aria-invalid',bad?'true':'false');if(bad)return;
  btn.disabled=true;btn.textContent='Locking and sending…';
  try{var kr=await fetch('/api/key/'+encodeURIComponent(to));
    if(kr.status===404){err.textContent='There’s no mailbox with this address. Check it with the person you’re writing to.';err.hidden=false;$('to').setAttribute('aria-invalid','true');return}
    if(!kr.ok)throw new Error(await kr.text());
    var their=await S.importKey('raw',ub64(await kr.text()),P,false,[]);
    var eph=await S.generateKey(P,true,['deriveBits']);var epk=await S.exportKey('raw',eph.publicKey);
    var key=await aes(await S.deriveBits({name:'ECDH',public:their},eph.privateKey,256),epk,'encrypt');
    var iv=crypto.getRandomValues(new Uint8Array(12));
    var msg=JSON.stringify({from:me(),subject:$('subj').value,body:$('text').value,ts:Date.now()});
    var ct=await S.encrypt({name:'AES-GCM',iv:iv},key,E.encode(msg));
    var r=await post('/api/send',{to:to,blob:{epk:b64(epk),iv:b64(iv),ct:b64(ct)}});
    if(r.status===413)throw new Error('This letter is too long. The limit is %MAX_KB% KB.');
    if(r.status===507)throw new Error('The recipient’s mailbox is full. They need to delete some letters first.');
    if(!r.ok)throw new Error(await r.text());
    $('to').value=$('subj').value=$('text').value='';notice('Sent to '+to+'@mail.ov. Mail doesn’t keep a copy of letters you send.');location.hash='inbox'}
  catch(x){serr.textContent=x.message||String(x);serr.hidden=false}
  finally{btn.disabled=false;btn.textContent='Send'}};

window.addEventListener('hashchange',route);
var j=load('k');
if(j){useJwk(JSON.parse(j)).then(function(){paint();route();loadInbox()},function(){store('k',null);route()})}else route();
})();
</script>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_is_16_base32_chars() {
        let a = address_of(&[4u8; 65]);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)));
    }
}
