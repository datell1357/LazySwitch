//! Detection and process/terminal primitives for CLI session handover.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cli_cwd::{existing_cwd, normalize_windows_path, snapshot, ProcessRow, ProcessSnapshot};
use super::claude_sessions;
use super::codex_rollouts;

const TERMINALS: [&str; 11] = [
    "alacritty.exe", "bash.exe", "cmd.exe", "conhost.exe", "mintty.exe",
    "powershell.exe", "pwsh.exe", "terminal.exe", "wezterm-gui.exe",
    "windowsterminal.exe", "wt.exe",
];
const CLOSABLE_SHELLS: [&str; 4] = ["bash.exe", "cmd.exe", "powershell.exe", "pwsh.exe"];
const ORCA_DAEMON: &str = "orca-terminal-daemon.exe";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliTerminal { pub pid: u32, pub name: String, pub is_orca_hosted: bool }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliSession {
    pub provider_id: String,
    pub pid: u32,
    pub start_time: Option<String>,
    pub cwd: Option<String>,
    pub terminal: Option<CliTerminal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliResumeCommand { pub text: String, pub command: String, pub args: Vec<String> }

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliRestartResult { pub restarted: usize, pub closed: usize, pub manual: usize, pub failed: usize }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrcaTerminal { pub worktree_id: String, pub worktree_path: String }

pub trait CliRuntime: Send + Sync {
    fn snapshot(&self, provider: &str, root_pid: u32) -> Result<ProcessSnapshot, String>;
    fn is_alive(&self, pid: u32) -> bool;
    fn terminate(&self, pid: u32) -> bool;
    fn find_command(&self, command: &str) -> Option<String>;
    fn list_orca(&self, path: &str) -> Vec<OrcaTerminal>;
    fn create_orca(&self, path: &str, worktree_id: &str, command: &CliResumeCommand) -> bool;
    fn launch_windows_terminal(&self, cwd: &str, command: &CliResumeCommand) -> bool;
    fn launch_powershell(&self, cwd: &str, command: &CliResumeCommand) -> bool;
}

#[derive(Default)]
pub struct WindowsCliRuntime;

impl CliRuntime for WindowsCliRuntime {
    fn snapshot(&self, provider: &str, _root_pid: u32) -> Result<ProcessSnapshot, String> {
        snapshot(if provider == "claude" { "claude.exe" } else { "codex.exe" })
    }
    fn is_alive(&self, pid: u32) -> bool {
        #[cfg(windows)] {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
            if let Ok(handle) = handle {
                let mut code = 0u32;
                let alive = unsafe { GetExitCodeProcess(handle, &mut code).is_ok() && code == 259 };
                unsafe { let _ = CloseHandle(handle); }
                alive
            } else { false }
        }
        #[cfg(not(windows))] { let _ = pid; false }
    }
    fn terminate(&self, pid: u32) -> bool {
        #[cfg(windows)] {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE};
            let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
            let Ok(handle) = handle else { return false };
            let ok = unsafe { TerminateProcess(handle, 0).is_ok() };
            unsafe { let _ = CloseHandle(handle); }
            ok && !self.is_alive(pid)
        }
        #[cfg(not(windows))] { let _ = pid; false }
    }
    fn find_command(&self, command: &str) -> Option<String> {
        #[cfg(windows)] {
            let output = Command::new(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()).replace('/', "\\") + "\\System32\\where.exe")
                .arg(command).output().ok()?;
            String::from_utf8_lossy(&output.stdout).lines().find(|line| !line.trim().is_empty()).map(|line| line.trim().to_owned())
        }
        #[cfg(not(windows))] { let _ = command; None }
    }
    fn list_orca(&self, path: &str) -> Vec<OrcaTerminal> { run_orca(path, &["terminal", "list", "--json"]).ok().and_then(|output| parse_orca(&output)).unwrap_or_default() }
    fn create_orca(&self, path: &str, worktree_id: &str, command: &CliResumeCommand) -> bool {
        let command_line = std::iter::once(command.command.as_str()).chain(command.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
        let args = ["terminal", "create", "--worktree", &format!("id:{worktree_id}"), "--command", &command_line, "--json"];
        run_orca(path, &args).ok().and_then(|output| serde_json::from_str::<Value>(&output).ok()).and_then(|value| value.get("ok").and_then(Value::as_bool)).unwrap_or(false)
    }
    fn launch_windows_terminal(&self, cwd: &str, command: &CliResumeCommand) -> bool {
        #[cfg(windows)] { Command::new("wt.exe").args(["-d", cwd, &command.command]).args(&command.args).spawn().is_ok() }
        #[cfg(not(windows))] { let _ = (cwd, command); false }
    }
    fn launch_powershell(&self, cwd: &str, command: &CliResumeCommand) -> bool {
        #[cfg(windows)] {
            let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            let cmd = format!("{system_root}\\System32\\cmd.exe");
            let powershell = format!("{system_root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
            let script = "$cliArgs = @((ConvertFrom-Json $env:LAZYSWITCH_CLI_ARGS)); & $env:LAZYSWITCH_CLI_COMMAND @cliArgs";
            let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
            Command::new(cmd).args(["/d", "/c", "start", "", &powershell, "-NoExit", "-EncodedCommand", &encoded]).current_dir(cwd).env("LAZYSWITCH_CLI_COMMAND", &command.command).env("LAZYSWITCH_CLI_ARGS", serde_json::to_string(&command.args).unwrap_or_default()).spawn().is_ok()
        }
        #[cfg(not(windows))] { let _ = (cwd, command); false }
    }
}

