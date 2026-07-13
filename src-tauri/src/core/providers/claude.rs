use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::{DateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::{HttpRequest, Provider, ReqwestTransport, SharedTransport};
use crate::core::paths::ClaudePaths;
use crate::core::platform;
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
const LOGIN_PORT: u16 = 54545;
const LOGIN_REDIRECT: &str = "http://localhost:54545/callback";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

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
        self.add_via_login_with_callback(None).await
    }

    pub async fn add_via_login_with_callback(
        &self,
        on_url: Option<Arc<dyn Fn(String) + Send + Sync>>,
    ) -> LoginFlowResult {
        let mut verifier_bytes = [0u8; 32];
        let mut state_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut verifier_bytes);
        OsRng.fill_bytes(&mut state_bytes);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
        let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let auth_query = format!(
            "client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
            urlencoding::encode(CLIENT_ID), urlencoding::encode(LOGIN_REDIRECT),
            urlencoding::encode(SCOPES), urlencoding::encode(&challenge), urlencoding::encode(&state)
        );
        let url = format!(
            "https://claude.ai/logout?returnTo={}",
            urlencoding::encode(&format!("/oauth/authorize?{auth_query}"))
        );
        let callback = tokio::task::spawn_blocking({
            let state = state.clone();
            move || wait_for_callback(&state)
        });
        if let Some(on_url) = on_url { on_url(url.clone()); }
        if let Err(error) = platform::open_url(&url) {
            return LoginFlowResult { ok: false, error: Some(error), ..LoginFlowResult::default() };
        }
        let code = match callback.await {
            Ok(Ok(code)) => code,
            Ok(Err(error)) => return LoginFlowResult { ok: false, error: Some(error), ..LoginFlowResult::default() },
            Err(error) => return LoginFlowResult { ok: false, error: Some(error.to_string()), ..LoginFlowResult::default() },
        };
        let body = match self.exchange_authorization_code(&code, &state, &verifier).await {
            Ok(body) => body,
            Err(error) => return LoginFlowResult { ok: false, error: Some(error), ..LoginFlowResult::default() },
        };
        let access_token = match body.get("access_token").and_then(Value::as_str) {
            Some(token) if !token.is_empty() => token,
            _ => return LoginFlowResult { ok: false, error: Some("token exchange returned no access_token".into()), ..LoginFlowResult::default() },
        };
        let oauth = serde_json::json!({
            "accessToken": access_token,
            "refreshToken": body.get("refresh_token").cloned().unwrap_or(Value::Null),
            "expiresAt": Utc::now().timestamp_millis() + body.get("expires_in").and_then(Value::as_i64).unwrap_or(0) * 1000,
            "scopes": body.get("scope").and_then(Value::as_str).map(|s| s.split(' ').map(String::from).collect::<Vec<_>>()).unwrap_or_else(|| SCOPES.split(' ').map(String::from).collect::<Vec<_>>()),
            "subscriptionType": body.get("account").and_then(|v| v.get("subscription_type")).cloned().unwrap_or(Value::Null),
        });
        let mut email = body.get("account").and_then(|v| v.get("email_address")).and_then(Value::as_str).map(String::from)
            .or_else(|| body.get("account").and_then(|v| v.get("email")).and_then(Value::as_str).map(String::from));
        let mut uuid = body.get("account").and_then(|v| v.get("uuid")).and_then(Value::as_str).map(String::from);
        let mut org_uuid = None; let mut org_name = None;
        if let Ok(profile) = self.transport.request(HttpRequest { method: "GET".into(), url: PROFILE_URL.into(), headers: std::collections::HashMap::from([(String::from("Authorization"), format!("Bearer {access_token}")), (String::from("Accept"), String::from("application/json")), (String::from("anthropic-beta"), String::from("oauth-2025-04-20"))]), body: None }).await {
            if let Some(profile) = profile.body {
                email = email.or_else(|| profile.get("account").and_then(|v| v.get("email_address")).and_then(Value::as_str).map(String::from));
                uuid = uuid.or_else(|| profile.get("account").and_then(|v| v.get("uuid")).and_then(Value::as_str).map(String::from));
                org_uuid = profile.get("organization").and_then(|v| v.get("uuid")).and_then(Value::as_str).map(String::from);
                org_name = profile.get("organization").and_then(|v| v.get("name")).and_then(Value::as_str).map(String::from);
            }
        }
        let existing = uuid.as_deref().and_then(|id| self.list_slots().into_iter().find(|a| a.account_id.as_deref() == Some(id)));
        let slot = existing.map(|a| a.name).unwrap_or_else(|| self.derive_slot_name(email.as_deref()));
        let old = read_json(&self.paths.slot_meta_file(&slot)).unwrap_or_else(|| Value::Object(Map::new()));
        let mut account = old.as_object().cloned().unwrap_or_default();
        let mut oauth_account = account.remove("oauthAccount").unwrap_or(Value::Object(Map::new()));
        if let Some(object) = oauth_account.as_object_mut() {
            object.insert("accountUuid".into(), uuid.clone().map(Value::String).unwrap_or(Value::Null));
            object.insert("emailAddress".into(), email.clone().map(Value::String).unwrap_or(Value::Null));
            if let Some(value) = &org_uuid { object.insert("organizationUuid".into(), Value::String(value.clone())); }
            if let Some(value) = &org_name { object.insert("organizationName".into(), Value::String(value.clone())); }
        }
        account.insert("oauthAccount".into(), oauth_account.clone());
        if let Err(error) = std::fs::create_dir_all(self.paths.slot_dir(&slot))
            .map_err(CoreError::from)
            .and_then(|_| self.write_json(&self.paths.slot_credentials_file(&slot), &serde_json::json!({"claudeAiOauth": oauth}), true))
            .and_then(|_| self.write_json(&self.paths.slot_meta_file(&slot), &Value::Object(account), false))
        { return LoginFlowResult { ok: false, error: Some(error.to_string()), ..LoginFlowResult::default() }; }
        LoginFlowResult { ok: true, name: Some(slot), email, error: None }
    }

    async fn exchange_authorization_code(&self, code: &str, state: &str, verifier: &str) -> Result<Value, String> {
        let body = serde_json::json!({"grant_type":"authorization_code","code":code,"state":state,"client_id":CLIENT_ID,"redirect_uri":LOGIN_REDIRECT,"code_verifier":verifier});
        let response = self.transport.request(HttpRequest { method: "POST".into(), url: REFRESH_URL.into(), headers: std::collections::HashMap::from([(String::from("Content-Type"), String::from("application/json"))]), body: Some(body.to_string()) }).await.map_err(|error| error.to_string())?;
        if !(200..300).contains(&response.status) { return Err(format!("token exchange failed: HTTP {}", response.status)); }
        response.body.ok_or_else(|| "token exchange returned invalid JSON".into())
    }
}

