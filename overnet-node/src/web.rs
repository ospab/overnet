//! Прикладной слой overnet: HTTP-подобный запрос/ответ поверх onion.
//!
//! - `fetch_*` — клиентская сторона. **`fetch_first_service` дёргает Tauri-браузер**
//!   при открытии `overnet://<сервис>/`.
//! - `run_service` — серверная сторона (узел-сервис, твой «search.ov»).
//!
//! MVP: клиент ↔ сервис напрямую (1 хоп). Релеи в пути — следующий шаг (router уже
//! умеет Forward, а протокол ответа через `reply_path` это уже поддерживает).

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use overnet_core::onion::{self, OnionKey, Peeled};
use overnet_core::{Error, Link, Result};
use overnet_link_tcp::{TcpLink, TcpListenerLink};

use crate::bootstrap::NodeInfo;
use crate::router::Router;

/// REGISTER-маркер: узел сообщает соседу свой onion-pubkey для обратного пути.
const REGISTER_TAG: u8 = 0xFF;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HttpRequest {
    /// hex(onion-pubkey адресата ответа = клиент).
    pub reply_to: String,
    /// hex-хопы обратного пути (сервис → … → клиент), без самого клиента.
    /// Для direct (без релея) — пусто.
    pub reply_path: Vec<String>,
    pub method: String,
    pub path: String,
    /// Тело запроса (для загрузок — base64 байт файла). Пусто для обычного GET.
    #[serde(default)]
    pub body: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Запросить `path` у конкретного сервиса (direct, 1 хоп). Возвращает ответ.
pub async fn request(
    service: &NodeInfo,
    method: &str,
    path: &str,
    body: String,
) -> Result<HttpResponse> {
    let service_pub =
        hex_decode_32(&service.pubkey).ok_or_else(|| Error::Link("bad service pubkey".into()))?;

    // Своя одноразовая onion-личность на этот запрос.
    let client_key = OnionKey::generate();
    let client_pub = client_key.public();

    let link = TcpLink::connect(&service.address).await?;

    // REGISTER — чтобы сервис прислал ответ по этому же соединению.
    let mut register = Vec::with_capacity(33);
    register.push(REGISTER_TAG);
    register.extend_from_slice(&client_pub);
    link.send(&register).await?;

    // Запрос: direct → reply_path пуст, ответ адресован нам.
    let req = HttpRequest {
        reply_to: hex_encode(&client_pub),
        reply_path: Vec::new(),
        method: method.to_string(),
        path: path.to_string(),
        body,
    };
    let req_bytes = serde_json::to_vec(&req).map_err(|_| Error::Link("json encode".into()))?;
    let pkt = onion::wrap(&[service_pub], &req_bytes)?;
    link.send(&pkt).await?;

    // Ждём ответ (onion для нас), снимаем слой, парсим.
    let frame = tokio::time::timeout(Duration::from_secs(10), link.recv())
        .await
        .map_err(|_| Error::Link("service timeout".into()))??;
    match onion::peel(&client_key, &frame)? {
        Peeled::Deliver(resp_bytes) => {
            serde_json::from_slice(&resp_bytes).map_err(|_| Error::Link("bad response json".into()))
        }
        Peeled::Forward { .. } => Err(Error::Link("unexpected forward in reply".into())),
    }
}

/// GET-запрос к сервису (тонкая обёртка над `request`).
pub async fn fetch_from(service: &NodeInfo, path: &str) -> Result<HttpResponse> {
    request(service, "GET", path, String::new()).await
}

/// Найти узел-сервис в каталоге по имени или pubkey и запросить у него `path`.
pub async fn fetch_service(bootstrap_addr: &str, token: String, host: &str, path: &str) -> Result<HttpResponse> {
    use crate::bootstrap::fetch_directory_with_token;
    let dir = fetch_directory_with_token(bootstrap_addr, token).await?;
    let service = dir
        .nodes
        .into_iter()
        .find(|n| n.role == "service" && (n.name == host || n.pubkey == host))
        .ok_or_else(|| Error::Link(format!("service '{}' not found in directory", host)))?;
    fetch_from(&service, path).await
}

/// Запросить `path` у сервиса ЧЕРЕЗ релей (2 хопа): client → relay → service.
/// Релей не видит ни содержимого, ни адресата (onion); ответ идёт обратно через него.
pub async fn fetch_via_relay(
    relay: &NodeInfo,
    service_pub_hex: &str,
    path: &str,
) -> Result<HttpResponse> {
    let relay_pub =
        hex_decode_32(&relay.pubkey).ok_or_else(|| Error::Link("bad relay pubkey".into()))?;
    let service_pub =
        hex_decode_32(service_pub_hex).ok_or_else(|| Error::Link("bad service pubkey".into()))?;

    let client_key = OnionKey::generate();
    let client_pub = client_key.public();

    // Подключаемся к РЕЛЕЮ и регистрируемся (чтобы ответ дошёл до нас через него).
    let link = TcpLink::connect(&relay.address).await?;
    let mut register = Vec::with_capacity(33);
    register.push(REGISTER_TAG);
    register.extend_from_slice(&client_pub);
    link.send(&register).await?;

    // Обратный путь = [relay]; адресат ответа = мы.
    let req = HttpRequest {
        reply_to: hex_encode(&client_pub),
        reply_path: vec![hex_encode(&relay_pub)],
        method: "GET".into(),
        path: path.to_string(),
        body: String::new(),
    };
    let req_bytes = serde_json::to_vec(&req).map_err(|_| Error::Link("json encode".into()))?;
    // 2-хоповый onion: relay -> service.
    let pkt = onion::wrap(&[relay_pub, service_pub], &req_bytes)?;
    link.send(&pkt).await?;

    let frame = tokio::time::timeout(Duration::from_secs(10), link.recv())
        .await
        .map_err(|_| Error::Link("relay/service timeout".into()))??;
    match onion::peel(&client_key, &frame)? {
        Peeled::Deliver(resp_bytes) => {
            serde_json::from_slice(&resp_bytes).map_err(|_| Error::Link("bad response json".into()))
        }
        Peeled::Forward { .. } => Err(Error::Link("unexpected forward in reply".into())),
    }
}

/// Запустить узел-сервис: на каждый запрос зовёт `handler` и шлёт ответ обратно по
/// onion (обратный путь = reply_path + reply_to).
pub async fn run_service<F>(listener: TcpListenerLink, key: OnionKey, handler: F) -> Result<()>
where
    F: Fn(&HttpRequest) -> HttpResponse + Send + Sync + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel();
    let router = Arc::new(Router::new(key, tx));

    let responder = router.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let req: HttpRequest = match serde_json::from_slice(&msg) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let resp = handler(&req);
            let resp_bytes = match serde_json::to_vec(&resp) {
                Ok(b) => b,
                Err(_) => continue,
            };
            // Обратный путь: хопы reply_path, затем сам клиент (reply_to).
            let mut reverse = Vec::new();
            for hop in &req.reply_path {
                if let Some(pk) = hex_decode_32(hop) {
                    reverse.push(pk);
                }
            }
            if let Some(pk) = hex_decode_32(&req.reply_to) {
                reverse.push(pk);
            }
            if reverse.is_empty() {
                continue;
            }
            if let Ok(pkt) = onion::wrap(&reverse, &resp_bytes) {
                let first = reverse[0];
                let _ = responder.send_to_neighbor(&first, pkt).await;
            }
        }
    });

    router.serve(listener).await
}

