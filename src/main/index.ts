import {
  app,
  Tray,
  Menu,
  BrowserWindow,
  ipcMain,
  nativeImage,
  Notification,
  shell,
} from "electron";
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

if (process.platform === "win32") app.setAppUserModelId(APP_USER_MODEL_ID);

let tray: Tray | null = null;
let cfg: AppConfig = loadConfig();
let managerWin: BrowserWindow | null = null;
let onboardingWin: BrowserWindow | null = null;

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

function fmtWindow(
  w: UsageSnapshot["primary"],
  kind: "primary" | "secondary"
): string {
  const label = windowName(w?.windowMinutes, kind);
  if (!w) return T("tray.na", { label });
  const left = Math.max(0, 100 - w.usedPercent);
  const reset = w.resetsAt ? new Date(w.resetsAt).toLocaleTimeString() : "?";
  return T("tray.left", { label, pct: left.toFixed(0), time: reset });
}

function buildMenu(): Menu {
  const template: Electron.MenuItemConstructorOptions[] = [];

  for (const p of providers) {
    const st = stateOf(p);
    const accounts = p.listAccounts();
    if (accounts.length === 0 && !p.hasLiveAuth()) continue; // provider unused
    const active = p.activeAccountName();

    template.push({ label: p.displayName, enabled: false });
    template.push({
      label: st.lastUsage
        ? "  " + fmtWindow(st.lastUsage.primary, "primary")
        : "  " + T("tray.usageWaiting"),
      enabled: false,
    });
    if (st.lastUsage?.secondary) {
      template.push({
        label: "  " + fmtWindow(st.lastUsage.secondary, "secondary"),
        enabled: false,
      });
    }
    for (const a of accounts) {
      template.push({
        label: `  ${a.name === active ? "● " : "○ "}${accountDisplayName(a, a.name)}`,
        enabled: a.enabled,
        click: () => void manualSwitch(p, a.name),
      });
    }
    template.push({ type: "separator" });
  }

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

function refreshTray(): void {
  if (!tray) return;
  tray.setContextMenu(buildMenu());
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
    height: 640,
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
  onboardingWin.on("closed", () => (onboardingWin = null));
  onboardingWin.once("ready-to-show", () => {
    if (onboardingWin && !onboardingWin.isDestroyed()) {
      onboardingWin.show();
      onboardingWin.focus();
    }
  });
  onboardingWin.loadFile(rendererPath("onboarding.html"));
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
  ipcMain.handle("config:get", () => cfg);
  ipcMain.handle("config:set", (_e, patch: any) => {
    cfg = {
      ...cfg,
      ...patch,
      codex: { ...cfg.codex, ...(patch?.codex ?? {}) },
      claude: { ...cfg.claude, ...(patch?.claude ?? {}) },
    };
    saveConfig(cfg);
    if (patch && "launchAtLogin" in patch) applyLaunchAtLogin();
    if (patch && (patch.codex?.pollIntervalSec != null || patch.claude?.pollIntervalSec != null)) {
      restartMonitors(); // reschedule with the new interval
    }
    refreshTray();
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
  ensureCliStatusHooks();
  ensureWindowsToastShortcut();
  if (process.platform === "darwin") app.dock?.hide();
  tray = new Tray(trayIcon());
  tray.on("double-click", () => openManager());
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
  // The tray icon's registry key only exists after it's shown once; give it a
  // moment, then promote it to always-visible (Win11 best-effort).
  setTimeout(() => promoteTrayIcon(), 4000);
});

// Keep running as a tray-only app: a no-op listener suppresses the default quit.
app.on("window-all-closed", () => {
  /* stay alive in the tray */
});
