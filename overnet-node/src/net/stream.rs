//! Потоки внутри цепи: BEGIN → CONNECTED → DATA… → END, с окном как в Tor.
//!
//! Отправитель может держать в пути не больше `SEND_WINDOW` ячеек потока;
//! получатель после каждых `SENDME_INC` прочитанных ячеек шлёт SENDME. Так быстрый
//! источник не забивает память релеев и медленного получателя.
//!
//! Один и тот же код работает на любом конце: у клиента, на выходе и у сервиса.
//! Разница только в том, куда уходят сообщения (`MsgSink`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Semaphore};

use overnet_core::cell::{relay, RelayMsg, MAX_DATA};
use overnet_core::{Error, Result};

pub const SEND_WINDOW: usize = 500;
pub const SENDME_INC: u32 = 50;

/// Куда отправлять сообщения потока (в нужную сторону нужной цепи).
#[async_trait]
pub trait MsgSink: Send + Sync {
    async fn send_msg(&self, msg: RelayMsg) -> Result<()>;
}

pub enum StreamEvent {
    Connected,
    Data(Vec<u8>),
    End(String),
}

/// Входящая сторона потока в таблице цепи.
pub struct StreamIn {
    tx: mpsc::UnboundedSender<StreamEvent>,
    window: Arc<Semaphore>,
}

pub type StreamMap = Arc<Mutex<HashMap<u16, StreamIn>>>;

pub fn new_map() -> StreamMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Отдать сообщение потоку. `false` — такого потока нет или это не потоковая команда.
pub fn deliver(map: &StreamMap, msg: RelayMsg) -> bool {
    let mut m = map.lock().unwrap();
    let Some(s) = m.get(&msg.stream_id) else { return false };
    match msg.cmd {
        relay::DATA => {
            let _ = s.tx.send(StreamEvent::Data(msg.data));
        }
        relay::CONNECTED => {
            let _ = s.tx.send(StreamEvent::Connected);
        }
        relay::SENDME => s.window.add_permits(SENDME_INC as usize),
        relay::END => {
            let _ = s.tx.send(StreamEvent::End(String::from_utf8_lossy(&msg.data).into_owned()));
            m.remove(&msg.stream_id);
        }
        _ => return false,
    }
    true
}

/// Закрыть все потоки цепи (цепь умерла).
pub fn close_all(map: &StreamMap, why: &str) {
    for (_, s) in map.lock().unwrap().drain() {
        let _ = s.tx.send(StreamEvent::End(why.to_string()));
        s.window.close();
    }
}

/// Убирает поток из таблицы и шлёт END, когда обе половины отпущены.
struct Guard {
    id: u16,
    map: StreamMap,
    sink: Arc<dyn MsgSink>,
    ended: AtomicBool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.map.lock().unwrap().remove(&self.id);
        if !self.ended.swap(true, Ordering::Relaxed) {
            let sink = self.sink.clone();
            let id = self.id;
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let _ = sink.send_msg(RelayMsg::new(relay::END, id, Vec::new())).await;
                });
            }
        }
    }
}

/// Конец потока.
pub struct Stream {
    reader: StreamReader,
    writer: StreamWriter,
}

pub struct StreamReader {
    rx: mpsc::UnboundedReceiver<StreamEvent>,
    delivered: u32,
    guard: Arc<Guard>,
}

#[derive(Clone)]
pub struct StreamWriter {
    window: Arc<Semaphore>,
    guard: Arc<Guard>,
}

impl Stream {
    /// Зарегистрировать поток `id` в таблице цепи.
    pub fn open(map: &StreamMap, id: u16, sink: Arc<dyn MsgSink>) -> Stream {
        let (tx, rx) = mpsc::unbounded_channel();
        let window = Arc::new(Semaphore::new(SEND_WINDOW));
        map.lock().unwrap().insert(id, StreamIn { tx, window: window.clone() });
        let guard = Arc::new(Guard { id, map: map.clone(), sink, ended: AtomicBool::new(false) });
        Stream {
            reader: StreamReader { rx, delivered: 0, guard: guard.clone() },
            writer: StreamWriter { window, guard },
        }
    }

    pub fn id(&self) -> u16 {
        self.writer.guard.id
    }

    /// Дождаться CONNECTED (или END с причиной).
    pub async fn wait_connected(&mut self, timeout: Duration) -> Result<()> {
        let ev = tokio::time::timeout(timeout, self.reader.rx.recv())
            .await
            .map_err(|_| Error::Link("stream: no answer".into()))?;
        match ev {
            Some(StreamEvent::Connected) => Ok(()),
            Some(StreamEvent::End(why)) => {
                self.writer.guard.ended.store(true, Ordering::Relaxed);
                Err(Error::Link(if why.is_empty() { "stream refused".into() } else { why }))
            }
            Some(StreamEvent::Data(_)) => Err(Error::Link("stream: data before CONNECTED".into())),
            None => Err(Error::Link("circuit closed".into())),
        }
    }

