const assert = require("node:assert/strict");
const test = require("node:test");

const electronPath = require.resolve("electron");
require.cache[electronPath] = {
  id: electronPath,
  filename: electronPath,
  loaded: true,
  exports: {
    clipboard: { writeText: () => undefined },
    ipcMain: {
      handle: () => undefined,
      on: () => undefined,
      removeListener: () => undefined,
    },
  },
};

const cliSessions = require("../dist/main/cli-sessions.js");
const { createCliHandover } = require("../dist/main/cli-handover.js");

test("schedule uses a pre-captured CLI session snapshot", async () => {
  let detectCalls = 0;
  let restartedSessions = null;
  cliSessions.detectCliSessions = async () => {
    detectCalls += 1;
    return [];
  };
  cliSessions.restartCliSessions = async (sessions) => {
    restartedSessions = sessions;
    return { restarted: sessions.length, resumedInPlace: 0, manual: 0, failed: 0 };
  };

  const provider = { id: "codex", displayName: "Codex" };
  const sessions = [
    {
      providerId: "codex",
      pid: 1234,
      startTime: "2026-07-04T01:00:00.000+09:00",
      cwd: "D:\\Vibe Project\\My Project\\LazySwitch",
    },
  ];
  const handover = createCliHandover({
    getLang: () => "en",
    getPrefs: () => ({ autoRestartCli: true }),
    notify: () => undefined,
    t: (key) => key,
  });

  const result = await handover.schedule(provider, sessions);

  assert.equal(detectCalls, 0);
  assert.deepEqual(restartedSessions, sessions);
  assert.deepEqual(result, { restarted: 1, resumedInPlace: 0, manual: 0, failed: 0 });
});
