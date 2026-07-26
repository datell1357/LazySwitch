use std::path::PathBuf;

/// Codex home. Respects CODEX_HOME override, else ~/.codex.
/// This is the *live* store that both Codex CLI and Codex Desktop read.
pub fn codex_home() -> PathBuf {
    match std::env::var("CODEX_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => home_dir().join(".codex"),
    }
}

/// The single source of truth for the currently active account.
pub fn live_auth_file() -> PathBuf {
    codex_home().join("auth.json")
}

/// Where Codex writes session rollout files (used for usage monitoring).
pub fn sessions_dir() -> PathBuf {
    codex_home().join("sessions")
}

/// Root of the per-account isolated auth stores: ~/.codex-accounts/<name>/auth.json
pub fn accounts_root() -> PathBuf {
    home_dir().join(".codex-accounts")
}

pub fn account_dir(name: &str) -> PathBuf {
    accounts_root().join(name)
}

pub fn account_auth_file(name: &str) -> PathBuf {
    account_dir(name).join("auth.json")
}

fn home_dir() -> PathBuf {
    dirs::home_dir().expect("no home directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both cases share the CODEX_HOME env var, which is process-global and
    // would race under the test harness's default parallel execution if
    // split into separate #[test] fns — kept as one test to stay serial.
    #[test]
    fn codex_home_env_override_behavior() {
        std::env::set_var("CODEX_HOME", "C:\\custom\\codex");
        assert_eq!(codex_home(), PathBuf::from("C:\\custom\\codex"));

        std::env::set_var("CODEX_HOME", "");
        assert_eq!(codex_home(), home_dir().join(".codex"));

        std::env::remove_var("CODEX_HOME");
        assert_eq!(codex_home(), home_dir().join(".codex"));
    }

    #[test]
    fn account_paths_join_under_accounts_root() {
        assert_eq!(account_dir("alice"), accounts_root().join("alice"));
        assert_eq!(account_auth_file("alice"), accounts_root().join("alice").join("auth.json"));
    }
}
