use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cli_cwd::normalize_windows_path;
use super::cli_sessions::CliSession;

const CREATION_TOLERANCE_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSessionMatch {
    pub session_id: String,
    pub cwd: String,
    pub file: String,
    pub mtime_ms: i64,
}

#[derive(Clone)]
struct Candidate { matched: ClaudeSessionMatch, birth_ms: i64 }

fn valid_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && value.len() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn parse_time(value: Option<&str>) -> Option<i64> {
    value.and_then(|value| DateTime::<FixedOffset>::parse_from_rfc3339(value).ok()).map(|value| value.timestamp_millis())
}

fn metadata_ms(path: &Path, created: bool) -> Option<i64> {
    let metadata = std::fs::metadata(path).ok()?;
    let time = if created { metadata.created().ok()? } else { metadata.modified().ok()? };
    Some(time.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn files(root: &Path) -> Vec<PathBuf> {
    let Ok(projects) = std::fs::read_dir(root) else { return Vec::new() };
    projects.flatten().filter_map(|project| {
        let path = project.path();
        if !path.is_dir() { return None; }
        Some(std::fs::read_dir(path).ok()?.flatten().filter_map(|entry| {
            let path = entry.path();
            (path.is_file() && path.extension().is_some_and(|ext| ext == "jsonl")).then_some(path)
        }).collect::<Vec<_>>())
    }).flatten().collect()
}

fn read_match(path: &Path, mtime_ms: i64) -> Option<ClaudeSessionMatch> {
    let mut session_id = None;
    for line in std::fs::read_to_string(path).ok()?.lines() {
        let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(value) = object.get("sessionId").and_then(Value::as_str).filter(|value| valid_id(value)) {
            session_id = Some(value.to_owned());
        }
        let Some(cwd) = object.get("cwd").and_then(Value::as_str).filter(|value| !value.is_empty()) else { continue };
        let id = session_id.clone().or_else(|| path.file_stem()?.to_str().map(str::to_owned))?;
        return valid_id(&id).then(|| ClaudeSessionMatch { session_id: id, cwd: cwd.to_owned(), file: path.to_string_lossy().into_owned(), mtime_ms });
    }
    None
}

pub fn projects_root() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| super::user_home().join(".claude")).join("projects")
}

pub fn find_for_process(session: &CliSession, root: Option<&Path>, claimed: &HashSet<String>) -> Option<ClaudeSessionMatch> {
    let cwd = session.cwd.as_deref().map(normalize_windows_path);
    let start = parse_time(session.start_time.as_deref());
    if cwd.is_none() && start.is_none() { return None; }
    let mut candidates = Vec::new();
    let default_root;
    let root = if let Some(root) = root { root } else { default_root = projects_root(); &default_root };
    for path in files(root) {
        let Some(mtime_ms) = metadata_ms(&path, false) else { continue };
        if cwd.is_none() && start.is_some_and(|start| mtime_ms < start) { continue; }
        let Some(matched) = read_match(&path, mtime_ms) else { continue };
        if claimed.contains(&matched.session_id) || cwd.as_deref().is_some_and(|cwd| normalize_windows_path(&matched.cwd) != cwd) { continue; }
        candidates.push(Candidate { matched, birth_ms: metadata_ms(&path, true).unwrap_or(mtime_ms) });
    }
    if cwd.is_some() {
        let active = start.map(|start| candidates.iter().filter(|c| c.matched.mtime_ms >= start).collect::<Vec<_>>()).unwrap_or_else(|| candidates.iter().collect());
        let source = if active.is_empty() { candidates.iter().collect() } else { active };
        return source.into_iter().max_by_key(|candidate| candidate.matched.mtime_ms).map(|candidate| candidate.matched.clone());
    }
    let start = start?;
    let closest = candidates.iter().min_by_key(|candidate| ((candidate.birth_ms - start).abs(), -candidate.matched.mtime_ms));
    if closest.is_some_and(|candidate| (candidate.birth_ms - start).abs() <= CREATION_TOLERANCE_MS) { return closest.map(|candidate| candidate.matched.clone()); }
    (candidates.len() == 1).then(|| candidates[0].matched.clone())
}
