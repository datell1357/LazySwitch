pub mod accounts;
pub mod cli_cwd;
pub mod cli_handover;
pub mod cli_sessions;
pub mod claude_sessions;
pub mod config;
pub mod codex_rollouts;
pub mod desktop_processes;
pub mod i18n;
pub mod paths;
pub mod platform;
pub mod providers;
pub mod switcher;
pub mod types;

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("account not found: {0}")]
    NotFound(String),
    #[error("HTTP error: {0}")]
    Http(String),
}

pub(crate) fn atomic_write(path: &std::path::Path, contents: &[u8]) -> Result<(), CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::Invalid("path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_file_name(format!(
        ".{}.tmp-{}-{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::write(&tmp, contents)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(error) if cfg!(windows) && path.exists() => {
            // Windows' rename does not replace an existing file. The fallback
            // preserves the same temp-write protocol used by the Electron app.
            std::fs::remove_file(path)?;
            std::fs::rename(&tmp, path)
                .map_err(|_| error)
                .map_err(CoreError::from)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            Err(error.into())
        }
    }
}

pub(crate) fn read_json(path: &std::path::Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn user_home() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .or_else(|| {
                let drive = std::env::var_os("HOMEDRIVE")?;
                let path = std::env::var_os("HOMEPATH")?;
                Some(PathBuf::from(drive).join(path))
            })
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
