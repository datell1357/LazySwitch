import { EventEmitter } from "events";
import { Provider, ProviderPrefs, PWindow } from "./providers/types";

export interface Window extends PWindow {}

export interface UsageSnapshot {
  primary: Window | null; // short window (5h session / free monthly)
  secondary: Window | null; // long window (weekly)
  source: "session" | "backend" | "none";
  at: number;
}

export type LimitReason =
  | { kind: "threshold"; window: "primary" | "secondary"; percent: number }
  | { kind: "error"; message: string };

/**
 * Polls one provider's live account usage and emits:
 *  - "usage" (UsageSnapshot) every tick
 *  - "limit-hit" (LimitReason) when remaining % crosses the threshold or a
 *    usage-limit error shows up in the provider's local session logs.
 */
export class UsageMonitor extends EventEmitter {
  private timer: NodeJS.Timeout | null = null;
  private lastError: string | null = null;

  constructor(
    private provider: Provider,
    private getPrefs: () => ProviderPrefs
  ) {
    super();
  }

  start(): void {
    if (this.timer) return;
    const sec = Math.max(this.provider.minPollSec, this.getPrefs().pollIntervalSec);
    const tick = () => void this.evaluate();
    this.timer = setInterval(tick, sec * 1000);
    tick();
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  private fakePct(): number | null {
    // Test hooks: ROTATOR_FAKE_CODEX_PCT / ROTATOR_FAKE_CLAUDE_PCT force the
    // primary window's used-percent. ROTATOR_FAKE_PRIMARY_PCT is the legacy
    // alias for codex. Dev/testing only.
    const raw =
      process.env["ROTATOR_FAKE_" + this.provider.id.toUpperCase() + "_PCT"] ??
      (this.provider.id === "codex"
        ? process.env.ROTATOR_FAKE_PRIMARY_PCT
        : undefined);
    if (!raw) return null;
    const n = Number(raw);
    return Number.isFinite(n) ? n : null;
  }

  private async evaluate(): Promise<void> {
    const prefs = this.getPrefs();

    const backend = await this.provider.fetchUsage(null);
    let snapshot: UsageSnapshot;
    if (backend) {
      snapshot = {
        primary: backend.primary,
        secondary: backend.secondary,
        source: "backend",
        at: Date.now(),
      };
    } else {
      const session = this.provider.sessionUsage?.() ?? null;
      snapshot = session
        ? { primary: session.primary, secondary: session.secondary, source: "session", at: Date.now() }
        : { primary: null, secondary: null, source: "none", at: Date.now() };
    }

    const fake = this.fakePct();
    if (fake != null) {
      snapshot.primary = {
        usedPercent: fake,
        windowMinutes: snapshot.primary?.windowMinutes ?? 300,
        resetsAt: snapshot.primary?.resetsAt ?? Date.now() + 30 * 60 * 1000,
      };
    }

    this.emit("usage", snapshot);

    // Reactive backstop: a fresh usage-limit error forces a switch.
    const error = this.provider.scanError?.() ?? null;
    if (error && error !== this.lastError) {
      this.lastError = error;
      this.emit("limit-hit", { kind: "error", message: error } as LimitReason);
      return;
    }
    if (!error) this.lastError = null;

    // Proactive: switch when REMAINING percent drops to the threshold or below.
    if (
      snapshot.primary &&
      100 - snapshot.primary.usedPercent <= prefs.primaryMinLeftPct
    ) {
      this.emit("limit-hit", {
        kind: "threshold",
        window: "primary",
        percent: snapshot.primary.usedPercent,
      } as LimitReason);
      return;
    }
    if (
      snapshot.secondary &&
      100 - snapshot.secondary.usedPercent <= prefs.weeklyMinLeftPct
    ) {
      this.emit("limit-hit", {
        kind: "threshold",
        window: "secondary",
        percent: snapshot.secondary.usedPercent,
      } as LimitReason);
    }
  }
}
