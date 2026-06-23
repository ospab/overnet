//! Защищённая сессия overnet поверх любого `Link` — «сам протокол».
//!
//! Рукопожатие Noise (паттерн **XX**) + транспортный режим (шифрование сообщений).
//! Крипту НЕ катаем: драйвим библиотеку `snow` (тот же стек, что в ostp —
//! X25519 + ChaCha20-Poly1305 + SHA-256).
//!
//! XX = взаимная аутентификация; статические ключи сторон передаются в ходе
//! рукопожатия и доступны после (`remote_static`) — для сверки с ожидаемым
//! криптоадресом собеседника.

use snow::{params::NoiseParams, Builder, HandshakeState, TransportState};

use crate::{Error, Link, Result};

/// Паттерн и примитивы. Совпадают с остальным стеком overnet/ostp.
const PARAMS: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
const MAX_MSG: usize = 65535;

fn nerr<E: std::fmt::Debug>(e: E) -> Error {
    Error::Link(format!("noise: {e:?}"))
}

/// Статическая ключевая пара транспорта (X25519), используемая Noise.
///
/// TODO(идентичность): связать с ed25519-`Identity` подписью этого статического
/// ключа, чтобы адрес (= hash(ed25519)) удостоверял и транспортный ключ. Пока
/// транспортный ключ отдельный — это честно отражено, не подделка.
#[derive(Clone)]
pub struct TransportKey {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl TransportKey {
    /// Сгенерировать новую транспортную пару.
    pub fn generate() -> Result<Self> {
        let params: NoiseParams = PARAMS.parse().map_err(nerr)?;
        let kp = Builder::new(params).generate_keypair().map_err(nerr)?;
        Ok(TransportKey { private: kp.private, public: kp.public })
    }
}

/// Установленная зашифрованная сессия поверх `Link`.
pub struct Session<L: Link> {
    transport: TransportState,
    link: L,
    remote_static: Vec<u8>,
}

impl<L: Link> Session<L> {
    /// Инициатор (клиент): XX — `-> e` ; `<- e,ee,s,es` ; `-> s,se`.
    pub async fn initiate(link: L, key: &TransportKey) -> Result<Self> {
        let params: NoiseParams = PARAMS.parse().map_err(nerr)?;
        let mut hs = Builder::new(params)
            .local_private_key(&key.private)
            .build_initiator()
            .map_err(nerr)?;
        let mut buf = vec![0u8; MAX_MSG];

        // -> e
        let n = hs.write_message(&[], &mut buf).map_err(nerr)?;
        link.send(&buf[..n]).await?;
        // <- e, ee, s, es
        let msg = link.recv().await?;
        hs.read_message(&msg, &mut buf).map_err(nerr)?;
        // -> s, se
        let n = hs.write_message(&[], &mut buf).map_err(nerr)?;
        link.send(&buf[..n]).await?;

        Self::finish(hs, link)
    }

    /// Ответчик (сервер).
    pub async fn respond(link: L, key: &TransportKey) -> Result<Self> {
        let params: NoiseParams = PARAMS.parse().map_err(nerr)?;
        let mut hs = Builder::new(params)
            .local_private_key(&key.private)
            .build_responder()
            .map_err(nerr)?;
        let mut buf = vec![0u8; MAX_MSG];

        // <- e
        let msg = link.recv().await?;
        hs.read_message(&msg, &mut buf).map_err(nerr)?;
        // -> e, ee, s, es
        let n = hs.write_message(&[], &mut buf).map_err(nerr)?;
        link.send(&buf[..n]).await?;
        // <- s, se
        let msg = link.recv().await?;
        hs.read_message(&msg, &mut buf).map_err(nerr)?;

        Self::finish(hs, link)
    }

    fn finish(hs: HandshakeState, link: L) -> Result<Self> {
        let remote_static = hs.get_remote_static().map(|s| s.to_vec()).unwrap_or_default();
        let transport = hs.into_transport_mode().map_err(nerr)?;
        Ok(Session { transport, link, remote_static })
    }

    /// Статический публичный ключ собеседника (для сверки с ожидаемым адресом).
    pub fn remote_static(&self) -> &[u8] {
        &self.remote_static
    }

    /// Зашифровать и отправить сообщение.
    pub async fn send(&mut self, msg: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; msg.len() + 16]; // +16 = тег Poly1305
        let n = self.transport.write_message(msg, &mut buf).map_err(nerr)?;
        self.link.send(&buf[..n]).await
    }

    /// Принять и расшифровать сообщение.
    pub async fn recv(&mut self) -> Result<Vec<u8>> {
        let ct = self.link.recv().await?;
        let mut buf = vec![0u8; ct.len()];
        let n = self.transport.read_message(&ct, &mut buf).map_err(nerr)?;
        buf.truncate(n);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::loopback::Loopback;

    #[tokio::test]
    async fn handshake_and_bidirectional_over_loopback() {
        let ks = TransportKey::generate().unwrap();
        let kc = TransportKey::generate().unwrap();
        let ks_pub = ks.public.clone();
        let (la, lb) = Loopback::pair();

        let server = tokio::spawn(async move {
            let mut s = Session::respond(lb, &ks).await.unwrap();
            let m = s.recv().await.unwrap();
            s.send(&m).await.unwrap(); // эхо
            s.remote_static().to_vec()
        });

        let mut c = Session::initiate(la, &kc).await.unwrap();
        c.send(b"ping").await.unwrap();
        assert_eq!(c.recv().await.unwrap(), b"ping");

        // взаимная аутентификация: сервер увидел статический ключ клиента
        let seen_client = server.await.unwrap();
        assert_eq!(seen_client, kc.public);
        // и клиент увидел ключ сервера
        assert_eq!(c.remote_static(), ks_pub.as_slice());
    }
}
