import { createReadStream } from "fs";
import type { Dirent } from "fs";
import { readdir, stat } from "fs/promises";
import * as os from "os";
import * as path from "path";
import * as readline from "readline";
import { isCliSessionId, normalizeCwd } from "./codex-rollouts";

type ClaudeProcessSession = {
  readonly cwd: string | null;
  readonly startTime: string | null;
};

export type ClaudeSessionMatch = {
  readonly sessionId: string;
  readonly cwd: string;
  readonly file: string;
  readonly mtimeMs: number;
};

type ClaudeSessionCandidate = {
  readonly match: ClaudeSessionMatch;
  readonly birthtimeMs: number;
};

const CREATION_TOLERANCE_MS = 5 * 60 * 1000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function claudeProjectsRoot(): string {
  const home = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), ".claude");
  return path.join(home, "projects");
}

function parseTimeMs(value: string | null): number | null {
  if (!value) return null;
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? ms : null;
}

async function listSessionFiles(root: string): Promise<string[]> {
  let projects: Dirent[];
  try {
    projects = await readdir(root, { withFileTypes: true });
  } catch {
    return [];
  }

  const files: string[] = [];
  for (const project of projects) {
    if (!project.isDirectory()) continue;
    const projectRoot = path.join(root, project.name);
    let entries: Dirent[];
    try {
      entries = await readdir(projectRoot, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (entry.isFile() && entry.name.endsWith(".jsonl")) {
        files.push(path.join(projectRoot, entry.name));
      }
    }
  }
  return files;
}

async function readSessionMeta(
  file: string,
  mtimeMs: number
): Promise<ClaudeSessionMatch | null> {
  const input = createReadStream(file, { encoding: "utf8" });
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  let sessionId: string | null = null;
  try {
    for await (const line of lines) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(line);
      } catch {
        continue;
      }
      if (!isRecord(parsed)) continue;
      if (
        typeof parsed.sessionId === "string" &&
        isCliSessionId(parsed.sessionId)
      ) {
        sessionId = parsed.sessionId;
      }
      if (typeof parsed.cwd !== "string" || parsed.cwd.length === 0) continue;
      const resolvedSessionId = sessionId ?? path.basename(file, ".jsonl");
      if (!isCliSessionId(resolvedSessionId)) return null;
      return {
        sessionId: resolvedSessionId,
        cwd: parsed.cwd,
        file,
        mtimeMs,
      };
    }
    return null;
  } catch {
    return null;
  } finally {
    lines.close();
    input.destroy();
  }
}

export async function findClaudeSessionForProcess(
  session: ClaudeProcessSession,
  projectsRoot = claudeProjectsRoot(),
  claimedSessionIds: ReadonlySet<string> = new Set()
): Promise<ClaudeSessionMatch | null> {
  const cwd = session.cwd === null ? null : normalizeCwd(session.cwd);
  const startMs = parseTimeMs(session.startTime);
  if (cwd === null && startMs === null) return null;
  const candidates: ClaudeSessionCandidate[] = [];

  for (const file of await listSessionFiles(projectsRoot)) {
    let stats;
    try {
      stats = await stat(file);
    } catch {
      continue;
    }
    if (cwd === null && startMs !== null && stats.mtimeMs < startMs) continue;
    const match = await readSessionMeta(file, stats.mtimeMs);
    if (match === null || claimedSessionIds.has(match.sessionId)) continue;
    if (cwd !== null && normalizeCwd(match.cwd) !== cwd) continue;
    candidates.push({ match, birthtimeMs: stats.birthtimeMs });
  }

  if (cwd !== null) {
    const active =
      startMs === null
        ? candidates
        : candidates.filter((candidate) => candidate.match.mtimeMs >= startMs);
    const eligible = active.length > 0 ? active : candidates;
    const newest = eligible.reduce<ClaudeSessionCandidate | null>(
      (best, candidate) =>
        best === null || candidate.match.mtimeMs > best.match.mtimeMs
          ? candidate
          : best,
      null
    );
    return newest?.match ?? null;
  }
  if (startMs === null) return null;

  const closest = candidates.reduce<ClaudeSessionCandidate | null>(
    (best, candidate) => {
      if (best === null) return candidate;
      const delta = Math.abs(candidate.birthtimeMs - startMs);
      const bestDelta = Math.abs(best.birthtimeMs - startMs);
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
    Math.abs(closest.birthtimeMs - startMs) <= CREATION_TOLERANCE_MS
  ) {
    return closest.match;
  }
  return candidates.length === 1 ? candidates[0]?.match ?? null : null;
}
