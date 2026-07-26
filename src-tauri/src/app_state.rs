use std::collections::HashMap;

use crate::config::{AppConfig, ProviderPrefs};
use crate::monitor::{UsageMonitor, UsageSnapshot};
use crate::provider_types::ProviderId;

pub struct ProviderState {
    pub monitor: Option<UsageMonitor>,
    pub last_usage: Option<UsageSnapshot>,
    pub cooling_down: HashMap<String, i64>,
    pub switching: bool,
    pub handling_limit: bool,
    pub last_no_account_notify: i64,
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            monitor: None,
            last_usage: None,
            cooling_down: HashMap::new(),
            switching: false,
            handling_limit: false,
            last_no_account_notify: 0,
        }
    }
}

pub struct AppState {
    pub cfg: AppConfig,
    codex: ProviderState,
    claude: ProviderState,
}

impl AppState {
    pub fn new(cfg: AppConfig) -> Self {
        Self {
            cfg,
            codex: ProviderState::default(),
            claude: ProviderState::default(),
        }
    }

    pub fn prefs_of(&self, provider: ProviderId) -> &ProviderPrefs {
        match provider {
            ProviderId::Codex => &self.cfg.codex,
            ProviderId::Claude => &self.cfg.claude,
        }
    }

    pub fn state_of(&self, provider: ProviderId) -> &ProviderState {
        match provider {
            ProviderId::Codex => &self.codex,
            ProviderId::Claude => &self.claude,
        }
    }

    pub fn state_of_mut(&mut self, provider: ProviderId) -> &mut ProviderState {
        match provider {
            ProviderId::Codex => &mut self.codex,
            ProviderId::Claude => &mut self.claude,
        }
    }
}

pub const fn provider_by_id(id: ProviderId) -> ProviderId {
    id
}

pub fn prune_cooldowns(state: &mut ProviderState, now_ms: i64) {
    state.cooling_down.retain(|_, until| *until > now_ms);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_accessors_select_the_requested_provider() {
        let mut state = AppState::new(AppConfig::default());
        state.cfg.codex.poll_interval_sec = 11;
        state.cfg.claude.poll_interval_sec = 301;

        assert_eq!(state.prefs_of(ProviderId::Codex).poll_interval_sec, 11);
        assert_eq!(state.prefs_of(ProviderId::Claude).poll_interval_sec, 301);
        assert_eq!(provider_by_id(ProviderId::Claude), ProviderId::Claude);
    }

    #[test]
    fn prune_cooldowns_removes_expired_entries_only() {
        let mut state = ProviderState::default();
        state.cooling_down.insert("expired".into(), 1_000);
        state.cooling_down.insert("boundary".into(), 2_000);
        state.cooling_down.insert("active".into(), 2_001);

        prune_cooldowns(&mut state, 2_000);

        assert_eq!(
            state.cooling_down,
            HashMap::from([("active".to_string(), 2_001)])
        );
    }
}
