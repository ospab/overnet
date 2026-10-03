//! `overnet` — командная строка сети.

mod browser;
mod browser_ui;
mod config;
mod legacy;
mod update;

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

const HELP: &str = "overnet — a network that survives the hostility of its own infrastructure

Network:
  overnet bootstrap [address]           relay directory (default 0.0.0.0:8080)
  overnet relay [--listen A] [--advertise A] [--bootstrap A] [--exit off|direct|socks5://h:p] [--key F]
                                        relay; prints the line for \"relays\" in client configs

.ov services:
  overnet keygen <file>                 new service key, prints its .ov address
  overnet address <file>                .ov address of a key
  overnet service --key F --port 80=127.0.0.1:8080 [--port …]
                                        publish a local server as an .ov site
  overnet name-sign <name.ov> --key F   record for registering a name at name.ov
  overnet site name|search|files|mail [--key F] [--data DIR]
                                        a network service site, published

Client:
  overnet gateway [--listen A] [--clearnet direct|exit|block]
                                        local SOCKS5 gateway (127.0.0.1:9150)
  overnet browser [--gateway auto|always|never]
                                        Mullvad Browser with the overnet profile
  overnet resolve <name.ov>             what a name resolves to

  overnet demo                          the whole network on this machine: relays, sites, gateway
  overnet update [--version vX.Y.Z] [--force]
                                        install the latest release (Linux: with sudo)
  overnet version                       installed version
  overnet legacy …                      old commands (before v0.2 circuits)

Common flag: --config <file>. Without it: $OVERNET_CONFIG, then ./config.json,
then config.json in the data directory, then /etc/overnet/config.json.";

/// Флаги вида `--имя значение`; повторяемые копятся.
/// Флаги без значения: следующее слово остаётся само по себе.
const SWITCHES: &[&str] = &["browser", "exit-with-stdin", "force"];

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
                Some(k) if SWITCHES.contains(&k) => flags.entry(k.to_string()).or_default().push(String::new()),
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
    eprintln!("error: {msg}");
    std::process::exit(1)
}

#[tokio::main]
async fn main() {
    let raw: Vec<String> = std::env::args().collect();
    let cmd = raw.get(1).cloned().unwrap_or_default();
    update::cleanup();
    if cmd == "legacy" {
        let mut rest = vec![raw[0].clone()];
        rest.extend(raw[2..].iter().cloned());
        return legacy::run(rest).await;
    }
    let a = Args::parse(raw.get(2..).unwrap_or(&[]));
    let cfg_path = a.flag("config").map(PathBuf::from).unwrap_or_else(config::default_path);
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
            let path = a.pos.first().unwrap_or_else(|| die("specify the key file"));
            if Path::new(path).exists() {
                die(format!("{path} already exists — not overwriting it"));
            }
            let key = ServiceKey::generate();
            save_key(Path::new(path), &key);
            println!("{}", key.id().to_address());
        }
        "address" => {
            let path = a.pos.first().unwrap_or_else(|| die("specify the key file"));
            println!("{}", load_service_key(Path::new(path), false).id().to_address());
        }
        "name-sign" => {
            let name = a.pos.first().unwrap_or_else(|| die("specify a name, e.g. shop.ov")).to_ascii_lowercase();
            let key = load_service_key(Path::new(a.flag("key").unwrap_or_else(|| die("--key is required"))), false);
            let rec = NameRecord {
                address: key.id().to_address(),
                sig: hex::encode(key.sign(&name_message(&name))),
                name,
            };
            println!("{}", serde_json::to_string(&rec).unwrap());
        }
        "service" => {
            let key = load_service_key(Path::new(a.flag("key").unwrap_or_else(|| die("--key is required"))), false);
            let mut ports = HashMap::new();
            for p in a.all("port") {
                let (v, local) = p.split_once('=').unwrap_or_else(|| die(format!("--port {p}: expected PORT=host:port")));
                ports.insert(v.parse::<u16>().unwrap_or_else(|_| die(format!("--port {p}"))), local.to_string());
            }
            if ports.is_empty() {
                die("at least one --port 80=127.0.0.1:8080 is required");
            }
            publish(&cfg, key, ports, None).await;
        }
        "site" => site(&a, &cfg).await,
        "gateway" => {
            let listen = a.flag("listen").unwrap_or(&cfg.gateway.listen).to_string();
            // --browser: шлюз браузера overnet (его запускает сам браузер).
            let for_browser = a.flags.contains_key("browser");
            let default = if for_browser { &cfg.browser.clearnet } else { &cfg.gateway.clearnet };
            let clearnet = Clearnet::parse(a.flag("clearnet").unwrap_or(default)).unwrap_or_else(|e| die(e));
            if a.flags.contains_key("exit-with-stdin") {
                exit_with_stdin();
            }
            let gw = gateway(&cfg, clearnet, for_browser).await;
            serve_gateway(gw, &listen).await;
        }
        "browser" => run_browser(&a, &cfg).await,
        "resolve" => {
            let name = a.pos.first().unwrap_or_else(|| die("specify a name"));
            let c = client(&cfg);
            let names = Names::new(c, &cfg.reserved).unwrap_or_else(|e| die(e));
            match names.resolve(name).await {
                Ok(id) => println!("{}", id.to_address()),
                Err(e) => die(e),
            }
        }
        "demo" => demo(&cfg).await,
        "update" => update::run(a.flag("version"), a.flags.contains_key("force")).unwrap_or_else(|e| die(e)),
        "version" | "--version" | "-V" => println!("overnet {}", env!("CARGO_PKG_VERSION")),
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
        Ok(_) => die(format!("{}: not a service key", path.display())),
        Err(_) if create => {
            let k = ServiceKey::generate();
            save_key(path, &k);
            k
        }
        Err(_) => die(format!(
            "{p}: no such key file. Create one with: overnet keygen {p}",
            p = path.display()
        )),
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
    println!("overnet relay on {listen}, exit: {exit:?}");
    println!("line for \"relays\" in client configs:\n  {pubkey}@{advertise}");
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
        None => println!("service: {addr}"),
    }
    for (p, local) in &ports {
        println!("  port {p} → {local}");
    }
    if let Err(e) = c.refresh_directory().await {
        eprintln!("directory not available yet ({e}); will retry later");
    }
    c.spawn_directory_refresh(Duration::from_secs(600));
    let _ = Service::new(c, key, ports).run().await;
}

