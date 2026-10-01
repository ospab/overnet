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

use crate::{load_json, page, random_id, save_json};

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
    let db = m.db.lock().await;
    if !Mail::authorized(&db, &r.addr, &r.token) {
        return bad(StatusCode::FORBIDDEN, "forbidden");
    }
    Json(db.inbox.get(&r.addr).cloned().unwrap_or_default()).into_response()
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
    page("mail.ov — почта", "mail.ov", MAIL_UI)
}

const MAIL_UI: &str = r#"<h1>Почта</h1><p class="sub">Письма шифруются в вашем браузере ключом получателя; сервер хранит только шифротекст.</p>
<div id="nokey" class="card"><p>Ключа ещё нет. Браузер overnet не хранит данные между запусками — после создания <b>сохраните ключ файлом</b>.</p>
<p class="row"><button onclick="createKey()">Создать ящик</button><button class="ghost" onclick="document.getElementById('kf').click()">Загрузить ключ из файла</button>
<input type="file" id="kf" style="display:none" onchange="loadKey(this.files[0])"></p></div>
<div id="box" style="display:none">
<div class="card">Ваш адрес: <code id="me"></code> <button class="ghost" onclick="navigator.clipboard.writeText(me.textContent)">Копировать</button> <button class="ghost" onclick="saveKey()">Сохранить ключ</button></div>
<h2>Написать</h2><div class="card"><input id="to" placeholder="адрес@mail.ov"><p><input id="subj" placeholder="Тема"></p>
<textarea id="text" placeholder="Текст письма" style="font-family:inherit;font-size:15px"></textarea><p><button onclick="sendMail()">Отправить</button> <span id="sst"></span></p></div>
<h2>Входящие <button class="ghost" onclick="loadInbox()">Обновить</button></h2><div id="inbox"></div></div>
<script>
const S=crypto.subtle,E=new TextEncoder(),D=new TextDecoder(),P={name:'ECDH',namedCurve:'P-256'};
const b64=b=>btoa(String.fromCharCode(...new Uint8Array(b))),ub64=s=>Uint8Array.from(atob(s),c=>c.charCodeAt(0));
const A='abcdefghijklmnopqrstuvwxyz234567';
function b32(b){let o='',v=0,n=0;for(const x of b){v=(v<<8)|x;n+=8;while(n>=5){o+=A[(v>>>(n-5))&31];n-=5}}if(n>0)o+=A[(v<<(5-n))&31];return o}
let K=null; // {jwk, priv, pubRaw, addr, token}
async function useJwk(jwk){const priv=await S.importKey('jwk',jwk,P,true,['deriveBits']);
const pub=await S.importKey('jwk',{kty:jwk.kty,crv:jwk.crv,x:jwk.x,y:jwk.y},P,true,[]);const pubRaw=await S.exportKey('raw',pub);
const addr=b32(new Uint8Array(await S.digest('SHA-256',pubRaw)).slice(0,10));
const token=[...new Uint8Array(await S.digest('SHA-256',E.encode('mail.ov token:'+jwk.d)))].map(x=>x.toString(16).padStart(2,'0')).join('');
const r=await fetch('/api/register',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({addr,public:b64(pubRaw),token})});
if(!r.ok){alert('Сервер не принял ключ: '+await r.text());return}
K={jwk,priv,addr,token};try{sessionStorage.setItem('k',JSON.stringify(jwk))}catch(e){}
nokey.style.display='none';box.style.display='block';me.textContent=addr+'@mail.ov';loadInbox()}
async function createKey(){const kp=await S.generateKey(P,true,['deriveBits']);await useJwk(await S.exportKey('jwk',kp.privateKey));saveKey()}
function saveKey(){const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([JSON.stringify(K.jwk)],{type:'application/json'}));a.download=K.addr+'.mail.ov.key';a.click()}
async function loadKey(f){try{await useJwk(JSON.parse(await f.text()))}catch(e){alert('Это не ключ mail.ov')}}
async function aes(shared,salt,usage){const base=await S.importKey('raw',shared,'HKDF',false,['deriveKey']);
return S.deriveKey({name:'HKDF',hash:'SHA-256',salt,info:E.encode('mail.ov v1')},base,{name:'AES-GCM',length:256},false,[usage])}
async function sendMail(){const st=sst;st.className='muted';st.textContent='…';try{
const to=document.getElementById('to').value.trim().toLowerCase().replace(/@mail\.ov$/,'');
const kr=await fetch('/api/key/'+encodeURIComponent(to));if(!kr.ok)throw 'нет такого ящика';
const their=await S.importKey('raw',ub64(await kr.text()),P,false,[]);
const eph=await S.generateKey(P,true,['deriveBits']);const epk=await S.exportKey('raw',eph.publicKey);
const key=await aes(await S.deriveBits({name:'ECDH',public:their},eph.privateKey,256),epk,'encrypt');
const iv=crypto.getRandomValues(new Uint8Array(12));
const msg=JSON.stringify({from:K.addr+'@mail.ov',subject:subj.value,body:text.value,ts:Date.now()});
const ct=await S.encrypt({name:'AES-GCM',iv},key,E.encode(msg));
const r=await fetch('/api/send',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({to,blob:{epk:b64(epk),iv:b64(iv),ct:b64(ct)}})});
if(!r.ok)throw await r.text();st.className='ok';st.textContent='отправлено';text.value='';subj.value=''}catch(e){st.className='err';st.textContent=e}}
function esc(s){return String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}
async function open1(l){const epk=ub64(l.blob.epk);const pub=await S.importKey('raw',epk,P,false,[]);
const key=await aes(await S.deriveBits({name:'ECDH',public:pub},K.priv,256),epk,'decrypt');
return JSON.parse(D.decode(await S.decrypt({name:'AES-GCM',iv:ub64(l.blob.iv)},key,ub64(l.blob.ct))))}
async function loadInbox(){const r=await fetch('/api/inbox',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({addr:K.addr,token:K.token})});
const list=r.ok?await r.json():[];let h='';for(const l of list.reverse()){let m;try{m=await open1(l)}catch(e){m={from:'?',subject:'(не расшифровывается)',body:''}}
h+=`<div class="card"><div class="muted">${new Date(l.ts*1000).toLocaleString()} · от <span class="mono">${esc(m.from)}</span> <span class="muted">(не проверено)</span></div><b>${esc(m.subject||'(без темы)')}</b><p style="white-space:pre-wrap">${esc(m.body)}</p><button class="ghost" onclick="del('${l.id}')">Удалить</button></div>`}
inbox.innerHTML=h||'<p class="muted">Писем нет.</p>'}
async function del(id){await fetch('/api/delete',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({addr:K.addr,token:K.token,id})});loadInbox()}
if(!window.crypto||!crypto.subtle){nokey.innerHTML='<p class="err">Этот браузер не даёт странице шифрование: http://mail.ov для него не «безопасный контекст». Откройте почту в браузере overnet (<code>overnet browser</code>) — там это настроено.</p>'}
else{try{const j=sessionStorage.getItem('k');if(j)useJwk(JSON.parse(j))}catch(e){}}
</script>"#;

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
