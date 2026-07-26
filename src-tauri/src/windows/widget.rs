use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::menu::MenuBuilder;
use tauri::window::Color;
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent, Wry,
};

use crate::app_state::AppState;
use crate::config::{self, CompactPosition};
use crate::i18n::{resolve_lang, t};
use crate::provider;
use crate::provider_types::ProviderId;
use crate::windows::widget_geometry::{
    clamp_widget_bounds, compact_bottom_right_bounds, DisplayArea, WidgetBounds,
};
use crate::windows::widget_native;
use crate::windows::widget_taskbar;

const LABEL: &str = "usage-widget";
const ONBOARDING_LABEL: &str = "onboarding";
const DEFAULT_WIDGET_BACKGROUND: Color = Color(0x16, 0x17, 0x1b, 0xff);
const WIDGET_MIN_WIDTH: f64 = 300.0;
const WIDGET_MIN_HEIGHT: f64 = 260.0;
const WIDGET_COMPACT_WIDTH: f64 = 280.0;
const WIDGET_COMPACT_DEFAULT_HEIGHT: f64 = 70.0;
const WIDGET_COMPACT_MIN_HEIGHT: f64 = 38.0;
const BOUNDS_SAVE_DELAY: Duration = Duration::from_millis(400);
const TOPMOST_REASSERT_DELAY: Duration = Duration::from_millis(100);
const CONTEXT_SETTINGS_ID: &str = "widget-context-settings";
const CONTEXT_RESTORE_ID: &str = "widget-context-restore";
const CONTEXT_CLOSE_ID: &str = "widget-context-close";
static WIDGET_IS_TRANSPARENT: AtomicBool = AtomicBool::new(false);

pub fn widget_default_bounds(saved: WidgetBounds, work_area: DisplayArea) -> WidgetBounds {
    WidgetBounds {
        x: if saved.x.is_finite() {
            saved.x
        } else {
            work_area.x + work_area.width - saved.width
        },
        y: if saved.y.is_finite() {
            saved.y
        } else {
            work_area.y + work_area.height - saved.height
        },
        width: saved.width,
        height: saved.height,
    }
}

fn restore_widget_bounds(app: &AppHandle<Wry>) -> Result<WidgetBounds, String> {
    let monitor = app
        .primary_monitor()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "primary monitor unavailable".to_string())?;
    let display = monitor.size();
    let display_position = monitor.position();
    let work_area = monitor.work_area();
    let scale_factor = monitor.scale_factor();
    let state = app.state::<Mutex<AppState>>();
    let state = state.lock().map_err(|error| error.to_string())?;
    let saved = WidgetBounds {
        x: state.cfg.usage_widget.x.unwrap_or(f64::NAN),
        y: state.cfg.usage_widget.y.unwrap_or(f64::NAN),
        width: state.cfg.usage_widget.width,
        height: state.cfg.usage_widget.height,
    };
    Ok(clamp_widget_bounds(
        widget_default_bounds(
            saved,
            DisplayArea {
                x: f64::from(work_area.position.x) / scale_factor,
                y: f64::from(work_area.position.y) / scale_factor,
                width: f64::from(work_area.size.width) / scale_factor,
                height: f64::from(work_area.size.height) / scale_factor,
            },
        ),
        DisplayArea {
            x: f64::from(display_position.x) / scale_factor,
            y: f64::from(display_position.y) / scale_factor,
            width: f64::from(display.width) / scale_factor,
            height: f64::from(display.height) / scale_factor,
        },
    ))
}

fn save_widget_bounds(app: &AppHandle<Wry>) -> Result<(), String> {
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    let scale_factor = window.scale_factor().map_err(|error| error.to_string())?;
    let position = window
        .outer_position()
        .map_err(|error| error.to_string())?
        .to_logical::<f64>(scale_factor);
    let size = window
        .inner_size()
        .map_err(|error| error.to_string())?
        .to_logical::<f64>(scale_factor);
    let state = app.state::<Mutex<AppState>>();
    let mut state = state.lock().map_err(|error| error.to_string())?;
    if state.cfg.usage_widget.minimized {
        return Ok(());
    }
    state.cfg.usage_widget.x = Some(position.x);
    state.cfg.usage_widget.y = Some(position.y);
    state.cfg.usage_widget.width = size.width;
    state.cfg.usage_widget.height = size.height;
    config::save_config(&config::config_path(), &state.cfg).map_err(|error| error.to_string())
}

