const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const electronPath = require.resolve("electron");
require.cache[electronPath] = {
  id: electronPath,
  filename: electronPath,
  loaded: true,
  exports: { shell: { openExternal: () => Promise.resolve() } },
};

const providerPath = require.resolve("../dist/main/providers/claude.js");

async function fetchUsageWith(body) {
  const configDir = fs.mkdtempSync(path.join(os.tmpdir(), "lazyswitch-claude-"));
  const oldConfigDir = process.env.CLAUDE_CONFIG_DIR;
  const oldFetch = global.fetch;
  delete require.cache[providerPath];
  process.env.CLAUDE_CONFIG_DIR = configDir;
  fs.writeFileSync(
    path.join(configDir, ".credentials.json"),
    JSON.stringify({
      claudeAiOauth: {
        accessToken: "token",
        refreshToken: "refresh",
        expiresAt: Date.now() + 600_000,
        subscriptionType: "pro",
      },
    }),
  );
  global.fetch = async (url) => {
    assert.equal(url, "https://api.anthropic.com/api/oauth/usage");
    return {
      status: 200,
      ok: true,
      headers: { get: () => null },
      json: async () => body,
    };
  };

  try {
    const { claudeProvider } = require(providerPath);
    return await claudeProvider.fetchUsage(null);
  } finally {
    delete require.cache[providerPath];
    fs.rmSync(configDir, { recursive: true, force: true });
    if (oldConfigDir === undefined) delete process.env.CLAUDE_CONFIG_DIR;
    else process.env.CLAUDE_CONFIG_DIR = oldConfigDir;
    if (oldFetch === undefined) delete global.fetch;
    else global.fetch = oldFetch;
  }
}

test("Claude usage falls back to limits and reads Fable scoped weekly limit", async () => {
  const usage = await fetchUsageWith({
    five_hour: null,
    seven_day: null,
    seven_day_omelette: {
      percent: 99,
      resets_at: "2026-07-15T20:00:00+00:00",
    },
    limits: [
      {
        kind: "session",
        percent: 9,
        resets_at: "2026-07-10T05:50:00+00:00",
      },
      {
        kind: "weekly_all",
        percent: 1,
        resets_at: "2026-07-13T20:00:00+00:00",
      },
      {
        kind: "weekly_scoped",
        percent: 2,
        resets_at: "2026-07-13T20:00:00+00:00",
        scope: { model: { display_name: "FABLE" } },
      },
    ],
  });

  assert.equal(usage.primary.usedPercent, 9);
  assert.equal(usage.primary.windowMinutes, 300);
  assert.equal(usage.secondary.usedPercent, 1);
  assert.equal(usage.secondary.windowMinutes, 10080);
  assert.equal(usage.fable.usedPercent, 2);
  assert.equal(usage.fable.windowMinutes, 10080);
  assert.equal(usage.fable.resetsAt, Date.parse("2026-07-13T20:00:00+00:00"));
});

test("Claude usage keeps seven_day_omelette fallback for older responses", async () => {
  const usage = await fetchUsageWith({
    seven_day_omelette: {
      percent: 3,
      resets_at: "2026-07-14T20:00:00+00:00",
    },
  });

  assert.equal(usage.fable.usedPercent, 3);
  assert.equal(usage.fable.windowMinutes, 10080);
});
