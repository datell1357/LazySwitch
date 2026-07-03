import { Provider, PAccount, ProviderPrefs } from "./providers/types";

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

/** Pick the next account after the currently active one, skipping cooling-down ones. */
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
    if (coolingDown.has(cand.name)) continue;
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

  if (from) provider.syncLiveBackToSlot();
  provider.installAuth(name);

  let desktopRestarted = false;
  if (provider.desktop && (opts?.restartDesktop ?? true)) {
    desktopRestarted = await provider.desktop.restart(prefs);
  }
  return { from, to: name, desktopRestarted };
}
