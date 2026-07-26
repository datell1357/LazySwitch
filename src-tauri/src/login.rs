use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::accounts::{derive_slot_name, email_from_auth, read_auth};
use crate::paths::{account_auth_file, account_dir};
use crate::provider_types::LoginFlowResult;

/// Add a new Codex account WITHOUT disturbing the currently-live login.
///
/// Codex respects CODEX_HOME for all of its state, including auth.json. So
/// we point `codex login` at a throwaway home dir; the OAuth flow (browser)
/// writes auth.json there, and we move it into the account's slot. The real
/// ~/.codex/auth.json is never touched, so accounts can be stacked freely.
///
/// `codex login` starts a local server on localhost:1455 and tries to open
/// the browser, printing the authorize URL to stderr. That URL is captured
/// and handed back via `on_url` so the UI can show a clickable fallback if
/// the browser didn't open. Only one login can run at a time (fixed port).
pub async fn add_account_via_login(
    on_url: Option<impl Fn(String) + Send + Sync + 'static>,
) -> LoginFlowResult {
    let on_url: Option<Arc<dyn Fn(String) + Send + Sync>> =
        on_url.map(|f| Arc::new(f) as Arc<dyn Fn(String) + Send + Sync>);

    let tmp_home = match make_temp_login_dir() {
        Ok(p) => p,
        Err(e) => return err(e.to_string()),
    };

    let mut cmd = codex_login_command(&tmp_home);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            cleanup(&tmp_home);
            return err(e.to_string());
        }
    };

    let url_sent = Arc::new(AtomicBool::new(false));
    let stdout_task = child
        .stdout
        .take()
        .map(|r| spawn_scanner(r, url_sent.clone(), on_url.clone()));
    let stderr_task = child
        .stderr
        .take()
        .map(|r| spawn_scanner(r, url_sent.clone(), on_url.clone()));

    let status = child.wait().await;
    if let Some(h) = stdout_task {
        let _ = h.await;
    }
    if let Some(h) = stderr_task {
        let _ = h.await;
    }

    let status = match status {
        Ok(s) => s,
        Err(e) => {
            cleanup(&tmp_home);
            return err(e.to_string());
        }
    };

    let produced = tmp_home.join("auth.json");
    if !produced.exists() {
        cleanup(&tmp_home);
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return err(format!(
            "login did not complete (exit {code}); no auth.json produced"
        ));
    }

    let auth = read_auth(&produced);
    let email = email_from_auth(auth.as_ref());
    let name = derive_slot_name(email.as_deref());
    let result = std::fs::create_dir_all(account_dir(&name))
        .and_then(|_| std::fs::copy(&produced, account_auth_file(&name)).map(|_| ()));
    cleanup(&tmp_home);

    match result {
        Ok(()) => LoginFlowResult {
            ok: true,
            name: Some(name),
            email,
            error: None,
        },
        Err(e) => err(e.to_string()),
    }
}

fn err(message: String) -> LoginFlowResult {
    LoginFlowResult {
        ok: false,
        name: None,
        email: None,
        error: Some(message),
    }
}

fn cleanup(tmp_home: &Path) {
    let _ = std::fs::remove_dir_all(tmp_home);
}

#[cfg(windows)]
fn codex_login_command(tmp_home: &Path) -> Command {
    // npm's `codex` shim is codex.cmd on Windows; a plain Command::new("codex")
    // cannot resolve a .cmd shim on PATH without shell interpretation
    // (mirrors the original's `spawn(..., { shell: true })` on Windows).
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/d", "/s", "/c", "codex login"]);
    cmd.env("CODEX_HOME", tmp_home);
    cmd
}

#[cfg(not(windows))]
fn codex_login_command(tmp_home: &Path) -> Command {
    let mut cmd = Command::new("codex");
    cmd.arg("login");
    cmd.env("CODEX_HOME", tmp_home);
    cmd
}

fn make_temp_login_dir() -> std::io::Result<PathBuf> {
    use rand::Rng;
    let base = std::env::temp_dir();
    for _ in 0..10 {
        let suffix: u64 = rand::thread_rng().gen();
        let candidate = base.join(format!("codex-login-{suffix:x}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("could not create a unique temp login dir"))
}

fn extract_auth_url(text: &str) -> Option<String> {
    let idx = text.find("https://auth.openai.com/")?;
    let rest = &text[idx..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn spawn_scanner<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    url_sent: Arc<AtomicBool>,
    on_url: Option<Arc<dyn Fn(String) + Send + Sync>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if url_sent.load(Ordering::SeqCst) {
                continue;
            }
            if let (Some(url), Some(cb)) = (extract_auth_url(&line), &on_url) {
                url_sent.store(true, Ordering::SeqCst);
                cb(url);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_auth_url_finds_url_up_to_whitespace() {
        let text = "Opening browser to https://auth.openai.com/oauth/authorize?x=1 now...";
        assert_eq!(
            extract_auth_url(text),
            Some("https://auth.openai.com/oauth/authorize?x=1".to_string())
        );
    }

    #[test]
    fn extract_auth_url_none_when_absent() {
        assert_eq!(extract_auth_url("nothing here"), None);
    }

    #[test]
    fn extract_auth_url_takes_full_remainder_at_end_of_string() {
        let text = "url: https://auth.openai.com/oauth/authorize?x=1";
        assert_eq!(
            extract_auth_url(text),
            Some("https://auth.openai.com/oauth/authorize?x=1".to_string())
        );
    }

    #[test]
    fn make_temp_login_dir_creates_unique_existing_dir() {
        let dir = make_temp_login_dir().unwrap();
        assert!(dir.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
