//! Имена `.ov` поверх криптоадресов.
//!
//! - `<56 символов>.ov` — сам адрес, резолвить нечего.
//! - Зарезервированные имена (`name.ov`, `search.ov`, `mail.ov`, `files.ov`) —
//!   адреса вшиты в клиент или заданы в конфиге (`reserved`).
//! - Остальные (`shop.ov`) — у регистратора `name.ov`, через overnet. Ответ
//!   проверяется подписью владельца имени, поэтому регистратор может отказать
//!   или промолчать, но не подменить адрес.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use overnet_core::ovaddr::{name_message, ServiceId};
use overnet_core::{Error, Result};

use super::client::Client;

/// Зарезервированные имена: регистратор, поиск, почта, файлообменник и
/// исходный код самого overnet (Gitea).
pub const RESERVED: [&str; 5] = ["name.ov", "search.ov", "mail.ov", "files.ov", "source.ov"];

/// Имена, которые обслуживает сам шлюз на этой машине (`browser.ov` — страницы
/// браузера). В сеть они не уходят, и зарегистрировать их нельзя.
pub const LOCAL: [&str; 1] = ["browser.ov"];

/// Адреса официальных сервисов сети. `reserved` в конфиге их перекрывает
/// (своя сеть, `overnet demo`).
pub const PINNED: &[(&str, &str)] = &[
    ("name.ov", "cyprub3k5zzihweug4i7v7gafxisuqnrqvw4qch7kmrhgnny5sil2rab.ov"),
    ("search.ov", "kd5vfzijxxooolgcw45catledp2oc3tqd3aemyuu6y6xnrbal26y33ib.ov"),
    ("mail.ov", "xgtpz5xt7yf6elvkt2wubzumhfww772xcngcta5fxyeitrfqqw6ma4yb.ov"),
    ("files.ov", "2iasoxxypoy7q3zpo7xb4iagxa7uf6ttnlyo5f33ffshcgcsbkuxd5ib.ov"),
    ("source.ov", "4f6d2br4fxr7dq2c5rqozo44je6taqvy3or22pir76u4wvryplvxjlqb.ov"),
];

/// Стартовые релеи сети: с них клиент берёт каталог, если в конфиге своих нет.
pub const SEED_RELAYS: &[&str] = &[
    "f7d575b796aa434f05f72ed54ffdeebde43880d0e2b5ea64e7e97952c2de0f2d@138.124.241.23:4040",
    "111293d0ba0887e24d70a5f8a188bf2f7f0af0174692c7641264690f035b056d@138.124.71.217:4040",
    "185466dd00430796123d49d4cb8976987d144e75208ae90d0766bda810739d60@138.124.241.18:4040",
];

const CACHE_TTL: Duration = Duration::from_secs(600);

/// Запись регистратора.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NameRecord {
    pub name: String,
    pub address: String,
    /// hex ed25519-подписи владельца над `name_message(name)`.
    pub sig: String,
}

impl NameRecord {
    pub fn verify(&self) -> Option<ServiceId> {
        let id = ServiceId::from_address(&self.address).ok()?;
        let sig = hex::decode(&self.sig).ok()?;
        id.verify(&name_message(&self.name), &sig).then_some(id)
    }
}

/// Можно ли зарегистрировать такое имя: `метка.ov`, метка из [a-z0-9-].
pub fn valid_name(name: &str) -> bool {
    let Some(label) = name.strip_suffix(".ov") else { return false };
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !ServiceId::is_address(name)
}

/// `www.shop.ov` → `shop.ov`.
pub fn base_name(host: &str) -> String {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    let parts: Vec<&str> = h.split('.').collect();
    if parts.len() <= 2 {
        h
    } else {
        parts[parts.len() - 2..].join(".")
    }
}

pub struct Names {
    client: Arc<Client>,
    reserved: HashMap<String, ServiceId>,
    cache: Mutex<HashMap<String, (ServiceId, Instant)>>,
}

impl Names {
    /// `overrides` — `reserved` из конфига: имя → адрес.
    pub fn new(client: Arc<Client>, overrides: &HashMap<String, String>) -> Result<Arc<Names>> {
        let mut reserved = HashMap::new();
        for (n, a) in PINNED.iter().map(|(n, a)| (n.to_string(), a.to_string())).chain(overrides.clone()) {
            let n = n.to_ascii_lowercase();
            if !RESERVED.contains(&n.as_str()) {
                return Err(Error::Link(format!("{n} is not a reserved name ({})", RESERVED.join(", "))));
            }
            let id = ServiceId::from_address(&a).map_err(|_| Error::Link(format!("{n}: bad address {a}")))?;
            reserved.insert(n, id);
        }
        Ok(Arc::new(Names { client, reserved, cache: Mutex::new(HashMap::new()) }))
    }

