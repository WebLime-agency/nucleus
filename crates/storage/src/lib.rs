use std::{
    collections::HashSet,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result, anyhow, bail};
use nucleus_core::{
    AdapterKind, DEFAULT_OPENAI_COMPATIBLE_BASE_URL, DEFAULT_OPENAI_COMPATIBLE_MODEL, PRODUCT_SLUG,
};
use nucleus_protocol::{
    ApprovalRequestSummary, ArtifactSummary, AuditEvent, CommandSessionSummary,
    DEFAULT_JOB_MAX_STEPS, DEFAULT_JOB_MAX_TOOL_CALLS, DEFAULT_JOB_MAX_WALL_CLOCK_SECS,
    InstanceLogCategorySummary, InstanceLogEntry, JobDetail, JobEvent, JobSummary, McpServerRecord,
    McpServerSummary, MemoryCandidate, MemoryEntry, MemorySearchResult, PlaybookDetail,
    PlaybookSummary, PolicyDecisionSummary, ProjectSummary, RouteTarget, RouterProfileSummary,
    RunBudgetSummary, RuntimeSummary, SessionDetail, SessionProjectSummary, SessionSummary,
    SessionTurn, SessionTurnImage, SkillManifest, StorageSummary, ToolCallSummary,
    ToolCapabilitySummary, WorkerSummary, WorkspaceModelConfig, WorkspaceProfileSummary,
    WorkspaceSummary,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const LOCAL_AUTH_TOKEN_HASH_KEY: &str = "auth.local_token_hash";
const LOCAL_AUTH_TOKEN_FILE_NAME: &str = "local-auth-token";
const UPDATE_STATE_KEY: &str = "updates.state.v1";
const PLAYBOOK_RECENT_JOB_LIMIT: usize = 12;
pub const INSTANCE_LOG_RETENTION_DAYS: i64 = 30;
pub const INSTANCE_LOG_MAX_ROWS: usize = 5_000;
const INSTANCE_LOG_JSONL_FILE_NAME: &str = "events.jsonl";
const INSTANCE_LOG_JSONL_MAX_BYTES: u64 = 1_048_576;
const INSTANCE_LOG_JSONL_ROTATED_FILES: usize = 3;

#[derive(Debug, Clone)]
pub struct VaultStateRecord {
    pub id: String,
    pub version: i64,
    pub vault_id: String,
    pub status: String,
    pub kdf_algorithm: String,
    pub kdf_params_json: String,
    pub salt: Vec<u8>,
    pub cipher: String,
    pub encrypted_root_check: Vec<u8>,
    pub root_check_nonce: Vec<u8>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct VaultScopeKeyRecord {
    pub id: String,
    pub vault_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub encrypted_key: Vec<u8>,
    pub nonce: Vec<u8>,
    pub aad: String,
    pub key_version: i64,
    pub created_at: i64,
    pub rotated_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct VaultSecretRecord {
    pub id: String,
    pub scope_key_id: String,
    pub scope_kind: String,
    pub scope_id: String,
    pub name: String,
    pub description: String,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub aad: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultSecretPolicyRecord {
    pub id: String,
    pub secret_id: String,
    pub consumer_kind: String,
    pub consumer_id: String,
    pub permission: String,
    pub approval_mode: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePlan {
    pub state_dir: PathBuf,
    pub database_path: PathBuf,
    pub artifacts_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub memory_dir: PathBuf,
    pub transcripts_dir: PathBuf,
    pub playbooks_dir: PathBuf,
    pub scratch_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub profile_id: String,
    pub profile_title: String,
    pub route_id: String,
    pub route_title: String,
    pub scope: String,
    pub project_id: String,
    pub project_title: String,
    pub project_path: String,
    pub project_ids: Vec<String>,
    pub provider: String,
    pub model: String,
    pub provider_base_url: String,
    pub provider_api_key: String,
    pub working_dir: String,
    pub working_dir_kind: String,
    pub workspace_mode: String,
    pub source_project_path: String,
    pub git_root: String,
    pub worktree_path: String,
    pub git_branch: String,
    pub git_base_ref: String,
    pub git_head: String,
    pub git_dirty: bool,
    pub git_untracked_count: usize,
    pub git_remote_tracking_branch: String,
    pub workspace_warnings: Vec<String>,
    pub approval_mode: String,
    pub execution_mode: String,
    pub run_budget_mode: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPatch {
    pub title: Option<String>,
    pub profile_id: Option<String>,
    pub profile_title: Option<String>,
    pub route_id: Option<String>,
    pub route_title: Option<String>,
    pub scope: Option<String>,
    pub project_id: Option<String>,
    pub project_title: Option<String>,
    pub project_path: Option<String>,
    pub project_ids: Option<Vec<String>>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub provider_base_url: Option<String>,
    pub provider_api_key: Option<String>,
    pub working_dir: Option<String>,
    pub working_dir_kind: Option<String>,
    pub workspace_mode: Option<String>,
    pub source_project_path: Option<String>,
    pub git_root: Option<String>,
    pub worktree_path: Option<String>,
    pub git_branch: Option<String>,
    pub git_base_ref: Option<String>,
    pub git_head: Option<String>,
    pub git_dirty: Option<bool>,
    pub git_untracked_count: Option<usize>,
    pub git_remote_tracking_branch: Option<String>,
    pub workspace_warnings: Option<Vec<String>>,
    pub approval_mode: Option<String>,
    pub execution_mode: Option<String>,
    pub run_budget_mode: Option<String>,
    pub state: Option<String>,
    pub provider_session_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPatch {
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProfilePatch {
    pub title: String,
    pub main: WorkspaceModelConfig,
    pub utility: WorkspaceModelConfig,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEventRecord {
    pub kind: String,
    pub target: String,
    pub status: String,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceLogRecord {
    pub timestamp: i64,
    pub level: String,
    pub category: String,
    pub source: String,
    pub event: String,
    pub message: String,
    pub related_ids: serde_json::Value,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecisionRecord {
    pub decision: String,
    pub reason: String,
    pub matched_rule: String,
    pub scope_kind: String,
    pub risk_level: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobPatch {
    pub state: Option<String>,
    pub root_worker_id: Option<String>,
    pub visible_turn_id: Option<String>,
    pub result_summary: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRecord {
    pub id: String,
    pub job_id: String,
    pub parent_worker_id: Option<String>,
    pub title: String,
    pub lane: String,
    pub state: String,
    pub provider: String,
    pub model: String,
    pub provider_base_url: String,
    pub provider_api_key: String,
    pub provider_session_id: String,
    pub working_dir: String,
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_wall_clock_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerPatch {
    pub state: Option<String>,
    pub provider_session_id: Option<String>,
    pub step_count: Option<usize>,
    pub tool_call_count: Option<usize>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCapabilityGrantRecord {
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

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    pub id: String,
    pub job_id: String,
    pub worker_id: String,
    pub tool_id: String,
    pub status: String,
    pub summary: String,
    pub args_json: serde_json::Value,
    pub result_json: Option<serde_json::Value>,
    pub policy_decision: Option<PolicyDecisionRecord>,
    pub artifact_ids: Vec<String>,
    pub error_class: String,
    pub error_detail: String,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCallPatch {
    pub status: Option<String>,
    pub summary: Option<String>,
    pub result_json: Option<Option<serde_json::Value>>,
    pub policy_decision: Option<Option<PolicyDecisionRecord>>,
    pub artifact_ids: Option<Vec<String>>,
    pub error_class: Option<String>,
    pub error_detail: Option<String>,
    pub started_at: Option<Option<i64>>,
    pub completed_at: Option<Option<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequestRecord {
    pub id: String,
    pub job_id: String,
    pub worker_id: String,
    pub tool_call_id: String,
    pub state: String,
    pub risk_level: String,
    pub summary: String,
    pub detail: String,
    pub diff_preview: String,
    pub policy_decision: PolicyDecisionRecord,
    pub resolution_note: String,
    pub resolved_by: String,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobArtifactRecord {
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
    pub preview_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobArtifactPatch {
    pub kind: Option<String>,
    pub title: Option<String>,
    pub path: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub preview_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybookRecord {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub description: String,
    pub prompt: String,
    pub enabled: bool,
    pub policy_bundle: String,
    pub trigger_kind: String,
    pub schedule_interval_secs: Option<u64>,
    pub event_kind: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybookPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub session_id: Option<String>,
    pub enabled: Option<bool>,
    pub policy_bundle: Option<String>,
    pub trigger_kind: Option<String>,
    pub schedule_interval_secs: Option<Option<u64>>,
    pub event_kind: Option<Option<String>>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StoredPlaybook {
    id: String,
    session_id: String,
    title: String,
    description: String,
    prompt: String,
    enabled: bool,
    policy_bundle: String,
    trigger_kind: String,
    schedule_interval_secs: Option<u64>,
    event_kind: Option<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSessionRecord {
    pub id: String,
    pub job_id: String,
    pub worker_id: String,
    pub tool_call_id: Option<String>,
    pub mode: String,
    pub title: String,
    pub state: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub session_id: String,
    pub project_id: String,
    pub worktree_path: String,
    pub branch: String,
    pub port: Option<u16>,
    pub env_json: serde_json::Value,
    pub network_policy: String,
    pub timeout_secs: u64,
    pub output_limit_bytes: usize,
    pub last_error: String,
    pub exit_code: Option<i32>,
    pub stdout_artifact_id: Option<String>,
    pub stderr_artifact_id: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandSessionPatch {
    pub mode: Option<String>,
    pub title: Option<String>,
    pub state: Option<String>,
    pub last_error: Option<String>,
    pub exit_code: Option<Option<i32>>,
    pub stdout_artifact_id: Option<Option<String>>,
    pub stderr_artifact_id: Option<Option<String>>,
    pub started_at: Option<Option<i64>>,
    pub completed_at: Option<Option<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerPatch {
    pub title: Option<String>,
    pub transport: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_json: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub sync_status: Option<String>,
    pub last_error: Option<String>,
    pub last_synced_at: Option<Option<i64>>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolPatch {
    pub description: Option<String>,
    pub input_schema: Option<serde_json::Value>,
    pub source: Option<String>,
    pub discovered_at: Option<i64>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPackagePatch {
    pub name: Option<String>,
    pub version: Option<String>,
    pub manifest_json: Option<serde_json::Value>,
    pub instructions: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInstallationPatch {
    pub enabled: Option<bool>,
    pub pinned_version: Option<Option<String>>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobEventRecord {
    pub job_id: String,
    pub worker_id: Option<String>,
    pub event_type: String,
    pub status: String,
    pub summary: String,
    pub detail: String,
    pub data_json: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub relative_path: String,
    pub absolute_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredUpdateState {
    pub tracked_channel: Option<String>,
    pub tracked_ref: Option<String>,
    pub release_manifest_url: Option<String>,
    pub pending_restart_release_id: Option<String>,
    pub update_available: bool,
    pub last_successful_check_at: Option<i64>,
    pub last_successful_target_version: Option<String>,
    pub last_successful_target_release_id: Option<String>,
    pub last_successful_target_commit: Option<String>,
    pub last_attempted_check_at: Option<i64>,
    pub last_attempt_result: Option<String>,
    pub latest_error: Option<String>,
    pub latest_error_at: Option<i64>,
    pub restart_required: bool,
}

impl StoragePlan {
    pub fn resolve() -> Result<Self> {
        let state_dir = match env::var("NUCLEUS_STATE_DIR") {
            Ok(path) => PathBuf::from(path),
            Err(_) => default_state_dir()?,
        };

        Ok(Self::from_state_dir(state_dir))
    }

    pub fn from_state_dir(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        let database_path = state_dir.join(format!("{PRODUCT_SLUG}.db"));
        let artifacts_dir = state_dir.join("artifacts");
        let logs_dir = state_dir.join("logs");
        let memory_dir = state_dir.join("memory");
        let transcripts_dir = state_dir.join("transcripts");
        let playbooks_dir = state_dir.join("playbooks");
        let scratch_dir = state_dir.join("scratch");

        Self {
            state_dir,
            database_path,
            artifacts_dir,
            logs_dir,
            memory_dir,
            transcripts_dir,
            playbooks_dir,
            scratch_dir,
        }
    }

    pub fn ensure_layout(&self) -> Result<()> {
        for path in [
            &self.state_dir,
            &self.artifacts_dir,
            &self.logs_dir,
            &self.memory_dir,
            &self.transcripts_dir,
            &self.playbooks_dir,
            &self.scratch_dir,
        ] {
            fs::create_dir_all(path)
                .with_context(|| format!("failed to create state path {}", path.display()))?;
        }

        Ok(())
    }

    pub fn summary(&self) -> StorageSummary {
        StorageSummary {
            state_dir: display(&self.state_dir),
            database_path: display(&self.database_path),
            artifacts_dir: display(&self.artifacts_dir),
            logs_dir: display(&self.logs_dir),
            memory_dir: display(&self.memory_dir),
            transcripts_dir: display(&self.transcripts_dir),
            playbooks_dir: display(&self.playbooks_dir),
            scratch_dir: display(&self.scratch_dir),
        }
    }
}

pub struct StateStore {
    plan: StoragePlan,
    connection: Mutex<Connection>,
}

impl StateStore {
    pub fn initialize() -> Result<Self> {
        let plan = StoragePlan::resolve()?;
        Self::initialize_with_plan(plan)
    }

    pub fn initialize_at(state_dir: impl Into<PathBuf>) -> Result<Self> {
        Self::initialize_with_plan(StoragePlan::from_state_dir(state_dir))
    }

    fn initialize_with_plan(plan: StoragePlan) -> Result<Self> {
        plan.ensure_layout()?;

        let connection = Connection::open(&plan.database_path).with_context(|| {
            format!(
                "failed to open SQLite database at {}",
                plan.database_path.display()
            )
        })?;

        configure_connection(&connection)?;
        initialize_schema(&connection)?;
        seed_runtimes(&connection)?;
        seed_router_profiles(&connection)?;
        seed_workspace_settings(&connection)?;
        seed_workspace_profiles(&connection)?;
        migrate_legacy_workspace_targets(&connection)?;
        migrate_seeded_cli_defaults_to_protocol(&connection)?;
        ensure_local_auth_token_with_connection(&plan, &connection)?;
        sync_projects_with_connection(&connection)?;

        Ok(Self {
            plan,
            connection: Mutex::new(connection),
        })
    }

    pub fn storage_summary(&self) -> StorageSummary {
        self.plan.summary()
    }

    pub fn state_dir_path(&self) -> PathBuf {
        self.plan.state_dir.clone()
    }

    pub fn artifacts_dir_path(&self) -> PathBuf {
        self.plan.artifacts_dir.clone()
    }

    pub fn logs_dir_path(&self) -> PathBuf {
        self.plan.logs_dir.clone()
    }

    pub fn playbooks_dir_path(&self) -> PathBuf {
        self.plan.playbooks_dir.clone()
    }

    pub fn local_auth_token_path(&self) -> String {
        display(&self.plan.state_dir.join(LOCAL_AUTH_TOKEN_FILE_NAME))
    }

    pub fn read_local_auth_token(&self) -> Result<String> {
        fs::read_to_string(self.plan.state_dir.join(LOCAL_AUTH_TOKEN_FILE_NAME))
            .with_context(|| "failed to read local auth token".to_string())
            .map(|value| value.trim().to_string())
    }

    pub fn validate_access_token(&self, token: &str) -> Result<bool> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let Some(expected_hash) = setting_value_optional(&connection, LOCAL_AUTH_TOKEN_HASH_KEY)?
        else {
            return Ok(false);
        };

        Ok(hash_auth_token(token) == expected_hash)
    }

    pub fn read_update_state(&self) -> Result<StoredUpdateState> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let Some(payload) = setting_value_optional(&connection, UPDATE_STATE_KEY)? else {
            return Ok(StoredUpdateState::default());
        };

        serde_json::from_str(&payload).context("failed to decode stored update state")
    }

    pub fn write_update_state(&self, state: &StoredUpdateState) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let payload =
            serde_json::to_string(state).context("failed to serialize stored update state")?;
        set_setting_value(&connection, UPDATE_STATE_KEY, &payload)
    }

    pub fn list_runtimes(&self) -> Result<Vec<RuntimeSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, summary, state FROM runtimes ORDER BY CASE id
                WHEN 'openai_compatible' THEN 1
                WHEN 'claude' THEN 2
                WHEN 'codex' THEN 3
                WHEN 'system' THEN 4
                ELSE 100
             END, id",
        )?;

        let rows = statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let summary: String = row.get(1)?;
            let state: String = row.get(2)?;
            let kind = AdapterKind::parse(&id);

            Ok(RuntimeSummary {
                id,
                summary,
                state,
                auth_state: match kind {
                    Some(AdapterKind::System) => "not_required".to_string(),
                    _ => "unknown".to_string(),
                },
                version: String::new(),
                executable_path: String::new(),
                default_model: kind
                    .map(|adapter| adapter.default_model().to_string())
                    .unwrap_or_default(),
                note: String::new(),
                supports_sessions: kind
                    .map(|adapter| adapter.supports_sessions())
                    .unwrap_or(false),
                supports_prompting: kind
                    .map(|adapter| adapter.supports_prompting())
                    .unwrap_or(false),
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load runtimes")
    }

    pub fn workspace(&self) -> Result<WorkspaceSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        sync_projects_with_connection(&connection)?;
        load_workspace_summary(&connection)
    }

    pub fn update_workspace(
        &self,
        root_path: Option<&str>,
        default_profile_id: Option<&str>,
        main_target: Option<&str>,
        utility_target: Option<&str>,
        run_budget: Option<&RunBudgetSummary>,
    ) -> Result<WorkspaceSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        if let Some(root_path) = root_path {
            set_workspace_root(&connection, root_path)?;
        }
        if let Some(default_profile_id) = default_profile_id {
            set_workspace_default_profile_id(&connection, default_profile_id)?;
        }
        if let Some(main_target) = main_target {
            set_workspace_main_target(&connection, main_target)?;
        }
        if let Some(utility_target) = utility_target {
            set_workspace_utility_target(&connection, utility_target)?;
        }
        if let Some(run_budget) = run_budget {
            set_workspace_run_budget(&connection, run_budget)?;
        }
        sync_projects_with_connection(&connection)?;
        load_workspace_summary(&connection)
    }

    pub fn create_workspace_profile(
        &self,
        patch: WorkspaceProfilePatch,
    ) -> Result<WorkspaceProfileSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let profile_id = create_workspace_profile_with_connection(&connection, &patch)?;

        if patch.is_default {
            set_workspace_default_profile_id(&connection, &profile_id)?;
        }

        load_workspace_profile_summary(&connection, &profile_id)
    }

    pub fn update_workspace_profile(
        &self,
        profile_id: &str,
        patch: WorkspaceProfilePatch,
    ) -> Result<WorkspaceProfileSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        update_workspace_profile_with_connection(&connection, profile_id, &patch)?;

        if patch.is_default {
            set_workspace_default_profile_id(&connection, profile_id)?;
        } else if workspace_default_profile_id_optional(&connection)?
            .as_deref()
            .is_some_and(|current| current == profile_id)
        {
            set_workspace_default_profile_id(&connection, profile_id)?;
        }

        load_workspace_profile_summary(&connection, profile_id)
    }

    pub fn delete_workspace_profile(&self, profile_id: &str) -> Result<WorkspaceSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        delete_workspace_profile_with_connection(&connection, profile_id)?;
        ensure_workspace_default_profile(&connection)?;
        load_workspace_summary(&connection)
    }

    pub fn list_skill_manifests(&self) -> Result<Vec<SkillManifest>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_skill_manifests_with_connection(&connection)
    }

    pub fn upsert_skill_manifest(&self, manifest: &SkillManifest) -> Result<SkillManifest> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO skill_manifests (
                id, title, description, instructions, activation_mode, triggers_json,
                include_paths_json, required_tools_json, required_mcps_json,
                project_filters_json, enabled
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                instructions = excluded.instructions,
                activation_mode = excluded.activation_mode,
                triggers_json = excluded.triggers_json,
                include_paths_json = excluded.include_paths_json,
                required_tools_json = excluded.required_tools_json,
                required_mcps_json = excluded.required_mcps_json,
                project_filters_json = excluded.project_filters_json,
                enabled = excluded.enabled,
                updated_at = unixepoch()
            ",
            params![
                manifest.id,
                manifest.title,
                manifest.description,
                manifest.instructions,
                manifest.activation_mode,
                serde_json::to_string(&manifest.triggers)?,
                serde_json::to_string(&manifest.include_paths)?,
                serde_json::to_string(&manifest.required_tools)?,
                serde_json::to_string(&manifest.required_mcps)?,
                serde_json::to_string(&manifest.project_filters)?,
                manifest.enabled as i64,
            ],
        )?;
        load_skill_manifest(&connection, &manifest.id)
    }

    pub fn delete_skill_manifest(&self, id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute("DELETE FROM skill_manifests WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn list_mcp_servers(&self) -> Result<Vec<McpServerSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_mcp_servers_with_connection(&connection)
    }

    pub fn list_mcp_server_records(&self) -> Result<Vec<McpServerRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_mcp_server_records_with_connection(&connection)
    }

    pub fn upsert_mcp_server(&self, server: &McpServerSummary) -> Result<McpServerSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let existing = match load_mcp_server_record(&connection, &server.id) {
            Ok(record) => Some(record),
            Err(error) if error.to_string().contains("was not found") => None,
            Err(error) => return Err(error),
        };
        let record = McpServerRecord {
            id: server.id.clone(),
            workspace_id: existing
                .as_ref()
                .map(|value| value.workspace_id.clone())
                .unwrap_or_else(|| "workspace".to_string()),
            title: server.title.clone(),
            transport: if server.transport.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.transport.clone())
                    .unwrap_or_else(|| "stdio".to_string())
            } else {
                server.transport.clone()
            },
            command: if server.command.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.command.clone())
                    .unwrap_or_default()
            } else {
                server.command.clone()
            },
            args: if server.args.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.args.clone())
                    .unwrap_or_default()
            } else {
                server.args.clone()
            },
            env_json: if server.env_json.is_null() {
                existing
                    .as_ref()
                    .map(|value| value.env_json.clone())
                    .unwrap_or_else(|| json!({}))
            } else {
                server.env_json.clone()
            },
            url: if server.url.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.url.clone())
                    .unwrap_or_default()
            } else {
                server.url.clone()
            },
            headers_json: if server.headers_json.is_null() {
                existing
                    .as_ref()
                    .map(|value| value.headers_json.clone())
                    .unwrap_or_else(|| json!({}))
            } else {
                server.headers_json.clone()
            },
            auth_kind: if server.auth_kind.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.auth_kind.clone())
                    .unwrap_or_else(|| "none".to_string())
            } else {
                server.auth_kind.clone()
            },
            auth_ref: if server.auth_ref.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.auth_ref.clone())
                    .unwrap_or_default()
            } else {
                server.auth_ref.clone()
            },
            enabled: server.enabled,
            sync_status: if server.sync_status.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.sync_status.clone())
                    .unwrap_or_else(|| "pending".to_string())
            } else {
                server.sync_status.clone()
            },
            last_error: if server.last_error.is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.last_error.clone())
                    .unwrap_or_default()
            } else {
                server.last_error.clone()
            },
            last_synced_at: server
                .last_synced_at
                .or_else(|| existing.as_ref().and_then(|value| value.last_synced_at)),
            created_at: existing.as_ref().map(|value| value.created_at).unwrap_or(0),
            updated_at: existing.as_ref().map(|value| value.updated_at).unwrap_or(0),
        };
        upsert_mcp_server_record_with_summary(&connection, &record, server)
    }

    pub fn upsert_mcp_server_record(
        &self,
        record: &McpServerRecord,
        tools: &[nucleus_protocol::NucleusToolDescriptor],
        resources: &[String],
    ) -> Result<McpServerRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        upsert_mcp_server_record_only(&connection, record, tools, resources)?;
        load_mcp_server_record(&connection, &record.id)
    }

    pub fn delete_mcp_server(&self, id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute("DELETE FROM mcp_tools WHERE server_id = ?1", params![id])?;
        connection.execute("DELETE FROM mcp_servers WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn list_mcp_tools(&self) -> Result<Vec<nucleus_protocol::McpToolRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_mcp_tools_with_connection(&connection)
    }

    pub fn upsert_mcp_tool(
        &self,
        tool: &nucleus_protocol::McpToolRecord,
    ) -> Result<nucleus_protocol::McpToolRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO mcp_tools (
                id, server_id, name, description, input_schema_json, source, discovered_at, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                server_id = excluded.server_id,
                name = excluded.name,
                description = excluded.description,
                input_schema_json = excluded.input_schema_json,
                source = excluded.source,
                discovered_at = excluded.discovered_at,
                updated_at = excluded.updated_at
            ",
            params![
                tool.id,
                tool.server_id,
                tool.name,
                tool.description,
                serde_json::to_string(&tool.input_schema)?,
                tool.source,
                tool.discovered_at,
                tool.created_at,
                tool.updated_at,
            ],
        )?;
        load_mcp_tool(&connection, &tool.id)
    }

    pub fn list_memory_entries(&self) -> Result<Vec<MemoryEntry>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_memory_entries_with_connection(&connection)
    }

    pub fn get_memory_entry(&self, id: &str) -> Result<MemoryEntry> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_memory_entry(&connection, id)
    }

    pub fn upsert_memory_entry(&self, entry: &MemoryEntry) -> Result<MemoryEntry> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO memory_entries (
                id, scope_kind, scope_id, title, content, tags_json, enabled, status, memory_kind, source_kind, source_id, confidence, created_by, last_used_at, use_count, supersedes_id, metadata_json, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                scope_kind = excluded.scope_kind,
                scope_id = excluded.scope_id,
                title = excluded.title,
                content = excluded.content,
                tags_json = excluded.tags_json,
                enabled = excluded.enabled,
                status = excluded.status,
                memory_kind = excluded.memory_kind,
                source_kind = excluded.source_kind,
                source_id = excluded.source_id,
                confidence = excluded.confidence,
                created_by = excluded.created_by,
                last_used_at = excluded.last_used_at,
                use_count = excluded.use_count,
                supersedes_id = excluded.supersedes_id,
                metadata_json = excluded.metadata_json,
                updated_at = excluded.updated_at
            ",
            params![
                entry.id,
                entry.scope_kind,
                entry.scope_id,
                entry.title,
                entry.content,
                serde_json::to_string(&entry.tags)?,
                entry.enabled as i64,
                entry.status,
                entry.memory_kind,
                entry.source_kind,
                entry.source_id,
                entry.confidence,
                entry.created_by,
                entry.last_used_at,
                entry.use_count,
                entry.supersedes_id,
                serde_json::to_string(&entry.metadata_json)?,
                entry.created_at,
                entry.updated_at,
            ],
        )?;
        refresh_memory_fts_row(&connection, &entry.id)?;
        load_memory_entry(&connection, &entry.id)
    }

    pub fn delete_memory_entry(&self, id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        delete_memory_fts_row(&connection, id)?;
        connection.execute("DELETE FROM memory_entries WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn rebuild_memory_search_index(&self) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        rebuild_memory_search_index_with_connection(&connection)
    }

    pub fn search_memory_entries(
        &self,
        query: &str,
        scope_kind: Option<&str>,
        scope_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        search_memory_entries_with_connection(&connection, query, scope_kind, scope_id, limit)
    }

    pub fn record_memory_entries_used(&self, ids: &[String]) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        for id in ids {
            connection.execute(
                "UPDATE memory_entries SET use_count = use_count + 1, last_used_at = unixepoch(), updated_at = updated_at WHERE id = ?1",
                params![id],
            )?;
        }
        Ok(())
    }

    pub fn list_memory_candidates(&self) -> Result<Vec<MemoryCandidate>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_memory_candidates_with_connection(&connection)
    }

    pub fn upsert_memory_candidate(&self, candidate: &MemoryCandidate) -> Result<MemoryCandidate> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "INSERT INTO memory_candidates (id, scope_kind, scope_id, session_id, turn_id_start, turn_id_end, candidate_kind, title, content, tags_json, evidence_json, reason, confidence, status, dedupe_key, accepted_memory_id, created_by, metadata_json, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,unixepoch(),unixepoch()) ON CONFLICT(id) DO UPDATE SET scope_kind=excluded.scope_kind, scope_id=excluded.scope_id, session_id=excluded.session_id, turn_id_start=excluded.turn_id_start, turn_id_end=excluded.turn_id_end, candidate_kind=excluded.candidate_kind, title=excluded.title, content=excluded.content, tags_json=excluded.tags_json, evidence_json=excluded.evidence_json, reason=excluded.reason, confidence=excluded.confidence, status=excluded.status, dedupe_key=excluded.dedupe_key, accepted_memory_id=excluded.accepted_memory_id, created_by=excluded.created_by, metadata_json=excluded.metadata_json, updated_at=unixepoch()",
            params![candidate.id,candidate.scope_kind,candidate.scope_id,candidate.session_id,candidate.turn_id_start,candidate.turn_id_end,candidate.candidate_kind,candidate.title,candidate.content,serde_json::to_string(&candidate.tags)?,serde_json::to_string(&candidate.evidence)?,candidate.reason,candidate.confidence,candidate.status,candidate.dedupe_key,candidate.accepted_memory_id,candidate.created_by,serde_json::to_string(&candidate.metadata_json)?],
        )?;
        load_memory_candidate(&connection, &candidate.id)
    }

    pub fn load_memory_candidate(&self, id: &str) -> Result<MemoryCandidate> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_memory_candidate(&connection, id)
    }

    pub fn delete_memory_candidate(&self, id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute("UPDATE memory_candidates SET status = 'dismissed', updated_at = unixepoch() WHERE id = ?1 AND status != 'accepted'", params![id])?;
        Ok(())
    }

    pub fn load_vault_state(&self) -> Result<Option<VaultStateRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.query_row(
            "SELECT id, version, vault_id, status, kdf_algorithm, kdf_params_json, salt, cipher, encrypted_root_check, root_check_nonce, created_at, updated_at FROM vault_state WHERE id = 'default'",
            [],
            map_vault_state,
        ).optional().map_err(Into::into)
    }

    pub fn upsert_vault_state(&self, record: &VaultStateRecord) -> Result<VaultStateRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "INSERT INTO vault_state (id, version, vault_id, status, kdf_algorithm, kdf_params_json, salt, cipher, encrypted_root_check, root_check_nonce, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, COALESCE(NULLIF(?11, 0), unixepoch()), COALESCE(NULLIF(?12, 0), unixepoch()))
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, kdf_algorithm = excluded.kdf_algorithm, kdf_params_json = excluded.kdf_params_json, salt = excluded.salt, cipher = excluded.cipher, encrypted_root_check = excluded.encrypted_root_check, root_check_nonce = excluded.root_check_nonce, updated_at = unixepoch()",
            params![record.id, record.version, record.vault_id, record.status, record.kdf_algorithm, record.kdf_params_json, record.salt, record.cipher, record.encrypted_root_check, record.root_check_nonce, record.created_at, record.updated_at],
        )?;
        drop(connection);
        self.load_vault_state()?
            .ok_or_else(|| anyhow!("vault state was not found"))
    }

    pub fn load_vault_scope_key(
        &self,
        scope_kind: &str,
        scope_id: &str,
    ) -> Result<Option<VaultScopeKeyRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.query_row(
            "SELECT id, vault_id, scope_kind, scope_id, encrypted_key, nonce, aad, key_version, created_at, rotated_at FROM vault_scope_keys WHERE scope_kind = ?1 AND scope_id = ?2",
            params![scope_kind, scope_id],
            map_vault_scope_key,
        ).optional().map_err(Into::into)
    }

    pub fn upsert_vault_scope_key(
        &self,
        record: &VaultScopeKeyRecord,
    ) -> Result<VaultScopeKeyRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "INSERT INTO vault_scope_keys (id, vault_id, scope_kind, scope_id, encrypted_key, nonce, aad, key_version, created_at, rotated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, COALESCE(NULLIF(?9, 0), unixepoch()), ?10)
             ON CONFLICT(vault_id, scope_kind, scope_id) DO UPDATE SET encrypted_key = excluded.encrypted_key, nonce = excluded.nonce, aad = excluded.aad, key_version = excluded.key_version, rotated_at = excluded.rotated_at",
            params![record.id, record.vault_id, record.scope_kind, record.scope_id, record.encrypted_key, record.nonce, record.aad, record.key_version, record.created_at, record.rotated_at],
        )?;
        drop(connection);
        self.load_vault_scope_key(&record.scope_kind, &record.scope_id)?
            .ok_or_else(|| anyhow!("vault scope key was not found"))
    }

    pub fn list_vault_secrets(
        &self,
        scope_kind: Option<&str>,
        scope_id: Option<&str>,
    ) -> Result<Vec<VaultSecretRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let mut query = "SELECT id FROM vault_secrets".to_string();
        let mut values = Vec::new();
        if let (Some(kind), Some(id)) = (scope_kind, scope_id) {
            query.push_str(" WHERE scope_kind = ?1 AND scope_id = ?2");
            values.push(kind.to_string());
            values.push(id.to_string());
        }
        query.push_str(" ORDER BY scope_kind ASC, scope_id ASC, name ASC");
        let mut statement = connection.prepare(&query)?;
        let ids = if values.is_empty() {
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            statement
                .query_map(params![values[0], values[1]], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        ids.into_iter()
            .map(|id| load_vault_secret(&connection, &id))
            .collect()
    }

    pub fn load_vault_secret(&self, id: &str) -> Result<VaultSecretRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_vault_secret(&connection, id)
    }

    pub fn upsert_vault_secret(&self, record: &VaultSecretRecord) -> Result<VaultSecretRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "INSERT INTO vault_secrets (id, scope_key_id, scope_kind, scope_id, name, description, ciphertext, nonce, aad, version, created_at, updated_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, COALESCE(NULLIF(?11, 0), unixepoch()), COALESCE(NULLIF(?12, 0), unixepoch()), ?13)
             ON CONFLICT(id) DO UPDATE SET scope_key_id = excluded.scope_key_id, scope_kind = excluded.scope_kind, scope_id = excluded.scope_id, name = excluded.name, description = excluded.description, ciphertext = excluded.ciphertext, nonce = excluded.nonce, aad = excluded.aad, version = excluded.version, updated_at = unixepoch(), last_used_at = excluded.last_used_at",
            params![record.id, record.scope_key_id, record.scope_kind, record.scope_id, record.name, record.description, record.ciphertext, record.nonce, record.aad, record.version, record.created_at, record.updated_at, record.last_used_at],
        )?;
        drop(connection);
        self.load_vault_secret(&record.id)
    }

    pub fn delete_vault_secret(&self, id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "DELETE FROM vault_secret_policies WHERE secret_id = ?1",
            params![id],
        )?;
        connection.execute(
            "DELETE FROM vault_secret_usages WHERE secret_id = ?1",
            params![id],
        )?;
        connection.execute("DELETE FROM vault_secrets WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn list_vault_secret_policies(
        &self,
        secret_id: &str,
    ) -> Result<Vec<VaultSecretPolicyRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, secret_id, consumer_kind, consumer_id, permission, approval_mode, created_at, updated_at
             FROM vault_secret_policies WHERE secret_id = ?1 ORDER BY consumer_kind ASC, consumer_id ASC, permission ASC",
        )?;
        let rows = statement.query_map(params![secret_id], map_vault_secret_policy)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn upsert_vault_secret_policy(
        &self,
        record: &VaultSecretPolicyRecord,
    ) -> Result<VaultSecretPolicyRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "INSERT INTO vault_secret_policies (id, secret_id, consumer_kind, consumer_id, permission, approval_mode, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(NULLIF(?7, 0), unixepoch()), COALESCE(NULLIF(?8, 0), unixepoch()))
             ON CONFLICT(secret_id, consumer_kind, consumer_id, permission) DO UPDATE SET approval_mode = excluded.approval_mode, updated_at = unixepoch()",
            params![record.id, record.secret_id, record.consumer_kind, record.consumer_id, record.permission, record.approval_mode, record.created_at, record.updated_at],
        )?;
        connection.query_row(
            "SELECT id, secret_id, consumer_kind, consumer_id, permission, approval_mode, created_at, updated_at
             FROM vault_secret_policies WHERE secret_id = ?1 AND consumer_kind = ?2 AND consumer_id = ?3 AND permission = ?4",
            params![record.secret_id, record.consumer_kind, record.consumer_id, record.permission],
            map_vault_secret_policy,
        ).map_err(Into::into)
    }

    pub fn delete_vault_secret_policy(&self, secret_id: &str, policy_id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "DELETE FROM vault_secret_policies WHERE secret_id = ?1 AND id = ?2",
            params![secret_id, policy_id],
        )?;
        Ok(())
    }

    pub fn record_vault_secret_usage(
        &self,
        secret_id: &str,
        consumer_kind: &str,
        consumer_id: &str,
        purpose: &str,
    ) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "UPDATE vault_secrets SET last_used_at = unixepoch(), updated_at = updated_at WHERE id = ?1",
            params![secret_id],
        )?;
        connection.execute(
            "INSERT INTO vault_secret_usages (id, secret_id, consumer_kind, consumer_id, purpose, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, unixepoch(), unixepoch())
             ",
            params![Uuid::new_v4().to_string(), secret_id, consumer_kind, consumer_id, purpose],
        )?;
        Ok(())
    }

    pub fn list_skill_packages(&self) -> Result<Vec<nucleus_protocol::SkillPackageRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_skill_packages_with_connection(&connection)
    }

    pub fn upsert_skill_package(
        &self,
        package: &nucleus_protocol::SkillPackageRecord,
    ) -> Result<nucleus_protocol::SkillPackageRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO skill_packages (
                id, name, version, manifest_json, instructions, source_kind, source_url, source_repo_url, source_owner, source_repo, source_ref, source_parent_path, source_skill_path, source_commit, imported_at, last_checked_at, latest_source_commit, update_status, content_checksum, dirty_status, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                version = excluded.version,
                manifest_json = excluded.manifest_json,
                instructions = excluded.instructions,
                source_kind = excluded.source_kind,
                source_url = excluded.source_url,
                source_repo_url = excluded.source_repo_url,
                source_owner = excluded.source_owner,
                source_repo = excluded.source_repo,
                source_ref = excluded.source_ref,
                source_parent_path = excluded.source_parent_path,
                source_skill_path = excluded.source_skill_path,
                source_commit = excluded.source_commit,
                imported_at = excluded.imported_at,
                last_checked_at = excluded.last_checked_at,
                latest_source_commit = excluded.latest_source_commit,
                update_status = excluded.update_status,
                content_checksum = excluded.content_checksum,
                dirty_status = excluded.dirty_status,
                updated_at = excluded.updated_at
            ",
            params![
                package.id,
                package.name,
                package.version,
                serde_json::to_string(&package.manifest_json)?,
                package.instructions,
                package.source_kind,
                package.source_url,
                package.source_repo_url,
                package.source_owner,
                package.source_repo,
                package.source_ref,
                package.source_parent_path,
                package.source_skill_path,
                package.source_commit,
                package.imported_at,
                package.last_checked_at,
                package.latest_source_commit,
                package.update_status,
                package.content_checksum,
                package.dirty_status,
                package.created_at,
                package.updated_at,
            ],
        )?;
        load_skill_package(&connection, &package.id)
    }

    pub fn list_skill_installations(
        &self,
    ) -> Result<Vec<nucleus_protocol::SkillInstallationRecord>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_skill_installations_with_connection(&connection)
    }

    pub fn upsert_skill_installation(
        &self,
        installation: &nucleus_protocol::SkillInstallationRecord,
    ) -> Result<nucleus_protocol::SkillInstallationRecord> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO skill_installations (
                id, package_id, scope_kind, scope_id, enabled, pinned_version, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                package_id = excluded.package_id,
                scope_kind = excluded.scope_kind,
                scope_id = excluded.scope_id,
                enabled = excluded.enabled,
                pinned_version = excluded.pinned_version,
                updated_at = excluded.updated_at
            ",
            params![
                installation.id,
                installation.package_id,
                installation.scope_kind,
                installation.scope_id,
                installation.enabled,
                installation.pinned_version,
                installation.created_at,
                installation.updated_at,
            ],
        )?;
        load_skill_installation(&connection, &installation.id)
    }

    pub fn sync_projects(&self) -> Result<WorkspaceSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        sync_projects_with_connection(&connection)?;
        load_workspace_summary(&connection)
    }

    pub fn update_project(&self, project_id: &str, patch: ProjectPatch) -> Result<ProjectSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_project_summary(&connection, project_id)?;

        connection.execute(
            "
            UPDATE projects
            SET
                title = ?2,
                active = ?3,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![project_id, patch.title.unwrap_or(current.title), 1,],
        )?;

        load_project_summary(&connection, project_id)
    }

    pub fn resolve_project(&self, project_id: &str) -> Result<ResolvedProject> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_resolved_project(&connection, project_id)
    }

    pub fn resolve_projects(&self, project_ids: &[String]) -> Result<Vec<ResolvedProject>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        resolve_projects_with_connection(&connection, project_ids)
    }

    pub fn scratch_dir_for_session(&self, session_id: &str) -> Result<String> {
        let path = self.plan.scratch_dir.join(session_id);
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create session scratch path {}", path.display()))?;
        Ok(display(&path))
    }

    pub fn list_router_profiles(&self) -> Result<Vec<RouterProfileSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_router_profiles_with_connection(&connection)
    }

    pub fn get_router_profile(&self, route_id: &str) -> Result<RouterProfileSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_router_profile(&connection, route_id)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_sessions_with_connection(&connection)
    }

    pub fn get_session(&self, session_id: &str) -> Result<SessionDetail> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_session_detail(&connection, session_id)
    }

    pub fn create_session(&self, record: SessionRecord) -> Result<SessionSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO sessions (
                id,
                title,
                profile_id,
                profile_title,
                route_id,
                route_title,
                scope,
                project_id,
                project_title,
                project_path,
                provider,
                model,
                provider_base_url,
                provider_api_key,
                working_dir,
                working_dir_kind,
                workspace_mode, source_project_path, git_root, worktree_path, git_branch, git_base_ref, git_head, git_dirty, git_untracked_count, git_remote_tracking_branch, workspace_warnings_json,
                approval_mode,
                execution_mode,
                run_budget_mode,
                state,
                provider_session_id,
                last_error,
                last_message_excerpt,
                turn_count
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, 'active', '', '', '', 0)
            ",
            params![
                record.id,
                record.title,
                record.profile_id,
                record.profile_title,
                record.route_id,
                record.route_title,
                record.scope,
                record.project_id,
                record.project_title,
                record.project_path,
                record.provider,
                record.model,
                record.provider_base_url,
                record.provider_api_key,
                record.working_dir,
                record.working_dir_kind,
                record.workspace_mode,
                record.source_project_path,
                record.git_root,
                record.worktree_path,
                record.git_branch,
                record.git_base_ref,
                record.git_head,
                record.git_dirty,
                record.git_untracked_count,
                record.git_remote_tracking_branch,
                serde_json::to_string(&record.workspace_warnings).unwrap_or_else(|_| "[]".to_string()),
                record.approval_mode,
                record.execution_mode,
                record.run_budget_mode,
            ],
        )?;

        replace_session_projects(
            &connection,
            &record.id,
            &record.project_ids,
            &record.project_id,
        )?;

        load_session_summary(&connection, &record.id)
    }

    pub fn update_session(&self, session_id: &str, patch: SessionPatch) -> Result<SessionSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_session_summary(&connection, session_id)?;
        let next_title = patch.title.unwrap_or(current.title);
        let next_profile_id = patch.profile_id.unwrap_or(current.profile_id);
        let next_profile_title = patch.profile_title.unwrap_or(current.profile_title);
        let next_route_id = patch.route_id.unwrap_or(current.route_id);
        let next_route_title = patch.route_title.unwrap_or(current.route_title);
        let next_scope = patch.scope.unwrap_or(current.scope);
        let next_project_id = patch.project_id.unwrap_or(current.project_id);
        let next_project_title = patch.project_title.unwrap_or(current.project_title);
        let next_project_path = patch.project_path.unwrap_or(current.project_path);
        let next_provider = patch.provider.unwrap_or(current.provider);
        let next_model = patch.model.unwrap_or(current.model);
        let next_provider_base_url = patch.provider_base_url.unwrap_or(current.provider_base_url);
        let next_provider_api_key = patch.provider_api_key.unwrap_or(current.provider_api_key);
        let next_working_dir = patch.working_dir.unwrap_or(current.working_dir);
        let next_working_dir_kind = patch.working_dir_kind.unwrap_or(current.working_dir_kind);
        let next_workspace_mode = patch.workspace_mode.unwrap_or(current.workspace_mode);
        let next_source_project_path = patch
            .source_project_path
            .unwrap_or(current.source_project_path);
        let next_git_root = patch.git_root.unwrap_or(current.git_root);
        let next_worktree_path = patch.worktree_path.unwrap_or(current.worktree_path);
        let next_git_branch = patch.git_branch.unwrap_or(current.git_branch);
        let next_git_base_ref = patch.git_base_ref.unwrap_or(current.git_base_ref);
        let next_git_head = patch.git_head.unwrap_or(current.git_head);
        let next_git_dirty = patch.git_dirty.unwrap_or(current.git_dirty);
        let next_git_untracked_count = patch
            .git_untracked_count
            .unwrap_or(current.git_untracked_count);
        let next_git_remote_tracking_branch = patch
            .git_remote_tracking_branch
            .unwrap_or(current.git_remote_tracking_branch);
        let next_workspace_warnings = patch
            .workspace_warnings
            .unwrap_or(current.workspace_warnings);
        let next_approval_mode = patch.approval_mode.unwrap_or(current.approval_mode);
        let next_execution_mode = patch.execution_mode.unwrap_or(current.execution_mode);
        let next_run_budget_mode = patch.run_budget_mode.unwrap_or(current.run_budget_mode);
        let next_state = patch.state.unwrap_or(current.state);
        let next_provider_session_id = patch
            .provider_session_id
            .unwrap_or(current.provider_session_id);
        let next_last_error = patch.last_error.unwrap_or(current.last_error);

        connection.execute(
            "
            UPDATE sessions
            SET
                title = ?2,
                profile_id = ?3,
                profile_title = ?4,
                route_id = ?5,
                route_title = ?6,
                scope = ?7,
                project_id = ?8,
                project_title = ?9,
                project_path = ?10,
                provider = ?11,
                model = ?12,
                provider_base_url = ?13,
                provider_api_key = ?14,
                working_dir = ?15,
                working_dir_kind = ?16,
                workspace_mode = ?17,
                source_project_path = ?18,
                git_root = ?19,
                worktree_path = ?20,
                git_branch = ?21,
                git_base_ref = ?22,
                git_head = ?23,
                git_dirty = ?24,
                git_untracked_count = ?25,
                git_remote_tracking_branch = ?26,
                workspace_warnings_json = ?27,
                approval_mode = ?28,
                execution_mode = ?29,
                run_budget_mode = ?30,
                state = ?31,
                provider_session_id = ?32,
                last_error = ?33,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![
                session_id,
                next_title,
                next_profile_id,
                next_profile_title,
                next_route_id,
                next_route_title,
                next_scope,
                next_project_id.clone(),
                next_project_title,
                next_project_path,
                next_provider,
                next_model,
                next_provider_base_url,
                next_provider_api_key,
                next_working_dir,
                next_working_dir_kind,
                next_workspace_mode,
                next_source_project_path,
                next_git_root,
                next_worktree_path,
                next_git_branch,
                next_git_base_ref,
                next_git_head,
                next_git_dirty,
                next_git_untracked_count,
                next_git_remote_tracking_branch,
                serde_json::to_string(&next_workspace_warnings)
                    .unwrap_or_else(|_| "[]".to_string()),
                next_approval_mode,
                next_execution_mode,
                next_run_budget_mode,
                next_state,
                next_provider_session_id,
                next_last_error,
            ],
        )?;

        if let Some(project_ids) = patch.project_ids {
            replace_session_projects(&connection, session_id, &project_ids, &next_project_id)?;
        }

        load_session_summary(&connection, session_id)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let deleted =
            connection.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;

        if deleted == 0 {
            bail!("session '{session_id}' was not found");
        }

        Ok(())
    }

    pub fn list_playbooks(&self) -> Result<Vec<PlaybookSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_playbooks_with_connection(&connection, &self.plan.playbooks_dir)
    }

    pub fn get_playbook(&self, playbook_id: &str) -> Result<PlaybookDetail> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_playbook_detail(&connection, &self.plan.playbooks_dir, playbook_id)
    }

    pub fn create_playbook(&self, record: PlaybookRecord) -> Result<PlaybookDetail> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_session_exists(&connection, &record.session_id)?;
        let stored = StoredPlaybook {
            id: record.id,
            session_id: record.session_id,
            title: record.title,
            description: record.description,
            prompt: record.prompt,
            enabled: record.enabled,
            policy_bundle: record.policy_bundle,
            trigger_kind: record.trigger_kind,
            schedule_interval_secs: record.schedule_interval_secs,
            event_kind: record.event_kind,
            created_at: record.created_at,
            updated_at: record.updated_at,
        };
        write_playbook_file(&self.plan.playbooks_dir, &stored)?;
        playbook_detail_from_stored(&connection, &stored)
    }

    pub fn update_playbook(
        &self,
        playbook_id: &str,
        patch: PlaybookPatch,
    ) -> Result<PlaybookDetail> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let mut stored = read_playbook_file(&self.plan.playbooks_dir, playbook_id)?;
        if let Some(session_id) = patch.session_id {
            ensure_session_exists(&connection, &session_id)?;
            stored.session_id = session_id;
        }
        if let Some(title) = patch.title {
            stored.title = title;
        }
        if let Some(description) = patch.description {
            stored.description = description;
        }
        if let Some(prompt) = patch.prompt {
            stored.prompt = prompt;
        }
        if let Some(enabled) = patch.enabled {
            stored.enabled = enabled;
        }
        if let Some(policy_bundle) = patch.policy_bundle {
            stored.policy_bundle = policy_bundle;
        }
        if let Some(trigger_kind) = patch.trigger_kind {
            stored.trigger_kind = trigger_kind;
        }
        if let Some(schedule_interval_secs) = patch.schedule_interval_secs {
            stored.schedule_interval_secs = schedule_interval_secs;
        }
        if let Some(event_kind) = patch.event_kind {
            stored.event_kind = event_kind;
        }
        if let Some(updated_at) = patch.updated_at {
            stored.updated_at = updated_at;
        }
        write_playbook_file(&self.plan.playbooks_dir, &stored)?;
        playbook_detail_from_stored(&connection, &stored)
    }

    pub fn delete_playbook(&self, playbook_id: &str) -> Result<PlaybookDetail> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let detail = load_playbook_detail(&connection, &self.plan.playbooks_dir, playbook_id)?;
        let deleted = connection.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![detail.session.id],
        )?;
        if deleted == 0 {
            bail!("session '{}' was not found", detail.session.id);
        }
        fs::remove_file(playbook_file_path(&self.plan.playbooks_dir, playbook_id))
            .with_context(|| format!("failed to delete playbook '{}'", playbook_id))?;
        Ok(detail)
    }

    pub fn append_session_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        role: &str,
        content: &str,
        images: &[SessionTurnImage],
    ) -> Result<SessionTurn> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_session_exists(&connection, session_id)?;
        let images_json =
            serde_json::to_string(images).context("failed to serialize session turn images")?;

        connection.execute(
            "
            INSERT INTO session_turns (id, session_id, role, content, images_json)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![turn_id, session_id, role, content, images_json],
        )?;

        connection.execute(
            "
            UPDATE sessions
            SET
                turn_count = turn_count + 1,
                last_message_excerpt = ?2,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![session_id, session_turn_excerpt(content, images.len())],
        )?;

        load_session_turn(&connection, turn_id)
    }

    pub fn update_session_turn_content(
        &self,
        session_id: &str,
        turn_id: &str,
        content: &str,
    ) -> Result<SessionTurn> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_session_exists(&connection, session_id)?;

        let updated = connection.execute(
            "
            UPDATE session_turns
            SET content = ?3
            WHERE id = ?1 AND session_id = ?2
            ",
            params![turn_id, session_id, content],
        )?;

        if updated == 0 {
            bail!("session turn '{turn_id}' was not found");
        }

        connection.execute(
            "
            UPDATE sessions
            SET
                last_message_excerpt = ?2,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![session_id, session_turn_excerpt(content, 0)],
        )?;

        load_session_turn(&connection, turn_id)
    }

    pub fn list_audit_events(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_audit_events_with_connection(&connection, limit)
    }

    pub fn append_audit_event(&self, record: AuditEventRecord) -> Result<AuditEvent> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection.execute(
            "
            INSERT INTO audit_events (kind, target, status, summary, detail)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                record.kind,
                record.target,
                record.status,
                record.summary,
                record.detail
            ],
        )?;

        let event_id = connection.last_insert_rowid();
        load_audit_event(&connection, event_id)
    }

    pub fn append_instance_log(&self, record: InstanceLogRecord) -> Result<InstanceLogEntry> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let related_ids_json =
            serde_json::to_string(&record.related_ids).context("failed to encode related IDs")?;
        let metadata_json =
            serde_json::to_string(&record.metadata).context("failed to encode log metadata")?;

        connection.execute(
            "
            INSERT INTO instance_logs (
                timestamp, level, category, source, event, message, related_ids_json, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                record.timestamp,
                record.level,
                record.category,
                record.source,
                record.event,
                record.message,
                related_ids_json,
                metadata_json
            ],
        )?;

        let log_id = connection.last_insert_rowid();
        let entry = load_instance_log(&connection, log_id)?;
        prune_instance_logs_with_connection(
            &connection,
            INSTANCE_LOG_RETENTION_DAYS * 24 * 60 * 60,
            INSTANCE_LOG_MAX_ROWS,
        )?;
        drop(connection);

        append_instance_log_jsonl(&self.plan.logs_dir, &entry)?;
        Ok(entry)
    }

    pub fn list_instance_logs(
        &self,
        category: Option<&str>,
        level: Option<&str>,
        before: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<Vec<InstanceLogEntry>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_instance_logs_with_connection(&connection, category, level, before, limit)
    }

    pub fn list_instance_log_categories(&self) -> Result<Vec<InstanceLogCategorySummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_instance_log_categories_with_connection(&connection)
    }

    pub fn prune_instance_logs(&self, max_age_secs: i64, max_rows: usize) -> Result<usize> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        prune_instance_logs_with_connection(&connection, max_age_secs, max_rows)
    }

    pub fn list_jobs_for_session(&self, session_id: &str) -> Result<Vec<JobSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_jobs_for_session_with_connection(&connection, session_id)
    }

    pub fn list_jobs_for_template(
        &self,
        template_id: &str,
        limit: usize,
    ) -> Result<Vec<JobSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_jobs_for_template_with_connection(&connection, template_id, limit)
    }

    pub fn list_jobs_for_template_by_state(
        &self,
        template_id: &str,
        states: &[&str],
    ) -> Result<Vec<JobSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_jobs_for_template_by_state_with_connection(&connection, template_id, states)
    }

    pub fn list_jobs_by_state(&self, states: &[&str]) -> Result<Vec<JobSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_jobs_by_state_with_connection(&connection, states)
    }

    pub fn list_pending_approvals(&self) -> Result<Vec<ApprovalRequestSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_pending_approvals_with_connection(&connection)
    }

    pub fn get_approval_request(&self, approval_id: &str) -> Result<ApprovalRequestSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_approval_request_summary(&connection, approval_id)
    }

    pub fn get_job(&self, job_id: &str) -> Result<JobDetail> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_job_detail(&connection, job_id)
    }

    pub fn list_command_sessions_by_state(
        &self,
        states: &[&str],
    ) -> Result<Vec<CommandSessionSummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        list_command_sessions_by_state_with_connection(&connection, states)
    }

    pub fn get_command_session(&self, command_session_id: &str) -> Result<CommandSessionSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_command_session_summary(&connection, command_session_id)
    }

    pub fn get_job_artifact(&self, artifact_id: &str) -> Result<ArtifactSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        load_artifact_summary(&connection, artifact_id)
    }

    pub fn create_job(&self, record: JobRecord) -> Result<JobSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        if let Some(session_id) = record.session_id.as_deref() {
            ensure_session_exists(&connection, session_id)?;
        }
        if let Some(parent_job_id) = record.parent_job_id.as_deref() {
            ensure_job_exists(&connection, parent_job_id)?;
        }

        connection.execute(
            "
            INSERT INTO jobs (
                id,
                session_id,
                parent_job_id,
                template_id,
                title,
                purpose,
                trigger_kind,
                state,
                requested_by,
                prompt_excerpt
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                record.id,
                record.session_id,
                record.parent_job_id,
                record.template_id,
                record.title,
                record.purpose,
                record.trigger_kind,
                record.state,
                record.requested_by,
                record.prompt_excerpt,
            ],
        )?;

        load_job_summary(&connection, &record.id)
    }

    pub fn update_job(&self, job_id: &str, patch: JobPatch) -> Result<JobSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_job_summary(&connection, job_id)?;
        connection.execute(
            "
            UPDATE jobs
            SET
                state = ?2,
                root_worker_id = ?3,
                visible_turn_id = ?4,
                result_summary = ?5,
                last_error = ?6,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![
                job_id,
                patch.state.unwrap_or(current.state),
                patch.root_worker_id.or(current.root_worker_id),
                patch.visible_turn_id.or(current.visible_turn_id),
                patch.result_summary.unwrap_or(current.result_summary),
                patch.last_error.unwrap_or(current.last_error),
            ],
        )?;

        load_job_summary(&connection, job_id)
    }

    pub fn create_worker(&self, record: WorkerRecord) -> Result<WorkerSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_job_exists(&connection, &record.job_id)?;
        if let Some(parent_worker_id) = record.parent_worker_id.as_deref() {
            ensure_worker_exists(&connection, parent_worker_id)?;
        }
        let read_roots_json =
            serde_json::to_string(&record.read_roots).context("failed to serialize read roots")?;
        let write_roots_json = serde_json::to_string(&record.write_roots)
            .context("failed to serialize write roots")?;

        connection.execute(
            "
            INSERT INTO job_workers (
                id,
                job_id,
                parent_worker_id,
                title,
                lane,
                state,
                provider,
                model,
                provider_base_url,
                provider_api_key,
                provider_session_id,
                working_dir,
                read_roots_json,
                write_roots_json,
                max_steps,
                max_tool_calls,
                max_wall_clock_secs
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            ",
            params![
                record.id,
                record.job_id,
                record.parent_worker_id,
                record.title,
                record.lane,
                record.state,
                record.provider,
                record.model,
                record.provider_base_url,
                record.provider_api_key,
                record.provider_session_id,
                record.working_dir,
                read_roots_json,
                write_roots_json,
                record.max_steps as i64,
                record.max_tool_calls as i64,
                record.max_wall_clock_secs as i64,
            ],
        )?;

        load_worker_summary(&connection, &record.id)
    }

    pub fn update_worker(&self, worker_id: &str, patch: WorkerPatch) -> Result<WorkerSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_worker_summary(&connection, worker_id)?;
        connection.execute(
            "
            UPDATE job_workers
            SET
                state = ?2,
                provider_session_id = ?3,
                step_count = ?4,
                tool_call_count = ?5,
                last_error = ?6,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![
                worker_id,
                patch.state.unwrap_or(current.state),
                patch
                    .provider_session_id
                    .unwrap_or(current.provider_session_id),
                patch.step_count.unwrap_or(current.step_count) as i64,
                patch.tool_call_count.unwrap_or(current.tool_call_count) as i64,
                patch.last_error.unwrap_or(current.last_error),
            ],
        )?;

        load_worker_summary(&connection, worker_id)
    }

    pub fn replace_tool_capability_grants(
        &self,
        worker_id: &str,
        grants: &[ToolCapabilityGrantRecord],
    ) -> Result<Vec<ToolCapabilitySummary>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_worker_exists(&connection, worker_id)?;
        connection.execute(
            "DELETE FROM tool_capability_grants WHERE worker_id = ?1",
            params![worker_id],
        )?;

        for grant in grants {
            connection.execute(
                "
                INSERT INTO tool_capability_grants (
                    worker_id,
                    tool_id,
                    summary,
                    approval_mode,
                    risk_level,
                    side_effect_level,
                    timeout_secs,
                    max_output_bytes,
                    supports_streaming,
                    concurrency_group,
                    scope_kind
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ",
                params![
                    worker_id,
                    grant.tool_id,
                    grant.summary,
                    grant.approval_mode,
                    grant.risk_level,
                    grant.side_effect_level,
                    grant.timeout_secs as i64,
                    grant.max_output_bytes as i64,
                    if grant.supports_streaming { 1 } else { 0 },
                    grant.concurrency_group,
                    grant.scope_kind,
                ],
            )?;
        }

        load_worker_capabilities(&connection, worker_id)
    }

    pub fn create_tool_call(&self, record: ToolCallRecord) -> Result<ToolCallSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_job_exists(&connection, &record.job_id)?;
        ensure_worker_exists(&connection, &record.worker_id)?;
        let args_json = serde_json::to_string(&record.args_json)
            .context("failed to serialize tool call args")?;
        let result_json = record
            .result_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed to serialize tool call result")?;
        let policy_decision_json = record
            .policy_decision
            .as_ref()
            .map(policy_decision_to_json)
            .transpose()?;
        let artifact_ids_json = serde_json::to_string(&record.artifact_ids)
            .context("failed to serialize tool call artifacts")?;

        connection.execute(
            "
            INSERT INTO tool_calls (
                id,
                job_id,
                worker_id,
                tool_id,
                status,
                summary,
                args_json,
                result_json,
                policy_decision_json,
                artifact_ids_json,
                error_class,
                error_detail,
                started_at,
                completed_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ",
            params![
                record.id,
                record.job_id,
                record.worker_id,
                record.tool_id,
                record.status,
                record.summary,
                args_json,
                result_json,
                policy_decision_json,
                artifact_ids_json,
                record.error_class,
                record.error_detail,
                record.started_at,
                record.completed_at,
            ],
        )?;

        load_tool_call_summary(&connection, &record.id)
    }

    pub fn update_tool_call(
        &self,
        tool_call_id: &str,
        patch: ToolCallPatch,
    ) -> Result<ToolCallSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_tool_call_summary(&connection, tool_call_id)?;
        let next_result_json = match patch.result_json {
            Some(Some(value)) => Some(
                serde_json::to_string(&value).context("failed to serialize updated tool result")?,
            ),
            Some(None) => None,
            None => current
                .result_json
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .context("failed to serialize existing tool result")?,
        };
        let next_policy = match patch.policy_decision {
            Some(Some(value)) => Some(policy_decision_to_json(&value)?),
            Some(None) => None,
            None => current
                .policy_decision
                .as_ref()
                .map(policy_decision_summary_to_json)
                .transpose()?,
        };
        let next_artifact_ids_json =
            serde_json::to_string(&patch.artifact_ids.unwrap_or(current.artifact_ids))
                .context("failed to serialize updated artifact ids")?;

        connection.execute(
            "
            UPDATE tool_calls
            SET
                status = ?2,
                summary = ?3,
                result_json = ?4,
                policy_decision_json = ?5,
                artifact_ids_json = ?6,
                error_class = ?7,
                error_detail = ?8,
                started_at = ?9,
                completed_at = ?10
            WHERE id = ?1
            ",
            params![
                tool_call_id,
                patch.status.unwrap_or(current.status),
                patch.summary.unwrap_or(current.summary),
                next_result_json,
                next_policy,
                next_artifact_ids_json,
                patch.error_class.unwrap_or(current.error_class),
                patch.error_detail.unwrap_or(current.error_detail),
                patch.started_at.unwrap_or(current.started_at),
                patch.completed_at.unwrap_or(current.completed_at),
            ],
        )?;

        load_tool_call_summary(&connection, tool_call_id)
    }

    pub fn create_approval_request(
        &self,
        record: ApprovalRequestRecord,
    ) -> Result<ApprovalRequestSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_job_exists(&connection, &record.job_id)?;
        ensure_worker_exists(&connection, &record.worker_id)?;
        ensure_tool_call_exists(&connection, &record.tool_call_id)?;
        let policy_json = policy_decision_to_json(&record.policy_decision)?;
        connection.execute(
            "
            INSERT INTO approval_requests (
                id,
                job_id,
                worker_id,
                tool_call_id,
                state,
                risk_level,
                summary,
                detail,
                diff_preview,
                policy_decision_json,
                resolution_note,
                resolved_by,
                resolved_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ",
            params![
                record.id,
                record.job_id,
                record.worker_id,
                record.tool_call_id,
                record.state,
                record.risk_level,
                record.summary,
                record.detail,
                record.diff_preview,
                policy_json,
                record.resolution_note,
                record.resolved_by,
                record.resolved_at,
            ],
        )?;

        load_approval_request_summary(&connection, &record.id)
    }

    pub fn update_approval_request(
        &self,
        approval_id: &str,
        state: &str,
        resolution_note: Option<&str>,
        resolved_by: Option<&str>,
        resolved_at: Option<i64>,
    ) -> Result<ApprovalRequestSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_approval_request_summary(&connection, approval_id)?;
        connection.execute(
            "
            UPDATE approval_requests
            SET
                state = ?2,
                resolution_note = ?3,
                resolved_by = ?4,
                resolved_at = ?5
            WHERE id = ?1
            ",
            params![
                approval_id,
                state,
                resolution_note.unwrap_or(&current.resolution_note),
                resolved_by.unwrap_or(&current.resolved_by),
                resolved_at.or(current.resolved_at),
            ],
        )?;

        load_approval_request_summary(&connection, approval_id)
    }

    pub fn create_command_session(
        &self,
        record: CommandSessionRecord,
    ) -> Result<CommandSessionSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_job_exists(&connection, &record.job_id)?;
        ensure_worker_exists(&connection, &record.worker_id)?;
        if let Some(tool_call_id) = record.tool_call_id.as_deref() {
            ensure_tool_call_exists(&connection, tool_call_id)?;
        }
        let args_json =
            serde_json::to_string(&record.args).context("failed to serialize command args")?;
        let env_json = serde_json::to_string(&record.env_json)
            .context("failed to serialize command environment")?;
        connection.execute(
            "
            INSERT INTO command_sessions (
                id,
                job_id,
                worker_id,
                tool_call_id,
                mode,
                title,
                state,
                command,
                args_json,
                cwd,
                session_id,
                project_id,
                worktree_path,
                branch,
                port,
                env_json,
                network_policy,
                timeout_secs,
                output_limit_bytes,
                last_error,
                exit_code,
                stdout_artifact_id,
                stderr_artifact_id,
                started_at,
                completed_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
            ",
            params![
                record.id,
                record.job_id,
                record.worker_id,
                record.tool_call_id,
                record.mode,
                record.title,
                record.state,
                record.command,
                args_json,
                record.cwd,
                record.session_id,
                record.project_id,
                record.worktree_path,
                record.branch,
                record.port.map(|port| port as i64),
                env_json,
                record.network_policy,
                record.timeout_secs as i64,
                record.output_limit_bytes as i64,
                record.last_error,
                record.exit_code,
                record.stdout_artifact_id,
                record.stderr_artifact_id,
                record.started_at,
                record.completed_at,
            ],
        )?;

        load_command_session_summary(&connection, &record.id)
    }

    pub fn update_command_session(
        &self,
        command_session_id: &str,
        patch: CommandSessionPatch,
    ) -> Result<CommandSessionSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_command_session_summary(&connection, command_session_id)?;
        connection.execute(
            "
            UPDATE command_sessions
            SET
                mode = ?2,
                title = ?3,
                state = ?4,
                last_error = ?5,
                exit_code = ?6,
                stdout_artifact_id = ?7,
                stderr_artifact_id = ?8,
                started_at = ?9,
                completed_at = ?10,
                updated_at = unixepoch()
            WHERE id = ?1
            ",
            params![
                command_session_id,
                patch.mode.unwrap_or(current.mode),
                patch.title.unwrap_or(current.title),
                patch.state.unwrap_or(current.state),
                patch.last_error.unwrap_or(current.last_error),
                patch.exit_code.unwrap_or(current.exit_code),
                patch
                    .stdout_artifact_id
                    .unwrap_or(current.stdout_artifact_id),
                patch
                    .stderr_artifact_id
                    .unwrap_or(current.stderr_artifact_id),
                patch.started_at.unwrap_or(current.started_at),
                patch.completed_at.unwrap_or(current.completed_at),
            ],
        )?;

        load_command_session_summary(&connection, command_session_id)
    }

    pub fn create_job_artifact(&self, record: JobArtifactRecord) -> Result<ArtifactSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_job_exists(&connection, &record.job_id)?;
        if let Some(worker_id) = record.worker_id.as_deref() {
            ensure_worker_exists(&connection, worker_id)?;
        }
        if let Some(tool_call_id) = record.tool_call_id.as_deref() {
            ensure_tool_call_exists(&connection, tool_call_id)?;
        }
        if let Some(command_session_id) = record.command_session_id.as_deref() {
            ensure_command_session_exists(&connection, command_session_id)?;
        }
        connection.execute(
            "
            INSERT INTO job_artifacts (
                id,
                job_id,
                worker_id,
                tool_call_id,
                command_session_id,
                kind,
                title,
                path,
                mime_type,
                size_bytes,
                preview_text
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ",
            params![
                record.id,
                record.job_id,
                record.worker_id,
                record.tool_call_id,
                record.command_session_id,
                record.kind,
                record.title,
                record.path,
                record.mime_type,
                record.size_bytes as i64,
                record.preview_text,
            ],
        )?;

        load_artifact_summary(&connection, &record.id)
    }

    pub fn update_job_artifact(
        &self,
        artifact_id: &str,
        patch: JobArtifactPatch,
    ) -> Result<ArtifactSummary> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        let current = load_artifact_summary(&connection, artifact_id)?;
        connection.execute(
            "
            UPDATE job_artifacts
            SET
                kind = ?2,
                title = ?3,
                path = ?4,
                mime_type = ?5,
                size_bytes = ?6,
                preview_text = ?7
            WHERE id = ?1
            ",
            params![
                artifact_id,
                patch.kind.unwrap_or(current.kind),
                patch.title.unwrap_or(current.title),
                patch.path.unwrap_or(current.path),
                patch.mime_type.unwrap_or(current.mime_type),
                patch.size_bytes.unwrap_or(current.size_bytes) as i64,
                patch.preview_text.unwrap_or(current.preview_text),
            ],
        )?;

        load_artifact_summary(&connection, artifact_id)
    }

    pub fn append_job_event(&self, record: JobEventRecord) -> Result<JobEvent> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_job_exists(&connection, &record.job_id)?;
        if let Some(worker_id) = record.worker_id.as_deref() {
            ensure_worker_exists(&connection, worker_id)?;
        }
        let data_json = serde_json::to_string(&record.data_json)
            .context("failed to serialize job event data")?;
        connection.execute(
            "
            INSERT INTO job_events (job_id, worker_id, event_type, status, summary, detail, data_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                record.job_id,
                record.worker_id,
                record.event_type,
                record.status,
                record.summary,
                record.detail,
                data_json,
            ],
        )?;

        let event_id = connection.last_insert_rowid();
        load_job_event(&connection, event_id)
    }

    pub fn write_worker_checkpoint(
        &self,
        worker_id: &str,
        checkpoint: &serde_json::Value,
    ) -> Result<()> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        ensure_worker_exists(&connection, worker_id)?;
        let checkpoint_json =
            serde_json::to_string(checkpoint).context("failed to serialize worker checkpoint")?;
        connection.execute(
            "
            INSERT INTO worker_checkpoints (worker_id, checkpoint_json, updated_at)
            VALUES (?1, ?2, unixepoch())
            ON CONFLICT(worker_id) DO UPDATE SET
                checkpoint_json = excluded.checkpoint_json,
                updated_at = unixepoch()
            ",
            params![worker_id, checkpoint_json],
        )?;
        Ok(())
    }

    pub fn read_worker_checkpoint(&self, worker_id: &str) -> Result<Option<serde_json::Value>> {
        let connection = self.connection.lock().expect("storage mutex poisoned");
        connection
            .query_row(
                "SELECT checkpoint_json FROM worker_checkpoints WHERE worker_id = ?1",
                params![worker_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| {
                serde_json::from_str(&payload).context("failed to decode worker checkpoint")
            })
            .transpose()
    }
}

