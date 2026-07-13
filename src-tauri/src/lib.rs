use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Scaffold probe: the renderer reports what it loaded, so the port can be
/// verified without screen-grabbing the desktop. Removed once the real IPC
/// surface (window.rotator) lands.
#[tauri::command]
fn scaffold_probe(app: tauri::AppHandle, report: String) {
    if let Ok(path) = std::env::var("LAZYSWITCH_SCAFFOLD_PROBE") {
        let _ = std::fs::write(path, &report);
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        handle.exit(0);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![scaffold_probe])
        .setup(|app| {
            let window = WebviewWindowBuilder::new(
                app.handle(),
                "manager",
                WebviewUrl::App("manager.html".into()),
            )
            .title("Accounts")
            .inner_size(920.0, 730.0)
            .decorations(false)
            .background_color(tauri::window::Color(0x16, 0x17, 0x1b, 0xff))
            .build()?;

            if std::env::var("LAZYSWITCH_SCAFFOLD_PROBE").is_ok() {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(2500));
                    let _ = window.eval(
                        r#"window.__TAURI__.core.invoke('scaffold_probe', { report: JSON.stringify({
                            tauriGlobal: typeof window.__TAURI__,
                            rotator: typeof window.rotator,
                            columns: document.querySelectorAll('.col').length,
                            addButtons: [...document.querySelectorAll('.add button')].map(b => b.id),
                            titlebar: document.querySelector('#tbTitle') ? document.querySelector('#tbTitle').textContent : null,
                            location: location.href,
                        }) });"#,
                    );
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
