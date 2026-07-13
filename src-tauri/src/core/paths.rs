use std::path::PathBuf;

use super::user_home;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexPaths {
    pub home: PathBuf,
    pub accounts_root: PathBuf,
}

impl CodexPaths {
    pub fn from_env() -> Self {
        let home = std::env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join(".codex"));
        Self {
            home,
            accounts_root: user_home().join(".codex-accounts"),
        }
    }

    pub fn live_auth_file(&self) -> PathBuf {
        self.home.join("auth.json")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.home.join("sessions")
    }

    pub fn account_dir(&self, name: &str) -> PathBuf {
        self.accounts_root.join(name)
    }

    pub fn account_auth_file(&self, name: &str) -> PathBuf {
        self.account_dir(name).join("auth.json")
    }
}

pub fn codex_home() -> PathBuf {
    CodexPaths::from_env().home
}
pub fn live_auth_file() -> PathBuf {
    CodexPaths::from_env().live_auth_file()
}
pub fn sessions_dir() -> PathBuf {
    CodexPaths::from_env().sessions_dir()
}
pub fn accounts_root() -> PathBuf {
    CodexPaths::from_env().accounts_root
}
pub fn account_dir(name: &str) -> PathBuf {
    CodexPaths::from_env().account_dir(name)
}
pub fn account_auth_file(name: &str) -> PathBuf {
    CodexPaths::from_env().account_auth_file(name)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudePaths {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub accounts_root: PathBuf,
}

impl ClaudePaths {
    pub fn from_env() -> Self {
        let home = user_home();
        let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        Self {
            home: home.clone(),
            config_dir,
            accounts_root: home.join(".claude-accounts"),
        }
    }

    pub fn live_credentials_file(&self) -> PathBuf {
        self.config_dir.join(".credentials.json")
    }
    pub fn claude_json_file(&self) -> PathBuf {
        self.home.join(".claude.json")
    }
    pub fn slot_dir(&self, name: &str) -> PathBuf {
        self.accounts_root.join(name)
    }
    pub fn slot_credentials_file(&self, name: &str) -> PathBuf {
        self.slot_dir(name).join("credentials.json")
    }
    pub fn slot_meta_file(&self, name: &str) -> PathBuf {
        self.slot_dir(name).join("meta.json")
    }
}
