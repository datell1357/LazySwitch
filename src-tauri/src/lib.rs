#![allow(linker_messages)]

pub mod core;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu, CheckMenuItem, ContextMenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::core::config::{self, AppConfig};
use crate::core::providers::claude::ClaudeProvider;
use crate::core::providers::codex::CodexProvider;
use crate::core::providers::{Provider, ReqwestTransport};
use crate::core::switcher;
use crate::core::types::{LoginFlowResult, PAccount, PUsage, ProviderPrefs};
use crate::core::platform;

const SHIM: &str = include_str!("../../src/renderer/tauri-shim.js");
const DEFAULT_WIDGET_BACKGROUND: (u8, u8, u8, u8) = (0x16, 0x17, 0x1b, 0xff);
const WIDGET_COMPACT_WIDTH: f64 = 280.0;
const WIDGET_COMPACT_DEFAULT_HEIGHT: f64 = 70.0;
const WIDGET_COMPACT_MIN_HEIGHT: f64 = 38.0;
const TOAST_WIDTH: f64 = 360.0;
const TOAST_DEFAULT_HEIGHT: f64 = 112.0;
const TOAST_MIN_HEIGHT: f64 = 84.0;
const TOAST_MAX_HEIGHT: f64 = 160.0;
const TOAST_MARGIN: i32 = 18;
const TOAST_GAP: i32 = 10;

#[derive(Clone)]
struct ProviderSet {
    codex: Arc<CodexProvider>,
    claude: Arc<ClaudeProvider>,
}

type ProviderEntry = (&'static str, &'static str, bool, bool, Arc<dyn Provider>);

impl ProviderSet {
    fn get(&self, id: &str) -> Result<Arc<dyn Provider>, String> {
        match id {
            "codex" => Ok(self.codex.clone()),
            "claude" => Ok(self.claude.clone()),
            other => Err(format!("Unknown provider \"{other}\"")),
        }
    }

    fn all(&self) -> [ProviderEntry; 2] {
        [
            ("codex", "Codex", true, true, self.codex.clone()),
            ("claude", "Claude Code", true, false, self.claude.clone()),
        ]
    }
}

#[derive(Default)]
struct RuntimeData {
    cooling_down: HashMap<String, HashMap<String, i64>>,
    last_usage: HashMap<String, PUsage>,
    pending_refreshes: HashSet<String>,
    switching: HashSet<String>,
    notifications: VecDeque<ToastPayload>,
    active_toasts: Vec<String>,
    toast_payloads: HashMap<String, ToastPayload>,
    toast_heights: HashMap<String, f64>,
    next_toast_id: u64,
    approval: Option<tokio::sync::oneshot::Sender<bool>>,
    cli_payload: Option<Value>,
    probe_reports: HashMap<String, Value>,
    widget_context_menu_open: bool,
    widget_topmost_timer_started: bool,
}

