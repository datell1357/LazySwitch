use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{HttpRequest, HttpResponse, SharedTransport};
use crate::core::accounts::{read_auth, CodexAuth};
use crate::core::{atomic_write, CoreError};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_AGE_MS: i64 = 8 * 24 * 60 * 60 * 1000;
const USAGE_CACHE_MS: i64 = 5 * 60 * 1000;
const DEFAULT_429_BACKOFF_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub used_percent: f64,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodexUsage {
    pub primary: Option<UsageWindow>,
    pub secondary: Option<UsageWindow>,
    pub plan_type: Option<String>,
    pub credits_balance: Option<f64>,
    pub email: Option<String>,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    at: i64,
    usage: Option<CodexUsage>,
}

#[derive(Default)]
pub struct CodexApiState {
    cache: Mutex<HashMap<String, CacheEntry>>,
    rate_limited_until: Mutex<i64>,
}

impl CodexApiState {
    pub fn cached_usage(&self, file: &Path) -> Option<CodexUsage> {
        self.cache
            .lock()
            .ok()?
            .get(&file.to_string_lossy().to_string())?
            .usage
            .clone()
    }
    pub fn cached_usage_updated_at(&self, file: &Path) -> Option<i64> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(&file.to_string_lossy().to_string())?;
        entry.usage.as_ref()?;
        Some(entry.at)
    }
    pub fn invalidate_usage(&self, file: &Path) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(&file.to_string_lossy().to_string());
        }
    }

    pub async fn fetch_usage(
        &self,
        transport: &SharedTransport,
        file: &Path,
    ) -> Option<CodexUsage> {
        let now = Utc::now().timestamp_millis();
        let key = file.to_string_lossy().to_string();
        let cached = self
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).cloned());
        if cached
            .as_ref()
            .is_some_and(|entry| now - entry.at < USAGE_CACHE_MS)
        {
            return cached.and_then(|entry| entry.usage);
        }
        if self
            .rate_limited_until
            .lock()
            .map(|until| now < *until)
            .unwrap_or(false)
        {
            return cached.and_then(|entry| entry.usage);
        }
        let Some(mut auth) = read_auth(file) else {
            return cached.and_then(|entry| entry.usage);
        };
        if auth.tokens.as_ref().and_then(|t| t.access_token.as_ref()).is_none() {
            return cached.and_then(|entry| entry.usage);
        }
        if needs_refresh(&auth, now) {
            auth = refresh_token(transport, file, auth.clone())
                .await
                .unwrap_or(auth);
        }
        let mut response = call_usage(transport, &auth).await.ok()?;
        if response.status == 401 {
            if let Some(refreshed) = refresh_token(transport, file, auth.clone()).await {
                auth = refreshed;
                response = call_usage(transport, &auth).await.ok()?;
            }
        }
        if response.status == 429 {
            let until = now + retry_after(&response, DEFAULT_429_BACKOFF_MS);
            if let Ok(mut rate) = self.rate_limited_until.lock() {
                *rate = until;
            }
            return cached.and_then(|entry| entry.usage);
        }
        if !(200..300).contains(&response.status) {
            return cached.and_then(|entry| entry.usage);
        }
        let data = response.body.as_ref()?;
        let rl = data.get("rate_limit");
        let usage = CodexUsage {
            primary: window_from(
                response_header_number(&response, "x-codex-primary-used-percent"),
                rl.and_then(|v| v.get("primary_window")),
            ),
            secondary: window_from(
                response_header_number(&response, "x-codex-secondary-used-percent"),
                rl.and_then(|v| v.get("secondary_window")),
            ),
            plan_type: data
                .get("plan_type")
                .and_then(Value::as_str)
                .map(String::from),
            credits_balance: response_header_number(&response, "x-codex-credits-balance")
                .or_else(|| data.get("credits")?.get("balance").and_then(number)),
            email: data.get("email").and_then(Value::as_str).map(String::from),
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                key,
                CacheEntry {
                    at: now,
                    usage: Some(usage.clone()),
                },
            );
        }
        if let Ok(mut rate) = self.rate_limited_until.lock() {
            *rate = 0;
        }
        Some(usage)
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.is_empty() => s.parse().ok(),
        _ => None,
    }
}

