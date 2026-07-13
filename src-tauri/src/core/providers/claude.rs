use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use super::{HttpRequest, Provider, ReqwestTransport, SharedTransport};
use crate::core::paths::ClaudePaths;
use crate::core::types::{LoginFlowResult, PAccount, PUsage, PWindow};
use crate::core::{atomic_write, read_json, CoreError};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const REFRESH_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const REFRESH_BUFFER_MS: i64 = 5 * 60 * 1000;
const USAGE_CACHE_MS: i64 = 5 * 60 * 1000;
const DEFAULT_429_BACKOFF_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug)]
struct CacheEntry {
    at: i64,
    usage: Option<PUsage>,
}

#[derive(Default)]
struct UsageState {
    cache: Mutex<HashMap<String, CacheEntry>>,
    rate_limited_until: Mutex<HashMap<String, i64>>,
}

pub struct ClaudeProvider {
    pub paths: ClaudePaths,
    transport: SharedTransport,
    usage: UsageState,
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new(
            ClaudePaths::from_env(),
            Arc::new(ReqwestTransport::default()),
        )
    }
}

impl ClaudeProvider {
    pub fn new(paths: ClaudePaths, transport: SharedTransport) -> Self {
        Self {
            paths,
            transport,
            usage: UsageState::default(),
        }
    }

