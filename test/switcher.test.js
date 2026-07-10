const assert = require("node:assert/strict");
const test = require("node:test");

const { pickNextAccount, isExhausted, exhaustedUntil } = require("../dist/main/switcher.js");

const PREFS = {
  autoApprove: false,
  autoRestartCli: false,
  desktopAppPath: "",
  desktopProcessName: "",
  rotationOrder: [],
  primaryMinLeftPct: 5,
  weeklyMinLeftPct: 1,
  pollIntervalSec: 30,
};

function windowAt(usedPercent, resetsAt) {
  return { usedPercent, windowMinutes: 300, resetsAt };
}

function providerWith(accounts, active, usageByName) {
  return {
    listAccounts: () => accounts.map((name) => ({ name, email: null, accountId: null, label: null })),
    activeAccountName: () => active,
    cachedUsage: (name) => usageByName[name] ?? null,
  };
}

const NO_COOLDOWN = { has: () => false };
const FUTURE = Date.now() + 60 * 60 * 1000;
const PAST = Date.now() - 60 * 1000;

test("pickNextAccount skips accounts whose cached usage is exhausted", () => {
  const provider = providerWith(["a", "b", "c"], "a", {
    b: { primary: windowAt(100, FUTURE), secondary: null, planType: null, email: null },
  });
  assert.equal(pickNextAccount(provider, PREFS, NO_COOLDOWN)?.name, "c");
});

test("pickNextAccount returns null when every other account is exhausted", () => {
  const spent = { primary: windowAt(100, FUTURE), secondary: null, planType: null, email: null };
  const provider = providerWith(["a", "b", "c"], "a", { b: spent, c: spent });
  assert.equal(pickNextAccount(provider, PREFS, NO_COOLDOWN), null);
});

test("pickNextAccount treats a window past its reset time as usable again", () => {
  const provider = providerWith(["a", "b"], "a", {
    b: { primary: windowAt(100, PAST), secondary: null, planType: null, email: null },
  });
  assert.equal(pickNextAccount(provider, PREFS, NO_COOLDOWN)?.name, "b");
});

test("pickNextAccount keeps accounts without cached usage eligible", () => {
  const provider = providerWith(["a", "b"], "a", {});
  assert.equal(pickNextAccount(provider, PREFS, NO_COOLDOWN)?.name, "b");
});

test("exhaustedUntil returns the latest blocking reset time", () => {
  const usage = {
    primary: windowAt(100, FUTURE),
    secondary: windowAt(100, FUTURE + 1000),
    planType: null,
    email: null,
  };
  assert.equal(exhaustedUntil(usage, PREFS), FUTURE + 1000);
  assert.equal(exhaustedUntil({ primary: windowAt(10, FUTURE), secondary: null, planType: null, email: null }, PREFS), null);
  assert.equal(exhaustedUntil({ primary: windowAt(100, null), secondary: null, planType: null, email: null }, PREFS), null);
});

test("isExhausted honours the weekly threshold too", () => {
  const usage = {
    primary: windowAt(10, FUTURE),
    secondary: windowAt(99.5, FUTURE),
    planType: null,
    email: null,
  };
  assert.equal(isExhausted(usage, PREFS), true);
});
