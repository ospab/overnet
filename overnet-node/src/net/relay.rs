//! Хоп цепи: снять свой слой, переслать или обработать, добавить слой на обратном
//! пути. Здесь же — выход в интернет, раздача каталога, точка входа сервисов и
//! склейка цепи клиента с цепью сервиса.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;

use overnet_core::cell::{cmd, relay, Cell, Layer, RelayMsg};
use overnet_core::ntor::{HopKeys, ONIONSKIN_LEN, REPLY_LEN};
use overnet_core::ovaddr::{intro_message, ServiceId};
use overnet_core::Result;

use super::stream::{self, MsgSink, Stream, StreamMap};
use super::{Channel, Entry, Node};

enum Next {
    None,
    /// CREATE отправлен, ждём CREATED.
    Pending(Arc<Channel>, u32),
    Hop(Arc<Channel>, u32),
    /// Склеена с другой цепью у этой точки входа.
    Splice(Weak<RelayCirc>),
}

pub struct RelayCirc {
    node: Weak<Node>,
    prev: Arc<Channel>,
    prev_id: u32,
    binding: [u8; 32],
    /// Слой «от клиента». Трогает только читатель канала prev — по порядку.
    fwd: Mutex<Layer>,
    /// Слой «к клиенту»: шифрование и постановка в очередь — атомарно, иначе
    /// счётчики разъедутся.
    bwd: tokio::sync::Mutex<Layer>,
    next: Mutex<Next>,
    streams: StreamMap,
    dead: AtomicBool,
    me: Weak<RelayCirc>,
}

/// Лимит записей «ждущих склейки» (защита памяти точки входа).
const MAX_PENDING_RENDEZVOUS: usize = 4096;

impl RelayCirc {
    pub(crate) fn new(node: &Arc<Node>, prev: Arc<Channel>, prev_id: u32, keys: &HopKeys) -> Arc<RelayCirc> {
        let (f, b) = Layer::pair(keys);
        Arc::new_cyclic(|me| RelayCirc {
            node: Arc::downgrade(node),
            prev,
            prev_id,
            binding: keys.binding,
            fwd: Mutex::new(f),
            bwd: tokio::sync::Mutex::new(b),
            next: Mutex::new(Next::None),
            streams: stream::new_map(),
            dead: AtomicBool::new(false),
            me: me.clone(),
        })
    }

    fn arc(&self) -> Arc<RelayCirc> {
        self.me.upgrade().expect("circuit is alive while used")
    }

    /// Ячейка от предыдущего узла.
    pub(crate) async fn on_forward(&self, mut body: Vec<u8>) {
        let mine = self.fwd.lock().unwrap().unwrap(&mut body);
        if mine {
            match RelayMsg::decode_body(&body) {
                Ok(msg) => self.handle(msg).await,
                Err(_) => self.destroy(true, true).await,
            }
            return;
        }
        let next = match &*self.next.lock().unwrap() {
            Next::Hop(ch, id) => Ok((ch.clone(), *id)),
            Next::Splice(w) => Err(w.upgrade()),
            Next::None | Next::Pending(..) => Err(None),
        };
        match next {
            Ok((ch, id)) => {
                if ch.send(Cell { circ_id: id, cmd: cmd::RELAY, body }).await.is_err() {
                    self.destroy(true, false).await;
                }
            }
            // Через склейку ячейка клиента уходит сервису (и наоборот) по его цепи назад.
            Err(Some(other)) => other.send_backward_raw(body).await,
            Err(None) => self.destroy(true, true).await,
        }
    }

    /// Ячейка, идущая к началу цепи: добавить свой слой и отдать предыдущему.
    pub(crate) async fn send_backward_raw(&self, mut body: Vec<u8>) {
        if self.dead.load(Ordering::Relaxed) {
            return;
        }
        let mut l = self.bwd.lock().await;
        l.wrap(&mut body);
        let _ = self.prev.send(Cell { circ_id: self.prev_id, cmd: cmd::RELAY, body }).await;
    }

