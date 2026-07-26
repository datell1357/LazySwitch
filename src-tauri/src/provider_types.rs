use serde::{Deserialize, Serialize};

/// Provider abstraction: one implementation per AI service (Codex, Claude).
/// Unlike the original TS `Provider` interface (an object satisfying a duck
/// type), Rust dispatches on this enum plus per-provider modules
/// (`providers::codex`, `providers::claude`) rather than a trait object —
/// there are exactly two providers and no plugin model, so trait objects /
/// async-trait ceremony would be pure overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Codex,
    Claude,
}

impl ProviderId {
    pub const ALL: [ProviderId; 2] = [ProviderId::Codex, ProviderId::Claude];

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Codex => "codex",
            ProviderId::Claude => "claude",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ProviderId::Codex => "Codex",
            ProviderId::Claude => "Claude",
        }
    }

    /// Floor for pollIntervalSec — Claude's usage API rate-limits aggressively.
    pub fn min_poll_sec(self) -> u64 {
        match self {
            ProviderId::Codex => 10,
            ProviderId::Claude => 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PWindow {
    pub used_percent: f64,
    pub window_minutes: Option<i64>,
    /// epoch ms
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PUsage {
    /// short window (5h session / free monthly)
    pub primary: Option<PWindow>,
    /// long window (weekly)
    pub secondary: Option<PWindow>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fable: Option<PWindow>,
    pub plan_type: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PAccount {
    pub name: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub label: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginFlowResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

/// Local-session usage fallback (Codex rollout files) — the subset of
/// `PUsage` that `sessionUsage()` returns in the original TS.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionUsage {
    pub primary: Option<PWindow>,
    pub secondary: Option<PWindow>,
}
