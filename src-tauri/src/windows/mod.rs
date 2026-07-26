pub mod approval;
pub mod cli_restart;
pub mod manager;
pub mod notify;
pub mod onboarding;
pub mod widget;
mod widget_geometry;
mod widget_native;
pub mod widget_settings;
mod widget_taskbar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupWindow {
    Manager,
    Onboarding,
    None,
}

pub const fn startup_window(onboarded: bool, account_count: usize) -> StartupWindow {
    if !onboarded {
        StartupWindow::Onboarding
    } else if account_count < 2 {
        StartupWindow::Manager
    } else {
        StartupWindow::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_opens_onboarding_when_setup_is_incomplete() {
        assert_eq!(startup_window(false, 4), StartupWindow::Onboarding);
    }

    #[test]
    fn startup_opens_manager_when_fewer_than_two_accounts_exist() {
        assert_eq!(startup_window(true, 1), StartupWindow::Manager);
    }

    #[test]
    fn startup_stays_tray_only_when_setup_and_accounts_are_ready() {
        assert_eq!(startup_window(true, 2), StartupWindow::None);
    }
}
