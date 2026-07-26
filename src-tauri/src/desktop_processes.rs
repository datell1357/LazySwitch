use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone, PartialEq)]
pub struct DesktopProcessRow {
    pub pid: i64,
    pub parent_pid: i64,
    pub name: Option<String>,
    pub executable_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesktopProcessSnapshot {
    pub targets: Vec<DesktopProcessRow>,
    pub parents: Vec<DesktopProcessRow>,
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

/// MSIX package names the Codex/ChatGPT desktop app has shipped under.
const MSIX_PACKAGE_NAMES: &[&str] = &["OpenAI.Codex", "OpenAI.ChatGPT"];

fn encode_powershell(script: &str) -> String {
    let utf16le: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    STANDARD.encode(utf16le)
}

fn powershell_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

async fn exec_file_text(file: &str, args: &[&str], timeout_ms: u64) -> Result<String, String> {
    let mut cmd = Command::new(file);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = match timeout(Duration::from_millis(timeout_ms), cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err(format!("{file} timed out after {timeout_ms}ms")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{file} exited with {:?}", output.status.code())
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn run_powershell_json(script: &str) -> Result<Value, String> {
    let stdout = exec_file_text(
        "powershell.exe",
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encode_powershell(script),
        ],
        15_000,
    )
    .await?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(trimmed).map_err(|e| e.to_string())
}

fn read_process_row(value: &Value) -> Option<DesktopProcessRow> {
    let obj = value.as_object()?;
    let pid = obj.get("pid")?.as_i64()?;
    Some(DesktopProcessRow {
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
    })
}

fn read_process_rows(value: &Value) -> Vec<DesktopProcessRow> {
    match value {
        Value::Array(a) => a.iter().filter_map(read_process_row).collect(),
        other => read_process_row(other).into_iter().collect(),
    }
}

fn read_snapshot(value: &Value) -> DesktopProcessSnapshot {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return DesktopProcessSnapshot::default(),
    };
    DesktopProcessSnapshot {
        targets: obj
            .get("targets")
            .map(read_process_rows)
            .unwrap_or_default(),
        parents: obj
            .get("parents")
            .map(read_process_rows)
            .unwrap_or_default(),
    }
}

/// NOTE: helper names in the script must not collide with default PowerShell
/// aliases — aliases outrank functions, so a helper named e.g. `GP` or `R`
/// would silently run `Get-ItemProperty` / `Invoke-History` instead and the
/// enumeration would come back empty (this broke desktop kill entirely
/// before being caught in the original TS implementation).
async fn enumerate_desktop_processes(process_names: &[String]) -> DesktopProcessSnapshot {
    let names: Vec<String> = process_names
        .iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() {
        return DesktopProcessSnapshot::default();
    }
    let names_literal = names
        .iter()
        .map(|n| powershell_string(n))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        r#"
$ErrorActionPreference = "SilentlyContinue"
$Names = @({names_literal})
function Get-ProcRows {{
  if (Get-Command Get-CimInstance -ErrorAction SilentlyContinue) {{
    return Get-CimInstance Win32_Process -ErrorAction SilentlyContinue
  }}
  return Get-WmiObject Win32_Process -ErrorAction SilentlyContinue
}}
function ConvertTo-ProcRow($p) {{
  $pp = 0; if ($null -ne $p.ParentProcessId) {{ $pp = [int]$p.ParentProcessId }}
  $exe = $null; if ($null -ne $p.ExecutablePath) {{ $exe = [string]$p.ExecutablePath }}
  return [pscustomobject]@{{ pid = [int]$p.ProcessId; parentPid = $pp; name = [string]$p.Name; executablePath = $exe }}
}}
$parents = @(Get-ProcRows)
$targets = @($parents | Where-Object {{ $Names -contains $_.Name }})
ConvertTo-Json -InputObject ([pscustomobject]@{{
  targets = @($targets | ForEach-Object {{ ConvertTo-ProcRow $_ }})
  parents = @($parents | ForEach-Object {{ ConvertTo-ProcRow $_ }})
}}) -Compress
"#
    );
    match run_powershell_json(&script).await {
        Ok(v) => read_snapshot(&v),
        Err(_) => DesktopProcessSnapshot::default(),
    }
}

/// Lightweight stand-in for Node's `path.win32.normalize` sufficient for the
/// equality checks this feeds: unify slash direction, collapse repeats,
/// strip a trailing separator, and fold case. Does not resolve `.`/`..`
/// segments — the inputs here are always absolute WMI/config paths, never
/// relative ones, so that part of `normalize` never mattered in practice.
pub(crate) fn normalize_windows_path(value: &str) -> String {
    let unified = value.replace('/', "\\");
    let mut collapsed = String::with_capacity(unified.len());
    let mut last_was_sep = false;
    for c in unified.chars() {
        if c == '\\' {
            if !last_was_sep {
                collapsed.push(c);
            }
            last_was_sep = true;
        } else {
            collapsed.push(c);
            last_was_sep = false;
        }
    }
    while collapsed.ends_with('\\') {
        collapsed.pop();
    }
    collapsed.to_lowercase()
}

fn is_known_cli_executable_path(normalized_path: &str) -> bool {
    normalized_path.contains("\\appdata\\local\\openai\\codex\\")
        || normalized_path.contains("\\.codex\\")
}

fn is_desktop_executable_path(normalized_path: &str, desktop_app_path: Option<&str>) -> bool {
    if let Some(p) = desktop_app_path {
        if !p.to_lowercase().starts_with("shell:") && normalized_path == normalize_windows_path(p) {
            return true;
        }
    }
    // Codex Desktop >= 26.7 ships as the merged ChatGPT app; the Store
    // package is still OpenAI.Codex but the executable inside is
    // ChatGPT.exe. A future package rename to OpenAI.ChatGPT* is matched
    // pre-emptively.
    normalized_path.contains("\\program files\\windowsapps\\openai.codex_")
        || normalized_path.contains("\\program files\\windowsapps\\openai.chatgpt")
}

fn process_map(snapshot: &DesktopProcessSnapshot) -> HashMap<i64, &DesktopProcessRow> {
    let mut map = HashMap::new();
    for row in snapshot.parents.iter().chain(snapshot.targets.iter()) {
        map.insert(row.pid, row);
    }
    map
}

fn is_descendant_of_root(pid: i64, rows: &HashMap<i64, &DesktopProcessRow>, root_pid: i64) -> bool {
    if root_pid <= 0 || pid == root_pid {
        return pid == root_pid;
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

fn has_terminal_ancestor(pid: i64, rows: &HashMap<i64, &DesktopProcessRow>) -> bool {
    let mut seen = HashSet::new();
    let mut current = pid;
    while let Some(row) = rows.get(&current) {
        let parent_pid = row.parent_pid;
        if parent_pid <= 0 || seen.contains(&parent_pid) {
            return false;
        }
        if let Some(parent) = rows.get(&parent_pid) {
            if let Some(name) = &parent.name {
                if TERMINAL_PROCESS_NAMES
                    .iter()
                    .any(|t| *t == name.to_lowercase())
                {
                    return true;
                }
            }
        }
        seen.insert(parent_pid);
        current = parent_pid;
    }
    false
}

/// MSIX/elevated rows can hide ExecutablePath. Path is authoritative when
/// readable; unknown paths are included only when they are outside our own
/// process tree and no terminal-like ancestor is visible.
pub fn select_desktop_process_ids(
    snapshot: &DesktopProcessSnapshot,
    desktop_app_path: Option<&str>,
    root_pid: i64,
) -> Vec<i64> {
    let rows = process_map(snapshot);
    let mut selected = Vec::new();
    for target in &snapshot.targets {
        if target.pid <= 0 || is_descendant_of_root(target.pid, &rows, root_pid) {
            continue;
        }
        if let Some(exe) = &target.executable_path {
            let normalized = normalize_windows_path(exe);
            if is_known_cli_executable_path(&normalized) {
                continue;
            }
            if is_desktop_executable_path(&normalized, desktop_app_path) {
                selected.push(target.pid);
            }
            continue;
        }
        if !has_terminal_ancestor(target.pid, &rows) {
            selected.push(target.pid);
        }
    }
    selected
}

async fn taskkill_pid(pid: i64) {
    let _ = exec_file_text(
        "taskkill.exe",
        &["/PID", &pid.to_string(), "/T", "/F"],
        5_000,
    )
    .await;
}

pub async fn kill_windows_desktop_processes(
    process_names: &[String],
    desktop_app_path: Option<&str>,
) {
    let snapshot = enumerate_desktop_processes(process_names).await;
    let pids = select_desktop_process_ids(&snapshot, desktop_app_path, std::process::id() as i64);
    let handles: Vec<_> = pids
        .into_iter()
        .map(|pid| tokio::spawn(taskkill_pid(pid)))
        .collect();
    for h in handles {
        let _ = h.await;
    }
}

/// Resolve the Store/MSIX desktop app to a launchable AppUserModelID
/// ("shell:AppsFolder\\<PackageFamilyName>!<AppId>"). WindowsApps exes cannot
/// be spawned directly, so this is the only reliable launch handle.
pub async fn resolve_desktop_aumid() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let names_literal = MSIX_PACKAGE_NAMES
        .iter()
        .map(|n| powershell_string(n))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        r#"
$ErrorActionPreference = "SilentlyContinue"
foreach ($name in @({names_literal})) {{
  $pkg = Get-AppxPackage -Name $name
  if ($null -eq $pkg) {{ continue }}
  $manifest = Get-AppxPackageManifest $pkg
  $appId = @($manifest.Package.Applications.Application)[0].Id
  if ($appId) {{ Write-Output ($pkg.PackageFamilyName + '!' + $appId); break }}
}}
"#
    );
    let stdout = exec_file_text(
        "powershell.exe",
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encode_powershell(&script),
        ],
        15_000,
    )
    .await
    .ok()?;
    let line = stdout.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        None
    } else {
        Some(format!("shell:AppsFolder\\{line}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i64, parent_pid: i64, name: Option<&str>, exe: Option<&str>) -> DesktopProcessRow {
        DesktopProcessRow {
            pid,
            parent_pid,
            name: name.map(|s| s.to_string()),
            executable_path: exe.map(|s| s.to_string()),
        }
    }

    #[test]
    fn normalize_unifies_slashes_case_and_trailing_sep() {
        assert_eq!(
            normalize_windows_path(r"C:/Program Files/Codex/Codex.exe/"),
            r"c:\program files\codex\codex.exe"
        );
        assert_eq!(
            normalize_windows_path(r"C:\\Program Files\\\\Codex\\Codex.exe"),
            r"c:\program files\codex\codex.exe"
        );
    }

    #[test]
    fn desktop_executable_path_matches_configured_path() {
        let configured = r"C:\Users\me\AppData\Local\Programs\Codex\Codex.exe";
        let normalized = normalize_windows_path(configured);
        assert!(is_desktop_executable_path(&normalized, Some(configured)));
        assert!(!is_desktop_executable_path(
            &normalized,
            Some(r"C:\other\path.exe")
        ));
    }

    #[test]
    fn desktop_executable_path_ignores_shell_prefixed_config() {
        let normalized = normalize_windows_path(r"C:\real\Codex.exe");
        assert!(!is_desktop_executable_path(
            &normalized,
            Some("shell:AppsFolder\\Foo!App")
        ));
    }

    #[test]
    fn desktop_executable_path_matches_msix_install() {
        let normalized = normalize_windows_path(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0_x64__abc\ChatGPT.exe",
        );
        assert!(is_desktop_executable_path(&normalized, None));
    }

    #[test]
    fn known_cli_executable_path_excludes_codex_cli() {
        assert!(is_known_cli_executable_path(&normalize_windows_path(
            r"C:\Users\me\AppData\Local\OpenAI\Codex\codex.exe"
        )));
        assert!(is_known_cli_executable_path(&normalize_windows_path(
            r"C:\Users\me\.codex\bin\codex.exe"
        )));
    }

    #[test]
    fn select_skips_processes_in_own_tree() {
        let snapshot = DesktopProcessSnapshot {
            targets: vec![row(
                200,
                100,
                Some("Codex.exe"),
                Some(r"C:\Codex\Codex.exe"),
            )],
            parents: vec![row(100, 1, Some("app.exe"), None)],
        };
        // root_pid 100 is the ancestor of target 200 -> must be skipped.
        let selected = select_desktop_process_ids(&snapshot, None, 100);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_includes_desktop_exe_by_configured_path() {
        let path = r"C:\Users\me\AppData\Local\Programs\Codex\Codex.exe";
        let snapshot = DesktopProcessSnapshot {
            targets: vec![row(200, 1, Some("Codex.exe"), Some(path))],
            parents: vec![],
        };
        let selected = select_desktop_process_ids(&snapshot, Some(path), 999);
        assert_eq!(selected, vec![200]);
    }

    #[test]
    fn select_excludes_codex_cli_executable_even_if_named_like_target() {
        let snapshot = DesktopProcessSnapshot {
            targets: vec![row(
                200,
                1,
                Some("Codex.exe"),
                Some(r"C:\Users\me\.codex\bin\codex.exe"),
            )],
            parents: vec![],
        };
        let selected = select_desktop_process_ids(&snapshot, None, 999);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_without_executable_path_excludes_terminal_descendants() {
        // 300 (target, no exe path) is a child of 100 (powershell.exe) which
        // is unrelated to our own process tree (root_pid 999).
        let snapshot = DesktopProcessSnapshot {
            targets: vec![row(300, 100, Some("ChatGPT.exe"), None)],
            parents: vec![row(100, 1, Some("powershell.exe"), None)],
        };
        let selected = select_desktop_process_ids(&snapshot, None, 999);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_without_executable_path_includes_when_no_terminal_ancestor() {
        let snapshot = DesktopProcessSnapshot {
            targets: vec![row(300, 100, Some("ChatGPT.exe"), None)],
            parents: vec![row(100, 1, Some("explorer.exe"), None)],
        };
        let selected = select_desktop_process_ids(&snapshot, None, 999);
        assert_eq!(selected, vec![300]);
    }

    #[test]
    fn select_ignores_non_positive_pids() {
        let snapshot = DesktopProcessSnapshot {
            targets: vec![row(0, 0, Some("Codex.exe"), None)],
            parents: vec![],
        };
        assert!(select_desktop_process_ids(&snapshot, None, 999).is_empty());
    }

    #[test]
    fn read_snapshot_parses_targets_and_parents() {
        let v: Value = serde_json::from_str(
            r#"{"targets":[{"pid":5,"parentPid":1,"name":"Codex.exe","executablePath":"C:\\x.exe"}],"parents":[{"pid":1,"parentPid":0,"name":"","executablePath":""}]}"#,
        )
        .unwrap();
        let snap = read_snapshot(&v);
        assert_eq!(snap.targets.len(), 1);
        assert_eq!(snap.targets[0].pid, 5);
        assert_eq!(snap.targets[0].name.as_deref(), Some("Codex.exe"));
        // empty-string name/path collapse to None, matching the TS filter.
        assert_eq!(snap.parents[0].name, None);
        assert_eq!(snap.parents[0].executable_path, None);
    }

    #[test]
    fn encode_powershell_round_trips_utf16le_base64() {
        let encoded = encode_powershell("Write-Output 'hi'");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "Write-Output 'hi'");
    }
}
