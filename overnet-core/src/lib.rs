//! `overnet-core` — ядро overnet: личность, криптоадрес, абстракция транспорта.
//!
//! Принцип: крипту НЕ катаем сами. Примитивы — проверенные крейты (те же, что в
//! `ostp`): `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `snow`.
//! Новизна overnet — в конструкции (см. ../docs/architecture.md), не в примитивах.

use std::fmt;

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod cell;
pub mod link;
pub mod ntor;
pub mod onion;
pub mod ovaddr;
pub mod session;
pub use link::{Link, LinkProps};

/// Ошибки ядра.
#[derive(Debug, Error)]
pub enum Error {
    #[error("link error: {0}")]
    Link(String),
    #[error("invalid address")]
    InvalidAddress,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Криптоадрес узла = SHA-256 от публичного ключа (32 байта).
///
/// Самовыделяемый (реестр не нужен), самоудостоверяющий (подделать нельзя без
/// приватного ключа), неотзываемый централизованно. См. ../docs/naming.md.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address([u8; 32]);

impl Address {
    /// Вывести адрес из публичного ключа узла.
    pub fn from_verifying_key(vk: &VerifyingKey) -> Self {
        let mut h = Sha256::new();
        h.update(vk.as_bytes());
        let digest = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Address(out)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO(naming): base32 + контрольная сумма + версия (как .onion).
        // Пока hex с префиксом — для читаемости в логах.
        write!(f, "ovn:")?;
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

/// Долговременная личность узла (ed25519). Из неё детерминированно выводится адрес.
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Сгенерировать новую личность из системного CSPRNG.
    pub fn generate() -> Self {
        Identity {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    /// Восстановить личность из существующего приватного ключа.
    pub fn from_signing_key(signing: SigningKey) -> Self {
        Identity { signing }
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// Криптоадрес этой личности.
    pub fn address(&self) -> Address {
        Address::from_verifying_key(&self.verifying_key())
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_is_deterministic_from_key() {
        let id = Identity::generate();
        assert_eq!(id.address(), Address::from_verifying_key(&id.verifying_key()));
    }

    #[test]
    fn different_identities_differ() {
        assert_ne!(Identity::generate().address(), Identity::generate().address());
    }
}
