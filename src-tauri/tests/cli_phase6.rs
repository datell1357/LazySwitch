use std::sync::{Arc, Mutex};

use lazyswitch_lib::core::cli_cwd::{ProcessRow, ProcessSnapshot};
use lazyswitch_lib::core::cli_sessions::{
    restart_cli_sessions, terminal_ancestor, CliResumeCommand, CliRuntime, CliSession,
    CliTerminal, OrcaTerminal, restart_cli_sessions_with_roots,
};
use lazyswitch_lib::core::claude_sessions::find_for_process as find_claude;
use lazyswitch_lib::core::codex_rollouts::find_for_process as find_codex;
use lazyswitch_lib::core::desktop_processes::select_desktop_process_ids;
use lazyswitch_lib::core::cli_handover::{handle_with_runtime, CliRestartAction};
use tempfile::TempDir;

fn row(pid: u32, parent_pid: u32, name: &str, path: Option<&str>) -> ProcessRow {
    ProcessRow { pid, parent_pid, name: Some(name.into()), executable_path: path.map(str::to_owned), ..Default::default() }
}

fn session(provider: &str, pid: u32, cwd: Option<String>, terminal: Option<CliTerminal>) -> CliSession {
    CliSession { provider_id: provider.into(), pid, start_time: None, cwd, terminal }
}

#[derive(Clone, Default)]
struct FakeRuntime {
    calls: Arc<Mutex<Vec<String>>>,
    wt_ok: bool,
    powershell_ok: bool,
    orca_path: Option<String>,
    orca_terminals: Vec<OrcaTerminal>,
    orca_ok: bool,
    dead: bool,
    unkillable: Vec<u32>,
}

impl FakeRuntime {
    fn calls(&self) -> Vec<String> { self.calls.lock().unwrap().clone() }
    fn push(&self, value: String) { self.calls.lock().unwrap().push(value); }
}

impl CliRuntime for FakeRuntime {
    fn snapshot(&self, _provider: &str, _root_pid: u32) -> Result<ProcessSnapshot, String> { Ok(ProcessSnapshot::default()) }
    fn is_alive(&self, _pid: u32) -> bool { !self.dead }
    fn terminate(&self, pid: u32) -> bool { self.push(format!("kill:{pid}")); !self.unkillable.contains(&pid) && (self.dead || pid != 999) }
    fn find_command(&self, _command: &str) -> Option<String> { self.orca_path.clone() }
    fn list_orca(&self, _path: &str) -> Vec<OrcaTerminal> { self.orca_terminals.clone() }
    fn create_orca(&self, _path: &str, id: &str, command: &CliResumeCommand) -> bool { self.push(format!("orca:{id}:{}", command.text)); self.orca_ok }
    fn launch_windows_terminal(&self, cwd: &str, command: &CliResumeCommand) -> bool { self.push(format!("wt:{cwd}:{}", command.text)); self.wt_ok }
    fn launch_powershell(&self, cwd: &str, command: &CliResumeCommand) -> bool { self.push(format!("ps:{cwd}:{}", command.text)); self.powershell_ok }
}

fn codex_meta(id: &str, cwd: &str) -> String {
    serde_json::json!({"type":"session_meta","payload":{"session_id":id,"cwd":cwd}}).to_string() + "\n"
}

fn claude_meta(id: &str, cwd: &str) -> String {
    serde_json::json!({"sessionId":id,"cwd":cwd}).to_string() + "\n"
}

#[test]
fn detector_keeps_elevated_codex_and_trims_cwd() {
    let snapshot = ProcessSnapshot {
        targets: vec![ProcessRow { pid: 30676, parent_pid: 3164, name: Some("codex.exe".into()), executable_path: None, start_time: None, cwd: Some("D:\\work\\".into()) }],
        parents: vec![row(3164, 35876, "node.exe", None), row(35876, 2876, "powershell.exe", None), row(2876, 1, "explorer.exe", None)],
    };
    let found = lazyswitch_lib::core::cli_sessions::read_detector_output(&snapshot, "codex", 999);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].cwd.as_deref(), Some("D:\\work"));
}

#[test]
fn detector_excludes_store_desktop_and_own_descendants() {
    let snapshot = ProcessSnapshot {
        targets: vec![row(101, 1, "codex.exe", Some(r"C:\Program Files\WindowsApps\OpenAI.Codex_1\codex.exe")), row(102, 900, "codex.exe", Some(r"C:\tools\codex.exe")), row(103, 1, "codex.exe", Some(r"C:\tools\codex.exe"))],
        parents: vec![row(900, 1, "lazyswitch.exe", None), row(1, 0, "explorer.exe", None), row(103, 1, "codex.exe", Some(r"C:\tools\codex.exe"))],
    };
    let found = lazyswitch_lib::core::cli_sessions::read_detector_output(&snapshot, "codex", 900);
    assert!(found.is_empty());
}

