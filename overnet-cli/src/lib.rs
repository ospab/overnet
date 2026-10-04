//! The overnet gateway with its `browser.ov` pages, as a library: the `overnet`
//! binary and the Android app (`overnet-android`) start it the same way.

pub mod browser_ui;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use overnet_core::onion::OnionKey;
use overnet_node::net::client::Client;
use overnet_node::net::dir::RelayDesc;
use overnet_node::net::exit::ExitPolicy;
use overnet_node::net::gateway::{Clearnet, Gateway};
use overnet_node::net::names::{Names, RESERVED};
use overnet_node::net::Node;

/// Serve `app` over HTTP on `listen` (loopback); returns the bound address.
pub async fn serve_http(app: axum::Router, listen: &str) -> std::io::Result<String> {
    let l = tokio::net::TcpListener::bind(listen).await?;
    let addr = l.local_addr()?.to_string();
    tokio::spawn(async move { axum::serve(l, app).await });
    Ok(addr)
}

/// A client gateway: circuits through `relays`, `.ov` names (built-in ones,
/// overridden by `reserved`), and `browser.ov` served locally. The directory
/// loads in the background, so the gateway can listen right away.
///
/// `open_external`: the `browser.ov` pages may open a regular site in the
/// system's browser (a gateway started for a browser, not one on a server).
pub async fn start_gateway(
    relays: Vec<RelayDesc>,
    reserved: &HashMap<String, String>,
    clearnet: Clearnet,
    open_external: bool,
) -> Result<Arc<Gateway>, String> {
    Ok(start_gateway_with_ui(relays, reserved, clearnet, open_external).await?.0)
}

/// [`start_gateway`], also returning the `browser.ov` state: the Android app
/// reads its token to handle "Open in my regular browser" itself.
pub async fn start_gateway_with_ui(
    relays: Vec<RelayDesc>,
    reserved: &HashMap<String, String>,
    clearnet: Clearnet,
    open_external: bool,
) -> Result<(Arc<Gateway>, Arc<browser_ui::Ui>), String> {
    let c = Client::new(Node::new(OnionKey::generate(), ExitPolicy::Off), relays);
    let names = Names::new(c.clone(), reserved).map_err(|e| e.to_string())?;
    {
        let c = c.clone();
        tokio::spawn(async move {
            match c.refresh_directory().await {
                Ok(n) => println!("directory: {n} relays"),
                Err(e) => eprintln!("directory not available yet ({e}); will retry on the first request"),
            }
        });
    }
    c.spawn_directory_refresh(Duration::from_secs(600));
    let missing: Vec<&str> = RESERVED.iter().copied().filter(|n| !names.reserved().contains_key(*n)).collect();
    if !missing.is_empty() {
        eprintln!("addresses not set: {} (the \"reserved\" section of the config)", missing.join(", "));
    }
    let ui = Arc::new(browser_ui::Ui::new(c.clone(), clearnet, open_external));
    let ui_addr = serve_http(browser_ui::router(ui.clone()), "127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let local = [("browser.ov".to_string(), ui_addr.parse().expect("loopback address"))].into_iter().collect();
    Ok((Arc::new(Gateway { client: c, names, clearnet, local }), ui))
}
