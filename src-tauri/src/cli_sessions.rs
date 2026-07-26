use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

use crate::claude_sessions::{find_claude_session_for_process, ClaudeProcessSession};
use crate::cli_cwd_script::PEB_CWD_SCRIPT;
use crate::cli_resume_routing::{
    record_cli_restart_outcome, CliRestartCounters, CliRestartOutcome,
};
use crate::codex_rollouts::{
    find_codex_rollout_for_process, normalize_cwd as normalize_cwd_lower, CodexProcessSession,
};
use crate::desktop_processes::normalize_windows_path;
use crate::powershell;
use crate::provider_types::ProviderId;

pub type CliRestartResult = CliRestartCounters;

#[derive(Debug, Clone, PartialEq)]
pub struct CliTerminal {
    pub pid: i64,
    pub name: String,
    pub is_orca_hosted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliSession {
    pub provider_id: ProviderId,
    pub pid: i64,
    pub start_time: Option<String>,
    pub cwd: Option<String>,
    pub terminal: Option<CliTerminal>,
}

#[derive(Debug, Clone)]
struct OrcaTerminal {
    worktree_id: String,
    worktree_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliResumeCommand {
    pub text: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
struct DetectorProcessRow {
    pid: i64,
    parent_pid: i64,
    name: Option<String>,
    executable_path: Option<String>,
    start_time: Option<String>,
    cwd: Option<String>,
}

const TERMINAL_PROCESS_NAMES: &[&str] = &[
    "alacritty.exe",
    "bash.exe",
    "cmd.exe",
    "conhost.exe",
    "mintty.exe",
    "powershell.exe",
    "pwsh.exe",
    "terminal.exe",
    "wezterm-gui.exe",
    "windowsterminal.exe",
    "wt.exe",
];

/// Only these hosts are safe to close: they run exactly one CLI session. The
/// emulators in TERMINAL_PROCESS_NAMES (wt.exe, WindowsTerminal.exe, …) can
/// own unrelated tabs, so closing one would take the user's other work with it.
const CLOSABLE_SHELL_NAMES: &[&str] = &["bash.exe", "cmd.exe", "powershell.exe", "pwsh.exe"];

const ORCA_TERMINAL_DAEMON: &str = "orca-terminal-daemon.exe";

const POWERSHELL_RESUME_SCRIPT: &str =
    "$cliArgs = @((ConvertFrom-Json $env:LAZYSWITCH_CLI_ARGS)); \
& $env:LAZYSWITCH_CLI_COMMAND @cliArgs";

fn process_name_for(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Codex => "codex.exe",
        ProviderId::Claude => "claude.exe",
    }
}

fn system_root() -> PathBuf {
    std::env::var("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
}
fn system32_root() -> PathBuf {
    system_root().join("System32")
}
fn cmd_exe() -> PathBuf {
    system32_root().join("cmd.exe")
}
fn powershell_exe() -> PathBuf {
    system32_root()
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}
fn taskkill_exe() -> PathBuf {
    system32_root().join("taskkill.exe")
}
fn where_exe() -> PathBuf {
    system32_root().join("where.exe")
}

pub fn resume_command_for(id: ProviderId) -> CliResumeCommand {
    match id {
        ProviderId::Codex => CliResumeCommand {
            text: "codex resume".to_string(),
            command: "codex".to_string(),
            args: vec!["resume".to_string()],
        },
        ProviderId::Claude => CliResumeCommand {
            text: "claude --continue".to_string(),
            command: "claude".to_string(),
            args: vec!["--continue".to_string()],
        },
    }
}

/// Drop trailing separators (PEB cwd ends with "\") but keep drive roots. A
/// trailing backslash before a closing quote breaks cmd/wt argument parsing
/// when the path is passed to the relaunch command line.
fn trim_cwd(value: &str) -> String {
    let trimmed = value.trim_end_matches(['\\', '/']);
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        format!("{trimmed}\\")
    } else {
        trimmed.to_string()
    }
}

fn read_detector_process(value: &Value) -> Option<DetectorProcessRow> {
    let obj = value.as_object()?;
    let pid = obj.get("pid")?.as_i64()?;
    Some(DetectorProcessRow {
        pid,
        parent_pid: obj.get("parentPid").and_then(|v| v.as_i64()).unwrap_or(0),
        name: obj
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        executable_path: obj
            .get("executablePath")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        start_time: obj
            .get("startTime")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        cwd: obj
            .get("cwd")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(trim_cwd),
    })
}

fn read_detector_rows(value: &Value) -> Vec<DetectorProcessRow> {
    match value {
        Value::Array(a) => a.iter().filter_map(read_detector_process).collect(),
        other => read_detector_process(other).into_iter().collect(),
    }
}

fn is_codex_desktop_path(value: Option<&str>) -> bool {
    match value {
        Some(v) => normalize_windows_path(v).contains(r"\program files\windowsapps\openai.codex_"),
        None => false,
    }
}

fn is_descendant_of_root(
    pid: i64,
    rows: &HashMap<i64, &DetectorProcessRow>,
    root_pid: i64,
) -> bool {
    if root_pid <= 0 {
        return false;
    }
    let mut seen = HashSet::new();
    let mut current = pid;
    while let Some(row) = rows.get(&current) {
        let parent_pid = row.parent_pid;
        if parent_pid == root_pid {
            return true;
        }
        if parent_pid <= 0 || seen.contains(&parent_pid) {
            return false;
        }
        seen.insert(parent_pid);
        current = parent_pid;
    }
    false
}

fn terminal_ancestor(pid: i64, rows: &HashMap<i64, &DetectorProcessRow>) -> Option<CliTerminal> {
    let mut seen = HashSet::new();
    let mut current = pid;
    let mut shell: Option<(i64, String)> = None;
    let mut is_orca_hosted = false;
    // Keep walking past the shell: the Orca daemon sits above it, and only
    // that tells us the session lives in an Orca tab rather than a desktop
    // console.
    while let Some(row) = rows.get(&current) {
        let name_lower = row.name.as_deref().map(|n| n.to_lowercase());
        if name_lower.as_deref() == Some(ORCA_TERMINAL_DAEMON) {
            is_orca_hosted = true;
        }
        if shell.is_none() {
            if let Some(n) = &name_lower {
                if TERMINAL_PROCESS_NAMES.contains(&n.as_str()) {
                    shell = Some((row.pid, row.name.clone().unwrap_or_default()));
                }
            }
        }
        let parent_pid = row.parent_pid;
        if parent_pid <= 0 || seen.contains(&parent_pid) {
            break;
        }
        seen.insert(parent_pid);
        current = parent_pid;
    }
    shell.map(|(pid, name)| CliTerminal {
        pid,
        name,
        is_orca_hosted,
    })
}

fn is_cli_candidate(
    row: &DetectorProcessRow,
    rows: &HashMap<i64, &DetectorProcessRow>,
    provider_id: ProviderId,
    root_pid: i64,
) -> bool {
    if row.pid == root_pid || is_descendant_of_root(row.pid, rows, root_pid) {
        return false;
    }
    if provider_id != ProviderId::Codex {
        return true;
    }
    if is_codex_desktop_path(row.executable_path.as_deref())
        || is_codex_desktop_path(row.cwd.as_deref())
    {
        return false;
    }
    // Elevated (admin-terminal) sessions hide executablePath and cwd from an
    // unelevated scan — keep them as long as a terminal ancestor is visible;
    // the restart path degrades to copying the resume command for them.
    terminal_ancestor(row.pid, rows).is_some()
}

fn read_session(
    row: &DetectorProcessRow,
    provider_id: ProviderId,
    rows: &HashMap<i64, &DetectorProcessRow>,
) -> CliSession {
    CliSession {
        provider_id,
        pid: row.pid,
        start_time: row.start_time.clone(),
        cwd: row.cwd.clone(),
        terminal: terminal_ancestor(row.pid, rows),
    }
}

pub fn read_detector_output(
    value: &Value,
    provider_id: ProviderId,
    root_pid: i64,
) -> Vec<CliSession> {
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };
    let Some(targets_val) = obj.get("targets").filter(|v| v.is_array()) else {
        return if provider_id == ProviderId::Codex {
            Vec::new()
        } else {
            read_detector_rows(value)
                .iter()
                .map(|row| {
                    let mut single = HashMap::new();
                    single.insert(row.pid, row);
                    read_session(row, provider_id, &single)
                })
                .collect()
        };
    };

