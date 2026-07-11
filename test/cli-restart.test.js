const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const claudeSessions = require("../dist/main/claude-sessions.js");
const codexRollouts = require("../dist/main/codex-rollouts.js");
const { recordCliRestartOutcome } = require("../dist/main/cli-resume-routing.js");
const { restartCliSessions, resumeCommandFor } = require("../dist/main/cli-sessions.js");

const EMPTY = { restarted: 0, closed: 0, manual: 0, failed: 0 };

function shell(name, overrides = {}) {
  return { pid: 51, name, isOrcaHosted: false, ...overrides };
}

/**
 * Stubs the process-control surface restartCliSessions drives: the `where`
 * lookup, `taskkill`, the orca CLI, and the terminal launchers. Returns the
 * recorded calls so a test can assert what the restart actually did.
 */
function stubRestart(
  t,
  { hasWt = true, unkillable = [], hasOrca = false, orcaTerminals = [], orcaCreateFails = false } = {}
) {
  const killed = [];
  const launches = [];
  const orcaCreates = [];

  t.mock.method(childProcess, "execFile", (file, args, _options, callback) => {
    const exe = path.basename(String(file)).toLowerCase();
    if (exe === "where.exe") {
      if (String(args[0]).toLowerCase() === "orca" && hasOrca) {
        callback(null, "C:\\orca\\orca.cmd\r\n", "");
      } else {
        callback(new Error("not found"), "", "not found");
      }
      return;
    }
    if (exe === "taskkill.exe") {
      killed.push(Number(args[1]));
      callback(null, "", "");
      return;
    }
    // orca is a .cmd shim, so it runs through cmd.exe /d /c <path> ...
    if (exe === "cmd.exe" && String(args[2] ?? "").toLowerCase().endsWith("orca.cmd")) {
      const orcaArgs = args.slice(3);
      if (orcaArgs[1] === "list") {
        callback(null, JSON.stringify({ ok: true, result: { terminals: orcaTerminals } }), "");
        return;
      }
      if (orcaArgs[1] === "create") {
        orcaCreates.push({ worktree: orcaArgs[3], command: orcaArgs[5] });
        callback(null, JSON.stringify({ ok: !orcaCreateFails }), "");
        return;
      }
    }
    callback(null, "", "");
  });

  // Windows Terminal is never probed; a launch either fires "spawn" or "error".
  t.mock.method(childProcess, "spawn", (command, args, options) => {
    const isWt = path.basename(String(command)).toLowerCase() === "wt.exe";
    const succeeds = !isWt || hasWt;
    if (succeeds) launches.push({ command, args, options });
    const handlers = {};
    const child = {
      once(event, handler) {
        handlers[event] = handler;
        return this;
      },
      unref() {},
    };
    queueMicrotask(() => {
      const handler = handlers[succeeds ? "spawn" : "error"];
      if (handler) handler(succeeds ? undefined : new Error("ENOENT"));
    });
    return child;
  });

  // A pid in `unkillable` survives taskkill, standing in for an elevated shell.
  t.mock.method(process, "kill", (pid) => {
    if (unkillable.includes(pid)) return undefined;
    throw new Error("ESRCH");
  });
  t.mock.method(global, "setTimeout", (callback) => { callback(); return 0; });

  return { killed, launches, orcaCreates };
}

test("restart accounting records each session in exactly one bucket", () => {
  assert.deepEqual(recordCliRestartOutcome(EMPTY, "restarted"), { ...EMPTY, restarted: 1 });
  assert.deepEqual(recordCliRestartOutcome(EMPTY, "manual"), { ...EMPTY, manual: 1 });
  assert.deepEqual(recordCliRestartOutcome(EMPTY, "failed"), { ...EMPTY, failed: 1 });
});

test("a codex session closes its old shell and reopens in a new terminal", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t);
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 501, startTime: null, cwd, terminal: shell("powershell.exe") }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 1, manual: 0, failed: 0 });
  assert.deepEqual(calls.killed, [501, 51]); // CLI first, then its shell
  assert.equal(calls.launches.length, 1);
  assert.equal(calls.launches[0].command, "wt.exe");
  assert.deepEqual(calls.launches[0].args, ["-d", cwd, "codex", "resume"]);
});

