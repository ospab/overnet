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

use crate::bootstrap::{fetch_directory, NodeInfo};
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
pub async fn fetch_from(service: &NodeInfo, path: &str) -> Result<HttpResponse> {
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
        method: "GET".into(),
        path: path.to_string(),
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

        register_node(&boot_addr, NodeInfo {
            pubkey: hex_encode(&service_pub),
            address: addr,
            role: "service".into(),
        }).await.unwrap();

        let resp = fetch_service(&boot_addr, String::new(), &hex_encode(&service_pub), "/hello").await.unwrap();
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
}
