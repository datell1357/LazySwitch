import {
  app,
  Tray,
  Menu,
  BrowserWindow,
  ipcMain,
  nativeImage,
  Notification,
  nativeTheme,
  screen,
  shell,
} from "electron";
import { execFile } from "child_process";
import * as fs from "fs";
import * as path from "path";
import { loadConfig, saveConfig, AppConfig, ProviderPrefs } from "./config";
import { Provider, PAccount } from "./providers/types";
import { codexProvider } from "./providers/codex";
import { claudeProvider } from "./providers/claude";
import { UsageMonitor, LimitReason, UsageSnapshot } from "./monitor";
import { switchTo, pickNextAccount, isExhausted, exhaustedUntil } from "./switcher";
import { promoteTrayIcon } from "./tray-pin";
import { resolveLang, t as translate } from "./i18n";
import { createCliHandover } from "./cli-handover";
import { installHooks } from "./cli-hooks";
import { showAppNotification } from "./app-notify";
import type { CliSession } from "./cli-sessions";

const providers: Provider[] = [codexProvider, claudeProvider];
const APP_USER_MODEL_ID = "com.local.lazyswitch";
const DEFAULT_WIDGET_BACKGROUND = "#16171b";

if (process.platform === "win32") app.setAppUserModelId(APP_USER_MODEL_ID);

let tray: Tray | null = null;
let trayMenu: Menu | null = null;
let cfg: AppConfig = loadConfig();
let managerWin: BrowserWindow | null = null;
let onboardingWin: BrowserWindow | null = null;
let widgetWin: BrowserWindow | null = null;
let widgetSettingsWin: BrowserWindow | null = null;
let openingUsageWidget: Promise<void> | null = null;

/** Per-provider runtime state. */
interface PState {
  provider: Provider;
  monitor: UsageMonitor | null;
  lastUsage: UsageSnapshot | null;
  /** Accounts whose window is spent; name -> epoch ms when it frees up again. */
  coolingDown: Map<string, number>;
  switching: boolean;
  handlingLimit: boolean;
  lastNoAccountNotify: number;
}
const states = new Map<string, PState>(
  providers.map((p) => [
    p.id,
    {
      provider: p,
      monitor: null,
      lastUsage: null,
      coolingDown: new Map(),
      switching: false,
      handlingLimit: false,
      lastNoAccountNotify: 0,
    },
  ])
);
const pendingUsageRefreshes = new Set<string>();

function prefsOf(p: Provider): ProviderPrefs {
  return cfg[p.id];
}
function stateOf(p: Provider): PState {
  return states.get(p.id)!;
}
function providerById(id: string): Provider {
  const p = providers.find((x) => x.id === id);
  if (!p) throw new Error(`Unknown provider "${id}"`);
  return p;
}

function pruneCooldowns(st: PState): void {
  const now = Date.now();
  for (const [name, until] of st.coolingDown)
    if (until <= now) st.coolingDown.delete(name);
}

function rendererPath(file: string): string {
  return path.join(__dirname, "..", "..", "src", "renderer", file);
}

function trayIcon() {
  // Fallback to an empty image if no asset is bundled; replace with a real .png.
  const p = path.join(__dirname, "..", "..", "assets", "tray.png");
  const img = nativeImage.createFromPath(p);
  return img.isEmpty() ? nativeImage.createEmpty() : img;
}

function notify(title: string, body: string): void {
  if (Notification.isSupported()) new Notification({ title, body }).show();
  showAppNotification({ title, body });
}

function quoteShortcutArg(value: string): string {
  return `"${value.replace(/"/g, '\\"')}"`;
}

function shortcutMatches(
  actual: Electron.ShortcutDetails,
  expected: Electron.ShortcutDetails
): boolean {
  return (
    path.normalize(actual.target).toLowerCase() ===
      path.normalize(expected.target).toLowerCase() &&
    actual.args === expected.args &&
    actual.appUserModelId === expected.appUserModelId
  );
}

function ensureWindowsToastShortcut(): void {
  if (process.platform !== "win32") return;
  const appData = process.env.APPDATA;
  if (!appData) return;

  const shortcutPath = path.join(
    appData,
    "Microsoft",
    "Windows",
    "Start Menu",
    "Programs",
    "LazySwitch.lnk"
  );
  const details: Electron.ShortcutDetails = {
    target: process.execPath,
    args: quoteShortcutArg(app.getAppPath()),
    appUserModelId: APP_USER_MODEL_ID,
    description: "LazySwitch",
  };
  const exists = fs.existsSync(shortcutPath);
  let stale = !exists;

  if (exists) {
    try {
      stale = !shortcutMatches(shell.readShortcutLink(shortcutPath), details);
    } catch (error) {
      if (error instanceof Error) stale = true;
      else throw error;
    }
  }

  if (!stale) return;

  fs.mkdirSync(path.dirname(shortcutPath), { recursive: true });
  const operation = exists ? "replace" : "create";
  if (!shell.writeShortcutLink(shortcutPath, operation, details))
    console.warn(`Failed to write Windows notification shortcut: ${shortcutPath}`);
}

function accountDisplayName(
  account: PAccount | null | undefined,
  fallback: string
): string {
  return account?.email || account?.name || fallback;
}

function accountDisplayLabel(account: PAccount | null | undefined): string {
  if (!account?.label) return "";
  if (!account.email) return account.label;
  return account.label
    .split(" · ")
    .filter((part) => part !== account.email)
    .join(" · ");
}

/** Translate with the currently configured UI language. */
function T(key: string, vars?: Record<string, string | number>): string {
  return translate(resolveLang(cfg.language), key, vars);
}

const cliHandover = createCliHandover({
  getLang: () => resolveLang(cfg.language),
  getPrefs: prefsOf,
  notify,
  t: T,
});

/**
 * Human name for a rate-limit window from its real length: "5h session",
 * "Weekly", "Monthly" (free plan), … Falls back to a generic name.
 */
function windowName(
  mins: number | null | undefined,
  kind: "primary" | "secondary"
): string {
  if (mins == null) return T(kind === "primary" ? "win.session" : "win.week");
  const h = Math.round(mins / 60);
  if (h >= 24 * 28) return T("win.month");
  if (h >= 24 * 6 && h <= 24 * 8) return T("win.week");
  if (h >= 24) return T("win.days", { d: Math.round(h / 24) });
  return T("win.hours", { h });
}

