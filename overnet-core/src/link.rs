//! Абстракция транспорта — самое важное архитектурное свойство overnet.
//!
//! `Link` = одно ребро графа между двумя соседними узлами. Ядру всё равно, что
//! под ним: TCP, туннель ostp (Reality), WiFi-антенна, LoRa, Bluetooth, sneakernet.
//! Когда душат интернет — те же адреса/роутинг/onion переезжают на другой `Link`.

use async_trait::async_trait;

use crate::Result;

/// Свойства линка — нужны маршрутизации (по LoRa не погонишь видео).
#[derive(Clone, Copy, Debug)]
pub struct LinkProps {
    /// Примерная задержка в одну сторону, мс.
    pub latency_ms: u32,
    /// Примерная пропускная способность, байт/с (0 = неизвестно).
    pub bandwidth_bps: u64,
    /// Надёжная ли доставка (TCP — да, сырое радио — нет).
    pub reliable: bool,
    /// Направленный ли носитель (антенна точка-точка).
    pub directional: bool,
}

/// Двунаправленный канал доставки кадров соседнему узлу.
#[async_trait]
pub trait Link: Send + Sync {
    /// Отправить кадр соседу.
    async fn send(&self, frame: &[u8]) -> Result<()>;
    /// Принять следующий кадр от соседа.
    async fn recv(&self) -> Result<Vec<u8>>;
    /// Максимальный размер кадра.
    fn mtu(&self) -> usize;
    /// Характеристики линка для маршрутизации.
    fn properties(&self) -> LinkProps;
}

/// In-process `Link` для тестов — пара связанных endpoint'ов без сети.
pub mod loopback {
    use super::*;
    use crate::Error;
    use tokio::sync::{mpsc, Mutex};

    pub struct Loopback {
        tx: mpsc::UnboundedSender<Vec<u8>>,
        rx: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    }

    impl Loopback {
        /// Создать пару связанных endpoint'ов: что отправлено в `a`, приходит в `b`.
        pub fn pair() -> (Loopback, Loopback) {
            let (tx_a, rx_b) = mpsc::unbounded_channel();
            let (tx_b, rx_a) = mpsc::unbounded_channel();
            (
                Loopback { tx: tx_a, rx: Mutex::new(rx_a) },
                Loopback { tx: tx_b, rx: Mutex::new(rx_b) },
            )
        }
    }

    #[async_trait]
    impl Link for Loopback {
        async fn send(&self, frame: &[u8]) -> Result<()> {
            self.tx
                .send(frame.to_vec())
                .map_err(|e| Error::Link(e.to_string()))
        }

        async fn recv(&self) -> Result<Vec<u8>> {
            let mut rx = self.rx.lock().await;
            rx.recv().await.ok_or_else(|| Error::Link("link closed".into()))
        }

        fn mtu(&self) -> usize {
            65535
        }

        fn properties(&self) -> LinkProps {
            LinkProps { latency_ms: 0, bandwidth_bps: 0, reliable: true, directional: false }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn frame_roundtrips_regardless_of_transport() {
            let (a, b) = Loopback::pair();
            a.send(b"hello overnet").await.unwrap();
            assert_eq!(b.recv().await.unwrap(), b"hello overnet");
        }
    }
}
