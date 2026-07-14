#![allow(linker_messages)]

pub mod app;
pub mod core;

use std::time::Duration;

use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;

use crate::app::commands::*;
use crate::app::monitor::{install_cli_hooks_on_launch, start_monitors};
use crate::app::probe::open_probe_windows;
use crate::app::state::{build_state, state_config, AppState};
use crate::app::tray::{refresh_tray_tooltip, show_tray_menu, tray_menu_action};
use crate::app::updater::check_silently;
use crate::app::windows::{open_manager, open_onboarding, sync_usage_widget};
use crate::core::platform;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if !platform::acquire_single_instance() {
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(build_state())
        .invoke_handler(tauri::generate_handler![
            providers_list,
            usage_history,
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
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or_else(|| "default tray icon is unavailable".to_owned())?;
            TrayIconBuilder::with_id("main")
                .icon(icon)
                .tooltip("LazySwitch")
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| tray_menu_action(app, event.id().as_ref()))
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    match event {
                        TrayIconEvent::DoubleClick { .. } => open_manager(app),
                        TrayIconEvent::Click {
                            button: MouseButton::Right,
                            button_state: MouseButtonState::Down,
                            rect,
                            ..
                        } => show_tray_menu(app, rect),
                        _ => {}
                    }
                })
                .build(app)?;
            refresh_tray_tooltip(app.handle());
            install_cli_hooks_on_launch();
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
                if let Ok(exe) = std::env::current_exe() {
                    let _ = platform::apply_launch_at_login(state_config(&state).launch_at_login, &exe);
                }
            }
            start_monitors(app.handle());
            if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_none() {
                let updater_app = app.handle().clone();
                tauri::async_runtime::spawn(check_silently(updater_app));
            }
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
