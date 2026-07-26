use tauri::window::Color;
use tauri::{
    webview::PageLoadEvent, AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};

const LABEL: &str = "onboarding";

fn restore_show_focus<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    if window.is_minimized()? {
        window.unminimize()?;
    }
    window.show()?;
    window.set_focus()
}

pub fn open_onboarding<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    if let Some(window) = app.get_webview_window(LABEL) {
        restore_show_focus(&window)?;
        return Ok(window);
    }

    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("onboarding.html".into()))
        .inner_size(640.0, 560.0)
        .resizable(false)
        .decorations(false)
        .title("LazySwitch")
        .background_color(Color(0x16, 0x17, 0x1b, 0xff))
        .visible(false)
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let _ = restore_show_focus(&window);
            }
        })
        .build()?;

    window.on_window_event(|event| {
        if matches!(event, WindowEvent::Destroyed) {
            // TODO(window-layer): sync usage widget.
        }
    });
    // TODO(window-layer): sync usage widget while onboarding is open.
    Ok(window)
}
