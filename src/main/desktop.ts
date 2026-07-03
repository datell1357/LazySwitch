import { spawn, exec } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

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
    ];
  }
  // macOS
  return ["/Applications/Codex.app"];
}

function resolveDesktopPath(cfg: DesktopSettings): string | null {
  if (cfg.desktopAppPath) {
    // MSIX/Store installs are launched by AppUserModelID, not exe path,
    // e.g. "shell:AppsFolder\OpenAI.Codex_2p2nqsd0c76g0!App".
    if (cfg.desktopAppPath.startsWith("shell:")) return cfg.desktopAppPath;
    if (fs.existsSync(cfg.desktopAppPath)) return cfg.desktopAppPath;
  }
  return desktopCandidates().find((p) => fs.existsSync(p)) ?? null;
}

function killProcess(cfg: DesktopSettings): Promise<void> {
  return new Promise((resolve) => {
    if (process.platform === "win32") {
      // /T kills the process tree (tray + helper renderers).
      exec(`taskkill /IM "${cfg.desktopProcessName}" /T /F`, () => resolve());
    } else {
      exec(`pkill -f "${cfg.desktopProcessName}"`, () => resolve());
    }
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
  const appPath = resolveDesktopPath(cfg);
  await killProcess(cfg);
  if (!appPath) return false;
  // Small delay so the OS releases file/socket handles before relaunch.
  await new Promise((r) => setTimeout(r, 1500));
  launch(appPath);
  return true;
}
