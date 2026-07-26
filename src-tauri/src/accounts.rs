use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::{account_auth_file, account_dir, accounts_root, live_auth_file};

/// Shape of ~/.codex/auth.json (confirmed on codex-cli 0.142.5, auth_mode "chatgpt").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexAuthTokens {
    pub id_token: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexAuth {
    pub auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    pub tokens: Option<CodexAuthTokens>,
    pub last_refresh: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub name: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub auth_mode: Option<String>,
    /// Email/plan decoded from id_token if available.
    pub label: Option<String>,
    pub last_refresh: Option<String>,
    pub enabled: bool,
}

fn account_state_file(name: &str) -> PathBuf {
    account_dir(name).join(".lazyswitch.json")
}

/// `state["enabled"] !== false` semantics: anything other than an explicit
/// `false` (missing file, missing key, non-object JSON, non-bool value)
/// counts as enabled.
fn account_enabled_at(state_file: &Path) -> bool {
    let raw = match fs::read_to_string(state_file) {
        Ok(r) => r,
        Err(_) => return true,
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(map)) => !matches!(map.get("enabled"), Some(Value::Bool(false))),
        _ => true,
    }
}

fn account_enabled(name: &str) -> bool {
    account_enabled_at(&account_state_file(name))
}

pub fn read_auth(file: &Path) -> Option<CodexAuth> {
    let raw = fs::read_to_string(file).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Decode the JWT id_token payload without verifying (for display only).
fn decode_jwt(token: Option<&str>) -> Option<Value> {
    let token = token?;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn email_from_auth(auth: Option<&CodexAuth>) -> Option<String> {
    let payload = decode_jwt(
        auth.and_then(|a| a.tokens.as_ref())
            .and_then(|t| t.id_token.as_deref()),
    )?;
    payload
        .get("email")
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string())
}

fn label_from_auth(auth: Option<&CodexAuth>) -> Option<String> {
    let payload = decode_jwt(
        auth.and_then(|a| a.tokens.as_ref())
            .and_then(|t| t.id_token.as_deref()),
    )?;
    let email = payload
        .get("email")
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(|v| v.as_str())
        });
    let plan = payload
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str());
    let parts: Vec<&str> = [email, plan].into_iter().flatten().collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Replace path-forbidden characters (and whitespace) with `_` — used when
/// deriving a fresh slot name from an email local-part.
fn sanitize_derived_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if "\\/:*?\"<>|".contains(c) || c.is_whitespace() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Replace path-forbidden characters with `_`, preserving whitespace — used
/// when the user supplies an explicit new name (rename).
fn sanitize_explicit_name(s: &str) -> String {
    s.chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// Derive a friendly slot name from an email
/// ("hyunmin.kang27@gmail.com" -> "hyunmin.kang27"), deduped against
/// existing account directories.
pub fn derive_slot_name(email: Option<&str>) -> String {
    let local = email.unwrap_or("").split('@').next().unwrap_or("");
    let sanitized = sanitize_derived_name(local);
    let base = if sanitized.is_empty() {
        format!("account-{}", now_ms())
    } else {
        sanitized
    };
    let mut name = base.clone();
    let mut i = 2;
    while account_dir(&name).exists() {
        name = format!("{base}-{i}");
        i += 1;
    }
    name
}

fn to_account(name: String, auth: Option<CodexAuth>) -> Account {
    let enabled = account_enabled(&name);
    Account {
        email: email_from_auth(auth.as_ref()),
        account_id: auth
            .as_ref()
            .and_then(|a| a.tokens.as_ref())
            .and_then(|t| t.account_id.clone()),
        auth_mode: auth.as_ref().and_then(|a| a.auth_mode.clone()),
        label: label_from_auth(auth.as_ref()),
        last_refresh: auth.as_ref().and_then(|a| a.last_refresh.clone()),
        enabled,
        name,
    }
}

pub fn list_accounts() -> Vec<Account> {
    let root = accounts_root();
    let entries = match fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut accounts: Vec<Account> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let auth = read_auth(&account_auth_file(&name));
            to_account(name, auth)
        })
        .collect();
    accounts.sort_by(|a, b| a.name.cmp(&b.name));
    accounts
}

/// account_id currently installed in the live ~/.codex/auth.json.
pub fn active_account_id() -> Option<String> {
    read_auth(&live_auth_file())?.tokens?.account_id
}

