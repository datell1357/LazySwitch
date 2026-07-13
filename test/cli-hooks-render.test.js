const assert = require("node:assert/strict");
const test = require("node:test");

const { renderStatusline } = require("../dist/main/cli-render.js");

function account(name, email, label = null) {
  return { name, email, accountId: null, label, enabled: true };
}

function usage(primary, secondary, fable, resetMs = 3 * 24 * 60 * 60 * 1000) {
  const resetsAt = Date.now() + resetMs;
  return {
    primary: { usedPercent: primary, windowMinutes: 300, resetsAt },
    secondary: { usedPercent: secondary, windowMinutes: 10080, resetsAt },
    ...(fable === undefined
      ? {}
      : { fable: { usedPercent: fable, windowMinutes: 10080, resetsAt } }),
    planType: "pro",
    email: null,
  };
}

function stripAnsi(text) {
  return text.replace(/\x1b\[[0-9;]*m/g, "");
}

test("statusline adapts every account line to the available width", () => {
  const previousNoColor = process.env.NO_COLOR;
  process.env.NO_COLOR = "1";
  try {
    const resetMs = (20 * 24 + 17) * 60 * 60 * 1000 + 30 * 1000;
    const rows = [
      { provider: "Claude", account: account("slot-a", "hyunmin.kang27@example.com"), active: true, usage: usage(100, 74, 80, resetMs), error: null },
      { provider: "Claude", account: account("slot-b", "very-long-local-part-that-will-clip@example.com"), active: false, usage: usage(56, 78, 9, resetMs), error: null },
      { provider: "Codex", account: account("slot-name", null, "friendly label"), active: false, usage: usage(12, 34, 80, resetMs), error: null },
    ];
    for (const width of [44, 55, 80]) {
      const output = renderStatusline(rows, width);
      const lines = output.split("\n");
      assert.equal(lines.length, rows.length);
      assert.ok(lines.every((line) => line.length <= width), `plain width ${width}: ${output}`);
      if (width === 44) {
        assert.match(lines[0], /^RST/);
        assert.match(lines[1], /^WAI/);
        assert.match(lines[2], /^WAI/);
      } else {
        assert.match(lines[0], /^slot-a/);
        assert.match(lines[1], /^slot-b/);
        assert.match(lines[2], /^slot-name/);
      }
      const fiveHourIndexes = lines.map((line) => line.indexOf("5H"));
      assert.ok(fiveHourIndexes.every((index) => index === fiveHourIndexes[0]), `5H alignment at width ${width}: ${output}`);
      assert.ok(lines.every((line) => line.includes("5H [") && line.includes("Week [")), output);
      const gaugeInteriors = lines.map((line) => [...line.matchAll(/\[([^\]]*)\]/g)].map((match) => match[1]));
      assert.ok(gaugeInteriors.every((interiors) => interiors.every((interior) => interior.length === interiors[0].length)), output);
      assert.ok(gaugeInteriors.flat().every((interior) => interior.length === 6 || interior.length === 13), output);
      if (width === 80) assert.match(output, /20d17h/);
      if (width <= 55) {
        assert.doesNotMatch(lines[0], /20d17h/);
      }
      if (width === 44) {
        assert.equal(lines[0].length, 44);
        assert.match(lines[0], /5H \[[^\]]{6}\] Week \[[^\]]{6}\] Fable \[[^\]]{6}\]$/);
      }
    }
    const wideLines = renderStatusline(rows, 80).split("\n");
    assert.match(wideLines[0], /^slot-a/);
    assert.match(wideLines[1], /^slot-b/);
    assert.match(wideLines[2], /^slot-name/);
    for (const width of [44, 55, 80]) {
      assert.ok(!renderStatusline(rows, width).includes("@example.com"));
    }
    assert.ok(!renderStatusline(rows, 80).split("\n")[2].includes("Fable ["));
  } finally {
    if (previousNoColor === undefined) delete process.env.NO_COLOR;
    else process.env.NO_COLOR = previousNoColor;
  }
});

test("statusline orders active accounts first within each provider block", () => {
  const previousNoColor = process.env.NO_COLOR;
  process.env.NO_COLOR = "1";
  try {
    const rows = [
      { provider: "Codex", account: account("codex-waiting", null), active: false, usage: usage(12, 34), error: null },
      { provider: "Codex", account: account("codex-active", null), active: true, usage: usage(12, 34), error: null },
      { provider: "Claude", account: account("claude-active", null), active: true, usage: usage(12, 34, 56), error: null },
    ];
    const lines = renderStatusline(rows, 80).split("\n");
    assert.match(lines[0], /^codex-active/);
    assert.match(lines[1], /^codex-waiting/);
    assert.match(lines[2], /^claude-active/);
  } finally {
    if (previousNoColor === undefined) delete process.env.NO_COLOR;
    else process.env.NO_COLOR = previousNoColor;
  }
});

