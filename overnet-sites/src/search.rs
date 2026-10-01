//! `search.ov` — поиск по сайтам .ov.
//!
//! Список сайтов — у регистратора `name.ov`; сам обход идёт через overnet, как у
//! любого клиента: сайты не знают, что их индексирует именно search.ov. Запросы
//! посетителей не сохраняются.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tokio::sync::RwLock;

use overnet_node::net::client::Client;
use overnet_node::net::names::{http_get, NameRecord, Names, RESERVED};

use crate::{esc, page};

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub name: String,
    pub title: String,
    pub description: String,
    pub text: String,
}

#[derive(Default)]
pub struct Search {
    index: RwLock<Vec<Entry>>,
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
            Err(_) => Entry { name: name.clone(), ..Default::default() },
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
    Entry { name: name.to_string(), title, description, text }
}

#[derive(Deserialize)]
struct Q {
    q: Option<String>,
}

pub fn router(s: Arc<Search>) -> Router {
    Router::new().route("/", get(index)).with_state(s)
}

async fn index(State(s): State<Arc<Search>>, Query(q): Query<Q>) -> Html<String> {
    let q = q.q.unwrap_or_default();
    let mut body = format!(
        r#"<h1>Поиск по <span style="color:var(--accent)">overnet</span></h1><p class="sub">Сайты .ov: без трекинга, без журналов запросов.</p>
<form class="row" method="get"><input name="q" value="{}" placeholder="что ищем?" autofocus><button>Искать</button></form>"#,
        esc(&q)
    );
    if q.trim().is_empty() {
        body += r#"<h2>Сервисы сети</h2>
<div class="hit"><a class="t" href="http://name.ov/">name.ov</a><div class="muted">Имена: зарегистрировать короткое имя для своего сайта</div></div>
<div class="hit"><a class="t" href="http://mail.ov/">mail.ov</a><div class="muted">Почта со сквозным шифрованием в браузере</div></div>
<div class="hit"><a class="t" href="http://files.ov/">files.ov</a><div class="muted">Обмен файлами по ссылке</div></div>"#;
    } else {
        let hits = s.query(&q).await;
        if hits.is_empty() {
            body += r#"<p class="muted">Ничего не нашлось.</p>"#;
        }
        for e in hits {
            let snippet: String = if e.description.is_empty() { e.text.chars().take(220).collect() } else { e.description.clone() };
            body += &format!(
                r#"<div class="hit"><a class="t" href="http://{n}/">{t}</a><div class="u mono">{n}</div><div class="muted">{s}</div></div>"#,
                n = esc(&e.name),
                t = esc(if e.title.is_empty() { &e.name } else { &e.title }),
                s = esc(&snippet)
            );
        }
    }
    page("search.ov", "search.ov", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_description_and_text_without_scripts() {
        let e = extract(
            "shop.ov",
            r#"<html><head><title> Лавка </title><meta name="description" content="Чай и кофе"></head>
<body><script>var secret=1</script><h1>Добро пожаловать</h1><p>Свежий улун</p></body></html>"#,
        );
        assert_eq!(e.title, "Лавка");
        assert_eq!(e.description, "Чай и кофе");
        assert!(e.text.contains("Свежий улун"));
        assert!(!e.text.contains("secret"));
    }

    #[tokio::test]
    async fn ranking_prefers_name_and_title() {
        let s = Search::new();
        s.set_index(vec![
            Entry { name: "a.ov".into(), text: "про чай".into(), ..Default::default() },
            Entry { name: "tea.ov".into(), title: "чай".into(), ..Default::default() },
            Entry { name: "c.ov".into(), text: "про кофе".into(), ..Default::default() },
        ])
        .await;
        let r = s.query("Чай").await;
        assert_eq!(r.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["tea.ov", "a.ov"]);
    }
}
