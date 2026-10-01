//! overnet v0.2 — цепи и потоки по образцу Tor.
//!
//! - **Канал** (`Channel`) — линк к соседу, по которому идут ячейки многих цепей.
//! - **Цепь** строит клиент: CREATE к первому хопу, затем EXTEND через уже
//!   построенную часть (`circuit`). Хопы держат по слою (`relay`).
//! - **Потоки** (`stream`) — TCP-подобные соединения внутри цепи с окном (SENDME).
//! - **Выход** (`exit`) — хоп, открывающий потоки в обычный интернет; выключен
//!   по умолчанию.
//! - **Сервисы .ov** (`service`) — через точки входа и склейку цепей: ни клиент,
//!   ни сервис не знают адреса друг друга.
//! - **Каталог** (`dir`) — список релеев; клиент берёт его у релея через цепь,
//!   а не у bootstrap-сервера.
//!
//! Чего здесь пока нет (честно): шифрования самого линка (cell-заголовки видны
//! на проводе — закрывает транспорт ostp), защиты от Sybil и guard-политики Tor.

pub mod circuit;
pub mod client;
pub mod dir;
pub mod exit;
pub mod gateway;
pub mod names;
pub mod relay;
pub mod service;
pub mod stream;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use tokio::sync::mpsc;

use overnet_core::cell::{cmd, Cell};
use overnet_core::ntor;
use overnet_core::onion::OnionKey;
use overnet_core::{Error, Link, Result};
use overnet_link_tcp::{TcpLink, TcpListenerLink};

use circuit::OriginCirc;
use dir::RelayDesc;
use exit::ExitPolicy;
use relay::RelayCirc;

/// Очередь исходящих ячеек канала. Потоки ограничены окнами, поэтому до
/// предела очередь доходит только при перегрузке.
const CHANNEL_QUEUE: usize = 4096;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Линк к соседу.
pub struct Channel {
    pub id: u64,
    tx: mpsc::Sender<Cell>,
    /// Мы открыли соединение: наши circ_id со старшим битом, чтобы не пересечься
    /// с id, которые выбирает сосед.
    initiator: bool,
    next_circ: AtomicU32,
    closed: AtomicBool,
    peer: Option<[u8; 32]>,
}

impl Channel {
    pub async fn send(&self, cell: Cell) -> Result<()> {
        self.tx.send(cell).await.map_err(|_| Error::Link("channel closed".into()))
    }

