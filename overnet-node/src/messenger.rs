//! Мессенджер overnet: модель «почтовый ящик» (store-and-forward), E2E.
//!
//! Отправитель шифрует сообщение НА КЛЮЧ получателя (onion-слой к его pubkey) и
//! кладёт в его инбокс на узле-ящике. Получатель опрашивает ящик и расшифровывает
//! локально. Узел-ящик видит только шифротекст + pubkey получателя — содержимое не
//! читает (E2E). Метаданные (у кого есть ящик, размеры/тайминги) видны — это честно.
//!
//! Адрес пользователя = его onion-pubkey (X25519). TODO: связать с ed25519-Identity.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};

use overnet_core::onion::{self, OnionKey, Peeled};
use overnet_core::{Error, Link, Result};
use overnet_link_tcp::{TcpLink, TcpListenerLink};

use crate::bootstrap::NodeInfo;
use crate::router::Router;

/// REGISTER-маркер транспорта (как в web): узел сообщает соседу свой pubkey.
const REGISTER_TAG: u8 = 0xFF;

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(o)
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[derive(Serialize, Deserialize, Debug)]
enum MsgOp {
    /// Положить шифротекст `ct` в ящик получателя `to` (оба hex).
    Send { to: String, ct: String },
    /// Забрать сообщения для `who` (hex pubkey).
    Fetch { who: String },
}

#[derive(Serialize, Deserialize, Debug)]
struct MsgRequest {
    reply_to: String,
    reply_path: Vec<String>,
    op: MsgOp,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct MsgReply {
    /// Для Fetch — список шифротекстов (hex). Для Send — пусто.
    messages: Vec<String>,
}

/// Зашифровать сообщение НА КЛЮЧ получателя (E2E). Возвращает hex-шифротекст.
pub fn encrypt_for(recipient_pub: [u8; 32], plaintext: &[u8]) -> Result<String> {
    Ok(hex_encode(&onion::wrap(&[recipient_pub], plaintext)?))
}

/// Расшифровать сообщение своим ключом.
pub fn decrypt(my_key: &OnionKey, ct_hex: &str) -> Result<Vec<u8>> {
    let ct = hex_decode(ct_hex).ok_or_else(|| Error::Link("bad ciphertext hex".into()))?;
    match onion::peel(my_key, &ct)? {
        Peeled::Deliver(pt) => Ok(pt),
        Peeled::Forward { .. } => Err(Error::Link("message not addressed to us".into())),
    }
}

type Inbox = Arc<Mutex<HashMap<String, Vec<String>>>>; // recipient_hex -> [ct_hex]

/// Запустить узел-ящик: принимает Send (положить в инбокс) и Fetch (забрать).
pub async fn run_mailbox(listener: TcpListenerLink, key: OnionKey) -> Result<()> {
    let inbox: Inbox = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let router = Arc::new(Router::new(key, tx));
    let responder = router.clone();

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let req: MsgRequest = match serde_json::from_slice(&msg) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let reply = match req.op {
                MsgOp::Send { to, ct } => {
                    inbox.lock().await.entry(to).or_default().push(ct);
                    MsgReply::default()
                }
                MsgOp::Fetch { who } => {
                    // Забрали — удалили (простая семантика; later: ack/persist).
                    let messages = inbox.lock().await.remove(&who).unwrap_or_default();
                    MsgReply { messages }
                }
            };
            let reply_bytes = match serde_json::to_vec(&reply) {
                Ok(b) => b,
                Err(_) => continue,
            };
            // Обратный путь: reply_path + reply_to.
            let mut reverse = Vec::new();
            for h in &req.reply_path {
                if let Some(pk) = hex_decode_32(h) {
                    reverse.push(pk);
                }
            }
            if let Some(pk) = hex_decode_32(&req.reply_to) {
                reverse.push(pk);
            }
            if reverse.is_empty() {
                continue;
            }
            if let Ok(pkt) = onion::wrap(&reverse, &reply_bytes) {
                let _ = responder.send_to_neighbor(&reverse[0], pkt).await;
            }
        }
    });

    router.serve(listener).await
}

