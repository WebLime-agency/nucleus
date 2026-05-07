use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub scope: String,
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
    pub events: Vec<JobEvent>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPromptRequest {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<SessionTurnImage>,
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
    pub profiles: Vec<WorkspaceProfileSummary>,
    pub projects: Vec<ProjectSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceUpdateRequest {
    pub root_path: Option<String>,
    pub default_profile_id: Option<String>,
    pub main_target: Option<String>,
    pub utility_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectUpdateRequest {
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceModelConfig {
    pub adapter: String,
    pub model: String,
    pub base_url: String,
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