struct AppState {
    config: Mutex<AppConfig>,
    providers: ProviderSet,
    runtime: Mutex<RuntimeData>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInfo {
    id: &'static str,
    display_name: &'static str,
    has_login_flow: bool,
    has_desktop: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrichedAccount {
    #[serde(flatten)]
    account: PAccount,
    active: bool,
    cooling_down_until: Option<i64>,
    usage: Option<PUsage>,
}

#[derive(Clone, Debug, Serialize)]
struct OperationResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ToastPayload {
    title: String,
    body: String,
}

fn state_config(state: &AppState) -> AppConfig {
    state
        .config
        .lock()
        .map(|config| config.clone())
        .unwrap_or_else(|_| config::defaults())
}

fn emit_to(app: &AppHandle, label: &str, event: &str, payload: impl Serialize + Clone) {
    if let Err(error) = app.emit_to(label, event, payload) {
        eprintln!("[tauri:event:{event}] {error}");
    }
}

fn broadcast_changed(app: &AppHandle) {
    for label in ["manager", "widget", "widget-settings"] {
        if app.get_webview_window(label).is_some() {
            emit_to(app, label, "accounts:changed", ());
        }
    }
    sync_usage_widget(app);
    refresh_tray_tooltip(app);
}

fn provider_info(state: &AppState) -> Vec<ProviderInfo> {
    state
        .providers
        .all()
        .into_iter()
        .map(
            |(id, display_name, has_login_flow, has_desktop, _)| ProviderInfo {
                id,
                display_name,
                has_login_flow,
                has_desktop,
            },
        )
        .collect()
}

fn provider_prefs(config: &AppConfig, provider: &str) -> Result<ProviderPrefs, String> {
    match provider {
        "codex" => Ok(config.codex.clone()),
        "claude" => Ok(config.claude.clone()),
        other => Err(format!("Unknown provider \"{other}\"")),
    }
}

fn prune_cooldowns(state: &AppState, provider: &str) {
    let now = chrono::Utc::now().timestamp_millis();
    if let Ok(mut runtime) = state.runtime.lock() {
        if let Some(cooling) = runtime.cooling_down.get_mut(provider) {
            cooling.retain(|_, until| *until > now);
        }
    }
}

fn cooling_until(state: &AppState, provider: &str, name: &str) -> Option<i64> {
    state
        .runtime
        .lock()
        .ok()
        .and_then(|runtime| runtime.cooling_down.get(provider)?.get(name).copied())
}

fn start_usage_refresh(
    app: &AppHandle,
    provider_id: &str,
    provider: Arc<dyn Provider>,
    name: Option<String>,
    cached: Option<PUsage>,
) {
    let state = app.state::<AppState>();
    let key = format!("{}:{}", provider_id, name.as_deref().unwrap_or("live"));
    let should_start = state
        .runtime
        .lock()
        .map(|mut runtime| runtime.pending_refreshes.insert(key.clone()))
        .unwrap_or(false);
    if !should_start {
        return;
    }
    let app = app.clone();
    let provider_id = provider_id.to_owned();
    tauri::async_runtime::spawn(async move {
        let latest = provider.fetch_usage(name.as_deref()).await;
        let changed = latest != cached;
        if let Ok(mut runtime) = app.state::<AppState>().runtime.lock() {
            runtime.pending_refreshes.remove(&key);
        }
        if changed {
            broadcast_changed(&app);
        }
        let _ = provider_id;
    });
}

fn list_accounts(
    app: &AppHandle,
    state: &AppState,
    provider_id: &str,
) -> Result<Vec<EnrichedAccount>, String> {
    prune_cooldowns(state, provider_id);
    let provider = state.providers.get(provider_id)?;
    let active = provider.active_account_name();
    let accounts = provider.list_accounts();
    let mut rows = Vec::with_capacity(accounts.len());
    for account in accounts {
        let usage_name = if active.as_deref() == Some(account.name.as_str()) {
            None
        } else {
            Some(account.name.clone())
        };
        let cached = provider.cached_usage(usage_name.as_deref());
        start_usage_refresh(
            app,
            provider_id,
            provider.clone(),
            usage_name,
            cached.clone(),
        );
        rows.push(EnrichedAccount {
            active: active.as_deref() == Some(account.name.as_str()),
            cooling_down_until: cooling_until(state, provider_id, &account.name),
            account,
            usage: cached,
        });
    }
    Ok(rows)
}

fn set_window_bounds(window: &WebviewWindow, x: f64, y: f64, width: f64, height: f64) {
    let _ = window.set_size(tauri::PhysicalSize::new(
        width.max(1.0) as u32,
        height.max(1.0) as u32,
    ));
    let _ = window.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
}

fn widget_bounds(config: &AppConfig) -> (f64, f64, f64, f64) {
    let widget = &config.usage_widget;
    if widget.minimized {
        let x = widget.x.unwrap_or(0.0);
        let y = widget.y.unwrap_or(0.0);
        (
            x,
            y,
            WIDGET_COMPACT_WIDTH,
            WIDGET_COMPACT_DEFAULT_HEIGHT.max(WIDGET_COMPACT_MIN_HEIGHT),
        )
    } else {
        (
            widget.x.unwrap_or(0.0),
            widget.y.unwrap_or(0.0),
            widget.width,
            widget.height,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn create_window(
    app: &AppHandle,
    label: &str,
    file: &str,
    title: &str,
    width: f64,
    height: f64,
    decorations: bool,
    always_on_top: bool,
    transparent: bool,
) -> Result<WebviewWindow, tauri::Error> {
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App(file.into()))
        .title(title)
        .inner_size(width, height)
        .decorations(decorations)
        .always_on_top(always_on_top)
        .transparent(transparent)
        .background_color(tauri::window::Color(
            DEFAULT_WIDGET_BACKGROUND.0,
            DEFAULT_WIDGET_BACKGROUND.1,
            DEFAULT_WIDGET_BACKGROUND.2,
            if transparent { 0 } else { DEFAULT_WIDGET_BACKGROUND.3 },
        ))
        .initialization_script(if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some() {
            format!("{}\n{}", SHIM, probe_script())
        } else {
            SHIM.to_owned()
        })
        .build()?;
    Ok(window)
}

fn open_manager(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("manager") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Err(error) = create_window(
        app,
        "manager",
        "manager.html",
        "Accounts",
        920.0,
        730.0,
        false,
        false,
        false,
    ) { eprintln!("[tauri:manager] {error}"); }
}

fn open_onboarding(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("onboarding") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Ok(window) = create_window(
        app,
        "onboarding",
        "onboarding.html",
        "LazySwitch",
        640.0,
        560.0,
        false,
        false,
        false,
    ) {
        let app_for_event = app.clone();
        window.on_window_event(move |event| {
            if matches!(event, WindowEvent::Destroyed) {
                sync_usage_widget(&app_for_event);
            }
        });
    }
}

fn has_enrolled_accounts(state: &AppState) -> bool {
    state
        .providers
        .all()
        .into_iter()
        .any(|(_, _, _, _, provider)| !provider.list_accounts().is_empty())
}

fn open_widget(app: &AppHandle, force: bool) {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    if !force
        && (!config.usage_widget.enabled
            || !has_enrolled_accounts(&state)
            || app.get_webview_window("onboarding").is_some())
    {
        return;
    }
    if let Some(window) = app.get_webview_window("widget") {
        let _ = window.show();
        let _ = window.set_always_on_top(config.usage_widget.always_on_top);
        apply_widget_platform(app, &window, &config);
        return;
    }
    let (x, y, width, height) = widget_bounds(&config);
    match create_window(
        app,
        "widget",
        "widget.html",
        "LazySwitch Usage",
        width,
        height,
        false,
        config.usage_widget.always_on_top,
        config.usage_widget.minimized && config.usage_widget.compact_position == "taskbar",
    ) {
        Ok(window) => {
            let app_for_theme = app.clone();
            window.on_window_event(move |event| {
                if matches!(event, WindowEvent::ThemeChanged(_)) {
                    let state = app_for_theme.state::<AppState>();
                    let config = state_config(&state);
                    if let Some(widget) = app_for_theme.get_webview_window("widget") { emit_widget_taskbar_theme(&app_for_theme, &widget, &config); }
                }
            });
            if config.usage_widget.minimized
                && config.usage_widget.x.is_none()
                && config.usage_widget.y.is_none()
            {
                if let Ok(Some(monitor)) = window.current_monitor() {
                    let monitor_pos = monitor.position();
                    let monitor_size = monitor.size();
                    let compact_x = if config.usage_widget.compact_position == "bottom-left" {
                        monitor_pos.x as f64
                    } else {
                        monitor_pos.x as f64 + monitor_size.width as f64 - width
                    };
                    let compact_y = monitor_pos.y as f64 + monitor_size.height as f64 - height;
                    set_window_bounds(&window, compact_x, compact_y, width, height);
                }
            } else if config.usage_widget.x.is_some() || config.usage_widget.y.is_some() {
                set_window_bounds(&window, x, y, width, height);
            }
            apply_widget_platform(app, &window, &config);
            emit_widget_taskbar_theme(app, &window, &config);
        }
        Err(error) => eprintln!("[tauri:widget] {error}"),
    }
}

fn emit_widget_taskbar_theme(app: &AppHandle, _window: &WebviewWindow, config: &AppConfig) {
    if config.usage_widget.minimized && config.usage_widget.compact_position == "taskbar" {
        emit_to(app, "widget", "widget:taskbar-theme", platform::taskbar_theme().map(|light| serde_json::json!({"light": light})).unwrap_or_else(|| serde_json::json!({"light": false})));
    } else {
        emit_to(app, "widget", "widget:taskbar-theme", Value::Null);
    }
}

fn apply_widget_platform(app: &AppHandle, window: &WebviewWindow, config: &AppConfig) {
    let compact = config.usage_widget.minimized;
    let _ = window.set_resizable(!compact);
    #[cfg(windows)] {
        if let Ok(hwnd) = window.hwnd() {
            let scale = window.scale_factor().unwrap_or(1.0);
            let size = window.outer_size().unwrap_or(tauri::PhysicalSize::new(
                (WIDGET_COMPACT_WIDTH * scale).round() as u32,
                (WIDGET_COMPACT_DEFAULT_HEIGHT * scale).round() as u32,
            ));
            let width = size.width as i32;
            let height = size.height as i32;
            let hwnd_value = hwnd.0 as isize;
            if compact {
                if let Some(bounds) = platform::compact_bounds(hwnd_value, width, height, &config.usage_widget.compact_position) {
                    platform::set_bounds(hwnd_value, bounds, config.usage_widget.always_on_top);
                }
                if config.usage_widget.compact_position == "taskbar" && config.usage_widget.always_on_top {
                    start_widget_topmost_timer(app.clone());
                }
            } else if let (Some(x), Some(y)) = (config.usage_widget.x, config.usage_widget.y) {
                let bounds = platform::Rect { left: x.round() as i32, top: y.round() as i32, right: (x + config.usage_widget.width).round() as i32, bottom: (y + config.usage_widget.height).round() as i32 };
                platform::set_bounds(hwnd_value, bounds, config.usage_widget.always_on_top);
            }
            let should_layer = compact && config.usage_widget.compact_position == "taskbar";
            platform::set_layered(hwnd_value, should_layer);
            if platform::is_layered(hwnd_value) != should_layer {
                eprintln!("[widget] transparency style did not match requested state");
            }
        }
    }
}

#[cfg(windows)]
fn start_widget_topmost_timer(app: AppHandle) {
    let should_start = app
        .state::<AppState>()
        .runtime
        .lock()
        .map(|mut runtime| {
            if runtime.widget_topmost_timer_started { false } else {
                runtime.widget_topmost_timer_started = true;
                true
            }
        })
        .unwrap_or(false);
    if !should_start { return; }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(100));
        let Some(window) = app.get_webview_window("widget") else {
            if let Ok(mut runtime) = app.state::<AppState>().runtime.lock() { runtime.widget_topmost_timer_started = false; }
            break
        };
        let state = app.state::<AppState>();
        let config = state_config(&state);
        let paused = state.runtime.lock().map(|runtime| runtime.widget_context_menu_open).unwrap_or(false);
        if !config.usage_widget.minimized || config.usage_widget.compact_position != "taskbar" || !config.usage_widget.always_on_top {
            if let Ok(mut runtime) = state.runtime.lock() { runtime.widget_topmost_timer_started = false; }
            break;
        }
        if paused { continue; }
        if let Ok(hwnd) = window.hwnd() {
            let value = hwnd.0 as isize;
            // Dispatch the Win32 z-order operation on the windowing thread.
            // Direct calls from the timer worker return success but do not
            // reorder WebView2 relative to Shell_TrayWnd on this build.
            let _ = app.run_on_main_thread(move || {
                platform::set_topmost(value, true);
                if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some() { eprintln!("[probe:topmost] reasserted"); }
            });
        }
    });
}

