#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CliRestartCounters {
    pub restarted: u32,
    pub closed: u32,
    pub manual: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliRestartOutcome {
    Restarted,
    Manual,
    Failed,
}

pub fn record_cli_restart_outcome(
    mut counters: CliRestartCounters,
    outcome: CliRestartOutcome,
) -> CliRestartCounters {
    match outcome {
        CliRestartOutcome::Restarted => counters.restarted += 1,
        CliRestartOutcome::Manual => counters.manual += 1,
        CliRestartOutcome::Failed => counters.failed += 1,
    }
    counters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_each_outcome_kind() {
        let mut c = CliRestartCounters::default();
        c = record_cli_restart_outcome(c, CliRestartOutcome::Restarted);
        c = record_cli_restart_outcome(c, CliRestartOutcome::Manual);
        c = record_cli_restart_outcome(c, CliRestartOutcome::Failed);
        assert_eq!(
            c,
            CliRestartCounters {
                restarted: 1,
                closed: 0,
                manual: 1,
                failed: 1
            }
        );
    }
}
