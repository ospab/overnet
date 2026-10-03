//! `overnet-node` — рантайм узла: связывает `Link` + сессию в работающие
//! клиент и сервер. На этой стадии — зашифрованное эхо (Noise поверх TCP),
//! чтобы доказать связь туда-обратно. Onion и маршрутизация — следующие вехи.

pub use overnet_core::session::{Session, TransportKey};
use overnet_core::Result;
use overnet_link_tcp::{TcpLink, TcpListenerLink};

pub mod router;
pub mod bootstrap;
pub mod web;
pub mod guard;
pub mod messenger;
pub mod net;

/// Обслуживать входящие соединения: на каждое — рукопожатие (ответчик) и
/// эхо принятых сообщений. Каждое соединение — в своей задаче.
pub async fn serve_echo(listener: TcpListenerLink, key: TransportKey) -> Result<()> {
    loop {
        let link = listener.accept().await?;
        let key = key.clone();
        tokio::spawn(async move {
            match Session::respond(link, &key).await {
                Ok(mut sess) => {
                    while let Ok(msg) = sess.recv().await {
                        if sess.send(&msg).await.is_err() {
                            break;
                        }
                    }
                }
                Err(e) => eprintln!("handshake failed: {e}"),
            }
        });
    }
}

/// Поднять эхо-сервер на адресе.
pub async fn run_server(bind: &str, key: TransportKey) -> Result<()> {
    let listener = TcpListenerLink::bind(bind).await?;
    serve_echo(listener, key).await
}

/// Подключиться к серверу, провести рукопожатие, отправить `payload` и вернуть эхо.
pub async fn ping(addr: &str, key: &TransportKey, payload: &[u8]) -> Result<Vec<u8>> {
    let link = TcpLink::connect(addr).await?;
    let mut sess = Session::initiate(link, key).await?;
    sess.send(payload).await?;
    sess.recv().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn encrypted_echo_roundtrip_over_tcp() {
        let key_s = TransportKey::generate().unwrap();
        let key_c = TransportKey::generate().unwrap();

        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _ = serve_echo(listener, key_s).await;
        });

        let reply = ping(&addr, &key_c, b"hello overnet").await.unwrap();
        assert_eq!(reply, b"hello overnet");
    }
}
