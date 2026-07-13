use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PWindow {
    pub used_percent: f64,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PUsage {
    pub primary: Option<PWindow>,
    pub secondary: Option<PWindow>,
    pub fable: Option<PWindow>,
    pub plan_type: Option<String>,
    pub email: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PAccount {
    pub name: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub label: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LoginFlowResult {
    pub ok: bool,
    pub name: Option<String>,
    pub email: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
