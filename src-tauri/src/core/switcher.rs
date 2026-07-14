use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::providers::Provider;
use crate::core::types::{PAccount, PUsage, PWindow, ProviderPrefs};
use crate::core::CoreError;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn spent_window(window: Option<&PWindow>, min_left_pct: f64, now: i64) -> bool {
    window.is_some_and(|window| {
        (window.resets_at.is_none() || window.resets_at > Some(now))
            && 100.0 - window.used_percent <= min_left_pct
    })
}

pub fn rotation_list(provider: &dyn Provider, prefs: &ProviderPrefs) -> Vec<PAccount> {
    let all = provider.list_accounts();
    if prefs.rotation_order.is_empty() {
        return all;
    }
    let mut ordered = prefs
        .rotation_order
        .iter()
        .filter_map(|name| all.iter().find(|account| &account.name == name).cloned())
        .collect::<Vec<_>>();
    ordered.extend(
        all.into_iter()
            .filter(|account| !prefs.rotation_order.contains(&account.name)),
    );
    ordered
}

pub fn is_exhausted(usage: Option<&PUsage>, prefs: &ProviderPrefs) -> bool {
    is_exhausted_at(usage, prefs, now_ms())
}

pub fn is_exhausted_at(usage: Option<&PUsage>, prefs: &ProviderPrefs, now: i64) -> bool {
    let Some(usage) = usage else { return false };
    spent_window(usage.primary.as_ref(), prefs.primary_min_left_pct, now)
        || spent_window(usage.secondary.as_ref(), prefs.weekly_min_left_pct, now)
}

pub fn exhausted_until(usage: Option<&PUsage>, prefs: &ProviderPrefs) -> Option<i64> {
    exhausted_until_at(usage, prefs, now_ms())
}

pub fn exhausted_until_at(usage: Option<&PUsage>, prefs: &ProviderPrefs, now: i64) -> Option<i64> {
    let usage = usage?;
    let windows = [
        (usage.primary.as_ref(), prefs.primary_min_left_pct),
        (usage.secondary.as_ref(), prefs.weekly_min_left_pct),
    ];
    let mut until = None;
    for (window, threshold) in windows {
        if !spent_window(window, threshold, now) {
            continue;
        }
        let reset = window?.resets_at?;
        until = Some(until.map_or(reset, |current: i64| current.max(reset)));
    }
    until
}

pub fn pick_next_account(
    provider: &dyn Provider,
    prefs: &ProviderPrefs,
    cooling_down: &dyn Fn(&str) -> bool,
) -> Option<PAccount> {
    pick_next_account_at(provider, prefs, cooling_down, now_ms())
}

