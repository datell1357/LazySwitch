const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const { findClaudeSessionForProcess } = require("../dist/main/claude-sessions.js");

function writeClaudeSession(root, session) {
  const file = path.join(root, session.project, `${session.fileSessionId}.jsonl`);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const firstLine =
    session.lineSessionId === null ? {} : { sessionId: session.lineSessionId };
  const cwdLine = {
    cwd: session.cwd,
    ...(session.lineSessionId === null
      ? {}
      : { sessionId: session.lineSessionId }),
  };
  fs.writeFileSync(
    file,
    `${JSON.stringify(firstLine)}\n${JSON.stringify(cwdLine)}\n`,
    "utf8"
  );
  fs.utimesSync(file, session.mtime, session.mtime);
  return file;
}

test("findClaudeSessionForProcess returns the newest active session for matching cwd", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "claude-sessions-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const startedAt = new Date("2026-07-10T09:00:00.000Z");
  writeClaudeSession(root, {
    project: "D--Vibe-Project-LazySwitch",
    fileSessionId: "older-file-id",
    lineSessionId: "older-line-id",
    cwd: "D:\\Vibe Project\\LazySwitch",
    mtime: new Date(startedAt.getTime() + 10_000),
  });
  const expectedFile = writeClaudeSession(root, {
    project: "D--Vibe-Project-LazySwitch",
    fileSessionId: "newer-file-id",
    lineSessionId: "newer-line-id",
    cwd: "d:\\vibe project\\lazyswitch\\",
    mtime: new Date(startedAt.getTime() + 20_000),
  });
  writeClaudeSession(root, {
    project: "C--Users-datell1357",
    fileSessionId: "foreign-file-id",
    lineSessionId: "foreign-line-id",
    cwd: "C:\\Users\\datell1357",
    mtime: new Date(startedAt.getTime() + 30_000),
  });
  writeClaudeSession(root, {
    project: path.join("D--Vibe-Project-LazySwitch", "subagents"),
    fileSessionId: "nested-agent-file",
    lineSessionId: "nested-agent-session",
    cwd: "D:\\Vibe Project\\LazySwitch",
    mtime: new Date(startedAt.getTime() + 40_000),
  });

  const match = await findClaudeSessionForProcess({
    cwd: "\\\\?\\D:\\Vibe Project\\LazySwitch\\",
    startTime: startedAt.toISOString(),
  }, root);

  assert.deepEqual(match, {
    sessionId: "newer-line-id",
    cwd: "d:\\vibe project\\lazyswitch\\",
    file: expectedFile,
    mtimeMs: startedAt.getTime() + 20_000,
  });
});

test("findClaudeSessionForProcess matches cwd-less sessions by birth time", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "claude-sessions-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const initialMtime = new Date();
  const decoyFile = writeClaudeSession(root, {
    project: "C--Users-datell1357",
    fileSessionId: "desktop-decoy",
    lineSessionId: "desktop-decoy",
    cwd: "C:\\Users\\datell1357",
    mtime: initialMtime,
  });
  const expectedFile = writeClaudeSession(root, {
    project: "D--Vibe-Project-LazySwitch",
    fileSessionId: "target-file-id",
    lineSessionId: "target-session-id",
    cwd: "D:\\Vibe Project\\LazySwitch",
    mtime: initialMtime,
  });
  const startedAtMs = Date.parse(
    new Date(fs.statSync(expectedFile).birthtimeMs).toISOString()
  );
  fs.utimesSync(decoyFile, new Date(startedAtMs), new Date(startedAtMs + 10_000));
  fs.utimesSync(expectedFile, new Date(startedAtMs), new Date(startedAtMs + 20_000));
  const decoyDelta = Math.abs(fs.statSync(decoyFile).birthtimeMs - startedAtMs);
  const targetDelta = Math.abs(fs.statSync(expectedFile).birthtimeMs - startedAtMs);
  const targetIsClosest = targetDelta < decoyDelta || targetDelta === decoyDelta;

  const match = await findClaudeSessionForProcess({
    cwd: null,
    startTime: new Date(startedAtMs).toISOString(),
  }, root);

  assert.equal(
    match?.sessionId,
    targetIsClosest ? "target-session-id" : "desktop-decoy"
  );
  assert.equal(match?.file, targetIsClosest ? expectedFile : decoyFile);
});

