const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const { findCodexRolloutForProcess } = require("../dist/main/codex-rollouts.js");

function writeRollout(root, rollout) {
  const file = path.join(root, "2026", "07", "04", rollout.name);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    `${JSON.stringify({
      type: "session_meta",
      payload: {
        session_id: rollout.sessionId,
        id: rollout.sessionId,
        cwd: rollout.cwd,
      },
    })}\n`,
    "utf8"
  );
  fs.utimesSync(file, rollout.mtime, rollout.mtime);
  return file;
}

test("findCodexRolloutForProcess returns newest active rollout for matching cwd", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "codex-rollouts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const cwd = "D:\\Vibe Project\\My Project\\LazySwitch";
  const startTime = "2026-07-04T01:00:00.000+09:00";

  writeRollout(root, {
    name: "rollout-old.jsonl",
    sessionId: "old-session",
    cwd,
    mtime: new Date("2026-07-03T16:00:05.000Z"),
  });
  writeRollout(root, {
    name: "rollout-new.jsonl",
    sessionId: "new-session",
    cwd,
    mtime: new Date("2026-07-03T16:00:10.000Z"),
  });
  writeRollout(root, {
    name: "rollout-foreign.jsonl",
    sessionId: "foreign-session",
    cwd: "C:\\Users\\datell1357",
    mtime: new Date("2026-07-03T16:00:20.000Z"),
  });
  writeRollout(root, {
    name: "rollout-before-start.jsonl",
    sessionId: "stale-session",
    cwd,
    mtime: new Date("2026-07-03T15:59:59.000Z"),
  });

  const match = await findCodexRolloutForProcess({ cwd, startTime }, root);

  assert.equal(match?.sessionId, "new-session");
});

test("findCodexRolloutForProcess returns null without a cwd match", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "codex-rollouts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  writeRollout(root, {
    name: "rollout-home.jsonl",
    sessionId: "home-session",
    cwd: "C:\\Users\\datell1357",
    mtime: new Date(),
  });

  const match = await findCodexRolloutForProcess({
    cwd: "D:\\Vibe Project\\My Project\\LazySwitch",
    startTime: "2026-07-04T01:00:00.000+09:00",
  }, root);

  assert.equal(match, null);
});

test("findCodexRolloutForProcess matches cwd-less sessions by filename creation time", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "codex-rollouts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const startedAt = new Date(2026, 6, 10, 9, 58, 15);
  const activeMtime = new Date(startedAt.getTime() + 60_000);
  writeRollout(root, {
    name: "rollout-2026-07-10T08-00-00-00000000-0000-0000-0000-000000000001.jsonl",
    sessionId: "desktop-session",
    cwd: "C:\\Users\\datell1357",
    mtime: activeMtime,
  });
  const expectedFile = writeRollout(root, {
    name: "rollout-2026-07-10T09-58-12-00000000-0000-0000-0000-000000000002.jsonl",
    sessionId: "cli-session",
    cwd: "\\\\?\\D:\\Vibe Project\\My Project\\LazySwitch",
    mtime: activeMtime,
  });

  const match = await findCodexRolloutForProcess({
    cwd: null,
    startTime: startedAt.toISOString(),
  }, root);

  assert.deepEqual(match, {
    sessionId: "cli-session",
    cwd: "\\\\?\\D:\\Vibe Project\\My Project\\LazySwitch",
    file: expectedFile,
    mtimeMs: activeMtime.getTime(),
  });
});

test("findCodexRolloutForProcess uses the unique active rollout as a fallback", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "codex-rollouts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const startedAt = new Date(2020, 0, 1, 0, 0, 0);
  writeRollout(root, {
    name: "rollout-without-timestamp.jsonl",
    sessionId: "only-active-session",
    cwd: "D:\\Project",
    mtime: new Date(startedAt.getTime() + 60_000),
  });

  const match = await findCodexRolloutForProcess({
    cwd: null,
    startTime: startedAt.toISOString(),
  }, root);

  assert.equal(match?.sessionId, "only-active-session");
});

test("findCodexRolloutForProcess returns null for ambiguous active rollouts", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "codex-rollouts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const startedAt = new Date(2020, 0, 1, 0, 0, 0);
  writeRollout(root, {
    name: "rollout-ambiguous-one.jsonl",
    sessionId: "ambiguous-one",
    cwd: "D:\\One",
    mtime: new Date(startedAt.getTime() + 60_000),
  });
  writeRollout(root, {
    name: "rollout-ambiguous-two.jsonl",
    sessionId: "ambiguous-two",
    cwd: "D:\\Two",
    mtime: new Date(startedAt.getTime() + 120_000),
  });

  const match = await findCodexRolloutForProcess({
    cwd: null,
    startTime: startedAt.toISOString(),
  }, root);

  assert.equal(match, null);
});

test("findCodexRolloutForProcess excludes claimed session ids", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "codex-rollouts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const cwd = "D:\\Project";
  const startedAt = new Date("2026-07-10T09:00:00.000Z");
  writeRollout(root, {
    name: "rollout-older.jsonl",
    sessionId: "older-session",
    cwd,
    mtime: new Date(startedAt.getTime() + 10_000),
  });
  writeRollout(root, {
    name: "rollout-newer.jsonl",
    sessionId: "newer-session",
    cwd,
    mtime: new Date(startedAt.getTime() + 20_000),
  });

  const match = await findCodexRolloutForProcess(
    { cwd, startTime: startedAt.toISOString() },
    root,
    new Set(["newer-session"])
  );

  assert.equal(match?.sessionId, "older-session");
});

test("findCodexRolloutForProcess rejects unsafe session ids", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "codex-rollouts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  writeRollout(root, {
    name: "rollout-unsafe.jsonl",
    sessionId: "unsafe; Start-Process calc",
    cwd: "D:\\Project",
    mtime: new Date(),
  });

  const match = await findCodexRolloutForProcess({
    cwd: "D:\\Project",
    startTime: null,
  }, root);

  assert.equal(match, null);
});
