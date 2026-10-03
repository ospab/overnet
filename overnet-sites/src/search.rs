//! `search.ov` — поиск по сайтам .ov.
//!
//! Список сайтов — у регистратора `name.ov`; сам обход идёт через overnet, как у
//! любого клиента: сайты не знают, что их индексирует именно search.ov. Запросы
//! посетителей не сохраняются.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tokio::sync::RwLock;

use overnet_node::net::client::Client;
use overnet_node::net::names::{http_get, NameRecord, Names, RESERVED};

use crate::{esc, Site};

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub name: String,
    pub title: String,
    pub description: String,
    pub text: String,
    /// Не ответил при последнем обходе: в выдаче остаётся по имени.
    pub offline: bool,
}

#[derive(Default)]
pub struct Search {
    index: RwLock<Vec<Entry>>,
    /// Период обхода в секундах (для страниц); 0 — обход не запущен.
    every: AtomicU64,
}

impl Search {
    pub fn new() -> Arc<Search> {
        Arc::new(Search::default())
    }

    pub async fn set_index(&self, e: Vec<Entry>) {
        *self.index.write().await = e;
    }

    pub async fn query(&self, q: &str) -> Vec<Entry> {
        let terms: Vec<String> = q.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let idx = self.index.read().await;
        let mut scored: Vec<(u32, &Entry)> = idx
            .iter()
            .map(|e| {
                let (n, t, d, x) = (e.name.to_lowercase(), e.title.to_lowercase(), e.description.to_lowercase(), e.text.to_lowercase());
                let s = terms
                    .iter()
                    .map(|w| {
                        5 * n.contains(w.as_str()) as u32
                            + 3 * t.contains(w.as_str()) as u32
                            + 2 * d.contains(w.as_str()) as u32
                            + x.contains(w.as_str()) as u32
                    })
                    .sum();
                (s, e)
            })
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        scored.into_iter().take(50).map(|(_, e)| e.clone()).collect()
    }

    /// Обходить сайты раз в `every`.
    pub fn spawn_crawler(self: &Arc<Self>, client: Arc<Client>, names: Arc<Names>, every: Duration) {
        self.every.store(every.as_secs(), Ordering::Relaxed);
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let idx = crawl(&client, &names).await;
                if !idx.is_empty() {
                    me.set_index(idx).await;
                }
                tokio::time::sleep(every).await;
            }
        });
    }
}

/// Один обход: все зарегистрированные имена и служебные сайты.
pub async fn crawl(client: &Client, names: &Names) -> Vec<Entry> {
    let mut sites: Vec<(String, overnet_core::ovaddr::ServiceId)> =
        names.reserved().iter().map(|(n, id)| (n.clone(), *id)).collect();
    if let Some(reg) = names.reserved().get("name.ov") {
        if let Ok((200, body)) = http_get(client, *reg, "/api/list").await {
            let recs: Vec<NameRecord> = serde_json::from_slice(&body).unwrap_or_default();
            sites.extend(recs.into_iter().filter_map(|r| r.verify().map(|id| (r.name, id))));
        }
    }
    let mut out = Vec::new();
    for (name, id) in sites {
        let entry = match http_get(client, id, "/").await {
            Ok((_, body)) => extract(&name, &String::from_utf8_lossy(&body)),
            Err(_) if RESERVED.contains(&name.as_str()) => continue,
            // Недоступный сейчас сайт остаётся в выдаче по имени.
            Err(_) => Entry { name: name.clone(), offline: true, ..Default::default() },
        };
        out.push(entry);
    }
    out
}

fn between<'a>(s: &'a str, lower: &str, start: &str, end: &str) -> Option<&'a str> {
    let i = lower.find(start)? + start.len();
    let j = lower[i..].find(end)? + i;
    Some(&s[i..j])
}

