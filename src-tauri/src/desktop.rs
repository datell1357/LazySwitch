use std::collections::HashSet;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

use crate::config::ProviderPrefs;
use crate::desktop_processes::{kill_windows_desktop_processes, resolve_desktop_aumid};

/// Candidate install locations for the Codex Desktop executable.
fn desktop_candidates() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(target_os = "windows") {
        vec![
            home.join("AppData").join("Local").join("Programs").join("Codex").join("Codex.exe"),
            home.join("AppData").join("Local").join("Codex").join("Codex.exe"),
            PathBuf::from(r"C:\Program Files\Codex\Codex.exe"),
            // Codex Desktop >= 26.7 ships merged into the ChatGPT app.
            home.join("AppData").join("Local").join("Programs").join("ChatGPT").join("ChatGPT.exe"),
            home.join("AppData").join("Local").join("ChatGPT").join("ChatGPT.exe"),
            PathBuf::from(r"C:\Program Files\ChatGPT\ChatGPT.exe"),
        ]
    } else {
        // macOS
        vec![
            PathBuf::from("/Applications/Codex.app"),
            PathBuf::from("/Applications/ChatGPT.app"),
        ]
    }
}

async fn resolve_desktop_path(cfg: &ProviderPrefs) -> Option<String> {
    if !cfg.desktop_app_path.is_empty() {
        // MSIX/Store installs are launched by AppUserModelID, not exe path,
        // e.g. "shell:AppsFolder\OpenAI.Codex_2p2nqsd0c76g0!App".
        if cfg.desktop_app_path.starts_with("shell:") {
            return Some(cfg.desktop_app_path.clone());
        }
        if std::path::Path::new(&cfg.desktop_app_path).exists() {
            return Some(cfg.desktop_app_path.clone());
        }
    }
    if let Some(file) = desktop_candidates().into_iter().find(|p| p.exists()) {
        return Some(file.to_string_lossy().to_string());
    }
    // Store/MSIX install — no spawnable exe path; launch by AppUserModelID.
    resolve_desktop_aumid().await
}

/// Process names to kill. The merged ChatGPT app renamed the main process
/// from Codex.exe to ChatGPT.exe, so both known names are covered on Windows
/// in addition to whatever the user configured. The executable-path filter
/// in `select_desktop_process_ids` keeps unrelated same-named processes alive.
fn desktop_process_names(cfg: &ProviderPrefs) -> Vec<String> {
    let candidates: Vec<&str> = if cfg!(target_os = "windows") {
        vec![&cfg.desktop_process_name, "Codex.exe", "ChatGPT.exe"]
    } else {
        vec![&cfg.desktop_process_name]
    };
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && seen.insert(n.clone()))
        .collect()
}

async fn kill_process(cfg: &ProviderPrefs, desktop_app_path: Option<&str>) {
    if cfg!(target_os = "windows") {
        let names = desktop_process_names(cfg);
        kill_windows_desktop_processes(&names, desktop_app_path).await;
        return;
    }
    let _ = Command::new("pkill")
        .args(["-f", &cfg.desktop_process_name])
        .output()
        .await;
}

fn launch(app_path: &str) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open").arg(app_path).spawn();
    } else if app_path.starts_with("shell:") {
        // Store/MSIX app: WindowsApps exes can't be spawned directly.
        let _ = std::process::Command::new("explorer.exe").arg(app_path).spawn();
    } else {
        let _ = std::process::Command::new(app_path).spawn();
    }
}

/// Fully restart the Codex Desktop app so it re-reads the swapped auth.json.
/// Desktop keeps the old token in memory and in its session cache, so a full
/// kill (incl. tray) + relaunch is required — a plain window close is not
/// enough. Returns false if the executable could not be located.
pub async fn restart_desktop_app(cfg: &ProviderPrefs) -> bool {
    let app_path = resolve_desktop_path(cfg).await;
    kill_process(cfg, app_path.as_deref()).await;
    let Some(app_path) = app_path else {
        return false;
    };
    // Small delay so the OS releases file/socket handles before relaunch.
    sleep(Duration::from_millis(1500)).await;
    launch(&app_path);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(desktop_process_name: &str) -> ProviderPrefs {
        ProviderPrefs {
            auto_approve: false,
            auto_restart_cli: true,
            desktop_app_path: String::new(),
            desktop_process_name: desktop_process_name.to_string(),
            rotation_order: Vec::new(),
            primary_min_left_pct: 5.0,
            weekly_min_left_pct: 1.0,
            poll_interval_sec: 30,
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn desktop_process_names_dedupes_and_adds_known_aliases() {
        let names = desktop_process_names(&prefs("Codex.exe"));
        assert_eq!(names, vec!["Codex.exe".to_string(), "ChatGPT.exe".to_string()]);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn desktop_process_names_keeps_custom_configured_name() {
        let names = desktop_process_names(&prefs("MyCodex.exe"));
        assert_eq!(
            names,
            vec!["MyCodex.exe".to_string(), "Codex.exe".to_string(), "ChatGPT.exe".to_string()]
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn desktop_process_names_drops_blank_configured_name() {
        let names = desktop_process_names(&prefs("  "));
        assert_eq!(names, vec!["Codex.exe".to_string(), "ChatGPT.exe".to_string()]);
    }
}
