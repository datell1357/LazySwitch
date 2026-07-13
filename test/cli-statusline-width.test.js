const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

function runStatusline(home, input, columns, provider = "claude") {
  const result = spawnSync(process.execPath, [path.join(__dirname, "..", "dist", "main", "cli.js"), "statusline", provider], {
    env: {
      ...process.env,
      APPDATA: path.join(home, "AppData", "Roaming"),
      COLUMNS: String(columns),
      HOME: home,
      NO_COLOR: "1",
      USERPROFILE: home,
    },
    input,
    encoding: "utf8",
    timeout: 1000,
  });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function seedCache(home, width, provider = "claude") {
  const cacheDir = path.join(home, ".lazyswitch");
  fs.mkdirSync(cacheDir, { recursive: true });
  fs.writeFileSync(
    path.join(cacheDir, `statusline-cache-${provider}-${width}.json`),
    JSON.stringify({ at: Date.now(), text: `cached-width-${width}`, mode: "plain", version: 11, width }),
    "utf8"
  );
}

test("statusline stdin width takes priority and remains separated in the cache", () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "lazyswitch-statusline-"));
  try {
    for (const width of [44, 55, 80]) seedCache(home, width);
    assert.equal(runStatusline(home, JSON.stringify({ width: 44 }), 80), "cached-width-44");
    assert.equal(runStatusline(home, JSON.stringify({ terminal: { cols: 55 } }), 80), "cached-width-55");
    assert.equal(runStatusline(home, JSON.stringify({ workspace: { dimensions: { columns: 80 } } }), 44), "cached-width-80");
    assert.equal(runStatusline(home, JSON.stringify({ dimensions: { columns: 44 } }), 80), "cached-width-44");
    assert.equal(runStatusline(home, "", 55), "cached-width-55");
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test("Codex statusline uses an isolated home fixture and stays bounded", () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "lazyswitch-codex-statusline-"));
  try {
    seedCache(home, 44, "codex");
    const output = runStatusline(home, JSON.stringify({ width: 44 }), 80, "codex");
    assert.equal(output, "cached-width-44");
    assert.equal(output.split("\n").length, 1);
    assert.ok(output.length <= 44);
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});