fn schedule_save_widget_bounds(app: AppHandle<Wry>, save_generation: Arc<AtomicU64>) {
    let generation = save_generation.fetch_add(1, Ordering::Relaxed) + 1;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(BOUNDS_SAVE_DELAY).await;
        if save_generation.load(Ordering::Relaxed) == generation {
            let _ = save_widget_bounds(&app);
        }
    });
}

pub fn open_usage_widget(app: &AppHandle<Wry>) -> Result<WebviewWindow<Wry>, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        if WIDGET_IS_TRANSPARENT.load(Ordering::Acquire)
            != widget_taskbar::is_taskbar_compact_widget(app)
        {
            recreate_usage_widget(app, WIDGET_COMPACT_DEFAULT_HEIGHT)?;
            return app
                .get_webview_window(LABEL)
                .ok_or_else(|| "usage widget recreation failed".to_string());
        }
        window.show().map_err(|error| error.to_string())?;
        widget_taskbar::apply_taskbar_theme(app, window.clone());
        return Ok(window);
    }

    let cfg = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cfg
        .usage_widget
        .clone();
    let transparent = cfg.minimized && cfg.compact_position == CompactPosition::Taskbar;
    let bounds = if cfg.minimized {
        let (display, work_area, _) = widget_taskbar::display_areas(app)?;
        compact_bottom_right_bounds(
            WIDGET_COMPACT_DEFAULT_HEIGHT,
            WIDGET_COMPACT_WIDTH,
            work_area,
            display,
        )
    } else {
        restore_widget_bounds(app)?
    };
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("widget.html".into()))
        .position(bounds.x, bounds.y)
        .inner_size(bounds.width, bounds.height)
        .min_inner_size(
            if cfg.minimized {
                WIDGET_COMPACT_WIDTH
            } else {
                WIDGET_MIN_WIDTH
            },
            if cfg.minimized {
                WIDGET_COMPACT_MIN_HEIGHT
            } else {
                WIDGET_MIN_HEIGHT
            },
        )
        .resizable(!cfg.minimized)
        .decorations(false)
        .always_on_top(cfg.always_on_top)
        .skip_taskbar(true)
        .minimizable(false)
        .maximizable(false)
        .fullscreen(false)
        .title("LazySwitch Usage")
        .transparent(transparent)
        .background_color(if transparent {
            Color(0, 0, 0, 0)
        } else {
            DEFAULT_WIDGET_BACKGROUND
        })
        .build()
        .map_err(|error| error.to_string())?;
    WIDGET_IS_TRANSPARENT.store(transparent, Ordering::Release);

    if cfg.minimized {
        if let Err(error) = widget_native::install_context_menu_hook(app, &window) {
            let _ = window.destroy();
            return Err(error);
        }
    }
    let save_generation = Arc::new(AtomicU64::new(0));
    let event_app = app.clone();
    let event_window = window.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
            schedule_save_widget_bounds(event_app.clone(), Arc::clone(&save_generation));
        }
        if matches!(event, WindowEvent::ThemeChanged(_)) {
            widget_taskbar::apply_taskbar_theme(&event_app, event_window.clone());
        }
        if matches!(
            event,
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
        ) {
            let _ = widget_native::remove_context_menu_hook(&event_window);
        }
    });
    let menu_app = app.clone();
    window.on_menu_event(move |_window, event| {
        let _ = handle_widget_context_menu(&menu_app, event.id().as_ref());
    });
    let timer_app = app.clone();
    let timer_window = window.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TOPMOST_REASSERT_DELAY).await;
            let Some(current) = timer_app.get_webview_window(LABEL) else {
                break;
            };
            let should_reassert = timer_app
                .state::<Mutex<AppState>>()
                .lock()
                .map(|state| {
                    state.cfg.usage_widget.always_on_top
                        && state.cfg.usage_widget.minimized
                        && state.cfg.usage_widget.compact_position == CompactPosition::Taskbar
                })
                .unwrap_or(false);
            let (Ok(current_hwnd), Ok(timer_hwnd)) = (current.hwnd(), timer_window.hwnd()) else {
                break;
            };
            if current_hwnd != timer_hwnd {
                break;
            }
            if should_reassert && !widget_native::context_menu_open() {
                let _ = widget_native::reassert_topmost(&timer_window);
            }
        }
    });
    if cfg.minimized {
        let position_app = app.clone();
        let position_window = window.clone();
        tauri::async_runtime::spawn(async move {
            let _ = widget_taskbar::position_compact_widget(
                position_app,
                position_window,
                WIDGET_COMPACT_DEFAULT_HEIGHT,
            )
            .await;
        });
    }
    widget_taskbar::apply_taskbar_theme(app, window.clone());
    Ok(window)
}

