//! `overnet-link-ostp` — `Link` поверх обфусцированного туннеля `ostp` (фаза 1).
//!
//! **Переиспользуем, не переписываем.** Внутри — `ostp_core::protocol::ProtocolMachine`:
//! sans-io машина, которая делает Noise-рукопожатие, шифрует/паддит/обфусцирует
//! датаграммы и тянет надёжность (ACK/NACK/ретрансмит/congestion). Снаружи мы
//! выставляем обычный `overnet_core::Link`, поверх которого едут onion-слои overnet.
//!
//! Архитектура — «актор»: машину гоняем над несущим `Link` (carrier: TCP/Reality),
//! три петли (RX/TX/Tick) скармливают ей события и рассылают её датаграммы. Машина
//! `&mut`, поэтому за `std::sync::Mutex`; блокировку держим только на синхронный
//! `on_event`, await'ы — вне блокировки.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;

use ostp_core::{
    NoiseRole, OstpEvent, OstpState, PaddingStrategy, ProtocolAction, ProtocolConfig,
    ProtocolMachine,
};
use overnet_core::{Error, Link, LinkProps, Result};

fn perr(e: ostp_core::protocol::ProtocolError) -> Error {
    Error::Link(format!("ostp: {e:?}"))
}

/// Конфиг туннеля. Параметры — как в эталонных тестах ostp; общими для сторон
/// должны быть `psk`, `session_id`, `obfuscation_key` (роли — Initiator/Responder).
fn make_config(role: NoiseRole, psk: [u8; 32], session_id: u32, obf: [u8; 8]) -> ProtocolConfig {
    ProtocolConfig {
        role,
        psk,
        session_id,
        handshake_payload: Vec::new(),
        max_padding: 64,
        padding_strategy: PaddingStrategy::Adaptive,
        obfuscation_key: obf,
        max_reorder: 128,
        max_reorder_buffer: 256,
        ack_delay_ms: 5,
        rto_ms: 100,
        max_retries: 4,
        max_sent_history: 1024,
        handshake_pad_min: 8,
        handshake_pad_max: 32,
        mtu: 1400,
    }
}

/// Развернуть (возможно вложенный) `ProtocolAction` в плоский список листьев.
fn flatten(action: ProtocolAction, out: &mut Vec<ProtocolAction>) {
    match action {
        ProtocolAction::Multiple(v) => {
            for a in v {
                flatten(a, out);
            }
        }
        other => out.push(other),
    }
}

/// Исполнить действия машины: датаграммы — в несущий `Link`, доставленные
/// приложению payload'ы — в канал доставки.
async fn dispatch<C: Link>(
    action: ProtocolAction,
    carrier: &C,
    deliver_tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) -> Result<()> {
    let mut leaves = Vec::new();
    flatten(action, &mut leaves);
    for leaf in leaves {
        match leaf {
            ProtocolAction::SendDatagram(d) => carrier.send(&d).await?,
            ProtocolAction::HandshakePayload(_, Some(resp)) => carrier.send(&resp).await?,
            ProtocolAction::HandshakePayload(_, None) => {}
            ProtocolAction::DeliverApp(_stream, payload) => {
                let _ = deliver_tx.send(payload.to_vec());
            }
            ProtocolAction::Noop => {}
            ProtocolAction::Multiple(_) => {} // уже развёрнуто flatten
        }
    }
    Ok(())
}

/// Провести рукопожатие до состояния Established.
async fn drive_handshake<C: Link>(
    machine: &mut ProtocolMachine,
    carrier: &C,
    deliver_tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) -> Result<()> {
    // Initiator на Start шлёт msg1; Responder — Noop и ждёт msg1.
    let first = machine.on_event(OstpEvent::Start).map_err(perr)?;
    dispatch(first, carrier, deliver_tx).await?;

    let mut guard = 0;
    while machine.state() != OstpState::Established {
        let datagram = carrier.recv().await?;
        let action = machine
            .on_event(OstpEvent::Inbound(Bytes::from(datagram)))
            .map_err(perr)?;
        dispatch(action, carrier, deliver_tx).await?;
        guard += 1;
        if guard > 16 {
            return Err(Error::Link("ostp: handshake did not complete".into()));
        }
    }
    Ok(())
}

/// `Link` поверх обфусцированного туннеля `ostp`.
pub struct OstpLink {
    outbound_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    deliver_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
}

