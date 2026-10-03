//! Клиент overnet: каталог через релеи, цепи к сервисам .ov и к выходу.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::seq::SliceRandom;
use rand::RngCore;

use overnet_core::cell::{relay, RelayMsg};
use overnet_core::ntor::ClientHandshake;
use overnet_core::ovaddr::ServiceId;
use overnet_core::{Error, Result};

use super::circuit::OriginCirc;
use super::dir::{self, intro_points, path_to, RelayDesc};
use super::stream::Stream;
use super::Node;

/// Сколько точек входа у сервиса.
pub const INTRO_COUNT: usize = 2;
/// Сколько релеев до точки входа / выхода (плюс сама точка = 3 хопа).
const EXTRA_HOPS: usize = 2;
/// Цепь к выходу живёт не дольше, чтобы не связывать надолго разные сайты.
const EXIT_CIRCUIT_LIFETIME: Duration = Duration::from_secs(600);

pub struct Client {
    pub node: Arc<Node>,
    bootstrap: Vec<RelayDesc>,
    guard: Mutex<Option<RelayDesc>>,
    exit_circ: tokio::sync::Mutex<Option<(Arc<OriginCirc>, std::time::Instant)>>,
    rend: tokio::sync::Mutex<HashMap<ServiceId, Arc<OriginCirc>>>,
}

impl Client {
    /// `bootstrap` — релеи из конфига, у которых берётся первый каталог.
    pub fn new(node: Arc<Node>, bootstrap: Vec<RelayDesc>) -> Arc<Client> {
        Arc::new(Client {
            node,
            bootstrap,
            guard: Mutex::new(None),
            exit_circ: tokio::sync::Mutex::new(None),
            rend: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Обновить каталог у входного релея (или у bootstrap-релеев при первом запуске).
    pub async fn refresh_directory(&self) -> Result<usize> {
        let mut candidates: Vec<RelayDesc> = self.guard.lock().unwrap().iter().cloned().collect();
        let mut known = self.node.directory();
        known.shuffle(&mut rand::thread_rng());
        candidates.extend(known.into_iter().take(3));
        candidates.extend(self.bootstrap.iter().cloned());
        let mut last = Error::Link("no relays to ask for the directory (set \"relays\" in config)".into());
        for c in candidates {
            match dir::fetch_via(&self.node, &c).await {
                Ok(relays) => {
                    let n = relays.len();
                    self.node.set_directory(relays);
                    return Ok(n);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Обновлять каталог в фоне.
    pub fn spawn_directory_refresh(self: &Arc<Self>, every: Duration) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let _ = me.refresh_directory().await;
            }
        });
    }

    async fn directory(&self) -> Result<Vec<RelayDesc>> {
        let d = self.node.directory();
        if !d.is_empty() {
            return Ok(d);
        }
        self.refresh_directory().await?;
        Ok(self.node.directory())
    }

    /// Вход в сеть: один на всё время работы (меньше шансов попасть на чужой вход).
    pub fn guard(&self, relays: &[RelayDesc]) -> Option<RelayDesc> {
        let mut g = self.guard.lock().unwrap();
        if let Some(cur) = g.as_ref() {
            if relays.iter().any(|r| r.pubkey == cur.pubkey) {
                return Some(cur.clone());
            }
        }
        *g = relays.choose(&mut rand::thread_rng()).cloned();
        g.clone()
    }

    /// «Новая личность»: забыть вход, склейки с сервисами и цепь к выходу.
    /// Следующие соединения строятся с нуля, через новые цепи.
    pub async fn new_identity(&self) {
        self.forget_guard();
        let rend: Vec<_> = self.rend.lock().await.drain().map(|(_, c)| c).collect();
        let exit = self.exit_circ.lock().await.take();
        for c in rend.into_iter().chain(exit.map(|(c, _)| c)) {
            c.destroy().await;
        }
    }

    /// Сколько релеев в каталоге и сколько из них — выходы.
    pub fn directory_size(&self) -> (usize, usize) {
        let d = self.node.directory();
        (d.len(), d.iter().filter(|r| r.exit).count())
    }

    fn forget_guard(&self) {
        *self.guard.lock().unwrap() = None;
    }

    /// Построить цепь до `last` через вход и случайные релеи.
    pub async fn circuit_to(&self, last: &RelayDesc, extra: usize) -> Result<Arc<OriginCirc>> {
        let relays = self.directory().await?;
        let guard = self.guard(&relays);
        let path = path_to(last, &relays, extra, guard.as_ref());
        let r = self.node.build_circuit(&path).await;
        if r.is_err() {
            self.forget_guard();
        }
        r
    }

    /// Поток к сервису `id` на его порт `port`.
    pub async fn connect_service(&self, id: ServiceId, port: u16) -> Result<Stream> {
        let circ = self.rendezvous(id).await?;
        match circ.open_stream(circ.last_hop(), relay::BEGIN, &port.to_string()).await {
            Ok(s) => Ok(s),
            Err(e) => {
                if !circ.is_alive() {
                    self.rend.lock().await.remove(&id);
                }
                Err(e)
            }
        }
    }

    /// Цепь, склеенная с сервисом `id` (кэшируется, пока жива).
    async fn rendezvous(&self, id: ServiceId) -> Result<Arc<OriginCirc>> {
        let mut cache = self.rend.lock().await;
        if let Some(c) = cache.get(&id) {
            if c.is_alive() {
                return Ok(c.clone());
            }
        }
        let relays = self.directory().await?;
        let b = id.onion_pub()?;
        let mut last = Error::Link("service unreachable: no intro points".into());
        for intro in intro_points(&id, &relays, INTRO_COUNT) {
            let circ = match self.circuit_to(&intro, EXTRA_HOPS).await {
                Ok(c) => c,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            let mut cookie = [0u8; 20];
            rand::thread_rng().fill_bytes(&mut cookie);
            let (hs, x) = ClientHandshake::new(b);
            let res = async {
                let data = [&id.0[..], &cookie, &x].concat();
                circ.send_to(circ.last_hop(), RelayMsg::new(relay::INTRODUCE1, 0, data)).await?;
                let ack = circ.expect(relay::INTRODUCE_ACK).await?;
                if ack.first() != Some(&0) {
                    return Err(Error::Link("service is not at this intro point (offline?)".into()));
                }
                let reply = circ.expect(relay::RENDEZVOUS2).await?;
                // Сквозной слой: ключ сервиса проверен его адресом.
                let keys = hs.finish(&reply)?;
                circ.push_hop(&keys).await;
                Ok(())
            }
            .await;
            match res {
                Ok(()) => {
                    cache.insert(id, circ.clone());
                    return Ok(circ);
                }
                Err(e) => {
                    circ.destroy().await;
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// Поток в обычный интернет через выход overnet.
    pub async fn connect_exit(&self, target: &str) -> Result<Stream> {
        let circ = {
            let mut cur = self.exit_circ.lock().await;
            match cur.as_ref() {
                Some((c, t)) if c.is_alive() && t.elapsed() < EXIT_CIRCUIT_LIFETIME => c.clone(),
                _ => {
                    let relays = self.directory().await?;
                    let exits: Vec<&RelayDesc> = relays.iter().filter(|r| r.exit).collect();
                    let exit = exits
                        .choose(&mut rand::thread_rng())
                        .ok_or_else(|| Error::Link("no exit relays in the network".into()))?;
                    let c = self.circuit_to(exit, EXTRA_HOPS).await?;
                    *cur = Some((c.clone(), std::time::Instant::now()));
                    c
                }
            }
        };
        circ.open_stream(circ.last_hop(), relay::BEGIN, target).await
    }
}
