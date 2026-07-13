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
const STATUSLINE_CACHE_VERSION = 11;
const STATUSLINE_DEFAULT_WIDTH = 80;
const STATUSLINE_MIN_WIDTH = 20;
const STATUSLINE_MAX_WIDTH = 1000;
const STATUSLINE_WIDTH_KEYS = ["width", "cols", "columns", "terminal_width", "terminalWidth"] as const;

type JsonObject = { readonly [key: string]: unknown };

function isJsonObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function saneWidth(value: unknown): number | null {
  const numeric = typeof value === "number" || typeof value === "string" ? Number(value) : Number.NaN;
  if (!Number.isFinite(numeric) || numeric < STATUSLINE_MIN_WIDTH || numeric > STATUSLINE_MAX_WIDTH) return null;
  return Math.floor(numeric);
}

function directPayloadWidth(value: JsonObject): number | null {
  for (const key of STATUSLINE_WIDTH_KEYS) {
    const width = saneWidth(value[key]);
    if (width !== null) return width;
  }
  return null;
}

function nestedPayloadWidth(value: unknown): number | null {
  if (!isJsonObject(value)) return null;
  const direct = directPayloadWidth(value);
  if (direct !== null) return direct;
  for (const key of ["terminal", "workspace", "dimensions"] as const) {
    const nested = nestedPayloadWidth(value[key]);
    if (nested !== null) return nested;
  }
  return null;
}

function statuslinePayloadWidth(payload: unknown): number | null {
  if (!isJsonObject(payload)) return null;
  const direct = directPayloadWidth(payload);
  if (direct !== null) return direct;
  for (const key of ["terminal", "workspace", "dimensions"] as const) {
    const nested = nestedPayloadWidth(payload[key]);
    if (nested !== null) return nested;
  }
  return null;
}

function parsedJson(text: string): unknown | undefined {
  if (text.trim() === "") return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

async function readStatuslinePayload(): Promise<unknown | null> {
  if (process.stdin.isTTY === true || process.stdin.readable === false) return null;
  process.stdin.setEncoding("utf8");
  const buffered = process.stdin.read();
  if (typeof buffered === "string") {
    const payload = parsedJson(buffered);
    if (payload !== undefined) return payload;
  }
  return new Promise((resolve) => {
    let input = typeof buffered === "string" ? buffered : "";
    let settled = false;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const finish = (payload: unknown | null): void => {
      if (settled) return;
      settled = true;
      if (timeout !== undefined) clearTimeout(timeout);
      process.stdin.off("data", onData);
      process.stdin.off("end", onEnd);
      process.stdin.pause();
      resolve(payload);
    };
    const onData = (chunk: string): void => {
      input += chunk;
      const payload = parsedJson(input);
      if (payload !== undefined) finish(payload);
    };
    const onEnd = (): void => finish(parsedJson(input) ?? null);
    process.stdin.on("data", onData);
    process.stdin.once("end", onEnd);
    process.stdin.resume();
    timeout = setTimeout(() => finish(parsedJson(input) ?? null), 100);
  });
}

async function statuslineWidth(): Promise<number> {
  const payloadWidth = statuslinePayloadWidth(await readStatuslinePayload());
  if (payloadWidth !== null) return payloadWidth;
  const stdoutWidth = process.stdout.isTTY ? saneWidth(process.stdout.columns) : null;
  if (stdoutWidth !== null) return stdoutWidth;
  const envWidth = saneWidth(process.env.COLUMNS);
  return envWidth ?? STATUSLINE_DEFAULT_WIDTH;
}

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
        enabled: true,
      },
      active: true,
      usage,
      error: null,
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      provider: provider.displayName,
      account: {
        name: "@live",
        email: null,
        accountId: null,
        label: "live login",
        enabled: true,
      },
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

function statuslineCacheFile(filter: ProviderFilter, width: number): string {
  return path.join(os.homedir(), ".lazyswitch", `statusline-cache-${filter}-${width}.json`);
}

function colorMode(): "ansi" | "plain" {
  return process.env.NO_COLOR === "1" ? "plain" : "ansi";
}

function cachedStatusline(filter: ProviderFilter, width: number): string | null {
  try {
    const parsed: unknown = JSON.parse(fs.readFileSync(statuslineCacheFile(filter, width), "utf8"));
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    const at = "at" in parsed ? parsed.at : null;
    const text = "text" in parsed ? parsed.text : null;
    const version = "version" in parsed ? parsed.version : null;
    const mode = "mode" in parsed ? parsed.mode : null;
    const cachedWidth = "width" in parsed ? parsed.width : null;
    if (typeof at !== "number" || typeof text !== "string") return null;
    if (version !== STATUSLINE_CACHE_VERSION) return null;
    if (mode !== colorMode()) return null;
    if (cachedWidth !== width) return null;
    return Date.now() - at <= STATUSLINE_CACHE_MS ? text : null;
  } catch {
    return null;
  }
}

function writeStatusline(filter: ProviderFilter, width: number, text: string): void {
  const file = statuslineCacheFile(filter, width);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    JSON.stringify({ at: Date.now(), text, mode: colorMode(), version: STATUSLINE_CACHE_VERSION, width }),
    "utf8"
  );
}

function help(): string {
  return [
    "Usage:",
    "  lazyswitch status              print account usage table once",
    "  lazyswitch watch [--interval N] keep the table visible",
    "  lazyswitch statusline [provider] print one compact line per account",
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
  const width = await statuslineWidth();
  const cached = cachedStatusline(filter, width);
  if (cached !== null) {
    console.log(cached);
    return;
  }
  const text = renderStatusline(await rows(filter), width);
  writeStatusline(filter, width, text);
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
