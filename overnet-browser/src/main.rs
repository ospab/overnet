#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use tauri::State;
use tokio::sync::Mutex;

use base64::Engine;
use overnet_core::onion::OnionKey;
use overnet_node::bootstrap::{fetch_directory, Directory, NodeInfo};

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
    // Зарегистрировать наш P2P-файлоузел: релеи к нему подключатся, и другие смогут
    // тянуть наши файлы по overnet://<my_files_address>/<file>.
    let files_info = NodeInfo {
        pubkey: hex::encode(files_key().public()),
        address: FILES_BIND.into(),
        role: "service".into(),
        name: String::new(),
    };
    tauri::async_runtime::spawn(overnet_node::bootstrap::keep_registered(
        addr.clone(),
        files_info,
        30,
    ));
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

/// Конверт сообщения: кто отправил, текст, время. Шифруется целиком на ключ
/// получателя — даёт чаты по собеседнику и историю на клиенте.
#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    from: String,
    text: String,
    ts: u64,
    /// "text" | "image" | "video" | "sticker".
    #[serde(default)]
    kind: String,
    /// base64 медиа (для не-text).
    #[serde(default)]
    data: String,
    #[serde(default)]
    content_type: String,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Твой персистентный почтовый ключ (личность мессенджера, лежит в `my-mail.key`).
fn mail_key() -> OnionKey {
    overnet_node::web::load_or_create_key("my-mail.key")
}

/// Твой адрес мессенджера (его даёшь друзьям, чтобы писали тебе).
#[tauri::command]
fn my_address() -> String {
    hex::encode(mail_key().public())
}

/// Отправить `text` адресату `to` (hex onion-pubkey). Шифруется НА КЛЮЧ получателя
/// здесь, локально (E2E), и кладётся в его ящик на mail.ov.
#[tauri::command]
async fn msg_send(to: String, text: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let dir = state
        .directory
        .lock()
        .await
        .clone()
        .ok_or("Connect to a bootstrap node first")?;
    let mailbox = dir
        .nodes
        .into_iter()
        .find(|n| n.name == "mail.ov")
        .ok_or("mail.ov is not on the network (run `overnet mailbox`)")?;
    let bytes = hex::decode(to.trim()).map_err(|_| "address: expected hex")?;
    if bytes.len() != 32 {
        return Err("address must be 64 hex chars".into());
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&bytes);
    // Конверт {from, text, ts} — чтобы у получателя были чаты и история.
    let env = Envelope {
        from: hex::encode(mail_key().public()),
        text,
        ts: now_secs(),
        kind: "text".into(),
        data: String::new(),
        content_type: String::new(),
    };
    let payload = serde_json::to_vec(&env).map_err(|e| e.to_string())?;
    overnet_node::messenger::send_message(&mailbox, pk, &payload)
        .await
        .map_err(|e| e.to_string())
}

/// Отправить медиа (фото/видео/стикер): `kind`, `content_type`, `data` (base64).
/// Шифруется целиком на ключ получателя (E2E), как и текст.
#[tauri::command]
async fn msg_send_media(
    to: String,
    kind: String,
    content_type: String,
    data: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let dir = state
        .directory
        .lock()
        .await
        .clone()
        .ok_or("Connect to a bootstrap node first")?;
    let mailbox = dir
        .nodes
        .into_iter()
        .find(|n| n.name == "mail.ov")
        .ok_or("mail.ov is not on the network (run `overnet mailbox`)")?;
    let bytes = hex::decode(to.trim()).map_err(|_| "address: expected hex")?;
    if bytes.len() != 32 {
        return Err("address must be 64 hex chars".into());
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&bytes);
    let env = Envelope {
        from: hex::encode(mail_key().public()),
        text: String::new(),
        ts: now_secs(),
        kind,
        data,
        content_type,
    };
    let payload = serde_json::to_vec(&env).map_err(|e| e.to_string())?;
    overnet_node::messenger::send_message(&mailbox, pk, &payload)
        .await
        .map_err(|e| e.to_string())
}

/// Забрать свои входящие (расшифровываются локально твоим ключом).
#[tauri::command]
async fn msg_fetch(state: State<'_, Arc<AppState>>) -> Result<Vec<Envelope>, String> {
    let dir = state
        .directory
        .lock()
        .await
        .clone()
        .ok_or("Connect to a bootstrap node first")?;
    let mailbox = dir
        .nodes
        .into_iter()
        .find(|n| n.name == "mail.ov")
        .ok_or("mail.ov is not on the network")?;
    let key = mail_key();
    let raw = overnet_node::messenger::fetch_inbox(&mailbox, &key)
        .await
        .map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .filter_map(|b| serde_json::from_slice::<Envelope>(&b).ok())
        .collect())
}

