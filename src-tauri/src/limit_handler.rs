use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_notification::NotificationExt;

use crate::app_notify::AppNotifyPayload;
use crate::app_state::{prune_cooldowns, AppState};
use crate::cli_handover;
use crate::i18n::{resolve_lang, t, Lang};
use crate::monitor::{LimitReason, ThresholdWindow, UsageMonitor};
use crate::provider;
use crate::provider_types::{PAccount, ProviderId};
use crate::switcher;
use crate::windows::approval::{ask_approval, ApprovalRequest};
use crate::windows::cli_restart::TauriCliHandoverDeps;
use crate::windows::notify::{show_app_notification, NotifyRuntime};

const NO_ACCOUNT_NOTIFY_INTERVAL_MS: i64 = 15 * 60 * 1_000;
const ERROR_COOLDOWN_MS: i64 = 5 * 60 * 60 * 1_000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn account_name(account: Option<&PAccount>, fallback: &str) -> String {
    account
        .and_then(|account| account.email.as_deref())
        .unwrap_or(fallback)
        .to_string()
}

fn account_label(account: Option<&PAccount>) -> String {
    account
        .and_then(|account| account.label.clone())
        .unwrap_or_default()
}

fn lang_code(lang: Lang) -> &'static str {
    match lang {
        Lang::Ko => "ko",
        Lang::En => "en",
        Lang::Ja => "ja",
        Lang::Zh => "zh",
    }
}

fn notify(app: &AppHandle<Wry>, payload: AppNotifyPayload) {
    let _ = app
        .notification()
        .builder()
        .title(&payload.title)
        .body(&payload.body)
        .show();
    let runtime = app.state::<Mutex<NotifyRuntime>>();
    let _ = show_app_notification(runtime.inner(), payload);
}

async fn schedule_cli(
    app: AppHandle<Wry>,
    provider_id: ProviderId,
    sessions: Vec<crate::cli_sessions::CliSession>,
) {
    let _ = tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(async move {
            let mut deps = TauriCliHandoverDeps::new(app);
            cli_handover::schedule(provider_id, &sessions, &mut deps).await;
        });
    })
    .await;
}

pub fn broadcast_changed(app: &AppHandle<Wry>) {
    for label in ["manager", "usage-widget", "widget-settings"] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.emit("accounts:changed", ());
        }
    }
    let _ = crate::windows::widget::sync_usage_widget(app);
    let _ = crate::tray::refresh_tray(app);
}

fn monitor_callbacks(app: &AppHandle<Wry>, provider_id: ProviderId, monitor: &mut UsageMonitor) {
    let prefs_app = app.clone();
    let usage_app = app.clone();
    let limit_app = app.clone();
    monitor.start(
        provider_id,
        move || {
            prefs_app
                .state::<Mutex<AppState>>()
                .lock()
                .map(|state| state.prefs_of(provider_id).clone())
                .unwrap_or_else(|_| {
                    let defaults = crate::config::AppConfig::default();
                    match provider_id {
                        ProviderId::Codex => defaults.codex,
                        ProviderId::Claude => defaults.claude,
                    }
                })
        },
        move |usage| {
            if let Ok(mut state) = usage_app.state::<Mutex<AppState>>().lock() {
                state.state_of_mut(provider_id).last_usage = Some(usage);
            }
            let _ = crate::tray::refresh_tray(&usage_app);
        },
        move |reason| {
            let app = limit_app.clone();
            tauri::async_runtime::spawn(async move {
                on_limit_hit(app, provider_id, reason).await;
            });
        },
    );
}

pub fn wire_monitors(app: &AppHandle<Wry>) -> Result<(), String> {
    for provider_id in ProviderId::ALL {
        if provider::list_accounts(provider_id).is_empty() && !provider::has_live_auth(provider_id)
        {
            continue;
        }
        let mut monitor = UsageMonitor::new();
        monitor_callbacks(app, provider_id, &mut monitor);
        app.state::<Mutex<AppState>>()
            .lock()
            .map_err(|error| error.to_string())?
            .state_of_mut(provider_id)
            .monitor = Some(monitor);
    }
    Ok(())
}

