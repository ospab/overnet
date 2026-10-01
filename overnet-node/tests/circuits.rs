//! Сквозные проверки v0.2 на настоящих TCP-узлах в одном процессе:
//! каталог через релей, выход, сервис .ov через точку входа, SOCKS5-шлюз.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use overnet_core::onion::OnionKey;
use overnet_core::ovaddr::ServiceKey;
use overnet_link_tcp::TcpListenerLink;
use overnet_node::net::client::Client;
use overnet_node::net::dir::RelayDesc;
use overnet_node::net::exit::ExitPolicy;
use overnet_node::net::gateway::{Clearnet, Gateway};
use overnet_node::net::names::Names;
use overnet_node::net::service::Service;
use overnet_node::net::Node;

/// Сеть из `n` релеев; последний — выход (с доступом к localhost для теста).
async fn network(n: usize) -> (Vec<Arc<Node>>, Vec<RelayDesc>) {
    let mut nodes = Vec::new();
    let mut descs = Vec::new();
    for i in 0..n {
        let exit = if i == n - 1 { ExitPolicy::Direct { allow_private: true } } else { ExitPolicy::Off };
        let node = Node::new(OnionKey::generate(), exit.clone());
        let l = TcpListenerLink::bind("127.0.0.1:0").await.unwrap();
        descs.push(RelayDesc {
            pubkey: hex::encode(node.pubkey()),
            address: l.local_addr().unwrap().to_string(),
            exit: exit.enabled(),
        });
        let nd = node.clone();
        tokio::spawn(async move { nd.serve(l).await });
        nodes.push(node);
    }
    for node in &nodes {
        node.set_directory(descs.clone());
    }
    (nodes, descs)
}

async fn echo_server() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

fn client(bootstrap: &RelayDesc) -> Arc<Client> {
    Client::new(Node::new(OnionKey::generate(), ExitPolicy::Off), vec![bootstrap.clone()])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_comes_from_a_relay_not_from_bootstrap() {
    let (_nodes, descs) = network(4).await;
    let c = client(&descs[0]);
    assert_eq!(c.refresh_directory().await.unwrap(), 4);
    assert_eq!(c.node.directory(), descs);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exit_stream_carries_a_megabyte_both_ways() {
    let (_nodes, descs) = network(4).await;
    let echo = echo_server().await;
    let c = client(&descs[0]);
    let mut s = c.connect_exit(&echo).await.unwrap();

    // Больше окна потока: без SENDME застряло бы.
    let data: Vec<u8> = (0..1_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let writer = {
        let data = data.clone();
        let (r, w) = s.split();
        let t = tokio::spawn(async move { w.write(&data).await.unwrap(); w });
        s_reader_into(r, t)
    };
    let (got, _w) = tokio::time::timeout(Duration::from_secs(60), writer).await.unwrap();
    assert_eq!(got.len(), data.len());
    assert_eq!(got, data);
    // Цепь живёт: второй поток по ней же.
    s = c.connect_exit(&echo).await.unwrap();
    s.write(b"again").await.unwrap();
    assert_eq!(s.read().await.unwrap(), b"again");
}

/// Читать из потока, пока не придёт столько байт, сколько отправит писатель.
async fn s_reader_into(
    mut r: overnet_node::net::stream::StreamReader,
    w: tokio::task::JoinHandle<overnet_node::net::stream::StreamWriter>,
) -> (Vec<u8>, overnet_node::net::stream::StreamWriter) {
    let mut got = Vec::new();
    while got.len() < 1_000_000 {
        match r.read().await {
            Some(d) => got.extend_from_slice(&d),
            None => break,
        }
    }
    (got, w.await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_exit_relay_refuses_to_exit() {
    let (nodes, descs) = network(3).await;
    let echo = echo_server().await;
    let c = client(&descs[0]);
    c.refresh_directory().await.unwrap();
    // Цепь, оканчивающаяся на не-выходе.
    let circ = c.node.build_circuit(&descs[..2]).await.unwrap();
    let r = circ
        .open_stream(1, overnet_core::cell::relay::BEGIN, &echo)
        .await;
    assert!(r.is_err());
    assert!(r.err().unwrap().to_string().contains("exit disabled"));
    drop(nodes);
}

async fn start_service(descs: &[RelayDesc], port80: &str) -> (Arc<Service>, String) {
    let key = ServiceKey::generate();
    let addr = key.id().to_address();
    let sc = client(&descs[1]);
    sc.refresh_directory().await.unwrap();
    let svc = Service::new(sc, key, HashMap::from([(80u16, port80.to_string())]));
    assert_eq!(svc.ensure_intros().await, 2, "both intro points must be up");
    (svc, addr)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ov_service_through_intro_point_end_to_end() {
    let (_nodes, descs) = network(5).await;
    let echo = echo_server().await;
    let (svc, addr) = start_service(&descs, &echo).await;

    let c = client(&descs[0]);
    let id = overnet_core::ovaddr::ServiceId::from_address(&addr).unwrap();
    let mut s = c.connect_service(id, 80).await.unwrap();
    s.write(b"hello .ov").await.unwrap();
    assert_eq!(s.read().await.unwrap(), b"hello .ov");

    // Второй поток идёт по той же склеенной цепи.
    let mut s2 = c.connect_service(id, 80).await.unwrap();
    s2.write(b"second").await.unwrap();
    assert_eq!(s2.read().await.unwrap(), b"second");

    // Несуществующий порт — отказ, а не зависание.
    assert!(c.connect_service(id, 81).await.is_err());
    drop(svc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_service_fails_cleanly() {
    let (_nodes, descs) = network(4).await;
    let c = client(&descs[0]);
    let id = ServiceKey::generate().id();
    let r = tokio::time::timeout(Duration::from_secs(40), c.connect_service(id, 80)).await.unwrap();
    assert!(r.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socks_gateway_opens_reserved_name() {
    let (_nodes, descs) = network(5).await;
    let echo = echo_server().await;
    let (_svc, addr) = start_service(&descs, &echo).await;

    let c = client(&descs[0]);
    let names = Names::new(c.clone(), &HashMap::from([("search.ov".to_string(), addr)])).unwrap();
    let gw = Arc::new(Gateway { client: c, names, clearnet: Clearnet::Block });
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_addr = l.local_addr().unwrap();
    tokio::spawn(gw.serve(l));

    let mut s = TcpStream::connect(gw_addr).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut g = [0u8; 2];
    s.read_exact(&mut g).await.unwrap();
    assert_eq!(g, [5, 0]);
    let host = b"www.search.ov";
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host);
    req.extend_from_slice(&80u16.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 0, "gateway must connect search.ov");
    s.write_all(b"via socks").await.unwrap();
    let mut buf = [0u8; 9];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"via socks");

    // Clearnet заблокирован настройкой.
    let mut s = TcpStream::connect(gw_addr).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    s.read_exact(&mut g).await.unwrap();
    s.write_all(&[5, 1, 0, 1, 1, 1, 1, 1, 0, 80]).await.unwrap();
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 2);
}
