use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use crate::core::config::AppConfig;
use crate::core::platform;
use crate::core::types::PAccount;

use super::state::{has_enrolled_accounts, state_config, AppState, ToastPayload};
use super::tray::{refresh_tray_tooltip, tray_lang};

pub const SHIM: &str = include_str!("../../../src/renderer/tauri-shim.js");
pub const DEFAULT_WIDGET_BACKGROUND: (u8, u8, u8, u8) = (0x16, 0x17, 0x1b, 0xff);
pub const WIDGET_COMPACT_WIDTH: f64 = 280.0;
pub const WIDGET_COMPACT_DEFAULT_HEIGHT: f64 = 70.0;
pub const WIDGET_COMPACT_MIN_HEIGHT: f64 = 38.0;
pub const TOAST_WIDTH: f64 = 360.0;
pub const TOAST_DEFAULT_HEIGHT: f64 = 112.0;
pub const TOAST_MIN_HEIGHT: f64 = 84.0;
pub const TOAST_MAX_HEIGHT: f64 = 160.0;
pub const TOAST_MARGIN: i32 = 18;
pub const TOAST_GAP: i32 = 10;

pub fn emit_to(app: &AppHandle, label: &str, event: &str, payload: impl Serialize + Clone) {
    if let Err(error) = app.emit_to(label, event, payload) {
        eprintln!("[tauri:event:{event}] {error}");
    }
}

pub fn broadcast_changed(app: &AppHandle) {
    for label in ["manager", "widget", "widget-settings"] {
        if app.get_webview_window(label).is_some() {
            emit_to(app, label, "accounts:changed", ());
        }
    }
    sync_usage_widget(app);
    refresh_tray_tooltip(app);
}

pub fn set_window_bounds(window: &WebviewWindow, x: f64, y: f64, width: f64, height: f64) {
    let _ = window.set_size(tauri::PhysicalSize::new(
        width.max(1.0) as u32,
        height.max(1.0) as u32,
    ));
    let _ = window.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
}

pub fn widget_bounds(config: &AppConfig) -> (f64, f64, f64, f64) {
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
pub fn create_window(
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
            if transparent {
                0
            } else {
                DEFAULT_WIDGET_BACKGROUND.3
            },
        ))
        .initialization_script(if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some() {
            format!("{}\n{}", SHIM, super::probe::probe_script())
        } else {
            SHIM.to_owned()
        })
        .build()?;
    Ok(window)
}

pub fn open_manager(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("manager") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Err(error) = create_window(
        app, "manager", "manager.html", "Accounts", 920.0, 730.0, false, false, false,
    ) {
        eprintln!("[tauri:manager] {error}");
    }
}

