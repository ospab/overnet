//! Конфиг overnet (`config.json`, путь меняется флагом `--config`).
//! Все поля необязательны; флаги командной строки важнее конфига.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use overnet_node::net::dir::RelayDesc;

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Секрет сети (knock на линках) — как и раньше.
    pub token: Option<String>,
    /// Релеи для первого каталога: "pubkeyhex@host:port".
    pub relays: Vec<String>,
    /// Адреса зарезервированных имён: {"search.ov": "<56 символов>.ov", …}.
    pub reserved: HashMap<String, String>,
    pub gateway: GatewayCfg,
    pub relay: RelayCfg,
    pub browser: BrowserCfg,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct GatewayCfg {
    pub listen: String,
    /// direct | exit | block — куда идёт всё, что не .ov.
    pub clearnet: String,
}

impl Default for GatewayCfg {
    fn default() -> Self {
        GatewayCfg { listen: "127.0.0.1:9150".into(), clearnet: "direct".into() }
    }
}

#[derive(Deserialize)]
#[serde(default)]
pub struct RelayCfg {
    pub listen: String,
    /// Адрес, который релей сообщает каталогу (если listen = 0.0.0.0).
    pub advertise: String,
    pub bootstrap: String,
    /// off | direct | socks5://host:port
    pub exit: String,
    pub key: String,
}

impl Default for RelayCfg {
    fn default() -> Self {
        RelayCfg {
            listen: "0.0.0.0:4040".into(),
            advertise: String::new(),
            bootstrap: "127.0.0.1:8080".into(),
            exit: "off".into(),
            key: "relay.key".into(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct BrowserCfg {
    /// Путь к Mullvad Browser (если не нашёлся сам).
    pub path: String,
    /// auto | always | never — поднимать ли локальный шлюз.
    pub gateway: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
            Err(_) => Ok(Config::default()),
        }
    }

    pub fn relays(&self) -> Result<Vec<RelayDesc>, String> {
        self.relays
            .iter()
            .map(|r| RelayDesc::parse(r).ok_or_else(|| format!("relays: '{r}' is not pubkeyhex@host:port")))
            .collect()
    }
}

/// Каталог данных overnet (ключи по умолчанию, профиль браузера, данные сайтов).
pub fn data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("overnet")
}
