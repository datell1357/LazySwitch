// @allow SIZE_OK - urgent process-control fix; split only with regression tests allowed.
import { execFile, spawn } from "child_process";
import { stat } from "fs/promises";
import * as os from "os";
import * as path from "path";
import { findClaudeSessionForProcess } from "./claude-sessions";
import { findCodexRolloutForProcess } from "./codex-rollouts";
import { PEB_CWD_SCRIPT } from "./cli-cwd-script";
import { recordCliRestartOutcome } from "./cli-resume-routing";
import type { CliRestartCounters, CliRestartOutcome } from "./cli-resume-routing";
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

interface OrcaTerminal {
  readonly worktreeId: string;
  readonly worktreePath: string;
}

export interface CliResumeCommand {
  readonly text: string;
  readonly command: string;
  readonly args: readonly string[];
}

export interface CliRestartResult {
  readonly restarted: number;
  readonly closed: number;
  readonly manual: number;
  readonly failed: number;
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

/**
 * Only these hosts are safe to close: they run exactly one CLI session. The
 * emulators in TERMINAL_PROCESS_NAMES (wt.exe, WindowsTerminal.exe, …) can own
 * unrelated tabs, so closing one would take the user's other work with it.
 */
const CLOSABLE_SHELL_NAMES = new Set([
  "bash.exe",
  "cmd.exe",
  "powershell.exe",
  "pwsh.exe",
]);

const ORCA_TERMINAL_DAEMON = "orca-terminal-daemon.exe";

const RESUME_COMMANDS: Record<Provider["id"], CliResumeCommand> = {
  codex: { text: "codex resume", command: "codex", args: ["resume"] },
  claude: { text: "claude --continue", command: "claude", args: ["--continue"] },
};

const POWERSHELL_RESUME_SCRIPT =
  "$cliArgs = @((ConvertFrom-Json $env:LAZYSWITCH_CLI_ARGS)); " +
  "& $env:LAZYSWITCH_CLI_COMMAND @cliArgs";

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
  let shell: { pid: number; name: string } | null = null;
  let isOrcaHosted = false;
  // Keep walking past the shell: the Orca daemon sits above it, and only that
  // tells us the session lives in an Orca tab rather than a desktop console.
  while (rows.has(current)) {
    const row = rows.get(current);
    if (row === undefined) break;
    const name = row.name?.toLowerCase();
    if (name === ORCA_TERMINAL_DAEMON) isOrcaHosted = true;
    if (shell === null && name && TERMINAL_PROCESS_NAMES.has(name)) {
      shell = { pid: row.pid, name: row.name ?? "" };
    }
    const parentPid = row.parentPid;
    if (parentPid <= 0 || seen.has(parentPid)) break;
    seen.add(parentPid);
    current = parentPid;
  }
  return shell === null ? null : { ...shell, isOrcaHosted };
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

async function resolveCommandPath(command: string): Promise<string | null> {
  try {
    const stdout = await execFileText(WHERE_EXE, [command], 5_000);
    const first = stdout.split(/\r?\n/).find((line) => line.trim().length > 0);
    return first === undefined ? null : first.trim();
  } catch {
    return null;
  }
}

/**
 * `orca` is a .cmd shim, and execFile cannot launch one without a shell, so it
 * is routed through cmd.exe using the path `where` resolved.
 */
async function runOrca(
  orcaPath: string,
  args: readonly string[],
  timeoutMs: number
): Promise<string> {
  return /\.(cmd|bat)$/i.test(orcaPath)
    ? execFileText(CMD_EXE, ["/d", "/c", orcaPath, ...args], timeoutMs)
    : execFileText(orcaPath, args, timeoutMs);
}

function readOrcaTerminals(value: unknown): readonly OrcaTerminal[] {
  if (!isRecord(value)) return [];
  const result = isRecord(value.result) ? value.result : value;
  if (!Array.isArray(result.terminals)) return [];
  return result.terminals.flatMap((terminal) => {
    if (
      !isRecord(terminal) ||
      typeof terminal.worktreeId !== "string" ||
      terminal.worktreeId.length === 0 ||
      typeof terminal.worktreePath !== "string" ||
      terminal.worktreePath.length === 0
    ) {
      return [];
    }
    return [{ worktreeId: terminal.worktreeId, worktreePath: terminal.worktreePath }];
  });
}

async function listOrcaTerminals(orcaPath: string): Promise<readonly OrcaTerminal[]> {
  try {
    return readOrcaTerminals(
      JSON.parse(await runOrca(orcaPath, ["terminal", "list", "--json"], 10_000))
    );
  } catch {
    return [];
  }
}

/**
 * Pick the worktree that owns the session's directory. Orca's `path:` selector
 * times out waiting for a terminal handle, so the worktree is matched by path
 * here and then addressed by the `id:` taken from the same listing.
 */
export function orcaWorktreeIdForCwd(
  terminals: readonly OrcaTerminal[],
  cwd: string
): string | null {
  const target = normalizeWindowsPath(cwd);
  let best: OrcaTerminal | null = null;
  for (const terminal of terminals) {
    const worktree = normalizeWindowsPath(terminal.worktreePath);
    if (target !== worktree && !target.startsWith(worktree + "\\")) continue;
    if (best === null || worktree.length > normalizeWindowsPath(best.worktreePath).length) {
      best = terminal;
    }
  }
  return best === null ? null : best.worktreeId;
}

async function createOrcaTerminal(
  orcaPath: string,
  worktreeId: string,
  resume: CliResumeCommand
): Promise<boolean> {
  try {
    const output = await runOrca(
      orcaPath,
      [
        "terminal",
        "create",
        "--worktree",
        `id:${worktreeId}`,
        "--command",
        [resume.command, ...resume.args].join(" "),
        "--json",
      ],
      25_000
    );
    const result: unknown = JSON.parse(output);
    return isRecord(result) && result.ok === true;
  } catch {
    return false;
  }
}

/**
 * Windows Terminal cannot be probed before use: it ships as an app execution
 * alias that `where` misses and `stat` rejects with EACCES, and it is absent
 * from the PATH of a packaged app. So launch it and let the spawn itself be the
 * test — a failure surfaces as an async "error" event, not a throw.
 */
function spawnDetached(
  command: string,
  args: readonly string[],
  options: Parameters<typeof spawn>[2]
): Promise<boolean> {
  return new Promise((resolve) => {
    try {
      const child = spawn(command, [...args], options);
      child.once("error", () => resolve(false));
      child.once("spawn", () => {
        child.unref();
        resolve(true);
      });
    } catch {
      resolve(false);
    }
  });
}

function launchWithWindowsTerminal(cwd: string, resume: CliResumeCommand): Promise<boolean> {
  return spawnDetached("wt.exe", ["-d", cwd, resume.command, ...resume.args], {
    detached: true,
    stdio: "ignore",
    windowsHide: false,
  });
}

function launchWithPowerShell(cwd: string, resume: CliResumeCommand): Promise<boolean> {
  return spawnDetached(
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

/**
 * Close the shell the CLI was running in, so the user is not left with a dead
 * prompt beside the new terminal. Elevated shells cannot be killed from an
 * unelevated app; that is fine, the resume still opens in a fresh terminal.
 */
async function closeHostTerminal(terminal: CliTerminal): Promise<boolean> {
  if (!CLOSABLE_SHELL_NAMES.has(terminal.name.toLowerCase())) return false;
  return terminateProcess(terminal.pid);
}

interface OrcaContext {
  readonly orcaPath: string;
  readonly terminals: readonly OrcaTerminal[];
}

/**
 * Reopen the session in a fresh terminal. A session that lived in an Orca tab
 * gets another Orca tab and nothing else — it must never spill a desktop console
 * onto the user's machine, so a failure here is a failure, not a reason to open a
 * window. Everything else gets a Windows Terminal tab, or a PowerShell window if
 * that cannot launch.
 */
async function reopenInNewTerminal(
  session: CliSession,
  cwd: string,
  resume: CliResumeCommand,
  orca: OrcaContext | null
): Promise<boolean> {
  if (session.terminal?.isOrcaHosted === true) {
    if (orca === null) return false;
    const worktreeId = orcaWorktreeIdForCwd(orca.terminals, cwd);
    if (worktreeId === null) return false;
    return createOrcaTerminal(orca.orcaPath, worktreeId, resume);
  }
  if (await launchWithWindowsTerminal(cwd, resume)) return true;
  return launchWithPowerShell(cwd, resume);
}

async function resolveOrcaContext(
  sessions: readonly CliSession[]
): Promise<OrcaContext | null> {
  if (!sessions.some((session) => session.terminal?.isOrcaHosted === true)) return null;
  const orcaPath = await resolveCommandPath("orca");
  if (orcaPath === null) return null;
  return { orcaPath, terminals: await listOrcaTerminals(orcaPath) };
}

export async function restartCliSessions(
  sessions: readonly CliSession[],
  resume: CliResumeCommand
): Promise<CliRestartResult> {
  const orca = await resolveOrcaContext(sessions);
  const claimedCodexSessionIds = new Set<string>();
  const claimedClaudeSessionIds = new Set<string>();
  let counters: CliRestartCounters = { restarted: 0, closed: 0, manual: 0, failed: 0 };

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

    // Restarting kills any in-flight turn; the resume command restores the
    // conversation transcript, not work that was mid-flight.
    await terminateProcess(session.pid);
    if (session.terminal !== null && (await closeHostTerminal(session.terminal))) {
      counters = { ...counters, closed: counters.closed + 1 };
    }

    if (cwd === null) {
      if (session.providerId === "codex") {
        cwd = os.homedir(); // the resume picker works from anywhere
      } else {
        // `claude --continue` only finds sessions of its working directory.
        counters = recordCliRestartOutcome(counters, "manual");
        continue;
      }
    }

    const outcome: CliRestartOutcome = (await reopenInNewTerminal(
      session,
      cwd,
      sessionResume,
      orca
    ))
      ? "restarted"
      : "failed";
    counters = recordCliRestartOutcome(counters, outcome);
  }

  return counters;
}