/// Запустить узел-релей: пересылает onion-трафик и периодически устанавливает
/// двусторонние registered-связи со всеми сервисами из каталога — чтобы ответы
/// могли идти обратно через релей. Сам ничего не «доставляет».
/// NAT-traversal: узел за NAT сам подключается ИСХОДЯЩЕ к достижимому релею и
/// обслуживает запросы по этой же связи. Релей роутит к нам по нашему исходящему
/// соединению (через REGISTER). Так сервис/файлоузел работает с домашней/мобильной
/// сети, где входящие невозможны. Клиент достаёт его через `fetch_via_relay`.
pub async fn serve_via_relay<F>(relay_addr: &str, key: OnionKey, handler: F) -> Result<()>
where
    F: Fn(&HttpRequest) -> HttpResponse + Send + Sync + 'static,
{
    let my_pub = key.public();
    let link = TcpLink::connect(relay_addr).await?;
    // Регистрируемся у релея: теперь он шлёт нам пакеты по этой исходящей связи.
    let mut reg = Vec::with_capacity(33);
    reg.push(REGISTER_TAG);
    reg.extend_from_slice(&my_pub);
    link.send(&reg).await?;

    loop {
        let frame = match link.recv().await {
            Ok(f) => f,
            Err(_) => return Ok(()), // связь с релеем оборвалась
        };
        match onion::peel(&key, &frame) {
            Ok(Peeled::Deliver(req_bytes)) => {
                let req: HttpRequest = match serde_json::from_slice(&req_bytes) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let resp = handler(&req);
                let resp_bytes = match serde_json::to_vec(&resp) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                // Обратный путь = reply_path (релеи) + reply_to (клиент). Шлём релею.
                let mut reverse = Vec::new();
                for h in &req.reply_path {
                    if let Some(pk) = hex_decode_32(h) {
                        reverse.push(pk);
                    }
                }
                if let Some(pk) = hex_decode_32(&req.reply_to) {
                    reverse.push(pk);
                }
                if reverse.is_empty() {
                    continue;
                }
                if let Ok(pkt) = onion::wrap(&reverse, &resp_bytes) {
                    let _ = link.send(&pkt).await;
                }
            }
            Ok(Peeled::Forward { .. }) => {} // мы конечный узел — не пересылаем
            Err(_) => {}
        }
    }
}