    let targets = read_detector_rows(targets_val);
    let parents = obj
        .get("parents")
        .map(read_detector_rows)
        .unwrap_or_default();
    let mut rows: HashMap<i64, &DetectorProcessRow> = HashMap::new();
    for row in parents.iter().chain(targets.iter()) {
        rows.insert(row.pid, row);
    }
    targets
        .iter()
        .filter(|row| is_cli_candidate(row, &rows, provider_id, root_pid))
        .map(|row| read_session(row, provider_id, &rows))
        .collect()
}

pub async fn detect_cli_sessions(provider_id: ProviderId, root_pid: i64) -> Vec<CliSession> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let process_name = process_name_for(provider_id).replace('\'', "''");
    let script = PEB_CWD_SCRIPT
        .replace("__PROCESS_NAME__", &process_name)
        .replace("__ROOT_PID__", &root_pid.to_string());
    let exe = powershell_exe();
    let Ok(stdout) = powershell::run(&exe.to_string_lossy(), &script, 15_000).await else {
        return Vec::new();
    };
    let trimmed = stdout.trim();
    let parsed: Value = if trimmed.is_empty() {
        Value::Array(vec![])
    } else {
        match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        }
    };
    read_detector_output(&parsed, provider_id, root_pid)
}

fn is_process_alive(pid: i64) -> bool {
    #[cfg(windows)]
    {
        // OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) — a lightweight
        // liveness probe mirroring the original's `process.kill(pid, 0)`,
        // which on Windows (libuv) is itself backed by OpenProcess.
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32) {
                Ok(handle) => {
                    let _ = CloseHandle(handle);
                    true
                }
                Err(_) => false,
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

async fn taskkill(pid: i64, force: bool) {
    let pid_s = pid.to_string();
    let args: &[&str] = if force {
        &["/PID", &pid_s, "/T", "/F"]
    } else {
        &["/PID", &pid_s, "/T"]
    };
    let exe = taskkill_exe();
    let _ = powershell::exec_file_text(&exe.to_string_lossy(), args, 5_000).await;
}

/// Safety: restarting kills any in-flight turn; the resume command restores
/// the conversation transcript, not work that was mid-flight.
async fn terminate_process(pid: i64) -> bool {
    taskkill(pid, false).await;
    sleep(Duration::from_millis(3_000)).await;
    if !is_process_alive(pid) {
        return true;
    }
    taskkill(pid, true).await;
    sleep(Duration::from_millis(500)).await;
    !is_process_alive(pid)
}

async fn resolve_command_path(command: &str) -> Option<String> {
    let exe = where_exe();
    let stdout = powershell::exec_file_text(&exe.to_string_lossy(), &[command], 5_000)
        .await
        .ok()?;
    stdout
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

/// `orca` is a .cmd shim, and a plain Command::new cannot launch one without
/// shell interpretation, so it is routed through cmd.exe using the path
/// `where` resolved.
async fn run_orca(orca_path: &str, args: &[&str], timeout_ms: u64) -> Result<String, String> {
    if orca_path.to_lowercase().ends_with(".cmd") || orca_path.to_lowercase().ends_with(".bat") {
        let mut full_args: Vec<&str> = vec!["/d", "/c", orca_path];
        full_args.extend_from_slice(args);
        let exe = cmd_exe();
        powershell::exec_file_text(&exe.to_string_lossy(), &full_args, timeout_ms).await
    } else {
        powershell::exec_file_text(orca_path, args, timeout_ms).await
    }
}

fn read_orca_terminals(value: &Value) -> Vec<OrcaTerminal> {
    let root = value
        .get("result")
        .filter(|r| r.is_object())
        .unwrap_or(value);
    let Some(terminals) = root.get("terminals").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    terminals
        .iter()
        .filter_map(|t| {
            let worktree_id = t.get("worktreeId")?.as_str()?.to_string();
            let worktree_path = t.get("worktreePath")?.as_str()?.to_string();
            if worktree_id.is_empty() || worktree_path.is_empty() {
                return None;
            }
            Some(OrcaTerminal {
                worktree_id,
                worktree_path,
            })
        })
        .collect()
}

async fn list_orca_terminals(orca_path: &str) -> Vec<OrcaTerminal> {
    let Ok(output) = run_orca(orca_path, &["terminal", "list", "--json"], 10_000).await else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(&output) {
        Ok(v) => read_orca_terminals(&v),
        Err(_) => Vec::new(),
    }
}

/// Pick the worktree that owns the session's directory. Orca's `path:`
/// selector times out waiting for a terminal handle, so the worktree is
/// matched by path here and then addressed by the `id:` from the same listing.
fn orca_worktree_id_for_cwd(terminals: &[OrcaTerminal], cwd: &str) -> Option<String> {
    let target = normalize_cwd_lower(cwd);
    let mut best: Option<&OrcaTerminal> = None;
    for terminal in terminals {
        let worktree = normalize_cwd_lower(&terminal.worktree_path);
        if target != worktree && !target.starts_with(&format!("{worktree}\\")) {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => worktree.len() > normalize_cwd_lower(&b.worktree_path).len(),
        };
        if better {
            best = Some(terminal);
        }
    }
    best.map(|t| t.worktree_id.clone())
}

async fn create_orca_terminal(
    orca_path: &str,
    worktree_id: &str,
    resume: &CliResumeCommand,
) -> bool {
    let command_arg = std::iter::once(resume.command.as_str())
        .chain(resume.args.iter().map(|s| s.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let worktree_arg = format!("id:{worktree_id}");
    let Ok(output) = run_orca(
        orca_path,
        &[
            "terminal",
            "create",
            "--worktree",
            &worktree_arg,
            "--command",
            &command_arg,
            "--json",
        ],
        25_000,
    )
    .await
    else {
        return false;
    };
    serde_json::from_str::<Value>(&output)
        .ok()
        .and_then(|v| v.get("ok").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// Windows Terminal cannot be probed before use: it ships as an app execution
/// alias that `where` misses, and it is absent from the PATH of a packaged
/// app. So launch it and let the spawn itself be the test.
async fn spawn_detached(mut cmd: Command) -> bool {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Unlike POSIX, Windows has no zombie-reaping requirement, and
    // tokio::process::Child does not kill its process on drop by default —
    // so simply dropping the handle here is the exact equivalent of the
    // original's `child.unref()` (fire-and-forget).
    cmd.spawn().is_ok()
}

async fn launch_with_windows_terminal(cwd: &str, resume: &CliResumeCommand) -> bool {
    let mut cmd = Command::new("wt.exe");
    cmd.arg("-d")
        .arg(cwd)
        .arg(&resume.command)
        .args(&resume.args);
    spawn_detached(cmd).await
}

async fn launch_with_powershell(cwd: &str, resume: &CliResumeCommand) -> bool {
    let mut cmd = Command::new(cmd_exe());
    cmd.args(["/d", "/c", "start", ""])
        .arg(powershell_exe())
        .args([
            "-NoExit",
            "-EncodedCommand",
            &powershell::encode(POWERSHELL_RESUME_SCRIPT),
        ])
        .current_dir(cwd)
        .env("LAZYSWITCH_CLI_COMMAND", &resume.command)
        .env(
            "LAZYSWITCH_CLI_ARGS",
            serde_json::to_string(&resume.args).unwrap(),
        );
    spawn_detached(cmd).await
}

fn codex_resume_command(session_id: &str) -> CliResumeCommand {
    CliResumeCommand {
        text: format!("codex resume {session_id}"),
        command: "codex".to_string(),
        args: vec!["resume".to_string(), session_id.to_string()],
    }
}

fn claude_resume_command(session_id: &str) -> CliResumeCommand {
    CliResumeCommand {
        text: format!("claude --resume {session_id}"),
        command: "claude".to_string(),
        args: vec!["--resume".to_string(), session_id.to_string()],
    }
}

async fn existing_raw_cwd(value: &str) -> Option<String> {
    let cwd = trim_cwd(
        value
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .unwrap_or_else(|| value.strip_prefix(r"\\?\").unwrap_or(value).to_string())
            .as_str(),
    );
    match tokio::fs::metadata(&cwd).await {
        Ok(m) if m.is_dir() => Some(cwd),
        _ => None,
    }
}

/// Close the shell the CLI was running in, so the user is not left with a
/// dead prompt beside the new terminal. Elevated shells cannot be killed
/// from an unelevated app; that is fine, the resume still opens in a fresh
/// terminal.
async fn close_host_terminal(terminal: &CliTerminal) -> bool {
    if !CLOSABLE_SHELL_NAMES.contains(&terminal.name.to_lowercase().as_str()) {
        return false;
    }
    terminate_process(terminal.pid).await
}

struct OrcaContext {
    orca_path: String,
    terminals: Vec<OrcaTerminal>,
}

/// Reopen the session in a fresh terminal. A session that lived in an Orca
/// tab gets another Orca tab and nothing else — it must never spill a
/// desktop console onto the user's machine, so a failure here is a failure,
/// not a reason to open a window. Everything else gets a Windows Terminal
/// tab, or a PowerShell window if that cannot launch.
async fn reopen_in_new_terminal(
    session: &CliSession,
    cwd: &str,
    resume: &CliResumeCommand,
    orca: Option<&OrcaContext>,
) -> bool {
    if session
        .terminal
        .as_ref()
        .map(|t| t.is_orca_hosted)
        .unwrap_or(false)
    {
        let Some(orca) = orca else { return false };
        let Some(worktree_id) = orca_worktree_id_for_cwd(&orca.terminals, cwd) else {
            return false;
        };
        return create_orca_terminal(&orca.orca_path, &worktree_id, resume).await;
    }
    if launch_with_windows_terminal(cwd, resume).await {
        return true;
    }
    launch_with_powershell(cwd, resume).await
}

async fn resolve_orca_context(sessions: &[CliSession]) -> Option<OrcaContext> {
    if !sessions.iter().any(|s| {
        s.terminal
            .as_ref()
            .map(|t| t.is_orca_hosted)
            .unwrap_or(false)
    }) {
        return None;
    }
    let orca_path = resolve_command_path("orca").await?;
    let terminals = list_orca_terminals(&orca_path).await;
    Some(OrcaContext {
        orca_path,
        terminals,
    })
}

pub async fn restart_cli_sessions(
    sessions: &[CliSession],
    resume: &CliResumeCommand,
) -> CliRestartResult {
    let orca = resolve_orca_context(sessions).await;
    let mut claimed_codex_session_ids: HashSet<String> = HashSet::new();
    let mut claimed_claude_session_ids: HashSet<String> = HashSet::new();
    let mut counters = CliRestartCounters::default();

    for session in sessions {
        let mut session_resume = resume.clone();
        let mut cwd = session.cwd.clone();

        if session.provider_id == ProviderId::Codex {
            let codex_session = CodexProcessSession {
                cwd: session.cwd.as_deref(),
                start_time: session.start_time.as_deref(),
            };
            if let Some(rollout) =
                find_codex_rollout_for_process(&codex_session, None, &claimed_codex_session_ids)
            {
                claimed_codex_session_ids.insert(rollout.session_id.clone());
                session_resume = codex_resume_command(&rollout.session_id);
                if cwd.is_none() {
                    cwd = Some(match existing_raw_cwd(&rollout.cwd).await {
                        Some(c) => c,
                        None => dirs::home_dir()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                    });
                }
            }
        } else if let Some(session_cwd) = session.cwd.clone() {
            // Without a cwd the transcript cannot be identified: matching on
            // start time alone reaches across every project and has resumed
            // an unrelated conversation. Leave those to the user rather than
            // reopening the wrong one.
            let claude_session = ClaudeProcessSession {
                cwd: Some(session_cwd.as_str()),
                start_time: session.start_time.as_deref(),
            };
            if let Some(m) =
                find_claude_session_for_process(&claude_session, None, &claimed_claude_session_ids)
            {
                claimed_claude_session_ids.insert(m.session_id.clone());
                if let Some(matched_cwd) = existing_raw_cwd(&m.cwd).await {
                    session_resume = claude_resume_command(&m.session_id);
                    cwd = Some(matched_cwd);
                }
            }
        }

        // Restarting kills any in-flight turn; the resume command restores
        // the conversation transcript, not work that was mid-flight.
        if !terminate_process(session.pid).await {
            // An elevated CLI cannot be killed by an unelevated app.
            // Reopening the transcript now would leave two live sessions on
            // the same conversation, so hand it back to the user, who gets
            // the resume command on the clipboard.
            counters = record_cli_restart_outcome(counters, CliRestartOutcome::Manual);
            continue;
        }
        if let Some(terminal) = &session.terminal {
            if close_host_terminal(terminal).await {
                counters.closed += 1;
            }
        }

        let cwd = match cwd {
            Some(c) => c,
            None if session.provider_id == ProviderId::Codex => {
                // the resume picker works from anywhere
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            }
            None => {
                // `claude --continue` only finds sessions of its working directory.
                counters = record_cli_restart_outcome(counters, CliRestartOutcome::Manual);
                continue;
            }
        };

        let outcome = if reopen_in_new_terminal(session, &cwd, &session_resume, orca.as_ref()).await
        {
            CliRestartOutcome::Restarted
        } else {
            CliRestartOutcome::Failed
        };
        counters = record_cli_restart_outcome(counters, outcome);
    }

    counters
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        pid: i64,
        parent_pid: i64,
        name: Option<&str>,
        exe: Option<&str>,
        cwd: Option<&str>,
    ) -> Value {
        serde_json::json!({
            "pid": pid,
            "parentPid": parent_pid,
            "name": name,
            "executablePath": exe,
            "startTime": null,
            "cwd": cwd,
        })
    }

    #[test]
    fn trim_cwd_keeps_drive_root_backslash() {
        assert_eq!(trim_cwd(r"C:\"), r"C:\");
        assert_eq!(trim_cwd(r"C:\foo\"), r"C:\foo");
        assert_eq!(trim_cwd(r"C:\foo//"), r"C:\foo");
    }

    #[test]
    fn read_detector_output_excludes_own_process_tree() {
        let snapshot = serde_json::json!({
            "targets": [row(200, 100, Some("codex.exe"), Some(r"C:\codex.exe"), Some(r"C:\proj"))],
            "parents": [row(100, 1, Some("app.exe"), None, None)],
        });
        // root_pid 100 is the ancestor of target 200 -> must be excluded.
        let sessions = read_detector_output(&snapshot, ProviderId::Codex, 100);
        assert!(sessions.is_empty());
    }

    #[test]
    fn read_detector_output_excludes_codex_desktop_path() {
        let snapshot = serde_json::json!({
            "targets": [row(
                200,
                1,
                Some("codex.exe"),
                Some(r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0\codex.exe"),
                Some(r"C:\proj"),
            )],
            "parents": [],
        });
        let sessions = read_detector_output(&snapshot, ProviderId::Codex, 999);
        assert!(sessions.is_empty());
    }

    #[test]
    fn read_detector_output_includes_codex_session_with_terminal_ancestor() {
        let snapshot = serde_json::json!({
            "targets": [row(200, 100, Some("codex.exe"), Some(r"C:\Users\me\codex.exe"), Some(r"C:\proj"))],
            "parents": [row(100, 1, Some("powershell.exe"), None, None)],
        });
        let sessions = read_detector_output(&snapshot, ProviderId::Codex, 999);
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].terminal.as_ref().unwrap().name,
            "powershell.exe"
        );
    }

    #[test]
    fn read_detector_output_marks_orca_hosted_terminal() {
        let snapshot = serde_json::json!({
            "targets": [row(300, 200, Some("claude.exe"), Some(r"C:\claude.exe"), Some(r"C:\proj"))],
            "parents": [
                row(200, 100, Some("cmd.exe"), None, None),
                row(100, 1, Some("orca-terminal-daemon.exe"), None, None),
            ],
        });
        let sessions = read_detector_output(&snapshot, ProviderId::Claude, 999);
        assert_eq!(sessions.len(), 1);
        let terminal = sessions[0].terminal.as_ref().unwrap();
        assert_eq!(terminal.name, "cmd.exe");
        assert!(terminal.is_orca_hosted);
    }

    #[test]
    fn orca_worktree_id_for_cwd_prefers_longest_matching_prefix() {
        let terminals = vec![
            OrcaTerminal {
                worktree_id: "outer".to_string(),
                worktree_path: r"C:\repo".to_string(),
            },
            OrcaTerminal {
                worktree_id: "inner".to_string(),
                worktree_path: r"C:\repo\sub".to_string(),
            },
        ];
        assert_eq!(
            orca_worktree_id_for_cwd(&terminals, r"C:\repo\sub\deeper"),
            Some("inner".to_string())
        );
        assert_eq!(
            orca_worktree_id_for_cwd(&terminals, r"C:\repo\other"),
            Some("outer".to_string())
        );
        assert_eq!(orca_worktree_id_for_cwd(&terminals, r"C:\unrelated"), None);
    }

    #[test]
    fn read_orca_terminals_parses_flat_or_result_wrapped_shape() {
        let flat =
            serde_json::json!({ "terminals": [{ "worktreeId": "a", "worktreePath": "C:\\x" }] });
        assert_eq!(read_orca_terminals(&flat).len(), 1);
        let wrapped = serde_json::json!({ "result": { "terminals": [{ "worktreeId": "a", "worktreePath": "C:\\x" }] } });
        assert_eq!(read_orca_terminals(&wrapped).len(), 1);
    }
}
