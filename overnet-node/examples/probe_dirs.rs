//! Спросить каталог у каждого стартового релея и показать, что он отдаёт.
//! Диагностика сети: `cargo run --example probe_dirs`.
use overnet_core::onion::OnionKey;
use overnet_node::net::dir::{fetch_via, RelayDesc};
use overnet_node::net::exit::ExitPolicy;
use overnet_node::net::names::SEED_RELAYS;
use overnet_node::net::Node;

#[tokio::main]
async fn main() {
    for r in SEED_RELAYS {
        let via = RelayDesc::parse(r).unwrap();
        let node = Node::new(OnionKey::generate(), ExitPolicy::Off);
        println!("via {}:", via.address);
        match fetch_via(&node, &via).await {
            Ok(list) => {
                for d in list {
                    println!("    {}…@{} exit={}", &d.pubkey[..16], d.address, d.exit);
                }
            }
            Err(e) => println!("    FAIL: {e}"),
        }
    }
}
