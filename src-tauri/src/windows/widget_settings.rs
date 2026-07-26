use tauri::window::Color;
use tauri::{
    webview::PageLoadEvent, AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

const LABEL: &str = "widget-settings";
const WIDTH: f64 = 320.0;
const HEIGHT: f64 = 400.0;

pub fn open_widget_settings<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.show()?;
        window.set_focus()?;
        return Ok(window);
    }

    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("widget-settings.html".into()))
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .center()
        .title("Settings")
        .background_color(Color(0x16, 0x17, 0x1b, 0xff))
        .visible(false)
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let _ = window.show();
                let _ = window.set_focus();
            }
        })
        .build()
}

#[tauri::command]
pub fn widget_settings_close(window: WebviewWindow) -> Result<(), String> {
    if window.label() != LABEL {
        return Err("widget settings window required".to_string());
    }
    window.close().map_err(|error| error.to_string())
}
