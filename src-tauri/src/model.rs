use serde::{Deserialize, Serialize};

pub fn normalized_percent(value: impl Into<f64>) -> Option<f32> {
    let value = value.into();
    value
        .is_finite()
        .then_some(value)
        .filter(|value| (0.0..=100.0).contains(value))
        .map(|value| value as f32)
}

pub fn normalized_rfc3339(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|_| value.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Tool {
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "grok")]
    Grok,
    #[serde(rename = "cursor")]
    Cursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountLimit {
    pub label: String,
    pub used_percent: Option<f32>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub active: bool,
    pub context_used_percent: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub schema_version: String,
    pub pc_id: String,
    pub tool: Tool,
    #[serde(default)]
    pub session_id: String,
    pub captured_at: String,
    pub primary: Option<AccountLimit>,
    pub secondary: Option<AccountLimit>,
    pub session: SessionInfo,
    pub cost_estimate_usd: Option<f32>,
    pub approx: bool,
}

pub const SCHEMA_VERSION: &str = "agent_status.v1";