function buildMenu(): Menu {
  const template: Electron.MenuItemConstructorOptions[] = [];

  const langItem = (label: string, value: string) => ({
    label,
    type: "radio" as const,
    checked: (cfg.language || "") === value,
    click: () => {
      cfg.language = value;
      saveConfig(cfg);
      refreshTray();
      broadcastChanged();
    },
  });

  template.push(
    { label: T("tray.manage"), click: () => openManager() },
    { label: T("tray.tutorial"), click: () => openOnboarding() },
    {
      label: T("tray.autoApprove"),
      type: "checkbox",
      checked: cfg.codex.autoApprove,
      click: (item) => {
        cfg.codex.autoApprove = item.checked;
        saveConfig(cfg);
      },
    },
    {
      label: T("tray.autoRestartCli", { provider: cliHandover.providerName(codexProvider) }),
      type: "checkbox",
      checked: cfg.codex.autoRestartCli,
      click: (item) => {
        cfg.codex.autoRestartCli = item.checked;
        saveConfig(cfg);
      },
    },
    {
      label: T("tray.autoRestartCli", { provider: cliHandover.providerName(claudeProvider) }),
      type: "checkbox",
      checked: cfg.claude.autoRestartCli,
      click: (item) => {
        cfg.claude.autoRestartCli = item.checked;
        saveConfig(cfg);
      },
    },
    {
      label: T("tray.startAtLogin"),
      type: "checkbox",
      checked: cfg.launchAtLogin,
      click: (item) => {
        cfg.launchAtLogin = item.checked;
        saveConfig(cfg);
        applyLaunchAtLogin();
      },
    },
    {
      label: T("tray.usageWidget"),
      type: "checkbox",
      checked: cfg.usageWidget.enabled,
      click: (item) => setUsageWidgetEnabled(item.checked),
    },
    {
      label: T("tray.language"),
      submenu: [
        langItem(T("tray.langSystem"), ""),
        langItem("한국어", "ko"),
        langItem("English", "en"),
        langItem("日本語", "ja"),
        langItem("中文", "zh"),
      ],
    },
    { type: "separator" },
    { label: T("tray.quit"), role: "quit" }
  );

  return Menu.buildFromTemplate(template);
}

// Electron never reports a menu's size, so the "above the icon" placement below
// works off the measured size of this menu and clamps into the work area — a
// stale estimate then shifts the menu slightly instead of pushing it off-screen.
const TRAY_MENU_WIDTH = 352;
const TRAY_MENU_HEIGHT = 317;

const TRAY_MENU_GAP = 8;

/** The compact widget's rect, when it is parked where the tray menu wants to go. */
function bottomRightCompactWidgetRect(): Electron.Rectangle | null {
  if (!widgetWin || widgetWin.isDestroyed()) return null;
  if (!cfg.usageWidget.minimized || cfg.usageWidget.compactPosition !== "bottom-right") return null;
  return widgetWin.getBounds();
}

/** Top-left corner that puts the tray menu centred directly above the icon. */
function trayMenuPosition(): { x: number; y: number } | null {
  if (!tray) return null;
  const icon = tray.getBounds();
  if (icon.width === 0 && icon.height === 0) return null;
  const workArea = screen.getDisplayNearestPoint({ x: icon.x, y: icon.y }).workArea;
  const clampX = (value: number) =>
    Math.max(workArea.x, Math.min(value, workArea.x + workArea.width - TRAY_MENU_WIDTH));
  const y = Math.max(
    workArea.y,
    Math.min(icon.y - TRAY_MENU_HEIGHT, workArea.y + workArea.height - TRAY_MENU_HEIGHT)
  );
  let x = clampX(Math.round(icon.x + icon.width / 2 - TRAY_MENU_WIDTH / 2));

  // A bottom-right compact widget sits exactly where the menu opens, so slide
  // the menu clear of it instead of dropping it on top.
  const widget = bottomRightCompactWidgetRect();
  const overlaps =
    widget !== null &&
    x < widget.x + widget.width &&
    x + TRAY_MENU_WIDTH > widget.x &&
    y < widget.y + widget.height &&
    y + TRAY_MENU_HEIGHT > widget.y;
  if (widget !== null && overlaps) x = clampX(widget.x - TRAY_MENU_GAP - TRAY_MENU_WIDTH);

  return { x, y };
}

function showTrayMenu(): void {
  if (!tray || !trayMenu) return;
  const position = trayMenuPosition();
  if (position) tray.popUpContextMenu(trayMenu, position);
  else tray.popUpContextMenu(trayMenu);
}

function refreshTray(): void {
  if (!tray) return;
  trayMenu = buildMenu();
  // Letting the shell own the menu anchors it to the cursor, so it sprawls to
  // the side of the icon; on Windows we pop it up ourselves instead.
  if (process.platform === "win32") tray.setContextMenu(null);
  else tray.setContextMenu(trayMenu);
  const actives = providers
    .map((p) => {
      const active = p.activeAccountName();
      if (!active) return null;
      const account = p.listAccounts().find((a) => a.name === active);
      return `${p.displayName}: ${accountDisplayName(account, active)}`;
    })
    .filter(Boolean)
    .join(" · ");
  tray.setToolTip(actives || "LazySwitch");
}

async function manualSwitch(p: Provider, name: string): Promise<void> {
  const st = stateOf(p);
  if (st.switching) return;
  st.switching = true;
  try {
    const prefs = prefsOf(p);
    const cliSessions = await cliHandover.detect(p);
    const accounts = p.listAccounts();
    const displayName = (slot: string | null | undefined): string =>
      slot
        ? accountDisplayName(
            accounts.find((a) => a.name === slot),
            slot
          )
        : "?";
    // Manual switch = explicit user action; restart Desktop right away (codex).
    const res = await switchTo(p, name, prefs, { restartDesktop: false });
    let desktopRestarted = false;
    try {
      if (p.desktop) desktopRestarted = await p.desktop.restart(prefs);
      notify(
        `${p.displayName} — ${T("notif.switchedTitle")}`,
        T("notif.manualSwitched", {
          from: displayName(res.from),
          to: displayName(res.to),
          restarted: desktopRestarted ? T("notif.restartedSuffix") : "",
        })
      );
    } finally {
      void cliHandover.schedule(p, cliSessions);
    }
  } catch (e) {
    notify(`${p.displayName} — ${T("notif.switchFailed")}`, String(e));
  } finally {
    st.switching = false;
    broadcastChanged();
  }
}