fn default_state_dir() -> Result<PathBuf> {
    let home_dir = dirs::home_dir().context("failed to determine home directory")?;
    Ok(home_dir.join(".nucleus"))
}

fn default_workspace_root() -> String {
    dirs::home_dir()
        .map(|path| path.join("dev-projects").display().to_string())
        .unwrap_or_else(|| "/home/eba/dev-projects".to_string())
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

fn session_turn_excerpt(content: &str, image_count: usize) -> String {
    if !content.trim().is_empty() {
        return excerpt(content, 160);
    }

    if image_count == 0 {
        return String::new();
    }

    if image_count == 1 {
        "[1 image]".to_string()
    } else {
        format!("[{image_count} images]")
    }
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "busy_timeout", 5000)?;
    Ok(())
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS runtimes (
            id TEXT PRIMARY KEY,
            summary TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            profile_id TEXT NOT NULL DEFAULT '',
            profile_title TEXT NOT NULL DEFAULT '',
            route_id TEXT NOT NULL DEFAULT '',
            route_title TEXT NOT NULL DEFAULT '',
            scope TEXT NOT NULL DEFAULT 'ad_hoc',
            project_id TEXT NOT NULL DEFAULT '',
            project_title TEXT NOT NULL DEFAULT '',
            project_path TEXT NOT NULL DEFAULT '',
            provider TEXT NOT NULL,
            model TEXT NOT NULL DEFAULT '',
            provider_base_url TEXT NOT NULL DEFAULT '',
            provider_api_key TEXT NOT NULL DEFAULT '',
            working_dir TEXT NOT NULL DEFAULT '',
            working_dir_kind TEXT NOT NULL DEFAULT 'workspace_scratch',
            approval_mode TEXT NOT NULL DEFAULT 'ask',
            execution_mode TEXT NOT NULL DEFAULT 'act',
            run_budget_mode TEXT NOT NULL DEFAULT 'inherit',
            state TEXT NOT NULL,
            provider_session_id TEXT NOT NULL DEFAULT '',
            last_error TEXT NOT NULL DEFAULT '',
            last_message_excerpt TEXT NOT NULL DEFAULT '',
            turn_count INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS session_turns (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            images_json TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_session_turns_session_id_created_at
            ON session_turns(session_id, created_at, id);

        CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY,
            session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE,
            parent_job_id TEXT REFERENCES jobs(id) ON DELETE CASCADE,
            template_id TEXT,
            title TEXT NOT NULL,
            purpose TEXT NOT NULL DEFAULT '',
            trigger_kind TEXT NOT NULL DEFAULT 'session_prompt',
            state TEXT NOT NULL,
            requested_by TEXT NOT NULL DEFAULT 'user',
            prompt_excerpt TEXT NOT NULL DEFAULT '',
            root_worker_id TEXT,
            visible_turn_id TEXT,
            result_summary TEXT NOT NULL DEFAULT '',
            last_error TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_jobs_session_id_created_at
            ON jobs(session_id, created_at DESC, id DESC);

        CREATE INDEX IF NOT EXISTS idx_jobs_parent_job_id_created_at
            ON jobs(parent_job_id, created_at ASC, id ASC);

        CREATE TABLE IF NOT EXISTS job_workers (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            parent_worker_id TEXT REFERENCES job_workers(id) ON DELETE SET NULL,
            title TEXT NOT NULL,
            lane TEXT NOT NULL DEFAULT 'utility',
            state TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL DEFAULT '',
            provider_base_url TEXT NOT NULL DEFAULT '',
            provider_api_key TEXT NOT NULL DEFAULT '',
            provider_session_id TEXT NOT NULL DEFAULT '',
            working_dir TEXT NOT NULL,
            read_roots_json TEXT NOT NULL DEFAULT '[]',
            write_roots_json TEXT NOT NULL DEFAULT '[]',
            max_steps INTEGER NOT NULL DEFAULT 12,
            max_tool_calls INTEGER NOT NULL DEFAULT 24,
            max_wall_clock_secs INTEGER NOT NULL DEFAULT 600,
            step_count INTEGER NOT NULL DEFAULT 0,
            tool_call_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_job_workers_job_id_created_at
            ON job_workers(job_id, created_at ASC, id ASC);

        CREATE TABLE IF NOT EXISTS tool_capability_grants (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            worker_id TEXT NOT NULL REFERENCES job_workers(id) ON DELETE CASCADE,
            tool_id TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            approval_mode TEXT NOT NULL,
            risk_level TEXT NOT NULL,
            side_effect_level TEXT NOT NULL,
            timeout_secs INTEGER NOT NULL DEFAULT 30,
            max_output_bytes INTEGER NOT NULL DEFAULT 65536,
            supports_streaming INTEGER NOT NULL DEFAULT 0,
            concurrency_group TEXT NOT NULL DEFAULT '',
            scope_kind TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_tool_capability_grants_worker_tool
            ON tool_capability_grants(worker_id, tool_id);

        CREATE TABLE IF NOT EXISTS skill_manifests (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            activation_mode TEXT NOT NULL DEFAULT 'manual',
            triggers_json TEXT NOT NULL DEFAULT '[]',
            include_paths_json TEXT NOT NULL DEFAULT '[]',
            required_tools_json TEXT NOT NULL DEFAULT '[]',
            required_mcps_json TEXT NOT NULL DEFAULT '[]',
            project_filters_json TEXT NOT NULL DEFAULT '[]',
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS mcp_servers (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL DEFAULT 'workspace',
            title TEXT NOT NULL,
            transport TEXT NOT NULL DEFAULT 'stdio',
            command TEXT NOT NULL DEFAULT '',
            args_json TEXT NOT NULL DEFAULT '[]',
            env_json TEXT NOT NULL DEFAULT '{}',
            url TEXT NOT NULL DEFAULT '',
            headers_json TEXT NOT NULL DEFAULT '{}',
            auth_kind TEXT NOT NULL DEFAULT 'none',
            auth_ref TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            sync_status TEXT NOT NULL DEFAULT 'pending',
            last_error TEXT NOT NULL DEFAULT '',
            last_synced_at INTEGER,
            tools_json TEXT NOT NULL DEFAULT '[]',
            resources_json TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS mcp_tools (
            id TEXT PRIMARY KEY,
            server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            input_schema_json TEXT NOT NULL DEFAULT '{}',
            source TEXT NOT NULL DEFAULT '',
            discovered_at INTEGER NOT NULL DEFAULT (unixepoch()),
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_mcp_tools_server_id_name
            ON mcp_tools(server_id, name);

        CREATE TABLE IF NOT EXISTS memory_entries (
            id TEXT PRIMARY KEY,
            scope_kind TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            tags_json TEXT NOT NULL DEFAULT '[]',
            enabled INTEGER NOT NULL DEFAULT 1,
            status TEXT NOT NULL DEFAULT 'accepted',
            memory_kind TEXT NOT NULL DEFAULT 'note',
            source_kind TEXT NOT NULL DEFAULT 'manual',
            source_id TEXT NOT NULL DEFAULT '',
            confidence REAL NOT NULL DEFAULT 1.0,
            created_by TEXT NOT NULL DEFAULT 'user',
            last_used_at INTEGER,
            use_count INTEGER NOT NULL DEFAULT 0,
            supersedes_id TEXT NOT NULL DEFAULT '',
            metadata_json TEXT NOT NULL DEFAULT '{}',
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_memory_entries_scope
            ON memory_entries(scope_kind, scope_id, enabled);

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_entries_fts USING fts5(
            id UNINDEXED,
            title,
            content,
            tags
        );

        CREATE TABLE IF NOT EXISTS memory_candidates (
            id TEXT PRIMARY KEY,
            scope_kind TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            session_id TEXT NOT NULL DEFAULT '',
            turn_id_start TEXT NOT NULL DEFAULT '',
            turn_id_end TEXT NOT NULL DEFAULT '',
            candidate_kind TEXT NOT NULL DEFAULT 'note',
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            tags_json TEXT NOT NULL DEFAULT '[]',
            evidence_json TEXT NOT NULL DEFAULT '[]',
            reason TEXT NOT NULL DEFAULT '',
            confidence REAL NOT NULL DEFAULT 0.0,
            status TEXT NOT NULL DEFAULT 'pending',
            dedupe_key TEXT NOT NULL DEFAULT '',
            accepted_memory_id TEXT NOT NULL DEFAULT '',
            created_by TEXT NOT NULL DEFAULT 'utility_worker',
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );

        CREATE INDEX IF NOT EXISTS idx_memory_candidates_status
            ON memory_candidates(status, created_at);
        CREATE INDEX IF NOT EXISTS idx_memory_candidates_scope
            ON memory_candidates(scope_kind, scope_id, status);
        CREATE INDEX IF NOT EXISTS idx_memory_candidates_session
            ON memory_candidates(session_id, status);
        CREATE INDEX IF NOT EXISTS idx_memory_candidates_dedupe
            ON memory_candidates(dedupe_key, status);

        CREATE TABLE IF NOT EXISTS vault_state (
            id TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            vault_id TEXT NOT NULL,
            status TEXT NOT NULL,
            kdf_algorithm TEXT NOT NULL,
            kdf_params_json TEXT NOT NULL,
            salt BLOB NOT NULL,
            cipher TEXT NOT NULL,
            encrypted_root_check BLOB NOT NULL,
            root_check_nonce BLOB NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS vault_scope_keys (
            id TEXT PRIMARY KEY,
            vault_id TEXT NOT NULL,
            scope_kind TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            encrypted_key BLOB NOT NULL,
            nonce BLOB NOT NULL,
            aad TEXT NOT NULL,
            key_version INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            rotated_at INTEGER,
            UNIQUE(vault_id, scope_kind, scope_id)
        );

        CREATE TABLE IF NOT EXISTS vault_secrets (
            id TEXT PRIMARY KEY,
            scope_key_id TEXT NOT NULL,
            scope_kind TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            ciphertext BLOB NOT NULL,
            nonce BLOB NOT NULL,
            aad TEXT NOT NULL,
            version INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            last_used_at INTEGER,
            UNIQUE(scope_kind, scope_id, name)
        );

        CREATE TABLE IF NOT EXISTS vault_secret_policies (
            id TEXT PRIMARY KEY,
            secret_id TEXT NOT NULL,
            consumer_kind TEXT NOT NULL,
            consumer_id TEXT NOT NULL,
            permission TEXT NOT NULL,
            approval_mode TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(secret_id, consumer_kind, consumer_id, permission)
        );

        CREATE TABLE IF NOT EXISTS vault_secret_usages (
            id TEXT PRIMARY KEY,
            secret_id TEXT NOT NULL,
            consumer_kind TEXT NOT NULL,
            consumer_id TEXT NOT NULL,
            purpose TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            last_used_at INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_vault_secrets_scope
            ON vault_secrets(scope_kind, scope_id, name);
        CREATE INDEX IF NOT EXISTS idx_vault_scope_keys_scope
            ON vault_scope_keys(scope_kind, scope_id);

        CREATE TABLE IF NOT EXISTS skill_packages (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            version TEXT NOT NULL DEFAULT '',
            manifest_json TEXT NOT NULL DEFAULT '{}',
            instructions TEXT NOT NULL DEFAULT '',
            source_kind TEXT NOT NULL DEFAULT 'manual',
            source_url TEXT NOT NULL DEFAULT '',
            source_repo_url TEXT NOT NULL DEFAULT '',
            source_owner TEXT NOT NULL DEFAULT '',
            source_repo TEXT NOT NULL DEFAULT '',
            source_ref TEXT NOT NULL DEFAULT '',
            source_parent_path TEXT NOT NULL DEFAULT '',
            source_skill_path TEXT NOT NULL DEFAULT '',
            source_commit TEXT NOT NULL DEFAULT '',
            imported_at INTEGER,
            last_checked_at INTEGER,
            latest_source_commit TEXT NOT NULL DEFAULT '',
            update_status TEXT NOT NULL DEFAULT 'unknown',
            content_checksum TEXT NOT NULL DEFAULT '',
            dirty_status TEXT NOT NULL DEFAULT 'unknown',
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );



        CREATE TABLE IF NOT EXISTS skill_installations (
            id TEXT PRIMARY KEY,
            package_id TEXT NOT NULL REFERENCES skill_packages(id) ON DELETE CASCADE,
            scope_kind TEXT NOT NULL DEFAULT '',
            scope_id TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            pinned_version TEXT,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_skill_installations_package_scope
            ON skill_installations(package_id, scope_kind, scope_id);

        CREATE TABLE IF NOT EXISTS tool_calls (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            worker_id TEXT NOT NULL REFERENCES job_workers(id) ON DELETE CASCADE,
            tool_id TEXT NOT NULL,
            status TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            args_json TEXT NOT NULL,
            result_json TEXT,
            policy_decision_json TEXT,
            artifact_ids_json TEXT NOT NULL DEFAULT '[]',
            error_class TEXT NOT NULL DEFAULT '',
            error_detail TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            started_at INTEGER,
            completed_at INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_tool_calls_job_id_created_at
            ON tool_calls(job_id, created_at ASC, id ASC);

        CREATE INDEX IF NOT EXISTS idx_tool_calls_worker_id_created_at
            ON tool_calls(worker_id, created_at ASC, id ASC);

        CREATE TABLE IF NOT EXISTS approval_requests (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            worker_id TEXT NOT NULL REFERENCES job_workers(id) ON DELETE CASCADE,
            tool_call_id TEXT NOT NULL REFERENCES tool_calls(id) ON DELETE CASCADE,
            state TEXT NOT NULL,
            risk_level TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            detail TEXT NOT NULL DEFAULT '',
            diff_preview TEXT NOT NULL DEFAULT '',
            policy_decision_json TEXT NOT NULL,
            resolution_note TEXT NOT NULL DEFAULT '',
            resolved_by TEXT NOT NULL DEFAULT '',
            requested_at INTEGER NOT NULL DEFAULT (unixepoch()),
            resolved_at INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_approval_requests_job_id_requested_at
            ON approval_requests(job_id, requested_at DESC, id DESC);

        CREATE INDEX IF NOT EXISTS idx_approval_requests_state_requested_at
            ON approval_requests(state, requested_at DESC, id DESC);

        CREATE TABLE IF NOT EXISTS command_sessions (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            worker_id TEXT NOT NULL REFERENCES job_workers(id) ON DELETE CASCADE,
            tool_call_id TEXT REFERENCES tool_calls(id) ON DELETE SET NULL,
            mode TEXT NOT NULL DEFAULT 'oneshot',
            title TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL,
            command TEXT NOT NULL,
            args_json TEXT NOT NULL DEFAULT '[]',
            cwd TEXT NOT NULL,
            session_id TEXT NOT NULL DEFAULT '',
            project_id TEXT NOT NULL DEFAULT '',
            worktree_path TEXT NOT NULL DEFAULT '',
            branch TEXT NOT NULL DEFAULT '',
            port INTEGER,
            env_json TEXT NOT NULL DEFAULT '{}',
            network_policy TEXT NOT NULL DEFAULT 'inherit',
            timeout_secs INTEGER NOT NULL DEFAULT 300,
            output_limit_bytes INTEGER NOT NULL DEFAULT 131072,
            last_error TEXT NOT NULL DEFAULT '',
            exit_code INTEGER,
            stdout_artifact_id TEXT REFERENCES job_artifacts(id) ON DELETE SET NULL,
            stderr_artifact_id TEXT REFERENCES job_artifacts(id) ON DELETE SET NULL,
            started_at INTEGER,
            completed_at INTEGER,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_command_sessions_job_id_created_at
            ON command_sessions(job_id, created_at ASC, id ASC);

        CREATE INDEX IF NOT EXISTS idx_command_sessions_state_updated_at
            ON command_sessions(state, updated_at DESC, id DESC);

        CREATE TABLE IF NOT EXISTS job_artifacts (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            worker_id TEXT REFERENCES job_workers(id) ON DELETE SET NULL,
            tool_call_id TEXT REFERENCES tool_calls(id) ON DELETE SET NULL,
            command_session_id TEXT REFERENCES command_sessions(id) ON DELETE SET NULL,
            kind TEXT NOT NULL,
            title TEXT NOT NULL,
            path TEXT NOT NULL,
            mime_type TEXT NOT NULL DEFAULT 'text/plain',
            size_bytes INTEGER NOT NULL DEFAULT 0,
            preview_text TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_job_artifacts_job_id_created_at
            ON job_artifacts(job_id, created_at ASC, id ASC);

        CREATE TABLE IF NOT EXISTS job_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            worker_id TEXT REFERENCES job_workers(id) ON DELETE SET NULL,
            event_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT '',
            summary TEXT NOT NULL DEFAULT '',
            detail TEXT NOT NULL DEFAULT '',
            data_json TEXT NOT NULL DEFAULT '{}',
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_job_events_job_id_created_at
            ON job_events(job_id, created_at ASC, id ASC);

        CREATE TABLE IF NOT EXISTS worker_checkpoints (
            worker_id TEXT PRIMARY KEY REFERENCES job_workers(id) ON DELETE CASCADE,
            checkpoint_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS session_projects (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            sort_order INTEGER NOT NULL DEFAULT 0,
            is_primary INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY (session_id, project_id)
        );

        CREATE INDEX IF NOT EXISTS idx_session_projects_session_id_sort_order
            ON session_projects(session_id, sort_order, project_id);

        CREATE TABLE IF NOT EXISTS audit_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            target TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'info',
            summary TEXT NOT NULL DEFAULT '',
            detail TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS instance_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp INTEGER NOT NULL DEFAULT (unixepoch()),
            level TEXT NOT NULL,
            category TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT '',
            event TEXT NOT NULL,
            message TEXT NOT NULL DEFAULT '',
            related_ids_json TEXT NOT NULL DEFAULT '{}',
            metadata_json TEXT NOT NULL DEFAULT '{}',
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_instance_logs_timestamp_id
            ON instance_logs(timestamp DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_instance_logs_category_timestamp
            ON instance_logs(category, timestamp DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_instance_logs_level_timestamp
            ON instance_logs(level, timestamp DESC, id DESC);

        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            slug TEXT NOT NULL,
            relative_path TEXT NOT NULL UNIQUE,
            absolute_path TEXT NOT NULL UNIQUE,
            active INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS router_profiles (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            summary TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            targets_json TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS workspace_profiles (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            main_model_json TEXT NOT NULL,
            utility_model_json TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        ",
    )?;
    rebuild_memory_search_index_with_connection(connection)?;

    ensure_column(
        connection,
        "skill_manifests",
        "instructions",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    for (column, definition) in [
        ("status", "TEXT NOT NULL DEFAULT 'accepted'"),
        ("memory_kind", "TEXT NOT NULL DEFAULT 'note'"),
        ("source_kind", "TEXT NOT NULL DEFAULT 'manual'"),
        ("source_id", "TEXT NOT NULL DEFAULT ''"),
        ("confidence", "REAL NOT NULL DEFAULT 1.0"),
        ("created_by", "TEXT NOT NULL DEFAULT 'user'"),
        ("last_used_at", "INTEGER"),
        ("use_count", "INTEGER NOT NULL DEFAULT 0"),
        ("supersedes_id", "TEXT NOT NULL DEFAULT ''"),
        ("metadata_json", "TEXT NOT NULL DEFAULT '{}'"),
    ] {
        ensure_column(connection, "memory_entries", column, definition)?;
    }

    connection.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_memory_entries_context
            ON memory_entries(scope_kind, scope_id, enabled, status);
        CREATE INDEX IF NOT EXISTS idx_memory_entries_kind
            ON memory_entries(memory_kind);
        CREATE INDEX IF NOT EXISTS idx_memory_entries_source
            ON memory_entries(source_kind, source_id);
        CREATE INDEX IF NOT EXISTS idx_memory_entries_last_used
            ON memory_entries(last_used_at);
        CREATE VIRTUAL TABLE IF NOT EXISTS memory_entries_fts USING fts5(
            id UNINDEXED,
            title,
            content,
            tags
        );
        ",
    )?;

    ensure_column(
        connection,
        "mcp_servers",
        "workspace_id",
        "TEXT NOT NULL DEFAULT 'workspace'",
    )?;
    ensure_column(
        connection,
        "mcp_servers",
        "transport",
        "TEXT NOT NULL DEFAULT 'stdio'",
    )?;
    ensure_column(
        connection,
        "mcp_servers",
        "command",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "mcp_servers",
        "args_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        connection,
        "mcp_servers",
        "env_json",
        "TEXT NOT NULL DEFAULT '{}'",
    )?;
    ensure_column(connection, "mcp_servers", "url", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        connection,
        "mcp_servers",
        "headers_json",
        "TEXT NOT NULL DEFAULT '{}'",
    )?;
    ensure_column(
        connection,
        "mcp_servers",
        "auth_kind",
        "TEXT NOT NULL DEFAULT 'none'",
    )?;
    ensure_column(
        connection,
        "mcp_servers",
        "auth_ref",
        "TEXT NOT NULL DEFAULT ''",
    )?;

    ensure_column(
        connection,
        "mcp_servers",
        "sync_status",
        "TEXT NOT NULL DEFAULT 'pending'",
    )?;
    ensure_column(
        connection,
        "mcp_servers",
        "last_error",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(connection, "mcp_servers", "last_synced_at", "INTEGER")?;

    migrate_mcp_remote_bridge_records(connection)?;

    ensure_column(
        connection,
        "sessions",
        "profile_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "profile_title",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "route_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "route_title",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "scope",
        "TEXT NOT NULL DEFAULT 'ad_hoc'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "project_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "project_title",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "project_path",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(connection, "sessions", "model", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        connection,
        "sessions",
        "provider_base_url",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "provider_api_key",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "working_dir",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "working_dir_kind",
        "TEXT NOT NULL DEFAULT 'workspace_scratch'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "workspace_mode",
        "TEXT NOT NULL DEFAULT 'shared_project_root'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "source_project_path",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "git_root",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "worktree_path",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "git_branch",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "git_base_ref",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "git_head",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "git_dirty",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        connection,
        "sessions",
        "git_untracked_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        connection,
        "sessions",
        "git_remote_tracking_branch",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "workspace_warnings_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        connection,
        "command_sessions",
        "session_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "command_sessions",
        "project_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "command_sessions",
        "worktree_path",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "command_sessions",
        "branch",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(connection, "command_sessions", "port", "INTEGER")?;
    ensure_column(
        connection,
        "sessions",
        "approval_mode",
        "TEXT NOT NULL DEFAULT 'ask'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "execution_mode",
        "TEXT NOT NULL DEFAULT 'act'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "run_budget_mode",
        "TEXT NOT NULL DEFAULT 'inherit'",
    )?;
    ensure_column(
        connection,
        "sessions",
        "provider_session_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "last_error",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "last_message_excerpt",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "sessions",
        "turn_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        connection,
        "audit_events",
        "target",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "audit_events",
        "status",
        "TEXT NOT NULL DEFAULT 'info'",
    )?;
    ensure_column(
        connection,
        "audit_events",
        "summary",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "session_turns",
        "images_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        connection,
        "job_artifacts",
        "command_session_id",
        "TEXT REFERENCES command_sessions(id) ON DELETE SET NULL",
    )?;

    for (column, definition) in [
        ("source_kind", "TEXT NOT NULL DEFAULT 'manual'"),
        ("source_url", "TEXT NOT NULL DEFAULT ''"),
        ("source_repo_url", "TEXT NOT NULL DEFAULT ''"),
        ("source_owner", "TEXT NOT NULL DEFAULT ''"),
        ("source_repo", "TEXT NOT NULL DEFAULT ''"),
        ("source_ref", "TEXT NOT NULL DEFAULT ''"),
        ("source_parent_path", "TEXT NOT NULL DEFAULT ''"),
        ("source_skill_path", "TEXT NOT NULL DEFAULT ''"),
        ("source_commit", "TEXT NOT NULL DEFAULT ''"),
        ("imported_at", "INTEGER"),
        ("last_checked_at", "INTEGER"),
        ("latest_source_commit", "TEXT NOT NULL DEFAULT ''"),
        ("update_status", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("content_checksum", "TEXT NOT NULL DEFAULT ''"),
        ("dirty_status", "TEXT NOT NULL DEFAULT 'unknown'"),
    ] {
        ensure_column(connection, "skill_packages", column, definition)?;
    }

    Ok(())
}

fn seed_runtimes(connection: &Connection) -> Result<()> {
    for adapter in AdapterKind::RUNTIME_PROBE_ALL {
        connection.execute(
            "
            INSERT INTO runtimes (id, summary, state)
            VALUES (?1, ?2, 'planned')
            ON CONFLICT(id) DO UPDATE SET
                summary = excluded.summary,
                updated_at = unixepoch()
            ",
            params![adapter.as_str(), adapter.summary()],
        )?;
    }

    let runtime_ids = AdapterKind::RUNTIME_PROBE_ALL
        .iter()
        .map(|adapter| format!("'{}'", adapter.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    connection.execute(
        &format!("DELETE FROM runtimes WHERE id NOT IN ({runtime_ids})"),
        [],
    )?;

    Ok(())
}

fn seed_router_profiles(connection: &Connection) -> Result<()> {
    let profiles = [
        (
            "local-openai",
            "Local OpenAI-compatible",
            "Default protocol route through the local OpenAI-compatible proxy.",
            true,
            json!([{
                "provider": "openai_compatible",
                "model": DEFAULT_OPENAI_COMPATIBLE_MODEL,
                "base_url": DEFAULT_OPENAI_COMPATIBLE_BASE_URL,
                "api_key": ""
            }]),
        ),
        (
            "claude-sonnet",
            "Claude Sonnet",
            "Claude Sonnet route through a configured protocol backend.",
            true,
            json!([{ "provider": "claude", "model": "sonnet", "base_url": "", "api_key": "" }]),
        ),
        (
            "codex-default",
            "Codex Default",
            "Codex route through a configured protocol backend.",
            true,
            json!([{ "provider": "codex", "model": "", "base_url": "", "api_key": "" }]),
        ),
        (
            "balanced",
            "Balanced",
            "Prefer the local OpenAI-compatible protocol route, with Claude/Codex reserved for future protocol backends.",
            true,
            json!([
                {
                    "provider": "openai_compatible",
                    "model": DEFAULT_OPENAI_COMPATIBLE_MODEL,
                    "base_url": DEFAULT_OPENAI_COMPATIBLE_BASE_URL,
                    "api_key": ""
                },
                { "provider": "claude", "model": "sonnet", "base_url": "", "api_key": "" },
                { "provider": "codex", "model": "gpt-5.4", "base_url": "", "api_key": "" }
            ]),
        ),
    ];

    for (id, title, summary, enabled, targets) in profiles {
        connection.execute(
            "
            INSERT INTO router_profiles (id, title, summary, enabled, targets_json)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                summary = excluded.summary,
                enabled = excluded.enabled,
                targets_json = excluded.targets_json,
                updated_at = unixepoch()
            ",
            params![id, title, summary, enabled as i64, targets.to_string()],
        )?;
    }

    Ok(())
}

fn seed_workspace_settings(connection: &Connection) -> Result<()> {
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_root', ?1)
        ON CONFLICT(key) DO NOTHING
        ",
        params![default_workspace_root()],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_main_target', 'route:local-openai')
        ON CONFLICT(key) DO NOTHING
        ",
        [],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_utility_target', 'route:local-openai')
        ON CONFLICT(key) DO NOTHING
        ",
        [],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_default_profile_id', 'default')
        ON CONFLICT(key) DO NOTHING
        ",
        [],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_run_budget_max_steps', ?1)
        ON CONFLICT(key) DO NOTHING
        ",
        params![DEFAULT_JOB_MAX_STEPS.to_string()],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_run_budget_max_tool_calls', ?1)
        ON CONFLICT(key) DO NOTHING
        ",
        params![DEFAULT_JOB_MAX_TOOL_CALLS.to_string()],
    )?;
    connection.execute(
        "
        INSERT INTO app_settings (key, value)
        VALUES ('workspace_run_budget_max_wall_clock_secs', ?1)
        ON CONFLICT(key) DO NOTHING
        ",
        params![DEFAULT_JOB_MAX_WALL_CLOCK_SECS.to_string()],
    )?;

    Ok(())
}

fn seed_workspace_profiles(connection: &Connection) -> Result<()> {
    let protocol_model = WorkspaceModelConfig {
        adapter: "openai_compatible".to_string(),
        model: DEFAULT_OPENAI_COMPATIBLE_MODEL.to_string(),
        base_url: DEFAULT_OPENAI_COMPATIBLE_BASE_URL.to_string(),
        api_key: String::new(),
    };
    let profiles = [
        (
            "default",
            "Default",
            protocol_model.clone(),
            protocol_model.clone(),
        ),
        (
            "researcher",
            "Researcher",
            protocol_model.clone(),
            protocol_model.clone(),
        ),
        (
            "developer",
            "Developer",
            protocol_model.clone(),
            protocol_model.clone(),
        ),
    ];

    for (id, title, main, utility) in profiles {
        connection.execute(
            "
            INSERT INTO workspace_profiles (id, title, main_model_json, utility_model_json)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                updated_at = unixepoch()
            ",
            params![
                id,
                title,
                serde_json::to_string(&main)?,
                serde_json::to_string(&utility)?,
            ],
        )?;
    }

    ensure_workspace_default_profile(connection)?;
    Ok(())
}

fn migrate_legacy_workspace_targets(connection: &Connection) -> Result<()> {
    if setting_value_optional(connection, "workspace_profiles_migrated")?
        .as_deref()
        .is_some_and(|value| value == "1")
    {
        return Ok(());
    }

    let default_profile_id = workspace_default_profile_id(connection)?;
    let legacy_main = setting_value_optional(connection, "workspace_main_target")?;
    let legacy_utility = setting_value_optional(connection, "workspace_utility_target")?;

    let current = load_workspace_profile_summary(connection, &default_profile_id)?;
    let next_main = legacy_main
        .as_deref()
        .and_then(|value| workspace_model_from_legacy_target(connection, value).ok())
        .unwrap_or(current.main.clone());
    let next_utility = legacy_utility
        .as_deref()
        .and_then(|value| workspace_model_from_legacy_target(connection, value).ok())
        .unwrap_or(current.utility.clone());

    update_workspace_profile_with_connection(
        connection,
        &default_profile_id,
        &WorkspaceProfilePatch {
            title: current.title,
            main: next_main,
            utility: next_utility,
            is_default: true,
        },
    )?;
    set_setting_value(connection, "workspace_profiles_migrated", "1")?;

    Ok(())
}

fn migrate_seeded_cli_defaults_to_protocol(connection: &Connection) -> Result<()> {
    let default_protocol_model = WorkspaceModelConfig {
        adapter: "openai_compatible".to_string(),
        model: DEFAULT_OPENAI_COMPATIBLE_MODEL.to_string(),
        base_url: DEFAULT_OPENAI_COMPATIBLE_BASE_URL.to_string(),
        api_key: String::new(),
    };

    for profile_id in ["default", "researcher", "developer"] {
        let Ok(current) = load_workspace_profile_summary(connection, profile_id) else {
            continue;
        };
        let next_main = if is_seeded_cli_model(&current.main)
            || is_seeded_protocol_model_missing_transport(&current.main)
        {
            default_protocol_model.clone()
        } else {
            current.main.clone()
        };
        let next_utility = if is_seeded_cli_model(&current.utility)
            || is_seeded_protocol_model_missing_transport(&current.utility)
        {
            default_protocol_model.clone()
        } else {
            current.utility.clone()
        };

        if next_main != current.main || next_utility != current.utility {
            update_workspace_profile_with_connection(
                connection,
                profile_id,
                &WorkspaceProfilePatch {
                    title: current.title,
                    main: next_main,
                    utility: next_utility,
                    is_default: current.is_default,
                },
            )?;
        }
    }

    if setting_value_optional(connection, "workspace_main_target")?
        .as_deref()
        .is_some_and(is_seeded_cli_target)
    {
        set_workspace_main_target(connection, "route:local-openai")?;
    }
    if setting_value_optional(connection, "workspace_utility_target")?
        .as_deref()
        .is_some_and(is_seeded_cli_target)
    {
        set_workspace_utility_target(connection, "route:local-openai")?;
    }

    Ok(())
}

fn is_seeded_cli_model(config: &WorkspaceModelConfig) -> bool {
    matches!(config.adapter.as_str(), "claude" | "codex")
        && config.base_url.trim().is_empty()
        && config.api_key.trim().is_empty()
}

fn is_seeded_protocol_model_missing_transport(config: &WorkspaceModelConfig) -> bool {
    config.adapter == AdapterKind::OpenAiCompatible.as_str()
        && config.model == DEFAULT_OPENAI_COMPATIBLE_MODEL
        && config.base_url.trim().is_empty()
        && config.api_key.trim().is_empty()
}

fn is_seeded_cli_target(value: &str) -> bool {
    matches!(
        value.trim(),
        "route:balanced"
            | "route:claude-sonnet"
            | "route:codex-default"
            | "provider:claude"
            | "provider:codex"
    )
}

fn ensure_local_auth_token_with_connection(
    plan: &StoragePlan,
    connection: &Connection,
) -> Result<()> {
    let token_path = plan.state_dir.join(LOCAL_AUTH_TOKEN_FILE_NAME);
    let token_value = match setting_value_optional(connection, LOCAL_AUTH_TOKEN_HASH_KEY)? {
        Some(hash) => {
            let file_token = read_token_file(&token_path)?;

            if !file_token.is_empty() && hash_auth_token(&file_token) == hash {
                file_token
            } else {
                let next = generate_local_auth_token();
                write_token_file(&token_path, &next)?;
                set_setting_value(
                    connection,
                    LOCAL_AUTH_TOKEN_HASH_KEY,
                    &hash_auth_token(&next),
                )?;
                next
            }
        }
        None => {
            let next = generate_local_auth_token();
            write_token_file(&token_path, &next)?;
            set_setting_value(
                connection,
                LOCAL_AUTH_TOKEN_HASH_KEY,
                &hash_auth_token(&next),
            )?;
            next
        }
    };

    if !token_value.is_empty() {
        write_token_file(&token_path, &token_value)?;
    }

    Ok(())
}

fn migrate_mcp_remote_bridge_records(connection: &Connection) -> Result<()> {
    let known = [
        (
            "cloudflare-api",
            "https://mcp.cloudflare.com/mcp",
            "bearer_env",
            "CLOUDFLARE_API_TOKEN",
        ),
        (
            "cloudflare-bindings",
            "https://bindings.mcp.cloudflare.com/mcp",
            "bearer_env",
            "CLOUDFLARE_API_TOKEN",
        ),
        (
            "cloudflare-builds",
            "https://builds.mcp.cloudflare.com/mcp",
            "bearer_env",
            "CLOUDFLARE_API_TOKEN",
        ),
        (
            "cloudflare-docs",
            "https://docs.mcp.cloudflare.com/mcp",
            "none",
            "",
        ),
        (
            "cloudflare-observability",
            "https://observability.mcp.cloudflare.com/mcp",
            "bearer_env",
            "CLOUDFLARE_API_TOKEN",
        ),
        ("context7", "https://mcp.context7.com/mcp", "none", ""),
        ("emdash-docs", "https://docs.emdashcms.com/mcp", "none", ""),
        (
            "supabase",
            "https://mcp.supabase.com/mcp",
            "bearer_env",
            "SUPABASE_ACCESS_TOKEN",
        ),
        (
            "vercel",
            "https://mcp.vercel.com",
            "bearer_env",
            "VERCEL_TOKEN",
        ),
    ];
    for (id, url, auth_kind, auth_ref) in known {
        connection.execute(
            "
            UPDATE mcp_servers
            SET transport = 'streamable-http',
                url = ?2,
                command = '',
                args_json = '[]',
                auth_kind = ?3,
                auth_ref = ?4,
                headers_json = COALESCE(NULLIF(headers_json, ''), '{}'),
                sync_status = CASE WHEN ?3 = 'none' THEN 'pending' ELSE 'missing_credentials' END,
                last_error = CASE WHEN ?3 = 'none' THEN '' ELSE 'missing_credentials: configure native MCP credentials' END,
                updated_at = unixepoch()
            WHERE id = ?1
              AND transport = 'stdio'
              AND command = 'npx'
              AND args_json LIKE '%mcp-remote%'
            ",
            params![id, url, auth_kind, auth_ref],
        )?;
    }
    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let existing = table_columns(connection, table)?;

    if existing.contains(column) {
        return Ok(());
    }

    connection.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;

    Ok(())
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    let columns = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to inspect table columns")?;

    Ok(columns.into_iter().collect())
}

fn playbook_file_path(playbooks_dir: &Path, playbook_id: &str) -> PathBuf {
    playbooks_dir.join(format!("{playbook_id}.json"))
}

fn read_playbook_file(playbooks_dir: &Path, playbook_id: &str) -> Result<StoredPlaybook> {
    let path = playbook_file_path(playbooks_dir, playbook_id);
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read playbook '{}'", path.display()))?;
    serde_json::from_str::<StoredPlaybook>(&content)
        .with_context(|| format!("failed to decode playbook '{}'", path.display()))
}

fn write_playbook_file(playbooks_dir: &Path, playbook: &StoredPlaybook) -> Result<()> {
    fs::create_dir_all(playbooks_dir).with_context(|| {
        format!(
            "failed to create playbook directory '{}'",
            playbooks_dir.display()
        )
    })?;
    let path = playbook_file_path(playbooks_dir, &playbook.id);
    let encoded =
        serde_json::to_string_pretty(playbook).context("failed to encode playbook JSON")?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, encoded.as_bytes())
        .with_context(|| format!("failed to write playbook '{}'", temp_path.display()))?;
    fs::rename(&temp_path, &path)
        .with_context(|| format!("failed to move playbook '{}' into place", path.display()))?;
    Ok(())
}

fn list_playbooks_with_connection(
    connection: &Connection,
    playbooks_dir: &Path,
) -> Result<Vec<PlaybookSummary>> {
    if !playbooks_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = fs::read_dir(playbooks_dir)
        .with_context(|| format!("failed to read '{}'", playbooks_dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut playbooks = paths
        .into_iter()
        .map(|path| {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read playbook '{}'", path.display()))?;
            let stored = serde_json::from_str::<StoredPlaybook>(&content)
                .with_context(|| format!("failed to decode playbook '{}'", path.display()))?;
            playbook_summary_from_stored(connection, &stored)
        })
        .collect::<Result<Vec<_>>>()?;
    playbooks.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(playbooks)
}

fn load_playbook_detail(
    connection: &Connection,
    playbooks_dir: &Path,
    playbook_id: &str,
) -> Result<PlaybookDetail> {
    let stored = read_playbook_file(playbooks_dir, playbook_id)?;
    playbook_detail_from_stored(connection, &stored)
}

fn playbook_detail_from_stored(
    connection: &Connection,
    stored: &StoredPlaybook,
) -> Result<PlaybookDetail> {
    Ok(PlaybookDetail {
        playbook: playbook_summary_from_stored(connection, stored)?,
        session: load_session_summary(connection, &stored.session_id)?,
        prompt: stored.prompt.clone(),
        recent_jobs: list_jobs_for_template_with_connection(
            connection,
            &stored.id,
            PLAYBOOK_RECENT_JOB_LIMIT,
        )?,
    })
}

fn playbook_summary_from_stored(
    connection: &Connection,
    stored: &StoredPlaybook,
) -> Result<PlaybookSummary> {
    let session = load_session_summary(connection, &stored.session_id)?;
    let recent_jobs = list_jobs_for_template_with_connection(connection, &stored.id, 1)?;
    let latest = recent_jobs.first();
    Ok(PlaybookSummary {
        id: stored.id.clone(),
        session_id: stored.session_id.clone(),
        title: stored.title.clone(),
        description: stored.description.clone(),
        prompt_excerpt: excerpt(&stored.prompt, 160),
        enabled: stored.enabled,
        policy_bundle: stored.policy_bundle.clone(),
        trigger_kind: stored.trigger_kind.clone(),
        schedule_interval_secs: stored.schedule_interval_secs,
        event_kind: stored.event_kind.clone(),
        profile_id: session.profile_id,
        profile_title: session.profile_title,
        project_id: session.project_id,
        project_title: session.project_title,
        working_dir: session.working_dir,
        job_count: count_jobs_for_template(connection, &stored.id)?,
        last_job_id: latest.map(|job| job.id.clone()),
        last_job_state: latest.map(|job| job.state.clone()).unwrap_or_default(),
        last_run_at: latest.map(|job| job.created_at),
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    })
}

fn load_workspace_summary(connection: &Connection) -> Result<WorkspaceSummary> {
    let default_profile_id = workspace_default_profile_id(connection)?;
    Ok(WorkspaceSummary {
        root_path: workspace_root(connection)?,
        default_profile_id: default_profile_id.clone(),
        main_target: workspace_main_target(connection)?,
        utility_target: workspace_utility_target(connection)?,
        run_budget: workspace_run_budget(connection)?,
        profiles: list_workspace_profiles_with_connection(connection, &default_profile_id)?,
        projects: list_projects_with_connection(connection)?,
    })
}

fn workspace_root(connection: &Connection) -> Result<String> {
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'workspace_root'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("workspace root is not configured"))
}

fn set_workspace_root(connection: &Connection, root_path: &str) -> Result<()> {
    set_setting_value(connection, "workspace_root", root_path)?;
    Ok(())
}

fn workspace_default_profile_id(connection: &Connection) -> Result<String> {
    ensure_workspace_default_profile(connection)?;
    setting_value(connection, "workspace_default_profile_id")
}

fn workspace_default_profile_id_optional(connection: &Connection) -> Result<Option<String>> {
    setting_value_optional(connection, "workspace_default_profile_id")
}

fn set_workspace_default_profile_id(connection: &Connection, profile_id: &str) -> Result<()> {
    load_workspace_profile_summary(connection, profile_id)?;
    set_setting_value(connection, "workspace_default_profile_id", profile_id)?;
    Ok(())
}

fn ensure_workspace_default_profile(connection: &Connection) -> Result<()> {
    let candidate = workspace_default_profile_id_optional(connection)?;
    if let Some(profile_id) = candidate {
        if load_workspace_profile_summary(connection, &profile_id).is_ok() {
            return Ok(());
        }
    }

    let fallback = list_workspace_profile_ids(connection)?
        .into_iter()
        .next()
        .unwrap_or_else(|| "default".to_string());
    set_setting_value(connection, "workspace_default_profile_id", &fallback)
}

fn workspace_main_target(connection: &Connection) -> Result<String> {
    setting_value(connection, "workspace_main_target")
}

fn set_workspace_main_target(connection: &Connection, target: &str) -> Result<()> {
    set_setting_value(connection, "workspace_main_target", target)
}

fn workspace_utility_target(connection: &Connection) -> Result<String> {
    setting_value(connection, "workspace_utility_target")
}

fn set_workspace_utility_target(connection: &Connection, target: &str) -> Result<()> {
    set_setting_value(connection, "workspace_utility_target", target)
}

fn workspace_run_budget(connection: &Connection) -> Result<RunBudgetSummary> {
    Ok(RunBudgetSummary {
        mode: "standard".to_string(),
        max_steps: setting_usize_or_default(
            connection,
            "workspace_run_budget_max_steps",
            DEFAULT_JOB_MAX_STEPS,
        )?,
        max_tool_calls: setting_usize_or_default(
            connection,
            "workspace_run_budget_max_tool_calls",
            DEFAULT_JOB_MAX_TOOL_CALLS,
        )?,
        max_wall_clock_secs: setting_u64_or_default(
            connection,
            "workspace_run_budget_max_wall_clock_secs",
            DEFAULT_JOB_MAX_WALL_CLOCK_SECS,
        )?,
    })
}

fn set_workspace_run_budget(connection: &Connection, run_budget: &RunBudgetSummary) -> Result<()> {
    set_setting_value(
        connection,
        "workspace_run_budget_max_steps",
        &run_budget.max_steps.to_string(),
    )?;
    set_setting_value(
        connection,
        "workspace_run_budget_max_tool_calls",
        &run_budget.max_tool_calls.to_string(),
    )?;
    set_setting_value(
        connection,
        "workspace_run_budget_max_wall_clock_secs",
        &run_budget.max_wall_clock_secs.to_string(),
    )
}

fn session_run_budget(connection: &Connection, mode: &str) -> Result<RunBudgetSummary> {
    let workspace = workspace_run_budget(connection)?;
    let mode = mode.trim();
    let mut budget = match mode {
        "" | "inherit" => workspace,
        "standard" => RunBudgetSummary::default(),
        "extended" => RunBudgetSummary {
            mode: "extended".to_string(),
            max_steps: 200,
            max_tool_calls: 400,
            max_wall_clock_secs: 14_400,
        },
        "marathon" => RunBudgetSummary {
            mode: "marathon".to_string(),
            max_steps: 600,
            max_tool_calls: 1_200,
            max_wall_clock_secs: 28_800,
        },
        "unbounded" => RunBudgetSummary {
            mode: "unbounded".to_string(),
            max_steps: 0,
            max_tool_calls: 0,
            max_wall_clock_secs: 0,
        },
        _ => workspace,
    };
    budget.mode = if mode.is_empty() {
        "inherit".to_string()
    } else {
        mode.to_string()
    };
    Ok(budget)
}

fn list_workspace_profile_ids(connection: &Connection) -> Result<Vec<String>> {
    let mut statement =
        connection.prepare("SELECT id FROM workspace_profiles ORDER BY created_at ASC, id ASC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load workspace profile ids")
}

fn list_workspace_profiles_with_connection(
    connection: &Connection,
    default_profile_id: &str,
) -> Result<Vec<WorkspaceProfileSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id, title, main_model_json, utility_model_json, created_at, updated_at
        FROM workspace_profiles
        ORDER BY
            CASE id
                WHEN ?1 THEN 0
                ELSE 1
            END,
            updated_at DESC,
            created_at ASC,
            title ASC
        ",
    )?;

    let rows = statement.query_map(params![default_profile_id], |row| {
        map_workspace_profile_row(row, default_profile_id)
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load workspace profiles")
}

fn load_workspace_profile_summary(
    connection: &Connection,
    profile_id: &str,
) -> Result<WorkspaceProfileSummary> {
    let default_profile_id =
        workspace_default_profile_id_optional(connection)?.unwrap_or_else(|| "default".to_string());

    connection
        .query_row(
            "
            SELECT id, title, main_model_json, utility_model_json, created_at, updated_at
            FROM workspace_profiles
            WHERE id = ?1
            ",
            params![profile_id],
            |row| map_workspace_profile_row(row, &default_profile_id),
        )
        .optional()?
        .ok_or_else(|| anyhow!("workspace profile '{profile_id}' was not found"))
}

fn map_workspace_profile_row(
    row: &rusqlite::Row<'_>,
    default_profile_id: &str,
) -> rusqlite::Result<WorkspaceProfileSummary> {
    let id: String = row.get(0)?;
    let main_json: String = row.get(2)?;
    let utility_json: String = row.get(3)?;
    let main = decode_workspace_model_config(&main_json, 2)?;
    let utility = decode_workspace_model_config(&utility_json, 3)?;

    Ok(WorkspaceProfileSummary {
        is_default: id == default_profile_id,
        id,
        title: row.get(1)?,
        main,
        utility,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn decode_workspace_model_config(
    value: &str,
    column_index: usize,
) -> rusqlite::Result<WorkspaceModelConfig> {
    serde_json::from_str::<WorkspaceModelConfig>(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column_index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn create_workspace_profile_with_connection(
    connection: &Connection,
    patch: &WorkspaceProfilePatch,
) -> Result<String> {
    let profile_id = unique_workspace_profile_id(connection, &patch.title)?;
    connection.execute(
        "
        INSERT INTO workspace_profiles (id, title, main_model_json, utility_model_json)
        VALUES (?1, ?2, ?3, ?4)
        ",
        params![
            profile_id,
            patch.title.trim(),
            serde_json::to_string(&patch.main)?,
            serde_json::to_string(&patch.utility)?,
        ],
    )?;
    Ok(profile_id)
}

fn update_workspace_profile_with_connection(
    connection: &Connection,
    profile_id: &str,
    patch: &WorkspaceProfilePatch,
) -> Result<()> {
    let updated = connection.execute(
        "
        UPDATE workspace_profiles
        SET
            title = ?2,
            main_model_json = ?3,
            utility_model_json = ?4,
            updated_at = unixepoch()
        WHERE id = ?1
        ",
        params![
            profile_id,
            patch.title.trim(),
            serde_json::to_string(&patch.main)?,
            serde_json::to_string(&patch.utility)?,
        ],
    )?;

    if updated == 0 {
        bail!("workspace profile '{profile_id}' was not found");
    }

    Ok(())
}

fn delete_workspace_profile_with_connection(
    connection: &Connection,
    profile_id: &str,
) -> Result<()> {
    let ids = list_workspace_profile_ids(connection)?;
    if ids.len() <= 1 {
        bail!("at least one workspace profile is required");
    }

    let deleted = connection.execute(
        "DELETE FROM workspace_profiles WHERE id = ?1",
        params![profile_id],
    )?;

    if deleted == 0 {
        bail!("workspace profile '{profile_id}' was not found");
    }

    if workspace_default_profile_id_optional(connection)?
        .as_deref()
        .is_some_and(|current| current == profile_id)
    {
        let fallback = list_workspace_profile_ids(connection)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("at least one workspace profile is required"))?;
        set_setting_value(connection, "workspace_default_profile_id", &fallback)?;
    }

    Ok(())
}

fn unique_workspace_profile_id(connection: &Connection, title: &str) -> Result<String> {
    let base = slugify_profile_title(title);
    let mut candidate = base.clone();
    let mut suffix = 2usize;

    while load_workspace_profile_summary(connection, &candidate).is_ok() {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }

    Ok(candidate)
}

fn slugify_profile_title(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for character in value.trim().chars() {
        let next = character.to_ascii_lowercase();
        if next.is_ascii_alphanumeric() {
            slug.push(next);
            last_was_dash = false;
            continue;
        }

        if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "profile".to_string()
    } else {
        slug
    }
}

fn workspace_model_from_legacy_target(
    connection: &Connection,
    value: &str,
) -> Result<WorkspaceModelConfig> {
    let value = value.trim();

    if let Some(route_id) = value.strip_prefix("route:").map(str::trim) {
        let profile = load_router_profile(connection, route_id)?;
        let target = profile
            .targets
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("router profile '{route_id}' has no targets"))?;
        return Ok(WorkspaceModelConfig {
            adapter: target.provider,
            model: target.model,
            base_url: target.base_url,
            api_key: target.api_key,
        });
    }

    if let Some(provider) = value.strip_prefix("provider:").map(str::trim) {
        let base_url = if provider == AdapterKind::OpenAiCompatible.as_str() {
            DEFAULT_OPENAI_COMPATIBLE_BASE_URL.to_string()
        } else {
            String::new()
        };
        return Ok(WorkspaceModelConfig {
            adapter: provider.to_string(),
            model: AdapterKind::parse(provider)
                .map(|adapter| adapter.default_model().to_string())
                .unwrap_or_default(),
            base_url,
            api_key: String::new(),
        });
    }

    bail!("unsupported legacy workspace target '{value}'")
}

fn setting_value(connection: &Connection, key: &str) -> Result<String> {
    setting_value_optional(connection, key)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("setting '{key}' is not configured"))
}

fn setting_value_optional(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to load setting")
}

fn setting_usize_or_default(connection: &Connection, key: &str, default: usize) -> Result<usize> {
    Ok(setting_value_optional(connection, key)?
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default))
}

fn setting_u64_or_default(connection: &Connection, key: &str, default: u64) -> Result<u64> {
    Ok(setting_value_optional(connection, key)?
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default))
}

fn set_setting_value(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection.execute(
        "
        INSERT INTO app_settings (key, value, updated_at)
        VALUES (?1, ?2, unixepoch())
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at
        ",
        params![key, value],
    )?;
    Ok(())
}

fn generate_local_auth_token() -> String {
    format!(
        "nuctk_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn hash_auth_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);

    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }

    output
}

fn read_token_file(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }

    fs::read_to_string(path)
        .with_context(|| format!("failed to read token file '{}'", path.display()))
        .map(|value| value.trim().to_string())
}

fn write_token_file(path: &Path, token: &str) -> Result<()> {
    fs::write(path, format!("{token}\n"))
        .with_context(|| format!("failed to write token file '{}'", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions).with_context(|| {
            format!(
                "failed to set permissions on token file '{}'",
                path.display()
            )
        })?;
    }

    Ok(())
}

fn sync_projects_with_connection(connection: &Connection) -> Result<()> {
    let root_path = workspace_root(connection)?;
    let root = PathBuf::from(&root_path);

    if !root.exists() {
        return Ok(());
    }

    if !root.is_dir() {
        bail!("workspace root '{}' is not a directory", root.display());
    }

    for project in discover_projects(&root)? {
        connection.execute(
            "
            INSERT INTO projects (id, title, slug, relative_path, absolute_path, active)
            VALUES (?1, ?2, ?3, ?4, ?5, 1)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                slug = excluded.slug,
                relative_path = excluded.relative_path,
                absolute_path = excluded.absolute_path,
                updated_at = unixepoch()
            ",
            params![
                project.id,
                project.title,
                project.slug,
                project.relative_path,
                project.absolute_path
            ],
        )?;
    }

    backfill_legacy_session_projects_with_connection(connection)?;

    Ok(())
}

fn list_projects_with_connection(connection: &Connection) -> Result<Vec<ProjectSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id, title, slug, relative_path, absolute_path, created_at, updated_at
        FROM projects
        ORDER BY relative_path ASC, title ASC
        ",
    )?;

    let rows = statement.query_map([], map_project_summary)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load projects")
}

fn load_project_summary(connection: &Connection, project_id: &str) -> Result<ProjectSummary> {
    connection
        .query_row(
            "
            SELECT id, title, slug, relative_path, absolute_path, created_at, updated_at
            FROM projects
            WHERE id = ?1
            ",
            params![project_id],
            map_project_summary,
        )
        .optional()?
        .ok_or_else(|| anyhow!("project '{project_id}' was not found"))
}

fn map_project_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectSummary> {
    Ok(ProjectSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        slug: row.get(2)?,
        relative_path: row.get(3)?,
        absolute_path: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn load_resolved_project(connection: &Connection, project_id: &str) -> Result<ResolvedProject> {
    connection
        .query_row(
            "
            SELECT id, title, slug, relative_path, absolute_path
            FROM projects
            WHERE id = ?1
            ",
            params![project_id],
            |row| {
                Ok(ResolvedProject {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    slug: row.get(2)?,
                    relative_path: row.get(3)?,
                    absolute_path: row.get(4)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("project '{project_id}' was not found"))
}

fn load_resolved_project_by_path(
    connection: &Connection,
    path: &str,
) -> Result<Option<ResolvedProject>> {
    connection
        .query_row(
            "
            SELECT id, title, slug, relative_path, absolute_path
            FROM projects
            WHERE absolute_path = ?1 OR relative_path = ?1
            ORDER BY CASE
                WHEN absolute_path = ?1 THEN 0
                ELSE 1
            END
            LIMIT 1
            ",
            params![path],
            |row| {
                Ok(ResolvedProject {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    slug: row.get(2)?,
                    relative_path: row.get(3)?,
                    absolute_path: row.get(4)?,
                })
            },
        )
        .optional()
        .context("failed to resolve project by path")
}

fn resolve_projects_with_connection(
    connection: &Connection,
    project_ids: &[String],
) -> Result<Vec<ResolvedProject>> {
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();

    for project_id in project_ids {
        let project_id = project_id.trim();

        if project_id.is_empty() || !seen.insert(project_id.to_string()) {
            continue;
        }

        resolved.push(load_resolved_project(connection, project_id)?);
    }

    Ok(resolved)
}

fn backfill_legacy_session_projects_with_connection(connection: &Connection) -> Result<()> {
    #[derive(Debug)]
    struct LegacySessionProjectRecord {
        session_id: String,
        project_id: String,
        project_path: String,
        working_dir: String,
        working_dir_kind: String,
    }

    let mut statement = connection.prepare(
        "
        SELECT id, project_id, project_path, scope, working_dir, working_dir_kind
        FROM sessions
        WHERE TRIM(project_id) <> ''
           OR EXISTS (
                SELECT 1
                FROM session_projects
                WHERE session_projects.session_id = sessions.id
           )
        ",
    )?;

    let rows = statement.query_map([], |row| {
        Ok(LegacySessionProjectRecord {
            session_id: row.get(0)?,
            project_id: row.get(1)?,
            project_path: row.get(2)?,
            working_dir: row.get(4)?,
            working_dir_kind: row.get(5)?,
        })
    })?;

    let records = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to inspect legacy session project rows")?;
    drop(statement);

    for record in records {
        let existing_count = connection.query_row(
            "SELECT COUNT(*) FROM session_projects WHERE session_id = ?1",
            params![record.session_id],
            |row| row.get::<_, i64>(0),
        )?;

        let project = if existing_count > 0 {
            load_primary_project_for_session(connection, &record.session_id)?.or(
                resolve_legacy_session_project(
                    connection,
                    record.project_id.trim(),
                    record.project_path.trim(),
                )?,
            )
        } else {
            let project = resolve_legacy_session_project(
                connection,
                record.project_id.trim(),
                record.project_path.trim(),
            )?;

            if let Some(project) = project.as_ref() {
                connection.execute(
                    "
                    INSERT INTO session_projects (session_id, project_id, sort_order, is_primary)
                    VALUES (?1, ?2, 0, 1)
                    ON CONFLICT(session_id, project_id) DO UPDATE SET
                        sort_order = excluded.sort_order,
                        is_primary = excluded.is_primary
                    ",
                    params![record.session_id, project.id],
                )?;
            }

            project
        };
        let Some(project) = project else {
            continue;
        };

        let next_working_dir = if record.working_dir.trim().is_empty()
            || record.working_dir_kind == "workspace_scratch"
        {
            project.absolute_path.clone()
        } else {
            record.working_dir
        };
        let next_working_dir_kind = if record.working_dir_kind.trim().is_empty()
            || record.working_dir_kind == "workspace_scratch"
            || record.working_dir_kind == "project"
        {
            "project_root".to_string()
        } else {
            record.working_dir_kind
        };
        let next_scope = match existing_count.max(1) {
            1 => "project".to_string(),
            _ => "multi_project".to_string(),
        };

        connection.execute(
            "
            UPDATE sessions
            SET
                scope = ?2,
                project_id = ?3,
                project_title = ?4,
                project_path = ?5,
                working_dir = ?6,
                working_dir_kind = ?7
            WHERE id = ?1
            ",
            params![
                record.session_id,
                next_scope,
                project.id,
                project.title,
                project.absolute_path,
                next_working_dir,
                next_working_dir_kind,
            ],
        )?;
    }

    Ok(())
}

fn resolve_legacy_session_project(
    connection: &Connection,
    project_id: &str,
    project_path: &str,
) -> Result<Option<ResolvedProject>> {
    if !project_id.is_empty() {
        if let Ok(project) = load_resolved_project(connection, project_id) {
            return Ok(Some(project));
        }
    }

    if !project_path.is_empty() {
        if let Some(project) = load_resolved_project_by_path(connection, project_path)? {
            return Ok(Some(project));
        }
    }

    Ok(None)
}

fn load_primary_project_for_session(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<ResolvedProject>> {
    connection
        .query_row(
            "
            SELECT
                projects.id,
                projects.title,
                projects.slug,
                projects.relative_path,
                projects.absolute_path
            FROM session_projects
            INNER JOIN projects ON projects.id = session_projects.project_id
            WHERE session_projects.session_id = ?1
            ORDER BY session_projects.is_primary DESC, session_projects.sort_order ASC, projects.relative_path ASC
            LIMIT 1
            ",
            params![session_id],
            |row| {
                Ok(ResolvedProject {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    slug: row.get(2)?,
                    relative_path: row.get(3)?,
                    absolute_path: row.get(4)?,
                })
            },
        )
        .optional()
        .context("failed to load primary session project")
}

fn replace_session_projects(
    connection: &Connection,
    session_id: &str,
    project_ids: &[String],
    primary_project_id: &str,
) -> Result<()> {
    connection.execute(
        "DELETE FROM session_projects WHERE session_id = ?1",
        params![session_id],
    )?;

    for (index, project_id) in project_ids.iter().enumerate() {
        connection.execute(
            "
            INSERT INTO session_projects (session_id, project_id, sort_order, is_primary)
            VALUES (?1, ?2, ?3, ?4)
            ",
            params![
                session_id,
                project_id,
                index as i64,
                (project_id == primary_project_id) as i64,
            ],
        )?;
    }

    Ok(())
}

fn list_router_profiles_with_connection(
    connection: &Connection,
) -> Result<Vec<RouterProfileSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id, title, summary, enabled, targets_json
        FROM router_profiles
        ORDER BY CASE id
            WHEN 'local-openai' THEN 1
            WHEN 'balanced' THEN 2
            WHEN 'claude-sonnet' THEN 3
            WHEN 'codex-default' THEN 4
            ELSE 100
        END, title ASC
        ",
    )?;

    let rows = statement.query_map([], map_router_profile)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load router profiles")
}

fn load_router_profile(connection: &Connection, route_id: &str) -> Result<RouterProfileSummary> {
    connection
        .query_row(
            "
            SELECT id, title, summary, enabled, targets_json
            FROM router_profiles
            WHERE id = ?1
            ",
            params![route_id],
            map_router_profile,
        )
        .optional()?
        .ok_or_else(|| anyhow!("router profile '{route_id}' was not found"))
}

fn map_router_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<RouterProfileSummary> {
    let targets_json: String = row.get(4)?;
    let targets = serde_json::from_str::<Vec<RouteTarget>>(&targets_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;

    Ok(RouterProfileSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        summary: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        state: "configured".to_string(),
        targets,
    })
}

fn list_skill_manifests_with_connection(connection: &Connection) -> Result<Vec<SkillManifest>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM skill_manifests
        ORDER BY enabled DESC, title COLLATE NOCASE, id
        ",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load skill manifest ids")?;
    ids.into_iter()
        .map(|id| load_skill_manifest(connection, &id))
        .collect()
}

fn load_skill_manifest(connection: &Connection, id: &str) -> Result<SkillManifest> {
    connection
        .query_row(
            "
            SELECT
                id, title, description, instructions, activation_mode, triggers_json,
                include_paths_json, required_tools_json, required_mcps_json,
                project_filters_json, enabled
            FROM skill_manifests
            WHERE id = ?1
            ",
            params![id],
            map_skill_manifest,
        )
        .optional()?
        .ok_or_else(|| anyhow!("skill manifest '{id}' was not found"))
}

fn map_skill_manifest(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillManifest> {
    Ok(SkillManifest {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        instructions: row.get(3)?,
        activation_mode: row.get(4)?,
        triggers: decode_string_vec_column(row, 5)?,
        include_paths: decode_string_vec_column(row, 6)?,
        required_tools: decode_string_vec_column(row, 7)?,
        required_mcps: decode_string_vec_column(row, 8)?,
        project_filters: decode_string_vec_column(row, 9)?,
        enabled: row.get::<_, i64>(10)? != 0,
    })
}

fn list_mcp_servers_with_connection(connection: &Connection) -> Result<Vec<McpServerSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM mcp_servers
        ORDER BY enabled DESC, title COLLATE NOCASE, id
        ",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load MCP server ids")?;
    ids.into_iter()
        .map(|id| load_mcp_server(connection, &id))
        .collect()
}

fn list_mcp_server_records_with_connection(
    connection: &Connection,
) -> Result<Vec<McpServerRecord>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM mcp_servers
        ORDER BY enabled DESC, title COLLATE NOCASE, id
        ",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load MCP server record ids")?;
    ids.into_iter()
        .map(|id| load_mcp_server_record(connection, &id))
        .collect()
}

fn load_mcp_server(connection: &Connection, id: &str) -> Result<McpServerSummary> {
    connection
        .query_row(
            "
            SELECT id, title, enabled, transport, command, args_json, env_json, url, headers_json, auth_kind, auth_ref, sync_status, last_error, last_synced_at, tools_json, resources_json
            FROM mcp_servers
            WHERE id = ?1
            ",
            params![id],
            map_mcp_server,
        )
        .optional()?
        .ok_or_else(|| anyhow!("MCP server '{id}' was not found"))
}

fn load_mcp_server_record(connection: &Connection, id: &str) -> Result<McpServerRecord> {
    connection
        .query_row(
            "
            SELECT
                id, workspace_id, title, transport, command, args_json, env_json, url, headers_json, auth_kind, auth_ref, enabled,
                sync_status, last_error, last_synced_at, created_at, updated_at
            FROM mcp_servers
            WHERE id = ?1
            ",
            params![id],
            map_mcp_server_record,
        )
        .optional()?
        .ok_or_else(|| anyhow!("MCP server '{id}' was not found"))
}

fn map_mcp_server(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServerSummary> {
    let tools_json: String = row.get(14)?;
    let resources_json: String = row.get(15)?;
    let tools = serde_json::from_str(&tools_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let resources = serde_json::from_str(&resources_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;

    Ok(McpServerSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        transport: row.get(3)?,
        command: row.get(4)?,
        args: decode_string_list(row.get::<_, String>(5)?)?,
        env_json: decode_json_value(row.get::<_, String>(6)?)?,
        url: row.get(7)?,
        headers_json: redact_mcp_headers(decode_json_value(row.get::<_, String>(8)?)?),
        auth_kind: row.get(9)?,
        auth_ref: row.get(10)?,
        sync_status: row.get(11)?,
        last_error: row.get(12)?,
        last_synced_at: row.get(13)?,
        tools,
        resources,
    })
}

fn map_mcp_server_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServerRecord> {
    Ok(McpServerRecord {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        title: row.get(2)?,
        transport: row.get(3)?,
        command: row.get(4)?,
        args: decode_string_list(row.get::<_, String>(5)?)?,
        env_json: decode_json_value(row.get::<_, String>(6)?)?,
        url: row.get(7)?,
        headers_json: decode_json_value(row.get::<_, String>(8)?)?,
        auth_kind: row.get(9)?,
        auth_ref: row.get(10)?,
        enabled: row.get::<_, i64>(11)? != 0,
        sync_status: row.get(12)?,
        last_error: row.get(13)?,
        last_synced_at: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn redact_mcp_headers(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        for (key, val) in object.iter_mut() {
            let k = key.to_ascii_lowercase();
            if k.contains("authorization")
                || k.contains("token")
                || k.contains("key")
                || k.contains("cookie")
            {
                *val = serde_json::Value::String("[redacted]".to_string());
            }
        }
    }
    value
}

fn upsert_mcp_server_record_only(
    connection: &Connection,
    record: &McpServerRecord,
    tools: &[nucleus_protocol::NucleusToolDescriptor],
    resources: &[String],
) -> Result<()> {
    connection.execute(
        "
        INSERT INTO mcp_servers (
            id, workspace_id, title, transport, command, args_json, env_json, url, headers_json, auth_kind, auth_ref, enabled,
            sync_status, last_error, last_synced_at, tools_json, resources_json, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
            COALESCE(NULLIF(?18, 0), unixepoch()),
            COALESCE(NULLIF(?19, 0), unixepoch()))
        ON CONFLICT(id) DO UPDATE SET
            workspace_id = excluded.workspace_id,
            title = excluded.title,
            transport = excluded.transport,
            command = excluded.command,
            args_json = excluded.args_json,
            env_json = excluded.env_json,
            url = excluded.url,
            headers_json = excluded.headers_json,
            auth_kind = excluded.auth_kind,
            auth_ref = excluded.auth_ref,
            enabled = excluded.enabled,
            sync_status = excluded.sync_status,
            last_error = excluded.last_error,
            last_synced_at = excluded.last_synced_at,
            tools_json = excluded.tools_json,
            resources_json = excluded.resources_json,
            updated_at = COALESCE(NULLIF(excluded.updated_at, 0), unixepoch())
        ",
        params![
            record.id,
            record.workspace_id,
            record.title,
            record.transport,
            record.command,
            serde_json::to_string(&record.args)?,
            serde_json::to_string(&record.env_json)?,
            record.url,
            serde_json::to_string(&record.headers_json)?,
            record.auth_kind,
            record.auth_ref,
            record.enabled as i64,
            record.sync_status,
            record.last_error,
            record.last_synced_at,
            serde_json::to_string(tools)?,
            serde_json::to_string(resources)?,
            record.created_at,
            record.updated_at,
        ],
    )?;
    Ok(())
}

fn upsert_mcp_server_record_with_summary(
    connection: &Connection,
    record: &McpServerRecord,
    summary: &McpServerSummary,
) -> Result<McpServerSummary> {
    upsert_mcp_server_record_only(connection, record, &summary.tools, &summary.resources)?;
    load_mcp_server(connection, &record.id)
}

fn list_mcp_tools_with_connection(
    connection: &Connection,
) -> Result<Vec<nucleus_protocol::McpToolRecord>> {
    let mut statement =
        connection.prepare("SELECT id FROM mcp_tools ORDER BY server_id ASC, name ASC, id ASC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| load_mcp_tool(connection, &id))
        .collect()
}

fn load_mcp_tool(connection: &Connection, id: &str) -> Result<nucleus_protocol::McpToolRecord> {
    connection.query_row("SELECT id, server_id, name, description, input_schema_json, source, discovered_at, created_at, updated_at FROM mcp_tools WHERE id = ?1", params![id], map_mcp_tool).optional()?.ok_or_else(|| anyhow!("mcp tool {id} was not found"))
}

fn map_mcp_tool(row: &rusqlite::Row<'_>) -> rusqlite::Result<nucleus_protocol::McpToolRecord> {
    Ok(nucleus_protocol::McpToolRecord {
        id: row.get(0)?,
        server_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        input_schema: decode_json_value(row.get(4)?)?,
        source: row.get(5)?,
        discovered_at: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn map_vault_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<VaultStateRecord> {
    Ok(VaultStateRecord {
        id: row.get(0)?,
        version: row.get(1)?,
        vault_id: row.get(2)?,
        status: row.get(3)?,
        kdf_algorithm: row.get(4)?,
        kdf_params_json: row.get(5)?,
        salt: row.get(6)?,
        cipher: row.get(7)?,
        encrypted_root_check: row.get(8)?,
        root_check_nonce: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn map_vault_scope_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<VaultScopeKeyRecord> {
    Ok(VaultScopeKeyRecord {
        id: row.get(0)?,
        vault_id: row.get(1)?,
        scope_kind: row.get(2)?,
        scope_id: row.get(3)?,
        encrypted_key: row.get(4)?,
        nonce: row.get(5)?,
        aad: row.get(6)?,
        key_version: row.get(7)?,
        created_at: row.get(8)?,
        rotated_at: row.get(9)?,
    })
}

fn load_vault_secret(connection: &Connection, id: &str) -> Result<VaultSecretRecord> {
    connection.query_row(
        "SELECT id, scope_key_id, scope_kind, scope_id, name, description, ciphertext, nonce, aad, version, created_at, updated_at, last_used_at FROM vault_secrets WHERE id = ?1",
        params![id],
        map_vault_secret,
    ).optional()?.ok_or_else(|| anyhow!("vault secret {id} was not found"))
}

fn map_vault_secret(row: &rusqlite::Row<'_>) -> rusqlite::Result<VaultSecretRecord> {
    Ok(VaultSecretRecord {
        id: row.get(0)?,
        scope_key_id: row.get(1)?,
        scope_kind: row.get(2)?,
        scope_id: row.get(3)?,
        name: row.get(4)?,
        description: row.get(5)?,
        ciphertext: row.get(6)?,
        nonce: row.get(7)?,
        aad: row.get(8)?,
        version: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        last_used_at: row.get(12)?,
    })
}

fn map_vault_secret_policy(row: &rusqlite::Row<'_>) -> rusqlite::Result<VaultSecretPolicyRecord> {
    Ok(VaultSecretPolicyRecord {
        id: row.get(0)?,
        secret_id: row.get(1)?,
        consumer_kind: row.get(2)?,
        consumer_id: row.get(3)?,
        permission: row.get(4)?,
        approval_mode: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn list_memory_entries_with_connection(connection: &Connection) -> Result<Vec<MemoryEntry>> {
    let mut statement = connection.prepare(
        "SELECT id FROM memory_entries ORDER BY scope_kind ASC, scope_id ASC, title ASC, id ASC",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| load_memory_entry(connection, &id))
        .collect()
}

fn memory_entry_is_searchable(entry: &MemoryEntry) -> bool {
    entry.enabled && entry.status == "accepted"
}

fn delete_memory_fts_row(connection: &Connection, id: &str) -> Result<()> {
    connection.execute("DELETE FROM memory_entries_fts WHERE id = ?1", params![id])?;
    Ok(())
}

fn refresh_memory_fts_row(connection: &Connection, id: &str) -> Result<()> {
    delete_memory_fts_row(connection, id)?;
    let entry = load_memory_entry(connection, id)?;
    if memory_entry_is_searchable(&entry) {
        connection.execute(
            "INSERT INTO memory_entries_fts (id, title, content, tags) VALUES (?1, ?2, ?3, ?4)",
            params![
                entry.id,
                entry.title,
                entry.content,
                serde_json::to_string(&entry.tags)?
            ],
        )?;
    }
    Ok(())
}

fn rebuild_memory_search_index_with_connection(connection: &Connection) -> Result<()> {
    connection.execute("DELETE FROM memory_entries_fts", [])?;
    for entry in list_memory_entries_with_connection(connection)? {
        if memory_entry_is_searchable(&entry) {
            connection.execute(
                "INSERT INTO memory_entries_fts (id, title, content, tags) VALUES (?1, ?2, ?3, ?4)",
                params![
                    entry.id,
                    entry.title,
                    entry.content,
                    serde_json::to_string(&entry.tags)?
                ],
            )?;
        }
    }
    Ok(())
}

fn memory_fts_query(input: &str) -> Option<String> {
    let terms = input
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|character: char| {
                !character.is_alphanumeric() && character != '_' && character != '-'
            })
        })
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn search_memory_entries_with_connection(
    connection: &Connection,
    query: &str,
    scope_kind: Option<&str>,
    scope_id: Option<&str>,
    limit: usize,
) -> Result<Vec<MemorySearchResult>> {
    let Some(match_query) = memory_fts_query(query) else {
        return Ok(Vec::new());
    };
    let limit = limit.clamp(1, 50) as i64;
    let mut statement = connection.prepare(
        "
        SELECT memory_entries.id, bm25(memory_entries_fts) AS rank
        FROM memory_entries_fts
        JOIN memory_entries ON memory_entries.id = memory_entries_fts.id
        WHERE memory_entries_fts MATCH ?1
          AND memory_entries.enabled = 1
          AND memory_entries.status = 'accepted'
          AND (?2 IS NULL OR memory_entries.scope_kind = ?2)
          AND (?3 IS NULL OR memory_entries.scope_id = ?3)
        ORDER BY rank ASC, memory_entries.updated_at DESC, memory_entries.id ASC
        LIMIT ?4
        ",
    )?;
    let rows = statement.query_map(params![match_query, scope_kind, scope_id, limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;
    let matches = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    matches
        .into_iter()
        .map(|(id, rank)| {
            Ok(MemorySearchResult {
                entry: load_memory_entry(connection, &id)?,
                rank,
            })
        })
        .collect()
}

fn list_memory_candidates_with_connection(connection: &Connection) -> Result<Vec<MemoryCandidate>> {
    let mut statement = connection
        .prepare("SELECT id FROM memory_candidates ORDER BY status ASC, created_at DESC, id ASC")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| load_memory_candidate(connection, &id))
        .collect()
}

fn load_memory_candidate(connection: &Connection, id: &str) -> Result<MemoryCandidate> {
    connection.query_row("SELECT id, scope_kind, scope_id, session_id, turn_id_start, turn_id_end, candidate_kind, title, content, tags_json, evidence_json, reason, confidence, status, dedupe_key, accepted_memory_id, created_by, created_at, updated_at, metadata_json FROM memory_candidates WHERE id = ?1", params![id], map_memory_candidate).optional()?.ok_or_else(|| anyhow!("memory candidate {id} was not found"))
}

fn map_memory_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryCandidate> {
    Ok(MemoryCandidate {
        id: row.get(0)?,
        scope_kind: row.get(1)?,
        scope_id: row.get(2)?,
        session_id: row.get(3)?,
        turn_id_start: row.get(4)?,
        turn_id_end: row.get(5)?,
        candidate_kind: row.get(6)?,
        title: row.get(7)?,
        content: row.get(8)?,
        tags: serde_json::from_value(decode_json_value(row.get(9)?)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
        })?,
        evidence: serde_json::from_value(decode_json_value(row.get(10)?)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
        })?,
        reason: row.get(11)?,
        confidence: row.get(12)?,
        status: row.get(13)?,
        dedupe_key: row.get(14)?,
        accepted_memory_id: row.get(15)?,
        created_by: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
        metadata_json: decode_json_value(row.get(19)?)?,
    })
}

fn load_memory_entry(connection: &Connection, id: &str) -> Result<MemoryEntry> {
    connection.query_row(
        "SELECT id, scope_kind, scope_id, title, content, tags_json, enabled, status, memory_kind, source_kind, source_id, confidence, created_by, last_used_at, use_count, supersedes_id, metadata_json, created_at, updated_at FROM memory_entries WHERE id = ?1",
        params![id],
        map_memory_entry,
    ).optional()?.ok_or_else(|| anyhow!("memory entry {id} was not found"))
}

fn map_memory_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryEntry> {
    Ok(MemoryEntry {
        id: row.get(0)?,
        scope_kind: row.get(1)?,
        scope_id: row.get(2)?,
        title: row.get(3)?,
        content: row.get(4)?,
        tags: serde_json::from_value(decode_json_value(row.get(5)?)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        enabled: row.get::<_, i64>(6)? != 0,
        status: row.get(7)?,
        memory_kind: row.get(8)?,
        source_kind: row.get(9)?,
        source_id: row.get(10)?,
        confidence: row.get(11)?,
        created_by: row.get(12)?,
        last_used_at: row.get(13)?,
        use_count: row.get(14)?,
        supersedes_id: row.get(15)?,
        metadata_json: decode_json_value(row.get(16)?)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

fn list_skill_packages_with_connection(
    connection: &Connection,
) -> Result<Vec<nucleus_protocol::SkillPackageRecord>> {
    let mut statement = connection
        .prepare("SELECT id FROM skill_packages ORDER BY name ASC, version ASC, id ASC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| load_skill_package(connection, &id))
        .collect()
}

fn load_skill_package(
    connection: &Connection,
    id: &str,
) -> Result<nucleus_protocol::SkillPackageRecord> {
    connection.query_row("SELECT id, name, version, manifest_json, instructions, source_kind, source_url, source_repo_url, source_owner, source_repo, source_ref, source_parent_path, source_skill_path, source_commit, imported_at, last_checked_at, latest_source_commit, update_status, content_checksum, dirty_status, created_at, updated_at FROM skill_packages WHERE id = ?1", params![id], map_skill_package).optional()?.ok_or_else(|| anyhow!("skill package {id} was not found"))
}

fn map_skill_package(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<nucleus_protocol::SkillPackageRecord> {
    Ok(nucleus_protocol::SkillPackageRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        version: row.get(2)?,
        manifest_json: decode_json_value(row.get(3)?)?,
        instructions: row.get(4)?,
        source_kind: row.get(5)?,
        source_url: row.get(6)?,
        source_repo_url: row.get(7)?,
        source_owner: row.get(8)?,
        source_repo: row.get(9)?,
        source_ref: row.get(10)?,
        source_parent_path: row.get(11)?,
        source_skill_path: row.get(12)?,
        source_commit: row.get(13)?,
        imported_at: row.get(14)?,
        last_checked_at: row.get(15)?,
        latest_source_commit: row.get(16)?,
        update_status: row.get(17)?,
        content_checksum: row.get(18)?,
        dirty_status: row.get(19)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
    })
}

fn list_skill_installations_with_connection(
    connection: &Connection,
) -> Result<Vec<nucleus_protocol::SkillInstallationRecord>> {
    let mut statement = connection.prepare(
        "SELECT id FROM skill_installations ORDER BY scope_kind ASC, scope_id ASC, id ASC",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| load_skill_installation(connection, &id))
        .collect()
}

fn load_skill_installation(
    connection: &Connection,
    id: &str,
) -> Result<nucleus_protocol::SkillInstallationRecord> {
    connection.query_row("SELECT id, package_id, scope_kind, scope_id, enabled, pinned_version, created_at, updated_at FROM skill_installations WHERE id = ?1", params![id], map_skill_installation).optional()?.ok_or_else(|| anyhow!("skill installation {id} was not found"))
}

fn map_skill_installation(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<nucleus_protocol::SkillInstallationRecord> {
    Ok(nucleus_protocol::SkillInstallationRecord {
        id: row.get(0)?,
        package_id: row.get(1)?,
        scope_kind: row.get(2)?,
        scope_id: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        pinned_version: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn decode_string_vec_column(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Vec<String>> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn list_sessions_with_connection(connection: &Connection) -> Result<Vec<SessionSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM sessions
        WHERE scope != 'automation'
        ORDER BY updated_at DESC, created_at DESC, id DESC
        LIMIT 50
        ",
    )?;

    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load session identifiers")?;

    ids.into_iter()
        .map(|session_id| load_session_summary(connection, &session_id))
        .collect()
}

fn load_session_summary(connection: &Connection, session_id: &str) -> Result<SessionSummary> {
    let mut session = connection
        .query_row(
            "
            SELECT
                id,
                title,
                profile_id,
                profile_title,
                route_id,
                route_title,
                scope,
                project_id,
                project_title,
                project_path,
                provider,
                model,
                provider_base_url,
                provider_api_key,
                working_dir,
                working_dir_kind,
                workspace_mode, source_project_path, git_root, worktree_path, git_branch, git_base_ref, git_head, git_dirty, git_untracked_count, git_remote_tracking_branch, workspace_warnings_json,
                approval_mode,
                execution_mode,
                run_budget_mode,
                state,
                provider_session_id,
                last_error,
                last_message_excerpt,
                turn_count,
                created_at,
                updated_at
            FROM sessions
            WHERE id = ?1
            ",
            params![session_id],
            map_session_summary_row,
        )
        .optional()?
        .ok_or_else(|| anyhow!("session '{session_id}' was not found"))?;

    session.projects = load_session_projects(connection, session_id)?;
    session.project_count = session.projects.len();
    session.scope = session_scope_from_projects(&session.projects, &session.scope);
    session.run_budget = session_run_budget(connection, &session.run_budget_mode)?;

    if session.project_id.is_empty() {
        if let Some(primary) = session.projects.iter().find(|project| project.is_primary) {
            session.project_id = primary.id.clone();
            session.project_title = primary.title.clone();
            session.project_path = primary.absolute_path.clone();
        }
    }

    Ok(session)
}

fn map_session_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
    Ok(SessionSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        profile_id: row.get(2)?,
        profile_title: row.get(3)?,
        route_id: row.get(4)?,
        route_title: row.get(5)?,
        scope: row.get(6)?,
        project_id: row.get(7)?,
        project_title: row.get(8)?,
        project_path: row.get(9)?,
        provider: row.get(10)?,
        model: row.get(11)?,
        provider_base_url: row.get(12)?,
        provider_api_key: row.get(13)?,
        working_dir: row.get(14)?,
        working_dir_kind: row.get(15)?,
        workspace_mode: row.get(16)?,
        source_project_path: row.get(17)?,
        git_root: row.get(18)?,
        worktree_path: row.get(19)?,
        git_branch: row.get(20)?,
        git_base_ref: row.get(21)?,
        git_head: row.get(22)?,
        git_dirty: row.get::<_, i64>(23)? != 0,
        git_untracked_count: row.get::<_, i64>(24)? as usize,
        git_remote_tracking_branch: row.get(25)?,
        workspace_warnings: serde_json::from_str(&row.get::<_, String>(26)?).unwrap_or_default(),
        approval_mode: row.get(27)?,
        execution_mode: row.get(28)?,
        run_budget_mode: row.get(29)?,
        run_budget: RunBudgetSummary::default(),
        project_count: 0,
        projects: Vec::new(),
        state: row.get(30)?,
        provider_session_id: row.get(31)?,
        last_error: row.get(32)?,
        last_message_excerpt: row.get(33)?,
        turn_count: row.get(34)?,
        created_at: row.get(35)?,
        updated_at: row.get(36)?,
    })
}

fn load_session_detail(connection: &Connection, session_id: &str) -> Result<SessionDetail> {
    let session = load_session_summary(connection, session_id)?;
    let mut statement = connection.prepare(
        "
        SELECT id, session_id, role, content, images_json, created_at
        FROM session_turns
        WHERE session_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;

    let rows = statement.query_map(params![session_id], |row| {
        Ok(SessionTurn {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            images: decode_session_turn_images(row.get::<_, String>(4)?)?,
            created_at: row.get(5)?,
        })
    })?;

    let turns = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load session turns")?;

    Ok(SessionDetail { session, turns })
}

fn load_session_turn(connection: &Connection, turn_id: &str) -> Result<SessionTurn> {
    connection
        .query_row(
            "
            SELECT id, session_id, role, content, images_json, created_at
            FROM session_turns
            WHERE id = ?1
            ",
            params![turn_id],
            |row| {
                Ok(SessionTurn {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    images: decode_session_turn_images(row.get::<_, String>(4)?)?,
                    created_at: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("session turn '{turn_id}' was not found"))
}

fn decode_session_turn_images(value: String) -> rusqlite::Result<Vec<SessionTurnImage>> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn load_session_projects(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<SessionProjectSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT
            projects.id,
            projects.title,
            projects.slug,
            projects.relative_path,
            projects.absolute_path,
            session_projects.is_primary
        FROM session_projects
        INNER JOIN projects ON projects.id = session_projects.project_id
        WHERE session_projects.session_id = ?1
        ORDER BY session_projects.is_primary DESC, session_projects.sort_order ASC, projects.relative_path ASC
        ",
    )?;

    let rows = statement.query_map(params![session_id], |row| {
        Ok(SessionProjectSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            slug: row.get(2)?,
            relative_path: row.get(3)?,
            absolute_path: row.get(4)?,
            is_primary: row.get::<_, i64>(5)? != 0,
        })
    })?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load session projects")
}

fn session_scope_from_projects(projects: &[SessionProjectSummary], stored_scope: &str) -> String {
    if stored_scope == "automation" {
        return "automation".to_string();
    }

    if projects.is_empty() {
        if stored_scope.trim().is_empty() {
            "ad_hoc".to_string()
        } else {
            stored_scope.to_string()
        }
    } else if projects.len() == 1 {
        "project".to_string()
    } else {
        "multi_project".to_string()
    }
}

fn list_audit_events_with_connection(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AuditEvent>> {
    let mut statement = connection.prepare(
        "
        SELECT id, kind, target, status, summary, detail, created_at
        FROM audit_events
        ORDER BY id DESC
        LIMIT ?1
        ",
    )?;

    let rows = statement.query_map(params![limit as i64], map_audit_event)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load audit events")
}

fn load_audit_event(connection: &Connection, event_id: i64) -> Result<AuditEvent> {
    connection
        .query_row(
            "
            SELECT id, kind, target, status, summary, detail, created_at
            FROM audit_events
            WHERE id = ?1
            ",
            params![event_id],
            map_audit_event,
        )
        .optional()?
        .ok_or_else(|| anyhow!("audit event '{event_id}' was not found"))
}

fn map_audit_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    Ok(AuditEvent {
        id: row.get(0)?,
        kind: row.get(1)?,
        target: row.get(2)?,
        status: row.get(3)?,
        summary: row.get(4)?,
        detail: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn load_instance_log(connection: &Connection, log_id: i64) -> Result<InstanceLogEntry> {
    connection
        .query_row(
            "
            SELECT id, timestamp, level, category, source, event, message, related_ids_json, metadata_json
            FROM instance_logs
            WHERE id = ?1
            ",
            params![log_id],
            map_instance_log,
        )
        .optional()?
        .ok_or_else(|| anyhow!("instance log '{log_id}' was not found"))
}

fn list_instance_logs_with_connection(
    connection: &Connection,
    category: Option<&str>,
    level: Option<&str>,
    before: Option<(i64, i64)>,
    limit: usize,
) -> Result<Vec<InstanceLogEntry>> {
    let mut sql = String::from(
        "
        SELECT id, timestamp, level, category, source, event, message, related_ids_json, metadata_json
        FROM instance_logs
        WHERE 1 = 1
        ",
    );
    let mut values: Vec<rusqlite::types::Value> = Vec::new();

    if let Some(category) = category.filter(|value| !value.trim().is_empty()) {
        sql.push_str(" AND category = ? ");
        values.push(category.trim().to_string().into());
    }

    if let Some(level) = level.filter(|value| !value.trim().is_empty()) {
        sql.push_str(" AND level = ? ");
        values.push(level.trim().to_string().into());
    }

    if let Some((before_timestamp, before_id)) = before {
        sql.push_str(" AND (timestamp < ? OR (timestamp = ? AND id < ?)) ");
        values.push(before_timestamp.into());
        values.push(before_timestamp.into());
        values.push(before_id.into());
    }

    sql.push_str(" ORDER BY timestamp DESC, id DESC LIMIT ? ");
    values.push((limit as i64).into());

    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(values), map_instance_log)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load instance logs")
}

fn list_instance_log_categories_with_connection(
    connection: &Connection,
) -> Result<Vec<InstanceLogCategorySummary>> {
    let mut statement = connection.prepare(
        "
        SELECT category, COUNT(*)
        FROM instance_logs
        GROUP BY category
        ORDER BY category ASC
        ",
    )?;
    let rows = statement.query_map([], |row| {
        let count: i64 = row.get(1)?;
        Ok(InstanceLogCategorySummary {
            category: row.get(0)?,
            count: usize::try_from(count).unwrap_or(0),
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load instance log categories")
}

fn prune_instance_logs_with_connection(
    connection: &Connection,
    max_age_secs: i64,
    max_rows: usize,
) -> Result<usize> {
    let mut removed = 0;

    if max_age_secs > 0 {
        removed += connection.execute(
            "DELETE FROM instance_logs WHERE timestamp < unixepoch() - ?1",
            params![max_age_secs],
        )?;
    }

    if max_rows > 0 {
        removed += connection.execute(
            "
            DELETE FROM instance_logs
            WHERE id NOT IN (
                SELECT id FROM instance_logs
                ORDER BY timestamp DESC, id DESC
                LIMIT ?1
            )
            ",
            params![max_rows as i64],
        )?;
    }

    Ok(removed)
}

fn map_instance_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstanceLogEntry> {
    Ok(InstanceLogEntry {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        level: row.get(2)?,
        category: row.get(3)?,
        source: row.get(4)?,
        event: row.get(5)?,
        message: row.get(6)?,
        related_ids: decode_json_value(row.get(7)?)?,
        metadata: decode_json_value(row.get(8)?)?,
    })
}

fn append_instance_log_jsonl(logs_dir: &Path, entry: &InstanceLogEntry) -> Result<()> {
    fs::create_dir_all(logs_dir)
        .with_context(|| format!("failed to create logs directory '{}'", logs_dir.display()))?;
    let path = logs_dir.join(INSTANCE_LOG_JSONL_FILE_NAME);
    rotate_instance_log_jsonl(&path)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open log file '{}'", path.display()))?;
    serde_json::to_writer(&mut file, entry).context("failed to encode JSONL instance log")?;
    file.write_all(b"\n")
        .context("failed to write JSONL instance log")?;
    Ok(())
}

fn rotate_instance_log_jsonl(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() < INSTANCE_LOG_JSONL_MAX_BYTES {
        return Ok(());
    }

    for index in (1..=INSTANCE_LOG_JSONL_ROTATED_FILES).rev() {
        let rotated = rotated_instance_log_path(path, index);
        if index == INSTANCE_LOG_JSONL_ROTATED_FILES {
            let _ = fs::remove_file(rotated);
        } else {
            let next = rotated_instance_log_path(path, index + 1);
            if rotated.exists() {
                fs::rename(&rotated, &next).with_context(|| {
                    format!(
                        "failed to rotate log file '{}' to '{}'",
                        rotated.display(),
                        next.display()
                    )
                })?;
            }
        }
    }

    fs::rename(path, rotated_instance_log_path(path, 1))
        .with_context(|| format!("failed to rotate log file '{}'", path.display()))?;
    Ok(())
}

fn rotated_instance_log_path(path: &Path, index: usize) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), index))
}

fn list_jobs_for_session_with_connection(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<JobSummary>> {
    ensure_session_exists(connection, session_id)?;
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM jobs
        WHERE session_id = ?1 AND parent_job_id IS NULL
        ORDER BY created_at DESC, id DESC
        ",
    )?;
    let rows = statement.query_map(params![session_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load job ids")?
        .into_iter()
        .map(|job_id| load_job_summary(connection, &job_id))
        .collect()
}

fn list_jobs_for_template_with_connection(
    connection: &Connection,
    template_id: &str,
    limit: usize,
) -> Result<Vec<JobSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM jobs
        WHERE template_id = ?1 AND parent_job_id IS NULL
        ORDER BY created_at DESC, id DESC
        LIMIT ?2
        ",
    )?;
    let rows = statement.query_map(params![template_id, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load playbook job ids")?
        .into_iter()
        .map(|job_id| load_job_summary(connection, &job_id))
        .collect()
}

fn list_jobs_for_template_by_state_with_connection(
    connection: &Connection,
    template_id: &str,
    states: &[&str],
) -> Result<Vec<JobSummary>> {
    if states.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = (0..states.len())
        .map(|index| format!("?{}", index + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "
        SELECT id
        FROM jobs
        WHERE template_id = ?1 AND parent_job_id IS NULL AND state IN ({placeholders})
        ORDER BY created_at DESC, id DESC
        "
    );
    let params = std::iter::once(template_id)
        .chain(states.iter().copied())
        .collect::<Vec<_>>();
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load playbook jobs by state")?
        .into_iter()
        .map(|job_id| load_job_summary(connection, &job_id))
        .collect()
}

fn list_jobs_by_state_with_connection(
    connection: &Connection,
    states: &[&str],
) -> Result<Vec<JobSummary>> {
    if states.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = (0..states.len())
        .map(|index| format!("?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "
        SELECT id
        FROM jobs
        WHERE state IN ({placeholders})
        ORDER BY created_at ASC, id ASC
        "
    );
    let params = rusqlite::params_from_iter(states.iter().copied());
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load jobs by state")?
        .into_iter()
        .map(|job_id| load_job_summary(connection, &job_id))
        .collect()
}

fn list_pending_approvals_with_connection(
    connection: &Connection,
) -> Result<Vec<ApprovalRequestSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM approval_requests
        WHERE state = 'pending'
        ORDER BY requested_at DESC, id DESC
        ",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load pending approval ids")?
        .into_iter()
        .map(|approval_id| load_approval_request_summary(connection, &approval_id))
        .collect()
}

fn load_job_summary(connection: &Connection, job_id: &str) -> Result<JobSummary> {
    let mut job = connection
        .query_row(
            "
            SELECT
                id,
                session_id,
                parent_job_id,
                template_id,
                title,
                purpose,
                trigger_kind,
                state,
                requested_by,
                prompt_excerpt,
                root_worker_id,
                visible_turn_id,
                result_summary,
                last_error,
                created_at,
                updated_at
            FROM jobs
            WHERE id = ?1
            ",
            params![job_id],
            map_job_summary_row,
        )
        .optional()?
        .ok_or_else(|| anyhow!("job '{job_id}' was not found"))?;

    job.worker_count = count_for_job(connection, "job_workers", job_id)?;
    job.pending_approval_count = count_pending_approvals_for_job(connection, job_id)?;
    job.artifact_count = count_for_job(connection, "job_artifacts", job_id)?;
    Ok(job)
}

fn count_jobs_for_template(connection: &Connection, template_id: &str) -> Result<usize> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE template_id = ?1 AND parent_job_id IS NULL",
            params![template_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count.max(0) as usize)
        .context("failed to count playbook jobs")
}

fn map_job_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobSummary> {
    Ok(JobSummary {
        id: row.get(0)?,
        session_id: row.get(1)?,
        parent_job_id: row.get(2)?,
        template_id: row.get(3)?,
        title: row.get(4)?,
        purpose: row.get(5)?,
        trigger_kind: row.get(6)?,
        state: row.get(7)?,
        requested_by: row.get(8)?,
        prompt_excerpt: row.get(9)?,
        root_worker_id: row.get(10)?,
        visible_turn_id: row.get(11)?,
        result_summary: row.get(12)?,
        last_error: row.get(13)?,
        worker_count: 0,
        pending_approval_count: 0,
        artifact_count: 0,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn count_for_job(connection: &Connection, table: &str, job_id: &str) -> Result<usize> {
    connection
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE job_id = ?1"),
            params![job_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count.max(0) as usize)
        .context("failed to count job rows")
}

fn count_pending_approvals_for_job(connection: &Connection, job_id: &str) -> Result<usize> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM approval_requests WHERE job_id = ?1 AND state = 'pending'",
            params![job_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count.max(0) as usize)
        .context("failed to count pending approvals")
}

fn load_job_detail(connection: &Connection, job_id: &str) -> Result<JobDetail> {
    let job = load_job_summary(connection, job_id)?;
    let workers = load_workers_for_job(connection, job_id)?;
    let child_jobs = load_child_jobs(connection, job_id)?;
    let tool_calls = load_tool_calls_for_job(connection, job_id)?;
    let approvals = load_approval_requests_for_job(connection, job_id)?;
    let artifacts = load_artifacts_for_job(connection, job_id)?;
    let command_sessions = load_command_sessions_for_job(connection, job_id)?;
    let events = load_job_events_for_job(connection, job_id)?;

    Ok(JobDetail {
        job,
        workers,
        child_jobs,
        tool_calls,
        approvals,
        artifacts,
        command_sessions,
        events,
    })
}

fn load_child_jobs(connection: &Connection, parent_job_id: &str) -> Result<Vec<JobSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM jobs
        WHERE parent_job_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;
    let rows = statement.query_map(params![parent_job_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load child job ids")?
        .into_iter()
        .map(|job_id| load_job_summary(connection, &job_id))
        .collect()
}

fn load_workers_for_job(connection: &Connection, job_id: &str) -> Result<Vec<WorkerSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM job_workers
        WHERE job_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;
    let rows = statement.query_map(params![job_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load worker ids")?
        .into_iter()
        .map(|worker_id| load_worker_summary(connection, &worker_id))
        .collect()
}

fn load_worker_summary(connection: &Connection, worker_id: &str) -> Result<WorkerSummary> {
    let mut worker = connection
        .query_row(
            "
            SELECT
                id,
                job_id,
                parent_worker_id,
                title,
                lane,
                state,
                provider,
                model,
                provider_base_url,
                provider_api_key,
                provider_session_id,
                working_dir,
                read_roots_json,
                write_roots_json,
                max_steps,
                max_tool_calls,
                max_wall_clock_secs,
                step_count,
                tool_call_count,
                last_error,
                created_at,
                updated_at
            FROM job_workers
            WHERE id = ?1
            ",
            params![worker_id],
            |row| {
                Ok(WorkerSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    parent_worker_id: row.get(2)?,
                    title: row.get(3)?,
                    lane: row.get(4)?,
                    state: row.get(5)?,
                    provider: row.get(6)?,
                    model: row.get(7)?,
                    provider_base_url: row.get(8)?,
                    provider_api_key: row.get(9)?,
                    provider_session_id: row.get(10)?,
                    working_dir: row.get(11)?,
                    read_roots: decode_string_list(row.get::<_, String>(12)?)?,
                    write_roots: decode_string_list(row.get::<_, String>(13)?)?,
                    max_steps: row.get::<_, i64>(14)?.max(0) as usize,
                    max_tool_calls: row.get::<_, i64>(15)?.max(0) as usize,
                    max_wall_clock_secs: row.get::<_, i64>(16)?.max(0) as u64,
                    step_count: row.get::<_, i64>(17)?.max(0) as usize,
                    tool_call_count: row.get::<_, i64>(18)?.max(0) as usize,
                    last_error: row.get(19)?,
                    capabilities: Vec::new(),
                    created_at: row.get(20)?,
                    updated_at: row.get(21)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("worker '{worker_id}' was not found"))?;
    worker.capabilities = load_worker_capabilities(connection, worker_id)?;
    Ok(worker)
}

fn load_worker_capabilities(
    connection: &Connection,
    worker_id: &str,
) -> Result<Vec<ToolCapabilitySummary>> {
    let mut statement = connection.prepare(
        "
        SELECT
            tool_id,
            summary,
            approval_mode,
            risk_level,
            side_effect_level,
            timeout_secs,
            max_output_bytes,
            supports_streaming,
            concurrency_group,
            scope_kind
        FROM tool_capability_grants
        WHERE worker_id = ?1
        ORDER BY tool_id ASC
        ",
    )?;
    let rows = statement.query_map(params![worker_id], |row| {
        Ok(ToolCapabilitySummary {
            tool_id: row.get(0)?,
            summary: row.get(1)?,
            approval_mode: row.get(2)?,
            risk_level: row.get(3)?,
            side_effect_level: row.get(4)?,
            timeout_secs: row.get::<_, i64>(5)?.max(0) as u64,
            max_output_bytes: row.get::<_, i64>(6)?.max(0) as usize,
            supports_streaming: row.get::<_, i64>(7)? != 0,
            concurrency_group: row.get(8)?,
            scope_kind: row.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load worker capabilities")
}

fn load_tool_calls_for_job(connection: &Connection, job_id: &str) -> Result<Vec<ToolCallSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM tool_calls
        WHERE job_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;
    let rows = statement.query_map(params![job_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load tool call ids")?
        .into_iter()
        .map(|call_id| load_tool_call_summary(connection, &call_id))
        .collect()
}

fn load_tool_call_summary(connection: &Connection, tool_call_id: &str) -> Result<ToolCallSummary> {
    connection
        .query_row(
            "
            SELECT
                id,
                job_id,
                worker_id,
                tool_id,
                status,
                summary,
                args_json,
                result_json,
                policy_decision_json,
                artifact_ids_json,
                error_class,
                error_detail,
                created_at,
                started_at,
                completed_at
            FROM tool_calls
            WHERE id = ?1
            ",
            params![tool_call_id],
            |row| {
                let result_json = row.get::<_, Option<String>>(7)?;
                let policy_json = row.get::<_, Option<String>>(8)?;
                Ok(ToolCallSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    worker_id: row.get(2)?,
                    tool_id: row.get(3)?,
                    status: row.get(4)?,
                    summary: row.get(5)?,
                    args_json: decode_json_value(row.get::<_, String>(6)?)?,
                    result_json: result_json.map(decode_json_value).transpose()?,
                    policy_decision: policy_json.map(decode_policy_decision).transpose()?,
                    artifact_ids: decode_string_list(row.get::<_, String>(9)?)?,
                    error_class: row.get(10)?,
                    error_detail: row.get(11)?,
                    created_at: row.get(12)?,
                    started_at: row.get(13)?,
                    completed_at: row.get(14)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("tool call '{tool_call_id}' was not found"))
}

fn load_approval_requests_for_job(
    connection: &Connection,
    job_id: &str,
) -> Result<Vec<ApprovalRequestSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM approval_requests
        WHERE job_id = ?1
        ORDER BY requested_at DESC, id DESC
        ",
    )?;
    let rows = statement.query_map(params![job_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load approval ids")?
        .into_iter()
        .map(|approval_id| load_approval_request_summary(connection, &approval_id))
        .collect()
}

fn load_approval_request_summary(
    connection: &Connection,
    approval_id: &str,
) -> Result<ApprovalRequestSummary> {
    connection
        .query_row(
            "
            SELECT
                id,
                job_id,
                worker_id,
                tool_call_id,
                state,
                risk_level,
                summary,
                detail,
                diff_preview,
                policy_decision_json,
                resolution_note,
                resolved_by,
                requested_at,
                resolved_at
            FROM approval_requests
            WHERE id = ?1
            ",
            params![approval_id],
            |row| {
                Ok(ApprovalRequestSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    worker_id: row.get(2)?,
                    tool_call_id: row.get(3)?,
                    state: row.get(4)?,
                    risk_level: row.get(5)?,
                    summary: row.get(6)?,
                    detail: row.get(7)?,
                    diff_preview: row.get(8)?,
                    policy_decision: decode_policy_decision(row.get::<_, String>(9)?)?,
                    resolution_note: row.get(10)?,
                    resolved_by: row.get(11)?,
                    requested_at: row.get(12)?,
                    resolved_at: row.get(13)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("approval request '{approval_id}' was not found"))
}

fn load_artifacts_for_job(connection: &Connection, job_id: &str) -> Result<Vec<ArtifactSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM job_artifacts
        WHERE job_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;
    let rows = statement.query_map(params![job_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load artifact ids")?
        .into_iter()
        .map(|artifact_id| load_artifact_summary(connection, &artifact_id))
        .collect()
}

fn load_artifact_summary(connection: &Connection, artifact_id: &str) -> Result<ArtifactSummary> {
    connection
        .query_row(
            "
            SELECT
                id,
                job_id,
                worker_id,
                tool_call_id,
                command_session_id,
                kind,
                title,
                path,
                mime_type,
                size_bytes,
                preview_text,
                created_at
            FROM job_artifacts
            WHERE id = ?1
            ",
            params![artifact_id],
            |row| {
                Ok(ArtifactSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    worker_id: row.get(2)?,
                    tool_call_id: row.get(3)?,
                    command_session_id: row.get(4)?,
                    kind: row.get(5)?,
                    title: row.get(6)?,
                    path: row.get(7)?,
                    mime_type: row.get(8)?,
                    size_bytes: row.get::<_, i64>(9)?.max(0) as u64,
                    preview_text: row.get(10)?,
                    created_at: row.get(11)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("artifact '{artifact_id}' was not found"))
}

fn load_command_sessions_for_job(
    connection: &Connection,
    job_id: &str,
) -> Result<Vec<CommandSessionSummary>> {
    let mut statement = connection.prepare(
        "
        SELECT id
        FROM command_sessions
        WHERE job_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;
    let rows = statement.query_map(params![job_id], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load command session ids")?
        .into_iter()
        .map(|command_session_id| load_command_session_summary(connection, &command_session_id))
        .collect()
}

fn load_command_session_summary(
    connection: &Connection,
    command_session_id: &str,
) -> Result<CommandSessionSummary> {
    connection
        .query_row(
            "
            SELECT
                id,
                job_id,
                worker_id,
                tool_call_id,
                mode,
                title,
                state,
                command,
                args_json,
                cwd,
                session_id,
                project_id,
                worktree_path,
                branch,
                port,
                network_policy,
                timeout_secs,
                output_limit_bytes,
                last_error,
                exit_code,
                stdout_artifact_id,
                stderr_artifact_id,
                started_at,
                completed_at,
                created_at,
                updated_at
            FROM command_sessions
            WHERE id = ?1
            ",
            params![command_session_id],
            |row| {
                Ok(CommandSessionSummary {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    worker_id: row.get(2)?,
                    tool_call_id: row.get(3)?,
                    mode: row.get(4)?,
                    title: row.get(5)?,
                    state: row.get(6)?,
                    command: row.get(7)?,
                    args: decode_string_list(row.get::<_, String>(8)?)?,
                    cwd: row.get(9)?,
                    session_id: row.get(10)?,
                    project_id: row.get(11)?,
                    worktree_path: row.get(12)?,
                    branch: row.get(13)?,
                    port: row
                        .get::<_, Option<i64>>(14)?
                        .map(|port| port.max(0) as u16),
                    network_policy: row.get(15)?,
                    timeout_secs: row.get::<_, i64>(16)?.max(0) as u64,
                    output_limit_bytes: row.get::<_, i64>(17)?.max(0) as usize,
                    last_error: row.get(18)?,
                    exit_code: row.get(19)?,
                    stdout_artifact_id: row.get(20)?,
                    stderr_artifact_id: row.get(21)?,
                    started_at: row.get(22)?,
                    completed_at: row.get(23)?,
                    created_at: row.get(24)?,
                    updated_at: row.get(25)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("command session '{command_session_id}' was not found"))
}

fn list_command_sessions_by_state_with_connection(
    connection: &Connection,
    states: &[&str],
) -> Result<Vec<CommandSessionSummary>> {
    if states.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = (0..states.len())
        .map(|index| format!("?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection.prepare(&format!(
        "
        SELECT id
        FROM command_sessions
        WHERE state IN ({placeholders})
        ORDER BY updated_at DESC, id DESC
        "
    ))?;
    let rows = statement.query_map(rusqlite::params_from_iter(states.iter().copied()), |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load command sessions by state")?
        .into_iter()
        .map(|command_session_id| load_command_session_summary(connection, &command_session_id))
        .collect()
}

fn load_job_events_for_job(connection: &Connection, job_id: &str) -> Result<Vec<JobEvent>> {
    let mut statement = connection.prepare(
        "
        SELECT id, job_id, worker_id, event_type, status, summary, detail, data_json, created_at
        FROM job_events
        WHERE job_id = ?1
        ORDER BY created_at ASC, id ASC
        ",
    )?;
    let rows = statement.query_map(params![job_id], map_job_event)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to load job events")
}

fn load_job_event(connection: &Connection, event_id: i64) -> Result<JobEvent> {
    connection
        .query_row(
            "
            SELECT id, job_id, worker_id, event_type, status, summary, detail, data_json, created_at
            FROM job_events
            WHERE id = ?1
            ",
            params![event_id],
            map_job_event,
        )
        .optional()?
        .ok_or_else(|| anyhow!("job event '{event_id}' was not found"))
}

fn map_job_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobEvent> {
    Ok(JobEvent {
        id: row.get(0)?,
        job_id: row.get(1)?,
        worker_id: row.get(2)?,
        event_type: row.get(3)?,
        status: row.get(4)?,
        summary: row.get(5)?,
        detail: row.get(6)?,
        data_json: decode_json_value(row.get::<_, String>(7)?)?,
        created_at: row.get(8)?,
    })
}

fn decode_string_list(value: String) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn decode_json_value(value: String) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn decode_policy_decision(value: String) -> rusqlite::Result<PolicyDecisionSummary> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn policy_decision_to_json(record: &PolicyDecisionRecord) -> Result<String> {
    serde_json::to_string(&PolicyDecisionSummary {
        decision: record.decision.clone(),
        reason: record.reason.clone(),
        matched_rule: record.matched_rule.clone(),
        scope_kind: record.scope_kind.clone(),
        risk_level: record.risk_level.clone(),
    })
    .context("failed to serialize policy decision")
}

fn policy_decision_summary_to_json(summary: &PolicyDecisionSummary) -> Result<String> {
    serde_json::to_string(summary).context("failed to serialize policy decision summary")
}

fn ensure_session_exists(connection: &Connection, session_id: &str) -> Result<()> {
    let found = connection
        .query_row(
            "SELECT 1 FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if found.is_none() {
        bail!("session '{session_id}' was not found");
    }

    Ok(())
}

fn ensure_job_exists(connection: &Connection, job_id: &str) -> Result<()> {
    let found = connection
        .query_row("SELECT 1 FROM jobs WHERE id = ?1", params![job_id], |row| {
            row.get::<_, i64>(0)
        })
        .optional()?;
    if found.is_none() {
        bail!("job '{job_id}' was not found");
    }
    Ok(())
}

fn ensure_worker_exists(connection: &Connection, worker_id: &str) -> Result<()> {
    let found = connection
        .query_row(
            "SELECT 1 FROM job_workers WHERE id = ?1",
            params![worker_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if found.is_none() {
        bail!("worker '{worker_id}' was not found");
    }
    Ok(())
}

fn ensure_tool_call_exists(connection: &Connection, tool_call_id: &str) -> Result<()> {
    let found = connection
        .query_row(
            "SELECT 1 FROM tool_calls WHERE id = ?1",
            params![tool_call_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if found.is_none() {
        bail!("tool call '{tool_call_id}' was not found");
    }
    Ok(())
}

fn ensure_command_session_exists(connection: &Connection, command_session_id: &str) -> Result<()> {
    let found = connection
        .query_row(
            "SELECT 1 FROM command_sessions WHERE id = ?1",
            params![command_session_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if found.is_none() {
        bail!("command session '{command_session_id}' was not found");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredProject {
    id: String,
    title: String,
    slug: String,
    relative_path: String,
    absolute_path: String,
}

fn discover_projects(root: &Path) -> Result<Vec<DiscoveredProject>> {
    let mut projects = Vec::new();
    discover_projects_recursive(root, root, 0, &mut projects)?;
    projects.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    projects.dedup_by(|left, right| left.relative_path == right.relative_path);
    Ok(projects)
}

fn discover_projects_recursive(
    root: &Path,
    current: &Path,
    depth: usize,
    projects: &mut Vec<DiscoveredProject>,
) -> Result<()> {
    if depth > 3 {
        return Ok(());
    }

    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(anyhow!(
                "failed to read workspace directory '{}': {error}",
                current.display()
            ));
        }
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !path.is_dir() || is_ignored_project_dir(&name) {
            continue;
        }

        if is_project_dir(&path) {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let title = path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| relative.clone());
            let slug = slugify(&relative);

            projects.push(DiscoveredProject {
                id: slug.clone(),
                title,
                slug,
                relative_path: relative,
                absolute_path: path.display().to_string(),
            });
            continue;
        }

        discover_projects_recursive(root, &path, depth + 1, projects)?;
    }

    Ok(())
}

fn is_project_dir(path: &Path) -> bool {
    [
        ".git",
        "package.json",
        "Cargo.toml",
        "AGENTS.md",
        ".a0proj",
        "README.md",
    ]
    .iter()
    .any(|marker| path.join(marker).exists())
}

fn is_ignored_project_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".next" | "build" | "dist" | ".svelte-kit"
    )
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;

    for character in value.chars() {
        let lower = character.to_ascii_lowercase();

        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

fn excerpt(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();

    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::{
        env, fs,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn persists_and_lists_audit_events() {
        let state_dir = test_state_dir("audit-events");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let first = store
            .append_audit_event(AuditEventRecord {
                kind: "session.created".to_string(),
                target: "session:test-1".to_string(),
                status: "success".to_string(),
                summary: "Created a test session.".to_string(),
                detail: "provider=claude model=sonnet".to_string(),
            })
            .expect("first audit event should persist");
        let second = store
            .append_audit_event(AuditEventRecord {
                kind: "action.executed".to_string(),
                target: "action:runtime.refresh".to_string(),
                status: "success".to_string(),
                summary: "Refreshed runtime health.".to_string(),
                detail: "count=4".to_string(),
            })
            .expect("second audit event should persist");

        let recent = store
            .list_audit_events(10)
            .expect("audit events should load");

        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, second.id);
        assert_eq!(recent[0].summary, "Refreshed runtime health.");
        assert_eq!(recent[1].id, first.id);
        assert_eq!(recent[1].target, "session:test-1");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn persists_lists_and_prunes_instance_logs() {
        let state_dir = test_state_dir("instance-logs");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_secs() as i64;

        let first = store
            .append_instance_log(InstanceLogRecord {
                timestamp: now - 1,
                level: "info".to_string(),
                category: "system".to_string(),
                source: "test".to_string(),
                event: "daemon.started".to_string(),
                message: "Started".to_string(),
                related_ids: serde_json::json!({"session_id": "session-1"}),
                metadata: serde_json::json!({"safe": true}),
            })
            .expect("first log should persist");
        let second = store
            .append_instance_log(InstanceLogRecord {
                timestamp: now,
                level: "error".to_string(),
                category: "job".to_string(),
                source: "test".to_string(),
                event: "job.failed".to_string(),
                message: "Failed".to_string(),
                related_ids: serde_json::json!({"job_id": "job-1"}),
                metadata: serde_json::json!({}),
            })
            .expect("second log should persist");

        assert!(first.id < second.id);
        assert!(state_dir.join("logs/events.jsonl").is_file());

        let job_logs = store
            .list_instance_logs(Some("job"), None, None, 10)
            .expect("job logs should list");
        assert_eq!(job_logs.len(), 1);
        assert_eq!(job_logs[0].event, "job.failed");

        let categories = store
            .list_instance_log_categories()
            .expect("categories should list");
        assert_eq!(categories.len(), 2);

        let removed = store.prune_instance_logs(0, 1).expect("logs should prune");
        assert!(removed >= 1);
        let remaining = store
            .list_instance_logs(None, None, None, 10)
            .expect("remaining logs should list");
        assert_eq!(remaining.len(), 1);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn paginates_instance_logs_with_same_second_tiebreaker() {
        let state_dir = test_state_dir("instance-logs-pagination");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_secs() as i64;

        for index in 0..3 {
            store
                .append_instance_log(InstanceLogRecord {
                    timestamp,
                    level: "info".to_string(),
                    category: "system".to_string(),
                    source: "test".to_string(),
                    event: format!("event.{index}"),
                    message: "same second".to_string(),
                    related_ids: serde_json::json!({}),
                    metadata: serde_json::json!({}),
                })
                .expect("log should persist");
        }

        let first_page = store
            .list_instance_logs(None, None, None, 2)
            .expect("first page should list");
        assert_eq!(first_page.len(), 2);
        assert_eq!(first_page[0].event, "event.2");
        assert_eq!(first_page[1].event, "event.1");

        let cursor = first_page.last().expect("cursor record should exist");
        let second_page = store
            .list_instance_logs(None, None, Some((cursor.timestamp, cursor.id)), 2)
            .expect("second page should list");
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].event, "event.0");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn vault_records_persist_metadata_and_delete_cascades_policy_usage_rows() {
        let state_dir = test_state_dir("vault-records");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let state = store
            .upsert_vault_state(&VaultStateRecord {
                id: "default".to_string(),
                version: 1,
                vault_id: "vault-1".to_string(),
                status: "locked".to_string(),
                kdf_algorithm: "argon2id".to_string(),
                kdf_params_json:
                    r#"{"memory_kib":256,"time_cost":1,"parallelism":1,"output_len":32}"#
                        .to_string(),
                salt: b"random-salt-placeholder".to_vec(),
                cipher: "xchacha20poly1305".to_string(),
                encrypted_root_check: b"encrypted-root-check".to_vec(),
                root_check_nonce: b"root-check-nonce-24-bytes".to_vec(),
                created_at: 0,
                updated_at: 0,
            })
            .expect("vault state should persist");
        assert_eq!(state.vault_id, "vault-1");
        assert_eq!(state.kdf_algorithm, "argon2id");

        let scope = store
            .upsert_vault_scope_key(&VaultScopeKeyRecord {
                id: "scope-key-1".to_string(),
                vault_id: "vault-1".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                encrypted_key: b"encrypted-scope-key".to_vec(),
                nonce: b"scope-key-nonce-24-bytes".to_vec(),
                aad: "nucleus:vault:v1:vault-1:scope-key:workspace:workspace:1".to_string(),
                key_version: 1,
                created_at: 0,
                rotated_at: None,
            })
            .expect("scope key should persist");

        store
            .upsert_vault_secret(&VaultSecretRecord {
                id: "secret-1".to_string(),
                scope_key_id: scope.id,
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                name: "API_TOKEN".to_string(),
                description: "API token".to_string(),
                ciphertext: b"encrypted-secret-value".to_vec(),
                nonce: b"secret-nonce-24-bytes".to_vec(),
                aad: "nucleus:vault:v1:vault-1:workspace:workspace:secret-1:API_TOKEN:1"
                    .to_string(),
                version: 1,
                created_at: 0,
                updated_at: 0,
                last_used_at: None,
            })
            .expect("secret should persist");

        {
            let connection = store.connection.lock().expect("storage mutex poisoned");
            connection
                .execute(
                    "INSERT INTO vault_secret_policies (id, secret_id, consumer_kind, consumer_id, permission, approval_mode) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params!["policy-1", "secret-1", "mcp", "server-1", "read", "allow"],
                )
                .expect("policy should insert");
            connection
                .execute(
                    "INSERT INTO vault_secret_usages (id, secret_id, consumer_kind, consumer_id, purpose) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params!["usage-1", "secret-1", "mcp", "server-1", "auth"],
                )
                .expect("usage should insert");
        }

        let listed = store
            .list_vault_secrets(Some("workspace"), Some("workspace"))
            .expect("secrets should list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "API_TOKEN");
        assert_eq!(listed[0].ciphertext, b"encrypted-secret-value");

        drop(store);
        let store = StateStore::initialize_at(&state_dir).expect("store should reopen");
        assert_eq!(
            store
                .load_vault_state()
                .expect("state should load")
                .expect("state should exist")
                .vault_id,
            "vault-1"
        );
        assert_eq!(
            store
                .load_vault_secret("secret-1")
                .expect("secret should load")
                .ciphertext,
            b"encrypted-secret-value"
        );

        store
            .delete_vault_secret("secret-1")
            .expect("secret should delete");
        assert!(store.load_vault_secret("secret-1").is_err());
        let connection = store.connection.lock().expect("storage mutex poisoned");
        let policy_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM vault_secret_policies WHERE secret_id = ?1",
                params!["secret-1"],
                |row| row.get(0),
            )
            .expect("policy count should load");
        let usage_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM vault_secret_usages WHERE secret_id = ?1",
                params!["secret-1"],
                |row| row.get(0),
            )
            .expect("usage count should load");
        assert_eq!(policy_count, 0);
        assert_eq!(usage_count, 0);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn memory_fts_indexes_only_searchable_entries_and_rebuilds() {
        let state_dir = test_state_dir("memory-fts");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        for entry in [
            MemoryEntry {
                id: "accepted".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Release preference".to_string(),
                content: "Prefer concise release notes with exact validation results.".to_string(),
                tags: vec!["release".to_string()],
                enabled: true,
                status: "accepted".to_string(),
                memory_kind: "preference".to_string(),
                source_kind: "manual".to_string(),
                source_id: String::new(),
                confidence: 1.0,
                created_by: "user".to_string(),
                last_used_at: None,
                use_count: 0,
                supersedes_id: String::new(),
                metadata_json: serde_json::json!({}),
                created_at: 0,
                updated_at: 0,
            },
            MemoryEntry {
                id: "archived".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Archived release note".to_string(),
                content: "This archived validation phrase must not be searchable.".to_string(),
                tags: Vec::new(),
                enabled: true,
                status: "archived".to_string(),
                memory_kind: "note".to_string(),
                source_kind: "manual".to_string(),
                source_id: String::new(),
                confidence: 1.0,
                created_by: "user".to_string(),
                last_used_at: None,
                use_count: 0,
                supersedes_id: String::new(),
                metadata_json: serde_json::json!({}),
                created_at: 0,
                updated_at: 0,
            },
            MemoryEntry {
                id: "disabled".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Disabled release note".to_string(),
                content: "This disabled validation phrase must not be searchable.".to_string(),
                tags: Vec::new(),
                enabled: false,
                status: "accepted".to_string(),
                memory_kind: "note".to_string(),
                source_kind: "manual".to_string(),
                source_id: String::new(),
                confidence: 1.0,
                created_by: "user".to_string(),
                last_used_at: None,
                use_count: 0,
                supersedes_id: String::new(),
                metadata_json: serde_json::json!({}),
                created_at: 0,
                updated_at: 0,
            },
        ] {
            store
                .upsert_memory_entry(&entry)
                .expect("memory entry should persist");
        }

        let results = store
            .search_memory_entries("validation", Some("workspace"), Some("workspace"), 10)
            .expect("memory search should work");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.id, "accepted");

        {
            let connection = store.connection.lock().expect("storage mutex poisoned");
            connection
                .execute("DELETE FROM memory_entries_fts", [])
                .expect("derived fts rows should be removable");
        }
        assert!(
            store
                .search_memory_entries("validation", Some("workspace"), Some("workspace"), 10)
                .expect("empty derived index should search")
                .is_empty()
        );
        store
            .rebuild_memory_search_index()
            .expect("fts index should rebuild from source memory entries");
        let rebuilt = store
            .search_memory_entries("validation", Some("workspace"), Some("workspace"), 10)
            .expect("rebuilt memory search should work");
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0].entry.id, "accepted");

        store
            .delete_memory_entry("accepted")
            .expect("delete should remove source and fts row");
        assert!(
            store
                .search_memory_entries("validation", Some("workspace"), Some("workspace"), 10)
                .expect("memory search after delete should work")
                .is_empty()
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn memory_fts_initialization_populates_existing_accepted_entries() {
        let state_dir = test_state_dir("memory-fts-init-rebuild");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        {
            let connection = store.connection.lock().expect("storage mutex poisoned");
            for (id, enabled, status, content) in [
                (
                    "legacy-accepted",
                    1_i64,
                    "accepted",
                    "Legacy accepted migration phrase should become searchable.",
                ),
                (
                    "legacy-archived",
                    1_i64,
                    "archived",
                    "Legacy archived migration phrase must stay excluded.",
                ),
                (
                    "legacy-disabled",
                    0_i64,
                    "accepted",
                    "Legacy disabled migration phrase must stay excluded.",
                ),
            ] {
                connection
                    .execute(
                        "INSERT INTO memory_entries (id, scope_kind, scope_id, title, content, tags_json, enabled, status, memory_kind, source_kind, source_id, confidence, created_by, metadata_json) VALUES (?1, 'workspace', 'workspace', ?2, ?3, '[]', ?4, ?5, 'note', 'manual', '', 1.0, 'user', '{}')",
                        params![id, id, content, enabled, status],
                    )
                    .expect("legacy memory row should insert");
            }
            connection
                .execute("DELETE FROM memory_entries_fts", [])
                .expect("legacy database should have empty derived fts table");
        }
        drop(store);

        let reopened = StateStore::initialize_at(&state_dir)
            .expect("storage reinitialization should populate derived fts rows");
        let results = reopened
            .search_memory_entries("migration phrase", Some("workspace"), Some("workspace"), 10)
            .expect("search should use populated fts rows");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.id, "legacy-accepted");
        let debug = format!("{results:?}");
        assert!(!debug.contains("Legacy archived"));
        assert!(!debug.contains("Legacy disabled"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn seeds_workspace_profiles_and_default_profile() {
        let state_dir = test_state_dir("workspace-profiles");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let workspace = store.workspace().expect("workspace should load");

        assert_eq!(workspace.default_profile_id, "default");
        assert_eq!(workspace.profiles.len(), 3);
        assert_eq!(workspace.profiles[0].id, "default");
        assert!(workspace.profiles[0].is_default);
        assert_eq!(workspace.profiles[0].main.adapter, "openai_compatible");
        assert_eq!(
            workspace.profiles[0].main.model,
            DEFAULT_OPENAI_COMPATIBLE_MODEL
        );
        assert_eq!(
            workspace.profiles[0].main.base_url,
            DEFAULT_OPENAI_COMPATIBLE_BASE_URL
        );
        assert_eq!(workspace.profiles[0].utility, workspace.profiles[0].main);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn prunes_stale_runtime_rows_on_initialize() {
        let state_dir = test_state_dir("stale-runtime-prune");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let plan = StoragePlan::from_state_dir(&state_dir);
        let connection =
            Connection::open(&plan.database_path).expect("database should open directly");

        connection
            .execute(
                "INSERT INTO runtimes (id, summary, state) VALUES (?1, ?2, ?3)",
                params!["agent0", "Legacy adapter", "planned"],
            )
            .expect("stale runtime row should insert");
        drop(connection);
        drop(store);

        let store = StateStore::initialize_at(&state_dir).expect("store should reinitialize");
        let runtimes = store.list_runtimes().expect("runtimes should load");

        assert!(!runtimes.iter().any(|runtime| runtime.id == "agent0"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn creates_and_promotes_workspace_profiles() {
        let state_dir = test_state_dir("workspace-profile-promotion");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let created = store
            .create_workspace_profile(WorkspaceProfilePatch {
                title: "Research Ops".to_string(),
                main: WorkspaceModelConfig {
                    adapter: "openai_compatible".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                    base_url: "http://127.0.0.1:20128/v1".to_string(),
                    api_key: "secret-token".to_string(),
                },
                utility: WorkspaceModelConfig {
                    adapter: "claude".to_string(),
                    model: "sonnet".to_string(),
                    base_url: String::new(),
                    api_key: String::new(),
                },
                is_default: true,
            })
            .expect("workspace profile should create");

        let workspace = store.workspace().expect("workspace should load");
        let active = workspace
            .profiles
            .iter()
            .find(|profile| profile.id == created.id)
            .expect("created profile should be listed");

        assert_eq!(workspace.default_profile_id, created.id);
        assert!(active.is_default);
        assert_eq!(active.main.adapter, "openai_compatible");
        assert_eq!(active.main.base_url, "http://127.0.0.1:20128/v1");
        assert_eq!(active.main.api_key, "secret-token");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn resolves_discovered_projects_without_global_activation() {
        let state_dir = test_state_dir("resolve-project");
        let workspace_root = state_dir.join("workspace");
        let alpha = workspace_root.join("alpha");
        let alpha_git = alpha.join(".git");

        fs::create_dir_all(&alpha_git).expect("project marker should be created");

        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace = store
            .update_workspace(
                Some(
                    workspace_root
                        .to_str()
                        .expect("workspace root should be utf-8"),
                ),
                None,
                None,
                None,
                None,
            )
            .expect("workspace root should update");
        let project_id = workspace
            .projects
            .first()
            .expect("a discovered project should exist")
            .id
            .clone();

        let project = store
            .resolve_project(&project_id)
            .expect("discovered project should resolve");
        assert_eq!(project.id, project_id);
        assert_eq!(project.relative_path, "alpha");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn backfills_legacy_project_sessions_into_session_projects() {
        let state_dir = test_state_dir("legacy-session-backfill");
        let workspace_root = state_dir.join("workspace");
        let alpha = workspace_root.join("alpha");
        let alpha_git = alpha.join(".git");

        fs::create_dir_all(&alpha_git).expect("project marker should be created");

        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace = store
            .update_workspace(
                Some(
                    workspace_root
                        .to_str()
                        .expect("workspace root should be utf-8"),
                ),
                None,
                None,
                None,
                None,
            )
            .expect("workspace root should update");
        let project = workspace
            .projects
            .iter()
            .find(|project| project.relative_path == "alpha")
            .expect("alpha project should be discovered")
            .clone();
        drop(store);

        let plan = StoragePlan::from_state_dir(&state_dir);
        let connection =
            Connection::open(&plan.database_path).expect("legacy database should open");
        connection
            .execute(
                "
                INSERT INTO sessions (
                    id,
                    title,
                    scope,
                    project_id,
                    project_title,
                    project_path,
                    provider,
                    model,
                    working_dir,
                    working_dir_kind,
                    state
                )
                VALUES (?1, ?2, 'project', ?3, ?4, ?5, 'codex', 'gpt-5.4', '', 'workspace_scratch', 'active')
                ",
                params![
                    "legacy-session-1",
                    "Legacy session",
                    project.id,
                    project.title,
                    project.absolute_path,
                ],
            )
            .expect("legacy session row should insert");
        drop(connection);

        let store = StateStore::initialize_at(&state_dir).expect("store should reinitialize");
        let session = store
            .get_session("legacy-session-1")
            .expect("legacy session should load")
            .session;

        assert_eq!(session.scope, "project");
        assert_eq!(session.project_count, 1);
        assert_eq!(session.working_dir_kind, "project_root");
        assert_eq!(session.working_dir, project.absolute_path);
        assert_eq!(session.project_id, project.id);
        assert_eq!(session.projects.len(), 1);
        assert_eq!(session.projects[0].id, project.id);
        assert!(session.projects[0].is_primary);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn normalizes_existing_project_session_metadata_on_startup() {
        let state_dir = test_state_dir("project-session-normalization");
        let workspace_root = state_dir.join("workspace");
        let alpha = workspace_root.join("alpha");
        let alpha_git = alpha.join(".git");

        fs::create_dir_all(&alpha_git).expect("project marker should be created");

        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace = store
            .update_workspace(
                Some(
                    workspace_root
                        .to_str()
                        .expect("workspace root should be utf-8"),
                ),
                None,
                None,
                None,
                None,
            )
            .expect("workspace root should update");
        let project = workspace
            .projects
            .iter()
            .find(|project| project.relative_path == "alpha")
            .expect("alpha project should be discovered")
            .clone();
        drop(store);

        let plan = StoragePlan::from_state_dir(&state_dir);
        let connection =
            Connection::open(&plan.database_path).expect("legacy database should open");
        connection
            .execute(
                "
                INSERT INTO sessions (
                    id,
                    title,
                    scope,
                    project_id,
                    project_title,
                    project_path,
                    provider,
                    model,
                    working_dir,
                    working_dir_kind,
                    state
                )
                VALUES (?1, ?2, 'project', ?3, ?4, ?5, 'codex', 'gpt-5.4', ?5, 'project', 'active')
                ",
                params![
                    "legacy-session-2",
                    "Legacy normalized session",
                    project.id,
                    project.title,
                    project.absolute_path,
                ],
            )
            .expect("legacy session row should insert");
        connection
            .execute(
                "
                INSERT INTO session_projects (session_id, project_id, sort_order, is_primary)
                VALUES (?1, ?2, 0, 1)
                ",
                params!["legacy-session-2", project.id],
            )
            .expect("legacy session project row should insert");
        drop(connection);

        let store = StateStore::initialize_at(&state_dir).expect("store should reinitialize");
        let session = store
            .get_session("legacy-session-2")
            .expect("legacy session should load")
            .session;

        assert_eq!(session.scope, "project");
        assert_eq!(session.working_dir_kind, "project_root");
        assert_eq!(session.working_dir, project.absolute_path);
        assert_eq!(session.project_count, 1);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn persists_session_workspace_metadata() {
        let state_dir = test_state_dir("session-workspace-metadata");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let mut record = test_session_record(
            "workspace-session",
            "Workspace session",
            "project",
            "/tmp/worktree".to_string(),
        );
        record.workspace_mode = "isolated_worktree".to_string();
        record.source_project_path = "/tmp/source".to_string();
        record.git_root = "/tmp/source".to_string();
        record.worktree_path = "/tmp/worktree".to_string();
        record.git_branch = "work/project/abcdef12".to_string();
        record.git_base_ref = "dev".to_string();
        record.git_head = "0123456789abcdef".to_string();
        record.git_dirty = true;
        record.git_untracked_count = 2;
        record.git_remote_tracking_branch = "origin/dev".to_string();
        record.workspace_warnings = vec!["warning".to_string()];

        store
            .create_session(record)
            .expect("session should persist");
        let detail = store
            .get_session("workspace-session")
            .expect("session should load");

        assert_eq!(detail.session.workspace_mode, "isolated_worktree");
        assert_eq!(detail.session.source_project_path, "/tmp/source");
        assert_eq!(detail.session.worktree_path, "/tmp/worktree");
        assert_eq!(detail.session.git_branch, "work/project/abcdef12");
        assert_eq!(detail.session.git_base_ref, "dev");
        assert_eq!(detail.session.git_head, "0123456789abcdef");
        assert!(detail.session.git_dirty);
        assert_eq!(detail.session.git_untracked_count, 2);
        assert_eq!(detail.session.git_remote_tracking_branch, "origin/dev");
        assert_eq!(
            detail.session.workspace_warnings,
            vec!["warning".to_string()]
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn updates_existing_session_turn_content() {
        let state_dir = test_state_dir("update-session-turn");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let scratch_dir = store
            .scratch_dir_for_session("stream-session")
            .expect("scratch dir should resolve");

        store
            .create_session(SessionRecord {
                id: "stream-session".to_string(),
                profile_id: String::new(),
                profile_title: String::new(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "ad_hoc".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: String::new(),
                project_ids: Vec::new(),
                title: "Streaming session".to_string(),
                provider: "codex".to_string(),
                model: "gpt-5.4".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                working_dir: scratch_dir,
                working_dir_kind: "workspace_scratch".to_string(),
                workspace_mode: "scratch_only".to_string(),
                source_project_path: String::new(),
                git_root: String::new(),
                worktree_path: String::new(),
                git_branch: String::new(),
                git_base_ref: String::new(),
                git_head: String::new(),
                git_dirty: false,
                git_untracked_count: 0,
                git_remote_tracking_branch: String::new(),
                workspace_warnings: Vec::new(),
                approval_mode: "ask".to_string(),
                execution_mode: "act".to_string(),
                run_budget_mode: "inherit".to_string(),
            })
            .expect("session should persist");
        store
            .append_session_turn("stream-session", "user-1", "user", "Hello", &[])
            .expect("user turn should append");
        store
            .append_session_turn("stream-session", "assistant-1", "assistant", "Hi", &[])
            .expect("assistant turn should append");

        let turn = store
            .update_session_turn_content("stream-session", "assistant-1", "Hi there")
            .expect("assistant turn should update");
        let session = store
            .get_session("stream-session")
            .expect("session should reload");

        assert_eq!(turn.content, "Hi there");
        assert_eq!(
            session
                .turns
                .iter()
                .find(|turn| turn.id == "assistant-1")
                .expect("assistant turn should exist")
                .content,
            "Hi there"
        );
        assert_eq!(session.session.last_message_excerpt, "Hi there");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn session_run_budget_inherits_workspace_defaults_and_can_override() {
        let state_dir = test_state_dir("session-run-budget");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace_budget = RunBudgetSummary {
            mode: "standard".to_string(),
            max_steps: 120,
            max_tool_calls: 240,
            max_wall_clock_secs: 10_800,
        };
        store
            .update_workspace(None, None, None, None, Some(&workspace_budget))
            .expect("workspace run budget should update");
        let scratch_dir = store
            .scratch_dir_for_session("budget-session")
            .expect("scratch dir should resolve");
        store
            .create_session(test_session_record(
                "budget-session",
                "Budget session",
                "ad_hoc",
                scratch_dir,
            ))
            .expect("session should persist");

        let inherited = store
            .get_session("budget-session")
            .expect("session should reload")
            .session;
        assert_eq!(inherited.run_budget_mode, "inherit");
        assert_eq!(inherited.run_budget.max_steps, 120);
        assert_eq!(inherited.run_budget.max_tool_calls, 240);
        assert_eq!(inherited.run_budget.max_wall_clock_secs, 10_800);

        store
            .update_session(
                "budget-session",
                SessionPatch {
                    run_budget_mode: Some("unbounded".to_string()),
                    ..SessionPatch::default()
                },
            )
            .expect("session budget should update");
        let unbounded = store
            .get_session("budget-session")
            .expect("session should reload")
            .session;
        assert_eq!(unbounded.run_budget_mode, "unbounded");
        assert_eq!(unbounded.run_budget.max_steps, 0);
        assert_eq!(unbounded.run_budget.max_tool_calls, 0);
        assert_eq!(unbounded.run_budget.max_wall_clock_secs, 0);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn lists_only_root_jobs_for_a_session() {
        let state_dir = test_state_dir("list-root-session-jobs");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let scratch_dir = store
            .scratch_dir_for_session("session-jobs")
            .expect("scratch dir should resolve");

        store
            .create_session(SessionRecord {
                id: "session-jobs".to_string(),
                profile_id: String::new(),
                profile_title: String::new(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "ad_hoc".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: String::new(),
                project_ids: Vec::new(),
                title: "Session jobs".to_string(),
                provider: "codex".to_string(),
                model: "gpt-5.4".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                working_dir: scratch_dir,
                working_dir_kind: "workspace_scratch".to_string(),
                workspace_mode: "scratch_only".to_string(),
                source_project_path: String::new(),
                git_root: String::new(),
                worktree_path: String::new(),
                git_branch: String::new(),
                git_base_ref: String::new(),
                git_head: String::new(),
                git_dirty: false,
                git_untracked_count: 0,
                git_remote_tracking_branch: String::new(),
                workspace_warnings: Vec::new(),
                approval_mode: "ask".to_string(),
                execution_mode: "act".to_string(),
                run_budget_mode: "inherit".to_string(),
            })
            .expect("session should persist");

        store
            .create_job(JobRecord {
                id: "root-job".to_string(),
                session_id: Some("session-jobs".to_string()),
                parent_job_id: None,
                template_id: None,
                title: "Root job".to_string(),
                purpose: "test".to_string(),
                trigger_kind: "session_prompt".to_string(),
                state: "completed".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "root".to_string(),
            })
            .expect("root job should persist");
        store
            .create_job(JobRecord {
                id: "child-job".to_string(),
                session_id: Some("session-jobs".to_string()),
                parent_job_id: Some("root-job".to_string()),
                template_id: None,
                title: "Child job".to_string(),
                purpose: "test".to_string(),
                trigger_kind: "child_job".to_string(),
                state: "completed".to_string(),
                requested_by: "agent".to_string(),
                prompt_excerpt: "child".to_string(),
            })
            .expect("child job should persist");

        let jobs = store
            .list_jobs_for_session("session-jobs")
            .expect("session jobs should load");

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "root-job");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn excludes_automation_sessions_from_session_list() {
        let state_dir = test_state_dir("automation-session-filter");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let visible_scratch = store
            .scratch_dir_for_session("visible-session")
            .expect("visible scratch dir should resolve");
        let automation_scratch = store
            .scratch_dir_for_session("automation-session")
            .expect("automation scratch dir should resolve");

        store
            .create_session(test_session_record(
                "visible-session",
                "Visible session",
                "ad_hoc",
                visible_scratch,
            ))
            .expect("visible session should persist");
        store
            .create_session(test_session_record(
                "automation-session",
                "Automation session",
                "automation",
                automation_scratch,
            ))
            .expect("automation session should persist");

        let sessions = store.list_sessions().expect("sessions should load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "visible-session");
        assert!(
            sessions
                .iter()
                .all(|session| session.scope.as_str() != "automation")
        );

        let automation = store
            .get_session("automation-session")
            .expect("automation session should still be addressable");
        assert_eq!(automation.session.scope, "automation");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn persists_playbooks_and_lists_jobs_by_template_state() {
        let state_dir = test_state_dir("playbook-crud");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let working_dir = store
            .scratch_dir_for_session("playbook-session")
            .expect("playbook scratch dir should resolve");

        store
            .create_session(test_session_record(
                "playbook-session",
                "Playbook session",
                "automation",
                working_dir.clone(),
            ))
            .expect("playbook session should persist");

        let created = store
            .create_playbook(PlaybookRecord {
                id: "playbook-1".to_string(),
                session_id: "playbook-session".to_string(),
                title: "Workspace Sync".to_string(),
                description: "Refresh project metadata".to_string(),
                prompt: "Inspect the workspace and summarize changes.".to_string(),
                enabled: true,
                policy_bundle: "command_runner".to_string(),
                trigger_kind: "schedule".to_string(),
                schedule_interval_secs: Some(900),
                event_kind: None,
                created_at: 100,
                updated_at: 100,
            })
            .expect("playbook should persist");

        assert_eq!(created.playbook.id, "playbook-1");
        assert_eq!(created.playbook.job_count, 0);
        assert_eq!(
            store.playbooks_dir_path().join("playbook-1.json"),
            playbook_file_path(&store.playbooks_dir_path(), "playbook-1")
        );

        let listed = store.list_playbooks().expect("playbooks should list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "playbook-1");
        assert_eq!(listed[0].working_dir, working_dir);

        let detail = store
            .get_playbook("playbook-1")
            .expect("playbook detail should load");
        assert_eq!(
            detail.prompt,
            "Inspect the workspace and summarize changes."
        );
        assert_eq!(detail.session.scope, "automation");

        let updated = store
            .update_playbook(
                "playbook-1",
                PlaybookPatch {
                    title: Some("Workspace sync and test".to_string()),
                    prompt: Some("Run checks and summarize results.".to_string()),
                    enabled: Some(false),
                    trigger_kind: Some("event".to_string()),
                    schedule_interval_secs: Some(None),
                    event_kind: Some(Some("workspace_projects_synced".to_string())),
                    updated_at: Some(200),
                    ..PlaybookPatch::default()
                },
            )
            .expect("playbook should update");
        assert_eq!(updated.playbook.title, "Workspace sync and test");
        assert_eq!(updated.playbook.enabled, false);
        assert_eq!(updated.playbook.trigger_kind, "event");
        assert_eq!(updated.playbook.schedule_interval_secs, None);
        assert_eq!(
            updated.playbook.event_kind.as_deref(),
            Some("workspace_projects_synced")
        );
        assert_eq!(updated.prompt, "Run checks and summarize results.");

        store
            .create_job(JobRecord {
                id: "playbook-job-b".to_string(),
                session_id: Some("playbook-session".to_string()),
                parent_job_id: None,
                template_id: Some("playbook-1".to_string()),
                title: "Playbook run".to_string(),
                purpose: "automation".to_string(),
                trigger_kind: "playbook_event".to_string(),
                state: "running".to_string(),
                requested_by: "system".to_string(),
                prompt_excerpt: "summary".to_string(),
            })
            .expect("root playbook job should persist");
        store
            .create_job(JobRecord {
                id: "playbook-job-a".to_string(),
                session_id: Some("playbook-session".to_string()),
                parent_job_id: None,
                template_id: Some("playbook-1".to_string()),
                title: "Playbook run complete".to_string(),
                purpose: "automation".to_string(),
                trigger_kind: "playbook_manual".to_string(),
                state: "completed".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "summary".to_string(),
            })
            .expect("second root playbook job should persist");
        store
            .create_job(JobRecord {
                id: "playbook-child".to_string(),
                session_id: Some("playbook-session".to_string()),
                parent_job_id: Some("playbook-job-a".to_string()),
                template_id: Some("playbook-1".to_string()),
                title: "Playbook child".to_string(),
                purpose: "automation".to_string(),
                trigger_kind: "child_job".to_string(),
                state: "completed".to_string(),
                requested_by: "agent".to_string(),
                prompt_excerpt: "child".to_string(),
            })
            .expect("child job should persist");

        let template_jobs = store
            .list_jobs_for_template("playbook-1", 10)
            .expect("template jobs should load");
        assert_eq!(template_jobs.len(), 2);
        assert!(
            template_jobs
                .iter()
                .all(|job| job.parent_job_id.as_deref().is_none())
        );

        let running_jobs = store
            .list_jobs_for_template_by_state("playbook-1", &["running"])
            .expect("running playbook jobs should load");
        assert_eq!(running_jobs.len(), 1);
        assert_eq!(running_jobs[0].id, "playbook-job-b");

        let with_jobs = store
            .get_playbook("playbook-1")
            .expect("playbook should reload with jobs");
        assert_eq!(with_jobs.playbook.job_count, 2);
        assert_eq!(with_jobs.recent_jobs.len(), 2);

        let deleted = store
            .delete_playbook("playbook-1")
            .expect("playbook should delete");
        assert_eq!(deleted.playbook.id, "playbook-1");
        assert!(
            store
                .list_playbooks()
                .expect("playbooks should reload")
                .is_empty()
        );
        assert!(
            store.get_session("playbook-session").is_err(),
            "deleting a playbook should remove its hidden session"
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn provisions_and_validates_local_auth_token() {
        let state_dir = test_state_dir("local-auth-token");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let token_path = PathBuf::from(store.local_auth_token_path());
        let token = store
            .read_local_auth_token()
            .expect("local auth token should exist");

        assert!(token_path.is_file());
        assert!(token.starts_with("nuctk_"));
        assert!(
            store
                .validate_access_token(&token)
                .expect("token validation should succeed")
        );
        assert!(
            !store
                .validate_access_token("nuctk_invalid")
                .expect("invalid token check should succeed")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn persists_update_state_across_reinitialization() {
        let state_dir = test_state_dir("update-state");
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        store
            .write_update_state(&StoredUpdateState {
                tracked_channel: Some("stable".to_string()),
                tracked_ref: Some("main".to_string()),
                release_manifest_url: Some("file:///tmp/manifest-stable.json".to_string()),
                pending_restart_release_id: Some("rel_pending".to_string()),
                update_available: true,
                last_successful_check_at: Some(123),
                last_successful_target_version: Some("0.2.0".to_string()),
                last_successful_target_release_id: Some("rel_123".to_string()),
                last_successful_target_commit: Some("abcdef1234567890".to_string()),
                last_attempted_check_at: Some(124),
                last_attempt_result: Some("success".to_string()),
                latest_error: None,
                latest_error_at: None,
                restart_required: true,
            })
            .expect("update state should persist");
        drop(store);

        let store = StateStore::initialize_at(&state_dir).expect("store should reinitialize");
        let state = store
            .read_update_state()
            .expect("update state should reload");

        assert_eq!(state.tracked_channel.as_deref(), Some("stable"));
        assert_eq!(state.tracked_ref.as_deref(), Some("main"));
        assert_eq!(
            state.pending_restart_release_id.as_deref(),
            Some("rel_pending")
        );
        assert!(state.update_available);
        assert_eq!(
            state.last_successful_target_version.as_deref(),
            Some("0.2.0")
        );
        assert!(state.restart_required);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn initializes_even_when_default_workspace_root_is_missing() {
        let _env_lock = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let state_dir = test_state_dir("missing-default-workspace-root");
        let temp_home = state_dir.join("home");
        fs::create_dir_all(&temp_home).expect("temporary home should exist");

        let original_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", &temp_home);
        }

        let default_root = temp_home.join("dev-projects");

        if default_root.exists() {
            let _ = fs::remove_dir_all(&default_root);
        }

        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");
        let workspace = store.workspace().expect("workspace should load");

        assert_eq!(workspace.root_path, default_root.display().to_string());
        assert!(workspace.projects.is_empty());

        match original_home {
            Some(value) => unsafe {
                env::set_var("HOME", value);
            },
            None => unsafe {
                env::remove_var("HOME");
            },
        }

        let _ = fs::remove_dir_all(&state_dir);
    }

    fn test_state_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("nucleus-{label}-{}-{suffix}", std::process::id()))
    }

    fn test_session_record(
        session_id: &str,
        title: &str,
        scope: &str,
        working_dir: String,
    ) -> SessionRecord {
        SessionRecord {
            id: session_id.to_string(),
            title: title.to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            scope: scope.to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            project_ids: Vec::new(),
            provider: "codex".to_string(),
            model: "gpt-5.4".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            working_dir,
            working_dir_kind: "workspace_scratch".to_string(),
            workspace_mode: "scratch_only".to_string(),
            source_project_path: String::new(),
            git_root: String::new(),
            worktree_path: String::new(),
            git_branch: String::new(),
            git_base_ref: String::new(),
            git_head: String::new(),
            git_dirty: false,
            git_untracked_count: 0,
            git_remote_tracking_branch: String::new(),
            workspace_warnings: Vec::new(),
            approval_mode: "ask".to_string(),
            execution_mode: "act".to_string(),
            run_budget_mode: "inherit".to_string(),
        }
    }
}

#[cfg(test)]
mod skills_mcp_phase2_tests {
    use super::*;
    use nucleus_protocol::{McpServerSummary, NucleusToolDescriptor, SkillManifest};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn persists_skills_and_mcp_resources_across_reinitialization() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        let state_dir = std::env::temp_dir().join(format!(
            "nucleus-skills-mcp-persistence-{}-{suffix}",
            std::process::id()
        ));
        let store = StateStore::initialize_at(&state_dir).expect("store should initialize");

        let manifest = SkillManifest {
            id: "skill.manifest.docs".to_string(),
            title: "Docs Skill".to_string(),
            description: "Helps with docs".to_string(),
            instructions: String::new(),
            activation_mode: "manual".to_string(),
            triggers: vec!["docs".to_string()],
            include_paths: vec!["docs/**".to_string()],
            required_tools: vec!["shell".to_string()],
            required_mcps: vec!["mcp.docs".to_string()],
            project_filters: vec!["nucleus".to_string()],
            enabled: true,
        };
        store
            .upsert_skill_manifest(&manifest)
            .expect("skill manifest should persist");

        let server = McpServerSummary {
            id: "mcp.docs".to_string(),
            title: "Docs MCP".to_string(),
            enabled: true,
            transport: "stdio".to_string(),
            command: "mock".to_string(),
            args: Vec::new(),
            env_json: serde_json::json!({}),
            url: String::new(),
            headers_json: serde_json::json!({}),
            auth_kind: "none".to_string(),
            auth_ref: String::new(),
            sync_status: "ready".to_string(),
            last_error: String::new(),
            last_synced_at: None,
            tools: vec![NucleusToolDescriptor {
                id: "docs.search".to_string(),
                title: "Docs Search".to_string(),
                description: "Search docs".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                source: "mcp.docs".to_string(),
            }],
            resources: vec!["docs://index".to_string()],
        };
        store
            .upsert_mcp_server(&server)
            .expect("mcp server should persist");

        let tool = nucleus_protocol::McpToolRecord {
            id: "mcp-tool-1".to_string(),
            server_id: server.id.clone(),
            name: "searchDocs".to_string(),
            description: "Searches docs".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"query":{"type":"string"}}}),
            source: "mcp.docs".to_string(),
            discovered_at: 1,
            created_at: 2,
            updated_at: 3,
        };
        store
            .upsert_mcp_tool(&tool)
            .expect("mcp tool should persist");

        let package = nucleus_protocol::SkillPackageRecord {
            id: "skill-pkg-1".to_string(),
            name: "Docs Skill Package".to_string(),
            version: "0.1.0".to_string(),
            manifest_json: serde_json::json!({"manifest_id": manifest.id}),
            instructions: "Use docs skill".to_string(),
            source_kind: "manual".to_string(),
            source_url: String::new(),
            source_repo_url: String::new(),
            source_owner: String::new(),
            source_repo: String::new(),
            source_ref: String::new(),
            source_parent_path: String::new(),
            source_skill_path: String::new(),
            source_commit: String::new(),
            imported_at: Some(4),
            last_checked_at: None,
            latest_source_commit: String::new(),
            update_status: "unknown".to_string(),
            content_checksum: "abc".to_string(),
            dirty_status: "clean".to_string(),
            created_at: 4,
            updated_at: 5,
        };
        store
            .upsert_skill_package(&package)
            .expect("skill package should persist");

        let installation = nucleus_protocol::SkillInstallationRecord {
            id: "skill-install-1".to_string(),
            package_id: package.id.clone(),
            scope_kind: "workspace".to_string(),
            scope_id: "workspace".to_string(),
            enabled: true,
            pinned_version: Some("0.1.0".to_string()),
            created_at: 6,
            updated_at: 7,
        };
        store
            .upsert_skill_installation(&installation)
            .expect("skill installation should persist");

        drop(store);

        let store = StateStore::initialize_at(&state_dir).expect("store should reinitialize");
        assert_eq!(
            store
                .list_skill_manifests()
                .expect("skill manifests should load"),
            vec![manifest]
        );
        assert_eq!(
            store.list_mcp_servers().expect("mcp servers should load"),
            vec![server]
        );
        assert_eq!(
            store.list_mcp_tools().expect("mcp tools should load"),
            vec![tool]
        );
        assert_eq!(
            store
                .list_skill_packages()
                .expect("skill packages should load"),
            vec![package]
        );
        assert_eq!(
            store
                .list_skill_installations()
                .expect("skill installations should load"),
            vec![installation]
        );

        let _ = fs::remove_dir_all(&state_dir);
    }
}
