use std::sync::Mutex;

use serde_json::Value;
use tauri::State;

use crate::app_state::AppState;
use crate::config::{self, AppConfig, CompactPosition};
use crate::i18n::{resolve_lang, Lang};

fn merge_object(base: &mut Value, patch: Option<&Value>) {
    let (Some(base), Some(patch)) = (base.as_object_mut(), patch.and_then(Value::as_object)) else {
        return;
    };
    for (key, value) in patch {
        base.insert(key.clone(), value.clone());
    }
}

pub fn merge_config(current: &AppConfig, patch: &Value) -> Result<AppConfig, String> {
    let mut merged = serde_json::to_value(current).map_err(|error| error.to_string())?;
    let Some(patch_object) = patch.as_object() else {
        return Ok(current.clone());
    };

    let current_codex = merged["codex"].clone();
    let current_claude = merged["claude"].clone();
    let current_widget = merged["usageWidget"].clone();
    merge_object(&mut merged, Some(patch));
    merged["codex"] = current_codex;
    merged["claude"] = current_claude;
    merged["usageWidget"] = current_widget;
    merge_object(&mut merged["codex"], patch_object.get("codex"));
    merge_object(&mut merged["claude"], patch_object.get("claude"));
    merge_object(&mut merged["usageWidget"], patch_object.get("usageWidget"));

    if serde_json::from_value::<CompactPosition>(merged["usageWidget"]["compactPosition"].clone())
        .is_err()
    {
        merged["usageWidget"]["compactPosition"] = Value::String("taskbar".into());
    }

    serde_json::from_value(merged).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn config_get(state: State<'_, Mutex<AppState>>) -> Result<AppConfig, String> {
    state
        .lock()
        .map(|state| state.cfg.clone())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn config_set(state: State<'_, Mutex<AppState>>, patch: Value) -> Result<AppConfig, String> {
    let mut state = state.lock().map_err(|error| error.to_string())?;
    let previous = state.cfg.clone();
    let next = merge_config(&previous, &patch)?;

    // TODO(window-layer): capture bounds before minimizing, then synchronize
    // widget visibility, compact positioning, and always-on-top state.
    sync_usage_widget_stub(&previous, &next, &patch);
    // TODO(window-layer): restart provider monitors when pollIntervalSec changes.
    restart_monitors_stub(&previous, &next);
    // TODO(window-layer): apply launch-at-login via tauri-plugin-autostart.
    apply_launch_at_login_stub(&previous, &next, &patch);

    config::save_config(&config::config_path(), &next).map_err(|error| error.to_string())?;
    state.cfg = next.clone();
    Ok(next)
}

#[tauri::command]
pub fn lang_get(state: State<'_, Mutex<AppState>>) -> Result<&'static str, String> {
    state
        .lock()
        .map(|state| match resolve_lang(&state.cfg.language) {
            Lang::Ko => "ko",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Zh => "zh",
        })
        .map_err(|error| error.to_string())
}

fn sync_usage_widget_stub(_previous: &AppConfig, _next: &AppConfig, _patch: &Value) {}

fn restart_monitors_stub(_previous: &AppConfig, _next: &AppConfig) {}

fn apply_launch_at_login_stub(_previous: &AppConfig, _next: &AppConfig, _patch: &Value) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_patch_merges_nested_sections_without_erasing_siblings() {
        let current = AppConfig::default();

        let next = merge_config(
            &current,
            &json!({
                "language": "ko",
                "codex": { "pollIntervalSec": 45 },
                "usageWidget": { "minimized": true }
            }),
        )
        .unwrap();

        assert_eq!(next.language, "ko");
        assert_eq!(next.codex.poll_interval_sec, 45);
        assert_eq!(next.codex.auto_restart_cli, current.codex.auto_restart_cli);
        assert!(next.usage_widget.minimized);
        assert_eq!(next.usage_widget.width, current.usage_widget.width);
    }

    #[test]
    fn invalid_compact_position_falls_back_to_taskbar() {
        let next = merge_config(
            &AppConfig::default(),
            &json!({ "usageWidget": { "compactPosition": "upper-left" } }),
        )
        .unwrap();

        assert_eq!(next.usage_widget.compact_position, CompactPosition::Taskbar);
    }

    #[test]
    fn non_object_patch_leaves_config_unchanged() {
        let current = AppConfig::default();

        assert_eq!(merge_config(&current, &Value::Null).unwrap(), current);
    }
}