fn open_widget_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("widget-settings") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Err(error) = create_window(
        app,
        "widget-settings",
        "widget-settings.html",
        "Widget settings",
        320.0,
        400.0,
        false,
        true,
        false,
    ) { eprintln!("[tauri:widget-settings] {error}"); }
}

fn sync_usage_widget(app: &AppHandle) {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    let onboarding = app.get_webview_window("onboarding").is_some();
    if config.usage_widget.enabled && has_enrolled_accounts(&state) && !onboarding {
        open_widget(app, false);
    } else if let Some(window) = app.get_webview_window("widget") {
        let _ = window.close();
    }
}

fn close_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.close();
    }
}

fn clamp_toasts(state: &AppState, app: &AppHandle) {
    let anchor = app
        .get_webview_window("manager")
        .or_else(|| app.get_webview_window("widget"))
        .or_else(|| app.get_webview_window("onboarding"))
        .or_else(|| {
            state
                .runtime
                .lock()
                .ok()
                .and_then(|runtime| runtime.active_toasts.first().cloned())
                .and_then(|label| app.get_webview_window(&label))
        });
    let Some(window) = anchor else {
        // A manager is not needed for positioning, but keeping this branch
        // makes the no-display fallback harmless on headless test machines.
        return;
    };
    let Ok(monitor) = window.current_monitor() else {
        return;
    };
    let Some(monitor) = monitor else { return };
    let position = monitor.position();
    let size = monitor.size();
    let x = position.x + size.width as i32 - TOAST_WIDTH as i32 - TOAST_MARGIN;
    let mut y = position.y + size.height as i32 - TOAST_MARGIN;
    let labels = state
        .runtime
        .lock()
        .map(|runtime| runtime.active_toasts.clone())
        .unwrap_or_default();
    for label in labels {
        let height = state
            .runtime
            .lock()
            .ok()
            .and_then(|runtime| runtime.toast_heights.get(&label).copied())
            .unwrap_or(TOAST_DEFAULT_HEIGHT);
        y -= height as i32;
        if let Some(toast) = app.get_webview_window(&label) {
            let _ = toast.set_position(tauri::PhysicalPosition::new(x, y));
            let _ = toast.set_size(tauri::PhysicalSize::new(TOAST_WIDTH as u32, height as u32));
        }
        y -= TOAST_GAP;
    }
}

