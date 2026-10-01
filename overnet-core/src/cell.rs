//! Ячейки цепей и слоёвое шифрование — по образцу Tor (tor-spec §3, §6).
//!
//! **Ячейка** — кадр фиксированной длины `CELL_LEN` на линке между соседями:
//! `circ_id (u32) | cmd (u8) | body`. Фиксированная длина нужна, чтобы по размеру
//! не было видно ни типа трафика, ни позиции хопа в цепи.
//!
//! **Relay-ячейки** несут сообщения внутри цепи. Тело шифруется слоями: по слою
//! на каждый хоп (ChaCha20, ключ из ntor, nonce = номер ячейки в этом
//! направлении). Хоп снимает свой слой и проверяет, ему ли ячейка: `recognized`
//! равно нулю и MAC (HMAC-SHA256 ключом хопа по номеру ячейки и телу) сходится.
//! Если нет — пересылает дальше. Так каждый хоп видит только соседей, а
//! содержимое — только адресат.
//!
//! Отличие от Tor: вместо бегущего дайджеста MAC считается по номеру ячейки —
//! это то же «узнавание», но без общего состояния, кроме счётчика.

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::ntor::HopKeys;
use crate::{Error, Result};

/// Длина ячейки на линке.
pub const CELL_LEN: usize = 1024;
const CELL_HEADER: usize = 5;
/// Длина тела ячейки.
pub const BODY_LEN: usize = CELL_LEN - CELL_HEADER;
/// recognized (2) | mac (8) | cmd (1) | stream_id (2) | length (2)
const RELAY_HEADER: usize = 15;
const MAC_RANGE: std::ops::Range<usize> = 2..10;
/// Сколько данных помещается в одну relay-ячейку.
pub const MAX_DATA: usize = BODY_LEN - RELAY_HEADER;

/// Команды ячеек линка.
pub mod cmd {
    pub const PADDING: u8 = 0;
    /// Создать цепь с этим хопом: тело = X (ntor).
    pub const CREATE: u8 = 1;
    /// Ответ на CREATE: тело = Y || AUTH.
    pub const CREATED: u8 = 2;
    pub const RELAY: u8 = 3;
    /// Разобрать цепь.
    pub const DESTROY: u8 = 4;
}

/// Команды relay-сообщений (внутри цепи).
pub mod relay {
    /// Открыть поток: data = "host:port" (выход) или "port" (сервис .ov).
    pub const BEGIN: u8 = 1;
    pub const DATA: u8 = 2;
    /// Закрыть поток: data = причина (текст, может быть пустым).
    pub const END: u8 = 3;
    pub const CONNECTED: u8 = 4;
    /// Окно потока: получатель готов к следующей порции.
    pub const SENDME: u8 = 5;
    /// Продлить цепь: data = pubkey (32) | X (32) | адрес "host:port".
    pub const EXTEND: u8 = 6;
    /// Ответ на EXTEND: data = Y || AUTH.
    pub const EXTENDED: u8 = 7;
    /// Поток к каталогу, который хранит хоп.
    pub const BEGIN_DIR: u8 = 8;
    /// Сервис просит хоп быть его точкой входа: data = service_id (32) | подпись (64).
    pub const ESTABLISH_INTRO: u8 = 9;
    pub const INTRO_ESTABLISHED: u8 = 10;
    /// Клиент → точка входа: data = service_id (32) | cookie (20) | X (32).
    pub const INTRODUCE1: u8 = 11;
    /// Точка входа → сервис: data = cookie (20) | X (32).
    pub const INTRODUCE2: u8 = 12;
    /// Точка входа → клиент: data = статус (0 = передано сервису).
    pub const INTRODUCE_ACK: u8 = 13;
    /// Сервис → точка входа по новой цепи: data = cookie (20) | Y || AUTH (64).
    pub const RENDEZVOUS1: u8 = 14;
    /// Точка входа → клиент после склейки: data = Y || AUTH (64).
    pub const RENDEZVOUS2: u8 = 15;
}

/// Ячейка линка.
#[derive(Clone)]
pub struct Cell {
    pub circ_id: u32,
    pub cmd: u8,
    pub body: Vec<u8>,
}

impl Cell {
    /// Ячейка с данными в начале тела (остаток — нули).
    pub fn new(circ_id: u32, cmd: u8, data: &[u8]) -> Cell {
        let mut body = vec![0u8; BODY_LEN];
        let n = data.len().min(BODY_LEN);
        body[..n].copy_from_slice(&data[..n]);
        Cell { circ_id, cmd, body }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(CELL_LEN);
        out.extend_from_slice(&self.circ_id.to_be_bytes());
        out.push(self.cmd);
        out.extend_from_slice(&self.body);
        out.resize(CELL_LEN, 0);
        out
    }

    pub fn decode(frame: &[u8]) -> Result<Cell> {
        if frame.len() != CELL_LEN {
            return Err(Error::Link(format!("cell: {} bytes, expected {CELL_LEN}", frame.len())));
        }
        Ok(Cell {
            circ_id: u32::from_be_bytes(frame[..4].try_into().unwrap()),
            cmd: frame[4],
            body: frame[CELL_HEADER..].to_vec(),
        })
    }
}

