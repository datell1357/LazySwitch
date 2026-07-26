use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::accounts::{read_auth, CodexAuth};
use crate::atomic_fs::atomic_write;
use crate::provider_types::PWindow;

/// Direct access to the same ChatGPT backend the Codex clients use for
/// usage. Endpoint + refresh flow reverse-engineered from the OpenUsage
/// `codex` plugin.
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// refresh proactively after 8 days
const REFRESH_AGE_MS: i64 = 8 * 24 * 60 * 60 * 1000;
const USAGE_CACHE_MS: i64 = 5 * 60 * 1000;
const DEFAULT_429_BACKOFF_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsage {
    /// 5-hour session window
    pub primary: Option<PWindow>,
    /// weekly window
    pub secondary: Option<PWindow>,
    pub plan_type: Option<String>,
    pub credits_balance: Option<f64>,
    pub email: Option<String>,
}

struct CacheEntry {
    at: i64,
    usage: Option<CodexUsage>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn rate_limited_until() -> &'static AtomicI64 {
    static V: OnceLock<AtomicI64> = OnceLock::new();
    V.get_or_init(|| AtomicI64::new(0))
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

fn cached_at(file: &Path) -> Option<CodexUsage> {
    cache().lock().unwrap().get(file).and_then(|e| e.usage.clone())
}

pub fn cached_usage(file: &Path) -> Option<CodexUsage> {
    cached_at(file)
}

/// Forget cached usage for `file`. Must be called when a different
/// account's auth is installed at that path — otherwise the previous
/// account's (often exhausted) usage keeps being served for it and
/// re-triggers a switch.
pub fn invalidate_usage(file: &Path) {
    cache().lock().unwrap().remove(file);
}

fn needs_refresh(auth: &CodexAuth) -> bool {
    match &auth.last_refresh {
        None => true,
        Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(dt) => now_ms() - dt.timestamp_millis() > REFRESH_AGE_MS,
            Err(_) => true,
        },
    }
}

/// Exchange the refresh_token for a new access_token and persist the
/// rotated tokens back to `file`. Returns the updated auth, or None on
/// failure.
pub async fn refresh_token(file: &Path, mut auth: CodexAuth) -> Option<CodexAuth> {
    let rt = auth.tokens.as_ref()?.refresh_token.clone()?;
    let res = http_client()
        .post(REFRESH_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", rt.as_str()),
        ])
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let body: Value = res.json().await.ok()?;
    let access_token = body.get("access_token")?.as_str()?.to_string();

    let tokens = auth.tokens.get_or_insert_with(Default::default);
    tokens.access_token = Some(access_token);
    if let Some(rt2) = body.get("refresh_token").and_then(|v| v.as_str()) {
        tokens.refresh_token = Some(rt2.to_string());
    }
    if let Some(id_tok) = body.get("id_token").and_then(|v| v.as_str()) {
        tokens.id_token = Some(id_tok.to_string());
    }
    auth.last_refresh = Some(chrono::Utc::now().to_rfc3339());

    let json = serde_json::to_string_pretty(&auth).ok()?;
    atomic_write(file, json.as_bytes()).ok()?;
    Some(auth)
}

fn num_str(v: Option<&str>) -> Option<f64> {
    let s = v.filter(|s| !s.is_empty())?;
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

fn num_value(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => s.parse::<f64>().ok().filter(|n| n.is_finite()),
        Some(Value::Number(n)) => n.as_f64().filter(|n| n.is_finite()),
        _ => None,
    }
}

fn window_from(header_pct: Option<f64>, raw: Option<&Value>) -> Option<PWindow> {
    let pct = header_pct.or_else(|| num_value(raw.and_then(|r| r.get("used_percent"))))?;
    let reset_at = raw.and_then(|r| r.get("reset_at")).and_then(|v| v.as_f64());
    let reset_after = raw
        .and_then(|r| r.get("reset_after_seconds"))
        .and_then(|v| v.as_f64());
    let resets_at = if let Some(r) = reset_at {
        Some((r * 1000.0) as i64)
    } else {
        reset_after.map(|s| now_ms() + (s * 1000.0) as i64)
    };
    let win_sec = num_value(raw.and_then(|r| r.get("limit_window_seconds")));
    Some(PWindow {
        used_percent: pct,
        window_minutes: win_sec.map(|s| (s / 60.0).round() as i64),
        resets_at,
    })
}

