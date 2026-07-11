const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const claudeSessions = require("../dist/main/claude-sessions.js");
const codexRollouts = require("../dist/main/codex-rollouts.js");
const {
  formatResumeCommandForShell,
  pairOrcaTerminalHandles,
  restartCliSessions,
  resumeCommandFor,
} = require("../dist/main/cli-sessions.js");

function mockRestartRuntime(t, hasWt = true) {
  const launches = [];
  t.mock.method(
    childProcess,
    "execFile",
    (file, _args, _options, callback) => {
      if (path.basename(file).toLowerCase() === "where.exe" && !hasWt) {
        callback(new Error("not found"), "", "");
        return;
      }
      callback(null, "", "");
    }
  );
  t.mock.method(childProcess, "spawn", (command, args, options) => {
    launches.push({ command, args, options });
    return {
      on() {
        return this;
      },
      unref() {},
    };
  });
  t.mock.method(process, "kill", () => {
    throw new Error("not running");
  });
  t.mock.method(global, "setTimeout", (callback) => {
    callback();
    return 0;
  });
  return launches;
}

test("restartCliSessions resumes distinct Codex sessions in matched rollout directories", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const firstCwd = path.join(root, "first");
  const secondCwd = path.join(root, "second");
  fs.mkdirSync(firstCwd);
  fs.mkdirSync(secondCwd);
  const launches = mockRestartRuntime(t);
  const ids = ["first-session", "second-session", "missing-cwd-session"];
  const matchedCwds = [firstCwd, secondCwd, path.join(root, "missing")];
  let call = 0;
  t.mock.method(
    codexRollouts,
    "findCodexRolloutForProcess",
    async (_session, _root, claimed) => {
      assert.deepEqual([...claimed], ids.slice(0, call));
      const index = call++;
      return {
        sessionId: ids[index],
        cwd: "\\\\?\\" + matchedCwds[index],
        file: path.join(root, `${ids[index]}.jsonl`),
        mtimeMs: index,
      };
    }
  );

  const result = await restartCliSessions(
    [
      { providerId: "codex", pid: 101, startTime: null, cwd: null },
      { providerId: "codex", pid: 102, startTime: null, cwd: null },
      { providerId: "codex", pid: 103, startTime: null, cwd: null },
    ],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 3, resumedInPlace: 0, manual: 0, failed: 0 });
  assert.deepEqual(launches.map(({ command, args }) => ({ command, args })), [
    {
      command: "wt.exe",
      args: ["-d", firstCwd, "codex", "resume", "first-session"],
    },
    {
      command: "wt.exe",
      args: ["-d", secondCwd, "codex", "resume", "second-session"],
    },
    {
      command: "wt.exe",
      args: ["-d", os.homedir(), "codex", "resume", "missing-cwd-session"],
    },
  ]);
});

test("restartCliSessions resumes Claude by id and preserves fallbacks", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const exactCwd = path.join(root, "exact");
  const fallbackCwd = path.join(root, "fallback");
  fs.mkdirSync(exactCwd);
  fs.mkdirSync(fallbackCwd);
  const launches = mockRestartRuntime(t);
  const matches = [
    {
      sessionId: "exact-session",
      cwd: "\\\\?\\" + exactCwd,
      file: path.join(root, "exact.jsonl"),
      mtimeMs: 1,
    },
    {
      sessionId: "missing-session",
      cwd: path.join(root, "missing"),
      file: path.join(root, "missing.jsonl"),
      mtimeMs: 2,
    },
    null,
  ];
  let call = 0;
  t.mock.method(
    claudeSessions,
    "findClaudeSessionForProcess",
    async (_session, _root, claimed) => {
      assert.equal(claimed.size, call);
      return matches[call++];
    }
  );

  const result = await restartCliSessions(
    [
      { providerId: "claude", pid: 201, startTime: null, cwd: null },
      { providerId: "claude", pid: 202, startTime: null, cwd: fallbackCwd },
      { providerId: "claude", pid: 203, startTime: null, cwd: null },
    ],
    resumeCommandFor({ id: "claude" })
  );

  assert.deepEqual(result, { restarted: 2, resumedInPlace: 0, manual: 1, failed: 0 });
  assert.deepEqual(launches.map(({ command, args }) => ({ command, args })), [
    {
      command: "wt.exe",
      args: ["-d", exactCwd, "claude", "--resume", "exact-session"],
    },
    {
      command: "wt.exe",
      args: ["-d", fallbackCwd, "claude", "--continue"],
    },
  ]);
});

