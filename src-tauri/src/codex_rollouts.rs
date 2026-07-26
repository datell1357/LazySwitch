use chrono::TimeZone;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const ROLLOUT_PREFIX: &str = "rollout-";
const ROLLOUT_SUFFIX: &str = ".jsonl";
const CREATION_TOLERANCE_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct CodexRolloutMatch {
    pub session_id: String,
    pub cwd: String,
    pub file: PathBuf,
    pub mtime_ms: i64,
}

struct Candidate {
    m: CodexRolloutMatch,
    creation_ms: i64,
}

fn codex_sessions_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".codex")
        .join("sessions")
}

pub fn is_cli_session_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    rest.len() <= 127
        && rest
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
}

/// Simplified stand-in for Node's `path.win32.normalize` + lowercasing, as
/// used to compare cwds. See the equivalent note in desktop_processes.rs —
/// these inputs are always absolute paths, never `.`/`..`-relative ones.
pub fn normalize_cwd(value: &str) -> String {
    let stripped = value.strip_prefix(r"\\?\").unwrap_or(value);
    let unified = stripped.replace('/', "\\");
    let mut collapsed = String::with_capacity(unified.len());
    let mut last_sep = false;
    for c in unified.chars() {
        if c == '\\' {
            if !last_sep {
                collapsed.push(c);
            }
            last_sep = true;
        } else {
            collapsed.push(c);
            last_sep = false;
        }
    }
    let trimmed = if collapsed.len() > 3 {
        collapsed.trim_end_matches('\\').to_string()
    } else {
        collapsed
    };
    trimmed.to_lowercase()
}

fn parse_time_ms(value: Option<&str>) -> Option<i64> {
    let s = value?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// `rollout-YYYY-MM-DDTHH-MM-SS-<rest>.jsonl` — the filename encodes local
/// creation time; falls back to the file's actual birthtime if the name
/// doesn't match (older Codex builds, unexpected names).
fn rollout_creation_time_ms(file: &Path, birthtime_ms: i64) -> i64 {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Some(rest) = name
        .strip_prefix(ROLLOUT_PREFIX)
        .and_then(|r| r.strip_suffix(ROLLOUT_SUFFIX))
    else {
        return birthtime_ms;
    };
    let bytes = rest.as_bytes();
    if rest.len() <= 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b'-')
        || bytes.get(16) != Some(&b'-')
        || bytes.get(19) != Some(&b'-')
    {
        return birthtime_ms;
    }
    let parse_u = |r: std::ops::Range<usize>| rest.get(r).and_then(|s| s.parse::<u32>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) = (
        rest.get(0..4).and_then(|s| s.parse::<i32>().ok()),
        parse_u(5..7),
        parse_u(8..10),
        parse_u(11..13),
        parse_u(14..16),
        parse_u(17..19),
    ) else {
        return birthtime_ms;
    };
    chrono::NaiveDate::from_ymd_opt(y, mo, d)
        .and_then(|nd| nd.and_hms_opt(h, mi, s))
        .and_then(|dt| match chrono::Local.from_local_datetime(&dt) {
            chrono::LocalResult::Single(local) => Some(local.timestamp_millis()),
            chrono::LocalResult::Ambiguous(a, _) => Some(a.timestamp_millis()),
            chrono::LocalResult::None => None,
        })
        .unwrap_or(birthtime_ms)
}

fn list_rollout_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return files,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            files.extend(list_rollout_files(&path));
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with(ROLLOUT_PREFIX) && name.ends_with(ROLLOUT_SUFFIX) {
                files.push(path);
            }
        }
    }
    files
}

fn read_first_line(file: &Path) -> Option<String> {
    let f = std::fs::File::open(file).ok()?;
    BufReader::new(f).lines().next()?.ok()
}

fn read_rollout_meta(line: &str, file: &Path, mtime_ms: i64) -> Option<CodexRolloutMatch> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    if parsed.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
        return None;
    }
    let payload = parsed.get("payload")?;
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("id").and_then(|v| v.as_str()))?;
    if !is_cli_session_id(session_id) {
        return None;
    }
    let cwd = payload.get("cwd").and_then(|v| v.as_str())?;
    Some(CodexRolloutMatch {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        file: file.to_path_buf(),
        mtime_ms,
    })
}

pub struct CodexProcessSession<'a> {
    pub cwd: Option<&'a str>,
    pub start_time: Option<&'a str>,
}

