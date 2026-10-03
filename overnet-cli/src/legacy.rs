//! Команды overnet до цепей v0.2 (запрос/ответ без потоков). Оставлены для
//! Tauri-приложения; новые сети строятся командами из main.rs.
#![allow(dead_code)]

use std::time::Instant;
use std::sync::Arc;
use tokio::sync::mpsc;
use serde::{Serialize, Deserialize};

use overnet_core::session::TransportKey;
use overnet_core::Identity;
use overnet_core::onion::{self, OnionKey};
use overnet_node::router::Router;
use overnet_node::bootstrap::{BootstrapServer, NodeInfo, register_node};
use overnet_link_tcp::TcpListenerLink;

#[derive(Serialize, Deserialize, Debug)]
struct Request {
    reply_to: String,
    reply_path: Vec<String>,
    method: String,
    path: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct Response {
    status: u16,
    body: String,
}

/// Старые команды (до цепей v0.2): `overnet legacy <команда> …`.
/// Нужны, пока Tauri-приложение работает на старом стеке.
pub async fn run(args: Vec<String>) {
    match args.get(1).map(String::as_str) {
        Some("bootstrap") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:8080".into());
            let server = BootstrapServer::new();
            if let Err(e) = server.serve(&bind).await {
                eprintln!("Bootstrap stopped: {e}");
            }
        }
        Some("relay") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:4040".into());
            let bootstrap_addr = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            
            let key = OnionKey::generate();
            let pubkey_hex = hex(&key.public());
            println!("overnet relay listening on {bind}");
            println!("onion pubkey: {}", pubkey_hex);
            
            let advertise = std::env::var("ADVERTISE_IP").unwrap_or_else(|_| bind.clone());
            let info = NodeInfo {
                pubkey: pubkey_hex,
                address: advertise,
                role: "relay".to_string(),
                name: String::new(),
            };
            
            // Периодически регистрируемся (чтобы пережить рестарт bootstrap-сервера)
            tokio::spawn(overnet_node::bootstrap::keep_registered(
                bootstrap_addr.clone(),
                info,
                30,
            ));

            let listener = TcpListenerLink::bind(&bind).await.expect("bind");
            // run_relay сам пре-коннектится к сервисам — это и есть обратный путь для 2 хопов.
            if let Err(e) = overnet_node::web::run_relay(listener, key, bootstrap_addr).await {
                eprintln!("relay stopped: {e}");
            }
        }
        Some("service") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:4040".into());
            let bootstrap_addr = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            let name = args.get(4).cloned().unwrap_or_default();

            // Именованный сервис → персистентный ключ (домен переживает рестарт);
            // безымянный → эфемерный.
            let key = if name.is_empty() {
                OnionKey::generate()
            } else {
                overnet_node::web::load_or_create_key(&format!("{name}.key"))
            };
            let pubkey_hex = hex(&key.public());
            println!("overnet service listening on {bind}");
            if !name.is_empty() {
                println!("service name: {}", name);
            }
            println!("onion pubkey: {}", pubkey_hex);
            
            let advertise = std::env::var("ADVERTISE_IP").unwrap_or_else(|_| bind.clone());
            let info = NodeInfo {
                pubkey: pubkey_hex,
                address: advertise,
                role: "service".to_string(),
                name,
            };
            