fn drain_notifications(app: &AppHandle) {
    loop {
        let payload = {
            let state = app.state::<AppState>();
            let Ok(mut runtime) = state.runtime.lock() else {
                return;
            };
            if runtime.active_toasts.len() >= 4 {
                return;
            }
            runtime.notifications.pop_front()
        };
        let Some(payload) = payload else { return };
        show_notification_now(app, payload);
    }
}

fn show_notification_now(app: &AppHandle, payload: ToastPayload) {
    let state = app.state::<AppState>();
    let label = {
        let Ok(mut runtime) = state.runtime.lock() else {
            return;
        };
        runtime.next_toast_id += 1;
        format!("notify-{}", runtime.next_toast_id)
    };
    let Ok(window) = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("notify.html".into()))
        .title(&payload.title)
        .inner_size(TOAST_WIDTH, TOAST_DEFAULT_HEIGHT)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .background_color(tauri::window::Color(0, 0, 0, 0))
        .initialization_script(SHIM)
        .build()
    else {
        return;
    };
    if let Ok(mut runtime) = state.runtime.lock() {
        runtime.active_toasts.push(label.clone());
        runtime.toast_payloads.insert(label.clone(), payload);
        runtime
            .toast_heights
            .insert(label.clone(), TOAST_DEFAULT_HEIGHT);
    }
    let app_for_event = app.clone();
    let label_for_event = label.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            let state = app_for_event.state::<AppState>();
            if let Ok(mut runtime) = state.runtime.lock() {
                runtime
                    .active_toasts
                    .retain(|item| item != &label_for_event);
                runtime.toast_payloads.remove(&label_for_event);
                runtime.toast_heights.remove(&label_for_event);
            }
            clamp_toasts(&state, &app_for_event);
            drain_notifications(&app_for_event);
        }
    });
    clamp_toasts(&state, app);
    let _ = window.show();
}

fn notify(app: &AppHandle, title: impl Into<String>, body: impl Into<String>) {
    let state = app.state::<AppState>();
    let payload = ToastPayload {
        title: title.into(),
        body: body.into(),
    };
    let show_now = state
        .runtime
        .lock()
        .map(|runtime| runtime.active_toasts.len() < 4)
        .unwrap_or(false);
    if show_now {
        show_notification_now(app, payload);
    } else if let Ok(mut runtime) = state.runtime.lock() {
        runtime.notifications.push_back(payload);
    }
}

async fn handle_limit(app: AppHandle, provider_id: String, usage: PUsage) {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    let Ok(prefs) = provider_prefs(&config, &provider_id) else {
        return;
    };
    let primary_hit = usage
        .primary
        .as_ref()
        .is_some_and(|window| 100.0 - window.used_percent <= prefs.primary_min_left_pct);
    let secondary_hit = usage
        .secondary
        .as_ref()
        .is_some_and(|window| 100.0 - window.used_percent <= prefs.weekly_min_left_pct);
    if !primary_hit && !secondary_hit {
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
            let desktop_restarted = if prefs.auto_approve { provider.desktop_restart(&prefs).await } else { false };
            if desktop_restarted { eprintln!("[limit:{provider_id}] desktop restarted"); }
            eprintln!("[limit:{provider_id}] TODO(phase-6): CLI handover is deferred");
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
                emit_to(&app, "widget", "accounts:changed", ());
                handle_limit(app.clone(), provider_id.into(), usage).await;
            }
            let seconds = prefs.poll_interval_sec.max(1);
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        }
    });
}

