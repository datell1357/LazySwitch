import * as crypto from "crypto";
import * as fs from "fs";
import * as http from "http";
import * as os from "os";
import * as path from "path";
import { shell } from "electron";
import { Provider, PAccount, PUsage, LoginFlowResult } from "./types";

/**
 * Claude Code account provider.
 *
 * Live auth lives in TWO places (verified against Claude Code on Windows):
 *  - <claudeHome>/.credentials.json  → { claudeAiOauth: { accessToken, refreshToken,
 *    expiresAt(ms), scopes[], subscriptionType, rateLimitTier } }
 *  - ~/.claude.json → { oauthAccount: { accountUuid, emailAddress, … }, …other settings }
 *    (large file with unrelated state — only the oauthAccount field is patched)
 *
 * Usage + refresh endpoints mirror the OpenUsage `claude` plugin:
 *  - GET  https://api.anthropic.com/api/oauth/usage   (anthropic-beta: oauth-2025-04-20)
 *  - POST https://platform.claude.com/v1/oauth/token  (grant_type=refresh_token)
 *
 * The usage endpoint rate-limits aggressively — responses are cached for
 * USAGE_CACHE_MS per slot and a 429 sets a Retry-After backoff.
 */

const USAGE_URL = "https://api.anthropic.com/api/oauth/usage";
const REFRESH_URL = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const SCOPES =
  "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const REFRESH_BUFFER_MS = 5 * 60 * 1000; // refresh 5 min before expiry
const USAGE_CACHE_MS = 5 * 60 * 1000; // per-slot usage cache
const DEFAULT_429_BACKOFF_MS = 5 * 60 * 1000;

function claudeHome(): string {
  return process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), ".claude");
}
function liveCredFile(): string {
  return path.join(claudeHome(), ".credentials.json");
}
function claudeJsonFile(): string {
  return path.join(os.homedir(), ".claude.json");
}
function accountsRoot(): string {
  return path.join(os.homedir(), ".claude-accounts");
}
function slotDir(name: string): string {
  return path.join(accountsRoot(), name);
}
function slotCredFile(name: string): string {
  return path.join(slotDir(name), "credentials.json");
}
function slotMetaFile(name: string): string {
  return path.join(slotDir(name), "meta.json");
}

function readJson(file: string): any | null {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return null;
  }
}

/** MUST be minified — Claude Code chokes on pretty-printed credentials via keychain paths. */
function writeJsonAtomic(file: string, data: unknown, minify: boolean): void {
  const tmp = file + ".tmp-" + process.pid + "-" + Date.now();
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(tmp, minify ? JSON.stringify(data) : JSON.stringify(data, null, 2), "utf8");
  fs.renameSync(tmp, file);
}

function liveOauthAccount(): any | null {
  return readJson(claudeJsonFile())?.oauthAccount ?? null;
}

