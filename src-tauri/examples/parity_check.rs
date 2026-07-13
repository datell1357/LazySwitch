//! Read-only parity probe: run the ported Rust core against the real account
//! stores and print what it sees, so it can be diffed against the TypeScript.
//! Prints account names and usage percentages only — never tokens.

use lazyswitch_lib::core::providers::{claude::ClaudeProvider, codex::CodexProvider, Provider};

fn pct(w: &Option<lazyswitch_lib::core::types::PWindow>) -> String {
    match w {
        Some(w) => format!("{:.0}%", w.used_percent),
        None => "-".to_string(),
    }
}

async fn dump(label: &str, provider: &dyn Provider) {
    let accounts = provider.list_accounts();
    let active = provider.active_account_name();
    println!(
        "[{label}] accounts={} active={:?} live_auth={}",
        accounts.len(),
        active,
        provider.has_live_auth()
    );
    for account in accounts {
        let usage = provider
            .fetch_usage(if Some(&account.name) == active.as_ref() {
                None
            } else {
                Some(account.name.as_str())
            })
            .await;
        match usage {
            Some(usage) => println!(
                "   {:<14} enabled={} plan={:?} 5h={} week={} fable={}",
                account.name,
                account.enabled,
                usage.plan_type,
                pct(&usage.primary),
                pct(&usage.secondary),
                pct(&usage.fable),
            ),
            None => println!(
                "   {:<14} enabled={} usage=none",
                account.name, account.enabled
            ),
        }
    }
}

#[tokio::main]
async fn main() {
    dump("codex", &CodexProvider::default()).await;
    dump("claude", &ClaudeProvider::default()).await;
}