    fn alloc_circ_id(&self) -> u32 {
        let n = self.next_circ.fetch_add(1, Ordering::Relaxed) & 0x7fff_ffff;
        let n = n.max(1);
        if self.initiator {
            n | 0x8000_0000
        } else {
            n
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

/// Кому принадлежит (канал, circ_id).
#[derive(Clone)]
pub(crate) enum Entry {
    /// Цепь, которую строим мы.
    Origin(Arc<OriginCirc>),
    /// Мы — хоп; ячейки от предыдущего узла.
    Prev(Arc<RelayCirc>),
    /// Мы — хоп; ячейки от следующего узла.
    Next(Arc<RelayCirc>),
}

/// Узел overnet: релей, клиент или сервис — всё сразу, в зависимости от того,
/// что ему поручили.
pub struct Node {
    pub(crate) key: OnionKey,
    pub(crate) exit: ExitPolicy,
    table: Mutex<HashMap<(u64, u32), Entry>>,
    by_peer: tokio::sync::Mutex<HashMap<[u8; 32], Arc<Channel>>>,
    next_chan: AtomicU64,
    dir: RwLock<Vec<RelayDesc>>,
    /// Сервисы, для которых мы точка входа: service_id → цепь сервиса.
    pub(crate) intros: Mutex<HashMap<[u8; 32], Weak<RelayCirc>>>,
    /// Клиенты, ждущие склейки: cookie → цепь клиента.
    pub(crate) rendezvous: Mutex<HashMap<[u8; 20], Weak<RelayCirc>>>,
}

impl Node {
    pub fn new(key: OnionKey, exit: ExitPolicy) -> Arc<Node> {
        Arc::new(Node {
            key,
            exit,
            table: Mutex::new(HashMap::new()),
            by_peer: tokio::sync::Mutex::new(HashMap::new()),
            next_chan: AtomicU64::new(1),
            dir: RwLock::new(Vec::new()),
            intros: Mutex::new(HashMap::new()),
            rendezvous: Mutex::new(HashMap::new()),
        })
    }

    pub fn pubkey(&self) -> [u8; 32] {
        self.key.public()
    }

    pub fn exit_policy(&self) -> &ExitPolicy {
        &self.exit
    }

    pub fn directory(&self) -> Vec<RelayDesc> {
        self.dir.read().unwrap().clone()
    }

    pub fn set_directory(&self, relays: Vec<RelayDesc>) {
        *self.dir.write().unwrap() = relays;
    }

    // ── таблица цепей ──────────────────────────────────────────────────────

    pub(crate) fn insert(&self, chan: u64, circ: u32, e: Entry) {
        self.table.lock().unwrap().insert((chan, circ), e);
    }

    pub(crate) fn remove(&self, chan: u64, circ: u32) {
        self.table.lock().unwrap().remove(&(chan, circ));
    }

    fn get(&self, chan: u64, circ: u32) -> Option<Entry> {
        self.table.lock().unwrap().get(&(chan, circ)).cloned()
    }

    /// Сколько цепей узел сейчас держит (для тестов и статистики).
    pub fn circuit_count(&self) -> usize {
        self.table.lock().unwrap().len()
    }

    // ── каналы ─────────────────────────────────────────────────────────────

    /// Принимать входящие соединения (узел-релей).
    pub async fn serve(self: &Arc<Self>, listener: TcpListenerLink) -> Result<()> {
        loop {
            let link = listener.accept().await?;
            self.attach(Arc::new(link), false, None);
        }
    }

    /// Канал к узлу с ключом `pubkey` по адресу `addr`; существующий переиспользуется.
    pub async fn channel_to(self: &Arc<Self>, addr: &str, pubkey: [u8; 32]) -> Result<Arc<Channel>> {
        if let Some(ch) = self.by_peer.lock().await.get(&pubkey) {
            if !ch.is_closed() {
                return Ok(ch.clone());
            }
        }
        let link = tokio::time::timeout(CONNECT_TIMEOUT, TcpLink::connect(addr))
            .await
            .map_err(|_| Error::Link(format!("connect {addr}: timeout")))??;
        let ch = self.attach(Arc::new(link), true, Some(pubkey));
        self.by_peer.lock().await.insert(pubkey, ch.clone());
        Ok(ch)
    }

    /// Обслуживать уже открытый линк как канал.
    pub fn attach(self: &Arc<Self>, link: Arc<dyn Link>, initiator: bool, peer: Option<[u8; 32]>) -> Arc<Channel> {
        let (tx, mut rx) = mpsc::channel::<Cell>(CHANNEL_QUEUE);
        let ch = Arc::new(Channel {
            id: self.next_chan.fetch_add(1, Ordering::Relaxed),
            tx,
            initiator,
            next_circ: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            peer,
        });
        {
            let link = link.clone();
            let ch = ch.clone();
            tokio::spawn(async move {
                while let Some(cell) = rx.recv().await {
                    if link.send(&cell.encode()).await.is_err() {
                        break;
                    }
                }
                ch.closed.store(true, Ordering::Relaxed);
            });
        }
        {
            let node = self.clone();
            let ch = ch.clone();
            tokio::spawn(async move {
                while let Ok(frame) = link.recv().await {
                    match Cell::decode(&frame) {
                        Ok(cell) => node.handle_cell(&ch, cell).await,
                        Err(_) => break, // не наш протокол — рвём
                    }
                }
                node.channel_closed(&ch).await;
            });
        }
        ch
    }

    async fn channel_closed(&self, ch: &Arc<Channel>) {
        ch.closed.store(true, Ordering::Relaxed);
        if let Some(peer) = ch.peer {
            let mut m = self.by_peer.lock().await;
            if m.get(&peer).is_some_and(|c| c.id == ch.id) {
                m.remove(&peer);
            }
        }
        let gone: Vec<Entry> = {
            let mut t = self.table.lock().unwrap();
            let keys: Vec<_> = t.keys().filter(|k| k.0 == ch.id).cloned().collect();
            keys.into_iter().filter_map(|k| t.remove(&k)).collect()
        };
        for e in gone {
            match e {
                Entry::Origin(oc) => oc.on_destroyed(),
                Entry::Prev(rc) => rc.destroy(false, true).await,
                Entry::Next(rc) => rc.destroy(true, false).await,
            }
        }
    }

    async fn handle_cell(self: &Arc<Self>, ch: &Arc<Channel>, cell: Cell) {
        let id = cell.circ_id;
        match cell.cmd {
            cmd::CREATE => {
                if self.get(ch.id, id).is_some() {
                    return;
                }
                match ntor::server_handshake(&self.key, &cell.body[..ntor::ONIONSKIN_LEN]) {
                    Ok((reply, keys)) => {
                        let rc = RelayCirc::new(self, ch.clone(), id, &keys);
                        self.insert(ch.id, id, Entry::Prev(rc));
                        let _ = ch.send(Cell::new(id, cmd::CREATED, &reply)).await;
                    }
                    Err(_) => {
                        let _ = ch.send(Cell::new(id, cmd::DESTROY, &[])).await;
                    }
                }
            }
            cmd::CREATED => match self.get(ch.id, id) {
                Some(Entry::Next(rc)) => rc.on_created(&cell.body[..ntor::REPLY_LEN]).await,
                Some(Entry::Origin(oc)) => oc.on_created(&cell.body[..ntor::REPLY_LEN]),
                _ => {}
            },
            cmd::RELAY => match self.get(ch.id, id) {
                Some(Entry::Prev(rc)) => rc.on_forward(cell.body).await,
                Some(Entry::Next(rc)) => rc.send_backward_raw(cell.body).await,
                Some(Entry::Origin(oc)) => oc.on_relay(cell.body).await,
                None => {}
            },
            cmd::DESTROY => {
                let e = self.get(ch.id, id);
                self.remove(ch.id, id);
                match e {
                    Some(Entry::Origin(oc)) => oc.on_destroyed(),
                    Some(Entry::Prev(rc)) => rc.destroy(false, true).await,
                    Some(Entry::Next(rc)) => rc.destroy(true, false).await,
                    None => {}
                }
            }
            _ => {} // PADDING и неизвестное
        }
    }
}