    pub fn reserved(&self) -> &HashMap<String, ServiceId> {
        &self.reserved
    }

    pub async fn resolve(&self, host: &str) -> Result<ServiceId> {
        if let Ok(id) = ServiceId::from_address(host) {
            return Ok(id);
        }
        let name = base_name(host);
        if let Some(id) = self.reserved.get(&name) {
            return Ok(*id);
        }
        if LOCAL.contains(&name.as_str()) {
            return Err(Error::Link(format!("{name} is served by the local gateway only")));
        }
        if RESERVED.contains(&name.as_str()) {
            return Err(Error::Link(format!("{name} is reserved but its address is not configured")));
        }
        if !valid_name(&name) {
            return Err(Error::Link(format!("{name}: not a valid .ov name")));
        }
        if let Some((id, t)) = self.cache.lock().unwrap().get(&name) {
            if t.elapsed() < CACHE_TTL {
                return Ok(*id);
            }
        }
        let registrar = *self
            .reserved
            .get("name.ov")
            .ok_or_else(|| Error::Link("name.ov (registrar) address is not configured".into()))?;
        let (status, body) = http_get(&self.client, registrar, &format!("/api/resolve?name={name}")).await?;
        if status == 404 {
            return Err(Error::Link(format!("{name} is not registered")));
        }
        if status != 200 {
            return Err(Error::Link(format!("name.ov answered {status}")));
        }
        let rec: NameRecord = serde_json::from_slice(&body).map_err(|_| Error::Link("name.ov: bad answer".into()))?;
        if rec.name != name {
            return Err(Error::Link("name.ov answered for another name".into()));
        }
        let id = rec.verify().ok_or_else(|| Error::Link("name.ov: record signature is invalid".into()))?;
        self.cache.lock().unwrap().insert(name, (id, Instant::now()));
        Ok(id)
    }
}

/// Минимальный HTTP GET к сервису .ov на порт 80 (тело ≤ 1 МиБ).
pub async fn http_get(client: &Client, id: ServiceId, path: &str) -> Result<(u16, Vec<u8>)> {
    let mut s = client.connect_service(id, 80).await?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", id.to_address());
    s.write(req.as_bytes()).await?;
    let raw = s.read_to_end(1 << 20).await?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| Error::Link("bad HTTP answer".into()))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::Link("bad HTTP status".into()))?;
    Ok((status, raw[split + 4..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use overnet_core::ovaddr::ServiceKey;

    #[test]
    fn built_in_network_is_well_formed() {
        for (n, a) in PINNED {
            assert!(RESERVED.contains(n), "{n} is pinned but not reserved");
            assert!(ServiceId::from_address(a).is_ok(), "{n}: bad address {a}");
        }
        assert_eq!(PINNED.len(), RESERVED.len(), "every reserved name has an address");
        for r in SEED_RELAYS {
            assert!(super::super::dir::RelayDesc::parse(r).is_some(), "bad seed relay {r}");
        }
    }

    #[test]
    fn names() {
        assert!(valid_name("shop.ov"));
        assert!(valid_name("my-shop2.ov"));
        assert!(!valid_name("Shop.ov"));
        assert!(!valid_name("-a.ov"));
        assert!(!valid_name("a.b.ov"));
        assert!(!valid_name("shop.com"));
        assert!(!valid_name(&ServiceKey::generate().id().to_address()));
        assert_eq!(base_name("WWW.Shop.ov."), "shop.ov");
        assert_eq!(base_name("search.ov"), "search.ov");
    }

    #[test]
    fn record_signature_binds_name_and_address() {
        let k = ServiceKey::generate();
        let rec = NameRecord {
            name: "shop.ov".into(),
            address: k.id().to_address(),
            sig: hex::encode(k.sign(&name_message("shop.ov"))),
        };
        assert_eq!(rec.verify(), Some(k.id()));
        let mut moved = rec.clone();
        moved.address = ServiceKey::generate().id().to_address();
        assert_eq!(moved.verify(), None, "the registrar cannot point a name elsewhere");
        let mut renamed = rec;
        renamed.name = "bank.ov".into();
        assert_eq!(renamed.verify(), None);
    }
}
