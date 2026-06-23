use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use overnet_core::onion::{self, OnionKey, Peeled};
use overnet_core::Result;
use overnet_core::Link;
use overnet_link_tcp::{TcpLink, TcpListenerLink};

#[derive(Clone)]
pub enum RouteTarget {
    Address(String),
    Active(mpsc::Sender<Vec<u8>>),
}

/// Маршрутизатор узла (Релей-цикл).
/// Обрабатывает входящие onion-пакеты: если пакет адресован нам (Deliver),
/// передает его в приложение; если нужно переслать (Forward), ищет следующего 
/// соседа по публичному ключу и пересылает ему.
pub struct Router {
    pub onion_key: Arc<OnionKey>,
    /// Таблица соседей: onion_pub -> RouteTarget
    neighbors: Arc<RwLock<HashMap<[u8; 32], RouteTarget>>>,
    /// Канал для доставки сообщений конечному приложению (нам)
    deliver_tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl Router {
    pub fn new(onion_key: OnionKey, deliver_tx: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self {
            onion_key: Arc::new(onion_key),
            neighbors: Arc::new(RwLock::new(HashMap::new())),
            deliver_tx,
        }
    }

    /// Добавить соседа в таблицу маршрутизации (статика)
    pub async fn add_neighbor(&self, pubkey: [u8; 32], addr: String) {
        self.neighbors.write().await.insert(pubkey, RouteTarget::Address(addr));
    }

    /// Добавить активное соединение (например, мы сами к кому-то подключились)
    pub async fn add_active_link(&self, pubkey: [u8; 32], link: Arc<TcpLink>) {
        let (tx, mut rx) = mpsc::channel(100);
        self.neighbors.write().await.insert(pubkey, RouteTarget::Active(tx));
        
        let link_writer = link.clone();
        tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                let _ = link_writer.send(&f).await;
            }
        });

        let link_reader = link.clone();
        let key = self.onion_key.clone();
        let neighbors = self.neighbors.clone();
        let deliver_tx = self.deliver_tx.clone();

        tokio::spawn(async move {
            while let Ok(frame) = link_reader.recv().await {
                Self::handle_frame(frame, link_reader.clone(), &key, &neighbors, &deliver_tx).await;
            }
        });
    }

    /// Отправить готовый onion-пакет соседу по его pubkey
    pub async fn send_to_neighbor(&self, pubkey: &[u8; 32], packet: Vec<u8>) -> Result<()> {
        let target_opt = self.neighbors.read().await.get(pubkey).cloned();
        match target_opt {
            Some(RouteTarget::Address(addr)) => {
                tokio::spawn(async move {
                    if let Ok(next_link) = TcpLink::connect(&addr).await {
                        let _ = next_link.send(&packet).await;
                    }
                });
                Ok(())
            }
            Some(RouteTarget::Active(tx)) => {
                tx.send(packet).await.map_err(|_| overnet_core::Error::Link("send failed".into()))
            }
            None => Err(overnet_core::Error::Link("neighbor not found".into())),
        }
    }

    /// Обработка одного кадра (общая для входящих и исходящих соединений)
    async fn handle_frame(
        frame: Vec<u8>,
        link: Arc<TcpLink>,
        key: &OnionKey,
        neighbors: &RwLock<HashMap<[u8; 32], RouteTarget>>,
        deliver_tx: &mpsc::UnboundedSender<Vec<u8>>,
    ) {
        if frame.len() == 33 && frame[0] == 0xFF {
            // REGISTER Control Message
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&frame[1..33]);
            let (tx, mut rx) = mpsc::channel(100);
            neighbors.write().await.insert(pk, RouteTarget::Active(tx));
            let link_clone = link.clone();
            tokio::spawn(async move {
                while let Some(f) = rx.recv().await {
                    let _ = link_clone.send(&f).await;
                }
            });
            return;
        }

        match onion::peel(key, &frame) {
            Ok(Peeled::Forward { next, inner }) => {
                let target_opt = neighbors.read().await.get(&next).cloned();
                match target_opt {
                    Some(RouteTarget::Address(addr)) => {
                        tokio::spawn(async move {
                            if let Ok(next_link) = TcpLink::connect(&addr).await {
                                let _ = next_link.send(&inner).await;
                            } else {
                                eprintln!("Router: failed to connect to next-hop {}", addr);
                            }
                        });
                    }
                    Some(RouteTarget::Active(tx)) => {
                        let _ = tx.send(inner).await;
                    }
                    None => {
                        eprintln!("Router: unknown next-hop (not in neighbors table)");
                    }
                }
            }
            Ok(Peeled::Deliver(payload)) => {
                let _ = deliver_tx.send(payload);
            }
            Err(_e) => {
                // Silently drop invalid packets to resist active probing
            }
        }
    }

    /// Запустить сервер маршрутизатора
    pub async fn serve(&self, listener: TcpListenerLink) -> Result<()> {
        loop {
            let link = listener.accept().await?;
            let link = Arc::new(link);
            let key = self.onion_key.clone();
            let neighbors = self.neighbors.clone();
            let deliver_tx = self.deliver_tx.clone();

            tokio::spawn(async move {
                while let Ok(frame) = link.recv().await {
                    Self::handle_frame(frame, link.clone(), &key, &neighbors, &deliver_tx).await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn test_onion_routing_a_r_b() {
        let key_a = OnionKey::generate();
        let key_r = OnionKey::generate();
        let key_b = OnionKey::generate();

        let pub_r = key_r.public();
        let pub_b = key_b.public();

        let (tx_r, _rx_r) = mpsc::unbounded_channel();
        let router_r = Router::new(key_r, tx_r);

        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        let router_b = Router::new(key_b, tx_b);

        let listener_r = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr_r = listener_r.local_addr().unwrap().to_string();

        let listener_b = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr_b = listener_b.local_addr().unwrap().to_string();

        // R должен знать, как дойти до B (по его onion pubkey)
        router_r.add_neighbor(pub_b, addr_b.clone()).await;

        tokio::spawn(async move { let _ = router_r.serve(listener_r).await; });
        tokio::spawn(async move { let _ = router_b.serve(listener_b).await; });

        // Дадим серверам запуститься
        tokio::time::sleep(Duration::from_millis(100)).await;

        // A создает сообщение для B через R
        let payload = b"secret message A to B";
        let packet = onion::wrap(&[pub_r, pub_b], payload).unwrap();

        // A отправляет пакет в R
        let link_a = TcpLink::connect(&addr_r).await.unwrap();
        link_a.send(&packet).await.unwrap();

        // Ожидаем доставку на B
        let received = tokio::time::timeout(Duration::from_secs(2), rx_b.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert_eq!(received, payload);
    }
}
