use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, Wry};

use crate::app_state::AppState;
use crate::cli_handover;
use crate::config::{self, AppConfig, CompactPosition};
use crate::i18n::{resolve_lang, Lang};
use crate::provider;
use crate::provider_types::{LoginFlowResult, PAccount, PUsage, ProviderId};
use crate::windows::cli_restart::TauriCliHandoverDeps;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    id: ProviderId,
    display_name: &'static str,
    has_login_flow: bool,
    has_desktop: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummary {
    #[serde(flatten)]
    account: PAccount,
    active: bool,
    cooling_down_until: Option<i64>,
    usage: Option<PUsage>,
}

fn merge_object(base: &mut Value, patch: Option<&Value>) {
    let (Some(base), Some(patch)) = (base.as_object_mut(), patch.and_then(Value::as_object)) else {
        return;
    };
    for (key, value) in patch {
        base.insert(key.clone(), value.clone());
    }
}

pub fn merge_config(current: &AppConfig, patch: &Value) -> Result<AppConfig, String> {
    let mut merged = serde_json::to_value(current).map_err(|error| error.to_string())?;
    let Some(patch_object) = patch.as_object() else {
        return Ok(current.clone());
    };

    let current_codex = merged["codex"].clone();
    let current_claude = merged["claude"].clone();
    let current_widget = merged["usageWidget"].clone();
    merge_object(&mut merged, Some(patch));
    merged["codex"] = current_codex;
    merged["claude"] = current_claude;
    merged["usageWidget"] = current_widget;
    merge_object(&mut merged["codex"], patch_object.get("codex"));
    merge_object(&mut merged["claude"], patch_object.get("claude"));
    merge_object(&mut merged["usageWidget"], patch_object.get("usageWidget"));

    if serde_json::from_value::<CompactPosition>(merged["usageWidget"]["compactPosition"].clone())
        .is_err()
    {
        merged["usageWidget"]["compactPosition"] = Value::String("taskbar".into());
    }

    serde_json::from_value(merged).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn config_get(state: State<'_, Mutex<AppState>>) -> Result<AppConfig, String> {
    state
        .lock()
        .map(|state| state.cfg.clone())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn config_set(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    patch: Value,
) -> Result<AppConfig, String> {
    let mut state = state.lock().map_err(|error| error.to_string())?;
    let previous = state.cfg.clone();
    let next = merge_config(&previous, &patch)?;

    // TODO(widget-part2): capture bounds before minimizing and synchronize
    // compact positioning when minimized/compactPosition changes.
    let restart_monitors = previous.codex.poll_interval_sec != next.codex.poll_interval_sec
        || previous.claude.poll_interval_sec != next.claude.poll_interval_sec;
    // TODO(window-layer): apply launch-at-login via tauri-plugin-autostart.
    apply_launch_at_login_stub(&previous, &next, &patch);

    config::save_config(&config::config_path(), &next).map_err(|error| error.to_string())?;
    state.cfg = next.clone();
    drop(state);

    if previous.usage_widget.always_on_top != next.usage_widget.always_on_top {
        if let Some(window) = app.get_webview_window("usage-widget") {
            window
                .set_always_on_top(next.usage_widget.always_on_top)
                .map_err(|error| error.to_string())?;
        }
    }
    if previous.usage_widget.enabled != next.usage_widget.enabled {
        crate::windows::widget::sync_usage_widget(&app)?;
        crate::tray::refresh_tray(&app)?;
    }
    if restart_monitors {
        crate::limit_handler::restart_monitors(&app)?;
    }
    crate::limit_handler::broadcast_changed(&app);
    Ok(next)
}

#[tauri::command]
pub fn lang_get(state: State<'_, Mutex<AppState>>) -> Result<&'static str, String> {
    state
        .lock()
        .map(|state| match resolve_lang(&state.cfg.language) {
            Lang::Ko => "ko",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Zh => "zh",
        })
        .map_err(|error| error.to_string())
}

fn apply_launch_at_login_stub(_previous: &AppConfig, _next: &AppConfig, _patch: &Value) {}

#[tauri::command]
pub fn providers_list() -> Vec<ProviderSummary> {
    ProviderId::ALL
        .into_iter()
        .map(|id| ProviderSummary {
            id,
            display_name: id.display_name(),
            has_login_flow: true,
            has_desktop: id == ProviderId::Codex,
        })
        .collect()
}

#[tauri::command]
pub fn accounts_list(
    app: AppHandle<Wry>,
    state: State<'_, Mutex<AppState>>,
    pid: ProviderId,
) -> Result<Vec<AccountSummary>, String> {
    let active = provider::active_account_name(pid);
    let accounts = provider::list_accounts(pid);
    let mut state = state.lock().map_err(|error| error.to_string())?;
    let cooling_down = state.state_of(pid).cooling_down.clone();
    let mut summaries = Vec::with_capacity(accounts.len());
    for account in accounts {
        let usage_name = (Some(&account.name) != active.as_ref()).then_some(account.name.clone());
        let cached = provider::cached_usage(pid, usage_name.as_deref());
        let key = format!(
            "{}:{}",
            pid.as_str(),
            usage_name
                .as_deref()
                .map(|name| format!("slot:{name}"))
                .unwrap_or_else(|| "live".to_string())
        );
        if state.pending_usage_refreshes.insert(key.clone()) {
            let refresh_app = app.clone();
            let cached_for_task = cached.clone();
            tauri::async_runtime::spawn(async move {
                let latest = provider::fetch_usage(pid, usage_name.as_deref()).await;
                if latest != cached_for_task {
                    crate::limit_handler::broadcast_changed(&refresh_app);
                }
                if let Ok(mut state) = refresh_app.state::<Mutex<AppState>>().lock() {
                    state.pending_usage_refreshes.remove(&key);
                }
            });
        }
        summaries.push(AccountSummary {
            active: Some(&account.name) == active.as_ref(),
            cooling_down_until: cooling_down.get(&account.name).copied(),
            usage: cached,
            account,
        });
    }
    Ok(summaries)
}

#[tauri::command]
pub async fn accounts_switch(
    app: AppHandle<Wry>,
    pid: ProviderId,
    name: String,
) -> Result<Value, String> {
    crate::limit_handler::manual_switch(app, pid, name).await?;
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub fn accounts_set_enabled(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
    pid: ProviderId,
    name: String,
    enabled: bool,
) -> Value {
    if window.label() != "manager" {
        return json!({ "ok": false, "error": "manager window required" });
    }
    if !provider::list_accounts(pid)
        .iter()
        .any(|account| account.name == name)
    {
        return json!({ "ok": false, "error": "account is not enrolled" });
    }
    match provider::set_account_enabled(pid, &name, enabled) {
        Ok(()) => {
            crate::limit_handler::broadcast_changed(&app);
            json!({ "ok": true })
        }
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
pub fn accounts_remove(app: AppHandle<Wry>, pid: ProviderId, name: String) -> Value {
    provider::remove_account(pid, &name);
    crate::limit_handler::broadcast_changed(&app);
    json!({ "ok": true })
}

#[tauri::command]
pub fn accounts_rename(
    app: AppHandle<Wry>,
    pid: ProviderId,
    old_name: String,
    new_name: String,
) -> Value {
    match provider::rename_account(pid, &old_name, &new_name) {
        Ok(()) => {
            crate::limit_handler::broadcast_changed(&app);
            json!({ "ok": true })
        }
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
pub fn accounts_import_current(
    app: AppHandle<Wry>,
    pid: ProviderId,
    name: Option<String>,
) -> Value {
    provider::sync_live_back_to_slot(pid);
    match provider::import_current(pid, name.as_deref()) {
        Ok(account) => {
            crate::limit_handler::broadcast_changed(&app);
            json!({ "ok": true, "name": account.name })
        }
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
pub async fn accounts_add_via_login(app: AppHandle<Wry>, pid: ProviderId) -> LoginFlowResult {
    let event_app = app.clone();
    let result = provider::add_via_login(
        pid,
        Some(move |url| {
            if let Some(window) = event_app.get_webview_window("manager") {
                let _ = window.emit("login:url", url);
            }
        }),
    )
    .await;
    if result.ok {
        crate::limit_handler::broadcast_changed(&app);
    }
    result
}

#[tauri::command]
pub async fn cli_test_restart(app: AppHandle<Wry>, pid: ProviderId) -> Value {
    let sessions = cli_handover::detect(pid, i64::from(std::process::id())).await;
    let count = sessions.len();
    let result = tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(async move {
            let mut deps = TauriCliHandoverDeps::new(app);
            cli_handover::schedule(pid, &sessions, &mut deps).await
        })
    })
    .await
    .ok()
    .flatten();
    json!({
        "ok": true,
        "sessions": count,
        "result": result.map(|result| json!({
            "restarted": result.restarted,
            "closed": result.closed,
            "manual": result.manual
        }))
    })
}

#[tauri::command]
pub fn onboarding_finish(app: AppHandle<Wry>, open_accounts: bool) -> Result<bool, String> {
    {
        let state_handle = app.state::<Mutex<AppState>>();
        let mut state = state_handle.lock().map_err(|error| error.to_string())?;
        state.cfg.onboarded = true;
        config::save_config(&config::config_path(), &state.cfg)
            .map_err(|error| error.to_string())?;
    }
    if let Some(window) = app.get_webview_window("onboarding") {
        window.close().map_err(|error| error.to_string())?;
    }
    if open_accounts {
        crate::windows::manager::open_manager(&app).map_err(|error| error.to_string())?;
    }
    crate::windows::widget::sync_usage_widget(&app)?;
    Ok(true)
}

#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    open::that(url).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn manager_close(app: AppHandle<Wry>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("manager") {
        window.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_patch_merges_nested_sections_without_erasing_siblings() {
        let current = AppConfig::default();

        let next = merge_config(
            &current,
            &json!({
                "language": "ko",
                "codex": { "pollIntervalSec": 45 },
                "usageWidget": { "minimized": true }
            }),
        )
        .unwrap();

        assert_eq!(next.language, "ko");
        assert_eq!(next.codex.poll_interval_sec, 45);
        assert_eq!(next.codex.auto_restart_cli, current.codex.auto_restart_cli);
        assert!(next.usage_widget.minimized);
        assert_eq!(next.usage_widget.width, current.usage_widget.width);
    }

    #[test]
    fn invalid_compact_position_falls_back_to_taskbar() {
        let next = merge_config(
            &AppConfig::default(),
            &json!({ "usageWidget": { "compactPosition": "upper-left" } }),
        )
        .unwrap();

        assert_eq!(next.usage_widget.compact_position, CompactPosition::Taskbar);
    }

    #[test]
    fn non_object_patch_leaves_config_unchanged() {
        let current = AppConfig::default();

        assert_eq!(merge_config(&current, &Value::Null).unwrap(), current);
    }
}
