use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, PhysicalPosition, PhysicalRect, Runtime};

use crate::app_state::AppState;
use crate::config;
use crate::i18n::{resolve_lang, t};
use crate::provider;
use crate::provider_types::ProviderId;
use crate::windows::{manager, onboarding, widget};

const TRAY_ID: &str = "main";
const TRAY_MENU_WIDTH: i32 = 352;
const TRAY_MENU_HEIGHT: i32 = 317;
const TRAY_MENU_GAP: i32 = 8;

#[rustfmt::skip]
const LANGUAGE_IDS: [(&str, &str); 5] = [
    ("lang-system", ""), ("lang-ko", "ko"), ("lang-en", "en"), ("lang-ja", "ja"), ("lang-zh", "zh"),
];
type TrayRect = PhysicalRect<i32, u32>;
type TrayPosition = PhysicalPosition<i32>;

fn translated(state: &AppState, key: &str, vars: &[(&str, &str)]) -> String {
    t(resolve_lang(&state.cfg.language), key, vars)
}

fn language_item<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    label: &str,
    value: &str,
    selected: &str,
) -> tauri::Result<CheckMenuItem<R>> {
    CheckMenuItem::with_id(app, id, label, true, selected == value, None::<&str>)
}

pub fn build_menu<R: Runtime>(app: &AppHandle<R>, state: &AppState) -> tauri::Result<Menu<R>> {
    macro_rules! text {
        ($id:expr, $key:expr) => {
            MenuItem::with_id(app, $id, translated(state, $key, &[]), true, None::<&str>)?
        };
    }
    macro_rules! check {
        ($id:expr, $label:expr, $checked:expr) => {
            CheckMenuItem::with_id(app, $id, $label, true, $checked, None::<&str>)?
        };
    }
    let manage = text!("manage", "tray.manage");
    let tutorial = text!("tutorial", "tray.tutorial");
    let auto_approve = check!(
        "auto-approve",
        translated(state, "tray.autoApprove", &[]),
        state.cfg.codex.auto_approve
    );
    let restart_label = |provider: ProviderId| {
        translated(
            state,
            "tray.autoRestartCli",
            &[("provider", provider.display_name())],
        )
    };
    let codex_auto_restart = check!(
        "codex-auto-restart",
        restart_label(ProviderId::Codex),
        state.cfg.codex.auto_restart_cli
    );
    let claude_auto_restart = check!(
        "claude-auto-restart",
        restart_label(ProviderId::Claude),
        state.cfg.claude.auto_restart_cli
    );
    let start_at_login = check!(
        "start-at-login",
        translated(state, "tray.startAtLogin", &[]),
        state.cfg.launch_at_login
    );
    let usage_widget = check!(
        "usage-widget",
        translated(state, "tray.usageWidget", &[]),
        state.cfg.usage_widget.enabled
    );

    let language_items = [
        language_item(
            app,
            LANGUAGE_IDS[0].0,
            &translated(state, "tray.langSystem", &[]),
            LANGUAGE_IDS[0].1,
            &state.cfg.language,
        )?,
        language_item(app, LANGUAGE_IDS[1].0, "한국어", "ko", &state.cfg.language)?,
        language_item(app, LANGUAGE_IDS[2].0, "English", "en", &state.cfg.language)?,
        language_item(app, LANGUAGE_IDS[3].0, "日本語", "ja", &state.cfg.language)?,
        language_item(app, LANGUAGE_IDS[4].0, "中文", "zh", &state.cfg.language)?,
    ];
    let language = Submenu::with_items(
        app,
        translated(state, "tray.language", &[]),
        true,
        &[
            &language_items[0],
            &language_items[1],
            &language_items[2],
            &language_items[3],
            &language_items[4],
        ],
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = text!("quit", "tray.quit");

    Menu::with_items(
        app,
        &[
            &manage,
            &tutorial,
            &auto_approve,
            &codex_auto_restart,
            &claude_auto_restart,
            &start_at_login,
            &usage_widget,
            &language,
            &separator,
            &quit,
        ],
    )
}

fn active_tooltip() -> String {
    ProviderId::ALL
        .into_iter()
        .filter_map(|id| {
            let active = provider::active_account_name(id)?;
            let display = provider::list_accounts(id)
                .into_iter()
                .find(|account| account.name == active)
                .and_then(|account| account.email)
                .unwrap_or_else(|| active.clone());
            Some(format!("{}: {display}", id.display_name()))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn refresh_tray<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();
    let state = state.lock().map_err(|error| error.to_string())?;
    let menu = build_menu(app, &state).map_err(|error| error.to_string())?;
    let tray = app
        .tray_by_id(TRAY_ID)
        .ok_or_else(|| "tray icon is not initialized".to_string())?;
    tray.set_menu(Some(menu))
        .map_err(|error| error.to_string())?;
    let tooltip = active_tooltip();
    tray.set_tooltip(Some(if tooltip.is_empty() {
        "LazySwitch"
    } else {
        &tooltip
    }))
    .map_err(|error| error.to_string())
}

pub fn tray_menu_position(
    icon: TrayRect,
    work_area: TrayRect,
    widget: Option<TrayRect>,
) -> Option<TrayPosition> {
    if icon.size.width == 0 && icon.size.height == 0 {
        return None;
    }
    let work_width = i32::try_from(work_area.size.width).unwrap_or(i32::MAX);
    let work_height = i32::try_from(work_area.size.height).unwrap_or(i32::MAX);
    let icon_width = i32::try_from(icon.size.width).unwrap_or(i32::MAX);
    let max_x = work_area.position.x + work_width - TRAY_MENU_WIDTH;
    let max_y = work_area.position.y + work_height - TRAY_MENU_HEIGHT;
    let clamp_x = |value: i32| value.clamp(work_area.position.x, max_x);
    let y = (icon.position.y - TRAY_MENU_HEIGHT).clamp(work_area.position.y, max_y);
    let centered = icon.position.x + icon_width / 2 - TRAY_MENU_WIDTH / 2;
    let mut x = clamp_x(centered);

    if let Some(widget) = widget {
        let widget_width = i32::try_from(widget.size.width).unwrap_or(i32::MAX);
        let widget_height = i32::try_from(widget.size.height).unwrap_or(i32::MAX);
        let overlaps = x < widget.position.x + widget_width
            && x + TRAY_MENU_WIDTH > widget.position.x
            && y < widget.position.y + widget_height
            && y + TRAY_MENU_HEIGHT > widget.position.y;
        if overlaps {
            x = clamp_x(widget.position.x - TRAY_MENU_GAP - TRAY_MENU_WIDTH);
        }
    }
    Some(PhysicalPosition::new(x, y))
}

fn bottom_right_compact_widget_rect() -> Option<TrayRect> {
    // TODO(window-layer): return the compact widget bounds when that window exists.
    None
}

fn save_state(state: &AppState) -> Result<(), String> {
    config::save_config(&config::config_path(), &state.cfg).map_err(|error| error.to_string())
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if id == "quit" {
        app.exit(0);
        return;
    }
    if id == "manage" {
        let _ = manager::open_manager(app);
        return;
    }
    if id == "tutorial" {
        let _ = onboarding::open_onboarding(app);
        return;
    }

    let state = app.state::<Mutex<AppState>>();
    let Ok(mut state) = state.lock() else {
        return;
    };
    match id {
        "auto-approve" => state.cfg.codex.auto_approve = !state.cfg.codex.auto_approve,
        "codex-auto-restart" => {
            state.cfg.codex.auto_restart_cli = !state.cfg.codex.auto_restart_cli
        }
        "claude-auto-restart" => {
            state.cfg.claude.auto_restart_cli = !state.cfg.claude.auto_restart_cli
        }
        "start-at-login" => {
            state.cfg.launch_at_login = !state.cfg.launch_at_login;
            // TODO(window-layer): apply via tauri-plugin-autostart.
        }
        "usage-widget" => {
            state.cfg.usage_widget.enabled = !state.cfg.usage_widget.enabled;
        }
        language_id => {
            let Some((_, language)) = LANGUAGE_IDS
                .iter()
                .find(|(item_id, _)| *item_id == language_id)
            else {
                return;
            };
            state.cfg.language = (*language).to_string();
        }
    }
    if save_state(&state).is_err() {
        return;
    }
    drop(state);
    if id == "usage-widget" {
        let _ = widget::sync_usage_widget(app);
    }
    let _ = refresh_tray(app);
}

pub fn setup(app: &mut App) -> tauri::Result<()> {
    let handle = app.handle().clone();
    let state = handle.state::<Mutex<AppState>>();
    let state = state
        .lock()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let menu = build_menu(&handle, &state)?;
    drop(state);

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../../assets/tray.png"
        ))?)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu_event(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => {
                let _ = manager::open_manager(tray.app_handle());
            }
            TrayIconEvent::Click {
                button: MouseButton::Right,
                ..
            } => {
                // TODO(window-layer): Tauri's tray menu has no public
                // screen-positioned popup API. Rewire this once the widget
                // slice provides a native owner window for `popup_menu_at`.
            }
            _ => {}
        })
        .build(app)?;
    refresh_tray(&handle).map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::{PhysicalPosition, PhysicalSize};

    fn rect(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect<i32, u32> {
        PhysicalRect {
            position: PhysicalPosition::new(x, y),
            size: PhysicalSize::new(width, height),
        }
    }

    #[test]
    fn menu_position_centers_above_icon_and_clamps_to_work_area() {
        let work_area = rect(100, 50, 1_000, 700);

        assert_eq!(
            tray_menu_position(rect(600, 700, 24, 24), work_area, None),
            Some(PhysicalPosition::new(436, 383))
        );
        assert_eq!(
            tray_menu_position(rect(100, 60, 24, 24), work_area, None),
            Some(PhysicalPosition::new(100, 50))
        );
    }

    #[test]
    fn menu_position_moves_left_to_avoid_compact_widget() {
        let position = tray_menu_position(
            rect(900, 700, 24, 24),
            rect(0, 0, 1_200, 800),
            Some(rect(800, 650, 280, 70)),
        );

        assert_eq!(position, Some(PhysicalPosition::new(440, 383)));
    }

    #[test]
    fn zero_sized_icon_has_no_position() {
        assert_eq!(
            tray_menu_position(rect(0, 0, 0, 0), rect(0, 0, 1_200, 800), None),
            None
        );
    }
}
