import * as fs from "fs";
import { CodexAuth, readAuth } from "./accounts";

/**
 * Direct access to the same ChatGPT backend the Codex clients use for usage.
 * Endpoint + refresh flow reverse-engineered from the OpenUsage `codex` plugin.
 */
const USAGE_URL = "https://chatgpt.com/backend-api/wham/usage";
const REFRESH_URL = "https://auth.openai.com/oauth/token";
const CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_AGE_MS = 8 * 24 * 60 * 60 * 1000; // refresh proactively after 8 days
const USAGE_CACHE_MS = 5 * 60 * 1000;
const DEFAULT_429_BACKOFF_MS = 5 * 60 * 1000;

export interface UsageWindow {
  usedPercent: number;
  /** Window length in minutes (5h / weekly / monthly-free…), if reported. */
  windowMinutes: number | null;
  resetsAt: number | null; // epoch ms
}

export interface CodexUsage {
  primary: UsageWindow | null; // 5-hour session window
  secondary: UsageWindow | null; // weekly window
  planType: string | null;
  creditsBalance: number | null;
  email: string | null;
}

interface CacheEntry {
  at: number;
  usage: CodexUsage | null;
}
const usageCache = new Map<string, CacheEntry>();
let rateLimitedUntil = 0;

export function cachedUsage(file: string): CodexUsage | null {
  return usageCache.get(file)?.usage ?? null;
}

function writeAuthAtomic(file: string, auth: CodexAuth): void {
  const tmp = file + ".tmp-" + process.pid + "-" + Date.now();
  fs.writeFileSync(tmp, JSON.stringify(auth, null, 2), "utf8");
  fs.renameSync(tmp, file);
}

function needsRefresh(auth: CodexAuth): boolean {
  if (!auth.last_refresh) return true;
  const last = Date.parse(auth.last_refresh);
  if (Number.isNaN(last)) return true;
  return Date.now() - last > REFRESH_AGE_MS;
}

/**
 * Exchange the refresh_token for a new access_token and persist the rotated
 * tokens back to `file`. Returns the updated auth, or null on failure.
 */
export async function refreshToken(
  file: string,
  auth: CodexAuth
): Promise<CodexAuth | null> {
  const rt = auth.tokens?.refresh_token;
  if (!rt) return null;
  try {
    const res = await fetch(REFRESH_URL, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body:
        "grant_type=refresh_token&client_id=" +
        encodeURIComponent(CLIENT_ID) +
        "&refresh_token=" +
        encodeURIComponent(rt),
    });
    if (!res.ok) return null;
    const body: any = await res.json();
    if (!body?.access_token) return null;
    auth.tokens = auth.tokens ?? {};
    auth.tokens.access_token = body.access_token;
    if (body.refresh_token) auth.tokens.refresh_token = body.refresh_token;
    if (body.id_token) auth.tokens.id_token = body.id_token;
    auth.last_refresh = new Date().toISOString();
    writeAuthAtomic(file, auth);
    return auth;
  } catch {
    return null;
  }
}

function num(v: unknown): number | null {
  if (v == null || v === "") return null; // Number(null) is 0, not NaN
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

function windowFrom(
  headerPct: number | null,
  raw: any
): UsageWindow | null {
  const pct = headerPct ?? num(raw?.used_percent);
  if (pct == null) return null;
  let resetsAt: number | null = null;
  if (typeof raw?.reset_at === "number") resetsAt = raw.reset_at * 1000;
  else if (typeof raw?.reset_after_seconds === "number")
    resetsAt = Date.now() + raw.reset_after_seconds * 1000;
  const winSec = num(raw?.limit_window_seconds);
  return {
    usedPercent: pct,
    windowMinutes: winSec != null ? Math.round(winSec / 60) : null,
    resetsAt,
  };
}

/**
 * Fetch live usage for whichever account is installed at `file` (default: the
 * live ~/.codex/auth.json). Refreshes the token first if it is stale, and once
 * more if the server returns 401.
 */
export async function fetchUsage(file: string): Promise<CodexUsage | null> {
  const now = Date.now();
  const cached = usageCache.get(file);
  if (cached && now - cached.at < USAGE_CACHE_MS) return cached.usage;
  if (now < rateLimitedUntil) return cached?.usage ?? null;

  let auth = readAuth(file);
  if (!auth?.tokens?.access_token) return null;

  if (needsRefresh(auth)) {
    auth = (await refreshToken(file, auth)) ?? auth;
  }

  const call = async (a: CodexAuth) => {
    const headers: Record<string, string> = {
      Authorization: "Bearer " + a.tokens!.access_token,
      Accept: "application/json",
      "User-Agent": "codex-account-rotator",
    };
    if (a.tokens?.account_id) headers["ChatGPT-Account-Id"] = a.tokens.account_id;
    return fetch(USAGE_URL, { method: "GET", headers });
  };

  try {
    let res = await call(auth);
    if (res.status === 401) {
      const refreshed = await refreshToken(file, auth);
      if (refreshed) {
        auth = refreshed;
        res = await call(auth);
      }
    }
    if (res.status === 429) {
      const retry = parseInt(res.headers.get("retry-after") ?? "", 10);
      rateLimitedUntil =
        now + (Number.isFinite(retry) && retry >= 0 ? retry * 1000 : DEFAULT_429_BACKOFF_MS);
      return cached?.usage ?? null;
    }
    if (!res.ok) return cached?.usage ?? null;

    const headerPrimary = num(res.headers.get("x-codex-primary-used-percent"));
    const headerSecondary = num(
      res.headers.get("x-codex-secondary-used-percent")
    );
    const headerCredits = num(res.headers.get("x-codex-credits-balance"));

    const data: any = await res.json();
    const rl = data?.rate_limit ?? {};
    const usage = {
      primary: windowFrom(headerPrimary, rl.primary_window),
      secondary: windowFrom(headerSecondary, rl.secondary_window),
      planType: typeof data?.plan_type === "string" ? data.plan_type : null,
      creditsBalance: headerCredits ?? num(data?.credits?.balance),
      email: typeof data?.email === "string" ? data.email : null,
    };
    usageCache.set(file, { at: now, usage });
    rateLimitedUntil = 0;
    return usage;
  } catch {
    return cached?.usage ?? null;
  }
}
