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
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::{copy_button, esc, random_id, Site};

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
    // Запас на заголовки multipart у формы без скриптов; сам файл проверяется отдельно.
    let limit = f.max_size + 64 * 1024;
    Router::new()
        .route("/", get(index))
        .route("/help", get(help))
        .route("/upload", post(upload_form))
        .route("/api/upload", put(upload))
        .route("/f/{id}/{name}", get(download))
        .merge(crate::assets())
        .layer(DefaultBodyLimit::max(limit))
        .with_state(f)
}

impl Files {
    fn site(&self) -> (String, String) {
        let days = crate::human_duration(self.ttl);
        let note = format!("files.ov doesn’t know who you are and keeps no logs. Files are deleted after {days}. Works without JavaScript.");
        (days, note)
    }

    fn page(&self, title: &str, on: &str, body: &str) -> Html<String> {
        let (_, note) = self.site();
        Site { host: "files.ov", name: "Files", pages: &[("/", "Upload"), ("/help", "Help")], note: &note, help: ("/help", "Help") }
            .page(title, on, body)
    }

    fn too_large(&self) -> String {
        format!("The limit is {}. You can split it into parts with an archiver, or compress photos and video first.", crate::human_size(self.max_size as u64))
    }
}

async fn upload(State(f): State<Arc<Files>>, headers: HeaderMap, body: Bytes) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty file").into_response();
    }
    if body.len() > f.max_size {
        return (StatusCode::PAYLOAD_TOO_LARGE, f.too_large()).into_response();
    }
    let name = headers.get("x-filename").and_then(|v| v.to_str().ok()).map(pct_decode).unwrap_or_default();
    match f.store(&name, &body).await {
        Ok(path) => (StatusCode::OK, format!("http://files.ov{path}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Первый файл из `multipart/form-data`: (имя, содержимое). Формы проще
/// разобрать руками, чем тянуть ради одного поля отдельную зависимость.
fn multipart_file<'a>(content_type: &str, body: &'a [u8]) -> Option<(String, &'a [u8])> {
    let boundary = content_type.split(';').find_map(|p| p.trim().strip_prefix("boundary="))?.trim_matches('"');
    let delim = format!("--{boundary}");
    let next = format!("\r\n{delim}");
    let mut rest = &body[find(body, delim.as_bytes())? + delim.len()..];
    loop {
        if rest.starts_with(b"--") {
            return None;
        }
        let h_end = find(rest, b"\r\n\r\n")?;
        let headers = String::from_utf8_lossy(&rest[..h_end]);
        let data = &rest[h_end + 4..];
        let end = find(data, next.as_bytes())?;
        let filename = headers
            .lines()
            .filter(|l| l.to_ascii_lowercase().starts_with("content-disposition"))
            .find_map(|l| l.split("filename=\"").nth(1).and_then(|r| r.split('"').next()).map(str::to_string));
        if let Some(name) = filename {
            return Some((name, &data[..end]));
        }
        rest = &data[end + next.len()..];
    }
}

fn done_html(f: &Files, link: &str, name: &str, size: usize, created: u64) -> String {
    format!(
        r#"<div class="col" id="done"><div role="status" class="note ok" style="display:block"><strong>Uploaded.</strong> <span style="word-break:break-all">{n}</span>, {s}. Anyone with the link can download it until {until}.</div>
<div class="stack"><label for="link" class="lbl">Link to the file</label><div class="bar-form s"><input id="link" class="field mono" readonly value="{l}" onfocus="this.select()" style="font-size:14px">{copy}</div></div>
<p class="prose" style="font-size:15px">The link is the only way to the file. Send it through a channel you trust, for example <a href="http://mail.ov/">mail.ov</a>. files.ov can see what’s inside the file; if that matters, encrypt it before uploading.</p>
<p style="display:flex;flex-wrap:wrap;gap:10px 20px;align-items:center"><a class="btn plain" href="/">Upload another file</a><a href="{l}">See what the recipient sees</a></p></div>"#,
        n = esc(name),
        s = crate::human_size(size as u64),
        until = crate::human_date(created + f.ttl.as_secs()),
        l = esc(link),
        copy = copy_button(link, "Copy link").replace("btn small", "btn big"),
    )
}

/// Загрузка обычной формой — для тех, у кого выключены скрипты.
async fn upload_form(State(f): State<Arc<Files>>, headers: HeaderMap, body: Bytes) -> Response {
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let main = match multipart_file(ct, &body) {
        Some((_, data)) if data.is_empty() => {
            r#"<div role="alert" class="note err"><strong>No file was chosen.</strong><span>Choose a file, then press Upload.</span></div>"#.to_string()
        }
        Some((_, data)) if data.len() > f.max_size => format!(
            r#"<div role="alert" class="note err"><strong>This file is too large: {}.</strong><span>{}</span></div>"#,
            crate::human_size(data.len() as u64),
            f.too_large()
        ),
        Some((name, data)) => match f.store(&name, data).await {
            Ok(path) => done_html(&f, &format!("http://files.ov{path}"), &clean_name(&name), data.len(), now()),
            Err(_) => r#"<div role="alert" class="note err"><strong>files.ov couldn’t save the file.</strong><span>Please try again in a minute.</span></div>"#.into(),
        },
        None => r#"<div role="alert" class="note err"><strong>The upload didn’t arrive complete.</strong><span>Please choose the file and try again.</span></div>"#.into(),
    };
    let body = format!(r#"<div class="col intro"><h1>Share a file by link</h1></div>{main}<p><a href="/">Back to upload</a></p>"#);
    f.page("files.ov — share a file by link", "/", &body).into_response()
}

#[derive(Deserialize)]
struct DlQ {
    dl: Option<String>,
}

async fn download(State(f): State<Arc<Files>>, Path((id, _name)): Path<(String, String)>, Query(q): Query<DlQ>, headers: HeaderMap) -> Response {
    // Браузер получает страницу с кнопкой, curl и wget — сам файл.
    let wants_page = q.dl.is_none()
        && headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"));
    let ok_id = id.len() == 26 && id.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
    let meta: Option<Meta> = if ok_id {
        tokio::fs::read(f.dir.join(format!("{id}.json"))).await.ok().and_then(|b| serde_json::from_slice(&b).ok())
    } else {
        None
    };
    let Some(meta) = meta else {
        if !wants_page {
            return StatusCode::NOT_FOUND.into_response();
        }
        let (days, _) = f.site();
        let body = format!(
            r#"<div class="col intro" style="max-width:600px;gap:10px"><h1>This file is no longer here</h1>
<p class="prose">Files are deleted {days} after upload, and this link may have expired. It’s also possible the link was copied incompletely: check that it ends with the file name.</p>
<p class="prose">Ask the sender to upload it again, or <a href="/">share a file yourself</a>.</p></div>"#
        );
        return (StatusCode::NOT_FOUND, f.page("File not found — files.ov", "", &body)).into_response();
    };
    if wants_page {
        let expires = meta.created + f.ttl.as_secs();
        let left = expires.saturating_sub(now()).div_ceil(86400);
        let in_days = match left {
            0 => "today".to_string(),
            1 => "tomorrow".to_string(),
            n => format!("in {n} days"),
        };
        let size = crate::human_size(meta.size as u64);
        // Время при 1 Мбит/с — честная оценка для медленных цепей.
        let mins = (meta.size as u64 * 8 / 1_000_000).div_ceil(60);
        let eta = if mins <= 1 { "Under a minute at 1 Mbit/s".to_string() } else { format!("About {mins} minutes at 1 Mbit/s") };
        let body = format!(
            r#"<div class="col intro" style="max-width:640px;gap:18px"><p class="text-2">Someone shared a file with you.</p>
<div class="panel" style="gap:16px"><h1 style="font-size:clamp(20px,4.6vw,26px);line-height:1.25;word-break:break-all">{n}</h1>
<dl class="facts"><dt>Size</dt><dd>{size}</dd><dt>Uploaded</dt><dd>{up}</dd><dt>Deleted on</dt><dd>{del}, {in_days}</dd></dl>
<p style="display:flex;flex-wrap:wrap;gap:10px 16px;align-items:center"><a class="btn big" href="?dl=1" download="{n}">Download, {size}</a><span class="small muted">{eta}</span></p></div>
<div class="stack prose" style="font-size:15px"><p>files.ov doesn’t know who uploaded this and doesn’t check what’s inside. Open it only if you expect it from someone you know.</p>
<p>The file is always saved to your device, never opened in the browser.</p></div></div>"#,
            n = esc(&meta.name),
            up = crate::human_date(meta.created),
            del = crate::human_date(expires),
        );
        return f.page(&format!("{} — files.ov", meta.name), "", &body).into_response();
    }
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
    let (days, _) = f.site();
    let max = crate::human_size(f.max_size as u64);
    let body = format!(
        r#"<div class="col intro" style="gap:14px"><h1>Share a file by link</h1>
<p class="lede">Upload a file and send the link to anyone on overnet. Up to {max}. The file is deleted after {days}. files.ov doesn’t know who uploads or who downloads.</p></div>
<form id="up" method="post" action="/upload" enctype="multipart/form-data" class="col" style="gap:12px">
<label class="drop" id="drop"><b>Drop a file here</b><span class="text-2">or <span class="u">choose one from your device</span></span>
<span class="small muted" id="picked">One file, up to {max}. To send several, put them in a .zip first.</span><input type="file" name="file" id="file"></label>
<p class="no-js"><button class="btn">Upload</button></p>
<p class="small muted">Without JavaScript, choose the file and press Upload. With it, upload starts on its own.</p></form>
<div class="col" id="busy" role="status" aria-live="polite" hidden><div class="panel"><div style="display:flex;flex-wrap:wrap;gap:4px 16px;align-items:baseline">
<span id="bname" style="flex:1 1 220px;min-width:0;font-weight:500;word-break:break-all"></span><span id="bprog" class="small muted"></span></div>
<div class="progress" role="progressbar" aria-label="Upload progress" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0" id="pbar"><i id="pfill"></i></div>
<div style="display:flex;flex-wrap:wrap;gap:8px 16px;align-items:center"><span class="small muted" style="flex:1 1 240px">Keep this page open. On a slow connection this can take several minutes.</span>
<button type="button" class="btn plain" id="cancel">Cancel</button></div></div></div>
<div class="col" id="fail" hidden><div role="alert" class="note err"><strong id="ftitle"></strong><span id="fbody"></span></div><p><button type="button" class="btn plain" id="again">Choose another file</button></p></div>
<div id="result"></div>
<div class="qa"><details><summary>How it works</summary><div class="a"><p>The file travels to files.ov through overnet relays, like every overnet connection, so the service never learns your IP address. It’s stored under a random 128-bit ID, which becomes part of the link. Nobody can guess a link or list the files. A cleaner runs every hour and deletes files older than {days}.</p></div></details></div>
<script>
(function(){{var MAX={max_b},TTL={ttl},$=function(i){{return document.getElementById(i)}},x=null;
var form=$('up'),drop=$('drop'),input=$('file');
function fmt(b){{return b>=1e6?(b/1e6).toFixed(1)+' MB':b>=1e3?Math.floor(b/1e3)+' KB':b+(b===1?' byte':' bytes')}}
function show(id){{['up','busy','fail'].forEach(function(k){{$(k).hidden=k!==id}})}}
function fail(t,b){{$('ftitle').textContent=t;$('fbody').textContent=b;show('fail')}}
function esc(s){{return String(s).replace(/[&<>"']/g,function(c){{return {{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[c]}})}}
function start(file){{if(!file)return;
if(file.size>MAX){{fail('This file is too large: '+fmt(file.size)+'.','The limit is '+fmt(MAX)+'. You can split it into parts with an archiver, or compress photos and video first.');return}}
if(!file.size){{fail('This file is empty.','Choose a file with something in it.');return}}
show('busy');$('bname').textContent=file.name;
var prog=function(n){{var p=Math.round(100*n/file.size);$('pfill').style.width=p+'%';$('pbar').setAttribute('aria-valuenow',p);$('bprog').textContent=fmt(n)+' of '+fmt(file.size)+' · '+p+'%'}};prog(0);
x=new XMLHttpRequest();x.open('PUT','/api/upload');x.setRequestHeader('X-Filename',encodeURIComponent(file.name));
x.upload.onprogress=function(e){{if(e.lengthComputable)prog(e.loaded*file.size/e.total)}};
x.onload=function(){{if(x.status!==200){{fail(x.status===413?'This file is too large.':'The upload didn’t finish.',x.responseText||('Error '+x.status));return}}
var link=x.responseText.trim(),until=new Date(Date.now()+TTL*1000).toLocaleDateString('en-GB',{{day:'numeric',month:'long',year:'numeric'}});
$('busy').hidden=true;$('result').innerHTML='<div class="col"><div role="status" class="note ok" style="display:block"><strong>Uploaded.</strong> <span style="word-break:break-all">'+esc(file.name)+'</span>, '+fmt(file.size)+'. Anyone with the link can download it until '+until+'.</div>'
+'<div class="stack"><label for="link" class="lbl">Link to the file</label><div class="bar-form s"><input id="link" class="field mono" readonly style="font-size:14px" value="'+esc(link)+'"><button type="button" class="btn big" id="cp">Copy link</button></div></div>'
+'<p class="prose" style="font-size:15px">The link is the only way to the file. Send it through a channel you trust, for example <a href="http://mail.ov/">mail.ov</a>. files.ov can see what’s inside the file; if that matters, encrypt it before uploading.</p>'
+'<p style="display:flex;flex-wrap:wrap;gap:10px 20px;align-items:center"><a class="btn plain" href="/">Upload another file</a><a href="'+esc(link)+'">See what the recipient sees</a></p></div>';
$('link').onfocus=function(){{this.select()}};$('cp').onclick=function(){{ovCopy(link,this)}}}};
x.onerror=function(){{fail('The upload didn’t finish.','The connection was interrupted. Please try again.')}};x.send(file)}}
input.addEventListener('change',function(){{start(input.files[0])}});
form.addEventListener('submit',function(e){{e.preventDefault();start(input.files[0])}});
['dragenter','dragover'].forEach(function(t){{drop.addEventListener(t,function(e){{e.preventDefault();drop.classList.add('over')}})}});
['dragleave','drop'].forEach(function(t){{drop.addEventListener(t,function(e){{e.preventDefault();drop.classList.remove('over')}})}});
drop.addEventListener('drop',function(e){{start(e.dataTransfer.files[0])}});
$('cancel').onclick=function(){{if(x)x.abort();input.value='';show('up')}};
$('again').onclick=function(){{input.value='';show('up')}}}})();
</script>"#,
        max_b = f.max_size,
        ttl = f.ttl.as_secs(),
    );
    f.page("files.ov — share a file by link", "/", &body)
}

async fn help(State(f): State<Arc<Files>>) -> Html<String> {
    let (days, _) = f.site();
    let faq = [
        ("Who can download my file?", "Anyone who has the link. There’s no list of files, and links can’t be guessed, but anyone you forward the link to can pass it on.".to_string()),
        ("Can files.ov see what’s in my file?", "Yes, the service stores the file as you upload it. It can’t see who you are. If the contents are sensitive, put the file in an encrypted archive and send the password separately.".into()),
        ("How long is a file kept?", format!("{days} from upload. After that it’s deleted for good and the link stops working. There’s no way to extend it; upload it again if needed.")),
        ("Can I delete a file earlier?", format!("Not yet. If you shared something by mistake, the link stops working after {days}.")),
        ("Why does the file download instead of opening?", "For your safety. A shared web page or document can’t run inside files.ov, so it can’t pretend to be part of the service.".into()),
        ("Photos can reveal where they were taken", "Many phones store the location and the device model inside each photo. Remove this information before uploading, or take a screenshot of the photo and share that instead.".into()),
        ("Can I download with a command-line tool?", "Yes. curl or wget given the link get the file itself; only browsers see the page with the Download button.".into()),
    ];
    let qa: String = faq.iter().map(|(q, a)| format!(r#"<details><summary>{q}</summary><div class="a"><p>{a}</p></div></details>"#)).collect();
    let body = format!(r#"<article class="col intro" style="max-width:680px"><h1>Help with files</h1><div class="qa">{qa}</div></article>"#);
    f.page("Help — files.ov", "/help", &body)
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

    #[test]
    fn plain_form_uploads_are_parsed() {
        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a b.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--XyZ\r\nmore\r\n--XyZ--\r\n";
        let (name, data) = multipart_file("multipart/form-data; boundary=XyZ", body).unwrap();
        assert_eq!((name.as_str(), data), ("a b.txt", &b"hello"[..]));
        assert!(multipart_file("multipart/form-data; boundary=XyZ", b"--XyZ--\r\n").is_none());
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