/// Заголовок, описание и текст страницы — без HTML-парсера, грубо, но достаточно.
pub fn extract(name: &str, html: &str) -> Entry {
    // Только ASCII-нижний регистр: смещения байтов совпадают с оригиналом.
    let lower = html.to_ascii_lowercase();
    let title = between(html, &lower, "<title>", "</title>").unwrap_or("").trim().to_string();
    let description = lower
        .find("name=\"description\"")
        .and_then(|i| {
            let rest = &html[i..];
            let rl = &lower[i..];
            between(rest, rl, "content=\"", "\"")
        })
        .unwrap_or("")
        .to_string();
    let body = lower.find("<body").map(|i| &html[i..]).unwrap_or(html);
    let mut text = String::new();
    let mut in_tag = false;
    let mut skip = false;
    let bl = body.to_ascii_lowercase();
    let mut i = 0;
    let bytes = body.as_bytes();
    while i < bytes.len() && text.len() < 4000 {
        if bl[i..].starts_with("<script") || bl[i..].starts_with("<style") {
            skip = true;
        }
        if skip && (bl[i..].starts_with("</script>") || bl[i..].starts_with("</style>")) {
            skip = false;
        }
        let c = body[i..].chars().next().unwrap();
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag && !skip => text.push(c),
            _ => {}
        }
        i += c.len_utf8();
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Entry { name: name.to_string(), title, description, text, offline: false }
}

#[derive(Deserialize)]
struct Q {
    q: Option<String>,
    page: Option<usize>,
}

pub fn router(s: Arc<Search>) -> Router {
    Router::new().route("/", get(index)).route("/about", get(about)).merge(crate::assets()).with_state(s)
}

/// Запрос вида `shop.ov` — это адрес, а не поиск: сразу ведём на сайт.
fn as_site(q: &str) -> Option<String> {
    let q = q.trim().trim_start_matches("http://").trim_end_matches('/').to_ascii_lowercase();
    let host = q.split('/').next().unwrap_or("");
    let label = host.strip_suffix(".ov")?;
    let ok = !label.is_empty() && label.split('.').all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    ok.then(|| format!("http://{q}/"))
}

const SITE: Site = Site {
    host: "search.ov",
    name: "Search",
    pages: &[("/", "Search"), ("/about", "How search works")],
    note: "search.ov doesn’t know who you are and doesn’t record what you search for. Works without JavaScript.",
    help: ("/about", "How search works"),
};

const SERVICES: &[(&str, &str, &str)] = &[
    ("name.ov", "Names — a short name for your site", "Register an address like shop.ov instead of a 56-character key, or look up who owns a name."),
    ("mail.ov", "Mail — letters encrypted in your browser", "The server stores only scrambled text. Your key stays with you, saved as a file."),
    ("files.ov", "Files — share a file by link", "Upload a file and get a link. Files are deleted after a few days."),
    ("source.ov", "Source — the code of overnet itself", "Every part of the network, open to read, check and build yourself. Also mirrored on GitHub."),
];

const PER_PAGE: usize = 10;

fn search_form(id: &str, q: &str, big: bool) -> String {
    format!(
        r#"<form role="search" method="get" action="/" class="bar-form{s}"><label for="{id}" class="sr">Search .ov sites</label>
<input id="{id}" class="field" name="q" type="search" value="{v}" placeholder="What are you looking for?" autocomplete="off"{af}>
<button class="btn{b}">Search</button></form>"#,
        s = if big { "" } else { " s" },
        b = if big { " big" } else { "" },
        af = if big { " autofocus" } else { "" },
        v = esc(q)
    )
}

fn result(addr: &str, title: &str, snippet: &str, extra: &str) -> String {
    format!(
        r#"<li><div class="site"><span class="fav" aria-hidden="true">{l}</span><span class="addr">{a}</span></div><a class="t" href="http://{a}/">{t}</a><p>{s}</p>{extra}</li>"#,
        l = esc(&addr.chars().next().unwrap_or('?').to_string()),
        a = esc(addr),
        t = esc(title),
        s = esc(snippet)
    )
}

fn every_text(s: &Search) -> Option<String> {
    match s.every.load(Ordering::Relaxed) {
        0 => None,
        n => Some(crate::human_duration(Duration::from_secs(n))),
    }
}