/// Один onion-RPC к ящику (direct, 1 хоп) с эфемерным транспортным ключом.
async fn mailbox_rpc(mailbox: &NodeInfo, op: MsgOp) -> Result<MsgReply> {
    let mailbox_pub =
        hex_decode_32(&mailbox.pubkey).ok_or_else(|| Error::Link("bad mailbox pubkey".into()))?;
    let tkey = OnionKey::generate(); // эфемерная транспортная личность
    let tpub = tkey.public();

    let link = TcpLink::connect(&mailbox.address).await?;
    let mut reg = Vec::with_capacity(33);
    reg.push(REGISTER_TAG);
    reg.extend_from_slice(&tpub);
    link.send(&reg).await?;

    let req = MsgRequest {
        reply_to: hex_encode(&tpub),
        reply_path: Vec::new(),
        op,
    };
    let bytes = serde_json::to_vec(&req).map_err(|_| Error::Link("json encode".into()))?;
    let pkt = onion::wrap(&[mailbox_pub], &bytes)?;
    link.send(&pkt).await?;

    let frame = tokio::time::timeout(Duration::from_secs(10), link.recv())
        .await
        .map_err(|_| Error::Link("mailbox timeout".into()))??;
    match onion::peel(&tkey, &frame)? {
        Peeled::Deliver(b) => {
            serde_json::from_slice(&b).map_err(|_| Error::Link("bad mailbox reply".into()))
        }
        Peeled::Forward { .. } => Err(Error::Link("unexpected forward in reply".into())),
    }
}

/// Отправить сообщение получателю (E2E) через его ящик.
pub async fn send_message(
    mailbox: &NodeInfo,
    recipient_pub: [u8; 32],
    plaintext: &[u8],
) -> Result<()> {
    let ct = encrypt_for(recipient_pub, plaintext)?;
    mailbox_rpc(
        mailbox,
        MsgOp::Send {
            to: hex_encode(&recipient_pub),
            ct,
        },
    )
    .await?;
    Ok(())
}

/// Забрать свои сообщения из ящика и расшифровать.
pub async fn fetch_inbox(mailbox: &NodeInfo, my_key: &OnionKey) -> Result<Vec<Vec<u8>>> {
    let reply = mailbox_rpc(
        mailbox,
        MsgOp::Fetch {
            who: hex_encode(&my_key.public()),
        },
    )
    .await?;
    let mut out = Vec::new();
    for ct in reply.messages {
        if let Ok(pt) = decrypt(my_key, &ct) {
            out.push(pt);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_cannot_read_content() {
        let bob = OnionKey::generate();
        let ct = encrypt_for(bob.public(), b"secret").unwrap();
        // шифротекст не содержит открытый текст
        assert!(!ct.contains(&hex_encode(b"secret")));
        // чужим ключом не расшифровать, своим — да
        assert!(decrypt(&OnionKey::generate(), &ct).is_err());
        assert_eq!(decrypt(&bob, &ct).unwrap(), b"secret");
    }

    #[tokio::test]
    async fn mailbox_e2e_send_and_fetch() {
        let mb_key = OnionKey::generate();
        let mb_pub = mb_key.public();
        let listener = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(run_mailbox(listener, mb_key));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mailbox = NodeInfo {
            pubkey: hex_encode(&mb_pub),
            address: addr,
            role: "mailbox".into(),
            name: String::new(),
        };

        let bob = OnionKey::generate();
        let bob_pub = bob.public();

        // Alice → Bob
        send_message(&mailbox, bob_pub, b"privet bob").await.unwrap();
        // Bob забирает
        let msgs = fetch_inbox(&mailbox, &bob).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], b"privet bob");
        // повторный fetch пуст (сообщения забраны)
        assert!(fetch_inbox(&mailbox, &bob).await.unwrap().is_empty());
    }
}
