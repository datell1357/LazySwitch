use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::accounts::{
    active_account_id, active_account_name, derive_slot_name, email_from_auth, import_current_as,
    list_accounts as list_codex_accounts, read_auth, remove_account as remove_codex_account,
    rename_account as rename_codex_account, set_account_enabled as set_codex_account_enabled,
};
use crate::atomic_fs::atomic_copy;
use crate::codex_api::{
    cached_usage as cached_codex_usage, fetch_usage as fetch_codex_usage, invalidate_usage,
};
use crate::config::ProviderPrefs;
use crate::desktop::restart_desktop_app;
use crate::login::add_account_via_login;
use crate::paths::{account_auth_file, live_auth_file, sessions_dir};
use crate::provider_types::{LoginFlowResult, PAccount, PUsage, PWindow, SessionUsage};

pub fn list_accounts() -> Vec<PAccount> {
    list_codex_accounts()
        .into_iter()
        .map(|a| PAccount {
            name: a.name,
            email: a.email,
            account_id: a.account_id,
            label: a.label,
            enabled: a.enabled,
        })
        .collect()
}

pub fn active_name() -> Option<String> {
    active_account_name()
}

pub fn has_live_auth() -> bool {
    active_account_id().is_some()
}

pub fn import_current(name: Option<&str>) -> Result<PAccount, String> {
    let slot = match name.map(|n| n.trim()).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => derive_slot_name(
            email_from_auth(read_auth(&live_auth_file()).as_ref()).as_deref(),
        ),
    };
    let a = import_current_as(&slot)?;
    Ok(PAccount {
        name: a.name,
        email: a.email,
        account_id: a.account_id,
        label: a.label,
        enabled: a.enabled,
    })
}

pub fn remove_account(name: &str) {
    remove_codex_account(name);
}

pub fn rename_account(old_name: &str, new_name: &str) -> Result<(), String> {
    rename_codex_account(old_name, new_name)
}

pub fn set_account_enabled(name: &str, enabled: bool) -> Result<(), String> {
    set_codex_account_enabled(name, enabled)
}

pub fn sync_live_back_to_slot() {
    let Some(active) = active_account_name() else {
        return;
    };
    if read_auth(&live_auth_file()).is_none() {
        return;
    }
    let _ = atomic_copy(&live_auth_file(), &account_auth_file(&active));
}

pub fn install_auth(name: &str) -> Result<(), String> {
    let src = account_auth_file(name);
    if !src.exists() {
        return Err(format!("Account \"{name}\" has no auth.json"));
    }
    atomic_copy(&src, &live_auth_file()).map_err(|e| e.to_string())?;
    // The live-file cache still holds the previous account's usage; serving
    // it for the new account would immediately re-trigger a switch.
    invalidate_usage(&live_auth_file());
    Ok(())
}

pub async fn fetch_usage(name: Option<&str>) -> Option<PUsage> {
    let file = match name {
        None => live_auth_file(),
        Some(n) => account_auth_file(n),
    };
    let u = fetch_codex_usage(&file).await?;
    Some(PUsage {
        primary: u.primary,
        secondary: u.secondary,
        fable: None,
        plan_type: u.plan_type,
        email: u.email,
    })
}

pub fn cached_usage(name: Option<&str>) -> Option<PUsage> {
    let file = match name {
        None => live_auth_file(),
        Some(n) => account_auth_file(n),
    };
    let u = cached_codex_usage(&file)?;
    Some(PUsage {
        primary: u.primary,
        secondary: u.secondary,
        fable: None,
        plan_type: u.plan_type,
        email: u.email,
    })
}

pub async fn desktop_restart(prefs: &ProviderPrefs) -> bool {
    restart_desktop_app(prefs).await
}

pub async fn add_via_login(
    on_url: Option<impl Fn(String) + Send + Sync + 'static>,
) -> LoginFlowResult {
    add_account_via_login(on_url).await
}

// ---------------------------------------------------------------------------
// Session-file scanning (moved verbatim from monitor.ts — Codex-specific).
// ---------------------------------------------------------------------------

const SESSION_SCAN_CACHE_MS: i64 = 60 * 1000;

#[derive(Clone)]
struct SessionScanResult {
    usage: Option<SessionUsage>,
    error: Option<String>,
}

fn scan_cache() -> &'static Mutex<Option<(i64, SessionScanResult)>> {
    static CACHE: OnceLock<Mutex<Option<(i64, SessionScanResult)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

/// Recursively find the most recently modified rollout-*.jsonl.
fn newest_rollout(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, SystemTime)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                    if best.as_ref().map(|(_, t)| mtime > *t).unwrap_or(true) {
                        best = Some((path.clone(), mtime));
                    }
                }
            }
        }
    }
    best.map(|(p, _)| p)
}

fn to_window(raw: Option<&Value>) -> Option<PWindow> {
    let raw = raw?;
    let used_percent = raw.get("used_percent")?.as_f64()?;
    let resets_in = raw.get("resets_in_seconds").and_then(|v| v.as_i64());
    Some(PWindow {
        used_percent,
        window_minutes: raw.get("window_minutes").and_then(|v| v.as_i64()),
        resets_at: resets_in.map(|s| now_ms() + s * 1000),
    })
}

