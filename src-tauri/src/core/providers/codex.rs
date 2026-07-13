use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::codex_api::{CodexApiState, CodexUsage};
use super::{Provider, ReqwestTransport, SharedTransport};
use crate::core::accounts::{derive_slot_name, email_from_auth, read_auth, CodexAccounts};
use crate::core::paths::CodexPaths;
use crate::core::types::{PAccount, PUsage, PWindow, ProviderPrefs};
use crate::core::{atomic_write, CoreError};

pub trait CodexSessionHooks: Send + Sync {
    fn session_usage(&self) -> Option<(Option<PWindow>, Option<PWindow>)>;
    fn scan_error(&self) -> Option<String>;
}

#[derive(Default)]
pub struct DeferredCodexSessionHooks;

impl CodexSessionHooks for DeferredCodexSessionHooks {
    fn session_usage(&self) -> Option<(Option<PWindow>, Option<PWindow>)> {
        // TODO(phase-6): rollout-file scanning belongs with codex-rollouts and is intentionally deferred.
        None
    }
    fn scan_error(&self) -> Option<String> {
        // TODO(phase-6): rollout-file error scanning belongs with codex-rollouts and is intentionally deferred.
        None
    }
}

fn to_account(account: crate::core::accounts::Account) -> PAccount {
    PAccount {
        name: account.name,
        email: account.email,
        account_id: account.account_id,
        label: account.label,
        enabled: account.enabled,
    }
}

fn to_usage(usage: CodexUsage) -> PUsage {
    PUsage {
        primary: usage.primary.map(|window| PWindow {
            used_percent: window.used_percent,
            window_minutes: window.window_minutes,
            resets_at: window.resets_at,
        }),
        secondary: usage.secondary.map(|window| PWindow {
            used_percent: window.used_percent,
            window_minutes: window.window_minutes,
            resets_at: window.resets_at,
        }),
        fable: None,
        plan_type: usage.plan_type,
        email: usage.email,
    }
}

pub struct CodexProvider {
    pub paths: CodexPaths,
    accounts: CodexAccounts,
    transport: SharedTransport,
    api: CodexApiState,
    session_hooks: Arc<dyn CodexSessionHooks>,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new(
            CodexPaths::from_env(),
            Arc::new(ReqwestTransport::default()),
        )
    }
}

impl CodexProvider {
    pub fn new(paths: CodexPaths, transport: SharedTransport) -> Self {
        Self::new_with_session_hooks(paths, transport, Arc::new(DeferredCodexSessionHooks))
    }
    pub fn new_with_session_hooks(
        paths: CodexPaths,
        transport: SharedTransport,
        session_hooks: Arc<dyn CodexSessionHooks>,
    ) -> Self {
        Self {
            accounts: CodexAccounts::new(paths.clone()),
            paths,
            transport,
            api: CodexApiState::default(),
            session_hooks,
        }
    }
    pub fn session_usage(&self) -> Option<(Option<PWindow>, Option<PWindow>)> {
        self.session_hooks.session_usage()
    }
    pub fn scan_error(&self) -> Option<String> {
        self.session_hooks.scan_error()
    }
    fn atomic_copy(
        &self,
        source: &std::path::Path,
        destination: &std::path::Path,
    ) -> Result<(), CoreError> {
        let bytes = std::fs::read(source)?;
        atomic_write(destination, &bytes)
    }
}

impl Provider for CodexProvider {
    fn list_accounts(&self) -> Vec<PAccount> {
        self.accounts
            .list_accounts()
            .into_iter()
            .map(to_account)
            .collect()
    }
    fn active_account_name(&self) -> Option<String> {
        self.accounts.active_account_name()
    }
    fn has_live_auth(&self) -> bool {
        self.accounts.active_account_id().is_some()
    }
    fn import_current(&self, name: Option<&str>) -> Result<PAccount, CoreError> {
        let live = read_auth(&self.paths.live_auth_file());
        let slot = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                derive_slot_name(&self.paths, email_from_auth(live.as_ref()).as_deref())
            });
        Ok(to_account(self.accounts.import_current_as(&slot)?))
    }
    fn remove_account(&self, name: &str) -> Result<(), CoreError> {
        self.accounts.remove_account(name)
    }
    fn rename_account(&self, old_name: &str, new_name: &str) -> Result<(), CoreError> {
        self.accounts.rename_account(old_name, new_name)
    }
    fn set_account_enabled(&self, name: &str, enabled: bool) -> Result<(), CoreError> {
        self.accounts.set_account_enabled(name, enabled)
    }
    fn sync_live_back_to_slot(&self) -> Result<(), CoreError> {
        let Some(name) = self.accounts.active_account_name() else {
            return Ok(());
        };
        if !self.paths.live_auth_file().exists() {
            return Ok(());
        }
        self.atomic_copy(
            &self.paths.live_auth_file(),
            &self.paths.account_auth_file(&name),
        )
    }
    fn install_auth(&self, name: &str) -> Result<(), CoreError> {
        let source = self.paths.account_auth_file(name);
        if !source.exists() {
            return Err(CoreError::Invalid(format!(
                "Account \"{name}\" has no auth.json"
            )));
        }
        self.atomic_copy(&source, &self.paths.live_auth_file())?;
        self.api.invalidate_usage(&self.paths.live_auth_file());
        Ok(())
    }
    fn fetch_usage<'a>(
        &'a self,
        name: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<PUsage>> + Send + 'a>> {
        Box::pin(async move {
            let file = name
                .map(|n| self.paths.account_auth_file(n))
                .unwrap_or_else(|| self.paths.live_auth_file());
            self.api
                .fetch_usage(&self.transport, &file)
                .await
                .map(to_usage)
        })
    }
    fn cached_usage(&self, name: Option<&str>) -> Option<PUsage> {
        let file = name
            .map(|n| self.paths.account_auth_file(n))
            .unwrap_or_else(|| self.paths.live_auth_file());
        self.api.cached_usage(&file).map(to_usage)
    }
    fn desktop_restart<'a>(
        &'a self,
        _prefs: &'a ProviderPrefs,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}