test("restartCliSessions keeps metadata out of the PowerShell command text", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const launches = mockRestartRuntime(t, false);
  t.mock.method(
    codexRollouts,
    "findCodexRolloutForProcess",
    async () => ({
      sessionId: "safe-session-id",
      cwd,
      file: path.join(cwd, "rollout.jsonl"),
      mtimeMs: 1,
    })
  );

  const result = await restartCliSessions(
    [{ providerId: "codex", pid: 301, startTime: null, cwd }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, resumedInPlace: 0, manual: 0, failed: 0 });
  assert.equal(path.isAbsolute(launches[0]?.command), true);
  assert.equal(path.basename(launches[0]?.command).toLowerCase(), "cmd.exe");
  assert.deepEqual(launches[0]?.args.slice(0, 4), [
    "/d",
    "/c",
    "start",
    "",
  ]);
  assert.equal(path.isAbsolute(launches[0]?.args[4]), true);
  assert.equal(
    path.basename(launches[0]?.args[4]).toLowerCase(),
    "powershell.exe"
  );
  assert.deepEqual(launches[0]?.args.slice(5, 7), [
    "-NoExit",
    "-EncodedCommand",
  ]);
  assert.equal(launches[0]?.args.includes("safe-session-id"), false);
  assert.equal(launches[0]?.args.includes(cwd), false);
  assert.equal(launches[0]?.options.cwd, cwd);
  assert.equal(
    launches[0]?.options.env.LAZYSWITCH_CLI_COMMAND,
    "codex"
  );
  assert.deepEqual(
    JSON.parse(launches[0]?.options.env.LAZYSWITCH_CLI_ARGS),
    ["resume", "safe-session-id"]
  );
});

test("in-place resume only pairs an unambiguous Orca terminal and quotes for each shell", () => {
  const first = {
    providerId: "codex",
    pid: 1,
    startTime: null,
    cwd: "D:\\Work\\Same",
    terminal: { pid: 11, name: "powershell.exe", isOrcaHosted: true },
  };
  const second = {
    ...first,
    pid: 2,
    terminal: { pid: 12, name: "cmd.exe", isOrcaHosted: true },
  };
  const ambiguousHandles = pairOrcaTerminalHandles(
    [first, second],
    [
      { handle: "first", worktreePath: "d:/work/same" },
      { handle: "second", worktreePath: "D:/Work/Same" },
    ]
  );

  assert.deepEqual([...ambiguousHandles.entries()], []);
  const unambiguousHandles = pairOrcaTerminalHandles(
    [first],
    [{ handle: "first", worktreePath: "d:/work/same" }]
  );
  assert.deepEqual([...unambiguousHandles.entries()], [[1, "first"]]);
  const sharedPaneHandles = pairOrcaTerminalHandles(
    [first, second],
    [{ handle: "first", worktreePath: "d:/work/same" }]
  );
  assert.deepEqual([...sharedPaneHandles.entries()], []);
  assert.equal(
    formatResumeCommandForShell(
      "D:\\O'Brien & Work",
      { text: "codex resume abc-123", command: "codex", args: ["resume", "abc-123"] },
      "powershell.exe"
    ),
    "cd 'D:\\O''Brien & Work'; codex resume abc-123"
  );
  assert.equal(
    formatResumeCommandForShell(
      "D:\\Work & Notes",
      { text: "claude --continue", command: "claude", args: ["--continue"] },
      "cmd.exe"
    ),
    'cd /d "D:\\Work & Notes" && claude --continue'
  );
  assert.equal(
    formatResumeCommandForShell(
      "D:\\O'Brien & Work",
      { text: "codex resume abc-123", command: "codex", args: ["resume", "abc-123"] },
      "bash.exe"
    ),
    "cd -- 'D:\\O'\\''Brien & Work' && codex resume abc-123"
  );
});