test("a claude session reopens with the resolved session id in a new terminal", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t);
  t.mock.method(claudeSessions, "findClaudeSessionForProcess", async () => ({
    sessionId: "abc-123",
    cwd,
  }));

  const result = await restartCliSessions(
    [{ providerId: "claude", pid: 601, startTime: null, cwd, terminal: shell("cmd.exe") }],
    resumeCommandFor({ id: "claude" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 1, manual: 0, failed: 0 });
  assert.deepEqual(calls.launches[0].args, ["-d", cwd, "claude", "--resume", "abc-123"]);
});

test("an elevated shell that survives taskkill still gets a new terminal", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t, { unkillable: [51] });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 501, startTime: null, cwd, terminal: shell("powershell.exe") }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 0, manual: 0, failed: 0 });
  assert.equal(calls.launches.length, 1);
});

test("a terminal emulator host is never closed, only the new terminal opens", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t);
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 501, startTime: null, cwd, terminal: shell("WindowsTerminal.exe") }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 0, manual: 0, failed: 0 });
  assert.deepEqual(calls.killed, [501]); // the emulator may own other tabs
});

test("a session without a terminal ancestor still reopens in a new terminal", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t);
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 501, startTime: null, cwd, terminal: null }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 0, manual: 0, failed: 0 });
  assert.deepEqual(calls.killed, [501]);
  assert.equal(calls.launches.length, 1);
});

test("an orca session reopens as an orca tab and never opens a desktop terminal", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t, {
    hasOrca: true,
    orcaTerminals: [{ worktreeId: "wt-1", worktreePath: cwd.replace(/\\/g, "/") }],
  });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [
      {
        providerId: "codex",
        pid: 501,
        startTime: null,
        cwd,
        terminal: shell("powershell.exe", { isOrcaHosted: true }),
      },
    ],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 1, manual: 0, failed: 0 });
  assert.equal(calls.orcaCreates.length, 1);
  assert.equal(calls.orcaCreates[0].worktree, "id:wt-1");
  assert.equal(calls.orcaCreates[0].command, "codex resume");
  assert.equal(calls.launches.length, 0);
});

test("an orca session that cannot get an orca tab fails instead of opening a window", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t, {
    hasOrca: true,
    orcaCreateFails: true,
    orcaTerminals: [{ worktreeId: "wt-1", worktreePath: cwd }],
  });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [
      {
        providerId: "codex",
        pid: 501,
        startTime: null,
        cwd,
        terminal: shell("powershell.exe", { isOrcaHosted: true }),
      },
    ],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 0, closed: 1, manual: 0, failed: 1 });
  assert.equal(calls.launches.length, 0); // never spills a desktop console
});

test("an orca session without a resolvable worktree fails instead of opening a window", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t, {
    hasOrca: true,
    orcaTerminals: [{ worktreeId: "wt-other", worktreePath: "D:\\elsewhere" }],
  });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [
      {
        providerId: "codex",
        pid: 501,
        startTime: null,
        cwd,
        terminal: shell("powershell.exe", { isOrcaHosted: true }),
      },
    ],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 0, closed: 1, manual: 0, failed: 1 });
  assert.equal(calls.orcaCreates.length, 0);
  assert.equal(calls.launches.length, 0);
});

test("orca is never consulted when no session lives in an orca tab", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t, { hasOrca: true, orcaTerminals: [{ worktreeId: "wt-1", worktreePath: cwd }] });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 501, startTime: null, cwd, terminal: shell("powershell.exe") }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 1, manual: 0, failed: 0 });
  assert.equal(calls.orcaCreates.length, 0);
  assert.equal(calls.launches.length, 1);
});

test("a claude session without a working directory needs a manual resume", async (t) => {
  const calls = stubRestart(t);
  t.mock.method(claudeSessions, "findClaudeSessionForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "claude", pid: 601, startTime: null, cwd: null, terminal: shell("powershell.exe") }],
    resumeCommandFor({ id: "claude" })
  );

  assert.deepEqual(result, { restarted: 0, closed: 1, manual: 1, failed: 0 });
  assert.equal(calls.launches.length, 0);
});

test("a codex session without a working directory reopens from the home directory", async (t) => {
  const calls = stubRestart(t);
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 501, startTime: null, cwd: null, terminal: shell("powershell.exe") }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 1, manual: 0, failed: 0 });
  assert.deepEqual(calls.launches[0].args, ["-d", os.homedir(), "codex", "resume"]);
});

test("a failed Windows Terminal launch falls back to a PowerShell window", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = stubRestart(t, { hasWt: false });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 501, startTime: null, cwd, terminal: shell("powershell.exe") }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, closed: 1, manual: 0, failed: 0 });
  assert.equal(path.basename(calls.launches[0].command).toLowerCase(), "cmd.exe");
});
