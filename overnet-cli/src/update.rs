//! `overnet update` — обновиться до последнего релиза.
//!
//! Сам ничего не скачивает: запускает тот же установщик, что и при установке
//! (`scripts/install.sh` или `install.ps1`). Так у обновления та же проверка
//! контрольной суммы, те же пути и перезапуск сервисов overnet на сервере.

use std::path::Path;
use std::process::Command;

const REPO: &str = "ospab/overnet";

/// Последний релиз на GitHub (через curl: есть и в Linux, и в Windows 10+).
fn latest_tag() -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err("could not reach GitHub to check for a new release".into());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("GitHub answer: {e}"))?;
    v["tag_name"].as_str().map(str::to_string).ok_or_else(|| "GitHub answer has no tag_name".into())
}

/// `v0.2.1` → (0, 2, 1); всё, что не разобралось, считается нулём.
fn parse(v: &str) -> (u64, u64, u64) {
    let mut it = v.trim_start_matches('v').split(['.', '-']).map(|p| p.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// `version` — нужный релиз (`--version`), иначе последний.
pub fn run(version: Option<&str>, force: bool) -> Result<(), String> {
    let current = env!("CARGO_PKG_VERSION");
    let tag = match version {
        Some(v) => if v.starts_with('v') { v.to_string() } else { format!("v{v}") },
        None => latest_tag()?,
    };
    if version.is_none() && !force && parse(&tag) <= parse(current) {
        println!("overnet {current} is up to date (latest release: {tag})");
        return Ok(());
    }
    println!("updating overnet {current} → {tag}");
    let status = if cfg!(windows) { windows(&tag)? } else { unix(&tag)? };
    if status {
        Ok(())
    } else {
        Err("the installer failed; see its output above".into())
    }
}

fn unix(tag: &str) -> Result<bool, String> {
    let uid = Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if uid.ok().as_deref() != Some("0") {
        return Err("updating needs root: run `sudo overnet update`".into());
    }
    let script = format!(
        "curl -fsSL https://raw.githubusercontent.com/{REPO}/master/scripts/install.sh | bash -s -- --version {tag} --yes"
    );
    Command::new("bash").arg("-c").arg(script).status().map(|s| s.success()).map_err(|e| format!("bash: {e}"))
}

fn windows(tag: &str) -> Result<bool, String> {
    // Запущенный .exe нельзя перезаписать, но можно переименовать: освобождаем
    // имя для нового файла. Только если это установленная копия, а не сборка
    // из исходников, — установщик кладёт overnet в своё место.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let installed = std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("Programs").join("overnet"));
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    let moved = installed.as_deref().is_some_and(|d| exe.parent() == Some(d)) && std::fs::rename(&exe, &old).is_ok();
    let script = format!(
        "$ErrorActionPreference='Stop'; & ([scriptblock]::Create((irm https://raw.githubusercontent.com/{REPO}/master/scripts/install.ps1))) -Version {tag}"
    );
    let ok = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        // Установщик останавливает все overnet.exe, кроме того, кто его вызвал.
        .env("OVERNET_UPDATER_PID", std::process::id().to_string())
        .status()
        .map(|s| s.success())
        .map_err(|e| format!("powershell: {e}"))?;
    if !ok && moved && !exe.exists() {
        // Новый файл не лёг — вернуть старый на место.
        let _ = std::fs::rename(&old, &exe);
    }
    Ok(ok)
}

/// Убрать `overnet.exe.old`, оставшийся от прошлого обновления на Windows.
pub fn cleanup() {
    if cfg!(windows) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::remove_file(exe.with_extension("exe.old"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn versions_compare_numerically() {
        assert!(parse("v0.2.10") > parse("0.2.9"));
        assert!(parse("v0.3.0") > parse("0.2.1"));
        assert_eq!(parse("v0.2.1"), parse("0.2.1"));
        assert_eq!(parse("v0.2.1-beta.2"), (0, 2, 1));
    }
}
