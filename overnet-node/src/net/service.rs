//! Сервис .ov: держит цепи к своим точкам входа, по INTRODUCE2 строит новую цепь
//! к той же точке и склеивается там с клиентом. Свой адрес сервис не раскрывает
//! никому: точка входа видит только предыдущий релей его цепи.
//!
//! Входящие потоки (BEGIN "порт") ведутся на локальные адреса, как
//! HiddenServicePort в Tor: `.ov`-сайтом может быть любой HTTP-сервер.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::TcpStream;

use overnet_core::cell::{relay, RelayMsg};
use overnet_core::ntor::{server_handshake, ONIONSKIN_LEN};
use overnet_core::onion::OnionKey;
use overnet_core::ovaddr::{intro_message, ServiceId, ServiceKey};
use overnet_core::Result;

use super::circuit::{CircHandler, OriginCirc};
use super::client::{Client, INTRO_COUNT};
use super::dir::{intro_points, RelayDesc};
use super::stream;

const CHECK_EVERY: Duration = Duration::from_secs(20);

pub struct Service {
    client: Arc<Client>,
    key: ServiceKey,
    onion: OnionKey,
    /// виртуальный порт → локальный "host:port"
    ports: HashMap<u16, String>,
    intros: tokio::sync::Mutex<HashMap<String, Arc<OriginCirc>>>,
}

impl Service {
    pub fn new(client: Arc<Client>, key: ServiceKey, ports: HashMap<u16, String>) -> Arc<Service> {
        let onion = key.onion_key();
        Arc::new(Service { client, key, onion, ports, intros: tokio::sync::Mutex::new(HashMap::new()) })
    }

    pub fn id(&self) -> ServiceId {
        self.key.id()
    }

    /// Работать бесконечно: держать точки входа живыми.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        loop {
            self.ensure_intros().await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    }

    /// Поднять недостающие точки входа. Возвращает, сколько их живо.
    pub async fn ensure_intros(self: &Arc<Self>) -> usize {
        let mut relays = self.client.node.directory();
        if relays.is_empty() && self.client.refresh_directory().await.is_ok() {
            relays = self.client.node.directory();
        }
        let want = intro_points(&self.id(), &relays, INTRO_COUNT);
        let mut map = self.intros.lock().await;
        map.retain(|pk, c| c.is_alive() && want.iter().any(|w| &w.pubkey == pk));
        for ip in want {
            if map.contains_key(&ip.pubkey) {
                continue;
            }
            if let Ok(c) = self.establish(&ip).await {
                map.insert(ip.pubkey.clone(), c);
            }
        }
        map.len()
    }

    async fn establish(self: &Arc<Self>, intro: &RelayDesc) -> Result<Arc<OriginCirc>> {
        let circ = self.client.circuit_to(intro, 2).await?;
        let last = circ.last_hop();
        let binding = circ.binding(last).expect("hop exists");
        let sig = self.key.sign(&intro_message(&binding));
        let data = [&self.id().0[..], &sig[..]].concat();
        circ.set_handler(Arc::new(IntroHandler { svc: self.clone(), intro: intro.clone() }));
        let res = async {
            circ.send_to(last, RelayMsg::new(relay::ESTABLISH_INTRO, 0, data)).await?;
            circ.expect(relay::INTRO_ESTABLISHED).await
        }
        .await;
        match res {
            Ok(_) => Ok(circ),
            Err(e) => {
                circ.destroy().await;
                Err(e)
            }
        }
    }

    async fn rendezvous(self: &Arc<Self>, intro: &RelayDesc, data: &[u8]) -> Result<()> {
        if data.len() < 20 + ONIONSKIN_LEN {
            return Ok(());
        }
        let cookie = &data[..20];
        let (reply, keys) = server_handshake(&self.onion, &data[20..20 + ONIONSKIN_LEN])?;
        // Новая цепь к той же точке: там она склеится с цепью клиента.
        let circ = self.client.circuit_to(intro, 1).await?;
        let intro_hop = circ.last_hop();
        circ.push_hop(&keys.swapped()).await;
        circ.set_handler(Arc::new(RendHandler { svc: self.clone() }));
        let msg = RelayMsg::new(relay::RENDEZVOUS1, 0, [cookie, &reply[..]].concat());
        if let Err(e) = circ.send_to(intro_hop, msg).await {
            circ.destroy().await;
            return Err(e);
        }
        Ok(())
    }

    async fn begin(&self, circ: Arc<OriginCirc>, hop: usize, msg: RelayMsg) {
        let s = circ.accept_stream(hop, msg.stream_id);
        // Потоки принимаем только от клиента (сквозной хоп), не от релеев.
        if hop != circ.last_hop() {
            return s.end("not allowed").await;
        }
        let target = std::str::from_utf8(&msg.data)
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .and_then(|p| self.ports.get(&p));
        let Some(target) = target else { return s.end("no such port").await };
        match TcpStream::connect(target).await {
            Ok(tcp) => {
                if s.connected().await.is_ok() {
                    stream::pump(s, tcp).await;
                }
            }
            Err(_) => s.end("connection refused").await,
        }
    }
}

struct IntroHandler {
    svc: Arc<Service>,
    intro: RelayDesc,
}

#[async_trait]
impl CircHandler for IntroHandler {
    async fn on_introduce2(&self, _circ: Arc<OriginCirc>, data: Vec<u8>) {
        let _ = self.svc.rendezvous(&self.intro, &data).await;
    }
}

struct RendHandler {
    svc: Arc<Service>,
}

#[async_trait]
impl CircHandler for RendHandler {
    async fn on_begin(&self, circ: Arc<OriginCirc>, hop: usize, msg: RelayMsg) {
        self.svc.begin(circ, hop, msg).await
    }
}
