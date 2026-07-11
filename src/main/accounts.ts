import * as fs from "fs";
import * as path from "path";
import {
  liveAuthFile,
  accountsRoot,
  accountDir,
  accountAuthFile,
} from "./paths";

/** Shape of ~/.codex/auth.json (confirmed on codex-cli 0.142.5, auth_mode "chatgpt"). */
export interface CodexAuth {
  auth_mode?: string;
  OPENAI_API_KEY?: string | null;
  tokens?: {
    id_token?: string;
    access_token?: string;
    refresh_token?: string;
    account_id?: string;
  };
  last_refresh?: string;
}

export interface Account {
  name: string;
  email: string | null;
  accountId: string | null;
  authMode: string | null;
  /** Email/plan decoded from id_token if available. */
  label: string | null;
  lastRefresh: string | null;
  enabled: boolean;
}

function accountStateFile(name: string): string {
  return path.join(accountDir(name), ".lazyswitch.json");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function accountEnabled(name: string): boolean {
  try {
    const state: unknown = JSON.parse(fs.readFileSync(accountStateFile(name), "utf8"));
    return !isRecord(state) || state["enabled"] !== false;
  } catch {
    return true;
  }
}

export function readAuth(file: string): CodexAuth | null {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8")) as CodexAuth;
  } catch {
    return null;
  }
}

/** Decode the JWT id_token payload without verifying (for display only). */
function decodeJwt(token?: string): Record<string, unknown> | null {
  if (!token) return null;
  const parts = token.split(".");
  if (parts.length < 2) return null;
  try {
    const json = Buffer.from(parts[1], "base64url").toString("utf8");
    return JSON.parse(json);
  } catch {
    return null;
  }
}

export function emailFromAuth(auth: CodexAuth | null): string | null {
  const payload = decodeJwt(auth?.tokens?.id_token);
  if (!payload) return null;
  return (
    (payload["email"] as string) ||
    ((payload["https://api.openai.com/profile"] as any)?.email as string) ||
    null
  );
}

/** Derive a friendly slot name from an email ("hyunmin.kang27@gmail.com" -> "hyunmin.kang27"), deduped. */
export function deriveSlotName(email: string | null): string {
  const base =
    (email ?? "").split("@")[0].replace(/[\\/:*?"<>|\s]/g, "_") ||
    "account-" + Date.now();
  let name = base;
  let i = 2;
  while (fs.existsSync(accountDir(name))) name = `${base}-${i++}`;
  return name;
}

function labelFromAuth(auth: CodexAuth | null): string | null {
  const payload = decodeJwt(auth?.tokens?.id_token);
  if (!payload) return null;
  const email =
    (payload["email"] as string) ||
    ((payload["https://api.openai.com/profile"] as any)?.email as string);
  const plan =
    ((payload["https://api.openai.com/auth"] as any)?.chatgpt_plan_type as string) ||
    null;
  return [email, plan].filter(Boolean).join(" · ") || null;
}

function toAccount(name: string, auth: CodexAuth | null): Account {
  return {
    name,
    email: emailFromAuth(auth),
    accountId: auth?.tokens?.account_id ?? null,
    authMode: auth?.auth_mode ?? null,
    label: labelFromAuth(auth),
    lastRefresh: auth?.last_refresh ?? null,
    enabled: accountEnabled(name),
  };
}

export function listAccounts(): Account[] {
  const root = accountsRoot();
  if (!fs.existsSync(root)) return [];
  return fs
    .readdirSync(root, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => toAccount(d.name, readAuth(accountAuthFile(d.name))))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** account_id currently installed in the live ~/.codex/auth.json. */
export function activeAccountId(): string | null {
  return readAuth(liveAuthFile())?.tokens?.account_id ?? null;
}

/** Which stored account matches the live auth (by account_id). */
export function activeAccountName(): string | null {
  const id = activeAccountId();
  if (!id) return null;
  const match = listAccounts().find((a) => a.accountId === id);
  return match?.name ?? null;
}

/** Copy the current live auth.json into a named slot (first-time enrollment). */
export function importCurrentAs(name: string): Account {
  const auth = readAuth(liveAuthFile());
  if (!auth) throw new Error("No live ~/.codex/auth.json to import");
  fs.mkdirSync(accountDir(name), { recursive: true });
  fs.copyFileSync(liveAuthFile(), accountAuthFile(name));
  return toAccount(name, auth);
}

export function removeAccount(name: string): void {
  fs.rmSync(accountDir(name), { recursive: true, force: true });
}

export function renameAccount(oldName: string, newName: string): void {
  const clean = newName.trim().replace(/[\\/:*?"<>|]/g, "_");
  if (!clean) throw new Error("Invalid account name");
  if (fs.existsSync(accountDir(clean))) throw new Error("Name already in use");
  fs.renameSync(accountDir(oldName), accountDir(clean));
}

export function setAccountEnabled(name: string, enabled: boolean): void {
  if (!listAccounts().some((account) => account.name === name)) {
    throw new Error(`Account \"${name}\" is not enrolled`);
  }
  fs.writeFileSync(accountStateFile(name), JSON.stringify({ enabled }), "utf8");
}