const ERROR_MARKERS: &[&str] = &[
    "usage limit reached",
    "you've hit your usage limit",
    "rate limit",
    "too many requests",
    "quota",
];

/// Read the (approximately last-400-line) tail of the newest session file
/// and extract:
///  - the most recent rate_limits object (proactive signal, if this codex
///    build emits it)
///  - any usage-limit error line (reactive backstop)
fn scan_file(file: &Path) -> SessionScanResult {
    let contents = match fs::read_to_string(file) {
        Ok(c) => c,
        Err(_) => return SessionScanResult { usage: None, error: None },
    };
    let lines: Vec<&str> = contents.split('\n').collect();
    let mut usage: Option<SessionUsage> = None;
    let mut error: Option<String> = None;

    let floor = lines.len().saturating_sub(400);
    for line in lines[floor..].iter().rev() {
        if line.is_empty() {
            continue;
        }
        if error.is_none() || usage.is_none() {
            if let Ok(obj) = serde_json::from_str::<Value>(line) {
                if error.is_none() {
                    let ptype = obj
                        .get("payload")
                        .and_then(|p| p.get("type"))
                        .and_then(|v| v.as_str());
                    if ptype == Some("error") || ptype == Some("stream_error") {
                        let msg = obj
                            .get("payload")
                            .and_then(|p| p.get("message"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        if let Some(marker) = ERROR_MARKERS.iter().find(|m| msg.contains(**m)) {
                            error = Some((*marker).to_string());
                        }
                    }
                }
                if usage.is_none() {
                    let rl = obj
                        .get("payload")
                        .and_then(|p| p.get("info"))
                        .and_then(|i| i.get("rate_limits"))
                        .or_else(|| obj.get("rate_limits"));
                    if let Some(rl) = rl {
                        usage = Some(SessionUsage {
                            primary: to_window(rl.get("primary")),
                            secondary: to_window(rl.get("secondary")),
                        });
                    }
                }
            }
        }
        if usage.is_some() && error.is_some() {
            break;
        }
    }
    SessionScanResult { usage, error }
}

fn scan_session() -> SessionScanResult {
    let now = now_ms();
    {
        let guard = scan_cache().lock().unwrap();
        if let Some((at, result)) = guard.as_ref() {
            if now - at < SESSION_SCAN_CACHE_MS {
                return result.clone();
            }
        }
    }
    let result = match newest_rollout(&sessions_dir()) {
        None => SessionScanResult { usage: None, error: None },
        Some(f) => scan_file(&f),
    };
    *scan_cache().lock().unwrap() = Some((now, result.clone()));
    result
}

pub fn session_usage() -> Option<SessionUsage> {
    scan_session().usage
}

pub fn scan_error() -> Option<String> {
    scan_session().error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_window_requires_numeric_used_percent() {
        assert_eq!(to_window(None), None);
        assert_eq!(to_window(Some(&serde_json::json!({}))), None);
        let w = to_window(Some(&serde_json::json!({ "used_percent": 42.5, "window_minutes": 300 }))).unwrap();
        assert_eq!(w.used_percent, 42.5);
        assert_eq!(w.window_minutes, Some(300));
        assert_eq!(w.resets_at, None);
    }

    #[test]
    fn to_window_computes_resets_at_from_seconds() {
        let w = to_window(Some(&serde_json::json!({ "used_percent": 1, "resets_in_seconds": 60 }))).unwrap();
        assert!(w.resets_at.unwrap() > now_ms());
    }

    #[test]
    fn scan_file_detects_error_marker_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("lazyswitch-scan-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rollout-test.jsonl");
        fs::write(
            &file,
            r#"{"payload":{"type":"error","message":"You've Hit Your Usage Limit for today"}}"#,
        )
        .unwrap();
        let result = scan_file(&file);
        assert_eq!(result.error.as_deref(), Some("you've hit your usage limit"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_file_extracts_latest_rate_limits() {
        let dir = std::env::temp_dir().join(format!("lazyswitch-scan2-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rollout-test.jsonl");
        let content = "\
{\"rate_limits\":{\"primary\":{\"used_percent\":10}}}
{\"rate_limits\":{\"primary\":{\"used_percent\":90}}}
";
        fs::write(&file, content).unwrap();
        let result = scan_file(&file);
        assert_eq!(result.usage.unwrap().primary.unwrap().used_percent, 90.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn newest_rollout_picks_most_recently_modified_nested_file() {
        let dir = std::env::temp_dir().join(format!("lazyswitch-rollouts-{}", std::process::id()));
        let sub = dir.join("2024").join("01");
        fs::create_dir_all(&sub).unwrap();
        let old = dir.join("rollout-old.jsonl");
        let new = sub.join("rollout-new.jsonl");
        fs::write(&old, "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&new, "new").unwrap();
        assert_eq!(newest_rollout(&dir), Some(new));
        let _ = fs::remove_dir_all(&dir);
    }
}