fn wait_for_callback(state: &str) -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", LOGIN_PORT)).map_err(|error| error.to_string())?;
    listener.set_nonblocking(true).map_err(|error| error.to_string())?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut bytes = [0u8; 8192]; let count = stream.read(&mut bytes).map_err(|error| error.to_string())?;
                let request = String::from_utf8_lossy(&bytes[..count]);
                let target = request.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("/");
                let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
                let mut code = None; let mut returned_state = None; let mut error = None;
                for part in query.split('&') { let (key, value) = part.split_once('=').unwrap_or((part, "")); let value = urlencoding::decode(value).map_err(|e| e.to_string())?.into_owned(); match key { "code" => code = Some(value), "state" => returned_state = Some(value), "error" => error = Some(value), _ => {} } }
                if target.split('?').next() != Some("/callback") { let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n"); continue; }
                if returned_state.as_deref() != Some(state) { let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\nStale login attempt"); continue; }
                if let Some(error) = error { let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\nLogin denied"); return Err(format!("login denied: {error}")); }
                let Some(code) = code.filter(|code| !code.is_empty()) else { return Err("no code in callback".into()) };
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\nLogin complete - you can close this tab.");
                return Ok(code);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("login timed out (5 min)".into())
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::{usage_limit_window, usage_window, ClaudeProvider, REFRESH_URL};
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

    struct LoginFixtureTransport;
    impl HttpTransport for LoginFixtureTransport {
        fn request(&self, request: HttpRequest) -> HttpFuture {
            assert_eq!(request.method, "POST");
            assert_eq!(request.url, REFRESH_URL);
            let body = request.body.expect("token request body");
            assert!(body.contains("authorization-code"));
            assert!(body.contains("verifier-fixture"));
            Box::pin(async { Ok(HttpResponse { status: 200, headers: HashMap::new(), body: Some(json!({"access_token":"fake-access","refresh_token":"fake-refresh","expires_in":3600})) }) })
        }
    }

    #[tokio::test]
    async fn authorization_code_exchange_uses_pkce_without_touching_auth_files() {
        let root = tempdir().expect("tempdir");
        let paths = ClaudePaths { home: root.path().to_path_buf(), config_dir: root.path().join(".claude"), accounts_root: root.path().join(".claude-accounts") };
        let provider = ClaudeProvider::new(paths.clone(), Arc::new(LoginFixtureTransport));
        let response = provider.exchange_authorization_code("authorization-code", "state-fixture", "verifier-fixture").await.expect("token response");
        assert_eq!(response["access_token"], "fake-access");
        assert!(!paths.live_credentials_file().exists());
        assert!(!paths.accounts_root.exists());
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
