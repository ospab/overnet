//! `overnet browser` — Mullvad Browser (Firefox ESR с защитой от отпечатков от
//! Tor Project, без Tor) с отдельным профилем overnet.
//!
//! Сначала решаем, нужен ли локальный шлюз. Если `name.ov` резолвится в
//! фиктивный адрес ostp (198.18.0.0/15), значит работает VPN ostp (TUN) и его
//! сервер сам выводит `.ov` в overnet — браузеру прокси не нужен. Иначе
//! поднимаем SOCKS5-шлюз (или берём уже запущенный) и прописываем его в профиль.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Как выбирать режим.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GatewayMode {
    /// Решить по детекту ostp.
    Auto,
    /// Всегда через свой шлюз (даже под ostp: тогда .ov не видит сервер ostp).
    Always,
    /// Никогда: только если .ov уже обеспечивает ostp.
    Never,
}

impl GatewayMode {
    pub fn parse(s: &str) -> Result<GatewayMode, String> {
        match s {
            "" | "auto" => Ok(GatewayMode::Auto),
            "always" => Ok(GatewayMode::Always),
            "never" => Ok(GatewayMode::Never),
            o => Err(format!("browser gateway: '{o}' is not auto, always or never")),
        }
    }
}

fn is_ostp_fake(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18)
}

/// Обслуживает ли `.ov` VPN ostp: системный DNS даёт для `name.ov` фиктивный адрес.
pub async fn ostp_serves_ov() -> bool {
    match tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host("name.ov:80")).await {
        Ok(Ok(mut addrs)) => addrs.any(|a| is_ostp_fake(a.ip())),
        _ => false,
    }
}

