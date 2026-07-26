use std::sync::Mutex;

use tauri::webview::PageLoadEvent;
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime, State, WebviewUrl,
    WebviewWindowBuilder, Wry,
};

use crate::app_notify::{
    AppNotifyPayload, Bounds, ToastState, ToastWindowFactory, ToastWindowId, WorkArea,
    DEFAULT_HEIGHT, TOAST_WIDTH,
};

const LABEL_PREFIX: &str = "app-notify";

pub struct TauriToastWindowFactory<R: Runtime> {
    app: AppHandle<R>,
    next_id: u64,
}

impl<R: Runtime> TauriToastWindowFactory<R> {
    pub const fn new(app: AppHandle<R>) -> Self {
        Self { app, next_id: 0 }
    }

    fn label(id: ToastWindowId) -> String {
        format!("{LABEL_PREFIX}-{}", id.0)
    }
}

impl<R: Runtime> ToastWindowFactory for TauriToastWindowFactory<R> {
    fn create(&mut self, payload: &AppNotifyPayload) -> Result<ToastWindowId, String> {
        self.next_id += 1;
        let id = ToastWindowId(self.next_id);
        let label = Self::label(id);
        WebviewWindowBuilder::new(&self.app, &label, WebviewUrl::App("notify.html".into()))
            .inner_size(f64::from(TOAST_WIDTH), f64::from(DEFAULT_HEIGHT))
            .resizable(false)
            .minimizable(false)
            .maximizable(false)
            .fullscreen(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focusable(false)
            .decorations(false)
            .transparent(true)
            .visible(false)
            .title(&payload.title)
            .on_page_load(|window, event| {
                if event.event() == PageLoadEvent::Finished {
                    let _ = window.show();
                }
            })
            .build()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn is_destroyed(&self, id: ToastWindowId) -> bool {
        self.app.get_webview_window(&Self::label(id)).is_none()
    }

    fn work_area(&self) -> Result<WorkArea, String> {
        let monitor = self
            .app
            .primary_monitor()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "primary monitor unavailable".to_string())?;
        let area = monitor.work_area();
        Ok(WorkArea {
            x: area.position.x,
            y: area.position.y,
            width: i32::try_from(area.size.width).map_err(|error| error.to_string())?,
            height: i32::try_from(area.size.height).map_err(|error| error.to_string())?,
        })
    }

    fn apply_bounds(&self, bounds: &[(ToastWindowId, Bounds)]) -> Result<(), String> {
        for (id, bounds) in bounds {
            let Some(window) = self.app.get_webview_window(&Self::label(*id)) else {
                continue;
            };
            window
                .set_size(PhysicalSize::new(
                    u32::try_from(bounds.width).map_err(|error| error.to_string())?,
                    u32::try_from(bounds.height).map_err(|error| error.to_string())?,
                ))
                .map_err(|error| error.to_string())?;
            window
                .set_position(PhysicalPosition::new(bounds.x, bounds.y))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

pub struct NotifyRuntime {
    pub state: ToastState,
    pub windows: TauriToastWindowFactory<Wry>,
}

impl NotifyRuntime {
    pub fn new(app: AppHandle<Wry>) -> Self {
        Self {
            state: ToastState::default(),
            windows: TauriToastWindowFactory::new(app),
        }
    }
}

pub fn show_app_notification(
    runtime: &Mutex<NotifyRuntime>,
    payload: AppNotifyPayload,
) -> Result<(), String> {
    let mut runtime = runtime.lock().map_err(|error| error.to_string())?;
    let NotifyRuntime { state, windows } = &mut *runtime;
    state.show(payload, true, false, windows);
    Ok(())
}

#[tauri::command]
pub fn app_notify_payload(
    window: tauri::WebviewWindow,
    runtime: State<'_, Mutex<NotifyRuntime>>,
) -> Result<Option<AppNotifyPayload>, String> {
    let Some(id) = toast_id(window.label()) else {
        return Ok(None);
    };
    runtime
        .lock()
        .map(|runtime| runtime.state.payload(id).cloned())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn app_notify_resize(
    window: tauri::WebviewWindow,
    height: f64,
    runtime: State<'_, Mutex<NotifyRuntime>>,
) -> Result<(), String> {
    let Some(id) = toast_id(window.label()) else {
        return Ok(());
    };
    let mut runtime = runtime.lock().map_err(|error| error.to_string())?;
    let NotifyRuntime { state, windows } = &mut *runtime;
    state.resize(ToastWindowId(id), height);
    state.position(windows);
    Ok(())
}

#[tauri::command]
pub fn app_notify_dismiss(
    window: tauri::WebviewWindow,
    runtime: State<'_, Mutex<NotifyRuntime>>,
) -> Result<(), String> {
    let Some(id) = toast_id(window.label()) else {
        return Ok(());
    };
    window.close().map_err(|error| error.to_string())?;
    let mut runtime = runtime.lock().map_err(|error| error.to_string())?;
    let NotifyRuntime { state, windows } = &mut *runtime;
    state.remove(id, false, windows);
    Ok(())
}

fn toast_id(label: &str) -> Option<u64> {
    label
        .strip_prefix(&format!("{LABEL_PREFIX}-"))
        .and_then(|id| id.parse().ok())
}
