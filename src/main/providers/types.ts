/**
 * Provider abstraction: one implementation per AI service (Codex, Claude).
 * Everything the tray/monitor/switcher need, expressed service-neutrally.
 */

export interface PWindow {
  usedPercent: number; // 0–100
  windowMinutes: number | null;
  resetsAt: number | null; // epoch ms
}

export interface PUsage {
  primary: PWindow | null; // short window (5h session / free monthly)
  secondary: PWindow | null; // long window (weekly)
  planType: string | null;
  email: string | null;
}

export interface PAccount {
  name: string;
  email: string | null;
  accountId: string | null;
  label: string | null;
}

export interface LoginFlowResult {
  ok: boolean;
  name?: string;
  email?: string | null;
  error?: string;
}

/** Per-provider user preferences (see config.ts). */
export interface ProviderPrefs {
  autoApprove: boolean;
  desktopAppPath: string;
  desktopProcessName: string;
  rotationOrder: string[];
  primaryMinLeftPct: number;
  weeklyMinLeftPct: number;
  pollIntervalSec: number;
}

export interface Provider {
  id: "codex" | "claude";
  displayName: string;
  /** Floor for pollIntervalSec — Claude's usage API rate-limits aggressively. */
  minPollSec: number;

  // -- accounts --------------------------------------------------------
  listAccounts(): PAccount[];
  activeAccountName(): string | null;
  /** True if a live login exists at all (enrolled or not). */
  hasLiveAuth(): boolean;
  /** Enroll the current live login into a slot; derives a name if omitted. */
  importCurrent(name?: string): PAccount;
  removeAccount(name: string): void;
  renameAccount(oldName: string, newName: string): void;

  // -- switching -------------------------------------------------------
  /** Persist the live auth back into its owning slot (keep rotated tokens). */
  syncLiveBackToSlot(): void;
  /** Atomically install a slot's auth as the live one. */
  installAuth(name: string): void;

  // -- usage -----------------------------------------------------------
  /** Usage for a slot; name=null means the live login. */
  fetchUsage(name: string | null): Promise<PUsage | null>;
  cachedUsage?(name: string | null): PUsage | null;
  /** Optional local-session usage fallback (Codex rollout files). */
  sessionUsage?(): { primary: PWindow | null; secondary: PWindow | null } | null;
  /** Optional reactive scan for usage-limit errors in local session logs. */
  scanError?(): string | null;

  // -- integrations ----------------------------------------------------
  /** Desktop app that caches auth in memory; null = no restart needed. */
  desktop: { restart(prefs: ProviderPrefs): Promise<boolean> } | null;
  /** Interactive "add account via login" flow, if the CLI supports it headlessly. */
  addViaLogin?(onUrl?: (url: string) => void): Promise<LoginFlowResult>;
}