pub fn restart_monitors(app: &AppHandle<Wry>) -> Result<(), String> {
    for provider_id in ProviderId::ALL {
        let monitor = app
            .state::<Mutex<AppState>>()
            .lock()
            .map_err(|error| error.to_string())?
            .state_of_mut(provider_id)
            .monitor
            .take();
        if let Some(mut monitor) = monitor {
            monitor.stop();
            monitor_callbacks(app, provider_id, &mut monitor);
            app.state::<Mutex<AppState>>()
                .lock()
                .map_err(|error| error.to_string())?
                .state_of_mut(provider_id)
                .monitor = Some(monitor);
        }
    }
    Ok(())
}

pub async fn manual_switch(
    app: AppHandle<Wry>,
    provider_id: ProviderId,
    name: String,
) -> Result<(), String> {
    let prefs = {
        let state_handle = app.state::<Mutex<AppState>>();
        let mut state = state_handle.lock().map_err(|error| error.to_string())?;
        if state.state_of(provider_id).switching {
            return Ok(());
        }
        state.state_of_mut(provider_id).switching = true;
        state.prefs_of(provider_id).clone()
    };
    let sessions = cli_handover::detect(provider_id, i64::from(std::process::id())).await;
    let result = switcher::switch_to(provider_id, &name, &prefs, false).await;
    if let Ok(mut state) = app.state::<Mutex<AppState>>().lock() {
        state.state_of_mut(provider_id).switching = false;
    }
    let switch_result = result?;
    let desktop_restarted = provider::desktop_restart(provider_id, &prefs)
        .await
        .unwrap_or(false);
    let accounts = provider::list_accounts(provider_id);
    let from = switch_result
        .from
        .as_deref()
        .and_then(|slot| accounts.iter().find(|account| account.name == slot));
    let to = accounts
        .iter()
        .find(|account| account.name == switch_result.to);
    let lang = app
        .state::<Mutex<AppState>>()
        .lock()
        .map(|state| resolve_lang(&state.cfg.language))
        .unwrap_or(Lang::En);
    let restarted = if desktop_restarted {
        t(lang, "notif.restartedSuffix", &[])
    } else {
        String::new()
    };
    notify(
        &app,
        AppNotifyPayload {
            title: format!(
                "{} — {}",
                provider_id.display_name(),
                t(lang, "notif.switchedTitle", &[])
            ),
            body: t(
                lang,
                "notif.manualSwitched",
                &[
                    ("from", &account_name(from, "?")),
                    ("to", &account_name(to, &name)),
                    ("restarted", &restarted),
                ],
            ),
        },
    );
    schedule_cli(app.clone(), provider_id, sessions).await;
    broadcast_changed(&app);
    Ok(())
}

async fn on_limit_hit(app: AppHandle<Wry>, provider_id: ProviderId, reason: LimitReason) {
    let should_handle = app
        .state::<Mutex<AppState>>()
        .lock()
        .map(|mut state| {
            let provider_state = state.state_of_mut(provider_id);
            if provider_state.switching || provider_state.handling_limit {
                false
            } else {
                provider_state.handling_limit = true;
                true
            }
        })
        .unwrap_or(false);
    if !should_handle {
        return;
    }
    handle_limit(app.clone(), provider_id, reason).await;
    if let Ok(mut state) = app.state::<Mutex<AppState>>().lock() {
        state.state_of_mut(provider_id).handling_limit = false;
    }
}

