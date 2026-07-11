export type ConsoleInjectionResult = "ok" | "access-denied" | "failed";

export type ConsoleResumeRoute = "resumed" | "elevate" | "fallback";

export interface ConsoleResumeAttempt {
  readonly stopped: boolean;
  readonly hasTerminal: boolean;
  readonly injection: ConsoleInjectionResult | null;
}

export interface CliRestartCounters {
  readonly restarted: number;
  readonly resumedInPlace: number;
  readonly manual: number;
  readonly failed: number;
}

export type CliRestartOutcome = "resumed-in-place" | "restarted" | "manual" | "failed";

function assertNever(value: never): never {
  throw new Error(`Unexpected CLI restart outcome: ${value}`);
}

export function decideConsoleResumeRoute(attempt: ConsoleResumeAttempt): ConsoleResumeRoute {
  if (!attempt.hasTerminal) return "fallback";
  if (!attempt.stopped) return "elevate";

  switch (attempt.injection) {
    case "ok":
      return "resumed";
    case "access-denied":
      return "elevate";
    case "failed":
    case null:
      return "fallback";
    default:
      return assertNever(attempt.injection);
  }
}

export function recordCliRestartOutcome(
  counters: CliRestartCounters,
  outcome: CliRestartOutcome
): CliRestartCounters {
  switch (outcome) {
    case "resumed-in-place":
      return { ...counters, resumedInPlace: counters.resumedInPlace + 1 };
    case "restarted":
      return { ...counters, restarted: counters.restarted + 1 };
    case "manual":
      return { ...counters, manual: counters.manual + 1 };
    case "failed":
      return { ...counters, failed: counters.failed + 1 };
    default:
      return assertNever(outcome);
  }
}
