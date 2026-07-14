use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::core::cli_handover;
use crate::core::cli_sessions::{self, CliRestartResult, CliSession};
use crate::core::platform;
use crate::core::providers::Provider;
use crate::core::switcher;
use crate::core::types::{PUsage, PWindow};

use super::state::{provider_prefs, state_config, AppState};
use super::tray::{tray_lang, tray_text};
use super::windows::{broadcast_changed, emit_to, notify, open_cli_restart, open_switch_warning};

// How much earlier (in remaining-%) the pre-switch warning appears than the
// real auto-switch threshold, and how long "keep going a bit longer" holds
// off re-asking for the same account.
const PRE_WARN_MARGIN_PCT: f64 = 5.0;
const PRE_WARN_SNOOZE_MS: i64 = 20 * 60 * 1000;

pub fn cli_sessions_for(provider: &str) -> Vec<CliSession> {
    cli_sessions::detect_cli_sessions(provider, std::process::id())
}

pub fn cli_result_notification(app: &AppHandle, provider: &str, result: &CliRestartResult) {
    let state = app.state::<AppState>();
    let name = cli_handover::provider_name(provider);
    let mut vars = HashMap::new();
    vars.insert("provider".into(), name.to_owned());
    vars.insert("count".into(), result.restarted.to_string());
    vars.insert("closed".into(), result.closed.to_string());
    vars.insert("manual".into(), result.manual.to_string());
    let key = if result.manual > 0 {
        "notif.cliRestartedManualBody"
    } else {
        "notif.cliRestartedBody"
    };
    notify(
        app,
        format!(
            "{} — {}",
            if provider == "claude" { "Claude Code" } else { "Codex" },
            tray_text(&state, "notif.cliRestartedTitle", &[])
        ),
        crate::core::i18n::t(tray_lang(&state), key, Some(&vars)),
    );
}

pub async fn schedule_cli_handover(
    app: AppHandle,
    provider: String,
    sessions: Vec<CliSession>,
) -> Option<CliRestartResult> {
    if sessions.is_empty() {
        return None;
    }
    let state = app.state::<AppState>();
    let prefs = provider_prefs(&state_config(&state), &provider).ok()?;
    let action = if prefs.auto_restart_cli {
        "restart".to_owned()
    } else {
        let Ok(receiver) = open_cli_restart(&app, &provider, &sessions) else {
            return None;
        };
        receiver.await.unwrap_or_else(|_| "later".into())
    };
    if let Ok(mut runtime) = app.state::<AppState>().runtime.lock() {
        runtime.cli_payload = None;
        runtime.cli_response = None;
    }
    if action == "copy" {
        let command = cli_handover::resume_command(&provider);
        platform::set_clipboard_text(&command.text);
        notify(
            &app,
            "Resume command copied",
            format!(
                "Paste {} in any running {} terminal.",
                command.text,
                cli_handover::provider_name(&provider)
            ),
        );
        return None;
    }
    if action != "restart" {
        return None;
    }
    let command = cli_handover::resume_command(&provider);
    let sessions_for_worker = sessions.clone();
    let command_for_worker = command.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        cli_sessions::default_restart_cli_sessions(&sessions_for_worker, &command_for_worker)
    })
    .await
    .ok()?;
    if result.manual > 0 {
        platform::set_clipboard_text(&command.text);
    }
    cli_result_notification(&app, &provider, &result);
    Some(result)
}

fn window_left_pct(window: Option<&PWindow>) -> Option<f64> {
    window.map(|w| 100.0 - w.used_percent)
}

fn window_hit(window: Option<&PWindow>, min_left_pct: f64) -> bool {
    window_left_pct(window).is_some_and(|left| left <= min_left_pct)
}

// True in the band just before the real auto-switch threshold, so the
// pre-switch warning can fire once with a lead time before it happens.
fn window_nearing(window: Option<&PWindow>, min_left_pct: f64) -> bool {
    window_left_pct(window)
        .is_some_and(|left| left <= min_left_pct + PRE_WARN_MARGIN_PCT && left > min_left_pct)
}