    /// Своё сообщение к началу цепи.
    pub(crate) async fn send_backward_msg(&self, msg: RelayMsg) -> Result<()> {
        if self.dead.load(Ordering::Relaxed) {
            return Err(overnet_core::Error::Link("circuit closed".into()));
        }
        let mut body = msg.encode_body()?;
        let mut l = self.bwd.lock().await;
        l.seal(&mut body);
        self.prev.send(Cell { circ_id: self.prev_id, cmd: cmd::RELAY, body }).await
    }

    pub(crate) async fn on_created(&self, reply: &[u8]) {
        let ok = {
            let mut n = self.next.lock().unwrap();
            match &*n {
                Next::Pending(ch, id) => {
                    *n = Next::Hop(ch.clone(), *id);
                    true
                }
                _ => false,
            }
        };
        if ok {
            let _ = self.send_backward_msg(RelayMsg::new(relay::EXTENDED, 0, reply.to_vec())).await;
        }
    }

    async fn handle(&self, msg: RelayMsg) {
        let Some(node) = self.node.upgrade() else { return };
        match msg.cmd {
            relay::EXTEND => self.extend(&node, msg.data).await,
            relay::BEGIN => self.begin_exit(&node, msg).await,
            relay::BEGIN_DIR => self.begin_dir(&node, msg.stream_id).await,
            relay::ESTABLISH_INTRO => self.establish_intro(&node, &msg.data).await,
            relay::INTRODUCE1 => self.introduce(&node, &msg.data).await,
            relay::RENDEZVOUS1 => self.rendezvous(&node, &msg.data).await,
            _ => {
                stream::deliver(&self.streams, msg);
            }
        }
    }

    async fn extend(&self, node: &Arc<Node>, data: Vec<u8>) {
        if data.len() <= 32 + ONIONSKIN_LEN || !matches!(*self.next.lock().unwrap(), Next::None) {
            return self.destroy(true, true).await;
        }
        let pubkey: [u8; 32] = data[..32].try_into().unwrap();
        let onionskin = data[32..32 + ONIONSKIN_LEN].to_vec();
        let addr = String::from_utf8_lossy(&data[32 + ONIONSKIN_LEN..]).into_owned();
        let me = self.arc();
        let node = node.clone();
        // Соединение может занять секунды — не держим читателя канала.
        tokio::spawn(async move {
            let ch = match node.channel_to(&addr, pubkey).await {
                Ok(ch) => ch,
                Err(_) => return me.destroy(true, false).await,
            };
            let id = ch.alloc_circ_id();
            node.insert(ch.id, id, Entry::Next(me.clone()));
            *me.next.lock().unwrap() = Next::Pending(ch.clone(), id);
            if ch.send(Cell::new(id, cmd::CREATE, &onionskin)).await.is_err() {
                me.destroy(true, false).await;
            }
        });
    }

    async fn begin_exit(&self, node: &Arc<Node>, msg: RelayMsg) {
        let sid = msg.stream_id;
        if !node.exit.enabled() {
            let _ = self.send_backward_msg(RelayMsg::new(relay::END, sid, b"exit disabled".to_vec())).await;
            return;
        }
        let target = String::from_utf8_lossy(&msg.data).into_owned();
        let s = Stream::open(&self.streams, sid, Arc::new(HopSink(self.me.clone())));
        let policy = node.exit.clone();
        tokio::spawn(async move {
            match policy.connect(&target).await {
                Ok(tcp) => {
                    if s.connected().await.is_ok() {
                        stream::pump(s, tcp).await;
                    }
                }
                Err(e) => s.end(&e.to_string()).await,
            }
        });
    }

    async fn begin_dir(&self, node: &Arc<Node>, sid: u16) {
        let s = Stream::open(&self.streams, sid, Arc::new(HopSink(self.me.clone())));
        let body = serde_json::to_vec(&node.directory()).unwrap_or_default();
        tokio::spawn(async move {
            if s.connected().await.is_ok() && s.write(&body).await.is_ok() {
                s.end("").await;
            }
        });
    }

    async fn establish_intro(&self, node: &Arc<Node>, data: &[u8]) {
        if data.len() < 96 {
            return self.destroy(true, true).await;
        }
        let id: [u8; 32] = data[..32].try_into().unwrap();
        // Подпись над привязкой к этой цепи: чужую подпись сюда не перенести.
        if !ServiceId(id).verify(&intro_message(&self.binding), &data[32..96]) {
            return self.destroy(true, true).await;
        }
        node.intros.lock().unwrap().insert(id, self.me.clone());
        let _ = self.send_backward_msg(RelayMsg::new(relay::INTRO_ESTABLISHED, 0, Vec::new())).await;
    }