    fn read_credentials(&self, path: &std::path::Path) -> Option<Value> {
        read_json(path)
    }
    fn live_oauth_account(&self) -> Option<Value> {
        read_json(&self.paths.claude_json_file())
            .and_then(|value| value.get("oauthAccount").cloned())
    }
    fn list_slots(&self) -> Vec<PAccount> {
        let Ok(entries) = std::fs::read_dir(&self.paths.accounts_root) else {
            return Vec::new();
        };
        let mut slots = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                if !entry.file_type().ok()?.is_dir() {
                    return None;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let meta = read_json(&self.paths.slot_meta_file(&name));
                let cred = read_json(&self.paths.slot_credentials_file(&name));
                let account = meta.as_ref().and_then(|value| value.get("oauthAccount"));
                let email = account
                    .and_then(|value| value.get("emailAddress"))
                    .and_then(Value::as_str)
                    .map(String::from);
                let plan = cred
                    .as_ref()
                    .and_then(|value| value.get("claudeAiOauth"))
                    .and_then(|value| value.get("subscriptionType"))
                    .and_then(Value::as_str);
                let label = match (email.as_deref(), plan) {
                    (Some(email), Some(plan)) => Some(format!("{email} · {plan}")),
                    (Some(email), None) => Some(email.into()),
                    (None, Some(plan)) => Some(plan.into()),
                    _ => None,
                };
                Some(PAccount {
                    name,
                    email,
                    account_id: account
                        .and_then(|v| v.get("accountUuid"))
                        .and_then(Value::as_str)
                        .map(String::from),
                    label,
                    enabled: meta
                        .as_ref()
                        .and_then(|v| v.get("enabled"))
                        .and_then(Value::as_bool)
                        != Some(false),
                })
            })
            .collect::<Vec<_>>();
        slots.sort_by(|a, b| a.name.cmp(&b.name));
        slots
    }

    fn derive_slot_name(&self, email: Option<&str>) -> String {
        let base = email
            .unwrap_or("")
            .split('@')
            .next()
            .unwrap_or("")
            .chars()
            .map(|ch| {
                if "\\/:*?\"<>|".contains(ch) || ch.is_whitespace() {
                    '_'
                } else {
                    ch
                }
            })
            .collect::<String>();
        let base = if base.is_empty() {
            format!("claude-{}", Utc::now().timestamp_millis())
        } else {
            base
        };
        let mut name = base.clone();
        let mut index = 2;
        while self.paths.slot_dir(&name).exists() {
            name = format!("{base}-{index}");
            index += 1;
        }
        name
    }

    fn write_json(
        &self,
        path: &std::path::Path,
        value: &Value,
        minified: bool,
    ) -> Result<(), CoreError> {
        let bytes = if minified {
            serde_json::to_vec(value)?
        } else {
            serde_json::to_vec_pretty(value)?
        };
        atomic_write(path, &bytes)
    }

    fn refresh_if_needed<'a>(
        &'a self,
        path: &'a std::path::Path,
        mut cred: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            let oauth = cred.get("claudeAiOauth").cloned().unwrap_or(Value::Null);
            let Some(refresh) = oauth.get("refreshToken").and_then(Value::as_str) else {
                return cred;
            };
            let expires = oauth.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
            if Utc::now().timestamp_millis() < expires - REFRESH_BUFFER_MS {
                return cred;
            }
            let body = serde_json::json!({"grant_type":"refresh_token","refresh_token":refresh,"client_id":CLIENT_ID,"scope":SCOPES});
            let response = self
                .transport
                .request(HttpRequest {
                    method: "POST".into(),
                    url: REFRESH_URL.into(),
                    headers: HashMap::from([(
                        String::from("Content-Type"),
                        String::from("application/json"),
                    )]),
                    body: Some(body.to_string()),
                })
                .await;
            let Ok(response) = response else { return cred };
            if !(200..300).contains(&response.status) {
                return cred;
            }
            let Some(data) = response.body else {
                return cred;
            };
            let Some(access) = data.get("access_token").and_then(Value::as_str) else {
                return cred;
            };
            let Some(object) = cred.as_object_mut() else {
                return cred;
            };
            let oauth = object
                .entry("claudeAiOauth")
                .or_insert_with(|| Value::Object(Map::new()));
            let Some(oauth) = oauth.as_object_mut() else {
                return cred;
            };
            oauth.insert("accessToken".into(), Value::String(access.into()));
            if let Some(refresh) = data.get("refresh_token").and_then(Value::as_str) {
                oauth.insert("refreshToken".into(), Value::String(refresh.into()));
            }
            if let Some(seconds) = data.get("expires_in").and_then(Value::as_i64) {
                oauth.insert(
                    "expiresAt".into(),
                    Value::from(Utc::now().timestamp_millis() + seconds * 1000),
                );
            }
            let _ = self.write_json(path, &cred, true);
            cred
        })
    }

    pub fn fetch_usage_for<'a>(
        &'a self,
        name: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<PUsage>> + Send + 'a>> {
        self.fetch_usage(name)
    }

    pub async fn add_via_login(&self) -> LoginFlowResult {
        // TODO(phase-4): the interactive browser OAuth flow is intentionally not ported yet.
        LoginFlowResult {
            ok: false,
            error: Some("Claude browser login is deferred to phase 4".into()),
            ..LoginFlowResult::default()
        }
    }
}

fn iso_to_ms(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|date| date.timestamp_millis())
}
fn is_record(value: &Value) -> bool {
    value.is_object()
}

pub(crate) fn usage_window(raw: Option<&Value>, window_minutes: i64) -> Option<PWindow> {
    let raw = raw?;
    if let Some(number) = raw.as_f64() {
        return Some(PWindow {
            used_percent: number,
            window_minutes: Some(window_minutes),
            resets_at: None,
        });
    }
    if !is_record(raw) {
        return None;
    }
    let used_percent = raw
        .get("utilization")
        .and_then(Value::as_f64)
        .or_else(|| raw.get("percent").and_then(Value::as_f64))?;
    Some(PWindow {
        used_percent,
        window_minutes: Some(window_minutes),
        resets_at: iso_to_ms(raw.get("resets_at").or_else(|| raw.get("resetsAt"))),
    })
}

