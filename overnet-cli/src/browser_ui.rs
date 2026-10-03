//! `browser.ov` — страницы браузера, которые отдаёт сам шлюз, без сети:
//! стартовая страница с состоянием сети, заглушка для обычных сайтов и кнопка
//! «новые цепи».
//!
//! Браузер overnet переводит каждый переход на сайт не из .ov на
//! `http://browser.ov/go?url=…`; здесь решается, пускать ли его (через выход
//! overnet) или показать заглушку. «Открыть в обычном браузере» запускает
//! программу на этой машине, поэтому работает только у шлюза, запущенного для
//! браузера (`--browser`), и только с токеном со страницы: чужой сайт его не
//! прочитает, а встроить страницу во фрейм нельзя.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;

use overnet_node::net::client::Client;
use overnet_node::net::gateway::Clearnet;
use overnet_sites::{esc, Site};

pub struct Ui {
    pub client: Arc<Client>,
    pub clearnet: Clearnet,
    /// Можно ли открывать сайты в обычном браузере этой машины.
    pub open_external: bool,
    /// Секрет страниц на время работы шлюза: без него действия не выполняются.
    pub token: String,
}

impl Ui {
    pub fn new(client: Arc<Client>, clearnet: Clearnet, open_external: bool) -> Ui {
        let token = hex::encode(rand::random::<[u8; 16]>());
        Ui { client, clearnet, open_external, token }
    }

    /// Пускать ли обычные сайты прямо сейчас (через выход overnet или напрямую).
    fn clearnet_open(&self) -> bool {
        match self.clearnet {
            Clearnet::Direct => true,
            Clearnet::Exit => self.client.directory_size().1 > 0,
            Clearnet::Block => false,
        }
    }
}

pub fn router(ui: Arc<Ui>) -> Router {
    Router::new()
        .route("/", get(start))
        .route("/go", get(go))
        .route("/open", get(open))
        .route("/new-circuits", post(new_circuits))
        .merge(overnet_sites::assets())
        .with_state(ui)
}

const SITE: Site = Site {
    host: "browser.ov",
    name: "Browser",
    pages: &[("/", "Start")],
    note: "This page is served by overnet on your own computer. Nothing on it goes over the network.",
    help: ("https://github.com/ospab/overnet/blob/master/docs/running.md", "Help"),
};

