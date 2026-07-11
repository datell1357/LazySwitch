// @allow SIZE_OK - urgent process-control fix; split only with regression tests allowed.
import { execFile, spawn } from "child_process";
import { mkdtemp, readFile, rm, stat, writeFile } from "fs/promises";
import * as os from "os";
import * as path from "path";
import { findClaudeSessionForProcess } from "./claude-sessions";
import { findCodexRolloutForProcess } from "./codex-rollouts";
import { PEB_CWD_SCRIPT } from "./cli-cwd-script";
import {
  decideConsoleResumeRoute,
  recordCliRestartOutcome,
} from "./cli-resume-routing";
import type {
  CliRestartCounters,
  CliRestartOutcome,
  ConsoleInjectionResult,
} from "./cli-resume-routing";
import type { Provider } from "./providers/types";

export interface CliSession {
  readonly providerId: Provider["id"];
  readonly pid: number;
  readonly startTime: string | null;
  readonly cwd: string | null;
  readonly terminal: CliTerminal | null;
}

export interface CliTerminal {
  readonly pid: number;
  readonly name: string;
  readonly isOrcaHosted: boolean;
}

export interface CliResumeCommand {
  readonly text: string;
  readonly command: string;
  readonly args: readonly string[];
}

export interface CliRestartResult {
  readonly restarted: number;
  readonly resumedInPlace: number;
  readonly manual: number;
  readonly failed: number;
}

interface OrcaTerminal {
  readonly handle: string;
  readonly worktreePath: string;
}

interface DetectorProcessRow {
  readonly pid: number;
  readonly parentPid: number;
  readonly name: string | null;
  readonly executablePath: string | null;
  readonly startTime: string | null;
  readonly cwd: string | null;
}

const PROCESS_NAMES: Record<Provider["id"], string> = {
  codex: "codex.exe",
  claude: "claude.exe",
};

const SYSTEM_ROOT = process.env.SystemRoot || "C:\\Windows";
const SYSTEM32_ROOT = path.join(SYSTEM_ROOT, "System32");
const CMD_EXE = path.join(SYSTEM32_ROOT, "cmd.exe");
const POWERSHELL_EXE = path.join(
  SYSTEM32_ROOT,
  "WindowsPowerShell",
  "v1.0",
  "powershell.exe"
);
const TASKKILL_EXE = path.join(SYSTEM32_ROOT, "taskkill.exe");
const WHERE_EXE = path.join(SYSTEM32_ROOT, "where.exe");

const TERMINAL_PROCESS_NAMES = new Set([
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
]);

const RESUME_COMMANDS: Record<Provider["id"], CliResumeCommand> = {
  codex: { text: "codex resume", command: "codex", args: ["resume"] },
  claude: { text: "claude --continue", command: "claude", args: ["--continue"] },
};

const POWERSHELL_RESUME_SCRIPT =
  "$cliArgs = @((ConvertFrom-Json $env:LAZYSWITCH_CLI_ARGS)); " +
  "& $env:LAZYSWITCH_CLI_COMMAND @cliArgs";

