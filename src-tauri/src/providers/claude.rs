use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::atomic_fs::atomic_write;
use crate::provider_types::{LoginFlowResult, PAccount, PUsage, PWindow};

/// Claude Code account provider.
///
/// Live auth lives in TWO places (verified against Claude Code on Windows):
///  - <claudeHome>/.credentials.json -> { claudeAiOauth: { accessToken,
///    refreshToken, expiresAt(ms), scopes[], subscriptionType,
///    rateLimitTier } }
///  - ~/.claude.json -> { oauthAccount: { accountUuid, emailAddress, ... },
///    ...other settings } (large file with unrelated state — only the
///    oauthAccount field is patched)
///
/// Usage + refresh endpoints mirror the OpenUsage `claude` plugin. The usage
/// endpoint rate-limits aggressively — responses are cached for
/// USAGE_CACHE_MS per slot and a 429 sets a Retry-After backoff.
///
/// Credentials/meta are kept as loose `serde_json::Value` (not strongly
/// typed structs) deliberately: `~/.claude.json` in particular carries a lot
/// of unrelated Claude Code settings state that must survive a read-patch-
/// write round trip untouched, exactly like the original TS.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const REFRESH_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// refresh 5 min before expiry
const REFRESH_BUFFER_MS: i64 = 5 * 60 * 1000;
/// per-slot usage cache
const USAGE_CACHE_MS: i64 = 5 * 60 * 1000;
const DEFAULT_429_BACKOFF_MS: i64 = 5 * 60 * 1000;

const SWITCH_ACCOUNT_URL: &str = "https://claude.ai/logout";
const AUTHORIZE_PATH: &str = "/oauth/authorize";
/// Claude Code's own callback port — the only localhost redirect
/// whitelisted for CLIENT_ID. One login at a time (fixed port), same as the
/// Codex flow.
const LOGIN_PORT: u16 = 54545;
const LOGIN_REDIRECT: &str = "http://localhost:54545/callback";
const LOGIN_TIMEOUT_SECS: u64 = 5 * 60;
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn claude_home() -> PathBuf {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => dirs::home_dir().unwrap_or_default().join(".claude"),
    }
}
fn live_cred_file() -> PathBuf {
    claude_home().join(".credentials.json")
}
fn claude_json_file() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude.json")
}
fn accounts_root() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude-accounts")
}
fn slot_dir(name: &str) -> PathBuf {
    accounts_root().join(name)
}
fn slot_cred_file(name: &str) -> PathBuf {
    slot_dir(name).join("credentials.json")
}
fn slot_meta_file(name: &str) -> PathBuf {
    slot_dir(name).join("meta.json")
}

fn read_json(file: &Path) -> Option<Value> {
    let raw = fs::read_to_string(file).ok()?;
    serde_json::from_str(&raw).ok()
}

/// MUST be minified when `minify` — Claude Code chokes on pretty-printed
/// credentials via keychain paths.
fn write_json_atomic(file: &Path, data: &Value, minify: bool) -> std::io::Result<()> {
    let text = if minify {
        serde_json::to_string(data).unwrap()
    } else {
        serde_json::to_string_pretty(data).unwrap()
    };
    atomic_write(file, text.as_bytes())
}

fn live_oauth_account() -> Option<Value> {
    read_json(&claude_json_file())?
        .get("oauthAccount")
        .filter(|v| !v.is_null())
        .cloned()
}