test("restartCliSessions tries console injection before an unambiguous Orca fallback", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const orcaCwd = path.join(root, "orca");
  const fallbackCwd = path.join(root, "fallback");
  fs.mkdirSync(orcaCwd);
  fs.mkdirSync(fallbackCwd);
  const launches = [];
  const calls = [];
  t.mock.method(childProcess, "execFile", (file, args, _options, callback) => {
    calls.push({ file, args });
    if (path.basename(file).toLowerCase() === "where.exe") {
      callback(null, "", "");
      return;
    }
    if (file === "orca" && args[1] === "list") {
      callback(null, JSON.stringify({ ok: true, result: { terminals: [{ handle: "orca-pane", worktreePath: orcaCwd }] } }), "");
      return;
    }
    if (file === "orca" && args[1] === "send") {
      callback(null, JSON.stringify({ ok: true }), "");
      return;
    }
    if (path.basename(file).toLowerCase() === "powershell.exe") {
      callback(null, JSON.stringify({ ok: false }), "");
      return;
    }
    callback(null, "", "");
  });
  t.mock.method(childProcess, "spawn", (command, args, options) => {
    launches.push({ command, args, options });
    return { on() { return this; }, unref() {} };
  });
  t.mock.method(process, "kill", () => { throw new Error("not running"); });
  t.mock.method(global, "setTimeout", (callback) => { callback(); return 0; });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [
      {
        providerId: "codex", pid: 401, startTime: null, cwd: orcaCwd,
        terminal: { pid: 41, name: "powershell.exe", isOrcaHosted: true },
      },
      { providerId: "codex", pid: 402, startTime: null, cwd: fallbackCwd, terminal: null },
    ],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, resumedInPlace: 1, manual: 0, failed: 0 });
  const helperIndex = calls.findIndex(({ file }) => path.basename(file).toLowerCase() === "powershell.exe");
  const sendIndex = calls.findIndex(({ file, args }) => file === "orca" && args[1] === "send");
  assert.equal(helperIndex >= 0, true);
  assert.equal(sendIndex > helperIndex, true);
  const helper = calls[helperIndex];
  const injectionScript = Buffer.from(helper.args.at(-1), "base64").toString("utf16le");
  assert.match(injectionScript, /AttachConsole/);
  assert.match(injectionScript, /\$consoleInput/);
  assert.doesNotMatch(injectionScript, /\$input\s*=/);
  assert.match(injectionScript, /\$ProgressPreference = 'SilentlyContinue'/);
  assert.match(injectionScript, /exit 0/);
  assert.match(injectionScript, /exit 1/);
  assert.match(injectionScript, /\[uint32\]3221225472/);
  assert.doesNotMatch(injectionScript, /\[uint32\]0xC0000000/);
  assert.deepEqual(launches.map(({ command, args }) => ({ command, args })), [
    { command: "wt.exe", args: ["-d", fallbackCwd, "codex", "resume"] },
  ]);
});

test("restartCliSessions never sends through Orca when its worktree has multiple panes", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const calls = [];
  const launches = [];
  t.mock.method(childProcess, "execFile", (file, args, _options, callback) => {
    calls.push({ file, args });
    if (path.basename(file).toLowerCase() === "where.exe") {
      callback(null, "", "");
      return;
    }
    if (file === "orca" && args[1] === "list") {
      callback(null, JSON.stringify({ ok: true, result: { terminals: [
        { handle: "claude-pane", worktreePath: cwd },
        { handle: "codex-pane", worktreePath: cwd },
      ] } }), "");
      return;
    }
    if (path.basename(file).toLowerCase() === "powershell.exe") {
      callback(null, JSON.stringify({ ok: false }), "");
      return;
    }
    callback(null, "", "");
  });
  t.mock.method(childProcess, "spawn", (command, args, options) => {
    launches.push({ command, args, options });
    return { on() { return this; }, unref() {} };
  });
  t.mock.method(process, "kill", () => { throw new Error("not running"); });
  t.mock.method(global, "setTimeout", (callback) => { callback(); return 0; });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{
      providerId: "codex", pid: 501, startTime: null, cwd,
      terminal: { pid: 51, name: "powershell.exe", isOrcaHosted: true },
    }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, resumedInPlace: 0, manual: 0, failed: 0 });
  assert.equal(
    calls.some(({ file, args }) => file === "orca" && args[1] === "send"),
    false
  );
  assert.equal(
    calls.some(({ file }) => path.basename(file).toLowerCase() === "powershell.exe"),
    true
  );
  assert.equal(launches.length, 1);
});

