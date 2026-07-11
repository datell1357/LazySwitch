export interface CliRestartCounters {
  readonly restarted: number;
  readonly closed: number;
  readonly manual: number;
  readonly failed: number;
}

export type CliRestartOutcome = "restarted" | "manual" | "failed";

function assertNever(value: never): never {
  throw new Error(`Unexpected CLI restart outcome: ${value}`);
}

export function recordCliRestartOutcome(
  counters: CliRestartCounters,
  outcome: CliRestartOutcome
): CliRestartCounters {
  switch (outcome) {
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