async fn maybe_prompt_pre_switch_warning(app: &AppHandle, provider_id: &str, usage: &PUsage) {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    let Ok(prefs) = provider_prefs(&config, provider_id) else {
        return;
    };
    let Ok(provider) = state.providers.get(provider_id) else {
        return;
    };
    let Some(active) = provider.active_account_name() else {
        return;
    };
    let key = format!("{provider_id}:{active}");
    let now = chrono::Utc::now().timestamp_millis();
    let should_prompt = {
        let Ok(mut runtime) = state.runtime.lock() else {
            return;
        };
        if runtime
            .switch_prompt_shown
            .get(&key)
            .is_some_and(|at| now - at < PRE_WARN_SNOOZE_MS)
        {
            false
        } else {
            runtime.switch_prompt_shown.insert(key.clone(), now);
            true
        }
    };
    if !should_prompt {
        return;
    }
    let cooling = |name: &str| {
        state
            .runtime
            .lock()
            .ok()
            .and_then(|runtime| runtime.cooling_down.get(provider_id)?.get(name).copied())
            .is_some_and(|until| until > chrono::Utc::now().timestamp_millis())
    };
    let Some(next) = switcher::pick_next_account(provider.as_ref(), &prefs, &cooling) else {
        return;
    };
    let from_label = provider
        .list_accounts()
        .into_iter()
        .find(|account| account.name == active)
        .and_then(|account| account.label);
    let (bar_label, used_percent, resets_at) =
        if window_nearing(usage.primary.as_ref(), prefs.primary_min_left_pct) {
            let window = usage.primary.as_ref();
            (
                "Session",
                window.map_or(0.0, |w| w.used_percent),
                window.and_then(|w| w.resets_at),
            )
        } else {
            let window = usage.secondary.as_ref();
            (
                "Weekly",
                window.map_or(0.0, |w| w.used_percent),
                window.and_then(|w| w.resets_at),
            )
        };
    let approved = open_switch_warning(
        app,
        &active,
        from_label.as_deref(),
        &next,
        bar_label,
        used_percent,
        resets_at,
    )
    .await;
    if !approved {
        return;
    }
    let cli_sessions = cli_sessions_for(provider_id);
    match switcher::switch_to(provider.as_ref(), &next.name, &prefs, false).await {
        Ok(result) => {
            let desktop_restarted = if prefs.auto_approve {
                provider.desktop_restart(&prefs).await
            } else {
                false
            };
            let app_for_cli = app.clone();
            let provider_for_cli = provider_id.to_owned();
            tauri::async_runtime::spawn(async move {
                let _ = schedule_cli_handover(app_for_cli, provider_for_cli, cli_sessions).await;
            });
            notify(
                app,
                format!("{provider_id} switched"),
                format!(
                    "{} is now active{}",
                    result.to,
                    if desktop_restarted {
                        " (desktop restarted)"
                    } else {
                        ""
                    }
                ),
            );
            broadcast_changed(app);
        }
        Err(error) => eprintln!("[warn-switch:{provider_id}] switch failed: {error}"),
    }
}

async fn handle_limit(app: AppHandle, provider_id: String, usage: PUsage) {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    let Ok(prefs) = provider_prefs(&config, &provider_id) else {
        return;
    };
    let primary_hit = window_hit(usage.primary.as_ref(), prefs.primary_min_left_pct);
    let secondary_hit = window_hit(usage.secondary.as_ref(), prefs.weekly_min_left_pct);
    if !primary_hit && !secondary_hit {
        if window_nearing(usage.primary.as_ref(), prefs.primary_min_left_pct)
            || window_nearing(usage.secondary.as_ref(), prefs.weekly_min_left_pct)
        {
            maybe_prompt_pre_switch_warning(&app, &provider_id, &usage).await;
        }
        return;
    }

    let provider = match state.providers.get(&provider_id) {
        Ok(provider) => provider,
        Err(_) => return,
    };
    let active = provider.active_account_name();
    if let Some(active) = active.as_deref() {
        let until = usage
            .primary
            .as_ref()
            .and_then(|w| w.resets_at)
            .or_else(|| usage.secondary.as_ref().and_then(|w| w.resets_at));
        if let Ok(mut runtime) = state.runtime.lock() {
            runtime
                .cooling_down
                .entry(provider_id.clone())
                .or_default()
                .insert(
                    active.to_owned(),
                    until.unwrap_or_else(|| {
                        chrono::Utc::now().timestamp_millis() + 5 * 60 * 60 * 1000
                    }),
                );
        }
    }
    let next = {
        let cooling = |name: &str| {
            state
                .runtime
                .lock()
                .ok()
                .and_then(|runtime| runtime.cooling_down.get(&provider_id)?.get(name).copied())
                .is_some_and(|until| until > chrono::Utc::now().timestamp_millis())
        };
        switcher::pick_next_account(provider.as_ref(), &prefs, &cooling)
    };
    let Some(next) = next else { return };
    if let Ok(mut runtime) = state.runtime.lock() {
        if !runtime.switching.insert(provider_id.clone()) {
            return;
        }
    }
    let cli_sessions = cli_sessions_for(&provider_id);
    let result = switcher::switch_to(provider.as_ref(), &next.name, &prefs, false).await;
    if let Ok(mut runtime) = state.runtime.lock() {
        runtime.switching.remove(&provider_id);
    }
    match result {
        Ok(result) => {
            eprintln!(
                "[limit:{provider_id}] switched {:?} -> {}",
                result.from, result.to
            );
            let desktop_restarted = if prefs.auto_approve {
                provider.desktop_restart(&prefs).await
            } else {
                false
            };
            if desktop_restarted {
                eprintln!("[limit:{provider_id}] desktop restarted");
            }
            let app_for_cli = app.clone();
            let provider_for_cli = provider_id.clone();
            tauri::async_runtime::spawn(async move {
                let _ = schedule_cli_handover(app_for_cli, provider_for_cli, cli_sessions).await;
            });
            notify(
                &app,
                format!("{provider_id} switched"),
                format!("{} is now active", result.to),
            );
            broadcast_changed(&app);
        }
        Err(error) => eprintln!("[limit:{provider_id}] switch failed: {error}"),
    }
}

