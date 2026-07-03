import { spawn } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { accountDir, accountAuthFile } from "./paths";
import { readAuth, emailFromAuth, deriveSlotName } from "./accounts";

export interface LoginResult {
  ok: boolean;
  /** Slot name that was created (derived from the account's email). */
  name?: string;
  email?: string | null;
  error?: string;
}

/**
 * Add a new account WITHOUT disturbing the currently-live login.
 *
 * Codex respects CODEX_HOME for all of its state, including auth.json. So we
 * point `codex login` at a throwaway home dir; the OAuth flow (browser) writes
 * auth.json there, and we move it into the account's slot. The real
 * ~/.codex/auth.json is never touched, so you can stack accounts freely.
 *
 * `codex login` starts a local server on localhost:1455 and tries to open the
 * browser, printing the authorize URL to stderr. We capture that URL and hand
 * it back via `onUrl` so the UI can show a clickable fallback if the browser
 * didn't open. Only one login can run at a time (fixed port).
 */
export function addAccountViaLogin(
  onUrl?: (url: string) => void
): Promise<LoginResult> {
  return new Promise((resolve) => {
    let tmpHome: string;
    try {
      tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), "codex-login-"));
    } catch (e) {
      resolve({ ok: false, error: String(e) });
      return;
    }

    // shell:true so the npm `codex` shim (codex.cmd on Windows) resolves on
    // PATH. Args are static ("login") — no user input reaches the shell.
    const child = spawn("codex", ["login"], {
      env: { ...process.env, CODEX_HOME: tmpHome },
      shell: true,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let urlSent = false;
    const scan = (buf: Buffer) => {
      if (urlSent || !onUrl) return;
      const m = buf.toString().match(/https:\/\/auth\.openai\.com\/\S+/);
      if (m) {
        urlSent = true;
        onUrl(m[0]);
      }
    };
    child.stdout?.on("data", scan);
    child.stderr?.on("data", scan);

    const cleanup = () => {
      try {
        fs.rmSync(tmpHome, { recursive: true, force: true });
      } catch {
        /* best effort */
      }
    };

    child.on("error", (e) => {
      cleanup();
      resolve({ ok: false, error: String(e) });
    });

    child.on("exit", (code) => {
      const produced = path.join(tmpHome, "auth.json");
      if (fs.existsSync(produced)) {
        try {
          const email = emailFromAuth(readAuth(produced));
          const name = deriveSlotName(email);
          fs.mkdirSync(accountDir(name), { recursive: true });
          fs.copyFileSync(produced, accountAuthFile(name));
          cleanup();
          resolve({ ok: true, name, email });
        } catch (e) {
          cleanup();
          resolve({ ok: false, error: String(e) });
        }
      } else {
        cleanup();
        resolve({
          ok: false,
          error: `login did not complete (exit ${code}); no auth.json produced`,
        });
      }
    });
  });
}
