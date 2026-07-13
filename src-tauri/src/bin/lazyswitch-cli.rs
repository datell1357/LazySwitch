use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lazyswitch_lib::core::providers::claude::ClaudeProvider;
use lazyswitch_lib::core::providers::codex::CodexProvider;
use lazyswitch_lib::core::providers::{Provider, ReqwestTransport};
use lazyswitch_lib::core::types::{PAccount, PUsage, PWindow};
use lazyswitch_lib::core::{user_home, CoreError};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::io::AsyncReadExt;

const STATUSLINE_CACHE_MS: i64 = 60 * 1000;
const STATUSLINE_CACHE_VERSION: i64 = 11;
const STATUSLINE_DEFAULT_WIDTH: usize = 80;
const STATUSLINE_MIN_WIDTH: usize = 20;
const STATUSLINE_MAX_WIDTH: usize = 1000;
const STATUSLINE_GAUGE_WITH_RESET_WIDTH: usize = 13;
const STATUSLINE_GAUGE_WITHOUT_RESET_WIDTH: usize = 6;
const WINDOW_WIDTH: usize = 25;
const STATUS_ACCOUNT_MAX_WIDTH: usize = 20;
const STATUS_ACCOUNT_MIN_WIDTH: usize = 2;
const RESET: &str = "\x1b[0m";
const FG: &str = "\x1b[97m";
const BG_GREEN: &str = "\x1b[42m";
const BG_YELLOW: &str = "\x1b[43m";
const BG_RED: &str = "\x1b[41m";
const BG_GRAY: &str = "\x1b[100m";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRow {
    provider: String,
    account: PAccount,
    active: bool,
    usage: Option<PUsage>,
    error: Option<String>,
}

struct ProviderEntry {
    id: &'static str,
    display: &'static str,
    provider: Box<dyn Provider>,
}

#[derive(Debug)]
struct CliError(String);

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CliError {}

fn providers() -> Vec<ProviderEntry> {
    let transport = std::sync::Arc::new(ReqwestTransport::default());
    vec![
        ProviderEntry {
            id: "codex",
            display: "Codex",
            provider: Box::new(CodexProvider::new(
                lazyswitch_lib::core::paths::CodexPaths::from_env(),
                transport.clone(),
            )),
        },
        ProviderEntry {
            id: "claude",
            display: "Claude",
            provider: Box::new(ClaudeProvider::new(
                lazyswitch_lib::core::paths::ClaudePaths::from_env(),
                transport,
            )),
        },
    ]
}

async fn rows(filter: &str) -> Vec<UsageRow> {
    let mut result = Vec::new();
    for entry in providers() {
        if filter != "all" && filter != entry.id {
            continue;
        }
        let active_name = entry.provider.active_account_name();
        let mut accounts = entry.provider.list_accounts();
        // Node's provider lists use the host ICU locale comparator. In the
        // reference implementation this puts non-Latin slot names before
        // Latin names; keep the presentation order aligned across runtimes.
        accounts.sort_by(|left, right| {
            (left.name.is_ascii(), left.name.to_lowercase())
                .cmp(&(right.name.is_ascii(), right.name.to_lowercase()))
        });
        for account in accounts {
            result.push(row_for(&entry, account, active_name.as_deref()).await);
        }
        if active_name.is_none() && entry.provider.has_live_auth() {
            result.push(live_row_for(&entry).await);
        }
    }
    result
}

async fn row_for(entry: &ProviderEntry, account: PAccount, active_name: Option<&str>) -> UsageRow {
    let active = active_name == Some(account.name.as_str());
    let usage = entry
        .provider
        .fetch_usage(if active {
            None
        } else {
            Some(account.name.as_str())
        })
        .await;
    UsageRow {
        provider: entry.display.to_owned(),
        account,
        active,
        usage,
        error: None,
    }
}

