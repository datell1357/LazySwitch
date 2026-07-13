//! Policy layer around detection and restart. It is Tauri-free so the complete
//! handover can be exercised with a fake process/terminal runtime.

use serde::{Deserialize, Serialize};

use super::cli_sessions::{default_restart_cli_sessions, restart_cli_sessions, CliRestartResult, CliResumeCommand, CliRuntime, CliSession};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliRestartAction { Restart, Copy, Later }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliRestartPayload {
    pub provider_name: String,
    pub resume_command: String,
    pub sessions: Vec<CliSession>,
}

pub fn provider_name(provider: &str) -> &'static str { if provider == "claude" { "Claude Code" } else { "Codex CLI" } }

pub fn handle_with_runtime(runtime: &dyn CliRuntime, provider: &str, sessions: &[CliSession], auto_restart: bool, action: CliRestartAction) -> Option<CliRestartResult> {
    if sessions.is_empty() { return None; }
    let resume = super::cli_sessions::resume_command_for(provider);
    if !auto_restart && action != CliRestartAction::Restart { return None; }
    Some(restart_cli_sessions(runtime, sessions, &resume))
}

pub fn default_handle(provider: &str, sessions: &[CliSession]) -> CliRestartResult {
    let resume = super::cli_sessions::resume_command_for(provider);
    default_restart_cli_sessions(sessions, &resume)
}

pub fn payload(provider: &str, sessions: &[CliSession]) -> CliRestartPayload {
    CliRestartPayload { provider_name: provider_name(provider).into(), resume_command: super::cli_sessions::resume_command_for(provider).text, sessions: sessions.to_vec() }
}

pub fn resume_command(provider: &str) -> CliResumeCommand { super::cli_sessions::resume_command_for(provider) }