/** Show the approval popup; resolves true if the user approves. */
function askApproval(
  p: Provider,
  from: PAccount | null,
  to: PAccount,
  reason: LimitReason
) {
  return new Promise<boolean>((resolve) => {
    const st = stateOf(p);
    const win = new BrowserWindow({
      width: 440,
      height: 420,
      resizable: false,
      minimizable: false,
      maximizable: false,
      alwaysOnTop: true,
      frame: false,
      transparent: true,
      title: T("popup.title"),
      webPreferences: {
        preload: path.join(__dirname, "preload.js"),
        contextIsolation: true,
      },
    });
    const w =
      reason.kind === "threshold"
        ? reason.window === "primary"
          ? st.lastUsage?.primary
          : st.lastUsage?.secondary
        : null;
    win.loadFile(rendererPath("approval.html"), {
      query: {
        lang: resolveLang(cfg.language),
        provider: p.displayName,
        fromName: accountDisplayName(from, ""),
        fromLabel: accountDisplayLabel(from),
        toName: accountDisplayName(to, to.name),
        toLabel: accountDisplayLabel(to),
        kind: reason.kind,
        windowLabel:
          reason.kind === "error"
            ? T("popup.limitReached")
            : T("win.limit", {
                w: windowName(w?.windowMinutes, reason.window),
              }),
        barLabel:
          reason.kind === "threshold"
            ? windowName(w?.windowMinutes, reason.window)
            : "",
        percent: reason.kind === "threshold" ? String(reason.percent) : "",
        message: reason.kind === "error" ? reason.message : "",
        resetAt: w?.resetsAt ? String(w.resetsAt) : "",
      },
    });

    let settled = false;
    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
      ipcMain.removeListener("approval:respond", onRespond);
      if (!win.isDestroyed()) win.close();
      resolve(ok);
    };
    const onRespond = (_e: unknown, approved: boolean) => finish(approved);
    ipcMain.on("approval:respond", onRespond);
    win.on("closed", () => finish(false));
  });
}

async function onLimitHit(p: Provider, reason: LimitReason): Promise<void> {
  const st = stateOf(p);
  if (st.switching || st.handlingLimit) return;
  st.handlingLimit = true;
  try {
    await handleLimit(p, reason);
  } finally {
    st.handlingLimit = false;
  }
}

async function handleLimit(p: Provider, reason: LimitReason): Promise<void> {
  const st = stateOf(p);
  const prefs = prefsOf(p);
  pruneCooldowns(st);
  console.log(
    `[limit:${p.id}]`,
    JSON.stringify(reason),
    "active=",
    p.activeAccountName(),
    "cooling=",
    [...st.coolingDown.keys()]
  );

  // Park the current account until its window resets.
  const active = p.activeAccountName();
  if (active && reason.kind === "threshold") {
    const w =
      reason.window === "primary"
        ? st.lastUsage?.primary
        : st.lastUsage?.secondary;
    if (w?.resetsAt) st.coolingDown.set(active, w.resetsAt);
  } else if (active) {
    // Error with no reset info: cool down for the 5h window length by default.
    st.coolingDown.set(active, Date.now() + 5 * 60 * 60 * 1000);
  }

  // The cached-usage check in pickNextAccount misses accounts whose usage
  // was never fetched this run (fresh app start). Verify each candidate with
  // a live fetch before committing — switching to a 0%-left account just
  // bounces straight back here.
  let next = pickNextAccount(p, prefs, st.coolingDown);
  while (next) {
    const usage = await p.fetchUsage(next.name).catch(() => null);
    if (!isExhausted(usage, prefs)) break;
    const until = exhaustedUntil(usage, prefs) ?? Date.now() + 15 * 60_000;
    st.coolingDown.set(next.name, until);
    console.log(`[limit:${p.id}] skipping exhausted candidate`, next.name);
    next = pickNextAccount(p, prefs, st.coolingDown);
  }
  if (!next) {
    // Throttle: the monitor re-fires every poll tick while over the limit;
    // one nag per 15 minutes is plenty.
    if (Date.now() - st.lastNoAccountNotify > 15 * 60_000) {
      st.lastNoAccountNotify = Date.now();
      notify(
        `${p.displayName} — ${T("notif.noAccountTitle")}`,
        T("notif.noAccountBody")
      );
    }
    return;
  }
  st.lastNoAccountNotify = 0;
  const fromAcc = p.listAccounts().find((a) => a.name === active) ?? null;

  // Switch the live auth immediately; running CLI sessions are handed over
  // after the auth store has been swapped successfully.
  if (st.switching) return;
  st.switching = true;
  let cliSessions: readonly CliSession[] = [];
  try {
    cliSessions = await cliHandover.detect(p);
    console.log(`[limit:${p.id}] switching`, active, "->", next.name);
    await switchTo(p, next.name, prefs, { restartDesktop: false });
    notify(
      `${p.displayName} — ${T("notif.switchedTitle")}`,
      T("notif.switchedBody", {
        from: accountDisplayName(fromAcc, active ?? "?"),
        to: accountDisplayName(next, next.name),
      })
    );
  } catch (e) {
    notify(`${p.displayName} — ${T("notif.switchFailed")}`, String(e));
    return;
  } finally {
    st.switching = false;
    broadcastChanged();
  }

  // Desktop keeps the old token cached in memory, so applying the switch
  // there needs a full kill + relaunch — that is the only user-approved step.
  // Providers without a desktop app (Claude) are done at this point.
  try {
    if (p.desktop) {
      const approved =
        prefs.autoApprove || (await askApproval(p, fromAcc, next, reason));
      if (approved) {
        const ok = await p.desktop.restart(prefs);
        notify(
          ok
            ? `${p.displayName} — ${T("notif.desktopRestarted")}`
            : `${p.displayName} — ${T("notif.desktopFailed")}`,
          ok
            ? T("notif.desktopRestartedBody", {
                name: accountDisplayName(next, next.name),
              })
            : T("notif.desktopFailedBody")
        );
      }
    }
  } finally {
    void cliHandover.schedule(p, cliSessions);
  }
}

function wireMonitors(): void {
  for (const p of providers) {
    const st = stateOf(p);
    // Don't poll providers that have nothing enrolled and no live login.
    if (p.listAccounts().length === 0 && !p.hasLiveAuth()) continue;
    const m = new UsageMonitor(p, () => prefsOf(p));
    m.on("usage", (s: UsageSnapshot) => {
      st.lastUsage = s;
      refreshTray();
    });
    m.on("limit-hit", (r: LimitReason) => void onLimitHit(p, r));
    st.monitor = m;
    m.start();
  }
}

function restartMonitors(): void {
  for (const st of states.values()) {
    if (st.monitor) {
      st.monitor.stop();
      st.monitor.start();
    }
  }
}

function broadcastChanged(): void {
  if (managerWin && !managerWin.isDestroyed()) {
    managerWin.webContents.send("accounts:changed");
  }
  if (widgetWin && !widgetWin.isDestroyed()) {
    widgetWin.webContents.send("accounts:changed");
  }
  if (widgetSettingsWin && !widgetSettingsWin.isDestroyed()) {
    widgetSettingsWin.webContents.send("accounts:changed");
  }
  // Enrolling the first account (or removing the last one) flips whether the
  // widget has anything to show.
  syncUsageWidget();
  refreshTray();
}