function deriveSlotName(email: string | null): string {
  const base =
    (email ?? "").split("@")[0].replace(/[\\/:*?"<>|\s]/g, "_") ||
    "claude-" + Date.now();
  let name = base;
  let i = 2;
  while (fs.existsSync(slotDir(name))) name = `${base}-${i++}`;
  return name;
}

function labelFor(meta: any, cred: any): string | null {
  const email = meta?.oauthAccount?.emailAddress ?? null;
  const plan = cred?.claudeAiOauth?.subscriptionType ?? null;
  return [email, plan].filter(Boolean).join(" · ") || null;
}

function listSlots(): PAccount[] {
  const root = accountsRoot();
  if (!fs.existsSync(root)) return [];
  return fs
    .readdirSync(root, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => {
      const meta = readJson(slotMetaFile(d.name));
      const cred = readJson(slotCredFile(d.name));
      const email = meta?.oauthAccount?.emailAddress ?? null;
      return {
        name: d.name,
        email,
        accountId: meta?.oauthAccount?.accountUuid ?? null,
        label: labelFor(meta, cred),
      };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

function activeName(): string | null {
  const id = liveOauthAccount()?.accountUuid ?? null;
  if (!id) return null;
  return listSlots().find((a) => a.accountId === id)?.name ?? null;
}

// ---------------------------------------------------------------------------
// Token refresh + usage
// ---------------------------------------------------------------------------

async function refreshIfNeeded(credFile: string, cred: any): Promise<any> {
  const oauth = cred?.claudeAiOauth;
  if (!oauth?.refreshToken) return cred;
  const expiresAt = typeof oauth.expiresAt === "number" ? oauth.expiresAt : 0;
  if (Date.now() < expiresAt - REFRESH_BUFFER_MS) return cred;

  try {
    const res = await fetch(REFRESH_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        grant_type: "refresh_token",
        refresh_token: oauth.refreshToken,
        client_id: CLIENT_ID,
        scope: SCOPES,
      }),
    });
    if (!res.ok) return cred;
    const body: any = await res.json();
    if (!body?.access_token) return cred;
    oauth.accessToken = body.access_token;
    if (body.refresh_token) oauth.refreshToken = body.refresh_token;
    if (typeof body.expires_in === "number") {
      oauth.expiresAt = Date.now() + body.expires_in * 1000;
    }
    cred.claudeAiOauth = oauth;
    writeJsonAtomic(credFile, cred, true);
    return cred;
  } catch {
    return cred;
  }
}

interface CacheEntry {
  at: number;
  usage: PUsage | null;
}
const usageCache = new Map<string, CacheEntry>();
let rateLimitedUntil = 0;

function isoToMs(v: unknown): number | null {
  if (typeof v !== "string") return null;
  const ms = Date.parse(v);
  return Number.isFinite(ms) ? ms : null;
}

async function fetchUsageFor(name: string | null): Promise<PUsage | null> {
  const key = name ?? "@live";
  const cached = usageCache.get(key);
  const now = Date.now();
  if (cached && now - cached.at < USAGE_CACHE_MS) return cached.usage;
  if (now < rateLimitedUntil) return cached?.usage ?? null;

  const credFile = name === null ? liveCredFile() : slotCredFile(name);
  let cred = readJson(credFile);
  if (!cred?.claudeAiOauth?.accessToken) return null;
  cred = await refreshIfNeeded(credFile, cred);

  // Test hook mirrors the Codex one: ROTATOR_FAKE_CLAUDE_PCT.
  try {
    const res = await fetch(USAGE_URL, {
      headers: {
        Authorization: "Bearer " + cred.claudeAiOauth.accessToken,
        Accept: "application/json",
        "Content-Type": "application/json",
        "anthropic-beta": "oauth-2025-04-20",
        "User-Agent": "claude-code/2.1.69",
      },
    });
    if (res.status === 429) {
      const retry = parseInt(res.headers.get("retry-after") ?? "", 10);
      rateLimitedUntil =
        now + (Number.isFinite(retry) && retry >= 0 ? retry * 1000 : DEFAULT_429_BACKOFF_MS);
      return cached?.usage ?? null;
    }
    if (!res.ok) return cached?.usage ?? null;
    const data: any = await res.json();

    const meta = name === null ? { oauthAccount: liveOauthAccount() } : readJson(slotMetaFile(name));
    const usage: PUsage = {
      primary:
        typeof data?.five_hour?.utilization === "number"
          ? {
              usedPercent: data.five_hour.utilization,
              windowMinutes: 300,
              resetsAt: isoToMs(data.five_hour.resets_at),
            }
          : null,
      secondary:
        typeof data?.seven_day?.utilization === "number"
          ? {
              usedPercent: data.seven_day.utilization,
              windowMinutes: 10080,
              resetsAt: isoToMs(data.seven_day.resets_at),
            }
          : null,
      planType: cred.claudeAiOauth.subscriptionType ?? null,
      email: meta?.oauthAccount?.emailAddress ?? null,
    };
    usageCache.set(key, { at: now, usage });
    rateLimitedUntil = 0;
    return usage;
  } catch {
    return cached?.usage ?? null;
  }
}

function cachedUsageFor(name: string | null): PUsage | null {
  return usageCache.get(name ?? "@live")?.usage ?? null;
}

// ---------------------------------------------------------------------------
// Add-account login flow (OAuth authorization code + PKCE)
// ---------------------------------------------------------------------------

const SWITCH_ACCOUNT_URL = "https://claude.ai/logout";
const AUTHORIZE_PATH = "/oauth/authorize";
// Claude Code's own callback port — the only localhost redirect whitelisted
// for CLIENT_ID. One login at a time (fixed port), same as the Codex flow.
const LOGIN_PORT = 54545;
const LOGIN_REDIRECT = `http://localhost:${LOGIN_PORT}/callback`;
const LOGIN_TIMEOUT_MS = 5 * 60 * 1000;
const PROFILE_URL = "https://api.anthropic.com/api/oauth/profile";

const b64url = (b: Buffer): string =>
  b.toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

/** One-shot local server that waits for the OAuth redirect and yields the code. */
function waitForCallback(state: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const finish = (e: Error | null, code?: string) => {
      clearTimeout(timer);
      server.close();
      server.closeAllConnections(); // close() alone leaves keep-alive sockets serving
      if (e) reject(e);
      else resolve(code as string);
    };
    const server = http.createServer((req, res) => {
      const u = new URL(req.url ?? "/", LOGIN_REDIRECT);
      if (u.pathname !== "/callback") {
        res.writeHead(404).end();
        return;
      }
      const err = u.searchParams.get("error");
      const code = u.searchParams.get("code");
      const got = u.searchParams.get("state");
      if (got !== state) {
        // Stale tab from an earlier attempt — reject it but keep waiting for
        // the real callback instead of killing the login in progress.
        res
          .writeHead(400, { "Content-Type": "text/html; charset=utf-8" })
          .end("<html><body>Stale login attempt — go back to the app and click add again.</body></html>");
        return;
      }
      const fail = err
        ? `login denied: ${err}`
        : !code
        ? "no code in callback"
        : null;
      res
        .writeHead(fail ? 400 : 200, { "Content-Type": "text/html; charset=utf-8" })
        .end(
          fail
            ? `<html><body>Login failed: ${fail}</body></html>`
            : "<html><body>Login complete — you can close this tab.</body></html>"
        );
      finish(fail ? new Error(fail) : null, code ?? undefined);
    });
    const timer = setTimeout(
      () => finish(new Error(`login timed out (${LOGIN_TIMEOUT_MS / 60000} min)`)),
      LOGIN_TIMEOUT_MS
    );
    server.on("error", (e) => finish(e));
    server.listen(LOGIN_PORT);
  });
}

