//! Цепь, которую строим мы (клиент или сервис): CREATE к первому хопу, EXTEND
//! дальше, потоки поверх. Сервис и клиент .ov добавляют к ней «виртуальный хоп» —
//! сквозной слой друг до друга поверх склейки у точки входа.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use overnet_core::cell::{cmd, relay, BackwardLayers, Cell, ForwardLayers, Layer, RelayMsg};
use overnet_core::ntor::{ClientHandshake, HopKeys};
use overnet_core::{Error, Result};

use super::dir::RelayDesc;
use super::stream::{self, MsgSink, Stream, StreamMap};
use super::{Channel, Entry, Node};

const STEP_TIMEOUT: Duration = Duration::from_secs(20);
pub const STREAM_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) enum Ctl {
    Created(Vec<u8>),
    Msg(usize, RelayMsg),
    Destroyed,
}

/// Обработчик входящих событий цепи (нужен сервису).
#[async_trait]
pub trait CircHandler: Send + Sync {
    /// Точка входа передала нам клиента: data = cookie (20) | X (32).
    async fn on_introduce2(&self, _circ: Arc<OriginCirc>, _data: Vec<u8>) {}
    /// Клиент открывает поток к нам (через виртуальный хоп `hop`).
    async fn on_begin(&self, _circ: Arc<OriginCirc>, _hop: usize, _msg: RelayMsg) {}
}

pub struct OriginCirc {
    node: Arc<Node>,
    chan: Arc<Channel>,
    id: u32,
    fwd: tokio::sync::Mutex<ForwardLayers>,
    bwd: Mutex<BackwardLayers>,
    /// Привязки хопов (из ntor), по порядку.
    bindings: Mutex<Vec<[u8; 32]>>,
    ctl_tx: mpsc::UnboundedSender<Ctl>,
    ctl_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Ctl>>,
    pub(crate) streams: StreamMap,
    next_stream: AtomicU16,
    handler: Mutex<Option<Arc<dyn CircHandler>>>,
    dead: AtomicBool,
    me: Weak<OriginCirc>,
}

impl Node {
    /// Построить цепь через `path` (первый — вход, последний — конец цепи).
    pub async fn build_circuit(self: &Arc<Self>, path: &[RelayDesc]) -> Result<Arc<OriginCirc>> {
        let first = path.first().ok_or_else(|| Error::Link("empty path".into()))?;
        let key0 = first.key().ok_or_else(|| Error::Link("bad relay key".into()))?;
        let chan = self.channel_to(&first.address, key0).await?;
        let id = chan.alloc_circ_id();
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let circ = Arc::new_cyclic(|me| OriginCirc {
            node: self.clone(),
            chan: chan.clone(),
            id,
            fwd: tokio::sync::Mutex::new(ForwardLayers::default()),
            bwd: Mutex::new(BackwardLayers::default()),
            bindings: Mutex::new(Vec::new()),
            ctl_tx,
            ctl_rx: tokio::sync::Mutex::new(ctl_rx),
            streams: stream::new_map(),
            next_stream: AtomicU16::new(1),
            handler: Mutex::new(None),
            dead: AtomicBool::new(false),
            me: me.clone(),
        });
        self.insert(chan.id, id, Entry::Origin(circ.clone()));

        let (hs, x) = ClientHandshake::new(key0);
        chan.send(Cell::new(id, cmd::CREATE, &x)).await?;
        let reply = match circ.wait_ctl().await? {
            Ctl::Created(r) => r,
            _ => return Err(circ.fail("unexpected answer to CREATE").await),
        };
        match hs.finish(&reply) {
            Ok(keys) => circ.push_hop(&keys).await,
            Err(e) => {
                circ.destroy().await;
                return Err(at_hop(e, first));
            }
        }
        for hop in &path[1..] {
            if let Err(e) = circ.extend(hop).await {
                circ.destroy().await;
                return Err(at_hop(e, hop));
            }
        }
        Ok(circ)
    }
}

/// Ошибка шага цепи с адресом релея: без него «ntor: server authentication
/// failed» не говорит, какой релей ответил чужим ключом.
fn at_hop(e: Error, hop: &RelayDesc) -> Error {
    match e {
        Error::Link(m) => Error::Link(format!("{m} (relay {}…@{})", &hop.pubkey[..16.min(hop.pubkey.len())], hop.address)),
        other => other,
    }
}

impl OriginCirc {
    fn arc(&self) -> Arc<OriginCirc> {
        self.me.upgrade().expect("circuit is alive while used")
    }

    pub fn hop_count(&self) -> usize {
        self.bindings.lock().unwrap().len()
    }

    pub fn last_hop(&self) -> usize {
        self.hop_count().saturating_sub(1)
    }

    pub fn binding(&self, hop: usize) -> Option<[u8; 32]> {
        self.bindings.lock().unwrap().get(hop).copied()
    }

    pub fn is_alive(&self) -> bool {
        !self.dead.load(Ordering::Relaxed)
    }

    pub fn set_handler(&self, h: Arc<dyn CircHandler>) {
        *self.handler.lock().unwrap() = Some(h);
    }

    /// Добавить хоп с готовыми ключами (после ntor; для сервиса — `swapped()`).
    pub async fn push_hop(&self, keys: &HopKeys) {
        let (f, b) = Layer::pair(keys);
        // Сначала приём: ответ нового хопа может прийти сразу.
        self.bwd.lock().unwrap().push(b);
        self.fwd.lock().await.push(f);
        self.bindings.lock().unwrap().push(keys.binding);
    }

