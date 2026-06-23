#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use tauri::State;
use tokio::sync::Mutex;

use overnet_node::bootstrap::{fetch_directory, Directory};

/// Состояние приложения: запомненный адрес bootstrap и последний каталог сети.
struct AppState {
    bootstrap_addr: Mutex<Option<String>>,
    bootstrap_token: Mutex<Option<String>>,
    directory: Mutex<Option<Directory>>,
}

/// Подключиться к bootstrap-ноде и загрузить каталог сети.
#[tauri::command]
async fn connect_bootstrap(
    addr: String,
    token: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Directory, String> {
    let dir = overnet_node::bootstrap::fetch_directory_with_token(&addr, token.clone())
        .await
        .map_err(|e| format!("Не удалось получить каталог: {e}"))?;
    *state.directory.lock().await = Some(dir.clone());
    *state.bootstrap_addr.lock().await = Some(addr);
    *state.bootstrap_token.lock().await = Some(token);
    Ok(dir)
}

/// Открыть `overnet://<host>/<path>`.
///
/// MVP: **direct (1 хоп)** — авто-выбор первого сервиса из каталога (как рабочий
/// curl/gateway). Мультихоп guard→relay→target — следующий шаг (приватность),
/// когда в сети будут relay-ноды.
#[tauri::command]
async fn navigate(url: String, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // overnet://host/path -> вытащить host и path
    let after = url.splitn(2, "://").nth(1).unwrap_or(&url);
    let (host, path) = match after.find('/') {
        Some(i) => (after[..i].to_string(), after[i..].to_string()),
        None => (after.to_string(), "/".to_string()),
    };

    let bootstrap = state
        .bootstrap_addr
        .lock()
        .await
        .clone()
        .ok_or_else(|| "Сначала подключись к Bootstrap-ноде".to_string())?;

    let token = state
        .bootstrap_token
        .lock()
        .await
        .clone()
        .unwrap_or_default();

    overnet_node::web::fetch_service(&bootstrap, token, &host, &path)
        .await
        .map(|r| r.body)
        .map_err(|e| e.to_string())
}

fn main() {
    let app_state = Arc::new(AppState {
        bootstrap_addr: Mutex::new(None),
        bootstrap_token: Mutex::new(None),
        directory: Mutex::new(None),
    });

    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![connect_bootstrap, navigate])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
