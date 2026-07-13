use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cli_cwd::normalize_windows_path;
use super::cli_sessions::CliSession;

const CREATION_TOLERANCE_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRolloutMatch { pub session_id: String, pub cwd: String, pub file: String, pub mtime_ms: i64 }

#[derive(Clone)] struct Candidate { matched: CodexRolloutMatch, creation_ms: i64 }

pub fn is_cli_session_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric()) && value.len() <= 128 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn normalize_cwd(value: &str) -> String { normalize_windows_path(value) }

fn parse_time(value: Option<&str>) -> Option<i64> { value.and_then(|value| DateTime::<FixedOffset>::parse_from_rfc3339(value).ok()).map(|value| value.timestamp_millis()) }

fn metadata_ms(path: &Path, created: bool) -> Option<i64> {
    let metadata = std::fs::metadata(path).ok()?;
    let time = if created { metadata.created().ok()? } else { metadata.modified().ok()? };
    Some(time.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    entries.flatten().flat_map(|entry| {
        let path = entry.path();
        if path.is_dir() { files(&path) } else if path.is_file() && path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl")) { vec![path] } else { Vec::new() }
    }).collect()
}

fn filename_creation(path: &Path, fallback: i64) -> i64 {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else { return fallback };
    let Some(timestamp) = name.strip_prefix("rollout-").and_then(|name| name.get(..19)) else { return fallback };
    NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H-%M-%S").ok().map(|value| value.and_utc().timestamp_millis()).unwrap_or(fallback)
}

fn read_match(path: &Path, mtime_ms: i64) -> Option<CodexRolloutMatch> {
    let line = std::fs::read_to_string(path).ok()?.lines().next()?.to_owned();
    let Value::Object(object) = serde_json::from_str::<Value>(&line).ok()? else { return None };
    if object.get("type")?.as_str()? != "session_meta" { return None }
    let payload = object.get("payload")?.as_object()?;
    let id = payload.get("session_id").and_then(Value::as_str).or_else(|| payload.get("id").and_then(Value::as_str))?;
    let cwd = payload.get("cwd")?.as_str()?;
    (is_cli_session_id(id)).then(|| CodexRolloutMatch { session_id: id.to_owned(), cwd: cwd.to_owned(), file: path.to_string_lossy().into_owned(), mtime_ms })
}

pub fn sessions_root() -> PathBuf { std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| super::user_home().join(".codex")).join("sessions") }

pub fn find_for_process(session: &CliSession, root: Option<&Path>, claimed: &HashSet<String>) -> Option<CodexRolloutMatch> {
    let cwd = session.cwd.as_deref().map(normalize_cwd);
    let start = parse_time(session.start_time.as_deref());
    if cwd.is_none() && start.is_none() { return None; }
    let mut candidates = Vec::new();
    let default_root;
    let root = if let Some(root) = root { root } else { default_root = sessions_root(); &default_root };
    for path in files(root) {
        let Some(mtime_ms) = metadata_ms(&path, false) else { continue };
        if start.is_some_and(|start| mtime_ms < start) { continue; }
        let Some(matched) = read_match(&path, mtime_ms) else { continue };
        if claimed.contains(&matched.session_id) || cwd.as_deref().is_some_and(|cwd| normalize_cwd(&matched.cwd) != cwd) { continue; }
        candidates.push(Candidate { matched, creation_ms: filename_creation(&path, metadata_ms(&path, true).unwrap_or(mtime_ms)) });
    }
    if cwd.is_some() { return candidates.into_iter().max_by_key(|candidate| candidate.matched.mtime_ms).map(|candidate| candidate.matched); }
    let start = start?;
    let closest = candidates.iter().min_by_key(|candidate| ((candidate.creation_ms - start).abs(), -candidate.matched.mtime_ms));
    if closest.is_some_and(|candidate| (candidate.creation_ms - start).abs() <= CREATION_TOLERANCE_MS) { return closest.map(|candidate| candidate.matched.clone()); }
    (candidates.len() == 1).then(|| candidates[0].matched.clone())
}