/// Fetch live usage for whichever account is installed at `file` (typically
/// the live ~/.codex/auth.json). Refreshes the token first if it is stale,
/// and once more if the server returns 401.
pub async fn fetch_usage(file: &Path) -> Option<CodexUsage> {
    let now = now_ms();
    if let Some(entry_usage) = {
        let guard = cache().lock().unwrap();
        guard
            .get(file)
            .filter(|e| now - e.at < USAGE_CACHE_MS)
            .map(|e| e.usage.clone())
    } {
        return entry_usage;
    }
    if now < rate_limited_until().load(Ordering::SeqCst) {
        return cached_at(file);
    }

    let mut auth = read_auth(file)?;
    auth.tokens.as_ref()?.access_token.as_ref()?;

    if needs_refresh(&auth) {
        if let Some(refreshed) = refresh_token(file, auth.clone()).await {
            auth = refreshed;
        }
    }

    let build_request = |auth: &CodexAuth| {
        let access_token = auth.tokens.as_ref().unwrap().access_token.as_ref().unwrap();
        let mut req = http_client()
            .get(USAGE_URL)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Accept", "application/json")
            .header("User-Agent", "LazySwitch");
        if let Some(acc) = auth.tokens.as_ref().and_then(|t| t.account_id.as_ref()) {
            req = req.header("ChatGPT-Account-Id", acc);
        }
        req
    };

    let mut res = match build_request(&auth).send().await {
        Ok(r) => r,
        Err(_) => return cached_at(file),
    };

    if res.status().as_u16() == 401 {
        if let Some(refreshed) = refresh_token(file, auth.clone()).await {
            auth = refreshed;
            res = match build_request(&auth).send().await {
                Ok(r) => r,
                Err(_) => return cached_at(file),
            };
        }
    }

    if res.status().as_u16() == 429 {
        let retry = res
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok());
        let backoff = match retry {
            Some(r) if r >= 0 => r * 1000,
            _ => DEFAULT_429_BACKOFF_MS,
        };
        rate_limited_until().store(now + backoff, Ordering::SeqCst);
        return cached_at(file);
    }
    if !res.status().is_success() {
        return cached_at(file);
    }

    let header_primary = num_str(
        res.headers()
            .get("x-codex-primary-used-percent")
            .and_then(|v| v.to_str().ok()),
    );
    let header_secondary = num_str(
        res.headers()
            .get("x-codex-secondary-used-percent")
            .and_then(|v| v.to_str().ok()),
    );
    let header_credits = num_str(
        res.headers()
            .get("x-codex-credits-balance")
            .and_then(|v| v.to_str().ok()),
    );

    let data: Value = match res.json().await {
        Ok(d) => d,
        Err(_) => return cached_at(file),
    };
    let rl = data.get("rate_limit");
    let usage = CodexUsage {
        primary: window_from(header_primary, rl.and_then(|r| r.get("primary_window"))),
        secondary: window_from(header_secondary, rl.and_then(|r| r.get("secondary_window"))),
        plan_type: data
            .get("plan_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        credits_balance: header_credits
            .or_else(|| num_value(data.get("credits").and_then(|c| c.get("balance")))),
        email: data.get("email").and_then(|v| v.as_str()).map(|s| s.to_string()),
    };
    cache().lock().unwrap().insert(
        file.to_path_buf(),
        CacheEntry {
            at: now,
            usage: Some(usage.clone()),
        },
    );
    rate_limited_until().store(0, Ordering::SeqCst);
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_str_parses_finite_numbers_only() {
        assert_eq!(num_str(Some("42")), Some(42.0));
        assert_eq!(num_str(Some("")), None);
        assert_eq!(num_str(None), None);
        assert_eq!(num_str(Some("nope")), None);
    }

    #[test]
    fn num_value_handles_string_and_number_json() {
        assert_eq!(num_value(Some(&serde_json::json!("12.5"))), Some(12.5));
        assert_eq!(num_value(Some(&serde_json::json!(7))), Some(7.0));
        assert_eq!(num_value(Some(&serde_json::json!(null))), None);
        assert_eq!(num_value(Some(&serde_json::json!(""))), None);
        assert_eq!(num_value(None), None);
    }

    #[test]
    fn window_from_prefers_header_pct_over_raw() {
        let raw = serde_json::json!({ "used_percent": 10, "limit_window_seconds": 300 });
        let w = window_from(Some(55.0), Some(&raw)).unwrap();
        assert_eq!(w.used_percent, 55.0);
        assert_eq!(w.window_minutes, Some(5));
    }

    #[test]
    fn window_from_falls_back_to_raw_used_percent() {
        let raw = serde_json::json!({ "used_percent": 33, "reset_after_seconds": 60 });
        let w = window_from(None, Some(&raw)).unwrap();
        assert_eq!(w.used_percent, 33.0);
        assert!(w.resets_at.is_some());
    }

    #[test]
    fn window_from_none_without_any_percent() {
        assert_eq!(window_from(None, None), None);
        assert_eq!(window_from(None, Some(&serde_json::json!({}))), None);
    }

    #[test]
    fn needs_refresh_true_without_last_refresh() {
        assert!(needs_refresh(&CodexAuth::default()));
    }

    #[test]
    fn needs_refresh_false_when_recent() {
        let mut auth = CodexAuth::default();
        auth.last_refresh = Some(chrono::Utc::now().to_rfc3339());
        assert!(!needs_refresh(&auth));
    }

    #[test]
    fn needs_refresh_true_when_stale() {
        let mut auth = CodexAuth::default();
        auth.last_refresh = Some("2000-01-01T00:00:00Z".to_string());
        assert!(needs_refresh(&auth));
    }

    #[test]
    fn cache_round_trips_and_invalidates() {
        let file = std::path::PathBuf::from("Z:\\fake\\auth.json");
        assert_eq!(cached_usage(&file), None);
        cache().lock().unwrap().insert(
            file.clone(),
            CacheEntry {
                at: now_ms(),
                usage: Some(CodexUsage {
                    primary: None,
                    secondary: None,
                    plan_type: None,
                    credits_balance: None,
                    email: Some("x@y.com".to_string()),
                }),
            },
        );
        assert_eq!(
            cached_usage(&file).and_then(|u| u.email),
            Some("x@y.com".to_string())
        );
        invalidate_usage(&file);
        assert_eq!(cached_usage(&file), None);
    }
}
