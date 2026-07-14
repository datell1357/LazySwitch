use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::core::config::{self, AppConfig};
use crate::core::platform;
use crate::core::switcher;
use crate::core::types::LoginFlowResult;

use super::monitor::{cli_sessions_for, schedule_cli_handover};
use super::probe::parse_probe_report;
use super::state::{
    list_accounts, provider_info, provider_prefs, state_config, AppState, EnrichedAccount,
    OperationResult, ProviderInfo, ToastPayload,
};
use super::tray::show_widget_context_menu;
use super::windows::{
    apply_widget_platform, broadcast_changed, clamp_toasts, close_window,
    emit_widget_taskbar_theme, emit_to, notify, open_external, open_manager, open_widget,
    sync_usage_widget, TOAST_DEFAULT_HEIGHT, TOAST_MAX_HEIGHT, TOAST_MIN_HEIGHT,
    WIDGET_COMPACT_MIN_HEIGHT, WIDGET_COMPACT_WIDTH,
};

#[tauri::command(rename = "providers:list")]
pub fn providers_list(state: tauri::State<'_, AppState>) -> Vec<ProviderInfo> {
    provider_info(&state)
}

#[tauri::command(rename = "usage:history")]
pub fn usage_history(provider: String, name: String) -> Vec<crate::core::usage_history::Sample> {
    crate::core::usage_history::history_for(&provider, &name)
}

#[tauri::command(rename = "accounts:list")]
pub fn accounts_list(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
) -> Result<Vec<EnrichedAccount>, String> {
    list_accounts(&app, &state, &provider)
}

#[tauri::command(rename = "accounts:switch")]
pub async fn accounts_switch(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
    name: String,
) -> Result<OperationResult, String> {
    let Ok(account_provider) = state.providers.get(&provider) else {
        return Ok(OperationResult {
            ok: false,
            error: Some("unknown provider".into()),
            name: None,
        });
    };
    let config = state_config(&state);
    let Ok(prefs) = provider_prefs(&config, &provider) else {
        return Ok(OperationResult {
            ok: false,
            error: Some("unknown provider".into()),
            name: None,
        });
    };
    let cli_sessions = cli_sessions_for(&provider);
    match switcher::switch_to(account_provider.as_ref(), &name, &prefs, false).await {
        Ok(_) => {
            let desktop_restarted = account_provider.desktop_restart(&prefs).await;
            if desktop_restarted {
                eprintln!("[accounts:switch] desktop restarted");
            }
            let app_for_cli = app.clone();
            let provider_for_cli = provider.clone();
            tauri::async_runtime::spawn(async move {
                let _ = schedule_cli_handover(app_for_cli, provider_for_cli, cli_sessions).await;
            });
            broadcast_changed(&app);
            notify(
                &app,
                format!("{provider} switched"),
                format!(
                    "{name} is now active{}",
                    if desktop_restarted {
                        " (desktop restarted)"
                    } else {
                        ""
                    }
                ),
            );
            Ok(OperationResult {
                ok: true,
                error: None,
                name: None,
            })
        }
        Err(error) => Ok(OperationResult {
            ok: false,
            error: Some(error.to_string()),
            name: None,
        }),
    }
}

#[tauri::command(rename = "accounts:setEnabled")]
pub fn accounts_set_enabled(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
    provider: String,
    name: String,
    enabled: bool,
) -> OperationResult {
    if window.label() != "manager" {
        return OperationResult {
            ok: false,
            error: Some("manager window required".into()),
            name: None,
        };
    }
    let Ok(account_provider) = state.providers.get(&provider) else {
        return OperationResult {
            ok: false,
            error: Some("unknown provider".into()),
            name: None,
        };
    };
    if !account_provider
        .list_accounts()
        .iter()
        .any(|account| account.name == name)
    {
        return OperationResult {
            ok: false,
            error: Some("account is not enrolled".into()),
            name: None,
        };
    }
    match account_provider.set_account_enabled(&name, enabled) {
        Ok(()) => {
            broadcast_changed(&app);
            OperationResult {
                ok: true,
                error: None,
                name: None,
            }
        }
        Err(error) => OperationResult {
            ok: false,
            error: Some(error.to_string()),
            name: None,
        },
    }
}

