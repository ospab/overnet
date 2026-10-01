//! `overnet` — командная строка сети.

mod browser;
mod config;
mod legacy;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use overnet_core::onion::OnionKey;
use overnet_core::ovaddr::{name_message, ServiceKey};
use overnet_link_tcp::TcpListenerLink;
use overnet_node::bootstrap::{keep_registered, BootstrapServer, NodeInfo};
use overnet_node::net::client::Client;
use overnet_node::net::dir::{self, RelayDesc};
use overnet_node::net::exit::ExitPolicy;
use overnet_node::net::gateway::{Clearnet, Gateway};
use overnet_node::net::names::{NameRecord, Names, RESERVED};
use overnet_node::net::service::Service;
use overnet_node::net::Node;

use config::{data_dir, Config};

const HELP: &str = "overnet — сеть, которая переживает враждебность собственной инфраструктуры

Сеть:
  overnet bootstrap [адрес]             каталог релеев (по умолчанию 0.0.0.0:8080)
  overnet relay [--listen A] [--advertise A] [--bootstrap A] [--exit off|direct|socks5://h:p] [--key F]
                                        релей; печатает строку для \"relays\" в конфиге клиентов

Сервисы .ov:
  overnet keygen <файл>                 новый ключ сервиса, печатает его адрес .ov
  overnet address <файл>                адрес .ov ключа
  overnet service --key F --port 80=127.0.0.1:8080 [--port …]
                                        опубликовать локальный сервер как сайт .ov
  overnet name-sign <имя.ov> --key F    запись для регистрации имени на name.ov
  overnet site name|search|files|mail [--key F] [--data DIR]
                                        служебный сайт сети вместе с публикацией

Клиент:
  overnet gateway [--listen A] [--clearnet direct|exit|block]
                                        локальный SOCKS5-шлюз (127.0.0.1:9150)
  overnet browser [--gateway auto|always|never]
                                        Mullvad Browser с профилем overnet
  overnet resolve <имя.ov>              во что резолвится имя

  overnet demo                          вся сеть на этой машине: релеи, сайты, шлюз
  overnet legacy …                      старые команды (до цепей v0.2)

Общий флаг: --config <файл> (по умолчанию config.json).";

/// Флаги вида `--имя значение`; повторяемые копятся.
struct Args {
    pos: Vec<String>,
    flags: HashMap<String, Vec<String>>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut pos = Vec::new();
        let mut flags: HashMap<String, Vec<String>> = HashMap::new();
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            match a.strip_prefix("--") {
                Some(k) => {
                    let v = it.next().cloned().unwrap_or_default();
                    flags.entry(k.to_string()).or_default().push(v);
                }
                None => pos.push(a.clone()),
            }
        }
        Args { pos, flags }
    }

    fn flag(&self, k: &str) -> Option<&str> {
        self.flags.get(k).and_then(|v| v.last()).map(String::as_str)
    }

    fn all(&self, k: &str) -> Vec<String> {
        self.flags.get(k).cloned().unwrap_or_default()
    }
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("ошибка: {msg}");
    std::process::exit(1)
}