impl OstpLink {
    /// Клиентская сторона туннеля (инициатор рукопожатия).
    pub async fn initiator<C: Link + 'static>(
        carrier: C,
        psk: [u8; 32],
        session_id: u32,
        obfuscation_key: [u8; 8],
    ) -> Result<Self> {
        Self::start(carrier, NoiseRole::Initiator, psk, session_id, obfuscation_key).await
    }

    /// Серверная сторона туннеля (ответчик).
    pub async fn responder<C: Link + 'static>(
        carrier: C,
        psk: [u8; 32],
        session_id: u32,
        obfuscation_key: [u8; 8],
    ) -> Result<Self> {
        Self::start(carrier, NoiseRole::Responder, psk, session_id, obfuscation_key).await
    }

    async fn start<C: Link + 'static>(
        carrier: C,
        role: NoiseRole,
        psk: [u8; 32],
        session_id: u32,
        obf: [u8; 8],
    ) -> Result<Self> {
        let carrier = Arc::new(carrier);
        let mut machine = ProtocolMachine::new(make_config(role, psk, session_id, obf)).map_err(perr)?;

        let (deliver_tx, deliver_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

        // Рукопожатие до Established (на голой машине, до петель).
        drive_handshake(&mut machine, &*carrier, &deliver_tx).await?;

        let machine = Arc::new(std::sync::Mutex::new(machine));

        // RX: входящие датаграммы -> Inbound -> доставка/ответные датаграммы.
        {
            let carrier = carrier.clone();
            let machine = machine.clone();
            let deliver_tx = deliver_tx.clone();
            tokio::spawn(async move {
                loop {
                    let datagram = match carrier.recv().await {
                        Ok(d) => d,
                        Err(_) => break,
                    };
                    let action = {
                        let mut m = machine.lock().unwrap();
                        m.on_event(OstpEvent::Inbound(Bytes::from(datagram)))
                    };
                    match action {
                        Ok(a) => {
                            if dispatch(a, &*carrier, &deliver_tx).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // TX: данные приложения -> Outbound -> датаграммы.
        {
            let carrier = carrier.clone();
            let machine = machine.clone();
            let deliver_tx = deliver_tx.clone();
            tokio::spawn(async move {
                while let Some(app) = outbound_rx.recv().await {
                    let action = {
                        let mut m = machine.lock().unwrap();
                        m.on_event(OstpEvent::Outbound(0, Bytes::from(app)))
                    };
                    match action {
                        Ok(a) => {
                            if dispatch(a, &*carrier, &deliver_tx).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // Tick: ретрансмиты и отложенные ACK.
        {
            let carrier = carrier.clone();
            let machine = machine.clone();
            let deliver_tx = deliver_tx.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(20));
                loop {
                    interval.tick().await;
                    let action = {
                        let mut m = machine.lock().unwrap();
                        m.on_event(OstpEvent::Tick)
                    };
                    match action {
                        Ok(a) => {
                            if dispatch(a, &*carrier, &deliver_tx).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        Ok(OstpLink {
            outbound_tx,
            deliver_rx: tokio::sync::Mutex::new(deliver_rx),
        })
    }
}

#[async_trait]
impl Link for OstpLink {
    async fn send(&self, frame: &[u8]) -> Result<()> {
        self.outbound_tx
            .send(frame.to_vec())
            .map_err(|_| Error::Link("ostp link closed".into()))
    }

    async fn recv(&self) -> Result<Vec<u8>> {
        let mut rx = self.deliver_rx.lock().await;
        rx.recv().await.ok_or_else(|| Error::Link("ostp link closed".into()))
    }

    fn mtu(&self) -> usize {
        1200
    }

    fn properties(&self) -> LinkProps {
        LinkProps { latency_ms: 0, bandwidth_bps: 0, reliable: true, directional: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use overnet_link_tcp::{TcpLink, TcpListenerLink};

    #[tokio::test]
    async fn ostp_tunnel_carries_frames_both_ways() {
        let psk = [7u8; 32];
        let sid = 0x1234_5678;
        let obf = [9u8; 8];

        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server = tokio::spawn(async move {
            let carrier = listener.accept().await.unwrap();
            let link = OstpLink::responder(carrier, psk, sid, obf).await.unwrap();
            let msg = link.recv().await.unwrap();
            link.send(&msg).await.unwrap(); // эхо обратно через туннель
            tokio::time::sleep(Duration::from_millis(300)).await; // дать дойти
        });

        let carrier = TcpLink::connect(&addr).await.unwrap();
        let link = OstpLink::initiator(carrier, psk, sid, obf).await.unwrap();
        link.send(b"through the obfuscated ostp tunnel").await.unwrap();

        let got = tokio::time::timeout(Duration::from_secs(5), link.recv())
            .await
            .expect("timeout")
            .expect("link closed");
        assert_eq!(got, b"through the obfuscated ostp tunnel");
        server.await.unwrap();
    }
}
