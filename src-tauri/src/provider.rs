use crate::config::ProviderPrefs;
use crate::provider_types::{LoginFlowResult, PAccount, ProviderId, PUsage, SessionUsage};
use crate::providers::{claude, codex};

/// Thin ProviderId-based dispatch to the per-provider modules. See
/// `provider_types::ProviderId` for why this is a match-based dispatcher
/// rather than a trait object.
pub fn list_accounts(id: ProviderId) -> Vec<PAccount> {
    match id {
        ProviderId::Codex => codex::list_accounts(),
        ProviderId::Claude => claude::list_accounts(),
    }
}

pub fn active_account_name(id: ProviderId) -> Option<String> {
    match id {
        ProviderId::Codex => codex::active_name(),
        ProviderId::Claude => claude::active_account_name(),
    }
}

pub fn has_live_auth(id: ProviderId) -> bool {
    match id {
        ProviderId::Codex => codex::has_live_auth(),
        ProviderId::Claude => claude::has_live_auth(),
    }
}

pub fn import_current(id: ProviderId, name: Option<&str>) -> Result<PAccount, String> {
    match id {
        ProviderId::Codex => codex::import_current(name),
        ProviderId::Claude => claude::import_current(name),
    }
}

pub fn remove_account(id: ProviderId, name: &str) {
    match id {
        ProviderId::Codex => codex::remove_account(name),
        ProviderId::Claude => claude::remove_account(name),
    }
}

pub fn rename_account(id: ProviderId, old_name: &str, new_name: &str) -> Result<(), String> {
    match id {
        ProviderId::Codex => codex::rename_account(old_name, new_name),
        ProviderId::Claude => claude::rename_account(old_name, new_name),
    }
}

pub fn set_account_enabled(id: ProviderId, name: &str, enabled: bool) -> Result<(), String> {
    match id {
        ProviderId::Codex => codex::set_account_enabled(name, enabled),
        ProviderId::Claude => claude::set_account_enabled(name, enabled),
    }
}

pub fn sync_live_back_to_slot(id: ProviderId) {
    match id {
        ProviderId::Codex => codex::sync_live_back_to_slot(),
        ProviderId::Claude => claude::sync_live_back_to_slot(),
    }
}

pub fn install_auth(id: ProviderId, name: &str) -> Result<(), String> {
    match id {
        ProviderId::Codex => codex::install_auth(name),
        ProviderId::Claude => claude::install_auth(name),
    }
}

pub async fn fetch_usage(id: ProviderId, name: Option<&str>) -> Option<PUsage> {
    match id {
        ProviderId::Codex => codex::fetch_usage(name).await,
        ProviderId::Claude => claude::fetch_usage_for(name).await,
    }
}

pub fn cached_usage(id: ProviderId, name: Option<&str>) -> Option<PUsage> {
    match id {
        ProviderId::Codex => codex::cached_usage(name),
        ProviderId::Claude => claude::cached_usage_for(name),
    }
}

/// Local-session usage fallback — Codex-only (reads rollout files).
pub fn session_usage(id: ProviderId) -> Option<SessionUsage> {
    match id {
        ProviderId::Codex => codex::session_usage(),
        ProviderId::Claude => None,
    }
}

/// Reactive scan for usage-limit errors in local session logs — Codex-only.
pub fn scan_error(id: ProviderId) -> Option<String> {
    match id {
        ProviderId::Codex => codex::scan_error(),
        ProviderId::Claude => None,
    }
}

/// Desktop app that caches auth in memory; returns None when the provider
/// has no desktop integration (Claude Desktop does not share CLI auth).
pub async fn desktop_restart(id: ProviderId, prefs: &ProviderPrefs) -> Option<bool> {
    match id {
        ProviderId::Codex => Some(codex::desktop_restart(prefs).await),
        ProviderId::Claude => None,
    }
}

pub async fn add_via_login(
    id: ProviderId,
    on_url: Option<impl Fn(String) + Send + Sync + 'static>,
) -> LoginFlowResult {
    match id {
        ProviderId::Codex => codex::add_via_login(on_url).await,
        ProviderId::Claude => claude::add_via_login(on_url).await,
    }
}