#[tokio::main]
async fn main() {
    let raw: Vec<String> = std::env::args().collect();
    let cmd = raw.get(1).cloned().unwrap_or_default();
    if cmd == "legacy" {
        let mut rest = vec![raw[0].clone()];
        rest.extend(raw[2..].iter().cloned());
        return legacy::run(rest).await;
    }
    let a = Args::parse(raw.get(2..).unwrap_or(&[]));
    let cfg_path = PathBuf::from(a.flag("config").unwrap_or("config.json"));
    let cfg = Config::load(&cfg_path).unwrap_or_else(|e| die(e));

    match cmd.as_str() {
        "bootstrap" => {
            let bind = a.pos.first().cloned().unwrap_or_else(|| "0.0.0.0:8080".into());
            if let Err(e) = BootstrapServer::new().serve(&bind).await {
                die(e);
            }
        }
        "relay" => relay(&a, &cfg).await,
        "keygen" => {
            let path = a.pos.first().unwrap_or_else(|| die("укажите файл ключа"));
            if Path::new(path).exists() {
                die(format!("{path} уже есть — не перезаписываю"));
            }
            let key = ServiceKey::generate();
            save_key(Path::new(path), &key);
            println!("{}", key.id().to_address());
        }
        "address" => {
            let path = a.pos.first().unwrap_or_else(|| die("укажите файл ключа"));
            println!("{}", load_service_key(Path::new(path), false).id().to_address());
        }
        "name-sign" => {
            let name = a.pos.first().unwrap_or_else(|| die("укажите имя, например shop.ov")).to_ascii_lowercase();
            let key = load_service_key(Path::new(a.flag("key").unwrap_or_else(|| die("нужен --key"))), false);
            let rec = NameRecord {
                address: key.id().to_address(),
                sig: hex::encode(key.sign(&name_message(&name))),
                name,
            };
            println!("{}", serde_json::to_string(&rec).unwrap());
        }
        "service" => {
            let key = load_service_key(Path::new(a.flag("key").unwrap_or_else(|| die("нужен --key"))), false);
            let mut ports = HashMap::new();
            for p in a.all("port") {
                let (v, local) = p.split_once('=').unwrap_or_else(|| die(format!("--port {p}: нужно ПОРТ=host:port")));
                ports.insert(v.parse::<u16>().unwrap_or_else(|_| die(format!("--port {p}"))), local.to_string());
            }
            if ports.is_empty() {
                die("нужен хотя бы один --port 80=127.0.0.1:8080");
            }
            publish(&cfg, key, ports, None).await;
        }
        "site" => site(&a, &cfg).await,
        "gateway" => {
            let listen = a.flag("listen").unwrap_or(&cfg.gateway.listen).to_string();
            let clearnet = Clearnet::parse(a.flag("clearnet").unwrap_or(&cfg.gateway.clearnet)).unwrap_or_else(|e| die(e));
            let gw = gateway(&cfg, clearnet).await;
            serve_gateway(gw, &listen).await;
        }
        "browser" => run_browser(&a, &cfg).await,
        "resolve" => {
            let name = a.pos.first().unwrap_or_else(|| die("укажите имя"));
            let c = client(&cfg);
            let names = Names::new(c, &cfg.reserved).unwrap_or_else(|e| die(e));
            match names.resolve(name).await {
                Ok(id) => println!("{}", id.to_address()),
                Err(e) => die(e),
            }
        }
        "demo" => demo(&cfg).await,
        _ => println!("{HELP}"),
    }
}

// ── ключи ──────────────────────────────────────────────────────────────────

fn save_key(path: &Path, key: &ServiceKey) {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, key.to_bytes()).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
}

fn load_service_key(path: &Path, create: bool) -> ServiceKey {
    match std::fs::read(path) {
        Ok(b) if b.len() == 32 => ServiceKey::from_bytes(b.try_into().unwrap()),
        Ok(_) => die(format!("{}: не ключ сервиса", path.display())),
        Err(_) if create => {
            let k = ServiceKey::generate();
            save_key(path, &k);
            k
        }
        Err(e) => die(format!("{}: {e}", path.display())),
    }
}

// ── узлы ───────────────────────────────────────────────────────────────────

fn client(cfg: &Config) -> Arc<Client> {
    let relays = cfg.relays().unwrap_or_else(|e| die(e));
    Client::new(Node::new(OnionKey::generate(), ExitPolicy::Off), relays)
}

async fn relay(a: &Args, cfg: &Config) {
    let listen = a.flag("listen").unwrap_or(&cfg.relay.listen).to_string();
    let bootstrap = a.flag("bootstrap").unwrap_or(&cfg.relay.bootstrap).to_string();
    let exit = ExitPolicy::parse(a.flag("exit").unwrap_or(&cfg.relay.exit)).unwrap_or_else(|e| die(e));
    let key = overnet_node::web::load_or_create_key(a.flag("key").unwrap_or(&cfg.relay.key));
    let advertise = a
        .flag("advertise")
        .map(str::to_string)
        .or_else(|| Some(cfg.relay.advertise.clone()).filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("ADVERTISE_IP").ok())
        .unwrap_or_else(|| listen.clone());
    let node = Node::new(key, exit.clone());
    let listener = TcpListenerLink::bind(&listen).await.unwrap_or_else(|e| die(e));
    let pubkey = hex::encode(node.pubkey());
    println!("overnet relay на {listen}, выход: {exit:?}");
    println!("строка для \"relays\" в конфиге клиентов:\n  {pubkey}@{advertise}");
    let info = NodeInfo {
        pubkey,
        address: advertise,
        role: if exit.enabled() { "exit" } else { "relay" }.into(),
        name: String::new(),
    };
    tokio::spawn(keep_registered(bootstrap.clone(), info, 30));
    tokio::spawn(dir::keep_fresh_from_bootstrap(node.clone(), bootstrap, Duration::from_secs(60)));
    if let Err(e) = node.serve(listener).await {
        die(e);
    }
}