pub async fn handle_limit(app: AppHandle<Wry>, provider_id: ProviderId, reason: LimitReason) {
    let now = now_ms();
    let active = provider::active_account_name(provider_id);
    let (prefs, mut cooling_down) = {
        let state_handle = app.state::<Mutex<AppState>>();
        let Ok(mut state) = state_handle.lock() else {
            return;
        };
        let prefs = state.prefs_of(provider_id).clone();
        let provider_state = state.state_of_mut(provider_id);
        prune_cooldowns(provider_state, now);
        if let Some(active) = active.as_ref() {
            let until = match &reason {
                LimitReason::Threshold { window, .. } => {
                    let usage = provider_state.last_usage.as_ref();
                    match window {
                        ThresholdWindow::Primary => usage.and_then(|usage| usage.primary),
                        ThresholdWindow::Secondary => usage.and_then(|usage| usage.secondary),
                    }
                    .and_then(|window| window.resets_at)
                }
                LimitReason::Error { .. } => Some(now + ERROR_COOLDOWN_MS),
            };
            if let Some(until) = until {
                provider_state.cooling_down.insert(active.clone(), until);
            }
        }
        (
            prefs,
            provider_state
                .cooling_down
                .keys()
                .cloned()
                .collect::<HashSet<_>>(),
        )
    };

    let mut next = switcher::pick_next_account(provider_id, &prefs, &cooling_down, now);
    while let Some(candidate) = next.clone() {
        let usage = provider::fetch_usage(provider_id, Some(&candidate.name)).await;
        if !switcher::is_exhausted(usage.as_ref(), &prefs, now) {
            break;
        }
        let until = switcher::exhausted_until(usage.as_ref(), &prefs, now)
            .unwrap_or(now + NO_ACCOUNT_NOTIFY_INTERVAL_MS);
        cooling_down.insert(candidate.name.clone());
        if let Ok(mut state) = app.state::<Mutex<AppState>>().lock() {
            state
                .state_of_mut(provider_id)
                .cooling_down
                .insert(candidate.name, until);
        }
        next = switcher::pick_next_account(provider_id, &prefs, &cooling_down, now);
    }

    let Some(next) = next else {
        notify_no_account(&app, provider_id, now);
        return;
    };
    if let Ok(mut state) = app.state::<Mutex<AppState>>().lock() {
        state.state_of_mut(provider_id).last_no_account_notify = 0;
        if state.state_of(provider_id).switching {
            return;
        }
        state.state_of_mut(provider_id).switching = true;
    }
    let accounts = provider::list_accounts(provider_id);
    let from = active
        .as_deref()
        .and_then(|slot| accounts.iter().find(|account| account.name == slot))
        .cloned();
    let sessions = cli_handover::detect(provider_id, i64::from(std::process::id())).await;
    let switched = switcher::switch_to(provider_id, &next.name, &prefs, false).await;
    if let Ok(mut state) = app.state::<Mutex<AppState>>().lock() {
        state.state_of_mut(provider_id).switching = false;
    }
    if let Err(error) = switched {
        notify_switch_failed(&app, provider_id, &error);
        broadcast_changed(&app);
        return;
    }
    notify_switched(&app, provider_id, from.as_ref(), &next);
    broadcast_changed(&app);

    if provider_id == ProviderId::Codex {
        let approved = prefs.auto_approve
            || ask_approval(
                &app,
                approval_request(&app, provider_id, from.as_ref(), &next, &reason),
            )
            .await
            .unwrap_or(false);
        if approved {
            let restarted = provider::desktop_restart(provider_id, &prefs)
                .await
                .unwrap_or(false);
            notify_desktop_result(&app, provider_id, &next, restarted);
        }
    }
    schedule_cli(app.clone(), provider_id, sessions).await;
}

fn notify_no_account(app: &AppHandle<Wry>, provider_id: ProviderId, now: i64) {
    let should_notify = app
        .state::<Mutex<AppState>>()
        .lock()
        .map(|mut state| {
            let provider_state = state.state_of_mut(provider_id);
            if now - provider_state.last_no_account_notify <= NO_ACCOUNT_NOTIFY_INTERVAL_MS {
                false
            } else {
                provider_state.last_no_account_notify = now;
                true
            }
        })
        .unwrap_or(false);
    if !should_notify {
        return;
    }
    let lang = app_lang(app);
    notify(
        app,
        AppNotifyPayload {
            title: format!(
                "{} — {}",
                provider_id.display_name(),
                t(lang, "notif.noAccountTitle", &[])
            ),
            body: t(lang, "notif.noAccountBody", &[]),
        },
    );
}

fn notify_switch_failed(app: &AppHandle<Wry>, provider_id: ProviderId, error: &str) {
    let lang = app_lang(app);
    notify(
        app,
        AppNotifyPayload {
            title: format!(
                "{} — {}",
                provider_id.display_name(),
                t(lang, "notif.switchFailed", &[])
            ),
            body: error.to_string(),
        },
    );
}

