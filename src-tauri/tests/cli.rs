#![cfg(test)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_lazyswitch-cli")
}

fn seed_cache(home: &Path, provider: &str, width: usize) {
    let directory = home.join(".lazyswitch");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("statusline-cache-{provider}-{width}.json")),
        serde_json::json!({
            "at": chrono::Utc::now().timestamp_millis(),
            "text": format!("cached-width-{width}"),
            "mode": "plain",
            "version": 11,
            "width": width
        })
        .to_string(),
    )
    .unwrap();
}

fn run_statusline(home: &Path, input: &str, columns: usize, provider: &str) -> String {
    let mut child = Command::new(binary())
        .args(["statusline", provider])
        .env("APPDATA", home.join("AppData").join("Roaming"))
        .env("COLUMNS", columns.to_string())
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("USERPROFILE", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn statusline_stdin_width_takes_priority_and_separates_cache_files() {
    let home = TempDir::new().unwrap();
    for width in [44, 55, 80] {
        seed_cache(home.path(), "claude", width);
    }
    assert_eq!(
        run_statusline(home.path(), r#"{"width":44}"#, 80, "claude"),
        "cached-width-44"
    );
    assert_eq!(
        run_statusline(home.path(), r#"{"terminal":{"cols":55}}"#, 80, "claude"),
        "cached-width-55"
    );
    assert_eq!(
        run_statusline(
            home.path(),
            r#"{"workspace":{"dimensions":{"columns":80}}}"#,
            44,
            "claude"
        ),
        "cached-width-80"
    );
    assert_eq!(
        run_statusline(
            home.path(),
            r#"{"dimensions":{"columns":44}}"#,
            80,
            "claude"
        ),
        "cached-width-44"
    );
    assert_eq!(
        run_statusline(home.path(), "", 55, "claude"),
        "cached-width-55"
    );
}

#[test]
fn codex_statusline_uses_isolated_home_fixture_and_stays_bounded() {
    let home = TempDir::new().unwrap();
    seed_cache(home.path(), "codex", 44);
    let output = run_statusline(home.path(), r#"{"width":44}"#, 80, "codex");
    assert_eq!(output, "cached-width-44");
    assert_eq!(output.lines().count(), 1);
    assert!(output.len() <= 44);
}
