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
pub struct WorktreeSummary {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub path: String,
    pub branch: String,
    pub base_ref: String,
    pub base_commit: String,
    pub origin_url: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserFacingErrorSummary {
    pub code: String,
    pub title: String,
    pub message: String,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub technical_detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    #[serde(default = "default_attachment_mode")]
    pub attachment_mode: String,
    #[serde(default)]
    pub worktree_id: String,
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
    pub base_ref: String,
    #[serde(default)]
    pub base_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind_by: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state_observed_at: Option<i64>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_error: Option<UserFacingErrorSummary>,
    #[serde(default)]
    pub capabilities: Vec<ToolCapabilitySummary>,
    pub last_message_excerpt: String,
    pub turn_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_resumed_at: Option<i64>,
    #[serde(default)]
    pub last_reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_at: Option<i64>,
    #[serde(default)]
    pub token_usage_known: bool,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_estimate: Option<f64>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
pub struct CompletionGateSummary {
    pub id: String,
    pub title: String,
    pub state: String,
    pub summary: String,
    #[serde(default)]
    pub task_class: String,
    #[serde(default)]
    pub required_evidence: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRequirementSummary {
    pub id: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEvidenceContractSummary {
    pub task_class: String,
    pub title: String,
    pub summary: String,
    pub requirements: Vec<EvidenceRequirementSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobSummary {
    pub id: String,
    pub session_id: Option<String>,
    pub parent_job_id: Option<String>,
    pub template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    pub title: String,
    pub purpose: String,
    pub trigger_kind: String,
    pub state: String,
    pub requested_by: String,
    pub prompt_excerpt: String,
    pub root_worker_id: Option<String>,
    #[serde(default)]
    pub executor_lane: String,
    #[serde(default)]
    pub executor_provider: String,
    #[serde(default)]
    pub executor_model: String,
    #[serde(default)]
    pub executor_route_id: String,
    #[serde(default)]
    pub executor_route_title: String,
    pub visible_turn_id: Option<String>,
    pub result_summary: String,
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_error: Option<UserFacingErrorSummary>,
    pub ui_renderable: String,
    pub browser_verification_required: bool,
    #[serde(default = "default_browser_verification_status")]
    pub browser_verification_status: String,
    pub browser_verification_summary: String,
    pub browser_verification_artifact_ids: Vec<String>,
    #[serde(default)]
    pub publication_requested: bool,
    #[serde(default = "default_publication_status")]
    pub publication_status: String,
    #[serde(default)]
    pub publication_summary: String,
    #[serde(default)]
    pub pr_url: String,
    #[serde(default)]
    pub source_branch: String,
    #[serde(default)]
    pub target_branch: String,
    #[serde(default = "default_validation_status")]
    pub validation_status: String,
    #[serde(default = "default_cleanup_status")]
    pub cleanup_status: String,
    #[serde(default)]
    pub cleanup_paths: Vec<String>,
    #[serde(default)]
    pub task_evidence: Vec<String>,
    #[serde(default)]
    pub metadata_json: Value,
    #[serde(default)]
    pub worktree_base_ref: String,
    #[serde(default)]
    pub worktree_base_status: String,
    #[serde(default)]
    pub worktree_base_reason: String,
    #[serde(default)]
    pub worktree_origin_url: String,
    #[serde(default)]
    pub expected_origin_url: String,
    #[serde(default)]
    pub observed_git_branch: String,
    #[serde(default)]
    pub expected_git_branch: String,
    #[serde(default)]
    pub worktree_head_sha: String,
    #[serde(default)]
    pub canonical_base_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_behind_by: Option<i64>,
    #[serde(default)]
    pub branch_repo_status: String,
    #[serde(default)]
    pub branch_repo_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_session_cwd_evidence_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_entity_evidence_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_state_evidence_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state_observed_at: Option<i64>,
    #[serde(default)]
    pub completion_status: String,
    #[serde(default)]
    pub completion_gates: Vec<CompletionGateSummary>,
    #[serde(default)]
    pub completion_blockers: Vec<String>,
    pub worker_count: usize,
    pub pending_approval_count: usize,
    pub artifact_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_resumed_at: Option<i64>,
    #[serde(default)]
    pub last_reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_at: Option<i64>,
    #[serde(default)]
    pub token_usage_known: bool,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_estimate: Option<f64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl JobSummary {
    pub fn with_completion_gates(mut self) -> Self {
        let gates = derive_completion_gates(&self);
        let blockers = gates
            .iter()
            .filter(|gate| gate.state == "blocked")
            .map(|gate| gate.summary.clone())
            .collect::<Vec<_>>();
        let completion_status = if blockers.is_empty() {
            if gates.iter().any(|gate| gate.state == "pending") {
                "pending"
            } else if gates.is_empty() {
                "not_gated"
            } else {
                "satisfied"
            }
        } else {
            "blocked"
        };

        self.completion_status = completion_status.to_string();
        self.completion_gates = gates;
        self.completion_blockers = blockers;
        self
    }

    pub fn has_blocking_completion_gates(&self) -> bool {
        self.completion_status == "blocked"
            || self
                .completion_gates
                .iter()
                .any(|gate| gate.state == "blocked")
    }
}

fn default_publication_status() -> String {
    "not_requested".to_string()
}

fn default_validation_status() -> String {
    "not_performed".to_string()
}

fn default_browser_verification_status() -> String {
    "not_required".to_string()
}

fn default_cleanup_status() -> String {
    "unknown".to_string()
}

fn derive_completion_gates(job: &JobSummary) -> Vec<CompletionGateSummary> {
    if job.task_class.is_none()
        && !job.publication_requested
        && !job.browser_verification_required
        && job.cleanup_status != "cleanup_required"
        && !has_context_integrity_gate_state(job)
    {
        return Vec::new();
    }

    let terminal = matches!(
        job.state.as_str(),
        "completed" | "blocked" | "failed" | "canceled"
    );
    let mut gates = Vec::new();
    let task_class = normalized_task_class(job);
    let publication_gated = job.publication_requested || task_class == Some("github_pr");

    if publication_gated {
        gates.push(publication_gate(job, terminal));
        gates.push(validation_gate(job, terminal));
    }

    if publication_gated || job.browser_verification_required {
        gates.push(browser_gate(job, terminal));
    }

    if publication_gated || job.cleanup_status == "cleanup_required" {
        gates.push(cleanup_gate(job, terminal));
    }

    if has_worktree_base_gate_state(job) {
        gates.push(worktree_base_gate(job));
    }

    if has_branch_repo_gate_state(job) {
        gates.push(branch_repo_gate(job));
    }

    if has_cwd_gate_state(job) {
        gates.push(cwd_gate(job));
    }

    if has_session_state_gate_state(job) {
        gates.push(session_state_gate(job));
    }

    if has_target_entity_gate_state(job) {
        gates.push(target_entity_gate(job));
    }

    if has_process_state_gate_state(job) {
        gates.push(process_state_gate(job));
    }

    if let Some(task_class) = task_class
        && (!job.publication_requested || task_class != "github_pr")
        && let Some(gate) = task_class_gate(job, terminal, task_class)
    {
        gates.push(gate);
    }

    gates
}

pub fn task_evidence_contract_catalog() -> Vec<TaskEvidenceContractSummary> {
    vec![
        task_evidence_contract(
            "context_integrity",
            "Context integrity",
            "Ground session context claims in daemon-observed worktree freshness, branch/repo consistency, cwd, session-state, target-entity, and process/port evidence.",
            &[
                (
                    "worktree_base_evidence",
                    "Worktree base evidence",
                    "Record worktree HEAD, canonical base SHA, and commits behind canonical before work begins.",
                ),
                (
                    "branch_repo_evidence",
                    "Branch/repo evidence",
                    "Record observed and expected origin URLs plus observed and expected branch names.",
                ),
                (
                    "cwd_evidence",
                    "cwd evidence",
                    "Record the declared working directory, observed command-session cwds, and any command sessions that ran outside scope.",
                ),
                (
                    "session_state_evidence",
                    "Session-state evidence",
                    "Record stored and freshly observed git state plus an audit-event pointer when a daemon-observed mutation explains drift.",
                ),
                (
                    "target_entity_evidence",
                    "Target-entity evidence",
                    "Record extracted entity claims, entity type, daemon evidence searched, and match/no-match results.",
                ),
                (
                    "process_state_evidence",
                    "Process-state evidence",
                    "Record extracted process or port claims, daemon-managed identifier, last-observed state, and observation timestamp.",
                ),
            ],
        ),
        task_evidence_contract(
            "github_pr",
            "GitHub/PR work",
            "Ground PR lifecycle, review, and CI claims in direct GitHub evidence.",
            &[
                (
                    "pr_state",
                    "Direct PR state",
                    "Use direct PR state evidence for open, closed, merged, mergeability, and review-decision claims.",
                ),
                (
                    "review_threads",
                    "Thread-aware review evidence",
                    "Use unresolved review-thread evidence before claiming no actionable PR feedback remains.",
                ),
                (
                    "status_checks",
                    "Status check evidence",
                    "Use CI/check-suite evidence before claiming PR checks are green.",
                ),
            ],
        ),
        task_evidence_contract(
            "research",
            "Research",
            "Ground research answers in source freshness, provenance, and contradiction checks.",
            &[
                (
                    "fresh_sources",
                    "Fresh direct sources",
                    "Use direct, recent sources for time-sensitive claims.",
                ),
                (
                    "source_quality",
                    "Source quality",
                    "Prefer primary or authoritative sources before confident conclusions.",
                ),
                (
                    "contradictions",
                    "Contradiction check",
                    "Check for conflicting evidence before final-sounding answers.",
                ),
            ],
        ),
        task_evidence_contract(
            "automation",
            "Automation",
            "Ground automation claims in actual schedule, execution, and side-effect state.",
            &[
                (
                    "schedule_state",
                    "Schedule state",
                    "Verify the target automation schedule or monitor state.",
                ),
                (
                    "execution_logs",
                    "Execution logs",
                    "Use run logs or exit status before claiming an automation ran successfully.",
                ),
                (
                    "side_effects",
                    "Observed side effects",
                    "Confirm expected side effects before saying the automation completed.",
                ),
            ],
        ),
        task_evidence_contract(
            "local_project",
            "Local project work",
            "Ground local project claims in cwd, branch, changed files, and validation evidence.",
            &[
                (
                    "workspace_context",
                    "Workspace context",
                    "Verify cwd, branch, repo, and target project before using command output as evidence.",
                ),
                (
                    "changed_files",
                    "Changed files",
                    "Use the actual changed-file set before summarizing code changes.",
                ),
                (
                    "validation",
                    "Validation evidence",
                    "Use test, build, lint, or explicit unavailable evidence before claiming validation passed.",
                ),
            ],
        ),
        task_evidence_contract(
            "deployment",
            "Deployment/release work",
            "Ground shipped/released claims in remote deployment, version, and health evidence.",
            &[
                (
                    "deployment_status",
                    "Deployment status",
                    "Verify the remote deployment or release workflow status.",
                ),
                (
                    "version",
                    "Version evidence",
                    "Confirm the shipped version, artifact, or commit.",
                ),
                (
                    "health",
                    "Health check",
                    "Check service health before saying a deployment is live.",
                ),
            ],
        ),
        task_evidence_contract(
            "memory_session",
            "Memory/session operations",
            "Ground memory and session claims in target scope and persisted state.",
            &[
                (
                    "target_scope",
                    "Target scope",
                    "Verify the session, project, profile, or memory scope before mutating or reporting state.",
                ),
                (
                    "operation_result",
                    "Operation result",
                    "Use the store operation result before claiming memory/session state changed.",
                ),
                (
                    "retrieval_source",
                    "Retrieval source",
                    "Report or use the source of retrieved memory/session evidence.",
                ),
            ],
        ),
        task_evidence_contract(
            "process_server",
            "Process/server state",
            "Ground running/stopped/server-ready claims in process, port, and health evidence.",
            &[
                (
                    "process_state",
                    "Process state",
                    "Verify the target process or service state.",
                ),
                (
                    "port_state",
                    "Port state",
                    "Check the expected port or listener before claiming a server is reachable.",
                ),
                (
                    "health_or_logs",
                    "Health/log evidence",
                    "Use a health endpoint or logs before claiming a server is healthy.",
                ),
            ],
        ),
    ]
}

fn task_evidence_contract(
    task_class: &str,
    title: &str,
    summary: &str,
    requirements: &[(&str, &str, &str)],
) -> TaskEvidenceContractSummary {
    TaskEvidenceContractSummary {
        task_class: task_class.to_string(),
        title: title.to_string(),
        summary: summary.to_string(),
        requirements: requirements
            .iter()
            .map(|(id, title, summary)| EvidenceRequirementSummary {
                id: (*id).to_string(),
                title: (*title).to_string(),
                summary: (*summary).to_string(),
            })
            .collect(),
    }
}

fn required_evidence_for(task_class: &str, ids: &[&str]) -> Vec<String> {
    task_evidence_contract_catalog()
        .into_iter()
        .find(|contract| contract.task_class == task_class)
        .map(|contract| {
            contract
                .requirements
                .into_iter()
                .filter(|requirement| ids.contains(&requirement.id.as_str()))
                .map(|requirement| requirement.title)
                .collect()
        })
        .unwrap_or_default()
}

fn publication_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    let evidence = [
        non_empty_evidence("PR", &job.pr_url),
        non_empty_evidence("Source", &job.source_branch),
        non_empty_evidence("Target", &job.target_branch),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    let has_open_pr_evidence = job.publication_status == "opened"
        && !job.pr_url.is_empty()
        && !job.target_branch.is_empty();
    let state = if has_open_pr_evidence {
        "done"
    } else if matches!(
        job.publication_status.as_str(),
        "blocked" | "failed" | "not_opened"
    ) || terminal
    {
        "blocked"
    } else {
        "pending"
    };
    let summary = match state {
        "done" => format!(
            "PR is open against {}.",
            empty_fallback(&job.target_branch, "the requested base")
        ),
        "pending" => "PR publication evidence is still pending.".to_string(),
        _ if job.publication_summary.trim().is_empty() => {
            "Publication was requested, but no open PR URL and target branch evidence are recorded."
                .to_string()
        }
        _ => job.publication_summary.clone(),
    };

    completion_gate(
        "publication",
        "PR publication",
        state,
        summary,
        "github_pr",
        required_evidence_for("github_pr", &["pr_state"]),
        evidence,
    )
}

fn validation_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    let state = match job.validation_status.as_str() {
        "passed" => "done",
        "failed" => "blocked",
        "unavailable" | "not_performed" if terminal => "blocked",
        _ => "pending",
    };
    let summary = match state {
        "done" => "Validation passed.".to_string(),
        "pending" => "Validation evidence is still pending.".to_string(),
        _ => format!(
            "Validation is {}.",
            format_gate_status(&job.validation_status)
        ),
    };

    completion_gate(
        "validation",
        "Validation",
        state,
        summary,
        "local_project",
        required_evidence_for("local_project", &["validation"]),
        Vec::new(),
    )
}

fn browser_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    let evidence = job
        .browser_verification_artifact_ids
        .iter()
        .map(|id| format!("Artifact {id}"))
        .collect::<Vec<_>>();
    let state = match job.browser_verification_status.as_str() {
        "passed" | "not_required" => "done",
        "failed" => "blocked",
        "unavailable" | "not_performed" if terminal || job.publication_status == "blocked" => {
            "blocked"
        }
        "pending" => "pending",
        _ => "pending",
    };
    let summary = match state {
        "done" if job.browser_verification_status == "not_required" => {
            "Browser verification is not required.".to_string()
        }
        "done" => empty_fallback(
            &job.browser_verification_summary,
            "Browser verification passed.",
        )
        .to_string(),
        "pending" => "Browser verification evidence is still pending.".to_string(),
        _ => empty_fallback(
            &job.browser_verification_summary,
            "Browser verification is missing or blocked.",
        )
        .to_string(),
    };

    completion_gate(
        "browser_verification",
        "Browser verification",
        state,
        summary,
        "local_project",
        required_evidence_for("local_project", &["validation"]),
        evidence,
    )
}

fn cleanup_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    let evidence = job
        .cleanup_paths
        .iter()
        .map(|path| format!("Path {path}"))
        .collect::<Vec<_>>();
    let state = match job.cleanup_status.as_str() {
        "clean" | "cleaned" => "done",
        "cleanup_required" => "blocked",
        "unknown" if terminal => "blocked",
        _ => "pending",
    };
    let summary = match state {
        "done" => format!("Cleanup is {}.", format_gate_status(&job.cleanup_status)),
        "pending" => "Cleanup state is still pending.".to_string(),
        _ if job.cleanup_paths.is_empty() => {
            "Cleanup is required or unknown before completion can be claimed.".to_string()
        }
        _ => format!("Cleanup required for {}.", job.cleanup_paths.join(", ")),
    };

    completion_gate(
        "cleanup",
        "Cleanup",
        state,
        summary,
        "local_project",
        required_evidence_for("local_project", &["workspace_context", "changed_files"]),
        evidence,
    )
}

fn task_class_gate(
    job: &JobSummary,
    terminal: bool,
    task_class: &str,
) -> Option<CompletionGateSummary> {
    match task_class {
        "research" => Some(research_gate(job, terminal)),
        "automation" => Some(automation_gate(job, terminal)),
        "local_project" => Some(local_project_gate(job, terminal)),
        "deployment" => Some(deployment_gate(job, terminal)),
        "memory_session" => Some(memory_session_gate(job, terminal)),
        "process_server" => Some(process_server_gate(job, terminal)),
        _ => None,
    }
}

fn has_context_integrity_gate_state(job: &JobSummary) -> bool {
    has_worktree_base_gate_state(job)
        || has_branch_repo_gate_state(job)
        || has_cwd_gate_state(job)
        || has_session_state_gate_state(job)
        || has_target_entity_gate_state(job)
        || has_process_state_gate_state(job)
}

fn has_worktree_base_gate_state(job: &JobSummary) -> bool {
    !matches!(
        job.worktree_base_status.trim(),
        "" | "not_applicable" | "not_applicable_missing_context"
    )
}

fn has_branch_repo_gate_state(job: &JobSummary) -> bool {
    !matches!(
        job.branch_repo_status.trim(),
        "" | "not_applicable" | "not_applicable_missing_context"
    )
}

fn has_cwd_gate_state(job: &JobSummary) -> bool {
    job.command_session_cwd_evidence_json
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn has_session_state_gate_state(job: &JobSummary) -> bool {
    job.metadata_json
        .get("session_state_evidence")
        .is_some_and(|value| value.is_object())
}

fn has_target_entity_gate_state(job: &JobSummary) -> bool {
    job.target_entity_evidence_json
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn has_process_state_gate_state(job: &JobSummary) -> bool {
    job.process_state_evidence_json
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn worktree_base_gate(job: &JobSummary) -> CompletionGateSummary {
    let state = match job.worktree_base_status.as_str() {
        "satisfied" | "waived" => "done",
        "blocked" => "blocked",
        _ => "pending",
    };
    let behind = job
        .worktree_behind_by
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let summary = match job.worktree_base_status.as_str() {
        "satisfied" => format!(
            "Worktree base is fresh against {}.",
            empty_fallback(&job.worktree_base_ref, "the declared base")
        ),
        "waived" => format!(
            "Worktree base freshness is waived{}.",
            prefixed_reason(&job.worktree_base_reason)
        ),
        "blocked" => format!(
            "Worktree is {behind} commit(s) behind {}{}.",
            empty_fallback(&job.worktree_base_ref, "the declared base"),
            prefixed_reason(&job.worktree_base_reason)
        ),
        _ => format!(
            "Worktree base freshness is pending{}.",
            prefixed_reason(&job.worktree_base_reason)
        ),
    };

    completion_gate(
        "worktree_base_fresh",
        "Worktree base freshness",
        state,
        summary,
        "context_integrity",
        required_evidence_for("context_integrity", &["worktree_base_evidence"]),
        worktree_base_evidence(job),
    )
}

fn branch_repo_gate(job: &JobSummary) -> CompletionGateSummary {
    let state = match job.branch_repo_status.as_str() {
        "satisfied" => "done",
        "blocked" => "blocked",
        _ => "pending",
    };
    let summary = match state {
        "done" => "Worktree origin and branch match the declared session project.".to_string(),
        "pending" => format!(
            "Branch/repo consistency is pending{}.",
            prefixed_reason(&job.branch_repo_reason)
        ),
        _ => format!(
            "Worktree origin or branch does not match the declared session project{}.",
            prefixed_reason(&job.branch_repo_reason)
        ),
    };

    completion_gate(
        "branch_repo_consistent",
        "Branch/repo consistency",
        state,
        summary,
        "context_integrity",
        required_evidence_for("context_integrity", &["branch_repo_evidence"]),
        branch_repo_evidence(job),
    )
}

fn cwd_gate(job: &JobSummary) -> CompletionGateSummary {
    let evidence_json = job
        .command_session_cwd_evidence_json
        .as_deref()
        .and_then(parse_context_evidence_json)
        .unwrap_or(Value::Null);
    let status = evidence_status(&evidence_json);
    let reason = evidence_string(&evidence_json, "reason");
    let state = match status.as_str() {
        "satisfied" => "done",
        "blocked" => "blocked",
        _ => "pending",
    };
    let summary = match state {
        "done" => "Command sessions ran under the declared working directory.".to_string(),
        "blocked" => format!(
            "Command session cwd is outside the declared working directory{}.",
            prefixed_reason(&reason)
        ),
        _ => format!(
            "Command session cwd evidence is pending{}.",
            prefixed_reason(&reason)
        ),
    };

    completion_gate(
        "cwd_consistent",
        "cwd consistency",
        state,
        summary,
        "context_integrity",
        required_evidence_for("context_integrity", &["cwd_evidence"]),
        cwd_evidence(&evidence_json),
    )
}

fn session_state_gate(job: &JobSummary) -> CompletionGateSummary {
    let evidence_json = job
        .metadata_json
        .get("session_state_evidence")
        .cloned()
        .unwrap_or(Value::Null);
    let status = evidence_status(&evidence_json);
    let reason = evidence_string(&evidence_json, "reason");
    let state = match status.as_str() {
        "satisfied" => "done",
        "blocked" => "blocked",
        _ => "pending",
    };
    let summary = match state {
        "done" => "Stored session git metadata matches observed worktree state.".to_string(),
        "blocked" => format!(
            "Stored session git metadata drifted from observed worktree state{}.",
            prefixed_reason(&reason)
        ),
        _ => format!(
            "Session git metadata refresh is pending{}.",
            prefixed_reason(&reason)
        ),
    };

    completion_gate(
        "session_state_consistent",
        "Session-state consistency",
        state,
        summary,
        "context_integrity",
        required_evidence_for("context_integrity", &["session_state_evidence"]),
        session_state_evidence(&evidence_json),
    )
}

fn target_entity_gate(job: &JobSummary) -> CompletionGateSummary {
    let evidence_json = job
        .target_entity_evidence_json
        .as_deref()
        .and_then(parse_context_evidence_json)
        .unwrap_or(Value::Null);
    let status = evidence_status(&evidence_json);
    let reason = evidence_string(&evidence_json, "reason");
    let state = match status.as_str() {
        "satisfied" => "done",
        "blocked" => "blocked",
        _ => "pending",
    };
    let summary = match state {
        "done" => format!(
            "Target-entity claims match daemon evidence{}.",
            prefixed_reason(&reason)
        ),
        "blocked" => format!(
            "Target-entity claim has no matching daemon evidence{}.",
            prefixed_reason(&reason)
        ),
        _ => format!(
            "Target-entity evidence is pending{}.",
            prefixed_reason(&reason)
        ),
    };

    completion_gate(
        "target_entity_consistent",
        "Target-entity consistency",
        state,
        summary,
        "context_integrity",
        required_evidence_for("context_integrity", &["target_entity_evidence"]),
        target_entity_evidence(&evidence_json),
    )
}

fn process_state_gate(job: &JobSummary) -> CompletionGateSummary {
    let evidence_json = job
        .process_state_evidence_json
        .as_deref()
        .and_then(parse_context_evidence_json)
        .unwrap_or(Value::Null);
    let status = evidence_status(&evidence_json);
    let reason = evidence_string(&evidence_json, "reason");
    let state = match status.as_str() {
        "satisfied" => "done",
        "blocked" => "blocked",
        _ => "pending",
    };
    let summary = match state {
        "done" => format!(
            "Process and port claims match daemon observations{}.",
            prefixed_reason(&reason)
        ),
        "blocked" => format!(
            "Process or port claim contradicts daemon observation{}.",
            prefixed_reason(&reason)
        ),
        _ => format!(
            "Process and port evidence is pending{}.",
            prefixed_reason(&reason)
        ),
    };

    completion_gate(
        "process_state_consistent",
        "Process-state consistency",
        state,
        summary,
        "context_integrity",
        required_evidence_for("context_integrity", &["process_state_evidence"]),
        process_state_evidence(&evidence_json),
    )
}

fn worktree_base_evidence(job: &JobSummary) -> Vec<String> {
    [
        non_empty_evidence("base_ref", &job.worktree_base_ref),
        non_empty_evidence("head", &job.worktree_head_sha),
        non_empty_evidence("canonical", &job.canonical_base_sha),
        job.worktree_behind_by
            .map(|count| format!("behind_by {count}")),
        non_empty_evidence("origin", &job.worktree_origin_url),
        non_empty_evidence("reason", &job.worktree_base_reason),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn branch_repo_evidence(job: &JobSummary) -> Vec<String> {
    [
        non_empty_evidence("observed_origin", &job.worktree_origin_url),
        non_empty_evidence("expected_origin", &job.expected_origin_url),
        non_empty_evidence("observed_branch", &job.observed_git_branch),
        non_empty_evidence("expected_branch", &job.expected_git_branch),
        non_empty_evidence("reason", &job.branch_repo_reason),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn cwd_evidence(value: &Value) -> Vec<String> {
    let mut evidence = Vec::new();
    push_json_evidence(&mut evidence, "declared_working_dir", value);
    push_json_evidence(&mut evidence, "reason", value);
    if let Some(cwds) = value.get("observed_cwds").and_then(Value::as_array) {
        for observed in cwds {
            let id = evidence_string(observed, "command_session_id");
            let cwd = evidence_string(observed, "cwd");
            if !id.is_empty() || !cwd.is_empty() {
                evidence.push(format!(
                    "observed_cwd {} {}",
                    empty_fallback(&id, "unknown_session"),
                    empty_fallback(&cwd, "unknown_cwd")
                ));
            }
        }
    }
    if let Some(ids) = value
        .get("offending_command_session_ids")
        .and_then(Value::as_array)
    {
        let ids = ids
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            evidence.push(format!("offending_command_session_ids {}", ids.join(", ")));
        }
    }
    if let Some(cwds) = value.get("offending_cwds").and_then(Value::as_array) {
        let cwds = cwds
            .iter()
            .filter_map(Value::as_str)
            .filter(|cwd| !cwd.trim().is_empty())
            .collect::<Vec<_>>();
        if !cwds.is_empty() {
            evidence.push(format!("offending_cwds {}", cwds.join(", ")));
        }
    }
    evidence
}

fn session_state_evidence(value: &Value) -> Vec<String> {
    let mut evidence = Vec::new();
    push_nested_json_evidence(&mut evidence, "stored", "git_head", value);
    push_nested_json_evidence(&mut evidence, "observed", "git_head", value);
    push_nested_json_evidence(&mut evidence, "stored", "git_branch", value);
    push_nested_json_evidence(&mut evidence, "observed", "git_branch", value);
    push_nested_json_evidence(&mut evidence, "stored", "git_dirty", value);
    push_nested_json_evidence(&mut evidence, "observed", "git_dirty", value);
    push_nested_json_evidence(&mut evidence, "stored", "git_untracked_count", value);
    push_nested_json_evidence(&mut evidence, "observed", "git_untracked_count", value);
    push_json_evidence(&mut evidence, "audit_event_id", value);
    push_json_evidence(&mut evidence, "mutation_command_session_id", value);
    push_json_evidence(&mut evidence, "audit_hint", value);
    push_json_evidence(&mut evidence, "reason", value);
    evidence
}

fn target_entity_evidence(value: &Value) -> Vec<String> {
    let mut evidence = Vec::new();
    push_json_evidence(&mut evidence, "reason", value);
    push_claim_evidence(&mut evidence, value);
    evidence
}

fn process_state_evidence(value: &Value) -> Vec<String> {
    let mut evidence = Vec::new();
    push_json_evidence(&mut evidence, "reason", value);
    push_claim_evidence(&mut evidence, value);
    evidence
}

fn push_claim_evidence(evidence: &mut Vec<String>, value: &Value) {
    if let Some(claims) = value.get("claims").and_then(Value::as_array) {
        for claim in claims {
            let claim_text = evidence_string(claim, "claim_text");
            let entity_type = evidence_string(claim, "entity_type");
            let identifier = evidence_string(claim, "identifier");
            let searched = evidence_string(claim, "daemon_evidence_searched");
            let result = evidence_string(claim, "result");
            let reason = evidence_string(claim, "reason");
            let observed = evidence_string(claim, "last_observed_state");
            let observed_at = evidence_string(claim, "observation_timestamp");
            if !claim_text.trim().is_empty() {
                evidence.push(format!("claim_text {claim_text}"));
            }
            if !entity_type.trim().is_empty() || !identifier.trim().is_empty() {
                evidence.push(format!(
                    "entity {} {}",
                    empty_fallback(&entity_type, "unknown"),
                    empty_fallback(&identifier, "unknown")
                ));
            }
            if !searched.trim().is_empty() {
                evidence.push(format!("daemon_evidence_searched {searched}"));
            }
            if !result.trim().is_empty() {
                evidence.push(format!("result {result}"));
            }
            if !reason.trim().is_empty() {
                evidence.push(format!("reason {reason}"));
            }
            if !observed.trim().is_empty() {
                evidence.push(format!("last_observed_state {observed}"));
            }
            if !observed_at.trim().is_empty() {
                evidence.push(format!("observation_timestamp {observed_at}"));
            }
        }
    }
}

fn parse_context_evidence_json(value: &str) -> Option<Value> {
    serde_json::from_str(value).ok()
}

fn evidence_status(value: &Value) -> String {
    evidence_string(value, "status")
}

fn evidence_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|item| {
            item.as_str()
                .map(ToString::to_string)
                .or_else(|| item.as_bool().map(|flag| flag.to_string()))
                .or_else(|| item.as_i64().map(|number| number.to_string()))
        })
        .unwrap_or_default()
}

fn push_json_evidence(evidence: &mut Vec<String>, key: &str, value: &Value) {
    let item = evidence_string(value, key);
    if !item.trim().is_empty() {
        evidence.push(format!("{key} {item}"));
    }
}

fn push_nested_json_evidence(evidence: &mut Vec<String>, object: &str, key: &str, value: &Value) {
    let item = value
        .get(object)
        .map(|nested| evidence_string(nested, key))
        .unwrap_or_default();
    if !item.trim().is_empty() {
        evidence.push(format!("{object}.{key} {item}"));
    }
}

fn prefixed_reason(reason: &str) -> String {
    if reason.trim().is_empty() {
        String::new()
    } else {
        format!(": {}", reason.trim())
    }
}

fn research_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    evidence_contract_gate(
        "research_evidence",
        "Research evidence",
        "research",
        &["fresh_sources", "source_quality", "contradictions"],
        job,
        terminal,
        "Research sources, quality, and contradiction checks are recorded.",
        "Research source evidence is still pending.",
        "Research completion was claimed without captured fresh sources, source quality, and contradiction-check evidence.",
    )
}

fn automation_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    evidence_contract_gate(
        "automation_evidence",
        "Automation evidence",
        "automation",
        &["schedule_state"],
        job,
        terminal,
        "Automation schedule evidence is recorded.",
        "Automation schedule evidence is still pending.",
        "Automation setup was claimed without a daemon-owned schedule record.",
    )
}

fn local_project_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    let failed = evidence_with_prefix(job, "failed_command");
    let waived = has_evidence(job, "waiver:command_failure");
    let has_validation = job.validation_status == "passed" || has_evidence(job, "validation");
    let state = if !failed.is_empty() && !waived {
        "blocked"
    } else if has_validation {
        "done"
    } else if terminal || job.validation_status == "failed" {
        "blocked"
    } else {
        "pending"
    };
    let summary = match state {
        "done" => "Local validation evidence is recorded.".to_string(),
        "pending" => "Local validation evidence is still pending.".to_string(),
        _ if !failed.is_empty() && !waived => {
            format!(
                "Recent command failure blocks completion: {}.",
                failed.join(", ")
            )
        }
        _ => "Local project completion was claimed without successful validation evidence."
            .to_string(),
    };

    completion_gate(
        "local_project_evidence",
        "Local project evidence",
        state,
        summary,
        "local_project",
        required_evidence_for("local_project", &["validation"]),
        evidence_for_requirements(job, &["validation", "failed_command", "waiver"]),
    )
}

fn deployment_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    let has_deployment = has_evidence(job, "deployment_status");
    let has_verification = has_any_evidence(
        job,
        &["health", "version", "waiver:deployment_verification"],
    );
    let state = if has_deployment && has_verification {
        "done"
    } else if terminal {
        "blocked"
    } else {
        "pending"
    };
    let summary = match state {
        "done" => {
            "Deployment and post-deploy verification evidence are recorded.".to_string()
        }
        "pending" => "Deployment verification evidence is still pending.".to_string(),
        _ => "Deployment was claimed without a successful deployment command and post-deploy verification evidence.".to_string(),
    };

    completion_gate(
        "deployment_evidence",
        "Deployment evidence",
        state,
        summary,
        "deployment",
        required_evidence_for("deployment", &["deployment_status", "version", "health"]),
        evidence_for_requirements(
            job,
            &[
                "deployment_status",
                "version",
                "health",
                "waiver:deployment_verification",
            ],
        ),
    )
}

fn memory_session_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    evidence_contract_gate(
        "memory_session_evidence",
        "Memory/session evidence",
        "memory_session",
        &["target_scope", "operation_result"],
        job,
        terminal,
        "Memory/session operation receipts are recorded.",
        "Memory/session operation evidence is still pending.",
        "Memory/session changes were claimed without matching daemon-side write or delete receipts.",
    )
}

fn process_server_gate(job: &JobSummary, terminal: bool) -> CompletionGateSummary {
    evidence_contract_gate(
        "process_server_evidence",
        "Process/server evidence",
        "process_server",
        &["process_state", "port_state"],
        job,
        terminal,
        "Process/server state evidence is recorded.",
        "Process/server state evidence is still pending.",
        "Process/server completion was claimed without observable process or port state evidence.",
    )
}

#[allow(clippy::too_many_arguments)]
fn evidence_contract_gate(
    id: &str,
    title: &str,
    task_class: &str,
    requirement_ids: &[&str],
    job: &JobSummary,
    terminal: bool,
    done_summary: &str,
    pending_summary: &str,
    blocked_summary: &str,
) -> CompletionGateSummary {
    let missing = requirement_ids
        .iter()
        .filter(|requirement_id| !has_evidence(job, requirement_id))
        .copied()
        .collect::<Vec<_>>();
    let state = if missing.is_empty() {
        "done"
    } else if terminal {
        "blocked"
    } else {
        "pending"
    };
    let summary = match state {
        "done" => done_summary.to_string(),
        "pending" => pending_summary.to_string(),
        _ => blocked_summary.to_string(),
    };

    completion_gate(
        id,
        title,
        state,
        summary,
        task_class,
        required_evidence_for(task_class, requirement_ids),
        evidence_for_requirements(job, requirement_ids),
    )
}

fn completion_gate(
    id: &str,
    title: &str,
    state: &str,
    summary: String,
    task_class: &str,
    required_evidence: Vec<String>,
    evidence: Vec<String>,
) -> CompletionGateSummary {
    CompletionGateSummary {
        id: id.to_string(),
        title: title.to_string(),
        state: state.to_string(),
        summary,
        task_class: task_class.to_string(),
        required_evidence,
        evidence,
    }
}

fn non_empty_evidence(label: &str, value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(format!("{label} {value}"))
    }
}

fn normalized_task_class(job: &JobSummary) -> Option<&str> {
    job.task_class
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn has_evidence(job: &JobSummary, requirement_id: &str) -> bool {
    let prefix = format!("{requirement_id}:");
    job.task_evidence
        .iter()
        .any(|evidence| evidence == requirement_id || evidence.starts_with(&prefix))
}

fn has_any_evidence(job: &JobSummary, requirement_ids: &[&str]) -> bool {
    requirement_ids
        .iter()
        .any(|requirement_id| has_evidence(job, requirement_id))
}

fn evidence_with_prefix(job: &JobSummary, requirement_id: &str) -> Vec<String> {
    job.task_evidence
        .iter()
        .filter_map(|evidence| evidence.strip_prefix(requirement_id))
        .map(str::trim)
        .filter(|evidence| !evidence.is_empty())
        .map(str::to_string)
        .collect()
}

fn evidence_for_requirements(job: &JobSummary, requirement_ids: &[&str]) -> Vec<String> {
    job.task_evidence
        .iter()
        .filter(|evidence| {
            requirement_ids.iter().any(|requirement_id| {
                evidence.as_str() == *requirement_id
                    || evidence.starts_with(&format!("{requirement_id}:"))
            })
        })
        .cloned()
        .collect()
}

fn empty_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn format_gate_status(status: &str) -> String {
    status.replace('_', " ")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    pub route_id: String,
    #[serde(default)]
    pub route_title: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_until_json: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_started_at: Option<i64>,
    #[serde(default)]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_error: Option<UserFacingErrorSummary>,
    #[serde(default)]
    pub capabilities: Vec<ToolCapabilitySummary>,
    #[serde(default)]
    pub last_reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_at: Option<i64>,
    #[serde(default)]
    pub token_usage_known: bool,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_estimate: Option<f64>,
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
    #[serde(default)]
    pub metadata_json: serde_json::Value,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    #[serde(default)]
    pub memory_outcomes: Vec<MemoryOutcome>,
    pub created_at: i64,
}

/// Request to create a session.
///
/// New clients should send `attachment_mode`:
/// - `new_worktree`
/// - `project_root`
/// - `scratch`
///
/// `workspace_mode` is retained for legacy clients and maps as follows:
/// - `isolated_worktree` <-> `new_worktree`
/// - `shared_project_root` <-> `project_root`
/// - `scratch_only` <-> `scratch`
///
/// When both fields are present, the daemon rejects requests whose values do
/// not describe the same mode.
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
    pub attachment_mode: Option<String>,
    pub workspace_mode: Option<String>,
    pub branch_name: Option<String>,
}

fn default_workspace_mode() -> String {
    "shared_project_root".to_string()
}

fn default_attachment_mode() -> String {
    "project_root".to_string()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
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
fn default_mcp_invocation_status() -> String {
    "unknown".to_string()
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
    #[serde(default = "default_mcp_invocation_status")]
    pub invocation_status: String,
    #[serde(default)]
    pub invocation_message: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySearchResult {
    pub entry: MemoryEntry,
    pub rank: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySearchResponse {
    #[serde(default)]
    pub results: Vec<MemorySearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOutcome {
    pub kind: String,
    pub state: String,
    #[serde(default)]
    pub memory_id: String,
    #[serde(default)]
    pub candidate_id: String,
    #[serde(default)]
    pub dedupe_key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryCandidate {
    pub id: String,
    pub scope_kind: String,
    pub scope_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub turn_id_start: String,
    #[serde(default)]
    pub turn_id_end: String,
    #[serde(default = "default_memory_kind")]
    pub candidate_kind: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default = "default_candidate_status")]
    pub status: String,
    #[serde(default)]
    pub dedupe_key: String,
    #[serde(default)]
    pub accepted_memory_id: String,
    #[serde(default = "default_candidate_created_by")]
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub metadata_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryCandidateUpsertRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub scope_kind: String,
    pub scope_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turn_id_start: Option<String>,
    #[serde(default)]
    pub turn_id_end: Option<String>,
    #[serde(default)]
    pub candidate_kind: Option<String>,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub accepted_memory_id: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub metadata_json: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryCandidateAcceptRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub memory_kind: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub metadata_json: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryCandidateListResponse {
    #[serde(default)]
    pub candidates: Vec<MemoryCandidate>,
}

fn default_candidate_status() -> String {
    "pending".to_string()
}
fn default_candidate_created_by() -> String {
    "utility_worker".to_string()
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
pub struct VaultSecretPolicySummary {
    pub id: String,
    pub secret_id: String,
    pub consumer_kind: String,
    pub consumer_id: String,
    pub permission: String,
    pub approval_mode: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSecretPolicyListResponse {
    #[serde(default)]
    pub policies: Vec<VaultSecretPolicySummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSecretPolicyUpsertRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub consumer_kind: String,
    pub consumer_id: String,
    pub permission: String,
    pub approval_mode: String,
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
    #[serde(default)]
    pub origin_url: String,
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
pub struct InstanceLogEntry {
    pub id: i64,
    pub timestamp: i64,
    pub level: String,
    pub category: String,
    pub source: String,
    pub event: String,
    pub message: String,
    pub related_ids: Value,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceLogCategorySummary {
    pub category: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstanceLogListResponse {
    pub records: Vec<InstanceLogEntry>,
    pub categories: Vec<InstanceLogCategorySummary>,
    pub logs_dir: String,
    pub retention: String,
    pub next_before: Option<i64>,
    pub next_before_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceLogCategoriesResponse {
    pub categories: Vec<InstanceLogCategorySummary>,
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
    #[serde(default)]
    pub logs_dir: String,
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
    #[serde(default)]
    pub bind_mode: String,
    #[serde(default)]
    pub bind_mode_label: String,
    #[serde(default)]
    pub recommended_bind: Option<String>,
    #[serde(default)]
    pub vault_origin_requirement: String,
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
    #[serde(rename = "job.blocked")]
    JobBlocked(JobSummary),
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
    #[serde(rename = "browser.frame")]
    BrowserFrame(BrowserFrameEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserFrameEvent {
    pub session_id: String,
    pub page_id: String,
    pub mime: String,
    pub image: String,
    pub url: String,
    pub title: String,
    pub captured_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserPageSummary {
    pub id: String,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub error: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserContextSummary {
    pub session_id: String,
    pub active_page_id: Option<String>,
    pub pages: Vec<BrowserPageSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserNavigateRequest {
    pub url: String,
    #[serde(default)]
    pub page_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserActionRequest {
    pub action: String,
    #[serde(default)]
    pub page_id: Option<String>,
    #[serde(default)]
    pub target_ref: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub snapshot: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSnapshotRef {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub selector: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserDownload {
    pub id: String,
    pub page_id: String,
    pub url: String,
    pub suggested_filename: String,
    pub path: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSnapshot {
    pub session_id: String,
    pub page_id: String,
    pub url: String,
    pub title: String,
    pub content: String,
    pub refs: Vec<BrowserSnapshotRef>,
    #[serde(default)]
    pub downloads: Vec<BrowserDownload>,
    pub screenshot_data_url: String,
    pub captured_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn job_summary_serializes_browser_verification_fields() {
        let summary = JobSummary {
            id: "job-1".to_string(),
            session_id: Some("session-1".to_string()),
            parent_job_id: None,
            template_id: None,
            task_class: None,
            title: "UI job".to_string(),
            purpose: "test".to_string(),
            trigger_kind: "session_prompt".to_string(),
            state: "completed".to_string(),
            requested_by: "user".to_string(),
            prompt_excerpt: "fix layout".to_string(),
            root_worker_id: Some("worker-1".to_string()),
            executor_lane: "utility".to_string(),
            executor_provider: "openai_compatible".to_string(),
            executor_model: "gpt-5.4-mini".to_string(),
            executor_route_id: String::new(),
            executor_route_title: String::new(),
            visible_turn_id: Some("turn-1".to_string()),
            result_summary: "done".to_string(),
            last_error: String::new(),
            user_error: None,
            ui_renderable: "true".to_string(),
            browser_verification_required: true,
            browser_verification_status: "passed".to_string(),
            browser_verification_summary: "Verified dropdown clickability.".to_string(),
            browser_verification_artifact_ids: vec!["artifact-1".to_string()],
            publication_requested: false,
            publication_status: "not_requested".to_string(),
            publication_summary: String::new(),
            pr_url: String::new(),
            source_branch: String::new(),
            target_branch: String::new(),
            validation_status: "not_performed".to_string(),
            cleanup_status: "unknown".to_string(),
            cleanup_paths: Vec::new(),
            task_evidence: Vec::new(),
            metadata_json: json!({}),
            worktree_base_ref: String::new(),
            worktree_base_status: String::new(),
            worktree_base_reason: String::new(),
            worktree_origin_url: String::new(),
            expected_origin_url: String::new(),
            observed_git_branch: String::new(),
            expected_git_branch: String::new(),
            worktree_head_sha: String::new(),
            canonical_base_sha: String::new(),
            worktree_behind_by: None,
            branch_repo_status: String::new(),
            branch_repo_reason: String::new(),
            command_session_cwd_evidence_json: None,
            target_entity_evidence_json: None,
            process_state_evidence_json: None,
            session_state_observed_at: None,
            completion_status: String::new(),
            completion_gates: Vec::new(),
            completion_blockers: Vec::new(),
            worker_count: 1,
            pending_approval_count: 0,
            artifact_count: 1,
            last_resumed_at: None,
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 1,
            updated_at: 2,
        };

        let value = serde_json::to_value(&summary).expect("summary should serialize");
        assert_eq!(value["ui_renderable"], "true");
        assert_eq!(value["browser_verification_required"], true);
        assert_eq!(value["browser_verification_status"], "passed");
        assert_eq!(value["browser_verification_artifact_ids"][0], "artifact-1");
    }

    #[test]
    fn publication_completion_gates_block_missing_pr_evidence() {
        let summary = JobSummary {
            id: "job-1".to_string(),
            session_id: Some("session-1".to_string()),
            parent_job_id: None,
            template_id: None,
            task_class: Some("github_pr".to_string()),
            title: "Publish job".to_string(),
            purpose: "test".to_string(),
            trigger_kind: "session_prompt".to_string(),
            state: "blocked".to_string(),
            requested_by: "user".to_string(),
            prompt_excerpt: "open a pr to merge to dev".to_string(),
            root_worker_id: Some("worker-1".to_string()),
            executor_lane: "utility".to_string(),
            executor_provider: "openai_compatible".to_string(),
            executor_model: "gpt-5.4-mini".to_string(),
            executor_route_id: String::new(),
            executor_route_title: String::new(),
            visible_turn_id: Some("turn-1".to_string()),
            result_summary: "Done.".to_string(),
            last_error: String::new(),
            user_error: None,
            ui_renderable: "unknown".to_string(),
            browser_verification_required: false,
            browser_verification_status: "not_performed".to_string(),
            browser_verification_summary: String::new(),
            browser_verification_artifact_ids: Vec::new(),
            publication_requested: true,
            publication_status: "blocked".to_string(),
            publication_summary: String::new(),
            pr_url: String::new(),
            source_branch: "weblime/issue-270".to_string(),
            target_branch: "dev".to_string(),
            validation_status: "passed".to_string(),
            cleanup_status: "clean".to_string(),
            cleanup_paths: Vec::new(),
            task_evidence: Vec::new(),
            metadata_json: json!({}),
            worktree_base_ref: String::new(),
            worktree_base_status: String::new(),
            worktree_base_reason: String::new(),
            worktree_origin_url: String::new(),
            expected_origin_url: String::new(),
            observed_git_branch: String::new(),
            expected_git_branch: String::new(),
            worktree_head_sha: String::new(),
            canonical_base_sha: String::new(),
            worktree_behind_by: None,
            branch_repo_status: String::new(),
            branch_repo_reason: String::new(),
            command_session_cwd_evidence_json: None,
            target_entity_evidence_json: None,
            process_state_evidence_json: None,
            session_state_observed_at: None,
            completion_status: String::new(),
            completion_gates: Vec::new(),
            completion_blockers: Vec::new(),
            worker_count: 1,
            pending_approval_count: 0,
            artifact_count: 0,
            last_resumed_at: None,
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 1,
            updated_at: 2,
        }
        .with_completion_gates();

        assert_eq!(summary.completion_status, "blocked");
        assert!(summary.has_blocking_completion_gates());
        assert!(
            summary
                .completion_gates
                .iter()
                .any(|gate| gate.id == "publication" && gate.state == "blocked")
        );
        let publication_gate = summary
            .completion_gates
            .iter()
            .find(|gate| gate.id == "publication")
            .expect("publication gate should be present");
        assert_eq!(publication_gate.task_class, "github_pr");
        assert_eq!(
            publication_gate.required_evidence,
            vec!["Direct PR state".to_string()]
        );
        assert!(
            summary
                .completion_gates
                .iter()
                .any(|gate| gate.id == "validation" && gate.state == "done")
        );
    }

    #[test]
    fn task_evidence_contract_catalog_covers_grounding_classes() {
        let catalog = task_evidence_contract_catalog();
        for task_class in [
            "context_integrity",
            "github_pr",
            "research",
            "automation",
            "local_project",
            "deployment",
            "memory_session",
            "process_server",
        ] {
            let contract = catalog
                .iter()
                .find(|contract| contract.task_class == task_class)
                .unwrap_or_else(|| panic!("{task_class} contract should be present"));
            assert!(contract.requirements.len() >= 2);
        }
    }

    #[test]
    fn context_integrity_gates_cover_happy_blocked_pending_and_compatibility_paths() {
        let fresh = JobSummary {
            worktree_base_status: "satisfied".to_string(),
            worktree_base_ref: "dev".to_string(),
            worktree_head_sha: "head".to_string(),
            canonical_base_sha: "head".to_string(),
            worktree_behind_by: Some(0),
            branch_repo_status: "satisfied".to_string(),
            worktree_origin_url: "git@github.com:WebLime-agency/nucleus.git".to_string(),
            expected_origin_url: "https://github.com/WebLime-agency/nucleus".to_string(),
            observed_git_branch: "feature".to_string(),
            expected_git_branch: "feature".to_string(),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        assert!(
            fresh
                .completion_gates
                .iter()
                .any(|gate| { gate.id == "worktree_base_fresh" && gate.state == "done" })
        );
        assert!(
            fresh
                .completion_gates
                .iter()
                .any(|gate| { gate.id == "branch_repo_consistent" && gate.state == "done" })
        );
        let cwd_ok = JobSummary {
            command_session_cwd_evidence_json: Some(
                json!({
                    "status": "satisfied",
                    "declared_working_dir": "/repo",
                    "observed_cwds": [{"command_session_id": "cmd-1", "cwd": "/repo/crate"}],
                    "offending_command_session_ids": [],
                    "offending_cwds": [],
                })
                .to_string(),
            ),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        assert!(
            cwd_ok
                .completion_gates
                .iter()
                .any(|gate| gate.id == "cwd_consistent" && gate.state == "done")
        );

        let stale = JobSummary {
            state: "blocked".to_string(),
            worktree_base_status: "blocked".to_string(),
            worktree_base_ref: "dev".to_string(),
            worktree_head_sha: "old".to_string(),
            canonical_base_sha: "new".to_string(),
            worktree_behind_by: Some(2),
            worktree_base_reason: "behind canonical by 2 commit(s)".to_string(),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        let stale_gate = stale
            .completion_gates
            .iter()
            .find(|gate| gate.id == "worktree_base_fresh")
            .expect("stale base gate should be derived");
        assert_eq!(stale_gate.state, "blocked");
        assert!(stale_gate.evidence.iter().any(|item| item == "head old"));
        assert!(
            stale_gate
                .evidence
                .iter()
                .any(|item| item == "canonical new")
        );
        assert!(stale_gate.evidence.iter().any(|item| item == "behind_by 2"));

        let mismatch = JobSummary {
            state: "blocked".to_string(),
            branch_repo_status: "blocked".to_string(),
            branch_repo_reason: "origin URL mismatch; branch mismatch".to_string(),
            worktree_origin_url: "git@github.com:other/repo.git".to_string(),
            expected_origin_url: "https://github.com/WebLime-agency/nucleus".to_string(),
            observed_git_branch: "other".to_string(),
            expected_git_branch: "feature".to_string(),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        let mismatch_gate = mismatch
            .completion_gates
            .iter()
            .find(|gate| gate.id == "branch_repo_consistent")
            .expect("branch/repo gate should be derived");
        assert_eq!(mismatch_gate.state, "blocked");
        assert!(
            mismatch_gate
                .evidence
                .iter()
                .any(|item| item == "observed_origin git@github.com:other/repo.git")
        );
        assert!(
            mismatch_gate
                .evidence
                .iter()
                .any(|item| item == "expected_branch feature")
        );
        let cwd_blocked = JobSummary {
            state: "blocked".to_string(),
            command_session_cwd_evidence_json: Some(
                json!({
                    "status": "blocked",
                    "reason": "one or more command sessions ran outside the declared working_dir",
                    "declared_working_dir": "/repo",
                    "observed_cwds": [{"command_session_id": "cmd-1", "cwd": "/tmp/outside"}],
                    "offending_command_session_ids": ["cmd-1"],
                    "offending_cwds": ["/tmp/outside"],
                })
                .to_string(),
            ),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        let cwd_gate = cwd_blocked
            .completion_gates
            .iter()
            .find(|gate| gate.id == "cwd_consistent")
            .expect("cwd gate should be derived");
        assert_eq!(cwd_gate.state, "blocked");
        assert!(
            cwd_gate
                .evidence
                .iter()
                .any(|item| { item == "offending_command_session_ids cmd-1" })
        );

        let session_state_blocked = JobSummary {
            state: "blocked".to_string(),
            session_state_observed_at: Some(123),
            metadata_json: json!({
                "session_state_evidence": {
                    "status": "blocked",
                    "reason": "stored session git metadata differed from observed disk state",
                    "stored": {"git_head": "old", "git_branch": "dev", "git_dirty": false, "git_untracked_count": 0},
                    "observed": {"git_head": "new", "git_branch": "dev", "git_dirty": false, "git_untracked_count": 0},
                    "audit_hint": "inspect audit_events"
                }
            }),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        let session_state_gate = session_state_blocked
            .completion_gates
            .iter()
            .find(|gate| gate.id == "session_state_consistent")
            .expect("session-state gate should be derived");
        assert_eq!(session_state_gate.state, "blocked");
        assert!(
            session_state_gate
                .evidence
                .iter()
                .any(|item| item == "stored.git_head old")
        );
        assert!(
            session_state_gate
                .evidence
                .iter()
                .any(|item| item == "observed.git_head new")
        );

        let target_entity_blocked = JobSummary {
            state: "blocked".to_string(),
            target_entity_evidence_json: Some(
                json!({
                    "status": "blocked",
                    "reason": "one or more target-entity claims have no daemon evidence",
                    "claims": [{
                        "claim_text": "created file missing.txt",
                        "entity_type": "file",
                        "identifier": "missing.txt",
                        "result": "blocked",
                        "daemon_evidence_searched": "filesystem metadata",
                        "reason": "claimed file does not exist"
                    }]
                })
                .to_string(),
            ),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        let target_gate = target_entity_blocked
            .completion_gates
            .iter()
            .find(|gate| gate.id == "target_entity_consistent")
            .expect("target-entity gate should be derived");
        assert_eq!(target_gate.state, "blocked");
        assert!(
            target_gate
                .required_evidence
                .iter()
                .any(|item| item == "Target-entity evidence")
        );
        assert!(
            target_gate
                .evidence
                .iter()
                .any(|item| item == "claim_text created file missing.txt")
        );
        assert!(
            target_gate
                .evidence
                .iter()
                .any(|item| item == "entity file missing.txt")
        );

        let process_state_pending = JobSummary {
            process_state_evidence_json: Some(
                json!({
                    "status": "pending",
                    "reason": "process or port evidence is still pending",
                    "claims": [{
                        "claim_text": "Browser is ready",
                        "entity_type": "browser_sidecar",
                        "identifier": "browser_sidecar",
                        "result": "pending",
                        "daemon_evidence_searched": "browser sidecar process handle",
                        "reason": "no observation recorded"
                    }]
                })
                .to_string(),
            ),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        let process_gate = process_state_pending
            .completion_gates
            .iter()
            .find(|gate| gate.id == "process_state_consistent")
            .expect("process-state gate should be derived");
        assert_eq!(process_gate.state, "pending");
        assert!(
            process_gate
                .required_evidence
                .iter()
                .any(|item| item == "Process-state evidence")
        );

        let pending = JobSummary {
            worktree_base_status: "pending".to_string(),
            worktree_base_reason: "could not reach canonical: auth failed".to_string(),
            ..task_class_job("local_project", vec!["validation".to_string()])
        }
        .with_completion_gates();
        assert!(
            pending
                .completion_gates
                .iter()
                .any(|gate| { gate.id == "worktree_base_fresh" && gate.state == "pending" })
        );

        let backward_compatible =
            task_class_job("local_project", vec!["validation".to_string()]).with_completion_gates();
        assert!(
            !backward_compatible
                .completion_gates
                .iter()
                .any(|gate| matches!(
                    gate.id.as_str(),
                    "worktree_base_fresh"
                        | "branch_repo_consistent"
                        | "cwd_consistent"
                        | "session_state_consistent"
                        | "target_entity_consistent"
                        | "process_state_consistent"
                ))
        );
    }

    #[test]
    fn task_class_completion_gates_cover_happy_and_blocked_paths() {
        let cases = [
            (
                "research",
                vec![
                    "fresh_sources:https://example.test/source".to_string(),
                    "source_quality:primary source".to_string(),
                    "contradictions:checked alternate sources".to_string(),
                ],
                "research_evidence",
            ),
            (
                "automation",
                vec!["schedule_state:automation task created".to_string()],
                "automation_evidence",
            ),
            (
                "local_project",
                vec!["validation:cargo test passed".to_string()],
                "local_project_evidence",
            ),
            (
                "deployment",
                vec![
                    "deployment_status:deploy command passed".to_string(),
                    "health:post-deploy endpoint returned 200".to_string(),
                ],
                "deployment_evidence",
            ),
            (
                "memory_session",
                vec![
                    "target_scope:session".to_string(),
                    "operation_result:memory write receipt".to_string(),
                ],
                "memory_session_evidence",
            ),
            (
                "process_server",
                vec![
                    "process_state:restart completed".to_string(),
                    "port_state:listener observed".to_string(),
                ],
                "process_server_evidence",
            ),
        ];

        for (task_class, evidence, gate_id) in cases {
            let satisfied = task_class_job(task_class, evidence.clone()).with_completion_gates();
            assert_eq!(satisfied.completion_status, "satisfied", "{task_class}");
            assert!(satisfied.completion_gates.iter().any(|gate| {
                gate.id == gate_id && gate.state == "done" && gate.task_class == task_class
            }));

            let blocked = task_class_job(task_class, Vec::new()).with_completion_gates();
            assert_eq!(blocked.completion_status, "blocked", "{task_class}");
            assert!(blocked.completion_blockers.iter().any(|blocker| {
                blocker.to_ascii_lowercase().contains("without")
                    || blocker.to_ascii_lowercase().contains("missing")
            }));
        }
    }

    #[test]
    fn task_class_none_derives_no_extra_gates() {
        let summary = task_class_job("research", Vec::new()).with_completion_gates();
        let mut ungated = summary.clone();
        ungated.task_class = None;
        ungated.completion_status.clear();
        ungated.completion_gates.clear();
        ungated.completion_blockers.clear();

        let ungated = ungated.with_completion_gates();
        assert_eq!(ungated.completion_status, "not_gated");
        assert!(ungated.completion_gates.is_empty());
    }

    #[test]
    fn explicit_github_pr_task_class_derives_publication_gates() {
        let summary = task_class_job("github_pr", Vec::new()).with_completion_gates();
        assert_eq!(summary.completion_status, "blocked");
        assert!(
            summary
                .completion_gates
                .iter()
                .any(|gate| gate.id == "publication" && gate.state == "blocked")
        );
        assert!(
            summary
                .completion_gates
                .iter()
                .any(|gate| gate.id == "validation" && gate.state == "blocked")
        );
    }

    #[test]
    fn deployment_gate_requires_deploy_scoped_verification_waiver() {
        let command_failure_waiver = task_class_job(
            "deployment",
            vec![
                "deployment_status:wrangler deploy passed".to_string(),
                "waiver:command_failure".to_string(),
            ],
        )
        .with_completion_gates();
        assert_eq!(command_failure_waiver.completion_status, "blocked");

        let deployment_waiver = task_class_job(
            "deployment",
            vec![
                "deployment_status:wrangler deploy passed".to_string(),
                "waiver:deployment_verification".to_string(),
            ],
        )
        .with_completion_gates();
        assert_eq!(deployment_waiver.completion_status, "satisfied");
    }

    fn task_class_job(task_class: &str, task_evidence: Vec<String>) -> JobSummary {
        JobSummary {
            id: format!("{task_class}-job"),
            session_id: Some("session-1".to_string()),
            parent_job_id: None,
            template_id: None,
            task_class: Some(task_class.to_string()),
            title: format!("{task_class} job"),
            purpose: "test".to_string(),
            trigger_kind: "session_prompt".to_string(),
            state: "completed".to_string(),
            requested_by: "user".to_string(),
            prompt_excerpt: "prompt".to_string(),
            root_worker_id: None,
            executor_lane: String::new(),
            executor_provider: String::new(),
            executor_model: String::new(),
            executor_route_id: String::new(),
            executor_route_title: String::new(),
            visible_turn_id: None,
            result_summary: "done".to_string(),
            last_error: String::new(),
            user_error: None,
            ui_renderable: "unknown".to_string(),
            browser_verification_required: false,
            browser_verification_status: "not_required".to_string(),
            browser_verification_summary: String::new(),
            browser_verification_artifact_ids: Vec::new(),
            publication_requested: false,
            publication_status: "not_requested".to_string(),
            publication_summary: String::new(),
            pr_url: String::new(),
            source_branch: String::new(),
            target_branch: String::new(),
            validation_status: "not_performed".to_string(),
            cleanup_status: "unknown".to_string(),
            cleanup_paths: Vec::new(),
            task_evidence,
            metadata_json: json!({}),
            worktree_base_ref: String::new(),
            worktree_base_status: String::new(),
            worktree_base_reason: String::new(),
            worktree_origin_url: String::new(),
            expected_origin_url: String::new(),
            observed_git_branch: String::new(),
            expected_git_branch: String::new(),
            worktree_head_sha: String::new(),
            canonical_base_sha: String::new(),
            worktree_behind_by: None,
            branch_repo_status: String::new(),
            branch_repo_reason: String::new(),
            command_session_cwd_evidence_json: None,
            target_entity_evidence_json: None,
            process_state_evidence_json: None,
            session_state_observed_at: None,
            completion_status: String::new(),
            completion_gates: Vec::new(),
            completion_blockers: Vec::new(),
            worker_count: 0,
            pending_approval_count: 0,
            artifact_count: 0,
            last_resumed_at: None,
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn job_summary_accepts_missing_user_error() {
        let value = json!({
            "id": "job-1",
            "session_id": "session-1",
            "parent_job_id": null,
            "template_id": null,
            "title": "Utility job",
            "purpose": "Test",
            "trigger_kind": "manual",
            "state": "failed",
            "requested_by": "test",
            "prompt_excerpt": "prompt",
            "root_worker_id": null,
            "visible_turn_id": null,
            "result_summary": "",
            "last_error": "raw error",
            "ui_renderable": "unknown",
            "browser_verification_required": false,
            "browser_verification_status": "not_required",
            "browser_verification_summary": "",
            "browser_verification_artifact_ids": [],
            "worker_count": 1,
            "pending_approval_count": 0,
            "artifact_count": 0,
            "created_at": 1,
            "updated_at": 1
        });

        let summary: JobSummary = serde_json::from_value(value).expect("summary should parse");
        assert_eq!(summary.last_error, "raw error");
        assert!(summary.user_error.is_none());
    }

    #[test]
    fn job_summary_accepts_user_error_metadata() {
        let value = json!({
            "id": "job-1",
            "session_id": "session-1",
            "parent_job_id": null,
            "template_id": null,
            "title": "Utility job",
            "purpose": "Test",
            "trigger_kind": "manual",
            "state": "failed",
            "requested_by": "test",
            "prompt_excerpt": "prompt",
            "root_worker_id": null,
            "visible_turn_id": null,
            "result_summary": "",
            "last_error": "raw error",
            "user_error": {
                "code": "model_credentials_missing",
                "title": "Nucleus needs model credentials",
                "message": "Set up your Base model and Utility model in Profiles, then retry this job.",
                "actions": ["open_profiles", "retry_job"],
                "technical_detail": "raw error"
            },
            "ui_renderable": "unknown",
            "browser_verification_required": false,
            "browser_verification_status": "not_required",
            "browser_verification_summary": "",
            "browser_verification_artifact_ids": [],
            "worker_count": 1,
            "pending_approval_count": 0,
            "artifact_count": 0,
            "created_at": 1,
            "updated_at": 1
        });

        let summary: JobSummary = serde_json::from_value(value).expect("summary should parse");
        let user_error = summary.user_error.expect("friendly error should parse");
        assert_eq!(user_error.code, "model_credentials_missing");
        assert!(
            user_error
                .actions
                .iter()
                .any(|action| action == "open_profiles")
        );
    }
}