async fn live_row_for(entry: &ProviderEntry) -> UsageRow {
    let usage = entry.provider.fetch_usage(None).await;
    let email = usage.as_ref().and_then(|value| value.email.clone());
    UsageRow {
        provider: entry.display.to_owned(),
        account: PAccount {
            name: "@live".into(),
            email,
            account_id: None,
            label: Some("live login".into()),
            enabled: true,
        },
        active: true,
        usage,
        error: None,
    }
}

fn color_mode() -> &'static str {
    if std::env::var("NO_COLOR").as_deref() == Ok("1") {
        "plain"
    } else {
        "ansi"
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn reset_text(resets_at: Option<i64>) -> String {
    let Some(resets_at) = resets_at else {
        return String::new();
    };
    let mins = (((resets_at - now_ms()) as f64) / 60_000.0)
        .round()
        .max(0.0) as i64;
    let hours = mins / 60;
    let minutes = mins % 60;
    if hours >= 48 {
        format!("{}d{}h", hours / 24, hours % 24)
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn bg(used: i64) -> &'static str {
    if used >= 90 {
        BG_RED
    } else if used >= 70 {
        BG_YELLOW
    } else {
        BG_GREEN
    }
}

fn pad(text: &str, width: usize) -> String {
    let clipped = if text.chars().count() > width {
        format!(
            "{}...",
            text.chars()
                .take(width.saturating_sub(3))
                .collect::<String>()
        )
    } else {
        text.to_owned()
    };
    format!("{clipped:<width$}")
}

fn table_gauge(window: Option<&PWindow>, width: usize) -> String {
    let Some(window) = window else {
        let text = pad(" unavailable", width);
        return if color_mode() == "plain" {
            text
        } else {
            format!("{BG_GRAY}{FG}{text}{RESET}")
        };
    };
    let used = window.used_percent.round().clamp(0.0, 100.0) as i64;
    let label = format!("{}% {}", used, reset_text(window.resets_at))
        .trim()
        .to_owned();
    let leading = width.saturating_sub(label.chars().count()) / 2;
    let text = pad(&format!("{}{}", " ".repeat(leading), label), width);
    if color_mode() == "plain" {
        let blocks = ((used as f64 / 100.0) * 10.0).round() as usize;
        return format!(
            "[{}{}] {}",
            "#".repeat(blocks),
            "-".repeat(10 - blocks),
            label
        );
    }
    let filled = ((used as f64 / 100.0) * width as f64).round() as usize;
    let mut result = String::new();
    for (index, character) in text.chars().enumerate() {
        result.push_str(if index < filled { bg(used) } else { BG_GRAY });
        result.push_str(FG);
        result.push(character);
    }
    result.push_str(RESET);
    result
}

fn status_gauge(window: Option<&PWindow>, include_reset: bool, interior_width: usize) -> String {
    let used = window.map_or(0, |value| {
        value.used_percent.round().clamp(0.0, 100.0) as i64
    });
    let label = match window {
        None => "n/a".to_owned(),
        Some(value) => format!(
            "{}%{}",
            used,
            if include_reset {
                value
                    .resets_at
                    .map(|at| format!(" {}", reset_text(Some(at))))
                    .unwrap_or_default()
            } else {
                String::new()
            }
        ),
    };
    let centered = if label.chars().count() > interior_width {
        label.clone()
    } else {
        let left = (interior_width - label.chars().count()) / 2;
        pad(&format!("{}{}", " ".repeat(left), label), interior_width)
    };
    if color_mode() == "plain" {
        return format!("[{centered}]");
    }
    let block_width = interior_width + 2;
    let text = if window.is_none() {
        pad(
            &format!(
                "{}{}",
                " ".repeat((block_width - label.chars().count()) / 2),
                label
            ),
            block_width,
        )
    } else {
        format!(" {centered} ")
    };
    let filled = ((used as f64 / 100.0) * block_width as f64).round() as usize;
    let mut result = String::new();
    for (index, character) in text.chars().enumerate() {
        result.push_str(if window.is_some() && index < filled {
            bg(used)
        } else {
            BG_GRAY
        });
        result.push_str(FG);
        result.push(character);
    }
    result.push_str(RESET);
    result
}

fn is_mark(character: char) -> bool {
    matches!(
        character as u32,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe20..=0xfe2f
    )
}

fn is_wide(character: char) -> bool {
    matches!(
        character as u32,
        0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1faff
    )
}

fn strip_ansi(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn clean_account_name(name: &str) -> String {
    strip_ansi(name)
        .chars()
        .filter(|character| {
            let code = *character as u32;
            code > 0x1f && !(0x7f..=0x9f).contains(&code)
        })
        .collect()
}

fn visible_width(text: &str) -> usize {
    strip_ansi(text)
        .chars()
        .filter(|character| *character != '\u{200d}' && !is_mark(*character))
        .map(|character| if is_wide(character) { 2 } else { 1 })
        .sum()
}

fn fit_status_account_label(label: &str, width: usize) -> String {
    if visible_width(label) <= width {
        return format!(
            "{label}{}",
            " ".repeat(width.saturating_sub(visible_width(label)))
        );
    }
    let suffix = if width > 3 { "..." } else { "" };
    let target = width.saturating_sub(visible_width(suffix));
    let mut result = String::new();
    for character in label.chars() {
        let next = format!("{result}{character}");
        if visible_width(&next) > target {
            break;
        }
        result.push(character);
    }
    let fitted = format!("{result}{suffix}");
    format!(
        "{fitted}{}",
        " ".repeat(width.saturating_sub(visible_width(&fitted)))
    )
}

fn is_resting(usage: Option<&PUsage>) -> bool {
    let Some(usage) = usage else {
        return false;
    };
    [
        usage.primary.as_ref(),
        usage.secondary.as_ref(),
        usage.fable.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|window| window.used_percent >= 100.0)
}

fn account_state(row: &UsageRow) -> &'static str {
    if row.error.is_some() || row.usage.is_none() {
        "UNKNOWN"
    } else if is_resting(row.usage.as_ref()) {
        "RESTING"
    } else if row.active {
        "ACTIVE"
    } else {
        "WAITING"
    }
}

fn status_account_state(row: &UsageRow) -> &'static str {
    if row.error.is_some() || row.usage.is_none() {
        "UNK"
    } else if is_resting(row.usage.as_ref()) {
        "RST"
    } else if row.active {
        "ACT"
    } else {
        "WAI"
    }
}

fn status_line_for(row: &UsageRow, label_width: usize, include_reset: bool) -> String {
    let gauge_width = if include_reset {
        STATUSLINE_GAUGE_WITH_RESET_WIDTH
    } else {
        STATUSLINE_GAUGE_WITHOUT_RESET_WIDTH
    };
    let account_prefix = if label_width == 0 {
        status_account_state(row).to_owned()
    } else {
        format!(
            "{} {}",
            fit_status_account_label(&clean_account_name(&row.account.name), label_width),
            status_account_state(row)
        )
    };
    let fable = if row.provider == "Claude" {
        format!(
            " Fable {}",
            status_gauge(
                row.usage.as_ref().and_then(|usage| usage.fable.as_ref()),
                include_reset,
                gauge_width
            )
        )
    } else {
        String::new()
    };
    format!(
        "{account_prefix} 5H {} Week {}{fable}",
        status_gauge(
            row.usage.as_ref().and_then(|usage| usage.primary.as_ref()),
            include_reset,
            gauge_width
        ),
        status_gauge(
            row.usage
                .as_ref()
                .and_then(|usage| usage.secondary.as_ref()),
            include_reset,
            gauge_width
        )
    )
}

fn order_status_rows(rows: &[UsageRow]) -> Vec<UsageRow> {
    let mut ordered = Vec::with_capacity(rows.len());
    let mut index = 0;
    while index < rows.len() {
        let provider = &rows[index].provider;
        let mut end = index + 1;
        while end < rows.len() && rows[end].provider == *provider {
            end += 1;
        }
        ordered.extend(rows[index..end].iter().filter(|row| row.active).cloned());
        ordered.extend(rows[index..end].iter().filter(|row| !row.active).cloned());
        index = end;
    }
    ordered
}

fn render_statusline(rows: &[UsageRow], width: usize) -> String {
    if rows.is_empty() {
        return "LazySwitch: no accounts".into();
    }
    let rows = order_status_rows(rows);
    let name_width = rows
        .iter()
        .map(|row| {
            visible_width(&clean_account_name(&row.account.name)).clamp(1, STATUS_ACCOUNT_MAX_WIDTH)
        })
        .max()
        .unwrap_or(1);
    let minimum = STATUS_ACCOUNT_MIN_WIDTH.min(name_width);
    let fits = |label_width: usize, include_reset: bool| {
        rows.iter().all(|row| {
            visible_width(&status_line_for(row, label_width, include_reset)) <= width.max(1)
        })
    };
    if fits(minimum, true) {
        for label_width in (minimum..=name_width).rev() {
            if fits(label_width, true) {
                return rows
                    .iter()
                    .map(|row| status_line_for(row, label_width, true))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
    }
    for label_width in (minimum..=name_width).rev() {
        if fits(label_width, false) {
            return rows
                .iter()
                .map(|row| status_line_for(row, label_width, false))
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    if minimum > 0 && fits(0, false) {
        return rows
            .iter()
            .map(|row| status_line_for(row, 0, false))
            .collect::<Vec<_>>()
            .join("\n");
    }
    rows.iter()
        .map(|row| status_line_for(row, minimum, false))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_table(items: &[UsageRow]) -> String {
    let show_fable = items
        .iter()
        .any(|row| row.provider == "Claude" && row.usage.is_some());
    let mut lines = vec![
        "LazySwitch".into(),
        String::new(),
        format!(
            "{}{} {} {} {} {}{}",
            pad("", 2),
            pad("provider", 8),
            pad("account", 24),
            pad("state", 10),
            pad("5H", WINDOW_WIDTH),
            pad("Week", WINDOW_WIDTH),
            if show_fable {
                format!(" {}", pad("Fable", WINDOW_WIDTH))
            } else {
                String::new()
            }
        ),
    ];
    for row in items {
        let marker = if row.active { ">" } else { " " };
        let account = row
            .account
            .email
            .as_deref()
            .or(row.account.label.as_deref())
            .unwrap_or(&row.account.name);
        let fable = if show_fable {
            format!(
                " {}",
                table_gauge(
                    row.usage.as_ref().and_then(|usage| usage.fable.as_ref()),
                    WINDOW_WIDTH
                )
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "{marker} {} {} {} {} {}{fable}",
            pad(&row.provider, 8),
            pad(account, 24),
            pad(account_state(row), 10),
            table_gauge(
                row.usage.as_ref().and_then(|usage| usage.primary.as_ref()),
                WINDOW_WIDTH
            ),
            table_gauge(
                row.usage
                    .as_ref()
                    .and_then(|usage| usage.secondary.as_ref()),
                WINDOW_WIDTH
            )
        ));
    }
    if items.is_empty() {
        lines.push("  No enrolled accounts.".into());
    }
    lines.join("\n")
}

fn sane_width(value: &Value) -> Option<usize> {
    let numeric = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    if !numeric.is_finite()
        || numeric < STATUSLINE_MIN_WIDTH as f64
        || numeric > STATUSLINE_MAX_WIDTH as f64
    {
        None
    } else {
        Some(numeric.floor() as usize)
    }
}

fn direct_payload_width(value: &Map<String, Value>) -> Option<usize> {
    [
        "width",
        "cols",
        "columns",
        "terminal_width",
        "terminalWidth",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(sane_width))
}

fn nested_payload_width(value: &Value) -> Option<usize> {
    let object = value.as_object()?;
    direct_payload_width(object).or_else(|| {
        ["terminal", "workspace", "dimensions"]
            .into_iter()
            .find_map(|key| object.get(key).and_then(nested_payload_width))
    })
}

async fn read_statusline_payload() -> Option<Value> {
    if io::stdin().is_terminal() {
        return None;
    }
    let mut input = tokio::io::stdin();
    let mut bytes = Vec::new();
    if tokio::time::timeout(Duration::from_millis(100), input.read_to_end(&mut bytes))
        .await
        .is_err()
    {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.trim().is_empty() {
        None
    } else {
        serde_json::from_str(text.trim()).ok()
    }
}

async fn statusline_width() -> usize {
    if let Some(width) = read_statusline_payload()
        .await
        .and_then(|value| nested_payload_width(&value))
    {
        return width;
    }
    if io::stdout().is_terminal() {
        if let Some((terminal_size::Width(width), _)) = terminal_size::terminal_size() {
            if (STATUSLINE_MIN_WIDTH as u16..=STATUSLINE_MAX_WIDTH as u16).contains(&width) {
                return width as usize;
            }
        }
    }
    sane_width(&Value::String(std::env::var("COLUMNS").unwrap_or_default()))
        .unwrap_or(STATUSLINE_DEFAULT_WIDTH)
}

fn cache_file(filter: &str, width: usize) -> PathBuf {
    user_home()
        .join(".lazyswitch")
        .join(format!("statusline-cache-{filter}-{width}.json"))
}

fn cached_statusline(filter: &str, width: usize) -> Option<String> {
    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(cache_file(filter, width)).ok()?).ok()?;
    let object = value.as_object()?;
    if object.get("version")?.as_i64()? != STATUSLINE_CACHE_VERSION
        || object.get("mode")?.as_str()? != color_mode()
        || object.get("width")?.as_u64()? != width as u64
    {
        return None;
    }
    if now_ms() - object.get("at")?.as_i64()? <= STATUSLINE_CACHE_MS {
        object.get("text")?.as_str().map(String::from)
    } else {
        None
    }
}

fn write_statusline(filter: &str, width: usize, text: &str) -> Result<(), CoreError> {
    let file = cache_file(filter, width);
    std::fs::create_dir_all(file.parent().expect("cache has parent"))?;
    let value = serde_json::json!({
        "at": now_ms(), "text": text, "mode": color_mode(),
        "version": STATUSLINE_CACHE_VERSION, "width": width
    });
    std::fs::write(file, serde_json::to_string(&value)?)?;
    Ok(())
}

fn read_json_object(file: &Path) -> Map<String, Value> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn backup(file: &Path) -> Result<(), CoreError> {
    if file.exists() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let name = file.file_name().expect("file name").to_string_lossy();
        std::fs::copy(
            file,
            file.with_file_name(format!("{name}.lazyswitch-bak-{timestamp}")),
        )?;
    }
    Ok(())
}

fn write_json_object(file: &Path, value: &Map<String, Value>) -> Result<bool, CoreError> {
    std::fs::create_dir_all(file.parent().expect("JSON file has parent"))?;
    let mut next = serde_json::to_string_pretty(value)?;
    next.push('\n');
    let current = std::fs::read_to_string(file).unwrap_or_default();
    if next == current {
        return Ok(false);
    }
    backup(file)?;
    std::fs::write(file, next)?;
    Ok(true)
}

fn shell_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn statusline_command() -> String {
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lazyswitch-cli"));
    format!("\"{}\" statusline claude", shell_path(&executable))
}

fn install_claude() -> Result<(String, PathBuf, bool, String), CoreError> {
    let file = user_home().join(".claude").join("settings.json");
    let mut settings = read_json_object(&file);
    settings.insert(
        "statusLine".into(),
        serde_json::json!({
            "type": "command", "command": statusline_command(), "padding": 0, "refreshInterval": 60
        }),
    );
    let changed = write_json_object(&file, &settings)?;
    Ok((
        "Claude Code".into(),
        file,
        changed,
        "installed command statusLine".into(),
    ))
}

fn replace_tui_setting(config: &str, key: &str, line: &str) -> String {
    let mut output = String::with_capacity(config.len() + line.len());
    let mut found = false;
    for part in config.split_inclusive('\n') {
        let body = part.trim_end_matches(['\r', '\n']);
        let trimmed = body.trim_start();
        if trimmed.starts_with(key) && trimmed[key.len()..].trim_start().starts_with('=') {
            output.push_str(line);
            if part.ends_with('\n') {
                output.push('\n');
            }
            found = true;
        } else {
            output.push_str(part);
        }
    }
    if found {
        return output;
    }
    if let Some(index) = config.lines().position(|line| line.trim() == "[tui]") {
        let mut result = String::new();
        for (line_index, part) in config.split_inclusive('\n').enumerate() {
            result.push_str(part);
            if line_index == index {
                result.push_str(line);
                result.push('\n');
            }
        }
        if !result.is_empty() {
            return result;
        }
    }
    format!("{}\n\n[tui]\n{line}\n", config.trim_end())
}

fn install_codex() -> Result<(String, PathBuf, bool, String), CoreError> {
    let file = user_home().join(".codex").join("config.toml");
    let current = std::fs::read_to_string(&file).unwrap_or_default();
    let mut next = replace_tui_setting(&current, "status_line", "status_line = [\"model-with-reasoning\", \"context-remaining\", \"five-hour-limit\", \"weekly-limit\"]");
    next = replace_tui_setting(
        &next,
        "status_line_use_colors",
        "status_line_use_colors = true",
    );
    let changed = next != current;
    if changed {
        std::fs::create_dir_all(file.parent().expect("TOML file has parent"))?;
        backup(&file)?;
        std::fs::write(&file, next)?;
    }
    Ok(("Codex CLI".into(), file, changed, "enabled built-in status_line quota fields with colored limit gauges; external command statusline is not supported by Codex CLI".into()))
}

fn install_hooks() -> Result<Vec<(String, PathBuf, bool, String)>, CoreError> {
    Ok(vec![install_claude()?, install_codex()?])
}

fn help() -> &'static str {
    "Usage:\n  lazyswitch status              print account usage table once\n  lazyswitch watch [--interval N] keep the table visible\n  lazyswitch statusline [provider] print one compact line per account\n  lazyswitch install-hooks       install Claude statusLine and Codex built-ins"
}

fn interval_seconds(args: &[String]) -> Result<u64, CliError> {
    let Some(index) = args.iter().position(|arg| arg == "--interval") else {
        return Ok(30);
    };
    args.get(index + 1)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 5.0)
        .map(|value| value.round() as u64)
        .ok_or_else(|| CliError("--interval must be a number >= 5".into()))
}

fn provider_filter(args: &[String]) -> Result<&str, CliError> {
    let filter = args.get(1).map(String::as_str).unwrap_or("all");
    if matches!(filter, "all" | "codex" | "claude") {
        Ok(filter)
    } else {
        Err(CliError(
            "statusline provider must be all, codex, or claude".into(),
        ))
    }
}

async fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str).unwrap_or("status") {
        "help" | "--help" | "-h" => println!("{}", help()),
        "status" => {
            let items = rows("all").await;
            if args.iter().any(|arg| arg == "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "at": chrono::Utc::now().to_rfc3339(), "rows": items
                    }))?
                );
            } else {
                println!("{}", render_table(&items));
            }
        }
        "watch" => {
            let delay = Duration::from_secs(interval_seconds(args)?);
            loop {
                let items = rows("all").await;
                println!("\x1b[2J\x1b[H{}", render_table(&items));
                io::stdout().flush()?;
                tokio::time::sleep(delay).await;
            }
        }
        "statusline" => {
            let filter = provider_filter(args)?;
            let width = statusline_width().await;
            if let Some(cached) = cached_statusline(filter, width) {
                println!("{}", cached);
            } else {
                let text = render_statusline(&rows(filter).await, width);
                write_statusline(filter, width, &text)?;
                println!("{}", text);
            }
        }
        "install-hooks" => {
            for (target, path, changed, note) in install_hooks()? {
                println!(
                    "{}: {} {}",
                    target,
                    if changed { "updated" } else { "unchanged" },
                    path.display()
                );
                println!("  {}", note);
            }
        }
        other => return Err(Box::new(CliError(format!("Unknown command: {other}")))),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .enable_io()
        .build()
        .expect("failed to initialize CLI runtime");
    match runtime.block_on(run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error);
            if error.downcast_ref::<CliError>().is_some() {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn account(name: &str) -> PAccount {
        PAccount {
            name: name.into(),
            email: Some("example@example.com".into()),
            account_id: None,
            label: None,
            enabled: true,
        }
    }

    fn usage(primary: f64, secondary: f64, fable: Option<f64>, reset_ms: i64) -> PUsage {
        let resets_at = Some(now_ms() + reset_ms);
        PUsage {
            primary: Some(PWindow {
                used_percent: primary,
                window_minutes: Some(300),
                resets_at,
            }),
            secondary: Some(PWindow {
                used_percent: secondary,
                window_minutes: Some(10080),
                resets_at,
            }),
            fable: fable.map(|used_percent| PWindow {
                used_percent,
                window_minutes: Some(10080),
                resets_at,
            }),
            plan_type: Some("pro".into()),
            email: None,
        }
    }

    fn row(provider: &str, name: &str, active: bool, usage: Option<PUsage>) -> UsageRow {
        UsageRow {
            provider: provider.into(),
            account: account(name),
            active,
            usage,
            error: None,
        }
    }

    #[test]
    fn statusline_adapts_width_and_keeps_gauges_centered() {
        let _guard = env_lock();
        std::env::set_var("NO_COLOR", "1");
        let reset_ms = (20 * 24 + 17) * 60 * 60 * 1000 + 30 * 1000;
        let rows = vec![
            row(
                "Claude",
                "slot-a",
                true,
                Some(usage(100.0, 74.0, Some(80.0), reset_ms)),
            ),
            row(
                "Claude",
                "slot-b",
                false,
                Some(usage(56.0, 78.0, Some(9.0), reset_ms)),
            ),
            row(
                "Codex",
                "slot-name",
                false,
                Some(usage(12.0, 34.0, Some(80.0), reset_ms)),
            ),
        ];
        for width in [44, 55, 80] {
            let output = render_statusline(&rows, width);
            let lines = output.lines().collect::<Vec<_>>();
            assert_eq!(lines.len(), rows.len());
            assert!(lines.iter().all(|line| line.len() <= width));
            if width == 44 {
                assert!(lines[0].starts_with("RST"));
                assert!(lines[1].starts_with("WAI"));
                assert!(lines[2].starts_with("WAI"));
            } else {
                assert!(lines[0].starts_with("slot-a"));
                assert!(lines[1].starts_with("slot-b"));
                assert!(lines[2].starts_with("slot-name"));
            }
            let indexes = lines
                .iter()
                .map(|line| line.find("5H").unwrap())
                .collect::<Vec<_>>();
            assert!(indexes.iter().all(|index| *index == indexes[0]));
            assert!(lines
                .iter()
                .all(|line| line.contains("5H [") && line.contains("Week [")));
            let interiors = lines
                .iter()
                .flat_map(|line| {
                    line.split('[')
                        .skip(1)
                        .filter_map(|part| part.split(']').next())
                })
                .collect::<Vec<_>>();
            assert!(interiors
                .iter()
                .all(|interior| interior.len() == 6 || interior.len() == 13));
            if width == 80 {
                assert!(output.contains("20d17h"));
            }
            if width <= 55 {
                assert!(!lines[0].contains("20d17h"));
            }
            if width == 44 {
                assert_eq!(lines[0].len(), 44);
            }
        }
        assert!(render_statusline(&rows, 80)
            .lines()
            .all(|line| !line.contains("@example.com")));
        assert!(!render_statusline(&rows, 80)
            .lines()
            .nth(2)
            .unwrap()
            .contains("Fable ["));
    }

    #[test]
    fn statusline_orders_active_first_in_each_provider_block() {
        let _guard = env_lock();
        std::env::set_var("NO_COLOR", "1");
        let rows = vec![
            row(
                "Codex",
                "codex-waiting",
                false,
                Some(usage(12.0, 34.0, None, 1_000)),
            ),
            row(
                "Codex",
                "codex-active",
                true,
                Some(usage(12.0, 34.0, None, 1_000)),
            ),
            row(
                "Claude",
                "claude-active",
                true,
                Some(usage(12.0, 34.0, Some(56.0), 1_000)),
            ),
        ];
        let output = render_statusline(&rows, 80);
        let lines = output.lines().collect::<Vec<_>>();
        assert!(lines[0].starts_with("codex-active"));
        assert!(lines[1].starts_with("codex-waiting"));
        assert!(lines[2].starts_with("claude-active"));
    }

    #[test]
    fn colored_statusline_stays_within_width_after_ansi_is_removed() {
        let _guard = env_lock();
        std::env::remove_var("NO_COLOR");
        let reset_ms = (20 * 24 + 17) * 60 * 60 * 1000 + 30 * 1000;
        let rows = vec![row(
            "Claude",
            "slot-a",
            true,
            Some(usage(100.0, 74.0, Some(80.0), reset_ms)),
        )];
        for width in [44, 55, 80] {
            let line = strip_ansi(&render_statusline(&rows, width));
            assert!(visible_width(&line) <= width);
            assert!(!line.contains('[') && !line.contains(']'));
            if width == 80 {
                assert!(line.contains("100% 20d17h"));
            }
            if width == 44 {
                assert!(!line.contains("20d17h"));
            }
        }
    }

    #[test]
    fn missing_usage_is_centered_in_plain_and_colored_modes() {
        let _guard = env_lock();
        std::env::set_var("NO_COLOR", "1");
        let plain = render_statusline(&[row("Claude", "slot-a", true, None)], 80);
        assert!(plain.ends_with("5H [     n/a     ] Week [     n/a     ] Fable [     n/a     ]"));
        std::env::remove_var("NO_COLOR");
        let colored = strip_ansi(&render_statusline(
            &[row("Claude", "slot-a", true, None)],
            80,
        ));
        assert!(!colored.contains('[') && !colored.contains(']'));
        for marker in ["5H", "Week", "Fable"] {
            let start = colored.find(marker).unwrap() + marker.len() + 1;
            assert_eq!(&colored[start..start + 15], "      n/a      ");
        }
    }

    #[test]
    fn controls_are_removed_and_wide_labels_use_display_columns() {
        let _guard = env_lock();
        std::env::set_var("NO_COLOR", "1");
        let controls = row(
            "Claude",
            "slot\nname\x1b[31m",
            false,
            Some(usage(12.0, 34.0, None, 1_000)),
        );
        let line = render_statusline(&[controls], 80);
        assert_eq!(line.lines().count(), 1);
        assert!(!line.chars().any(|character| character.is_control()));
        let wide = row(
            "Codex",
            &"가나다".repeat(30),
            true,
            Some(usage(12.0, 34.0, None, 1_000)),
        );
        assert!(visible_width(&render_statusline(&[wide], 44)) <= 44);
    }
}