/**
 * Add a new account WITHOUT disturbing the live login: run the browser OAuth
 * flow ourselves and write the tokens straight into a new slot. The live
 * ~/.claude/.credentials.json is never touched.
 */
async function addViaLogin(onUrl?: (url: string) => void): Promise<LoginFlowResult> {
  const verifier = b64url(crypto.randomBytes(32));
  const state = b64url(crypto.randomBytes(32));
  const authQuery = new URLSearchParams({
    client_id: CLIENT_ID,
    response_type: "code",
    redirect_uri: LOGIN_REDIRECT,
    scope: SCOPES,
    code_challenge: b64url(crypto.createHash("sha256").update(verifier).digest()),
    code_challenge_method: "S256",
    state,
  }).toString();
  // Going straight to /oauth/authorize silently consents as the current
  // claude.ai browser session. Use the same /logout?returnTo= route as
  // claude.ai's switch-account link so a fresh login page is guaranteed.
  // This logs the browser out of claude.ai, matching that official link.
  const url =
    SWITCH_ACCOUNT_URL +
    "?" +
    new URLSearchParams({
      returnTo: AUTHORIZE_PATH + "?" + authQuery,
    });

  const cbPromise = waitForCallback(state); // claim the port before opening the browser
  cbPromise.catch(() => {}); // errors surface via the await below
  onUrl?.(url);
  void shell.openExternal(url);

  let code: string;
  try {
    code = await cbPromise;
  } catch (e) {
    return { ok: false, error: String(e instanceof Error ? e.message : e) };
  }

  let body: any;
  try {
    const res = await fetch(REFRESH_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        grant_type: "authorization_code",
        code,
        state,
        client_id: CLIENT_ID,
        redirect_uri: LOGIN_REDIRECT,
        code_verifier: verifier,
      }),
    });
    if (!res.ok) {
      return {
        ok: false,
        error: `token exchange failed: HTTP ${res.status} ${(await res.text()).slice(0, 200)}`,
      };
    }
    body = await res.json();
  } catch (e) {
    return { ok: false, error: "token exchange failed: " + String(e) };
  }
  if (!body?.access_token) {
    return { ok: false, error: "token exchange returned no access_token" };
  }

  const oauth = {
    accessToken: body.access_token,
    refreshToken: body.refresh_token ?? null,
    expiresAt:
      Date.now() + (typeof body.expires_in === "number" ? body.expires_in * 1000 : 0),
    scopes:
      typeof body.scope === "string" ? body.scope.split(" ") : SCOPES.split(" "),
    subscriptionType: body.account?.subscription_type ?? null,
  };

  // Identify the account for the slot label/identity. The token response
  // usually carries an `account`; the profile endpoint fills in the rest.
  let email: string | null = body.account?.email_address ?? body.account?.email ?? null;
  let uuid: string | null = body.account?.uuid ?? null;
  let orgUuid: string | null = null;
  let orgName: string | null = null;
  try {
    const pr = await fetch(PROFILE_URL, {
      headers: {
        Authorization: "Bearer " + oauth.accessToken,
        Accept: "application/json",
        "anthropic-beta": "oauth-2025-04-20",
        "User-Agent": "claude-code/2.1.69",
      },
    });
    if (pr.ok) {
      const p: any = await pr.json();
      email = email ?? p?.account?.email_address ?? p?.account?.email ?? null;
      uuid = uuid ?? p?.account?.uuid ?? null;
      orgUuid = p?.organization?.uuid ?? null;
      orgName = p?.organization?.name ?? null;
    }
  } catch {
    /* profile is best-effort — the slot still works without it */
  }

  // Re-login of an already-enrolled account refreshes that slot in place.
  const existing = uuid ? listSlots().find((a) => a.accountId === uuid) : undefined;
  const slot = existing?.name ?? deriveSlotName(email);
  const oldMeta = readJson(slotMetaFile(slot));
  fs.mkdirSync(slotDir(slot), { recursive: true });
  writeJsonAtomic(slotCredFile(slot), { claudeAiOauth: oauth }, true);
  writeJsonAtomic(
    slotMetaFile(slot),
    {
      oauthAccount: {
        ...oldMeta?.oauthAccount,
        accountUuid: uuid ?? oldMeta?.oauthAccount?.accountUuid ?? null,
        emailAddress: email ?? oldMeta?.oauthAccount?.emailAddress ?? null,
        ...(orgUuid ? { organizationUuid: orgUuid } : {}),
        ...(orgName ? { organizationName: orgName } : {}),
      },
    },
    false
  );
  return { ok: true, name: slot, email };
}

