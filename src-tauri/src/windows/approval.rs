use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::{
    AppHandle, Manager, Runtime, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry,
};
use tokio::sync::oneshot;

const LABEL_PREFIX: &str = "approval";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub title: String,
    pub lang: String,
    pub provider: String,
    pub from_name: String,
    pub from_label: String,
    pub to_name: String,
    pub to_label: String,
    pub kind: String,
    pub window_label: String,
    pub bar_label: String,
    pub percent: String,
    pub message: String,
    pub reset_at: String,
}

#[derive(Default)]
pub struct ApprovalRuntime {
    pending: HashMap<String, oneshot::Sender<bool>>,
}

pub async fn ask_approval<R: Runtime>(
    app: &AppHandle<R>,
    request: ApprovalRequest,
) -> Result<bool, String> {
    let label = format!("{LABEL_PREFIX}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let url = approval_url(&request);
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
        .inner_size(440.0, 420.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .always_on_top(true)
        .decorations(false)
        .transparent(true)
        .title(&request.title)
        .build()
        .map_err(|error| error.to_string())?;

    let (sender, receiver) = oneshot::channel();
    app.state::<Mutex<ApprovalRuntime>>()
        .lock()
        .map_err(|error| error.to_string())?
        .pending
        .insert(label.clone(), sender);

    let app_handle = app.clone();
    let event_label = label.clone();
    window.on_window_event(move |event| {
        if !matches!(event, WindowEvent::Destroyed) {
            return;
        }
        if let Ok(mut runtime) = app_handle.state::<Mutex<ApprovalRuntime>>().lock() {
            settle(&mut runtime, &event_label, false);
        }
    });

    Ok(receiver.await.unwrap_or(false))
}

#[tauri::command]
pub fn approval_respond(
    window: tauri::WebviewWindow<Wry>,
    approved: bool,
    runtime: State<'_, Mutex<ApprovalRuntime>>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut runtime = runtime.lock().map_err(|error| error.to_string())?;
    settle(&mut runtime, &label, approved);
    window.close().map_err(|error| error.to_string())
}

fn settle(runtime: &mut ApprovalRuntime, label: &str, approved: bool) {
    if let Some(sender) = runtime.pending.remove(label) {
        let _ = sender.send(approved);
    }
}

fn approval_url(request: &ApprovalRequest) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("lang", &request.lang)
        .append_pair("provider", &request.provider)
        .append_pair("fromName", &request.from_name)
        .append_pair("fromLabel", &request.from_label)
        .append_pair("toName", &request.to_name)
        .append_pair("toLabel", &request.to_label)
        .append_pair("kind", &request.kind)
        .append_pair("windowLabel", &request.window_label)
        .append_pair("barLabel", &request.bar_label)
        .append_pair("percent", &request.percent)
        .append_pair("message", &request.message)
        .append_pair("resetAt", &request.reset_at)
        .finish();
    format!("approval.html?{query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_url_preserves_renderer_query_shape() {
        let url = approval_url(&ApprovalRequest {
            title: "Restart?".into(),
            lang: "en".into(),
            provider: "Codex".into(),
            from_name: "old@example.com".into(),
            from_label: "Old".into(),
            to_name: "new@example.com".into(),
            to_label: "New".into(),
            kind: "threshold".into(),
            window_label: "Session".into(),
            bar_label: "5h".into(),
            percent: "95".into(),
            message: String::new(),
            reset_at: "1234".into(),
        });

        assert!(url.starts_with("approval.html?"));
        assert!(url.contains("fromName=old%40example.com"));
        assert!(url.contains("resetAt=1234"));
    }
}