fn recreate_usage_widget(app: &AppHandle<Wry>, compact_height: f64) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        widget_native::remove_context_menu_hook(&window).ok();
        window.destroy().map_err(|error| error.to_string())?;
    }
    let window = open_usage_widget(app)?;
    if widget_taskbar::is_taskbar_compact_widget(app) {
        let position_app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ =
                widget_taskbar::position_compact_widget(position_app, window, compact_height).await;
        });
    }
    Ok(())
}

pub fn apply_widget_minimized(
    app: &AppHandle<Wry>,
    minimized: bool,
    compact_height: f64,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    if WIDGET_IS_TRANSPARENT.load(Ordering::Acquire)
        != widget_taskbar::is_taskbar_compact_widget(app)
    {
        return recreate_usage_widget(app, compact_height);
    }
    if minimized {
        widget_native::install_context_menu_hook(app, &window)?;
        window
            .set_min_size(Some(LogicalSize::new(
                WIDGET_COMPACT_WIDTH,
                WIDGET_COMPACT_MIN_HEIGHT,
            )))
            .map_err(|error| error.to_string())?;
        window
            .set_resizable(false)
            .map_err(|error| error.to_string())?;
        widget_taskbar::apply_taskbar_theme(app, window.clone());
        let position_app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ =
                widget_taskbar::position_compact_widget(position_app, window, compact_height).await;
        });
    } else {
        widget_taskbar::send_taskbar_theme(&window, None);
        widget_native::remove_context_menu_hook(&window).ok();
        window
            .set_min_size(Some(LogicalSize::new(WIDGET_MIN_WIDTH, WIDGET_MIN_HEIGHT)))
            .map_err(|error| error.to_string())?;
        window
            .set_resizable(true)
            .map_err(|error| error.to_string())?;
        let bounds = restore_widget_bounds(app)?;
        window
            .set_size(LogicalSize::new(bounds.width, bounds.height))
            .map_err(|error| error.to_string())?;
        window
            .set_position(LogicalPosition::new(bounds.x, bounds.y))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn show_widget_context_menu(app: &AppHandle<Wry>) -> Result<(), String> {
    let minimized = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cfg
        .usage_widget
        .minimized;
    let Some(window) = app.get_webview_window(LABEL).filter(|_| minimized) else {
        return Ok(());
    };
    let lang = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())
        .map(|state| resolve_lang(&state.cfg.language))?;
    let menu = MenuBuilder::new(app)
        .text(CONTEXT_SETTINGS_ID, t(lang, "widget.settings", &[]))
        .text(CONTEXT_RESTORE_ID, t(lang, "widget.maximize", &[]))
        .text(CONTEXT_CLOSE_ID, t(lang, "widget.close", &[]))
        .build()
        .map_err(|error| error.to_string())?;
    window.popup_menu(&menu).map_err(|error| error.to_string())
}

fn handle_widget_context_menu(app: &AppHandle<Wry>, id: &str) -> Result<(), String> {
    match id {
        CONTEXT_SETTINGS_ID => {
            crate::windows::widget_settings::open_widget_settings(app)
                .map_err(|error| error.to_string())?;
        }
        CONTEXT_RESTORE_ID => {
            {
                let state = app.state::<Mutex<AppState>>();
                let mut state = state.lock().map_err(|error| error.to_string())?;
                state.cfg.usage_widget.minimized = false;
                config::save_config(&config::config_path(), &state.cfg)
                    .map_err(|error| error.to_string())?;
            }
            apply_widget_minimized(app, false, WIDGET_COMPACT_DEFAULT_HEIGHT)?;
            crate::limit_handler::broadcast_changed(app);
            crate::tray::refresh_tray(app)?;
        }
        CONTEXT_CLOSE_ID => set_usage_widget_enabled(app, false)?,
        _ => {}
    }
    Ok(())
}