/// Сообщение внутри цепи.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayMsg {
    pub cmd: u8,
    pub stream_id: u16,
    pub data: Vec<u8>,
}

impl RelayMsg {
    pub fn new(cmd: u8, stream_id: u16, data: impl Into<Vec<u8>>) -> RelayMsg {
        RelayMsg { cmd, stream_id, data: data.into() }
    }

    /// Открытое тело relay-ячейки (recognized = 0, место под MAC пустое).
    pub fn encode_body(&self) -> Result<Vec<u8>> {
        if self.data.len() > MAX_DATA {
            return Err(Error::Link(format!("relay: {} bytes do not fit a cell", self.data.len())));
        }
        let mut body = vec![0u8; BODY_LEN];
        body[10] = self.cmd;
        body[11..13].copy_from_slice(&self.stream_id.to_be_bytes());
        body[13..15].copy_from_slice(&(self.data.len() as u16).to_be_bytes());
        body[RELAY_HEADER..RELAY_HEADER + self.data.len()].copy_from_slice(&self.data);
        Ok(body)
    }

    /// Разобрать открытое (уже узнанное) тело.
    pub fn decode_body(body: &[u8]) -> Result<RelayMsg> {
        if body.len() != BODY_LEN {
            return Err(Error::Link("relay: bad body length".into()));
        }
        let len = u16::from_be_bytes([body[13], body[14]]) as usize;
        if len > MAX_DATA {
            return Err(Error::Link("relay: bad length".into()));
        }
        Ok(RelayMsg {
            cmd: body[10],
            stream_id: u16::from_be_bytes([body[11], body[12]]),
            data: body[RELAY_HEADER..RELAY_HEADER + len].to_vec(),
        })
    }
}

type HmacSha256 = Hmac<Sha256>;

/// Один слой одного направления: ключ шифра, ключ MAC и номер следующей ячейки.
pub struct Layer {
    key: [u8; 32],
    mac: [u8; 32],
    ctr: u64,
}

impl Layer {
    pub fn new(key: [u8; 32], mac: [u8; 32]) -> Layer {
        Layer { key, mac, ctr: 0 }
    }

    /// Слои хопа: (forward, backward).
    pub fn pair(keys: &HopKeys) -> (Layer, Layer) {
        (Layer::new(keys.kf, keys.mf), Layer::new(keys.kb, keys.mb))
    }

    fn crypt(&self, body: &mut [u8]) {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.ctr.to_be_bytes());
        let mut c = ChaCha20::new(&self.key.into(), &nonce.into());
        c.apply_keystream(body);
    }

    fn tag(&self, body: &[u8]) -> [u8; 8] {
        let mut m = HmacSha256::new_from_slice(&self.mac).expect("any key length");
        m.update(&self.ctr.to_be_bytes());
        m.update(&body[..MAC_RANGE.start]);
        m.update(&[0u8; 8]);
        m.update(&body[MAC_RANGE.end..]);
        m.finalize().into_bytes()[..8].try_into().unwrap()
    }

    /// Мы — адресат слоя на отправке: поставить MAC и зашифровать.
    pub fn seal(&mut self, body: &mut [u8]) {
        body[..2].copy_from_slice(&[0, 0]);
        let tag = self.tag(body);
        body[MAC_RANGE].copy_from_slice(&tag);
        self.crypt(body);
        self.ctr += 1;
    }

    /// Добавить слой поверх чужого (ячейка идёт дальше, не нам).
    pub fn wrap(&mut self, body: &mut [u8]) {
        self.crypt(body);
        self.ctr += 1;
    }

    /// Снять слой. `true` — ячейка адресована этому слою.
    pub fn unwrap(&mut self, body: &mut [u8]) -> bool {
        self.crypt(body);
        let mine = body[0] == 0 && body[1] == 0 && {
            let tag = self.tag(body);
            tag.iter().zip(&body[MAC_RANGE]).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
        };
        self.ctr += 1;
        mine
    }
}

/// Слои отправки на стороне создателя цепи (forward), по хопам.
#[derive(Default)]
pub struct ForwardLayers(Vec<Layer>);

impl ForwardLayers {
    pub fn push(&mut self, layer: Layer) {
        self.0.push(layer);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Зашифровать сообщение для хопа `hop` (0 — первый).
    pub fn encrypt_to(&mut self, hop: usize, msg: &RelayMsg) -> Result<Vec<u8>> {
        if hop >= self.0.len() {
            return Err(Error::Link("circuit: no such hop".into()));
        }
        let mut body = msg.encode_body()?;
        self.0[hop].seal(&mut body);
        for layer in self.0[..hop].iter_mut().rev() {
            layer.wrap(&mut body);
        }
        Ok(body)
    }
}

/// Слои приёма на стороне создателя цепи (backward), по хопам.
#[derive(Default)]
pub struct BackwardLayers(Vec<Layer>);

impl BackwardLayers {
    pub fn push(&mut self, layer: Layer) {
        self.0.push(layer);
    }

