import { app } from "electron";
import * as fs from "fs";
import * as path from "path";
import { ProviderPrefs } from "./providers/types";

export { ProviderPrefs };

export interface UsageWidgetConfig {
  /** The persistent usage widget is shown. */
  enabled: boolean;
  /** Last user-chosen position/size; null coordinates fall back to a default corner. */
  x: number | null;
  y: number | null;
  width: number;
  height: number;
  alwaysOnTop: boolean;
  compactPosition: "taskbar" | "bottom-right" | "bottom-left";
  minimized: boolean;
  hiddenAccounts: readonly string[];
}

export interface AppConfig {
  /** UI language: "" = follow system; "ko" | "en" | "ja" | "zh". */
  language: string;
  /** Launch automatically at OS login (registered on first run). */
  launchAtLogin: boolean;
  /** The first-run tutorial has been completed; do not open it again. */
  onboarded: boolean;
  codex: ProviderPrefs;
  claude: ProviderPrefs;
  usageWidget: UsageWidgetConfig;
}

const USAGE_WIDGET_DEFAULTS: UsageWidgetConfig = {
  enabled: true,
  x: null,
  y: null,
  width: 354,
  height: 563,
  alwaysOnTop: true,
  compactPosition: "taskbar",
  minimized: false,
  hiddenAccounts: [],
};

const CODEX_DEFAULTS: ProviderPrefs = {
  autoApprove: false,
  autoRestartCli: true,
  desktopAppPath: "",
  desktopProcessName: process.platform === "win32" ? "Codex.exe" : "Codex",
  rotationOrder: [],
  primaryMinLeftPct: 5,
  weeklyMinLeftPct: 1,
  pollIntervalSec: 30,
};

const CLAUDE_DEFAULTS: ProviderPrefs = {
  autoApprove: false, // no desktop restart for Claude — kept for shape parity
  autoRestartCli: true,
  desktopAppPath: "",
  desktopProcessName: "",
  rotationOrder: [],
  primaryMinLeftPct: 5,
  weeklyMinLeftPct: 1,
  pollIntervalSec: 300, // usage API rate-limits below ~5 min
};

const DEFAULTS: AppConfig = {
  language: "",
  launchAtLogin: true,
  onboarded: false,
  codex: CODEX_DEFAULTS,
  claude: CLAUDE_DEFAULTS,
  usageWidget: USAGE_WIDGET_DEFAULTS,
};

/** Legacy flat keys (pre multi-provider) that map into the codex section. */
const LEGACY_CODEX_KEYS: Array<keyof ProviderPrefs> = [
  "autoApprove",
  "autoRestartCli",
  "desktopAppPath",
  "desktopProcessName",
  "rotationOrder",
  "primaryMinLeftPct",
  "weeklyMinLeftPct",
  "pollIntervalSec",
];

function migrate(raw: any): Partial<AppConfig> {
  if (!raw || typeof raw !== "object") return {};
  const out: any = {};
  if (typeof raw.language === "string") out.language = raw.language;
  if (typeof raw.launchAtLogin === "boolean") out.launchAtLogin = raw.launchAtLogin;
  // An existing config predates the tutorial, so its owner has already set the
  // app up by hand — don't greet them with a first-run wizard on upgrade.
  out.onboarded = typeof raw.onboarded === "boolean" ? raw.onboarded : true;

  // New shape takes precedence; otherwise lift legacy flat keys into codex.
  const legacyCodex: any = {};
  for (const k of LEGACY_CODEX_KEYS) {
    if (raw[k] !== undefined) legacyCodex[k] = raw[k];
  }
  out.codex = { ...CODEX_DEFAULTS, ...legacyCodex, ...(raw.codex ?? {}) };
  out.claude = { ...CLAUDE_DEFAULTS, ...(raw.claude ?? {}) };
  out.usageWidget = { ...USAGE_WIDGET_DEFAULTS, ...(raw.usageWidget ?? {}) };
  if (out.usageWidget.compactPosition !== "taskbar" &&
      out.usageWidget.compactPosition !== "bottom-right" &&
      out.usageWidget.compactPosition !== "bottom-left") {
    out.usageWidget.compactPosition = "taskbar";
  }
  return out;
}

function configPath(): string {
  return path.join(app.getPath("userData"), "config.json");
}

export function loadConfig(): AppConfig {
  try {
    // Strip a UTF-8 BOM if present (e.g. config hand-written via PowerShell).
    const raw = fs.readFileSync(configPath(), "utf8").replace(/^﻿/, "");
    return { ...DEFAULTS, ...migrate(JSON.parse(raw)) };
  } catch {
    return {
      ...DEFAULTS,
      codex: { ...CODEX_DEFAULTS },
      claude: { ...CLAUDE_DEFAULTS },
      usageWidget: { ...USAGE_WIDGET_DEFAULTS },
    };
  }
}

export function saveConfig(cfg: AppConfig): void {
  fs.mkdirSync(path.dirname(configPath()), { recursive: true });
  fs.writeFileSync(configPath(), JSON.stringify(cfg, null, 2), "utf8");
}
