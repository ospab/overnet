//! Посмотреть служебные сайты без сети: все четыре на localhost.
//!
//! `cargo run -p overnet-sites --example preview` → http://127.0.0.1:8701 … 8704.
//! Ссылки между сайтами ведут на настоящие адреса .ov и здесь не откроются.

use std::time::Duration;

use overnet_sites::{files, mail, registrar, search};

#[tokio::main]
async fn main() {
    let data = std::env::temp_dir().join("overnet-sites-preview");
    std::fs::create_dir_all(&data).unwrap();

    let s = search::Search::new();
    s.set_index(vec![
        search::Entry {
            name: "openwire.ov".into(),
            title: "Open Wire — independent news, updated daily".into(),
            description: "Reporting from the region by a team of eleven journalists.".into(),
            ..Default::default()
        },
        search::Entry { name: "weather-north.ov".into(), offline: true, ..Default::default() },
        search::Entry {
            name: "shop.ov".into(),
            title: "Tea and coffee shop".into(),
            text: "Fresh oolong and green tea, posted within the city. News about new harvests.".into(),
            ..Default::default()
        },
    ])
    .await;

    let f = files::Files::open(&data, 64 << 20, Duration::from_secs(7 * 86400)).unwrap();
    let sites = [
        ("name.ov", 8701, registrar::router(registrar::Registrar::open(&data))),
        ("search.ov", 8702, search::router(s)),
        ("files.ov", 8703, files::router(f)),
        ("mail.ov", 8704, mail::router(mail::Mail::open(&data))),
    ];
    for (name, port, app) in sites {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        println!("{name}: http://127.0.0.1:{port}/");
        tokio::spawn(async move { axum::serve(l, app).await });
    }
    tokio::signal::ctrl_c().await.unwrap();
}
