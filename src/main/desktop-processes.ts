import { execFile } from "child_process";
import * as path from "path";

export interface DesktopProcessRow {
  readonly pid: number;
  readonly parentPid: number;
  readonly name: string | null;
  readonly executablePath: string | null;
}

export interface DesktopProcessSnapshot {
  readonly targets: readonly DesktopProcessRow[];
  readonly parents: readonly DesktopProcessRow[];
}

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

function encodePowerShell(script: string): string {
  return Buffer.from(script, "utf16le").toString("base64");
}

function powershellString(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
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

async function runPowerShellJson(script: string): Promise<unknown> {
  const stdout = await execFileText(
    "powershell.exe",
    ["-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encodePowerShell(script)],
    15_000
  );
  const trimmed = stdout.trim();
  if (!trimmed) return {};
  const parsed: unknown = JSON.parse(trimmed);
  return parsed;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function readProcessRow(value: unknown): DesktopProcessRow | null {
  if (!isRecord(value) || typeof value.pid !== "number") return null;
  return {
    pid: value.pid,
    parentPid: typeof value.parentPid === "number" ? value.parentPid : 0,
    name: typeof value.name === "string" && value.name.length > 0 ? value.name : null,
    executablePath:
      typeof value.executablePath === "string" && value.executablePath.length > 0
        ? value.executablePath
        : null,
  };
}

function readProcessRows(value: unknown): readonly DesktopProcessRow[] {
  const rows = Array.isArray(value) ? value : [value];
  return rows
    .map(readProcessRow)
    .filter((row): row is DesktopProcessRow => row !== null);
}

function readSnapshot(value: unknown): DesktopProcessSnapshot {
  if (!isRecord(value)) return { targets: [], parents: [] };
  return {
    targets: readProcessRows(value.targets),
    parents: readProcessRows(value.parents),
  };
}

async function enumerateDesktopProcesses(
  processNames: readonly string[]
): Promise<DesktopProcessSnapshot> {
  const names = processNames.map((n) => n.trim()).filter((n) => n.length > 0);
  if (names.length === 0) return { targets: [], parents: [] };
  // NOTE: helper names must not collide with default PowerShell aliases —
  // aliases outrank functions, so a helper named GP or R silently runs
  // Get-ItemProperty / Invoke-History instead and the enumeration comes back
  // empty (this broke desktop kill entirely before being caught).
  const script = `
$ErrorActionPreference = "SilentlyContinue"
$Names = @(${names.map(powershellString).join(", ")})
function Get-ProcRows {
  if (Get-Command Get-CimInstance -ErrorAction SilentlyContinue) {
    return Get-CimInstance Win32_Process -ErrorAction SilentlyContinue
  }
  return Get-WmiObject Win32_Process -ErrorAction SilentlyContinue
}
function ConvertTo-ProcRow($p) {
  $pp = 0; if ($null -ne $p.ParentProcessId) { $pp = [int]$p.ParentProcessId }
  $exe = $null; if ($null -ne $p.ExecutablePath) { $exe = [string]$p.ExecutablePath }
  return [pscustomobject]@{ pid = [int]$p.ProcessId; parentPid = $pp; name = [string]$p.Name; executablePath = $exe }
}
$parents = @(Get-ProcRows)
$targets = @($parents | Where-Object { $Names -contains $_.Name })
ConvertTo-Json -InputObject ([pscustomobject]@{
  targets = @($targets | ForEach-Object { ConvertTo-ProcRow $_ })
  parents = @($parents | ForEach-Object { ConvertTo-ProcRow $_ })
}) -Compress
`;
  try {
    return readSnapshot(await runPowerShellJson(script));
  } catch {
    return { targets: [], parents: [] };
  }
}

function normalizeWindowsPath(value: string): string {
  return path.win32.normalize(value).replace(/\\+$/, "").toLowerCase();
}

function isKnownCliExecutablePath(normalizedPath: string): boolean {
  return (
    normalizedPath.includes("\\appdata\\local\\openai\\codex\\") ||
    normalizedPath.includes("\\.codex\\")
  );
}

function isDesktopExecutablePath(
  normalizedPath: string,
  desktopAppPath: string | null
): boolean {
  if (
    desktopAppPath !== null &&
    !desktopAppPath.toLowerCase().startsWith("shell:") &&
    normalizedPath === normalizeWindowsPath(desktopAppPath)
  ) {
    return true;
  }
  // Codex Desktop ≥26.7 ships as the merged ChatGPT app; the Store package is
  // still OpenAI.Codex but the executable inside is ChatGPT.exe. A future
  // package rename to OpenAI.ChatGPT* is matched pre-emptively.
  return (
    normalizedPath.includes("\\program files\\windowsapps\\openai.codex_") ||
    normalizedPath.includes("\\program files\\windowsapps\\openai.chatgpt")
  );
}

function processMap(snapshot: DesktopProcessSnapshot): ReadonlyMap<number, DesktopProcessRow> {
  return new Map([...snapshot.parents, ...snapshot.targets].map((row) => [row.pid, row]));
}

function isDescendantOfRoot(
  pid: number,
  rows: ReadonlyMap<number, DesktopProcessRow>,
  rootPid: number
): boolean {
  if (rootPid <= 0 || pid === rootPid) return pid === rootPid;
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

function hasTerminalAncestor(
  pid: number,
  rows: ReadonlyMap<number, DesktopProcessRow>
): boolean {
  const seen = new Set<number>();
  let current = pid;
  while (rows.has(current)) {
    const row = rows.get(current);
    if (row === undefined) return false;
    const parentPid = row.parentPid;
    if (parentPid <= 0 || seen.has(parentPid)) return false;
    const parent = rows.get(parentPid);
    if (parent && parent.name && TERMINAL_PROCESS_NAMES.has(parent.name.toLowerCase())) {
      return true;
    }
    seen.add(parentPid);
    current = parentPid;
  }
  return false;
}

export function selectDesktopProcessIds(
  snapshot: DesktopProcessSnapshot,
  desktopAppPath: string | null,
  rootPid = process.pid
): number[] {
  const rows = processMap(snapshot);
  const selected: number[] = [];
  for (const target of snapshot.targets) {
    if (target.pid <= 0 || isDescendantOfRoot(target.pid, rows, rootPid)) continue;
    if (target.executablePath) {
      const normalizedPath = normalizeWindowsPath(target.executablePath);
      if (isKnownCliExecutablePath(normalizedPath)) continue;
      if (isDesktopExecutablePath(normalizedPath, desktopAppPath)) selected.push(target.pid);
      continue;
    }
    // MSIX/elevated rows can hide ExecutablePath. Path is authoritative when
    // readable; unknown paths are included only when they are outside our own
    // process tree and no terminal-like ancestor is visible.
    if (!hasTerminalAncestor(target.pid, rows)) selected.push(target.pid);
  }
  return selected;
}

async function taskkillPid(pid: number): Promise<void> {
  try {
    await execFileText("taskkill.exe", ["/PID", String(pid), "/T", "/F"], 5_000);
  } catch {
    return;
  }
}

export async function killWindowsDesktopProcesses(
  processNames: readonly string[],
  desktopAppPath: string | null
): Promise<void> {
  const snapshot = await enumerateDesktopProcesses(processNames);
  const pids = selectDesktopProcessIds(snapshot, desktopAppPath);
  await Promise.all(pids.map(taskkillPid));
}

/** MSIX package names the Codex/ChatGPT desktop app has shipped under. */
const MSIX_PACKAGE_NAMES = ["OpenAI.Codex", "OpenAI.ChatGPT"];

/**
 * Resolve the Store/MSIX desktop app to a launchable AppUserModelID
 * ("shell:AppsFolder\\<PackageFamilyName>!<AppId>"). WindowsApps exes cannot
 * be spawned directly, so this is the only reliable launch handle.
 */
export async function resolveDesktopAumid(): Promise<string | null> {
  if (process.platform !== "win32") return null;
  const script = `
$ErrorActionPreference = "SilentlyContinue"
foreach ($name in @(${MSIX_PACKAGE_NAMES.map(powershellString).join(", ")})) {
  $pkg = Get-AppxPackage -Name $name
  if ($null -eq $pkg) { continue }
  $manifest = Get-AppxPackageManifest $pkg
  $appId = @($manifest.Package.Applications.Application)[0].Id
  if ($appId) { Write-Output ($pkg.PackageFamilyName + '!' + $appId); break }
}
`;
  try {
    const stdout = await execFileText(
      "powershell.exe",
      ["-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encodePowerShell(script)],
      15_000
    );
    const line = stdout.trim().split(/\r?\n/)[0]?.trim();
    return line ? "shell:AppsFolder\\" + line : null;
  } catch {
    return null;
  }
}
