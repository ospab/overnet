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
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::RwLock;

use overnet_node::net::names::{valid_name, NameRecord, RESERVED};

use crate::{esc, load_json, page, save_json};

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
            return Err((StatusCode::BAD_REQUEST, "имя: латиница в нижнем регистре, цифры и дефис, вида shop.ov".into()));
        }
        if RESERVED.contains(&name.as_str()) {
            return Err((StatusCode::FORBIDDEN, "это имя зарезервировано за сервисом сети".into()));
        }
        let Some(id) = rec.verify() else {
            return Err((StatusCode::BAD_REQUEST, "подпись не сходится с адресом".into()));
        };
        let mut names = self.names.write().await;
        if let Some(cur) = names.get(&name) {
            if cur.verify() != Some(id) {
                return Err((StatusCode::CONFLICT, "имя уже занято другим владельцем".into()));
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
        .route("/api/resolve", get(resolve))
        .route("/api/list", get(list))
        .route("/api/register", post(register))
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

async fn index(State(reg): State<Arc<Registrar>>, Query(q): Query<NameQ>) -> Html<String> {
    let mut body = String::from(
        r#"<h1>Имена .ov</h1><p class="sub">Короткое имя вместо адреса из 56 символов. Владелец имени — тот, у кого ключ сервиса: регистратор хранит его подпись и подменить адрес не может.</p>
<form class="row" method="get"><input name="name" placeholder="shop.ov" autofocus><button>Найти</button></form>"#,
    );
    if let Some(name) = q.name.filter(|n| !n.is_empty()) {
        let name = name.to_ascii_lowercase();
        let name = if name.ends_with(".ov") { name } else { format!("{name}.ov") };
        body += &match reg.names.read().await.get(&name) {
            Some(r) => format!(
                r#"<div class="card"><b>{n}</b> занято<br><span class="mono">{a}</span><br><a href="http://{n}/">открыть</a></div>"#,
                n = esc(&r.name),
                a = esc(&r.address)
            ),
            None if RESERVED.contains(&name.as_str()) => format!(r#"<div class="card"><b>{}</b> зарезервировано за сервисом сети</div>"#, esc(&name)),
            None if valid_name(&name) => format!(r#"<div class="card"><b class="ok">{}</b> свободно</div>"#, esc(&name)),
            None => r#"<div class="card err">Такое имя нельзя: латиница в нижнем регистре, цифры и дефис.</div>"#.into(),
        };
    }
    body += r#"<h2>Зарегистрировать</h2><div class="card">
<p class="muted">На машине с сервисом выполните<br><code>overnet name-sign shop.ov --key shop.key</code><br>и вставьте сюда то, что она напечатает.</p>
<textarea id="rec" placeholder='{"name":"shop.ov","address":"…","sig":"…"}'></textarea>
<p><button onclick="reg()">Зарегистрировать</button> <span id="res"></span></p></div>
<script>
async function reg(){const r=document.getElementById('res');r.className='';r.textContent='…';
try{const resp=await fetch('/api/register',{method:'POST',headers:{'Content-Type':'application/json'},body:document.getElementById('rec').value});
const t=await resp.text();r.className=resp.ok?'ok':'err';r.textContent=resp.ok?'Готово':t;}catch(e){r.className='err';r.textContent=e;}}
</script><h2>Все имена</h2>"#;
    let list = reg.list().await;
    if list.is_empty() {
        body += r#"<p class="muted">Пока ни одного.</p>"#;
    }
    for r in list {
        body += &format!(r#"<div class="hit"><a class="t" href="http://{n}/">{n}</a><div class="u mono">{a}</div></div>"#, n = esc(&r.name), a = esc(&r.address));
    }
    page("name.ov — имена", "name.ov", &body)
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
