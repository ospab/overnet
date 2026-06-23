use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::sync::Arc;
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
}

pub struct BootstrapServer {
    nodes: Arc<RwLock<HashMap<String, NodeInfo>>>,
    /// Общий токен из config.json. Пусто = открыто (localhost); задан = чужие дропаются.
    token: String,
}

#[derive(Deserialize)]
struct Config {
    token: Option<String>,
}

fn load_token() -> String {
    if let Ok(content) = std::fs::read_to_string("config.json") {
        if let Ok(cfg) = serde_json::from_str::<Config>(&content) {
            if let Some(t) = cfg.token {
                return t;
            }
        }
    }
    // Fallback to env var if config file not found
    std::env::var("OVERNET_TOKEN").unwrap_or_default()
}

impl BootstrapServer {
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
            token: load_token(),
        }
    }

    pub async fn serve(&self, bind_addr: &str) -> Result<()> {
        let listener = TcpListenerLink::bind(bind_addr).await?;
        println!("Bootstrap server listening on {}", bind_addr);
        self.serve_on(listener).await
    }

    /// Обслуживать каталог на уже привязанном listener (для тестов / открытого порта).
    pub async fn serve_on(&self, listener: TcpListenerLink) -> Result<()> {
        loop {
            let link = listener.accept().await?;
            let nodes = self.nodes.clone();
            let expected = self.token.clone();

            tokio::spawn(async move {
                if let Ok(frame) = link.recv().await {
                    if let Ok(msg) = serde_json::from_slice::<BootstrapMessage>(&frame) {
                        match msg {
                            BootstrapMessage::Register { info, token } => {
                                // Токен задан (config) → чужие молча дропаются.
                                if !expected.is_empty() && token != expected {
                                    return;
                                }
                                println!("Registered node: {} ({}) name='{}' at {}", info.pubkey, info.role, info.name, info.address);
                                nodes.write().await.insert(info.pubkey.clone(), info);
                                let ack = serde_json::to_vec(&BootstrapMessage::Ack).unwrap();
                                let _ = link.send(&ack).await;
                            }
                            BootstrapMessage::GetDirectory { token } => {
                                if !expected.is_empty() && token != expected {
                                    return;
                                }
                                let dir = Directory {
                                    nodes: nodes.read().await.values().cloned().collect(),
                                };
                                let reply = serde_json::to_vec(&BootstrapMessage::Directory(dir)).unwrap();
                                let _ = link.send(&reply).await;
                            }
                            _ => {}
                        }
                    }
                }
            });
        }
    }
}

pub async fn register_node(bootstrap_addr: &str, info: NodeInfo) -> Result<()> {
    let link = TcpLink::connect(bootstrap_addr).await?;
    let token = load_token();
    let msg = BootstrapMessage::Register { info, token };
    let frame = serde_json::to_vec(&msg).map_err(|_| Error::Link("JSON error".into()))?;
    link.send(&frame).await?;
    
    // Wait for Ack
    if let Ok(_ack) = tokio::time::timeout(std::time::Duration::from_secs(5), link.recv()).await {
        Ok(())
    } else {
        Err(Error::Link("Bootstrap timeout".into()))
    }
}

pub async fn fetch_directory(bootstrap_addr: &str) -> Result<Directory> {
    let link = TcpLink::connect(bootstrap_addr).await?;
    let token = load_token();
    let msg = BootstrapMessage::GetDirectory { token };
    let frame = serde_json::to_vec(&msg).map_err(|_| Error::Link("JSON error".into()))?;
    link.send(&frame).await?;
    
    if let Ok(Ok(reply_frame)) = tokio::time::timeout(std::time::Duration::from_secs(5), link.recv()).await {
        if let Ok(BootstrapMessage::Directory(dir)) = serde_json::from_slice(&reply_frame) {
            Ok(dir)
        } else {
            Err(Error::Link("Invalid response".into()))
        }
    } else {
        Err(Error::Link("Bootstrap timeout".into()))
    }
}