fn notify_switched(
    app: &AppHandle<Wry>,
    provider_id: ProviderId,
    from: Option<&PAccount>,
    to: &PAccount,
) {
    let lang = app_lang(app);
    let from_name = account_name(from, "?");
    let to_name = account_name(Some(to), &to.name);
    notify(
        app,
        AppNotifyPayload {
            title: format!(
                "{} — {}",
                provider_id.display_name(),
                t(lang, "notif.switchedTitle", &[])
            ),
            body: t(
                lang,
                "notif.switchedBody",
                &[("from", &from_name), ("to", &to_name)],
            ),
        },
    );
}

fn notify_desktop_result(
    app: &AppHandle<Wry>,
    provider_id: ProviderId,
    account: &PAccount,
    restarted: bool,
) {
    let lang = app_lang(app);
    let title_key = if restarted {
        "notif.desktopRestarted"
    } else {
        "notif.desktopFailed"
    };
    let body = if restarted {
        t(
            lang,
            "notif.desktopRestartedBody",
            &[("name", &account_name(Some(account), &account.name))],
        )
    } else {
        t(lang, "notif.desktopFailedBody", &[])
    };
    notify(
        app,
        AppNotifyPayload {
            title: format!(
                "{} — {}",
                provider_id.display_name(),
                t(lang, title_key, &[])
            ),
            body,
        },
    );
}

fn app_lang(app: &AppHandle<Wry>) -> Lang {
    app.state::<Mutex<AppState>>()
        .lock()
        .map(|state| resolve_lang(&state.cfg.language))
        .unwrap_or(Lang::En)
}

fn approval_request(
    app: &AppHandle<Wry>,
    provider_id: ProviderId,
    from: Option<&PAccount>,
    to: &PAccount,
    reason: &LimitReason,
) -> ApprovalRequest {
    let lang = app_lang(app);
    let last_usage = app
        .state::<Mutex<AppState>>()
        .lock()
        .ok()
        .and_then(|state| state.state_of(provider_id).last_usage.clone());
    let window = match reason {
        LimitReason::Threshold {
            window: ThresholdWindow::Primary,
            ..
        } => last_usage.and_then(|usage| usage.primary),
        LimitReason::Threshold {
            window: ThresholdWindow::Secondary,
            ..
        } => last_usage.and_then(|usage| usage.secondary),
        LimitReason::Error { .. } => None,
    };
    let kind = match reason {
        LimitReason::Threshold { .. } => "threshold",
        LimitReason::Error { .. } => "error",
    };
    let percent = match reason {
        LimitReason::Threshold { percent, .. } => percent.to_string(),
        LimitReason::Error { .. } => String::new(),
    };
    let message = match reason {
        LimitReason::Threshold { .. } => String::new(),
        LimitReason::Error { message } => message.clone(),
    };
    let window_name = window
        .and_then(|window| window.window_minutes)
        .map(|minutes| format!("{}h", minutes / 60))
        .unwrap_or_default();
    ApprovalRequest {
        title: t(lang, "popup.title", &[]),
        lang: lang_code(lang).to_string(),
        provider: provider_id.display_name().to_string(),
        from_name: account_name(from, ""),
        from_label: account_label(from),
        to_name: account_name(Some(to), &to.name),
        to_label: account_label(Some(to)),
        kind: kind.to_string(),
        window_label: if kind == "error" {
            t(lang, "popup.limitReached", &[])
        } else {
            t(lang, "win.limit", &[("w", &window_name)])
        },
        bar_label: window_name,
        percent,
        message,
        reset_at: window
            .and_then(|window| window.resets_at)
            .map(|value| value.to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::ProviderState;

    #[test]
    fn cooldown_constants_match_original_windows() {
        assert_eq!(NO_ACCOUNT_NOTIFY_INTERVAL_MS, 900_000);
        assert_eq!(ERROR_COOLDOWN_MS, 18_000_000);
    }

    #[test]
    fn expired_cooldowns_are_pruned_before_candidate_selection() {
        let mut state = ProviderState::default();
        state.cooling_down.insert("expired".into(), 99);
        state.cooling_down.insert("active".into(), 101);

        prune_cooldowns(&mut state, 100);

        assert_eq!(state.cooling_down.len(), 1);
        assert_eq!(state.cooling_down.get("active"), Some(&101));
    }
}
