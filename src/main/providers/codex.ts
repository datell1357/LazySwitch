import * as fs from "fs";
import * as path from "path";
import { Provider, PAccount, PUsage, PWindow, ProviderPrefs } from "./types";
import { sessionsDir, liveAuthFile, accountAuthFile } from "../paths";
import {
  listAccounts,
  activeAccountName,
  activeAccountId,
  importCurrentAs,
  removeAccount,
  renameAccount,
  readAuth,
  emailFromAuth,
  deriveSlotName,
} from "../accounts";
import { fetchUsage } from "../codex-api";
import { restartDesktopApp } from "../desktop";
import { addAccountViaLogin } from "../login";

/** Write file atomically: temp write + rename, so no reader sees a half-written auth.json. */
function atomicCopy(src: string, dest: string): void {
  const tmp = dest + ".tmp-" + process.pid + "-" + Date.now();
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.copyFileSync(src, tmp);
  fs.renameSync(tmp, dest);
}

// ---------------------------------------------------------------------------
// Session-file scanning (moved verbatim from monitor.ts — Codex-specific).
// ---------------------------------------------------------------------------

/** Recursively find the most recently modified rollout-*.jsonl. */
function newestRollout(dir: string): string | null {
  const found: Array<{ file: string; mtime: number }> = [];
  const walk = (d: string) => {
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      const full = path.join(d, e.name);
      if (e.isDirectory()) walk(full);
      else if (e.name.startsWith("rollout-") && e.name.endsWith(".jsonl")) {
        found.push({ file: full, mtime: fs.statSync(full).mtimeMs });
      }
    }
  };
  walk(dir);
  if (found.length === 0) return null;
  return found.reduce((a, b) => (b.mtime > a.mtime ? b : a)).file;
}

function toWindow(raw: any): PWindow | null {
  if (!raw || typeof raw.used_percent !== "number") return null;
  const resetsIn =
    typeof raw.resets_in_seconds === "number" ? raw.resets_in_seconds : null;
  return {
    usedPercent: raw.used_percent,
    windowMinutes:
      typeof raw.window_minutes === "number" ? raw.window_minutes : null,
    resetsAt: resetsIn != null ? Date.now() + resetsIn * 1000 : null,
  };
}

const ERROR_MARKERS = [
  "usage limit reached",
  "you've hit your usage limit",
  "rate limit",
  "too many requests",
  "quota",
];

/**
 * Read the tail of the newest session file and extract:
 *  - the most recent rate_limits object (proactive signal, if this codex build emits it)
 *  - any usage-limit error line (reactive backstop)
 */
function scanSession(): {
  usage: { primary: PWindow | null; secondary: PWindow | null } | null;
  error: string | null;
} {
  const file = newestRollout(sessionsDir());
  if (!file) return { usage: null, error: null };

  let lines: string[];
  try {
    lines = fs.readFileSync(file, "utf8").split("\n");
  } catch {
    return { usage: null, error: null };
  }

  let usage: { primary: PWindow | null; secondary: PWindow | null } | null =
    null;
  let error: string | null = null;
  // Walk from the end; take the last rate_limits we see.
  for (let i = lines.length - 1; i >= 0 && i > lines.length - 400; i--) {
    const line = lines[i];
    if (!line) continue;

    // Reactive backstop: only trust genuine error EVENTS. Substring matching
    // on the raw line false-positives on conversation text that merely talks
    // about rate limits (e.g. transcripts of this very project).
    if (!error || !usage) {
      try {
        const obj = JSON.parse(line);
        if (!error) {
          const ptype = obj?.payload?.type;
          if (ptype === "error" || ptype === "stream_error") {
            const msg = String(obj?.payload?.message ?? "").toLowerCase();
            const marker = ERROR_MARKERS.find((m) => msg.includes(m));
            if (marker) error = marker;
          }
        }
        if (!usage) {
          const rl = obj?.payload?.info?.rate_limits ?? obj?.rate_limits;
          if (rl) {
            usage = {
              primary: toWindow(rl.primary),
              secondary: toWindow(rl.secondary),
            };
          }
        }
      } catch {
        /* ignore malformed line */
      }
    }
    if (usage && error) break;
  }
  return { usage, error };
}

// ---------------------------------------------------------------------------

async function usageFor(name: string | null): Promise<PUsage | null> {
  const file = name === null ? liveAuthFile() : accountAuthFile(name);
  const u = await fetchUsage(file);
  if (!u) return null;
  return {
    primary: u.primary,
    secondary: u.secondary,
    planType: u.planType,
    email: u.email,
  };
}

export const codexProvider: Provider = {
  id: "codex",
  displayName: "Codex",
  minPollSec: 10,

  listAccounts: (): PAccount[] =>
    listAccounts().map((a) => ({
      name: a.name,
      email: a.email,
      accountId: a.accountId,
      label: a.label,
    })),
  activeAccountName,
  hasLiveAuth: () => !!activeAccountId(),
  importCurrent: (name?: string): PAccount => {
    const slot =
      name?.trim() || deriveSlotName(emailFromAuth(readAuth(liveAuthFile())));
    const a = importCurrentAs(slot);
    return { name: a.name, email: a.email, accountId: a.accountId, label: a.label };
  },
  removeAccount,
  renameAccount,

  syncLiveBackToSlot: (): void => {
    const activeName = activeAccountName();
    if (!activeName) return;
    const live = readAuth(liveAuthFile());
    if (!live) return;
    atomicCopy(liveAuthFile(), accountAuthFile(activeName));
  },
  installAuth: (name: string): void => {
    if (!fs.existsSync(accountAuthFile(name))) {
      throw new Error(`Account "${name}" has no auth.json`);
    }
    atomicCopy(accountAuthFile(name), liveAuthFile());
  },

  fetchUsage: usageFor,
  sessionUsage: () => scanSession().usage,
  scanError: () => scanSession().error,

  desktop: {
    restart: (prefs: ProviderPrefs) => restartDesktopApp(prefs),
  },
  addViaLogin: addAccountViaLogin,
};