function openManager(): void {
  if (managerWin && !managerWin.isDestroyed()) {
    // focus() alone does not surface a minimized/hidden window on Windows.
    if (managerWin.isMinimized()) managerWin.restore();
    managerWin.show();
    managerWin.focus();
    return;
  }
  managerWin = new BrowserWindow({
    width: 920,
    height: 730,
    resizable: true,
    frame: false,
    title: "Accounts",
    backgroundColor: "#16171b",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
    },
  });
  managerWin.on("closed", () => (managerWin = null));
  // Force-show once ready: when the app itself was launched hidden (e.g. a
  // SW_HIDE startup state inherited from the launcher), the window's default
  // first show can be swallowed and the manager never appears.
  managerWin.once("ready-to-show", () => {
    if (managerWin && !managerWin.isDestroyed()) {
      managerWin.show();
      managerWin.focus();
    }
  });
  managerWin.loadFile(rendererPath("manager.html"));
}

function openOnboarding(): void {
  if (onboardingWin && !onboardingWin.isDestroyed()) {
    if (onboardingWin.isMinimized()) onboardingWin.restore();
    onboardingWin.show();
    onboardingWin.focus();
    return;
  }
  onboardingWin = new BrowserWindow({
    width: 640,
    height: 560,
    resizable: false,
    frame: false,
    title: "LazySwitch",
    backgroundColor: "#16171b",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
    },
  });
  onboardingWin.on("closed", () => {
    onboardingWin = null;
    syncUsageWidget(); // the widget waits until the tutorial is done
  });
  onboardingWin.once("ready-to-show", () => {
    if (onboardingWin && !onboardingWin.isDestroyed()) {
      onboardingWin.show();
      onboardingWin.focus();
    }
  });
  onboardingWin.loadFile(rendererPath("onboarding.html"));
  syncUsageWidget(); // hide it while the tutorial is up
}

const WIDGET_MIN_WIDTH = 220;
const WIDGET_MIN_HEIGHT = 200;
const WIDGET_COMPACT_WIDTH = 280;
const WIDGET_COMPACT_DEFAULT_HEIGHT = 70;
const WIDGET_COMPACT_MIN_HEIGHT = 38;
const WM_CONTEXTMENU = 0x007b;
let widgetBoundsSaveTimer: ReturnType<typeof setTimeout> | null = null;
let widgetContextMenuHooked = false;
let widgetContextMenuOpen = false;
let compactPositionRequest = 0;

interface WidgetRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface PhysicalRect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

let cachedTrayNotifyRect: PhysicalRect | null = null;

interface WidgetTaskbarTheme {
  background: string;
  light: boolean;
}

let cachedTaskbarTheme: WidgetTaskbarTheme | null | undefined;
let taskbarThemeQuery: Promise<WidgetTaskbarTheme | null> | null = null;

function isCompactPosition(value: unknown): value is "taskbar" | "bottom-right" | "bottom-left" {
  return value === "taskbar" || value === "bottom-right" || value === "bottom-left";
}

const TRAY_RECT_POWERSHELL = `
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class LazySwitchTrayNative {
  [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern IntPtr FindWindowW(string lpClassName, string lpWindowName);
  [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern IntPtr FindWindowExW(IntPtr hWndParent, IntPtr hWndChildAfter, string lpszClass, string lpszWindow);
  [DllImport("user32.dll", SetLastError = true)]
  public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
$tray = [LazySwitchTrayNative]::FindWindowW('Shell_TrayWnd', $null)
$notify = [LazySwitchTrayNative]::FindWindowExW($tray, [IntPtr]::Zero, 'TrayNotifyWnd', $null)
$rect = New-Object LazySwitchTrayNative+RECT
if ($tray -eq [IntPtr]::Zero -or $notify -eq [IntPtr]::Zero -or -not [LazySwitchTrayNative]::GetWindowRect($notify, [ref]$rect)) { exit 1 }
[Console]::WriteLine((ConvertTo-Json @{ left = $rect.Left; top = $rect.Top; right = $rect.Right; bottom = $rect.Bottom } -Compress))
`.trim();

function encodePowerShell(script: string): string {
  return Buffer.from(script, "utf16le").toString("base64");
}

function readPhysicalRect(value: unknown): PhysicalRect | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  const values = [row.left, row.top, row.right, row.bottom];
  if (!values.every((item) => typeof item === "number" && Number.isFinite(item))) return null;
  const [left, top, right, bottom] = values as number[];
  if (right <= left || bottom <= top) return null;
  return { left, top, right, bottom };
}

function queryTrayNotifyRect(): Promise<PhysicalRect | null> {
  if (process.platform !== "win32") return Promise.resolve(null);
  return new Promise((resolve) => {
    execFile(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encodePowerShell(TRAY_RECT_POWERSHELL)],
      { timeout: 1_800, windowsHide: true, maxBuffer: 64 * 1024 },
      (error, stdout) => {
        if (error) {
          resolve(null);
          return;
        }
        try {
          const rect = readPhysicalRect(JSON.parse(String(stdout).trim()));
          if (rect) cachedTrayNotifyRect = rect;
          resolve(rect);
        } catch {
          resolve(null);
        }
      }
    );
  });
}

const TASKBAR_THEME_POWERSHELL = `
$personalize = Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize' -ErrorAction Stop
$dwm = Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\DWM' -ErrorAction Stop
[Console]::WriteLine((ConvertTo-Json @{
  SystemUsesLightTheme = [int]$personalize.SystemUsesLightTheme
  ColorPrevalence = [int]$dwm.ColorPrevalence
  AccentColor = [uint32]$dwm.AccentColor
} -Compress))
`.trim();