/// Слушает ли кто-то уже адрес шлюза.
pub async fn gateway_running(addr: &str) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(500), tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Найти браузер: явный путь, Mullvad Browser, затем Firefox (с предупреждением).
pub fn find_browser(configured: &str) -> Result<(PathBuf, bool), String> {
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return if p.exists() { Ok((p, true)) } else { Err(format!("browser.path: {configured} not found")) };
    }
    if let Some(p) = std::env::var_os("OVERNET_BROWSER").map(PathBuf::from).filter(|p| p.exists()) {
        return Ok((p, true));
    }
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut mullvad: Vec<PathBuf> = Vec::new();
    let mut firefox: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for base in [env("LOCALAPPDATA"), env("ProgramFiles"), env("ProgramFiles(x86)")].into_iter().flatten() {
            mullvad.push(base.join("Mullvad/MullvadBrowser/Release/mullvadbrowser.exe"));
            mullvad.push(base.join("Mullvad Browser/Browser/mullvadbrowser.exe"));
        }
        if let Some(h) = env("USERPROFILE") {
            mullvad.push(h.join("Desktop/Mullvad Browser/Browser/mullvadbrowser.exe"));
        }
        for base in [env("ProgramFiles"), env("ProgramFiles(x86)")].into_iter().flatten() {
            for dir in ["Mozilla Firefox", "Firefox Developer Edition", "Firefox Nightly"] {
                firefox.push(base.join(dir).join("firefox.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        mullvad.push("/Applications/Mullvad Browser.app/Contents/MacOS/mullvadbrowser".into());
        for app in ["Firefox", "Firefox Developer Edition", "Firefox Nightly"] {
            firefox.push(format!("/Applications/{app}.app/Contents/MacOS/firefox").into());
        }
    } else {
        mullvad.extend(which("mullvad-browser"));
        if let Some(h) = env("HOME") {
            mullvad.push(h.join("mullvad-browser/Browser/start-mullvad-browser"));
            mullvad.push(h.join(".local/share/flatpak/exports/bin/net.mullvad.MullvadBrowser"));
        }
        mullvad.push("/var/lib/flatpak/exports/bin/net.mullvad.MullvadBrowser".into());
        for name in ["firefox", "firefox-developer-edition", "firefox-nightly"] {
            firefox.extend(which(name));
        }
    }
    if let Some(p) = mullvad.into_iter().find(|p| p.exists()) {
        return Ok((p, true));
    }
    if let Some(p) = firefox.into_iter().find(|p| p.exists()) {
        return Ok((p, false));
    }
    Err("Mullvad Browser not found. Install it from https://mullvad.net/browser \
         or set its path in config.json: \"browser\": {\"path\": \"…\"}"
        .into())
}

/// Настройки профиля. `proxy` — адрес SOCKS5-шлюза, `None` — без прокси (ostp).
pub fn user_js(proxy: Option<&str>) -> String {
    let mut p: Vec<(String, String)> = Vec::new();
    let mut set = |k: &str, v: &str| p.push((k.into(), v.into()));
    match proxy.and_then(|a| a.rsplit_once(':')) {
        Some((host, port)) => {
            set("network.proxy.type", "1");
            set("network.proxy.socks", &format!("\"{host}\""));
            set("network.proxy.socks_port", port);
            set("network.proxy.socks_version", "5");
            // Имена резолвит шлюз: иначе .ov уйдёт в системный DNS.
            set("network.proxy.socks_remote_dns", "true");
            set("network.proxy.no_proxies_on", "\"\"");
            set("network.proxy.allow_hijacking_localhost", "true");
        }
        None => set("network.proxy.type", "0"),
    }
    // Свой DoH браузера (у Mullvad он включён) обошёл бы и шлюз, и DNS ostp.
    set("network.trr.mode", "5");
    // «search.ov» в адресной строке — сайт, а не поисковый запрос.
    set("browser.fixup.domainsuffixwhitelist.ov", "true");
    set("browser.startup.homepage", "\"http://search.ov/\"");
    set("browser.startup.page", "1");
    // Сайты .ov — http (шифрование даёт сама сеть). HTTPS-only сломал бы их,
    // поэтому HTTPS-first: обычные сайты всё равно переводятся на https.
    set("dom.security.https_only_mode", "false");
    set("dom.security.https_only_mode_pbm", "false");
    set("dom.security.https_first", "true");
    set("dom.security.https_first_pbm", "true");
    // Шифрование в браузере (WebCrypto) — только в «безопасном контексте». Нужно
    // одной почте; остальные сайты в списке давали бы предупреждение Firefox
    // «форма уходит с безопасной страницы на небезопасную» при каждом поиске.
    set("dom.securecontext.allowlist", "\"mail.ov\"");
    set("toolkit.legacyUserProfileCustomizations.stylesheets", "true");
    set("browser.search.suggest.enabled", "false");
    let mut out = String::from("// overnet: this file is rewritten every time `overnet browser` starts.\n");
    for (k, v) in p {
        out += &format!("user_pref(\"{k}\", {v});\n");
    }
    out
}

/// Оформление окна браузера в цветах overnet.
pub const USER_CHROME: &str = r#"/* overnet */
:root{
  --toolbar-bgcolor:#15151d !important; --toolbar-color:#ececf1 !important;
  --lwt-accent-color:#0b0b10 !important; --lwt-text-color:#ececf1 !important;
  --tab-selected-bgcolor:#1c1c26 !important; --tab-selected-textcolor:#ececf1 !important;
  --toolbar-field-background-color:#1c1c26 !important; --toolbar-field-color:#ececf1 !important;
  --toolbar-field-focus-background-color:#1c1c26 !important; --toolbar-field-focus-color:#ececf1 !important;
  --toolbar-field-focus-border-color:#9d6bff !important; --focus-outline-color:#9d6bff !important;
  --button-primary-bgcolor:#9d6bff !important; --toolbarbutton-icon-fill:#ececf1 !important;
  --arrowpanel-background:#15151d !important; --arrowpanel-color:#ececf1 !important;
}
#navigator-toolbox,#TabsToolbar,#titlebar{background:#0b0b10 !important}
#nav-bar{background:#15151d !important;border-bottom:1px solid rgba(255,255,255,.08) !important}
.tab-background[selected]{background:#1c1c26 !important;border-top:2px solid #9d6bff !important}
#urlbar-background{border-radius:18px !important;border:1px solid rgba(255,255,255,.08) !important}
#urlbar[focused] #urlbar-background{border-color:#9d6bff !important}
"#;

/// Подготовить профиль в `dir`.
pub fn write_profile(dir: &Path, proxy: Option<&str>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("chrome"))?;
    std::fs::write(dir.join("user.js"), user_js(proxy))?;
    std::fs::write(dir.join("chrome/userChrome.css"), USER_CHROME)
}

/// Запустить браузер и дождаться, пока его закроют.
///
/// Запущенный процесс — плохой признак: на новом профиле (и после обновления)
/// Firefox перезапускает сам себя, первый процесс выходит, а окно остаётся. Шлюз
/// при этом закрываться не должен, поэтому ждём, пока браузер держит профиль.
pub async fn launch(exe: &Path, profile: &Path) -> std::io::Result<std::process::ExitStatus> {
    let status = tokio::process::Command::new(exe)
        .arg("--profile")
        .arg(profile)
        .arg("--no-remote")
        .arg("http://search.ov/")
        .status()
        .await?;
    // Перезапущенному браузеру нужно время, чтобы снова занять профиль.
    tokio::time::sleep(Duration::from_secs(3)).await;
    while profile_in_use(profile) {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(status)
}

/// Занят ли профиль работающим браузером.
fn profile_in_use(profile: &Path) -> bool {
    if cfg!(windows) {
        // Firefox держит parent.lock открытым без права удаления: удалось
        // удалить — браузера нет (файл он создаст заново при запуске).
        let lock = profile.join("parent.lock");
        lock.exists() && std::fs::remove_file(&lock).is_err()
    } else {
        // Ссылка `lock` есть, пока браузер работает; при выходе он её убирает.
        std::fs::symlink_metadata(profile.join("lock")).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_profile_sends_names_to_the_gateway() {
        let js = user_js(Some("127.0.0.1:9150"));
        assert!(js.contains("user_pref(\"network.proxy.socks\", \"127.0.0.1\");"));
        assert!(js.contains("user_pref(\"network.proxy.socks_port\", 9150);"));
        assert!(js.contains("user_pref(\"network.proxy.socks_remote_dns\", true);"));
        assert!(js.contains("user_pref(\"network.trr.mode\", 5);"));
        assert!(js.contains("mail.ov"));
    }

    #[test]
    fn ostp_profile_has_no_proxy() {
        let js = user_js(None);
        assert!(js.contains("user_pref(\"network.proxy.type\", 0);"));
        assert!(!js.contains("socks_port"));
    }

    #[test]
    fn fake_range() {
        assert!(is_ostp_fake("198.18.0.5".parse().unwrap()));
        assert!(is_ostp_fake("198.19.255.1".parse().unwrap()));
        assert!(!is_ostp_fake("198.20.0.1".parse().unwrap()));
    }
}