/// Опубликовать сервис и работать, пока не остановят.
async fn publish(cfg: &Config, key: ServiceKey, ports: HashMap<u16, String>, label: Option<&str>) {
    let c = client(cfg);
    let addr = key.id().to_address();
    match label {
        Some(l) => println!("{l} → {addr}"),
        None => println!("сервис: {addr}"),
    }
    for (p, local) in &ports {
        println!("  порт {p} → {local}");
    }
    if let Err(e) = c.refresh_directory().await {
        eprintln!("каталог пока недоступен ({e}); повторю позже");
    }
    c.spawn_directory_refresh(Duration::from_secs(600));
    let _ = Service::new(c, key, ports).run().await;
}

async fn site(a: &Args, cfg: &Config) {
    let kind = a.pos.first().map(String::as_str).unwrap_or_else(|| die("какой сайт: name, search, files или mail"));
    if !["name", "search", "files", "mail"].contains(&kind) {
        die(format!("нет такого сайта: {kind}"));
    }
    let data = a.flag("data").map(PathBuf::from).unwrap_or_else(|| data_dir().join("sites").join(kind));
    std::fs::create_dir_all(&data).unwrap_or_else(|e| die(e));
    let key_path = a.flag("key").map(PathBuf::from).unwrap_or_else(|| data.join("service.key"));
    let key = load_service_key(&key_path, true);
    let app = site_router(kind, &data, cfg, Duration::from_secs(600));
    let local = serve_http(app, a.flag("listen").unwrap_or("127.0.0.1:0")).await;
    publish(cfg, key, HashMap::from([(80, local)]), Some(&format!("{kind}.ov"))).await;
}

fn site_router(kind: &str, data: &Path, cfg: &Config, crawl_every: Duration) -> axum::Router {
    use overnet_sites::{files, mail, registrar, search};
    match kind {
        "name" => registrar::router(registrar::Registrar::open(data)),
        "files" => {
            let f = files::Files::open(data, 64 << 20, Duration::from_secs(7 * 86400)).unwrap_or_else(|e| die(e));
            f.spawn_sweeper();
            files::router(f)
        }
        "mail" => mail::router(mail::Mail::open(data)),
        _ => {
            let s = search::Search::new();
            let c = client(cfg);
            let names = Names::new(c.clone(), &cfg.reserved).unwrap_or_else(|e| die(e));
            s.spawn_crawler(c, names, crawl_every);
            search::router(s)
        }
    }
}

/// Поднять HTTP-сервер на loopback; вернуть его адрес.
async fn serve_http(app: axum::Router, listen: &str) -> String {
    let l = tokio::net::TcpListener::bind(listen).await.unwrap_or_else(|e| die(e));
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(l, app).await });
    addr
}

async fn gateway(cfg: &Config, clearnet: Clearnet) -> Arc<Gateway> {
    let c = client(cfg);
    let names = Names::new(c.clone(), &cfg.reserved).unwrap_or_else(|e| die(e));
    match c.refresh_directory().await {
        Ok(n) => println!("каталог: {n} релеев"),
        Err(e) => eprintln!("каталог пока недоступен ({e}); повторю при первом запросе"),
    }
    c.spawn_directory_refresh(Duration::from_secs(600));
    let missing: Vec<&str> = RESERVED.iter().copied().filter(|n| !names.reserved().contains_key(*n)).collect();
    if !missing.is_empty() {
        eprintln!("не заданы адреса: {} (раздел \"reserved\" в конфиге)", missing.join(", "));
    }
    Arc::new(Gateway { client: c, names, clearnet })
}

async fn serve_gateway(gw: Arc<Gateway>, listen: &str) {
    let l = tokio::net::TcpListener::bind(listen).await.unwrap_or_else(|e| die(format!("{listen}: {e}")));
    println!("шлюз: socks5://{listen}  (не-.ov: {:?})", gw.clearnet);
    if let Err(e) = gw.serve(l).await {
        die(e);
    }
}

