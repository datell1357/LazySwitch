use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::ProviderPrefs;
use super::{user_home, CoreError};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWidgetConfig {
    pub enabled: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: f64,
    pub height: f64,
    pub always_on_top: bool,
    pub compact_position: String,
    pub minimized: bool,
    pub hidden_accounts: Vec<String>,
    pub click_through: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub language: String,
    pub launch_at_login: bool,
    pub onboarded: bool,
    pub codex: ProviderPrefs,
    pub claude: ProviderPrefs,
    pub usage_widget: UsageWidgetConfig,
}

fn provider_defaults(claude: bool) -> ProviderPrefs {
    ProviderPrefs {
        auto_approve: false,
        auto_restart_cli: true,
        desktop_app_path: String::new(),
        desktop_process_name: if claude {
            String::new()
        } else if cfg!(windows) {
            "Codex.exe".into()
        } else {
            "Codex".into()
        },
        rotation_order: Vec::new(),
        primary_min_left_pct: 5.0,
        weekly_min_left_pct: 1.0,
        poll_interval_sec: if claude { 300 } else { 30 },
    }
}

pub fn defaults() -> AppConfig {
    AppConfig {
        language: String::new(),
        launch_at_login: true,
        onboarded: false,
        codex: provider_defaults(false),
        claude: provider_defaults(true),
        usage_widget: UsageWidgetConfig {
            enabled: true,
            x: None,
            y: None,
            width: 354.0,
            height: 563.0,
            always_on_top: true,
            compact_position: "taskbar".into(),
            minimized: false,
            hidden_accounts: Vec::new(),
            click_through: false,
        },
    }
}

pub fn config_path() -> PathBuf {
    #[cfg(windows)]
    let dir = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join("AppData").join("Roaming"));
    #[cfg(not(windows))]
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".config"));
    dir.join("lazyswitch").join("config.json")
}

fn merge_object(default: Value, raw: Option<&Value>) -> Value {
    let mut out = default;
    if let (Value::Object(dst), Some(Value::Object(src))) = (&mut out, raw) {
        for (key, value) in src {
            dst.insert(key.clone(), value.clone());
        }
    }
    out
}

pub fn migrate(raw: &Value) -> Value {
    let Some(object) = raw.as_object() else {
        return Value::Object(Default::default());
    };
    let defaults = defaults();
    let default_value = serde_json::to_value(defaults).unwrap_or(Value::Null);
    let mut out = serde_json::Map::new();
    if let Some(Value::String(language)) = object.get("language") {
        out.insert("language".into(), Value::String(language.clone()));
    }
    if let Some(Value::Bool(launch)) = object.get("launchAtLogin") {
        out.insert("launchAtLogin".into(), Value::Bool(*launch));
    }
    out.insert(
        "onboarded".into(),
        object
            .get("onboarded")
            .filter(|v| v.is_boolean())
            .cloned()
            .unwrap_or(Value::Bool(true)),
    );

    let legacy_keys = [
        "autoApprove",
        "autoRestartCli",
        "desktopAppPath",
        "desktopProcessName",
        "rotationOrder",
        "primaryMinLeftPct",
        "weeklyMinLeftPct",
        "pollIntervalSec",
    ];
    let mut legacy = serde_json::Map::new();
    for key in legacy_keys {
        if let Some(value) = object.get(key) {
            legacy.insert(key.into(), value.clone());
        }
    }
    let codex = merge_object(
        merge_object(default_value["codex"].clone(), Some(&Value::Object(legacy))),
        object.get("codex"),
    );
    out.insert("codex".into(), codex);
    out.insert(
        "claude".into(),
        merge_object(default_value["claude"].clone(), object.get("claude")),
    );
    let mut widget = merge_object(
        default_value["usageWidget"].clone(),
        object.get("usageWidget"),
    );
    if !matches!(
        widget.get("compactPosition").and_then(Value::as_str),
        Some("taskbar" | "bottom-right" | "bottom-left")
    ) {
        widget["compactPosition"] = Value::String("taskbar".into());
    }
    out.insert("usageWidget".into(), widget);
    Value::Object(out)
}

pub fn load_config_at(path: &Path) -> AppConfig {
    let bytes = std::fs::read(path);
    let Ok(bytes) = bytes else { return defaults() };
    let text = String::from_utf8_lossy(&bytes)
        .trim_start_matches('\u{feff}')
        .to_owned();
    let Ok(raw) = serde_json::from_str::<Value>(&text) else {
        return defaults();
    };
    let migrated = migrate(&raw);
    serde_json::from_value(migrated).unwrap_or_else(|_| defaults())
}

pub fn load_config() -> AppConfig {
    load_config_at(&config_path())
}

pub fn save_config_at(path: &Path, config: &AppConfig) -> Result<(), CoreError> {
    let data = serde_json::to_vec_pretty(config)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)?;
    Ok(())
}

pub fn save_config(config: &AppConfig) -> Result<(), CoreError> {
    save_config_at(&config_path(), config)
}
