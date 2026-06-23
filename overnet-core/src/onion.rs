//! Onion-маршрутизация — сердце overnet (Веха 3).
//!
//! Сообщение шифруется слоями, как луковица. Каждый хоп снимает СВОЙ слой и
//! узнаёт только следующий хоп — никогда payload и никогда адресата. Релей,
//! сняв слой, видит лишь «переслать вот это вон туда».
//!
//! Это упрощённый onion (не полный Sphinx): фиксируем суть — per-hop X25519 +
//! AEAD-слои. Строгий Sphinx (фиксированная длина пакета, защита от replay,
//! паддинг до неразличимости) — следующая итерация поверх этого.
//!
//! Крипту НЕ катаем: X25519 (`x25519-dalek`) + ChaCha20-Poly1305
//! (`chacha20poly1305`) + SHA-256 как KDF.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::{Error, Result};

/// Безопасно фиксированный nonce: на КАЖДЫЙ слой берётся свежий эфемерный ключ,
/// значит пара (ключ, nonce) уникальна — переиспользования nonce нет.
const NONCE: [u8; 12] = [0u8; 12];

const TAG_DELIVER: u8 = 0x00; // мы адресат
const TAG_FORWARD: u8 = 0x01; // мы релей: переслать дальше

/// Долговременный onion-ключ узла (X25519).
///
/// TODO(идентичность): связать с ed25519-`Identity` подписью этого ключа, чтобы
/// криптоадрес (= hash(ed25519)) удостоверял и onion-ключ. Пока отдельный.
pub struct OnionKey {
    secret: StaticSecret,
    public: [u8; 32],
}

impl OnionKey {
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(&mut OsRng);
        let public = PublicKey::from(&secret).to_bytes();
        OnionKey { secret, public }
    }

    /// Публичный onion-ключ (его кладут в путь те, кто строит onion).
    pub fn public(&self) -> [u8; 32] {
        self.public
    }
}

/// KDF: общий секрет X25519 -> 32-байтный ключ AEAD (с доменным разделителем).
/// TODO: заменить на HKDF для строгости; для прототипа SHA-256 достаточно.
fn layer_key(shared: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"overnet-onion-v0");
    h.update(shared);
    let d = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&d);
    k
}

/// Запечатать один слой для хопа с публичным ключом `hop_pub`.
/// Формат на проводе: `[эфемерный_pub(32) || ciphertext]`.
fn seal(hop_pub: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let eph = StaticSecret::random_from_rng(&mut OsRng);
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let shared = eph.diffie_hellman(&PublicKey::from(*hop_pub));
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&layer_key(shared.as_bytes())));
    let ct = cipher
        .encrypt(Nonce::from_slice(&NONCE), plaintext)
        .map_err(|e| Error::Link(format!("onion seal: {e}")))?;
    let mut out = Vec::with_capacity(32 + ct.len());
    out.extend_from_slice(&eph_pub);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Снять один слой своим приватным ключом.
fn open(secret: &StaticSecret, packet: &[u8]) -> Result<Vec<u8>> {
    if packet.len() < 32 {
        return Err(Error::Link("onion: пакет короче 32 байт".into()));
    }
    let mut eph_pub = [0u8; 32];
    eph_pub.copy_from_slice(&packet[..32]);
    let shared = secret.diffie_hellman(&PublicKey::from(eph_pub));
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&layer_key(shared.as_bytes())));
    cipher
        .decrypt(Nonce::from_slice(&NONCE), &packet[32..])
        .map_err(|e| Error::Link(format!("onion open: {e}")))
}

/// Построить onion. `path` — публичные onion-ключи хопов по порядку:
/// `[hop1, hop2, ..., адресат]`. Возвращает пакет для ПЕРВОГО хопа.
pub fn wrap(path: &[[u8; 32]], payload: &[u8]) -> Result<Vec<u8>> {
    if path.is_empty() {
        return Err(Error::Link("onion: пустой путь".into()));
    }
    let last = path.len() - 1;

    // Внутренний слой — доставка адресату.
    let mut inner = Vec::with_capacity(1 + payload.len());
    inner.push(TAG_DELIVER);
    inner.extend_from_slice(payload);
    let mut packet = seal(&path[last], &inner)?;

    // Оборачиваем наружу: каждый промежуточный хоп узнаёт next-hop + вложенный пакет.
    for i in (0..last).rev() {
        let next_id = path[i + 1];
        let mut layer = Vec::with_capacity(1 + 32 + packet.len());
        layer.push(TAG_FORWARD);
        layer.extend_from_slice(&next_id);
        layer.extend_from_slice(&packet);
        packet = seal(&path[i], &layer)?;
    }
    Ok(packet)
}

/// Результат снятия одного слоя.
pub enum Peeled {
    /// Мы — релей: переслать `inner` следующему хопу `next` (его onion-id).
    Forward { next: [u8; 32], inner: Vec<u8> },
    /// Мы — адресат: вот payload.
    Deliver(Vec<u8>),
}

/// Снять ровно один слой своим ключом.
pub fn peel(key: &OnionKey, packet: &[u8]) -> Result<Peeled> {
    let plain = open(&key.secret, packet)?;
    match plain.first() {
        Some(&TAG_DELIVER) => Ok(Peeled::Deliver(plain[1..].to_vec())),
        Some(&TAG_FORWARD) => {
            if plain.len() < 1 + 32 {
                return Err(Error::Link("onion: короткий forward-слой".into()));
            }
            let mut next = [0u8; 32];
            next.copy_from_slice(&plain[1..33]);
            Ok(Peeled::Forward { next, inner: plain[33..].to_vec() })
        }
        _ => Err(Error::Link("onion: неизвестный тег слоя".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn relay_forwards_without_reading_payload() {
        let relay = OnionKey::generate();
        let dest = OnionKey::generate();
        let secret = b"super secret payload";

        let packet = wrap(&[relay.public(), dest.public()], secret).unwrap();

        // Релей снимает свой слой: видит только next-hop + непрозрачный блоб.
        let inner = match peel(&relay, &packet).unwrap() {
            Peeled::Forward { next, inner } => {
                assert_eq!(next, dest.public(), "релей должен узнать только следующий хоп");
                inner
            }
            Peeled::Deliver(_) => panic!("релей не должен доставлять"),
        };

        // Payload недоступен ни в исходном пакете, ни в том, что видит релей.
        assert!(!contains(&packet, secret), "payload не должен быть виден на проводе");
        assert!(!contains(&inner, secret), "релей не должен видеть payload");

        // Адресат снимает последний слой и читает payload.
        match peel(&dest, &inner).unwrap() {
            Peeled::Deliver(p) => assert_eq!(p, secret),
            Peeled::Forward { .. } => panic!("адресат должен доставлять, не пересылать"),
        }
    }

    #[test]
    fn wrong_key_cannot_peel() {
        let relay = OnionKey::generate();
        let dest = OnionKey::generate();
        let attacker = OnionKey::generate();

        let packet = wrap(&[relay.public(), dest.public()], b"x").unwrap();
        assert!(peel(&attacker, &packet).is_err(), "чужой ключ не должен снимать слой");
    }

    #[test]
    fn single_hop_delivers_directly() {
        let dest = OnionKey::generate();
        let packet = wrap(&[dest.public()], b"direct").unwrap();
        match peel(&dest, &packet).unwrap() {
            Peeled::Deliver(p) => assert_eq!(p, b"direct"),
            Peeled::Forward { .. } => panic!("один хоп = сразу доставка"),
        }
    }
}