pub async fn run_relay(
    listener: TcpListenerLink,
    key: OnionKey,
    bootstrap_addr: String,
) -> Result<()> {
    let relay_pub = key.public();
    // Релею не нужен канал доставки — просто осушаем его.
    let (deliver_tx, mut deliver_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move { while deliver_rx.recv().await.is_some() {} });

    let router = Arc::new(Router::new(key, deliver_tx));

    // Фон: подключаемся к сервисам и регистрируемся у них (для обратного пути).
    {
        let router = router.clone();
        tokio::spawn(async move {
            let mut known: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
            loop {
                if let Ok(dir) = crate::bootstrap::fetch_directory(&bootstrap_addr).await {
                    for n in dir.nodes {
                        if n.role != "service" {
                            continue;
                        }
                        let Some(svc_pub) = hex_decode_32(&n.pubkey) else { continue };
                        if svc_pub == relay_pub || known.contains(&svc_pub) {
                            continue;
                        }
                        if let Ok(link) = TcpLink::connect(&n.address).await {
                            let link = Arc::new(link);
                            let mut reg = Vec::with_capacity(33);
                            reg.push(REGISTER_TAG);
                            reg.extend_from_slice(&relay_pub);
                            if link.send(&reg).await.is_ok() {
                                router.add_active_link(svc_pub, link).await;
                                known.insert(svc_pub);
                            }
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
    }

    router.serve(listener).await
}

/// Локальный HTTP-шлюз для браузера на Chromium (Electron/CEF): принимает обычный
/// HTTP GET на `127.0.0.1` и отдаёт HTML, сходив в overnet. Браузер вешает схему
/// `overnet://` на этот эндпоинт — так ядро overnet не зависит от выбора браузера.
pub async fn run_gateway(bind: &str, bootstrap_addr: String) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| Error::Link(format!("gateway bind: {e}")))?;
    run_gateway_on(listener, bootstrap_addr).await
}

/// То же на уже привязанном listener (для тестов / заранее открытого порта).
pub async fn run_gateway_on(
    listener: tokio::net::TcpListener,
    bootstrap_addr: String,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| Error::Link(format!("gateway accept: {e}")))?;
        let bootstrap = bootstrap_addr.clone();
        tokio::spawn(async move {
            // Читаем запрос до конца заголовков.
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                match stream.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16384 {
                            break;
                        }
                    }
                    Err(_) => return,
                }
            }
            let (host, path) = parse_request(&buf).unwrap_or_else(|| ("".to_string(), "/".to_string()));
            let token = crate::bootstrap::load_token(); // Gateway использует локальный токен
            let (status, body) = match fetch_service(&bootstrap, token, &host, &path).await {
                Ok(r) => (r.status, r.body),
                Err(e) => (502, format!("<h1>overnet gateway error</h1><pre>{e}</pre>")),
            };
            let resp = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.as_bytes().len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.flush().await;
        });
    }
}