#[tauri::command(rename = "accounts:remove")]
pub fn accounts_remove(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
    name: String,
) -> OperationResult {
    let Ok(account_provider) = state.providers.get(&provider) else {
        return OperationResult {
            ok: false,
            error: Some("unknown provider".into()),
            name: None,
        };
    };
    match account_provider.remove_account(&name) {
        Ok(()) => {
            broadcast_changed(&app);
            OperationResult {
                ok: true,
                error: None,
                name: None,
            }
        }
        Err(error) => OperationResult {
            ok: false,
            error: Some(error.to_string()),
            name: None,
        },
    }
}

#[tauri::command(rename = "accounts:rename")]
pub fn accounts_rename(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
    old_name: String,
    new_name: String,
) -> OperationResult {
    let Ok(account_provider) = state.providers.get(&provider) else {
        return OperationResult {
            ok: false,
            error: Some("unknown provider".into()),
            name: None,
        };
    };
    match account_provider.rename_account(&old_name, &new_name) {
        Ok(()) => {
            broadcast_changed(&app);
            OperationResult {
                ok: true,
                error: None,
                name: None,
            }
        }
        Err(error) => OperationResult {
            ok: false,
            error: Some(error.to_string()),
            name: None,
        },
    }
}

#[tauri::command(rename = "accounts:importCurrent")]
pub fn accounts_import_current(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
    name: Option<String>,
) -> OperationResult {
    let Ok(account_provider) = state.providers.get(&provider) else {
        return OperationResult {
            ok: false,
            error: Some("unknown provider".into()),
            name: None,
        };
    };
    match account_provider
        .sync_live_back_to_slot()
        .and_then(|_| account_provider.import_current(name.as_deref()))
    {
        Ok(account) => {
            let result = OperationResult {
                ok: true,
                error: None,
                name: Some(account.name),
            };
            broadcast_changed(&app);
            result
        }
        Err(error) => OperationResult {
            ok: false,
            error: Some(error.to_string()),
            name: None,
        },
    }
}

#[tauri::command(rename = "accounts:addViaLogin")]
pub async fn accounts_add_via_login(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
) -> Result<LoginFlowResult, String> {
    let app_for_url = app.clone();
    let on_url: Arc<dyn Fn(String) + Send + Sync> =
        Arc::new(move |url| emit_to(&app_for_url, "manager", "login:url", url));
    match provider.as_str() {
        "claude" => Ok(state
            .providers
            .claude
            .add_via_login_with_callback(Some(on_url))
            .await),
        "codex" => Ok(state
            .providers
            .codex
            .add_via_login_with_callback(Some(on_url))
            .await),
        _ => Ok(LoginFlowResult {
            ok: false,
            error: Some("unsupported".into()),
            ..LoginFlowResult::default()
        }),
    }
}

#[tauri::command(rename = "cli:testRestart")]
pub async fn cli_test_restart(app: AppHandle, provider: String) -> Value {
    let sessions = cli_sessions_for(&provider);
    let count = sessions.len();
    let result = schedule_cli_handover(app, provider, sessions).await;
    serde_json::json!({"ok": true, "sessions": count, "result": result})
}

#[tauri::command(rename = "config:get")]
pub fn config_get(state: tauri::State<'_, AppState>) -> AppConfig {
    state_config(&state)
}

#[tauri::command(rename = "lang:get")]
pub fn lang_get(state: tauri::State<'_, AppState>) -> String {
    let config = state_config(&state);
    let locale = std::env::var("LANG").ok();
    format!(
        "{:?}",
        crate::core::i18n::resolve_lang(&config.language, locale.as_deref())
    )
    .to_lowercase()
}

