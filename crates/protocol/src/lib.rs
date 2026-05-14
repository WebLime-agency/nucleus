use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_JOB_MAX_STEPS: usize = 80;
pub const DEFAULT_JOB_MAX_TOOL_CALLS: usize = 160;
pub const DEFAULT_JOB_MAX_WALL_CLOCK_SECS: u64 = 7_200;
pub const DEFAULT_CHILD_JOB_MAX_STEPS: usize = 24;
pub const DEFAULT_CHILD_JOB_MAX_TOOL_CALLS: usize = 48;
pub const MAX_CONFIGURED_JOB_STEPS: usize = 1_000;
pub const MAX_CONFIGURED_JOB_TOOL_CALLS: usize = 2_000;
pub const MAX_CONFIGURED_JOB_WALL_CLOCK_SECS: u64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunBudgetSummary {
    pub mode: String,
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_wall_clock_secs: u64,
}

impl Default for RunBudgetSummary {
    fn default() -> Self {
        Self {
            mode: "standard".to_string(),
            max_steps: DEFAULT_JOB_MAX_STEPS,
            max_tool_calls: DEFAULT_JOB_MAX_TOOL_CALLS,
            max_wall_clock_secs: DEFAULT_JOB_MAX_WALL_CLOCK_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

impl HealthResponse {
    pub fn ok(service: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            status: "ok".to_string(),
            service: service.into(),
            version: version.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSummary {
    pub id: String,
    pub summary: String,
    pub state: String,
    pub auth_state: String,
    pub version: String,
    pub executable_path: String,
    pub default_model: String,
    pub note: String,
    pub supports_sessions: bool,
    pub supports_prompting: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionProjectSummary {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub profile_title: String,
    pub route_id: String,
    pub route_title: String,
    pub project_id: String,
    pub project_title: String,
    pub project_path: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub provider_base_url: String,
    #[serde(default)]
    pub provider_api_key: String,
    pub working_dir: String,
    pub working_dir_kind: String,
    #[serde(default = "default_workspace_mode")]
    pub workspace_mode: String,
    #[serde(default)]
    pub source_project_path: String,
    #[serde(default)]
    pub git_root: String,
    #[serde(default)]
    pub worktree_path: String,
    #[serde(default)]
    pub git_branch: String,
    #[serde(default)]
    pub git_base_ref: String,
    #[serde(default)]
    pub git_head: String,
    #[serde(default)]
    pub git_dirty: bool,
    #[serde(default)]
    pub git_untracked_count: usize,
    #[serde(default)]
    pub git_remote_tracking_branch: String,
    #[serde(default)]
    pub workspace_warnings: Vec<String>,
    pub scope: String,
    #[serde(default = "default_session_approval_mode")]
    pub approval_mode: String,
    #[serde(default = "default_session_execution_mode")]
    pub execution_mode: String,
    #[serde(default = "default_session_run_budget_mode")]
    pub run_budget_mode: String,
    #[serde(default)]
    pub run_budget: RunBudgetSummary,
    pub project_count: usize,
    pub projects: Vec<SessionProjectSummary>,
    pub state: String,
    pub provider_session_id: String,
    pub last_error: String,
    pub last_message_excerpt: String,
    pub turn_count: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTurnImage {
    pub display_name: String,
    pub mime_type: String,
    pub data_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTurn {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub images: Vec<SessionTurnImage>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDetail {
    pub session: SessionSummary,
    pub turns: Vec<SessionTurn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyDecisionSummary {
    pub decision: String,
    pub reason: String,
    pub matched_rule: String,
    pub scope_kind: String,
    pub risk_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCapabilitySummary {
    pub tool_id: String,
    pub summary: String,
    pub approval_mode: String,
    pub risk_level: String,
    pub side_effect_level: String,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
    pub supports_streaming: bool,
    pub concurrency_group: String,
    pub scope_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobSummary {
    pub id: String,
    pub session_id: Option<String>,
    pub parent_job_id: Option<String>,
    pub template_id: Option<String>,
    pub title: String,
    pub purpose: String,
    pub trigger_kind: String,
    pub state: String,
    pub requested_by: String,
    pub prompt_excerpt: String,
    pub root_worker_id: Option<String>,
    pub visible_turn_id: Option<String>,
    pub result_summary: String,
    pub last_error: String,
    pub worker_count: usize,
    pub pending_approval_count: usize,
    pub artifact_count: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSummary {
    pub id: String,
    pub job_id: String,
    pub parent_worker_id: Option<String>,
    pub title: String,
    pub lane: String,
    pub state: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub provider_base_url: String,
    #[serde(default)]
    pub provider_api_key: String,
    #[serde(default)]
    pub provider_session_id: String,
    pub working_dir: String,
    #[serde(default)]
    pub read_roots: Vec<String>,
    #[serde(default)]
    pub write_roots: Vec<String>,
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_wall_clock_secs: u64,
    pub step_count: usize,
    pub tool_call_count: usize,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub capabilities: Vec<ToolCapabilitySummary>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallSummary {
    pub id: String,
    pub job_id: String,
    pub worker_id: String,
    pub tool_id: String,
    pub status: String,
    #[serde(default)]
    pub summary: String,
    pub args_json: Value,
    pub result_json: Option<Value>,
    pub policy_decision: Option<PolicyDecisionSummary>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    #[serde(default)]
    pub error_class: String,
    #[serde(default)]
    pub error_detail: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRequestSummary {
    pub id: String,
    pub job_id: String,
    pub worker_id: String,
    pub tool_call_id: String,
    pub state: String,
    pub risk_level: String,
    pub summary: String,
    pub detail: String,
    #[serde(default)]
    pub diff_preview: String,
    pub policy_decision: PolicyDecisionSummary,
    #[serde(default)]
    pub resolution_note: String,
    #[serde(default)]
    pub resolved_by: String,
    pub requested_at: i64,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub id: String,
    pub job_id: String,
    pub worker_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub command_session_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub path: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub preview_text: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandSessionSummary {
    pub id: String,
    pub job_id: String,
    pub worker_id: String,
    pub tool_call_id: Option<String>,
    pub mode: String,
    pub title: String,
    pub state: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub worktree_path: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub network_policy: String,
    pub timeout_secs: u64,
    pub output_limit_bytes: usize,
    #[serde(default)]
    pub last_error: String,
    pub exit_code: Option<i32>,
    pub stdout_artifact_id: Option<String>,
    pub stderr_artifact_id: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobEvent {
    pub id: i64,
    pub job_id: String,
    pub worker_id: Option<String>,
    pub event_type: String,
    pub status: String,
    pub summary: String,
    pub detail: String,
    pub data_json: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobDetail {
    pub job: JobSummary,
    #[serde(default)]
    pub workers: Vec<WorkerSummary>,
    #[serde(default)]
    pub child_jobs: Vec<JobSummary>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallSummary>,
    #[serde(default)]
    pub approvals: Vec<ApprovalRequestSummary>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactSummary>,
    #[serde(default)]
    pub command_sessions: Vec<CommandSessionSummary>,
    #[serde(default)]
    pub events: Vec<JobEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaybookSummary {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub description: String,
    pub prompt_excerpt: String,
    pub enabled: bool,
    pub policy_bundle: String,
    pub trigger_kind: String,
    pub schedule_interval_secs: Option<u64>,
    pub event_kind: Option<String>,
    pub profile_id: String,
    pub profile_title: String,
    pub project_id: String,
    pub project_title: String,
    pub working_dir: String,
    pub job_count: usize,
    pub last_job_id: Option<String>,
    pub last_job_state: String,
    pub last_run_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaybookDetail {
    pub playbook: PlaybookSummary,
    pub session: SessionSummary,
    pub prompt: String,
    pub recent_jobs: Vec<JobSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptProgressUpdate {
    pub session_id: String,
    pub status: String,
    pub label: String,
    pub detail: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub profile_title: String,
    pub route_id: String,
    pub route_title: String,
    pub attempt: usize,
    pub attempt_count: usize,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateSessionRequest {
    pub profile_id: Option<String>,
    pub route_id: Option<String>,
    pub provider: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub project_id: Option<String>,
    pub primary_project_id: Option<String>,
    pub project_ids: Option<Vec<String>>,
    pub approval_mode: Option<String>,
    pub execution_mode: Option<String>,
    pub run_budget_mode: Option<String>,
    pub workspace_mode: Option<String>,
    pub branch_name: Option<String>,
}

fn default_workspace_mode() -> String {
    "shared_project_root".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateSessionRequest {
    pub title: Option<String>,
    pub profile_id: Option<String>,
    pub route_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub state: Option<String>,
    pub project_id: Option<String>,
    pub primary_project_id: Option<String>,
    pub project_ids: Option<Vec<String>>,
    pub approval_mode: Option<String>,
    pub execution_mode: Option<String>,
    pub run_budget_mode: Option<String>,
    pub workspace_mode: Option<String>,
    pub branch_name: Option<String>,
}

fn default_session_approval_mode() -> String {
    "ask".to_string()
}

fn default_session_execution_mode() -> String {
    "act".to_string()
}

fn default_session_run_budget_mode() -> String {
    "inherit".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPromptRequest {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<SessionTurnImage>,
    #[serde(default = "default_compiler_role")]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledTurn {
    pub id: String,
    pub role: String,
    pub provider_neutral: bool,
    pub system_layers: Vec<CompiledPromptLayer>,
    pub project_layers: Vec<CompiledPromptLayer>,
    pub skill_layers: Vec<CompiledPromptLayer>,
    pub tool_catalog: Vec<NucleusToolDescriptor>,
    pub mcp_catalog: Vec<McpServerSummary>,
    pub history: Vec<CompiledConversationTurn>,
    pub user_turn: CompiledConversationTurn,
    pub capabilities: CompiledTurnCapabilities,
    pub debug_summary: CompiledTurnDebugSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledPromptLayer {
    pub id: String,
    pub kind: String,
    pub scope: String,
    pub title: String,
    pub source_path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledConversationTurn {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub images: Vec<SessionTurnImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledTurnCapabilities {
    pub needs_images: bool,
    pub needs_tools: bool,
    pub needs_mcp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledTurnDebugSummary {
    pub include_count: usize,
    #[serde(default)]
    pub memory_count: usize,
    #[serde(default)]
    pub memory_included_count: usize,
    #[serde(default)]
    pub memory_skipped_count: usize,
    #[serde(default)]
    pub memory_truncated_count: usize,
    pub skill_count: usize,
    pub mcp_server_count: usize,
    pub tool_count: usize,
    pub layer_count: usize,
    pub summary: String,
    #[serde(default)]
    pub skill_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillManifest {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    pub activation_mode: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub include_paths: Vec<String>,
    #[serde(default)]
    pub required_tools: Vec<String>,
    #[serde(default)]
    pub required_mcps: Vec<String>,
    #[serde(default)]
    pub project_filters: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NucleusToolDescriptor {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
    pub source: String,
}

fn default_mcp_transport() -> String {
    "stdio".to_string()
}
fn default_mcp_auth_kind() -> String {
    "none".to_string()
}
fn default_mcp_sync_status() -> String {
    "pending".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSummary {
    pub id: String,
    pub title: String,
    pub enabled: bool,
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env_json: Value,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers_json: Value,
    #[serde(default = "default_mcp_auth_kind")]
    pub auth_kind: String,
    #[serde(default)]
    pub auth_ref: String,
    #[serde(default = "default_mcp_sync_status")]
    pub sync_status: String,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub last_synced_at: Option<i64>,
    #[serde(default)]
    pub tools: Vec<NucleusToolDescriptor>,
    #[serde(default)]
    pub resources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerRecord {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub env_json: Value,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers_json: Value,
    #[serde(default = "default_mcp_auth_kind")]
    pub auth_kind: String,
    #[serde(default)]
    pub auth_ref: String,
    pub enabled: bool,
    pub sync_status: String,
    #[serde(default)]
    pub last_error: String,
    pub last_synced_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpToolRecord {
    pub id: String,
    pub server_id: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default)]
    pub source: String,
    pub discovered_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEntry {
    pub id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub enabled: bool,
    #[serde(default = "default_memory_status")]
    pub status: String,
    #[serde(default = "default_memory_kind")]
    pub memory_kind: String,
    #[serde(default = "default_memory_source_kind")]
    pub source_kind: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default = "default_memory_confidence")]
    pub confidence: f64,
    #[serde(default = "default_memory_created_by")]
    pub created_by: String,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    #[serde(default)]
    pub use_count: i64,
    #[serde(default)]
    pub supersedes_id: String,
    #[serde(default)]
    pub metadata_json: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEntryUpsertRequest {
    pub id: Option<String>,
    pub scope_kind: String,
    pub scope_id: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub enabled: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub memory_kind: Option<String>,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    #[serde(default)]
    pub use_count: Option<i64>,
    #[serde(default)]
    pub supersedes_id: Option<String>,
    #[serde(default)]
    pub metadata_json: Option<Value>,
}

fn default_memory_status() -> String {
    "accepted".to_string()
}
fn default_memory_kind() -> String {
    "note".to_string()
}
fn default_memory_source_kind() -> String {
    "manual".to_string()
}
fn default_memory_created_by() -> String {
    "user".to_string()
}
fn default_memory_confidence() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySummary {
    #[serde(default)]
    pub entries: Vec<MemoryEntry>,
    pub enabled_count: usize,
    pub scope_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultStatusSummary {
    pub initialized: bool,
    pub locked: bool,
    pub state: String,
    #[serde(default)]
    pub vault_id: String,
    #[serde(default)]
    pub cipher: String,
    #[serde(default)]
    pub kdf_algorithm: String,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultInitRequest {
    pub passphrase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultUnlockRequest {
    pub passphrase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSecretSummary {
    pub id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub configured: bool,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSecretListResponse {
    #[serde(default)]
    pub secrets: Vec<VaultSecretSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSecretUpsertRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub scope_kind: String,
    pub scope_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSecretUpdateRequest {
    #[serde(default)]
    pub scope_kind: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillPackageRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub manifest_json: Value,
    pub instructions: String,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub source_repo_url: String,
    #[serde(default)]
    pub source_owner: String,
    #[serde(default)]
    pub source_repo: String,
    #[serde(default)]
    pub source_ref: String,
    #[serde(default)]
    pub source_parent_path: String,
    #[serde(default)]
    pub source_skill_path: String,
    #[serde(default)]
    pub source_commit: String,
    #[serde(default)]
    pub imported_at: Option<i64>,
    #[serde(default)]
    pub last_checked_at: Option<i64>,
    #[serde(default)]
    pub latest_source_commit: String,
    #[serde(default)]
    pub update_status: String,
    #[serde(default)]
    pub content_checksum: String,
    #[serde(default)]
    pub dirty_status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillInstallationRecord {
    pub id: String,
    pub package_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub enabled: bool,
    pub pinned_version: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillPackageUpsertRequest {
    pub id: Option<String>,
    pub name: String,
    pub version: String,
    pub manifest_json: Value,
    pub instructions: String,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub source_repo_url: String,
    #[serde(default)]
    pub source_owner: String,
    #[serde(default)]
    pub source_repo: String,
    #[serde(default)]
    pub source_ref: String,
    #[serde(default)]
    pub source_parent_path: String,
    #[serde(default)]
    pub source_skill_path: String,
    #[serde(default)]
    pub source_commit: String,
    #[serde(default)]
    pub content_checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillImportRequest {
    pub source: String,
    #[serde(default)]
    pub scope_kind: String,
    #[serde(default)]
    pub scope_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillReconcileRequest {
    #[serde(default)]
    pub skill_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillReconcileCandidate {
    pub skill_id: String,
    pub title: String,
    pub path: String,
    pub already_registered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillReconcileScanResponse {
    pub skills_dir: String,
    #[serde(default)]
    pub candidates: Vec<SkillReconcileCandidate>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillInstallVerification {
    pub files_copied: bool,
    pub manifest_registered: bool,
    pub package_registered: bool,
    pub installation_registered: bool,
    pub instructions_non_empty: bool,
    pub source_metadata_stored: bool,
    pub checksum_recorded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillInstallResult {
    pub skill_id: String,
    pub package_id: String,
    pub installation_id: String,
    pub source_kind: String,
    pub source_url: String,
    pub source_repo: String,
    pub source_ref: String,
    pub source_skill_path: String,
    pub source_commit: String,
    pub content_checksum: String,
    pub dirty_status: String,
    pub update_status: String,
    pub status: String,
    pub verification: SkillInstallVerification,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillImportResponse {
    #[serde(default)]
    pub installed: Vec<SkillInstallResult>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillInstallationUpsertRequest {
    pub id: Option<String>,
    pub package_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub enabled: Option<bool>,
    pub pinned_version: Option<String>,
}

fn default_compiler_role() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalResolutionRequest {
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreatePlaybookRequest {
    pub title: String,
    pub description: Option<String>,
    pub prompt: String,
    pub profile_id: Option<String>,
    pub project_id: Option<String>,
    pub enabled: Option<bool>,
    pub policy_bundle: String,
    pub trigger_kind: String,
    pub schedule_interval_secs: Option<u64>,
    pub event_kind: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdatePlaybookRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub profile_id: Option<String>,
    pub project_id: Option<String>,
    pub enabled: Option<bool>,
    pub policy_bundle: Option<String>,
    pub trigger_kind: Option<String>,
    pub schedule_interval_secs: Option<Option<u64>>,
    pub event_kind: Option<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSummary {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub root_path: String,
    pub default_profile_id: String,
    pub main_target: String,
    pub utility_target: String,
    #[serde(default)]
    pub run_budget: RunBudgetSummary,
    pub profiles: Vec<WorkspaceProfileSummary>,
    pub projects: Vec<ProjectSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceUpdateRequest {
    pub root_path: Option<String>,
    pub default_profile_id: Option<String>,
    pub main_target: Option<String>,
    pub utility_target: Option<String>,
    pub run_budget: Option<RunBudgetSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectUpdateRequest {
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceModelConfig {
    pub adapter: String,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceProfileSummary {
    pub id: String,
    pub title: String,
    pub is_default: bool,
    pub main: WorkspaceModelConfig,
    pub utility: WorkspaceModelConfig,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceProfileWriteRequest {
    pub title: String,
    pub main: WorkspaceModelConfig,
    pub utility: WorkspaceModelConfig,
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteTarget {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouterProfileSummary {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub enabled: bool,
    pub state: String,
    pub targets: Vec<RouteTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionParameter {
    pub name: String,
    pub label: String,
    pub value_type: String,
    pub required: bool,
    pub description: String,
    pub default_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionSummary {
    pub id: String,
    pub title: String,
    pub category: String,
    pub summary: String,
    pub risk: String,
    pub requires_confirmation: bool,
    pub parameters: Vec<ActionParameter>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionRunRequest {
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionRunResponse {
    pub action_id: String,
    pub status: String,
    pub message: String,
    pub result: Value,
    pub audit_event_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: i64,
    pub kind: String,
    pub target: String,
    pub status: String,
    pub summary: String,
    pub detail: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostStatus {
    pub hostname: String,
    pub cpu_usage_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub process_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CpuCoreStat {
    pub id: usize,
    pub usage_percent: f32,
    pub frequency_mhz: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CpuStats {
    pub load_percent: f32,
    pub cores: Vec<CpuCoreStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryStats {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiskStat {
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemStats {
    pub hostname: String,
    pub current_user: String,
    pub process_count: usize,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    pub disks: Vec<DiskStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessStat {
    pub pid: u32,
    pub name: String,
    pub command: String,
    pub params: String,
    pub user: String,
    pub cwd: String,
    pub status: String,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub name: String,
    pub command: String,
    pub params: String,
    pub user: String,
    pub cwd: String,
    pub status: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub memory_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessListResponseMeta {
    pub total_processes: usize,
    pub matching_processes: usize,
    pub current_user: String,
    pub sort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessListResponse {
    pub processes: Vec<ProcessSnapshot>,
    pub meta: ProcessListResponseMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessKillRequest {
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessKillResponse {
    pub killed_pid: u32,
    pub name: String,
    pub signal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamConnected {
    pub service: String,
    pub version: String,
    pub compatibility: CompatibilitySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessStreamUpdate {
    pub sort: String,
    pub response: ProcessListResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageSummary {
    pub state_dir: String,
    pub database_path: String,
    pub artifacts_dir: String,
    pub memory_dir: String,
    pub transcripts_dir: String,
    pub playbooks_dir: String,
    pub scratch_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceSummary {
    pub name: String,
    pub repo_root: Option<String>,
    pub daemon_bind: String,
    pub install_kind: String,
    pub restart_mode: String,
    pub restart_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthSummary {
    pub enabled: bool,
    pub token_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionSummary {
    pub local_url: String,
    pub hostname_url: Option<String>,
    pub tailscale_url: Option<String>,
    pub web_mode: String,
    pub web_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalInterfaceSummary {
    pub name: String,
    pub address: String,
    pub is_loopback: bool,
    pub is_private: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecurityPostureSummary {
    pub configured_bind: String,
    pub exposure: String,
    pub https_active: bool,
    pub current_origin: Option<String>,
    pub current_origin_vault_safe: bool,
    pub current_origin_reason: String,
    #[serde(default)]
    pub local_interfaces: Vec<LocalInterfaceSummary>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompatibilitySummary {
    pub server_version: String,
    pub minimum_client_version: Option<String>,
    pub minimum_server_version: Option<String>,
    pub surface_version: String,
    #[serde(default)]
    pub capability_flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateStatus {
    pub install_kind: String,
    pub tracked_channel: Option<String>,
    pub tracked_ref: Option<String>,
    pub repo_root: Option<String>,
    pub current_ref: Option<String>,
    pub remote_name: Option<String>,
    pub remote_url: Option<String>,
    pub current_commit: Option<String>,
    pub current_commit_short: Option<String>,
    pub latest_commit: Option<String>,
    pub latest_commit_short: Option<String>,
    pub latest_version: Option<String>,
    pub latest_release_id: Option<String>,
    pub update_available: bool,
    pub dirty_worktree: bool,
    pub restart_required: bool,
    pub last_successful_check_at: Option<i64>,
    pub last_attempted_check_at: Option<i64>,
    pub last_attempt_result: Option<String>,
    pub latest_error: Option<String>,
    pub latest_error_at: Option<i64>,
    pub state: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsSummary {
    pub product: String,
    pub version: String,
    pub instance: InstanceSummary,
    pub storage: StorageSummary,
    pub auth: AuthSummary,
    pub connection: ConnectionSummary,
    pub security: SecurityPostureSummary,
    pub compatibility: CompatibilitySummary,
    pub update: UpdateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateConfigRequest {
    pub tracked_channel: Option<String>,
    pub tracked_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeOverview {
    pub product: String,
    pub version: String,
    pub runtimes: Vec<RuntimeSummary>,
    pub router_profiles: Vec<RouterProfileSummary>,
    pub workspace: WorkspaceSummary,
    pub sessions: Vec<SessionSummary>,
    pub host: HostStatus,
    pub storage: StorageSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", content = "data")]
pub enum DaemonEvent {
    #[serde(rename = "connected")]
    Connected(StreamConnected),
    #[serde(rename = "overview.updated")]
    OverviewUpdated(RuntimeOverview),
    #[serde(rename = "session.updated")]
    SessionUpdated(SessionDetail),
    #[serde(rename = "job.created")]
    JobCreated(JobSummary),
    #[serde(rename = "job.updated")]
    JobUpdated(JobSummary),
    #[serde(rename = "worker.updated")]
    WorkerUpdated(WorkerSummary),
    #[serde(rename = "approval.requested")]
    ApprovalRequested(ApprovalRequestSummary),
    #[serde(rename = "approval.resolved")]
    ApprovalResolved(ApprovalRequestSummary),
    #[serde(rename = "artifact.added")]
    ArtifactAdded(ArtifactSummary),
    #[serde(rename = "command_session.updated")]
    CommandSessionUpdated(CommandSessionSummary),
    #[serde(rename = "job.completed")]
    JobCompleted(JobSummary),
    #[serde(rename = "job.failed")]
    JobFailed(JobSummary),
    #[serde(rename = "prompt.progress")]
    PromptProgress(PromptProgressUpdate),
    #[serde(rename = "audit.updated")]
    AuditUpdated(Vec<AuditEvent>),
    #[serde(rename = "system.updated")]
    SystemUpdated(SystemStats),
    #[serde(rename = "processes.updated")]
    ProcessesUpdated(ProcessStreamUpdate),
    #[serde(rename = "update.updated")]
    UpdateUpdated(UpdateStatus),
}
