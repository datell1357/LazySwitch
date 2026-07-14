use std::collections::HashMap;

use tauri::menu::{CheckMenuItem, ContextMenu, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager};

use crate::core::config::AppConfig;
use crate::core::i18n;
use crate::core::platform;

use super::commands::widget_close;
use super::state::{state_config, AppState};
use super::windows::{open_manager, open_onboarding, open_widget, open_widget_settings};

pub fn tray_lang(state: &AppState) -> i18n::Lang {
    let config = state_config(state);
    i18n::resolve_lang(&config.language, std::env::var("LANG").ok().as_deref())
}

pub fn tray_text(state: &AppState, key: &str, vars: &[(&str, String)]) -> String {
    let vars = vars
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect::<HashMap<_, _>>();
    i18n::t(tray_lang(state), key, Some(&vars))
}

pub fn refresh_tray_tooltip(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut active = Vec::new();
    for (id, display, _, _, provider) in state.providers.all() {
        if let Some(name) = provider.active_account_name() {
            active.push(format!("{display}: {name}"));
        }
        let _ = id;
    }
    let name = if active.is_empty() {
        "LazySwitch".into()
    } else {
        active.join(" · ")
    };
    let tooltip = tray_text(&state, "tray.tooltip", &[("name", name)]);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

pub fn update_app_config(app: &AppHandle, update: impl FnOnce(&mut AppConfig)) {
    let state = app.state::<AppState>();
    if let Ok(mut config) = state.config.lock() {
        update(&mut config);
        let _ = crate::core::config::save_config(&config);
    };
}

pub fn build_tray_menu(app: &AppHandle) -> Result<Menu<tauri::Wry>, String> {
    let state = app.state::<AppState>();
    let config = state_config(&state);
    let menu = Menu::new(app).map_err(|e| e.to_string())?;
    let manage = MenuItem::with_id(app, "tray.manage", tray_text(&state, "tray.manage", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    let tutorial = MenuItem::with_id(app, "tray.tutorial", tray_text(&state, "tray.tutorial", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    let auto = CheckMenuItem::with_id(app, "tray.autoApprove", tray_text(&state, "tray.autoApprove", &[]), true, config.codex.auto_approve, None::<&str>).map_err(|e| e.to_string())?;
    let codex_cli = CheckMenuItem::with_id(app, "tray.autoRestartCli.codex", tray_text(&state, "tray.autoRestartCli", &[("provider", "Codex".into())]), true, config.codex.auto_restart_cli, None::<&str>).map_err(|e| e.to_string())?;
    let claude_cli = CheckMenuItem::with_id(app, "tray.autoRestartCli.claude", tray_text(&state, "tray.autoRestartCli", &[("provider", "Claude Code".into())]), true, config.claude.auto_restart_cli, None::<&str>).map_err(|e| e.to_string())?;
    let widget = CheckMenuItem::with_id(app, "tray.usageWidget", tray_text(&state, "tray.usageWidget", &[]), true, config.usage_widget.enabled, None::<&str>).map_err(|e| e.to_string())?;
    let widget_settings = MenuItem::with_id(app, "tray.widgetSettings", tray_text(&state, "tray.widgetSettings", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    let login = CheckMenuItem::with_id(app, "tray.startAtLogin", tray_text(&state, "tray.startAtLogin", &[]), true, config.launch_at_login, None::<&str>).map_err(|e| e.to_string())?;
    let language = Submenu::with_id(app, "tray.language", tray_text(&state, "tray.language", &[]), true).map_err(|e| e.to_string())?;
    for (id, value, label) in [("system", "", "tray.langSystem"), ("ko", "ko", "한국어"), ("en", "en", "English"), ("ja", "ja", "日本語"), ("zh", "zh", "中文")] {
        let item = CheckMenuItem::with_id(app, format!("tray.language.{id}"), if label.starts_with("tray.") { tray_text(&state, label, &[]) } else { label.into() }, true, config.language == value, None::<&str>).map_err(|e| e.to_string())?;
        language.append(&item).map_err(|e| e.to_string())?;
    }
    let check_updates = MenuItem::with_id(app, "tray.checkUpdates", tray_text(&state, "tray.checkUpdates", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    let separator = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "tray.quit", tray_text(&state, "tray.quit", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
    for item in [&manage as &dyn tauri::menu::IsMenuItem<tauri::Wry>, &tutorial, &auto, &codex_cli, &claude_cli, &widget, &widget_settings, &login, &language, &check_updates, &separator, &quit] {
        menu.append(item).map_err(|e| e.to_string())?;
    }
    Ok(menu)
}

pub fn set_widget_context_menu_open(app: &AppHandle, value: bool) {
    if let Ok(mut runtime) = app.state::<AppState>().runtime.lock() {
        runtime.widget_context_menu_open = value;
    }
}

pub fn show_widget_context_menu(app: &AppHandle) {
    let Some(window) = app.get_webview_window("widget") else {
        return;
    };
    let config = state_config(&app.state::<AppState>());
    if !config.usage_widget.minimized {
        return;
    }
    let Ok(menu) = (|| -> Result<Menu<tauri::Wry>, String> {
        let menu = Menu::new(app).map_err(|e| e.to_string())?;
        let state = app.state::<AppState>();
        let settings = MenuItem::with_id(app, "widget.settings", tray_text(&state, "widget.settings", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
        let maximize = MenuItem::with_id(app, "widget.maximize", tray_text(&state, "widget.maximize", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
        let close = MenuItem::with_id(app, "widget.close", tray_text(&state, "widget.close", &[]), true, None::<&str>).map_err(|e| e.to_string())?;
        menu.append(&settings).map_err(|e| e.to_string())?;
        menu.append(&maximize).map_err(|e| e.to_string())?;
        menu.append(&close).map_err(|e| e.to_string())?;
        Ok(menu)
    })() else {
        return;
    };
    set_widget_context_menu_open(app, true);
    let _ = menu.popup(window.as_ref().window());
    set_widget_context_menu_open(app, false);
}

pub fn tray_menu_action(app: &AppHandle, id: &str) {
    match id {
        "tray.manage" => open_manager(app),
        "tray.tutorial" => open_onboarding(app),
        "tray.usageWidget" => {
            let state = app.state::<AppState>();
            if let Ok(mut config) = state.config.lock() {
                config.usage_widget.enabled = !config.usage_widget.enabled;
                let _ = crate::core::config::save_config(&config);
            }
            super::windows::sync_usage_widget(app);
        }
        "tray.widgetSettings" => open_widget_settings(app),
        "tray.autoApprove" => update_app_config(app, |config| config.codex.auto_approve = !config.codex.auto_approve),
        "tray.autoRestartCli.codex" | "tray.autoRestartCli.claude" => update_app_config(app, |config| {
            if id.ends_with("codex") {
                config.codex.auto_restart_cli = !config.codex.auto_restart_cli;
            } else {
                config.claude.auto_restart_cli = !config.claude.auto_restart_cli;
            }
        }),
        "tray.startAtLogin" => {
            update_app_config(app, |config| config.launch_at_login = !config.launch_at_login);
            #[cfg(windows)]
            if let Ok(exe) = std::env::current_exe() {
                let _ = platform::apply_launch_at_login(state_config(&app.state::<AppState>()).launch_at_login, &exe);
            }
        }
        "tray.checkUpdates" => {
            let app = app.clone();
            tauri::async_runtime::spawn(super::updater::check_and_install(app));
        }
        "tray.quit" => app.exit(0),
        id if id.starts_with("tray.language.") => {
            let value = id.rsplit('.').next().unwrap_or("");
            let value = if value == "system" { "" } else { value };
            update_app_config(app, |config| config.language = value.into());
        }
        "widget.settings" => open_widget_settings(app),
        "widget.maximize" => {
            let state = app.state::<AppState>();
            if let Ok(mut config) = state.config.lock() {
                config.usage_widget.minimized = false;
                let _ = crate::core::config::save_config(&config);
            }
            open_widget(app, true);
        }
        "widget.close" => widget_close(app.clone(), app.state::<AppState>()),
        _ => {}
    }
}

pub fn show_tray_menu(app: &AppHandle, rect: tauri::Rect) {
    let Ok(menu) = build_tray_menu(app) else { return };
    #[cfg(windows)]
    {
        let (left, top) = match rect.position {
            tauri::Position::Physical(p) => (p.x, p.y),
            tauri::Position::Logical(p) => (p.x as i32, p.y as i32),
        };
        let (width, height) = match rect.size {
            tauri::Size::Physical(s) => (s.width as i32, s.height as i32),
            tauri::Size::Logical(s) => (s.width as i32, s.height as i32),
        };
        let icon = platform::Rect { left, top, right: left + width, bottom: top + height };
        let widget = app
            .get_webview_window("widget")
            .and_then(|window| window.hwnd().ok())
            .and_then(|hwnd| platform::rect(hwnd.0 as isize));
        if let Some((x, y)) = platform::tray_menu_position(icon, widget) {
            let anchor = app
                .get_webview_window("manager")
                .or_else(|| app.get_webview_window("onboarding"))
                .or_else(|| app.get_webview_window("widget"));
            if let Some(anchor) = anchor {
                if let Ok(position) = anchor.outer_position() {
                    let _ = menu.popup_at(anchor.as_ref().window(), tauri::PhysicalPosition::new(x - position.x, y - position.y));
                }
            } else if let Ok(hwnd) = menu.hpopupmenu() {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::TrackPopupMenu(
                        windows::Win32::UI::WindowsAndMessaging::HMENU(hwnd as *mut _),
                        windows::Win32::UI::WindowsAndMessaging::TPM_LEFTALIGN
                            | windows::Win32::UI::WindowsAndMessaging::TPM_TOPALIGN,
                        x,
                        y,
                        Some(0),
                        windows::Win32::Foundation::HWND(std::ptr::null_mut()),
                        None,
                    );
                }
            }
            return;
        }
    }
    if let Some(anchor) = app
        .get_webview_window("manager")
        .or_else(|| app.get_webview_window("onboarding"))
        .or_else(|| app.get_webview_window("widget"))
    {
        let _ = menu.popup(anchor.as_ref().window());
    }
}
