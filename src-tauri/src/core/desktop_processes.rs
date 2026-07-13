//! Filtering for Codex Desktop processes. Unknown/elevated rows are retained
//! only when they cannot be traced to a terminal or to this process.

use std::collections::HashMap;

use super::cli_cwd::{normalize_windows_path, ProcessRow, ProcessSnapshot};
use super::cli_sessions::is_terminal_emulator;

fn known_cli(path: &str) -> bool {
    let path = normalize_windows_path(path);
    path.contains("\\appdata\\local\\openai\\codex\\") || path.contains("\\.codex\\")
}

fn desktop_path(path: &str, configured: Option<&str>) -> bool {
    let normalized = normalize_windows_path(path);
    if let Some(configured) = configured.filter(|value| !value.to_ascii_lowercase().starts_with("shell:")) {
        if normalized == normalize_windows_path(configured) { return true; }
    }
    normalized.contains("\\program files\\windowsapps\\openai.codex_") || normalized.contains("\\program files\\windowsapps\\openai.chatgpt")
}

fn map(snapshot: &ProcessSnapshot) -> HashMap<u32, ProcessRow> { snapshot.parents.iter().chain(snapshot.targets.iter()).cloned().map(|row| (row.pid, row)).collect() }

fn descendant(pid: u32, rows: &HashMap<u32, ProcessRow>, root: u32) -> bool {
    if pid == root { return true; }
    if root == 0 { return false; }
    let mut current = pid;
    let mut seen = std::collections::HashSet::new();
    while let Some(row) = rows.get(&current) {
        if row.parent_pid == root { return true; }
        if row.parent_pid == 0 || !seen.insert(row.parent_pid) { return false; }
        current = row.parent_pid;
    }
    false
}

fn has_terminal_ancestor(pid: u32, rows: &HashMap<u32, ProcessRow>) -> bool {
    let mut current = pid;
    let mut seen = std::collections::HashSet::new();
    while let Some(row) = rows.get(&current) {
        if row.parent_pid == 0 || !seen.insert(row.parent_pid) { return false; }
        if rows.get(&row.parent_pid).and_then(|parent| parent.name.as_deref()).is_some_and(is_terminal_emulator) { return true; }
        current = row.parent_pid;
    }
    false
}

pub fn select_desktop_process_ids(snapshot: &ProcessSnapshot, desktop_app_path: Option<&str>, root_pid: u32) -> Vec<u32> {
    let rows = map(snapshot);
    snapshot.targets.iter().filter_map(|target| {
        if target.pid == 0 || descendant(target.pid, &rows, root_pid) { return None; }
        if let Some(path) = target.executable_path.as_deref() {
            let normalized = normalize_windows_path(path);
            if known_cli(&normalized) { return None; }
            return desktop_path(&normalized, desktop_app_path).then_some(target.pid);
        }
        (!has_terminal_ancestor(target.pid, &rows)).then_some(target.pid)
    }).collect()
}

pub fn select_desktop_process_ids_from_rows(targets: Vec<ProcessRow>, parents: Vec<ProcessRow>, desktop_app_path: Option<&str>, root_pid: u32) -> Vec<u32> {
    select_desktop_process_ids(&ProcessSnapshot { targets, parents }, desktop_app_path, root_pid)
}

/// Enumerate and terminate only the processes selected by the filter. The
/// caller supplies the configured process names; no WMI/PowerShell process
/// query is involved.
pub fn kill_windows_desktop_processes(process_names: &[String], desktop_app_path: Option<&str>) -> Vec<u32> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        let mut parents = Vec::new();
        let mut targets = Vec::new();
        for name in process_names.iter().map(String::as_str).filter(|name| !name.trim().is_empty()) {
            if let Ok(snapshot) = super::cli_cwd::snapshot(name) {
                parents.extend(snapshot.parents);
                targets.extend(snapshot.targets);
            }
        }
        let snapshot = ProcessSnapshot { targets, parents };
        let selected = select_desktop_process_ids(&snapshot, desktop_app_path, std::process::id());
        for pid in &selected {
            if let Ok(handle) = unsafe { OpenProcess(PROCESS_TERMINATE, false, *pid) } {
                unsafe { let _ = TerminateProcess(handle, 0); let _ = CloseHandle(handle); }
            }
        }
        selected
    }
    #[cfg(not(windows))]
    { let _ = (process_names, desktop_app_path); Vec::new() }
}

/// Store/MSIX package lookup is deliberately kept separate from process
/// enumeration. Windows exposes the launchable AUMID through AppX APIs; the
/// existing platform launcher may also use a user-supplied shell: AUMID.
pub fn resolve_desktop_aumid() -> Option<String> {
    #[cfg(windows)]
    {
        let script = "foreach ($name in @('OpenAI.Codex','OpenAI.ChatGPT')) { $pkg=Get-AppxPackage -Name $name; if ($pkg) { $m=Get-AppxPackageManifest $pkg; $id=@($m.Package.Applications.Application)[0].Id; if ($id) { Write-Output ($pkg.PackageFamilyName+'!'+$id); break } } }";
        let output = std::process::Command::new("powershell.exe").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script]).output().ok()?;
        let line = String::from_utf8_lossy(&output.stdout).lines().next()?.trim().to_owned();
        (!line.is_empty()).then(|| format!("shell:AppsFolder\\{line}"))
    }
    #[cfg(not(windows))]
    { None }
}
