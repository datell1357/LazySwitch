pub mod commands;
pub mod monitor;
pub mod probe;
pub mod state;
pub mod tray;
pub mod updater;
pub mod windows;

use std::sync::atomic::AtomicBool;

/// Closing every window must not kill the tray-resident app, so `lib.rs`
/// prevents the default exit-on-last-window-close behavior. Call sites that
/// really do want to terminate (the "Quit" menu item, the probe harness)
/// set this first so the RunEvent handler lets that one request through.
pub static ALLOW_EXIT: AtomicBool = AtomicBool::new(false);
