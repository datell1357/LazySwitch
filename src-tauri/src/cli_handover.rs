use crate::cli_sessions::{
    detect_cli_sessions, resume_command_for, CliRestartResult, CliResumeCommand, CliSession,
};
use crate::provider_types::ProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliRestartAction {
    Restart,
    Copy,
    Later,
}

#[derive(Debug)]
pub struct CliRestartRequest<'a> {
    pub provider_name: &'static str,
    pub resume_command: &'a str,
    pub sessions: &'a [CliSession],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliNotification {
    Restarted {
        provider: &'static str,
        result: CliRestartResult,
    },
    CommandCopied {
        provider: &'static str,
        command: String,
    },
}

pub trait CliHandoverDeps {
    fn auto_restart_cli(&self, provider: ProviderId) -> bool;
    fn ask_restart<'a>(
        &'a mut self,
        request: CliRestartRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = CliRestartAction> + 'a>>;
    fn restart<'a>(
        &'a mut self,
        sessions: &'a [CliSession],
        resume: &'a CliResumeCommand,
    ) -> Pin<Box<dyn Future<Output = CliRestartResult> + 'a>>;
    fn copy_to_clipboard(&mut self, text: &str);
    fn notify(&mut self, notification: CliNotification);
}

pub const fn provider_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Codex => "Codex CLI",
        ProviderId::Claude => "Claude Code",
    }
}

pub async fn detect(provider: ProviderId, root_pid: i64) -> Vec<CliSession> {
    detect_cli_sessions(provider, root_pid).await
}

pub async fn schedule(
    provider: ProviderId,
    sessions: &[CliSession],
    deps: &mut impl CliHandoverDeps,
) -> Option<CliRestartResult> {
    if sessions.is_empty() {
        return None;
    }

    let resume = resume_command_for(provider);
    let action = if deps.auto_restart_cli(provider) {
        CliRestartAction::Restart
    } else {
        deps.ask_restart(CliRestartRequest {
            provider_name: provider_name(provider),
            resume_command: &resume.text,
            sessions,
        })
        .await
    };

    match action {
        CliRestartAction::Copy => {
            deps.copy_to_clipboard(&resume.text);
            deps.notify(CliNotification::CommandCopied {
                provider: provider_name(provider),
                command: resume.text,
            });
            None
        }
        CliRestartAction::Later => None,
        CliRestartAction::Restart => {
            let result = deps.restart(sessions, &resume).await;
            if result.manual > 0 {
                deps.copy_to_clipboard(&resume.text);
            }
            deps.notify(CliNotification::Restarted {
                provider: provider_name(provider),
                result,
            });
            Some(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auto_restart_restarts_and_copies_manual_resume_command() {
        let mut deps = FakeDeps::new(true, CliRestartAction::Later);
        deps.restart_result.manual = 1;
        let sessions = vec![session()];
        let result = schedule(ProviderId::Codex, &sessions, &mut deps).await;
        assert_eq!(result, Some(deps.restart_result));
        assert_eq!(deps.clipboard, vec!["codex resume"]);
        assert_eq!(deps.restart_calls, 1);
    }

    #[tokio::test]
    async fn copy_action_copies_without_restarting() {
        let mut deps = FakeDeps::new(false, CliRestartAction::Copy);
        let result = schedule(ProviderId::Codex, &[session()], &mut deps).await;
        assert_eq!(result, None);
        assert_eq!(deps.clipboard, vec!["codex resume"]);
        assert_eq!(deps.restart_calls, 0);
    }

    #[tokio::test]
    async fn later_action_does_nothing() {
        let mut deps = FakeDeps::new(false, CliRestartAction::Later);
        let result = schedule(ProviderId::Codex, &[session()], &mut deps).await;
        assert_eq!(result, None);
        assert!(deps.clipboard.is_empty());
        assert_eq!(deps.restart_calls, 0);
    }

    struct FakeDeps {
        auto_restart: bool,
        action: CliRestartAction,
        restart_result: CliRestartResult,
        restart_calls: usize,
        clipboard: Vec<String>,
    }

    impl FakeDeps {
        fn new(auto_restart: bool, action: CliRestartAction) -> Self {
            Self {
                auto_restart,
                action,
                restart_result: CliRestartResult::default(),
                restart_calls: 0,
                clipboard: Vec::new(),
            }
        }
    }

    impl CliHandoverDeps for FakeDeps {
        fn auto_restart_cli(&self, _provider: ProviderId) -> bool {
            self.auto_restart
        }
        fn ask_restart<'a>(
            &'a mut self,
            _request: CliRestartRequest<'a>,
        ) -> Pin<Box<dyn Future<Output = CliRestartAction> + 'a>> {
            Box::pin(async move { self.action })
        }
        fn restart<'a>(
            &'a mut self,
            _sessions: &'a [CliSession],
            _resume: &'a CliResumeCommand,
        ) -> Pin<Box<dyn Future<Output = CliRestartResult> + 'a>> {
            self.restart_calls += 1;
            Box::pin(async move { self.restart_result })
        }
        fn copy_to_clipboard(&mut self, text: &str) {
            self.clipboard.push(text.to_string());
        }
        fn notify(&mut self, _notification: CliNotification) {}
    }

    fn session() -> CliSession {
        CliSession {
            provider_id: ProviderId::Codex,
            pid: 42,
            start_time: None,
            cwd: Some(r"C:\repo".to_string()),
            terminal: None,
        }
    }
}
use std::future::Future;
use std::pin::Pin;