pub fn open_onboarding(app: &AppHandle) {
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

pub fn open_widget(app: &AppHandle, force: bool) {
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
        // This path runs on every routine sync (each account's usage refresh
        // completing, etc.), not just real compact/full transitions — forcing
        // bounds here would snap a window the user just dragged back to
        // whatever position was last saved. Only reassert styling.
        apply_widget_platform(app, &window, &config, false);
        return;
    }
    // Several async paths (each account's usage refresh completing, the
    // onboarding-finish double sync) can all decide "no widget yet, make
    // one" within the same tick. Without this guard that races into two
    // real windows sharing the "widget" label, where only one stays
    // reachable via get_webview_window and the other is an orphan.
    let should_create = state
        .runtime
        .lock()
        .map(|mut runtime| {
            if runtime.creating_widget {
                false
            } else {
                runtime.creating_widget = true;
                true
            }
        })
        .unwrap_or(false);
    if !should_create {
        return;
    }
    let (x, y, width, height) = widget_bounds(&config);
    let result = create_window(
        app,
        "widget",
        "widget.html",
        "LazySwitch Usage",
        width,
        height,
        false,
        config.usage_widget.always_on_top,
        config.usage_widget.minimized && config.usage_widget.compact_position == "taskbar",
    );
    if let Ok(mut runtime) = state.runtime.lock() {
        runtime.creating_widget = false;
    }
    match result {
        Ok(window) => {
            let app_for_theme = app.clone();
            window.on_window_event(move |event| {
                if matches!(event, WindowEvent::ThemeChanged(_)) {
                    let state = app_for_theme.state::<AppState>();
                    let config = state_config(&state);
                    if let Some(widget) = app_for_theme.get_webview_window("widget") {
                        emit_widget_taskbar_theme(&app_for_theme, &widget, &config);
                    }
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
            apply_widget_platform(app, &window, &config, true);
            emit_widget_taskbar_theme(app, &window, &config);
        }
        Err(error) => eprintln!("[tauri:widget] {error}"),
    }
}

pub fn emit_widget_taskbar_theme(app: &AppHandle, _window: &WebviewWindow, config: &AppConfig) {
    if config.usage_widget.minimized && config.usage_widget.compact_position == "taskbar" {
        emit_to(
            app,
            "widget",
            "widget:taskbar-theme",
            platform::taskbar_theme()
                .map(|light| serde_json::json!({"light": light}))
                .unwrap_or_else(|| serde_json::json!({"light": false})),
        );
    } else {
        emit_to(app, "widget", "widget:taskbar-theme", Value::Null);
    }
}

pub fn apply_widget_platform(
    app: &AppHandle,
    window: &WebviewWindow,
    config: &AppConfig,
    reposition: bool,
) {
    let compact = config.usage_widget.minimized;
    let _ = window.set_resizable(!compact);
    #[cfg(windows)]
    {
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
                // The compact corner is computed from live monitor/taskbar
                // geometry, not something the user can drag away from, so
                // it's fine (and necessary, e.g. after a monitor change) to
                // keep reasserting it on every call.
                if let Some(bounds) = platform::compact_bounds(
                    hwnd_value,
                    width,
                    height,
                    &config.usage_widget.compact_position,
                ) {
                    platform::set_bounds(hwnd_value, bounds, config.usage_widget.always_on_top);
                }
                if config.usage_widget.compact_position == "taskbar"
                    && config.usage_widget.always_on_top
                {
                    super::monitor::start_widget_topmost_timer(app.clone());
                }
            } else if reposition {
                // Only reapply full-size bounds on an actual creation or
                // compact/full transition. This function also runs on every
                // routine sync (each account's usage refresh completing,
                // etc.) via open_widget's "already exists" path; forcing
                // bounds there would snap a window the user just dragged
                // back to whatever position was last saved every ~30s.
                let (left, top) = match (config.usage_widget.x, config.usage_widget.y) {
                    (Some(x), Some(y)) => (x.round() as i32, y.round() as i32),
                    _ => platform::rect(hwnd_value)
                        .map(|current| (current.left, current.top))
                        .unwrap_or((0, 0)),
                };
                let bounds = platform::Rect {
                    left,
                    top,
                    right: left + config.usage_widget.width.round() as i32,
                    bottom: top + config.usage_widget.height.round() as i32,
                };
                platform::set_bounds(hwnd_value, bounds, config.usage_widget.always_on_top);
            }
            let should_layer = compact && config.usage_widget.compact_position == "taskbar";
            // Real transparency is set up once, at window-construction time,
            // by create_window's `.transparent(true)` (see
            // sync_widget_after_config_change, which always destroys and
            // recreates the window for any transparency-relevant change —
            // so construction-time state always matches `should_layer` by
            // the time this runs). Forcing WS_EX_LAYERED back on here after
            // the fact used to fight that: it switches the HWND into the
            // legacy GDI-composited redirection surface, which is
            // incompatible with WebView2's own DirectComposition swap chain
            // and left the compact widget opaque despite the bit being set.
            // Click-through only makes sense while pinned to the taskbar in
            // compact mode; otherwise the widget must stay fully interactive.
            let should_click_through = should_layer && config.usage_widget.click_through;
            platform::set_click_through(hwnd_value, should_click_through);
        }
    }
}

/// Applies a config change that may affect the widget window, recreating it
/// when the compact+taskbar transparency flag flips (Tauri's `transparent`
/// window attribute is fixed at construction time) and just re-applying
/// platform bounds/styling otherwise. Used by both `config:set` and any
/// tray/context-menu action that flips `minimized` directly, so every path
/// that can change compact state behaves the same way.
pub fn sync_widget_after_config_change(app: &AppHandle, previous: &AppConfig, next: &AppConfig) {
    let previous_transparent =
        previous.usage_widget.minimized && previous.usage_widget.compact_position == "taskbar";
    let next_transparent =
        next.usage_widget.minimized && next.usage_widget.compact_position == "taskbar";
    let Some(widget) = app.get_webview_window("widget") else {
        return;
    };
    if previous_transparent != next_transparent {
        // Minimizing (full -> compact) destroys and recreates the window
        // (see the module doc comment above). Remember exactly where the
        // full-size window was sitting *before* it goes compact, so that
        // maximizing later restores to that position — using the compact
        // widget's own (taskbar-corner) position instead would leave the
        // much bigger full-size window mostly off the taskbar-side edge of
        // the screen when it's restored.
        if !previous.usage_widget.minimized && next.usage_widget.minimized {
            if let Ok(position) = widget.outer_position() {
                let state = app.state::<AppState>();
                let lock_result = state.config.lock();
                if let Ok(mut config) = lock_result {
                    config.usage_widget.x = Some(position.x as f64);
                    config.usage_widget.y = Some(position.y as f64);
                    let _ = crate::core::config::save_config(&config);
                }
            }
        }
        let _ = widget.destroy();
        let app_for_recreate = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            open_widget(&app_for_recreate, true);
        });
    } else {
        let _ = widget.set_always_on_top(next.usage_widget.always_on_top);
        apply_widget_platform(app, &widget, next, true);
        emit_widget_taskbar_theme(app, &widget, next);
    }
}

pub fn open_widget_settings(app: &AppHandle) {
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
    ) {
        eprintln!("[tauri:widget-settings] {error}");
    }
}

