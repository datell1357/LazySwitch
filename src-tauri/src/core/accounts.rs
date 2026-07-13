use std::path::{Path, PathBuf};

use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::paths::CodexPaths;
use super::{read_json, CoreError};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodexTokens {
    pub id_token: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodexAuth {
    pub auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    pub tokens: Option<CodexTokens>,
    pub last_refresh: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub name: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub auth_mode: Option<String>,
    pub label: Option<String>,
    pub last_refresh: Option<String>,
    pub enabled: bool,
}

fn auth_state_file(paths: &CodexPaths, name: &str) -> PathBuf {
    paths.account_dir(name).join(".lazyswitch.json")
}

fn account_enabled(paths: &CodexPaths, name: &str) -> bool {
    read_json(&auth_state_file(paths, name))
        .and_then(|value| value.as_object().cloned())
        .and_then(|state| state.get("enabled").cloned())
        .and_then(|value| value.as_bool())
        != Some(false)
}

pub fn read_auth(path: &Path) -> Option<CodexAuth> {
    serde_json::from_value(read_json(path)?).ok()
}

fn decode_jwt(token: Option<&str>) -> Option<Value> {
    let payload = token?.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn email_from_auth(auth: Option<&CodexAuth>) -> Option<String> {
    let payload = decode_jwt(auth?.tokens.as_ref()?.id_token.as_deref())?;
    payload
        .get("email")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            payload
                .get("https://api.openai.com/profile")?
                .get("email")
                .and_then(Value::as_str)
                .map(String::from)
        })
}

pub fn derive_slot_name(paths: &CodexPaths, email: Option<&str>) -> String {
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
        format!("account-{}", chrono::Utc::now().timestamp_millis())
    } else {
        base
    };
    let mut name = base.clone();
    let mut i = 2;
    while paths.account_dir(&name).exists() {
        name = format!("{base}-{i}");
        i += 1;
    }
    name
}

fn label_from_auth(auth: Option<&CodexAuth>) -> Option<String> {
    let payload = decode_jwt(auth?.tokens.as_ref()?.id_token.as_deref())?;
    let email = payload.get("email").and_then(Value::as_str).or_else(|| {
        payload
            .get("https://api.openai.com/profile")?
            .get("email")
            .and_then(Value::as_str)
    });
    let plan = payload
        .get("https://api.openai.com/auth")
        .and_then(|value| value.get("chatgpt_plan_type"))
        .and_then(Value::as_str);
    match (email, plan) {
        (Some(email), Some(plan)) => Some(format!("{email} · {plan}")),
        (Some(email), None) => Some(email.into()),
        (None, Some(plan)) => Some(plan.into()),
        (None, None) => None,
    }
}

fn to_account(paths: &CodexPaths, name: &str, auth: Option<&CodexAuth>) -> Account {
    Account {
        name: name.into(),
        email: email_from_auth(auth),
        account_id: auth
            .and_then(|a| a.tokens.as_ref())
            .and_then(|t| t.account_id.clone()),
        auth_mode: auth.and_then(|a| a.auth_mode.clone()),
        label: label_from_auth(auth),
        last_refresh: auth.and_then(|a| a.last_refresh.clone()),
        enabled: account_enabled(paths, name),
    }
}

#[derive(Clone, Debug)]
pub struct CodexAccounts {
    pub paths: CodexPaths,
}

impl Default for CodexAccounts {
    fn default() -> Self {
        Self {
            paths: CodexPaths::from_env(),
        }
    }
}

impl CodexAccounts {
    pub fn new(paths: CodexPaths) -> Self {
        Self { paths }
    }

    pub fn list_accounts(&self) -> Vec<Account> {
        let Ok(entries) = std::fs::read_dir(&self.paths.accounts_root) else {
            return Vec::new();
        };
        let mut accounts = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                if !kind.is_dir() {
                    return None;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let auth = read_auth(&self.paths.account_auth_file(&name));
                Some(to_account(&self.paths, &name, auth.as_ref()))
            })
            .collect::<Vec<_>>();
        accounts.sort_by(|a, b| a.name.cmp(&b.name));
        accounts
    }

    pub fn active_account_id(&self) -> Option<String> {
        read_auth(&self.paths.live_auth_file())
            .and_then(|a| a.tokens)
            .and_then(|t| t.account_id)
            .filter(|id| !id.is_empty())
    }

    pub fn active_account_name(&self) -> Option<String> {
        let id = self.active_account_id()?;
        self.list_accounts()
            .into_iter()
            .find(|a| a.account_id.as_deref() == Some(&id))
            .map(|a| a.name)
    }

    pub fn import_current_as(&self, name: &str) -> Result<Account, CoreError> {
        let auth = read_auth(&self.paths.live_auth_file())
            .ok_or_else(|| CoreError::Invalid("No live ~/.codex/auth.json to import".into()))?;
        std::fs::create_dir_all(self.paths.account_dir(name))?;
        std::fs::copy(
            self.paths.live_auth_file(),
            self.paths.account_auth_file(name),
        )?;
        Ok(to_account(&self.paths, name, Some(&auth)))
    }

    pub fn remove_account(&self, name: &str) -> Result<(), CoreError> {
        let directory = self.paths.account_dir(name);
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

    pub fn rename_account(&self, old_name: &str, new_name: &str) -> Result<(), CoreError> {
        let clean = sanitize_name(new_name);
        if clean.is_empty() {
            return Err(CoreError::Invalid("Invalid account name".into()));
        }
        if self.paths.account_dir(&clean).exists() {
            return Err(CoreError::Invalid("Name already in use".into()));
        }
        std::fs::rename(
            self.paths.account_dir(old_name),
            self.paths.account_dir(&clean),
        )?;
        Ok(())
    }

    pub fn set_account_enabled(&self, name: &str, enabled: bool) -> Result<(), CoreError> {
        if !self
            .list_accounts()
            .iter()
            .any(|account| account.name == name)
        {
            return Err(CoreError::Invalid(format!(
                "Account \"{name}\" is not enrolled"
            )));
        }
        std::fs::write(
            auth_state_file(&self.paths, name),
            serde_json::to_vec(&serde_json::json!({"enabled": enabled}))?,
        )?;
        Ok(())
    }
}

pub fn sanitize_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|ch| if "\\/:*?\"<>|".contains(ch) { '_' } else { ch })
        .collect()
}
