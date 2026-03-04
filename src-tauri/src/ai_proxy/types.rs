//! Shared types for the AI proxy feature.
//!
//! All types use snake_case serialization (persisted data convention).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Proxy runtime status returned by `get_ai_proxy_status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub proxy_port: Option<u16>,
    pub backend_port: Option<u16>,
    pub backend_installed: bool,
}

/// Backend installation check returned by `check_ai_proxy_backend_installed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendStatus {
    pub installed: bool,
    pub path: Option<String>,
}

/// A provider account parsed from `~/.cli-proxy-api/*.json` files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAccount {
    /// e.g., "claude", "codex", "github-copilot"
    pub provider: String,
    /// Filename without `.json`
    pub account_id: String,
    /// Email or login field from JSON
    pub display_name: String,
    /// `true` if in active dir, `false` if in `.disabled/`
    pub enabled: bool,
    /// `true` if the `expired` field is in the past
    pub expired: bool,
}

/// A model group configuration persisted in `AppPreferences`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyModelGroup {
    pub name: String,
    pub models: Vec<ProxyModelGroupMember>,
    pub enabled: bool,
    /// `"round_robin"` or `"fill_first"`
    pub strategy: String,
}

/// A single member within a model group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyModelGroupMember {
    pub model: String,
    pub provider: String,
    pub enabled: bool,
}

/// Aggregated usage statistics returned by `get_ai_proxy_usage`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageStats {
    pub total_requests: u64,
    pub per_model: HashMap<String, u64>,
    pub per_provider: HashMap<String, u64>,
    pub per_account: HashMap<String, u64>,
    pub by_request: Vec<UsageRequestSummary>,
}

/// Request count grouped by provider/model/account.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageRequestSummary {
    pub provider: String,
    pub model: String,
    pub account: String,
    pub requests: u64,
}

/// Per-account usage data from provider APIs (Claude, Codex).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountUsage {
    pub account_id: String,
    pub provider: String,
    /// Primary window usage (5-hour for Claude, hourly for Codex).
    pub primary_used_percent: Option<f64>,
    /// Primary window remaining capacity percentage.
    pub primary_left_percent: Option<f64>,
    /// Seconds until primary window resets.
    pub primary_reset_seconds: Option<i64>,
    /// Secondary window usage (7-day for Claude, weekly for Codex).
    pub secondary_used_percent: Option<f64>,
    /// Secondary window remaining capacity percentage.
    pub secondary_left_percent: Option<f64>,
    /// Seconds until secondary window resets.
    pub secondary_reset_seconds: Option<i64>,
    pub plan_type: Option<String>,
    /// "loaded", "error", "unsupported"
    pub status: String,
}

/// A live model entry discovered from `/v1/models`.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ProxyModelInfo {
    pub provider: String,
    pub model: String,
    /// Canonical target identifier (`provider/model` when provider is known).
    pub id: String,
}

/// Payload emitted on the `ai-proxy:login-status` Tauri event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginStatusEvent {
    pub provider: String,
    /// `"started"`, `"completed"`, `"failed"`, or `"timed_out"`
    pub status: String,
}

/// A line of output from a login process (e.g. device codes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginOutputEvent {
    pub provider: String,
    pub line: String,
}

/// Known providers and their CLI login flags.
/// Empty string means no OAuth (API key only).
pub const KNOWN_PROVIDERS: &[(&str, &str)] = &[
    ("claude", "--claude-login"),
    ("codex", "--codex-login"),
    ("github-copilot", "--github-copilot-login"),
    ("gemini", "--login"),
    ("antigravity", "--antigravity-login"),
    ("qwen", "--qwen-login"),
    ("zai", ""), // Z.AI uses API keys, not OAuth
    ("kiro", "--kiro-login"),
];

/// Get the CLI login flag for a provider name.
/// Returns `None` for unknown providers, `Some("")` for API-key-only providers.
pub fn login_flag_for_provider(provider: &str) -> Option<&'static str> {
    KNOWN_PROVIDERS
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, flag)| *flag)
}
