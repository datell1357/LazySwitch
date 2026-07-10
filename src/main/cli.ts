#!/usr/bin/env node
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { installCodexWrapper, installHooks } from "./cli-hooks";
import { renderStatusline, renderTable, type UsageRow } from "./cli-render";
import { codexProvider } from "./providers/codex";
import { claudeProvider } from "./providers/claude";
import type { PAccount, Provider } from "./providers/types";

type Command =
  | "status"
  | "watch"
  | "statusline"
  | "install-hooks"
  | "install-codex-wrapper"
  | "help";
type ProviderFilter = Provider["id"] | "all";

class CliError extends Error {}

const providers: readonly Provider[] = [codexProvider, claudeProvider];
const STATUSLINE_CACHE_MS = 60 * 1000;
const STATUSLINE_CACHE_VERSION = 8;

function command(args: readonly string[]): Command {
  const first = args[0];
  if (first === undefined || first === "status") return "status";
  if (
    first === "watch" ||
    first === "statusline" ||
    first === "install-hooks" ||
    first === "install-codex-wrapper" ||
    first === "help"
  )
    return first;
  if (first === "--help" || first === "-h") return "help";
  throw new CliError(`Unknown command: ${first}`);
}

function intervalSeconds(args: readonly string[]): number {
  const index = args.indexOf("--interval");
  if (index < 0) return 30;
  const raw = args[index + 1];
  const parsed = raw === undefined ? Number.NaN : Number(raw);
  if (!Number.isFinite(parsed) || parsed < 5) {
    throw new CliError("--interval must be a number >= 5");
  }
  return Math.round(parsed);
}

function providerFilter(args: readonly string[]): ProviderFilter {
  const raw = args[1] ?? "all";
  if (raw === "all" || raw === "codex" || raw === "claude") return raw;
  throw new CliError("statusline provider must be all, codex, or claude");
}

async function rowFor(provider: Provider, account: PAccount): Promise<UsageRow> {
  const active = provider.activeAccountName() === account.name;
  try {
    const usage = await provider.fetchUsage(active ? null : account.name);
    return { provider: provider.displayName, account, active, usage, error: null };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { provider: provider.displayName, account, active, usage: null, error: message };
  }
}

async function liveRowFor(provider: Provider): Promise<UsageRow> {
  try {
    const usage = await provider.fetchUsage(null);
    return {
      provider: provider.displayName,
      account: {
        name: "@live",
        email: usage?.email ?? null,
        accountId: null,
        label: "live login",
      },
      active: true,
      usage,
      error: null,
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      provider: provider.displayName,
      account: { name: "@live", email: null, accountId: null, label: "live login" },
      active: true,
      usage: null,
      error: message,
    };
  }
}

async function rows(filter: ProviderFilter = "all"): Promise<readonly UsageRow[]> {
  const all: UsageRow[] = [];
  for (const provider of providers) {
    if (filter !== "all" && provider.id !== filter) continue;
    const accounts = provider.listAccounts();
    for (const account of accounts) all.push(await rowFor(provider, account));
    if (provider.activeAccountName() === null && provider.hasLiveAuth()) {
      all.push(await liveRowFor(provider));
    }
  }
  return all;
}

function statuslineCacheFile(filter: ProviderFilter): string {
  return path.join(os.homedir(), ".lazyswitch", `statusline-cache-${filter}.json`);
}

function colorMode(): "ansi" | "plain" {
  return process.env.NO_COLOR === "1" ? "plain" : "ansi";
}

function cachedStatusline(filter: ProviderFilter): string | null {
  try {
    const parsed: unknown = JSON.parse(fs.readFileSync(statuslineCacheFile(filter), "utf8"));
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    const at = "at" in parsed ? parsed.at : null;
    const text = "text" in parsed ? parsed.text : null;
    const version = "version" in parsed ? parsed.version : null;
    const mode = "mode" in parsed ? parsed.mode : null;
    if (typeof at !== "number" || typeof text !== "string") return null;
    if (version !== STATUSLINE_CACHE_VERSION) return null;
    if (mode !== colorMode()) return null;
    return Date.now() - at <= STATUSLINE_CACHE_MS ? text : null;
  } catch {
    return null;
  }
}

function writeStatusline(filter: ProviderFilter, text: string): void {
  const file = statuslineCacheFile(filter);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    JSON.stringify({ at: Date.now(), text, mode: colorMode(), version: STATUSLINE_CACHE_VERSION }),
    "utf8"
  );
}

function help(): string {
  return [
    "Usage:",
    "  lazyswitch status              print account usage table once",
    "  lazyswitch watch [--interval N] keep the table visible",
    "  lazyswitch statusline [provider] print one compact line",
    "  lazyswitch install-hooks       install Claude statusLine and Codex built-ins",
    "  lazyswitch install-codex-wrapper wrap codex with a LazySwitch usage pane",
  ].join("\n");
}

async function printStatus(json: boolean): Promise<void> {
  const items = await rows();
  console.log(json ? JSON.stringify({ at: new Date().toISOString(), rows: items }, null, 2) : renderTable(items));
}

async function watch(args: readonly string[]): Promise<void> {
  const delayMs = intervalSeconds(args) * 1000;
  for (;;) {
    const items = await rows();
    process.stdout.write("\x1b[2J\x1b[H" + renderTable(items) + "\n");
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }
}

async function printStatusline(args: readonly string[]): Promise<void> {
  const filter = providerFilter(args);
  const cached = cachedStatusline(filter);
  if (cached !== null) {
    console.log(cached);
    return;
  }
  const text = renderStatusline(await rows(filter));
  writeStatusline(filter, text);
  console.log(text);
}

function printInstallHooks(): void {
  for (const result of installHooks()) {
    const mark = result.changed ? "updated" : "unchanged";
    console.log(`${result.target}: ${mark} ${result.path}`);
    console.log(`  ${result.note}`);
  }
}

function printInstallCodexWrapper(): void {
  const result = installCodexWrapper();
  const mark = result.changed ? "updated" : "unchanged";
  console.log(`${result.target}: ${mark} ${result.path}`);
  console.log(`  ${result.note}`);
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  const cmd = command(args);
  if (cmd === "help") {
    console.log(help());
  } else if (cmd === "watch") {
    await watch(args);
  } else if (cmd === "statusline") {
    await printStatusline(args);
  } else if (cmd === "install-hooks") {
    printInstallHooks();
  } else if (cmd === "install-codex-wrapper") {
    printInstallCodexWrapper();
  } else {
    await printStatus(args.includes("--json"));
  }
}

main().catch((error: unknown) => {
  if (error instanceof CliError) {
    console.error(error.message);
    process.exitCode = 2;
    return;
  }
  throw error;
});