const FILES_BIND: &str = "0.0.0.0:4090";
const SHARED_DIR: &str = "overnet-shared";

/// Персистентный ключ твоего файлоузла (личность для P2P-файлов).
fn files_key() -> OnionKey {
    overnet_node::web::load_or_create_key("my-files.key")
}

/// Адрес твоего файлоузла — часть ссылок `overnet://<addr>/<file>`.
#[tauri::command]
fn my_files_address() -> String {
    hex::encode(files_key().public())
}

/// Поделиться файлом: сохраняем его в локальную shared-папку (файл остаётся у тебя
/// на машине) и возвращаем ссылку. Другие тянут его P2P напрямую с твоего узла.
#[tauri::command]
fn share_file(name: String, data: String) -> Result<String, String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("bad file name".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|_| "bad base64")?;
    std::fs::create_dir_all(SHARED_DIR).ok();
    std::fs::write(format!("{SHARED_DIR}/{name}"), &bytes).map_err(|e| e.to_string())?;
    Ok(format!("overnet://{}/{}", hex::encode(files_key().public()), name))
}

/// Список файлов, которыми ты делишься.
#[tauri::command]
fn list_files() -> Vec<String> {
    std::fs::read_dir(SHARED_DIR)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(serde::Serialize)]
struct DownloadResult {
    name: String,
    /// base64 содержимого файла.
    data: String,
}

/// Скачать файл по ссылке `overnet://<addr>/<file>` напрямую с узла-владельца (onion).
#[tauri::command]
async fn download_url(
    url: String,
    state: State<'_, Arc<AppState>>,
) -> Result<DownloadResult, String> {
    let after = url.trim().trim_start_matches("overnet://");
    let (host, name) = after
        .split_once('/')
        .ok_or("bad link, expected overnet://<addr>/<file>")?;
    if name.is_empty() {
        return Err("no file name in link".into());
    }
    let dir = state
        .directory
        .lock()
        .await
        .clone()
        .ok_or("Connect to a bootstrap node first")?;
    let svc = dir
        .nodes
        .into_iter()
        .find(|n| n.name == host || n.pubkey == host)
        .ok_or("file host not found in directory")?;
    let resp = overnet_node::web::request(&svc, "GET", &format!("/{name}"), String::new())
        .await
        .map_err(|e| e.to_string())?;
    if resp.status != 200 {
        return Err(format!("download failed: {}", resp.status));
    }
    Ok(DownloadResult { name: name.to_string(), data: resp.body })
}

/// Перечитать каталог у уже подключённого bootstrap (фоновый опрос из браузера).
/// Лёгкая версия connect: не перерегистрирует файлоузел.
#[tauri::command]
async fn refresh_directory(state: State<'_, Arc<AppState>>) -> Result<Directory, String> {
    let addr = state
        .bootstrap_addr
        .lock()
        .await
        .clone()
        .ok_or("not connected")?;
    let token = state.bootstrap_token.lock().await.clone().unwrap_or_default();
    let dir = overnet_node::bootstrap::fetch_directory_with_token(&addr, token)
        .await
        .map_err(|e| e.to_string())?;
    *state.directory.lock().await = Some(dir.clone());
    Ok(dir)
}

fn main() {
    let app_state = Arc::new(AppState {
        bootstrap_addr: Mutex::new(None),
        bootstrap_token: Mutex::new(None),
        directory: Mutex::new(None),
    });

    tauri::Builder::default()
        .manage(app_state)
        .setup(|_app| {
            // Поднять свой P2P-файлоузел: отдаёт файлы из overnet-shared по onion.
            tauri::async_runtime::spawn(async {
                match overnet_link_tcp::TcpListenerLink::bind(FILES_BIND).await {
                    Ok(l) => {
                        let _ = overnet_node::web::run_fileserver(l, files_key(), SHARED_DIR.into()).await;
                    }
                    Err(e) => eprintln!("fileserver bind failed: {e}"),
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect_bootstrap,
            navigate,
            my_address,
            msg_send,
            msg_send_media,
            msg_fetch,
            my_files_address,
            share_file,
            list_files,
            download_url,
            refresh_directory
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
