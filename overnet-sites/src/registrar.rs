//! `name.ov` — регистратор имён.
//!
//! Имя принадлежит первому, кто подписал его ключом своего сервиса
//! (`overnet name-sign`). Регистратор хранит запись с подписью владельца, и
//! клиенты проверяют её сами: подменить адрес он не может, может только
//! отказать. Переназначить имя может только владелец того же ключа.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use tokio::sync::RwLock;

use overnet_node::net::names::{valid_name, NameRecord, RESERVED};

use crate::{esc, load_json, save_json, Site};

pub struct Registrar {
    path: PathBuf,
    names: RwLock<BTreeMap<String, NameRecord>>,
}

impl Registrar {
    pub fn open(data_dir: &std::path::Path) -> Arc<Registrar> {
        let path = data_dir.join("names.json");
        let names = load_json(&path);
        Arc::new(Registrar { path, names: RwLock::new(names) })
    }

    pub async fn list(&self) -> Vec<NameRecord> {
        self.names.read().await.values().cloned().collect()
    }

    /// Зарегистрировать или обновить (тем же ключом) запись.
    pub async fn register(&self, rec: NameRecord) -> Result<(), (StatusCode, String)> {
        let name = rec.name.to_ascii_lowercase();
        if rec.name != name || !valid_name(&name) {
            return Err((StatusCode::BAD_REQUEST, "name: lowercase latin letters, digits and hyphens, like shop.ov".into()));
        }
        if RESERVED.contains(&name.as_str()) {
            return Err((StatusCode::FORBIDDEN, "this name is reserved for a network service".into()));
        }
        let Some(id) = rec.verify() else {
            return Err((StatusCode::BAD_REQUEST, "the signature does not match the address".into()));
        };
        let mut names = self.names.write().await;
        if let Some(cur) = names.get(&name) {
            if cur.verify() != Some(id) {
                return Err((StatusCode::CONFLICT, "the name is already taken by another owner".into()));
            }
        }
        names.insert(name, rec);
        save_json(&self.path, &*names)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    }
}

#[derive(Deserialize)]
struct NameQ {
    name: Option<String>,
}

pub fn router(reg: Arc<Registrar>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/claim", get(claim).post(claim_post))
        .route("/directory", get(directory))
        .route("/help", get(help))
        .route("/api/resolve", get(resolve))
        .route("/api/list", get(list))
        .route("/api/register", post(register))
        .merge(crate::assets())
        .with_state(reg)
}

