//! Read-only detector probe: what the Rust port sees on this machine right now.
//! Detects only — never kills a process or touches a terminal.

use lazyswitch_lib::core::cli_sessions::detect_cli_sessions;

fn main() {
    for provider in ["codex", "claude"] {
        let sessions = detect_cli_sessions(provider, std::process::id());
        println!("{provider}: {} session(s)", sessions.len());
        for session in &sessions {
            println!(
                "   pid={} cwd={:?} terminal={:?}",
                session.pid, session.cwd, session.terminal
            );
        }
    }
}
