use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::core::config::{self, AppConfig};
use crate::core::providers::claude::ClaudeProvider;
use crate::core::providers::codex::CodexProvider;
use crate::core::providers::{Provider, ReqwestTransport};
use crate::core::types::PUsage;

#[derive(Clone)]
pub struct ProviderSet {
    pub codex: Arc<CodexProvider>,
    pub claude: Arc<ClaudeProvider>,
}

pub type ProviderEntry = (&'static str, &'static str, bool, bool, Arc<dyn Provider>);

impl ProviderSet {
    pub fn get(&self, id: &str) -> Result<Arc<dyn Provider>, String> {
        match id {
            "codex" => Ok(self.codex.clone()),
            "claude" => Ok(self.claude.clone()),
            other => Err(format!("Unknown provider \"{other}\"")),
        }
    }

    pub fn all(&self) -> [ProviderEntry; 2] {
        [
            ("codex", "Codex", true, true, self.codex.clone()),
            ("claude", "Claude Code", true, false, self.claude.clone()),
        ]
    }
}

#[derive(Default)]
pub struct RuntimeData {
    pub cooling_down: HashMap<String, HashMap<String, i64>>,
    pub last_usage: HashMap<String, PUsage>,
    pub pending_refreshes: HashSet<String>,
    pub switching: HashSet<String>,
    pub notifications: VecDeque<ToastPayload>,
    pub active_toasts: Vec<String>,
    pub toast_payloads: HashMap<String, ToastPayload>,
    pub toast_heights: HashMap<String, f64>,
    pub next_toast_id: u64,
    pub approval: Option<tokio::sync::oneshot::Sender<bool>>,
    pub cli_payload: Option<serde_json::Value>,
    pub cli_response: Option<tokio::sync::oneshot::Sender<String>>,
    pub probe_reports: HashMap<String, serde_json::Value>,
    pub widget_context_menu_open: bool,
    pub widget_topmost_timer_started: bool,
    pub switch_prompt_shown: HashMap<String, i64>,
}

pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub providers: ProviderSet,
    pub runtime: Mutex<RuntimeData>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: &'static str,
    pub display_name: &'static str,
    pub has_login_flow: bool,
    pub has_desktop: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedAccount {
    #[serde(flatten)]
    pub account: crate::core::types::PAccount,
    pub active: bool,
    pub cooling_down_until: Option<i64>,
    pub usage: Option<PUsage>,
    pub usage_updated_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OperationResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToastPayload {
    pub title: String,
    pub body: String,
}

pub fn state_config(state: &AppState) -> AppConfig {
    state
        .config
        .lock()
        .map(|config| config.clone())
        .unwrap_or_else(|_| config::defaults())
}

pub fn provider_info(state: &AppState) -> Vec<ProviderInfo> {
    state
        .providers
        .all()
        .into_iter()
        .map(
            |(id, display_name, has_login_flow, has_desktop, _)| ProviderInfo {
                id,
                display_name,
                has_login_flow,
                has_desktop,
            },
        )
        .collect()
}

pub fn provider_prefs(
    config: &AppConfig,
    provider: &str,
) -> Result<crate::core::types::ProviderPrefs, String> {
    match provider {
        "codex" => Ok(config.codex.clone()),
        "claude" => Ok(config.claude.clone()),
        other => Err(format!("Unknown provider \"{other}\"")),
    }
}

pub fn prune_cooldowns(state: &AppState, provider: &str) {
    let now = chrono::Utc::now().timestamp_millis();
    if let Ok(mut runtime) = state.runtime.lock() {
        if let Some(cooling) = runtime.cooling_down.get_mut(provider) {
            cooling.retain(|_, until| *until > now);
        }
    }
}

pub fn cooling_until(state: &AppState, provider: &str, name: &str) -> Option<i64> {
    state
        .runtime
        .lock()
        .ok()
        .and_then(|runtime| runtime.cooling_down.get(provider)?.get(name).copied())
}

pub fn has_enrolled_accounts(state: &AppState) -> bool {
    state
        .providers
        .all()
        .into_iter()
        .any(|(_, _, _, _, provider)| !provider.list_accounts().is_empty())
}

pub fn start_usage_refresh(
    app: &AppHandle,
    provider_id: &str,
    provider: Arc<dyn Provider>,
    name: Option<String>,
    cached: Option<PUsage>,
) {
    let state = app.state::<AppState>();
    let key = format!("{}:{}", provider_id, name.as_deref().unwrap_or("live"));
    let should_start = state
        .runtime
        .lock()
        .map(|mut runtime| runtime.pending_refreshes.insert(key.clone()))
        .unwrap_or(false);
    if !should_start {
        return;
    }
    let app = app.clone();
    let provider_id = provider_id.to_owned();
    tauri::async_runtime::spawn(async move {
        let latest = provider.fetch_usage(name.as_deref()).await;
        let changed = latest != cached;
        if let Ok(mut runtime) = app.state::<AppState>().runtime.lock() {
            runtime.pending_refreshes.remove(&key);
        }
        if changed {
            crate::app::windows::broadcast_changed(&app);
        }
        let _ = provider_id;
    });
}

pub fn list_accounts(
    app: &AppHandle,
    state: &AppState,
    provider_id: &str,
) -> Result<Vec<EnrichedAccount>, String> {
    prune_cooldowns(state, provider_id);
    let provider = state.providers.get(provider_id)?;
    let active = provider.active_account_name();
    let accounts = provider.list_accounts();
    let mut rows = Vec::with_capacity(accounts.len());
    for account in accounts {
        let usage_name = if active.as_deref() == Some(account.name.as_str()) {
            None
        } else {
            Some(account.name.clone())
        };
        let cached = provider.cached_usage(usage_name.as_deref());
        let usage_updated_at = provider.cached_usage_updated_at(usage_name.as_deref());
        start_usage_refresh(
            app,
            provider_id,
            provider.clone(),
            usage_name,
            cached.clone(),
        );
        rows.push(EnrichedAccount {
            active: active.as_deref() == Some(account.name.as_str()),
            cooling_down_until: cooling_until(state, provider_id, &account.name),
            account,
            usage: cached,
            usage_updated_at,
        });
    }
    Ok(rows)
}

pub fn build_state() -> AppState {
    AppState {
        config: Mutex::new(config::load_config()),
        providers: ProviderSet {
            codex: Arc::new(CodexProvider::new(
                crate::core::paths::CodexPaths::from_env(),
                Arc::new(ReqwestTransport::default()),
            )),
            claude: Arc::new(ClaudeProvider::new(
                crate::core::paths::ClaudePaths::from_env(),
                Arc::new(ReqwestTransport::default()),
            )),
        },
        runtime: Mutex::new(RuntimeData::default()),
    }
}