pub fn pick_next_account_at(
    provider: &dyn Provider,
    prefs: &ProviderPrefs,
    cooling_down: &dyn Fn(&str) -> bool,
    now: i64,
) -> Option<PAccount> {
    let list = rotation_list(provider, prefs);
    if list.is_empty() {
        return None;
    }
    let active = provider.active_account_name();
    let start = active
        .as_ref()
        .and_then(|name| list.iter().position(|account| &account.name == name))
        .unwrap_or(0);
    for offset in 1..=list.len() {
        let candidate = &list[(start + offset) % list.len()];
        if active.as_deref() == Some(candidate.name.as_str())
            || !candidate.enabled
            || cooling_down(&candidate.name)
            || is_exhausted_at(
                provider.cached_usage(Some(&candidate.name)).as_ref(),
                prefs,
                now,
            )
        {
            continue;
        }
        return Some(candidate.clone());
    }
    None
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwitchResult {
    pub from: Option<String>,
    pub to: String,
    pub desktop_restarted: bool,
}

pub async fn switch_to(
    provider: &dyn Provider,
    name: &str,
    prefs: &ProviderPrefs,
    restart_desktop: bool,
) -> Result<SwitchResult, CoreError> {
    let from = provider.active_account_name();
    if provider
        .list_accounts()
        .into_iter()
        .find(|account| account.name == name)
        .is_some_and(|account| !account.enabled)
    {
        return Err(CoreError::Invalid(format!(
            "Account \"{name}\" is disabled"
        )));
    }
    if from.is_some() {
        provider.sync_live_back_to_slot()?;
    }
    provider.install_auth(name)?;
    let desktop_restarted = if restart_desktop {
        provider.desktop_restart(prefs).await
    } else {
        false
    };
    Ok(SwitchResult {
        from,
        to: name.into(),
        desktop_restarted,
    })
}

pub fn default_now() -> i64 {
    now_ms()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;

    use super::*;

    const FUTURE: i64 = 2_000_000;
    const PAST: i64 = 1_000;

    fn prefs() -> ProviderPrefs {
        ProviderPrefs {
            auto_approve: false,
            auto_restart_cli: false,
            desktop_app_path: String::new(),
            desktop_process_name: String::new(),
            rotation_order: Vec::new(),
            primary_min_left_pct: 5.0,
            weekly_min_left_pct: 1.0,
            poll_interval_sec: 30,
        }
    }
    fn window(used_percent: f64, resets_at: Option<i64>) -> PWindow {
        PWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at,
        }
    }
    fn make_usage(primary: Option<PWindow>, secondary: Option<PWindow>) -> PUsage {
        PUsage {
            primary,
            secondary,
            fable: None,
            plan_type: None,
            email: None,
        }
    }

    struct FakeProvider {
        accounts: Vec<PAccount>,
        active: Option<String>,
        usage: HashMap<String, PUsage>,
        installed: std::sync::Mutex<bool>,
    }
    impl Provider for FakeProvider {
        fn list_accounts(&self) -> Vec<PAccount> {
            self.accounts.clone()
        }
        fn active_account_name(&self) -> Option<String> {
            self.active.clone()
        }
        fn has_live_auth(&self) -> bool {
            self.active.is_some()
        }
        fn import_current(&self, _name: Option<&str>) -> Result<PAccount, CoreError> {
            Err(CoreError::Invalid("unused".into()))
        }
        fn remove_account(&self, _name: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn rename_account(&self, _old_name: &str, _new_name: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn set_account_enabled(&self, _name: &str, _enabled: bool) -> Result<(), CoreError> {
            Ok(())
        }
        fn sync_live_back_to_slot(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn install_auth(&self, _name: &str) -> Result<(), CoreError> {
            *self.installed.lock().expect("test mutex") = true;
            Ok(())
        }
        fn fetch_usage<'a>(
            &'a self,
            _name: Option<&'a str>,
        ) -> Pin<Box<dyn Future<Output = Option<PUsage>> + Send + 'a>> {
            Box::pin(async { None })
        }
        fn cached_usage(&self, name: Option<&str>) -> Option<PUsage> {
            name.and_then(|name| self.usage.get(name)).cloned()
        }
        fn cached_usage_updated_at(&self, _name: Option<&str>) -> Option<i64> {
            None
        }
    }
    fn provider(
        names: &[&str],
        active: &str,
        usage: HashMap<String, PUsage>,
        disabled: &[&str],
    ) -> FakeProvider {
        FakeProvider {
            accounts: names
                .iter()
                .map(|name| PAccount {
                    name: (*name).into(),
                    email: None,
                    account_id: None,
                    label: None,
                    enabled: !disabled.contains(name),
                })
                .collect(),
            active: Some(active.into()),
            usage,
            installed: std::sync::Mutex::new(false),
        }
    }
    fn no_cooldown(_: &str) -> bool {
        false
    }

    #[test]
    fn pick_next_skips_exhausted() {
        let mut usage = HashMap::new();
        usage.insert(
            "b".into(),
            make_usage(Some(window(100.0, Some(FUTURE))), None),
        );
        let provider = provider(&["a", "b", "c"], "a", usage, &[]);
        assert_eq!(
            pick_next_account_at(&provider, &prefs(), &no_cooldown, 100_000).map(|a| a.name),
            Some("c".into())
        );
    }
    #[test]
    fn pick_next_returns_none_when_all_are_exhausted() {
        let spent = make_usage(Some(window(100.0, Some(FUTURE))), None);
        let provider = provider(
            &["a", "b", "c"],
            "a",
            HashMap::from([("b".into(), spent.clone()), ("c".into(), spent)]),
            &[],
        );
        assert_eq!(
            pick_next_account_at(&provider, &prefs(), &no_cooldown, 100_000),
            None
        );
    }
    #[test]
    fn reset_past_is_usable_and_missing_cache_is_eligible() {
        let past_provider = provider(
            &["a", "b"],
            "a",
            HashMap::from([(
                "b".into(),
                make_usage(Some(window(100.0, Some(PAST))), None),
            )]),
            &[],
        );
        assert_eq!(
            pick_next_account_at(&past_provider, &prefs(), &no_cooldown, 100_000).map(|a| a.name),
            Some("b".into())
        );
        let empty_provider = provider(&["a", "b"], "a", HashMap::new(), &[]);
        assert_eq!(
            pick_next_account_at(&empty_provider, &prefs(), &no_cooldown, 100_000).map(|a| a.name),
            Some("b".into())
        );
    }
    #[test]
    fn disabled_accounts_are_skipped() {
        let provider = provider(&["a", "b", "c"], "a", HashMap::new(), &["b"]);
        assert_eq!(
            pick_next_account_at(&provider, &prefs(), &no_cooldown, 100_000).map(|a| a.name),
            Some("c".into())
        );
    }
    #[tokio::test]
    async fn switch_refuses_disabled_account() {
        let provider = provider(&["a", "b"], "a", HashMap::new(), &["b"]);
        let result = switch_to(&provider, "b", &prefs(), false).await;
        assert!(matches!(result, Err(CoreError::Invalid(message)) if message.contains("disabled")));
        assert!(!*provider.installed.lock().expect("test mutex"));
    }
    #[test]
    fn exhausted_until_uses_latest_reset_and_handles_unknown_reset() {
        let usage = make_usage(
            Some(window(100.0, Some(FUTURE))),
            Some(window(100.0, Some(FUTURE + 1_000))),
        );
        assert_eq!(
            exhausted_until_at(Some(&usage), &prefs(), 100_000),
            Some(FUTURE + 1_000)
        );
        assert_eq!(
            exhausted_until_at(
                Some(&make_usage(Some(window(10.0, Some(FUTURE))), None)),
                &prefs(),
                100_000
            ),
            None
        );
        assert_eq!(
            exhausted_until_at(
                Some(&make_usage(Some(window(100.0, None)), None)),
                &prefs(),
                100_000
            ),
            None
        );
    }
    #[test]
    fn weekly_threshold_is_honoured() {
        let usage = make_usage(
            Some(window(10.0, Some(FUTURE))),
            Some(window(99.5, Some(FUTURE))),
        );
        assert!(is_exhausted_at(Some(&usage), &prefs(), 100_000));
    }
}