test("restartCliSessions keeps a successful console injection in place when the helper exits non-zero", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const launches = [];
  t.mock.method(childProcess, "execFile", (file, _args, _options, callback) => {
    if (path.basename(file).toLowerCase() === "where.exe") {
      callback(null, "", "");
      return;
    }
    if (path.basename(file).toLowerCase() === "powershell.exe") {
      callback(new Error("helper exited non-zero"), JSON.stringify({ ok: true }), "#< CLIXML");
      return;
    }
    callback(null, "", "");
  });
  t.mock.method(childProcess, "spawn", (command, args, options) => {
    launches.push({ command, args, options });
    return { on() { return this; }, unref() {} };
  });
  t.mock.method(process, "kill", () => { throw new Error("not running"); });
  t.mock.method(global, "setTimeout", (callback) => { callback(); return 0; });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [{
      providerId: "codex",
      pid: 601,
      startTime: null,
      cwd,
      terminal: { pid: 61, name: "powershell.exe", isOrcaHosted: false },
    }],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 0, resumedInPlace: 1, manual: 0, failed: 0 });
  assert.equal(launches.length, 0);
});

test("restartCliSessions injects a cwd-less Claude session before manual fallback", async (t) => {
  const calls = [];
  const launches = [];
  t.mock.method(childProcess, "execFile", (file, args, _options, callback) => {
    calls.push({ file, args });
    if (path.basename(file).toLowerCase() === "where.exe") {
      callback(null, "", "");
      return;
    }
    if (path.basename(file).toLowerCase() === "powershell.exe") {
      callback(null, JSON.stringify({ ok: true }), "");
      return;
    }
    callback(null, "", "");
  });
  t.mock.method(childProcess, "spawn", (command, args, options) => {
    launches.push({ command, args, options });
    return { on() { return this; }, unref() {} };
  });
  t.mock.method(process, "kill", () => { throw new Error("not running"); });
  t.mock.method(global, "setTimeout", (callback) => { callback(); return 0; });
  t.mock.method(claudeSessions, "findClaudeSessionForProcess", async () => null);

  const result = await restartCliSessions(
    [{
      providerId: "claude",
      pid: 701,
      startTime: null,
      cwd: null,
      terminal: { pid: 71, name: "powershell.exe", isOrcaHosted: false },
    }],
    resumeCommandFor({ id: "claude" })
  );

  assert.deepEqual(result, { restarted: 0, resumedInPlace: 1, manual: 0, failed: 0 });
  const helper = calls.find(({ file }) => path.basename(file).toLowerCase() === "powershell.exe");
  const injectionScript = Buffer.from(helper.args.at(-1), "base64").toString("utf16le");
  const encodedText = injectionScript.match(/FromBase64String\('([^']+)'\)/)?.[1];
  assert.equal(Buffer.from(encodedText, "base64").toString("utf16le"), "claude --continue\r");
  assert.equal(launches.length, 0);
});

test("restartCliSessions opens a new window without injecting when termination fails", async (t) => {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "cli-restart-"));
  t.after(() => fs.rmSync(cwd, { recursive: true, force: true }));
  const launches = [];
  const calls = [];
  t.mock.method(childProcess, "execFile", (file, args, _options, callback) => {
    calls.push({ file, args });
    callback(null, "", "");
  });
  t.mock.method(childProcess, "spawn", (command, args, options) => {
    launches.push({ command, args, options });
    return { on() { return this; }, unref() {} };
  });
  t.mock.method(process, "kill", () => undefined);
  t.mock.method(global, "setTimeout", (callback) => { callback(); return 0; });
  t.mock.method(codexRollouts, "findCodexRolloutForProcess", async () => null);

  const result = await restartCliSessions(
    [
      {
        providerId: "codex", pid: 501, startTime: null, cwd,
        terminal: { pid: 51, name: "powershell.exe", isOrcaHosted: false },
      },
    ],
    resumeCommandFor({ id: "codex" })
  );

  assert.deepEqual(result, { restarted: 1, resumedInPlace: 0, manual: 0, failed: 1 });
  assert.equal(calls.some(({ file }) => path.basename(file).toLowerCase() === "powershell.exe"), false);
  assert.equal(launches.length, 1);
});
