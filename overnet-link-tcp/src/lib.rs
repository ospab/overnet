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
        Ok(Self::from_stream(stream))
    }

    /// Обернуть готовый поток (например, принятый сервером).
    pub fn from_stream(stream: TcpStream) -> Self {
        let (r, w) = stream.into_split();
        TcpLink { read: Mutex::new(r), write: Mutex::new(w) }
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
        let (stream, _peer) = self.listener.accept().await.map_err(io_err)?;
        stream.set_nodelay(true).ok();
        Ok(TcpLink::from_stream(stream))
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
}