function consoleInjectionSupportScript(): string {
  return `
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class LazySwitchConsole {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct KEY_EVENT_RECORD {
    [MarshalAs(UnmanagedType.Bool)] public bool bKeyDown;
    public ushort wRepeatCount;
    public ushort wVirtualKeyCode;
    public ushort wVirtualScanCode;
    public char UnicodeChar;
    public uint dwControlKeyState;
  }

  [StructLayout(LayoutKind.Explicit, CharSet = CharSet.Unicode)]
  public struct INPUT_RECORD {
    [FieldOffset(0)] public ushort EventType;
    [FieldOffset(4)] public KEY_EVENT_RECORD KeyEvent;
  }

  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool FreeConsole();

  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool AttachConsole(uint dwProcessId);

  [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  public static extern IntPtr CreateFileW(
    string lpFileName,
    uint dwDesiredAccess,
    uint dwShareMode,
    IntPtr lpSecurityAttributes,
    uint dwCreationDisposition,
    uint dwFlagsAndAttributes,
    IntPtr hTemplateFile
  );

  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool FlushConsoleInputBuffer(IntPtr hConsoleInput);

  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool WriteConsoleInputW(
    IntPtr hConsoleInput,
    [In] INPUT_RECORD[] lpBuffer,
    uint nLength,
    out uint lpNumberOfEventsWritten
  );

  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool CloseHandle(IntPtr hObject);
}
'@

function Get-LazySwitchNativeErrorCode([Exception]$Exception) {
  $current = $Exception
  while ($null -ne $current) {
    if ($current -is [ComponentModel.Win32Exception]) {
      return $current.NativeErrorCode
    }
    $current = $current.InnerException
  }
  return $null
}

function Invoke-LazySwitchConsoleResume([uint32]$ShellPid, [string]$EncodedText) {
  [void][LazySwitchConsole]::FreeConsole()
  try {
    if (-not [LazySwitchConsole]::AttachConsole($ShellPid)) {
      throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
    }
    $consoleInput = [LazySwitchConsole]::CreateFileW(
      'CONIN$',
      [uint32]3221225472,
      [uint32]0x00000003,
      [IntPtr]::Zero,
      [uint32]3,
      [uint32]0,
      [IntPtr]::Zero
    )
    if ($consoleInput -eq [IntPtr]::Zero -or $consoleInput -eq [IntPtr](-1)) {
      throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
    }

    try {
      if (-not [LazySwitchConsole]::FlushConsoleInputBuffer($consoleInput)) {
        throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
      }
      $text = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($EncodedText))
      $records = [LazySwitchConsole+INPUT_RECORD[]]::new($text.Length * 2)
      $index = 0
      foreach ($character in $text.ToCharArray()) {
        foreach ($isDown in @($true, $false)) {
          $key = [LazySwitchConsole+KEY_EVENT_RECORD]::new()
          $key.bKeyDown = $isDown
          $key.wRepeatCount = 1
          $key.wVirtualKeyCode = if ($character -eq [char]13) { 13 } else { 0 }
          $key.UnicodeChar = $character
          $record = [LazySwitchConsole+INPUT_RECORD]::new()
          $record.EventType = 1
          $record.KeyEvent = $key
          $records[$index] = $record
          $index += 1
        }
      }
      [uint32]$written = 0
      if (-not [LazySwitchConsole]::WriteConsoleInputW($consoleInput, $records, [uint32]$records.Length, [ref]$written) -or $written -ne $records.Length) {
        throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
      }
    } finally {
      [void][LazySwitchConsole]::CloseHandle($consoleInput)
    }
  } finally {
    [void][LazySwitchConsole]::FreeConsole()
  }
}
`;
}

function consoleInjectionScript(shellPid: number, text: string): string {
  const encodedText = Buffer.from(`${text}\r`, "utf16le").toString("base64");
  return `
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
${consoleInjectionSupportScript()}

try {
    Invoke-LazySwitchConsoleResume ([uint32]${shellPid}) '${encodedText}'
    [pscustomobject]@{ ok = $true } | ConvertTo-Json -Compress
    exit 0
} catch {
  [pscustomobject]@{ ok = $false; error = $_.Exception.Message; nativeErrorCode = (Get-LazySwitchNativeErrorCode $_.Exception) } | ConvertTo-Json -Compress
  exit 1
}
`;
}

function encodePowerShell(script: string): string {
  return Buffer.from(script, "utf16le").toString("base64");
}

function execFileText(
  file: string,
  args: readonly string[],
  timeoutMs: number
): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      file,
      [...args],
      { timeout: timeoutMs, windowsHide: true, maxBuffer: 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error) {
          reject(new Error(String(stderr || error.message)));
          return;
        }
        resolve(String(stdout));
      }
    );
  });
}

function execFileStdoutRegardlessOfExit(
  file: string,
  args: readonly string[],
  timeoutMs: number
): Promise<string> {
  return new Promise((resolve) => {
    execFile(
      file,
      [...args],
      { timeout: timeoutMs, windowsHide: true, maxBuffer: 1024 * 1024 },
      (_error, stdout) => resolve(String(stdout))
    );
  });
}

