use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config::config_path;
use super::{atomic_write, CoreError};

// A poll tick can be as frequent as every 30s (Codex); sampling on that
// cadence would blow past a week's worth of history in a few hours, so
// samples are throttled to at most one per bucket regardless of how often
// the poller ticks.
const MIN_SAMPLE_INTERVAL_MS: i64 = 15 * 60 * 1000;
const MAX_AGE_MS: i64 = 8 * 24 * 60 * 60 * 1000;
const MAX_SAMPLES_PER_ACCOUNT: usize = 700;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub at: i64,
    #[serde(rename = "usedPercent")]
    pub used_percent: f64,
}

type HistoryMap = HashMap<String, Vec<Sample>>;

pub fn history_path() -> PathBuf {
    config_path()
        .parent()
        .map(|dir| dir.join("usage-history.json"))
        .unwrap_or_else(|| PathBuf::from("usage-history.json"))
}

fn key(provider: &str, account: &str) -> String {
    format!("{provider}:{account}")
}

fn load(path: &Path) -> HistoryMap {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(path: &Path, map: &HistoryMap) -> Result<(), CoreError> {
    atomic_write(path, &serde_json::to_vec(map)?)
}

pub fn record_sample(provider: &str, account: &str, used_percent: f64, now: i64) {
    record_sample_at(&history_path(), provider, account, used_percent, now);
}

pub fn record_sample_at(path: &Path, provider: &str, account: &str, used_percent: f64, now: i64) {
    let mut map = load(path);
    let entries = map.entry(key(provider, account)).or_default();
    if entries
        .last()
        .is_some_and(|last| now - last.at < MIN_SAMPLE_INTERVAL_MS)
    {
        return;
    }
    entries.push(Sample { at: now, used_percent });
    entries.retain(|sample| now - sample.at <= MAX_AGE_MS);
    if entries.len() > MAX_SAMPLES_PER_ACCOUNT {
        let excess = entries.len() - MAX_SAMPLES_PER_ACCOUNT;
        entries.drain(0..excess);
    }
    let _ = save(path, &map);
}

pub fn history_for(provider: &str, account: &str) -> Vec<Sample> {
    history_for_at(&history_path(), provider, account)
}

pub fn history_for_at(path: &Path, provider: &str, account: &str) -> Vec<Sample> {
    load(path).remove(&key(provider, account)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttles_samples_and_prunes_old_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("usage-history.json");
        let base = 1_000_000_000_000_i64;

        record_sample_at(&path, "codex", "acct", 10.0, base);
        record_sample_at(&path, "codex", "acct", 20.0, base + 60_000); // too soon, dropped
        record_sample_at(&path, "codex", "acct", 30.0, base + MIN_SAMPLE_INTERVAL_MS);

        let samples = history_for_at(&path, "codex", "acct");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].used_percent, 10.0);
        assert_eq!(samples[1].used_percent, 30.0);

        // A sample far enough in the future that both earlier ones fall out
        // of the retention window should prune them.
        let far_future = base + MAX_AGE_MS + MIN_SAMPLE_INTERVAL_MS * 2;
        record_sample_at(&path, "codex", "acct", 40.0, far_future);
        let samples = history_for_at(&path, "codex", "acct");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].used_percent, 40.0);
    }

    #[test]
    fn caps_sample_count_per_account() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("usage-history.json");
        let base = 1_000_000_000_000_i64;
        for i in 0..(MAX_SAMPLES_PER_ACCOUNT + 10) {
            record_sample_at(
                &path,
                "codex",
                "acct",
                1.0,
                base + i as i64 * MIN_SAMPLE_INTERVAL_MS,
            );
        }
        let samples = history_for_at(&path, "codex", "acct");
        assert_eq!(samples.len(), MAX_SAMPLES_PER_ACCOUNT);
    }

    #[test]
    fn separate_accounts_do_not_share_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("usage-history.json");
        record_sample_at(&path, "codex", "a", 5.0, 1000);
        record_sample_at(&path, "claude", "a", 9.0, 1000);
        assert_eq!(history_for_at(&path, "codex", "a").len(), 1);
        assert_eq!(history_for_at(&path, "claude", "a").len(), 1);
        assert_eq!(history_for_at(&path, "codex", "b").len(), 0);
    }
}