// ---------------------------------------------------------------------------

export const claudeProvider: Provider = {
  id: "claude",
  displayName: "Claude",
  minPollSec: 300, // usage API rate-limits; OpenUsage enforces the same floor

  listAccounts: listSlots,
  activeAccountName: activeName,
  hasLiveAuth: () => !!readJson(liveCredFile())?.claudeAiOauth?.accessToken,

  importCurrent: (name?: string): PAccount => {
    const cred = readJson(liveCredFile());
    if (!cred?.claudeAiOauth?.accessToken) {
      throw new Error("No live Claude login (~/.claude/.credentials.json)");
    }
    const oauthAccount = liveOauthAccount();
    const slot = name?.trim() || deriveSlotName(oauthAccount?.emailAddress ?? null);
    fs.mkdirSync(slotDir(slot), { recursive: true });
    writeJsonAtomic(slotCredFile(slot), cred, true);
    writeJsonAtomic(slotMetaFile(slot), { oauthAccount }, false);
    return {
      name: slot,
      email: oauthAccount?.emailAddress ?? null,
      accountId: oauthAccount?.accountUuid ?? null,
      label: labelFor({ oauthAccount }, cred),
    };
  },

  removeAccount: (name: string): void => {
    fs.rmSync(slotDir(name), { recursive: true, force: true });
  },
  renameAccount: (oldName: string, newName: string): void => {
    const clean = newName.trim().replace(/[\\/:*?"<>|]/g, "_");
    if (!clean) throw new Error("Invalid account name");
    if (fs.existsSync(slotDir(clean))) throw new Error("Name already in use");
    fs.renameSync(slotDir(oldName), slotDir(clean));
  },

  syncLiveBackToSlot: (): void => {
    const name = activeName();
    if (!name) return;
    const cred = readJson(liveCredFile());
    if (!cred?.claudeAiOauth) return;
    writeJsonAtomic(slotCredFile(name), cred, true);
  },

  installAuth: (name: string): void => {
    const cred = readJson(slotCredFile(name));
    if (!cred?.claudeAiOauth) {
      throw new Error(`Account "${name}" has no credentials.json`);
    }
    // 1. credentials file (what the CLI authenticates with)
    writeJsonAtomic(liveCredFile(), cred, true);
    // 2. oauthAccount inside ~/.claude.json (account identity/metadata) —
    //    patch only that field, the file holds lots of unrelated state.
    const meta = readJson(slotMetaFile(name));
    if (meta?.oauthAccount) {
      const cj = readJson(claudeJsonFile());
      if (cj) {
        cj.oauthAccount = meta.oauthAccount;
        writeJsonAtomic(claudeJsonFile(), cj, false);
      }
    }
  },

  fetchUsage: fetchUsageFor,
  cachedUsage: cachedUsageFor,
  addViaLogin,

  desktop: null, // Claude Desktop does not share CLI auth — nothing to restart
};
