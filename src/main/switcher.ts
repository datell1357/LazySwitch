import { Provider, PAccount, ProviderPrefs, PUsage, PWindow } from "./providers/types";

/** Order in which accounts are considered for rotation. */
export function rotationList(provider: Provider, prefs: ProviderPrefs): PAccount[] {
  const all = provider.listAccounts();
  if (prefs.rotationOrder.length === 0) return all;
  const byName = new Map(all.map((a) => [a.name, a]));
  const ordered = prefs.rotationOrder
    .map((n) => byName.get(n))
    .filter((a): a is PAccount => !!a);
  // Append any accounts not named in the explicit order.
  for (const a of all) if (!prefs.rotationOrder.includes(a.name)) ordered.push(a);
  return ordered;
}

function spentWindow(
  w: PWindow | null | undefined,
  minLeftPct: number,
  now: number
): boolean {
  return (
    !!w &&
    (w.resetsAt === null || w.resetsAt > now) &&
    100 - w.usedPercent <= minLeftPct
  );
}

/**
 * True when the last known usage says the account would trip the switch
 * thresholds immediately. A window whose reset time has passed no longer
 * counts — the quota is back even if the cache is stale.
 */
export function isExhausted(
  usage: PUsage | null,
  prefs: ProviderPrefs,
  now = Date.now()
): boolean {
  if (!usage) return false;
  return (
    spentWindow(usage.primary, prefs.primaryMinLeftPct, now) ||
    spentWindow(usage.secondary, prefs.weeklyMinLeftPct, now)
  );
}

/**
 * When the account is exhausted, the epoch ms at which its last blocking
 * window resets — null if it is not exhausted or no reset time is known
 * (callers should fall back to a short cooldown).
 */
export function exhaustedUntil(
  usage: PUsage | null,
  prefs: ProviderPrefs,
  now = Date.now()
): number | null {
  if (!usage) return null;
  let until: number | null = null;
  const windows: Array<[PWindow | null | undefined, number]> = [
    [usage.primary, prefs.primaryMinLeftPct],
    [usage.secondary, prefs.weeklyMinLeftPct],
  ];
  for (const [w, minLeftPct] of windows) {
    if (!spentWindow(w, minLeftPct, now)) continue;
    if (w!.resetsAt === null) return null;
    until = until === null ? w!.resetsAt : Math.max(until, w!.resetsAt);
  }
  return until;
}

/**
 * Pick the next account after the currently active one, skipping cooling-down
 * ones and ones whose cached usage is already at the limit — switching to
 * those would just bounce straight back here.
 */
export function pickNextAccount(
  provider: Provider,
  prefs: ProviderPrefs,
  coolingDown: { has(name: string): boolean }
): PAccount | null {
  const list = rotationList(provider, prefs);
  if (list.length === 0) return null;
  const activeName = provider.activeAccountName();
  const startIdx = Math.max(
    0,
    list.findIndex((a) => a.name === activeName)
  );
  for (let i = 1; i <= list.length; i++) {
    const cand = list[(startIdx + i) % list.length];
    if (cand.name === activeName) continue;
    if (cand.enabled === false) continue;
    if (coolingDown.has(cand.name)) continue;
    if (isExhausted(provider.cachedUsage?.(cand.name) ?? null, prefs)) continue;
    return cand;
  }
  return null;
}

export interface SwitchResult {
  from: string | null;
  to: string;
  desktopRestarted: boolean;
}

/**
 * Perform the switch:
 *  1. save the current live auth back to its slot (preserve refreshed tokens)
 *  2. atomically install the target slot's auth as the live one
 *  3. optionally restart the provider's desktop app so it reloads the account
 */
export async function switchTo(
  provider: Provider,
  name: string,
  prefs: ProviderPrefs,
  opts?: { restartDesktop?: boolean }
): Promise<SwitchResult> {
  const from = provider.activeAccountName();
  const target = provider.listAccounts().find((account) => account.name === name);
  if (target?.enabled === false) {
    throw new Error(`Account \"${name}\" is disabled`);
  }

  if (from) provider.syncLiveBackToSlot();
  provider.installAuth(name);

  let desktopRestarted = false;
  if (provider.desktop && (opts?.restartDesktop ?? true)) {
    desktopRestarted = await provider.desktop.restart(prefs);
  }
  return { from, to: name, desktopRestarted };
}
