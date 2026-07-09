use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Tool {
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "codex")]
    Codex,
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