fn run_orca(path: &str, args: &[&str]) -> Result<String, String> {
    #[cfg(windows)] {
        let is_script = path.to_ascii_lowercase().ends_with(".cmd") || path.to_ascii_lowercase().ends_with(".bat");
        let output = if is_script { Command::new(format!("{}\\System32\\cmd.exe", std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()))).args(["/d", "/c", path]).args(args).output() } else { Command::new(path).args(args).output() };
        output.map_err(|error| error.to_string()).and_then(|output| if output.status.success() { Ok(String::from_utf8_lossy(&output.stdout).into_owned()) } else { Err(String::from_utf8_lossy(&output.stderr).into_owned()) })
    }
    #[cfg(not(windows))] { let _ = (path, args); Err("Orca is only available on Windows".into()) }
}

fn parse_orca(value: &str) -> Option<Vec<OrcaTerminal>> {
    let value: Value = serde_json::from_str(value).ok()?;
    let result = value.get("result").unwrap_or(&value);
    Some(result.get("terminals")?.as_array()?.iter().filter_map(|terminal| Some(OrcaTerminal { worktree_id: terminal.get("worktreeId")?.as_str()?.to_owned(), worktree_path: terminal.get("worktreePath")?.as_str()?.to_owned() })).collect())
}

fn process_map(snapshot: &ProcessSnapshot) -> HashMap<u32, ProcessRow> {
    snapshot.parents.iter().chain(snapshot.targets.iter()).cloned().map(|row| (row.pid, row)).collect()
}

fn descendant_of(pid: u32, rows: &HashMap<u32, ProcessRow>, root_pid: u32) -> bool {
    if root_pid == 0 { return false; }
    let mut current = pid;
    let mut seen = HashSet::new();
    while let Some(row) = rows.get(&current) {
        if row.parent_pid == root_pid { return true; }
        if row.parent_pid == 0 || !seen.insert(row.parent_pid) { return false; }
        current = row.parent_pid;
    }
    false
}

pub fn terminal_ancestor(pid: u32, snapshot: &ProcessSnapshot) -> Option<CliTerminal> {
    let rows = process_map(snapshot);
    let mut current = pid;
    let mut seen = HashSet::new();
    let mut shell = None;
    let mut orca = false;
    while let Some(row) = rows.get(&current) {
        if row.name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(ORCA_DAEMON)) { orca = true; }
        if shell.is_none() && row.name.as_deref().is_some_and(|name| TERMINALS.iter().any(|terminal| name.eq_ignore_ascii_case(terminal))) {
            shell = Some(CliTerminal { pid: row.pid, name: row.name.clone().unwrap_or_default(), is_orca_hosted: false });
        }
        if row.parent_pid == 0 || !seen.insert(row.parent_pid) { break; }
        current = row.parent_pid;
    }
    shell.map(|mut shell| { shell.is_orca_hosted = orca; shell })
}

fn is_codex_desktop_path(value: Option<&str>) -> bool { value.is_some_and(|value| normalize_windows_path(value).contains("\\program files\\windowsapps\\openai.codex_")) }

pub fn read_detector_output(snapshot: &ProcessSnapshot, provider: &str, root_pid: u32) -> Vec<CliSession> {
    let rows = process_map(snapshot);
    snapshot.targets.iter().filter(|row| {
        row.pid != root_pid && !descendant_of(row.pid, &rows, root_pid) && (provider != "codex" || (!is_codex_desktop_path(row.executable_path.as_deref()) && !is_codex_desktop_path(row.cwd.as_deref()) && terminal_ancestor(row.pid, snapshot).is_some()))
    }).map(|row| CliSession { provider_id: provider.to_owned(), pid: row.pid, start_time: row.start_time.clone(), cwd: row.cwd.as_deref().filter(|cwd| !cwd.is_empty()).map(super::cli_cwd::trim_cwd), terminal: terminal_ancestor(row.pid, snapshot) }).collect()
}

pub fn detect_cli_sessions(provider: &str, root_pid: u32) -> Vec<CliSession> {
    if !cfg!(windows) { return Vec::new(); }
    WindowsCliRuntime.snapshot(provider, root_pid).map(|snapshot| read_detector_output(&snapshot, provider, root_pid)).unwrap_or_default()
}

pub fn resume_command_for(provider: &str) -> CliResumeCommand {
    if provider == "claude" { CliResumeCommand { text: "claude --continue".into(), command: "claude".into(), args: vec!["--continue".into()] } } else { CliResumeCommand { text: "codex resume".into(), command: "codex".into(), args: vec!["resume".into()] } }
}