fn parse_request(buf: &[u8]) -> Option<(String, String)> {
    let text = std::str::from_utf8(buf).ok()?;
    let mut lines = text.lines();
    let first_line = lines.next()?;
    let mut parts = first_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;
    
    let mut host = String::new();
    for line in lines {
        if line.to_lowercase().starts_with("host:") {
            host = line[5..].trim().to_string();
            break;
        }
    }
    Some((host, path.to_string()))
}

/// Отрендерить каталог сервисов как HTML (страница `search.ov`). `q` — фильтр по имени.
/// Загрузить onion-ключ из файла `path` или создать новый и сохранить.
/// Персистентная личность нужна, чтобы имя (.ov) оставалось за узлом между рестартами
/// (бутстрап привязывает имя к ключу-владельцу — см. bootstrap.rs).
pub fn load_or_create_key(path: &str) -> OnionKey {
    if let Ok(bytes) = std::fs::read(path) {
        if bytes.len() == 32 {
            let mut b = [0u8; 32];
            b.copy_from_slice(&bytes);
            return OnionKey::from_secret_bytes(b);
        }
    }
    let key = OnionKey::generate();
    let _ = std::fs::write(path, key.secret_bytes());
    key
}

/// Сайт-сервер: отдаёт файлы из каталога `dir` по onion (GET /path).
/// «Фреймворк для сайтов»: положи HTML в папку — и `overnet://name.ov/` работает,
/// без единой строки кода. Сейчас текстовые файлы (HTML/CSS/JS/SVG); бинарь — следующий шаг.
pub async fn run_site(listener: TcpListenerLink, key: OnionKey, dir: String) -> Result<()> {
    let root = std::path::PathBuf::from(dir);
    run_service(listener, key, move |req| {
        // путь без query, без ведущего '/'
        let rel = req.path.split('?').next().unwrap_or("/").trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };
        // защита от traversal (минимальная; later — canonicalize под root)
        if rel.contains("..") || rel.contains('\\') {
            return HttpResponse { status: 403, body: "<h1>403</h1>".into() };
        }
        let mut path = root.clone();
        path.push(rel);
        if path.is_dir() {
            path.push("index.html");
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => HttpResponse { status: 200, body: content },
            Err(_) => HttpResponse {
                status: 404,
                body: format!("<h1>404</h1><p>{} не найден на этом .ov-сайте.</p>", req.path),
            },
        }
    })
    .await
}

fn b64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Файлообменник: `PUT` (тело = base64 файла) сохраняет и возвращает id (sha256-hex);
/// `GET /<id>` отдаёт файл (тело = base64). Контент-адресация = дедуп + целостность
/// (id это хеш содержимого, подменить нельзя). Бинарь идёт base64 в JSON-теле.
pub async fn run_filestore(listener: TcpListenerLink, key: OnionKey, dir: String) -> Result<()> {
    let root = std::path::PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&root);
    run_service(listener, key, move |req| {
        if req.method == "PUT" {
            match b64_decode(&req.body) {
                Some(bytes) => {
                    let id = sha256_hex(&bytes);
                    let _ = std::fs::write(root.join(&id), &bytes);
                    HttpResponse { status: 200, body: id }
                }
                None => HttpResponse { status: 400, body: "bad upload".into() },
            }
        } else {
            let id = req.path.trim_start_matches('/');
            if id.is_empty() || id.contains('/') || id.contains("..") || id.contains('\\') {
                return HttpResponse { status: 400, body: "bad id".into() };
            }
            match std::fs::read(root.join(id)) {
                Ok(bytes) => HttpResponse { status: 200, body: b64_encode(&bytes) },
                Err(_) => HttpResponse { status: 404, body: "not found".into() },
            }
        }
    })
    .await
}