async fn index(State(s): State<Arc<Search>>, Query(q): Query<Q>) -> Response {
    let query = q.q.unwrap_or_default();
    if let Some(url) = as_site(&query) {
        return Redirect::to(&url).into_response();
    }
    let query = query.trim().to_string();
    let every = every_text(&s);
    if query.is_empty() {
        let n = s.index.read().await.len();
        let status = match (n, &every) {
            (0, _) => "The first crawl of the network is still running.".to_string(),
            (n, Some(e)) => format!("{n} site{} in the index, refreshed every {e}.", if n == 1 { "" } else { "s" }),
            (n, None) => format!("{n} site{} in the index.", if n == 1 { "" } else { "s" }),
        };
        let services: String = SERVICES.iter().map(|(a, t, sn)| result(a, t, sn, "")).collect();
        let body = format!(
            r#"<div class="col intro"><h1>Search all <span style="color:var(--accent-text)">over</span> the <span style="color:var(--accent-text)">net</span></h1>
<p class="lede">Finds .ov sites by their name, title and text. Your searches are not recorded, and the sites you find don’t learn where you came from.</p>
{form}<p class="small muted">{status} You can also type an address, like <span class="mono">shop.ov</span>, to go straight there. <a href="/about">How search works</a></p></div>
<section aria-labelledby="svc-h" class="col"><h2 id="svc-h" class="s text-2">Services built into the network</h2><ul class="results">{services}</ul></section>"#,
            form = search_form("q-home", "", true)
        );
        return SITE.page("search.ov — search sites on overnet", "/", &body).into_response();
    }

    let all = s.query(&query).await;
    let pages = all.len().div_ceil(PER_PAGE).max(1);
    let p = q.page.unwrap_or(1).clamp(1, pages);
    let count = if all.is_empty() {
        "This search is not saved.".to_string()
    } else {
        format!(
            "{} site{} for “{}”{}. This search is not saved.",
            all.len(),
            if all.len() == 1 { "" } else { "s" },
            esc(&query),
            if pages > 1 { format!(", page {p} of {pages}") } else { String::new() }
        )
    };
    let mut body = format!(
        r#"<div class="col intro" style="gap:14px">{form}<p role="status" class="small muted">{count}</p></div>"#,
        form = search_form("q-res", &query, false)
    );
    if all.is_empty() {
        let refresh = match &every {
            Some(e) => format!("New sites appear after the next index refresh, within {e}."),
            None => "New sites appear after the next index refresh.".into(),
        };
        body += &format!(
            r#"<div class="col" style="max-width:600px;gap:10px"><h2>Nothing found for “{q}”</h2><ul class="plain">
<li>Check the spelling, or try fewer or more general words.</li><li>{refresh}</li>
<li>If you know a site’s short name, look it up on <a href="http://name.ov/">name.ov</a>.</li></ul></div>"#,
            q = esc(&query)
        );
    } else {
        let hits: String = all
            .iter()
            .skip((p - 1) * PER_PAGE)
            .take(PER_PAGE)
            .map(|e| {
                let snippet: String = if e.description.is_empty() { e.text.chars().take(220).collect() } else { e.description.clone() };
                let title = if e.title.is_empty() { &e.name } else { &e.title };
                let extra = if e.offline {
                    r#"<p class="small warn-text">Didn’t answer at the last check. It may be back later.</p>"#
                } else {
                    ""
                };
                result(&e.name, title, &snippet, extra)
            })
            .collect();
        body += &format!(r#"<ol class="results">{hits}</ol>"#);
        if pages > 1 {
            let link = |n: usize| format!("/?q={}&amp;page={n}", pct(&query));
            let mut nav = String::from(r#"<nav class="pager" aria-label="Result pages">"#);
            if p > 1 {
                nav += &format!(r#"<a class="edge" href="{}">Previous</a>"#, link(p - 1));
            }
            for n in 1..=pages {
                nav += &if n == p {
                    format!(r#"<span aria-current="page">{n}</span>"#)
                } else {
                    format!(r#"<a href="{}" aria-label="Page {n}">{n}</a>"#, link(n))
                };
            }
            if p < pages {
                nav += &format!(r#"<a class="edge" href="{}">Next</a>"#, link(p + 1));
            }
            body += &(nav + "</nav>");
        }
    }
    SITE.page(&format!("{query} — search.ov"), "/", &body).into_response()
}

/// Кодирование для параметра запроса.
fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

async fn about(State(s): State<Arc<Search>>) -> Response {
    let every = every_text(&s).map(|e| format!("Every {e}")).unwrap_or_else(|| "Regularly".into());
    let body = format!(
        r#"<article class="col intro" style="max-width:680px"><h1>How search works</h1>
<p class="lede">search.ov keeps a list of every site that has a short name on name.ov, plus the network’s own services. {every} it visits each one, reads its title and text, and builds an index. When you search, it looks your words up in that index.</p>
<h2 style="margin-top:16px">What we keep, and what we don’t</h2>
<p class="prose">We keep the index: site names, titles, descriptions and the beginning of each site’s text. We don’t keep your searches. There are no accounts, no cookies and no logs. Like every overnet service, search.ov can’t see who you are: your request reaches it through a chain of relays, and none of them knows both who you are and what you asked for.</p>
<h2 style="margin-top:16px">Why a site might be missing</h2>
<ul class="plain"><li>It has no short name yet. Sites known only by their long address aren’t listed until their owner registers a name on <a href="http://name.ov/">name.ov</a>.</li>
<li>It was registered very recently. It will appear after the next refresh.</li>
<li>It didn’t answer when we visited. It stays in results under its name, marked as not reachable.</li></ul>
<h2 style="margin-top:16px">How results are ordered</h2>
<p class="prose">A match in the site’s name counts most, then its title, then its description, then the rest of the text. Nobody can pay to appear higher, and there are no ads.</p>
<p class="prose">Typing an address like <span class="mono">shop.ov</span> into the search box takes you straight to the site.</p>
<div class="qa" style="margin-top:12px">
<details><summary>Technical details: crawling</summary><div class="a"><p>The crawler fetches the list of names from <code>name.ov/api/list</code>, checks each owner’s signature, then requests <code>/</code> from every site through ordinary overnet circuits. Sites can’t tell that the visitor is search.ov.</p>
<p>From each page it keeps the <code>&lt;title&gt;</code>, the description meta tag and the first 4,000 characters of visible text. Scripts and styles are skipped.</p></div></details>
<details><summary>Technical details: ranking</summary><div class="a"><p>Each search word scores 5 points for a match in the name, 3 in the title, 2 in the description and 1 in the text. Results are sorted by total score, then alphabetically, and capped at 50.</p></div></details>
<details><summary>Does this page work without JavaScript?</summary><div class="a"><p>Yes. Search is a plain form, and results are a plain page. Nothing on search.ov needs scripts.</p></div></details>
</div></article>"#
    );
    SITE.page("How search works — search.ov", "/about", &body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_description_and_text_without_scripts() {
        let e = extract(
            "shop.ov",
            r#"<html><head><title> Shop </title><meta name="description" content="Tea and coffee"></head>
<body><script>var secret=1</script><h1>Welcome</h1><p>Fresh oolong</p></body></html>"#,
        );
        assert_eq!(e.title, "Shop");
        assert_eq!(e.description, "Tea and coffee");
        assert!(e.text.contains("Fresh oolong"));
        assert!(!e.text.contains("secret"));
    }

    #[test]
    fn addresses_in_the_search_box_go_to_the_site() {
        assert_eq!(as_site("Shop.ov").as_deref(), Some("http://shop.ov/"));
        assert_eq!(as_site("http://www.shop.ov/a").as_deref(), Some("http://www.shop.ov/a/"));
        assert_eq!(as_site("cheap tea"), None);
        assert_eq!(as_site("shop.com"), None);
        assert_eq!(as_site(".ov"), None);
    }

    #[tokio::test]
    async fn ranking_prefers_name_and_title() {
        let s = Search::new();
        s.set_index(vec![
            Entry { name: "a.ov".into(), text: "about tea".into(), ..Default::default() },
            Entry { name: "tea.ov".into(), title: "tea".into(), ..Default::default() },
            Entry { name: "c.ov".into(), text: "about coffee".into(), ..Default::default() },
        ])
        .await;
        let r = s.query("Tea").await;
        assert_eq!(r.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["tea.ov", "a.ov"]);
    }
}
