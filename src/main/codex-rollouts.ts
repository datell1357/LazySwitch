import { createReadStream } from "fs";
import type { Dirent } from "fs";
import { readdir, stat } from "fs/promises";
import * as os from "os";
import * as path from "path";
import * as readline from "readline";

type CodexProcessSession = {
  readonly cwd: string | null;
  readonly startTime: string | null;
};

export type CodexRolloutMatch = {
  readonly sessionId: string;
  readonly cwd: string;
  readonly file: string;
  readonly mtimeMs: number;
};

type CodexRolloutCandidate = {
  readonly match: CodexRolloutMatch;
  readonly creationMs: number;
};

const ROLLOUT_PREFIX = "rollout-";
const ROLLOUT_SUFFIX = ".jsonl";
const CREATION_TOLERANCE_MS = 5 * 60 * 1000;
const SESSION_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function codexSessionsRoot(): string {
  return path.join(os.homedir(), ".codex", "sessions");
}

export function isCliSessionId(value: string): boolean {
  return SESSION_ID_PATTERN.test(value);
}

export function normalizeCwd(value: string): string {
  const withoutPrefix = value.replace(/^\\\\\?\\/, "");
  const normalized = path.win32.normalize(withoutPrefix);
  const trimmed =
    normalized.length > 3 ? normalized.replace(/[\\/]+$/, "") : normalized;
  return trimmed.toLowerCase();
}

function parseTimeMs(value: string | null): number | null {
  if (!value) return null;
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? ms : null;
}

function rolloutCreationTimeMs(file: string, birthtimeMs: number): number {
  const match =
    /^rollout-(\d{4}-\d{2}-\d{2})T(\d{2})-(\d{2})-(\d{2})-.+\.jsonl$/.exec(
      path.basename(file)
    );
  if (match === null) return birthtimeMs;
  const parsed = Date.parse(`${match[1]}T${match[2]}:${match[3]}:${match[4]}`);
  return Number.isFinite(parsed) ? parsed : birthtimeMs;
}

async function listRolloutFiles(root: string): Promise<string[]> {
  let entries: Dirent[];
  try {
    entries = await readdir(root, { withFileTypes: true });
  } catch {
    return [];
  }

  const files: string[] = [];
  for (const entry of entries) {
    const fullPath = path.join(root, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await listRolloutFiles(fullPath)));
    } else if (
      entry.isFile() &&
      entry.name.startsWith(ROLLOUT_PREFIX) &&
      entry.name.endsWith(ROLLOUT_SUFFIX)
    ) {
      files.push(fullPath);
    }
  }
  return files;
}

async function readFirstLine(file: string): Promise<string | null> {
  const input = createReadStream(file, { encoding: "utf8" });
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  try {
    for await (const line of lines) return line;
    return null;
  } catch {
    return null;
  } finally {
    lines.close();
    input.destroy();
  }
}

function readRolloutMeta(
  line: string,
  file: string,
  mtimeMs: number
): CodexRolloutMatch | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    return null;
  }

  if (!isRecord(parsed) || parsed.type !== "session_meta") return null;
  const payload = parsed.payload;
  if (!isRecord(payload)) return null;

  const sessionId =
    typeof payload.session_id === "string"
      ? payload.session_id
      : typeof payload.id === "string"
        ? payload.id
        : null;
  if (
    sessionId === null ||
    !isCliSessionId(sessionId) ||
    typeof payload.cwd !== "string"
  ) {
    return null;
  }

  return { sessionId, cwd: payload.cwd, file, mtimeMs };
}

export async function findCodexRolloutForProcess(
  session: CodexProcessSession,
  sessionsRoot = codexSessionsRoot(),
  claimedSessionIds: ReadonlySet<string> = new Set()
): Promise<CodexRolloutMatch | null> {
  const cwd = session.cwd === null ? null : normalizeCwd(session.cwd);
  const startMs = parseTimeMs(session.startTime);
  if (cwd === null && startMs === null) return null;
  const candidates: CodexRolloutCandidate[] = [];

  for (const file of await listRolloutFiles(sessionsRoot)) {
    let stats;
    try {
      stats = await stat(file);
    } catch {
      continue;
    }
    if (startMs !== null && stats.mtimeMs < startMs) continue;

    const line = await readFirstLine(file);
    if (line === null) continue;
    const match = readRolloutMeta(line, file, stats.mtimeMs);
    if (match === null || claimedSessionIds.has(match.sessionId)) continue;
    if (cwd !== null && normalizeCwd(match.cwd) !== cwd) continue;
    candidates.push({
      match,
      creationMs: rolloutCreationTimeMs(file, stats.birthtimeMs),
    });
  }

  if (cwd !== null) {
    const newest = candidates.reduce<CodexRolloutCandidate | null>(
      (best, candidate) =>
        best === null || candidate.match.mtimeMs > best.match.mtimeMs
          ? candidate
          : best,
      null
    );
    return newest?.match ?? null;
  }
  if (startMs === null) return null;

  const closest = candidates.reduce<CodexRolloutCandidate | null>(
    (best, candidate) => {
      if (best === null) return candidate;
      const delta = Math.abs(candidate.creationMs - startMs);
      const bestDelta = Math.abs(best.creationMs - startMs);
      return delta < bestDelta ||
        (delta === bestDelta &&
          candidate.match.mtimeMs > best.match.mtimeMs)
        ? candidate
        : best;
    },
    null
  );
  if (
    closest !== null &&
    Math.abs(closest.creationMs - startMs) <= CREATION_TOLERANCE_MS
  ) {
    return closest.match;
  }
  return candidates.length === 1 ? candidates[0]?.match ?? null : null;
}
