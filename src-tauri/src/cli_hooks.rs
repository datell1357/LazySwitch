use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub struct InstallResult {
    pub target: String,
    pub path: PathBuf,
    pub changed: bool,
    pub note: String,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn backup(file: &Path) {
    if file.exists() {
        let backup_path = PathBuf::from(format!("{}.lazyswitch-bak-{}", file.display(), now_ms()));
        let _ = fs::copy(file, backup_path);
    }
}

fn read_json_object(file: &Path) -> serde_json::Map<String, serde_json::Value> {
    fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_json_object(file: &Path, value: &serde_json::Map<String, serde_json::Value>) -> bool {
    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let next = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
    let current = fs::read_to_string(file).unwrap_or_default();
    if next == current {
        return false;
    }
    backup(file);
    let _ = fs::write(file, next);
    true
}

fn shell_path(file: &Path) -> String {
    file.display().to_string().replace('\\', "/")
}

/// `cli_js` is the standalone `lazyswitch` CLI's compiled entry point
/// (`dist/main/cli.js`), executed with plain `node` — the CLI package is
/// intentionally kept as a Node script (it must run headlessly wherever the
/// user's shell invokes `claude`/`codex`, unrelated to whether the GUI shell
/// is Electron or Tauri) and is bundled as a Tauri resource so this path is
/// still valid once the app is installed.
fn statusline_command(cli_js: &Path) -> String {
    format!("node \"{}\" statusline claude", shell_path(cli_js))
}

fn install_claude(cli_js: &Path) -> InstallResult {
    let file = dirs::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("settings.json");
    let mut settings = read_json_object(&file);
    settings.insert(
        "statusLine".to_string(),
        serde_json::json!({
            "type": "command",
            "command": statusline_command(cli_js),
            "padding": 0,
            "refreshInterval": 60,
        }),
    );
    let changed = write_json_object(&file, &settings);
    InstallResult {
        target: "Claude Code".to_string(),
        path: file,
        changed,
        note: "installed command statusLine".to_string(),
    }
}

const CODEX_WRAPPER_MARKER: &str = "LazySwitch Codex wrapper";
const CODEX_DIRECT_COMMANDS: &[&str] = &[
    "exec",
    "e",
    "review",
    "logout",
    "mcp",
    "plugin",
    "mcp-server",
    "app-server",
    "remote-control",
    "app",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "archive",
    "delete",
    "unarchive",
    "cloud",
    "exec-server",
    "features",
    "help",
    "-h",
    "--help",
    "-V",
    "--version",
];

fn npm_bin_dir() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(v) if !v.is_empty() => PathBuf::from(v).join("npm"),
        _ => dirs::home_dir()
            .unwrap_or_default()
            .join("AppData")
            .join("Roaming")
            .join("npm"),
    }
}

fn write_text_if_changed(file: &Path, text: &str) -> bool {
    let current = fs::read_to_string(file).unwrap_or_default();
    if current == text {
        return false;
    }
    if !current.is_empty() && !current.contains(CODEX_WRAPPER_MARKER) {
        backup(file);
    }
    let _ = fs::write(file, text);
    true
}

fn ensure_native_shim(dir: &Path, file: &str, native_file: &str) -> bool {
    let shim = dir.join(file);
    let native = dir.join(native_file);
    if native.exists() {
        return true;
    }
    if !shim.exists() {
        return false;
    }
    let Ok(current) = fs::read_to_string(&shim) else {
        return false;
    };
    if current.contains(CODEX_WRAPPER_MARKER) {
        return false;
    }
    fs::copy(&shim, &native).is_ok()
}

fn ps_quote(file: &Path) -> String {
    shell_path(file).replace('\'', "''")
}

fn watch_script(node_exe: &Path, cli_js: &Path) -> String {
    format!(
        "# {CODEX_WRAPPER_MARKER}\n\
$Host.UI.RawUI.WindowTitle = \"LazySwitch Codex Usage\"\n\
while ($true) {{\n\
  Clear-Host\n\
  & '{}' '{}' statusline codex\n\
  Start-Sleep -Seconds 60\n\
}}\n",
        ps_quote(node_exe),
        ps_quote(cli_js)
    )
}

