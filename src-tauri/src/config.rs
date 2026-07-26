use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPrefs {
    pub auto_approve: bool,
    pub auto_restart_cli: bool,
    pub desktop_app_path: String,
    pub desktop_process_name: String,
    pub rotation_order: Vec<String>,
    pub primary_min_left_pct: f64,
    pub weekly_min_left_pct: f64,
    pub poll_interval_sec: u64,
}

impl ProviderPrefs {
    fn codex_defaults() -> Self {
        ProviderPrefs {
            auto_approve: false,
            auto_restart_cli: true,
            desktop_app_path: String::new(),
            desktop_process_name: if cfg!(target_os = "windows") {
                "Codex.exe".to_string()
            } else {
                "Codex".to_string()
            },
            rotation_order: Vec::new(),
            primary_min_left_pct: 5.0,
            weekly_min_left_pct: 1.0,
            poll_interval_sec: 30,
        }
    }

    fn claude_defaults() -> Self {
        ProviderPrefs {
            // no desktop restart for Claude — kept for shape parity
            auto_approve: false,
            auto_restart_cli: true,
            desktop_app_path: String::new(),
            desktop_process_name: String::new(),
            rotation_order: Vec::new(),
            primary_min_left_pct: 5.0,
            weekly_min_left_pct: 1.0,
            // usage API rate-limits below ~5 min
            poll_interval_sec: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompactPosition {
    Taskbar,
    BottomRight,
    BottomLeft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWidgetConfig {
    /// The persistent usage widget is shown.
    pub enabled: bool,
    /// Last user-chosen position/size; null coordinates fall back to a default corner.
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: f64,
    pub height: f64,
    pub always_on_top: bool,
    pub compact_position: CompactPosition,
    pub minimized: bool,
    pub hidden_accounts: Vec<String>,
}

impl Default for UsageWidgetConfig {
    fn default() -> Self {
        UsageWidgetConfig {
            enabled: true,
            x: None,
            y: None,
            width: 354.0,
            height: 563.0,
            always_on_top: true,
            compact_position: CompactPosition::Taskbar,
            minimized: false,
            hidden_accounts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// UI language: "" = follow system; "ko" | "en" | "ja" | "zh".
    pub language: String,
    /// Launch automatically at OS login (registered on first run).
    pub launch_at_login: bool,
    /// The first-run tutorial has been completed; do not open it again.
    pub onboarded: bool,
    pub codex: ProviderPrefs,
    pub claude: ProviderPrefs,
    pub usage_widget: UsageWidgetConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            language: String::new(),
            launch_at_login: true,
            onboarded: false,
            codex: ProviderPrefs::codex_defaults(),
            claude: ProviderPrefs::claude_defaults(),
            usage_widget: UsageWidgetConfig::default(),
        }
    }
}

/// Legacy flat keys (pre multi-provider) that map into the codex section.
const LEGACY_CODEX_KEYS: &[&str] = &[
    "autoApprove",
    "autoRestartCli",
    "desktopAppPath",
    "desktopProcessName",
    "rotationOrder",
    "primaryMinLeftPct",
    "weeklyMinLeftPct",
    "pollIntervalSec",
];

fn merge_objects(base: &Value, overlay: &Value) -> Value {
    let mut result = base.clone();
    if let (Some(rm), Some(om)) = (result.as_object_mut(), overlay.as_object()) {
        for (k, v) in om {
            rm.insert(k.clone(), v.clone());
        }
    }
    result
}

/// Mirrors the original TS `migrate(raw)`: lifts legacy flat codex keys,
/// layers new-shape overrides on top of defaults, and sanitizes
/// `compactPosition`. Operates on loose JSON, like the original.
fn migrate(raw: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let raw_obj = raw.as_object();

    if let Some(lang) = raw_obj.and_then(|o| o.get("language")).and_then(|v| v.as_str()) {
        out.insert("language".into(), Value::String(lang.to_string()));
    }
    if let Some(b) = raw_obj
        .and_then(|o| o.get("launchAtLogin"))
        .and_then(|v| v.as_bool())
    {
        out.insert("launchAtLogin".into(), Value::Bool(b));
    }
    // An existing config predates the tutorial, so its owner has already set
    // the app up by hand — don't greet them with a first-run wizard on upgrade.
    let onboarded = raw_obj
        .and_then(|o| o.get("onboarded"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    out.insert("onboarded".into(), Value::Bool(onboarded));

    let mut legacy_codex = serde_json::Map::new();
    if let Some(o) = raw_obj {
        for k in LEGACY_CODEX_KEYS {
            if let Some(v) = o.get(*k) {
                legacy_codex.insert((*k).to_string(), v.clone());
            }
        }
    }

    let codex_defaults = serde_json::to_value(ProviderPrefs::codex_defaults()).unwrap();
    let mut codex = merge_objects(&codex_defaults, &Value::Object(legacy_codex));
    if let Some(c) = raw_obj.and_then(|o| o.get("codex")) {
        codex = merge_objects(&codex, c);
    }
    out.insert("codex".into(), codex);

    let claude_defaults = serde_json::to_value(ProviderPrefs::claude_defaults()).unwrap();
    let claude_raw = raw_obj
        .and_then(|o| o.get("claude"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    out.insert("claude".into(), merge_objects(&claude_defaults, &claude_raw));

    let widget_defaults = serde_json::to_value(UsageWidgetConfig::default()).unwrap();
    let widget_raw = raw_obj
        .and_then(|o| o.get("usageWidget"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let mut widget = merge_objects(&widget_defaults, &widget_raw);
    let valid_positions = ["taskbar", "bottom-right", "bottom-left"];
    let cp_ok = widget
        .get("compactPosition")
        .and_then(|v| v.as_str())
        .map(|s| valid_positions.contains(&s))
        .unwrap_or(false);
    if !cp_ok {
        widget["compactPosition"] = Value::String("taskbar".into());
    }
    out.insert("usageWidget".into(), widget);

    Value::Object(out)
}

/// Matches Electron's `app.getPath("userData")` for this app, which — absent
/// an explicit `app.setName()` call — resolves to the packaged productName
/// ("LazySwitch") under Roaming AppData. Kept path-compatible with existing
/// installs rather than using Tauri's default identifier-based app data dir.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .expect("no config directory")
        .join("LazySwitch")
        .join("config.json")
}

pub fn load_config(path: &Path) -> AppConfig {
    let parsed = fs::read_to_string(path)
        .ok()
        // Strip a UTF-8 BOM if present (e.g. config hand-written via PowerShell).
        .map(|raw| raw.trim_start_matches('\u{feff}').to_string())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());

    match parsed {
        Some(raw_val) => {
            let migrated = migrate(&raw_val);
            let defaults = serde_json::to_value(AppConfig::default()).unwrap();
            let merged = merge_objects(&defaults, &migrated);
            serde_json::from_value(merged).unwrap_or_default()
        }
        None => AppConfig::default(),
    }
}

pub fn save_config(path: &Path, cfg: &AppConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cfg)?;
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lazyswitch-config-test-{name}-{}.json", std::process::id()))
    }

    #[test]
    fn missing_file_returns_defaults() {
        let path = temp_path("missing");
        let _ = fs::remove_file(&path);
        assert_eq!(load_config(&path), AppConfig::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let mut cfg = AppConfig::default();
        cfg.language = "ko".to_string();
        cfg.codex.rotation_order = vec!["a".to_string(), "b".to_string()];
        cfg.usage_widget.x = Some(1005.0);
        cfg.usage_widget.compact_position = CompactPosition::BottomRight;

        save_config(&path, &cfg).unwrap();
        let loaded = load_config(&path);
        assert_eq!(loaded, cfg);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn legacy_flat_keys_lift_into_codex_section() {
        let path = temp_path("legacy");
        let raw = serde_json::json!({
            "language": "en",
            "autoApprove": true,
            "pollIntervalSec": 99,
        });
        fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let loaded = load_config(&path);
        assert!(loaded.codex.auto_approve);
        assert_eq!(loaded.codex.poll_interval_sec, 99);
        // legacy keys must not leak into claude's section
        assert!(!loaded.claude.auto_approve);
        assert_eq!(loaded.claude.poll_interval_sec, 300);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn new_shape_codex_overrides_win_over_legacy_flat_keys() {
        let path = temp_path("precedence");
        let raw = serde_json::json!({
            "pollIntervalSec": 11,
            "codex": { "pollIntervalSec": 22 },
        });
        fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let loaded = load_config(&path);
        assert_eq!(loaded.codex.poll_interval_sec, 22);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn invalid_compact_position_falls_back_to_taskbar() {
        let path = temp_path("badpos");
        let raw = serde_json::json!({
            "usageWidget": { "compactPosition": "nonsense" },
        });
        fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let loaded = load_config(&path);
        assert_eq!(loaded.usage_widget.compact_position, CompactPosition::Taskbar);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn existing_config_without_onboarded_defaults_to_true() {
        let path = temp_path("onboarded-legacy");
        let raw = serde_json::json!({ "language": "en" });
        fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let loaded = load_config(&path);
        assert!(loaded.onboarded);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn bom_prefixed_file_parses() {
        let path = temp_path("bom");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(b"\xEF\xBB\xBF").unwrap();
        f.write_all(br#"{"language":"ja"}"#).unwrap();
        drop(f);

        let loaded = load_config(&path);
        assert_eq!(loaded.language, "ja");
        let _ = fs::remove_file(&path);
    }
}
