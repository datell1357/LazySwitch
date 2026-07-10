import { spawn, exec } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import {
  killWindowsDesktopProcesses,
  resolveDesktopAumid,
  selectDesktopProcessIds,
} from "./desktop-processes";

export { selectDesktopProcessIds };

/** The subset of settings desktop restart needs (AppConfig or ProviderPrefs both fit). */
interface DesktopSettings {
  desktopAppPath: string;
  desktopProcessName: string;
}

/** Candidate install locations for the Codex Desktop executable. */
function desktopCandidates(): string[] {
  const home = os.homedir();
  if (process.platform === "win32") {
    return [
      path.join(home, "AppData", "Local", "Programs", "Codex", "Codex.exe"),
      path.join(home, "AppData", "Local", "Codex", "Codex.exe"),
      "C:\\Program Files\\Codex\\Codex.exe",
      // Codex Desktop ≥26.7 ships merged into the ChatGPT app.
      path.join(home, "AppData", "Local", "Programs", "ChatGPT", "ChatGPT.exe"),
      path.join(home, "AppData", "Local", "ChatGPT", "ChatGPT.exe"),
      "C:\\Program Files\\ChatGPT\\ChatGPT.exe",
    ];
  }
  // macOS
  return ["/Applications/Codex.app", "/Applications/ChatGPT.app"];
}

async function resolveDesktopPath(cfg: DesktopSettings): Promise<string | null> {
  if (cfg.desktopAppPath) {
    // MSIX/Store installs are launched by AppUserModelID, not exe path,
    // e.g. "shell:AppsFolder\OpenAI.Codex_2p2nqsd0c76g0!App".
    if (cfg.desktopAppPath.startsWith("shell:")) return cfg.desktopAppPath;
    if (fs.existsSync(cfg.desktopAppPath)) return cfg.desktopAppPath;
  }
  const file = desktopCandidates().find((p) => fs.existsSync(p));
  if (file) return file;
  // Store/MSIX install — no spawnable exe path; launch by AppUserModelID.
  return resolveDesktopAumid();
}

/**
 * Process names to kill. The merged ChatGPT app renamed the main process from
 * Codex.exe to ChatGPT.exe, so both known names are covered on Windows in
 * addition to whatever the user configured. The executable-path filter in
 * selectDesktopProcessIds keeps unrelated same-named processes alive.
 */
function desktopProcessNames(cfg: DesktopSettings): string[] {
  const names =
    process.platform === "win32"
      ? [cfg.desktopProcessName, "Codex.exe", "ChatGPT.exe"]
      : [cfg.desktopProcessName];
  return [...new Set(names.map((n) => n.trim()).filter((n) => n.length > 0))];
}

async function killProcess(
  cfg: DesktopSettings,
  desktopAppPath: string | null
): Promise<void> {
  if (process.platform === "win32") {
    await killWindowsDesktopProcesses(desktopProcessNames(cfg), desktopAppPath);
    return;
  }
  await new Promise<void>((resolve) => {
    exec(`pkill -f "${cfg.desktopProcessName}"`, () => resolve());
  });
}

function launch(appPath: string): void {
  if (process.platform === "darwin") {
    spawn("open", [appPath], { detached: true, stdio: "ignore" }).unref();
  } else if (appPath.startsWith("shell:")) {
    // Store/MSIX app: WindowsApps exes can't be spawned directly.
    spawn("explorer.exe", [appPath], { detached: true, stdio: "ignore" }).unref();
  } else {
    spawn(appPath, [], { detached: true, stdio: "ignore" }).unref();
  }
}

/**
 * Fully restart the Codex Desktop app so it re-reads the swapped auth.json.
 * Desktop keeps the old token in memory and in its session cache, so a full
 * kill (incl. tray) + relaunch is required — a plain window close is not enough.
 * Returns false if the executable could not be located.
 */
export async function restartDesktopApp(cfg: DesktopSettings): Promise<boolean> {
  const appPath = await resolveDesktopPath(cfg);
  await killProcess(cfg, appPath);
  if (!appPath) return false;
  // Small delay so the OS releases file/socket handles before relaunch.
  await new Promise((r) => setTimeout(r, 1500));
  launch(appPath);
  return true;
}
