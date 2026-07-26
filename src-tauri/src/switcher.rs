use std::collections::HashSet;

use crate::config::ProviderPrefs;
use crate::provider;
use crate::provider_types::{PAccount, PUsage, PWindow, ProviderId};

/// Order in which accounts are considered for rotation.
pub fn rotation_list(id: ProviderId, prefs: &ProviderPrefs) -> Vec<PAccount> {
    let all = provider::list_accounts(id);
    if prefs.rotation_order.is_empty() {
        return all;
    }
    let mut ordered: Vec<PAccount> = prefs
        .rotation_order
        .iter()
        .filter_map(|n| all.iter().find(|a| &a.name == n).cloned())
        .collect();
    // Append any accounts not named in the explicit order.
    for a in &all {
        if !prefs.rotation_order.contains(&a.name) {
            ordered.push(a.clone());
        }
    }
    ordered
}

fn spent_window(w: Option<&PWindow>, min_left_pct: f64, now: i64) -> bool {
    match w {
        Some(w) => (w.resets_at.is_none() || w.resets_at.unwrap() > now) && 100.0 - w.used_percent <= min_left_pct,
        None => false,
    }
}

/// True when the last known usage says the account would trip the switch
/// thresholds immediately. A window whose reset time has passed no longer
/// counts — the quota is back even if the cache is stale.
pub fn is_exhausted(usage: Option<&PUsage>, prefs: &ProviderPrefs, now: i64) -> bool {
    let Some(usage) = usage else {
        return false;
    };
    spent_window(usage.primary.as_ref(), prefs.primary_min_left_pct, now)
        || spent_window(usage.secondary.as_ref(), prefs.weekly_min_left_pct, now)
}

/// When the account is exhausted, the epoch ms at which its last blocking
/// window resets — None if it is not exhausted or no reset time is known
/// (callers should fall back to a short cooldown).
pub fn exhausted_until(usage: Option<&PUsage>, prefs: &ProviderPrefs, now: i64) -> Option<i64> {
    let usage = usage?;
    let mut until: Option<i64> = None;
    for (w, min_left_pct) in [
        (usage.primary.as_ref(), prefs.primary_min_left_pct),
        (usage.secondary.as_ref(), prefs.weekly_min_left_pct),
    ] {
        if !spent_window(w, min_left_pct, now) {
            continue;
        }
        match w.unwrap().resets_at {
            None => return None,
            Some(r) => until = Some(until.map(|u| u.max(r)).unwrap_or(r)),
        }
    }
    until
}

/// Pick the next account after the currently active one, skipping
/// cooling-down ones and ones whose cached usage is already at the limit —
/// switching to those would just bounce straight back here.
pub fn pick_next_account(
    id: ProviderId,
    prefs: &ProviderPrefs,
    cooling_down: &HashSet<String>,
    now: i64,
) -> Option<PAccount> {
    let list = rotation_list(id, prefs);
    if list.is_empty() {
        return None;
    }
    let active_name = provider::active_account_name(id);
    let start_idx = active_name
        .as_ref()
        .and_then(|n| list.iter().position(|a| &a.name == n))
        .unwrap_or(0);
    for i in 1..=list.len() {
        let cand = &list[(start_idx + i) % list.len()];
        if Some(&cand.name) == active_name.as_ref() {
            continue;
        }
        if !cand.enabled {
            continue;
        }
        if cooling_down.contains(&cand.name) {
            continue;
        }
        let cached = provider::cached_usage(id, Some(&cand.name));
        if is_exhausted(cached.as_ref(), prefs, now) {
            continue;
        }
        return Some(cand.clone());
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitchResult {
    pub from: Option<String>,
    pub to: String,
    pub desktop_restarted: bool,
}

/// Perform the switch:
///  1. save the current live auth back to its slot (preserve refreshed tokens)
///  2. atomically install the target slot's auth as the live one
///  3. optionally restart the provider's desktop app so it reloads the account
pub async fn switch_to(
    id: ProviderId,
    name: &str,
    prefs: &ProviderPrefs,
    restart_desktop: bool,
) -> Result<SwitchResult, String> {
    let from = provider::active_account_name(id);
    let target = provider::list_accounts(id).into_iter().find(|a| a.name == name);
    if let Some(t) = &target {
        if !t.enabled {
            return Err(format!("Account \"{name}\" is disabled"));
        }
    }

    if from.is_some() {
        provider::sync_live_back_to_slot(id);
    }
    provider::install_auth(id, name)?;

    let mut desktop_restarted = false;
    if restart_desktop {
        if let Some(restarted) = provider::desktop_restart(id, prefs).await {
            desktop_restarted = restarted;
        }
    }
    Ok(SwitchResult {
        from,
        to: name.to_string(),
        desktop_restarted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(rotation_order: Vec<String>) -> ProviderPrefs {
        ProviderPrefs {
            auto_approve: false,
            auto_restart_cli: true,
            desktop_app_path: String::new(),
            desktop_process_name: String::new(),
            rotation_order,
            primary_min_left_pct: 5.0,
            weekly_min_left_pct: 1.0,
            poll_interval_sec: 30,
        }
    }

    fn window(used_percent: f64, resets_at: Option<i64>) -> PWindow {
        PWindow { used_percent, window_minutes: Some(300), resets_at }
    }

    #[test]
    fn is_exhausted_false_without_usage() {
        assert!(!is_exhausted(None, &prefs(vec![]), 0));
    }

    #[test]
    fn is_exhausted_true_when_remaining_at_or_below_threshold() {
        let usage = PUsage {
            primary: Some(window(96.0, None)),
            secondary: None,
            fable: None,
            plan_type: None,
            email: None,
        };
        assert!(is_exhausted(Some(&usage), &prefs(vec![]), 0));
    }

    #[test]
    fn is_exhausted_false_once_reset_time_has_passed() {
        let usage = PUsage {
            primary: Some(window(99.0, Some(1000))),
            secondary: None,
            fable: None,
            plan_type: None,
            email: None,
        };
        // now (2000) is past resets_at (1000) -> quota is back.
        assert!(!is_exhausted(Some(&usage), &prefs(vec![]), 2000));
    }

    #[test]
    fn exhausted_until_none_when_reset_time_unknown() {
        let usage = PUsage {
            primary: Some(window(99.0, None)),
            secondary: None,
            fable: None,
            plan_type: None,
            email: None,
        };
        assert_eq!(exhausted_until(Some(&usage), &prefs(vec![]), 0), None);
    }

    #[test]
    fn exhausted_until_returns_max_of_blocking_windows() {
        let usage = PUsage {
            primary: Some(window(99.0, Some(500))),
            secondary: Some(window(99.5, Some(900))),
            fable: None,
            plan_type: None,
            email: None,
        };
        assert_eq!(exhausted_until(Some(&usage), &prefs(vec![]), 0), Some(900));
    }
}