/// Страница с запретом фреймов и кэша: на ней токен.
fn page(title: &str, body: &str) -> Response {
    (
        [
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        SITE.page(title, "/", body),
    )
        .into_response()
}

#[derive(Deserialize)]
struct Done {
    done: Option<String>,
}

async fn start(State(ui): State<Arc<Ui>>, Query(q): Query<Done>) -> Response {
    let (relays, exits) = ui.client.directory_size();
    let status = if relays == 0 {
        r#"<div class="note warn"><strong>The network directory hasn't loaded yet.</strong><span>overnet keeps trying; .ov sites open once it arrives.</span></div>"#.to_string()
    } else {
        format!(r#"<div class="note ok"><strong>Connected to overnet.</strong><span>{relays} relays in the directory.</span></div>"#)
    };
    let regular = match (ui.clearnet, exits) {
        (Clearnet::Block, _) => "blocked: this browser opens only .ov sites".to_string(),
        (Clearnet::Exit, 0) => "through exit relays, but none are available right now".to_string(),
        (Clearnet::Exit, n) => format!("through overnet exit relays ({n} available)"),
        (Clearnet::Direct, _) => "directly, without overnet — sites see your IP address".to_string(),
    };
    let done = match q.done.as_deref() {
        Some("circuits") => r#"<div class="note ok"><strong>New circuits.</strong><span>The next pages open through new relays.</span></div>"#,
        _ => "",
    };
    let services: String = [
        ("search.ov", "Search", "Find sites on overnet."),
        ("name.ov", "Names", "A short name like shop.ov for your site."),
        ("mail.ov", "Mail", "Letters encrypted in your browser."),
        ("files.ov", "Files", "Share a file by link."),
        ("source.ov", "Source", "The code of overnet itself."),
    ]
    .iter()
    .map(|(h, t, d)| format!(r#"<a class="panel" style="text-decoration:none;color:inherit" href="http://{h}/"><h2 class="s">{t}</h2><p class="text-2">{d}</p><span class="addr">{h}</span></a>"#))
    .collect();
    let body = format!(
        r#"<div class="col intro"><h1>overnet browser</h1>
<p class="lede">Sites ending in .ov open through overnet: nobody on the way sees which site you visit, and the site doesn't learn who you are.</p>
<form role="search" method="get" action="http://search.ov/" class="bar-form"><label for="q" class="sr">Search .ov sites</label>
<input id="q" class="field" name="q" type="search" placeholder="Search overnet or type an address like shop.ov" autocomplete="off" autofocus>
<button class="btn big">Search</button></form>
{done}{status}</div>
<div class="panels">{services}</div>
<div class="col"><h2>This browser</h2>
<p class="text-2">Regular sites: {regular}.</p>
<form method="post" action="/new-circuits" class="stack"><input type="hidden" name="t" value="{t}">
<p class="text-2">Start over with new paths through the network: open .ov sites through other relays.</p>
<div><button class="btn plain">New circuits</button></div></form>
<p class="muted small">overnet {v}</p></div>"#,
        t = ui.token,
        v = env!("CARGO_PKG_VERSION"),
    );
    page("overnet browser", &body)
}

#[derive(Deserialize)]
struct Target {
    url: String,
    t: Option<String>,
}

/// Хост ссылки http(s)://; другие схемы сюда не попадают.
fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let host = auth.rsplit('@').next()?;
    let host = if host.starts_with('[') {
        host.split(']').next()?.trim_start_matches('[')
    } else {
        host.split(':').next()?
    };
    (!host.is_empty() && !url.contains(['\r', '\n', '"', ' '])).then(|| host.to_ascii_lowercase())
}

async fn go(State(ui): State<Arc<Ui>>, Query(q): Query<Target>) -> Response {
    let Some(host) = host_of(&q.url) else {
        return (StatusCode::BAD_REQUEST, "not an http(s) link").into_response();
    };
    if ui.clearnet_open() {
        // Браузер пропускает переход, у которого в цепочке редиректов есть browser.ov.
        return Redirect::to(&q.url).into_response();
    }
    let why = match ui.clearnet {
        Clearnet::Exit => "overnet can carry regular sites through exit relays, but none are available in the network right now.",
        _ => "This browser opens only .ov sites. A regular site would see your connection and could tie your visits together, so it stays outside.",
    };
    let open = if ui.open_external {
        format!(
            r#"<a class="btn big" href="/open?url={u}&amp;t={t}">Open in my regular browser</a>"#,
            u = urlencode(&q.url),
            t = ui.token
        )
    } else {
        String::new()
    };
    let body = format!(
        r#"<div class="col intro"><h1>{h} is not an overnet site</h1>
<p class="lede">{why}</p>
<p class="addr">{u}</p>
<div class="row-btns" style="display:flex;flex-wrap:wrap;gap:12px">{open}<a class="btn plain big" href="/" onclick="if(history.length>1){{history.back();return false}}">Go back</a></div>
<p class="muted small">Your regular browser connects directly: the site sees your IP address, as it always does.</p></div>"#,
        h = esc(&host),
        u = esc(&q.url),
    );
    page(&format!("{host} is not an overnet site"), &body)
}

async fn open(State(ui): State<Arc<Ui>>, Query(q): Query<Target>) -> Response {
    if !ui.open_external || q.t.as_deref() != Some(ui.token.as_str()) || host_of(&q.url).is_none() {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let res = open_in_system_browser(&q.url);
    let body = match res {
        Ok(()) => r#"<div class="col intro"><h1>Opened in your regular browser</h1>
<p class="lede">The site is open in your regular browser, outside overnet.</p>
<div><a class="btn plain big" href="/" onclick="if(history.length>2){history.go(-2);return false}">Back</a></div></div>"#
            .to_string(),
        Err(e) => format!(
            r#"<div class="col intro"><h1>Couldn't open your regular browser</h1><p class="lede">{}</p><p class="addr">{}</p></div>"#,
            esc(&e),
            esc(&q.url)
        ),
    };
    page("Opened in your regular browser", &body)
}

#[derive(Deserialize)]
struct Token {
    t: String,
}

async fn new_circuits(State(ui): State<Arc<Ui>>, Form(f): Form<Token>) -> Response {
    if f.t != ui.token {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    ui.client.new_identity().await;
    Redirect::to("/?done=circuits").into_response()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Открыть ссылку в браузере системы по умолчанию. Ссылка уже проверена:
/// http(s), без пробелов и кавычек, так что в аргументы её можно отдать как есть.
fn open_in_system_browser(url: &str) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        // Без cmd.exe: «&» в ссылке не станет разделителем команд.
        let mut c = std::process::Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{host_of, urlencode};

    #[test]
    fn hosts_of_links() {
        assert_eq!(host_of("https://www.youtube.com/watch?v=1").as_deref(), Some("www.youtube.com"));
        assert_eq!(host_of("http://user@Example.com:8080/x").as_deref(), Some("example.com"));
        assert_eq!(host_of("https://[::1]:443/").as_deref(), Some("::1"));
        assert_eq!(host_of("file:///etc/passwd"), None);
        assert_eq!(host_of("javascript:alert(1)"), None);
        assert_eq!(host_of("https://a.com/\" onload=x"), None);
    }

    #[test]
    fn links_are_encoded_for_query() {
        assert_eq!(urlencode("https://a.com/?x=1&y=2"), "https%3A%2F%2Fa.com%2F%3Fx%3D1%26y%3D2");
    }
}
