//! `files.ov` — обмен файлами по ссылке.
//!
//! Загрузил — получил ссылку `http://files.ov/f/<id>/<имя>`. Кто загрузил и кто
//! скачивает, сервис не знает (до него доходят только цепи overnet). Файлы
//! удаляются через `ttl` после загрузки. Хотите, чтобы и сервис не видел
//! содержимое, — шифруйте файл до загрузки.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, put};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::{page, random_id};

pub struct Files {
    dir: PathBuf,
    pub max_size: usize,
    pub ttl: Duration,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    name: String,
    size: usize,
    created: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Имя файла без путей и управляющих символов.
fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let s: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>' | ':' | '|' | '?' | '*'))
        .take(120)
        .collect();
    let s = s.trim().trim_start_matches('.').to_string();
    if s.is_empty() {
        "file".into()
    } else {
        s
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

impl Files {
    pub fn open(dir: &std::path::Path, max_size: usize, ttl: Duration) -> std::io::Result<Arc<Files>> {
        let dir = dir.join("files");
        std::fs::create_dir_all(&dir)?;
        Ok(Arc::new(Files { dir, max_size, ttl }))
    }

    /// Удалять просроченные файлы раз в час.
    pub fn spawn_sweeper(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                me.sweep().await;
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
    }

    pub async fn sweep(&self) {
        let Ok(mut rd) = tokio::fs::read_dir(&self.dir).await else { return };
        while let Ok(Some(e)) = rd.next_entry().await {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                let meta: Option<Meta> = tokio::fs::read(&p).await.ok().and_then(|b| serde_json::from_slice(&b).ok());
                if meta.is_none_or(|m| now().saturating_sub(m.created) > self.ttl.as_secs()) {
                    let _ = tokio::fs::remove_file(p.with_extension("bin")).await;
                    let _ = tokio::fs::remove_file(&p).await;
                }
            }
        }
    }

    pub async fn store(&self, name: &str, data: &[u8]) -> std::io::Result<String> {
        let id = random_id();
        let meta = Meta { name: clean_name(name), size: data.len(), created: now() };
        tokio::fs::write(self.dir.join(format!("{id}.bin")), data).await?;
        tokio::fs::write(self.dir.join(format!("{id}.json")), serde_json::to_vec(&meta)?).await?;
        Ok(format!("/f/{id}/{}", pct_encode(&meta.name)))
    }
}

pub fn router(f: Arc<Files>) -> Router {
    let limit = f.max_size;
    Router::new()
        .route("/", get(index))
        .route("/api/upload", put(upload))
        .route("/f/{id}/{name}", get(download))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(f)
}

async fn upload(State(f): State<Arc<Files>>, headers: HeaderMap, body: Bytes) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "пустой файл").into_response();
    }
    let name = headers.get("x-filename").and_then(|v| v.to_str().ok()).map(pct_decode).unwrap_or_default();
    match f.store(&name, &body).await {
        Ok(path) => (StatusCode::OK, format!("http://files.ov{path}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn download(State(f): State<Arc<Files>>, Path((id, _name)): Path<(String, String)>) -> Response {
    if id.len() != 26 || !id.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let meta: Option<Meta> = tokio::fs::read(f.dir.join(format!("{id}.json")))
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let Some(meta) = meta else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(data) = tokio::fs::read(f.dir.join(format!("{id}.bin"))).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            // Всегда «скачать», а не «открыть»: загруженный HTML не исполнится на files.ov.
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename*=UTF-8''{}", pct_encode(&meta.name))),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        data,
    )
        .into_response()
}

async fn index(State(f): State<Arc<Files>>) -> Html<String> {
    let body = format!(
        r#"<h1>Файлы</h1><p class="sub">Загрузите файл — получите ссылку. До {mb} МБ, хранится {days} дн. Сервис не знает, кто загрузил и кто скачает; если важно, чтобы он не видел и содержимое, зашифруйте файл заранее.</p>
<div class="card"><input type="file" id="f"><p><button onclick="up()">Загрузить</button> <span id="st" class="muted"></span></p>
<p id="out" style="display:none">Ссылка: <code id="link"></code> <button class="ghost" onclick="navigator.clipboard.writeText(document.getElementById('link').textContent)">Копировать</button></p></div>
<script>
async function up(){{const f=document.getElementById('f').files[0];const st=document.getElementById('st');if(!f){{st.textContent='выберите файл';return}}
st.className='muted';st.textContent='загрузка…';
try{{const r=await fetch('/api/upload',{{method:'PUT',headers:{{'X-Filename':encodeURIComponent(f.name)}},body:f}});const t=await r.text();
if(!r.ok){{st.className='err';st.textContent=t||('ошибка '+r.status);return}}
st.className='ok';st.textContent='готово';document.getElementById('link').textContent=t;document.getElementById('out').style.display='block';}}
catch(e){{st.className='err';st.textContent=e}}}}
</script>"#,
        mb = f.max_size >> 20,
        days = f.ttl.as_secs() / 86400
    );
    page("files.ov — файлы", "files.ov", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned_and_encoded() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\x\\отчёт \"1\".pdf"), "отчёт 1.pdf");
        assert_eq!(clean_name("..."), "file");
        assert_eq!(pct_decode(&pct_encode("отчёт 1.pdf")), "отчёт 1.pdf");
    }

    #[tokio::test]
    async fn store_and_expire() {
        let dir = std::env::temp_dir().join(format!("ov-files-{}", random_id()));
        let f = Files::open(&dir, 1 << 20, Duration::from_secs(0)).unwrap();
        let path = f.store("a.txt", b"hello").await.unwrap();
        assert!(path.starts_with("/f/") && path.ends_with("/a.txt"));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        f.sweep().await;
        assert_eq!(std::fs::read_dir(dir.join("files")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
