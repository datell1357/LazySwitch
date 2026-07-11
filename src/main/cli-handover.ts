import { BrowserWindow, clipboard, ipcMain } from "electron";
import * as path from "path";
import {
  detectCliSessions,
  restartCliSessions,
  resumeCommandFor,
} from "./cli-sessions";
import type { CliRestartResult, CliSession } from "./cli-sessions";
import type { Provider, ProviderPrefs } from "./providers/types";

type CliRestartAction = "restart" | "copy" | "later";

interface CliRestartPayload {
  readonly providerName: string;
  readonly resumeCommand: string;
  readonly sessions: readonly CliSession[];
}

interface CliHandoverDeps {
  readonly getLang: () => string;
  readonly getPrefs: (provider: Provider) => ProviderPrefs;
  readonly notify: (title: string, body: string) => void;
  readonly t: (key: string, vars?: Record<string, string | number>) => string;
}

export interface CliHandover {
  readonly providerName: (provider: Provider) => string;
  readonly detect: (provider: Provider) => Promise<CliSession[]>;
  readonly schedule: (
    provider: Provider,
    sessions: readonly CliSession[]
  ) => Promise<CliRestartResult | null>;
}

function rendererPath(file: string): string {
  return path.join(__dirname, "..", "..", "src", "renderer", file);
}

function preloadPath(): string {
  return path.join(__dirname, "preload.js");
}

function normalizeCliRestartAction(action: string): CliRestartAction {
  if (action === "restart" || action === "copy") return action;
  return "later";
}

function formatError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createCliHandover(deps: CliHandoverDeps): CliHandover {
  const payloads = new Map<number, CliRestartPayload>();

  function providerName(provider: Provider): string {
    return provider.id === "claude" ? "Claude Code" : "Codex CLI";
  }

  function notifyCliRestart(provider: Provider, result: CliRestartResult): void {
    const name = providerName(provider);
    deps.notify(
      `${provider.displayName} — ${deps.t("notif.cliRestartedTitle")}`,
      result.manual > 0
        ? deps.t("notif.cliRestartedManualBody", {
          provider: name,
          count: result.restarted,
          resumedInPlace: result.resumedInPlace,
          manual: result.manual,
        })
        : deps.t("notif.cliRestartedBody", {
          provider: name,
          count: result.restarted,
          resumedInPlace: result.resumedInPlace,
        })
    );
  }

  function askRestart(
    provider: Provider,
    sessions: readonly CliSession[]
  ): Promise<CliRestartAction> {
    return new Promise((resolve) => {
      const resume = resumeCommandFor(provider);
      const win = new BrowserWindow({
        width: 520,
        height: 520,
        resizable: false,
        minimizable: false,
        maximizable: false,
        alwaysOnTop: true,
        frame: false,
        transparent: true,
        title: deps.t("popup.cliTitle"),
        webPreferences: {
          preload: preloadPath(),
          contextIsolation: true,
        },
      });
      payloads.set(win.webContents.id, {
        providerName: providerName(provider),
        resumeCommand: resume.text,
        sessions,
      });
      win.loadFile(rendererPath("cli-restart.html"), {
        query: { lang: deps.getLang() },
      });

      let settled = false;
      const finish = (action: CliRestartAction) => {
        if (settled) return;
        settled = true;
        ipcMain.removeListener("cli-restart:respond", onRespond);
        payloads.delete(win.webContents.id);
        if (!win.isDestroyed()) win.close();
        resolve(action);
      };
      const onRespond = (event: Electron.IpcMainEvent, action: string) => {
        if (event.sender.id !== win.webContents.id) return;
        finish(normalizeCliRestartAction(action));
      };
      ipcMain.on("cli-restart:respond", onRespond);
      win.on("closed", () => finish("later"));
    });
  }

  async function handle(
    provider: Provider,
    sessions: readonly CliSession[]
  ): Promise<CliRestartResult | null> {
    if (sessions.length === 0) return null;
    const resume = resumeCommandFor(provider);
    if (deps.getPrefs(provider).autoRestartCli) {
      const result = await restartCliSessions(sessions, resume);
      if (result.manual > 0) clipboard.writeText(resume.text);
      notifyCliRestart(provider, result);
      return result;
    }

    const action = await askRestart(provider, sessions);
    if (action === "copy") {
      clipboard.writeText(resume.text);
      deps.notify(
        `${provider.displayName} — ${deps.t("notif.cliCommandCopiedTitle")}`,
        deps.t("notif.cliCommandCopiedBody", {
          provider: providerName(provider),
          command: resume.text,
        })
      );
      return null;
    }
    if (action !== "restart") return null;

    const result = await restartCliSessions(sessions, resume);
    if (result.manual > 0) clipboard.writeText(resume.text);
    notifyCliRestart(provider, result);
    return result;
  }

  ipcMain.handle(
    "cli-restart:payload",
    (event) => payloads.get(event.sender.id) ?? null
  );

  return {
    providerName,
    detect: (provider: Provider) => detectCliSessions(provider),
    schedule: async (provider: Provider, sessions: readonly CliSession[]) => {
      try {
        return await handle(provider, sessions);
      } catch (error) {
        console.warn(`[cli:${provider.id}] handover failed`, formatError(error));
        return null;
      }
    },
  };
}
