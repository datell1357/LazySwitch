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
const STATUS_DEFAULT_WIDTH = 80;
const STATUS_ACCOUNT_MAX_WIDTH = 20;
const STATUS_ACCOUNT_MIN_WIDTH = 2;
const STATUS_GAUGE_WITH_RESET_WIDTH = 12;
const STATUS_GAUGE_WITHOUT_RESET_WIDTH = 6;
const STATUS_ANSI_PATTERN = /\x1b\[[0-?]*[ -/]*[@-~]/g;
const STATUS_ZERO_WIDTH_PATTERN = /\p{Mark}/u;

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

function visibleWidth(text: string): number {
  return Array.from(text.replace(STATUS_ANSI_PATTERN, "")).reduce((width, character) => {
    if (character === "\u200d" || STATUS_ZERO_WIDTH_PATTERN.test(character)) return width;
    const codePoint = character.codePointAt(0) ?? 0;
    const isWide =
      (codePoint >= 0x1100 && codePoint <= 0x115f) ||
      (codePoint >= 0x2329 && codePoint <= 0x232a) ||
      (codePoint >= 0x2e80 && codePoint <= 0xa4cf) ||
      (codePoint >= 0xac00 && codePoint <= 0xd7a3) ||
      (codePoint >= 0xf900 && codePoint <= 0xfaff) ||
      (codePoint >= 0xfe10 && codePoint <= 0xfe6f) ||
      (codePoint >= 0xff00 && codePoint <= 0xff60) ||
      (codePoint >= 0xffe0 && codePoint <= 0xffe6) ||
      (codePoint >= 0x1f300 && codePoint <= 0x1faff);
    return width + (isWide ? 2 : 1);
  }, 0);
}

function statusGauge(window: PWindow | null, includeReset: boolean, interiorWidth: number): string {
  const used = window === null ? 0 : Math.max(0, Math.min(100, Math.round(window.usedPercent)));
  const label =
    window === null
      ? "n/a"
      : `${used}%${includeReset && window.resetsAt !== null ? ` ${resetText(window.resetsAt)}` : ""}`;
  const centeredLabel =
    label.length > interiorWidth
      ? label
      : window === null
        ? `${" ".repeat(Math.floor((interiorWidth - label.length) / 2))}${label}`.padEnd(interiorWidth, " ")
        : includeReset
          ? ` ${`${used}%`.padStart(4, " ")} ${window.resetsAt === null ? "" : resetText(window.resetsAt)}`.padEnd(interiorWidth, " ")
          : ` ${`${used}%`.padStart(4, " ")} `;
  const plain = `[${centeredLabel}]`;
  if (process.env.NO_COLOR === "1") return plain;
  const blockWidth = interiorWidth + 2;
  if (window === null) {
    const text = `${" ".repeat(Math.floor((blockWidth - label.length) / 2))}${label}`.padEnd(blockWidth, " ");
    return Array.from(text)
      .map((ch) => `${BG_GRAY}${FG}${ch}`)
      .join("") + RESET;
  }
  const text = centeredLabel.padEnd(blockWidth, " ");
  const filled = Math.round((used / 100) * blockWidth);
  return Array.from(text)
    .map((ch, index) => {
      const isFilled = index < filled;
      return `${isFilled ? bg(used) : BG_GRAY}${FG}${ch}`;
    })
    .join("") + RESET;
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

function statusAccountName(row: UsageRow): string {
  return row.account.name
    .replace(STATUS_ANSI_PATTERN, "")
    .replace(/[\u0000-\u001f\u007f-\u009f]/g, "");
}

function fitStatusAccountLabel(label: string, width: number): string {
  let fitted = label;
  if (visibleWidth(label) > width) {
    const suffix = width > 3 ? "..." : "";
    const targetWidth = width - visibleWidth(suffix);
    let result = "";
    for (const character of label) {
      const next = result + character;
      if (visibleWidth(next) > targetWidth) break;
      result = next;
    }
    fitted = `${result}${suffix}`;
  }
  return `${fitted}${" ".repeat(Math.max(0, width - visibleWidth(fitted)))}`;
}

function statusAccountLabel(row: UsageRow, width: number): string {
  return fitStatusAccountLabel(statusAccountName(row), width);
}

function statusAccountState(row: UsageRow): string {
  if (row.error !== null || row.usage === null) return "UNK";
  if (isResting(row.usage)) return "RST";
  return row.active ? "ACT" : "WAI";
}

function statusLineFor(row: UsageRow, labelWidth: number, includeReset: boolean): string {
  const gaugeWidth = includeReset ? STATUS_GAUGE_WITH_RESET_WIDTH : STATUS_GAUGE_WITHOUT_RESET_WIDTH;
  const accountPrefix =
    labelWidth === 0 ? statusAccountState(row) : `${statusAccountLabel(row, labelWidth)} ${statusAccountState(row)}`;
  const fable =
    row.provider === "Claude" ? ` Fable ${statusGauge(row.usage?.fable ?? null, includeReset, gaugeWidth)}` : "";
  return `${accountPrefix} 5H ${statusGauge(row.usage?.primary ?? null, includeReset, gaugeWidth)} Week ${statusGauge(row.usage?.secondary ?? null, includeReset, gaugeWidth)}${fable}`;
}

function statusLineLayout(rows: readonly UsageRow[], width: number): readonly string[] {
  const nameWidth = Math.max(
    ...rows.map((row) => Math.min(STATUS_ACCOUNT_MAX_WIDTH, Math.max(1, visibleWidth(statusAccountName(row)))))
  );
  const minimumLabelWidth = Math.min(STATUS_ACCOUNT_MIN_WIDTH, nameWidth);
  const fits = (labelWidth: number, includeReset: boolean): boolean =>
    rows.every((row) => visibleWidth(statusLineFor(row, labelWidth, includeReset)) <= width);

  if (fits(minimumLabelWidth, true)) {
    for (let labelWidth = nameWidth; labelWidth >= minimumLabelWidth; labelWidth -= 1) {
      if (fits(labelWidth, true)) return rows.map((row) => statusLineFor(row, labelWidth, true));
    }
  }
  for (let labelWidth = nameWidth; labelWidth >= minimumLabelWidth; labelWidth -= 1) {
    if (fits(labelWidth, false)) return rows.map((row) => statusLineFor(row, labelWidth, false));
  }
  if (minimumLabelWidth > 0 && fits(0, false)) {
    return rows.map((row) => statusLineFor(row, 0, false));
  }
  return rows.map((row) => statusLineFor(row, minimumLabelWidth, false));
}

/** Keep provider blocks intact while putting the active account first in each block. */
export function orderStatusRows(rows: readonly UsageRow[]): readonly UsageRow[] {
  const ordered: UsageRow[] = [];
  for (let index = 0; index < rows.length;) {
    const provider = rows[index].provider;
    let end = index + 1;
    while (end < rows.length && rows[end].provider === provider) end += 1;
    const block = rows.slice(index, end);
    ordered.push(...block.filter((row) => row.active), ...block.filter((row) => !row.active));
    index = end;
  }
  return ordered;
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

export function renderStatusline(items: readonly UsageRow[], width = STATUS_DEFAULT_WIDTH): string {
  if (items.length === 0) return "LazySwitch: no accounts";
  const budget = Number.isFinite(width) ? Math.max(1, Math.floor(width)) : STATUS_DEFAULT_WIDTH;
  return statusLineLayout(orderStatusRows(items), budget).join("\n");
}