#[test]
fn detector_finds_nearest_shell_and_orca_host() {
    let snapshot = ProcessSnapshot {
        targets: vec![row(100, 80, "codex.exe", Some(r"C:\tools\codex.exe"))],
        parents: vec![row(80, 60, "node.exe", None), row(60, 40, "pwsh.exe", None), row(40, 1, "orca-terminal-daemon.exe", None)],
    };
    assert_eq!(terminal_ancestor(100, &snapshot), Some(CliTerminal { pid: 60, name: "pwsh.exe".into(), is_orca_hosted: true }));
}

#[test]
fn claude_matching_honours_active_newest_claims_and_safe_ids() {
    let root = TempDir::new().unwrap();
    let project = root.path().join("D--Project");
    std::fs::create_dir_all(&project).unwrap();
    let older = project.join("older.jsonl");
    let newer = project.join("newer.jsonl");
    std::fs::write(&older, claude_meta("older", r"D:\Project")).unwrap();
    std::fs::write(&newer, claude_meta("newer", r"D:\Project")).unwrap();
    let found = find_claude(&session("claude", 1, Some(r"D:\Project".into()), None), Some(root.path()), &std::collections::HashSet::new()).unwrap();
    assert!(found.session_id == "older" || found.session_id == "newer");
    let claimed = [found.session_id.clone()].into_iter().collect();
    assert!(find_claude(&session("claude", 1, Some(r"D:\Project".into()), None), Some(root.path()), &claimed).is_some());
    let unsafe_file = project.join("unsafe.jsonl");
    std::fs::write(&unsafe_file, claude_meta("unsafe;calc", r"D:\Project")).unwrap();
    assert!(find_claude(&session("claude", 1, Some(r"D:\Project".into()), None), Some(root.path()), &std::collections::HashSet::new()).is_some());
}

#[test]
fn codex_matching_supports_nested_files_claims_and_unsafe_ids() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("2026").join("07").join("04");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("rollout-new.jsonl");
    std::fs::write(&path, codex_meta("new-session", r"D:\Project")).unwrap();
    let found = find_codex(&session("codex", 1, Some(r"D:\Project".into()), None), Some(root.path()), &std::collections::HashSet::new()).unwrap();
    assert_eq!(found.session_id, "new-session");
    let claimed = ["new-session".to_owned()].into_iter().collect();
    assert!(find_codex(&session("codex", 1, Some(r"D:\Project".into()), None), Some(root.path()), &claimed).is_none());
}

#[test]
fn desktop_filter_keeps_desktop_excludes_cli_and_own_children() {
    let snapshot = ProcessSnapshot {
        targets: vec![row(101, 1, "Codex.exe", Some(r"C:\Program Files\WindowsApps\OpenAI.Codex_1\Codex.exe")), row(102, 1, "Codex.exe", Some(r"C:\Users\me\AppData\Local\Programs\Codex\Codex.exe")), row(103, 900, "Codex.exe", None)],
        parents: vec![row(900, 1, "lazyswitch.exe", None), row(1, 0, "explorer.exe", None)],
    };
    assert_eq!(select_desktop_process_ids(&snapshot, Some(r"C:\Users\me\AppData\Local\Programs\Codex\Codex.exe"), 900), vec![101, 102]);
}