test("colored statusline remains width-aware after stripping ANSI", () => {
  const previousNoColor = process.env.NO_COLOR;
  delete process.env.NO_COLOR;
  try {
    const resetMs = (20 * 24 + 17) * 60 * 60 * 1000 + 30 * 1000;
    const rows = [
      { provider: "Claude", account: account("slot-a", "hyunmin.kang27@example.com"), active: true, usage: usage(100, 74, 80, resetMs), error: null },
    ];
    for (const width of [44, 55, 80]) {
      const line = stripAnsi(renderStatusline(rows, width));
      assert.ok(line.length <= width, `ANSI width ${width}: ${line}`);
      assert.ok(!line.includes("[") && !line.includes("]"), line);
      const gaugeWidth = width === 80 ? 15 : 8;
      for (const marker of ["5H", "Week", "Fable"]) {
        const markerIndex = line.indexOf(marker);
        assert.notEqual(markerIndex, -1, line);
        assert.equal(line[markerIndex + marker.length], " ", line);
        assert.equal(line.slice(markerIndex + marker.length + 1, markerIndex + marker.length + 1 + gaugeWidth).length, gaugeWidth, line);
      }
      if (width === 80) assert.match(line, /100% 20d17h/);
      if (width === 44) assert.doesNotMatch(line, /20d17h/);
    }
  } finally {
    if (previousNoColor === undefined) delete process.env.NO_COLOR;
    else process.env.NO_COLOR = previousNoColor;
  }
});

test("statusline renders missing usage as centered n/a gauges", () => {
  const previousNoColor = process.env.NO_COLOR;
  try {
    process.env.NO_COLOR = "1";
    const plain = renderStatusline([
      { provider: "Claude", account: account("slot-a", "slot-a@example.com"), active: true, usage: null, error: "unavailable" },
    ]);
    assert.match(plain, /5H \[     n\/a     \] Week \[     n\/a     \] Fable \[     n\/a     \]$/);
    assert.ok(!plain.includes("unavail"));

    delete process.env.NO_COLOR;
    const colored = stripAnsi(renderStatusline([
      { provider: "Claude", account: account("slot-a", "slot-a@example.com"), active: true, usage: null, error: "unavailable" },
    ]));
    assert.ok(!colored.includes("[") && !colored.includes("]"));
    for (const marker of ["5H", "Week", "Fable"]) {
      const markerIndex = colored.indexOf(marker);
      assert.notEqual(markerIndex, -1, colored);
      assert.equal(colored[markerIndex + marker.length], " ", colored);
      assert.equal(colored.slice(markerIndex + marker.length + 1, markerIndex + marker.length + 1 + 15), "      n/a      ", colored);
    }
    assert.ok(!colored.includes("unavail"));
  } finally {
    if (previousNoColor === undefined) delete process.env.NO_COLOR;
    else process.env.NO_COLOR = previousNoColor;
  }
});

test("statusline strips terminal controls from account labels", () => {
  const previousNoColor = process.env.NO_COLOR;
  process.env.NO_COLOR = "1";
  try {
    const line = renderStatusline([
      {
        provider: "Claude",
        account: account("slot\nname", "hyunmin.\x1b[31mkang27@example.com"),
        active: false,
        usage: usage(12, 34),
        error: null,
      },
    ]);
    assert.equal(line.split("\n").length, 1);
    assert.ok(!/[\u0000-\u001f\u007f-\u009f]/.test(line));
  } finally {
    if (previousNoColor === undefined) delete process.env.NO_COLOR;
    else process.env.NO_COLOR = previousNoColor;
  }
});

test("statusline budgets wide Unicode labels by terminal columns", () => {
  const previousNoColor = process.env.NO_COLOR;
  process.env.NO_COLOR = "1";
  try {
    const line = renderStatusline([
      { provider: "Codex", account: account("한글".repeat(30), null), active: true, usage: usage(12, 34), error: null },
    ], 44);
    const terminalWidth = [...line].reduce((width, character) => width + (/^[\uac00-\ud7a3]$/.test(character) ? 2 : 1), 0);
    assert.ok(terminalWidth <= 44, line);
  } finally {
    if (previousNoColor === undefined) delete process.env.NO_COLOR;
    else process.env.NO_COLOR = previousNoColor;
  }
});
