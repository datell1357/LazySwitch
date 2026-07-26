use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::oneshot;

use crate::app_notify::AppNotifyPayload;
use crate::app_state::AppState;
use crate::cli_handover::{CliHandoverDeps, CliNotification, CliRestartAction, CliRestartRequest};
use crate::cli_sessions::{restart_cli_sessions, CliRestartResult, CliResumeCommand, CliSession};
use crate::i18n::{resolve_lang, t};
use crate::provider_types::ProviderId;

use super::notify::{show_app_notification, NotifyRuntime};

const LABEL_PREFIX: &str = "cli-restart";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliRestartPayload {
    provider_name: String,
    resume_command: String,
    sessions: Vec<CliSessionPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliSessionPayload {
    pid: i64,
    start_time: Option<String>,
    cwd: Option<String>,
}

#[derive(Default)]
pub struct CliRestartRuntime {
    payloads: HashMap<String, CliRestartPayload>,
    pending: HashMap<String, oneshot::Sender<CliRestartAction>>,
}

pub struct TauriCliHandoverDeps {
    app: AppHandle<Wry>,
}

impl TauriCliHandoverDeps {
    pub const fn new(app: AppHandle<Wry>) -> Self {
        Self { app }
    }
}

impl CliHandoverDeps for TauriCliHandoverDeps {
    fn auto_restart_cli(&self, provider: ProviderId) -> bool {
        let state_handle = self.app.state::<Mutex<AppState>>();
        let Ok(state) = state_handle.lock() else {
            return false;
        };
        match provider {
            ProviderId::Codex => state.cfg.codex.auto_restart_cli,
            ProviderId::Claude => state.cfg.claude.auto_restart_cli,
        }
    }

    fn ask_restart<'a>(
        &'a mut self,
        request: CliRestartRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = CliRestartAction> + 'a>> {
        let payload = CliRestartPayload {
            provider_name: request.provider_name.to_string(),
            resume_command: request.resume_command.to_string(),
            sessions: request
                .sessions
                .iter()
                .map(|session| CliSessionPayload {
                    pid: session.pid,
                    start_time: session.start_time.clone(),
                    cwd: session.cwd.clone(),
                })
                .collect(),
        };
        Box::pin(ask_restart_window(self.app.clone(), payload))
    }

    fn restart<'a>(
        &'a mut self,
        sessions: &'a [CliSession],
        resume: &'a CliResumeCommand,
    ) -> Pin<Box<dyn Future<Output = CliRestartResult> + 'a>> {
        Box::pin(restart_cli_sessions(sessions, resume))
    }

    fn copy_to_clipboard(&mut self, text: &str) {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text);
        }
    }

    fn notify(&mut self, notification: CliNotification) {
        let lang = self
            .app
            .state::<Mutex<AppState>>()
            .lock()
            .map(|state| resolve_lang(&state.cfg.language))
            .unwrap_or(crate::i18n::Lang::En);
        let payload = notification_payload(lang, notification);
        let _ = self
            .app
            .notification()
            .builder()
            .title(&payload.title)
            .body(&payload.body)
            .show();
        let runtime = self.app.state::<Mutex<NotifyRuntime>>();
        let _ = show_app_notification(runtime.inner(), payload);
    }
}

async fn ask_restart_window(app: AppHandle<Wry>, payload: CliRestartPayload) -> CliRestartAction {
    let label = format!("{LABEL_PREFIX}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let lang = app
        .state::<Mutex<AppState>>()
        .lock()
        .map(|state| lang_code(resolve_lang(&state.cfg.language)))
        .unwrap_or("en");
    let url = format!("cli-restart.html?lang={lang}");
    let title = app
        .state::<Mutex<AppState>>()
        .lock()
        .map(|state| t(resolve_lang(&state.cfg.language), "popup.cliTitle", &[]))
        .unwrap_or_else(|_| "CLI sessions still running".to_string());
    let Ok(window) = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .inner_size(520.0, 520.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .always_on_top(true)
        .decorations(false)
        .transparent(true)
        .title(&title)
        .build()
    else {
        return CliRestartAction::Later;
    };

    let (sender, receiver) = oneshot::channel();
    let runtime_state = app.state::<Mutex<CliRestartRuntime>>();
    let Ok(mut runtime) = runtime_state.lock() else {
        let _ = window.close();
        return CliRestartAction::Later;
    };
    runtime.payloads.insert(label.clone(), payload);
    runtime.pending.insert(label.clone(), sender);
    drop(runtime);

    let app_handle = app.clone();
    let event_label = label.clone();
    window.on_window_event(move |event| {
        if !matches!(event, WindowEvent::Destroyed) {
            return;
        }
        if let Ok(mut runtime) = app_handle.state::<Mutex<CliRestartRuntime>>().lock() {
            settle(&mut runtime, &event_label, CliRestartAction::Later);
        }
    });

    receiver.await.unwrap_or(CliRestartAction::Later)
}

#[tauri::command]
pub fn cli_restart_payload(
    window: tauri::WebviewWindow<Wry>,
    runtime: State<'_, Mutex<CliRestartRuntime>>,
) -> Result<Option<serde_json::Value>, String> {
    let runtime = runtime.lock().map_err(|error| error.to_string())?;
    runtime
        .payloads
        .get(window.label())
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn cli_restart_respond(
    window: tauri::WebviewWindow<Wry>,
    action: String,
    runtime: State<'_, Mutex<CliRestartRuntime>>,
) -> Result<(), String> {
    let action = match action.as_str() {
        "restart" => CliRestartAction::Restart,
        "copy" => CliRestartAction::Copy,
        "later" => CliRestartAction::Later,
        _ => CliRestartAction::Later,
    };
    let label = window.label().to_string();
    let mut runtime = runtime.lock().map_err(|error| error.to_string())?;
    settle(&mut runtime, &label, action);
    window.close().map_err(|error| error.to_string())
}

fn settle(runtime: &mut CliRestartRuntime, label: &str, action: CliRestartAction) {
    runtime.payloads.remove(label);
    if let Some(sender) = runtime.pending.remove(label) {
        let _ = sender.send(action);
    }
}

fn notification_payload(
    lang: crate::i18n::Lang,
    notification: CliNotification,
) -> AppNotifyPayload {
    match notification {
        CliNotification::CommandCopied { provider, command } => AppNotifyPayload {
            title: format!(
                "{provider} — {}",
                t(lang, "notif.cliCommandCopiedTitle", &[])
            ),
            body: t(
                lang,
                "notif.cliCommandCopiedBody",
                &[("provider", provider), ("command", &command)],
            ),
        },
        CliNotification::Restarted { provider, result } => {
            let restarted = result.restarted.to_string();
            let closed = result.closed.to_string();
            let manual = result.manual.to_string();
            let key = if result.manual > 0 {
                "notif.cliRestartedManualBody"
            } else {
                "notif.cliRestartedBody"
            };
            AppNotifyPayload {
                title: format!("{provider} — {}", t(lang, "notif.cliRestartedTitle", &[])),
                body: t(
                    lang,
                    key,
                    &[
                        ("provider", provider),
                        ("count", &restarted),
                        ("closed", &closed),
                        ("manual", &manual),
                    ],
                ),
            }
        }
    }
}

const fn lang_code(lang: crate::i18n::Lang) -> &'static str {
    match lang {
        crate::i18n::Lang::Ko => "ko",
        crate::i18n::Lang::En => "en",
        crate::i18n::Lang::Ja => "ja",
        crate::i18n::Lang::Zh => "zh",
    }
}