    /// Расшифровать ячейку, пришедшую к нам: (номер хопа-отправителя, сообщение).
    pub fn decrypt(&mut self, mut body: Vec<u8>) -> Result<(usize, RelayMsg)> {
        for (i, layer) in self.0.iter_mut().enumerate() {
            if layer.unwrap(&mut body) {
                return Ok((i, RelayMsg::decode_body(&body)?));
            }
        }
        Err(Error::Link("circuit: cell not recognized by any hop".into()))
    }
}

/// Оба направления вместе (удобно, когда блокировки не нужно разделять).
#[derive(Default)]
pub struct OriginLayers {
    pub fwd: ForwardLayers,
    pub bwd: BackwardLayers,
}

impl OriginLayers {
    pub fn push(&mut self, keys: &HopKeys) {
        let (f, b) = Layer::pair(keys);
        self.fwd.push(f);
        self.bwd.push(b);
    }

    pub fn encrypt_to(&mut self, hop: usize, msg: &RelayMsg) -> Result<Vec<u8>> {
        self.fwd.encrypt_to(hop, msg)
    }

    pub fn decrypt(&mut self, body: Vec<u8>) -> Result<(usize, RelayMsg)> {
        self.bwd.decrypt(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntor::{server_handshake, ClientHandshake};
    use crate::onion::OnionKey;

    fn hop() -> (HopKeys, HopKeys) {
        let k = OnionKey::generate();
        let (c, x) = ClientHandshake::new(k.public());
        let (reply, sk) = server_handshake(&k, &x).unwrap();
        (c.finish(&reply).unwrap(), sk)
    }

    fn contains(h: &[u8], n: &[u8]) -> bool {
        h.windows(n.len()).any(|w| w == n)
    }

    #[test]
    fn cell_roundtrips_and_has_fixed_length() {
        let c = Cell::new(7, cmd::CREATE, b"abc");
        let e = c.encode();
        assert_eq!(e.len(), CELL_LEN);
        let d = Cell::decode(&e).unwrap();
        assert_eq!((d.circ_id, d.cmd, &d.body[..3]), (7, cmd::CREATE, &b"abc"[..]));
        assert!(Cell::decode(&e[..100]).is_err());
    }

    #[test]
    fn three_hops_each_sees_only_its_own_cells() {
        let hops: Vec<(HopKeys, HopKeys)> = (0..3).map(|_| hop()).collect();
        let mut origin = OriginLayers::default();
        for (ck, _) in &hops {
            origin.push(ck);
        }
        let mut relays: Vec<(Layer, Layer)> = hops.iter().map(|(_, sk)| Layer::pair(sk)).collect();

        for round in 0..3 {
            // Вперёд к последнему хопу: первые два не узнают и не видят данных.
            let secret = format!("to the exit, round {round}");
            let msg = RelayMsg::new(relay::DATA, 5, secret.as_bytes());
            let mut body = origin.encrypt_to(2, &msg).unwrap();
            assert!(!contains(&body, secret.as_bytes()));
            assert!(!relays[0].0.unwrap(&mut body));
            assert!(!contains(&body, secret.as_bytes()));
            assert!(!relays[1].0.unwrap(&mut body));
            assert!(relays[2].0.unwrap(&mut body));
            assert_eq!(RelayMsg::decode_body(&body).unwrap(), msg);

            // Сообщение среднему хопу: узнаёт он, а не первый.
            let ctl = RelayMsg::new(relay::EXTEND, 0, vec![1, 2, 3]);
            let mut body = origin.encrypt_to(1, &ctl).unwrap();
            assert!(!relays[0].0.unwrap(&mut body));
            assert!(relays[1].0.unwrap(&mut body));

            // Обратно от последнего хопа: средний и первый добавляют слои.
            let back = RelayMsg::new(relay::DATA, 5, b"reply".to_vec());
            let mut body = back.encode_body().unwrap();
            relays[2].1.seal(&mut body);
            relays[1].1.wrap(&mut body);
            relays[0].1.wrap(&mut body);
            assert_eq!(origin.decrypt(body).unwrap(), (2, back));
        }
    }

    #[test]
    fn tampered_cell_is_not_recognized() {
        let (ck, sk) = hop();
        let mut origin = OriginLayers::default();
        origin.push(&ck);
        let (mut fwd, _) = Layer::pair(&sk);
        let mut body = origin.encrypt_to(0, &RelayMsg::new(relay::DATA, 1, b"x".to_vec())).unwrap();
        body[200] ^= 0x40;
        assert!(!fwd.unwrap(&mut body));
    }

    #[test]
    fn data_must_fit_a_cell() {
        assert!(RelayMsg::new(relay::DATA, 1, vec![0; MAX_DATA]).encode_body().is_ok());
        assert!(RelayMsg::new(relay::DATA, 1, vec![0; MAX_DATA + 1]).encode_body().is_err());
    }
}
