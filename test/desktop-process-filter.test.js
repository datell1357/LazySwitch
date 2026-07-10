const assert = require("node:assert/strict");
const test = require("node:test");

const { selectDesktopProcessIds } = require("../dist/main/desktop.js");

test("selectDesktopProcessIds keeps Desktop paths and excludes Codex CLI paths", () => {
  const rootPid = 900;
  const selected = selectDesktopProcessIds(
    {
      targets: [
        {
          pid: 101,
          parentPid: 1,
          name: "Codex.exe",
          executablePath:
            "C:\\Program Files\\WindowsApps\\OpenAI.Codex_1.2.3_x64__2p2nqsd0c76g0\\Codex.exe",
        },
        {
          pid: 102,
          parentPid: 1,
          name: "Codex.exe",
          executablePath: "C:\\Users\\me\\AppData\\Local\\Programs\\Codex\\Codex.exe",
        },
        {
          pid: 201,
          parentPid: 1,
          name: "codex.exe",
          executablePath: "C:\\Users\\me\\AppData\\Local\\OpenAI\\Codex\\bin\\codex.exe",
        },
        {
          pid: 202,
          parentPid: 1,
          name: "codex.exe",
          executablePath: "C:\\Users\\me\\.codex\\bin\\codex.exe",
        },
      ],
      parents: [],
    },
    "C:\\Users\\me\\AppData\\Local\\Programs\\Codex\\Codex.exe",
    rootPid
  );

  assert.deepEqual(selected, [101, 102]);
});

test("selectDesktopProcessIds keeps merged ChatGPT.exe inside the OpenAI Store package", () => {
  const rootPid = 900;
  const selected = selectDesktopProcessIds(
    {
      targets: [
        {
          pid: 401,
          parentPid: 1,
          name: "ChatGPT.exe",
          executablePath:
            "C:\\Program Files\\WindowsApps\\OpenAI.Codex_26.707.3563.0_x64__2p2nqsd0c76g0\\app\\ChatGPT.exe",
        },
        {
          pid: 402,
          parentPid: 1,
          name: "ChatGPT.exe",
          executablePath: "C:\\Users\\me\\AppData\\Local\\SomeOtherVendor\\ChatGPT.exe",
        },
      ],
      parents: [],
    },
    "shell:AppsFolder\\OpenAI.Codex_2p2nqsd0c76g0!App",
    rootPid
  );

  assert.deepEqual(selected, [401]);
});

test("selectDesktopProcessIds excludes unknown-path terminal and own descendants", () => {
  const rootPid = 900;
  const selected = selectDesktopProcessIds(
    {
      targets: [
        { pid: 301, parentPid: 700, name: "Codex.exe", executablePath: null },
        { pid: 302, parentPid: 800, name: "Codex.exe", executablePath: null },
        { pid: 303, parentPid: rootPid, name: "Codex.exe", executablePath: null },
      ],
      parents: [
        { pid: 700, parentPid: 1, name: "WindowsTerminal.exe", executablePath: null },
        { pid: 800, parentPid: 1, name: "explorer.exe", executablePath: null },
        { pid: rootPid, parentPid: 1, name: "electron.exe", executablePath: null },
      ],
    },
    "shell:AppsFolder\\OpenAI.Codex_2p2nqsd0c76g0!App",
    rootPid
  );

  assert.deepEqual(selected, [302]);
});
