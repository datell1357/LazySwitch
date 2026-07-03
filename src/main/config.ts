import { app } from "electron";
import * as fs from "fs";
import * as path from "path";
import { ProviderPrefs } from "./providers/types";

export { ProviderPrefs };

export interface AppConfig {
  /** UI language: "" = follow system; "ko" | "en" | "ja" | "zh". */
  language: string;
  /** Launch automatically at OS login (registered on first run). */
  launchAtLogin: boolean;
  codex: ProviderPrefs;
  claude: ProviderPrefs;
}

const CODEX_DEFAULTS: ProviderPrefs = {
  autoApprove: false,
  desktopAppPath: "",
  desktopProcessName: process.platform === "win32" ? "Codex.exe" : "Codex",
  rotationOrder: [],
  primaryMinLeftPct: 5,
  weeklyMinLeftPct: 1,
  pollIntervalSec: 30,
};

const CLAUDE_DEFAULTS: ProviderPrefs = {
  autoApprove: false, // no desktop restart for Claude — kept for shape parity
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
  codex: CODEX_DEFAULTS,
  claude: CLAUDE_DEFAULTS,
};

/** Legacy flat keys (pre multi-provider) that map into the codex section. */
const LEGACY_CODEX_KEYS: Array<keyof ProviderPrefs> = [
  "autoApprove",
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

  // New shape takes precedence; otherwise lift legacy flat keys into codex.
  const legacyCodex: any = {};
  for (const k of LEGACY_CODEX_KEYS) {
    if (raw[k] !== undefined) legacyCodex[k] = raw[k];
  }
  out.codex = { ...CODEX_DEFAULTS, ...legacyCodex, ...(raw.codex ?? {}) };
  out.claude = { ...CLAUDE_DEFAULTS, ...(raw.claude ?? {}) };
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
    };
  }
}

export function saveConfig(cfg: AppConfig): void {
  fs.mkdirSync(path.dirname(configPath()), { recursive: true });
  fs.writeFileSync(configPath(), JSON.stringify(cfg, null, 2), "utf8");
}
