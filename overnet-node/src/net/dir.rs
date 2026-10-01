//! Каталог релеев и выбор путей.
//!
//! Bootstrap-сервер знает только релеи (они публичны по своей природе). Релеи
//! держат копию каталога и раздают её по BEGIN_DIR. Клиент и сервис спрашивают
//! каталог у релея через цепь — с bootstrap-сервером они не общаются вовсе.

use std::sync::Arc;
use std::time::Duration;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use overnet_core::cell::relay;
use overnet_core::ovaddr::ServiceId;
use overnet_core::{Error, Result};

use super::Node;
use crate::bootstrap::{fetch_directory, Directory};

/// Запись о релее.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RelayDesc {
    /// hex X25519 onion-ключа.
    pub pubkey: String,
    /// host:port
    pub address: String,
    /// Выпускает в обычный интернет.
    #[serde(default)]
    pub exit: bool,
}

impl RelayDesc {
    pub fn key(&self) -> Option<[u8; 32]> {
        let b = hex::decode(&self.pubkey).ok()?;
        b.try_into().ok()
    }

    /// Разобрать `pubkeyhex@host:port` (так релеи пишутся в конфиге клиента).
    pub fn parse(s: &str) -> Option<RelayDesc> {
        let (pk, addr) = s.split_once('@')?;
        let d = RelayDesc { pubkey: pk.to_ascii_lowercase(), address: addr.to_string(), exit: false };
        d.key().map(|_| d)
    }
}

/// Релеи из каталога bootstrap-сервера (сервисы там больше не регистрируются).
pub fn from_bootstrap(d: &Directory) -> Vec<RelayDesc> {
    d.nodes
        .iter()
        .filter(|n| n.role == "relay" || n.role == "exit")
        .map(|n| RelayDesc { pubkey: n.pubkey.clone(), address: n.address.clone(), exit: n.role == "exit" })
        .filter(|d| d.key().is_some())
        .collect()
}

/// Релей: держать каталог свежим, забирая его у bootstrap-сервера.
pub async fn keep_fresh_from_bootstrap(node: Arc<Node>, bootstrap: String, every: Duration) {
    loop {
        if let Ok(d) = fetch_directory(&bootstrap).await {
            let relays = from_bootstrap(&d);
            if !relays.is_empty() {
                node.set_directory(relays);
            }
        }
        tokio::time::sleep(every).await;
    }
}

/// Скачать каталог у релея `via` по одноходовой цепи.
pub async fn fetch_via(node: &Arc<Node>, via: &RelayDesc) -> Result<Vec<RelayDesc>> {
    let circ = node.build_circuit(std::slice::from_ref(via)).await?;
    let res = async {
        let mut s = circ.open_stream(0, relay::BEGIN_DIR, "").await?;
        let body = s.read_to_end(4 << 20).await?;
        serde_json::from_slice::<Vec<RelayDesc>>(&body).map_err(|_| Error::Link("bad directory".into()))
    }
    .await;
    circ.destroy().await;
    let relays: Vec<RelayDesc> = res?.into_iter().filter(|d| d.key().is_some()).collect();
    if relays.is_empty() {
        return Err(Error::Link("empty directory".into()));
    }
    Ok(relays)
}

/// Точки входа сервиса: `n` релеев, ближайших к нему по хешу. Клиент и сервис
/// вычисляют их одинаково из одного каталога — публиковать ничего не нужно.
pub fn intro_points(id: &ServiceId, relays: &[RelayDesc], n: usize) -> Vec<RelayDesc> {
    let mut scored: Vec<([u8; 32], &RelayDesc)> = relays
        .iter()
        .map(|r| {
            let mut h = Sha256::new();
            h.update(b"overnet-intro-v1");
            h.update(id.0);
            h.update(r.pubkey.as_bytes());
            (h.finalize().into(), r)
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0));
    scored.into_iter().take(n).map(|(_, r)| r.clone()).collect()
}

/// Путь до `last`: до `extra` случайных релеев перед ним, без повторов.
/// В маленькой сети путь короче — это честно хуже для анонимности.
pub fn path_to(last: &RelayDesc, relays: &[RelayDesc], extra: usize, first: Option<&RelayDesc>) -> Vec<RelayDesc> {
    let mut rng = rand::thread_rng();
    let mut path = Vec::new();
    if let Some(g) = first {
        if g.pubkey != last.pubkey {
            path.push(g.clone());
        }
    }
    let mut pool: Vec<&RelayDesc> = relays
        .iter()
        .filter(|r| r.pubkey != last.pubkey && path.iter().all(|p: &RelayDesc| p.pubkey != r.pubkey))
        .collect();
    pool.shuffle(&mut rng);
    for r in pool {
        if path.len() >= extra {
            break;
        }
        path.push(r.clone());
    }
    path.push(last.clone());
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use overnet_core::ovaddr::ServiceKey;

    fn relays(n: usize) -> Vec<RelayDesc> {
        (0..n)
            .map(|i| RelayDesc { pubkey: format!("{:064x}", i + 1), address: format!("10.0.0.{i}:1"), exit: false })
            .collect()
    }

    #[test]
    fn intro_points_are_stable_and_order_independent() {
        let id = ServiceKey::generate().id();
        let mut r = relays(10);
        let a = intro_points(&id, &r, 2);
        r.reverse();
        assert_eq!(a, intro_points(&id, &r, 2));
        assert_eq!(a.len(), 2);
        assert_ne!(a, intro_points(&ServiceKey::generate().id(), &r, 2));
    }

    #[test]
    fn paths_have_no_repeats_and_end_at_target() {
        let r = relays(5);
        for _ in 0..50 {
            let p = path_to(&r[3], &r, 2, Some(&r[0]));
            assert_eq!(p.len(), 3);
            assert_eq!(p[0], r[0]);
            assert_eq!(p[2], r[3]);
            assert_ne!(p[1], r[0]);
            assert_ne!(p[1], r[3]);
        }
        // Сеть из одного релея: путь из одного хопа.
        assert_eq!(path_to(&r[0], &r[..1], 2, Some(&r[0])).len(), 1);
    }

    #[test]
    fn relay_from_config_string() {
        let d = RelayDesc::parse(&format!("{}@1.2.3.4:4040", "ab".repeat(32))).unwrap();
        assert_eq!(d.address, "1.2.3.4:4040");
        assert!(RelayDesc::parse("zz@1.2.3.4:1").is_none());
    }
}