fn start_monitors(app: &AppHandle) {
    let state = app.state::<AppState>();
    for (id, _, _, _, provider) in state.providers.all() {
        if provider.list_accounts().is_empty() && !provider.has_live_auth() {
            continue;
        }
        start_monitor(app, id, provider);
    }
}

fn parse_probe_report(report: &str) -> Value {
    serde_json::from_str(report).unwrap_or_else(|_| Value::String(report.to_owned()))
}

fn probe_script() -> String {
    r#"(() => {
      const run = async () => {
        try {
          if (!(await window.__TAURI__.core.invoke('probe:enabled'))) return;
          const page = location.pathname.split('/').pop() || '';
          const report = { page, loaded: true, rotator: typeof window.rotator, call: null };
          if (page === 'manager.html') {
            const providers = await window.rotator.providers();
            const lists = {};
            for (const provider of providers) lists[provider.id] = await window.rotator.list(provider.id);
            report.call = { providers, lists };
          } else if (page === 'onboarding.html') {
            report.call = { config: await window.rotator.getConfig(), lang: await window.rotator.getLang() };
          } else if (page === 'widget.html') {
            const config = await window.rotator.getConfig();
            const providers = await window.rotator.providers();
            const lists = {};
            for (const provider of providers) lists[provider.id] = await window.rotator.list(provider.id);
            await new Promise(resolve => setTimeout(resolve, 250));
            const windows = await window.__TAURI__.core.invoke('probe:windows');
            report.call = { config, providers, lists, windows, renderMode: document.body.classList.contains('compact') ? 'compact' : 'normal', bodyBackground: getComputedStyle(document.body).backgroundColor, rendered: document.body.innerText.length > 0 };
          } else if (page === 'widget-settings.html') {
            const config = await window.rotator.getConfig();
            const providers = await window.rotator.providers();
            const first = providers[0];
            report.call = { config, providers, firstList: first ? await window.rotator.list(first.id) : [] };
          } else return;
          await window.__TAURI__.core.invoke('probe:report', { page: page.replace('.html', ''), report: JSON.stringify(report) });
        } catch (error) {
          await window.__TAURI__.core.invoke('probe:report', { page: location.pathname, report: JSON.stringify({ loaded: true, rotator: typeof window.rotator, error: String(error) }) });
        }
      };
      setTimeout(run, 900);
    })();"#
    .to_owned()
}

fn open_probe_windows(app: &AppHandle) {
    open_manager(app);
    if std::env::var_os("LAZYSWITCH_PROBE_WIDGET").is_some() {
        open_widget(app, true);
        open_widget_settings(app);
    } else {
        open_onboarding(app);
        open_widget_settings(app);
    }
}

fn open_external(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    return platform::open_url(url);
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    let result: Result<std::process::Child, std::io::Error> = Err(std::io::Error::other("unsupported platform"));
    #[cfg(not(windows))]
    result.map(|_| ()).map_err(|error| error.to_string())
}

#[tauri::command(rename = "providers:list")]
fn providers_list(state: tauri::State<'_, AppState>) -> Vec<ProviderInfo> {
    provider_info(&state)
}

#[tauri::command(rename = "accounts:list")]
fn accounts_list(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
) -> Result<Vec<EnrichedAccount>, String> {
    list_accounts(&app, &state, &provider)
}