pub fn close_usage_widget(app: &AppHandle<Wry>) -> Result<(), String> {
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    save_widget_bounds(app)?;
    widget_native::remove_context_menu_hook(&window).ok();
    window.close().map_err(|error| error.to_string())
}

pub fn has_enrolled_accounts() -> bool {
    ProviderId::ALL
        .into_iter()
        .any(|provider_id| !provider::list_accounts(provider_id).is_empty())
}

pub fn is_onboarding(app: &AppHandle<Wry>) -> bool {
    app.get_webview_window(ONBOARDING_LABEL).is_some()
}

pub fn sync_usage_widget(app: &AppHandle<Wry>) -> Result<(), String> {
    let enabled = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cfg
        .usage_widget
        .enabled;
    if enabled && has_enrolled_accounts() && !is_onboarding(app) {
        open_usage_widget(app)?;
    } else {
        close_usage_widget(app)?;
    }
    Ok(())
}

pub fn set_usage_widget_enabled(app: &AppHandle<Wry>, enabled: bool) -> Result<(), String> {
    {
        let state = app.state::<Mutex<AppState>>();
        let mut state = state.lock().map_err(|error| error.to_string())?;
        state.cfg.usage_widget.enabled = enabled;
        config::save_config(&config::config_path(), &state.cfg)
            .map_err(|error| error.to_string())?;
    }
    sync_usage_widget(app)?;
    crate::tray::refresh_tray(app)
}

#[tauri::command]
pub fn widget_close(app: AppHandle) -> Result<(), String> {
    set_usage_widget_enabled(&app, false)
}

#[tauri::command]
pub fn widget_compact_height(
    app: AppHandle,
    window: WebviewWindow,
    height: f64,
) -> Result<(), String> {
    if window.label() != LABEL || !height.is_finite() {
        return Ok(());
    }
    let minimized = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cfg
        .usage_widget
        .minimized;
    if minimized {
        let next_height = height.round().max(WIDGET_COMPACT_MIN_HEIGHT);
        tauri::async_runtime::spawn(async move {
            let _ = widget_taskbar::position_compact_widget(app, window, next_height).await;
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: DisplayArea = DisplayArea {
        x: 0.0,
        y: 0.0,
        width: 1_920.0,
        height: 1_080.0,
    };
    const WORK_AREA: DisplayArea = DisplayArea {
        x: 0.0,
        y: 0.0,
        width: 1_920.0,
        height: 1_040.0,
    };

    #[test]
    fn default_bounds_use_saved_coordinates_when_present() {
        let bounds = widget_default_bounds(
            WidgetBounds {
                x: 120.0,
                y: 80.0,
                width: 354.0,
                height: 563.0,
            },
            WORK_AREA,
        );

        assert_eq!(bounds.x, 120.0);
        assert_eq!(bounds.y, 80.0);
    }

    #[test]
    fn default_bounds_fall_back_to_work_area_bottom_right() {
        let bounds = widget_default_bounds(
            WidgetBounds {
                x: f64::NAN,
                y: f64::NAN,
                width: 354.0,
                height: 563.0,
            },
            WORK_AREA,
        );

        assert_eq!(
            bounds,
            WidgetBounds {
                x: 1_566.0,
                y: 477.0,
                width: 354.0,
                height: 563.0,
            }
        );
    }

    #[test]
    fn clamp_bounds_keeps_window_inside_display() {
        let bounds = clamp_widget_bounds(
            WidgetBounds {
                x: 1_800.0,
                y: -40.0,
                width: 354.0,
                height: 563.0,
            },
            DISPLAY,
        );

        assert_eq!(bounds.x, 1_566.0);
        assert_eq!(bounds.y, 0.0);
    }

    #[test]
    fn clamp_bounds_shrinks_window_larger_than_display() {
        let bounds = clamp_widget_bounds(
            WidgetBounds {
                x: -100.0,
                y: -100.0,
                width: 2_400.0,
                height: 1_200.0,
            },
            DISPLAY,
        );

        assert_eq!(
            bounds,
            WidgetBounds {
                x: 0.0,
                y: 0.0,
                width: 1_920.0,
                height: 1_080.0,
            }
        );
    }
}
