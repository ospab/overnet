//! Построить цепь через каждую пару и тройку стартовых релеев и показать, на
//! каком хопе она ломается. Диагностика сети: `cargo run --example probe_paths`.
use overnet_core::onion::OnionKey;
use overnet_node::net::dir::RelayDesc;
use overnet_node::net::exit::ExitPolicy;
use overnet_node::net::names::SEED_RELAYS;
use overnet_node::net::Node;

#[tokio::main]
async fn main() {
    let relays: Vec<RelayDesc> = SEED_RELAYS.iter().map(|r| RelayDesc::parse(r).unwrap()).collect();
    let n = relays.len();
    let mut paths: Vec<Vec<usize>> = Vec::new();
    for a in 0..n {
        paths.push(vec![a]);
        for b in 0..n {
            if b != a {
                paths.push(vec![a, b]);
                for c in 0..n {
                    if c != a && c != b {
                        paths.push(vec![a, b, c]);
                    }
                }
            }
        }
    }
    if std::env::args().any(|a| a == "--shared") {
        // Один узел, все цепи разом: как в живом шлюзе.
        let node = Node::new(OnionKey::generate(), ExitPolicy::Off);
        let mut tasks = Vec::new();
        for p in paths.iter().chain(paths.iter()) {
            let path: Vec<RelayDesc> = p.iter().map(|&i| relays[i].clone()).collect();
            let node = node.clone();
            tasks.push(tokio::spawn(async move {
                let names: Vec<String> = path.iter().map(|r| r.address.clone()).collect();
                match node.build_circuit(&path).await {
                    Ok(_c) => format!("ok    {}", names.join(" -> ")),
                    Err(e) => format!("FAIL  {}: {e}", names.join(" -> ")),
                }
            }));
        }
        for t in tasks {
            println!("{}", t.await.unwrap());
        }
        return;
    }
    for p in paths {
        let path: Vec<RelayDesc> = p.iter().map(|&i| relays[i].clone()).collect();
        let names: Vec<&str> = path.iter().map(|r| r.address.as_str()).collect();
        // Свежий узел на каждую цепь: без общих каналов.
        let node = Node::new(OnionKey::generate(), ExitPolicy::Off);
        match node.build_circuit(&path).await {
            Ok(c) => { println!("ok    {}", names.join(" -> ")); c.destroy().await; }
            Err(e) => println!("FAIL  {}: {e}", names.join(" -> ")),
        }
    }
}