/// Загрузить файл на файлообменник, вернуть его id (sha256-hex).
pub async fn upload_file(service: &NodeInfo, data: &[u8]) -> Result<String> {
    let resp = request(service, "PUT", "/", b64_encode(data)).await?;
    if resp.status == 200 {
        Ok(resp.body)
    } else {
        Err(Error::Link(format!("upload failed: {}", resp.status)))
    }
}

/// Скачать файл по id.
pub async fn download_file(service: &NodeInfo, id: &str) -> Result<Vec<u8>> {
    let resp = request(service, "GET", &format!("/{id}"), String::new()).await?;
    if resp.status != 200 {
        return Err(Error::Link(format!("download failed: {}", resp.status)));
    }
    b64_decode(&resp.body).ok_or_else(|| Error::Link("bad base64 in file".into()))
}

/// P2P-файлосервер: отдаёт файлы из `dir` **только на чтение** по onion (GET /<name>),
/// тело — base64. Твой узел отдаёт ТВОИ файлы; загрузок от других нет (никакого склада
/// чужого контента). Релеи несут зашифрованные байты — не видят и не хранят содержимое.
pub async fn run_fileserver(listener: TcpListenerLink, key: OnionKey, dir: String) -> Result<()> {
    let root = std::path::PathBuf::from(&dir);
    let _ = std::fs::create_dir_all(&root);
    run_service(listener, key, move |req| {
        let name = req.path.split('?').next().unwrap_or("/").trim_start_matches('/');
        if name.is_empty() || name.contains("..") || name.contains('/') || name.contains('\\') {
            return HttpResponse { status: 400, body: "bad name".into() };
        }
        match std::fs::read(root.join(name)) {
            Ok(bytes) => HttpResponse { status: 200, body: b64_encode(&bytes) },
            Err(_) => HttpResponse { status: 404, body: "not found".into() },
        }
    })
    .await
}

pub fn render_catalog(dir: &crate::bootstrap::Directory, q: &str) -> String {
    let ql = q.to_lowercase();
    let mut services: Vec<&NodeInfo> = dir.nodes.iter().filter(|n| n.role == "service").collect();
    services.sort_by(|a, b| a.name.cmp(&b.name));
    let mut items = String::new();
    for n in services {
        if !ql.is_empty() && !n.name.to_lowercase().contains(&ql) {
            continue;
        }
        let host = if n.name.is_empty() { &n.pubkey } else { &n.name };
        let label = if n.name.is_empty() {
            format!("{}…", &n.pubkey[..16.min(n.pubkey.len())])
        } else {
            n.name.clone()
        };
        items.push_str(&format!(
            "<a class=\"card\" href=\"overnet://{host}/\"><div class=\"name\">{label}</div>\
             <div class=\"sub\">{}…</div></a>",
            &n.pubkey[..8.min(n.pubkey.len())]
        ));
    }
    let body = if items.is_empty() {
        "<div class=\"empty\"><p>Пока здесь только каталог — сеть новая.</p>\
         <p>Добавь сервис, и он появится тут:</p>\
         <pre>overnet service 0.0.0.0:4040 &lt;bootstrap&gt; news.ov</pre></div>"
            .to_string()
    } else {
        format!("<div class=\"grid\">{items}</div>")
    };
    format!(
        "<html><head><meta charset=\"utf-8\"><title>search.ov</title><style>\
         body{{font-family:system-ui,sans-serif;background:radial-gradient(circle at 50% 0%,#1e1e2a,#0d0d12);color:#eee;margin:0;min-height:100vh}}\
         .wrap{{max-width:760px;margin:0 auto;padding:48px 20px}}\
         h1{{font-size:44px;margin:0 0 4px;letter-spacing:-1px}}h1 span{{color:#9d4edd}}\
         .tag{{color:#888;margin:0 0 28px}}\
         .grid{{display:grid;grid-template-columns:repeat(auto-fill,minmax(160px,1fr));gap:12px}}\
         .card{{display:block;text-decoration:none;color:#eee;background:#1b1b24;border:1px solid #2c2c38;border-radius:12px;padding:16px}}\
         .card:hover{{border-color:#9d4edd}}.name{{font-weight:600}}.sub{{color:#777;font-family:monospace;font-size:12px;margin-top:4px}}\
         .empty{{background:#1b1b24;border:1px solid #2c2c38;border-radius:12px;padding:24px;color:#bbb}}\
         pre{{background:#0d0d12;padding:12px;border-radius:8px;color:#9d4edd;overflow-x:auto}}\
         </style></head><body><div class=\"wrap\">\
         <h1>over<span>net</span></h1><p class=\"tag\">Каталог сети · search.ov</p>{body}\
         </div></body></html>"
    )
}

