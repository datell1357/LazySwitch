use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use super::windows::notify;

/// Startup/periodic check that only surfaces a toast when an update is
/// found — it never downloads or installs anything on its own. Installing
/// requires the explicit tray-menu action so a rotation in progress is
/// never interrupted by an unattended restart.
pub async fn check_silently(app: AppHandle) {
    let Ok(updater) = app.updater() else { return };
    match updater.check().await {
        Ok(Some(update)) => {
            notify(
                &app,
                "LazySwitch update available",
                format!(
                    "Version {} is available. Use the tray menu's \"Check for updates…\" to install it.",
                    update.version
                ),
            );
        }
        Ok(None) => {}
        Err(error) => eprintln!("[updater] silent check failed: {error}"),
    }
}

/// The tray-menu-triggered flow: check, download, install, then restart.
pub async fn check_and_install(app: AppHandle) {
    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(error) => {
            notify(&app, "Update check failed", error.to_string());
            return;
        }
    };
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            notify(&app, "LazySwitch is up to date", "");
            return;
        }
        Err(error) => {
            notify(&app, "Update check failed", error.to_string());
            return;
        }
    };
    let version = update.version.clone();
    notify(
        &app,
        "Installing update",
        format!("Downloading LazySwitch {version}…"),
    );
    match update.download_and_install(|_, _| {}, || {}).await {
        Ok(()) => {
            notify(
                &app,
                "Update installed",
                format!("LazySwitch {version} is ready — restarting now."),
            );
            app.restart();
        }
        Err(error) => notify(&app, "Update failed", error.to_string()),
    }
}
