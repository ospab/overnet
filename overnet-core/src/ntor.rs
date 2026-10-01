//! Рукопожатие с хопом цепи — ntor, как в Tor (tor-spec §5.1.4, proposal 216).
//!
//! Клиент знает статический onion-ключ хопа `B` (из каталога) и шлёт эфемерный
//! `X`. Хоп отвечает эфемерным `Y` и `AUTH`. Общий секрет зависит от `x·Y` и `x·B`:
//! без приватного `b` правильный `AUTH` не построить, поэтому клиент знает, что
//! говорит именно с владельцем ключа, а сам хоп про клиента не знает ничего.
//!
//! Тот же обмен работает сквозным слоем «клиент ↔ сервис .ov»: там `B` —
//! X25519-форма ключа сервиса, выведенная из его адреса (см. `ovaddr`).
//!
//! Крипту не катаем: X25519, HMAC-SHA256, HKDF-SHA256 — готовые крейты.

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::onion::OnionKey;
use crate::{Error, Result};

const PROTOID: &[u8] = b"overnet-ntor-curve25519-sha256-1";
const T_KEY: &[u8] = b"overnet-ntor-curve25519-sha256-1:key_extract";
const T_VERIFY: &[u8] = b"overnet-ntor-curve25519-sha256-1:verify";
const T_MAC: &[u8] = b"overnet-ntor-curve25519-sha256-1:mac";
const M_EXPAND: &[u8] = b"overnet-ntor-curve25519-sha256-1:key_expand";

/// Длина «луковой кожуры» клиента (X) и ответа хопа (Y || AUTH).
pub const ONIONSKIN_LEN: usize = 32;
pub const REPLY_LEN: usize = 64;

/// Ключи одного хопа цепи: шифрование и MAC в каждую сторону, плюс `binding` —
/// значение, известное только двум концам рукопожатия (им привязывают подписи
/// к конкретной цепи, см. ESTABLISH_INTRO).
#[derive(Clone)]
pub struct HopKeys {
    /// Ключ слоя «от клиента» (forward).
    pub kf: [u8; 32],
    /// Ключ слоя «к клиенту» (backward).
    pub kb: [u8; 32],
    pub mf: [u8; 32],
    pub mb: [u8; 32],
    pub binding: [u8; 32],
}

impl HopKeys {
    /// Ключи с другой стороны: что для клиента forward, для сервиса — backward.
    /// Нужно сквозному слою, где сервис — второй «клиент» той же склейки.
    pub fn swapped(&self) -> HopKeys {
        HopKeys { kf: self.kb, kb: self.kf, mf: self.mb, mb: self.mf, binding: self.binding }
    }
}

type HmacSha256 = Hmac<Sha256>;