#[test]
fn terminal_close_and_resume_are_accounted_once() {
    let runtime = FakeRuntime { wt_ok: true, ..Default::default() };
    let cwd = TempDir::new().unwrap().path().to_string_lossy().into_owned();
    let result = restart_cli_sessions(&runtime, &[session("codex", 501, Some(cwd.clone()), Some(CliTerminal { pid: 51, name: "powershell.exe".into(), is_orca_hosted: false }))], &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"));
    assert_eq!(result.restarted, 1);
    assert_eq!(result.closed, 1);
    assert_eq!(result.manual + result.failed, 0);
    assert_eq!(runtime.calls(), vec!["kill:501".to_owned(), "kill:51".to_owned(), format!("wt:{cwd}:codex resume")]);
}

#[test]
fn emulator_is_not_closed_and_wt_falls_back_to_powershell() {
    let runtime = FakeRuntime { wt_ok: false, powershell_ok: true, ..Default::default() };
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let result = restart_cli_sessions(&runtime, &[session("codex", 501, Some(cwd.clone()), Some(CliTerminal { pid: 51, name: "WindowsTerminal.exe".into(), is_orca_hosted: false }))], &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"));
    assert_eq!(result, lazyswitch_lib::core::cli_sessions::CliRestartResult { restarted: 1, closed: 0, manual: 0, failed: 0 });
    assert_eq!(runtime.calls().iter().filter(|call| call.starts_with("kill")).count(), 1);
    assert!(runtime.calls().iter().any(|call| call.starts_with("ps:")));
}

#[test]
fn elevated_shell_survives_but_cli_still_reopens() {
    let runtime = FakeRuntime { wt_ok: true, unkillable: vec![51], ..Default::default() };
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let result = restart_cli_sessions(&runtime, &[session("codex", 501, Some(cwd), Some(CliTerminal { pid: 51, name: "powershell.exe".into(), is_orca_hosted: false }))], &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"));
    assert_eq!(result.restarted, 1);
    assert_eq!(result.closed, 0);
}

#[test]
fn no_terminal_ancestor_still_reopens_and_orca_success_uses_only_tab() {
    let runtime = FakeRuntime { wt_ok: true, ..Default::default() };
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let result = restart_cli_sessions(&runtime, &[session("codex", 501, Some(cwd.clone()), None)], &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"));
    assert_eq!(result.restarted, 1);
    let runtime = FakeRuntime { orca_path: Some("orca.cmd".into()), orca_terminals: vec![OrcaTerminal { worktree_id: "wt-1".into(), worktree_path: cwd.clone() }], orca_ok: true, ..Default::default() };
    let result = restart_cli_sessions(&runtime, &[session("codex", 502, Some(cwd), Some(CliTerminal { pid: 51, name: "powershell.exe".into(), is_orca_hosted: true }))], &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"));
    assert_eq!(result.restarted, 1);
    assert!(runtime.calls().iter().any(|call| call.starts_with("orca:")));
    assert!(!runtime.calls().iter().any(|call| call.starts_with("wt:") || call.starts_with("ps:")));
}

#[test]
fn orca_never_falls_back_to_desktop_and_missing_claude_cwd_is_manual() {
    let runtime = FakeRuntime { orca_path: Some("orca.cmd".into()), orca_terminals: vec![OrcaTerminal { worktree_id: "wt-1".into(), worktree_path: r"D:\Project".into() }], orca_ok: false, wt_ok: true, ..Default::default() };
    let result = restart_cli_sessions(&runtime, &[session("codex", 501, Some(r"D:\Project".into()), Some(CliTerminal { pid: 51, name: "powershell.exe".into(), is_orca_hosted: true }))], &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"));
    assert_eq!(result.failed, 1);
    assert!(!runtime.calls().iter().any(|call| call.starts_with("wt:") || call.starts_with("ps:")));
    let runtime = FakeRuntime { wt_ok: true, ..Default::default() };
    let result = restart_cli_sessions(&runtime, &[session("claude", 601, None, Some(CliTerminal { pid: 51, name: "cmd.exe".into(), is_orca_hosted: false }))], &lazyswitch_lib::core::cli_sessions::resume_command_for("claude"));
    assert_eq!(result.manual, 1);
}

#[test]
fn elevated_cli_failure_is_manual_and_codex_without_cwd_uses_home() {
    let runtime = FakeRuntime { dead: false, wt_ok: true, ..Default::default() };
    let result = restart_cli_sessions(&runtime, &[session("codex", 999, None, None)], &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"));
    assert_eq!(result.manual, 1);
}

#[test]
fn resolved_claude_and_codex_sessions_reopen_with_specific_resume_ids() {
    let root = TempDir::new().unwrap();
    let claude_dir = root.path().join("project");
    let codex_dir = root.path().join("2026").join("07");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::create_dir_all(&codex_dir).unwrap();
    let claude_cwd = root.path().to_string_lossy().into_owned();
    std::fs::write(claude_dir.join("abc-123.jsonl"), claude_meta("abc-123", &claude_cwd)).unwrap();
    std::fs::write(codex_dir.join("rollout-2026-07-10T09-58-12-00000000-0000-0000-0000-000000000001.jsonl"), codex_meta("codex-123", &claude_cwd)).unwrap();
    let runtime = FakeRuntime { wt_ok: true, ..Default::default() };
    let sessions = [session("claude", 601, Some(claude_cwd.clone()), None), session("codex", 602, Some(claude_cwd.clone()), None)];
    let result = restart_cli_sessions_with_roots(&runtime, &sessions, &lazyswitch_lib::core::cli_sessions::resume_command_for("codex"), Some(root.path()), Some(root.path()));
    assert_eq!(result.restarted, 2);
    let calls = runtime.calls();
    assert!(calls.iter().any(|call| call.contains("claude --resume abc-123")));
    assert!(calls.iter().any(|call| call.contains("codex resume codex-123")));
}

#[test]
fn handover_uses_the_captured_snapshot_and_manual_policy() {
    let runtime = FakeRuntime { wt_ok: true, ..Default::default() };
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let captured = vec![session("codex", 701, Some(cwd), None)];
    let result = handle_with_runtime(&runtime, "codex", &captured, true, CliRestartAction::Restart).unwrap();
    assert_eq!(result.restarted, 1);
    assert_eq!(runtime.calls().iter().filter(|call| call.starts_with("kill:")).count(), 1);
    let later = handle_with_runtime(&runtime, "codex", &captured, false, CliRestartAction::Later);
    assert!(later.is_none());
    let copied = handle_with_runtime(&runtime, "codex", &captured, false, CliRestartAction::Copy);
    assert!(copied.is_none());
}