/// Which stored account matches the live auth (by account_id).
pub fn active_account_name() -> Option<String> {
    let id = active_account_id()?;
    list_accounts()
        .into_iter()
        .find(|a| a.account_id.as_deref() == Some(id.as_str()))
        .map(|a| a.name)
}

/// Copy the current live auth.json into a named slot (first-time enrollment).
pub fn import_current_as(name: &str) -> Result<Account, String> {
    let auth = read_auth(&live_auth_file())
        .ok_or_else(|| "No live ~/.codex/auth.json to import".to_string())?;
    fs::create_dir_all(account_dir(name)).map_err(|e| e.to_string())?;
    fs::copy(live_auth_file(), account_auth_file(name)).map_err(|e| e.to_string())?;
    Ok(to_account(name.to_string(), Some(auth)))
}

pub fn remove_account(name: &str) {
    let _ = fs::remove_dir_all(account_dir(name));
}

pub fn rename_account(old_name: &str, new_name: &str) -> Result<(), String> {
    let clean = sanitize_explicit_name(new_name.trim());
    if clean.is_empty() {
        return Err("Invalid account name".to_string());
    }
    if account_dir(&clean).exists() {
        return Err("Name already in use".to_string());
    }
    fs::rename(account_dir(old_name), account_dir(&clean)).map_err(|e| e.to_string())
}

pub fn set_account_enabled(name: &str, enabled: bool) -> Result<(), String> {
    if !list_accounts().iter().any(|a| a.name == name) {
        return Err(format!("Account \"{name}\" is not enrolled"));
    }
    let body = serde_json::json!({ "enabled": enabled }).to_string();
    fs::write(account_state_file(name), body).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(payload: &Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let body = URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("{header}.{body}.sig")
    }

    fn auth_with_id_token(payload: &Value) -> CodexAuth {
        CodexAuth {
            tokens: Some(CodexAuthTokens {
                id_token: Some(make_jwt(payload)),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn email_from_auth_reads_top_level_email() {
        let auth = auth_with_id_token(&serde_json::json!({ "email": "a@b.com" }));
        assert_eq!(email_from_auth(Some(&auth)), Some("a@b.com".to_string()));
    }

    #[test]
    fn email_from_auth_falls_back_to_profile_field() {
        let auth = auth_with_id_token(&serde_json::json!({
            "https://api.openai.com/profile": { "email": "c@d.com" }
        }));
        assert_eq!(email_from_auth(Some(&auth)), Some("c@d.com".to_string()));
    }

    #[test]
    fn email_from_auth_none_without_token() {
        assert_eq!(email_from_auth(None), None);
        assert_eq!(email_from_auth(Some(&CodexAuth::default())), None);
    }

    #[test]
    fn label_from_auth_joins_email_and_plan() {
        let auth = auth_with_id_token(&serde_json::json!({
            "email": "a@b.com",
            "https://api.openai.com/auth": { "chatgpt_plan_type": "plus" }
        }));
        assert_eq!(label_from_auth(Some(&auth)), Some("a@b.com · plus".to_string()));
    }

    #[test]
    fn label_from_auth_email_only() {
        let auth = auth_with_id_token(&serde_json::json!({ "email": "a@b.com" }));
        assert_eq!(label_from_auth(Some(&auth)), Some("a@b.com".to_string()));
    }

    #[test]
    fn sanitize_derived_name_replaces_forbidden_and_whitespace() {
        assert_eq!(sanitize_derived_name("a b\\c/d:e"), "a_b_c_d_e");
    }

    #[test]
    fn sanitize_explicit_name_keeps_whitespace() {
        assert_eq!(sanitize_explicit_name("a b\\c"), "a b_c");
    }

    #[test]
    fn account_enabled_at_missing_file_is_true() {
        let path = std::env::temp_dir().join("lazyswitch-does-not-exist.json");
        assert!(account_enabled_at(&path));
    }

    #[test]
    fn account_enabled_at_explicit_false_is_false() {
        let path = std::env::temp_dir().join(format!(
            "lazyswitch-enabled-test-{}.json",
            std::process::id()
        ));
        fs::write(&path, r#"{"enabled":false}"#).unwrap();
        assert!(!account_enabled_at(&path));
        fs::write(&path, r#"{"enabled":true}"#).unwrap();
        assert!(account_enabled_at(&path));
        fs::write(&path, r#"{}"#).unwrap();
        assert!(account_enabled_at(&path));
        fs::write(&path, r#"not json"#).unwrap();
        assert!(account_enabled_at(&path));
        let _ = fs::remove_file(&path);
    }
}
