use tauri::window::Color;
use tauri::{
    webview::PageLoadEvent, AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

const LABEL: &str = "manager";

fn restore_show_focus<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    if window.is_minimized()? {
        window.unminimize()?;
    }
    window.show()?;
    window.set_focus()
}

pub fn open_manager<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    if let Some(window) = app.get_webview_window(LABEL) {
        restore_show_focus(&window)?;
        return Ok(window);
    }

    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("manager.html".into()))
        .inner_size(920.0, 730.0)
        .resizable(true)
        .decorations(false)
        .title("Accounts")
        .background_color(Color(0x16, 0x17, 0x1b, 0xff))
        .visible(false)
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let _ = restore_show_focus(&window);
            }
        })
        .build()
}
