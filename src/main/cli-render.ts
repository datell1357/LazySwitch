import type { PAccount, PUsage, PWindow } from "./providers/types";

export type UsageRow = {
  readonly provider: string;
  readonly account: PAccount;
  readonly active: boolean;
  readonly usage: PUsage | null;
  readonly error: string | null;
};

const RESET = "\x1b[0m";
const FG = "\x1b[97m";
const BG_GREEN = "\x1b[42m";
const BG_YELLOW = "\x1b[43m";
const BG_RED = "\x1b[41m";
const BG_GRAY = "\x1b[100m";
const WINDOW_WIDTH = 25;
const STATUS_GAUGE_WIDTH = 12;

function pad(text: string, width: number): string {
  const clipped =
    text.length > width ? text.slice(0, Math.max(0, width - 3)) + "..." : text;
  return clipped.padEnd(width, " ");
}

function resetText(resetsAt: number | null): string {
  if (resetsAt === null) return "";
  const mins = Math.max(0, Math.round((resetsAt - Date.now()) / 60000));
  const hours = Math.floor(mins / 60);
  const minutes = mins % 60;
  if (hours >= 48) return `${Math.floor(hours / 24)}d${hours % 24}h`;
  if (hours > 0) return `${hours}h${minutes}m`;
  return `${minutes}m`;
}

function bg(used: number): string {
  if (used >= 90) return BG_RED;
  if (used >= 70) return BG_YELLOW;
  return BG_GREEN;
}

function gauge(window: PWindow | null, width: number): string {
  if (window === null) {
    const text = pad(" unavailable", width);
    return process.env.NO_COLOR === "1" ? text : `${BG_GRAY}${FG}${text}${RESET}`;
  }
  const used = Math.max(0, Math.min(100, Math.round(window.usedPercent)));
  const filled = Math.round((used / 100) * width);
  const label = `${used}% ${resetText(window.resetsAt)}`.trim();
  const text = pad(`${" ".repeat(Math.max(0, Math.floor((width - label.length) / 2)))}${label}`, width);
  if (process.env.NO_COLOR === "1") {
    const blocks = Math.round((used / 100) * 10);
    return `[${"#".repeat(blocks)}${"-".repeat(10 - blocks)}] ${label}`;
  }
  return Array.from(text)
    .map((ch, index) => `${index < filled ? bg(used) : BG_GRAY}${FG}${ch}`)
    .join("") + RESET;
}

function optionalGauge(window: PWindow | null | undefined, width: number): string {
  if (window === undefined) return pad("", width);
  return gauge(window, width);
}

function isResting(usage: PUsage | null): boolean {
  if (usage === null) return false;
  const windows = [usage.primary, usage.secondary, usage.fable ?? null].filter((w) => w !== null);
  return windows.some((w) => w.usedPercent >= 100);
}

function accountState(row: UsageRow): string {
  if (row.error !== null || row.usage === null) return "UNKNOWN";
  if (isResting(row.usage)) return "RESTING";
  return row.active ? "ACTIVE" : "WAITING";
}

export function renderTable(items: readonly UsageRow[]): string {
  const showFable = items.some((row) => row.usage?.fable !== undefined);
  const lines = [
    "LazySwitch",
    "",
    `${pad("", 2)}${pad("provider", 8)} ${pad("account", 24)} ${pad("state", 10)} ${pad("5H", WINDOW_WIDTH)} ${pad("Week", WINDOW_WIDTH)}${showFable ? " " + pad("Fable", WINDOW_WIDTH) : ""}`,
  ];
  for (const row of items) {
    const marker = row.active ? ">" : " ";
    const account = row.account.email ?? row.account.label ?? row.account.name;
    lines.push(
      `${marker} ${pad(row.provider, 8)} ${pad(account, 24)} ${pad(accountState(row), 10)} ${gauge(row.usage?.primary ?? null, WINDOW_WIDTH)} ${gauge(row.usage?.secondary ?? null, WINDOW_WIDTH)}${showFable ? " " + optionalGauge(row.usage?.fable, WINDOW_WIDTH) : ""}`
    );
  }
  if (items.length === 0) lines.push("  No enrolled accounts.");
  return lines.join("\n");
}

export function renderStatusline(items: readonly UsageRow[]): string {
  if (items.length === 0) return "LazySwitch: no accounts";
  return items
    .map((row) => {
      const name = row.account.email ?? row.account.name;
      const fable =
        row.usage?.fable === undefined ? "" : ` Fable ${gauge(row.usage.fable, STATUS_GAUGE_WIDTH)}`;
      return `${row.provider}:${name} ${accountState(row)} 5H ${gauge(row.usage?.primary ?? null, STATUS_GAUGE_WIDTH)} Week ${gauge(row.usage?.secondary ?? null, STATUS_GAUGE_WIDTH)}${fable}`;
    })
    .join(" | ");
}
