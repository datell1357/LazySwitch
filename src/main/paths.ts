import * as os from "os";
import * as path from "path";

/**
 * Codex home. Respects CODEX_HOME override, else ~/.codex.
 * This is the *live* store that both Codex CLI and Codex Desktop read.
 */
export function codexHome(): string {
  return process.env.CODEX_HOME || path.join(os.homedir(), ".codex");
}

/** The single source of truth for the currently active account. */
export function liveAuthFile(): string {
  return path.join(codexHome(), "auth.json");
}

/** Where Codex writes session rollout files (used for usage monitoring). */
export function sessionsDir(): string {
  return path.join(codexHome(), "sessions");
}

/** Root of the per-account isolated auth stores: ~/.codex-accounts/<name>/auth.json */
export function accountsRoot(): string {
  return path.join(os.homedir(), ".codex-accounts");
}

export function accountDir(name: string): string {
  return path.join(accountsRoot(), name);
}

export function accountAuthFile(name: string): string {
  return path.join(accountDir(name), "auth.json");
}