async fn resolve(State(reg): State<Arc<Registrar>>, Query(q): Query<NameQ>) -> Response {
    let name = q.name.unwrap_or_default().to_ascii_lowercase();
    match reg.names.read().await.get(&name) {
        Some(r) => Json(r.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn list(State(reg): State<Arc<Registrar>>) -> Json<Vec<NameRecord>> {
    Json(reg.list().await)
}

async fn register(State(reg): State<Arc<Registrar>>, Json(rec): Json<NameRecord>) -> Response {
    match reg.register(rec).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err((code, why)) => (code, why).into_response(),
    }
}

const SITE: Site = Site {
    host: "name.ov",
    name: "Names",
    pages: &[("/", "Check a name"), ("/claim", "Claim"), ("/directory", "Directory"), ("/help", "Help")],
    note: "name.ov doesn’t know who you are and keeps no logs. It stores names and their owners’ signatures, and can’t change where a name points. Works without JavaScript.",
    help: ("/help", "Help"),
};

/// Что ввёл посетитель → `имя.ov` (пусто — ничего).
fn normalize(raw: Option<&str>) -> Option<String> {
    let n = raw?.trim().trim_start_matches("http://").trim_end_matches('/').to_ascii_lowercase();
    (!n.is_empty()).then(|| if n.ends_with(".ov") { n } else { format!("{n}.ov") })
}

/// Шаги: заголовок, коротко, подробно, команда, что она печатает, как устроено.
fn steps(name: &str) -> [(&'static str, &'static str, String, Option<String>, Option<String>, &'static str); 4] {
    let base = name.trim_end_matches(".ov");
    let key = format!("{base}.key");
    [
        (
            "Create a key",
            "The key is your site’s identity and proof of ownership.",
            format!("Run this once. It creates the file {key} and prints your site’s long address. Keep the file somewhere safe and make a backup: whoever has it controls the site and the name."),
            Some(format!("overnet keygen {key}")),
            Some("your site’s long address, 56 letters and digits ending in .ov".into()),
            "The key is an Ed25519 key pair. The address is the public key plus a checksum and version, written in base32. Nobody issues addresses, so nobody can take one away.",
        ),
        (
            "Run your site",
            "Connect any website on your computer to overnet.",
            "Start your website as usual (here it listens on port 8080; even python -m http.server 8080 in a folder with an index.html will do), then connect it to the network. Your site doesn’t need a public IP address, a domain or a certificate.".into(),
            Some(format!("overnet service --key {key} --port 80=127.0.0.1:8080")),
            None,
            "The service picks introduction points from the relay directory and keeps circuits open to them. Visitors reach you through those circuits, so neither side learns the other’s IP address.",
        ),
        (
            "Sign the name",
            "Prove the name is yours with your key.",
            "This prints a short record: the name, your address and a signature. It’s safe to share; it doesn’t contain the key.".into(),
            Some(format!("overnet name-sign {name} --key {key}")),
            Some(format!(r#"{{"name":"{name}","address":"…","sig":"…"}}"#)),
            "The command signs the text “overnet-name-v1:” followed by the name. Anyone can check this signature against the address, so the registrar can’t point your name somewhere else.",
        ),
        (
            "Paste the record",
            "Paste the signed record here to register.",
            "Copy the whole output of step 3 and paste it below.".into(),
            None,
            None,
            "name.ov stores the record as it is. The first valid signature for a name wins; after that, only the same key can update it. The registrar can refuse or forget a name, but it can’t forge one.",
        ),
    ]
}

#[derive(Deserialize)]
struct ClaimForm {
    record: String,
}

async fn index(State(reg): State<Arc<Registrar>>, Query(q): Query<NameQ>) -> Html<String> {
    let asked = normalize(q.name.as_deref());
    let mut result = String::new();
    if let Some(name) = &asked {
        let n = esc(name);
        result = match reg.names.read().await.get(name) {
            Some(r) => format!(
                r#"<div role="status" class="note plain"><p><strong class="mono">{n}</strong> is taken. It points to:</p><span class="addr" style="font-size:14px;color:var(--text-2)">{a}</span>
<p style="display:flex;flex-wrap:wrap;gap:6px 20px;font-size:15px;margin-top:4px"><a href="http://{n}/">Open {n}</a><a href="/directory">See all names</a></p></div>"#,
                a = esc(&r.address)
            ),
            None if RESERVED.contains(&name.as_str()) => format!(
                r#"<div role="status" class="note warn"><p><strong class="mono">{n}</strong> is reserved for a network service and can’t be registered.</p></div>"#
            ),
            None if valid_name(name) => format!(
                r#"<div role="status" class="note ok row"><p><strong class="mono">{n}</strong> is free. You can claim it now, it takes about ten minutes.</p><a class="btn on-ok" href="/claim?name={n}">Claim this name</a></div>"#
            ),
            None => r#"<div role="alert" class="note err"><p>This name can’t be used. Use only lowercase Latin letters (a–z), digits and hyphens, for example <span class="mono">tea-house</span>. A name can’t start or end with a hyphen.</p></div>"#.into(),
        };
    }
    let mini: String = steps(asked.as_deref().unwrap_or("yoursite.ov"))
        .iter()
        .enumerate()
        .map(|(i, s)| format!(r#"<li><span class="n">{}</span><b>{}</b><span>{}</span></li>"#, i + 1, s.0, s.1))
        .collect();
    let draft = asked.as_deref().map(|n| n.trim_end_matches(".ov")).unwrap_or("");
    let body = format!(
        r#"<div class="col intro"><h1>A short name for your site</h1>
<p class="lede">Every .ov site has a long address made from its key, like <span class="mono" style="font-size:15px;word-break:break-all">k3mqx7v2…5ybhq3a.ov</span>. Here you can give it a name people can remember and say out loud, like <span class="mono" style="font-size:15px">shop.ov</span>.</p>
<form method="get" action="/" class="stack" style="margin-top:4px"><label for="name-q" class="lbl">Check if a name is free</label>
<div class="bar-form"><div class="suffix"><input id="name-q" name="name" value="{v}" autocapitalize="none" autocomplete="off" spellcheck="false" aria-describedby="name-rules"{af}><span aria-hidden="true">.ov</span></div>
<button class="btn big">Check</button></div>
<p id="name-rules" class="small muted">Lowercase Latin letters, digits and hyphens. First come, first served. Free of charge.</p></form>
{result}</div>
<section aria-labelledby="how-h" class="col" style="gap:14px"><h2 id="how-h">How claiming works</h2><ol class="mini-steps">{mini}</ol>
<p class="text-2" style="font-size:15px">You’ll need a computer with overnet installed. <a href="/claim{claim_q}">See the full instructions</a></p></section>"#,
        v = esc(draft),
        af = if asked.is_none() { " autofocus" } else { "" },
        claim_q = asked.as_deref().map(|n| format!("?name={}", esc(n))).unwrap_or_default(),
    );
    SITE.page("name.ov — short names for .ov sites", "/", &body)
}

/// Страница «как получить имя»; `outcome` — итог отправленной формы.
fn claim_page(name: &str, record: &str, outcome: &str) -> Html<String> {
    let n = esc(name);
    let list: String = steps(name)
        .into_iter()
        .enumerate()
        .map(|(i, (title, _, body, cmd, out, tech))| {
            let mut x = format!(r#"<h2>{title}</h2><p class="prose">{}</p>"#, esc(&body));
            if let Some(c) = cmd {
                x += &format!(r#"<div class="cmd"><code>{}</code>{}</div>"#, esc(&c), crate::copy_button(&c, "Copy"));
            }
            if let Some(o) = out {
                x += &format!(r#"<p class="small muted">It prints: <span class="mono text-2" style="word-break:break-all">{}</span></p>"#, esc(&o));
            }
            if i == 3 {
                x += &format!(
                    r#"<form method="post" action="/claim" class="stack" style="gap:8px"><label for="rec" class="lbl">Signed record</label>
<textarea id="rec" name="record" class="field mono" spellcheck="false" aria-describedby="rec-help" placeholder='{{"name":"{n}","address":"…","sig":"…"}}'>{r}</textarea>
<p id="rec-help" class="small muted">Paste everything the command printed, including the curly brackets.</p>
<div><button class="btn">Register name</button></div>{outcome}</form>"#,
                    r = esc(record)
                );
            }
            x += &format!(r#"<details class="more"><summary>How it works</summary><p>{tech}</p></details>"#);
            format!(r#"<li><div class="rail"><span class="num">{}</span></div><div class="body">{x}</div></li>"#, i + 1)
        })
        .collect();
    let body = format!(
        r#"<div class="col intro" style="max-width:760px;gap:12px"><h1>Claim <span class="mono" style="font-weight:400">{n}</span></h1>
<p class="lede">Four steps. The first three happen on your computer, in a terminal. Nothing secret is sent to name.ov: your key stays on your computer, only a signature is pasted here.</p></div>
<ol class="steps">{list}</ol>"#
    );
    SITE.page(&format!("Claim {name} — name.ov"), "/claim", &body)
}

async fn claim(Query(q): Query<NameQ>) -> Html<String> {
    let name = normalize(q.name.as_deref()).filter(|n| valid_name(n)).unwrap_or_else(|| "yoursite.ov".into());
    claim_page(&name, "", "")
}

async fn claim_post(State(reg): State<Arc<Registrar>>, Form(f): Form<ClaimForm>) -> Html<String> {
    let parsed: Result<NameRecord, _> = serde_json::from_str(f.record.trim());
    let (name, outcome) = match parsed {
        Err(_) => (
            "yoursite.ov".to_string(),
            r#"<div role="alert" class="note err"><strong>This isn’t a complete record.</strong><span>It may have been cut off while copying. Run step 3 again and paste the whole output, including the curly brackets.</span></div>"#.to_string(),
        ),
        Ok(rec) => {
            let name = rec.name.to_ascii_lowercase();
            let outcome = match reg.register(rec).await {
                Ok(()) => format!(
                    r#"<div role="status" class="note ok"><strong>Done. {n} is yours.</strong><span>People can open it now at <a href="http://{n}/">{n}</a>. Search will list it after its next refresh. Keep your key file safe: it’s the only way to update this name.</span></div>"#,
                    n = esc(&name)
                ),
                Err((code, why)) => {
                    let (title, hint) = match code {
                        StatusCode::CONFLICT => ("This name is already taken by another owner.", "Choose another name and sign it again."),
                        StatusCode::FORBIDDEN => ("This name is reserved for a network service.", "Choose another name and sign it again."),
                        StatusCode::BAD_REQUEST if why.starts_with("the signature") => (
                            "The signature doesn’t match the address.",
                            "The record may have been cut off while copying. Run step 3 again and paste the whole output.",
                        ),
                        StatusCode::BAD_REQUEST => ("This name can’t be used.", "Use only lowercase Latin letters, digits and hyphens, like tea-house.ov."),
                        _ => ("name.ov couldn’t save the record.", "Please try again in a minute."),
                    };
                    format!(r#"<div role="alert" class="note err"><strong>{title}</strong><span>{hint}</span></div>"#)
                }
            };
            (if valid_name(&name) { name } else { "yoursite.ov".into() }, outcome)
        }
    };
    claim_page(&name, &f.record, &outcome)
}

#[derive(Deserialize)]
struct DirQ {
    filter: Option<String>,
}

async fn directory(State(reg): State<Arc<Registrar>>, Query(q): Query<DirQ>) -> Html<String> {
    let all = reg.list().await;
    let filter = q.filter.unwrap_or_default().trim().to_ascii_lowercase();
    let shown: Vec<&NameRecord> = all.iter().filter(|r| r.name.contains(filter.as_str())).collect();
    let rows: String = shown
        .iter()
        .map(|r| {
            format!(
                r#"<li><a class="n" href="http://{n}/">{n}</a><span class="addr">{a}</span></li>"#,
                n = esc(&r.name),
                a = esc(&r.address)
            )
        })
        .collect();
    let empty = if !shown.is_empty() {
        String::new()
    } else if filter.is_empty() {
        r#"<div style="padding:28px 0" class="stack"><p style="font-weight:500">No names yet.</p><p class="text-2">Yours can be the first. <a href="/">Check a name</a></p></div>"#.into()
    } else {
        let f = esc(&filter);
        let base = esc(filter.trim_end_matches(".ov"));
        format!(
            r#"<div style="padding:28px 0" class="stack"><p style="font-weight:500">No names contain “{f}”.</p><p class="text-2">That means it may be free. <a href="/?name={base}">Check {base}.ov</a></p></div>"#
        )
    };
    let body = format!(
        r#"<div class="col intro wide" style="gap:14px"><h1>All registered names</h1>
<p class="text-2">{count} name{s}. Each one is signed by its owner; overnet checks the signature whenever it looks a name up.</p>
<form method="get" action="/directory" class="stack" style="max-width:420px"><label for="dir-q" class="lbl">Filter</label>
<div class="bar-form s"><input id="dir-q" name="filter" type="search" class="field" value="{v}" placeholder="Part of a name" autocomplete="off"><button class="btn plain">Filter</button></div></form></div>
<div class="dir"><div class="head" aria-hidden="true"><span style="flex:0 0 200px">Name</span><span style="flex:1">Address</span></div><ul>{rows}</ul>{empty}</div>"#,
        count = all.len(),
        s = if all.len() == 1 { "" } else { "s" },
        v = esc(&filter)
    );
    SITE.page("Directory — name.ov", "/directory", &body)
}

const FAQ: &[(&str, &str)] = &[
    ("Do I need a name?", "No. Every site already works at its long address. A name only makes it easier to remember and to tell other people."),
    ("Who owns a name?", "Whoever holds the key that signed it. name.ov doesn’t know who you are and never asks for an email or a phone number."),
    ("Can name.ov take my name or redirect it?", "It can’t redirect it: overnet checks the owner’s signature when it looks a name up and refuses a record that doesn’t match. In the worst case the registrar could refuse a name or stop listing it. Your site would still work at its long address."),
    ("I lost my key file. What now?", "Without the key, nobody can update the name, including you, and nobody can take it over. Create a new key, run your site with it, and register a different name."),
    ("Can I move my name to a new address?", "Not yet. A name stays tied to the key that first signed it."),
    ("Which names are reserved?", "search.ov, name.ov, mail.ov, files.ov and source.ov are built into the network and can’t be registered."),
    ("Does this site work without JavaScript?", "Yes. Checking a name, the directory and registration are all plain forms."),
];

async fn help() -> Html<String> {
    let qa: String = FAQ.iter().map(|(q, a)| format!(r#"<details><summary>{q}</summary><div class="a"><p>{a}</p></div></details>"#)).collect();
    let body = format!(
        r#"<article class="col intro" style="max-width:680px"><h1>Help with names</h1>
<p class="lede">A name is a shortcut to a site’s real address. Typing <span class="mono">shop.ov</span> takes you to the same place as its long key address.</p>
<div class="qa">{qa}</div></article>"#
    );
    SITE.page("Help — name.ov", "/help", &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use overnet_core::ovaddr::{name_message, ServiceKey};

    fn rec(k: &ServiceKey, name: &str) -> NameRecord {
        NameRecord { name: name.into(), address: k.id().to_address(), sig: hex::encode(k.sign(&name_message(name))) }
    }

    #[tokio::test]
    async fn first_come_and_only_owner_can_update() {
        let dir = std::env::temp_dir().join(format!("ov-reg-{}", crate::random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = Registrar::open(&dir);
        let a = ServiceKey::generate();
        let b = ServiceKey::generate();
        reg.register(rec(&a, "shop.ov")).await.unwrap();
        assert_eq!(reg.register(rec(&b, "shop.ov")).await.unwrap_err().0, StatusCode::CONFLICT);
        reg.register(rec(&a, "shop.ov")).await.unwrap();
        assert_eq!(reg.register(rec(&a, "search.ov")).await.unwrap_err().0, StatusCode::FORBIDDEN);
        let mut forged = rec(&a, "bank.ov");
        forged.address = b.id().to_address();
        assert!(reg.register(forged).await.is_err());
        // Переживает перезапуск.
        let again = Registrar::open(&dir);
        assert_eq!(again.list().await.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