pub fn orca_worktree_id_for_cwd(terminals: &[OrcaTerminal], cwd: &str) -> Option<String> {
    let target = normalize_windows_path(cwd);
    terminals.iter().filter_map(|terminal| {
        let worktree = normalize_windows_path(&terminal.worktree_path);
        (target == worktree || target.starts_with(&(worktree.clone() + "\\"))).then_some((worktree.len(), terminal.worktree_id.clone()))
    }).max_by_key(|(length, _)| *length).map(|(_, id)| id)
}

fn record(result: &mut CliRestartResult, outcome: &str) { match outcome { "restarted" => result.restarted += 1, "manual" => result.manual += 1, "failed" => result.failed += 1, _ => {} } }

fn close_shell(runtime: &dyn CliRuntime, terminal: &CliTerminal) -> bool { CLOSABLE_SHELLS.iter().any(|name| terminal.name.eq_ignore_ascii_case(name)) && runtime.terminate(terminal.pid) }

pub fn restart_cli_sessions(runtime: &dyn CliRuntime, sessions: &[CliSession], resume: &CliResumeCommand) -> CliRestartResult {
    restart_cli_sessions_with_roots(runtime, sessions, resume, None, None)
}

/// Testable variant: the roots are injected so session matching never needs
/// to read a user's real transcript store.
pub fn restart_cli_sessions_with_roots(
    runtime: &dyn CliRuntime,
    sessions: &[CliSession],
    resume: &CliResumeCommand,
    codex_root: Option<&Path>,
    claude_root: Option<&Path>,
) -> CliRestartResult {
    let orca = if sessions.iter().any(|session| session.terminal.as_ref().is_some_and(|terminal| terminal.is_orca_hosted)) { runtime.find_command("orca").map(|path| (path.clone(), runtime.list_orca(&path))) } else { None };
    let mut claimed_codex = HashSet::new();
    let mut claimed_claude = HashSet::new();
    let mut result = CliRestartResult::default();
    for session in sessions {
        let mut cwd = session.cwd.clone();
        let mut command = resume.clone();
        if session.provider_id == "codex" {
            if let Some(match_) = codex_rollouts::find_for_process(session, codex_root, &claimed_codex) {
                claimed_codex.insert(match_.session_id.clone());
                command = CliResumeCommand { text: format!("codex resume {}", match_.session_id), command: "codex".into(), args: vec!["resume".into(), match_.session_id] };
                if cwd.is_none() { cwd = existing_cwd(&match_.cwd).or_else(|| super::user_home().to_str().map(str::to_owned)); }
            }
        } else if session.cwd.is_some() {
            if let Some(match_) = claude_sessions::find_for_process(session, claude_root, &claimed_claude) {
                claimed_claude.insert(match_.session_id.clone());
                if let Some(matched_cwd) = existing_cwd(&match_.cwd) { command = CliResumeCommand { text: format!("claude --resume {}", match_.session_id), command: "claude".into(), args: vec!["--resume".into(), match_.session_id] }; cwd = Some(matched_cwd); }
            }
        }
        if !runtime.terminate(session.pid) { record(&mut result, "manual"); continue; }
        if session.terminal.as_ref().is_some_and(|terminal| close_shell(runtime, terminal)) { result.closed += 1; }
        let Some(cwd) = cwd.or_else(|| (session.provider_id == "codex").then(|| super::user_home().to_string_lossy().into_owned())) else { record(&mut result, "manual"); continue; };
        let launched = if session.terminal.as_ref().is_some_and(|terminal| terminal.is_orca_hosted) {
            orca.as_ref().and_then(|(path, terminals)| orca_worktree_id_for_cwd(terminals, &cwd).map(|id| runtime.create_orca(path, &id, &command))).unwrap_or(false)
        } else { runtime.launch_windows_terminal(&cwd, &command) || runtime.launch_powershell(&cwd, &command) };
        record(&mut result, if launched { "restarted" } else { "failed" });
    }
    result
}

pub fn default_restart_cli_sessions(sessions: &[CliSession], resume: &CliResumeCommand) -> CliRestartResult { restart_cli_sessions(&WindowsCliRuntime, sessions, resume) }

pub fn default_cwd_exists(value: &str) -> Option<String> { existing_cwd(value) }

pub fn is_terminal_emulator(name: &str) -> bool { TERMINALS.iter().any(|terminal| name.eq_ignore_ascii_case(terminal)) }
pub fn is_descendant(snapshot: &ProcessSnapshot, pid: u32, root_pid: u32) -> bool { descendant_of(pid, &process_map(snapshot), root_pid) }
pub fn process_snapshot_for(provider: &str) -> Result<ProcessSnapshot, String> { WindowsCliRuntime.snapshot(provider, std::process::id()) }
pub fn path_is_desktop(value: &str) -> bool { is_codex_desktop_path(Some(value)) }
pub fn path_exists(path: &str) -> bool { Path::new(path).exists() }
