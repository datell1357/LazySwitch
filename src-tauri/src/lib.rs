mod accounts;
mod app_notify;
mod app_state;
mod atomic_fs;
mod claude_sessions;
mod cli_cwd_script;
mod cli_handover;
mod cli_hooks;
mod cli_resume_routing;
mod cli_sessions;
mod codex_api;
mod codex_rollouts;
mod config;
mod desktop;
mod desktop_processes;
mod i18n;
mod ipc;
mod login;
mod monitor;
mod paths;
mod powershell;
mod provider;
mod provider_types;
mod providers;
mod switcher;
mod tray;
mod tray_pin;

use std::sync::Mutex;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let cfg = config::load_config(&config::config_path());
            app.manage(Mutex::new(app_state::AppState::new(cfg)));
            tray::setup(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::config_get,
            ipc::config_set,
            ipc::lang_get
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