async fn run_browser(a: &Args, cfg: &Config) {
    use browser::GatewayMode;
    let mode = GatewayMode::parse(a.flag("gateway").unwrap_or(&cfg.browser.gateway)).unwrap_or_else(|e| die(e));
    let listen = cfg.gateway.listen.clone();
    let ostp = browser::ostp_serves_ov().await;
    let proxy = match mode {
        GatewayMode::Never => {
            if !ostp {
                eprintln!("внимание: ostp не обслуживает .ov — сайты .ov не откроются");
            }
            None
        }
        GatewayMode::Auto if ostp => {
            println!("VPN ostp обслуживает .ov — браузер без прокси");
            None
        }
        _ => {
            if browser::gateway_running(&listen).await {
                println!("шлюз уже работает на {listen}");
            } else {
                if cfg.relays.is_empty() {
                    die("нет релеев в конфиге (\"relays\") и нет ostp с overnet; для пробы запустите `overnet demo`");
                }
                let clearnet = Clearnet::parse(&cfg.gateway.clearnet).unwrap_or_else(|e| die(e));
                let gw = gateway(cfg, clearnet).await;
                let l = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|e| die(format!("{listen}: {e}")));
                tokio::spawn(gw.serve(l));
                println!("шлюз: socks5://{listen}");
            }
            Some(listen)
        }
    };
    let (exe, mullvad) = browser::find_browser(&cfg.browser.path).unwrap_or_else(|e| die(e));
    if !mullvad {
        eprintln!("внимание: Mullvad Browser не найден, запускаю Firefox — у него нет защиты от отпечатков");
    }
    let profile = data_dir().join("browser-profile");
    browser::write_profile(&profile, proxy.as_deref()).unwrap_or_else(|e| die(e));
    println!("браузер: {}", exe.display());
    match browser::launch(&exe, &profile).await {
        Ok(_) => {}
        Err(e) => die(format!("{}: {e}", exe.display())),
    }
}

/// Вся сеть на одной машине: 5 релеев (последний — выход), четыре служебных
/// сайта и шлюз. Ключи сайтов сохраняются, адреса между запусками не меняются.
async fn demo(cfg: &Config) {
    let mut relays: Vec<RelayDesc> = Vec::new();
    let mut nodes = Vec::new();
    for i in 0..5 {
        let exit = if i == 4 { ExitPolicy::Direct { allow_private: false } } else { ExitPolicy::Off };
        let node = Node::new(OnionKey::generate(), exit.clone());
        let l = TcpListenerLink::bind("127.0.0.1:0").await.unwrap_or_else(|e| die(e));
        relays.push(RelayDesc {
            pubkey: hex::encode(node.pubkey()),
            address: l.local_addr().unwrap().to_string(),
            exit: exit.enabled(),
        });
        let n = node.clone();
        tokio::spawn(async move { n.serve(l).await });
        nodes.push(node);
    }
    for n in &nodes {
        n.set_directory(relays.clone());
    }
    let dir = data_dir().join("demo");
    let mut reserved = HashMap::new();
    let mut keys = Vec::new();
    for kind in ["name", "search", "files", "mail"] {
        let key = load_service_key(&dir.join(format!("{kind}.key")), true);
        reserved.insert(format!("{kind}.ov"), key.id().to_address());
        keys.push((kind, key));
    }
    let demo_cfg = Config {
        relays: relays.iter().map(|r| format!("{}@{}", r.pubkey, r.address)).collect(),
        reserved,
        ..Config::default()
    };
    for (kind, key) in keys {
        let data = dir.join("sites").join(kind);
        std::fs::create_dir_all(&data).unwrap_or_else(|e| die(e));
        let app = site_router(kind, &data, &demo_cfg, Duration::from_secs(30));
        let local = serve_http(app, "127.0.0.1:0").await;
        let c = Client::new(Node::new(OnionKey::generate(), ExitPolicy::Off), demo_cfg.relays().unwrap());
        let svc = Service::new(c, key, HashMap::from([(80, local.clone())]));
        let up = svc.ensure_intros().await;
        println!("{kind}.ov  {}  (точек входа: {up}, локально http://{local})", svc.id().to_address());
        tokio::spawn(svc.run());
    }
    let gw = gateway(&demo_cfg, Clearnet::parse(&cfg.gateway.clearnet).unwrap_or(Clearnet::Direct)).await;
    println!("\nдемо-сеть работает. В другом окне: overnet browser");
    println!("или любой браузер с SOCKS5 {} и удалённым DNS.\n", cfg.gateway.listen);
    serve_gateway(gw, &cfg.gateway.listen).await;
}