async fn site(a: &Args, cfg: &Config) {
    let kind = a.pos.first().map(String::as_str).unwrap_or_else(|| die("which site: name, search, files or mail"));
    if !["name", "search", "files", "mail"].contains(&kind) {
        die(format!("no such site: {kind}"));
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

/// Выйти, когда закроется stdin: браузер держит трубу открытой, пока жив, и
/// шлюз не переживёт его, даже если браузер упадёт.
fn exit_with_stdin() {
    std::thread::spawn(|| {
        let mut buf = [0u8; 256];
        let mut stdin = std::io::stdin();
        while matches!(std::io::Read::read(&mut stdin, &mut buf), Ok(n) if n > 0) {}
        std::process::exit(0);
    });
}

/// `open_external` — страницы browser.ov могут открыть сайт в обычном браузере
/// этой машины (шлюз запущен для браузера, а не на сервере).
async fn gateway(cfg: &Config, clearnet: Clearnet, open_external: bool) -> Arc<Gateway> {
    let c = client(cfg);
    let names = Names::new(c.clone(), &cfg.reserved).unwrap_or_else(|e| die(e));
    match c.refresh_directory().await {
        Ok(n) => println!("directory: {n} relays"),
        Err(e) => eprintln!("directory not available yet ({e}); will retry on the first request"),
    }
    c.spawn_directory_refresh(Duration::from_secs(600));
    let missing: Vec<&str> = RESERVED.iter().copied().filter(|n| !names.reserved().contains_key(*n)).collect();
    if !missing.is_empty() {
        eprintln!("addresses not set: {} (the \"reserved\" section of the config)", missing.join(", "));
    }
    let ui = Arc::new(browser_ui::Ui::new(c.clone(), clearnet, open_external));
    let ui_addr = serve_http(browser_ui::router(ui), "127.0.0.1:0").await;
    let local = [("browser.ov".to_string(), ui_addr.parse().expect("loopback address"))].into_iter().collect();
    Arc::new(Gateway { client: c, names, clearnet, local })
}

async fn serve_gateway(gw: Arc<Gateway>, listen: &str) {
    let l = tokio::net::TcpListener::bind(listen).await.unwrap_or_else(|e| die(format!("{listen}: {e}")));
    println!("gateway: socks5://{listen}  (non-.ov: {:?})", gw.clearnet);
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
                eprintln!("warning: ostp does not serve .ov — .ov sites will not open");
            }
            None
        }
        GatewayMode::Auto if ostp => {
            println!("the ostp VPN serves .ov — starting the browser without a proxy");
            None
        }
        _ => {
            if browser::gateway_running(&listen).await {
                println!("gateway already running on {listen}");
            } else {
                let clearnet = Clearnet::parse(&cfg.browser.clearnet).unwrap_or_else(|e| die(e));
                let gw = gateway(cfg, clearnet, true).await;
                let l = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|e| die(format!("{listen}: {e}")));
                tokio::spawn(gw.serve(l));
                println!("gateway: socks5://{listen}");
            }
            Some(listen)
        }
    };
    let (exe, mullvad) = browser::find_browser(&cfg.browser.path).unwrap_or_else(|e| die(e));
    if !mullvad {
        eprintln!("warning: Mullvad Browser not found, starting Firefox — it has no fingerprinting protection");
    }
    let profile = data_dir().join("browser-profile");
    browser::write_profile(&profile, proxy.as_deref()).unwrap_or_else(|e| die(e));
    println!("browser: {}", exe.display());
    if proxy.is_some() {
        println!("keep this window open: the browser reaches overnet through it");
    }
    match browser::launch(&exe, &profile).await {
        Ok(_) => {}
        Err(e) => die(format!("{}: {e}", exe.display())),
    }
}