fn response_header_number(response: &HttpResponse, name: &str) -> Option<f64> {
    response
        .headers
        .get(name)
        .and_then(|value| value.parse().ok())
}
fn retry_after(response: &HttpResponse, default: i64) -> i64 {
    response
        .headers
        .get("retry-after")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .map(|v| v * 1000)
        .unwrap_or(default)
}

fn window_from(header_pct: Option<f64>, raw: Option<&Value>) -> Option<UsageWindow> {
    let pct = header_pct.or_else(|| raw.and_then(|v| v.get("used_percent")).and_then(number))?;
    let resets_at = raw
        .and_then(|v| v.get("reset_at"))
        .and_then(number)
        .map(|v| (v * 1000.0) as i64)
        .or_else(|| {
            raw.and_then(|v| v.get("reset_after_seconds"))
                .and_then(number)
                .map(|v| Utc::now().timestamp_millis() + (v * 1000.0) as i64)
        });
    let window_minutes = raw
        .and_then(|v| v.get("limit_window_seconds"))
        .and_then(number)
        .map(|v| (v / 60.0).round() as i64);
    Some(UsageWindow {
        used_percent: pct,
        window_minutes,
        resets_at,
    })
}

fn needs_refresh(auth: &CodexAuth, now: i64) -> bool {
    let Some(last) = auth
        .last_refresh
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    else {
        return true;
    };
    now - last.timestamp_millis() > REFRESH_AGE_MS
}

async fn call_usage(
    transport: &SharedTransport,
    auth: &CodexAuth,
) -> Result<HttpResponse, CoreError> {
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| CoreError::Invalid("missing tokens".into()))?;
    let access = tokens
        .access_token
        .as_ref()
        .ok_or_else(|| CoreError::Invalid("missing access token".into()))?;
    let mut headers = HashMap::from([
        (String::from("Authorization"), format!("Bearer {access}")),
        (String::from("Accept"), String::from("application/json")),
        (String::from("User-Agent"), String::from("LazySwitch")),
    ]);
    if let Some(id) = tokens.account_id.as_deref() {
        headers.insert("ChatGPT-Account-Id".into(), id.into());
    }
    transport
        .request(HttpRequest {
            method: "GET".into(),
            url: USAGE_URL.into(),
            headers,
            body: None,
        })
        .await
}

pub(crate) async fn refresh_token(
    transport: &SharedTransport,
    file: &Path,
    mut auth: CodexAuth,
) -> Option<CodexAuth> {
    let refresh = auth.tokens.as_ref()?.refresh_token.as_ref()?;
    let body = format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}",
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(refresh)
    );
    let response = transport
        .request(HttpRequest {
            method: "POST".into(),
            url: REFRESH_URL.into(),
            headers: HashMap::from([(
                String::from("Content-Type"),
                String::from("application/x-www-form-urlencoded"),
            )]),
            body: Some(body),
        })
        .await
        .ok()?;
    if !(200..300).contains(&response.status) {
        return None;
    }
    let data = response.body?;
    let access = data.get("access_token").and_then(Value::as_str)?.to_owned();
    let tokens = auth.tokens.get_or_insert_with(Default::default);
    tokens.access_token = Some(access);
    if let Some(token) = data.get("refresh_token").and_then(Value::as_str) {
        tokens.refresh_token = Some(token.into());
    }
    if let Some(token) = data.get("id_token").and_then(Value::as_str) {
        tokens.id_token = Some(token.into());
    }
    auth.last_refresh = Some(Utc::now().to_rfc3339());
    let bytes = serde_json::to_vec_pretty(&auth).ok()?;
    atomic_write(file, &bytes).ok()?;
    Some(auth)
}

#[cfg(test)]
mod tests {
    use super::window_from;
    use serde_json::json;

    #[test]
    fn parses_header_and_reset_window() {
        let window = window_from(
            Some(9.0),
            Some(&json!({"reset_at": 2.0, "limit_window_seconds": 18000})),
        )
        .expect("window");
        assert_eq!(window.used_percent, 9.0);
        assert_eq!(window.window_minutes, Some(300));
        assert_eq!(window.resets_at, Some(2000));
    }
}