pub(crate) fn usage_limit_window(
    raw: Option<&Value>,
    kind: &str,
    model_display_name: Option<&str>,
    window_minutes: i64,
) -> Option<PWindow> {
    let entries = raw?.as_array()?;
    let entry = entries.iter().find(|entry| {
        if entry.get("kind").and_then(Value::as_str) != Some(kind) {
            return false;
        }
        let Some(model_name) = model_display_name else {
            return true;
        };
        entry
            .get("scope")
            .and_then(|v| v.get("model"))
            .and_then(|v| v.get("display_name"))
            .and_then(Value::as_str)
            .map(|value| value.to_lowercase() == model_name)
            .unwrap_or(false)
    });
    usage_window(entry, window_minutes)
}

impl Provider for ClaudeProvider {
    fn list_accounts(&self) -> Vec<PAccount> {
        self.list_slots()
    }
    fn active_account_name(&self) -> Option<String> {
        let id = self
            .live_oauth_account()?
            .get("accountUuid")?
            .as_str()?
            .to_owned();
        self.list_slots()
            .into_iter()
            .find(|account| account.account_id.as_deref() == Some(&id))
            .map(|account| account.name)
    }
    fn has_live_auth(&self) -> bool {
        self.read_credentials(&self.paths.live_credentials_file())
            .and_then(|value| value.get("claudeAiOauth").cloned())
            .and_then(|value| value.get("accessToken").cloned())
            .and_then(|value| {
                value
                    .as_str()
                    .filter(|token| !token.is_empty())
                    .map(String::from)
            })
            .is_some()
    }
    fn import_current(&self, name: Option<&str>) -> Result<PAccount, CoreError> {
        let cred = self
            .read_credentials(&self.paths.live_credentials_file())
            .filter(|v| {
                v.get("claudeAiOauth")
                    .and_then(|v| v.get("accessToken"))
                    .and_then(Value::as_str)
                    .is_some()
            })
            .ok_or_else(|| {
                CoreError::Invalid("No live Claude login (~/.claude/.credentials.json)".into())
            })?;
        let oauth_account = self.live_oauth_account();
        let slot = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                self.derive_slot_name(
                    oauth_account
                        .as_ref()
                        .and_then(|v| v.get("emailAddress"))
                        .and_then(Value::as_str),
                )
            });
        let old_meta = read_json(&self.paths.slot_meta_file(&slot))
            .unwrap_or_else(|| Value::Object(Map::new()));
        std::fs::create_dir_all(self.paths.slot_dir(&slot))?;
        self.write_json(&self.paths.slot_credentials_file(&slot), &cred, true)?;
        let mut meta = old_meta.as_object().cloned().unwrap_or_default();
        meta.insert(
            "oauthAccount".into(),
            oauth_account.clone().unwrap_or(Value::Null),
        );
        self.write_json(
            &self.paths.slot_meta_file(&slot),
            &Value::Object(meta.clone()),
            false,
        )?;
        Ok(PAccount {
            name: slot,
            email: oauth_account
                .as_ref()
                .and_then(|v| v.get("emailAddress"))
                .and_then(Value::as_str)
                .map(String::from),
            account_id: oauth_account
                .as_ref()
                .and_then(|v| v.get("accountUuid"))
                .and_then(Value::as_str)
                .map(String::from),
            label: label_for(&Value::Object(meta), &cred),
            enabled: old_meta.get("enabled").and_then(Value::as_bool) != Some(false),
        })
    }
    fn remove_account(&self, name: &str) -> Result<(), CoreError> {
        let directory = self.paths.slot_dir(name);
        if !directory.exists() {
            return Ok(());
        }
        if directory.is_dir() {
            std::fs::remove_dir_all(directory)?;
        } else {
            std::fs::remove_file(directory)?;
        }
        Ok(())
    }
    fn rename_account(&self, old_name: &str, new_name: &str) -> Result<(), CoreError> {
        let clean = sanitize_name(new_name);
        if clean.is_empty() {
            return Err(CoreError::Invalid("Invalid account name".into()));
        }
        if self.paths.slot_dir(&clean).exists() {
            return Err(CoreError::Invalid("Name already in use".into()));
        }
        std::fs::rename(self.paths.slot_dir(old_name), self.paths.slot_dir(&clean))?;
        Ok(())
    }
    fn set_account_enabled(&self, name: &str, enabled: bool) -> Result<(), CoreError> {
        if !self.list_slots().iter().any(|account| account.name == name) {
            return Err(CoreError::Invalid(format!(
                "Account \"{name}\" is not enrolled"
            )));
        }
        let mut meta = read_json(&self.paths.slot_meta_file(name))
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        meta.insert("enabled".into(), Value::Bool(enabled));
        self.write_json(
            &self.paths.slot_meta_file(name),
            &Value::Object(meta),
            false,
        )
    }
    fn sync_live_back_to_slot(&self) -> Result<(), CoreError> {
        let Some(name) = self.active_account_name() else {
            return Ok(());
        };
        let Some(cred) = self
            .read_credentials(&self.paths.live_credentials_file())
            .filter(|v| v.get("claudeAiOauth").is_some())
        else {
            return Ok(());
        };
        self.write_json(&self.paths.slot_credentials_file(&name), &cred, true)
    }
    fn install_auth(&self, name: &str) -> Result<(), CoreError> {
        let cred = self
            .read_credentials(&self.paths.slot_credentials_file(name))
            .filter(|v| v.get("claudeAiOauth").is_some())
            .ok_or_else(|| {
                CoreError::Invalid(format!("Account \"{name}\" has no credentials.json"))
            })?;
        if let Ok(mut cache) = self.usage.cache.lock() {
            cache.remove("@live");
        }
        self.write_json(&self.paths.live_credentials_file(), &cred, true)?;
        if let Some(account) =
            read_json(&self.paths.slot_meta_file(name)).and_then(|v| v.get("oauthAccount").cloned())
        {
            if let Some(mut claude_json) =
                read_json(&self.paths.claude_json_file()).and_then(|v| v.as_object().cloned())
            {
                claude_json.insert("oauthAccount".into(), account);
                self.write_json(
                    &self.paths.claude_json_file(),
                    &Value::Object(claude_json),
                    false,
                )?;
            }
        }
        Ok(())
    }
    fn fetch_usage<'a>(
        &'a self,
        name: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<PUsage>> + Send + 'a>> {
        Box::pin(async move {
            let key = name.unwrap_or("@live").to_owned();
            let now = Utc::now().timestamp_millis();
            let cached = self
                .usage
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
                .usage
                .rate_limited_until
                .lock()
                .ok()
                .and_then(|map| map.get(&key).copied())
                .is_some_and(|until| now < until)
            {
                return cached.and_then(|entry| entry.usage);
            }
            let path = name
                .map(|n| self.paths.slot_credentials_file(n))
                .unwrap_or_else(|| self.paths.live_credentials_file());
            let mut cred = self.read_credentials(&path)?;
            cred.get("claudeAiOauth")
                .and_then(|v| v.get("accessToken"))
                .and_then(Value::as_str)?;
            cred = self.refresh_if_needed(&path, cred).await;
            let token = cred.get("claudeAiOauth")?.get("accessToken")?.as_str()?;
            let headers = HashMap::from([
                (String::from("Authorization"), format!("Bearer {token}")),
                (String::from("Accept"), String::from("application/json")),
                (
                    String::from("Content-Type"),
                    String::from("application/json"),
                ),
                (
                    String::from("anthropic-beta"),
                    String::from("oauth-2025-04-20"),
                ),
                (
                    String::from("User-Agent"),
                    String::from("claude-code/2.1.69"),
                ),
            ]);
            let response = self
                .transport
                .request(HttpRequest {
                    method: "GET".into(),
                    url: USAGE_URL.into(),
                    headers,
                    body: None,
                })
                .await
                .ok()?;
            if response.status == 429 {
                let backoff = response
                    .headers
                    .get("retry-after")
                    .and_then(|v| v.parse::<i64>().ok())
                    .filter(|v| *v >= 0)
                    .map(|v| v * 1000)
                    .unwrap_or(DEFAULT_429_BACKOFF_MS);
                if let Ok(mut map) = self.usage.rate_limited_until.lock() {
                    map.insert(key, now + backoff);
                }
                return cached.and_then(|entry| entry.usage);
            }
            if !(200..300).contains(&response.status) {
                return cached.and_then(|entry| entry.usage);
            }
            let data = response.body.unwrap_or(Value::Object(Map::new()));
            let limits = data.get("limits");
            let meta = name
                .map(|n| read_json(&self.paths.slot_meta_file(n)))
                .unwrap_or_else(|| {
                    let mut object = Map::new();
                    object.insert(
                        "oauthAccount".into(),
                        self.live_oauth_account().unwrap_or(Value::Null),
                    );
                    Some(Value::Object(object))
                });
            let usage = PUsage {
                primary: usage_window(data.get("five_hour"), 300)
                    .or_else(|| usage_limit_window(limits, "session", None, 300)),
                secondary: usage_window(data.get("seven_day"), 10080)
                    .or_else(|| usage_limit_window(limits, "weekly_all", None, 10080)),
                fable: usage_limit_window(limits, "weekly_scoped", Some("fable"), 10080)
                    .or_else(|| usage_window(data.get("seven_day_omelette"), 10080)),
                plan_type: cred
                    .get("claudeAiOauth")
                    .and_then(|v| v.get("subscriptionType"))
                    .and_then(Value::as_str)
                    .map(String::from),
                email: meta
                    .as_ref()
                    .and_then(|v| v.get("oauthAccount"))
                    .and_then(|v| v.get("emailAddress"))
                    .and_then(Value::as_str)
                    .map(String::from),
            };
            if let Ok(mut cache) = self.usage.cache.lock() {
                cache.insert(
                    key.clone(),
                    CacheEntry {
                        at: now,
                        usage: Some(usage.clone()),
                    },
                );
            }
            if let Ok(mut map) = self.usage.rate_limited_until.lock() {
                map.remove(&key);
            }
            Some(usage)
        })
    }
    fn cached_usage(&self, name: Option<&str>) -> Option<PUsage> {
        self.usage
            .cache
            .lock()
            .ok()?
            .get(name.unwrap_or("@live"))?
            .usage
            .clone()
    }
}