fn derive_slot_name(email: Option<&str>) -> String {
    let local = email.unwrap_or("").split('@').next().unwrap_or("");
    let sanitized: String = local
        .chars()
        .map(|c| {
            if "\\/:*?\"<>|".contains(c) || c.is_whitespace() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let base = if sanitized.is_empty() {
        format!("claude-{}", now_ms())
    } else {
        sanitized
    };
    let mut name = base.clone();
    let mut i = 2;
    while slot_dir(&name).exists() {
        name = format!("{base}-{i}");
        i += 1;
    }
    name
}

fn label_for(meta: Option<&Value>, cred: Option<&Value>) -> Option<String> {
    let email = meta
        .and_then(|m| m.get("oauthAccount"))
        .and_then(|o| o.get("emailAddress"))
        .and_then(|v| v.as_str());
    let plan = cred
        .and_then(|c| c.get("claudeAiOauth"))
        .and_then(|o| o.get("subscriptionType"))
        .and_then(|v| v.as_str());
    let parts: Vec<&str> = [email, plan].into_iter().flatten().collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

fn list_slots() -> Vec<PAccount> {
    let root = accounts_root();
    let entries = match fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut accounts: Vec<PAccount> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let meta = read_json(&slot_meta_file(&name));
            let cred = read_json(&slot_cred_file(&name));
            let oauth_account = meta.as_ref().and_then(|m| m.get("oauthAccount"));
            PAccount {
                email: oauth_account
                    .and_then(|o| o.get("emailAddress"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                account_id: oauth_account
                    .and_then(|o| o.get("accountUuid"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                label: label_for(meta.as_ref(), cred.as_ref()),
                enabled: meta
                    .as_ref()
                    .map(|m| !matches!(m.get("enabled"), Some(Value::Bool(false))))
                    .unwrap_or(true),
                name,
            }
        })
        .collect();
    accounts.sort_by(|a, b| a.name.cmp(&b.name));
    accounts
}

fn active_name() -> Option<String> {
    let id = live_oauth_account()?
        .get("accountUuid")?
        .as_str()?
        .to_string();
    list_slots()
        .into_iter()
        .find(|a| a.account_id.as_deref() == Some(id.as_str()))
        .map(|a| a.name)
}

// ---------------------------------------------------------------------------
// Token refresh + usage
// ---------------------------------------------------------------------------

async fn refresh_if_needed(cred_file: &Path, cred: Value) -> Value {
    let oauth = match cred.get("claudeAiOauth") {
        Some(o) if o.is_object() => o.clone(),
        _ => return cred,
    };
    let Some(refresh_token_val) = oauth.get("refreshToken").and_then(|v| v.as_str()) else {
        return cred;
    };
    let refresh_token_val = refresh_token_val.to_string();
    let expires_at = oauth.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);
    if now_ms() < expires_at - REFRESH_BUFFER_MS {
        return cred;
    }

    let res = match http_client()
        .post(REFRESH_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token_val,
            "client_id": CLIENT_ID,
            "scope": SCOPES,
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return cred,
    };
    if !res.status().is_success() {
        return cred;
    }
    let body: Value = match res.json().await {
        Ok(b) => b,
        Err(_) => return cred,
    };
    let Some(access_token) = body.get("access_token").and_then(|v| v.as_str()) else {
        return cred;
    };

    let mut new_oauth = oauth;
    new_oauth["accessToken"] = Value::String(access_token.to_string());
    if let Some(rt) = body.get("refresh_token").and_then(|v| v.as_str()) {
        new_oauth["refreshToken"] = Value::String(rt.to_string());
    }
    if let Some(exp) = body.get("expires_in").and_then(|v| v.as_i64()) {
        new_oauth["expiresAt"] = Value::from(now_ms() + exp * 1000);
    }
    let mut new_cred = cred;
    new_cred["claudeAiOauth"] = new_oauth;
    let _ = write_json_atomic(cred_file, &new_cred, true);
    new_cred
}

fn iso_to_ms(v: Option<&Value>) -> Option<i64> {
    let s = v?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn usage_window(raw: &Value, window_minutes: i64) -> Option<PWindow> {
    if let Some(n) = raw.as_f64() {
        return Some(PWindow {
            used_percent: n,
            window_minutes: Some(window_minutes),
            resets_at: None,
        });
    }
    let obj = raw.as_object()?;
    let used_percent = obj
        .get("utilization")
        .and_then(|v| v.as_f64())
        .or_else(|| obj.get("percent").and_then(|v| v.as_f64()))?;
    let resets_at = iso_to_ms(obj.get("resets_at")).or_else(|| iso_to_ms(obj.get("resetsAt")));
    Some(PWindow {
        used_percent,
        window_minutes: Some(window_minutes),
        resets_at,
    })
}

fn usage_limit_window(
    raw: Option<&Value>,
    kind: &str,
    model_display_name: Option<&str>,
    window_minutes: i64,
) -> Option<PWindow> {
    let arr = raw?.as_array()?;
    let limit = arr.iter().find(|entry| {
        let Some(obj) = entry.as_object() else {
            return false;
        };
        if obj.get("kind").and_then(|v| v.as_str()) != Some(kind) {
            return false;
        }
        let Some(want_model) = model_display_name else {
            return true;
        };
        obj.get("scope")
            .and_then(|s| s.get("model"))
            .and_then(|m| m.get("display_name"))
            .and_then(|v| v.as_str())
            .map(|d| d.to_lowercase() == want_model)
            .unwrap_or(false)
    })?;
    usage_window(limit, window_minutes)
}

fn usage_cache() -> &'static Mutex<HashMap<String, (i64, Option<PUsage>)>> {
    static C: OnceLock<Mutex<HashMap<String, (i64, Option<PUsage>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}
fn rate_limited_until_map() -> &'static Mutex<HashMap<String, i64>> {
    static R: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_fallback(key: &str) -> Option<PUsage> {
    usage_cache().lock().unwrap().get(key).and_then(|(_, u)| u.clone())
}

pub async fn fetch_usage_for(name: Option<&str>) -> Option<PUsage> {
    let key = name.unwrap_or("@live").to_string();
    let now = now_ms();
    if let Some((at, usage)) = usage_cache().lock().unwrap().get(&key).cloned() {
        if now - at < USAGE_CACHE_MS {
            return usage;
        }
    }
    let rl_until = *rate_limited_until_map().lock().unwrap().get(&key).unwrap_or(&0);
    if now < rl_until {
        return cached_fallback(&key);
    }

    let cred_file = match name {
        None => live_cred_file(),
        Some(n) => slot_cred_file(n),
    };
    let Some(mut cred) = read_json(&cred_file) else {
        return None;
    };
    let has_access = cred
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|v| v.as_str())
        .is_some();
    if !has_access {
        return None;
    }
    cred = refresh_if_needed(&cred_file, cred).await;
    let access_token = cred
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let res = match http_client()
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", "claude-code/2.1.69")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return cached_fallback(&key),
    };

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
        rate_limited_until_map().lock().unwrap().insert(key.clone(), now + backoff);
        return cached_fallback(&key);
    }
    if !res.status().is_success() {
        return cached_fallback(&key);
    }

    let data: Value = match res.json().await {
        Ok(d) => d,
        Err(_) => return cached_fallback(&key),
    };
    let limits = data.get("limits");

    let meta = if name.is_none() {
        serde_json::json!({ "oauthAccount": live_oauth_account() })
    } else {
        read_json(&slot_meta_file(name.unwrap())).unwrap_or(Value::Null)
    };

    let usage = PUsage {
        primary: data
            .get("five_hour")
            .and_then(|v| usage_window(v, 300))
            .or_else(|| usage_limit_window(limits, "session", None, 300)),
        secondary: data
            .get("seven_day")
            .and_then(|v| usage_window(v, 10080))
            .or_else(|| usage_limit_window(limits, "weekly_all", None, 10080)),
        fable: usage_limit_window(limits, "weekly_scoped", Some("fable"), 10080)
            .or_else(|| data.get("seven_day_omelette").and_then(|v| usage_window(v, 10080))),
        plan_type: cred
            .get("claudeAiOauth")
            .and_then(|o| o.get("subscriptionType"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        email: meta
            .get("oauthAccount")
            .and_then(|o| o.get("emailAddress"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    };
    usage_cache()
        .lock()
        .unwrap()
        .insert(key.clone(), (now, Some(usage.clone())));
    rate_limited_until_map().lock().unwrap().remove(&key);
    Some(usage)
}

pub fn cached_usage_for(name: Option<&str>) -> Option<PUsage> {
    cached_fallback(name.unwrap_or("@live"))
}

// ---------------------------------------------------------------------------
// Add-account login flow (OAuth authorization code + PKCE)
// ---------------------------------------------------------------------------

fn b64url(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_bytes(n: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

fn sha256(data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

fn build_query(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

fn login_err(message: String) -> LoginFlowResult {
    LoginFlowResult {
        ok: false,
        name: None,
        email: None,
        error: Some(message),
    }
}

fn bind_callback_server(port: u16) -> Result<tiny_http::Server, String> {
    tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| e.to_string())
}

/// Blocking accept loop, run on a dedicated blocking thread. Mirrors the
/// original's one-shot local HTTP server: waits for `/callback`, validates
/// `state`, and yields the authorization `code` (or an error).
fn run_callback_loop(server: tiny_http::Server, state: String) -> Result<String, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(LOGIN_TIMEOUT_SECS);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "login timed out ({} min)",
                LOGIN_TIMEOUT_SECS / 60
            ));
        }
        let req = match server.recv_timeout(remaining) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        };

        let url_str = req.url().to_string();
        let (path, query) = url_str.split_once('?').unwrap_or((url_str.as_str(), ""));
        if path != "/callback" {
            let _ = req.respond(tiny_http::Response::from_string("").with_status_code(404));
            continue;
        }
        let params: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let got_state = params.get("state").cloned().unwrap_or_default();
        if got_state != state {
            // Stale tab from an earlier attempt — reject it but keep waiting
            // for the real callback instead of killing the login in progress.
            let body = "<html><body>Stale login attempt — go back to the app and click add again.</body></html>";
            let _ = respond_html(req, 400, body);
            continue;
        }

        let err_param = params.get("error").cloned();
        let code = params.get("code").cloned();
        let fail = if let Some(e) = &err_param {
            Some(format!("login denied: {e}"))
        } else if code.is_none() {
            Some("no code in callback".to_string())
        } else {
            None
        };
        let (status, body) = match &fail {
            Some(f) => (400, format!("<html><body>Login failed: {f}</body></html>")),
            None => (
                200,
                "<html><body>Login complete — you can close this tab.</body></html>".to_string(),
            ),
        };
        let _ = respond_html(req, status, &body);
        return match fail {
            Some(f) => Err(f),
            None => Ok(code.unwrap()),
        };
    }
}

fn respond_html(req: tiny_http::Request, status: u16, body: &str) -> std::io::Result<()> {
    let header = tiny_http::Header::from_bytes(
        &b"Content-Type"[..],
        &b"text/html; charset=utf-8"[..],
    )
    .unwrap();
    req.respond(
        tiny_http::Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(header),
    )
}

/// Add a new account WITHOUT disturbing the live login: run the browser
/// OAuth flow ourselves and write the tokens straight into a new slot. The
/// live ~/.claude/.credentials.json is never touched.
pub async fn add_via_login(
    on_url: Option<impl Fn(String) + Send + Sync + 'static>,
) -> LoginFlowResult {
    let verifier = b64url(&random_bytes(32));
    let state = b64url(&random_bytes(32));
    let challenge = b64url(&sha256(verifier.as_bytes()));

    let auth_query = build_query(&[
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", LOGIN_REDIRECT),
        ("scope", SCOPES),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
    ]);
    let return_to = format!("{AUTHORIZE_PATH}?{auth_query}");
    // Going straight to /oauth/authorize silently consents as the current
    // claude.ai browser session. Use the same /logout?returnTo= route as
    // claude.ai's switch-account link so a fresh login page is guaranteed.
    let outer_query = build_query(&[("returnTo", &return_to)]);
    let url = format!("{SWITCH_ACCOUNT_URL}?{outer_query}");

    // Claim the port before opening the browser: Server::http binds
    // synchronously, so this happens before any await below.
    let server = match bind_callback_server(LOGIN_PORT) {
        Ok(s) => s,
        Err(e) => return login_err(e),
    };
    let state_for_loop = state.clone();
    let callback_join =
        tokio::task::spawn_blocking(move || run_callback_loop(server, state_for_loop));

    if let Some(cb) = &on_url {
        cb(url.clone());
    }
    let _ = open::that(&url);

    let code = match callback_join.await {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => return login_err(e),
        Err(e) => return login_err(e.to_string()),
    };

    let client = http_client();
    let res = match client
        .post(REFRESH_URL)
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "state": state,
            "client_id": CLIENT_ID,
            "redirect_uri": LOGIN_REDIRECT,
            "code_verifier": verifier,
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return login_err(format!("token exchange failed: {e}")),
    };
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let snippet: String = text.chars().take(200).collect();
        return login_err(format!("token exchange failed: HTTP {status} {snippet}"));
    }
    let body: Value = match res.json().await {
        Ok(b) => b,
        Err(e) => return login_err(format!("token exchange failed: {e}")),
    };
    let Some(access_token) = body.get("access_token").and_then(|v| v.as_str()) else {
        return login_err("token exchange returned no access_token".to_string());
    };

    let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(0);
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let scopes: Vec<String> = body
        .get("scope")
        .and_then(|v| v.as_str())
        .map(|s| s.split(' ').map(|x| x.to_string()).collect())
        .unwrap_or_else(|| SCOPES.split(' ').map(|x| x.to_string()).collect());
    let subscription_type = body
        .get("account")
        .and_then(|a| a.get("subscription_type"))
        .cloned()
        .unwrap_or(Value::Null);

    let oauth = serde_json::json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresAt": now_ms() + expires_in * 1000,
        "scopes": scopes,
        "subscriptionType": subscription_type,
    });

    // Identify the account for the slot label/identity. The token response
    // usually carries an `account`; the profile endpoint fills in the rest.
    let mut email = body
        .get("account")
        .and_then(|a| a.get("email_address").or_else(|| a.get("email")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut uuid = body
        .get("account")
        .and_then(|a| a.get("uuid"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut org_uuid: Option<String> = None;
    let mut org_name: Option<String> = None;

    if let Ok(pr) = client
        .get(PROFILE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", "claude-code/2.1.69")
        .send()
        .await
    {
        // profile is best-effort — the slot still works without it
        if pr.status().is_success() {
            if let Ok(p) = pr.json::<Value>().await {
                if email.is_none() {
                    email = p
                        .get("account")
                        .and_then(|a| a.get("email_address").or_else(|| a.get("email")))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                if uuid.is_none() {
                    uuid = p
                        .get("account")
                        .and_then(|a| a.get("uuid"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                org_uuid = p
                    .get("organization")
                    .and_then(|o| o.get("uuid"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                org_name = p
                    .get("organization")
                    .and_then(|o| o.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
        }
    }

    // Re-login of an already-enrolled account refreshes that slot in place.
    let existing = uuid
        .as_ref()
        .and_then(|u| list_slots().into_iter().find(|a| a.account_id.as_deref() == Some(u.as_str())));
    let slot = existing.map(|a| a.name).unwrap_or_else(|| derive_slot_name(email.as_deref()));
    let old_meta = read_json(&slot_meta_file(&slot)).unwrap_or(Value::Null);

    if let Err(e) = fs::create_dir_all(slot_dir(&slot)) {
        return login_err(e.to_string());
    }
    if let Err(e) = write_json_atomic(&slot_cred_file(&slot), &serde_json::json!({ "claudeAiOauth": oauth }), true) {
        return login_err(e.to_string());
    }

    let mut meta = match &old_meta {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    let mut oauth_account = match meta.get("oauthAccount") {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    let uuid_val = uuid.clone().or_else(|| {
        oauth_account.get("accountUuid").and_then(|v| v.as_str()).map(|s| s.to_string())
    });
    oauth_account.insert(
        "accountUuid".to_string(),
        uuid_val.map(Value::String).unwrap_or(Value::Null),
    );
    let email_val = email.clone().or_else(|| {
        oauth_account.get("emailAddress").and_then(|v| v.as_str()).map(|s| s.to_string())
    });
    oauth_account.insert(
        "emailAddress".to_string(),
        email_val.map(Value::String).unwrap_or(Value::Null),
    );
    if let Some(ou) = &org_uuid {
        oauth_account.insert("organizationUuid".to_string(), Value::String(ou.clone()));
    }
    if let Some(on) = &org_name {
        oauth_account.insert("organizationName".to_string(), Value::String(on.clone()));
    }
    meta.insert("oauthAccount".to_string(), Value::Object(oauth_account));

    if let Err(e) = write_json_atomic(&slot_meta_file(&slot), &Value::Object(meta), false) {
        return login_err(e.to_string());
    }

    LoginFlowResult {
        ok: true,
        name: Some(slot),
        email,
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Provider surface
// ---------------------------------------------------------------------------

pub fn list_accounts() -> Vec<PAccount> {
    list_slots()
}

pub fn active_account_name() -> Option<String> {
    active_name()
}

pub fn has_live_auth() -> bool {
    read_json(&live_cred_file())
        .and_then(|c| c.get("claudeAiOauth").and_then(|o| o.get("accessToken")).and_then(|v| v.as_str()).map(|s| s.to_string()))
        .is_some()
}

pub fn import_current(name: Option<&str>) -> Result<PAccount, String> {
    const NO_LIVE_LOGIN: &str = "No live Claude login (~/.claude/.credentials.json)";
    let cred = read_json(&live_cred_file()).ok_or_else(|| NO_LIVE_LOGIN.to_string())?;
    let has_access = cred
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|v| v.as_str())
        .is_some();
    if !has_access {
        return Err(NO_LIVE_LOGIN.to_string());
    }
    let oauth_account = live_oauth_account();
    let slot = name
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            derive_slot_name(
                oauth_account
                    .as_ref()
                    .and_then(|o| o.get("emailAddress"))
                    .and_then(|v| v.as_str()),
            )
        });
    let old_meta = read_json(&slot_meta_file(&slot)).unwrap_or(Value::Object(Default::default()));
    fs::create_dir_all(slot_dir(&slot)).map_err(|e| e.to_string())?;
    write_json_atomic(&slot_cred_file(&slot), &cred, true).map_err(|e| e.to_string())?;

    let mut meta_obj = match &old_meta {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    meta_obj.insert(
        "oauthAccount".to_string(),
        oauth_account.clone().unwrap_or(Value::Null),
    );
    write_json_atomic(&slot_meta_file(&slot), &Value::Object(meta_obj), false).map_err(|e| e.to_string())?;

    Ok(PAccount {
        email: oauth_account
            .as_ref()
            .and_then(|o| o.get("emailAddress"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        account_id: oauth_account
            .as_ref()
            .and_then(|o| o.get("accountUuid"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        label: label_for(Some(&serde_json::json!({ "oauthAccount": oauth_account })), Some(&cred)),
        enabled: !matches!(old_meta.get("enabled"), Some(Value::Bool(false))),
        name: slot,
    })
}

pub fn remove_account(name: &str) {
    let _ = fs::remove_dir_all(slot_dir(name));
}

pub fn rename_account(old_name: &str, new_name: &str) -> Result<(), String> {
    let clean: String = new_name
        .trim()
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    if clean.is_empty() {
        return Err("Invalid account name".to_string());
    }
    if slot_dir(&clean).exists() {
        return Err("Name already in use".to_string());
    }
    fs::rename(slot_dir(old_name), slot_dir(&clean)).map_err(|e| e.to_string())
}

pub fn set_account_enabled(name: &str, enabled: bool) -> Result<(), String> {
    if !list_slots().iter().any(|a| a.name == name) {
        return Err(format!("Account \"{name}\" is not enrolled"));
    }
    let mut meta = read_json(&slot_meta_file(name)).unwrap_or(Value::Object(Default::default()));
    match &mut meta {
        Value::Object(m) => {
            m.insert("enabled".to_string(), Value::Bool(enabled));
        }
        _ => meta = serde_json::json!({ "enabled": enabled }),
    }
    write_json_atomic(&slot_meta_file(name), &meta, false).map_err(|e| e.to_string())
}

pub fn sync_live_back_to_slot() {
    let Some(name) = active_name() else {
        return;
    };
    let Some(cred) = read_json(&live_cred_file()) else {
        return;
    };
    if cred.get("claudeAiOauth").is_none() {
        return;
    }
    let _ = write_json_atomic(&slot_cred_file(&name), &cred, true);
}

pub fn install_auth(name: &str) -> Result<(), String> {
    let cred = read_json(&slot_cred_file(name))
        .filter(|c| c.get("claudeAiOauth").is_some())
        .ok_or_else(|| format!("Account \"{name}\" has no credentials.json"))?;
    // The "@live" cache entry still holds the previous account's usage;
    // serving it for the new account would immediately re-trigger a switch.
    usage_cache().lock().unwrap().remove("@live");
    // 1. credentials file (what the CLI authenticates with)
    write_json_atomic(&live_cred_file(), &cred, true).map_err(|e| e.to_string())?;
    // 2. oauthAccount inside ~/.claude.json (account identity/metadata) —
    //    patch only that field, the file holds lots of unrelated state.
    if let Some(oauth_account) = read_json(&slot_meta_file(name)).and_then(|m| m.get("oauthAccount").cloned()) {
        if let Some(mut cj) = read_json(&claude_json_file()) {
            if let Value::Object(cj_obj) = &mut cj {
                cj_obj.insert("oauthAccount".to_string(), oauth_account);
            }
            write_json_atomic(&claude_json_file(), &cj, false).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_query_percent_encodes_like_urlsearchparams() {
        assert_eq!(build_query(&[("a", "b c"), ("d", "e&f")]), "a=b+c&d=e%26f");
    }

    #[test]
    fn b64url_matches_known_vector() {
        // "hello" -> base64url without padding
        assert_eq!(b64url(b"hello"), "aGVsbG8");
    }

    #[test]
    fn sha256_matches_known_vector() {
        // sha256("") is a well-known test vector.
        let digest = sha256(b"");
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn usage_window_reads_number_or_object_shape() {
        let w = usage_window(&serde_json::json!(42), 300).unwrap();
        assert_eq!(w.used_percent, 42.0);
        assert_eq!(w.window_minutes, Some(300));

        let w = usage_window(&serde_json::json!({ "utilization": 10 }), 300).unwrap();
        assert_eq!(w.used_percent, 10.0);

        let w = usage_window(&serde_json::json!({ "percent": 20 }), 300).unwrap();
        assert_eq!(w.used_percent, 20.0);

        assert_eq!(usage_window(&serde_json::json!({}), 300), None);
    }

    #[test]
    fn usage_limit_window_matches_kind_and_model() {
        let raw = serde_json::json!([
            { "kind": "weekly_scoped", "scope": { "model": { "display_name": "Fable" } }, "utilization": 5 },
            { "kind": "weekly_scoped", "scope": { "model": { "display_name": "Sonnet" } }, "utilization": 9 },
        ]);
        let w = usage_limit_window(Some(&raw), "weekly_scoped", Some("fable"), 10080).unwrap();
        assert_eq!(w.used_percent, 5.0);
        assert_eq!(usage_limit_window(Some(&raw), "weekly_all", None, 10080), None);
    }

    #[test]
    fn iso_to_ms_parses_rfc3339() {
        assert!(iso_to_ms(Some(&serde_json::json!("2024-01-01T00:00:00Z"))).is_some());
        assert_eq!(iso_to_ms(Some(&serde_json::json!("not-a-date"))), None);
        assert_eq!(iso_to_ms(None), None);
    }

    #[test]
    fn label_for_joins_email_and_plan() {
        let meta = serde_json::json!({ "oauthAccount": { "emailAddress": "a@b.com" } });
        let cred = serde_json::json!({ "claudeAiOauth": { "subscriptionType": "pro" } });
        assert_eq!(label_for(Some(&meta), Some(&cred)), Some("a@b.com · pro".to_string()));
    }
}