    async fn extend(&self, hop: &RelayDesc) -> Result<()> {
        let key = hop.key().ok_or_else(|| Error::Link("bad relay key".into()))?;
        let (hs, x) = ClientHandshake::new(key);
        let data = [&key[..], &x, hop.address.as_bytes()].concat();
        self.send_to(self.last_hop(), RelayMsg::new(relay::EXTEND, 0, data)).await?;
        let reply = self.expect(relay::EXTENDED).await?;
        let keys = hs.finish(&reply)?;
        self.push_hop(&keys).await;
        Ok(())
    }

    /// Отправить сообщение хопу `hop`.
    pub async fn send_to(&self, hop: usize, msg: RelayMsg) -> Result<()> {
        if !self.is_alive() {
            return Err(Error::Link("circuit closed".into()));
        }
        let mut fwd = self.fwd.lock().await;
        let body = fwd.encrypt_to(hop, &msg)?;
        self.chan.send(Cell { circ_id: self.id, cmd: cmd::RELAY, body }).await
    }

    async fn wait_ctl(&self) -> Result<Ctl> {
        let mut rx = self.ctl_rx.lock().await;
        match tokio::time::timeout(STEP_TIMEOUT, rx.recv()).await {
            Ok(Some(Ctl::Destroyed)) | Ok(None) => Err(Error::Link("circuit destroyed".into())),
            Ok(Some(c)) => Ok(c),
            Err(_) => Err(Error::Link("circuit: timeout".into())),
        }
    }

    /// Дождаться служебного сообщения с командой `want` от последнего хопа;
    /// вернуть его данные. От другого хопа такое сообщение — подделка.
    pub async fn expect(&self, want: u8) -> Result<Vec<u8>> {
        let last = self.last_hop();
        match self.wait_ctl().await? {
            Ctl::Msg(hop, m) if m.cmd == want && hop == last => Ok(m.data),
            Ctl::Msg(_, m) => Err(Error::Link(format!("circuit: got command {} instead of {want}", m.cmd))),
            _ => Err(Error::Link("circuit: unexpected cell".into())),
        }
    }

    async fn fail(&self, why: &str) -> Error {
        self.destroy().await;
        Error::Link(format!("circuit: {why}"))
    }

    /// Открыть поток к хопу `hop`: BEGIN target (выход/сервис) или BEGIN_DIR.
    pub async fn open_stream(&self, hop: usize, command: u8, target: &str) -> Result<Stream> {
        let id = loop {
            let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
            if id != 0 && !self.streams.lock().unwrap().contains_key(&id) {
                break id;
            }
        };
        let mut s = Stream::open(&self.streams, id, Arc::new(OriginSink { circ: self.arc(), hop }));
        self.send_to(hop, RelayMsg::new(command, id, target.as_bytes().to_vec())).await?;
        s.wait_connected(STREAM_TIMEOUT).await?;
        Ok(s)
    }

    /// Принять поток, который открыл собеседник (у сервиса).
    pub fn accept_stream(&self, hop: usize, id: u16) -> Stream {
        Stream::open(&self.streams, id, Arc::new(OriginSink { circ: self.arc(), hop }))
    }

    pub(crate) fn on_created(&self, reply: &[u8]) {
        let _ = self.ctl_tx.send(Ctl::Created(reply.to_vec()));
    }

    pub(crate) async fn on_relay(&self, body: Vec<u8>) {
        let decoded = self.bwd.lock().unwrap().decrypt(body);
        let (hop, msg) = match decoded {
            Ok(x) => x,
            // Нераспознанная ячейка = кто-то в пути портит трафик. Цепь в утиль.
            Err(_) => return self.destroy().await,
        };
        match msg.cmd {
            relay::DATA | relay::END | relay::SENDME | relay::CONNECTED if msg.stream_id != 0 => {
                stream::deliver(&self.streams, msg);
            }
            relay::BEGIN | relay::INTRODUCE2 => {
                let h = self.handler.lock().unwrap().clone();
                if let Some(h) = h {
                    let me = self.arc();
                    tokio::spawn(async move {
                        if msg.cmd == relay::BEGIN {
                            h.on_begin(me, hop, msg).await
                        } else {
                            h.on_introduce2(me, msg.data).await
                        }
                    });
                } else if msg.cmd == relay::BEGIN {
                    let _ = self.send_to(hop, RelayMsg::new(relay::END, msg.stream_id, Vec::new())).await;
                }
            }
            _ => {
                let _ = self.ctl_tx.send(Ctl::Msg(hop, msg));
            }
        }
    }

    pub(crate) fn on_destroyed(&self) {
        if self.dead.swap(true, Ordering::Relaxed) {
            return;
        }
        stream::close_all(&self.streams, "circuit closed");
        let _ = self.ctl_tx.send(Ctl::Destroyed);
    }

    /// Разобрать цепь.
    pub async fn destroy(&self) {
        if !self.is_alive() {
            return;
        }
        self.node.remove(self.chan.id, self.id);
        let _ = self.chan.send(Cell::new(self.id, cmd::DESTROY, &[])).await;
        self.on_destroyed();
    }
}

struct OriginSink {
    circ: Arc<OriginCirc>,
    hop: usize,
}

#[async_trait]
impl MsgSink for OriginSink {
    async fn send_msg(&self, msg: RelayMsg) -> Result<()> {
        self.circ.send_to(self.hop, msg).await
    }
}