fn label_for(meta: &Value, cred: &Value) -> Option<String> {
    let email = meta
        .get("oauthAccount")
        .and_then(|v| v.get("emailAddress"))
        .and_then(Value::as_str);
    let plan = cred
        .get("claudeAiOauth")
        .and_then(|v| v.get("subscriptionType"))
        .and_then(Value::as_str);
    match (email, plan) {
        (Some(email), Some(plan)) => Some(format!("{email} · {plan}")),
        (Some(email), None) => Some(email.into()),
        (None, Some(plan)) => Some(plan.into()),
        _ => None,
    }
}

fn sanitize_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|ch| if "\\/:*?\"<>|".contains(ch) { '_' } else { ch })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{usage_limit_window, usage_window, ClaudeProvider};
    use crate::core::paths::ClaudePaths;
    use crate::core::providers::{HttpFuture, HttpRequest, HttpResponse, HttpTransport, Provider};
    use serde_json::json;
    use tempfile::tempdir;

    struct FixtureTransport {
        body: serde_json::Value,
        status: u16,
    }
    impl HttpTransport for FixtureTransport {
        fn request(&self, request: HttpRequest) -> HttpFuture {
            assert_eq!(request.url, "https://api.anthropic.com/api/oauth/usage");
            let response = HttpResponse {
                status: self.status,
                headers: std::collections::HashMap::new(),
                body: Some(self.body.clone()),
            };
            Box::pin(async move { Ok(response) })
        }
    }

    #[test]
    fn parses_limits_fable_and_older_fallback() {
        let body = json!({"five_hour":null,"seven_day":null,"seven_day_omelette":{"percent":3,"resets_at":"2026-07-14T20:00:00+00:00"},"limits":[{"kind":"session","percent":9},{"kind":"weekly_all","percent":1},{"kind":"weekly_scoped","percent":2,"scope":{"model":{"display_name":"FABLE"}}}]});
        assert_eq!(
            usage_window(body.get("seven_day_omelette"), 10080)
                .expect("fable")
                .used_percent,
            3.0
        );
        assert_eq!(
            usage_limit_window(body.get("limits"), "session", None, 300)
                .expect("session")
                .used_percent,
            9.0
        );
        assert_eq!(
            usage_limit_window(body.get("limits"), "weekly_scoped", Some("fable"), 10080)
                .expect("scoped")
                .used_percent,
            2.0
        );
    }

    #[tokio::test]
    async fn fetches_fixture_with_fallbacks_and_fable_scope() {
        let root = tempdir().expect("tempdir");
        let paths = ClaudePaths {
            home: root.path().to_path_buf(),
            config_dir: root.path().join(".claude"),
            accounts_root: root.path().join(".claude-accounts"),
        };
        std::fs::create_dir_all(&paths.config_dir).expect("config dir");
        std::fs::write(
            paths.live_credentials_file(),
            json!({"claudeAiOauth":{"accessToken":"token","subscriptionType":"pro"}}).to_string(),
        )
        .expect("credentials");
        let body = json!({"five_hour":null,"seven_day":null,"seven_day_omelette":{"percent":99,"resets_at":"2026-07-15T20:00:00+00:00"},"limits":[{"kind":"session","percent":9,"resets_at":"2026-07-10T05:50:00+00:00"},{"kind":"weekly_all","percent":1,"resets_at":"2026-07-13T20:00:00+00:00"},{"kind":"weekly_scoped","percent":2,"resets_at":"2026-07-13T20:00:00+00:00","scope":{"model":{"display_name":"FABLE"}}}]});
        let provider = ClaudeProvider::new(paths, Arc::new(FixtureTransport { body, status: 200 }));
        let usage = provider.fetch_usage(None).await.expect("usage");
        assert_eq!(usage.primary.expect("primary").used_percent, 9.0);
        assert_eq!(
            usage.secondary.expect("secondary").window_minutes,
            Some(10080)
        );
        assert_eq!(usage.fable.expect("fable").used_percent, 2.0);
    }

    #[test]
    fn import_preserves_disabled_slot_and_unknown_state_change_is_rejected() {
        let root = tempdir().expect("tempdir");
        let paths = ClaudePaths {
            home: root.path().to_path_buf(),
            config_dir: root.path().join(".claude"),
            accounts_root: root.path().join(".claude-accounts"),
        };
        std::fs::create_dir_all(&paths.config_dir).expect("config dir");
        std::fs::create_dir_all(paths.slot_dir("qa")).expect("slot dir");
        std::fs::write(
            paths.live_credentials_file(),
            json!({"claudeAiOauth":{"accessToken":"token"}}).to_string(),
        )
        .expect("credentials");
        std::fs::write(
            paths.slot_meta_file("qa"),
            json!({"enabled":false}).to_string(),
        )
        .expect("meta");
        let provider = ClaudeProvider::new(
            paths,
            Arc::new(FixtureTransport {
                body: json!({}),
                status: 200,
            }),
        );
        let account = provider.import_current(Some("qa")).expect("import");
        assert!(!account.enabled);
        assert!(!provider.list_accounts()[0].enabled);
        let error = provider
            .set_account_enabled("missing", false)
            .expect_err("unknown account");
        assert!(error.to_string().contains("not enrolled"));
    }
}