    pub async fn connected(&self) -> Result<()> {
        self.writer.guard.sink.send_msg(RelayMsg::new(relay::CONNECTED, self.id(), Vec::new())).await
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        self.writer.write(data).await
    }

    pub async fn read(&mut self) -> Option<Vec<u8>> {
        self.reader.read().await
    }

    /// Прочитать всё до END (для коротких ответов: каталог, HTTP).
    pub async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(chunk) = self.read().await {
            out.extend_from_slice(&chunk);
            if out.len() > limit {
                return Err(Error::Link("stream: answer too large".into()));
            }
        }
        Ok(out)
    }

    pub async fn end(&self, why: &str) {
        self.writer.end(why).await
    }

    pub fn split(self) -> (StreamReader, StreamWriter) {
        (self.reader, self.writer)
    }
}

impl StreamReader {
    pub async fn read(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.rx.recv().await? {
                StreamEvent::Data(d) => {
                    self.delivered += 1;
                    if self.delivered % SENDME_INC == 0 {
                        let g = &self.guard;
                        let _ = g.sink.send_msg(RelayMsg::new(relay::SENDME, g.id, Vec::new())).await;
                    }
                    return Some(d);
                }
                StreamEvent::End(_) => {
                    self.guard.ended.store(true, Ordering::Relaxed);
                    return None;
                }
                StreamEvent::Connected => continue,
            }
        }
    }
}

impl StreamWriter {
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        for chunk in data.chunks(MAX_DATA) {
            let permit = self
                .window
                .acquire()
                .await
                .map_err(|_| Error::Link("stream closed".into()))?;
            permit.forget();
            let g = &self.guard;
            g.sink.send_msg(RelayMsg::new(relay::DATA, g.id, chunk.to_vec())).await?;
        }
        Ok(())
    }

    pub async fn end(&self, why: &str) {
        let g = &self.guard;
        g.map.lock().unwrap().remove(&g.id);
        if !g.ended.swap(true, Ordering::Relaxed) {
            let _ = g.sink.send_msg(RelayMsg::new(relay::END, g.id, why.as_bytes().to_vec())).await;
        }
    }
}

/// Гонять байты между потоком и TCP-соединением, пока одна сторона не закроется.
pub async fn pump(stream: Stream, tcp: TcpStream) {
    let (mut tr, mut tw) = tcp.into_split();
    let (mut sr, sw) = stream.split();
    let up = async {
        let mut buf = vec![0u8; MAX_DATA * 8];
        loop {
            match tr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sw.write(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        sw.end("").await;
    };
    let down = async {
        while let Some(d) = sr.read().await {
            if tw.write_all(&d).await.is_err() {
                break;
            }
        }
        let _ = tw.shutdown().await;
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сток, который складывает сообщения в канал (вместо цепи).
    struct ChanSink(mpsc::UnboundedSender<RelayMsg>);

    #[async_trait]
    impl MsgSink for ChanSink {
        async fn send_msg(&self, msg: RelayMsg) -> Result<()> {
            self.0.send(msg).map_err(|_| Error::Link("closed".into()))
        }
    }

    #[tokio::test]
    async fn window_blocks_until_sendme() {
        let map = new_map();
        let (tx, mut out) = mpsc::unbounded_channel();
        let s = Stream::open(&map, 3, Arc::new(ChanSink(tx)));
        // Окно — ровно SEND_WINDOW ячеек.
        s.write(&vec![1u8; MAX_DATA * SEND_WINDOW]).await.unwrap();
        let blocked = tokio::time::timeout(Duration::from_millis(100), s.write(b"x")).await;
        assert!(blocked.is_err(), "past the window the writer must wait");
        assert!(deliver(&map, RelayMsg::new(relay::SENDME, 3, Vec::new())));
        tokio::time::timeout(Duration::from_millis(100), s.write(b"x")).await.unwrap().unwrap();
        let mut n = 0;
        while let Ok(m) = out.try_recv() {
            assert_eq!((m.cmd, m.stream_id), (relay::DATA, 3));
            n += 1;
        }
        assert_eq!(n, SEND_WINDOW + 1);
    }

    #[tokio::test]
    async fn reader_sends_sendme_and_sees_end() {
        let map = new_map();
        let (tx, mut out) = mpsc::unbounded_channel();
        let mut s = Stream::open(&map, 9, Arc::new(ChanSink(tx)));
        for i in 0..SENDME_INC {
            assert!(deliver(&map, RelayMsg::new(relay::DATA, 9, vec![i as u8])));
        }
        assert!(deliver(&map, RelayMsg::new(relay::END, 9, Vec::new())));
        let got = s.read_to_end(1 << 20).await.unwrap();
        assert_eq!(got.len(), SENDME_INC as usize);
        let m = out.try_recv().unwrap();
        assert_eq!(m.cmd, relay::SENDME);
        // Поток закрыт удалённо — свой END не шлём.
        drop(s);
        tokio::task::yield_now().await;
        assert!(out.try_recv().is_err());
        assert!(map.lock().unwrap().is_empty());
    }
}