/// Вся сеть на одной машине: 5 релеев (последний — выход), четыре служебных
/// сайта и шлюз. Порты и ключи постоянные, поэтому адреса между запусками не
/// меняются, а записанный конфиг годится для своих команд в другом окне.
const DEMO_PORTS: std::ops::Range<u16> = 47401..47406;

async fn demo(cfg: &Config) {
    let dir = data_dir().join("demo");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| die(e));
    let mut relays: Vec<RelayDesc> = Vec::new();
    let mut nodes = Vec::new();
    for (i, port) in DEMO_PORTS.enumerate() {
        let exit = if i == 4 { ExitPolicy::Direct { allow_private: false } } else { ExitPolicy::Off };
        let key = overnet_node::web::load_or_create_key(&dir.join(format!("relay{i}.key")).to_string_lossy());
        let node = Node::new(key, exit.clone());
        let addr = format!("127.0.0.1:{port}");
        let l = TcpListenerLink::bind(&addr)
            .await
            .unwrap_or_else(|e| die(format!("{addr}: {e} — is the demo already running in another window?")));
        relays.push(RelayDesc { pubkey: hex::encode(node.pubkey()), address: addr, exit: exit.enabled() });
        let n = node.clone();
        tokio::spawn(async move { n.serve(l).await });
        nodes.push(node);
    }
    for n in &nodes {
        n.set_directory(relays.clone());
    }
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
    // Конфиг для своих команд в другом окне (service, gateway, resolve…).
    let cfg_file = dir.join("config.json");
    let json = serde_json::json!({
        "relays": demo_cfg.relays,
        "reserved": demo_cfg.reserved,
        "gateway": { "listen": cfg.gateway.listen, "clearnet": cfg.gateway.clearnet },
    });
    std::fs::write(&cfg_file, serde_json::to_string_pretty(&json).unwrap()).unwrap_or_else(|e| die(e));
    for (kind, key) in keys {
        let data = dir.join("sites").join(kind);
        std::fs::create_dir_all(&data).unwrap_or_else(|e| die(e));
        let app = site_router(kind, &data, &demo_cfg, Duration::from_secs(30));
        let local = serve_http(app, "127.0.0.1:0").await;
        let c = Client::new(Node::new(OnionKey::generate(), ExitPolicy::Off), demo_cfg.relays().unwrap());
        let svc = Service::new(c, key, HashMap::from([(80, local.clone())]));
        let up = svc.ensure_intros().await;
        println!("{kind}.ov  {}  (intro points: {up}, local http://{local})", svc.id().to_address());
        tokio::spawn(svc.run());
    }
    let gw = gateway(&demo_cfg, Clearnet::parse(&cfg.gateway.clearnet).unwrap_or(Clearnet::Direct), true).await;
    println!("\nthe demo network is up. In another window:");
    println!("  browser:    overnet browser --gateway always");
    println!("  your site:  overnet service --config \"{}\" --key my.key --port 80=127.0.0.1:8080", cfg_file.display());
    println!("  or any browser with SOCKS5 {} and \"proxy DNS\" on.\n", cfg.gateway.listen);
    serve_gateway(gw, &cfg.gateway.listen).await;
}

#[cfg(test)]
mod tests {
    use super::Args;

    #[test]
    fn switches_take_no_value() {
        let raw: Vec<String> = ["--browser", "--listen", "127.0.0.1:1", "--force", "x"].iter().map(|s| s.to_string()).collect();
        let a = Args::parse(&raw);
        assert!(a.flags.contains_key("browser") && a.flags.contains_key("force"));
        assert_eq!(a.flag("listen"), Some("127.0.0.1:1"));
        assert_eq!(a.pos, vec!["x"]);
    }
}