            tokio::spawn(async move {
                loop {
                    println!("Registering with bootstrap server {}...", bootstrap_addr);
                    match register_node(&bootstrap_addr, info.clone()).await {
                        Ok(_) => println!("Successfully registered with bootstrap server!"),
                        Err(e) => eprintln!("Failed to register with bootstrap server: {:?}", e),
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
            });
            
            let (tx, mut rx) = mpsc::unbounded_channel();
            let router = Arc::new(Router::new(key, tx));
            
            let router_clone = router.clone();
            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    println!("=> Received request: {}", String::from_utf8_lossy(&msg));
                    if let Ok(req) = serde_json::from_slice::<Request>(&msg) {
                        println!("Processing request to {}", req.path);
                        
                        let body = format!("<html><body><h1>Welcome to overnet!</h1><p>You requested: {}</p></body></html>", req.path);
                        let resp = Response { status: 200, body };
                        let resp_bytes = serde_json::to_vec(&resp).unwrap();
                        
                        // Parse reverse path
                        let mut reverse_path = Vec::new();
                        for hop_hex in req.reply_path {
                            if let Ok(hop_bytes) = hex::decode(&hop_hex) {
                                if hop_bytes.len() == 32 {
                                    let mut pk = [0u8; 32];
                                    pk.copy_from_slice(&hop_bytes);
                                    reverse_path.push(pk);
                                }
                            }
                        }
                        
                        if let Ok(target_bytes) = hex::decode(&req.reply_to) {
                            if target_bytes.len() == 32 {
                                let mut pk = [0u8; 32];
                                pk.copy_from_slice(&target_bytes);
                                reverse_path.push(pk);
                            }
                        }

                        if reverse_path.is_empty() { continue; }
                        
                        // Wrap response in an onion packet
                        if let Ok(packet) = onion::wrap(&reverse_path, &resp_bytes) {
                            // Send packet to the first hop (the relay connected to us)
                            let first_hop = reverse_path[0];
                            if let Err(e) = router_clone.send_to_neighbor(&first_hop, packet).await {
                                eprintln!("Failed to send response: {:?}", e);
                            } else {
                                println!("Response sent successfully!");
                            }
                        }
                    }
                }
            });

            let listener = TcpListenerLink::bind(&bind).await.expect("bind");
            if let Err(e) = router.serve(listener).await {
                eprintln!("service stopped: {e}");
            }
        }
        Some("server") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:4040".into());
            let key = TransportKey::generate().expect("keygen");
            println!("overnet server listening on {bind}");
            println!("static key: {}", hex(&key.public));
            if let Err(e) = overnet_node::run_server(&bind, key).await {
                eprintln!("server stopped: {e}");
                std::process::exit(1);
            }
        }
        Some("client") => {
            let servers: Vec<String> = args.iter().skip(2).cloned().collect();
            if servers.is_empty() {
                eprintln!("usage: overnet client <addr> [addr...]");
                std::process::exit(2);
            }
            let key = TransportKey::generate().expect("keygen");
            println!("pinging {} servers...", servers.len());
            let mut ok = 0usize;
            for addr in &servers {
                let t = Instant::now();
                match overnet_node::ping(addr, &key, b"ping").await {
                    Ok(reply) => {
                        ok += 1;
                        println!(
                            "[OK]   {addr}  echo={:?}  {} ms",
                            String::from_utf8_lossy(&reply),
                            t.elapsed().as_millis()
                        );
                    }
                    Err(e) => println!("[FAIL] {addr}  {e}"),
                }
            }
            println!("result: {ok}/{} reachable", servers.len());
        }
        Some("gateway") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:8088".into());
            let bootstrap = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            println!("overnet gateway http://{bind} -> bootstrap {bootstrap}");
            if let Err(e) = overnet_node::web::run_gateway(&bind, bootstrap).await {
                eprintln!("gateway stopped: {e}");
                std::process::exit(1);
            }
        }
        // search-узел убран: каталог сети показывает домашняя страница браузера.
        Some("mailbox") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:4060".into());
            let boot = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            let key = overnet_node::web::load_or_create_key("mail.ov.key");
            println!("overnet mailbox (mail.ov) on {bind}, pubkey {}", hex(&key.public()));
            let info = NodeInfo { pubkey: hex(&key.public()), address: bind.clone(), role: "service".into(), name: "mail.ov".into() };
            tokio::spawn(overnet_node::bootstrap::keep_registered(boot.clone(), info, 30));
            let l = TcpListenerLink::bind(&bind).await.expect("bind");
            if let Err(e) = overnet_node::messenger::run_mailbox(l, key).await {
                eprintln!("mailbox stopped: {e}");
            }
        }
        Some("msg-demo") => {
            let mb_addr = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:4060".into());
            let Some(mb_pub) = args.get(3).cloned() else {
                eprintln!("usage: overnet msg-demo <mailbox_addr> <mailbox_pubkey>");
                std::process::exit(2);
            };
            let mailbox = NodeInfo { pubkey: mb_pub, address: mb_addr, role: "service".into(), name: "mail.ov".into() };
            let bob = OnionKey::generate();
            println!("Bob pubkey: {}", hex(&bob.public()));
            match overnet_node::messenger::send_message(&mailbox, bob.public(), b"privet bob!").await {
                Ok(_) => println!("Alice -> sent a message to Bob"),
                Err(e) => { eprintln!("send error: {e}"); std::process::exit(1); }
            }
            match overnet_node::messenger::fetch_inbox(&mailbox, &bob).await {
                Ok(msgs) => {
                    println!("Bob fetched {} messages:", msgs.len());
                    for m in msgs { println!("  > {}", String::from_utf8_lossy(&m)); }
                }
                Err(e) => eprintln!("fetch error: {e}"),
            }
        }
        Some("demo") => {
            // Всё-в-одном локально: bootstrap + search.ov + relay. Решает «пустую сеть»:
            // одна команда → рабочая мини-сеть с контентом.
            let boot = "127.0.0.1:8080".to_string();
            {
                let boot = boot.clone();
                tokio::spawn(async move {
                    if let Err(e) = BootstrapServer::new().serve(&boot).await {
                        eprintln!("demo bootstrap FAILED: {e} — is port 8080 busy? Stop the old overnet.");
                    }
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;

            // search.ov — лендинг + каталог
            {
                let key = OnionKey::generate();
                let addr = "127.0.0.1:7001".to_string();
                let info = NodeInfo {
                    pubkey: hex(&key.public()),
                    address: addr.clone(),
                    role: "service".into(),
                    name: "search.ov".into(),
                };
                tokio::spawn(overnet_node::bootstrap::keep_registered(boot.clone(), info, 30));
                let l = TcpListenerLink::bind(&addr).await.expect("bind search");
                let boot = boot.clone();
                tokio::spawn(async move {
                    let _ = overnet_node::web::run_search(l, key, boot).await;
                });
            }

            // relay
            {
                let key = OnionKey::generate();
                let addr = "127.0.0.1:7002".to_string();
                let info = NodeInfo {
                    pubkey: hex(&key.public()),
                    address: addr.clone(),
                    role: "relay".into(),
                    name: String::new(),
                };
                tokio::spawn(overnet_node::bootstrap::keep_registered(boot.clone(), info, 30));
                let l = TcpListenerLink::bind(&addr).await.expect("bind relay");
                let boot = boot.clone();
                tokio::spawn(async move {
                    let _ = overnet_node::web::run_relay(l, key, boot).await;
                });
            }

            println!("overnet demo network is up:");
            println!("  bootstrap  127.0.0.1:8080");
            println!("  search.ov  (welcome + catalog)");
            println!("  relay");
            println!("→ browser: Bootstrap = 127.0.0.1:8080, then overnet://search.ov/");
            std::future::pending::<()>().await
        }
        Some("files") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:4080".into());
            let boot = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            let dir = args.get(4).cloned().unwrap_or_else(|| "overnet-files".into());
            let key = overnet_node::web::load_or_create_key("files.ov.key");
            println!("overnet files (files.ov) on {bind}, store={dir}, pubkey {}", hex(&key.public()));
            let advertise = std::env::var("ADVERTISE_IP").unwrap_or_else(|_| bind.clone());
            let info = NodeInfo {
                pubkey: hex(&key.public()),
                address: advertise,
                role: "service".into(),
                name: "files.ov".into(),
            };
            tokio::spawn(overnet_node::bootstrap::keep_registered(boot, info, 30));
            let l = TcpListenerLink::bind(&bind).await.expect("bind");
            if let Err(e) = overnet_node::web::run_filestore(l, key, dir).await {
                eprintln!("files stopped: {e}");
            }
        }
        Some("host") => {
            let dir = args.get(2).cloned().unwrap_or_else(|| ".".into());
            let bind = args.get(3).cloned().unwrap_or_else(|| "0.0.0.0:4070".into());
            let boot = args.get(4).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            let name = args.get(5).cloned().unwrap_or_else(|| "site.ov".into());
            let key = overnet_node::web::load_or_create_key(&format!("{name}.key"));
            println!("overnet host '{name}' from {dir} on {bind}, pubkey {}", hex(&key.public()));
            let advertise = std::env::var("ADVERTISE_IP").unwrap_or_else(|_| bind.clone());
            let info = NodeInfo {
                pubkey: hex(&key.public()),
                address: advertise,
                role: "service".into(),
                name,
            };
            tokio::spawn(overnet_node::bootstrap::keep_registered(boot, info, 30));
            let l = TcpListenerLink::bind(&bind).await.expect("bind");
            if let Err(e) = overnet_node::web::run_site(l, key, dir).await {
                eprintln!("host stopped: {e}");
            }
        }
        Some("new-site") => {
            let dir = args.get(2).cloned().unwrap_or_else(|| "mysite".into());
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("create dir failed: {e}");
                std::process::exit(1);
            }
            let starter = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>My .ov site</title>\n<style>body{font-family:system-ui;max-width:680px;margin:60px auto;\
padding:0 20px;background:#0d0d12;color:#eee}h1 span{color:#9d4edd}code,pre{color:#9d4edd}</style>\
</head>\n<body><h1>Hello from <span>overnet</span>!</h1>\
<p>This is your site. Edit <code>index.html</code> and publish it:</p>\n\
<pre>overnet host ./FOLDER 127.0.0.1:8080 mysite.ov</pre></body></html>\n";
            let path = std::path::Path::new(&dir).join("index.html");
            if let Err(e) = std::fs::write(&path, starter) {
                eprintln!("write failed: {e}");
                std::process::exit(1);
            }
            println!("starter site created: {}", path.display());
            println!("publish it: overnet host {dir} 127.0.0.1:8080 mysite.ov");
        }
        Some("dir") => {
            // Диагностика: дёрнуть каталог у bootstrap и показать узлы.
            let boot = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            println!("querying bootstrap {boot} ...");
            match overnet_node::bootstrap::fetch_directory(&boot).await {
                Ok(d) => {
                    println!("OK — {} node(s):", d.nodes.len());
                    for n in d.nodes {
                        let pk = &n.pubkey[..16.min(n.pubkey.len())];
                        println!("  {:8} name='{}' {}… @ {}", n.role, n.name, pk, n.address);
                    }
                }
                Err(e) => println!("FAIL: {e}"),
            }
        }
        _ => {
            let id = Identity::generate();
            println!("overnet identity: {}", id.address());
            println!("legacy usage (overnet legacy …):");
            println!("  overnet bootstrap [bind]              run directory server");
            println!("  overnet relay [bind] [bootstrap]      run onion relay");
            println!("  overnet service [bind] [bootstrap] [name] run onion service");
            println!("  overnet search [bind] [bootstrap]     run search.ov catalog");
            println!("  overnet mailbox [bind] [bootstrap]    run mail.ov mailbox");
            println!("  overnet msg-demo <mb_addr> <mb_pubkey> send+fetch test message");
            println!("  overnet gateway [bind] [bootstrap]    run local http gateway for browser");
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
