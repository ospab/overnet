//! Локальный SOCKS5-шлюз: через него в overnet ходит браузер и ostp-сервер
//! (раздел `overnet.gateway` в его конфиге).
//!
//! `*.ov` — к сервису через цепи. Остальное — по настройке `clearnet`:
//! `direct` (как без шлюза), `exit` (через выход overnet) или `block`.
//! Имена браузер должен отдавать шлюзу (socks5h / remote DNS), иначе `.ov`
//! утечёт в системный DNS — профиль браузера это включает.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use overnet_core::{Error, Result};

use super::client::Client;
use super::exit::is_ov;
use super::names::Names;
use super::stream;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clearnet {
    Direct,
    Exit,
    Block,
}

impl Clearnet {
    pub fn parse(s: &str) -> Result<Clearnet> {
        match s {
            "direct" => Ok(Clearnet::Direct),
            "exit" => Ok(Clearnet::Exit),
            "block" => Ok(Clearnet::Block),
            o => Err(Error::Link(format!("clearnet: '{o}' is not direct, exit or block"))),
        }
    }
}

pub struct Gateway {
    pub client: Arc<Client>,
    pub names: Arc<Names>,
    pub clearnet: Clearnet,
}

impl Gateway {
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        loop {
            let (tcp, _) = listener.accept().await.map_err(|e| Error::Link(format!("gateway: {e}")))?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle(tcp).await;
            });
        }
    }

    async fn handle(&self, mut tcp: TcpStream) -> Result<()> {
        let io = |e: std::io::Error| Error::Link(e.to_string());
        let mut head = [0u8; 2];
        tcp.read_exact(&mut head).await.map_err(io)?;
        if head[0] != 5 {
            return Err(Error::Link("not SOCKS5".into()));
        }
        let mut methods = vec![0u8; head[1] as usize];
        tcp.read_exact(&mut methods).await.map_err(io)?;
        if !methods.contains(&0) {
            tcp.write_all(&[5, 0xff]).await.map_err(io)?;
            return Ok(());
        }
        tcp.write_all(&[5, 0]).await.map_err(io)?;
        let mut req = [0u8; 4];
        tcp.read_exact(&mut req).await.map_err(io)?;
        if req[1] != 1 {
            return reply(&mut tcp, 7).await; // только CONNECT
        }
        let host = match req[3] {
            1 => {
                let mut b = [0u8; 4];
                tcp.read_exact(&mut b).await.map_err(io)?;
                Ipv4Addr::from(b).to_string()
            }
            4 => {
                let mut b = [0u8; 16];
                tcp.read_exact(&mut b).await.map_err(io)?;
                format!("[{}]", Ipv6Addr::from(b))
            }
            3 => {
                let mut l = [0u8; 1];
                tcp.read_exact(&mut l).await.map_err(io)?;
                let mut b = vec![0u8; l[0] as usize];
                tcp.read_exact(&mut b).await.map_err(io)?;
                String::from_utf8(b).map_err(|_| Error::Link("bad host".into()))?
            }
            _ => return reply(&mut tcp, 8).await,
        };
        let mut p = [0u8; 2];
        tcp.read_exact(&mut p).await.map_err(io)?;
        let port = u16::from_be_bytes(p);

        if is_ov(&host) {
            let res = async {
                let id = self.names.resolve(&host).await?;
                self.client.connect_service(id, port).await
            }
            .await;
            return match res {
                Ok(s) => {
                    reply(&mut tcp, 0).await?;
                    stream::pump(s, tcp).await;
                    Ok(())
                }
                Err(e) if port == 80 => error_page(tcp, &host, &e.to_string()).await,
                Err(_) => reply(&mut tcp, 4).await, // host unreachable
            };
        }

        // Фиктивные адреса ostp (198.18/15) сюда приходить не должны: значит, DNS
        // браузера идёт мимо шлюза. Не пускаем такое в интернет.
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            if matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18) {
                return reply(&mut tcp, 2).await;
            }
        }
        let target = format!("{host}:{port}");
        match self.clearnet {
            Clearnet::Block => reply(&mut tcp, 2).await,
            Clearnet::Direct => match TcpStream::connect(&target).await {
                Ok(mut up) => {
                    reply(&mut tcp, 0).await?;
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut up).await;
                    Ok(())
                }
                Err(_) => reply(&mut tcp, 5).await,
            },
            Clearnet::Exit => match self.client.connect_exit(&target).await {
                Ok(s) => {
                    reply(&mut tcp, 0).await?;
                    stream::pump(s, tcp).await;
                    Ok(())
                }
                Err(_) => reply(&mut tcp, 5).await,
            },
        }
    }
}

async fn reply(tcp: &mut TcpStream, code: u8) -> Result<()> {
    tcp.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await.map_err(|e| Error::Link(e.to_string()))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Вместо безликой «прокси отказал» — страница с причиной (для http://*.ov).
async fn error_page(mut tcp: TcpStream, host: &str, why: &str) -> Result<()> {
    reply(&mut tcp, 0).await?;
    let mut req = [0u8; 4096];
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), tcp.read(&mut req)).await;
    let body = format!(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\"><title>{h} недоступен</title>\
<style>body{{background:#0b0b10;color:#ececf1;font-family:system-ui,sans-serif;max-width:640px;margin:12vh auto;padding:0 24px}}\
h1{{font-size:26px}}h1 span{{color:#9d6bff}}code{{color:#b88bff;word-break:break-all}}p{{color:#8a8a99}}</style></head>\
<body><h1><span>overnet</span>: {h} недоступен</h1><p>Причина:</p><code>{w}</code>\
<p>Сервис может быть выключен, или сеть ещё не загрузила каталог. Попробуйте через минуту.</p></body></html>",
        h = escape(host),
        w = escape(why)
    );
    let resp = format!(
        "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tcp.write_all(resp.as_bytes()).await;
    Ok(())
}
