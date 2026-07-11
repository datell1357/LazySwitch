const assert = require("node:assert/strict");
const test = require("node:test");

const { readDetectorOutput } = require("../dist/main/cli-sessions.js");

const ROOT_PID = 999;

function detectorValue(targets, parents) {
  return { targets, parents };
}

test("codex sessions from elevated terminals (no exe path, no cwd) are kept", () => {
  const sessions = readDetectorOutput(
    detectorValue(
      [
        {
          pid: 30676,
          parentPid: 3164,
          name: "codex.exe",
          executablePath: null,
          startTime: "2026-07-10T16:00:00.000+09:00",
          cwd: null,
        },
      ],
      [
        { pid: 3164, parentPid: 35876, name: "node.exe", executablePath: null },
        { pid: 35876, parentPid: 2876, name: "powershell.exe", executablePath: null },
        { pid: 2876, parentPid: 1, name: "explorer.exe", executablePath: null },
      ]
    ),
    "codex",
    ROOT_PID
  );
  assert.equal(sessions.length, 1);
  assert.equal(sessions[0].pid, 30676);
  assert.equal(sessions[0].cwd, null);
});

test("codex desktop process (no terminal ancestor) stays excluded", () => {
  const sessions = readDetectorOutput(
    detectorValue(
      [
        {
          pid: 27932,
          parentPid: 15132,
          name: "codex.exe",
          executablePath:
            "C:\\Program Files\\WindowsApps\\OpenAI.Codex_26.707.3748.0_x64__2p2nqsd0c76g0\\app\\resources\\codex.exe",
          startTime: "2026-07-10T10:00:00.000+09:00",
          cwd: null,
        },
      ],
      [
        { pid: 15132, parentPid: 2876, name: "ChatGPT.exe", executablePath: null },
        { pid: 2876, parentPid: 1, name: "explorer.exe", executablePath: null },
      ]
    ),
    "codex",
    ROOT_PID
  );
  assert.equal(sessions.length, 0);
});

test("detected cwd loses its trailing backslash but keeps drive roots", () => {
  const sessions = readDetectorOutput(
    detectorValue(
      [
        {
          pid: 100,
          parentPid: 50,
          name: "codex.exe",
          executablePath: "C:\\somewhere\\codex.exe",
          startTime: null,
          cwd: "D:\\Vibe Project\\My Project\\codex-account-rotator\\",
        },
        {
          pid: 101,
          parentPid: 50,
          name: "codex.exe",
          executablePath: "C:\\somewhere\\codex.exe",
          startTime: null,
          cwd: "D:\\",
        },
      ],
      [{ pid: 50, parentPid: 1, name: "cmd.exe", executablePath: null }]
    ),
    "codex",
    ROOT_PID
  );
  assert.equal(sessions[0].cwd, "D:\\Vibe Project\\My Project\\codex-account-rotator");
  assert.equal(sessions[1].cwd, "D:\\");
});

test("detector resolves the nearest shell through intermediate processes", () => {
  const sessions = readDetectorOutput(
    detectorValue(
      [
        {
          pid: 100,
          parentPid: 80,
          name: "codex.exe",
          executablePath: "C:\\tools\\codex.exe",
          startTime: null,
          cwd: "D:\\work",
        },
      ],
      [
        { pid: 80, parentPid: 60, name: "node.exe", executablePath: null },
        { pid: 60, parentPid: 40, name: "pwsh.exe", executablePath: null },
        { pid: 40, parentPid: 1, name: "explorer.exe", executablePath: null },
      ]
    ),
    "codex",
    ROOT_PID
  );

  assert.deepEqual(sessions[0].terminal, { pid: 60, name: "pwsh.exe" });
});