fn pane_script() -> String {
    format!(
        r#"# {CODEX_WRAPPER_MARKER}
$ErrorActionPreference = "Stop"
$basedir = Split-Path $MyInvocation.MyCommand.Definition -Parent
$watch = Join-Path $basedir "lazyswitch-codex-watch.ps1"
$wt = (Get-Command wt.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source)
if (-not $wt) {{ $candidate = Join-Path (Join-Path (Join-Path $env:LOCALAPPDATA "Microsoft") "WindowsApps") "wt.exe"; if (Test-Path $candidate) {{ $wt = $candidate }} }}
$wtArgs = if ($env:WT_SESSION) {{
  @("-w", "0", "split-pane", "-H", "--size", "0.34", "--title", "LazySwitch-Codex-Usage", "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $watch)
}} else {{ @("new-tab", "--title", "LazySwitch-Codex-Usage", "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $watch) }}
if ($wt) {{ & $wt @wtArgs | Out-Null }} else {{ Start-Process powershell.exe -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $watch) | Out-Null }}
"#
    )
}

fn ps_wrapper() -> String {
    let direct = CODEX_DIRECT_COMMANDS
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"#!/usr/bin/env pwsh
# {CODEX_WRAPPER_MARKER}
$basedir = Split-Path $MyInvocation.MyCommand.Definition -Parent
$first = if ($args.Count -gt 0) {{ $args[0] }} else {{ "" }}
$direct = @({direct})
if (-not $env:LAZYSWITCH_CODEX_WRAPPED -and -not $direct.Contains($first)) {{
  & "$basedir/lazyswitch-codex-pane.ps1"
}}
$env:LAZYSWITCH_CODEX_WRAPPED = "1"
& "$basedir/codex-native.ps1" @args
exit $LASTEXITCODE
"#
    )
}

fn cmd_wrapper() -> String {
    let direct = CODEX_DIRECT_COMMANDS.join(" ");
    format!(
        r#"@ECHO off
REM {CODEX_WRAPPER_MARKER}
SETLOCAL
SET "basedir=%~dp0"
SET "first=%~1"
SET "wrap=1"
IF DEFINED LAZYSWITCH_CODEX_WRAPPED SET "wrap=0"
FOR %%C IN ({direct}) DO IF /I "%first%"=="%%C" SET "wrap=0"
IF "%wrap%"=="1" powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%basedir%lazyswitch-codex-pane.ps1" >NUL 2>NUL
SET "LAZYSWITCH_CODEX_WRAPPED=1"
CALL "%basedir%codex-native.cmd" %*
EXIT /B %ERRORLEVEL%
"#
    )
}

fn sh_wrapper() -> String {
    let direct = CODEX_DIRECT_COMMANDS.join("|");
    format!(
        r#"#!/bin/sh
# {CODEX_WRAPPER_MARKER}
basedir=$(dirname "$(echo "$0" | sed -e 's,\\,/,g')")
wrap=1
case "$1" in
  {direct}) wrap=0 ;;
esac
if [ -n "$LAZYSWITCH_CODEX_WRAPPED" ]; then
  wrap=0
fi
if [ "$wrap" = "1" ]; then
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$basedir/lazyswitch-codex-pane.ps1" >/dev/null 2>&1 &
fi
export LAZYSWITCH_CODEX_WRAPPED=1
exec "$basedir/codex-native" "$@"
"#
    )
}

fn install_codex_wrapper(node_exe: &Path, cli_js: &Path) -> InstallResult {
    let dir = npm_bin_dir();
    let _ = fs::create_dir_all(&dir);
    let native_ready = [
        ensure_native_shim(&dir, "codex.ps1", "codex-native.ps1"),
        ensure_native_shim(&dir, "codex.cmd", "codex-native.cmd"),
        ensure_native_shim(&dir, "codex", "codex-native"),
    ]
    .iter()
    .all(|ok| *ok);
    if !native_ready {
        return InstallResult {
            target: "Codex CLI wrapper".to_string(),
            path: dir,
            changed: false,
            note: "failed to find original codex shims; wrapper was not installed".to_string(),
        };
    }
    let changed = [
        write_text_if_changed(&dir.join("lazyswitch-codex-pane.ps1"), &pane_script()),
        write_text_if_changed(
            &dir.join("lazyswitch-codex-watch.ps1"),
            &watch_script(node_exe, cli_js),
        ),
        write_text_if_changed(&dir.join("codex.ps1"), &ps_wrapper()),
        write_text_if_changed(&dir.join("codex.cmd"), &cmd_wrapper()),
        write_text_if_changed(&dir.join("codex"), &sh_wrapper()),
    ]
    .iter()
    .any(|ok| *ok);
    InstallResult {
        target: "Codex CLI wrapper".to_string(),
        path: dir,
        changed,
        note: "wrapped interactive codex launches with a Windows Terminal LazySwitch usage pane; use codex-native to bypass".to_string(),
    }
}

fn replace_tui_setting(config: &str, key: &str, line: &str) -> String {
    let key_prefix = format!("{key} ");
    for l in config.lines() {
        if l.trim_start().starts_with(&key_prefix)
            && l.trim_start()[key.len()..].trim_start().starts_with('=')
        {
            return config.replacen(l, line, 1);
        }
    }
    if config.lines().any(|l| l.trim() == "[tui]") {
        return config.replacen("[tui]", &format!("[tui]\n{line}"), 1);
    }
    format!("{}\n\n[tui]\n{line}\n", config.trim_end())
}

fn install_codex(cwd_home: Option<&Path>) -> InstallResult {
    let home = cwd_home
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
    let file = home.join(".codex").join("config.toml");
    let current = fs::read_to_string(&file).unwrap_or_default();
    let line = r#"status_line = ["model-with-reasoning", "context-remaining", "five-hour-limit", "weekly-limit"]"#;
    let mut next = replace_tui_setting(&current, "status_line", line);
    next = replace_tui_setting(
        &next,
        "status_line_use_colors",
        "status_line_use_colors = true",
    );
    let changed = next != current;
    if changed {
        if let Some(parent) = file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        backup(&file);
        let _ = fs::write(&file, &next);
    }
    InstallResult {
        target: "Codex CLI".to_string(),
        path: file,
        changed,
        note: "enabled built-in status_line quota fields with colored limit gauges; external command statusline is not supported by Codex CLI".to_string(),
    }
}

/// `cli_js` / `node_exe` are only needed for the (Windows-only, best-effort)
/// codex wrapper install; `install_hooks` always runs the Claude + Codex
/// status-line installs, matching the original's unconditional pair.
pub fn install_hooks(cli_js: &Path) -> Vec<InstallResult> {
    vec![install_claude(cli_js), install_codex(None)]
}

pub fn install_codex_wrapper_hook(node_exe: &Path, cli_js: &Path) -> InstallResult {
    install_codex_wrapper(node_exe, cli_js)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_tui_setting_inserts_new_section_when_absent() {
        let out = replace_tui_setting("", "status_line", "status_line = [\"a\"]");
        assert!(out.contains("[tui]\nstatus_line = [\"a\"]"));
    }

    #[test]
    fn replace_tui_setting_appends_into_existing_tui_section() {
        let out = replace_tui_setting("[tui]\nother = 1\n", "status_line", "status_line = [\"a\"]");
        assert!(out.contains("[tui]\nstatus_line = [\"a\"]\nother = 1"));
    }

    #[test]
    fn replace_tui_setting_replaces_existing_key_in_place() {
        let out = replace_tui_setting(
            "[tui]\nstatus_line = [\"old\"]\nother = 1\n",
            "status_line",
            "status_line = [\"new\"]",
        );
        assert!(out.contains("status_line = [\"new\"]"));
        assert!(!out.contains("\"old\""));
    }

    #[test]
    fn write_json_object_reports_unchanged_when_content_matches() {
        let dir = std::env::temp_dir().join(format!("lazyswitch-hooks-{}", std::process::id()));
        let file = dir.join("settings.json");
        let mut m = serde_json::Map::new();
        m.insert("a".to_string(), serde_json::json!(1));
        assert!(write_json_object(&file, &m));
        assert!(!write_json_object(&file, &m));
        let _ = fs::remove_dir_all(&dir);
    }
}