#[tauri::command(rename = "config:set")]
pub fn config_set(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    patch: Value,
) -> Result<AppConfig, String> {
    let previous = state_config(&state);
    let mut current =
        serde_json::to_value(state_config(&state)).map_err(|error| error.to_string())?;
    let Some(patch_object) = patch.as_object() else {
        return Err("config patch must be an object".into());
    };
    let Some(current_object) = current.as_object_mut() else {
        return Err("invalid current config".into());
    };
    for (key, value) in patch_object {
        if let (Some(Value::Object(destination)), Value::Object(source)) =
            (current_object.get_mut(key), value)
        {
            for (nested_key, nested_value) in source {
                destination.insert(nested_key.clone(), nested_value.clone());
            }
        } else {
            current_object.insert(key.clone(), value.clone());
        }
    }
    let next: AppConfig = serde_json::from_value(current).map_err(|error| error.to_string())?;
    let mut next = next;
    if !matches!(
        next.usage_widget.compact_position.as_str(),
        "taskbar" | "bottom-right" | "bottom-left"
    ) {
        next.usage_widget.compact_position = "taskbar".into();
    }
    config::save_config(&next).map_err(|error| error.to_string())?;
    if let Ok(mut config) = state.config.lock() {
        *config = next.clone();
    }
    #[cfg(windows)]
    if previous.launch_at_login != next.launch_at_login {
        if let Ok(exe) = std::env::current_exe() {
            platform::apply_launch_at_login(next.launch_at_login, &exe)
                .map_err(|error| format!("start at login: {error}"))?;
        }
    }
    let previous_transparent =
        previous.usage_widget.minimized && previous.usage_widget.compact_position == "taskbar";
    let next_transparent =
        next.usage_widget.minimized && next.usage_widget.compact_position == "taskbar";
    if let Some(widget) = app.get_webview_window("widget") {
        if previous_transparent != next_transparent {
            let _ = widget.destroy();
            let app_for_recreate = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                open_widget(&app_for_recreate, true);
            });
        } else {
            let _ = widget.set_always_on_top(next.usage_widget.always_on_top);
            apply_widget_platform(&app, &widget, &next);
            emit_widget_taskbar_theme(&app, &widget, &next);
        }
    }
    sync_usage_widget(&app);
    broadcast_changed(&app);
    Ok(next)
}

#[tauri::command(rename = "onboarding:finish")]
pub fn onboarding_finish(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    open_accounts: bool,
) -> bool {
    if let Ok(mut config) = state.config.lock() {
        config.onboarded = true;
        let _ = config::save_config(&config);
    }
    close_window(&app, "onboarding");
    if open_accounts {
        open_manager(&app);
    }
    sync_usage_widget(&app);
    let app_for_sync = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        sync_usage_widget(&app_for_sync);
    });
    true
}

#[tauri::command(rename = "open:url")]
pub fn open_url(url: String) -> Result<(), String> {
    open_external(&url)
}

#[tauri::command(rename = "manager:close")]
pub fn manager_close(app: AppHandle) {
    close_window(&app, "manager");
}

#[tauri::command(rename = "widget:close")]
pub fn widget_close(app: AppHandle, state: tauri::State<'_, AppState>) {
    if let Ok(mut config) = state.config.lock() {
        config.usage_widget.enabled = false;
        let _ = config::save_config(&config);
    }
    close_window(&app, "widget");
}

#[tauri::command(rename = "widget-settings:close")]
pub fn widget_settings_close(app: AppHandle) {
    close_window(&app, "widget-settings");
}

#[tauri::command(rename = "widget:compact-height")]
pub fn widget_compact_height(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
    height: f64,
) {
    if window.label() != "widget" || !state_config(&state).usage_widget.minimized {
        return;
    }
    let next = height.ceil().clamp(WIDGET_COMPACT_MIN_HEIGHT, 160.0);
    let _ = window.set_size(tauri::PhysicalSize::new(
        WIDGET_COMPACT_WIDTH as u32,
        next as u32,
    ));
    apply_widget_platform(&app, &window, &state_config(&state));
}

