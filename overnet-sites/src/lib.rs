//! Служебные сайты overnet: `name.ov`, `search.ov`, `files.ov`, `mail.ov`.
//!
//! Это обычные HTTP-серверы на loopback; в сеть их выводит сервис overnet
//! (виртуальный порт 80 → локальный адрес). Логов посетителей нет: сервис и так
//! не знает, кто к нему пришёл, и не должен это копить.

use axum::response::Html;

pub mod files;
pub mod mail;
pub mod registrar;
pub mod search;

/// Общий стиль (тот же, что у остальных частей overnet).
pub const STYLE: &str = r#"
:root{--bg:#0b0b10;--panel:#15151d;--panel2:#1c1c26;--line:rgba(255,255,255,.08);
--text:#ececf1;--muted:#8a8a99;--accent:#9d6bff;--accent2:#b88bff;--ok:#34d399;--err:#f87171}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:15px/1.55 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
a{color:var(--accent2);text-decoration:none}a:hover{text-decoration:underline}
header{display:flex;align-items:center;gap:18px;padding:14px 24px;border-bottom:1px solid var(--line)}
header .logo{font-weight:700;font-size:18px;color:var(--text)}header .logo span{color:var(--accent)}
header nav{display:flex;gap:14px;font-size:14px}header nav a{color:var(--muted)}header nav a.on{color:var(--text)}
main{max-width:860px;margin:0 auto;padding:32px 24px 80px}
h1{font-size:30px;margin:0 0 6px}h2{font-size:19px;margin:32px 0 10px}
.sub{color:var(--muted);margin:0 0 24px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:14px;padding:18px;margin:12px 0}
input,textarea{width:100%;background:var(--panel2);border:1px solid var(--line);color:var(--text);border-radius:10px;padding:10px 12px;font:inherit}
input:focus,textarea:focus{outline:none;border-color:var(--accent)}
textarea{min-height:110px;font-family:ui-monospace,monospace;font-size:13px}
button,.btn{background:var(--accent);color:#fff;border:0;border-radius:10px;padding:10px 16px;font:inherit;cursor:pointer}
button.ghost{background:var(--panel2);color:var(--text);border:1px solid var(--line)}
.row{display:flex;gap:10px;align-items:center}.row>input{flex:1}
code,.mono{font-family:ui-monospace,monospace;font-size:13px;word-break:break-all;color:var(--accent2)}
.muted{color:var(--muted)}.ok{color:var(--ok)}.err{color:var(--err)}
.hit{padding:14px 0;border-bottom:1px solid var(--line)}.hit .t{font-size:17px}.hit .u{font-size:12px}
footer{color:var(--muted);font-size:12px;text-align:center;padding:30px}
"#;

/// Страница в общем оформлении. `on` — какой сервис подсвечен в шапке.
pub fn page(title: &str, on: &str, body: &str) -> Html<String> {
    let nav = [("search.ov", "Поиск"), ("name.ov", "Имена"), ("mail.ov", "Почта"), ("files.ov", "Файлы")]
        .iter()
        .map(|(h, t)| format!(r#"<a href="http://{h}/"{}>{t}</a>"#, if *h == on { r#" class="on""# } else { "" }))
        .collect::<String>();
    Html(format!(
        r#"<!doctype html><html lang="ru"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{t}</title><style>{STYLE}</style></head><body>
<header><a class="logo" href="http://search.ov/"><span>over</span>net</a><nav>{nav}</nav></header>
<main>{body}</main><footer>overnet · сервис не знает, кто вы, и не ведёт журналов</footer></body></html>"#,
        t = esc(title)
    ))
}

/// Экранирование для HTML.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Случайный идентификатор (base32, 128 бит).
pub fn random_id() -> String {
    let mut b = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    data_encoding::BASE32_NOPAD.encode(&b).to_ascii_lowercase()
}

/// Сохранить JSON атомарно (через временный файл).
pub async fn save_json<T: serde::Serialize>(path: &std::path::Path, v: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(v)?).await?;
    tokio::fs::rename(&tmp, path).await
}

pub fn load_json<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}