fn start_monitor(app: &AppHandle, provider_id: &'static str, provider: Arc<dyn Provider>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let state = app.state::<AppState>();
            let config = state_config(&state);
            let Ok(prefs) = provider_prefs(&config, provider_id) else {
                return;
            };
            let usage = provider.fetch_usage(None).await;
            if let Some(usage) = usage.clone() {
                if let Ok(mut runtime) = state.runtime.lock() {
                    runtime.last_usage.insert(provider_id.into(), usage.clone());
                }
                if let (Some(active), Some(window)) =
                    (provider.active_account_name(), usage.primary.as_ref())
                {
                    crate::core::usage_history::record_sample(
                        provider_id,
                        &active,
                        window.used_percent,
                        chrono::Utc::now().timestamp_millis(),
                    );
                }
                emit_to(&app, "widget", "accounts:changed", ());
                handle_limit(app.clone(), provider_id.into(), usage).await;
            }
            let seconds = prefs.poll_interval_sec.max(1);
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        }
    });
}

pub fn start_monitors(app: &AppHandle) {
    let state = app.state::<AppState>();
    for (id, _, _, _, provider) in state.providers.all() {
        if provider.list_accounts().is_empty() && !provider.has_live_auth() {
            continue;
        }
        start_monitor(app, id, provider);
    }
}

pub fn install_cli_hooks_on_launch() {
    let Ok(app_exe) = std::env::current_exe() else {
        eprintln!("[hooks] unable to resolve the LazySwitch executable path");
        return;
    };
    let cli = app_exe
        .parent()
        .map(|parent| parent.join("lazyswitch-cli.exe"))
        .unwrap_or_else(|| std::path::PathBuf::from("lazyswitch-cli.exe"));
    if !cli.is_file() {
        eprintln!("[hooks] installed CLI is missing: {}", cli.display());
        return;
    }
    match Command::new(&cli).arg("install-hooks").status() {
        Ok(status) if status.success() => eprintln!("[hooks] installed via {}", cli.display()),
        Ok(status) => eprintln!("[hooks] CLI exited with {status} ({})", cli.display()),
        Err(error) => eprintln!("[hooks] failed to run {}: {error}", cli.display()),
    }
}

#[cfg(windows)]
pub fn start_widget_topmost_timer(app: AppHandle) {
    let should_start = app
        .state::<AppState>()
        .runtime
        .lock()
        .map(|mut runtime| {
            if runtime.widget_topmost_timer_started {
                false
            } else {
                runtime.widget_topmost_timer_started = true;
                true
            }
        })
        .unwrap_or(false);
    if !should_start {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(100));
        let Some(window) = app.get_webview_window("widget") else {
            if let Ok(mut runtime) = app.state::<AppState>().runtime.lock() {
                runtime.widget_topmost_timer_started = false;
            }
            break;
        };
        let state = app.state::<AppState>();
        let config = state_config(&state);
        let paused = state
            .runtime
            .lock()
            .map(|runtime| runtime.widget_context_menu_open)
            .unwrap_or(false);
        if !config.usage_widget.minimized
            || config.usage_widget.compact_position != "taskbar"
            || !config.usage_widget.always_on_top
        {
            if let Ok(mut runtime) = state.runtime.lock() {
                runtime.widget_topmost_timer_started = false;
            }
            break;
        }
        if paused {
            continue;
        }
        if let Ok(hwnd) = window.hwnd() {
            let value = hwnd.0 as isize;
            // Dispatch the Win32 z-order operation on the windowing thread.
            // Direct calls from the timer worker return success but do not
            // reorder WebView2 relative to Shell_TrayWnd on this build.
            let _ = app.run_on_main_thread(move || {
                platform::set_topmost(value, true);
                if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some() {
                    eprintln!("[probe:topmost] reasserted");
                }
            });
        }
    });
}
