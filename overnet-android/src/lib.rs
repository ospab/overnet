//! The overnet gateway inside the Android app, over JNI
//! (`com.ospab.overnet.Gateway` on the Kotlin side).
//!
//! The same gateway as `overnet gateway --browser`: circuits through the seed
//! relays, `.ov` names, `browser.ov` served locally, and regular sites blocked
//! (the app sends them to the `browser.ov/go` page). GeckoView talks to it over
//! SOCKS5 on a loopback port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use jni::objects::JClass;
use jni::sys::{jint, jstring};
use jni::JNIEnv;
use tokio::runtime::Runtime;

use overnet_cli::browser_ui::Ui;
use overnet_node::net::dir::RelayDesc;
use overnet_node::net::gateway::Clearnet;
use overnet_node::net::names::SEED_RELAYS;

struct Running {
    port: u16,
    ui: Arc<Ui>,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static ERROR: Mutex<String> = Mutex::new(String::new());

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("overnet")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

fn start() -> Result<u16, String> {
    let mut running = RUNNING.lock().unwrap();
    if let Some(r) = running.as_ref() {
        return Ok(r.port);
    }
    let relays = SEED_RELAYS
        .iter()
        .map(|r| RelayDesc::parse(r).ok_or_else(|| format!("bad seed relay {r}")))
        .collect::<Result<Vec<_>, _>>()?;
    // block_on only binds the ports; the gateway keeps running on the
    // runtime's worker threads afterwards.
    let (port, ui) = runtime().block_on(async {
        let (gw, ui) = overnet_cli::start_gateway_with_ui(relays, &HashMap::new(), Clearnet::Block, true).await?;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("gateway: {e}"))?;
        let port = l.local_addr().map_err(|e| e.to_string())?.port();
        tokio::spawn(gw.serve(l));
        Ok::<_, String>((port, ui))
    })?;
    *running = Some(Running { port, ui });
    Ok(port)
}

fn string(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

/// Start the gateway (once per process). Returns its SOCKS5 port on
/// 127.0.0.1, or -1 — then `status()` has the error.
#[no_mangle]
pub extern "system" fn Java_com_ospab_overnet_Gateway_start(_env: JNIEnv, _class: JClass) -> jint {
    match start() {
        Ok(port) => port as jint,
        Err(e) => {
            *ERROR.lock().unwrap() = e;
            -1
        }
    }
}

/// `{"running", "relays", "exits", "token", "error"}` as JSON. The token
/// guards `browser.ov/open`, which the app handles itself.
#[no_mangle]
pub extern "system" fn Java_com_ospab_overnet_Gateway_status(mut env: JNIEnv, _class: JClass) -> jstring {
    let running = RUNNING.lock().unwrap();
    let json = match running.as_ref() {
        Some(r) => {
            let (relays, exits) = r.ui.client.directory_size();
            serde_json::json!({ "running": true, "relays": relays, "exits": exits, "token": r.ui.token, "error": "" })
        }
        None => serde_json::json!({
            "running": false, "relays": 0, "exits": 0, "token": "", "error": *ERROR.lock().unwrap(),
        }),
    };
    string(&mut env, &json.to_string())
}

/// New circuits for everything opened from now on.
#[no_mangle]
pub extern "system" fn Java_com_ospab_overnet_Gateway_newCircuits(_env: JNIEnv, _class: JClass) {
    let client = RUNNING.lock().unwrap().as_ref().map(|r| r.ui.client.clone());
    if let Some(c) = client {
        runtime().spawn(async move { c.new_identity().await });
    }
}