fn h(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

fn nonzero(dh: [u8; 32]) -> Result<[u8; 32]> {
    // Точка малого порядка даёт нулевой секрет — такое рукопожатие отвергаем.
    if dh.iter().all(|&b| b == 0) {
        return Err(Error::Link("ntor: degenerate key".into()));
    }
    Ok(dh)
}

/// Общая часть: secret_input → (AUTH, ключи).
fn derive(xy: &[u8; 32], xb: &[u8; 32], b: &[u8; 32], x: &[u8; 32], y: &[u8; 32]) -> ([u8; 32], HopKeys) {
    let secret_input: Vec<u8> = [&xy[..], xb, b, x, y, PROTOID].concat();
    let key_seed = h(T_KEY, &[&secret_input]);
    let verify = h(T_VERIFY, &[&secret_input]);
    let auth = h(T_MAC, &[&verify, b, y, x, PROTOID, b"Server"]);

    let hk = Hkdf::<Sha256>::from_prk(&key_seed).expect("32-byte PRK");
    let mut okm = [0u8; 160];
    hk.expand(M_EXPAND, &mut okm).expect("160 bytes is a valid HKDF length");
    let take = |i: usize| -> [u8; 32] { okm[i * 32..(i + 1) * 32].try_into().unwrap() };
    (auth, HopKeys { kf: take(0), kb: take(1), mf: take(2), mb: take(3), binding: take(4) })
}

/// Состояние клиента между отправкой X и получением ответа.
pub struct ClientHandshake {
    x: StaticSecret,
    x_pub: [u8; 32],
    b: [u8; 32],
}

impl ClientHandshake {
    /// Начать рукопожатие с хопом, чей статический ключ `server_pub`.
    /// Возвращает состояние и X для отправки.
    pub fn new(server_pub: [u8; 32]) -> (Self, [u8; ONIONSKIN_LEN]) {
        let x = StaticSecret::random_from_rng(OsRng);
        let x_pub = PublicKey::from(&x).to_bytes();
        (ClientHandshake { x, x_pub, b: server_pub }, x_pub)
    }

    /// Проверить ответ хопа и получить ключи. Ошибка = хоп не владеет ключом.
    pub fn finish(self, reply: &[u8]) -> Result<HopKeys> {
        if reply.len() < REPLY_LEN {
            return Err(Error::Link("ntor: short reply".into()));
        }
        let y: [u8; 32] = reply[..32].try_into().unwrap();
        let their_auth = &reply[32..64];
        let xy = nonzero(self.x.diffie_hellman(&PublicKey::from(y)).to_bytes())?;
        let xb = nonzero(self.x.diffie_hellman(&PublicKey::from(self.b)).to_bytes())?;
        let (auth, keys) = derive(&xy, &xb, &self.b, &self.x_pub, &y);
        // Сравнение за постоянное время.
        let diff = auth.iter().zip(their_auth).fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if diff != 0 {
            return Err(Error::Link("ntor: server authentication failed".into()));
        }
        Ok(keys)
    }
}

/// Сторона хопа: ответить на X своим ключом. Возвращает (Y || AUTH, ключи).
pub fn server_handshake(key: &OnionKey, onionskin: &[u8]) -> Result<([u8; REPLY_LEN], HopKeys)> {
    if onionskin.len() < ONIONSKIN_LEN {
        return Err(Error::Link("ntor: short onionskin".into()));
    }
    let x: [u8; 32] = onionskin[..32].try_into().unwrap();
    let y = StaticSecret::random_from_rng(OsRng);
    let y_pub = PublicKey::from(&y).to_bytes();
    let xy = nonzero(y.diffie_hellman(&PublicKey::from(x)).to_bytes())?;
    let xb = nonzero(key.dh(&x))?;
    let b = key.public();
    let (auth, keys) = derive(&xy, &xb, &b, &x, &y_pub);
    let mut reply = [0u8; REPLY_LEN];
    reply[..32].copy_from_slice(&y_pub);
    reply[32..].copy_from_slice(&auth);
    Ok((reply, keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_agree_on_keys() {
        let server = OnionKey::generate();
        let (c, x) = ClientHandshake::new(server.public());
        let (reply, sk) = server_handshake(&server, &x).unwrap();
        let ck = c.finish(&reply).unwrap();
        assert_eq!(ck.kf, sk.kf);
        assert_eq!(ck.kb, sk.kb);
        assert_eq!(ck.mf, sk.mf);
        assert_eq!(ck.mb, sk.mb);
        assert_eq!(ck.binding, sk.binding);
        assert_ne!(ck.kf, ck.kb);
    }

    #[test]
    fn impostor_without_the_key_is_rejected() {
        let real = OnionKey::generate();
        let impostor = OnionKey::generate();
        let (c, x) = ClientHandshake::new(real.public());
        // Хоп с чужим ключом не может правильно ответить.
        let (reply, _) = server_handshake(&impostor, &x).unwrap();
        assert!(c.finish(&reply).is_err());
    }

    #[test]
    fn tampered_reply_is_rejected() {
        let server = OnionKey::generate();
        let (c, x) = ClientHandshake::new(server.public());
        let (mut reply, _) = server_handshake(&server, &x).unwrap();
        reply[40] ^= 1;
        assert!(c.finish(&reply).is_err());
    }

    #[test]
    fn low_order_point_is_rejected() {
        let server = OnionKey::generate();
        assert!(server_handshake(&server, &[0u8; 32]).is_err());
    }
}