pub fn sync_usage_widget(app: &AppHandle) {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    let onboarding = app.get_webview_window("onboarding").is_some();
    if config.usage_widget.enabled && has_enrolled_accounts(&state) && !onboarding {
        open_widget(app, false);
    } else if let Some(window) = app.get_webview_window("widget") {
        let _ = window.close();
    }
}

pub fn close_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.close();
    }
}

pub fn clamp_toasts(state: &AppState, app: &AppHandle) {
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

pub fn drain_notifications(app: &AppHandle) {
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

pub fn show_notification_now(app: &AppHandle, payload: ToastPayload) {
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

pub fn notify(app: &AppHandle, title: impl Into<String>, body: impl Into<String>) {
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

pub fn open_external(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    return platform::open_url(url);
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    let result: Result<std::process::Child, std::io::Error> =
        Err(std::io::Error::other("unsupported platform"));
    #[cfg(not(windows))]
    result.map(|_| ()).map_err(|error| error.to_string())
}

pub fn open_cli_restart(
    app: &AppHandle,
    provider: &str,
    sessions: &[crate::core::cli_sessions::CliSession],
) -> Result<tokio::sync::oneshot::Receiver<String>, String> {
    let state = app.state::<AppState>();
    let payload = crate::core::cli_handover::payload(provider, sessions);
    let value = serde_json::to_value(payload).map_err(|error| error.to_string())?;
    let (sender, receiver) = tokio::sync::oneshot::channel::<String>();
    if let Ok(mut runtime) = state.runtime.lock() {
        runtime.cli_payload = Some(value);
        runtime.cli_response = Some(sender);
    }
    let lang = format!("{:?}", super::tray::tray_lang(&state)).to_lowercase();
    match WebviewWindowBuilder::new(
        app,
        "cli-restart",
        WebviewUrl::App(format!("cli-restart.html?lang={lang}").into()),
    )
    .title(crate::core::i18n::t(
        super::tray::tray_lang(&state),
        "popup.cliTitle",
        None,
    ))
    .inner_size(520.0, 520.0)
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .transparent(true)
    .background_color(tauri::window::Color(0, 0, 0, 0))
    .initialization_script(SHIM)
    .build()
    {
        Ok(window) => {
            let app_for_close = app.clone();
            window.on_window_event(move |event| {
                if matches!(event, WindowEvent::Destroyed) {
                    if let Ok(mut runtime) = app_for_close.state::<AppState>().runtime.lock() {
                        if let Some(sender) = runtime.cli_response.take() {
                            let _ = sender.send("later".into());
                        }
                    }
                }
            });
            Ok(receiver)
        }
        Err(error) => {
            if let Ok(mut runtime) = state.runtime.lock() {
                runtime.cli_payload = None;
                runtime.cli_response = None;
            }
            Err(error.to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn open_switch_warning(
    app: &AppHandle,
    from_name: &str,
    from_label: Option<&str>,
    to: &PAccount,
    bar_label: &str,
    used_percent: f64,
    resets_at: Option<i64>,
) -> bool {
    let state = app.state::<AppState>();
    if app.get_webview_window("approval").is_some() {
        return false;
    }
    let (sender, receiver) = tokio::sync::oneshot::channel::<bool>();
    if let Ok(mut runtime) = state.runtime.lock() {
        runtime.approval = Some(sender);
    }
    let encode = |value: &str| urlencoding::encode(value).into_owned();
    let mut url = format!(
        "approval.html?lang={}&fromName={}&toName={}&barLabel={}&percent={}",
        encode(&format!("{:?}", tray_lang(&state)).to_lowercase()),
        encode(from_name),
        encode(&to.name),
        encode(bar_label),
        used_percent,
    );
    if let Some(label) = from_label {
        url.push_str(&format!("&fromLabel={}", encode(label)));
    }
    if let Some(label) = to.label.as_deref() {
        url.push_str(&format!("&toLabel={}", encode(label)));
    }
    if let Some(resets_at) = resets_at {
        url.push_str(&format!("&resetAt={resets_at}"));
    }
    let window = match WebviewWindowBuilder::new(app, "approval", WebviewUrl::App(url.into()))
        .title(crate::core::i18n::t(
            tray_lang(&state),
            "popup.limitReached",
            None,
        ))
        .inner_size(360.0, 300.0)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .transparent(true)
        .background_color(tauri::window::Color(0, 0, 0, 0))
        .initialization_script(SHIM)
        .build()
    {
        Ok(window) => window,
        Err(_) => {
            if let Ok(mut runtime) = state.runtime.lock() {
                runtime.approval = None;
            }
            return false;
        }
    };
    let app_for_close = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            if let Ok(mut runtime) = app_for_close.state::<AppState>().runtime.lock() {
                if let Some(sender) = runtime.approval.take() {
                    let _ = sender.send(false);
                }
            }
        }
    });
    // Advisory only: if the user never answers, the real auto-switch will
    // still fire later at the actual threshold, so timing out is safe.
    let result = tokio::time::timeout(Duration::from_secs(20), receiver).await;
    let _ = window.close();
    matches!(result, Ok(Ok(true)))
}

