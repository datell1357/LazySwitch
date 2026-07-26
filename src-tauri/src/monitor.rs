use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::config::ProviderPrefs;
use crate::provider;
use crate::provider_types::{PWindow, ProviderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    Session,
    Backend,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    /// short window (5h session / free monthly)
    pub primary: Option<PWindow>,
    /// long window (weekly)
    pub secondary: Option<PWindow>,
    pub source: UsageSource,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdWindow {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LimitReason {
    Threshold {
        window: ThresholdWindow,
        percent: f64,
    },
    Error {
        message: String,
    },
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Test hooks: ROTATOR_FAKE_CODEX_PCT / ROTATOR_FAKE_CLAUDE_PCT force the
/// primary window's used-percent. ROTATOR_FAKE_PRIMARY_PCT is the legacy
/// alias for codex. Dev/testing only.
fn fake_pct(id: ProviderId) -> Option<f64> {
    let key = format!("ROTATOR_FAKE_{}_PCT", id.as_str().to_uppercase());
    let raw = std::env::var(&key).ok().or_else(|| {
        if id == ProviderId::Codex {
            std::env::var("ROTATOR_FAKE_PRIMARY_PCT").ok()
        } else {
            None
        }
    })?;
    raw.parse::<f64>().ok().filter(|n| n.is_finite())
}

async fn evaluate(
    id: ProviderId,
    prefs: &ProviderPrefs,
    last_error: &mut Option<String>,
) -> (UsageSnapshot, Option<LimitReason>) {
    let backend = provider::fetch_usage(id, None).await;
    let mut snapshot = if let Some(b) = backend {
        UsageSnapshot {
            primary: b.primary,
            secondary: b.secondary,
            source: UsageSource::Backend,
            at: now_ms(),
        }
    } else if let Some(session) = provider::session_usage(id) {
        UsageSnapshot {
            primary: session.primary,
            secondary: session.secondary,
            source: UsageSource::Session,
            at: now_ms(),
        }
    } else {
        UsageSnapshot {
            primary: None,
            secondary: None,
            source: UsageSource::None,
            at: now_ms(),
        }
    };

    if let Some(fake) = fake_pct(id) {
        snapshot.primary = Some(PWindow {
            used_percent: fake,
            window_minutes: snapshot
                .primary
                .and_then(|p| p.window_minutes)
                .or(Some(300)),
            resets_at: snapshot
                .primary
                .and_then(|p| p.resets_at)
                .or(Some(now_ms() + 30 * 60 * 1000)),
        });
    }

    // Reactive backstop: a fresh usage-limit error forces a switch.
    let error = provider::scan_error(id);
    match &error {
        Some(err) if last_error.as_deref() != Some(err.as_str()) => {
            *last_error = Some(err.clone());
            return (
                snapshot,
                Some(LimitReason::Error {
                    message: err.clone(),
                }),
            );
        }
        None => *last_error = None,
        _ => {}
    }

    // Proactive: switch when REMAINING percent drops to the threshold or below.
    if let Some(p) = &snapshot.primary {
        if 100.0 - p.used_percent <= prefs.primary_min_left_pct {
            let percent = p.used_percent;
            return (
                snapshot,
                Some(LimitReason::Threshold {
                    window: ThresholdWindow::Primary,
                    percent,
                }),
            );
        }
    }
    if let Some(s) = &snapshot.secondary {
        if 100.0 - s.used_percent <= prefs.weekly_min_left_pct {
            let percent = s.used_percent;
            return (
                snapshot,
                Some(LimitReason::Threshold {
                    window: ThresholdWindow::Secondary,
                    percent,
                }),
            );
        }
    }
    (snapshot, None)
}

/// Polls one provider's live account usage in a background tokio task and
/// invokes `on_usage` every tick and `on_limit_hit` when remaining % crosses
/// the threshold or a usage-limit error shows up in the provider's local
/// session logs. Decoupled from Tauri's AppHandle by design — the caller
/// (the tray/window wiring, Phase 4) supplies the callbacks.
pub struct UsageMonitor {
    handle: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
}

impl UsageMonitor {
    pub fn new() -> Self {
        UsageMonitor {
            handle: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// `sec` (the poll interval) is resolved once at start time from
    /// `get_prefs()`, matching the original's `setInterval` — a changed
    /// `pollIntervalSec` only takes effect after `stop()` + `start()` again.
    /// Threshold prefs used inside each tick's evaluation are still
    /// re-fetched fresh every tick.
    pub fn start<FGetPrefs, FOnUsage, FOnLimitHit>(
        &mut self,
        id: ProviderId,
        get_prefs: FGetPrefs,
        on_usage: FOnUsage,
        on_limit_hit: FOnLimitHit,
    ) where
        FGetPrefs: Fn() -> ProviderPrefs + Send + 'static,
        FOnUsage: Fn(UsageSnapshot) + Send + 'static,
        FOnLimitHit: Fn(LimitReason) + Send + 'static,
    {
        if self.handle.is_some() {
            return;
        }
        self.stop_flag.store(false, Ordering::SeqCst);
        let stop_flag = self.stop_flag.clone();
        let sec = id.min_poll_sec().max(get_prefs().poll_interval_sec);

        let handle = tokio::spawn(async move {
            let mut last_error: Option<String> = None;
            loop {
                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }
                let prefs = get_prefs();
                let (snapshot, limit) = evaluate(id, &prefs, &mut last_error).await;
                on_usage(snapshot);
                if let Some(reason) = limit {
                    on_limit_hit(reason);
                }
                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(sec)).await;
            }
        });
        self.handle = Some(handle);
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

impl Default for UsageMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(primary_min_left_pct: f64, weekly_min_left_pct: f64) -> ProviderPrefs {
        ProviderPrefs {
            auto_approve: false,
            auto_restart_cli: true,
            desktop_app_path: String::new(),
            desktop_process_name: String::new(),
            rotation_order: Vec::new(),
            primary_min_left_pct,
            weekly_min_left_pct,
            poll_interval_sec: 30,
        }
    }

    // All cases below touch the process-global ROTATOR_FAKE_CODEX_PCT /
    // ROTATOR_FAKE_PRIMARY_PCT env vars, which would race under the test
    // harness's default parallel execution if split into independent
    // #[test] fns — kept as one test to stay serial.
    #[tokio::test]
    async fn fake_pct_env_var_and_threshold_behavior() {
        std::env::remove_var("ROTATOR_FAKE_CODEX_PCT");
        std::env::remove_var("ROTATOR_FAKE_PRIMARY_PCT");

        std::env::set_var("ROTATOR_FAKE_CODEX_PCT", "77");
        assert_eq!(fake_pct(ProviderId::Codex), Some(77.0));
        std::env::remove_var("ROTATOR_FAKE_CODEX_PCT");

        std::env::set_var("ROTATOR_FAKE_PRIMARY_PCT", "55");
        assert_eq!(fake_pct(ProviderId::Codex), Some(55.0));
        assert_eq!(fake_pct(ProviderId::Claude), None);
        std::env::remove_var("ROTATOR_FAKE_PRIMARY_PCT");

        // Uses the fake-pct test hook so this doesn't depend on any live
        // account/network state on the machine running the tests.
        std::env::set_var("ROTATOR_FAKE_CODEX_PCT", "97");
        let mut last_error = None;
        let (snapshot, limit) =
            evaluate(ProviderId::Codex, &prefs(5.0, 1.0), &mut last_error).await;
        assert_eq!(snapshot.primary.unwrap().used_percent, 97.0);
        assert_eq!(
            limit,
            Some(LimitReason::Threshold {
                window: ThresholdWindow::Primary,
                percent: 97.0
            })
        );

        std::env::set_var("ROTATOR_FAKE_CODEX_PCT", "10");
        let mut last_error = None;
        let (_snapshot, limit) =
            evaluate(ProviderId::Codex, &prefs(5.0, 1.0), &mut last_error).await;
        assert_eq!(limit, None);

        std::env::remove_var("ROTATOR_FAKE_CODEX_PCT");
    }
}
