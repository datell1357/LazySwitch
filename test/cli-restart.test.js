const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const claudeSessions = require("../dist/main/claude-sessions.js");
const codexRollouts = require("../dist/main/codex-rollouts.js");
const {
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

  assert.deepEqual(result, { restarted: 3, manual: 0, failed: 0 });
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

  assert.deepEqual(result, { restarted: 2, manual: 1, failed: 0 });
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

  assert.deepEqual(result, { restarted: 1, manual: 0, failed: 0 });
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