#[tauri::command(rename = "accounts:switch")]
async fn accounts_switch(
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
    match switcher::switch_to(account_provider.as_ref(), &name, &prefs, false).await {
        Ok(_) => {
            let desktop_restarted = account_provider.desktop_restart(&prefs).await;
            if desktop_restarted { eprintln!("[accounts:switch] desktop restarted"); }
            eprintln!("[accounts:switch] TODO(phase-6): CLI handover is deferred");
            broadcast_changed(&app);
            notify(
                &app,
                format!("{provider} switched"),
                format!("{name} is now active{}", if desktop_restarted { " (desktop restarted)" } else { "" }),
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
fn accounts_set_enabled(
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
fn accounts_remove(
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
fn accounts_rename(
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
fn accounts_import_current(
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
async fn accounts_add_via_login(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    provider: String,
) -> Result<LoginFlowResult, String> {
    let app_for_url = app.clone();
    let on_url: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |url| emit_to(&app_for_url, "manager", "login:url", url));
    match provider.as_str() {
        "claude" => Ok(state.providers.claude.add_via_login_with_callback(Some(on_url)).await),
        "codex" => Ok(state.providers.codex.add_via_login_with_callback(Some(on_url)).await),
        _ => Ok(LoginFlowResult { ok: false, error: Some("unsupported".into()), ..LoginFlowResult::default() }),
    }
}

#[tauri::command(rename = "cli:testRestart")]
fn cli_test_restart(provider: String) -> Value {
    eprintln!("[cli:testRestart:{provider}] TODO(phase-6): CLI handover is deferred");
    serde_json::json!({"ok": true, "sessions": 0, "result": null})
}

#[tauri::command(rename = "config:get")]
fn config_get(state: tauri::State<'_, AppState>) -> AppConfig {
    state_config(&state)
}

#[tauri::command(rename = "lang:get")]
fn lang_get(state: tauri::State<'_, AppState>) -> String {
    let config = state_config(&state);
    let locale = std::env::var("LANG").ok();
    format!(
        "{:?}",
        core::i18n::resolve_lang(&config.language, locale.as_deref())
    )
    .to_lowercase()
}

#[tauri::command(rename = "config:set")]
fn config_set(
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
        if let Ok(exe) = std::env::current_exe() { platform::apply_launch_at_login(next.launch_at_login, &exe).map_err(|error| format!("start at login: {error}"))?; }
    }
    let previous_transparent = previous.usage_widget.minimized && previous.usage_widget.compact_position == "taskbar";
    let next_transparent = next.usage_widget.minimized && next.usage_widget.compact_position == "taskbar";
    if let Some(widget) = app.get_webview_window("widget") {
        if previous_transparent != next_transparent {
            let _ = widget.destroy();
            let app_for_recreate = app.clone();
            tauri::async_runtime::spawn(async move { tokio::time::sleep(Duration::from_millis(100)).await; open_widget(&app_for_recreate, true); });
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
fn onboarding_finish(
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
fn open_url(url: String) -> Result<(), String> {
    open_external(&url)
}

#[tauri::command(rename = "manager:close")]
fn manager_close(app: AppHandle) {
    close_window(&app, "manager");
}

#[tauri::command(rename = "widget:close")]
fn widget_close(app: AppHandle, state: tauri::State<'_, AppState>) {
    if let Ok(mut config) = state.config.lock() {
        config.usage_widget.enabled = false;
        let _ = config::save_config(&config);
    }
    close_window(&app, "widget");
}

#[tauri::command(rename = "widget-settings:close")]
fn widget_settings_close(app: AppHandle) {
    close_window(&app, "widget-settings");
}

#[tauri::command(rename = "widget:compact-height")]
fn widget_compact_height(app: AppHandle, window: WebviewWindow, state: tauri::State<'_, AppState>, height: f64) {
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

#[tauri::command(rename = "approval:respond")]
fn approval_respond(app: AppHandle, state: tauri::State<'_, AppState>, approved: bool) {
    if let Ok(mut runtime) = state.runtime.lock() {
        if let Some(sender) = runtime.approval.take() {
            let _ = sender.send(approved);
        }
    }
    close_window(&app, "approval");
}

#[tauri::command(rename = "cli-restart:payload")]
fn cli_restart_payload(state: tauri::State<'_, AppState>) -> Option<Value> {
    state
        .runtime
        .lock()
        .ok()
        .and_then(|runtime| runtime.cli_payload.clone())
}

#[tauri::command(rename = "cli-restart:respond")]
fn cli_restart_respond(app: AppHandle, state: tauri::State<'_, AppState>, action: String) {
    eprintln!("[cli-restart:{action}] TODO(phase-6): CLI handover is deferred");
    if let Ok(mut runtime) = state.runtime.lock() {
        runtime.cli_payload = None;
    }
    close_window(&app, "cli-restart");
}

#[tauri::command(rename = "app-notify:payload")]
fn app_notify_payload(
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
fn app_notify_resize(
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
fn app_notify_dismiss(window: WebviewWindow) {
    let _ = window.close();
}

#[tauri::command(rename = "window:startDragging")]
fn start_dragging(window: WebviewWindow) {
    if let Err(error) = window.start_dragging() {
        eprintln!("[tauri:drag] {error}");
    }
}

#[tauri::command(rename = "probe:enabled")]
fn probe_enabled() -> bool {
    std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some()
}

#[tauri::command(rename = "probe:windows")]
async fn probe_windows(app: AppHandle) -> Value {
    #[cfg(windows)]
    {
        let value = app
            .get_webview_window("widget")
            .and_then(|window| window.hwnd().ok())
            .map(|hwnd| hwnd.0 as isize);
        if let Some(value) = value {
            return tauri::async_runtime::spawn_blocking(move || platform::widget_probe(value, true))
                .await
                .unwrap_or_else(|_| serde_json::json!({}));
        }
    }
    serde_json::json!({})
}

#[tauri::command(rename = "probe:report")]
fn probe_report(app: AppHandle, state: tauri::State<'_, AppState>, page: String, report: String) {
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

fn build_state() -> AppState {
    AppState {
        config: Mutex::new(config::load_config()),
        providers: ProviderSet {
            codex: Arc::new(CodexProvider::new(
                crate::core::paths::CodexPaths::from_env(),
                Arc::new(ReqwestTransport::default()),
            )),
            claude: Arc::new(ClaudeProvider::new(
                crate::core::paths::ClaudePaths::from_env(),
                Arc::new(ReqwestTransport::default()),
            )),
        },
        runtime: Mutex::new(RuntimeData::default()),
    }
}

#[tauri::command(rename = "widget:context-menu")]
fn widget_context_menu(app: AppHandle) {
    show_widget_context_menu(&app);
}

fn tray_lang(state: &AppState) -> core::i18n::Lang {
    let config = state_config(state);
    core::i18n::resolve_lang(&config.language, std::env::var("LANG").ok().as_deref())
}

fn tray_text(state: &AppState, key: &str, vars: &[(&str, String)]) -> String {
    let vars = vars.iter().map(|(key, value)| ((*key).to_owned(), value.clone())).collect::<HashMap<_, _>>();
    core::i18n::t(tray_lang(state), key, Some(&vars))
}

fn refresh_tray_tooltip(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut active = Vec::new();
    for (id, display, _, _, provider) in state.providers.all() {
        if let Some(name) = provider.active_account_name() { active.push(format!("{display}: {name}")); }
        let _ = id;
    }
    let name = if active.is_empty() { "LazySwitch".into() } else { active.join(" · ") };
    let tooltip = tray_text(&state, "tray.tooltip", &[("name", name)]);
    if let Some(tray) = app.tray_by_id("main") { let _ = tray.set_tooltip(Some(tooltip)); }
}

fn build_tray_menu(app: &AppHandle) -> Result<Menu<tauri::Wry>, String> {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    let menu = Menu::new(app).map_err(|e| e.to_string())?;
    let manage = MenuItem::with_id(app, "tray.manage", tray_text(&state, "tray.manage", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    let tutorial = MenuItem::with_id(app, "tray.tutorial", tray_text(&state, "tray.tutorial", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    let auto = CheckMenuItem::with_id(app, "tray.autoApprove", tray_text(&state, "tray.autoApprove", &[]), true, config.codex.auto_approve, None::<&str>).map_err(|e| e.to_string())?;
    let codex_cli = CheckMenuItem::with_id(app, "tray.autoRestartCli.codex", tray_text(&state, "tray.autoRestartCli", &[("provider", "Codex".into())]), true, config.codex.auto_restart_cli, None::<&str>).map_err(|e| e.to_string())?;
    let claude_cli = CheckMenuItem::with_id(app, "tray.autoRestartCli.claude", tray_text(&state, "tray.autoRestartCli", &[("provider", "Claude Code".into())]), true, config.claude.auto_restart_cli, None::<&str>).map_err(|e| e.to_string())?;
    let widget = CheckMenuItem::with_id(app, "tray.usageWidget", tray_text(&state, "tray.usageWidget", &[]), true, config.usage_widget.enabled, None::<&str>).map_err(|e| e.to_string())?;
    let login = CheckMenuItem::with_id(app, "tray.startAtLogin", tray_text(&state, "tray.startAtLogin", &[]), true, config.launch_at_login, None::<&str>).map_err(|e| e.to_string())?;
    let language = Submenu::with_id(app, "tray.language", tray_text(&state, "tray.language", &[]), true).map_err(|e| e.to_string())?;
    for (id, value, label) in [("system", "", "tray.langSystem"), ("ko", "ko", "한국어"), ("en", "en", "English"), ("ja", "ja", "日本語"), ("zh", "zh", "中文")] {
        let item = CheckMenuItem::with_id(app, format!("tray.language.{id}"), if label.starts_with("tray.") { tray_text(&state, label, &[]) } else { label.into() }, true, config.language == value, None::<&str>).map_err(|e| e.to_string())?;
        language.append(&item).map_err(|e| e.to_string())?;
    }
    let separator = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "tray.quit", tray_text(&state, "tray.quit", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    for item in [&manage as &dyn tauri::menu::IsMenuItem<tauri::Wry>, &tutorial, &auto, &codex_cli, &claude_cli, &widget, &login, &language, &separator, &quit] { menu.append(item).map_err(|e| e.to_string())?; }
    Ok(menu)
}

fn set_widget_context_menu_open(app: &AppHandle, value: bool) {
    if let Ok(mut runtime) = app.state::<AppState>().runtime.lock() { runtime.widget_context_menu_open = value; }
}

fn update_app_config(app: &AppHandle, update: impl FnOnce(&mut AppConfig)) {
    let state = app.state::<AppState>();
    if let Ok(mut config) = state.config.lock() {
        update(&mut config);
        let _ = config::save_config(&config);
    };
}

fn show_widget_context_menu(app: &AppHandle) {
    let Some(window) = app.get_webview_window("widget") else { return; };
    let config = state_config(&app.state::<AppState>());
    if !config.usage_widget.minimized { return; }
    let Ok(menu) = (|| -> Result<Menu<tauri::Wry>, String> {
        let menu = Menu::new(app).map_err(|e| e.to_string())?;
        let state = app.state::<AppState>();
        let settings = MenuItem::with_id(app, "widget.settings", tray_text(&state, "widget.settings", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
        let maximize = MenuItem::with_id(app, "widget.maximize", tray_text(&state, "widget.maximize", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
        let close = MenuItem::with_id(app, "widget.close", tray_text(&state, "widget.close", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
        menu.append(&settings).map_err(|e| e.to_string())?; menu.append(&maximize).map_err(|e| e.to_string())?; menu.append(&close).map_err(|e| e.to_string())?;
        Ok(menu)
    })() else { return };
    set_widget_context_menu_open(app, true);
    let _ = menu.popup(window.as_ref().window());
    set_widget_context_menu_open(app, false);
}

fn tray_menu_action(app: &AppHandle, id: &str) {
    match id {
        "tray.manage" => open_manager(app),
        "tray.tutorial" => open_onboarding(app),
        "tray.usageWidget" => {
            let state = app.state::<AppState>(); if let Ok(mut config) = state.config.lock() { config.usage_widget.enabled = !config.usage_widget.enabled; let _ = config::save_config(&config); } sync_usage_widget(app);
        }
        "tray.autoApprove" => update_app_config(app, |config| config.codex.auto_approve = !config.codex.auto_approve),
        "tray.autoRestartCli.codex" | "tray.autoRestartCli.claude" => update_app_config(app, |config| if id.ends_with("codex") { config.codex.auto_restart_cli = !config.codex.auto_restart_cli; } else { config.claude.auto_restart_cli = !config.claude.auto_restart_cli; }),
        "tray.startAtLogin" => { update_app_config(app, |config| config.launch_at_login = !config.launch_at_login); #[cfg(windows)] if let Ok(exe) = std::env::current_exe() { let _ = platform::apply_launch_at_login(state_config(&app.state::<AppState>()).launch_at_login, &exe); } }
        "tray.quit" => app.exit(0),
        id if id.starts_with("tray.language.") => { let value = id.rsplit('.').next().unwrap_or(""); let value = if value == "system" { "" } else { value }; update_app_config(app, |config| config.language = value.into()); }
        "widget.settings" => open_widget_settings(app),
        "widget.maximize" => { let state = app.state::<AppState>(); if let Ok(mut config) = state.config.lock() { config.usage_widget.minimized = false; let _ = config::save_config(&config); } open_widget(app, true); }
        "widget.close" => widget_close(app.clone(), app.state::<AppState>()),
        _ => {}
    }
}

fn show_tray_menu(app: &AppHandle, rect: tauri::Rect) {
    let Ok(menu) = build_tray_menu(app) else { return };
    #[cfg(windows)] {
        let (left, top) = match rect.position { tauri::Position::Physical(p) => (p.x, p.y), tauri::Position::Logical(p) => (p.x as i32, p.y as i32) };
        let (width, height) = match rect.size { tauri::Size::Physical(s) => (s.width as i32, s.height as i32), tauri::Size::Logical(s) => (s.width as i32, s.height as i32) };
        let icon = platform::Rect { left, top, right: left + width, bottom: top + height };
        let widget = app.get_webview_window("widget").and_then(|window| window.hwnd().ok()).and_then(|hwnd| platform::rect(hwnd.0 as isize));
        if let Some((x, y)) = platform::tray_menu_position(icon, widget) {
            let anchor = app.get_webview_window("manager").or_else(|| app.get_webview_window("onboarding")).or_else(|| app.get_webview_window("widget"));
            if let Some(anchor) = anchor {
                if let Ok(position) = anchor.outer_position() {
                    let _ = menu.popup_at(anchor.as_ref().window(), tauri::PhysicalPosition::new(x - position.x, y - position.y));
                }
            } else if let Ok(hwnd) = menu.hpopupmenu() {
                unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::TrackPopupMenu(windows::Win32::UI::WindowsAndMessaging::HMENU(hwnd as *mut _), windows::Win32::UI::WindowsAndMessaging::TPM_LEFTALIGN | windows::Win32::UI::WindowsAndMessaging::TPM_TOPALIGN, x, y, Some(0), windows::Win32::Foundation::HWND(std::ptr::null_mut()), None); }
            }
            return;
        }
    }
    if let Some(anchor) = app.get_webview_window("manager").or_else(|| app.get_webview_window("onboarding")).or_else(|| app.get_webview_window("widget")) { let _ = menu.popup(anchor.as_ref().window()); }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if !platform::acquire_single_instance() { return; }
    tauri::Builder::default()
        .manage(build_state())
        .invoke_handler(tauri::generate_handler![
            providers_list,
            accounts_list,
            accounts_switch,
            accounts_set_enabled,
            accounts_remove,
            accounts_rename,
            accounts_import_current,
            accounts_add_via_login,
            cli_test_restart,
            config_get,
            config_set,
            lang_get,
            onboarding_finish,
            open_url,
            manager_close,
            widget_close,
            widget_settings_close,
            widget_compact_height,
            widget_context_menu,
            approval_respond,
            cli_restart_payload,
            cli_restart_respond,
            app_notify_payload,
            app_notify_resize,
            app_notify_dismiss,
            start_dragging,
            probe_enabled,
            probe_windows,
            probe_report
        ])
        .setup(move |app| {
            let icon = app.default_window_icon().cloned().ok_or_else(|| "default tray icon is unavailable".to_owned())?;
            TrayIconBuilder::with_id("main")
                .icon(icon)
                .tooltip("LazySwitch")
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| tray_menu_action(app, event.id().as_ref()))
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    match event {
                        TrayIconEvent::DoubleClick { .. } => open_manager(app),
                        TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Down, rect, .. } => show_tray_menu(app, rect),
                        _ => {}
                    }
                })
                .build(app)?;
            refresh_tray_tooltip(app.handle());
            // The probe is appended to the same initialization script so it
            // observes the real page and the real rotator surface.
            let state = app.state::<AppState>();
            if std::env::var_os("LAZYSWITCH_PROBE_TASKBAR").is_some() {
                if let Ok(mut config) = state.config.lock() {
                    config.usage_widget.minimized = true;
                    config.usage_widget.compact_position = "taskbar".into();
                    config.usage_widget.always_on_top = true;
                }
            }
            let config = state_config(&state);
            if !config.onboarded {
                open_onboarding(app.handle());
            } else if state
                .providers
                .all()
                .into_iter()
                .map(|(_, _, _, _, provider)| provider.list_accounts().len())
                .sum::<usize>()
                < 2
            {
                open_manager(app.handle());
            }
            sync_usage_widget(app.handle());
            #[cfg(windows)]
            if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_none() {
                if let Ok(exe) = std::env::current_exe() { let _ = platform::apply_launch_at_login(state_config(&state).launch_at_login, &exe); }
            }
            start_monitors(app.handle());
            if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some() {
                // Make the four requested windows observable without changing
                // the user's saved configuration or account store.
                let probe_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    open_probe_windows(&probe_app);
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| eprintln!("error while running tauri application: {error}"));
}