async function runPowerShellJson(script: string): Promise<unknown> {
  const stdout = await execFileText(
    POWERSHELL_EXE,
    ["-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encodePowerShell(script)],
    15_000
  );
  const trimmed = stdout.trim();
  if (!trimmed) return [];
  const parsed: unknown = JSON.parse(trimmed);
  return parsed;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/**
 * Drop trailing separators (PEB cwd ends with "\") but keep drive roots.
 * A trailing backslash before a closing quote breaks cmd/wt argument
 * parsing when the path is passed to the relaunch command line.
 */
function trimCwd(value: string): string {
  const trimmed = value.replace(/[\\/]+$/, "");
  return /^[A-Za-z]:$/.test(trimmed) ? trimmed + "\\" : trimmed;
}

function readDetectorProcess(value: unknown): DetectorProcessRow | null {
  if (!isRecord(value) || typeof value.pid !== "number") return null;
  return {
    pid: value.pid,
    parentPid: typeof value.parentPid === "number" ? value.parentPid : 0,
    name: typeof value.name === "string" && value.name.length > 0 ? value.name : null,
    executablePath:
      typeof value.executablePath === "string" && value.executablePath.length > 0
        ? value.executablePath
        : null,
    startTime: typeof value.startTime === "string" ? value.startTime : null,
    cwd: typeof value.cwd === "string" && value.cwd.length > 0 ? trimCwd(value.cwd) : null,
  };
}

function readSession(
  row: DetectorProcessRow,
  providerId: Provider["id"],
  rows: ReadonlyMap<number, DetectorProcessRow>
): CliSession {
  return {
    providerId,
    pid: row.pid,
    startTime: row.startTime,
    cwd: row.cwd,
    terminal: terminalAncestor(row.pid, rows),
  };
}

function readDetectorRows(value: unknown): readonly DetectorProcessRow[] {
  const rows = Array.isArray(value) ? value : [value];
  return rows
    .map(readDetectorProcess)
    .filter((row): row is DetectorProcessRow => row !== null);
}

function normalizeWindowsPath(value: string): string {
  return value.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
}

export function pairOrcaTerminalHandles(
  sessions: readonly CliSession[],
  terminals: readonly OrcaTerminal[]
): Map<number, string> {
  const handlesByWorktree = new Map<string, readonly string[]>();
  for (const terminal of terminals) {
    const worktree = normalizeWindowsPath(terminal.worktreePath);
    handlesByWorktree.set(worktree, [...(handlesByWorktree.get(worktree) ?? []), terminal.handle]);
  }

  const sessionsByWorktree = new Map<string, readonly CliSession[]>();
  for (const session of sessions) {
    if (!session.terminal?.isOrcaHosted || session.cwd === null) continue;
    const worktree = normalizeWindowsPath(session.cwd);
    sessionsByWorktree.set(worktree, [...(sessionsByWorktree.get(worktree) ?? []), session]);
  }

  const pairs = new Map<number, string>();
  for (const [worktree, matchingSessions] of sessionsByWorktree) {
    const handles = handlesByWorktree.get(worktree);
    if (
      matchingSessions.length !== 1 ||
      handles === undefined ||
      handles.length !== 1
    ) {
      continue;
    }
    pairs.set(matchingSessions[0].pid, handles[0]);
  }
  return pairs;
}

export function formatResumeCommandForShell(
  cwd: string,
  resume: CliResumeCommand,
  shellName: string
): string {
  const normalizedShell = shellName.toLowerCase();
  if (normalizedShell === "cmd.exe") {
    return `cd /d "${cwd.replace(/"/g, '""')}" && ${resume.text}`;
  }
  if (normalizedShell === "bash.exe") {
    return `cd -- '${cwd.replace(/'/g, "'\\''")}' && ${resume.text}`;
  }
  return `cd '${cwd.replace(/'/g, "''")}'; ${resume.text}`;
}

function isCodexDesktopPath(value: string | null): boolean {
  if (value === null) return false;
  return normalizeWindowsPath(value).includes("\\program files\\windowsapps\\openai.codex_");
}

function isDescendantOfRoot(
  pid: number,
  rows: ReadonlyMap<number, DetectorProcessRow>,
  rootPid: number
): boolean {
  if (rootPid <= 0) return false;
  const seen = new Set<number>();
  let current = pid;
  while (rows.has(current)) {
    const row = rows.get(current);
    if (row === undefined) return false;
    const parentPid = row.parentPid;
    if (parentPid === rootPid) return true;
    if (parentPid <= 0 || seen.has(parentPid)) return false;
    seen.add(parentPid);
    current = parentPid;
  }
  return false;
}

function terminalAncestor(
  pid: number,
  rows: ReadonlyMap<number, DetectorProcessRow>
): CliTerminal | null {
  const seen = new Set<number>();
  let current = pid;
  let terminal: Omit<CliTerminal, "isOrcaHosted"> | null = null;
  let isOrcaHosted = false;
  while (rows.has(current)) {
    const row = rows.get(current);
    if (row === undefined) return null;
    const name = row.name?.toLowerCase();
    if (name === "orca-terminal-daemon.exe") isOrcaHosted = true;
    if (terminal === null && name && TERMINAL_PROCESS_NAMES.has(name)) {
      terminal = { pid: row.pid, name: row.name ?? "" };
    }
    const parentPid = row.parentPid;
    if (parentPid <= 0 || seen.has(parentPid)) break;
    seen.add(parentPid);
    current = parentPid;
  }
  return terminal === null ? null : { ...terminal, isOrcaHosted };
}

function isCliCandidate(
  row: DetectorProcessRow,
  rows: ReadonlyMap<number, DetectorProcessRow>,
  providerId: Provider["id"],
  rootPid: number
): boolean {
  if (row.pid === rootPid) return false;
  if (isDescendantOfRoot(row.pid, rows, rootPid)) return false;
  if (providerId !== "codex") return true;
  if (isCodexDesktopPath(row.executablePath) || isCodexDesktopPath(row.cwd)) return false;
  // Elevated (admin-terminal) sessions hide executablePath and cwd from an
  // unelevated scan — keep them as long as a terminal ancestor is visible;
  // the restart path degrades to copying the resume command for them.
  return terminalAncestor(row.pid, rows) !== null;
}

export function readDetectorOutput(
  value: unknown,
  providerId: Provider["id"],
  rootPid: number
): CliSession[] {
  if (!isRecord(value) || !Array.isArray(value.targets)) {
    return providerId === "codex"
      ? []
      : readDetectorRows(value).map((row) =>
          readSession(row, providerId, new Map([[row.pid, row]]))
        );
  }

  const targets = readDetectorRows(value.targets);
  const parents = readDetectorRows(value.parents);
  const rows = new Map([...parents, ...targets].map((row) => [row.pid, row]));
  return value.targets
    .map(readDetectorProcess)
    .filter((row): row is DetectorProcessRow => row !== null)
    .filter((row) => isCliCandidate(row, rows, providerId, rootPid))
    .map((row) => readSession(row, providerId, rows));
}

export function resumeCommandFor(provider: Provider): CliResumeCommand {
  return RESUME_COMMANDS[provider.id];
}

export async function detectCliSessions(
  provider: Provider,
  rootPid = process.pid
): Promise<CliSession[]> {
  if (process.platform !== "win32") return [];
  const processName = PROCESS_NAMES[provider.id].replace(/'/g, "''");
  const script = PEB_CWD_SCRIPT.replace("__PROCESS_NAME__", processName).replace(
    "__ROOT_PID__",
    String(rootPid)
  );
  try {
    const parsed = await runPowerShellJson(script);
    return readDetectorOutput(parsed, provider.id, rootPid);
  } catch {
    return [];
  }
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function taskkill(pid: number, force: boolean): Promise<void> {
  const args = force
    ? ["/PID", String(pid), "/T", "/F"]
    : ["/PID", String(pid), "/T"];
  try {
    await execFileText(TASKKILL_EXE, args, 5_000);
  } catch {
    return;
  }
}

async function terminateProcess(pid: number): Promise<boolean> {
  // Safety: restarting kills any in-flight turn; the resume command restores
  // the conversation transcript, not work that was mid-flight.
  await taskkill(pid, false);
  await delay(3_000);
  if (!isProcessAlive(pid)) return true;
  await taskkill(pid, true);
  await delay(500);
  return !isProcessAlive(pid);
}

async function commandExists(command: string): Promise<boolean> {
  try {
    await execFileText(WHERE_EXE, [command], 5_000);
    return true;
  } catch {
    return false;
  }
}

function readOrcaTerminals(value: unknown): readonly OrcaTerminal[] {
  if (!isRecord(value)) return [];
  const result = isRecord(value.result) ? value.result : value;
  if (!Array.isArray(result.terminals)) return [];
  return result.terminals.flatMap((terminal) => {
    if (
      !isRecord(terminal) ||
      typeof terminal.handle !== "string" ||
      terminal.handle.length === 0 ||
      typeof terminal.worktreePath !== "string" ||
      terminal.worktreePath.length === 0
    ) {
      return [];
    }
    return [{ handle: terminal.handle, worktreePath: terminal.worktreePath }];
  });
}

async function listOrcaTerminals(): Promise<readonly OrcaTerminal[]> {
  try {
    const output = await execFileText("orca", ["terminal", "list", "--json"], 5_000);
    return readOrcaTerminals(JSON.parse(output));
  } catch {
    return [];
  }
}

async function sendOrcaResume(handle: string, text: string): Promise<boolean> {
  try {
    const output = await execFileText(
      "orca",
      ["terminal", "send", "--terminal", handle, "--text", text, "--enter", "--json"],
      5_000
    );
    const result: unknown = JSON.parse(output);
    return isRecord(result) && result.ok === true;
  } catch {
    return false;
  }
}

async function injectConsoleResume(
  shellPid: number,
  text: string
): Promise<ConsoleInjectionResult> {
  try {
    const stdout = await execFileStdoutRegardlessOfExit(
      POWERSHELL_EXE,
      [
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        encodePowerShell(consoleInjectionScript(shellPid, text)),
      ],
      15_000
    );
    const result: unknown = JSON.parse(stdout.trim());
    if (isRecord(result) && result.ok === true) return "ok";
    return isRecord(result) && result.nativeErrorCode === 5 ? "access-denied" : "failed";
  } catch {
    return "failed";
  }
}

interface ElevatedConsoleResumeRequest {
  readonly sessionKey: string;
  readonly cliPid: number;
  readonly shellPid: number;
  readonly shellName: string;
  readonly resumeText: string;
}

function encodeUtf16(value: string): string {
  return Buffer.from(value, "utf16le").toString("base64");
}

function elevatedConsoleResumeScript(batchFile: string, resultFile: string): string {
  const encodedBatchFile = encodeUtf16(batchFile);
  const encodedResultFile = encodeUtf16(resultFile);
  return `
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
${consoleInjectionSupportScript()}

$batchFile = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('${encodedBatchFile}'))
$resultFile = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('${encodedResultFile}'))
$results = [System.Collections.Generic.List[object]]::new()
$batch = @(Get-Content -LiteralPath $batchFile -Raw | ConvertFrom-Json)

foreach ($item in $batch) {
  $ok = $false
  try {
    & (Join-Path $env:SystemRoot 'System32\\taskkill.exe') /PID ([string]$item.cliPid) /T /F | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($null -ne (Get-Process -Id ([int]$item.cliPid) -ErrorAction SilentlyContinue) -and [DateTime]::UtcNow -lt $deadline) {
      Start-Sleep -Milliseconds 100
    }
    if ($null -ne (Get-Process -Id ([int]$item.cliPid) -ErrorAction SilentlyContinue)) {
      throw 'The CLI process did not exit after elevated taskkill.'
    }
    $resumeText = [string]$item.resumeText + [char]13
    $encodedText = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($resumeText))
    Invoke-LazySwitchConsoleResume ([uint32]$item.shellPid) $encodedText
    $ok = $true
  } catch {
    $ok = $false
  } finally {
    [void][LazySwitchConsole]::FreeConsole()
  }
  [void]$results.Add([pscustomobject]@{ sessionKey = [string]$item.sessionKey; ok = $ok })
}

$output = [pscustomobject]@{ results = @($results.ToArray()) } | ConvertTo-Json -Compress -Depth 3
[IO.File]::WriteAllText($resultFile, $output, [Text.UTF8Encoding]::new($false))
Write-Output $output
`;
}

function elevatedConsoleLauncherScript(elevatedScriptFile: string): string {
  const escapedElevatedScriptFile = elevatedScriptFile.replace(/'/g, "''");
  return `
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try {
  Start-Process -FilePath '${POWERSHELL_EXE.replace(/'/g, "''")}' -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', '${escapedElevatedScriptFile}' -Verb RunAs -Wait -PassThru -WindowStyle Hidden | Out-Null
  [pscustomobject]@{ launched = $true } | ConvertTo-Json -Compress
} catch {
  [pscustomobject]@{ launched = $false } | ConvertTo-Json -Compress
}
`;
}

function readElevatedResumeSuccesses(
  value: unknown,
  expectedSessionKeys: ReadonlySet<string>
): ReadonlySet<string> {
  if (!isRecord(value) || !Array.isArray(value.results)) return new Set();
  const successes = new Set<string>();
  for (const result of value.results) {
    if (
      isRecord(result) &&
      typeof result.sessionKey === "string" &&
      result.ok === true &&
      expectedSessionKeys.has(result.sessionKey)
    ) {
      successes.add(result.sessionKey);
    }
  }
  return successes;
}

async function runElevatedConsoleResumeBatch(
  batch: readonly ElevatedConsoleResumeRequest[]
): Promise<ReadonlySet<string>> {
  const tempDirectory = await mkdtemp(path.join(os.tmpdir(), "lazyswitch-cli-resume-"));
  const batchFile = path.join(tempDirectory, "batch.json");
  const resultFile = path.join(tempDirectory, "result.json");
  const elevatedScriptFile = path.join(tempDirectory, "elevated-resume.ps1");
  try {
    await Promise.all([
      writeFile(batchFile, JSON.stringify(batch), "utf8"),
      writeFile(
        elevatedScriptFile,
        `\uFEFF${elevatedConsoleResumeScript(batchFile, resultFile)}`,
        "utf8"
      ),
    ]);
    const launcherOutput = await execFileStdoutRegardlessOfExit(
      POWERSHELL_EXE,
      [
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        encodePowerShell(elevatedConsoleLauncherScript(elevatedScriptFile)),
      ],
      0
    );
    const launcherResult: unknown = JSON.parse(launcherOutput.trim());
    if (!isRecord(launcherResult) || launcherResult.launched !== true) return new Set();
    // RunAs uses ShellExecute, which cannot redirect stdout. The elevated helper
    // mirrors its stdout JSON into this result file for the unelevated caller.
    const helperResult: unknown = JSON.parse(await readFile(resultFile, "utf8"));
    return readElevatedResumeSuccesses(
      helperResult,
      new Set(batch.map((request) => request.sessionKey))
    );
  } catch {
    return new Set();
  } finally {
    await rm(tempDirectory, { recursive: true, force: true });
  }
}

function launchWithWindowsTerminal(cwd: string, resume: CliResumeCommand): boolean {
  try {
    const child = spawn("wt.exe", ["-d", cwd, resume.command, ...resume.args], {
      detached: true,
      stdio: "ignore",
      windowsHide: false,
    });
    // Launch failures surface as an async "error" event; without a listener
    // they crash the main process instead of just losing the relaunch.
    child.on("error", (error) => console.warn("[cli] wt launch failed", error));
    child.unref();
    return true;
  } catch {
    return false;
  }
}

function launchWithPowerShell(cwd: string, resume: CliResumeCommand): boolean {
  try {
    const child = spawn(
      CMD_EXE,
      [
        "/d",
        "/c",
        "start",
        "",
        POWERSHELL_EXE,
        "-NoExit",
        "-EncodedCommand",
        encodePowerShell(POWERSHELL_RESUME_SCRIPT),
      ],
      {
        cwd,
        detached: true,
        env: {
          ...process.env,
          LAZYSWITCH_CLI_COMMAND: resume.command,
          LAZYSWITCH_CLI_ARGS: JSON.stringify(resume.args),
        },
        stdio: "ignore",
        windowsHide: false,
      }
    );
    child.on("error", (error) => console.warn("[cli] relaunch failed", error));
    child.unref();
    return true;
  } catch {
    return false;
  }
}

function codexResumeCommand(sessionId: string): CliResumeCommand {
  return { text: `codex resume ${sessionId}`, command: "codex", args: ["resume", sessionId] };
}

function claudeResumeCommand(sessionId: string): CliResumeCommand {
  return {
    text: `claude --resume ${sessionId}`,
    command: "claude",
    args: ["--resume", sessionId],
  };
}

async function existingRawCwd(value: string): Promise<string | null> {
  const cwd = trimCwd(
    value
      .replace(/^\\\\\?\\UNC\\/i, "\\\\")
      .replace(/^\\\\\?\\/, "")
  );
  try {
    return (await stat(cwd)).isDirectory() ? cwd : null;
  } catch {
    return null;
  }
}

interface ResumeFallback {
  readonly providerId: Provider["id"];
  readonly cwd: string | null;
  readonly resume: CliResumeCommand;
}

interface DeferredElevatedResume {
  readonly elevated: ElevatedConsoleResumeRequest;
  readonly fallback: ResumeFallback;
}

function resumeFallbackOutcome(fallback: ResumeFallback, hasWt: boolean): CliRestartOutcome {
  let cwd = fallback.cwd;
  if (cwd === null) {
    if (fallback.providerId === "codex") {
      cwd = os.homedir(); // the resume picker works from anywhere
    } else {
      // `claude --continue` only finds sessions of its working directory.
      return "manual";
    }
  }
  const launched = hasWt
    ? launchWithWindowsTerminal(cwd, fallback.resume)
    : launchWithPowerShell(cwd, fallback.resume);
  return launched ? "restarted" : "failed";
}

export async function restartCliSessions(
  sessions: readonly CliSession[],
  resume: CliResumeCommand
): Promise<CliRestartResult> {
  const hasWt = await commandExists("wt.exe");
  let orcaHandles: Map<number, string> | null = null;
  const claimedCodexSessionIds = new Set<string>();
  const claimedClaudeSessionIds = new Set<string>();
  let counters: CliRestartCounters = {
    restarted: 0,
    resumedInPlace: 0,
    manual: 0,
    failed: 0,
  };
  const deferredElevatedResumes: DeferredElevatedResume[] = [];

  for (const session of sessions) {
    let sessionResume = resume;
    let cwd = session.cwd;
    if (session.providerId === "codex") {
      const rollout = await findCodexRolloutForProcess(
        session,
        undefined,
        claimedCodexSessionIds
      );
      if (rollout !== null) {
        claimedCodexSessionIds.add(rollout.sessionId);
        sessionResume = codexResumeCommand(rollout.sessionId);
        if (cwd === null) {
          cwd = (await existingRawCwd(rollout.cwd)) ?? os.homedir();
        }
      }
    } else {
      const match = await findClaudeSessionForProcess(
        session,
        undefined,
        claimedClaudeSessionIds
      );
      if (match !== null) {
        claimedClaudeSessionIds.add(match.sessionId);
        const matchedCwd = await existingRawCwd(match.cwd);
        if (matchedCwd !== null) {
          sessionResume = claudeResumeCommand(match.sessionId);
          cwd = matchedCwd;
        }
      }
    }

    // Best effort — an elevated session can't be killed from an unelevated
    // app, but a fresh window with the resume command still gets the user
    // back to the conversation; the old window just stays open.
    const stopped = await terminateProcess(session.pid);
    const fallback: ResumeFallback = {
      providerId: session.providerId,
      cwd,
      resume: sessionResume,
    };

    if (session.terminal) {
      const inPlaceCommand =
        cwd === null
          ? sessionResume.text
          : formatResumeCommandForShell(cwd, sessionResume, session.terminal.name);
      const injection = stopped
        ? await injectConsoleResume(session.terminal.pid, inPlaceCommand)
        : null;
      let resumed = injection === "ok";
      if (stopped && !resumed && session.terminal.isOrcaHosted && session.cwd !== null) {
        if (orcaHandles === null) {
          const terminals = (await commandExists("orca")) ? await listOrcaTerminals() : [];
          orcaHandles = pairOrcaTerminalHandles(sessions, terminals);
        }
        const orcaHandle = orcaHandles.get(session.pid);
        resumed = orcaHandle !== undefined && (await sendOrcaResume(orcaHandle, inPlaceCommand));
      }
      if (resumed) {
        counters = recordCliRestartOutcome(counters, "resumed-in-place");
        continue;
      }
      if (
        decideConsoleResumeRoute({
          stopped,
          hasTerminal: true,
          injection,
        }) === "elevate"
      ) {
        deferredElevatedResumes.push({
          elevated: {
            sessionKey: String(session.pid),
            cliPid: session.pid,
            shellPid: session.terminal.pid,
            shellName: session.terminal.name,
            resumeText: inPlaceCommand,
          },
          fallback,
        });
        continue;
      }
    }

    counters = recordCliRestartOutcome(counters, resumeFallbackOutcome(fallback, hasWt));
  }

  const elevatedSuccesses =
    deferredElevatedResumes.length === 0
      ? new Set<string>()
      : await runElevatedConsoleResumeBatch(
          deferredElevatedResumes.map((deferred) => deferred.elevated)
        );
  for (const deferred of deferredElevatedResumes) {
    if (elevatedSuccesses.has(deferred.elevated.sessionKey)) {
      counters = recordCliRestartOutcome(counters, "resumed-in-place");
    } else {
      counters = recordCliRestartOutcome(
        counters,
        resumeFallbackOutcome(deferred.fallback, hasWt)
      );
    }
  }

  return counters;
}
