use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::codex_rollouts::{is_cli_session_id, normalize_cwd};

const CREATION_TOLERANCE_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeSessionMatch {
    pub session_id: String,
    pub cwd: String,
    pub file: PathBuf,
    pub mtime_ms: i64,
}

struct Candidate {
    m: ClaudeSessionMatch,
    birthtime_ms: i64,
}

fn claude_projects_root() -> PathBuf {
    let home = match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => dirs::home_dir().unwrap_or_default().join(".claude"),
    };
    home.join("projects")
}

fn parse_time_ms(value: Option<&str>) -> Option<i64> {
    let s = value?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn list_session_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let projects = match std::fs::read_dir(root) {
        Ok(p) => p,
        Err(_) => return files,
    };
    for project in projects.filter_map(|e| e.ok()) {
        let Ok(file_type) = project.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    files
}

fn read_session_meta(file: &Path, mtime_ms: i64) -> Option<ClaudeSessionMatch> {
    let f = std::fs::File::open(file).ok()?;
    let mut session_id: Option<String> = None;
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(parsed) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(sid) = parsed.get("sessionId").and_then(|v| v.as_str()) {
            if is_cli_session_id(sid) {
                session_id = Some(sid.to_string());
            }
        }
        let Some(cwd) = parsed
            .get("cwd")
            .and_then(|v| v.as_str())
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        let resolved = session_id.clone().unwrap_or_else(|| {
            file.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        });
        if !is_cli_session_id(&resolved) {
            return None;
        }
        return Some(ClaudeSessionMatch {
            session_id: resolved,
            cwd: cwd.to_string(),
            file: file.to_path_buf(),
            mtime_ms,
        });
    }
    None
}

pub struct ClaudeProcessSession<'a> {
    pub cwd: Option<&'a str>,
    pub start_time: Option<&'a str>,
}

pub fn find_claude_session_for_process(
    session: &ClaudeProcessSession,
    projects_root: Option<&Path>,
    claimed_session_ids: &HashSet<String>,
) -> Option<ClaudeSessionMatch> {
    let root_owned;
    let root = match projects_root {
        Some(r) => r,
        None => {
            root_owned = claude_projects_root();
            &root_owned
        }
    };
    let cwd = session.cwd.map(normalize_cwd);
    let start_ms = parse_time_ms(session.start_time);
    if cwd.is_none() && start_ms.is_none() {
        return None;
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for file in list_session_files(root) {
        let Ok(meta) = std::fs::metadata(&file) else {
            continue;
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if cwd.is_none() {
            if let Some(s) = start_ms {
                if mtime_ms < s {
                    continue;
                }
            }
        }
        let Some(m) = read_session_meta(&file, mtime_ms) else {
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
        candidates.push(Candidate { m, birthtime_ms });
    }

    if cwd.is_some() {
        let active: Vec<&Candidate> = match start_ms {
            None => candidates.iter().collect(),
            Some(s) => {
                let filtered: Vec<&Candidate> =
                    candidates.iter().filter(|c| c.m.mtime_ms >= s).collect();
                if filtered.is_empty() {
                    candidates.iter().collect()
                } else {
                    filtered
                }
            }
        };
        return active
            .into_iter()
            .reduce(|best, c| {
                if c.m.mtime_ms > best.m.mtime_ms {
                    c
                } else {
                    best
                }
            })
            .map(|c| c.m.clone());
    }
    let start_ms = start_ms?;

    let closest = candidates.iter().enumerate().reduce(|(bi, best), (i, c)| {
        let delta = (c.birthtime_ms - start_ms).abs();
        let best_delta = (best.birthtime_ms - start_ms).abs();
        if delta < best_delta || (delta == best_delta && c.m.mtime_ms > best.m.mtime_ms) {
            (i, c)
        } else {
            (bi, best)
        }
    });
    if let Some((_, c)) = closest {
        if (c.birthtime_ms - start_ms).abs() <= CREATION_TOLERANCE_MS {
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
    fn find_session_matches_by_cwd_preferring_active_after_start() {
        let dir =
            std::env::temp_dir().join(format!("lazyswitch-claude-sess-{}", std::process::id()));
        let project = dir.join("proj1");
        std::fs::create_dir_all(&project).unwrap();
        let f1 = project.join("sess1.jsonl");
        std::fs::write(&f1, r#"{"sessionId":"sess1","cwd":"C:\\proj"}"#).unwrap();

        let session = ClaudeProcessSession {
            cwd: Some(r"C:\proj"),
            start_time: None,
        };
        let found = find_claude_session_for_process(&session, Some(&dir), &HashSet::new()).unwrap();
        assert_eq!(found.session_id, "sess1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_session_falls_back_to_filename_when_no_session_id_field() {
        let dir =
            std::env::temp_dir().join(format!("lazyswitch-claude-sess2-{}", std::process::id()));
        let project = dir.join("proj1");
        std::fs::create_dir_all(&project).unwrap();
        let f1 = project.join("abc123.jsonl");
        std::fs::write(&f1, r#"{"cwd":"C:\\proj"}"#).unwrap();

        let session = ClaudeProcessSession {
            cwd: Some(r"C:\proj"),
            start_time: None,
        };
        let found = find_claude_session_for_process(&session, Some(&dir), &HashSet::new()).unwrap();
        assert_eq!(found.session_id, "abc123");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
