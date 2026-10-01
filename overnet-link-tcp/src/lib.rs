//! `overnet-link-tcp` — реализация `Link` поверх TCP (length-framed).
//!
//! Транспорт для разработки и для фазы 1 поверх интернета. Каждый кадр —
//! префикс длины (u32, big-endian) + тело. Тот же `Link`, что и loopback, —
//! значит протокол/сессия/onion поверх него не меняются (transport-agnostic).

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use overnet_core::{Error, Link, LinkProps, Result};

/// Защита от абсурдных длин кадров (16 МиБ).
const MAX_FRAME: usize = 16 * 1024 * 1024;

fn io_err(e: std::io::Error) -> Error {
    Error::Link(format!("tcp: {e}"))
}

// ── Knock: первый кадр соединения — доказательство знания секрета сети ──────────
// Секрет не задан → knock выключен (открытый режим). Неверный/пустой knock → узел
// молча роняет соединение (порт «глухой» для DPI/сканеров).

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const KNOCK_LEN: usize = 16;
const KNOCK_WINDOW_SECS: u64 = 30;
const KNOCK_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Секрет сети из `config.json` (поле `token`) или env `OVERNET_TOKEN`.
/// Пусто = knock выключен. Читается один раз и кэшируется.
fn network_secret() -> &'static str {
    static SECRET: OnceLock<String> = OnceLock::new();
    SECRET.get_or_init(|| {
        if let Ok(content) = std::fs::read_to_string("config.json") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(t) = v.get("token").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        return t.to_string();
                    }
                }
            }
        }
        std::env::var("OVERNET_TOKEN").unwrap_or_default()
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// knock = HMAC-SHA256(secret, time_bucket)[..KNOCK_LEN].
fn knock_for(secret: &[u8], bucket: u64) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&bucket.to_be_bytes());
    mac.finalize().into_bytes()[..KNOCK_LEN].to_vec()
}

/// Проверить knock против текущего окна ±1 (допуск рассинхрона часов).
fn knock_valid(secret: &[u8], received: &[u8]) -> bool {
    if received.len() != KNOCK_LEN {
        return false;
    }
    let now = now_unix() / KNOCK_WINDOW_SECS;
    [now.wrapping_sub(1), now, now + 1]
        .iter()
        .any(|&bucket| knock_for(secret, bucket) == received)
}

/// `Link` поверх одного TCP-соединения.
pub struct TcpLink {
    read: Mutex<OwnedReadHalf>,
    write: Mutex<OwnedWriteHalf>,
}

impl TcpLink {
    /// Подключиться к узлу (клиентская сторона).
    pub async fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).await.map_err(io_err)?;
        stream.set_nodelay(true).ok();
        let link = Self::from_stream(stream);
        // Knock первым кадром (если секрет сети задан).
        let secret = network_secret();
        if !secret.is_empty() {
            let knock = knock_for(secret.as_bytes(), now_unix() / KNOCK_WINDOW_SECS);
            link.send(&knock).await?;
        }
        Ok(link)
    }

    /// Обернуть готовый поток (например, принятый сервером).
    pub fn from_stream(stream: TcpStream) -> Self {
        let (r, w) = stream.into_split();
        TcpLink { read: Mutex::new(r), write: Mutex::new(w) }
    }

    pub async fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.read.lock().await.peer_addr()
    }
}

#[async_trait]
impl Link for TcpLink {
    async fn send(&self, frame: &[u8]) -> Result<()> {
        if frame.len() > MAX_FRAME {
            return Err(Error::Link("frame too large".into()));
        }
        let mut w = self.write.lock().await;
        w.write_all(&(frame.len() as u32).to_be_bytes()).await.map_err(io_err)?;
        w.write_all(frame).await.map_err(io_err)?;
        w.flush().await.map_err(io_err)?;
        Ok(())
    }

    async fn recv(&self) -> Result<Vec<u8>> {
        let mut r = self.read.lock().await;
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).await.map_err(io_err)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME {
            return Err(Error::Link("frame too large".into()));
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await.map_err(io_err)?;
        Ok(buf)
    }

    fn mtu(&self) -> usize {
        MAX_FRAME
    }

    fn properties(&self) -> LinkProps {
        LinkProps { latency_ms: 0, bandwidth_bps: 0, reliable: true, directional: false }
    }
}

/// Слушающий сокет, отдающий принятые соединения как `TcpLink`.
pub struct TcpListenerLink {
    listener: TcpListener,
}

impl TcpListenerLink {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr).await.map_err(io_err)?;
        Ok(TcpListenerLink { listener })
    }

    pub async fn accept(&self) -> Result<TcpLink> {
        let secret = network_secret();
        loop {
            let (stream, _peer) = self.listener.accept().await.map_err(io_err)?;
            stream.set_nodelay(true).ok();
            let link = TcpLink::from_stream(stream);
            if secret.is_empty() {
                return Ok(link); // открытый режим — без knock
            }
            // Требуем валидный knock первым кадром; иначе молча роняем (анти-проба).
            match tokio::time::timeout(KNOCK_READ_TIMEOUT, link.recv()).await {
                Ok(Ok(frame)) if knock_valid(secret.as_bytes(), &frame) => return Ok(link),
                _ => continue,
            }
        }
    }

    pub fn local_addr(&self) -> Result<std::net::SocketAddr> {
        self.listener.local_addr().map_err(io_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tcp_frame_roundtrips_both_ways() {
        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server = tokio::spawn(async move {
            let link = listener.accept().await.unwrap();
            let m = link.recv().await.unwrap();
            link.send(&m).await.unwrap(); // эхо обратно
        });

        let client = TcpLink::connect(&addr).await.unwrap();
        client.send(b"abc").await.unwrap();
        assert_eq!(client.recv().await.unwrap(), b"abc");
        server.await.unwrap();
    }

    #[test]
    fn knock_roundtrip_and_skew() {
        let secret = b"net-secret";
        let bucket = now_unix() / KNOCK_WINDOW_SECS;
        let k = knock_for(secret, bucket);
        assert!(knock_valid(secret, &k)); // текущее окно
        assert!(knock_valid(secret, &knock_for(secret, bucket - 1))); // соседнее окно
        assert!(!knock_valid(b"wrong-secret", &k)); // другой секрет
        assert!(!knock_valid(secret, b"short")); // мусор
    }
}
