use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use overnet_core::{Error, Link, Result};
use overnet_link_tcp::{TcpLink, TcpListenerLink};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeInfo {
    pub pubkey: String,
    pub address: String,
    pub role: String, // "guard", "relay", "service"
    /// Человекочитаемое имя сервиса (напр. "search.ov"). Пусто для relay.
    #[serde(default)]
    pub name: String,
}

/// Запись каталога с временем последней регистрации — для TTL и федерации.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeEntry {
    pub info: NodeInfo,
    /// Unix-секунды последней регистрации (или последнего sync с более свежим временем).
    pub last_seen: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Directory {
    pub nodes: Vec<NodeInfo>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum BootstrapMessage {
    Register { info: NodeInfo, token: String },
    GetDirectory { token: String },
    Directory(Directory),
    Ack,
    /// BS↔BS федерация: один сервер шлёт другому свои записи (анти-энтропия).
    Sync { entries: Vec<NodeEntry>, token: String },
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Свежа ли запись. `ttl_secs == 0` → истечение выключено (запись всегда свежа).
fn fresh(e: &NodeEntry, ttl_secs: u64) -> bool {
    ttl_secs == 0 || now_unix().saturating_sub(e.last_seen) <= ttl_secs
}

/// Конфиг overnet из `config.json` (рядом с бинарём). Все поля опциональны.
/// Приоритет: config.json → env-фолбэк → дефолт.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    /// Общий токен сети. Пусто = открыто.
    token: Option<String>,
    /// Адреса других BS для федерации, напр. ["1.2.3.4:8080"].
    bs_peers: Vec<String>,
    /// TTL записей каталога, сек. 0/не задан = без истечения.
    node_ttl_secs: Option<u64>,
}

fn read_config() -> Config {
    std::fs::read_to_string("config.json")
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

pub fn load_token() -> String {
    if let Some(t) = read_config().token {
        return t;
    }
    std::env::var("OVERNET_TOKEN").unwrap_or_default()
}

/// Адреса других BS для федерации. Пусто = одиночный BS.
fn load_peers() -> Vec<String> {
    let cfg = read_config();
    if !cfg.bs_peers.is_empty() {
        return cfg.bs_peers;
    }
    std::env::var("OVERNET_BS_PEERS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// TTL записей каталога в секундах. 0/не задан = без истечения.
fn load_ttl() -> u64 {
    if let Some(t) = read_config().node_ttl_secs {
        return t;
    }
    std::env::var("OVERNET_NODE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct BootstrapServer {
    nodes: Arc<RwLock<HashMap<String, NodeEntry>>>,
    /// Владение именами: .ov-имя → pubkey-владелец (first-come).
    names: Arc<RwLock<HashMap<String, String>>>,
    /// Общий токен. Пусто = открыто; задан = чужие дропаются.
    token: String,
    /// Другие BS для синхронизации каталога.
    peers: Vec<String>,
    /// TTL записей; 0 = без истечения.
    ttl_secs: u64,
}

impl Default for BootstrapServer {
    fn default() -> Self {
        Self::new()
    }
}

impl BootstrapServer {
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
            names: Arc::new(RwLock::new(HashMap::new())),
            token: load_token(),
            peers: load_peers(),
            ttl_secs: load_ttl(),
        }
    }

    pub async fn serve(&self, bind_addr: &str) -> Result<()> {
        let listener = TcpListenerLink::bind(bind_addr).await?;
        println!(
            "Bootstrap server on {} | peers={:?} | ttl={}s",
            bind_addr, self.peers, self.ttl_secs
        );
        self.serve_on(listener).await
    }

    /// Отдать свои свежие записи одному пиру-BS (используется фоном и в тестах).
    pub async fn sync_to(&self, peer: &str) -> Result<()> {
        push_sync(&self.nodes, &self.token, self.ttl_secs, peer).await
    }

    /// Обслуживать каталог на уже привязанном listener (для тестов / открытого порта).
    pub async fn serve_on(&self, listener: TcpListenerLink) -> Result<()> {
        // Фон: периодически рассылаем свой каталог пирам-BS (анти-энтропия).
        if !self.peers.is_empty() {
            let nodes = self.nodes.clone();
            let token = self.token.clone();
            let peers = self.peers.clone();
            let ttl = self.ttl_secs;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    for peer in &peers {
                        let _ = push_sync(&nodes, &token, ttl, peer).await;
                    }
                }
            });
        }

        loop {
            let link = listener.accept().await?;
            let nodes = self.nodes.clone();
            let names_map = self.names.clone();
            let expected = self.token.clone();
            let ttl = self.ttl_secs;

            tokio::spawn(async move {
                if let Ok(frame) = link.recv().await {
                    if let Ok(msg) = serde_json::from_slice::<BootstrapMessage>(&frame) {
                        match msg {
                            BootstrapMessage::Register { mut info, token } => {
                                if !expected.is_empty() && token != expected {
                                    return;
                                }
                                // Владение именем: .ov принадлежит первому ключу (first-come).
                                // Чужой ключ на занятое имя → молча роняем (клиент получит ошибку).
                                if !info.name.is_empty() {
                                    let mut nm = names_map.write().await;
                                    match nm.get(&info.name) {
                                        Some(owner) if owner != &info.pubkey => {
                                            eprintln!(
                                                "name '{}' already owned by another key — rejected",
                                                info.name
                                            );
                                            return;
                                        }
                                        _ => {
                                            nm.insert(info.name.clone(), info.pubkey.clone());
                                        }
                                    }
                                }
                                // Нода прислала 0.0.0.0 — подставим её реальный внешний IP.
                                if info.address.starts_with("0.0.0.0:") {
                                    if let Ok(peer) = link.peer_addr().await {
                                        let port = info.address.split(':').last().unwrap_or("4040");
                                        info.address = format!("{}:{}", peer.ip(), port);
                                    }
                                }
                                println!(
                                    "Registered: {} ({}) name='{}' at {}",
                                    info.pubkey, info.role, info.name, info.address
                                );
                                let pubkey = info.pubkey.clone();
                                nodes.write().await.insert(
                                    pubkey,
                                    NodeEntry { info, last_seen: now_unix() },
                                );
                                let ack = serde_json::to_vec(&BootstrapMessage::Ack).unwrap();
                                let _ = link.send(&ack).await;
                            }
                            BootstrapMessage::GetDirectory { token } => {
                                if !expected.is_empty() && token != expected {
                                    return;
                                }
                                let dir = Directory {
                                    nodes: nodes
                                        .read()
                                        .await
                                        .values()
                                        .filter(|e| fresh(e, ttl))
                                        .map(|e| e.info.clone())
                                        .collect(),
                                };
                                let reply =
                                    serde_json::to_vec(&BootstrapMessage::Directory(dir)).unwrap();
                                let _ = link.send(&reply).await;
                            }
                            BootstrapMessage::Sync { entries, token } => {
                                if !expected.is_empty() && token != expected {
                                    return;
                                }
                                // Анти-энтропия: оставляем запись с более свежим last_seen.
                                // Мёртвые ноды перестают «молодеть» везде → истекают по TTL,
                                // без взаимного воскрешения.
                                let mut g = nodes.write().await;
                                for e in entries {
                                    let newer = match g.get(&e.info.pubkey) {
                                        Some(local) => e.last_seen > local.last_seen,
                                        None => true,
                                    };
                                    if newer {
                                        g.insert(e.info.pubkey.clone(), e);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            });
        }
    }
}

/// Послать свои свежие записи одному пиру-BS.
async fn push_sync(
    nodes: &Arc<RwLock<HashMap<String, NodeEntry>>>,
    token: &str,
    ttl: u64,
    peer: &str,
) -> Result<()> {
    let entries: Vec<NodeEntry> = {
        let g = nodes.read().await;
        g.values().filter(|e| fresh(e, ttl)).cloned().collect()
    };
    if entries.is_empty() {
        return Ok(());
    }
    let msg = BootstrapMessage::Sync {
        entries,
        token: token.to_string(),
    };
    let frame = serde_json::to_vec(&msg).map_err(|_| Error::Link("JSON error".into()))?;
    let link = TcpLink::connect(peer).await?;
    link.send(&frame).await
}

pub async fn register_node(bootstrap_addr: &str, info: NodeInfo) -> Result<()> {
    let link = TcpLink::connect(bootstrap_addr).await?;
    let token = load_token();
    let msg = BootstrapMessage::Register { info, token };
    let frame = serde_json::to_vec(&msg).map_err(|_| Error::Link("JSON error".into()))?;
    link.send(&frame).await?;

    if let Ok(Ok(_frame)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), link.recv()).await
    {
        Ok(())
    } else {
        Err(Error::Link("Bootstrap timeout or rejected".into()))
    }
}

/// Держать ноду зарегистрированной (heartbeat) — иначе её запись истечёт по TTL.
/// Запускать вместо одноразового `register_node`.
pub async fn keep_registered(bootstrap_addr: String, info: NodeInfo, every_secs: u64) {
    let every = every_secs.max(5);
    loop {
        let _ = register_node(&bootstrap_addr, info.clone()).await;
        tokio::time::sleep(Duration::from_secs(every)).await;
    }
}

pub async fn fetch_directory(bootstrap_addr: &str) -> Result<Directory> {
    fetch_directory_with_token(bootstrap_addr, load_token()).await
}

pub async fn fetch_directory_with_token(bootstrap_addr: &str, token: String) -> Result<Directory> {
    let link = TcpLink::connect(bootstrap_addr).await?;
    let msg = BootstrapMessage::GetDirectory { token };
    let frame = serde_json::to_vec(&msg).map_err(|_| Error::Link("JSON error".into()))?;
    link.send(&frame).await?;

    if let Ok(Ok(reply_frame)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), link.recv()).await
    {
        if let Ok(BootstrapMessage::Directory(dir)) = serde_json::from_slice(&reply_frame) {
            Ok(dir)
        } else {
            Err(Error::Link("Invalid response".into()))
        }
    } else {
        Err(Error::Link("Bootstrap timeout".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn federation_syncs_directory_between_bs() {
        // BS-A
        let bsa = BootstrapServer::new();
        let la = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let a_addr = la.local_addr().unwrap().to_string();
        {
            let bsa = bsa.clone();
            tokio::spawn(async move {
                let _ = bsa.serve_on(la).await;
            });
        }

        // BS-B
        let bsb = BootstrapServer::new();
        let lb = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let b_addr = lb.local_addr().unwrap().to_string();
        {
            let bsb = bsb.clone();
            tokio::spawn(async move {
                let _ = bsb.serve_on(lb).await;
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Регистрируем сервис ТОЛЬКО на BS-A.
        register_node(
            &a_addr,
            NodeInfo {
                pubkey: "aa".repeat(32),
                address: "127.0.0.1:4040".into(),
                role: "service".into(),
                name: "search.ov".into(),
            },
        )
        .await
        .unwrap();

        // BS-B о нём пока не знает.
        assert!(fetch_directory(&b_addr).await.unwrap().nodes.is_empty());

        // A синхронизирует каталог в B.
        bsa.sync_to(&b_addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Теперь B знает сервис.
        let dir_b = fetch_directory(&b_addr).await.unwrap();
        assert!(dir_b.nodes.iter().any(|n| n.name == "search.ov"));
    }
}
