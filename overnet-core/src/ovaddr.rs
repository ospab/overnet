//! Адреса сервисов `.ov` — самоудостоверяющие, как `.onion` v3.
//!
//! Личность сервиса — ed25519-ключ. Адрес = base32(pubkey || checksum || version)
//! + ".ov": 56 символов, подделать нельзя без приватного ключа, реестр не нужен.
//! Человекочитаемые имена (`search.ov`) — слой поверх: зарезервированные вшиты в
//! клиент, остальные выдаёт регистратор `name.ov` (см. docs/naming.md).
//!
//! Для сквозного рукопожатия клиент ↔ сервис нужен X25519-ключ сервиса: он
//! выводится из того же ed25519 (стандартное бирациональное отображение, как
//! `crypto_sign_ed25519_pk_to_curve25519` в libsodium), поэтому адреса хватает.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

use crate::onion::OnionKey;
use crate::{Error, Result};

const VERSION: u8 = 1;
const CHECKSUM_LABEL: &[u8] = b".ov checksum";
/// Длина адреса без ".ov".
pub const ADDRESS_LEN: usize = 56;

fn checksum(pubkey: &[u8; 32]) -> [u8; 2] {
    let mut h = Sha256::new();
    h.update(CHECKSUM_LABEL);
    h.update(pubkey);
    h.update([VERSION]);
    h.finalize()[..2].try_into().unwrap()
}

/// Публичная личность сервиса (ed25519).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ServiceId(pub [u8; 32]);

impl ServiceId {
    /// `<56 символов>.ov`
    pub fn to_address(&self) -> String {
        let mut raw = Vec::with_capacity(35);
        raw.extend_from_slice(&self.0);
        raw.extend_from_slice(&checksum(&self.0));
        raw.push(VERSION);
        format!("{}.ov", data_encoding::BASE32_NOPAD.encode(&raw).to_ascii_lowercase())
    }

    /// Разобрать адрес (с ".ov" или без, регистр не важен). Проверяет checksum,
    /// версию и что это действительно точка кривой.
    pub fn from_address(addr: &str) -> Result<ServiceId> {
        let label = addr.trim_end_matches('.').to_ascii_lowercase();
        let label = label.strip_suffix(".ov").unwrap_or(&label);
        // Поддомены (www.<адрес>.ov) ведут на тот же сервис.
        let label = label.rsplit('.').next().unwrap_or(label);
        if label.len() != ADDRESS_LEN {
            return Err(Error::InvalidAddress);
        }
        let raw = data_encoding::BASE32_NOPAD
            .decode(label.to_ascii_uppercase().as_bytes())
            .map_err(|_| Error::InvalidAddress)?;
        if raw.len() != 35 || raw[34] != VERSION {
            return Err(Error::InvalidAddress);
        }
        let pubkey: [u8; 32] = raw[..32].try_into().unwrap();
        if raw[32..34] != checksum(&pubkey) {
            return Err(Error::InvalidAddress);
        }
        VerifyingKey::from_bytes(&pubkey).map_err(|_| Error::InvalidAddress)?;
        Ok(ServiceId(pubkey))
    }

    /// Похоже ли имя на криптоадрес (а не на имя для регистратора).
    pub fn is_address(name: &str) -> bool {
        ServiceId::from_address(name).is_ok()
    }

    /// X25519-ключ сервиса для сквозного ntor.
    pub fn onion_pub(&self) -> Result<[u8; 32]> {
        let vk = VerifyingKey::from_bytes(&self.0).map_err(|_| Error::InvalidAddress)?;
        Ok(vk.to_montgomery().to_bytes())
    }

    pub fn verify(&self, msg: &[u8], sig: &[u8]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.0) else { return false };
        let Ok(sig) = Signature::from_slice(sig) else { return false };
        vk.verify(msg, &sig).is_ok()
    }
}

/// Приватный ключ сервиса.
pub struct ServiceKey {
    signing: SigningKey,
}

impl ServiceKey {
    pub fn generate() -> ServiceKey {
        ServiceKey { signing: SigningKey::generate(&mut OsRng) }
    }

    pub fn from_bytes(seed: [u8; 32]) -> ServiceKey {
        ServiceKey { signing: SigningKey::from_bytes(&seed) }
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn id(&self) -> ServiceId {
        ServiceId(self.signing.verifying_key().to_bytes())
    }

    /// X25519-половина для сквозного ntor (сервер-сторона).
    pub fn onion_key(&self) -> OnionKey {
        OnionKey::from_secret_bytes(self.signing.to_scalar_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }
}

/// Что подписывает сервис в ESTABLISH_INTRO: привязка к конкретной цепи, чтобы
/// подпись нельзя было переиграть на другой цепи.
pub fn intro_message(binding: &[u8; 32]) -> Vec<u8> {
    [&b"overnet-intro-v1:"[..], binding].concat()
}

/// Что подписывает владелец сервиса, регистрируя имя у `name.ov`.
pub fn name_message(name: &str) -> Vec<u8> {
    [&b"overnet-name-v1:"[..], name.to_ascii_lowercase().as_bytes()].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntor::{server_handshake, ClientHandshake};

    #[test]
    fn address_roundtrips_and_rejects_typos() {
        let id = ServiceKey::generate().id();
        let addr = id.to_address();
        assert_eq!(addr.len(), ADDRESS_LEN + 3);
        assert_eq!(ServiceId::from_address(&addr).unwrap(), id);
        assert_eq!(ServiceId::from_address(&addr.to_uppercase()).unwrap(), id);
        assert_eq!(ServiceId::from_address(&format!("www.{addr}")).unwrap(), id);
        assert_eq!(ServiceId::from_address(addr.trim_end_matches(".ov")).unwrap(), id);

        // Одна опечатка — checksum не сходится.
        let mut bad: Vec<char> = addr.chars().collect();
        bad[10] = if bad[10] == 'a' { 'b' } else { 'a' };
        assert!(ServiceId::from_address(&bad.into_iter().collect::<String>()).is_err());
        assert!(ServiceId::from_address("search.ov").is_err());
    }

    #[test]
    fn address_is_enough_for_an_end_to_end_handshake() {
        let key = ServiceKey::generate();
        let b = ServiceId::from_address(&key.id().to_address()).unwrap().onion_pub().unwrap();
        assert_eq!(b, key.onion_key().public());
        let (c, x) = ClientHandshake::new(b);
        let (reply, _) = server_handshake(&key.onion_key(), &x).unwrap();
        assert!(c.finish(&reply).is_ok());
    }

    #[test]
    fn signatures_verify_only_for_the_owner() {
        let key = ServiceKey::generate();
        let sig = key.sign(&name_message("Shop.ov"));
        assert!(key.id().verify(&name_message("shop.ov"), &sig));
        assert!(!key.id().verify(&name_message("other.ov"), &sig));
        assert!(!ServiceKey::generate().id().verify(&name_message("shop.ov"), &sig));
    }
}
