import * as fs from "fs";
import * as os from "os";
import * as path from "path";

export type InstallResult = {
  readonly target: string;
  readonly path: string;
  readonly changed: boolean;
  readonly note: string;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readJsonObject(file: string): Record<string, unknown> {
  try {
    const parsed: unknown = JSON.parse(fs.readFileSync(file, "utf8"));
    return isRecord(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

function backup(file: string): void {
  if (fs.existsSync(file)) fs.copyFileSync(file, `${file}.lazyswitch-bak-${Date.now()}`);
}

function writeJsonObject(file: string, value: Record<string, unknown>): boolean {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const next = JSON.stringify(value, null, 2) + "\n";
  const current = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  if (next === current) return false;
  backup(file);
  fs.writeFileSync(file, next, "utf8");
  return true;
}

function shellPath(file: string): string {
  return file.replace(/\\/g, "/");
}

function statuslineCommand(provider: "claude"): string {
  // Claude runs this with plain node, which cannot read inside an asar archive;
  // point it at the unpacked copy (see asarUnpack in package.json).
  const cli = path.join(__dirname, "cli.js").replace(/app\.asar([\\/])/, "app.asar.unpacked$1");
  return `node "${shellPath(cli)}" statusline ${provider}`;
}

const CODEX_WRAPPER_MARKER = "LazySwitch Codex wrapper";
const CODEX_DIRECT_COMMANDS = [
  "exec", "e",
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
  "apply", "a",
  "archive",
  "delete",
  "unarchive",
  "cloud",
  "exec-server",
  "features",
  "help", "-h", "--help", "-V", "--version",
] as const;

function installClaude(): InstallResult {
  const file = path.join(os.homedir(), ".claude", "settings.json");
  const settings = readJsonObject(file);
  settings.statusLine = {
    type: "command",
    command: statuslineCommand("claude"),
    padding: 0,
    refreshInterval: 60,
  };
  const changed = writeJsonObject(file, settings);
  return {
    target: "Claude Code",
    path: file,
    changed,
    note: "installed command statusLine",
  };
}

function npmBinDir(): string {
  const appData = process.env.APPDATA ?? path.join(os.homedir(), "AppData", "Roaming");
  return path.join(appData, "npm");
}

function writeTextIfChanged(file: string, text: string): boolean {
  const current = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  if (current === text) return false;
  if (current && !current.includes(CODEX_WRAPPER_MARKER)) backup(file);
  fs.writeFileSync(file, text, "utf8");
  return true;
}

function ensureNativeShim(dir: string, file: string, nativeFile: string): boolean {
  const shim = path.join(dir, file);
  const native = path.join(dir, nativeFile);
  if (fs.existsSync(native)) return true;
  if (!fs.existsSync(shim)) return false;
  const current = fs.readFileSync(shim, "utf8");
  if (current.includes(CODEX_WRAPPER_MARKER)) return false;
  fs.copyFileSync(shim, native);
  return true;
}

function psQuote(file: string): string {
  return shellPath(file).replace(/'/g, "''");
}

function watchScript(nodeExe: string, cliJs: string): string {
  return `# ${CODEX_WRAPPER_MARKER}
$Host.UI.RawUI.WindowTitle = "LazySwitch Codex Usage"
while ($true) {
  Clear-Host
  & '${psQuote(nodeExe)}' '${psQuote(cliJs)}' statusline codex
  Start-Sleep -Seconds 60
}
`;
}

function paneScript(): string {
  return `# ${CODEX_WRAPPER_MARKER}
$ErrorActionPreference = "Stop"
$basedir = Split-Path $MyInvocation.MyCommand.Definition -Parent
$watch = Join-Path $basedir "lazyswitch-codex-watch.ps1"
$wt = (Get-Command wt.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source)
if (-not $wt) { $candidate = Join-Path (Join-Path (Join-Path $env:LOCALAPPDATA "Microsoft") "WindowsApps") "wt.exe"; if (Test-Path $candidate) { $wt = $candidate } }
$wtArgs = if ($env:WT_SESSION) {
  @("-w", "0", "split-pane", "-H", "--size", "0.34", "--title", "LazySwitch-Codex-Usage", "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $watch)
} else { @("new-tab", "--title", "LazySwitch-Codex-Usage", "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $watch) }
if ($wt) { & $wt @wtArgs | Out-Null } else { Start-Process powershell.exe -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $watch) | Out-Null }
`;
}

function psWrapper(): string {
  return `#!/usr/bin/env pwsh
# ${CODEX_WRAPPER_MARKER}
$basedir = Split-Path $MyInvocation.MyCommand.Definition -Parent
$first = if ($args.Count -gt 0) { $args[0] } else { "" }
$direct = @(${CODEX_DIRECT_COMMANDS.map((cmd) => `"${cmd}"`).join(", ")})
if (-not $env:LAZYSWITCH_CODEX_WRAPPED -and -not $direct.Contains($first)) {
  & "$basedir/lazyswitch-codex-pane.ps1"
}
$env:LAZYSWITCH_CODEX_WRAPPED = "1"
& "$basedir/codex-native.ps1" @args
exit $LASTEXITCODE
`;
}

function cmdWrapper(): string {
  return `@ECHO off
REM ${CODEX_WRAPPER_MARKER}
SETLOCAL
SET "basedir=%~dp0"
SET "first=%~1"
SET "wrap=1"
IF DEFINED LAZYSWITCH_CODEX_WRAPPED SET "wrap=0"
FOR %%C IN (${CODEX_DIRECT_COMMANDS.join(" ")}) DO IF /I "%first%"=="%%C" SET "wrap=0"
IF "%wrap%"=="1" powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%basedir%lazyswitch-codex-pane.ps1" >NUL 2>NUL
SET "LAZYSWITCH_CODEX_WRAPPED=1"
CALL "%basedir%codex-native.cmd" %*
EXIT /B %ERRORLEVEL%
`;
}

function shWrapper(): string {
  return `#!/bin/sh
# ${CODEX_WRAPPER_MARKER}
basedir=$(dirname "$(echo "$0" | sed -e 's,\\\\,/,g')")
wrap=1
case "$1" in
  ${CODEX_DIRECT_COMMANDS.join("|")}) wrap=0 ;;
esac
if [ -n "$LAZYSWITCH_CODEX_WRAPPED" ]; then
  wrap=0
fi
if [ "$wrap" = "1" ]; then
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$basedir/lazyswitch-codex-pane.ps1" >/dev/null 2>&1 &
fi
export LAZYSWITCH_CODEX_WRAPPED=1
exec "$basedir/codex-native" "$@"
`;
}

export function installCodexWrapper(): InstallResult {
  const dir = npmBinDir();
  fs.mkdirSync(dir, { recursive: true });
  const nativeReady = [
    ensureNativeShim(dir, "codex.ps1", "codex-native.ps1"),
    ensureNativeShim(dir, "codex.cmd", "codex-native.cmd"),
    ensureNativeShim(dir, "codex", "codex-native"),
  ].every(Boolean);
  if (!nativeReady) {
    return {
      target: "Codex CLI wrapper",
      path: dir,
      changed: false,
      note: "failed to find original codex shims; wrapper was not installed",
    };
  }
  const cliJs = path.join(__dirname, "cli.js");
  const nodeExe = process.execPath;
  const changed = [
    writeTextIfChanged(path.join(dir, "lazyswitch-codex-pane.ps1"), paneScript()),
    writeTextIfChanged(path.join(dir, "lazyswitch-codex-watch.ps1"), watchScript(nodeExe, cliJs)),
    writeTextIfChanged(path.join(dir, "codex.ps1"), psWrapper()),
    writeTextIfChanged(path.join(dir, "codex.cmd"), cmdWrapper()),
    writeTextIfChanged(path.join(dir, "codex"), shWrapper()),
  ].some(Boolean);
  return {
    target: "Codex CLI wrapper",
    path: dir,
    changed,
    note: "wrapped interactive codex launches with a Windows Terminal LazySwitch usage pane; use codex-native to bypass",
  };
}

function replaceTuiSetting(config: string, key: string, line: string): string {
  const existing = new RegExp(`^\\s*${key}\\s*=.*$`, "m");
  if (existing.test(config)) {
    return config.replace(existing, line);
  }
  if (/^\[tui\]\s*$/m.test(config)) {
    return config.replace(/^\[tui\]\s*$/m, `[tui]\n${line}`);
  }
  return `${config.trimEnd()}\n\n[tui]\n${line}\n`;
}

function installCodex(): InstallResult {
  const file = path.join(os.homedir(), ".codex", "config.toml");
  const current = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  const line =
    'status_line = ["model-with-reasoning", "context-remaining", "five-hour-limit", "weekly-limit"]';
  let next = replaceTuiSetting(current, "status_line", line);
  next = replaceTuiSetting(next, "status_line_use_colors", "status_line_use_colors = true");
  if (next !== current) {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    backup(file);
    fs.writeFileSync(file, next, "utf8");
  }
  return {
    target: "Codex CLI",
    path: file,
    changed: next !== current,
    note: "enabled built-in status_line quota fields with colored limit gauges; external command statusline is not supported by Codex CLI",
  };
}

export function installHooks(): readonly InstallResult[] {
  return [installClaude(), installCodex()];
}