#[tauri::command(rename = "widget:context-menu")]
pub fn widget_context_menu(app: AppHandle) {
    show_widget_context_menu(&app);
}

#[tauri::command(rename = "approval:respond")]
pub fn approval_respond(app: AppHandle, state: tauri::State<'_, AppState>, approved: bool) {
    if let Ok(mut runtime) = state.runtime.lock() {
        if let Some(sender) = runtime.approval.take() {
            let _ = sender.send(approved);
        }
    }
    close_window(&app, "approval");
}

#[tauri::command(rename = "cli-restart:payload")]
pub fn cli_restart_payload(state: tauri::State<'_, AppState>) -> Option<Value> {
    state
        .runtime
        .lock()
        .ok()
        .and_then(|runtime| runtime.cli_payload.clone())
}

#[tauri::command(rename = "cli-restart:respond")]
pub fn cli_restart_respond(app: AppHandle, state: tauri::State<'_, AppState>, action: String) {
    if let Ok(mut runtime) = state.runtime.lock() {
        if let Some(sender) = runtime.cli_response.take() {
            let action = if action == "restart" || action == "copy" {
                action
            } else {
                "later".into()
            };
            let _ = sender.send(action);
        }
    }
    close_window(&app, "cli-restart");
}

#[tauri::command(rename = "app-notify:payload")]
pub fn app_notify_payload(
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
) -> Option<ToastPayload> {
    state
        .runtime
        .lock()
        .ok()
        .and_then(|runtime| runtime.toast_payloads.get(window.label()).cloned())
}

#[tauri::command(rename = "app-notify:resize")]
pub fn app_notify_resize(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
    height: f64,
) {
    let next = if height.is_finite() {
        height.ceil().clamp(TOAST_MIN_HEIGHT, TOAST_MAX_HEIGHT)
    } else {
        TOAST_DEFAULT_HEIGHT
    };
    if let Ok(mut runtime) = state.runtime.lock() {
        if runtime.toast_heights.contains_key(window.label()) {
            runtime
                .toast_heights
                .insert(window.label().to_owned(), next);
        }
    }
    clamp_toasts(&state, &app);
}

#[tauri::command(rename = "app-notify:dismiss")]
pub fn app_notify_dismiss(window: WebviewWindow) {
    let _ = window.close();
}

#[tauri::command(rename = "window:startDragging")]
pub fn start_dragging(window: WebviewWindow) {
    if let Err(error) = window.start_dragging() {
        eprintln!("[tauri:drag] {error}");
    }
}

#[tauri::command(rename = "probe:enabled")]
pub fn probe_enabled() -> bool {
    std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some()
}

#[tauri::command(rename = "probe:windows")]
pub async fn probe_windows(app: AppHandle) -> Value {
    #[cfg(windows)]
    {
        let value = app
            .get_webview_window("widget")
            .and_then(|window| window.hwnd().ok())
            .map(|hwnd| hwnd.0 as isize);
        if let Some(value) = value {
            return tauri::async_runtime::spawn_blocking(move || {
                platform::widget_probe(value, true)
            })
            .await
            .unwrap_or_else(|_| serde_json::json!({}));
        }
    }
    serde_json::json!({})
}

#[tauri::command(rename = "probe:report")]
pub fn probe_report(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    page: String,
    report: String,
) {
    let path = std::env::var_os("LAZYSWITCH_TAURI_PROBE");
    let should_exit = {
        let Ok(mut runtime) = state.runtime.lock() else {
            return;
        };
        runtime
            .probe_reports
            .insert(page, parse_probe_report(&report));
        let Some(path) = path else { return };
        let Ok(bytes) = serde_json::to_vec_pretty(&runtime.probe_reports) else {
            return;
        };
        if std::fs::write(path, bytes).is_err() {
            return;
        }
        ["manager", "onboarding", "widget", "widget-settings"]
            .iter()
            .all(|name| runtime.probe_reports.contains_key(*name))
    };
    if should_exit {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            app.exit(0);
        });
    }
}
