use std::time::Instant;
use std::sync::Arc;
use tokio::sync::mpsc;
use serde::{Serialize, Deserialize};

use overnet_core::session::TransportKey;
use overnet_core::Identity;
use overnet_core::onion::{self, OnionKey};
use overnet_node::router::Router;
use overnet_node::bootstrap::{BootstrapServer, NodeInfo, register_node};
use overnet_link_tcp::{TcpLink, TcpListenerLink};
use overnet_core::Link;

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

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
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
            
            let info = NodeInfo {
                pubkey: pubkey_hex,
                address: bind.clone(),
                role: "relay".to_string(),
                name: String::new(),
            };
            
            // Register with bootstrap in background (with retries in a real app, here just once)
            tokio::spawn(async move {
                println!("Registering with bootstrap server {}...", bootstrap_addr);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await; // wait for listener
                match register_node(&bootstrap_addr, info).await {
                    Ok(_) => println!("Successfully registered with bootstrap server!"),
                    Err(e) => eprintln!("Failed to register with bootstrap server: {:?}", e),
                }
            });
            
            let (tx, mut rx) = mpsc::unbounded_channel();
            let router = Arc::new(Router::new(key, tx));
            
            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    println!("[DELIVERED] {}", String::from_utf8_lossy(&msg));
                }
            });

            let listener = TcpListenerLink::bind(&bind).await.expect("bind");
            if let Err(e) = router.serve(listener).await {
                eprintln!("relay stopped: {e}");
            }
        }
        Some("service") => {
            let bind = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:4040".into());
            let bootstrap_addr = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
            let name = args.get(4).cloned().unwrap_or_default();
            
            let key = OnionKey::generate();
            let pubkey_hex = hex(&key.public());
            println!("overnet service listening on {bind}");
            if !name.is_empty() {
                println!("service name: {}", name);
            }
            println!("onion pubkey: {}", pubkey_hex);
            
            let info = NodeInfo {
                pubkey: pubkey_hex,
                address: bind.clone(),
                role: "service".to_string(),
                name,
            };
            
            tokio::spawn(async move {
                println!("Registering with bootstrap server {}...", bootstrap_addr);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await; // wait for listener
                match register_node(&bootstrap_addr, info).await {
                    Ok(_) => println!("Successfully registered with bootstrap server!"),
                    Err(e) => eprintln!("Failed to register with bootstrap server: {:?}", e),
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
                            "[OK]   {addr}  эхо={:?}  {} мс",
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
        _ => {
            let id = Identity::generate();
            println!("overnet identity: {}", id.address());
            println!("usage:");
            println!("  overnet bootstrap [bind]              run directory server");
            println!("  overnet relay [bind] [bootstrap]      run onion relay");
            println!("  overnet service [bind] [bootstrap] [name] run onion service");
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
