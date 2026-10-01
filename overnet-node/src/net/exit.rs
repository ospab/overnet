//! Выход в обычный интернет. Выключен по умолчанию: всё, что выходит через
//! exit, приписывается его адресу.
//!
//! - `direct` — соединяться самому; сам узел, частные и служебные сети закрыты.
//! - `socks5://127.0.0.1:9151` — через выход ostp-сервера (раздел `overnet.exit`
//!   в его конфиге): тогда работают его правила, upstream-прокси и блок-листы.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use overnet_core::{Error, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitPolicy {
    Off,
    Direct { allow_private: bool },
    Socks5(String),
}

fn err(s: impl Into<String>) -> Error {
    Error::Link(s.into())
}

impl ExitPolicy {
    /// "off" | "direct" | "socks5://host:port"
    pub fn parse(s: &str) -> Result<ExitPolicy> {
        match s.trim() {
            "" | "off" => Ok(ExitPolicy::Off),
            "direct" => Ok(ExitPolicy::Direct { allow_private: false }),
            other => match other.strip_prefix("socks5://") {
                Some(addr) if addr.contains(':') => Ok(ExitPolicy::Socks5(addr.to_string())),
                _ => Err(err(format!("exit: '{other}' is not off, direct or socks5://host:port"))),
            },
        }
    }

    pub fn enabled(&self) -> bool {
        !matches!(self, ExitPolicy::Off)
    }

    /// Соединиться с `host:port` по политике выхода.
    pub async fn connect(&self, target: &str) -> Result<TcpStream> {
        let (host, port) = split(target).ok_or_else(|| err("bad target"))?;
        if is_ov(host) {
            return Err(err(".ov is not reached through an exit"));
        }
        match self {
            ExitPolicy::Off => Err(err("exit disabled")),
            ExitPolicy::Direct { allow_private } => {
                let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
                    .await
                    .map_err(|e| err(format!("{host}: {e}")))?
                    .collect();
                if addrs.is_empty() {
                    return Err(err(format!("{host}: no addresses")));
                }
                if !allow_private {
                    if let Some(bad) = addrs.iter().find(|a| !is_public(a.ip())) {
                        return Err(err(format!("{host}: {} is not a public address", bad.ip())));
                    }
                }
                let mut last = err("unreachable");
                for a in addrs.iter().filter(|a| a.is_ipv4()).chain(addrs.iter().filter(|a| a.is_ipv6())) {
                    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(a)).await {
                        Ok(Ok(s)) => return Ok(s),
                        Ok(Err(e)) => last = err(format!("{a}: {e}")),
                        Err(_) => last = err(format!("{a}: timeout")),
                    }
                }
                Err(last)
            }
            ExitPolicy::Socks5(proxy) => socks5_connect(proxy, host, port).await,
        }
    }
}

pub fn is_ov(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "ov" || h.ends_with(".ov")
}

fn split(target: &str) -> Option<(&str, u16)> {
    let (h, p) = target.rsplit_once(':')?;
    Some((h.trim_matches(['[', ']']), p.parse().ok()?))
}

/// Публичный ли адрес (выход не должен вести в сам узел и частные сети).
pub fn is_public(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// CONNECT через SOCKS5 без аутентификации; имя передаётся прокси как есть.
pub async fn socks5_connect(proxy: &str, host: &str, port: u16) -> Result<TcpStream> {
    let io = |e: std::io::Error| err(format!("socks5 {proxy}: {e}"));
    let mut s = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(proxy))
        .await
        .map_err(|_| err(format!("socks5 {proxy}: timeout")))?
        .map_err(io)?;
    s.write_all(&[5, 1, 0]).await.map_err(io)?;
    let mut g = [0u8; 2];
    s.read_exact(&mut g).await.map_err(io)?;
    if g != [5, 0] {
        return Err(err(format!("socks5 {proxy}: no-auth refused")));
    }
    let mut req = vec![5, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            req.push(1);
            req.extend_from_slice(&v4.octets());
        }
        Ok(IpAddr::V6(v6)) => {
            req.push(4);
            req.extend_from_slice(&v6.octets());
        }
        Err(_) => {
            let h = host.as_bytes();
            if h.len() > 255 {
                return Err(err("host name too long"));
            }
            req.push(3);
            req.push(h.len() as u8);
            req.extend_from_slice(h);
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await.map_err(io)?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await.map_err(io)?;
    if head[1] != 0 {
        return Err(err(format!("socks5 {proxy}: {host}:{port} refused (code {})", head[1])));
    }
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await.map_err(io)?;
            l[0] as usize
        }
        _ => return Err(err("socks5: bad reply")),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest).await.map_err(io)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_policies() {
        assert_eq!(ExitPolicy::parse("off").unwrap(), ExitPolicy::Off);
        assert_eq!(ExitPolicy::parse("direct").unwrap(), ExitPolicy::Direct { allow_private: false });
        assert_eq!(
            ExitPolicy::parse("socks5://127.0.0.1:9151").unwrap(),
            ExitPolicy::Socks5("127.0.0.1:9151".into())
        );
        assert!(ExitPolicy::parse("yes").is_err());
    }

    #[tokio::test]
    async fn direct_exit_refuses_private_and_ov() {
        let p = ExitPolicy::Direct { allow_private: false };
        assert!(p.connect("127.0.0.1:80").await.is_err());
        assert!(p.connect("192.168.0.1:80").await.is_err());
        assert!(p.connect("search.ov:80").await.is_err());
        assert!(ExitPolicy::Off.connect("1.1.1.1:80").await.is_err());
    }

    #[test]
    fn public_addresses() {
        for ip in ["127.0.0.1", "10.1.0.1", "100.64.1.1", "198.18.0.1", "fd00::1", "::ffff:10.0.0.1"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }
}
