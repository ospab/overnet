//! Служебные сайты overnet: `name.ov`, `search.ov`, `files.ov`, `mail.ov`.
//!
//! Это обычные HTTP-серверы на loopback; в сеть их выводит сервис overnet
//! (виртуальный порт 80 → локальный адрес). Логов посетителей нет: сервис и так
//! не знает, кто к нему пришёл, и не должен это копить.
//!
//! Оформление и шрифты каждый сайт отдаёт сам (`/_ov/…`): никаких внешних
//! шрифтов и CDN — сайт .ov не должен тянуть ничего из обычного интернета.

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;

pub mod files;
pub mod mail;
pub mod registrar;
pub mod search;

/// Общая тема всех служебных сайтов (дизайн-система overnet, тёмная и светлая).
pub const STYLE: &str = include_str!("../assets/overnet.css");

const FONTS: &[(&str, &[u8])] = &[
    ("IBMPlexSans-Regular.woff2", include_bytes!("../assets/fonts/IBMPlexSans-Regular.woff2")),
    ("IBMPlexSans-Medium.woff2", include_bytes!("../assets/fonts/IBMPlexSans-Medium.woff2")),
    ("IBMPlexSans-SemiBold.woff2", include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.woff2")),
    ("IBMPlexMono-Regular.woff2", include_bytes!("../assets/fonts/IBMPlexMono-Regular.woff2")),
    ("IBMPlexMono-Medium.woff2", include_bytes!("../assets/fonts/IBMPlexMono-Medium.woff2")),
];

/// Стили и шрифты: каждый сайт отдаёт их сам, со своего адреса.
pub fn assets<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route("/_ov/overnet.css", get(css)).route("/_ov/fonts/{f}", get(font))
}

async fn css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "max-age=86400")], STYLE).into_response()
}

