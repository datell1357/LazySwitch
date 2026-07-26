use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::window::Color;
use tauri::{
    AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use crate::app_state::AppState;
use crate::config;
use crate::provider;
use crate::provider_types::ProviderId;

const LABEL: &str = "usage-widget";
const ONBOARDING_LABEL: &str = "onboarding";
const DEFAULT_WIDGET_BACKGROUND: Color = Color(0x16, 0x17, 0x1b, 0xff);
const WIDGET_MIN_WIDTH: f64 = 300.0;
const WIDGET_MIN_HEIGHT: f64 = 260.0;
const BOUNDS_SAVE_DELAY: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidgetBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

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

pub fn clamp_widget_bounds(bounds: WidgetBounds, display: DisplayArea) -> WidgetBounds {
    let width = bounds.width.min(display.width);
    let height = bounds.height.min(display.height);
    WidgetBounds {
        x: bounds.x.clamp(display.x, display.x + display.width - width),
        y: bounds
            .y
            .clamp(display.y, display.y + display.height - height),
        width,
        height,
    }
}

fn restore_widget_bounds<R: Runtime>(app: &AppHandle<R>) -> Result<WidgetBounds, String> {
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

fn save_widget_bounds<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
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
    // TODO(widget-part2): skip saving while the widget is in compact mode.
    state.cfg.usage_widget.x = Some(position.x);
    state.cfg.usage_widget.y = Some(position.y);
    state.cfg.usage_widget.width = size.width;
    state.cfg.usage_widget.height = size.height;
    config::save_config(&config::config_path(), &state.cfg).map_err(|error| error.to_string())
}

fn schedule_save_widget_bounds<R: Runtime>(app: AppHandle<R>, save_generation: Arc<AtomicU64>) {
    let generation = save_generation.fetch_add(1, Ordering::Relaxed) + 1;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(BOUNDS_SAVE_DELAY).await;
        if save_generation.load(Ordering::Relaxed) == generation {
            let _ = save_widget_bounds(&app);
        }
    });
}

pub fn open_usage_widget<R: Runtime>(app: &AppHandle<R>) -> Result<WebviewWindow<R>, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.show().map_err(|error| error.to_string())?;
        return Ok(window);
    }

    let bounds = restore_widget_bounds(app)?;
    let always_on_top = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cfg
        .usage_widget
        .always_on_top;
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("widget.html".into()))
        .position(bounds.x, bounds.y)
        .inner_size(bounds.width, bounds.height)
        .min_inner_size(WIDGET_MIN_WIDTH, WIDGET_MIN_HEIGHT)
        .resizable(true)
        .decorations(false)
        .always_on_top(always_on_top)
        .skip_taskbar(true)
        .minimizable(false)
        .maximizable(false)
        .fullscreen(false)
        .title("LazySwitch Usage")
        .background_color(DEFAULT_WIDGET_BACKGROUND)
        .build()
        .map_err(|error| error.to_string())?;

    // TODO(widget-part2): compact construction, taskbar docking, context-menu
    // hook, transparency, and no-activate topmost reassertion belong to part 2.
    let save_generation = Arc::new(AtomicU64::new(0));
    let event_app = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
            schedule_save_widget_bounds(event_app.clone(), Arc::clone(&save_generation));
        }
    });
    Ok(window)
}

pub fn close_usage_widget<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    save_widget_bounds(app)?;
    window.close().map_err(|error| error.to_string())
}

pub fn has_enrolled_accounts() -> bool {
    ProviderId::ALL
        .into_iter()
        .any(|provider_id| !provider::list_accounts(provider_id).is_empty())
}

pub fn is_onboarding<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window(ONBOARDING_LABEL).is_some()
}

pub fn sync_usage_widget<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
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

pub fn set_usage_widget_enabled<R: Runtime>(
    app: &AppHandle<R>,
    enabled: bool,
) -> Result<(), String> {
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