test("findClaudeSessionForProcess uses the unique active session as a fallback", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "claude-sessions-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const startedAt = new Date("2020-01-01T00:00:00.000Z");
  writeClaudeSession(root, {
    project: "D--Project",
    fileSessionId: "unique-file-id",
    lineSessionId: null,
    cwd: "D:\\Project",
    mtime: new Date(startedAt.getTime() + 60_000),
  });

  const match = await findClaudeSessionForProcess({
    cwd: null,
    startTime: startedAt.toISOString(),
  }, root);

  assert.equal(match?.sessionId, "unique-file-id");
});

test("findClaudeSessionForProcess returns null for ambiguous active sessions", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "claude-sessions-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const startedAt = new Date("2020-01-01T00:00:00.000Z");
  writeClaudeSession(root, {
    project: "D--One",
    fileSessionId: "ambiguous-one",
    lineSessionId: "ambiguous-one",
    cwd: "D:\\One",
    mtime: new Date(startedAt.getTime() + 60_000),
  });
  writeClaudeSession(root, {
    project: "D--Two",
    fileSessionId: "ambiguous-two",
    lineSessionId: "ambiguous-two",
    cwd: "D:\\Two",
    mtime: new Date(startedAt.getTime() + 120_000),
  });

  const match = await findClaudeSessionForProcess({
    cwd: null,
    startTime: startedAt.toISOString(),
  }, root);

  assert.equal(match, null);
});

test("findClaudeSessionForProcess uses newest cwd match when none are active", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "claude-sessions-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const cwd = "D:\\Project";
  writeClaudeSession(root, {
    project: "D--Project",
    fileSessionId: "older",
    lineSessionId: "older",
    cwd,
    mtime: new Date("2026-01-01T00:00:00.000Z"),
  });
  writeClaudeSession(root, {
    project: "D--Project",
    fileSessionId: "newer",
    lineSessionId: "newer",
    cwd,
    mtime: new Date("2026-01-02T00:00:00.000Z"),
  });

  const match = await findClaudeSessionForProcess({
    cwd,
    startTime: "2030-01-01T00:00:00.000Z",
  }, root);

  assert.equal(match?.sessionId, "newer");
});

test("findClaudeSessionForProcess respects CLAUDE_CONFIG_DIR", async (t) => {
  const configRoot = fs.mkdtempSync(path.join(os.tmpdir(), "claude-config-"));
  const previous = process.env.CLAUDE_CONFIG_DIR;
  process.env.CLAUDE_CONFIG_DIR = configRoot;
  t.after(() => {
    if (previous === undefined) delete process.env.CLAUDE_CONFIG_DIR;
    else process.env.CLAUDE_CONFIG_DIR = previous;
    fs.rmSync(configRoot, { recursive: true, force: true });
  });
  writeClaudeSession(path.join(configRoot, "projects"), {
    project: "D--Configured",
    fileSessionId: "configured-session",
    lineSessionId: "configured-session",
    cwd: "D:\\Configured",
    mtime: new Date(),
  });

  const match = await findClaudeSessionForProcess({
    cwd: "D:\\Configured",
    startTime: null,
  });

  assert.equal(match?.sessionId, "configured-session");
});

test("findClaudeSessionForProcess excludes claimed session ids", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "claude-sessions-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const cwd = "D:\\Project";
  writeClaudeSession(root, {
    project: "D--Project",
    fileSessionId: "older-session",
    lineSessionId: "older-session",
    cwd,
    mtime: new Date("2026-07-10T09:00:10.000Z"),
  });
  writeClaudeSession(root, {
    project: "D--Project",
    fileSessionId: "newer-session",
    lineSessionId: "newer-session",
    cwd,
    mtime: new Date("2026-07-10T09:00:20.000Z"),
  });

  const match = await findClaudeSessionForProcess(
    { cwd, startTime: "2026-07-10T09:00:00.000Z" },
    root,
    new Set(["newer-session"])
  );

  assert.equal(match?.sessionId, "older-session");
});

test("findClaudeSessionForProcess ignores unsafe line session ids", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "claude-sessions-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  writeClaudeSession(root, {
    project: "D--Project",
    fileSessionId: "safe-file-id",
    lineSessionId: "unsafe; Start-Process calc",
    cwd: "D:\\Project",
    mtime: new Date(),
  });

  const match = await findClaudeSessionForProcess({
    cwd: "D:\\Project",
    startTime: null,
  }, root);

  assert.equal(match?.sessionId, "safe-file-id");
});