async fn font(Path(f): Path<String>) -> Response {
    match FONTS.iter().find(|(n, _)| *n == f) {
        Some((_, b)) => ([(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "max-age=604800")], *b).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

const SERVICES: [(&str, &str); 4] = [("search.ov", "Search"), ("name.ov", "Names"), ("mail.ov", "Mail"), ("files.ov", "Files")];

/// Тема до первой отрисовки, чтобы не мигала; класс `js` открывает то, что без скриптов не работает.
const HEAD_SCRIPT: &str = r#"<script>document.documentElement.classList.add('js');try{var t=localStorage.getItem('ov-theme');if(t)document.documentElement.dataset.theme=t}catch(e){}</script>"#;

/// Переключатель темы и кнопки «Copy» (`data-copy`). Вне защищённого контекста
/// clipboard API нет — тогда копируем через выделение.
const SCRIPT: &str = r#"<script>
(function(){var d=document.documentElement,b=document.querySelector('.theme');
function lab(){b.textContent=d.dataset.theme==='light'?'Dark theme':'Light theme'}
if(b){lab();b.onclick=function(){var t=d.dataset.theme==='light'?'dark':'light';d.dataset.theme=t;try{localStorage.setItem('ov-theme',t)}catch(e){}lab()}}
window.ovCopy=function(text,btn){var done=function(){if(!btn)return;var o=btn.dataset.label||btn.textContent;btn.dataset.label=o;btn.textContent='Copied';setTimeout(function(){btn.textContent=o},2000)};
if(navigator.clipboard&&window.isSecureContext){navigator.clipboard.writeText(text).then(done,function(){})}else{var t=document.createElement('textarea');t.value=text;t.style.position='fixed';t.style.opacity='0';document.body.appendChild(t);t.select();try{document.execCommand('copy');done()}catch(e){}t.remove()}};
document.querySelectorAll('[data-copy]').forEach(function(x){x.addEventListener('click',function(){ovCopy(x.dataset.copy,x)})})})();
</script>"#;

/// Один служебный сайт: адрес, название, свои страницы и строка в подвале.
pub struct Site<'a> {
    pub host: &'a str,
    pub name: &'a str,
    /// (ссылка, подпись) страниц сайта во второй строке шапки.
    pub pages: &'a [(&'a str, &'a str)],
    /// Строка в подвале: что сервис знает о посетителе. HTML.
    pub note: &'a str,
    /// (ссылка, подпись) справки в подвале.
    pub help: (&'a str, &'a str),
}

impl Site<'_> {
    /// Страница в общем оформлении. `on` — ссылка подсвеченной страницы сайта.
    pub fn page(&self, title: &str, on: &str, body: &str) -> Html<String> {
        let svc: String = SERVICES
            .iter()
            .map(|(h, t)| format!(r#"<a href="http://{h}/"{}>{t}</a>"#, if *h == self.host { r#" aria-current="true""# } else { "" }))
            .collect();
        let pages: String = self
            .pages
            .iter()
            .map(|(href, t)| format!(r#"<a href="{href}"{}>{t}</a>"#, if *href == on { r#" aria-current="page""# } else { "" }))
            .collect();
        Html(format!(
            r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="dark light"><title>{t}</title>{HEAD_SCRIPT}<link rel="stylesheet" href="/_ov/overnet.css"></head><body>
<header><a class="skip" href="#main">Skip to content</a>
<div class="wrap top"><a class="brand" href="http://search.ov/" aria-label="overnet home"><span class="mark" aria-hidden="true"></span>overnet</a>
<nav class="svc" aria-label="overnet services">{svc}</nav><button type="button" class="theme js-only" aria-label="Switch colour theme">Light theme</button></div>
<div class="rule"></div>
<div class="wrap sub"><div class="sub-name"><b>{name}</b><span>{host}</span></div><nav class="pages" aria-label="{host} pages">{pages}</nav></div>
</header>
<main id="main">{body}</main>
<footer><div class="rule"></div><div class="wrap foot"><p>{note}</p><nav aria-label="Footer"><a href="{hh}">{hl}</a><a href="http://search.ov/">All services</a><a href="http://source.ov/">Source code</a><a href="https://github.com/ospab/overnet">GitHub</a></nav></div></footer>
{SCRIPT}</body></html>"##,
            t = esc(title),
            name = self.name,
            host = self.host,
            note = self.note,
            hh = self.help.0,
            hl = self.help.1,
        ))
    }
}

/// Кнопка «Copy» для текста `v` (видна только со скриптами).
pub fn copy_button(v: &str, label: &str) -> String {
    format!(r#"<button type="button" class="btn small js-only" data-copy="{}">{label}</button>"#, esc(v))
}

/// Размер для людей: «18.4 MB», «312 KB», «14 bytes».
pub fn human_size(b: u64) -> String {
    if b >= 1_000_000 {
        format!("{:.1} MB", b as f64 / 1e6)
    } else if b >= 1000 {
        format!("{} KB", b / 1000)
    } else {
        format!("{b} byte{}", if b == 1 { "" } else { "s" })
    }
}

/// Длительность для людей: «10 minutes», «7 days».
pub fn human_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (n, unit) = if s >= 86400 && s % 86400 == 0 {
        (s / 86400, "day")
    } else if s >= 3600 && s % 3600 == 0 {
        (s / 3600, "hour")
    } else if s >= 60 {
        (s / 60, "minute")
    } else {
        (s, "second")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// Дата (UTC) из секунд Unix: «28 September 2026».
pub fn human_date(unix: u64) -> String {
    const M: [&str; 12] =
        ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    // civil_from_days Говарда Хиннанта.
    let z = (unix / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{d} {} {y}", M[(m - 1) as usize])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_sizes_and_durations_read_naturally() {
        assert_eq!(human_date(0), "1 January 1970");
        assert_eq!(human_date(951_782_400), "29 February 2000");
        assert_eq!(human_size(18_400_000), "18.4 MB");
        assert_eq!(human_size(312_400), "312 KB");
        assert_eq!(human_size(14), "14 bytes");
        assert_eq!(human_duration(std::time::Duration::from_secs(600)), "10 minutes");
        assert_eq!(human_duration(std::time::Duration::from_secs(7 * 86400)), "7 days");
    }
}