pub fn find_codex_rollout_for_process(
    session: &CodexProcessSession,
    sessions_root: Option<&Path>,
    claimed_session_ids: &HashSet<String>,
) -> Option<CodexRolloutMatch> {
    let root_owned;
    let root = match sessions_root {
        Some(r) => r,
        None => {
            root_owned = codex_sessions_root();
            &root_owned
        }
    };
    let cwd = session.cwd.map(normalize_cwd);
    let start_ms = parse_time_ms(session.start_time);
    if cwd.is_none() && start_ms.is_none() {
        return None;
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for file in list_rollout_files(root) {
        let Ok(meta) = std::fs::metadata(&file) else {
            continue;
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if let Some(s) = start_ms {
            if mtime_ms < s {
                continue;
            }
        }
        let Some(line) = read_first_line(&file) else {
            continue;
        };
        let Some(m) = read_rollout_meta(&line, &file, mtime_ms) else {
            continue;
        };
        if claimed_session_ids.contains(&m.session_id) {
            continue;
        }
        if let Some(c) = &cwd {
            if &normalize_cwd(&m.cwd) != c {
                continue;
            }
        }
        let birthtime_ms = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(mtime_ms);
        candidates.push(Candidate {
            creation_ms: rollout_creation_time_ms(&file, birthtime_ms),
            m,
        });
    }

    if cwd.is_some() {
        return candidates
            .into_iter()
            .reduce(|best, c| {
                if c.m.mtime_ms > best.m.mtime_ms {
                    c
                } else {
                    best
                }
            })
            .map(|c| c.m);
    }
    let start_ms = start_ms?;

    let closest = candidates.iter().enumerate().reduce(|(bi, best), (i, c)| {
        let delta = (c.creation_ms - start_ms).abs();
        let best_delta = (best.creation_ms - start_ms).abs();
        if delta < best_delta || (delta == best_delta && c.m.mtime_ms > best.m.mtime_ms) {
            (i, c)
        } else {
            (bi, best)
        }
    });
    if let Some((_, c)) = closest {
        if (c.creation_ms - start_ms).abs() <= CREATION_TOLERANCE_MS {
            return Some(c.m.clone());
        }
    }
    if candidates.len() == 1 {
        return candidates.into_iter().next().map(|c| c.m);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cli_session_id_validates_charset_and_length() {
        assert!(is_cli_session_id("abc123"));
        assert!(is_cli_session_id("a"));
        assert!(!is_cli_session_id(""));
        assert!(!is_cli_session_id("_leading"));
        assert!(!is_cli_session_id("has space"));
        assert!(!is_cli_session_id(&"a".repeat(129)));
    }

    #[test]
    fn normalize_cwd_strips_extended_prefix_and_trailing_sep() {
        assert_eq!(normalize_cwd(r"\\?\C:\Users\me\proj\"), r"c:\users\me\proj");
        assert_eq!(normalize_cwd("C:/Users/me/"), r"c:\users\me");
        assert_eq!(normalize_cwd(r"C:\"), r"c:\");
    }

    #[test]
    fn find_rollout_matches_by_cwd_picking_newest() {
        let dir =
            std::env::temp_dir().join(format!("lazyswitch-rollouts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("rollout-2024-01-01T00-00-00-a.jsonl");
        let f2 = dir.join("rollout-2024-01-01T00-00-01-b.jsonl");
        std::fs::write(
            &f1,
            r#"{"type":"session_meta","payload":{"session_id":"sess1","cwd":"C:\\proj"}}"#,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &f2,
            r#"{"type":"session_meta","payload":{"session_id":"sess2","cwd":"C:\\proj"}}"#,
        )
        .unwrap();

        let session = CodexProcessSession {
            cwd: Some(r"C:\proj"),
            start_time: None,
        };
        let found = find_codex_rollout_for_process(&session, Some(&dir), &HashSet::new()).unwrap();
        assert_eq!(found.session_id, "sess2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_rollout_skips_claimed_session_ids() {
        let dir = std::env::temp_dir().join(format!(
            "lazyswitch-rollouts-claimed-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("rollout-2024-01-01T00-00-00-a.jsonl");
        std::fs::write(
            &f1,
            r#"{"type":"session_meta","payload":{"session_id":"sess1","cwd":"C:\\proj"}}"#,
        )
        .unwrap();

        let session = CodexProcessSession {
            cwd: Some(r"C:\proj"),
            start_time: None,
        };
        let mut claimed = HashSet::new();
        claimed.insert("sess1".to_string());
        assert_eq!(
            find_codex_rollout_for_process(&session, Some(&dir), &claimed),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