    async fn introduce(&self, node: &Arc<Node>, data: &[u8]) {
        if data.len() < 32 + 20 + ONIONSKIN_LEN {
            return self.destroy(true, true).await;
        }
        let id: [u8; 32] = data[..32].try_into().unwrap();
        let cookie: [u8; 20] = data[32..52].try_into().unwrap();
        let service = node.intros.lock().unwrap().get(&id).and_then(|w| w.upgrade());
        let status = match service {
            Some(svc) => {
                {
                    let mut rv = node.rendezvous.lock().unwrap();
                    if rv.len() >= MAX_PENDING_RENDEZVOUS {
                        rv.retain(|_, w| w.strong_count() > 0);
                    }
                    if rv.len() < MAX_PENDING_RENDEZVOUS {
                        rv.insert(cookie, self.me.clone());
                    }
                }
                let ok = svc
                    .send_backward_msg(RelayMsg::new(relay::INTRODUCE2, 0, data[32..].to_vec()))
                    .await
                    .is_ok();
                if ok { 0u8 } else { 2 }
            }
            None => 1,
        };
        let _ = self.send_backward_msg(RelayMsg::new(relay::INTRODUCE_ACK, 0, vec![status])).await;
    }

    async fn rendezvous(&self, node: &Arc<Node>, data: &[u8]) {
        if data.len() < 20 + REPLY_LEN {
            return self.destroy(true, true).await;
        }
        let cookie: [u8; 20] = data[..20].try_into().unwrap();
        let client = node.rendezvous.lock().unwrap().remove(&cookie).and_then(|w| w.upgrade());
        let Some(client) = client else { return self.destroy(true, true).await };
        let free = {
            let mut cn = client.next.lock().unwrap();
            let free = matches!(*cn, Next::None);
            if free {
                *cn = Next::Splice(self.me.clone());
            }
            free
        };
        if !free {
            return self.destroy(true, true).await;
        }
        *self.next.lock().unwrap() = Next::Splice(Arc::downgrade(&client));
        let _ = client
            .send_backward_msg(RelayMsg::new(relay::RENDEZVOUS2, 0, data[20..20 + REPLY_LEN].to_vec()))
            .await;
    }

    /// `destroy` для запуска отдельной задачей (рекурсия через склейку).
    fn destroy_boxed(self: Arc<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move { self.destroy(true, true).await })
    }

    /// Разобрать цепь. `to_prev`/`to_next` — слать ли DESTROY соседям.
    pub(crate) async fn destroy(&self, to_prev: bool, to_next: bool) {
        if self.dead.swap(true, Ordering::Relaxed) {
            return;
        }
        stream::close_all(&self.streams, "circuit closed");
        let Some(node) = self.node.upgrade() else { return };
        node.remove(self.prev.id, self.prev_id);
        if to_prev {
            let _ = self.prev.send(Cell::new(self.prev_id, cmd::DESTROY, &[])).await;
        }
        let next = std::mem::replace(&mut *self.next.lock().unwrap(), Next::None);
        match next {
            Next::Hop(ch, id) | Next::Pending(ch, id) => {
                node.remove(ch.id, id);
                if to_next {
                    let _ = ch.send(Cell::new(id, cmd::DESTROY, &[])).await;
                }
            }
            Next::Splice(w) => {
                if let Some(other) = w.upgrade() {
                    // Другая половина склейки умирает целиком.
                    tokio::spawn(other.destroy_boxed());
                }
            }
            Next::None => {}
        }
    }
}

/// Сток потоков на хопе: сообщения уходят назад к началу цепи.
struct HopSink(Weak<RelayCirc>);

#[async_trait]
impl MsgSink for HopSink {
    async fn send_msg(&self, msg: RelayMsg) -> Result<()> {
        match self.0.upgrade() {
            Some(rc) => rc.send_backward_msg(msg).await,
            None => Err(overnet_core::Error::Link("circuit closed".into())),
        }
    }
}