function readTaskbarTheme(value: unknown): WidgetTaskbarTheme | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  const systemUsesLightTheme = row.SystemUsesLightTheme;
  const colorPrevalence = row.ColorPrevalence;
  const accentColor = row.AccentColor;
  if (![systemUsesLightTheme, colorPrevalence, accentColor].every(
    (item) => typeof item === "number" && Number.isInteger(item) && Number.isFinite(item)
  )) return null;
  if (systemUsesLightTheme !== 0 && systemUsesLightTheme !== 1) return null;
  if (colorPrevalence !== 0 && colorPrevalence !== 1) return null;
  if ((accentColor as number) < 0 || (accentColor as number) > 0xffffffff) return null;

  let red: number;
  let green: number;
  let blue: number;
  if (colorPrevalence === 1) {
    const accent = accentColor as number;
    // AccentColor is stored as ABGR, so the low three bytes are R, G, B.
    const darken = (channel: number) => Math.round(channel * 0.82);
    red = darken(accent & 0xff);
    green = darken((accent >>> 8) & 0xff);
    blue = darken((accent >>> 16) & 0xff);
  } else if (systemUsesLightTheme === 1) {
    red = 0xf3;
    green = 0xf3;
    blue = 0xf3;
  } else {
    red = 0x20;
    green = 0x20;
    blue = 0x20;
  }
  const background = `#${[red, green, blue].map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
  const luminance = [red, green, blue].map((channel) => {
    const normalized = channel / 255;
    return normalized <= 0.03928
      ? normalized / 12.92
      : Math.pow((normalized + 0.055) / 1.055, 2.4);
  });
  const relativeLuminance = 0.2126 * luminance[0] + 0.7152 * luminance[1] + 0.0722 * luminance[2];
  return { background, light: relativeLuminance > 0.5 };
}

function queryTaskbarTheme(): Promise<WidgetTaskbarTheme | null> {
  if (process.platform !== "win32") return Promise.resolve(null);
  return new Promise((resolve) => {
    execFile(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encodePowerShell(TASKBAR_THEME_POWERSHELL)],
      { timeout: 1_800, windowsHide: true, maxBuffer: 64 * 1024 },
      (error, stdout) => {
        if (error) {
          resolve(null);
          return;
        }
        try {
          resolve(readTaskbarTheme(JSON.parse(String(stdout).trim())));
        } catch {
          resolve(null);
        }
      }
    );
  });
}

function getTaskbarTheme(): Promise<WidgetTaskbarTheme | null> {
  if (cachedTaskbarTheme !== undefined) return Promise.resolve(cachedTaskbarTheme);
  if (!taskbarThemeQuery) {
    taskbarThemeQuery = queryTaskbarTheme().then((theme) => {
      cachedTaskbarTheme = theme;
      return theme;
    }).finally(() => {
      taskbarThemeQuery = null;
    });
  }
  return taskbarThemeQuery;
}

function isTaskbarCompactWidget(): boolean {
  return cfg.usageWidget.minimized && cfg.usageWidget.compactPosition === "taskbar";
}

function sendWidgetTaskbarTheme(theme: WidgetTaskbarTheme | null): void {
  if (!widgetWin || widgetWin.isDestroyed()) return;
  widgetWin.setBackgroundColor(theme?.background ?? DEFAULT_WIDGET_BACKGROUND);
  widgetWin.webContents.send("widget:taskbar-theme", theme);
}

function applyWidgetTaskbarTheme(): void {
  if (!widgetWin || widgetWin.isDestroyed()) return;
  if (!isTaskbarCompactWidget()) {
    sendWidgetTaskbarTheme(null);
    return;
  }
  void getTaskbarTheme().then((theme) => {
    if (isTaskbarCompactWidget()) sendWidgetTaskbarTheme(theme);
  });
}

/**
 * Default widget position: flush with the bottom-right of the primary
 * display's work area. Saved positions still take precedence.
 */
function widgetDefaultBounds(): { x: number; y: number; width: number; height: number } {
  const workArea = screen.getPrimaryDisplay().workArea;
  const width = cfg.usageWidget.width;
  const height = cfg.usageWidget.height;
  const x = cfg.usageWidget.x ?? workArea.x + workArea.width - width;
  const y = cfg.usageWidget.y ?? workArea.y + workArea.height - height;
  return { x, y, width, height };
}

function widgetInitialCompactBounds(): { x: number; y: number; width: number; height: number } {
  return compactBottomRightBounds(WIDGET_COMPACT_DEFAULT_HEIGHT);
}

function clampWidgetBounds(bounds: { x: number; y: number; width: number; height: number }) {
  const displayBounds = screen.getPrimaryDisplay().bounds;
  const width = Math.min(bounds.width, displayBounds.width);
  const height = Math.min(bounds.height, displayBounds.height);
  return {
    x: Math.max(displayBounds.x, Math.min(bounds.x, displayBounds.x + displayBounds.width - width)),
    y: Math.max(displayBounds.y, Math.min(bounds.y, displayBounds.y + displayBounds.height - height)),
    width,
    height,
  };
}

function compactBottomRightBounds(height: number): WidgetRect {
  const workArea = screen.getPrimaryDisplay().workArea;
  return clampWidgetBounds({
    x: workArea.x + workArea.width - WIDGET_COMPACT_WIDTH,
    y: workArea.y + workArea.height - height,
    width: WIDGET_COMPACT_WIDTH,
    height,
  });
}

function compactBottomLeftBounds(height: number): WidgetRect {
  const workArea = screen.getPrimaryDisplay().workArea;
  return clampWidgetBounds({
    x: workArea.x,
    y: workArea.y + workArea.height - height,
    width: WIDGET_COMPACT_WIDTH,
    height,
  });
}

function isBottomTaskbar(): boolean {
  const display = screen.getPrimaryDisplay();
  const bounds = display.bounds;
  const workArea = display.workArea;
  return workArea.x === bounds.x && workArea.y === bounds.y &&
    workArea.width === bounds.width && workArea.height < bounds.height;
}

async function compactTaskbarBounds(height: number): Promise<WidgetRect | null> {
  if (!isBottomTaskbar()) return null;
  const trayRect = await queryTrayNotifyRect();
  if (!trayRect) return null;
  try {
    const trayDip = screen.screenToDipRect(null, {
      x: trayRect.left,
      y: trayRect.top,
      width: trayRect.right - trayRect.left,
      height: trayRect.bottom - trayRect.top,
    });
    const display = screen.getPrimaryDisplay();
    const bounds = display.bounds;
    const workArea = display.workArea;
    const stripTop = workArea.y + workArea.height;
    const stripHeight = bounds.y + bounds.height - stripTop;
    if (trayDip.x < bounds.x || trayDip.x > bounds.x + bounds.width || stripHeight <= 0) return null;
    const right = trayDip.x - 4;
    const y = height <= stripHeight
      ? stripTop + Math.round((stripHeight - height) / 2)
      : bounds.y + bounds.height - height;
    return clampWidgetBounds({
      x: right - WIDGET_COMPACT_WIDTH,
      y,
      width: WIDGET_COMPACT_WIDTH,
      height,
    });
  } catch {
    return null;
  }
}

async function positionCompactWidget(height: number): Promise<void> {
  if (!widgetWin || widgetWin.isDestroyed() || !cfg.usageWidget.minimized) return;
  const request = ++compactPositionRequest;
  const position = cfg.usageWidget.compactPosition;
  let bounds: WidgetRect | null;
  if (position === "taskbar") bounds = await compactTaskbarBounds(height) ?? compactBottomRightBounds(height);
  else if (position === "bottom-left") bounds = compactBottomLeftBounds(height);
  else bounds = compactBottomRightBounds(height);
  if (!bounds || request !== compactPositionRequest || !widgetWin || widgetWin.isDestroyed() || !cfg.usageWidget.minimized) return;
  widgetWin.setBounds(bounds);
}

function restoreWidgetBounds() {
  const bounds = cfg.usageWidget.x != null && cfg.usageWidget.y != null
    ? {
      x: cfg.usageWidget.x,
      y: cfg.usageWidget.y,
      width: cfg.usageWidget.width,
      height: cfg.usageWidget.height,
    }
    : widgetDefaultBounds();
  return clampWidgetBounds(bounds);
}

function saveWidgetBounds(): void {
  if (!widgetWin || widgetWin.isDestroyed()) return;
  if (cfg.usageWidget.minimized) return;
  const bounds = widgetWin.getBounds();
  cfg.usageWidget.x = bounds.x;
  cfg.usageWidget.y = bounds.y;
  cfg.usageWidget.width = bounds.width;
  cfg.usageWidget.height = bounds.height;
  saveConfig(cfg);
}

function scheduleSaveWidgetBounds(): void {
  if (widgetBoundsSaveTimer) clearTimeout(widgetBoundsSaveTimer);
  widgetBoundsSaveTimer = setTimeout(saveWidgetBounds, 400);
}

function showWidgetContextMenu(): void {
  if (!widgetWin || widgetWin.isDestroyed() || !cfg.usageWidget.minimized) return;
  const menu = Menu.buildFromTemplate([
    {
      label: T("widget.settings"),
      click: () => openWidgetSettings(),
    },
    {
      label: T("widget.maximize"),
      click: () => restoreUsageWidget(),
    },
    {
      label: T("widget.close"),
      click: () => setUsageWidgetEnabled(false),
    },
  ]);
  // The menu is itself a top-level window: re-raising the widget underneath it
  // would push it behind or dismiss it, so pause the topmost timer meanwhile.
  widgetContextMenuOpen = true;
  menu.popup({
    window: widgetWin,
    callback: () => {
      widgetContextMenuOpen = false;
    },
  });
}

function setWidgetContextMenuHooked(shouldHook: boolean): void {
  if (process.platform !== "win32") return;
  if (!widgetWin || widgetWin.isDestroyed()) {
    widgetContextMenuHooked = false;
    return;
  }
  if (shouldHook && !widgetContextMenuHooked) {
    // Drag regions consume renderer context-menu events, so hook WM_CONTEXTMENU on Windows.
    widgetWin.hookWindowMessage(WM_CONTEXTMENU, () => showWidgetContextMenu());
    widgetContextMenuHooked = true;
  } else if (!shouldHook && widgetContextMenuHooked) {
    widgetWin.unhookWindowMessage(WM_CONTEXTMENU);
    widgetContextMenuHooked = false;
  }
}

function restoreUsageWidget(): void {
  if (!widgetWin || widgetWin.isDestroyed()) return;
  cfg.usageWidget.minimized = false;
  saveConfig(cfg);
  applyWidgetMinimized(false);
  widgetWin.webContents.send("accounts:changed");
  refreshTray();
}

function openWidgetSettings(): void {
  if (widgetSettingsWin && !widgetSettingsWin.isDestroyed()) {
    widgetSettingsWin.show();
    widgetSettingsWin.focus();
    return;
  }
  const workArea = screen.getPrimaryDisplay().workArea;
  const width = 320;
  const height = 400;
  widgetSettingsWin = new BrowserWindow({
    x: workArea.x + Math.round((workArea.width - width) / 2),
    y: workArea.y + Math.round((workArea.height - height) / 2),
    width,
    height,
    resizable: false,
    frame: false,
    alwaysOnTop: true,
    title: T("widget.settings"),
    backgroundColor: "#16171b",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
    },
  });
  widgetSettingsWin.on("closed", () => (widgetSettingsWin = null));
  widgetSettingsWin.once("ready-to-show", () => {
    if (widgetSettingsWin && !widgetSettingsWin.isDestroyed()) {
      widgetSettingsWin.show();
      widgetSettingsWin.focus();
    }
  });
  widgetSettingsWin.loadFile(rendererPath("widget-settings.html"));
}

function openUsageWidget(): void {
  if (widgetWin && !widgetWin.isDestroyed()) {
    widgetWin.show();
    applyWidgetTaskbarTheme();
    return;
  }
  if (openingUsageWidget) return;
  openingUsageWidget = openUsageWidgetWindow().finally(() => {
    openingUsageWidget = null;
  });
}

async function openUsageWidgetWindow(): Promise<void> {
  const initialTheme = isTaskbarCompactWidget() ? await getTaskbarTheme() : null;
  if (!cfg.usageWidget.enabled || !hasEnrolledAccounts() || isOnboarding()) return;
  if (widgetWin && !widgetWin.isDestroyed()) {
    widgetWin.show();
    return;
  }
  const normalBounds = widgetDefaultBounds();
  const initialBounds = cfg.usageWidget.minimized ? widgetInitialCompactBounds() : normalBounds;
  const initialBackground = isTaskbarCompactWidget()
    ? initialTheme?.background ?? DEFAULT_WIDGET_BACKGROUND
    : DEFAULT_WIDGET_BACKGROUND;
  widgetWin = new BrowserWindow({
    ...initialBounds,
    minWidth: cfg.usageWidget.minimized ? WIDGET_COMPACT_WIDTH : WIDGET_MIN_WIDTH,
    minHeight: cfg.usageWidget.minimized ? WIDGET_COMPACT_MIN_HEIGHT : WIDGET_MIN_HEIGHT,
    resizable: !cfg.usageWidget.minimized,
    movable: !cfg.usageWidget.minimized,
    frame: false,
    alwaysOnTop: cfg.usageWidget.alwaysOnTop,
    skipTaskbar: true,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    title: "LazySwitch Usage",
    backgroundColor: initialBackground,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
    },
  });
  widgetWin.setAlwaysOnTop(cfg.usageWidget.alwaysOnTop, "screen-saver");
  const reassertWidgetAlwaysOnTop = () => {
    if (!widgetWin || widgetWin.isDestroyed() || !cfg.usageWidget.alwaysOnTop) return;
    // A plain re-set is a no-op while the topmost flag is already on, and the
    // taskbar re-enters the topmost band above us every time it is clicked.
    // Dropping and re-adding the flag re-inserts this window at the top.
    widgetWin.setAlwaysOnTop(false);
    widgetWin.setAlwaysOnTop(true, "screen-saver");
    widgetWin.moveTop();
  };
  widgetWin.on("blur", reassertWidgetAlwaysOnTop);
  widgetWin.on("show", reassertWidgetAlwaysOnTop);
  // The taskbar is itself topmost and jumps above the widget whenever it is
  // clicked; no window event fires on the unfocused widget when that happens,
  // so keep re-raising on a timer. Only the taskbar-pinned compact widget
  // overlaps it, and re-raising also outranks the user's other always-on-top
  // windows — so run the timer for that case alone.
  const topmostTimer = setInterval(() => {
    if (!cfg.usageWidget.minimized || cfg.usageWidget.compactPosition !== "taskbar") return;
    if (widgetContextMenuOpen) return;
    reassertWidgetAlwaysOnTop();
  }, 100);
  widgetWin.on("closed", () => clearInterval(topmostTimer));
  widgetWin.on("moved", scheduleSaveWidgetBounds);
  widgetWin.on("resized", scheduleSaveWidgetBounds);
  widgetWin.webContents.on("context-menu", (event) => {
    if (!cfg.usageWidget.minimized) return;
    event.preventDefault();
    showWidgetContextMenu();
  });
  widgetWin.on("close", () => setWidgetContextMenuHooked(false));
  widgetWin.on("closed", () => {
    setWidgetContextMenuHooked(false);
    widgetWin = null;
  });
  setWidgetContextMenuHooked(cfg.usageWidget.minimized);
  // Chromium persists per-origin zoom (file://) in the profile, so an
  // accidental Ctrl+wheel / Ctrl+- permanently shrinks every widget load.
  widgetWin.webContents.on("did-finish-load", () => {
    if (!widgetWin || widgetWin.isDestroyed()) return;
    widgetWin.webContents.setZoomFactor(1);
    void widgetWin.webContents.setVisualZoomLevelLimits(1, 1);
    applyWidgetTaskbarTheme();
  });
  widgetWin.loadFile(rendererPath("widget.html"));
  if (cfg.usageWidget.minimized) void applyWidgetMinimized(true);
}

function applyWidgetMinimized(minimized: boolean, compactHeight = WIDGET_COMPACT_DEFAULT_HEIGHT): void {
  if (!widgetWin || widgetWin.isDestroyed()) return;
  if (minimized) {
    applyWidgetTaskbarTheme();
    setWidgetContextMenuHooked(true);
    widgetWin.setMinimumSize(WIDGET_COMPACT_WIDTH, WIDGET_COMPACT_MIN_HEIGHT);
    widgetWin.setResizable(false);
    widgetWin.setMovable(false);
    void positionCompactWidget(compactHeight);
  } else {
    sendWidgetTaskbarTheme(null);
    setWidgetContextMenuHooked(false);
    widgetWin.setMinimumSize(WIDGET_MIN_WIDTH, WIDGET_MIN_HEIGHT);
    widgetWin.setResizable(true);
    widgetWin.setMovable(true);
    widgetWin.setBounds(restoreWidgetBounds());
    saveWidgetBounds();
  }
}

function closeUsageWidget(): void {
  if (widgetBoundsSaveTimer) {
    clearTimeout(widgetBoundsSaveTimer);
    widgetBoundsSaveTimer = null;
  }
  if (widgetWin && !widgetWin.isDestroyed()) {
    setWidgetContextMenuHooked(false);
    saveWidgetBounds();
    widgetWin.close();
  }
}

/** With nothing enrolled the widget would just be an empty box, so hold it back. */
function hasEnrolledAccounts(): boolean {
  return providers.some((p) => p.listAccounts().length > 0);
}

function isOnboarding(): boolean {
  return onboardingWin !== null && !onboardingWin.isDestroyed();
}

/**
 * Open or close the widget to match the setting. It stays away until an account
 * exists and the tutorial is out of the way — an empty box floating over the
 * first-run wizard helps nobody.
 */
function syncUsageWidget(): void {
  const shouldShow = cfg.usageWidget.enabled && hasEnrolledAccounts() && !isOnboarding();
  if (shouldShow) openUsageWidget();
  else if (widgetWin && !widgetWin.isDestroyed()) closeUsageWidget();
}

function setUsageWidgetEnabled(enabled: boolean): void {
  cfg.usageWidget.enabled = enabled;
  saveConfig(cfg);
  syncUsageWidget();
  refreshTray();
}

/** Live usage per account slot, so the manager can show every account at once. */
async function listWithUsage(p: Provider) {
  const st = stateOf(p);
  const active = p.activeAccountName();
  const accounts = p.listAccounts();
  const usages = accounts.map((a) => {
    const usageName = a.name === active ? null : a.name;
    const cached = p.cachedUsage?.(usageName) ?? null;
    const key = `${p.id}:${usageName === null ? "live" : "slot:" + usageName}`;
    if (!pendingUsageRefreshes.has(key)) {
      pendingUsageRefreshes.add(key);
      void p
        .fetchUsage(usageName)
        .then((latest) => {
          if (JSON.stringify(latest) !== JSON.stringify(cached)) broadcastChanged();
        })
        .catch(() => undefined)
        .finally(() => pendingUsageRefreshes.delete(key));
    }
    return cached;
  });
  return accounts.map((a, i) => ({
    ...a,
    active: a.name === active,
    coolingDownUntil: st.coolingDown.get(a.name) ?? null,
    usage: usages[i],
  }));
}

function registerIpc(): void {
  ipcMain.handle("providers:list", () =>
    providers.map((p) => ({
      id: p.id,
      displayName: p.displayName,
      hasLoginFlow: !!p.addViaLogin,
      hasDesktop: !!p.desktop,
    }))
  );
  ipcMain.handle("accounts:list", (_e, pid: string) =>
    listWithUsage(providerById(pid))
  );
  ipcMain.handle("accounts:switch", async (_e, pid: string, name: string) => {
    await manualSwitch(providerById(pid), name);
    broadcastChanged();
    return { ok: true };
  });
  ipcMain.handle("accounts:setEnabled", (event, pid: string, name: string, enabled: boolean) => {
    if (!managerWin || managerWin.isDestroyed() || event.sender !== managerWin.webContents) {
      return { ok: false, error: "manager window required" };
    }
    if (typeof name !== "string" || typeof enabled !== "boolean") {
      return { ok: false, error: "invalid account state" };
    }
    const provider = providerById(pid);
    if (!provider.listAccounts().some((account) => account.name === name)) {
      return { ok: false, error: "account is not enrolled" };
    }
    provider.setAccountEnabled(name, enabled);
    broadcastChanged();
    return { ok: true };
  });
  ipcMain.handle("accounts:remove", (_e, pid: string, name: string) => {
    providerById(pid).removeAccount(name);
    broadcastChanged();
    return { ok: true };
  });
  ipcMain.handle(
    "accounts:rename",
    (_e, pid: string, oldName: string, newName: string) => {
      try {
        providerById(pid).renameAccount(oldName, newName);
        broadcastChanged();
        return { ok: true };
      } catch (e) {
        return { ok: false, error: String(e) };
      }
    }
  );
  ipcMain.handle("accounts:importCurrent", (_e, pid: string, name?: string) => {
    try {
      const p = providerById(pid);
      p.syncLiveBackToSlot();
      const acc = p.importCurrent(name);
      broadcastChanged();
      return { ok: true, name: acc.name };
    } catch (e) {
      return { ok: false, error: String(e) };
    }
  });
  ipcMain.handle("accounts:addViaLogin", async (_e, pid: string) => {
    const p = providerById(pid);
    if (!p.addViaLogin) return { ok: false, error: "unsupported" };
    const res = await p.addViaLogin((url) => {
      if (managerWin && !managerWin.isDestroyed()) {
        managerWin.webContents.send("login:url", url);
      }
    });
    if (res.ok) broadcastChanged();
    return res;
  });
  // Dry-run of the CLI handover: detect + restart/resume running sessions
  // without touching any account. Lets the user verify resume works.
  ipcMain.handle("cli:testRestart", async (_e, pid: string) => {
    const p = providerById(pid);
    const sessions = await cliHandover.detect(p);
    const result = sessions.length > 0 ? await cliHandover.schedule(p, sessions) : null;
    return { ok: true, sessions: sessions.length, result };
  });
  ipcMain.handle("onboarding:finish", (_e, openAccounts: boolean) => {
    cfg.onboarded = true;
    saveConfig(cfg);
    onboardingWin?.close();
    if (openAccounts) openManager();
    return true;
  });
  ipcMain.handle("lang:get", () => resolveLang(cfg.language));
  ipcMain.handle("open:url", (_e, url: string) => shell.openExternal(url));
  ipcMain.handle("manager:close", () => managerWin?.close());
  ipcMain.handle("widget:close", () => setUsageWidgetEnabled(false));
  ipcMain.handle("widget-settings:close", (_event) => widgetSettingsWin?.close());
  ipcMain.on("widget:compact-height", (event, height: unknown) => {
    if (typeof height !== "number" || !Number.isFinite(height)) return;
    if (!widgetWin || widgetWin.isDestroyed() || event.sender !== widgetWin.webContents) return;
    if (!cfg.usageWidget.minimized) return;
    const nextHeight = Math.max(WIDGET_COMPACT_MIN_HEIGHT, Math.round(height));
    void positionCompactWidget(nextHeight);
  });
  ipcMain.handle("config:get", () => cfg);
  ipcMain.handle("config:set", (_e, patch: any) => {
    const widgetPatch = patch?.usageWidget;
    const previousEnabled = cfg.usageWidget.enabled;
    const previousMinimized = cfg.usageWidget.minimized;
    const previousCompactPosition = cfg.usageWidget.compactPosition;
    const nextMinimized = typeof widgetPatch?.minimized === "boolean"
      ? widgetPatch.minimized
      : previousMinimized;
    if (nextMinimized && !previousMinimized && widgetWin && !widgetWin.isDestroyed()) {
      const bounds = widgetWin.getBounds();
      cfg.usageWidget.x = bounds.x;
      cfg.usageWidget.y = bounds.y;
      cfg.usageWidget.width = bounds.width;
      cfg.usageWidget.height = bounds.height;
    }
    const nextUsageWidget = {
      ...cfg.usageWidget,
      ...(patch?.usageWidget ?? {}),
    };
    if (!isCompactPosition(nextUsageWidget.compactPosition)) {
      nextUsageWidget.compactPosition = "taskbar";
    }
    cfg = {
      ...cfg,
      ...patch,
      codex: { ...cfg.codex, ...(patch?.codex ?? {}) },
      claude: { ...cfg.claude, ...(patch?.claude ?? {}) },
      usageWidget: nextUsageWidget,
    };
    saveConfig(cfg);
    if (typeof patch?.usageWidget?.alwaysOnTop === "boolean" && widgetWin && !widgetWin.isDestroyed()) {
      widgetWin.setAlwaysOnTop(patch.usageWidget.alwaysOnTop, "screen-saver");
    }
    if (typeof widgetPatch?.enabled === "boolean" && widgetPatch.enabled !== previousEnabled) {
      setUsageWidgetEnabled(widgetPatch.enabled);
    }
    if (typeof widgetPatch?.minimized === "boolean" && widgetPatch.minimized !== previousMinimized) {
      applyWidgetMinimized(widgetPatch.minimized);
    } else if (isCompactPosition(widgetPatch?.compactPosition) && widgetPatch.compactPosition !== previousCompactPosition && cfg.usageWidget.minimized) {
      applyWidgetTaskbarTheme();
      void positionCompactWidget(widgetWin?.getBounds().height ?? WIDGET_COMPACT_DEFAULT_HEIGHT);
    }
    if (patch && "launchAtLogin" in patch) applyLaunchAtLogin();
    if (patch && (patch.codex?.pollIntervalSec != null || patch.claude?.pollIntervalSec != null)) {
      restartMonitors(); // reschedule with the new interval
    }
    broadcastChanged();
    return cfg;
  });
}

/** Register (or unregister) the app to start at OS login. */
function applyLaunchAtLogin(): void {
  try {
    app.setLoginItemSettings({
      openAtLogin: cfg.launchAtLogin,
      openAsHidden: true, // start straight to the tray, no window
      args: ["--hidden"],
    });
  } catch {
    /* not supported on this platform */
  }
}

/**
 * If a provider's live login isn't enrolled yet, enroll it automatically so
 * the current account shows up without any manual "import" step.
 */
function ensureLiveEnrolled(): void {
  for (const p of providers) {
    try {
      if (!p.hasLiveAuth() || p.activeAccountName()) continue;
      p.importCurrent();
    } catch {
      /* live auth may be absent; fine */
    }
  }
}

function ensureCliStatusHooks(): void {
  try {
    installHooks();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.warn("LazySwitch hook install failed", message);
  }
}

if (!app.requestSingleInstanceLock()) {
  app.quit();
}
app.on("second-instance", () => openManager());

app.whenReady().then(() => {
  nativeTheme.on("updated", () => {
    cachedTaskbarTheme = undefined;
    if (isTaskbarCompactWidget()) applyWidgetTaskbarTheme();
  });
  ensureCliStatusHooks();
  ensureWindowsToastShortcut();
  if (process.platform === "darwin") app.dock?.hide();
  tray = new Tray(trayIcon());
  tray.on("double-click", () => openManager());
  tray.on("right-click", () => showTrayMenu());
  screen.on("display-metrics-changed", () => {
    if (cfg.usageWidget.minimized && widgetWin && !widgetWin.isDestroyed()) {
      void positionCompactWidget(widgetWin.getBounds().height);
    }
  });
  registerIpc();
  applyLaunchAtLogin();
  ensureLiveEnrolled();
  refreshTray();
  wireMonitors();
  // First install: walk the user through what LazySwitch does and let them pick
  // the switch behaviour, ending on "add an account". Afterwards, still surface
  // the manager whenever there aren't enough accounts to rotate between, rather
  // than hiding in the tray with nothing to do.
  const total = providers.reduce((n, p) => n + p.listAccounts().length, 0);
  if (!cfg.onboarded) openOnboarding();
  else if (total < 2) openManager();
  syncUsageWidget();
  // The tray icon's registry key only exists after it's shown once; give it a
  // moment, then promote it to always-visible (Win11 best-effort).
  setTimeout(() => promoteTrayIcon(), 4000);
});

// Keep running as a tray-only app: a no-op listener suppresses the default quit.
app.on("window-all-closed", () => {
  /* stay alive in the tray */
});