/// Узел-поисковик `search.ov`: кэширует каталог из bootstrap и отдаёт его как HTML.
pub async fn run_search(
    listener: TcpListenerLink,
    key: OnionKey,
    bootstrap_addr: String,
) -> Result<()> {
    let cache = Arc::new(std::sync::RwLock::new(crate::bootstrap::Directory { nodes: Vec::new() }));
    {
        let cache = cache.clone();
        let boot = bootstrap_addr.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(dir) = crate::bootstrap::fetch_directory(&boot).await {
                    if let Ok(mut g) = cache.write() {
                        *g = dir;
                    }
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
    }
    run_service(listener, key, move |req| {
        let q = req
            .path
            .split_once("q=")
            .map(|(_, v)| v.to_string())
            .unwrap_or_default();
        let dir = cache
            .read()
            .map(|g| g.clone())
            .unwrap_or(crate::bootstrap::Directory { nodes: Vec::new() });
        HttpResponse {
            status: 200,
            body: render_catalog(&dir, &q),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Полная вертикаль MVP: клиент → сервис → HTML обратно (онион в обе стороны).
    #[tokio::test]
    async fn mvp_client_fetches_page_from_service() {
        let key = OnionKey::generate();
        let service_pub = key.public();
        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        tokio::spawn(run_service(listener, key, |req| HttpResponse {
            status: 200,
            body: format!("<h1>overnet</h1><p>path={}</p>", req.path),
        }));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let service = NodeInfo {
            pubkey: hex_encode(&service_pub),
            address: addr,
            role: "service".into(),
            name: "search.ov".into(),
        };
        let resp = fetch_from(&service, "/hello").await.unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("path=/hello"));
    }

    /// Полная браузерная вертикаль: HTTP GET → шлюз → bootstrap → onion → сервис → HTML.
    #[tokio::test]
    async fn gateway_serves_overnet_page_over_http() {
        use crate::bootstrap::{register_node, BootstrapServer};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // bootstrap на эфемерном порту
        let boot_listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let boot_addr = boot_listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _ = BootstrapServer::new().serve_on(boot_listener).await;
        });

        // сервис
        let key = OnionKey::generate();
        let service_pub = key.public();
        let svc_listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let svc_addr = svc_listener.local_addr().unwrap().to_string();
        tokio::spawn(run_service(svc_listener, key, |req| HttpResponse {
            status: 200,
            body: format!("<h1>ov</h1>{}", req.path),
        }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        register_node(
            &boot_addr,
            NodeInfo {
                pubkey: hex_encode(&service_pub),
                address: svc_addr,
                role: "service".into(),
                name: "search.ov".into(),
            },
        )
        .await
        .unwrap();

        // gateway на эфемерном порту
        let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw_addr = gw_listener.local_addr().unwrap().to_string();
        tokio::spawn(run_gateway_on(gw_listener, boot_addr));
        tokio::time::sleep(Duration::from_millis(100)).await;

        // обычный HTTP-клиент (как делает браузер)
        let mut client = tokio::net::TcpStream::connect(&gw_addr).await.unwrap();
        client
            .write_all(b"GET /hello HTTP/1.1\r\nHost: search.ov\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            match client.read(&mut tmp).await {
                Ok(0) => break,
                Ok(n) => resp.extend_from_slice(&tmp[..n]),
                Err(_) => break,
            }
        }
        let text = String::from_utf8_lossy(&resp);
        assert!(text.contains("200"), "resp: {text}");
        assert!(text.contains("/hello"), "resp: {text}");
    }

    /// 2 хопа: client → relay → service, ответ обратно через релей.
    #[tokio::test]
    async fn relay_two_hop_delivers_with_reply() {
        use crate::bootstrap::{register_node, BootstrapServer};

        let boot_listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let boot_addr = boot_listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _ = BootstrapServer::new().serve_on(boot_listener).await;
        });

        // сервис
        let svc_key = OnionKey::generate();
        let svc_pub = svc_key.public();
        let svc_listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let svc_addr = svc_listener.local_addr().unwrap().to_string();
        tokio::spawn(run_service(svc_listener, svc_key, |req| HttpResponse {
            status: 200,
            body: format!("relayed:{}", req.path),
        }));
        register_node(
            &boot_addr,
            NodeInfo {
                pubkey: hex_encode(&svc_pub),
                address: svc_addr,
                role: "service".into(),
                name: "search.ov".into(),
            },
        )
        .await
        .unwrap();

        // релей (сам подключится к сервису из каталога)
        let relay_key = OnionKey::generate();
        let relay_pub = relay_key.public();
        let relay_listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_listener.local_addr().unwrap().to_string();
        let boot_for_relay = boot_addr.clone();
        tokio::spawn(async move {
            let _ = run_relay(relay_listener, relay_key, boot_for_relay).await;
        });
        tokio::time::sleep(Duration::from_millis(500)).await; // релей пре-коннектится к сервису

        // клиент: 2-хоповый запрос через релей
        let relay_info = NodeInfo {
            pubkey: hex_encode(&relay_pub),
            address: relay_addr,
            role: "relay".into(),
            name: String::new(),
        };
        let resp = fetch_via_relay(&relay_info, &hex_encode(&svc_pub), "/hello")
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("relayed:/hello"));
    }

    /// P2P-файлосервер: отдаёт свои файлы (бинарь) read-only.
    #[tokio::test]
    async fn fileserver_serves_own_files_readonly() {
        let dir = std::env::temp_dir().join(format!("ovserve_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pic.bin"), [0u8, 1, 2, 3, 255, 254]).unwrap();

        let key = OnionKey::generate();
        let pubk = key.public();
        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(run_fileserver(listener, key, dir.to_string_lossy().to_string()));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let svc = NodeInfo {
            pubkey: hex_encode(&pubk),
            address: addr,
            role: "service".into(),
            name: String::new(),
        };
        let got = download_file(&svc, "pic.bin").await.unwrap();
        assert_eq!(got, vec![0u8, 1, 2, 3, 255, 254]);
        assert!(download_file(&svc, "missing").await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// NAT-traversal: сервис «за NAT» подключается исходяще к релею; клиент достаёт.
    #[tokio::test]
    async fn nat_traversal_service_via_relay() {
        // Достижимый релей.
        let relay_key = OnionKey::generate();
        let relay_pub = relay_key.public();
        let relay_listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_listener.local_addr().unwrap().to_string();
        let (tx, _rx) = mpsc::unbounded_channel();
        let router = std::sync::Arc::new(Router::new(relay_key, tx));
        {
            let r = router.clone();
            tokio::spawn(async move {
                let _ = r.serve(relay_listener).await;
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Сервис «за NAT»: сам коннектится к релею и обслуживает.
        let svc_key = OnionKey::generate();
        let svc_pub = svc_key.public();
        let relay_addr2 = relay_addr.clone();
        tokio::spawn(async move {
            let _ = serve_via_relay(&relay_addr2, svc_key, |req| HttpResponse {
                status: 200,
                body: format!("nat:{}", req.path),
            })
            .await;
        });
        tokio::time::sleep(Duration::from_millis(200)).await; // сервис зарегался у релея

        // Клиент: через релей (2 хопа) к сервису за NAT.
        let relay_info = NodeInfo {
            pubkey: hex_encode(&relay_pub),
            address: relay_addr,
            role: "relay".into(),
            name: String::new(),
        };
        let resp = fetch_via_relay(&relay_info, &hex_encode(&svc_pub), "/hi")
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("nat:/hi"));
    }

    #[tokio::test]
    async fn site_serves_files_from_dir() {
        let dir = std::env::temp_dir().join(format!("ovsite_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<h1>hi from site</h1>").unwrap();

        let key = OnionKey::generate();
        let svc_pub = key.public();
        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(run_site(listener, key, dir.to_string_lossy().to_string()));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let service = NodeInfo {
            pubkey: hex_encode(&svc_pub),
            address: addr,
            role: "service".into(),
            name: "x.ov".into(),
        };
        let home = fetch_from(&service, "/").await.unwrap();
        assert!(home.body.contains("hi from site"));
        let nf = fetch_from(&service, "/nope").await.unwrap();
        assert_eq!(nf.status, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn domain_name_ownership_first_come() {
        use crate::bootstrap::{register_node, BootstrapServer};
        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _ = BootstrapServer::new().serve_on(listener).await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let a = NodeInfo { pubkey: "a".repeat(64), address: "1.1.1.1:1".into(), role: "service".into(), name: "shop.ov".into() };
        let b = NodeInfo { pubkey: "b".repeat(64), address: "2.2.2.2:2".into(), role: "service".into(), name: "shop.ov".into() };
        // первый владелец — ок
        register_node(&addr, a).await.unwrap();
        // чужой ключ на занятое имя — отказ
        assert!(register_node(&addr, b).await.is_err());
        // тот же владелец переригистрируется (новый адрес) — ок
        let a2 = NodeInfo { pubkey: "a".repeat(64), address: "1.1.1.1:9".into(), role: "service".into(), name: "shop.ov".into() };
        register_node(&addr, a2).await.unwrap();
    }

    #[tokio::test]
    async fn filestore_upload_download_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ovfiles_{}", std::process::id()));
        let key = OnionKey::generate();
        let svc_pub = key.public();
        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(run_filestore(listener, key, dir.to_string_lossy().to_string()));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let service = NodeInfo {
            pubkey: hex_encode(&svc_pub),
            address: addr,
            role: "service".into(),
            name: "files.ov".into(),
        };
        let data = b"hello \x00\x01\x02 binary bytes";
        let id = upload_file(&service, data).await.unwrap();
        let got = download_file(&service, &id).await.unwrap();
        assert_eq!(got, data);
        // несуществующий id → ошибка
        assert!(download_file(&service, "deadbeef").await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_lists_and_filters_services() {
        use crate::bootstrap::Directory;
        let dir = Directory {
            nodes: vec![
                NodeInfo { pubkey: "a".repeat(64), address: "x".into(), role: "service".into(), name: "news.ov".into() },
                NodeInfo { pubkey: "b".repeat(64), address: "x".into(), role: "service".into(), name: "shop.ov".into() },
                NodeInfo { pubkey: "c".repeat(64), address: "x".into(), role: "relay".into(), name: String::new() },
            ],
        };
        let all = render_catalog(&dir, "");
        assert!(all.contains("news.ov") && all.contains("shop.ov"));
        let filtered = render_catalog(&dir, "news");
        assert!(filtered.contains("news.ov") && !filtered.contains("shop.ov"));
    }
}
