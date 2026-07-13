//! Live end-to-end handover check against a SACRIFICIAL session only.
//! Detects codex sessions, restarts exactly the one whose cwd matches the
//! sacrificial marker directory, and leaves every other session alone.

use lazyswitch_lib::core::cli_sessions::{
    default_restart_cli_sessions, detect_cli_sessions, resume_command_for,
};

fn main() {
    let marker = std::env::var("LAZYSWITCH_SACRIFICE_CWD").expect("marker cwd env missing");
    let marker_norm = marker.trim_end_matches('\\').to_ascii_lowercase();

    let sessions = detect_cli_sessions("codex", std::process::id());
    println!("detected codex sessions: {}", sessions.len());
    for s in &sessions {
        println!(
            "   pid={} cwd={:?} terminal={:?}",
            s.pid,
            s.cwd,
            s.terminal.as_ref().map(|t| (&t.name, t.pid))
        );
    }

    let sacrificial: Vec<_> = sessions
        .into_iter()
        .filter(|s| {
            s.cwd
                .as_deref()
                .map(|c| c.trim_end_matches('\\').eq_ignore_ascii_case(&marker_norm))
                .unwrap_or(false)
        })
        .collect();
    println!("sacrificial matches: {}", sacrificial.len());
    if sacrificial.len() != 1 {
        println!("RESULT: expected exactly one sacrificial session; aborting without touching anything");
        std::process::exit(2);
    }

    let result = default_restart_cli_sessions(&sacrificial, &resume_command_for("codex"));
    println!(
        "RESULT: restarted={} closed={} manual={} failed={}",
        result.restarted, result.closed, result.manual, result.failed
    );
}
