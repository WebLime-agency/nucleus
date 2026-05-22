use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::ErrorKind,
    net::TcpListener as StdTcpListener,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex as StdMutex},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use nucleus_protocol::{
    ApprovalRequestSummary, ArtifactSummary, BrowserActionRequest, BrowserNavigateRequest,
    BrowserSnapshot, CommandSessionSummary, CompiledTurn, CreatePlaybookRequest, DaemonEvent,
    JobDetail, JobSummary, McpServerRecord, McpToolRecord, MemoryOutcome, PlaybookDetail,
    PlaybookSummary, PromptProgressUpdate, RunBudgetSummary, SessionDetail, SessionPromptRequest,
    SessionSummary, SessionTurn, SessionTurnImage, UpdatePlaybookRequest, WorkerSummary,
    WorkspaceProfileSummary, WorkspaceSummary,
};
use nucleus_storage::{
    ApprovalRequestRecord, AuditEventRecord, CommandSessionPatch, CommandSessionRecord,
    JobArtifactPatch, JobArtifactRecord, JobEventRecord, JobPatch, JobRecord, PlaybookPatch,
    PlaybookRecord, PolicyDecisionRecord, SessionPatch, SessionRecord, ToolCallPatch,
    ToolCallRecord, ToolCapabilityGrantRecord, WorkerPatch, WorkerRecord, WorkerUsageDelta,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{Mutex, mpsc, oneshot, watch},
    time::{Duration, timeout},
};
use tracing::warn;
use uuid::Uuid;

use super::{
    ApiError, AppState, MCP_ENV_BEARER_MIGRATION_MESSAGE, assemble_prompt_input,
    ensure_prompting_runtime, excerpt, load_router_profiles, publish_overview_event,
    publish_prompt_progress_event, publish_session_event, record_instance_log, record_memory_audit,
    resolve_mcp_vault_bearer_token, resolve_profile_targets, resolve_session_projects,
    resolve_workspace_profile, resolve_workspace_profile_target, try_record_audit_event,
    unix_timestamp,
};
use crate::compaction::{
    CompactionOutcome, audit_compaction_outcome, compact_conversation,
    compaction_token_threshold_for_model, estimate_prompt_tokens, should_compact,
};
use crate::runtime::{PromptStreamEvent, ProviderTurnResult};
#[cfg(test)]
use crate::worker_action::parse_worker_action;
use crate::worker_action::{
    BrowserVerificationClaim, ChildJobProposal, FinalAnswerArtifact, WaitUntil, WorkerAction,
    parse_worker_action_with_registered_mcp_tools,
};
use crate::{error_display, security};

const DEFAULT_JOB_MAX_WALL_CLOCK_SECS: u64 = 7_200;
const MAX_CONFIGURED_JOB_STEPS: usize = 1_000;
const MAX_CONFIGURED_JOB_TOOL_CALLS: usize = 2_000;
const MAX_CONFIGURED_JOB_WALL_CLOCK_SECS: u64 = 86_400;
const JOB_MAX_CHILDREN_PER_FANOUT: usize = 5;
const DEFAULT_CHILD_JOB_MAX_STEPS: usize = 24;
const DEFAULT_CHILD_JOB_MAX_TOOL_CALLS: usize = 48;
const CHILD_JOB_POLL_INTERVAL_MS: u64 = 250;
const SESSION_HISTORY_TURN_LIMIT: usize = 8;
const TOOL_OUTPUT_CHAR_LIMIT: usize = 8_000;
const READ_FILE_CHAR_LIMIT: usize = 12_000;
const LIST_LIMIT: usize = 120;
const RG_LIMIT: usize = 80;
const DIFF_PREVIEW_CHAR_LIMIT: usize = 12_000;
const COMMAND_PREVIEW_CHAR_LIMIT: usize = 4_000;
const COMMAND_LABEL_CHAR_LIMIT: usize = 140;
const COMMAND_DEFAULT_TIMEOUT_SECS: u64 = 300;
const COMMAND_MAX_TIMEOUT_SECS: u64 = 1_800;
const COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES: usize = 131_072;
const COMMAND_MAX_OUTPUT_LIMIT_BYTES: usize = 524_288;
const COMMAND_DEFAULT_WAIT_FOR_OUTPUT_MS: u64 = 250;
const COMMAND_MAX_WAIT_FOR_OUTPUT_MS: u64 = 2_000;
const COMMAND_STATE_SETTLE_WAIT_MS: u64 = 50;
const COMMAND_TERMINATE_SETTLE_WAIT_MS: u64 = 2_000;
const WRITE_LOCK_POLL_INTERVAL_MS: u64 = 250;
const PLAYBOOK_SCHEDULER_INTERVAL_SECS: u64 = 30;
const PLAYBOOK_MIN_INTERVAL_SECS: u64 = 60;
const PLAYBOOK_MAX_INTERVAL_SECS: u64 = 86_400;
const WAIT_WATCHER_INTERVAL_SECS: u64 = 1;
const WAIT_CHILD_JOB_POLL_INTERVAL_SECS: u64 = 5;
const JOB_REGISTRATION_RETRY_ATTEMPTS: usize = 20;
const JOB_REGISTRATION_RETRY_DELAY_MS: u64 = 100;
const COMMAND_TRUNCATED_NOTE: &str = "[output truncated by the Nucleus budget]";
const UI_RENDERABLE_TERMS: &[&str] = &[
    "ui",
    "visual",
    "layout",
    "responsive",
    "interaction",
    "browser",
    "screenshot",
    "sidebar",
    "drawer",
    "dropdown",
    "modal",
    "clickable",
    "mobile",
    "shadcn",
    "css",
    "page",
];
const PATCH_LOOP_CORRECTION_PHRASES: &[&str] = &[
    "still wrong",
    "not clickable",
    "looks horrible",
    "for the millionth time",
    "completely broken",
    "going in circles",
];
const BROWSER_VERIFICATION_STATUSES: &[&str] = &[
    "not_required",
    "pending",
    "passed",
    "failed",
    "not_performed",
    "unavailable",
];
pub(crate) const ACTION_EXECUTOR_LANE: &str = "utility";

fn configured_job_max_wall_clock_secs() -> u64 {
    configured_u64_env(
        "NUCLEUS_JOB_MAX_WALL_CLOCK_SECS",
        DEFAULT_JOB_MAX_WALL_CLOCK_SECS,
        60,
        MAX_CONFIGURED_JOB_WALL_CLOCK_SECS,
    )
}

fn classify_prompt_ui_renderable(prompt: &str, image_count: usize) -> String {
    let normalized = prompt.to_ascii_lowercase();
    if image_count > 0 {
        if mentions_non_ui_image_context(&normalized) {
            return "false".to_string();
        }
        return "true".to_string();
    }
    if UI_RENDERABLE_TERMS
        .iter()
        .any(|term| normalized_contains_term(&normalized, term))
    {
        return "true".to_string();
    }
    "false".to_string()
}

fn mentions_non_ui_image_context(prompt: &str) -> bool {
    [
        "not ui",
        "not a ui",
        "unrelated to ui",
        "not related to ui",
        "unrelated screenshot",
        "receipt",
        "document scan",
    ]
    .iter()
    .any(|term| prompt.contains(term))
}

fn normalized_contains_term(text: &str, term: &str) -> bool {
    if term.len() <= 3 {
        text.split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|word| word == term)
    } else {
        text.contains(term)
    }
}

fn browser_verification_initial_patch(
    ui_renderable: &str,
    browser_available_error: Option<String>,
    browser_tools_granted: bool,
) -> JobPatch {
    if ui_renderable == "true" {
        if !browser_tools_granted {
            return JobPatch {
                ui_renderable: Some("true".to_string()),
                browser_verification_required: Some(true),
                browser_verification_status: Some("unavailable".to_string()),
                browser_verification_summary: Some(
                    "Browser verification is required for this UI-renderable job, but Browser tools are not granted in this session mode."
                        .to_string(),
                ),
                ..JobPatch::default()
            };
        }
        if let Some(error) = browser_available_error {
            return JobPatch {
                ui_renderable: Some("true".to_string()),
                browser_verification_required: Some(true),
                browser_verification_status: Some("unavailable".to_string()),
                browser_verification_summary: Some(format!(
                    "Browser verification is required for this UI-renderable job, but Browser runtime is unavailable: {error}"
                )),
                ..JobPatch::default()
            };
        }
        return JobPatch {
            ui_renderable: Some("true".to_string()),
            browser_verification_required: Some(true),
            browser_verification_status: Some("pending".to_string()),
            browser_verification_summary: Some(
                "Browser verification is required for this UI-renderable job.".to_string(),
            ),
            ..JobPatch::default()
        };
    }

    JobPatch {
        ui_renderable: Some(ui_renderable.to_string()),
        browser_verification_required: Some(false),
        browser_verification_status: Some("not_required".to_string()),
        browser_verification_summary: Some(String::new()),
        browser_verification_artifact_ids: Some(Vec::new()),
        ..JobPatch::default()
    }
}

fn is_ui_renderable_path(path: &Path, worker: &WorkerSummary) -> bool {
    let relative = path
        .strip_prefix(Path::new(&worker.working_dir))
        .ok()
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let relative = relative.trim_start_matches("./");
    let web_prefixed = relative.starts_with("apps/web/src/routes/")
        || relative.starts_with("apps/web/src/lib/components/")
        || (relative.starts_with("apps/web/src/") && relative.ends_with(".svelte"))
        || (relative.starts_with("apps/web/src/") && relative.ends_with(".css"));
    if web_prefixed {
        return true;
    }

    let worker_dir = worker.working_dir.replace('\\', "/");
    let worker_is_web_root = worker_dir == "apps/web" || worker_dir.ends_with("/apps/web");
    worker_is_web_root
        && (relative.starts_with("src/routes/")
            || relative.starts_with("src/lib/components/")
            || (relative.starts_with("src/") && relative.ends_with(".svelte"))
            || (relative.starts_with("src/") && relative.ends_with(".css")))
}

fn mutation_result_ui_renderable_path(tool: &str, result: &Value, worker: &WorkerSummary) -> bool {
    if !matches!(tool, "fs.apply_patch" | "fs.write_text" | "fs.move") {
        return false;
    }
    let paths = match tool {
        "fs.move" => ["from_path", "to_path"]
            .iter()
            .filter_map(|key| result.get(key).and_then(Value::as_str))
            .collect::<Vec<_>>(),
        _ => result
            .get("path")
            .and_then(Value::as_str)
            .into_iter()
            .collect::<Vec<_>>(),
    };
    paths
        .iter()
        .map(PathBuf::from)
        .any(|path| is_ui_renderable_path(&path, worker))
}

fn status_from_browser_verification_text(text: &str) -> Option<&'static str> {
    let normalized = text.to_ascii_lowercase();
    if normalized.contains("browser verification failed")
        || normalized.contains("browser verification: failed")
        || normalized.contains("browser verification - failed")
    {
        return Some("failed");
    }
    if normalized.contains("verification unavailable")
        || normalized.contains("browser verification unavailable")
        || normalized.contains("browser verification: unavailable")
    {
        return Some("unavailable");
    }
    if normalized.contains("not browser-verified")
        || normalized.contains("not browser verified")
        || normalized.contains("browser verification not performed")
        || normalized.contains("browser verification: not performed")
    {
        return Some("not_performed");
    }
    if normalized.contains("browser-verified")
        || normalized.contains("browser verified")
        || normalized.contains("browser verification passed")
        || normalized.contains("browser verification: passed")
    {
        return Some("passed");
    }
    None
}

fn normalize_browser_verification_claim_status(status: &str) -> Option<&'static str> {
    let normalized = status.trim().to_ascii_lowercase().replace('-', "_");
    BROWSER_VERIFICATION_STATUSES
        .iter()
        .copied()
        .find(|candidate| *candidate == normalized)
        .filter(|status| *status != "not_required" && *status != "pending")
}

fn browser_verification_completion_label(status: &str) -> &'static str {
    match status {
        "passed" => "Completed, browser-verified",
        "failed" => "Completed, browser verification failed",
        "unavailable" => "Completed, verification unavailable",
        "not_performed" | "pending" => "Completed, not browser-verified",
        _ => "Completed, not browser-verified",
    }
}

fn detects_patch_loop_correction(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    PATCH_LOOP_CORRECTION_PHRASES
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

fn should_trigger_patch_loop_guardrail(
    prompt: &str,
    prior_turns: &[SessionTurn],
    recent_jobs: &[JobSummary],
) -> bool {
    if !detects_patch_loop_correction(prompt) {
        return false;
    }
    let recent_correction = prior_turns
        .iter()
        .rev()
        .filter(|turn| turn.role == "user")
        .take(6)
        .any(|turn| detects_patch_loop_correction(&turn.content));
    let recent_ui_job = recent_jobs.iter().take(3).any(|job| {
        job.ui_renderable == "true"
            || matches!(
                job.browser_verification_status.as_str(),
                "failed" | "not_performed" | "unavailable"
            )
    });
    recent_correction || recent_ui_job
}

async fn mark_job_ui_renderable_from_mutation(
    state: &AppState,
    job_id: &str,
    reason: &str,
) -> Result<()> {
    let detail = state.store.get_job(job_id)?;
    if detail.job.ui_renderable == "true" {
        return Ok(());
    }

    let browser_error = crate::browser::BrowserRuntime::availability_error();
    let browser_tools_granted = detail.workers.iter().any(|worker| {
        worker
            .capabilities
            .iter()
            .any(|capability| capability.tool_id.starts_with("browser."))
    });
    let patch = browser_verification_initial_patch("true", browser_error, browser_tools_granted);
    state.store.update_job(job_id, patch)?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: None,
        event_type: "job.browser_verification.required".to_string(),
        status: "pending".to_string(),
        summary: "Marked job UI-renderable from file mutation.".to_string(),
        detail: reason.to_string(),
        data_json: json!({ "reason": reason }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    Ok(())
}

fn append_unique_ids(mut current: Vec<String>, next: &[String]) -> Vec<String> {
    for id in next {
        if !current.iter().any(|candidate| candidate == id) {
            current.push(id.clone());
        }
    }
    current
}

async fn attach_browser_verification_artifacts(
    state: &AppState,
    job_id: &str,
    artifact_ids: &[String],
) -> Result<()> {
    if artifact_ids.is_empty() {
        return Ok(());
    }
    let job = state.store.get_job(job_id)?.job;
    let next_ids = append_unique_ids(job.browser_verification_artifact_ids.clone(), artifact_ids);
    let summary = if job.browser_verification_summary.trim().is_empty()
        || job.browser_verification_summary
            == "Browser verification is required for this UI-renderable job."
    {
        "Browser evidence was captured; a verification outcome still needs to be asserted."
            .to_string()
    } else {
        job.browser_verification_summary
    };
    state.store.update_job(
        job_id,
        JobPatch {
            browser_verification_artifact_ids: Some(next_ids.clone()),
            browser_verification_summary: Some(summary),
            ..JobPatch::default()
        },
    )?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: None,
        event_type: "job.browser_verification.evidence".to_string(),
        status: "running".to_string(),
        summary: format!("Attached {} Browser artifact(s).", artifact_ids.len()),
        detail: artifact_ids.join(", "),
        data_json: json!({ "artifact_ids": artifact_ids }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    Ok(())
}

fn remaining_budget_for_browser_verification(
    worker: &WorkerSummary,
    step_count_after_rejection: usize,
    tool_calls: usize,
) -> bool {
    let has_step_room = worker.max_steps == 0 || step_count_after_rejection < worker.max_steps;
    let has_action_room = worker.max_tool_calls == 0 || tool_calls < worker.max_tool_calls;
    has_step_room && has_action_room
}

fn configured_child_job_max_steps() -> usize {
    configured_usize_env(
        "NUCLEUS_CHILD_JOB_MAX_STEPS",
        DEFAULT_CHILD_JOB_MAX_STEPS,
        1,
        MAX_CONFIGURED_JOB_STEPS,
    )
}

fn configured_child_job_max_tool_calls() -> usize {
    configured_usize_env(
        "NUCLEUS_CHILD_JOB_MAX_TOOL_CALLS",
        DEFAULT_CHILD_JOB_MAX_TOOL_CALLS,
        1,
        MAX_CONFIGURED_JOB_TOOL_CALLS,
    )
}

fn configured_usize_env(name: &str, default: usize, min: usize, max: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= min)
        .map(|value| value.min(max))
        .unwrap_or(default)
}

fn configured_u64_env(name: &str, default: u64, min: u64, max: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= min)
        .map(|value| value.min(max))
        .unwrap_or(default)
}

#[derive(Default)]
pub struct AgentRuntime {
    running_jobs: Mutex<BTreeSet<String>>,
    cancel_tokens: Mutex<BTreeMap<String, watch::Sender<bool>>>,
    command_sessions: Mutex<BTreeMap<String, ActiveCommandSessionHandle>>,
    write_locks: StdMutex<BTreeMap<String, WriteLockClaim>>,
}

#[derive(Debug, Clone)]
struct HiddenWorkerTarget {
    provider: String,
    model: String,
    provider_base_url: String,
    provider_api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkerCheckpoint {
    pub(crate) session_id: String,
    pub(crate) prompt_text: String,
    #[serde(default)]
    pub(crate) images: Vec<SessionTurnImage>,
    #[serde(default)]
    pub(crate) conversation: Vec<CheckpointMessage>,
    pub(crate) next_prompt: Option<String>,
    pub(crate) pending_action: Option<PendingToolAction>,
    #[serde(default)]
    pub(crate) browser_verification_final_answer_rejected: bool,
    #[serde(default)]
    pub(crate) patch_loop_guardrail_triggered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CheckpointMessage {
    pub(crate) role: String,
    pub(crate) content: String,
    #[serde(default)]
    pub(crate) images: Vec<SessionTurnImage>,
    #[serde(default)]
    pub(crate) compacted: bool,
    #[serde(default)]
    pub(crate) compacted_range: Option<CompactedRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CompactedRange {
    pub(crate) turn_id_start: String,
    pub(crate) turn_id_end: String,
    #[serde(default)]
    pub(crate) images: Vec<SessionTurnImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingToolAction {
    #[serde(default)]
    pub(crate) action_kind: String,
    pub(crate) tool_call_id: String,
    pub(crate) approval_id: Option<String>,
    pub(crate) command_session_id: Option<String>,
    #[serde(default)]
    pub(crate) child_job_ids: Vec<String>,
    pub(crate) summary: String,
    pub(crate) tool: String,
    pub(crate) args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WorkerWaitRecord {
    pub(crate) id: String,
    pub(crate) summary: String,
    pub(crate) until: WaitUntil,
    #[serde(default)]
    pub(crate) max_wait_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) wake_note: Option<String>,
    pub(crate) started_at: i64,
    #[serde(default)]
    pub(crate) last_checked_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct FsListArgs {
    path: Option<String>,
    recursive: Option<bool>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct FsReadTextArgs {
    path: String,
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RgSearchArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GitDiffArgs {
    pathspec: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PatchEditArgs {
    find: String,
    replace: String,
    replace_all: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct FsApplyPatchArgs {
    path: String,
    edits: Vec<PatchEditArgs>,
}

#[derive(Debug, Clone, Deserialize)]
struct FsWriteTextArgs {
    path: String,
    content: String,
    create_parent_dirs: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct FsMoveArgs {
    from_path: String,
    to_path: String,
    overwrite: Option<bool>,
    create_parent_dirs: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct FsMkdirArgs {
    path: String,
    recursive: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitStagePatchArgs {
    pathspecs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CommandRunArgs {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
    output_limit_bytes: Option<usize>,
    network_policy: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CommandSessionOpenArgs {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
    output_limit_bytes: Option<usize>,
    network_policy: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    title: Option<String>,
    wait_for_output_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct CommandSessionWriteArgs {
    session_id: String,
    input: String,
    append_newline: Option<bool>,
    wait_for_output_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct CommandSessionCloseArgs {
    session_id: String,
    wait_for_exit_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct TestsRunArgs {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
    output_limit_bytes: Option<usize>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubPrReviewThreadsArgs {
    owner: Option<String>,
    repo: Option<String>,
    pr_number: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubPrStateArgs {
    owner: Option<String>,
    repo: Option<String>,
    pr_number: Option<u64>,
    branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubCommentArgs {
    owner: Option<String>,
    repo: Option<String>,
    target_kind: String,
    number: u64,
    body: String,
}

#[derive(Debug, Clone, Deserialize)]
struct BrowserNavigateArgs {
    url: String,
    #[serde(default)]
    page_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct BrowserPageArgs {
    #[serde(default)]
    page_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct BrowserClickArgs {
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    target_ref: Option<String>,
    #[serde(default)]
    x: Option<i64>,
    #[serde(default)]
    y: Option<i64>,
    #[serde(default)]
    button: Option<String>,
    #[serde(default)]
    snapshot: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct BrowserTextArgs {
    #[serde(default)]
    page_id: Option<String>,
    target_ref: String,
    text: String,
    #[serde(default)]
    snapshot: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct BrowserScrollArgs {
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    target_ref: Option<String>,
    #[serde(default)]
    delta_x: Option<i64>,
    #[serde(default)]
    delta_y: Option<i64>,
    #[serde(default)]
    snapshot: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct BrowserPressArgs {
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    target_ref: Option<String>,
    key: String,
    #[serde(default)]
    snapshot: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct BrowserSubmitArgs {
    #[serde(default)]
    page_id: Option<String>,
    target_ref: String,
    #[serde(default)]
    snapshot: Option<bool>,
}

#[derive(Debug, Clone)]
struct MutationPreview {
    detail: String,
    diff_preview: String,
    artifact: Option<ArtifactDraft>,
}

#[derive(Debug, Clone)]
struct ActiveCommandSessionHandle {
    job_id: String,
    control: mpsc::Sender<CommandControl>,
    done: watch::Receiver<bool>,
}

#[derive(Debug, Clone)]
struct WriteLockClaim {
    owner_id: String,
    job_id: String,
    worker_id: String,
    roots: Vec<PathBuf>,
    reason: String,
}

#[derive(Debug)]
enum CommandControl {
    Snapshot {
        wait_for_output_ms: u64,
        reply: oneshot::Sender<Result<CommandInteractionResult, String>>,
    },
    Write {
        input: String,
        append_newline: bool,
        wait_for_output_ms: u64,
        reply: oneshot::Sender<Result<CommandInteractionResult, String>>,
    },
    Close {
        wait_for_exit_secs: u64,
        reply: oneshot::Sender<Result<CommandCloseResult, String>>,
    },
    Terminate {
        reason: String,
        final_state: String,
    },
}

#[derive(Debug, Clone)]
struct CommandInteractionResult {
    stdout_tail: String,
    stderr_tail: String,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct CommandCloseResult {
    state: String,
    exit_code: Option<i32>,
    last_error: String,
    stdout_tail: String,
    stderr_tail: String,
    truncated: bool,
}

#[derive(Debug, Clone, Default)]
struct LiveCommandOutput {
    stdout_tail: String,
    stderr_tail: String,
    stdout_bytes: u64,
    stderr_bytes: u64,
    total_captured_bytes: usize,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct ResolvedCommandSpec {
    mode: String,
    title: String,
    command: String,
    args: Vec<String>,
    cwd: PathBuf,
    timeout_secs: u64,
    output_limit_bytes: usize,
    network_policy: String,
    env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct ArtifactDraft {
    kind: String,
    title: String,
    mime_type: String,
    extension: String,
    content: String,
    preview_text: String,
    metadata_json: Value,
}

#[derive(Debug, Clone)]
struct ArtifactBytesDraft {
    kind: String,
    title: String,
    mime_type: String,
    extension: String,
    bytes: Vec<u8>,
    preview_text: String,
    metadata_json: Value,
}

pub async fn start_prompt_job(
    state: AppState,
    session_id: String,
    payload: SessionPromptRequest,
    current: SessionDetail,
    execution_prompt: String,
    compiler_role: String,
) -> Result<SessionDetail, ApiError> {
    if current.session.state == "paused" {
        return Err(ApiError::bad_request(
            "this session has a paused job that must be resumed or canceled first",
        ));
    }
    let prompt_excerpt = excerpt(&execution_prompt, 160);
    let visible_prompt = payload.prompt.trim().to_string();
    let job_id = Uuid::new_v4().to_string();
    let root_worker_id = Uuid::new_v4().to_string();
    let needs_vision_tools = !payload.images.is_empty();
    // Memory classification now happens entirely post-turn (see
    // `extract_memory_decisions_after_turn`). The user response must never be
    // gated on a memory decision, so this enqueue path no longer touches the
    // memory subsystem.
    let memory_outcomes: Vec<MemoryOutcome> = Vec::new();
    let target = resolve_hidden_worker_target(
        &state,
        &current.session,
        ACTION_EXECUTOR_LANE,
        needs_vision_tools,
    )
    .await?;
    let root_capabilities = if current.session.execution_mode == "plan" {
        Vec::new()
    } else {
        let mut capabilities = root_worker_capabilities();
        capabilities.extend(mcp_tool_capabilities(&state));
        capabilities
    };
    let browser_tools_granted = root_capabilities
        .iter()
        .any(|capability| capability.tool_id.starts_with("browser."));
    let ui_renderable = classify_prompt_ui_renderable(&visible_prompt, payload.images.len());
    let recent_jobs = state
        .store
        .list_jobs_for_session(&session_id)
        .unwrap_or_default();
    let patch_loop_guardrail_triggered =
        should_trigger_patch_loop_guardrail(&visible_prompt, &current.turns, &recent_jobs);

    state.store.update_session(
        &session_id,
        SessionPatch {
            state: Some("running".to_string()),
            last_error: Some(String::new()),
            ..SessionPatch::default()
        },
    )?;
    state.store.append_session_turn(
        &session_id,
        &Uuid::new_v4().to_string(),
        "user",
        visible_prompt.as_str(),
        &payload.images,
    )?;

    state.store.create_job(JobRecord {
        id: job_id.clone(),
        session_id: Some(session_id.clone()),
        parent_job_id: None,
        template_id: None,
        title: format!("Prompt {}", excerpt(&execution_prompt, 48)),
        purpose: "Session prompt".to_string(),
        trigger_kind: "session_prompt".to_string(),
        state: "queued".to_string(),
        requested_by: "user".to_string(),
        prompt_excerpt: prompt_excerpt.clone(),
        publication_intent_text: Some(visible_prompt.clone()),
    })?;
    let job = state.store.update_job(
        &job_id,
        browser_verification_initial_patch(
            &ui_renderable,
            crate::browser::BrowserRuntime::availability_error(),
            browser_tools_granted,
        ),
    )?;
    if job.publication_requested {
        record_publication_git_hygiene_baseline(&state, &job, &current.session.working_dir)?;
    }
    if patch_loop_guardrail_triggered {
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job_id.clone(),
            worker_id: None,
            event_type: "job.guardrail.patch_loop".to_string(),
            status: "warning".to_string(),
            summary: "Patch-loop guardrail triggered.".to_string(),
            detail:
                "Recent correction phrasing indicates UI patch chasing; worker prompt includes reset/reassess guidance."
                    .to_string(),
            data_json: json!({ "prompt_excerpt": excerpt(&visible_prompt, 240) }),
        });
    }

    let _created_worker = state.store.create_worker(WorkerRecord {
        id: root_worker_id.clone(),
        job_id: job_id.clone(),
        parent_worker_id: None,
        title: "Utility Worker".to_string(),
        lane: ACTION_EXECUTOR_LANE.to_string(),
        state: "queued".to_string(),
        provider: target.provider.clone(),
        model: target.model.clone(),
        provider_base_url: target.provider_base_url.clone(),
        provider_api_key: target.provider_api_key.clone(),
        provider_session_id: String::new(),
        working_dir: current.session.working_dir.clone(),
        read_roots: worker_read_roots(&current.session),
        write_roots: worker_write_roots(&current.session),
        max_steps: current.session.run_budget.max_steps,
        max_tool_calls: current.session.run_budget.max_tool_calls,
        max_wall_clock_secs: current.session.run_budget.max_wall_clock_secs,
    })?;
    state.store.update_job(
        &job_id,
        JobPatch {
            root_worker_id: Some(root_worker_id.clone()),
            ..JobPatch::default()
        },
    )?;
    state
        .store
        .replace_tool_capability_grants(&root_worker_id, &root_capabilities)?;
    let worker = state
        .store
        .get_job(&job_id)?
        .workers
        .into_iter()
        .find(|item| item.id == root_worker_id)
        .ok_or_else(|| {
            ApiError::internal_message("failed to reload Utility Worker capabilities")
        })?;

    // Pre-flight compile so any compile-time error (missing include file,
    // overflowing budget, malformed metadata) surfaces synchronously to the
    // user before the job queues. The actual CompiledTurn sent to the provider
    // is rebuilt per model call inside `execute_worker_text_turn` so memory,
    // skill, and include layers reflect the latest session state.
    let _ = crate::compile_session_turn(
        &state,
        &current.session,
        &current.turns,
        &payload.prompt,
        &payload.images,
        &compiler_role,
    )?;

    let checkpoint = WorkerCheckpoint {
        session_id: session_id.clone(),
        prompt_text: execution_prompt.clone(),
        images: payload.images.clone(),
        conversation: initial_worker_conversation(
            &worker,
            &current.session.execution_mode,
            &current.turns,
        ),
        next_prompt: None,
        pending_action: None,
        browser_verification_final_answer_rejected: false,
        patch_loop_guardrail_triggered,
    };
    state
        .store
        .write_worker_checkpoint(&root_worker_id, &serde_json::to_value(checkpoint).unwrap())?;

    let started = state.store.get_session(&session_id)?;
    let _ = publish_session_event(&state, started).await;
    publish_job_created(&state, &state.store.get_job(&job_id)?.job).await;
    publish_worker_updated(&state, &worker).await;
    publish_prompt_status(
        &state,
        &current.session,
        &worker,
        "queued",
        "Queued Utility Worker",
        if payload.images.is_empty() {
            "Nucleus accepted the prompt and created a Utility Worker."
        } else {
            "Nucleus accepted the prompt with scoped image attachment(s) and created a Utility Worker."
        },
        &memory_outcomes,
    )
    .await;
    let _ = publish_overview_event(&state).await;

    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "job.created".to_string(),
            target: format!("job:{job_id}"),
            status: "success".to_string(),
            summary: format!(
                "Queued Utility Worker job for session '{}'.",
                current.session.title
            ),
            detail: format!(
                "session_id={} executor_lane={} utility_provider={} utility_model={}",
                session_id, ACTION_EXECUTOR_LANE, target.provider, target.model
            ),
        },
    )
    .await;

    spawn_job_task(state.clone(), job_id.clone());

    Ok(state.store.get_session(&session_id)?)
}

fn collect_job_subtree_ids(state: &AppState, root_job_id: &str) -> Result<Vec<String>> {
    let mut ordered = Vec::new();
    let mut stack = vec![root_job_id.to_string()];
    while let Some(job_id) = stack.pop() {
        let detail = state.store.get_job(&job_id)?;
        for child in detail.child_jobs.iter().rev() {
            stack.push(child.id.clone());
        }
        ordered.push(job_id);
    }
    Ok(ordered)
}

pub async fn cancel_job(state: AppState, job_id: String) -> Result<JobDetail, ApiError> {
    let detail = state.store.get_job(&job_id)?;
    match detail.job.state.as_str() {
        "completed" | "canceled" => {
            return Ok(detail);
        }
        _ => {}
    }

    let subtree = collect_job_subtree_ids(&state, &job_id)?;
    for child_job_id in subtree.iter().rev() {
        let child_detail = state.store.get_job(child_job_id)?;
        let previous_state = child_detail.job.state.clone();
        if let Some(sender) = state
            .agent
            .cancel_tokens
            .lock()
            .await
            .get(child_job_id)
            .cloned()
        {
            let _ = sender.send(true);
        }

        state.store.update_job(
            child_job_id,
            JobPatch {
                state: Some("canceled".to_string()),
                last_error: Some(String::new()),
                ..JobPatch::default()
            },
        )?;
        for worker in child_detail.workers {
            if worker.state == "waiting" {
                cancel_waiting_worker(&state, &worker).await;
            }
            let _ = state.store.update_worker(
                &worker.id,
                WorkerPatch {
                    state: Some("canceled".to_string()),
                    last_error: Some(String::new()),
                    ..WorkerPatch::default()
                },
            );
        }
        for approval in child_detail.approvals {
            if approval.state == "pending" {
                let _ = state.store.update_approval_request(
                    &approval.id,
                    "canceled",
                    Some("The job was canceled before this approval was resolved."),
                    Some("system"),
                    Some(unix_timestamp()),
                );
            }
        }
        state
            .agent
            .terminate_job_command_sessions(
                child_job_id,
                "The job was canceled before this command session finished.",
                "canceled",
            )
            .await;
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: child_job_id.clone(),
            worker_id: None,
            event_type: "job.canceled".to_string(),
            status: "canceled".to_string(),
            summary: "Canceled Utility Worker job.".to_string(),
            detail: if previous_state == "failed" {
                "Nucleus dismissed the failed job and unblocked the session.".to_string()
            } else {
                "Nucleus stopped the job before it finished.".to_string()
            },
            data_json: json!({ "previous_state": previous_state }),
        });
        publish_job_updated(&state, &state.store.get_job(child_job_id)?.job).await;
        if let Some(parent_job_id) = child_detail.job.parent_job_id.as_deref() {
            publish_job_updated(&state, &state.store.get_job(parent_job_id)?.job).await;
        }
    }

    if detail.job.parent_job_id.is_none() {
        if let Some(session_id) = detail.job.session_id.as_deref() {
            let _ = state.store.update_session(
                session_id,
                SessionPatch {
                    state: Some("active".to_string()),
                    last_error: Some(String::new()),
                    ..SessionPatch::default()
                },
            );
            if let Ok(session) = state.store.get_session(session_id) {
                let _ = publish_session_event(&state, session).await;
            }
        }
    }
    publish_job_updated(&state, &state.store.get_job(&job_id)?.job).await;
    let _ = publish_overview_event(&state).await;
    Ok(state.store.get_job(&job_id)?)
}

pub async fn resume_job(state: AppState, job_id: String) -> Result<JobDetail, ApiError> {
    let detail = state.store.get_job(&job_id)?;
    if detail.job.state != "paused" && detail.job.state != "failed" {
        return Err(ApiError::bad_request(
            "only paused or checkpointed failed Utility Worker jobs can be resumed",
        ));
    }
    if detail.job.state == "failed" && !job_has_worker_checkpoint(&state, &detail)? {
        return Err(ApiError::bad_request(
            "failed Utility Worker job has no checkpoint to resume from",
        ));
    }

    let subtree = collect_job_subtree_ids(&state, &job_id)?;
    for child_job_id in subtree.iter().rev() {
        let child_detail = state.store.get_job(child_job_id)?;
        if child_detail.job.state != "paused" && child_detail.job.state != "failed" {
            continue;
        }
        if child_detail.job.state == "failed" && !job_has_worker_checkpoint(&state, &child_detail)?
        {
            continue;
        }
        state.store.update_job(
            child_job_id,
            JobPatch {
                state: Some("queued".to_string()),
                last_error: Some(String::new()),
                last_resumed_at: Some(Some(unix_timestamp())),
                ..JobPatch::default()
            },
        )?;
        for worker in child_detail.workers {
            let _ = state.store.update_worker(
                &worker.id,
                WorkerPatch {
                    state: Some("queued".to_string()),
                    last_error: Some(String::new()),
                    ..WorkerPatch::default()
                },
            );
        }
    }
    if detail.job.parent_job_id.is_none() {
        if let Some(session_id) = detail.job.session_id.as_deref() {
            let _ = state.store.update_session(
                session_id,
                SessionPatch {
                    state: Some("running".to_string()),
                    last_error: Some(String::new()),
                    ..SessionPatch::default()
                },
            );
            if let Ok(session) = state.store.get_session(session_id) {
                let _ = publish_session_event(&state, session).await;
            }
        }
    }
    for child_job_id in subtree.iter().rev() {
        if state.store.get_job(child_job_id)?.job.state == "queued" {
            spawn_job_task(state.clone(), child_job_id.clone());
        }
    }
    Ok(state.store.get_job(&job_id)?)
}

fn job_has_worker_checkpoint(state: &AppState, detail: &JobDetail) -> Result<bool> {
    for worker in &detail.workers {
        if state.store.read_worker_checkpoint(&worker.id)?.is_some() {
            return Ok(true);
        }
    }

    Ok(false)
}

pub async fn list_pending_approvals(
    state: AppState,
) -> Result<Vec<ApprovalRequestSummary>, ApiError> {
    Ok(state.store.list_pending_approvals()?)
}

pub async fn approve_request(
    state: AppState,
    approval_id: String,
    note: Option<String>,
) -> Result<JobDetail, ApiError> {
    resolve_approval_request(state, approval_id, true, note).await
}

pub async fn deny_request(
    state: AppState,
    approval_id: String,
    note: Option<String>,
) -> Result<JobDetail, ApiError> {
    resolve_approval_request(state, approval_id, false, note).await
}

pub fn spawn_playbook_scheduler(state: AppState) {
    tokio::spawn(async move {
        if let Err(error) = dispatch_playbook_event_inner(&state, "daemon_started").await {
            warn!(error = %error, "failed to dispatch daemon_started playbooks");
        }

        let mut interval =
            tokio::time::interval(Duration::from_secs(PLAYBOOK_SCHEDULER_INTERVAL_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = run_scheduled_playbooks(&state).await {
                warn!(error = %error, "playbook scheduler tick failed");
            }
        }
    });
}

pub async fn list_playbooks(state: AppState) -> Result<Vec<PlaybookSummary>, ApiError> {
    Ok(state.store.list_playbooks()?)
}

pub async fn get_playbook(
    state: AppState,
    playbook_id: String,
) -> Result<PlaybookDetail, ApiError> {
    Ok(state.store.get_playbook(&playbook_id)?)
}

pub async fn create_playbook(
    state: AppState,
    payload: CreatePlaybookRequest,
) -> Result<PlaybookDetail, ApiError> {
    let title = normalize_playbook_title(&payload.title)?;
    let prompt = normalize_playbook_prompt(&payload.prompt)?;
    let description = normalize_playbook_description(payload.description.as_deref());
    let policy_bundle = normalize_playbook_policy_bundle(&payload.policy_bundle)?;
    let (trigger_kind, schedule_interval_secs, event_kind) = normalize_playbook_trigger(
        &payload.trigger_kind,
        payload.schedule_interval_secs,
        payload.event_kind.as_deref(),
    )?;

    let session_id = Uuid::new_v4().to_string();
    let session = create_playbook_session(
        &state,
        &session_id,
        &title,
        payload.profile_id.as_deref(),
        payload.project_id.as_deref(),
    )
    .await?;
    let created_at = unix_timestamp();
    let detail = state.store.create_playbook(PlaybookRecord {
        id: Uuid::new_v4().to_string(),
        session_id,
        title: title.clone(),
        description: description.clone(),
        prompt,
        enabled: payload.enabled.unwrap_or(true),
        policy_bundle,
        trigger_kind: trigger_kind.clone(),
        schedule_interval_secs,
        event_kind: event_kind.clone(),
        created_at,
        updated_at: created_at,
    })?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "playbook.created".to_string(),
            target: format!("playbook:{}", detail.playbook.id),
            status: "success".to_string(),
            summary: format!("Created playbook '{}'.", detail.playbook.title),
            detail: format!(
                "session_id={} trigger_kind={} policy_bundle={} working_dir={}",
                session.session.id,
                trigger_kind,
                detail.playbook.policy_bundle,
                detail.playbook.working_dir
            ),
        },
    )
    .await;
    Ok(detail)
}

pub async fn update_playbook(
    state: AppState,
    playbook_id: String,
    payload: UpdatePlaybookRequest,
) -> Result<PlaybookDetail, ApiError> {
    ensure_no_active_playbook_jobs(&state, &playbook_id)?;
    let before = state.store.get_playbook(&playbook_id)?;

    let next_title = match payload.title {
        Some(value) => normalize_playbook_title(&value)?,
        None => before.playbook.title.clone(),
    };
    let next_prompt = match payload.prompt {
        Some(value) => normalize_playbook_prompt(&value)?,
        None => read_playbook_prompt(&state, &playbook_id)?,
    };
    let next_description = match payload.description {
        Some(value) => normalize_playbook_description(Some(value.as_str())),
        None => before.playbook.description.clone(),
    };
    let next_policy_bundle = match payload.policy_bundle {
        Some(value) => normalize_playbook_policy_bundle(&value)?,
        None => before.playbook.policy_bundle.clone(),
    };
    let next_trigger_kind_input = payload
        .trigger_kind
        .as_deref()
        .unwrap_or(before.playbook.trigger_kind.as_str());
    let next_schedule_input = match payload.schedule_interval_secs {
        Some(value) => value,
        None => before.playbook.schedule_interval_secs,
    };
    let next_event_input = match payload.event_kind {
        Some(Some(value)) => Some(value),
        Some(None) => None,
        None => before.playbook.event_kind.clone(),
    };
    let (next_trigger_kind, next_schedule_interval_secs, next_event_kind) =
        normalize_playbook_trigger(
            next_trigger_kind_input,
            next_schedule_input,
            next_event_input.as_deref(),
        )?;

    let profile_id = payload
        .profile_id
        .as_deref()
        .or(Some(before.session.profile_id.as_str()))
        .filter(|value| !value.trim().is_empty());
    let project_id = payload
        .project_id
        .as_deref()
        .or(Some(before.session.project_id.as_str()))
        .filter(|value| !value.trim().is_empty());

    update_playbook_session(&state, &before.session, &next_title, profile_id, project_id).await?;

    let detail = state.store.update_playbook(
        &playbook_id,
        PlaybookPatch {
            title: Some(next_title.clone()),
            description: Some(next_description),
            prompt: Some(next_prompt),
            enabled: payload.enabled,
            policy_bundle: Some(next_policy_bundle),
            trigger_kind: Some(next_trigger_kind),
            schedule_interval_secs: Some(next_schedule_interval_secs),
            event_kind: Some(next_event_kind),
            updated_at: Some(unix_timestamp()),
            ..PlaybookPatch::default()
        },
    )?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "playbook.updated".to_string(),
            target: format!("playbook:{}", detail.playbook.id),
            status: "success".to_string(),
            summary: format!("Updated playbook '{}'.", detail.playbook.title),
            detail: format!(
                "trigger_kind={} policy_bundle={} enabled={}",
                detail.playbook.trigger_kind,
                detail.playbook.policy_bundle,
                detail.playbook.enabled
            ),
        },
    )
    .await;
    Ok(detail)
}

pub async fn delete_playbook(
    state: AppState,
    playbook_id: String,
) -> Result<PlaybookDetail, ApiError> {
    ensure_no_active_playbook_jobs(&state, &playbook_id)?;
    let detail = state.store.delete_playbook(&playbook_id)?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "playbook.deleted".to_string(),
            target: format!("playbook:{}", detail.playbook.id),
            status: "success".to_string(),
            summary: format!("Deleted playbook '{}'.", detail.playbook.title),
            detail: format!("session_id={}", detail.session.id),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(detail)
}

pub async fn run_playbook(state: AppState, playbook_id: String) -> Result<JobDetail, ApiError> {
    queue_playbook_job(&state, &playbook_id, "playbook_manual", "user").await
}

pub async fn dispatch_playbook_event(state: AppState, event_kind: &str) -> Result<(), ApiError> {
    dispatch_playbook_event_inner(&state, event_kind).await?;
    Ok(())
}

pub async fn recover_interrupted_jobs(state: &AppState) -> Result<()> {
    let restart_error = "Nucleus restarted before this command session completed.";
    let jobs = state.store.list_jobs_by_state(&["queued", "running"])?;
    for job in jobs {
        let _ = state.store.update_job(
            &job.id,
            JobPatch {
                state: Some("paused".to_string()),
                last_error: Some("Nucleus restarted before this job completed.".to_string()),
                ..JobPatch::default()
            },
        );
        let detail = state.store.get_job(&job.id)?;
        for worker in detail.workers {
            let _ = state.store.update_worker(
                &worker.id,
                WorkerPatch {
                    state: Some("paused".to_string()),
                    last_error: Some(
                        "Nucleus restarted before this Utility Worker completed.".to_string(),
                    ),
                    ..WorkerPatch::default()
                },
            );
        }
        if let Some(session_id) = job.session_id.as_deref() {
            let _ = state.store.update_session(
                session_id,
                SessionPatch {
                    state: Some("paused".to_string()),
                    last_error: Some("Resume or cancel the paused Utility Worker job.".to_string()),
                    ..SessionPatch::default()
                },
            );
        }
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job.id.clone(),
            worker_id: None,
            event_type: "job.paused".to_string(),
            status: "paused".to_string(),
            summary: "Paused a Utility Worker job after Nucleus restart.".to_string(),
            detail: "Nucleus recovered persisted job state and is waiting for an explicit resume."
                .to_string(),
            data_json: json!({ "reason": "daemon_restart" }),
        });
        publish_job_updated(state, &state.store.get_job(&job.id)?.job).await;
    }
    for command_session in state
        .store
        .list_command_sessions_by_state(&["starting", "running"])?
    {
        if let Some(tool_call_id) = command_session.tool_call_id.as_deref() {
            if let Ok(detail) = state.store.get_job(&command_session.job_id) {
                if detail
                    .tool_calls
                    .iter()
                    .find(|tool_call| tool_call.id == tool_call_id)
                    .is_some_and(|tool_call| is_non_terminal_tool_call_status(&tool_call.status))
                {
                    let _ = state.store.update_tool_call(
                        tool_call_id,
                        ToolCallPatch {
                            status: Some("failed".to_string()),
                            error_class: Some("daemon_restart".to_string()),
                            error_detail: Some(restart_error.to_string()),
                            completed_at: Some(Some(unix_timestamp())),
                            ..ToolCallPatch::default()
                        },
                    );
                }
            }
        }
        let _ = state.store.update_command_session(
            &command_session.id,
            CommandSessionPatch {
                state: Some("orphaned".to_string()),
                last_error: Some(restart_error.to_string()),
                completed_at: Some(Some(unix_timestamp())),
                ..CommandSessionPatch::default()
            },
        );
    }
    Ok(())
}

pub fn spawn_wait_watcher(state: AppState) {
    tokio::spawn(async move {
        let mut events = state.events.subscribe();
        let mut interval = tokio::time::interval(Duration::from_secs(WAIT_WATCHER_INTERVAL_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        if let Err(error) = process_waiting_workers(&state, None).await {
            warn!(error = %error, "wait watcher startup rehydration failed");
        }
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) = process_waiting_workers(&state, None).await {
                        warn!(error = %error, "wait watcher tick failed");
                    }
                }
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            if let Err(error) = process_waiting_workers(&state, Some(&event)).await {
                                warn!(error = %error, "wait watcher event pass failed");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if let Err(error) = process_waiting_workers(&state, None).await {
                                warn!(error = %error, "wait watcher lag recovery failed");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });
}

async fn process_waiting_workers(state: &AppState, event: Option<&DaemonEvent>) -> Result<()> {
    let now = unix_timestamp();
    for worker in state.store.list_workers_by_state(&["waiting"])? {
        let Some(wait_value) = worker.wait_until_json.clone() else {
            continue;
        };
        let mut wait: WorkerWaitRecord =
            serde_json::from_value(wait_value).context("failed to decode worker wait")?;
        if worker_wall_clock_exceeded(&worker, now) {
            complete_wait_with_wall_clock_budget(state, &worker, &wait).await?;
            continue;
        }
        if wait_timed_out(&wait, now) {
            resume_waiting_worker(state, &worker, &wait, "timeout", "worker.wait.timeout").await?;
            continue;
        }
        let child_poll_due = child_job_poll_due(&wait, event, now);
        if wait_condition_satisfied(state, &wait, event, now)? {
            resume_waiting_worker(state, &worker, &wait, "satisfied", "worker.wait.completed")
                .await?;
        } else if child_poll_due {
            wait.last_checked_at = Some(now);
            persist_worker_wait_record(state, &worker.id, &wait)?;
        }
    }
    Ok(())
}

fn wait_timed_out(wait: &WorkerWaitRecord, now: i64) -> bool {
    wait.max_wait_seconds.is_some_and(|max_wait_seconds| {
        now.saturating_sub(wait.started_at) >= max_wait_seconds as i64
    })
}

fn wait_condition_satisfied(
    state: &AppState,
    wait: &WorkerWaitRecord,
    event: Option<&DaemonEvent>,
    now: i64,
) -> Result<bool> {
    match &wait.until {
        WaitUntil::DelaySeconds { delay_seconds } => {
            Ok(now.saturating_sub(wait.started_at) >= *delay_seconds as i64)
        }
        WaitUntil::AbsoluteUnix { absolute_unix } => Ok(now >= *absolute_unix),
        WaitUntil::AuditEvent {
            event_kind,
            target_pattern,
            status,
        } => Ok(event_matches_audit_wait(
            event,
            wait.started_at,
            event_kind,
            target_pattern,
            status,
        ) || persisted_audit_matches_wait(
            state,
            wait.started_at,
            event_kind,
            target_pattern,
            status,
        )?),
        WaitUntil::ChildJobsCompleted { job_ids } => {
            if job_ids.is_empty() {
                return Ok(true);
            }
            if !child_job_poll_due(wait, event, now) {
                return Ok(false);
            }
            for job_id in job_ids {
                if !state.store.job_exists(job_id)? {
                    return Ok(false);
                }
                let detail = state.store.get_job(job_id)?;
                if !matches!(
                    detail.job.state.as_str(),
                    "completed" | "failed" | "canceled"
                ) {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        WaitUntil::ArtifactKind {
            job_id,
            artifact_kind,
        } => {
            if event_matches_artifact_wait(event, job_id, artifact_kind, wait.started_at) {
                return Ok(true);
            }
            if !state.store.job_exists(job_id)? {
                return Ok(false);
            }
            let detail = state.store.get_job(job_id)?;
            Ok(detail.artifacts.iter().any(|artifact| {
                artifact.kind == *artifact_kind && artifact.created_at > wait.started_at
            }))
        }
    }
}

fn child_job_poll_due(wait: &WorkerWaitRecord, event: Option<&DaemonEvent>, now: i64) -> bool {
    let WaitUntil::ChildJobsCompleted { job_ids } = &wait.until else {
        return false;
    };
    let elapsed = now.saturating_sub(wait.last_checked_at.unwrap_or(wait.started_at));
    elapsed >= WAIT_CHILD_JOB_POLL_INTERVAL_SECS as i64
        || event_matches_child_job_wait(event, job_ids)
}

fn event_matches_child_job_wait(event: Option<&DaemonEvent>, job_ids: &[String]) -> bool {
    let Some(event) = event else {
        return false;
    };
    let job = match event {
        DaemonEvent::JobUpdated(job)
        | DaemonEvent::JobCompleted(job)
        | DaemonEvent::JobFailed(job) => job,
        _ => return false,
    };
    job_ids.iter().any(|job_id| job_id == &job.id)
}

fn persist_worker_wait_record(
    state: &AppState,
    worker_id: &str,
    wait: &WorkerWaitRecord,
) -> Result<()> {
    state.store.update_worker(
        worker_id,
        WorkerPatch {
            wait_until_json: Some(Some(
                serde_json::to_value(wait).context("failed to encode worker wait")?,
            )),
            wait_started_at: Some(Some(wait.started_at)),
            ..WorkerPatch::default()
        },
    )?;
    Ok(())
}

fn event_matches_audit_wait(
    event: Option<&DaemonEvent>,
    started_at: i64,
    kind: &str,
    target_pattern: &Option<String>,
    status: &Option<String>,
) -> bool {
    let Some(DaemonEvent::AuditUpdated(events)) = event else {
        return false;
    };
    events.iter().any(|event| {
        event.created_at > started_at
            && audit_event_matches_wait(event, kind, target_pattern.as_deref(), status.as_deref())
    })
}

fn persisted_audit_matches_wait(
    state: &AppState,
    started_at: i64,
    kind: &str,
    target_pattern: &Option<String>,
    status: &Option<String>,
) -> Result<bool> {
    Ok(state
        .store
        .list_audit_events_since(started_at)?
        .iter()
        .any(|event| {
            event.created_at > started_at
                && audit_event_matches_wait(
                    event,
                    kind,
                    target_pattern.as_deref(),
                    status.as_deref(),
                )
        }))
}

fn audit_event_matches_wait(
    event: &nucleus_protocol::AuditEvent,
    kind: &str,
    target_pattern: Option<&str>,
    status: Option<&str>,
) -> bool {
    event.kind == kind
        && target_pattern
            .filter(|value| !value.is_empty())
            .is_none_or(|pattern| event.target.contains(pattern))
        && status
            .filter(|value| !value.is_empty())
            .is_none_or(|expected| event.status == expected)
}

fn event_matches_artifact_wait(
    event: Option<&DaemonEvent>,
    job_id: &str,
    kind: &str,
    started_at: i64,
) -> bool {
    let Some(DaemonEvent::ArtifactAdded(artifact)) = event else {
        return false;
    };
    artifact.job_id == job_id && artifact.kind == kind && artifact.created_at > started_at
}

async fn complete_wait_with_wall_clock_budget(
    state: &AppState,
    worker: &WorkerSummary,
    wait: &WorkerWaitRecord,
) -> Result<()> {
    let detail = state.store.get_job(&worker.job_id)?;
    if matches!(
        detail.job.state.as_str(),
        "completed" | "failed" | "canceled"
    ) {
        return Ok(());
    }
    let session_id = detail.job.session_id.clone().ok_or_else(|| {
        anyhow!(
            "waiting job '{}' is not attached to a session",
            detail.job.id
        )
    })?;
    let session = state.store.get_session(&session_id)?;
    let checkpoint_value = state
        .store
        .read_worker_checkpoint(&worker.id)?
        .ok_or_else(|| anyhow!("waiting worker '{}' has no checkpoint", worker.id))?;
    let checkpoint: WorkerCheckpoint = serde_json::from_value(checkpoint_value)
        .context("failed to decode waiting worker checkpoint")?;
    let mut worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            wait_until_json: Some(None),
            wait_started_at: Some(None),
            ..WorkerPatch::default()
        },
    )?;
    emit_wait_finished_event(
        state,
        &worker,
        wait,
        "wall_clock_exceeded",
        "worker.wait.timeout",
    )
    .await;
    let step_count = worker.step_count;
    let tool_call_count = worker.tool_call_count;
    complete_job_with_budget_checkpoint(
        state,
        &session,
        &detail.job.id,
        &mut worker,
        &checkpoint,
        step_count,
        tool_call_count,
        "wall-clock",
    )
    .await
}

async fn resume_waiting_worker(
    state: &AppState,
    worker: &WorkerSummary,
    wait: &WorkerWaitRecord,
    reason: &str,
    event_type: &str,
) -> Result<()> {
    let detail = state.store.get_job(&worker.job_id)?;
    if matches!(
        detail.job.state.as_str(),
        "completed" | "failed" | "canceled"
    ) {
        return Ok(());
    }
    let session_id = detail.job.session_id.clone().ok_or_else(|| {
        anyhow!(
            "waiting job '{}' is not attached to a session",
            detail.job.id
        )
    })?;
    let mut checkpoint: WorkerCheckpoint = state
        .store
        .read_worker_checkpoint(&worker.id)?
        .ok_or_else(|| anyhow!("waiting worker '{}' has no checkpoint", worker.id))
        .and_then(|value| {
            serde_json::from_value(value).context("failed to decode waiting worker checkpoint")
        })?;
    checkpoint.conversation.push(CheckpointMessage {
        role: "system".to_string(),
        content: build_wake_system_note(wait, reason),
        images: Vec::new(),
        compacted: false,
        compacted_range: None,
    });
    checkpoint.next_prompt = Some(build_wait_resume_prompt(wait, reason));
    state.store.write_worker_checkpoint(
        &worker.id,
        &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
    )?;
    state.store.update_job(
        &worker.job_id,
        JobPatch {
            state: Some("queued".to_string()),
            last_error: Some(String::new()),
            last_resumed_at: Some(Some(unix_timestamp())),
            ..JobPatch::default()
        },
    )?;
    if detail.job.parent_job_id.is_none() {
        state.store.update_session(
            &session_id,
            SessionPatch {
                state: Some("running".to_string()),
                last_error: Some(String::new()),
                ..SessionPatch::default()
            },
        )?;
        if let Ok(session) = state.store.get_session(&session_id) {
            let _ = publish_session_event(state, session).await;
        }
    }
    let worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("queued".to_string()),
            wait_until_json: Some(None),
            wait_started_at: Some(None),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    emit_wait_finished_event(state, &worker, wait, reason, event_type).await;
    publish_job_updated(state, &state.store.get_job(&worker.job_id)?.job).await;
    publish_worker_updated(state, &worker).await;
    let _ = publish_overview_event(state).await;
    spawn_job_task(state.clone(), worker.job_id.clone());
    Ok(())
}

async fn cancel_waiting_worker(state: &AppState, worker: &WorkerSummary) {
    let Some(wait_value) = worker.wait_until_json.clone() else {
        return;
    };
    let Ok(wait) = serde_json::from_value::<WorkerWaitRecord>(wait_value) else {
        return;
    };
    let _ = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            wait_until_json: Some(None),
            wait_started_at: Some(None),
            ..WorkerPatch::default()
        },
    );
    emit_wait_finished_event(state, worker, &wait, "canceled", "worker.wait.canceled").await;
}

async fn emit_wait_finished_event(
    state: &AppState,
    worker: &WorkerSummary,
    wait: &WorkerWaitRecord,
    reason: &str,
    event_type: &str,
) {
    let status = match event_type {
        "worker.wait.canceled" => "canceled",
        "worker.wait.timeout" => "timeout",
        _ => "completed",
    };
    let data_json = {
        let mut data = wait_audit_data(wait);
        if let Some(object) = data.as_object_mut() {
            object.insert("reason".to_string(), json!(reason));
            object.insert("completed_at".to_string(), json!(unix_timestamp()));
        }
        data
    };
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: worker.job_id.clone(),
        worker_id: Some(worker.id.clone()),
        event_type: event_type.to_string(),
        status: status.to_string(),
        summary: format!("Worker wait {} ended with reason {reason}", wait.id),
        detail: wait.summary.clone(),
        data_json: data_json.clone(),
    });
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: event_type.to_string(),
            target: format!("worker:{}", worker.id),
            status: status.to_string(),
            summary: format!("Worker wait {} ended with reason {reason}.", wait.id),
            detail: serde_json::to_string(&data_json).unwrap_or_else(|_| "{}".to_string()),
        },
    )
    .await;
}

fn build_wake_system_note(wait: &WorkerWaitRecord, reason: &str) -> String {
    let mut note = format!(
        "[wake-up at {} | reason={} | condition={} | wait_id={}]",
        format_unix_timestamp(unix_timestamp()),
        reason,
        wait_condition_label(&wait.until),
        wait.id
    );
    if let Some(wake_note) = wait
        .wake_note
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        note.push_str("\nWake note: ");
        note.push_str(&excerpt(wake_note, 500));
    }
    note
}

fn build_wait_resume_prompt(wait: &WorkerWaitRecord, reason: &str) -> String {
    format!(
        "Nucleus resumed this worker after a wait.\nWait summary: {}\nWake reason: {}\nCondition: {}\nReturn exactly one valid Nucleus worker action JSON object for the next step.",
        excerpt(&wait.summary, 500),
        reason,
        wait_condition_label(&wait.until)
    )
}

fn format_unix_timestamp(timestamp: i64) -> String {
    let Ok(duration) = u64::try_from(timestamp).map(Duration::from_secs) else {
        return format!("unix:{timestamp}");
    };
    httpdate::fmt_http_date(UNIX_EPOCH + duration)
}

fn is_non_terminal_tool_call_status(status: &str) -> bool {
    matches!(status, "queued" | "starting" | "running")
}

fn spawn_job_task(state: AppState, job_id: String) {
    tokio::spawn(async move {
        if let Err(error) = run_job(state.clone(), job_id.clone()).await {
            warn!(job_id = %job_id, error = %error, "hidden worker job crashed");
            let _ = fail_job(&state, &job_id, &error.to_string()).await;
        }
    });
}

impl AgentRuntime {
    async fn register_job(&self, job_id: &str) -> Option<watch::Receiver<bool>> {
        let mut running = self.running_jobs.lock().await;
        if !running.insert(job_id.to_string()) {
            return None;
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancel_tokens
            .lock()
            .await
            .insert(job_id.to_string(), cancel_tx);
        drop(running);
        Some(cancel_rx)
    }

    async fn finish_job(&self, job_id: &str) {
        self.running_jobs.lock().await.remove(job_id);
        self.cancel_tokens.lock().await.remove(job_id);
    }

    fn try_claim_write_lock(
        &self,
        owner_id: &str,
        job_id: &str,
        worker_id: &str,
        roots: &[String],
        reason: &str,
    ) -> Result<Option<WriteLockClaim>> {
        let normalized_roots = normalize_lock_roots(roots)?;
        if normalized_roots.is_empty() {
            return Ok(None);
        }

        let mut locks = self
            .write_locks
            .lock()
            .expect("write lock registry mutex poisoned");
        if locks.contains_key(owner_id) {
            return Ok(None);
        }

        if let Some(conflict) = locks
            .values()
            .find(|claim| write_lock_roots_conflict(&claim.roots, &normalized_roots))
            .cloned()
        {
            return Ok(Some(conflict));
        }

        locks.insert(
            owner_id.to_string(),
            WriteLockClaim {
                owner_id: owner_id.to_string(),
                job_id: job_id.to_string(),
                worker_id: worker_id.to_string(),
                roots: normalized_roots,
                reason: reason.to_string(),
            },
        );
        Ok(None)
    }

    fn transfer_write_lock(&self, from_owner_id: &str, to_owner_id: &str) -> Result<()> {
        if from_owner_id == to_owner_id {
            return Ok(());
        }

        let mut locks = self
            .write_locks
            .lock()
            .expect("write lock registry mutex poisoned");
        if locks.contains_key(to_owner_id) {
            bail!("write lock owner '{}' already exists", to_owner_id);
        }
        if let Some(mut claim) = locks.remove(from_owner_id) {
            claim.owner_id = to_owner_id.to_string();
            locks.insert(to_owner_id.to_string(), claim);
        }
        Ok(())
    }

    fn release_write_lock(&self, owner_id: &str) {
        self.write_locks
            .lock()
            .expect("write lock registry mutex poisoned")
            .remove(owner_id);
    }

    async fn register_command_session(
        &self,
        command_session_id: &str,
        job_id: &str,
        control: mpsc::Sender<CommandControl>,
        done: watch::Receiver<bool>,
    ) {
        self.command_sessions.lock().await.insert(
            command_session_id.to_string(),
            ActiveCommandSessionHandle {
                job_id: job_id.to_string(),
                control,
                done,
            },
        );
    }

    async fn get_command_session(
        &self,
        command_session_id: &str,
    ) -> Option<ActiveCommandSessionHandle> {
        self.command_sessions
            .lock()
            .await
            .get(command_session_id)
            .cloned()
    }

    async fn finish_command_session(&self, command_session_id: &str) {
        self.command_sessions
            .lock()
            .await
            .remove(command_session_id);
    }

    async fn terminate_job_command_sessions(&self, job_id: &str, reason: &str, final_state: &str) {
        let handles = self
            .command_sessions
            .lock()
            .await
            .values()
            .filter(|handle| handle.job_id == job_id)
            .cloned()
            .collect::<Vec<_>>();

        for handle in &handles {
            let _ = handle
                .control
                .send(CommandControl::Terminate {
                    reason: reason.to_string(),
                    final_state: final_state.to_string(),
                })
                .await;
        }
        for handle in &handles {
            let mut done = handle.done.clone();
            if !*done.borrow() {
                let _ = timeout(
                    Duration::from_millis(COMMAND_TERMINATE_SETTLE_WAIT_MS),
                    done.changed(),
                )
                .await;
            }
        }
    }
}

async fn run_job(state: AppState, job_id: String) -> Result<()> {
    let Some(mut cancel_rx) = register_queued_job_with_retry(&state, &job_id).await? else {
        return Ok(());
    };
    let result = run_job_loop(&state, &job_id, &mut cancel_rx).await;
    state.agent.finish_job(&job_id).await;
    result
}

async fn register_queued_job_with_retry(
    state: &AppState,
    job_id: &str,
) -> Result<Option<watch::Receiver<bool>>> {
    for attempt in 0..=JOB_REGISTRATION_RETRY_ATTEMPTS {
        if let Some(cancel_rx) = state.agent.register_job(job_id).await {
            return Ok(Some(cancel_rx));
        }

        let detail = state.store.get_job(job_id)?;
        if detail.job.state != "queued" {
            return Ok(None);
        }
        if attempt == JOB_REGISTRATION_RETRY_ATTEMPTS {
            bail!("job '{job_id}' is queued but another runner did not release registration");
        }
        tokio::time::sleep(Duration::from_millis(JOB_REGISTRATION_RETRY_DELAY_MS)).await;
    }

    Ok(None)
}

async fn run_job_loop(
    state: &AppState,
    job_id: &str,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<()> {
    let detail = state.store.get_job(job_id)?;
    let session_id = detail
        .job
        .session_id
        .clone()
        .ok_or_else(|| anyhow!("job '{job_id}' is not attached to a session"))?;
    let mut session = state.store.get_session(&session_id)?;
    let worker_id = detail
        .job
        .root_worker_id
        .clone()
        .ok_or_else(|| anyhow!("job '{job_id}' has no root worker"))?;
    let mut worker = detail
        .workers
        .into_iter()
        .find(|item| item.id == worker_id)
        .ok_or_else(|| anyhow!("job '{job_id}' root worker was not found"))?;
    worker =
        migrate_legacy_root_worker_to_utility(state, &detail.job, &session.session, worker).await?;
    ensure_utility_worker_executor(&worker)?;

    state.store.update_job(
        job_id,
        JobPatch {
            state: Some("running".to_string()),
            last_error: Some(String::new()),
            ..JobPatch::default()
        },
    )?;
    worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("running".to_string()),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    if let Some(parent_job_id) = detail.job.parent_job_id.as_deref() {
        publish_job_updated(state, &state.store.get_job(parent_job_id)?.job).await;
    }
    publish_worker_updated(state, &worker).await;
    publish_prompt_status(
        state,
        &session.session,
        &worker,
        "running",
        "Utility Worker running",
        "Nucleus is planning the next repo-inspection step.",
        &[],
    )
    .await;

    let checkpoint_value = state
        .store
        .read_worker_checkpoint(&worker.id)?
        .ok_or_else(|| anyhow!("worker '{}' has no checkpoint", worker.id))?;
    let mut checkpoint: WorkerCheckpoint = serde_json::from_value(checkpoint_value)
        .context("failed to decode worker checkpoint payload")?;

    let assembled_prompt = assemble_prompt_input(state, &session.session, &checkpoint.prompt_text)
        .map_err(|error| anyhow!(error.message))?;

    let mut step = worker.step_count;
    let mut tool_calls = worker.tool_call_count;

    loop {
        if *cancel_rx.borrow() {
            return Ok(());
        }

        session = state.store.get_session(&session_id)?;
        if matches!(
            state.store.get_job(job_id)?.job.state.as_str(),
            "completed" | "failed" | "canceled"
        ) {
            return Ok(());
        }
        if let LoopDisposition::Return = handle_pending_action(
            state,
            &session,
            job_id,
            &mut worker,
            &mut checkpoint,
            &mut step,
            &mut tool_calls,
            cancel_rx,
        )
        .await?
        {
            return Ok(());
        }

        if worker_wall_clock_exceeded(&worker, unix_timestamp()) {
            complete_job_with_budget_checkpoint(
                state,
                &session,
                job_id,
                &mut worker,
                &checkpoint,
                step,
                tool_calls,
                "wall-clock",
            )
            .await?;
            return Ok(());
        }

        if worker.max_steps > 0 && step >= worker.max_steps {
            complete_job_with_budget_checkpoint(
                state,
                &session,
                job_id,
                &mut worker,
                &checkpoint,
                step,
                tool_calls,
                "step",
            )
            .await?;
            return Ok(());
        }

        if worker.max_tool_calls > 0 && tool_calls >= worker.max_tool_calls {
            complete_job_with_budget_checkpoint(
                state,
                &session,
                job_id,
                &mut worker,
                &checkpoint,
                step,
                tool_calls,
                "action",
            )
            .await?;
            return Ok(());
        }

        if !checkpoint.images.is_empty() && !worker_supports_vision_with_tools(&worker) {
            let detail = unsupported_vision_with_tools_detail(&worker, checkpoint.images.len());
            publish_prompt_status(
                state,
                &session.session,
                &worker,
                "degraded",
                "Vision unavailable for Utility Worker",
                &detail,
                &[],
            )
            .await;
            let metadata = json!({});
            complete_job_with_final_answer(
                state,
                &session,
                job_id,
                &mut worker,
                step,
                tool_calls,
                "Vision with tools is unsupported for the selected runtime.",
                &detail,
                &metadata,
                &[],
            )
            .await?;
            return Ok(());
        }

        let attach_initial_images = should_attach_initial_worker_images(&checkpoint);
        let prompt = checkpoint.next_prompt.take().unwrap_or_else(|| {
            let initial =
                build_initial_step_prompt(&session.session, &assembled_prompt.prompt, &worker);
            let prompt = if checkpoint.patch_loop_guardrail_triggered {
                build_patch_loop_guardrail_prompt(initial)
            } else {
                initial
            };
            match state.store.get_job(job_id) {
                Ok(detail) => add_publication_initial_prompt_guidance(state, &detail.job, prompt),
                Err(_) => prompt,
            }
        });
        let prompt = add_budget_guidance(prompt, &worker, step, tool_calls);
        let prompt_images = if attach_initial_images {
            checkpoint.images.clone()
        } else {
            Vec::new()
        };

        publish_prompt_status(
            state,
            &session.session,
            &worker,
            "thinking",
            "Planning the next step",
            "The Utility Worker is deciding whether to inspect the repo or answer directly.",
            &[],
        )
        .await;

        if let Err(error) = compact_checkpoint_if_needed(
            state,
            &session.session,
            &worker,
            &mut checkpoint,
            &prompt,
            &prompt_images,
            cancel_rx,
        )
        .await
        {
            if *cancel_rx.borrow() {
                return Ok(());
            }
            warn!(
                ?error,
                worker_id = worker.id.as_str(),
                "conversation compaction failed; continuing with uncompacted checkpoint",
            );
            record_memory_audit(
                state,
                "memory.compaction.failed",
                &worker.id,
                "failed",
                &format!("Conversation compaction failed before worker turn: {error}"),
            )
            .await;
        }

        let response = match call_worker_model(
            state,
            Some(&session.session),
            &worker,
            &checkpoint.conversation,
            &prompt,
            &prompt_images,
            cancel_rx,
        )
        .await
        {
            Ok(response) => response,
            Err(_) if *cancel_rx.borrow() => return Ok(()),
            Err(error) => {
                return Err(anyhow!(
                    "Utility Worker route failed (lane={}, provider={}, model={}): check Utility model credentials and endpoint settings: {error}",
                    worker.lane,
                    worker.provider,
                    worker.model
                ));
            }
        };
        if *cancel_rx.borrow() {
            return Ok(());
        }
        checkpoint.conversation.push(CheckpointMessage {
            role: "user".to_string(),
            content: prompt.clone(),
            images: prompt_images.clone(),
            compacted: false,
            compacted_range: None,
        });
        if attach_initial_images {
            checkpoint.images.clear();
        }
        checkpoint.conversation.push(CheckpointMessage {
            role: "assistant".to_string(),
            content: response.raw.clone(),
            images: Vec::new(),
            compacted: false,
            compacted_range: None,
        });
        if !response.provider_session_id.is_empty() {
            worker = state.store.update_worker(
                &worker.id,
                WorkerPatch {
                    provider_session_id: Some(response.provider_session_id.clone()),
                    ..WorkerPatch::default()
                },
            )?;
        }

        session = state.store.get_session(&session_id)?;
        match response.action {
            WorkerAction::SpawnChildJobs { summary, jobs } => {
                if session.session.execution_mode == "plan" {
                    retry_plan_mode_action(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        &summary,
                        &format!("spawn {} Utility Subworker job(s)", jobs.len()),
                    )
                    .await?;
                    continue;
                }

                if let LoopDisposition::Return = handle_child_job_proposal(
                    state,
                    &session,
                    job_id,
                    &mut worker,
                    &mut checkpoint,
                    &mut step,
                    summary,
                    jobs,
                )
                .await?
                {
                    return Ok(());
                }
            }
            WorkerAction::FinalAnswer {
                summary,
                final_answer,
                metadata,
                artifacts,
                browser_verification,
            } => {
                let detail = state.store.get_job(job_id)?;
                if should_retry_zero_tool_action_final_answer(
                    &detail.job,
                    &summary,
                    &final_answer,
                    &session.session.execution_mode,
                    &worker,
                    step,
                    tool_calls,
                    detail.child_jobs.len(),
                ) {
                    retry_worker_final_answer(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        "Rejected zero-tool action completion.",
                        "zero_tool_action_final_answer",
                        &build_zero_tool_action_retry_prompt(&detail.job, &summary, &final_answer),
                        &final_answer,
                    )
                    .await?;
                    continue;
                }

                if should_retry_internal_action_item_final_answer(&final_answer, tool_calls) {
                    retry_worker_final_answer(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        "Rejected internal action item as final answer.",
                        "internal_action_item_final_answer",
                        &build_internal_action_item_retry_prompt(&summary, &final_answer),
                        &final_answer,
                    )
                    .await?;
                    continue;
                }

                if should_retry_incomplete_progress_final_answer(
                    &summary,
                    &final_answer,
                    &session.session.execution_mode,
                    &worker,
                    step,
                    tool_calls,
                ) {
                    retry_worker_final_answer(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        "Rejected incomplete progress report as final answer.",
                        "incomplete_progress_final_answer",
                        &build_incomplete_progress_retry_prompt(&summary, &final_answer),
                        &final_answer,
                    )
                    .await?;
                    continue;
                }

                if should_retry_unsupported_confident_negative_final_answer(
                    &detail,
                    &summary,
                    &final_answer,
                    &session.session.execution_mode,
                    &checkpoint,
                    &worker,
                    step,
                    tool_calls,
                ) {
                    retry_worker_final_answer(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        "Rejected unsupported confident negative answer.",
                        "evidence_incomplete_confident_negative",
                        &build_evidence_completion_retry_prompt(&summary, &final_answer),
                        &final_answer,
                    )
                    .await?;
                    continue;
                }

                let current_job = state.store.get_job(job_id)?.job;
                if should_retry_browser_verification_final_answer(
                    &current_job,
                    browser_verification.as_ref(),
                    &final_answer,
                    &metadata,
                    &checkpoint,
                    &worker,
                    step + 1,
                    tool_calls,
                ) {
                    checkpoint.browser_verification_final_answer_rejected = true;
                    retry_worker_final_answer(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        "Rejected final answer without Browser verification outcome.",
                        "browser_verification_required",
                        &build_browser_verification_retry_prompt(&current_job, &final_answer),
                        &final_answer,
                    )
                    .await?;
                    continue;
                }

                if should_retry_missing_publication_outcome(
                    &detail,
                    &summary,
                    &final_answer,
                    &metadata,
                    browser_verification.as_ref(),
                    &worker,
                    step,
                    tool_calls,
                ) {
                    retry_worker_final_answer(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        "Requested explicit publication outcome metadata.",
                        "publication_outcome_missing",
                        &build_publication_outcome_retry_prompt(&summary, &final_answer),
                        &final_answer,
                    )
                    .await?;
                    continue;
                }

                let final_answer = apply_browser_verification_final_state(
                    state,
                    job_id,
                    browser_verification,
                    &final_answer,
                    &metadata,
                )
                .await?;

                complete_job_with_final_answer(
                    state,
                    &session,
                    job_id,
                    &mut worker,
                    step + 1,
                    tool_calls,
                    &summary,
                    &final_answer,
                    &metadata,
                    &artifacts,
                )
                .await?;
                return Ok(());
            }
            WorkerAction::ProgressUpdate { summary, detail } => {
                if session.session.execution_mode == "plan" {
                    retry_plan_mode_action(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        &summary,
                        "record a progress checkpoint",
                    )
                    .await?;
                    continue;
                }

                record_worker_progress_update(
                    state,
                    &session,
                    job_id,
                    &mut worker,
                    &mut checkpoint,
                    &mut step,
                    tool_calls,
                    &summary,
                    &detail,
                )
                .await?;
                continue;
            }
            WorkerAction::Wait {
                summary,
                until,
                max_wait_seconds,
                wake_note,
            } => {
                if session.session.execution_mode == "plan" {
                    retry_plan_mode_action(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        &summary,
                        "park the worker with wait",
                    )
                    .await?;
                    continue;
                }

                park_worker_wait(
                    state,
                    &session,
                    job_id,
                    &mut worker,
                    &checkpoint,
                    summary,
                    until,
                    max_wait_seconds,
                    wake_note,
                )
                .await?;
                return Ok(());
            }
            WorkerAction::ToolCall {
                summary,
                tool,
                args,
            } => {
                if session.session.execution_mode == "plan" {
                    retry_plan_mode_action(
                        state,
                        job_id,
                        &mut worker,
                        &mut checkpoint,
                        &mut step,
                        tool_calls,
                        &summary,
                        &format!("run {}", tool),
                    )
                    .await?;
                    continue;
                }

                if let LoopDisposition::Return = handle_tool_call_proposal(
                    state,
                    &session,
                    job_id,
                    &mut worker,
                    &mut checkpoint,
                    &mut step,
                    &mut tool_calls,
                    cancel_rx,
                    summary,
                    tool,
                    args,
                )
                .await?
                {
                    return Ok(());
                }
            }
        }
    }
}

async fn migrate_legacy_root_worker_to_utility(
    state: &AppState,
    job: &JobSummary,
    session: &SessionSummary,
    worker: WorkerSummary,
) -> Result<WorkerSummary> {
    if worker.lane == ACTION_EXECUTOR_LANE {
        return Ok(worker);
    }
    if worker.lane != "main"
        || worker.parent_worker_id.is_some()
        || job.root_worker_id.as_deref() != Some(worker.id.as_str())
        || job.trigger_kind != "session_prompt"
    {
        return Ok(worker);
    }

    let Some(checkpoint_value) = state.store.read_worker_checkpoint(&worker.id)? else {
        return Ok(worker);
    };
    let checkpoint: WorkerCheckpoint = serde_json::from_value(checkpoint_value)
        .context("failed to decode legacy worker checkpoint payload")?;
    let target = resolve_hidden_worker_target(
        state,
        session,
        ACTION_EXECUTOR_LANE,
        !checkpoint.images.is_empty(),
    )
    .await
    .map_err(|error| anyhow!(error.message))?;
    let mut migrated = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            title: Some("Utility Worker".to_string()),
            lane: Some(ACTION_EXECUTOR_LANE.to_string()),
            provider: Some(target.provider.clone()),
            model: Some(target.model.clone()),
            provider_base_url: Some(target.provider_base_url.clone()),
            provider_api_key: Some(target.provider_api_key),
            provider_session_id: Some(String::new()),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    let root_capabilities = if session.execution_mode == "plan" {
        Vec::new()
    } else {
        let mut capabilities = root_worker_capabilities();
        capabilities.extend(mcp_tool_capabilities(state));
        capabilities
    };
    migrated.capabilities = state
        .store
        .replace_tool_capability_grants(&worker.id, &root_capabilities)?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job.id.clone(),
        worker_id: Some(worker.id.clone()),
        event_type: "worker.legacy_lane_migrated".to_string(),
        status: "running".to_string(),
        summary: "Migrated legacy main-lane root worker to Utility Worker.".to_string(),
        detail: format!(
            "Legacy persisted root worker '{}' was moved from lane 'main' to lane '{}' before resume.",
            worker.id, ACTION_EXECUTOR_LANE
        ),
        data_json: json!({
            "from_lane": "main",
            "to_lane": ACTION_EXECUTOR_LANE,
            "provider": target.provider,
            "model": target.model,
        }),
    });
    Ok(migrated)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopDisposition {
    Continue,
    Return,
}

async fn retry_plan_mode_action(
    state: &AppState,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: usize,
    summary: &str,
    attempted_action: &str,
) -> Result<()> {
    checkpoint.next_prompt = Some(build_plan_mode_retry_prompt(summary, attempted_action));
    state.store.write_worker_checkpoint(
        &worker.id,
        &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
    )?;
    *step += 1;
    *worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("running".to_string()),
            step_count: Some(*step),
            tool_call_count: Some(tool_calls),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "worker.retry".to_string(),
        status: "retrying".to_string(),
        summary: "Rejected action while Plan mode is enabled.".to_string(),
        detail: format!(
            "Plan mode blocks Nucleus actions. Attempted action: {}. Summary: {}",
            attempted_action,
            excerpt(summary, 240)
        ),
        data_json: json!({
            "reason": "plan_mode_action_rejected",
            "attempted_action": attempted_action,
        }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    Ok(())
}

async fn reject_pending_action_for_plan_mode(
    state: &AppState,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: usize,
    pending: &PendingToolAction,
) -> Result<()> {
    checkpoint.pending_action = None;
    if let Some(approval_id) = pending.approval_id.as_deref() {
        if let Ok(approval) = state.store.get_approval_request(approval_id) {
            if approval.state == "pending" {
                let resolved = state.store.update_approval_request(
                    approval_id,
                    "denied",
                    Some("Session switched to Plan mode before this action ran."),
                    Some("system"),
                    Some(unix_timestamp()),
                )?;
                publish_approval_resolved(state, &resolved).await;
            }
        }
    }
    if !pending.tool_call_id.is_empty() {
        state.store.update_tool_call(
            &pending.tool_call_id,
            ToolCallPatch {
                status: Some("denied".to_string()),
                error_class: Some("plan_mode_action_rejected".to_string()),
                error_detail: Some(
                    "Session switched to Plan mode before this action ran.".to_string(),
                ),
                completed_at: Some(Some(unix_timestamp())),
                ..ToolCallPatch::default()
            },
        )?;
    }
    retry_plan_mode_action(
        state,
        job_id,
        worker,
        checkpoint,
        step,
        tool_calls,
        &pending.summary,
        &format!("run {}", pending.tool),
    )
    .await
}

async fn handle_pending_action(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: &mut usize,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<LoopDisposition> {
    let Some(pending) = checkpoint.pending_action.clone() else {
        return Ok(LoopDisposition::Continue);
    };

    if is_pending_child_job_action(&pending) {
        let child_details = pending
            .child_job_ids
            .iter()
            .map(|child_job_id| state.store.get_job(child_job_id))
            .collect::<Result<Vec<_>>>()?;
        let all_complete = child_details.iter().all(|detail| {
            matches!(
                detail.job.state.as_str(),
                "completed" | "failed" | "canceled"
            )
        });
        if all_complete {
            let results = child_details
                .iter()
                .map(child_job_result_json)
                .collect::<Result<Vec<_>>>()?;
            checkpoint.pending_action = None;
            checkpoint.next_prompt =
                Some(build_child_job_results_prompt(&pending.summary, &results));
            state.store.write_worker_checkpoint(
                &worker.id,
                &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
            )?;
            let completed_count = child_details
                .iter()
                .filter(|detail| detail.job.state == "completed")
                .count();
            let failed_count = child_details.len().saturating_sub(completed_count);
            let _ = state.store.append_job_event(JobEventRecord {
                job_id: job_id.to_string(),
                worker_id: Some(worker.id.clone()),
                event_type: "child.jobs.joined".to_string(),
                status: "running".to_string(),
                summary: format!("Joined {} child jobs", child_details.len()),
                detail: format!(
                    "{} child jobs completed and {} ended without success.",
                    completed_count, failed_count
                ),
                data_json: json!({
                    "child_job_ids": pending.child_job_ids,
                }),
            });
            publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
            publish_worker_updated(state, worker).await;
            return Ok(LoopDisposition::Continue);
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(CHILD_JOB_POLL_INTERVAL_MS)) => {}
            changed = cancel_rx.changed() => {
                if changed.is_ok() && *cancel_rx.borrow() {
                    return Ok(LoopDisposition::Return);
                }
            }
        }
        return Ok(LoopDisposition::Continue);
    }

    if session.session.execution_mode == "plan" {
        reject_pending_action_for_plan_mode(
            state,
            job_id,
            worker,
            checkpoint,
            step,
            *tool_calls,
            &pending,
        )
        .await?;
        return Ok(LoopDisposition::Continue);
    }

    if let Some(approval_id) = pending.approval_id.as_deref() {
        let approval = state.store.get_approval_request(approval_id)?;
        match approval.state.as_str() {
            "pending" => {
                if session.session.approval_mode == "trusted" {
                    let resolved = state.store.update_approval_request(
                        approval_id,
                        "approved",
                        Some("Auto-approved because this session is set to Run Actions."),
                        Some("system"),
                        Some(unix_timestamp()),
                    )?;
                    let _ = state.store.append_job_event(JobEventRecord {
                        job_id: job_id.to_string(),
                        worker_id: Some(worker.id.clone()),
                        event_type: "approval.resolved".to_string(),
                        status: "approved".to_string(),
                        summary: format!("Approved {}", approval.summary),
                        detail: resolved.resolution_note.clone(),
                        data_json: json!({
                            "approval_id": resolved.id,
                            "tool_call_id": resolved.tool_call_id,
                            "resolved_by": resolved.resolved_by,
                        }),
                    });
                    publish_approval_resolved(state, &resolved).await;
                } else {
                    let pause_reason = format!("Waiting for approval to run {}.", pending.tool);
                    state.store.update_job(
                        job_id,
                        JobPatch {
                            state: Some("paused".to_string()),
                            last_error: Some(pause_reason.clone()),
                            ..JobPatch::default()
                        },
                    )?;
                    *worker = state.store.update_worker(
                        &worker.id,
                        WorkerPatch {
                            state: Some("paused".to_string()),
                            tool_call_count: Some(*tool_calls),
                            last_error: Some(pause_reason.clone()),
                            ..WorkerPatch::default()
                        },
                    )?;
                    state.store.update_session(
                        &session.session.id,
                        SessionPatch {
                            state: Some("paused".to_string()),
                            last_error: Some(pause_reason),
                            ..SessionPatch::default()
                        },
                    )?;
                    if let Ok(updated) = state.store.get_session(&session.session.id) {
                        let _ = publish_session_event(state, updated).await;
                    }
                    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
                    publish_worker_updated(state, worker).await;
                    return Ok(LoopDisposition::Return);
                }
            }
            "approved" => {}
            _ => {
                checkpoint.pending_action = None;
                checkpoint.next_prompt = Some(build_tool_denied_prompt(
                    &pending.tool,
                    &pending.summary,
                    fallback_note(
                        approval.resolution_note.as_str(),
                        "The approval request was not approved.",
                    )
                    .as_str(),
                ));
                state.store.write_worker_checkpoint(
                    &worker.id,
                    &serde_json::to_value(&checkpoint)
                        .context("failed to encode worker checkpoint")?,
                )?;
                state.store.update_tool_call(
                    &pending.tool_call_id,
                    ToolCallPatch {
                        status: Some("denied".to_string()),
                        error_class: Some("approval_denied".to_string()),
                        error_detail: Some(fallback_note(
                            &approval.resolution_note,
                            "The approval request was denied.",
                        )),
                        completed_at: Some(Some(unix_timestamp())),
                        ..ToolCallPatch::default()
                    },
                )?;
                *step += 1;
                *worker = state.store.update_worker(
                    &worker.id,
                    WorkerPatch {
                        state: Some("running".to_string()),
                        step_count: Some(*step),
                        tool_call_count: Some(*tool_calls),
                        last_error: Some(String::new()),
                        ..WorkerPatch::default()
                    },
                )?;
                let _ = state.store.append_job_event(JobEventRecord {
                    job_id: job_id.to_string(),
                    worker_id: Some(worker.id.clone()),
                    event_type: "tool.denied".to_string(),
                    status: "denied".to_string(),
                    summary: format!("Denied {}", pending.tool),
                    detail: fallback_note(&approval.resolution_note, &approval.detail),
                    data_json: json!({
                        "tool_id": pending.tool,
                        "tool_call_id": pending.tool_call_id,
                        "approval_id": approval.id,
                    }),
                });
                publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
                publish_worker_updated(state, worker).await;
                return Ok(LoopDisposition::Continue);
            }
        }
    }

    if let Some(command_session_id) = pending.command_session_id.as_deref() {
        if let Ok(command_session) = state.store.get_command_session(command_session_id) {
            if command_session.state == "orphaned" {
                let snapshot = artifact_snapshot_from_summary(state, &command_session)?;
                let result = command_session_result_json(&command_session, &snapshot);
                checkpoint.pending_action = None;
                checkpoint.next_prompt = Some(build_tool_result_prompt(
                    &pending.tool,
                    &pending.summary,
                    &result,
                ));
                state.store.write_worker_checkpoint(
                    &worker.id,
                    &serde_json::to_value(&checkpoint)
                        .context("failed to encode worker checkpoint")?,
                )?;
                state.store.update_tool_call(
                    &pending.tool_call_id,
                    ToolCallPatch {
                        status: Some("completed".to_string()),
                        result_json: Some(Some(result.clone())),
                        completed_at: Some(Some(unix_timestamp())),
                        ..ToolCallPatch::default()
                    },
                )?;
                *step += 1;
                *worker = state.store.update_worker(
                    &worker.id,
                    WorkerPatch {
                        state: Some("running".to_string()),
                        step_count: Some(*step),
                        tool_call_count: Some(*tool_calls),
                        last_error: Some(String::new()),
                        ..WorkerPatch::default()
                    },
                )?;
                let _ = state.store.append_job_event(JobEventRecord {
                    job_id: job_id.to_string(),
                    worker_id: Some(worker.id.clone()),
                    event_type: "tool.completed".to_string(),
                    status: "completed".to_string(),
                    summary: format!("Recovered {}", pending.tool),
                    detail:
                        "Nucleus resumed with the persisted command-session result after restart."
                            .to_string(),
                    data_json: json!({
                        "tool_id": pending.tool,
                        "tool_call_id": pending.tool_call_id,
                        "command_session_id": command_session.id,
                    }),
                });
                publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
                publish_worker_updated(state, worker).await;
                return Ok(LoopDisposition::Continue);
            }
        }
    }

    execute_pending_tool_action(
        state, session, job_id, worker, checkpoint, step, tool_calls, cancel_rx, pending,
    )
    .await
}

async fn handle_tool_call_proposal(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: &mut usize,
    cancel_rx: &mut watch::Receiver<bool>,
    summary: String,
    tool: String,
    args: Value,
) -> Result<LoopDisposition> {
    ensure_utility_worker_executor(worker)?;
    *tool_calls += 1;
    let policy = policy_for_tool_with_mode(&tool, &session.session.approval_mode);
    let tool_call_id = Uuid::new_v4().to_string();
    let requires_approval = policy.decision == "require_approval";
    let mut tool_call = state.store.create_tool_call(ToolCallRecord {
        id: tool_call_id.clone(),
        job_id: job_id.to_string(),
        worker_id: worker.id.clone(),
        tool_id: tool.clone(),
        status: if requires_approval {
            "pending_approval".to_string()
        } else {
            "queued".to_string()
        },
        summary: summary.clone(),
        args_json: args.clone(),
        result_json: None,
        policy_decision: Some(policy.clone()),
        artifact_ids: Vec::new(),
        error_class: String::new(),
        error_detail: String::new(),
        started_at: None,
        completed_at: None,
    })?;

    if requires_approval {
        let preview = preview_approval_tool(state, worker, &tool, &args)?;
        let artifact_ids = if let Some(draft) = preview.artifact {
            let artifact =
                write_job_artifact(state, job_id, Some(&worker.id), Some(&tool_call_id), draft)?;
            publish_artifact_added(state, &artifact).await;
            vec![artifact.id]
        } else {
            Vec::new()
        };
        if !artifact_ids.is_empty() {
            tool_call = state.store.update_tool_call(
                &tool_call_id,
                ToolCallPatch {
                    artifact_ids: Some(artifact_ids),
                    ..ToolCallPatch::default()
                },
            )?;
        }

        let approval = state.store.create_approval_request(ApprovalRequestRecord {
            id: Uuid::new_v4().to_string(),
            job_id: job_id.to_string(),
            worker_id: worker.id.clone(),
            tool_call_id: tool_call_id.clone(),
            state: "pending".to_string(),
            risk_level: policy.risk_level.clone(),
            summary: summary.clone(),
            detail: preview.detail,
            diff_preview: preview.diff_preview,
            policy_decision: policy.clone(),
            resolution_note: String::new(),
            resolved_by: String::new(),
            resolved_at: None,
        })?;

        checkpoint.pending_action = Some(PendingToolAction {
            action_kind: "tool".to_string(),
            tool_call_id: tool_call_id.clone(),
            approval_id: Some(approval.id.clone()),
            command_session_id: None,
            child_job_ids: Vec::new(),
            summary: summary.clone(),
            tool: tool.clone(),
            args,
        });
        state.store.write_worker_checkpoint(
            &worker.id,
            &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
        )?;

        let pause_reason = format!("Waiting for approval to run {}.", tool);
        state.store.update_job(
            job_id,
            JobPatch {
                state: Some("paused".to_string()),
                last_error: Some(pause_reason.clone()),
                ..JobPatch::default()
            },
        )?;
        *worker = state.store.update_worker(
            &worker.id,
            WorkerPatch {
                state: Some("paused".to_string()),
                tool_call_count: Some(*tool_calls),
                last_error: Some(pause_reason.clone()),
                ..WorkerPatch::default()
            },
        )?;
        state.store.update_session(
            &session.session.id,
            SessionPatch {
                state: Some("paused".to_string()),
                last_error: Some(pause_reason.clone()),
                ..SessionPatch::default()
            },
        )?;
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job_id.to_string(),
            worker_id: Some(worker.id.clone()),
            event_type: "approval.requested".to_string(),
            status: "paused".to_string(),
            summary: format!("Approval required for {}", tool),
            detail: summary,
            data_json: json!({
                "tool_id": tool,
                "tool_call_id": tool_call_id,
                "approval_id": approval.id,
            }),
        });
        let _ = try_record_audit_event(
            state,
            AuditEventRecord {
                kind: "job.approval.requested".to_string(),
                target: format!("approval:{}", approval.id),
                status: "pending".to_string(),
                summary: format!("Queued approval for {}.", tool),
                detail: format!(
                    "job_id={} worker_id={} tool_call_id={}",
                    job_id, worker.id, tool_call.id
                ),
            },
        )
        .await;
        if let Ok(updated) = state.store.get_session(&session.session.id) {
            let _ = publish_session_event(state, updated).await;
        }
        publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
        publish_worker_updated(state, worker).await;
        publish_approval_requested(state, &approval).await;
        publish_prompt_status(
            state,
            &session.session,
            worker,
            "paused",
            "Waiting for approval",
            &pause_reason,
            &[],
        )
        .await;
        let _ = publish_overview_event(state).await;
        return Ok(LoopDisposition::Return);
    }

    let pending = PendingToolAction {
        action_kind: "tool".to_string(),
        tool_call_id,
        approval_id: None,
        command_session_id: None,
        child_job_ids: Vec::new(),
        summary,
        tool,
        args,
    };
    execute_pending_tool_action(
        state, session, job_id, worker, checkpoint, step, tool_calls, cancel_rx, pending,
    )
    .await
}

async fn handle_child_job_proposal(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    summary: String,
    jobs: Vec<ChildJobProposal>,
) -> Result<LoopDisposition> {
    if worker.parent_worker_id.is_some() {
        bail!("only the root Utility Worker may spawn subtasks");
    }
    if jobs.is_empty() {
        bail!("spawn_child_jobs requires at least one child job");
    }
    if jobs.len() > JOB_MAX_CHILDREN_PER_FANOUT {
        bail!(
            "spawn_child_jobs supports at most {} child jobs per action",
            JOB_MAX_CHILDREN_PER_FANOUT
        );
    }

    let mut child_job_ids = Vec::with_capacity(jobs.len());
    for proposal in jobs {
        let child_job_id = create_child_job(state, session, job_id, worker, proposal).await?;
        child_job_ids.push(child_job_id);
    }

    *step += 1;
    *worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            step_count: Some(*step),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    checkpoint.pending_action = Some(PendingToolAction {
        action_kind: "child_jobs".to_string(),
        tool_call_id: String::new(),
        approval_id: None,
        command_session_id: None,
        child_job_ids: child_job_ids.clone(),
        summary: summary.clone(),
        tool: String::new(),
        args: Value::Null,
    });
    state.store.write_worker_checkpoint(
        &worker.id,
        &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
    )?;

    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "child.jobs.spawned".to_string(),
        status: "running".to_string(),
        summary: format!("Spawned {} child jobs", child_job_ids.len()),
        detail: summary.clone(),
        data_json: json!({
            "child_job_ids": child_job_ids,
        }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    publish_prompt_status(
        state,
        &session.session,
        worker,
        "running",
        "Spawning Utility Subworkers",
        &summary,
        &[],
    )
    .await;
    Ok(LoopDisposition::Continue)
}

async fn park_worker_wait(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &WorkerCheckpoint,
    summary: String,
    until: WaitUntil,
    max_wait_seconds: Option<u64>,
    wake_note: Option<String>,
) -> Result<()> {
    let started_at = unix_timestamp();
    let wait = WorkerWaitRecord {
        id: Uuid::new_v4().to_string(),
        summary: summary.clone(),
        until,
        max_wait_seconds,
        wake_note,
        started_at,
        last_checked_at: None,
    };
    let wait_json = serde_json::to_value(&wait).context("failed to encode worker wait")?;
    state.store.write_worker_checkpoint(
        &worker.id,
        &serde_json::to_value(checkpoint).context("failed to encode worker checkpoint")?,
    )?;
    state.store.update_job(
        job_id,
        JobPatch {
            state: Some("waiting".to_string()),
            last_error: Some(wait_status_text(&wait, started_at)),
            ..JobPatch::default()
        },
    )?;
    *worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("waiting".to_string()),
            wait_until_json: Some(Some(wait_json.clone())),
            wait_started_at: Some(Some(started_at)),
            last_error: Some(wait_status_text(&wait, started_at)),
            ..WorkerPatch::default()
        },
    )?;
    let data_json = wait_audit_data(&wait);
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "worker.wait.started".to_string(),
        status: "waiting".to_string(),
        summary: format!("Started worker wait {}", wait.id),
        detail: summary,
        data_json: data_json.clone(),
    });
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: "worker.wait.started".to_string(),
            target: format!("worker:{}", worker.id),
            status: "waiting".to_string(),
            summary: format!("Started worker wait {}.", wait.id),
            detail: serde_json::to_string(&data_json).unwrap_or_else(|_| "{}".to_string()),
        },
    )
    .await;
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    publish_prompt_status(
        state,
        &session.session,
        worker,
        "waiting",
        "Utility Worker waiting",
        &wait_status_text(&wait, started_at),
        &[],
    )
    .await;
    let _ = publish_overview_event(state).await;
    Ok(())
}

async fn retry_worker_final_answer(
    state: &AppState,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: usize,
    event_summary: &str,
    reason: &str,
    retry_prompt: &str,
    rejected_final_answer: &str,
) -> Result<()> {
    checkpoint.next_prompt = Some(retry_prompt.to_string());
    state.store.write_worker_checkpoint(
        &worker.id,
        &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
    )?;
    *step += 1;
    *worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("running".to_string()),
            step_count: Some(*step),
            tool_call_count: Some(tool_calls),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "worker.retry".to_string(),
        status: "retrying".to_string(),
        summary: event_summary.to_string(),
        detail: excerpt(rejected_final_answer, 320),
        data_json: json!({ "reason": reason }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    Ok(())
}

async fn record_worker_progress_update(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: usize,
    summary: &str,
    detail: &str,
) -> Result<()> {
    checkpoint.next_prompt = Some(build_progress_update_continuation_prompt(summary, detail));
    state.store.write_worker_checkpoint(
        &worker.id,
        &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
    )?;
    *step += 1;
    *worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("running".to_string()),
            step_count: Some(*step),
            tool_call_count: Some(tool_calls),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "worker.progress".to_string(),
        status: "running".to_string(),
        summary: summary.to_string(),
        detail: excerpt(detail, 1_200),
        data_json: json!({ "terminal": false }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    publish_prompt_status(
        state,
        &session.session,
        worker,
        "running",
        summary,
        &excerpt(detail, 320),
        &[],
    )
    .await;
    Ok(())
}

async fn create_child_job(
    state: &AppState,
    session: &SessionDetail,
    parent_job_id: &str,
    parent_worker: &WorkerSummary,
    proposal: ChildJobProposal,
) -> Result<String> {
    create_child_job_with_limits(
        state,
        session,
        parent_job_id,
        parent_worker,
        proposal,
        ChildJobRunLimits {
            max_steps: configured_child_job_max_steps(),
            max_tool_calls: configured_child_job_max_tool_calls(),
            max_wall_clock_secs: configured_job_max_wall_clock_secs(),
        },
    )
    .await
}

#[derive(Debug, Clone, Copy)]
struct ChildJobRunLimits {
    max_steps: usize,
    max_tool_calls: usize,
    max_wall_clock_secs: u64,
}

async fn create_child_job_with_limits(
    state: &AppState,
    session: &SessionDetail,
    parent_job_id: &str,
    parent_worker: &WorkerSummary,
    proposal: ChildJobProposal,
    limits: ChildJobRunLimits,
) -> Result<String> {
    let title = proposal.title.trim();
    if title.is_empty() {
        bail!("child job titles must not be empty");
    }
    let prompt = proposal.prompt.trim();
    if prompt.is_empty() {
        bail!("child job prompts must not be empty");
    }
    // `working_dir` is per-child and scope-checked here. It is not a lock:
    // parents that spawn write-capable siblings should pass a dedicated
    // worktree path for each child.
    let working_dir = if let Some(value) = proposal.working_dir.as_deref() {
        resolve_scoped_path_in_roots(
            parent_worker,
            value,
            &parent_worker.read_roots,
            false,
            "read",
        )?
    } else {
        PathBuf::from(&parent_worker.working_dir)
    };
    let read_roots = if proposal.working_dir.is_some() {
        vec![working_dir.display().to_string()]
    } else {
        parent_worker.read_roots.clone()
    };

    let child_job_id = Uuid::new_v4().to_string();
    let child_worker_id = Uuid::new_v4().to_string();
    let child_job = state.store.create_job(JobRecord {
        id: child_job_id.clone(),
        session_id: Some(session.session.id.clone()),
        parent_job_id: Some(parent_job_id.to_string()),
        template_id: None,
        title: format!("Child {}", title),
        purpose: title.to_string(),
        trigger_kind: "child_job".to_string(),
        state: "queued".to_string(),
        requested_by: "agent".to_string(),
        prompt_excerpt: excerpt(prompt, 160),
        publication_intent_text: Some(prompt.to_string()),
    })?;
    let child_working_dir = working_dir.display().to_string();
    if child_job.publication_requested {
        record_publication_git_hygiene_baseline(state, &child_job, &child_working_dir)?;
    }
    state.store.create_worker(WorkerRecord {
        id: child_worker_id.clone(),
        job_id: child_job_id.clone(),
        parent_worker_id: Some(parent_worker.id.clone()),
        title: format!("Child utility worker: {}", title),
        lane: "utility".to_string(),
        state: "queued".to_string(),
        provider: parent_worker.provider.clone(),
        model: parent_worker.model.clone(),
        provider_base_url: parent_worker.provider_base_url.clone(),
        provider_api_key: parent_worker.provider_api_key.clone(),
        provider_session_id: String::new(),
        working_dir: child_working_dir,
        read_roots,
        write_roots: Vec::new(),
        max_steps: limits.max_steps,
        max_tool_calls: limits.max_tool_calls,
        max_wall_clock_secs: limits.max_wall_clock_secs,
    })?;
    state.store.update_job(
        &child_job_id,
        JobPatch {
            root_worker_id: Some(child_worker_id.clone()),
            ..JobPatch::default()
        },
    )?;
    state
        .store
        .replace_tool_capability_grants(&child_worker_id, &child_worker_capabilities())?;
    let child_worker = state
        .store
        .get_job(&child_job_id)?
        .workers
        .into_iter()
        .find(|item| item.id == child_worker_id)
        .ok_or_else(|| {
            anyhow!(
                "Utility Subworker '{}' was not found after creation",
                child_worker_id
            )
        })?;

    let checkpoint = WorkerCheckpoint {
        session_id: session.session.id.clone(),
        prompt_text: prompt.to_string(),
        images: Vec::new(),
        conversation: vec![CheckpointMessage {
            role: "system".to_string(),
            content: worker_system_prompt(&child_worker),
            images: Vec::new(),
            compacted: false,
            compacted_range: None,
        }],
        next_prompt: None,
        pending_action: None,
        browser_verification_final_answer_rejected: false,
        patch_loop_guardrail_triggered: false,
    };
    state.store.write_worker_checkpoint(
        &child_worker.id,
        &serde_json::to_value(checkpoint)
            .context("failed to encode Utility Subworker checkpoint")?,
    )?;

    publish_job_created(state, &child_job).await;
    publish_worker_updated(state, &child_worker).await;
    publish_job_updated(state, &state.store.get_job(parent_job_id)?.job).await;
    spawn_job_task(state.clone(), child_job_id.clone());
    Ok(child_job_id)
}

fn build_child_job_results_prompt(summary: &str, results: &[Value]) -> String {
    format!(
        "Child job results are ready.\nReason for the fan-out: {}\nStructured results:\n{}\n\
Return one JSON action for the next step. If the work is done, return final_answer with a complete user-facing answer.",
        summary,
        serde_json::to_string_pretty(results)
            .unwrap_or_else(|_| Value::Array(results.to_vec()).to_string())
    )
}

fn is_pending_child_job_action(pending: &PendingToolAction) -> bool {
    pending.action_kind == "child_jobs" || !pending.child_job_ids.is_empty()
}

fn worker_wall_clock_exceeded(worker: &WorkerSummary, now: i64) -> bool {
    worker.max_wall_clock_secs > 0
        && now.saturating_sub(worker.created_at) >= worker.max_wall_clock_secs as i64
}

fn wait_audit_data(wait: &WorkerWaitRecord) -> Value {
    json!({
        "wait_id": wait.id,
        "summary": excerpt(&wait.summary, 240),
        "until": &wait.until,
        "max_wait_seconds": wait.max_wait_seconds,
        "started_at": wait.started_at,
    })
}

fn wait_status_text(wait: &WorkerWaitRecord, now: i64) -> String {
    let waited = now.saturating_sub(wait.started_at).max(0);
    format!(
        "waiting until {} (woke in {})",
        wait_condition_label(&wait.until),
        format_duration(wait_remaining_seconds(wait, now).unwrap_or(0).max(0) as u64)
            .unwrap_or_else(|| "0s".to_string())
    )
    .replace(
        "(woke in 0s)",
        &format!(
            "(waiting for {} elapsed)",
            format_duration(waited as u64).unwrap_or_else(|| "0s".to_string())
        ),
    )
}

fn wait_condition_label(until: &WaitUntil) -> String {
    match until {
        WaitUntil::DelaySeconds { delay_seconds } => {
            format!(
                "delay of {}",
                format_duration(*delay_seconds).unwrap_or_else(|| format!("{delay_seconds}s"))
            )
        }
        WaitUntil::AbsoluteUnix { absolute_unix } => {
            format!("unix time {absolute_unix}")
        }
        WaitUntil::AuditEvent {
            event_kind,
            target_pattern,
            status,
        } => {
            let mut parts = vec![format!("audit event '{event_kind}'")];
            if let Some(target_pattern) =
                target_pattern.as_deref().filter(|value| !value.is_empty())
            {
                parts.push(format!("target matching '{target_pattern}'"));
            }
            if let Some(status) = status.as_deref().filter(|value| !value.is_empty()) {
                parts.push(format!("status '{status}'"));
            }
            parts.join(", ")
        }
        WaitUntil::ChildJobsCompleted { job_ids } => {
            format!("{} child job(s) complete", job_ids.len())
        }
        WaitUntil::ArtifactKind {
            job_id,
            artifact_kind,
        } => {
            format!("artifact kind '{artifact_kind}' on job {job_id}")
        }
    }
}

fn wait_remaining_seconds(wait: &WorkerWaitRecord, now: i64) -> Option<i64> {
    let condition_remaining = match &wait.until {
        WaitUntil::DelaySeconds { delay_seconds } => Some(
            wait.started_at
                .saturating_add(*delay_seconds as i64)
                .saturating_sub(now),
        ),
        WaitUntil::AbsoluteUnix { absolute_unix } => Some(absolute_unix.saturating_sub(now)),
        WaitUntil::AuditEvent { .. }
        | WaitUntil::ChildJobsCompleted { .. }
        | WaitUntil::ArtifactKind { .. } => None,
    };
    let cap_remaining = wait.max_wait_seconds.map(|max_wait_seconds| {
        wait.started_at
            .saturating_add(max_wait_seconds as i64)
            .saturating_sub(now)
    });

    match (condition_remaining, cap_remaining) {
        (Some(condition), Some(cap)) => Some(condition.min(cap)),
        (Some(condition), None) => Some(condition),
        (None, Some(cap)) => Some(cap),
        (None, None) => None,
    }
}

fn format_duration(seconds: u64) -> Option<String> {
    if seconds == 0 {
        return Some("0s".to_string());
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        Some(format!("{hours}h {minutes}m"))
    } else if minutes > 0 {
        Some(format!("{minutes}m {secs}s"))
    } else {
        Some(format!("{secs}s"))
    }
}

fn child_job_result_json(detail: &JobDetail) -> Result<Value> {
    let report = detail
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "child-report")
        .map(|artifact| artifact.preview_text.clone())
        .unwrap_or_default();
    Ok(json!({
        "job_id": detail.job.id,
        "title": detail.job.title,
        "state": detail.job.state,
        "purpose": detail.job.purpose,
        "result_summary": detail.job.result_summary,
        "last_error": detail.job.last_error,
        "worker_count": detail.job.worker_count,
        "report": report,
        "outcome": {
            "publication_requested": detail.job.publication_requested,
            "publication_status": detail.job.publication_status,
            "publication_summary": detail.job.publication_summary,
            "pr_url": detail.job.pr_url,
            "source_branch": detail.job.source_branch,
            "target_branch": detail.job.target_branch,
            "validation_status": detail.job.validation_status,
            "browser_verification_required": detail.job.browser_verification_required,
            "browser_verification_status": detail.job.browser_verification_status,
            "browser_verification_summary": detail.job.browser_verification_summary,
            "browser_verification_artifact_ids": detail.job.browser_verification_artifact_ids,
            "cleanup_status": detail.job.cleanup_status,
            "cleanup_paths": detail.job.cleanup_paths,
        },
        "artifact_count": detail.job.artifact_count,
        "command_session_count": detail.command_sessions.len(),
        "tool_call_count": detail.tool_calls.len(),
        "worker_notes": detail
            .workers
            .iter()
            .map(|worker| json!({
                "id": worker.id,
                "title": worker.title,
                "state": worker.state,
                "working_dir": worker.working_dir,
                "last_error": worker.last_error,
            }))
            .collect::<Vec<_>>(),
        "events": detail
            .events
            .iter()
            .rev()
            .take(4)
            .map(|event| json!({
                "event_type": event.event_type,
                "status": event.status,
                "summary": event.summary,
                "detail": event.detail,
                "data_json": event.data_json,
            }))
            .collect::<Vec<_>>(),
        "report_path": detail
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "child-report")
            .map(|artifact| artifact.path.clone())
            .unwrap_or_default(),
    }))
}

async fn complete_job_with_final_answer(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    step_count: usize,
    tool_call_count: usize,
    summary: &str,
    final_answer: &str,
    final_answer_metadata: &Value,
    final_answer_artifacts: &[FinalAnswerArtifact],
) -> Result<()> {
    let detail = state.store.get_job(job_id)?;
    let mut publication_patch = publication_outcome_patch_with_metadata(
        &detail.job,
        summary,
        final_answer,
        final_answer_metadata,
        step_count,
        tool_call_count,
    );
    apply_publication_temp_hygiene(
        &detail,
        &session.session.working_dir,
        &mut publication_patch,
    );
    state
        .agent
        .terminate_job_command_sessions(
            job_id,
            "The job completed and closed any remaining Nucleus-owned command sessions.",
            "closed",
        )
        .await;

    if detail.job.browser_verification_required
        && matches!(
            detail.job.browser_verification_status.as_str(),
            "pending" | "not_performed"
        )
    {
        let summary = if detail.job.browser_verification_artifact_ids.is_empty() {
            "Browser verification was not performed before completion.".to_string()
        } else {
            "Browser evidence was captured, but no verification outcome was asserted before completion."
                .to_string()
        };
        let _ = state.store.update_job(
            job_id,
            JobPatch {
                browser_verification_status: Some("not_performed".to_string()),
                browser_verification_summary: Some(summary.clone()),
                ..JobPatch::default()
            },
        );
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job_id.to_string(),
            worker_id: Some(worker.id.clone()),
            event_type: "job.browser_verification.completed".to_string(),
            status: "not_performed".to_string(),
            summary: "Completed, not browser-verified".to_string(),
            detail: summary,
            data_json: json!({ "reason": "completion_without_verification" }),
        });
    }

    let completion_job = state.store.get_job(job_id)?.job;
    reconcile_publication_browser_status_with_completion(&completion_job, &mut publication_patch);

    let mut visible_turn_id = None;
    let mut report_artifact = None;
    let mut structured_artifacts = Vec::new();
    for artifact in final_answer_artifacts {
        let artifact = write_job_artifact(
            state,
            job_id,
            Some(&worker.id),
            None,
            text_artifact_with_metadata(
                &artifact.kind,
                artifact.title.clone(),
                "md",
                "text/markdown",
                artifact.content.clone(),
                artifact.metadata.clone(),
            ),
        )?;
        structured_artifacts.push(artifact);
    }
    let mut post_turn_memory_outcomes: Vec<MemoryOutcome> = Vec::new();
    if detail.job.parent_job_id.is_none() {
        let final_turn_id = Uuid::new_v4().to_string();
        state.store.append_session_turn(
            &session.session.id,
            &final_turn_id,
            "assistant",
            &final_answer,
            &[],
        )?;
        post_turn_memory_outcomes = crate::extract_memory_decisions_after_turn(
            state,
            &session.session.id,
            Some(&final_turn_id),
        )
        .await;
        visible_turn_id = Some(final_turn_id);
        state.store.update_session(
            &session.session.id,
            SessionPatch {
                state: Some("active".to_string()),
                last_error: Some(String::new()),
                ..SessionPatch::default()
            },
        )?;
    } else {
        let artifact = write_job_artifact(
            state,
            job_id,
            Some(&worker.id),
            None,
            text_artifact(
                "child-report",
                format!("{} report", detail.job.title),
                "md",
                "text/markdown",
                final_answer.to_string(),
            ),
        )?;
        report_artifact = Some(artifact);
    }

    state.store.update_job(
        job_id,
        JobPatch {
            state: Some("completed".to_string()),
            visible_turn_id,
            result_summary: Some(summary.to_string()),
            last_error: Some(String::new()),
            publication_requested: publication_patch.publication_requested,
            publication_status: publication_patch.publication_status.clone(),
            publication_summary: publication_patch.publication_summary.clone(),
            pr_url: publication_patch.pr_url.clone(),
            source_branch: publication_patch.source_branch.clone(),
            target_branch: publication_patch.target_branch.clone(),
            validation_status: publication_patch.validation_status.clone(),
            browser_verification_status: publication_patch.browser_verification_status.clone(),
            cleanup_status: publication_patch.cleanup_status.clone(),
            cleanup_paths: publication_patch.cleanup_paths.clone(),
            ..JobPatch::default()
        },
    )?;
    *worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("completed".to_string()),
            step_count: Some(step_count),
            tool_call_count: Some(tool_call_count),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    let terminal_metadata = final_answer_terminal_metadata(
        summary,
        final_answer,
        final_answer_metadata,
        &structured_artifacts,
        step_count,
        tool_call_count,
        &publication_patch,
    );
    let terminal_status = terminal_metadata
        .get("terminal_status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_string();
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "job.completed".to_string(),
        status: terminal_status,
        summary: summary.to_string(),
        detail: excerpt(&final_answer, 320),
        data_json: terminal_metadata,
    });
    if let Some(publication_status) = publication_patch.publication_status.as_deref() {
        if publication_patch.publication_requested.unwrap_or(false) {
            let blocked = matches!(publication_status, "blocked" | "not_opened" | "failed");
            let _ = state.store.append_job_event(JobEventRecord {
                job_id: job_id.to_string(),
                worker_id: Some(worker.id.clone()),
                event_type: if blocked {
                    "job.publication.blocked".to_string()
                } else {
                    "job.publication.completed".to_string()
                },
                status: publication_status.to_string(),
                summary: publication_patch
                    .publication_summary
                    .clone()
                    .unwrap_or_else(|| summary.to_string()),
                detail: excerpt(&final_answer, 320),
                data_json: json!({
                    "publication_requested": publication_patch.publication_requested.unwrap_or(true),
                    "publication_status": publication_status,
                    "pr_url": publication_patch.pr_url.clone().unwrap_or_default(),
                    "source_branch": publication_patch.source_branch.clone().unwrap_or_default(),
                    "target_branch": publication_patch.target_branch.clone().unwrap_or_default(),
                    "validation_status": publication_patch.validation_status.clone().unwrap_or_default(),
                    "browser_verification_status": publication_patch.browser_verification_status.clone().unwrap_or_default(),
                    "cleanup_status": publication_patch.cleanup_status.clone().unwrap_or_default(),
                    "cleanup_paths": publication_patch.cleanup_paths.clone().unwrap_or_default(),
                }),
            });
            if publication_patch.cleanup_status.as_deref() == Some("cleanup_required") {
                let cleanup_paths = publication_patch.cleanup_paths.clone().unwrap_or_default();
                let _ = state.store.append_job_event(JobEventRecord {
                    job_id: job_id.to_string(),
                    worker_id: Some(worker.id.clone()),
                    event_type: "job.publication.cleanup_required".to_string(),
                    status: "cleanup_required".to_string(),
                    summary: "Publication job left repo-local temp artifacts".to_string(),
                    detail: if cleanup_paths.is_empty() {
                        "Nucleus marked cleanup as required for this publication job.".to_string()
                    } else {
                        format!(
                            "New repo-local temp paths were detected after job start: {}",
                            cleanup_paths.join(", ")
                        )
                    },
                    data_json: json!({
                        "cleanup_status": "cleanup_required",
                        "cleanup_paths": cleanup_paths,
                    }),
                });
            }
        }
    }
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: "session.job.completed".to_string(),
            target: format!("job:{job_id}"),
            status: "success".to_string(),
            summary: format!(
                "Completed Utility Worker job for session '{}'.",
                session.session.title
            ),
            detail: format!(
                "session_id={} provider={} model={} steps={} tool_calls={}",
                session.session.id, worker.provider, worker.model, step_count, tool_call_count
            ),
        },
    )
    .await;

    if detail.job.parent_job_id.is_none() {
        if let Ok(updated) = state.store.get_session(&session.session.id) {
            let _ = publish_session_event(state, updated).await;
        }
        publish_prompt_status(
            state,
            &session.session,
            worker,
            "completed",
            "Utility Worker completed",
            "Nucleus persisted a clean assistant turn from the Utility Worker result.",
            &post_turn_memory_outcomes,
        )
        .await;
    } else {
        if let Some(artifact) = report_artifact.as_ref() {
            publish_artifact_added(state, artifact).await;
        }
        for artifact in &structured_artifacts {
            publish_artifact_added(state, artifact).await;
        }
        if let Some(parent_job_id) = detail.job.parent_job_id.as_deref() {
            publish_job_updated(state, &state.store.get_job(parent_job_id)?.job).await;
        }
    }
    if detail.job.parent_job_id.is_none() {
        for artifact in &structured_artifacts {
            publish_artifact_added(state, artifact).await;
        }
    }

    publish_job_completed(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    let _ = publish_overview_event(state).await;
    Ok(())
}

async fn complete_job_with_budget_checkpoint(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &WorkerCheckpoint,
    step_count: usize,
    tool_call_count: usize,
    budget_kind: &str,
) -> Result<()> {
    let summary = format!("Reached current {budget_kind} budget");
    let final_answer = build_budget_checkpoint_answer(
        session,
        worker,
        checkpoint,
        step_count,
        tool_call_count,
        budget_kind,
    );
    let metadata = json!({});
    complete_job_with_final_answer(
        state,
        session,
        job_id,
        worker,
        step_count,
        tool_call_count,
        &summary,
        &final_answer,
        &metadata,
        &[],
    )
    .await
}

fn build_budget_checkpoint_answer(
    session: &SessionDetail,
    worker: &WorkerSummary,
    checkpoint: &WorkerCheckpoint,
    step_count: usize,
    tool_call_count: usize,
    budget_kind: &str,
) -> String {
    let limit = match budget_kind {
        "action" => worker.max_tool_calls,
        "wall-clock" => worker.max_wall_clock_secs as usize,
        _ => worker.max_steps,
    };
    let latest_checkpoint = checkpoint
        .next_prompt
        .as_deref()
        .or_else(|| {
            checkpoint
                .conversation
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content.as_str())
        })
        .map(|value| excerpt(value, 1_200))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "No checkpoint detail was available.".to_string());
    let pending = checkpoint
        .pending_action
        .as_ref()
        .map(|action| {
            format!(
                "\n\nPending action: {} ({})",
                action.tool,
                excerpt(&action.summary, 240)
            )
        })
        .unwrap_or_default();
    let project = if session.session.working_dir.is_empty() {
        "the current workspace".to_string()
    } else {
        session.session.working_dir.clone()
    };

    format!(
        "Nucleus reached the current {budget_kind} budget for this run ({step_count} steps, {tool_call_count} actions, limit {limit}) while working in {project}.\n\nLatest checkpoint:\n{latest_checkpoint}{pending}\n\nSend a follow-up such as \"continue from the checkpoint\" to give Nucleus a fresh run budget without losing the visible session context."
    )
}

fn build_initial_step_prompt(
    session: &SessionSummary,
    prompt: &str,
    worker: &WorkerSummary,
) -> String {
    let project_context = if session.projects.is_empty() {
        format!(
            "No project is attached. Working directory: {}",
            session.working_dir
        )
    } else {
        format!(
            "Primary working directory: {}\nAttached projects:\n{}",
            session.working_dir,
            session
                .projects
                .iter()
                .map(|project| format!("- {} ({})", project.title, project.absolute_path))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "You are handling a Nucleus-owned session prompt.\n\
Session title: {}\n\
{}\n\
Visible provider: {} / {}\n\
Utility Worker provider: {} / {}\n\
Prompt-time context and user request:\n{}\n\
Return one JSON action for the next step. If repo or workspace inspection is needed, return a tool_call. If you need to persist a non-terminal checkpoint, return progress_update. Only return final_answer when it is a complete terminal response, never an action plan, progress update, or a description of what should happen next.\n\
If the current user request corrects, refines, or challenges the previous answer, treat it as a continuation of the unresolved task. Do not merely acknowledge or restate the correction; use the visible conversation history to continue troubleshooting or answer the corrected question.",
        session.title,
        project_context,
        session.provider,
        if session.model.is_empty() {
            "default"
        } else {
            session.model.as_str()
        },
        worker.provider,
        if worker.model.is_empty() {
            "default"
        } else {
            worker.model.as_str()
        },
        prompt
    )
}

fn add_publication_initial_prompt_guidance(
    state: &AppState,
    job: &JobSummary,
    prompt: String,
) -> String {
    if !job.publication_requested {
        return prompt;
    }

    let job_tmp_dir = state
        .store
        .state_dir_path()
        .join("jobs")
        .join(&job.id)
        .join("tmp");
    let _ = fs::create_dir_all(&job_tmp_dir);
    format!(
        "{}\n\nPublication job requirements:\n\
- The user request is publication-oriented, so the final response JSON must include explicit terminal metadata for publication_status, validation_status, browser_verification_status, and cleanup_status. Put these as JSON fields, not prose labels inside the visible final_answer message.\n\
- Allowed publication_status values: not_requested, opened, not_opened, blocked, failed.\n\
- Allowed validation_status values: passed, failed, not_performed, unavailable.\n\
- Allowed browser_verification_status values: not_required, pending, passed, failed, not_performed, unavailable.\n\
- Allowed cleanup_status values: clean, cleaned, cleanup_required, unknown.\n\
- Include pr_url, source_branch, and target_branch when known.\n\
- Use the daemon-owned Browser tools for rendered verification when possible. Do not create repo-local Playwright projects, .tmp-playwright, or ad-hoc .tmp-* verification folders.\n\
- Missing Browser or Playwright tooling is not a generic job failure. Record browser_verification_status=unavailable or not_performed with the concrete reason.\n\
- Put scratch verification scripts outside the git worktree when you need them. Scoped file tools can only write within worker write roots; use shell commands for daemon-owned scratch work in this job temp directory: {}\n\
- Before terminal final_answer, check for job-created .tmp-* leftovers in the repo. Clean only files this job created, or set cleanup_status=cleanup_required and list the paths.",
        prompt,
        job_tmp_dir.display()
    )
}

fn publication_job_temp_dir(state: &AppState, job_id: &str) -> PathBuf {
    state
        .store
        .state_dir_path()
        .join("jobs")
        .join(job_id)
        .join("tmp")
}

fn record_publication_git_hygiene_baseline(
    state: &AppState,
    job: &JobSummary,
    working_dir: &str,
) -> Result<()> {
    let job_tmp_dir = publication_job_temp_dir(state, &job.id);
    fs::create_dir_all(&job_tmp_dir)
        .with_context(|| format!("failed to create job temp dir '{}'", job_tmp_dir.display()))?;
    let temp_paths = collect_repo_temp_paths(working_dir);
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job.id.clone(),
        worker_id: None,
        event_type: "job.publication.git_baseline".to_string(),
        status: "captured".to_string(),
        summary: "Captured publication git hygiene baseline".to_string(),
        detail: if temp_paths.is_empty() {
            "No repo-local .tmp-* leftovers were present at job start.".to_string()
        } else {
            format!(
                "Repo-local temp paths present at job start: {}",
                temp_paths.join(", ")
            )
        },
        data_json: json!({
            "working_dir": working_dir,
            "job_tmp_dir": job_tmp_dir.display().to_string(),
            "temp_paths": temp_paths,
        }),
    });
    Ok(())
}

fn add_budget_guidance(
    prompt: String,
    worker: &WorkerSummary,
    step_count: usize,
    tool_call_count: usize,
) -> String {
    let final_step = worker.max_steps > 0 && worker.max_steps.saturating_sub(step_count) <= 1;
    let final_action =
        worker.max_tool_calls > 0 && worker.max_tool_calls.saturating_sub(tool_call_count) <= 1;

    if !final_step && !final_action {
        return prompt;
    }

    format!(
        "{}\n\nBudget note: this run is at the edge of its current {}budget. Prefer final_answer now with a clear summary of completed work, latest evidence, remaining blocker, and exact continuation point. Only call another tool if that single action is decisive and worth checkpointing immediately afterward.",
        prompt,
        if final_step && final_action {
            "step and action "
        } else if final_step {
            "step "
        } else {
            "action "
        }
    )
}

fn build_tool_result_prompt(tool: &str, summary: &str, result: &Value) -> String {
    format!(
        "Tool result for {}.\nReason for the call: {}\nStructured result:\n{}\n\
Return one JSON action for the next step. If the work is done, return final_answer with a complete user-facing answer. If the work is not done but a durable checkpoint is useful, return progress_update and continue afterward.",
        tool,
        summary,
        format_tool_result(result)
    )
}

fn build_tool_denied_prompt(tool: &str, summary: &str, reason: &str) -> String {
    format!(
        "Nucleus did not allow {}.\nReason for the proposed action: {}\nResolution detail: {}\n\
Return one JSON action for the next step. If the work can still be completed without this mutation, return final_answer with a complete user-facing answer.",
        tool, summary, reason
    )
}

fn should_retry_internal_action_item_final_answer(
    final_answer: &str,
    tool_call_count: usize,
) -> bool {
    if tool_call_count > 0 {
        return false;
    }

    let normalized = normalize_action_item_text(final_answer);
    if normalized.is_empty() {
        return false;
    }

    normalized.starts_with("next single step")
        || normalized.starts_with("single step")
        || normalized.starts_with("next step")
        || normalized.starts_with("check whether ")
        || normalized.starts_with("inspect ")
        || normalized.starts_with("confirm ")
        || normalized.starts_with("find the ")
        || normalized.starts_with("look for ")
}

fn should_retry_incomplete_progress_final_answer(
    summary: &str,
    final_answer: &str,
    execution_mode: &str,
    worker: &WorkerSummary,
    step_count: usize,
    tool_call_count: usize,
) -> bool {
    if execution_mode == "plan" || !has_remaining_worker_budget(worker, step_count, tool_call_count)
    {
        return false;
    }

    let text = normalize_action_item_text(&format!("{}\n{}", summary, final_answer));
    if text.is_empty() || contains_blocked_or_waiting_language(&text) {
        return false;
    }

    contains_incomplete_work_language(&text)
}

fn has_remaining_worker_budget(
    worker: &WorkerSummary,
    step_count: usize,
    tool_call_count: usize,
) -> bool {
    let has_step_budget = worker.max_steps == 0 || step_count.saturating_add(1) < worker.max_steps;
    let has_action_budget =
        worker.max_tool_calls == 0 || tool_call_count.saturating_add(1) < worker.max_tool_calls;
    has_step_budget && has_action_budget
}

fn contains_incomplete_work_language(text: &str) -> bool {
    [
        "not complete",
        "not completed",
        "not done",
        "not finished",
        "isn't complete",
        "isnt complete",
        "isn't done",
        "isnt done",
        "still not complete",
        "still incomplete",
        "still needs",
        "remaining work",
        "work remains",
        "left to do",
        "todo remains",
        "follow-up needed",
        "more remains",
        "need to continue",
        "needs further",
        "remaining refactor",
        "remaining refactors",
        "remaining task",
        "remaining tasks",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn contains_blocked_or_waiting_language(text: &str) -> bool {
    [
        "status: blocked",
        "blocked_without",
        "blocked without",
        "blocked, not browser",
        "blocked by",
        "blocked on",
        "cannot continue",
        "can't continue",
        "cant continue",
        "need your approval",
        "requires your approval",
        "waiting for approval",
        "waiting for you",
        "need you to",
        "requires user",
        "permission denied",
        "access denied",
        "budget exhausted",
        "reached the current step budget",
        "reached the current action budget",
        "reached the current run budget",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn should_retry_zero_tool_action_final_answer(
    job: &JobSummary,
    summary: &str,
    final_answer: &str,
    execution_mode: &str,
    worker: &WorkerSummary,
    step_count: usize,
    tool_call_count: usize,
    child_job_count: usize,
) -> bool {
    if execution_mode == "plan"
        || tool_call_count > 0
        || child_job_count > 0
        || !has_remaining_worker_budget(worker, step_count, tool_call_count)
        || !job_prompt_requires_action(job)
    {
        return false;
    }

    let text = normalize_action_item_text(&format!("{summary}\n{final_answer}"));
    !(final_answer_requests_confirmation(&text) || final_answer_reports_concrete_blocker(&text))
}

fn job_prompt_requires_action(job: &JobSummary) -> bool {
    if job.publication_requested {
        return true;
    }

    let text = normalize_action_item_text(&format!(
        "{}\n{}\n{}",
        job.title, job.purpose, job.prompt_excerpt
    ));
    if text.is_empty() || action_text_is_informational(&text) {
        return false;
    }
    if action_text_requests_text_only_artifact(&text) {
        return false;
    }

    let phrase_match = [
        "approved pr",
        "can merge",
        "merge pr",
        "merge pull request",
        "merge #",
        "merge into",
        "merge to",
        "delete the branch",
        "delete branch",
        "open a pr",
        "open the pr",
        "create a pr",
        "open a pull request",
        "create a pull request",
        "publish this branch",
        "publish the branch",
        "implement",
        "fix ",
        "edit ",
        "run ",
        "validate",
        "test ",
        "file issue",
        "create issue",
        "comment on",
        "post a comment",
        "commit the",
        "commit changes",
        "make a commit",
        "push ",
        "ship ",
        "deploy ",
        "publish release",
        "release to",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    phrase_match || contains_normalized_word(&text, "repair")
}

fn contains_normalized_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(index, _)| {
        let before = text[..index]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric());
        let after_index = index + word.len();
        let after = text[after_index..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric());
        before && after
    })
}

fn action_text_is_informational(text: &str) -> bool {
    text.starts_with("how ")
        || text.starts_with("what ")
        || text.starts_with("why ")
        || text.starts_with("explain ")
        || text.starts_with("summarize ")
        || text.contains("how do i ")
        || text.contains("how should i ")
        || text.contains("what is ")
}

fn action_text_requests_text_only_artifact(text: &str) -> bool {
    let text_only_verb = [
        "draft", "write", "generate", "compose", "prepare", "suggest", "provide",
    ]
    .iter()
    .any(|verb| {
        text.starts_with(&format!("{verb} "))
            || text.contains(&format!(" {verb} "))
            || text.contains(&format!(" {verb} a "))
            || text.contains(&format!(" {verb} an "))
            || text.contains(&format!(" {verb} the "))
    });
    if !text_only_verb {
        return false;
    }

    [
        "commit message",
        "commit title",
        "release note",
        "release notes",
        "issue comment",
        "pr comment",
        "pull request comment",
        "pr description",
        "pull request description",
        "pr summary",
        "pull request summary",
        "pr body",
        "pull request body",
        "implementation prompt",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn final_answer_requests_confirmation(text: &str) -> bool {
    [
        "please confirm",
        "confirm before",
        "need your confirmation",
        "need confirmation",
        "need your approval",
        "requires your approval",
        "waiting for approval",
        "should i proceed",
        "do you want me to",
        "would you like me to",
        "can i proceed",
        "may i proceed",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn final_answer_reports_concrete_blocker(text: &str) -> bool {
    [
        "status: blocked",
        "blocked by",
        "blocked on",
        "cannot continue",
        "can't continue",
        "cant continue",
        "cannot ",
        "can't ",
        "cant ",
        "unable to",
        "permission denied",
        "access denied",
        "missing ",
        "ambiguous",
        "not possible",
        "impossible",
        "no matching",
        "could not find",
        "couldn't find",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn should_retry_unsupported_confident_negative_final_answer(
    detail: &JobDetail,
    summary: &str,
    final_answer: &str,
    execution_mode: &str,
    checkpoint: &WorkerCheckpoint,
    worker: &WorkerSummary,
    step_count: usize,
    tool_call_count: usize,
) -> bool {
    if execution_mode == "plan"
        || !has_remaining_worker_budget(worker, step_count, tool_call_count)
        || !confident_negative_claim(summary, final_answer)
    {
        return false;
    }

    let task_text = evidence_task_text(detail, checkpoint);
    let missing_pr_review_evidence = requires_pr_review_thread_evidence(&task_text)
        && !has_thread_aware_pr_review_evidence(detail, &task_text, summary, final_answer);
    let missing_test_evidence = requires_test_validation_evidence(&task_text)
        && !has_test_validation_evidence(detail, &task_text, summary, final_answer);
    let zero_test_misread = confident_test_success_claim(summary, final_answer)
        && has_zero_test_no_match_evidence(detail);
    let missing_pr_lifecycle_state = confident_pr_lifecycle_claim(summary, final_answer)
        && !has_direct_pr_state_evidence_for_claim(detail, &task_text, summary, final_answer);
    let unsupported_pr_merged_claim = confident_pr_merged_claim(summary, final_answer)
        && !has_direct_pr_merged_evidence_for_claim(detail, &task_text, summary, final_answer);
    let challenged_clean_answer = requires_pr_review_thread_evidence(&task_text)
        && repeated_grounding_challenge(&task_text)
        && !has_thread_aware_pr_review_evidence(detail, &task_text, summary, final_answer);

    (missing_pr_review_evidence
        || missing_test_evidence
        || zero_test_misread
        || missing_pr_lifecycle_state
        || unsupported_pr_merged_claim
        || challenged_clean_answer)
        && !final_answer_reports_concrete_blocker(&normalize_action_item_text(final_answer))
}

fn evidence_task_text(detail: &JobDetail, checkpoint: &WorkerCheckpoint) -> String {
    let mut parts = vec![
        detail.job.title.as_str(),
        detail.job.purpose.as_str(),
        detail.job.prompt_excerpt.as_str(),
        checkpoint.prompt_text.as_str(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    parts.extend(
        checkpoint
            .conversation
            .iter()
            .filter(|message| message.role == "user")
            .map(|message| message.content.clone()),
    );
    normalize_action_item_text(&parts.join("\n"))
}

fn requires_pr_review_thread_evidence(text: &str) -> bool {
    let pr_context = text.contains(" pr ")
        || !extract_pr_numbers(text).is_empty()
        || text.contains("pull request")
        || text.contains("github pr")
        || text.contains("github pull request");
    let review_context = [
        "latest feedback",
        "review feedback",
        "review comments",
        "requested changes",
        "unresolved comment",
        "unresolved review",
        "inline comment",
        "inline review",
        "codex review",
        "actionable feedback",
        "pr feedback",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    pr_context && review_context
}

fn repeated_grounding_challenge(text: &str) -> bool {
    [
        "are you sure",
        "check again",
        "look again",
        "recheck",
        "double check",
        "i can see",
        "screenshot",
        "you missed",
        "still says",
        "that's not right",
        "that is not right",
        "not true",
        "you said",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn requires_test_validation_evidence(text: &str) -> bool {
    [
        "failed tests",
        "failing tests",
        "test failures",
        "checks failed",
        "failed checks",
        "validation",
        "tests pass",
        "tests passed",
        "run tests",
        "check failed tests",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn confident_negative_claim(summary: &str, final_answer: &str) -> bool {
    let text = normalize_action_item_text(&format!("{summary}\n{final_answer}"));
    [
        "no actionable feedback",
        "nothing actionable",
        "no actionable comments",
        "no requested changes",
        "no unresolved comments",
        "no unresolved review",
        "looks clean",
        "everything looks clean",
        "nothing to fix",
        "no failed tests",
        "no failed checks",
        "tests passed",
        "all tests passed",
        "validation passed",
        "checks passed",
        "clean to merge",
        "ready to merge",
        "clear to merge",
        "approved",
        "mergeable",
        "pr is open",
        "pull request is open",
        "pr is closed",
        "pull request is closed",
        "already merged",
        "has already been merged",
        "nothing left to merge",
        "nothing to merge",
        "no merge left",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn confident_pr_lifecycle_claim(summary: &str, final_answer: &str) -> bool {
    let text = normalize_action_item_text(&format!("{summary}\n{final_answer}"));
    [
        "already merged",
        "has already been merged",
        "nothing left to merge",
        "nothing to merge",
        "ready to merge",
        "clean to merge",
        "clear to merge",
        "mergeable",
        "approved",
        "pr is open",
        "pull request is open",
        "pr is closed",
        "pull request is closed",
        "merge state",
        "review decision",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn confident_pr_merged_claim(summary: &str, final_answer: &str) -> bool {
    let text = normalize_action_item_text(&format!("{summary}\n{final_answer}"));
    [
        "already merged",
        "has already been merged",
        "was already merged",
        "it is merged",
        "it's merged",
        "pr is merged",
        "pull request is merged",
        "nothing left to merge",
        "nothing to merge",
        "no merge left",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn confident_test_success_claim(summary: &str, final_answer: &str) -> bool {
    let text = normalize_action_item_text(&format!("{summary}\n{final_answer}"));
    [
        "no failed tests",
        "no failing tests",
        "tests passed",
        "all tests passed",
        "validation passed",
        "checks passed",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn has_thread_aware_pr_review_evidence(
    detail: &JobDetail,
    task_text: &str,
    summary: &str,
    final_answer: &str,
) -> bool {
    let pr_numbers = extract_pr_numbers(&format!("{task_text}\n{summary}\n{final_answer}"));
    if pr_numbers.is_empty() {
        return detail.tool_calls.iter().any(|tool_call| {
            tool_call.status == "completed"
                && tool_call
                    .result_json
                    .as_ref()
                    .is_some_and(value_has_complete_review_threads)
        });
    }
    pr_numbers.iter().all(|pr_number| {
        detail.tool_calls.iter().any(|tool_call| {
            tool_call.status == "completed"
                && tool_call.result_json.as_ref().is_some_and(|result| {
                    value_pr_number(result).is_some_and(|number| number == *pr_number)
                        && value_has_complete_review_threads(result)
                })
        })
    })
}

fn has_test_validation_evidence(
    detail: &JobDetail,
    task_text: &str,
    summary: &str,
    final_answer: &str,
) -> bool {
    let combined_text =
        normalize_action_item_text(&format!("{task_text}\n{summary}\n{final_answer}"));
    let pr_numbers = extract_pr_numbers(&combined_text);
    let requires_pr_status_checks =
        !pr_numbers.is_empty() && requires_pr_status_check_evidence(&combined_text);

    if requires_pr_status_checks {
        return pr_numbers.iter().all(|pr_number| {
            detail.tool_calls.iter().any(|tool_call| {
                tool_call.status == "completed"
                    && tool_call.result_json.as_ref().is_some_and(|result| {
                        value_pr_number(result).is_some_and(|number| number == *pr_number)
                            && value_has_successful_status_check_rollup(result)
                    })
            })
        });
    }

    if detail.tool_calls.iter().any(|tool_call| {
        tool_call.status == "completed"
            && tool_call.result_json.as_ref().is_some_and(|result| {
                tool_call.tool_id == "tests.run" && value_has_successful_test_run(result)
            })
    }) {
        return true;
    }

    if pr_numbers.is_empty() {
        return detail.tool_calls.iter().any(|tool_call| {
            tool_call.status == "completed"
                && tool_call
                    .result_json
                    .as_ref()
                    .is_some_and(value_has_successful_status_check_rollup)
        });
    }
    pr_numbers.iter().all(|pr_number| {
        detail.tool_calls.iter().any(|tool_call| {
            tool_call.status == "completed"
                && tool_call.result_json.as_ref().is_some_and(|result| {
                    value_pr_number(result).is_some_and(|number| number == *pr_number)
                        && value_has_successful_status_check_rollup(result)
                })
        })
    })
}

fn requires_pr_status_check_evidence(text: &str) -> bool {
    [
        "check passed",
        "checks passed",
        "no failed checks",
        "ci passed",
        "status check",
        "status checks",
        "github action",
        "github actions",
        "workflow passed",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn has_zero_test_no_match_evidence(detail: &JobDetail) -> bool {
    for tool_call in detail
        .tool_calls
        .iter()
        .rev()
        .filter(|tool_call| tool_call.status == "completed")
    {
        let Some(result) = tool_call.result_json.as_ref() else {
            continue;
        };
        if result
            .pointer("/validation_interpretation/status")
            .and_then(Value::as_str)
            == Some("no_tests_matched")
        {
            return true;
        }
        if tool_call.tool_id == "tests.run" && value_has_successful_test_run(result) {
            return false;
        }
    }
    false
}

fn has_direct_pr_merged_evidence(detail: &JobDetail) -> bool {
    detail.tool_calls.iter().any(|tool_call| {
        tool_call.status == "completed"
            && (tool_call.tool_id == "github.pr_state"
                || tool_call
                    .result_json
                    .as_ref()
                    .is_some_and(value_has_pr_state_evidence))
            && tool_call
                .result_json
                .as_ref()
                .is_some_and(value_says_pr_merged)
    })
}

fn has_direct_pr_state_evidence_for_claim(
    detail: &JobDetail,
    task_text: &str,
    summary: &str,
    final_answer: &str,
) -> bool {
    let pr_numbers = extract_pr_numbers(&format!("{task_text}\n{summary}\n{final_answer}"));
    if pr_numbers.is_empty() {
        return detail.tool_calls.iter().any(|tool_call| {
            tool_call.status == "completed"
                && (tool_call.tool_id == "github.pr_state"
                    || tool_call
                        .result_json
                        .as_ref()
                        .is_some_and(value_has_pr_state_evidence))
        });
    }
    pr_numbers.iter().all(|pr_number| {
        detail
            .tool_calls
            .iter()
            .any(|tool_call| completed_pr_state_tool_call_matches_number(tool_call, *pr_number))
    })
}

fn has_direct_pr_merged_evidence_for_claim(
    detail: &JobDetail,
    task_text: &str,
    summary: &str,
    final_answer: &str,
) -> bool {
    let pr_numbers = extract_pr_numbers(&format!("{task_text}\n{summary}\n{final_answer}"));
    if pr_numbers.is_empty() {
        return has_direct_pr_merged_evidence(detail);
    }
    pr_numbers.iter().all(|pr_number| {
        detail.tool_calls.iter().any(|tool_call| {
            completed_pr_state_tool_call_matches_number(tool_call, *pr_number)
                && tool_call
                    .result_json
                    .as_ref()
                    .is_some_and(value_says_pr_merged)
        })
    })
}

fn completed_pr_state_tool_call_matches_number(
    tool_call: &nucleus_protocol::ToolCallSummary,
    pr_number: u64,
) -> bool {
    tool_call.status == "completed"
        && (tool_call.tool_id == "github.pr_state"
            || tool_call
                .result_json
                .as_ref()
                .is_some_and(value_has_pr_state_evidence))
        && tool_call
            .result_json
            .as_ref()
            .and_then(value_pr_number)
            .is_some_and(|number| number == pr_number)
}

fn value_pr_number(value: &Value) -> Option<u64> {
    value
        .get("pr_number")
        .or_else(|| value.get("number"))
        .and_then(Value::as_u64)
}

fn extract_pr_numbers(text: &str) -> BTreeSet<u64> {
    let mut numbers = BTreeSet::new();
    let normalized = normalize_action_item_text(text);
    let tokens = normalized
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '#'))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut pr_list_active = false;
    for (index, token) in tokens.iter().enumerate() {
        if let Some(number) = token
            .strip_prefix("pr#")
            .and_then(|value| value.parse().ok())
        {
            numbers.insert(number);
            pr_list_active = true;
            continue;
        }
        if pr_list_active {
            if let Some(number) = token.strip_prefix('#').and_then(|value| value.parse().ok()) {
                numbers.insert(number);
                continue;
            }
            if matches!(*token, "and" | "request" | "requests") {
                continue;
            }
            pr_list_active = false;
        }
        if matches!(*token, "pr" | "prs") {
            if let Some(number) = tokens
                .get(index + 1)
                .and_then(|next| parse_pr_reference_number(next))
            {
                numbers.insert(number);
                pr_list_active = true;
            }
            continue;
        }
        if *token == "pull" && tokens.get(index + 1).copied() == Some("request") {
            if let Some(number) = tokens
                .get(index + 2)
                .and_then(|next| parse_pr_reference_number(next))
            {
                numbers.insert(number);
                pr_list_active = true;
            }
        }
        if *token == "pull" && tokens.get(index + 1).copied() == Some("requests") {
            if let Some(number) = tokens
                .get(index + 2)
                .and_then(|next| parse_pr_reference_number(next))
            {
                numbers.insert(number);
                pr_list_active = true;
            }
        }
    }
    numbers
}

fn parse_pr_reference_number(token: &str) -> Option<u64> {
    token.trim_start_matches('#').parse().ok()
}

fn value_has_complete_review_threads(value: &Value) -> bool {
    if value.get("review_threads").is_some() || value.get("reviewThreads").is_some() {
        let comments_truncated = value
            .get("thread_comments_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let threads_complete = value
            .get("review_threads_complete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return threads_complete && !comments_truncated;
    }
    match value {
        Value::Array(items) => items.iter().any(value_has_complete_review_threads),
        Value::Object(object) => object.values().any(value_has_complete_review_threads),
        _ => false,
    }
}

fn value_has_pr_state_evidence(value: &Value) -> bool {
    if value.get("evidence_kind").and_then(Value::as_str) == Some("github_pr_state") {
        return true;
    }
    if value.get("mergedAt").is_some()
        || value.get("merged_at").is_some()
        || value.get("mergeStateStatus").is_some()
        || value.get("merge_state_status").is_some()
    {
        return true;
    }
    match value {
        Value::Array(items) => items.iter().any(value_has_pr_state_evidence),
        Value::Object(object) => object.values().any(value_has_pr_state_evidence),
        _ => false,
    }
}

fn value_says_pr_merged(value: &Value) -> bool {
    value
        .get("state")
        .or_else(|| value.get("pr_state"))
        .and_then(Value::as_str)
        .is_some_and(|state| state.eq_ignore_ascii_case("MERGED"))
        || value
            .get("mergedAt")
            .or_else(|| value.get("merged_at"))
            .and_then(Value::as_str)
            .is_some_and(|merged_at| !merged_at.trim().is_empty())
}

fn value_has_successful_status_check_rollup(value: &Value) -> bool {
    if let Some(rollup) = value
        .get("status_check_rollup")
        .or_else(|| value.get("statusCheckRollup"))
    {
        return rollup
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.eq_ignore_ascii_case("SUCCESS"));
    }
    match value {
        Value::Array(items) => items.iter().any(value_has_successful_status_check_rollup),
        Value::Object(object) => object
            .values()
            .any(value_has_successful_status_check_rollup),
        _ => false,
    }
}

fn value_has_successful_test_run(value: &Value) -> bool {
    value.get("exit_code").and_then(Value::as_i64) == Some(0)
        && value
            .pointer("/validation_interpretation/status")
            .and_then(Value::as_str)
            != Some("no_tests_matched")
}

#[derive(Debug, Clone, Default)]
struct PublicationOutcomePatch {
    publication_requested: Option<bool>,
    publication_status: Option<String>,
    publication_summary: Option<String>,
    pr_url: Option<String>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    validation_status: Option<String>,
    browser_verification_status: Option<String>,
    cleanup_status: Option<String>,
    cleanup_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
struct PublicationTempBaseline {
    working_dir: String,
    temp_paths: Vec<String>,
}

fn final_answer_terminal_metadata(
    summary: &str,
    final_answer: &str,
    final_answer_metadata: &Value,
    final_answer_artifacts: &[ArtifactSummary],
    step_count: usize,
    tool_call_count: usize,
    publication_patch: &PublicationOutcomePatch,
) -> Value {
    let text = normalize_action_item_text(&format!("{summary}\n{final_answer}"));
    let blocked = contains_blocked_terminal_result_language(&text)
        || publication_patch_terminal_status_is_blocked(publication_patch);
    let mut metadata = json!({
        "step_count": step_count,
        "tool_call_count": tool_call_count,
        "terminal_status": if blocked { "blocked" } else { "completed" },
        "blocked": blocked,
    });
    if final_answer_metadata
        .as_object()
        .is_some_and(|object| !object.is_empty())
    {
        metadata["final_response_metadata"] = final_answer_metadata.clone();
    }
    if !final_answer_artifacts.is_empty() {
        metadata["final_response_artifacts"] = json!(
            final_answer_artifacts
                .iter()
                .map(|artifact| {
                    json!({
                        "id": artifact.id,
                        "kind": artifact.kind,
                        "title": artifact.title,
                        "path": artifact.path,
                        "metadata_json": artifact.metadata_json,
                    })
                })
                .collect::<Vec<_>>()
        );
    }

    if let Some(status) = infer_browser_verification_status(&text) {
        metadata["browser_verification_status"] = Value::String(status.to_string());
    }
    if publication_patch.publication_requested.unwrap_or(false) {
        metadata["publication_requested"] = Value::Bool(true);
        if let Some(value) = publication_patch.publication_status.as_deref() {
            metadata["publication_status"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.publication_summary.as_deref() {
            metadata["publication_summary"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.pr_url.as_deref() {
            metadata["pr_url"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.source_branch.as_deref() {
            metadata["source_branch"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.target_branch.as_deref() {
            metadata["target_branch"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.validation_status.as_deref() {
            metadata["validation_status"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.browser_verification_status.as_deref() {
            metadata["browser_verification_status"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.cleanup_status.as_deref() {
            metadata["cleanup_status"] = Value::String(value.to_string());
        }
        if let Some(value) = publication_patch.cleanup_paths.as_ref() {
            metadata["cleanup_paths"] = json!(value);
        }
    }

    metadata
}

fn publication_patch_terminal_status_is_blocked(patch: &PublicationOutcomePatch) -> bool {
    patch.publication_requested.unwrap_or(false)
        && patch
            .publication_status
            .as_deref()
            .is_some_and(|status| matches!(status, "blocked" | "not_opened" | "failed"))
}

fn contains_blocked_terminal_result_language(text: &str) -> bool {
    [
        "status: blocked",
        "publication status: blocked",
        "blocked_without",
        "blocked without browser",
        "blocked, not browser",
        "blocked by",
        "blocked on",
        "cannot continue",
        "cannot honestly open the pr",
        "unable to continue",
        "unable to complete",
        "unable to proceed",
        "unable to perform",
        "permission denied",
        "access denied",
        "not possible",
        "reached the current step budget",
        "reached the current action budget",
        "reached the current run budget",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn infer_browser_verification_status(text: &str) -> Option<&'static str> {
    if text.contains("browser verification status: not_required")
        || text.contains("browser verification status: not required")
        || text.contains("browser verification: status: not_required")
        || text.contains("browser verification: status: not required")
        || text.contains("browser verification: not required")
    {
        return Some("not_required");
    }

    if text.contains("browser verification status: pending")
        || text.contains("browser verification: status: pending")
        || text.contains("browser verification: pending")
    {
        return Some("pending");
    }

    if text.contains("browser verification status: unavailable")
        || text.contains("browser verification: status: unavailable")
        || text.contains("browser verification: unavailable")
        || text.contains("verification unavailable")
    {
        return Some("unavailable");
    }

    if text.contains("browser verification status: not_performed")
        || text.contains("browser verification status: not performed")
        || text.contains("browser verification: status: not_performed")
        || text.contains("browser verification: status: not performed")
        || text.contains("browser verification: not performed")
        || text.contains("not browser-verified")
        || text.contains("not browser verified")
    {
        return Some("not_performed");
    }

    if text.contains("browser verification status: failed")
        || text.contains("browser verification: status: failed")
        || text.contains("browser verification: failed")
        || text.contains("browser verification failed")
    {
        return Some("failed");
    }

    if text.contains("browser verification status: passed")
        || text.contains("browser verification: status: passed")
        || text.contains("browser verification: passed")
        || text.contains("browser-verified")
        || text.contains("browser verified")
    {
        return Some("passed");
    }

    None
}

fn publication_outcome_patch(
    current: &JobSummary,
    summary: &str,
    final_answer: &str,
    _step_count: usize,
    _tool_call_count: usize,
) -> PublicationOutcomePatch {
    publication_outcome_patch_with_metadata(
        current,
        summary,
        final_answer,
        &json!({}),
        _step_count,
        _tool_call_count,
    )
}

fn publication_outcome_patch_with_metadata(
    current: &JobSummary,
    summary: &str,
    final_answer: &str,
    final_answer_metadata: &Value,
    _step_count: usize,
    _tool_call_count: usize,
) -> PublicationOutcomePatch {
    if !current.publication_requested {
        return PublicationOutcomePatch::default();
    }

    let raw_text = format!("{summary}\n{final_answer}");
    let normalized = normalize_action_item_text(&raw_text);
    let blocked = contains_blocked_terminal_result_language(&normalized);
    let pr_url = final_response_metadata_string(
        final_answer_metadata,
        &["pr_url", "pr url", "pull_request", "pull request"],
    )
    .or_else(|| extract_labeled_value(&raw_text, &["pr_url", "pr url", "pull request"]))
    .filter(|value| value.starts_with("http"));
    let publication_status = final_response_metadata_string(
        final_answer_metadata,
        &["publication_status", "publication status"],
    )
    .or_else(|| {
        final_response_metadata_nested_string(
            final_answer_metadata,
            &["publication", "publication_outcome", "publication outcome"],
            &["status"],
        )
    })
    .or_else(|| extract_labeled_value(&raw_text, &["publication_status", "publication status"]))
    .or_else(|| {
        extract_nested_labeled_value(
            &raw_text,
            &["publication", "publication outcome"],
            &["status"],
        )
    })
    .and_then(|value| normalize_publication_status(&value))
    .or_else(|| {
        if pr_url.is_some() {
            Some("opened".to_string())
        } else if publication_text_says_opened(&normalized) {
            Some("opened".to_string())
        } else if normalized.contains("publication failed") || normalized.contains("pr failed") {
            Some("failed".to_string())
        } else if blocked {
            Some("blocked".to_string())
        } else if normalized.contains("pr not opened")
            || normalized.contains("not opened")
            || normalized.contains("did not open")
        {
            Some("not_opened".to_string())
        } else {
            match current.publication_status.as_str() {
                "" | "not_requested" => Some("blocked".to_string()),
                _ => Some(current.publication_status.clone()),
            }
        }
    });
    let validation_status = final_response_metadata_string(
        final_answer_metadata,
        &["validation_status", "validation status"],
    )
    .or_else(|| {
        final_response_metadata_nested_string(final_answer_metadata, &["validation"], &["status"])
    })
    .or_else(|| extract_labeled_value(&raw_text, &["validation_status", "validation status"]))
    .or_else(|| extract_nested_labeled_value(&raw_text, &["validation"], &["status"]))
    .and_then(|value| normalize_validation_status(&value))
    .or_else(|| infer_validation_status(&normalized))
    .unwrap_or_else(|| current.validation_status.clone());
    let metadata_browser_verification_status =
        final_response_browser_verification_status(final_answer_metadata);
    let text_browser_verification_status = extract_labeled_value(
        &raw_text,
        &["browser_verification_status", "browser verification status"],
    )
    .or_else(|| {
        extract_nested_labeled_value(
            &raw_text,
            &["browser_verification", "browser verification"],
            &["status"],
        )
    })
    .and_then(|value| normalize_browser_verification_status(&value))
    .or_else(|| infer_browser_verification_status(&normalized).map(str::to_string));
    let browser_verification_status = if current.browser_verification_required {
        metadata_browser_verification_status
            .or(text_browser_verification_status)
            .or_else(|| match current.browser_verification_status.as_str() {
                "" | "pending" => None,
                status => Some(status.to_string()),
            })
            .unwrap_or_else(|| current.browser_verification_status.clone())
    } else {
        metadata_browser_verification_status
            .or(text_browser_verification_status)
            .unwrap_or_else(|| current.browser_verification_status.clone())
    };
    let cleanup_status = final_response_metadata_string(
        final_answer_metadata,
        &["cleanup_status", "cleanup status"],
    )
    .or_else(|| {
        final_response_metadata_nested_string(final_answer_metadata, &["cleanup"], &["status"])
    })
    .or_else(|| extract_labeled_value(&raw_text, &["cleanup_status", "cleanup status"]))
    .or_else(|| extract_nested_labeled_value(&raw_text, &["cleanup"], &["status"]))
    .and_then(|value| normalize_cleanup_status(&value))
    .or_else(|| infer_cleanup_status(&normalized))
    .unwrap_or_else(|| current.cleanup_status.clone());
    let cleanup_paths = extract_cleanup_paths(&raw_text, &current.cleanup_paths);

    PublicationOutcomePatch {
        publication_requested: Some(true),
        publication_status,
        publication_summary: Some(
            final_response_metadata_string(
                final_answer_metadata,
                &["publication_summary", "publication summary"],
            )
            .or_else(|| {
                final_response_metadata_nested_string(
                    final_answer_metadata,
                    &["publication", "publication_outcome", "publication outcome"],
                    &["summary"],
                )
            })
            .or_else(|| {
                extract_labeled_value(&raw_text, &["publication_summary", "publication summary"])
            })
            .or_else(|| {
                extract_nested_labeled_value(
                    &raw_text,
                    &["publication", "publication outcome"],
                    &["summary"],
                )
            })
            .unwrap_or_else(|| summary.to_string()),
        ),
        pr_url: Some(pr_url.unwrap_or_else(|| current.pr_url.clone())),
        source_branch: Some(
            final_response_metadata_string(
                final_answer_metadata,
                &["source_branch", "source branch"],
            )
            .or_else(|| extract_labeled_value(&raw_text, &["source_branch", "source branch"]))
            .unwrap_or_else(|| current.source_branch.clone()),
        ),
        target_branch: Some(
            final_response_metadata_string(
                final_answer_metadata,
                &["target_branch", "target branch"],
            )
            .or_else(|| extract_labeled_value(&raw_text, &["target_branch", "target branch"]))
            .unwrap_or_else(|| current.target_branch.clone()),
        ),
        validation_status: Some(validation_status),
        browser_verification_status: Some(browser_verification_status),
        cleanup_status: Some(cleanup_status),
        cleanup_paths: Some(cleanup_paths),
    }
}

fn apply_publication_temp_hygiene(
    detail: &JobDetail,
    working_dir: &str,
    patch: &mut PublicationOutcomePatch,
) {
    if !detail.job.publication_requested {
        return;
    }

    let Some(baseline) = publication_temp_baseline(detail) else {
        return;
    };
    let working_dir = if baseline.working_dir.trim().is_empty() {
        working_dir
    } else {
        baseline.working_dir.as_str()
    };
    let baseline_paths = baseline.temp_paths.into_iter().collect::<BTreeSet<_>>();
    let current = collect_repo_temp_paths(working_dir)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let new_paths = current
        .difference(&baseline_paths)
        .cloned()
        .collect::<Vec<String>>();
    if new_paths.is_empty() {
        return;
    }

    patch.cleanup_status = Some("cleanup_required".to_string());
    let mut cleanup_paths = patch.cleanup_paths.clone().unwrap_or_default();
    for path in new_paths {
        if !cleanup_paths.iter().any(|existing| existing == &path) {
            cleanup_paths.push(path);
        }
    }
    patch.cleanup_paths = Some(cleanup_paths);
}

fn reconcile_publication_browser_status_with_completion(
    completion_job: &JobSummary,
    patch: &mut PublicationOutcomePatch,
) {
    if !patch.publication_requested.unwrap_or(false) {
        return;
    }
    if matches!(
        completion_job.browser_verification_status.as_str(),
        "passed" | "failed"
    ) {
        patch.browser_verification_status =
            Some(completion_job.browser_verification_status.clone());
        return;
    }
    if !matches!(
        patch.browser_verification_status.as_deref(),
        None | Some("pending")
    ) {
        return;
    }
    if completion_job.browser_verification_status == "pending" {
        return;
    }

    patch.browser_verification_status = Some(completion_job.browser_verification_status.clone());
}

fn publication_temp_baseline(detail: &JobDetail) -> Option<PublicationTempBaseline> {
    detail
        .events
        .iter()
        .rev()
        .find(|event| event.event_type == "job.publication.git_baseline")
        .map(|event| {
            let temp_paths = event
                .data_json
                .get("temp_paths")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let working_dir = event
                .data_json
                .get("working_dir")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            PublicationTempBaseline {
                working_dir,
                temp_paths,
            }
        })
}

fn collect_repo_temp_paths(working_dir: &str) -> Vec<String> {
    let root = Path::new(working_dir);
    if working_dir.trim().is_empty() || !root.is_dir() {
        return Vec::new();
    }

    let mut paths = collect_git_temp_paths(root);
    if paths.is_empty() {
        paths = collect_top_level_temp_paths(root);
    }
    paths.sort();
    paths.dedup();
    paths
}

fn collect_git_temp_paths(root: &Path) -> Vec<String> {
    let Some(repo_root) = git_worktree_root(root) else {
        return Vec::new();
    };

    let mut paths = collect_git_status_temp_paths(root, &repo_root);
    paths.extend(collect_git_tracked_temp_paths(root, &repo_root));
    paths
}

fn git_worktree_root(root: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

fn collect_git_status_temp_paths(root: &Path, repo_root: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let entries = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        let entry = entries[index];
        index += 1;
        if entry.len() <= 3 || entry[2] != b' ' {
            continue;
        }

        let status = &entry[..2];
        if status[0] != b'D' && status[1] != b'D' {
            collect_existing_temp_path(repo_root, &entry[3..], &mut paths);
        }

        if (status[0] == b'R' || status[1] == b'R' || status[0] == b'C' || status[1] == b'C')
            && index < entries.len()
        {
            collect_existing_temp_path(repo_root, entries[index], &mut paths);
            index += 1;
        }
    }

    paths
}

fn collect_git_tracked_temp_paths(root: &Path, repo_root: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--full-name", "-z"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter(|path| repo_root.join(path).exists())
        .filter_map(publication_temp_root_path)
        .collect()
}

fn collect_existing_temp_path(root: &Path, path: &[u8], paths: &mut Vec<String>) {
    let Ok(path) = std::str::from_utf8(path) else {
        return;
    };
    if !root.join(path).exists() {
        return;
    }
    if let Some(path) = publication_temp_root_path(path) {
        paths.push(path);
    }
}

fn collect_top_level_temp_paths(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|path| publication_temp_root_path(&path))
        .collect()
}

fn is_publication_temp_path(path: &str) -> bool {
    publication_temp_root_path(path).is_some()
}

fn publication_temp_root_path(path: &str) -> Option<String> {
    let normalized = path.trim_start_matches("./").trim_end_matches('/');
    let mut components = Vec::new();
    for component in normalized
        .split('/')
        .filter(|component| !component.is_empty())
    {
        components.push(component);
        if is_publication_temp_component(component) {
            return Some(components.join("/"));
        }
    }
    None
}

fn is_publication_temp_component(component: &str) -> bool {
    component == ".tmp-playwright"
        || component.starts_with(".tmp-")
        || component.starts_with(".playwright-")
}

fn publication_text_says_opened(text: &str) -> bool {
    [
        "pull request opened",
        "opened pull request",
        "pr opened",
        "opened pr",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn normalize_publication_status(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .trim_matches(|character: char| character == '"' || character == '\'' || character == '`')
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_");
    if normalized.starts_with("published") {
        return Some("opened".to_string());
    }
    normalize_enum_value(
        value,
        &["not_requested", "opened", "not_opened", "blocked", "failed"],
    )
}

fn normalize_validation_status(value: &str) -> Option<String> {
    normalize_enum_value(value, &["passed", "failed", "not_performed", "unavailable"])
}

fn normalize_browser_verification_status(value: &str) -> Option<String> {
    normalize_enum_value(
        value,
        &[
            "not_required",
            "pending",
            "passed",
            "failed",
            "not_performed",
            "unavailable",
        ],
    )
}

fn normalize_cleanup_status(value: &str) -> Option<String> {
    normalize_enum_value(value, &["cleanup_required", "cleaned", "clean", "unknown"])
}

fn normalize_enum_value(value: &str, allowed: &[&str]) -> Option<String> {
    let normalized = value
        .trim()
        .trim_matches(|character: char| character == '"' || character == '\'' || character == '`')
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_");
    allowed
        .iter()
        .find(|allowed_value| normalized.starts_with(**allowed_value))
        .map(|value| (*value).to_string())
}

fn infer_validation_status(text: &str) -> Option<String> {
    if text.contains("validation failed") || text.contains("validation status: failed") {
        return Some("failed".to_string());
    }
    if text.contains("validation unavailable") || text.contains("validation status: unavailable") {
        return Some("unavailable".to_string());
    }
    if text.contains("validation not performed")
        || text.contains("validation status: not_performed")
    {
        return Some("not_performed".to_string());
    }
    if (text.contains("validation") || text.contains("tests"))
        && (text.contains(" passed") || text.contains(" green"))
    {
        return Some("passed".to_string());
    }
    None
}

fn infer_cleanup_status(text: &str) -> Option<String> {
    let normalized;
    let text = if text.chars().any(char::is_uppercase) {
        normalized = text.to_ascii_lowercase();
        normalized.as_str()
    } else {
        text
    };
    if text.contains("cleanup status: cleanup_required") || text.contains("cleanup required") {
        return Some("cleanup_required".to_string());
    }
    if text.contains("not cleaned up")
        || text.contains("was not cleaned up")
        || text.contains("were not cleaned up")
    {
        return Some("cleanup_required".to_string());
    }
    if text.contains("cleanup status: cleaned") || text.contains("cleaned up") {
        return Some("cleaned".to_string());
    }
    if text.contains("cleanup status: clean") || text.contains("branch clean") {
        return Some("clean".to_string());
    }
    if text.contains(".tmp-playwright") || text.contains(".tmp-") {
        return Some("cleanup_required".to_string());
    }
    None
}

fn extract_labeled_value(text: &str, labels: &[&str]) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches('-').trim();
        let Some((label, value)) = trimmed.split_once(':') else {
            continue;
        };
        let normalized_label = label.trim().to_ascii_lowercase().replace('_', " ");
        if labels
            .iter()
            .any(|candidate| normalized_label == candidate.replace('_', " "))
        {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn final_response_metadata_string(metadata: &Value, labels: &[&str]) -> Option<String> {
    let object = metadata.as_object()?;
    for (key, value) in object {
        let normalized_key = normalize_label(key);
        if labels
            .iter()
            .any(|candidate| normalized_key == normalize_label(candidate))
        {
            if let Some(value) = final_response_metadata_value_string(value) {
                return Some(value);
            }
        }
    }
    None
}

fn final_response_metadata_nested_string(
    metadata: &Value,
    section_labels: &[&str],
    nested_labels: &[&str],
) -> Option<String> {
    let object = metadata.as_object()?;
    for (key, value) in object {
        let normalized_key = normalize_label(key);
        if !section_labels
            .iter()
            .any(|candidate| normalized_key == normalize_label(candidate))
        {
            continue;
        }
        let Some(nested_object) = value.as_object() else {
            continue;
        };
        for (nested_key, nested_value) in nested_object {
            let normalized_nested_key = normalize_label(nested_key);
            if nested_labels
                .iter()
                .any(|candidate| normalized_nested_key == normalize_label(candidate))
            {
                if let Some(value) = final_response_metadata_value_string(nested_value) {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn final_response_browser_verification_status(metadata: &Value) -> Option<String> {
    final_response_metadata_string(
        metadata,
        &[
            "browser_verification_status",
            "browser verification status",
            "browser_status",
        ],
    )
    .or_else(|| {
        final_response_metadata_nested_string(
            metadata,
            &["browser_verification", "browser verification"],
            &["status"],
        )
    })
    .and_then(|value| normalize_browser_verification_status(&value))
}

fn final_response_metadata_value_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        Value::Array(_) | Value::Object(_) | Value::Null => None,
    }
}

fn extract_nested_labeled_value(
    text: &str,
    section_labels: &[&str],
    nested_labels: &[&str],
) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let bullet = trimmed.starts_with('-') || trimmed.starts_with('*');
        let trimmed = trimmed
            .trim_start_matches(|character| character == '-' || character == '*')
            .trim();
        let Some((label, value)) = trimmed.split_once(':') else {
            if !bullet {
                in_section = false;
            }
            continue;
        };

        let normalized_label = normalize_label(label);
        if in_section
            && nested_labels
                .iter()
                .any(|candidate| normalized_label == normalize_label(candidate))
        {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }

        if section_labels
            .iter()
            .any(|candidate| normalized_label == normalize_label(candidate))
        {
            in_section = true;
            if let Some(value) = extract_inline_nested_labeled_value(value, nested_labels) {
                return Some(value);
            }
            continue;
        }

        if !bullet {
            in_section = false;
        }
    }

    None
}

fn extract_inline_nested_labeled_value(value: &str, nested_labels: &[&str]) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut saw_known_inline_label = false;
    for segment in value.split(|character| character == ',' || character == ';') {
        let Some((label, nested_value)) = segment.trim().split_once(':') else {
            continue;
        };
        let normalized_label = normalize_label(label);
        if nested_labels
            .iter()
            .any(|candidate| normalized_label == normalize_label(candidate))
        {
            let nested_value = nested_value.trim();
            if !nested_value.is_empty() {
                return Some(nested_value.to_string());
            }
        }
        if is_known_nested_inline_label(&normalized_label) {
            saw_known_inline_label = true;
        }
    }

    if saw_known_inline_label {
        return None;
    }

    Some(value.to_string())
}

fn is_known_nested_inline_label(label: &str) -> bool {
    matches!(
        label,
        "status"
            | "summary"
            | "publication status"
            | "publication summary"
            | "pr url"
            | "pull request"
            | "source branch"
            | "target branch"
            | "validation status"
            | "browser verification status"
            | "cleanup status"
            | "cleanup paths"
    )
}

fn normalize_label(label: &str) -> String {
    label.trim().to_ascii_lowercase().replace('_', " ")
}

fn extract_cleanup_paths(text: &str, existing: &[String]) -> Vec<String> {
    let mut paths = existing.to_vec();
    for token in text.split_whitespace() {
        let cleaned = token
            .trim_matches(|character: char| {
                matches!(
                    character,
                    ',' | ';' | ':' | ')' | '(' | '[' | ']' | '"' | '\'' | '`'
                )
            })
            .trim_end_matches('.');
        if (cleaned.contains(".tmp-") || cleaned.contains(".tmp-playwright"))
            && !paths.iter().any(|path| path == cleaned)
        {
            paths.push(cleaned.to_string());
        }
    }
    paths
}

fn normalize_action_item_text(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(|character: char| {
            character == '-' || character == '*' || character == ':' || character.is_whitespace()
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn build_internal_action_item_retry_prompt(summary: &str, final_answer: &str) -> String {
    format!(
        "Your previous final_answer was an internal action item, not a user-facing answer.\n\
Previous summary: {}\n\
Previous final_answer: {}\n\
Return exactly one valid Nucleus worker action JSON object.\n\
- If you need repo, workspace, file, git, search, or process information, return a tool_call instead of describing the action.\n\
- Prefer auto-approved read actions such as project.inspect, fs.list, fs.read_text, rg.search, git.status, and git.diff when they can answer the request.\n\
- Only return final_answer when the text directly answers the user.",
        excerpt(summary, 320),
        excerpt(final_answer, 1_200)
    )
}

fn build_incomplete_progress_retry_prompt(summary: &str, final_answer: &str) -> String {
    format!(
        "Your previous final_answer said the requested work is incomplete, so it was a progress report rather than a completion answer.\n\
Previous summary: {}\n\
Previous final_answer: {}\n\
Return exactly one valid Nucleus worker action JSON object.\n\
- Do not final_answer progress updates, partial completion notes, or lists of remaining work.\n\
- Continue with the next smallest useful tool_call unless you are genuinely blocked or the run budget is exhausted.\n\
- Only return final_answer when the user's requested phase/task is fully complete and validated, or when you clearly cannot continue without user input.",
        excerpt(summary, 320),
        excerpt(final_answer, 1_200)
    )
}

fn build_zero_tool_action_retry_prompt(
    job: &JobSummary,
    summary: &str,
    final_answer: &str,
) -> String {
    format!(
        "Your previous final_answer tried to complete an action-oriented request before any tool_call ran.\n\
User request excerpt: {}\n\
Previous summary: {}\n\
Previous final_answer: {}\n\
Return exactly one valid Nucleus worker action JSON object and continue the job.\n\
- If the requested action can be performed, return the smallest useful tool_call now.\n\
- If the action is ambiguous or requires user approval, return final_answer asking the specific confirmation question.\n\
- If the action is blocked or impossible, return final_answer with the concrete blocker and evidence.\n\
- Do not restate the requested action as if it completed.",
        excerpt(&job.prompt_excerpt, 320),
        excerpt(summary, 320),
        excerpt(final_answer, 1_200)
    )
}

fn build_evidence_completion_retry_prompt(summary: &str, final_answer: &str) -> String {
    format!(
        "Your previous final_answer made a confident negative claim without complete required evidence.\n\
Previous summary: {}\n\
Previous final_answer: {}\n\
Return exactly one valid Nucleus worker action JSON object.\n\
Rules:\n\
- For PR review or latest feedback tasks, fetch thread-aware review data before saying there is no actionable feedback. Prefer github.pr_review_threads for inline review threads.\n\
- For PR lifecycle claims such as already merged, nothing left to merge, open, closed, ready, mergeable, or approved, fetch direct PR state with github.pr_state. Local git status or log output is not enough.\n\
- For test or validation tasks, only say tests/checks passed after a tests.run result or check-state evidence actually supports that. If zero tests matched, say no tests matched.\n\
- If GitHub thread retrieval is unavailable or blocked, return a concise bounded final_answer that says you could not fully verify the PR feedback yet.\n\
- If the user challenged a prior answer, use a deeper or different evidence path before repeating a clean/no-action conclusion.\n\
- Keep the eventual user-facing final_answer short and plain.",
        excerpt(summary, 400),
        excerpt(final_answer, 1_200)
    )
}

fn build_browser_verification_retry_prompt(job: &JobSummary, final_answer: &str) -> String {
    let artifact_note = if job.browser_verification_artifact_ids.is_empty() {
        "No Browser evidence artifacts are currently attached to this job.".to_string()
    } else {
        format!(
            "Current Browser evidence artifact ids: {}.",
            job.browser_verification_artifact_ids.join(", ")
        )
    };
    format!(
        "Your previous final_answer tried to complete a UI-renderable job while Browser verification was still pending.\n\
Nucleus cannot treat typecheck/build success as sufficient rendered-UI evidence.\n\
{artifact_note}\n\n\
Before returning final_answer, either:\n\
- verify through Browser tools, capture snapshot/screenshot or interaction evidence, and return final_answer with browser_verification status \"passed\" or \"failed\"; or\n\
- explicitly return final_answer with browser_verification status \"unavailable\" or \"not_performed\" and explain why rendered Browser verification cannot be done.\n\n\
Use this final_answer shape when done:\n\
{{\"kind\":\"final_answer\",\"summary\":\"why the work is complete or blocked\",\"final_answer\":\"user-facing answer\",\"browser_verification\":{{\"status\":\"passed|failed|not_performed|unavailable\",\"summary\":\"concise verification result\",\"artifact_ids\":[\"artifact-id\"]}}}}\n\n\
Rejected final_answer:\n{}",
        excerpt(final_answer, 1_200)
    )
}

fn should_retry_missing_publication_outcome(
    detail: &JobDetail,
    summary: &str,
    final_answer: &str,
    final_answer_metadata: &Value,
    browser_verification_claim: Option<&BrowserVerificationClaim>,
    worker: &WorkerSummary,
    step_count: usize,
    tool_call_count: usize,
) -> bool {
    if !detail.job.publication_requested
        || !has_remaining_worker_budget(worker, step_count, tool_call_count)
        || publication_outcome_retry_already_attempted(detail)
    {
        return false;
    }

    !publication_final_answer_has_required_facts_with_metadata(
        summary,
        final_answer,
        final_answer_metadata,
        browser_verification_claim,
    )
}

fn publication_outcome_retry_already_attempted(detail: &JobDetail) -> bool {
    detail.events.iter().any(|event| {
        event.event_type == "worker.retry"
            && event
                .data_json
                .get("reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| reason == "publication_outcome_missing")
    })
}

fn publication_final_answer_has_required_facts(summary: &str, final_answer: &str) -> bool {
    publication_final_answer_has_required_facts_with_metadata(
        summary,
        final_answer,
        &json!({}),
        None,
    )
}

fn publication_final_answer_has_required_facts_with_metadata(
    summary: &str,
    final_answer: &str,
    final_answer_metadata: &Value,
    browser_verification_claim: Option<&BrowserVerificationClaim>,
) -> bool {
    let text = format!("{summary}\n{final_answer}");
    let normalized = normalize_action_item_text(&text);
    let has_publication = final_response_metadata_string(
        final_answer_metadata,
        &[
            "publication_status",
            "publication status",
            "pr_url",
            "pr url",
        ],
    )
    .is_some()
        || final_response_metadata_nested_string(
            final_answer_metadata,
            &["publication", "publication_outcome", "publication outcome"],
            &["status"],
        )
        .and_then(|value| normalize_publication_status(&value))
        .is_some()
        || extract_labeled_value(
            &text,
            &[
                "publication_status",
                "publication status",
                "pr_url",
                "pr url",
            ],
        )
        .is_some()
        || extract_nested_labeled_value(
            &text,
            &["publication", "publication outcome"],
            &["status"],
        )
        .and_then(|value| normalize_publication_status(&value))
        .is_some()
        || normalized.contains("blocked_without_browser_verification")
        || normalized.contains("pr not opened")
        || publication_text_says_opened(&normalized);
    let has_validation = final_response_metadata_string(
        final_answer_metadata,
        &["validation_status", "validation status"],
    )
    .and_then(|value| normalize_validation_status(&value))
    .is_some()
        || final_response_metadata_nested_string(
            final_answer_metadata,
            &["validation"],
            &["status"],
        )
        .and_then(|value| normalize_validation_status(&value))
        .is_some()
        || extract_labeled_value(&text, &["validation_status", "validation status"]).is_some()
        || extract_nested_labeled_value(&text, &["validation"], &["status"])
            .and_then(|value| normalize_validation_status(&value))
            .is_some();
    let has_browser_claim = browser_verification_claim
        .and_then(|claim| normalize_browser_verification_claim_status(&claim.status))
        .is_some();
    let has_browser_metadata =
        final_response_browser_verification_status(final_answer_metadata).is_some();
    let has_browser_text = extract_labeled_value(
        &text,
        &["browser_verification_status", "browser verification status"],
    )
    .and_then(|value| normalize_browser_verification_status(&value))
    .is_some()
        || extract_nested_labeled_value(
            &text,
            &["browser_verification", "browser verification"],
            &["status"],
        )
        .and_then(|value| normalize_browser_verification_status(&value))
        .is_some()
        || infer_browser_verification_status(&normalized).is_some();
    let has_browser = has_browser_claim || has_browser_text || has_browser_metadata;
    let has_cleanup = final_response_metadata_string(
        final_answer_metadata,
        &["cleanup_status", "cleanup status"],
    )
    .and_then(|value| normalize_cleanup_status(&value))
    .is_some()
        || final_response_metadata_nested_string(final_answer_metadata, &["cleanup"], &["status"])
            .and_then(|value| normalize_cleanup_status(&value))
            .is_some()
        || extract_labeled_value(&text, &["cleanup_status", "cleanup status"]).is_some()
        || extract_nested_labeled_value(&text, &["cleanup"], &["status"])
            .and_then(|value| normalize_cleanup_status(&value))
            .is_some();

    has_publication && has_validation && has_browser && has_cleanup
}

fn build_publication_outcome_retry_prompt(summary: &str, final_answer: &str) -> String {
    format!(
        "Your previous final_answer is missing explicit terminal metadata for a publication-oriented job.\n\
Previous summary: {}\n\
Previous final_answer: {}\n\
Return exactly one valid Nucleus worker action JSON object using kind=\"final_answer\".\n\
Return a clean user-facing final_answer message plus these terminal metadata fields as JSON fields on the action or inside a structured final_answer object:\n\
- publication_status: not_requested | opened | not_opened | blocked | failed\n\
- publication_summary\n\
- pr_url, source_branch, target_branch when known\n\
- validation_status: passed | failed | not_performed | unavailable\n\
- browser_verification_status: not_required | pending | passed | failed | not_performed | unavailable\n\
- cleanup_status: clean | cleaned | cleanup_required | unknown\n\
If Browser verification is unavailable or was not performed, say that explicitly. Do not call Playwright directly or create repo-local .tmp-playwright files.",
        excerpt(summary, 320),
        excerpt(final_answer, 1_200)
    )
}

fn build_patch_loop_guardrail_prompt(initial_prompt: String) -> String {
    format!(
        "{initial_prompt}\n\nPatch-loop guardrail:\n\
Recent user feedback indicates repeated UI correction loops. Before patching further, inspect the current diff, restate the acceptance criteria, identify fragile patch-chasing changes, simplify or partially revert risky changes where appropriate, and browser-verify before final completion."
    )
}

fn should_retry_browser_verification_final_answer(
    job: &JobSummary,
    claim: Option<&BrowserVerificationClaim>,
    final_answer: &str,
    final_answer_metadata: &Value,
    checkpoint: &WorkerCheckpoint,
    worker: &WorkerSummary,
    step_after_rejection: usize,
    tool_calls: usize,
) -> bool {
    job.browser_verification_required
        && matches!(
            job.browser_verification_status.as_str(),
            "pending" | "not_performed"
        )
        && claim.is_none()
        && final_response_browser_verification_status(final_answer_metadata).is_none()
        && status_from_browser_verification_text(final_answer).is_none()
        && !checkpoint.browser_verification_final_answer_rejected
        && remaining_budget_for_browser_verification(worker, step_after_rejection, tool_calls)
}

async fn apply_browser_verification_final_state(
    state: &AppState,
    job_id: &str,
    claim: Option<BrowserVerificationClaim>,
    final_answer: &str,
    final_answer_metadata: &Value,
) -> Result<String> {
    let job = state.store.get_job(job_id)?.job;
    if !job.browser_verification_required {
        return Ok(final_answer.to_string());
    }

    let claimed_status = claim
        .as_ref()
        .and_then(|claim| normalize_browser_verification_claim_status(&claim.status))
        .map(str::to_string)
        .or_else(|| final_response_browser_verification_status(final_answer_metadata))
        .or_else(|| status_from_browser_verification_text(final_answer).map(str::to_string));
    let next_status =
        claimed_status
            .as_deref()
            .unwrap_or(match job.browser_verification_status.as_str() {
                "failed" => "failed",
                "unavailable" => "unavailable",
                _ => "not_performed",
            });
    let claim_summary = claim
        .as_ref()
        .map(|claim| claim.summary.trim())
        .filter(|value| !value.is_empty());
    let summary = claim_summary
        .map(ToOwned::to_owned)
        .or_else(|| {
            if !job.browser_verification_summary.trim().is_empty() {
                Some(job.browser_verification_summary.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| match next_status {
            "passed" => {
                "Browser verification passed according to the worker's final assertion.".to_string()
            }
            "failed" => "Browser verification failed or reported rendered-UI problems.".to_string(),
            "unavailable" => "Browser verification was unavailable for this job.".to_string(),
            "not_performed" => {
                "Browser verification was not performed before completion.".to_string()
            }
            _ => String::new(),
        });
    let claim_artifact_ids = claim
        .as_ref()
        .map(|claim| claim.artifact_ids.as_slice())
        .unwrap_or(&[]);
    let artifact_ids = append_unique_ids(
        job.browser_verification_artifact_ids.clone(),
        claim_artifact_ids,
    );

    state.store.update_job(
        job_id,
        JobPatch {
            browser_verification_status: Some(next_status.to_string()),
            browser_verification_summary: Some(summary.clone()),
            browser_verification_artifact_ids: Some(artifact_ids.clone()),
            ..JobPatch::default()
        },
    )?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: None,
        event_type: "job.browser_verification.completed".to_string(),
        status: next_status.to_string(),
        summary: browser_verification_completion_label(next_status).to_string(),
        detail: summary,
        data_json: json!({
            "status": next_status,
            "artifact_ids": artifact_ids,
        }),
    });
    Ok(final_answer.to_string())
}

fn build_progress_update_continuation_prompt(summary: &str, detail: &str) -> String {
    format!(
        "Nucleus recorded your previous response as a non-terminal progress checkpoint.\n\
Checkpoint summary: {}\n\
Checkpoint detail: {}\n\
Return exactly one valid Nucleus worker action JSON object for the next step.\n\
- Continue working from this checkpoint.\n\
- Prefer a tool_call for the next concrete repo, file, command, test, or verification action.\n\
- You may use progress_update again only for a durable checkpoint; it does not complete the job.\n\
- Use final_answer only when the requested task is complete and validated, or when you are genuinely blocked.",
        excerpt(summary, 320),
        excerpt(detail, 1_200)
    )
}

fn build_plan_mode_retry_prompt(summary: &str, attempted_action: &str) -> String {
    format!(
        "Plan mode is enabled for this session, so Nucleus must not take actions.\n\
Previous summary: {}\n\
Attempted action: {}\n\
Return exactly one valid Nucleus worker action JSON object using kind=\"final_answer\".\n\
- Do not call tools.\n\
- Do not spawn Utility Subworkers.\n\
- Do not run commands, inspect files, edit files, or assume action results.\n\
- The final_answer should be a concise user-facing plan, including assumptions or information you would need before acting.",
        excerpt(summary, 320),
        attempted_action
    )
}

fn should_attach_initial_worker_images(checkpoint: &WorkerCheckpoint) -> bool {
    !checkpoint.images.is_empty()
        && checkpoint.next_prompt.is_none()
        && checkpoint.pending_action.is_none()
}

fn worker_supports_vision_with_tools(worker: &WorkerSummary) -> bool {
    provider_supports_vision_with_tools(&worker.provider)
}

fn target_supports_vision_with_tools(target: &HiddenWorkerTarget) -> bool {
    provider_supports_vision_with_tools(&target.provider)
}

fn provider_supports_vision_with_tools(provider: &str) -> bool {
    provider == "openai_compatible"
}

const MAX_COMPACTION_PASSES: usize = 10;

async fn compact_checkpoint_if_needed(
    state: &AppState,
    session: &SessionSummary,
    worker: &WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    prompt: &str,
    images: &[SessionTurnImage],
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<()> {
    let threshold = compaction_token_threshold_for_model(&worker.model);
    for _ in 0..MAX_COMPACTION_PASSES {
        let Some(compiled_turn) =
            compile_worker_prompt_for_estimate(state, session, worker, checkpoint, prompt, images)
        else {
            return Ok(());
        };
        if !should_compact(&compiled_turn, threshold) {
            return Ok(());
        }
        let before_tokens = estimate_prompt_tokens(&compiled_turn);

        let outcome = compact_conversation(state, session, worker, checkpoint, cancel_rx).await?;
        audit_compaction_outcome(state, worker, &outcome).await;
        match outcome {
            CompactionOutcome::Applied { .. } => {
                state.store.write_worker_checkpoint(
                    &worker.id,
                    &serde_json::to_value(&*checkpoint)
                        .context("failed to encode worker checkpoint")?,
                )?;
            }
            CompactionOutcome::Skipped { reason } => {
                record_memory_audit(
                    state,
                    "memory.compaction.failed",
                    &worker.id,
                    "failed",
                    &format!(
                        "Conversation compaction skipped while prompt remained over threshold: {reason}"
                    ),
                )
                .await;
                return Ok(());
            }
            CompactionOutcome::Failed { .. } => return Ok(()),
        }

        let Some(compiled_turn) =
            compile_worker_prompt_for_estimate(state, session, worker, checkpoint, prompt, images)
        else {
            return Ok(());
        };
        let after_tokens = estimate_prompt_tokens(&compiled_turn);
        if after_tokens >= before_tokens {
            warn!(
                worker_id = worker.id.as_str(),
                before_tokens,
                after_tokens,
                "conversation compaction did not reduce prompt estimate; stopping compaction loop",
            );
            record_memory_audit(
                state,
                "memory.compaction.failed",
                &worker.id,
                "failed",
                &format!(
                    "Conversation compaction stopped because prompt estimate did not shrink (before_tokens={before_tokens}, after_tokens={after_tokens})"
                ),
            )
            .await;
            return Ok(());
        }
    }
    record_memory_audit(
        state,
        "memory.compaction.failed",
        &worker.id,
        "failed",
        "Conversation compaction stopped after reaching the maximum pass limit",
    )
    .await;
    Ok(())
}

fn compile_worker_prompt_for_estimate(
    state: &AppState,
    session: &SessionSummary,
    worker: &WorkerSummary,
    checkpoint: &WorkerCheckpoint,
    prompt: &str,
    images: &[SessionTurnImage],
) -> Option<CompiledTurn> {
    let execution = build_execution_session(worker);
    let history = checkpoint_history(&checkpoint.conversation, &execution.id);
    let prompt_body = build_worker_prompt_input(worker, &checkpoint.conversation, prompt);
    crate::compile_session_turn(state, session, &history, &prompt_body, images, "utility")
        .inspect_err(|error| {
            warn!(
                ?error,
                session_id = session.id.as_str(),
                "session-aware prompt compile failed; skipping compaction threshold check",
            );
        })
        .ok()
}

fn unsupported_vision_with_tools_detail(worker: &WorkerSummary, image_count: usize) -> String {
    let plural = if image_count == 1 { "" } else { "s" };
    format!(
        "Nucleus stored the attached image{plural} on this turn, but the selected Utility Worker runtime '{} / {}' cannot inspect image attachments while preserving the Nucleus-owned action path. Image understanding with actions currently requires an OpenAI-compatible Utility Worker model.",
        worker.provider, worker.model
    )
}

async fn call_worker_model(
    state: &AppState,
    session: Option<&SessionSummary>,
    worker: &WorkerSummary,
    conversation: &[CheckpointMessage],
    prompt: &str,
    images: &[SessionTurnImage],
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<ModelResponse> {
    let result = execute_worker_text_turn(
        state,
        session,
        worker,
        conversation,
        prompt,
        images,
        cancel_rx,
    )
    .await?;
    let registered_mcp_tool_ids = registered_mcp_tool_ids(state);
    let action = match parse_worker_action_with_registered_mcp_tools(
        &result.content,
        registered_mcp_tool_ids.iter().map(String::as_str),
    ) {
        Ok(action) => action,
        Err(error)
            if worker_supports_action_contract_repair(worker)
                && error.is_repairable_contract_error() =>
        {
            let mut repair_conversation = conversation.to_vec();
            repair_conversation.push(CheckpointMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
                images: Vec::new(),
                compacted: false,
                compacted_range: None,
            });
            repair_conversation.push(CheckpointMessage {
                role: "assistant".to_string(),
                content: result.content.clone(),
                images: Vec::new(),
                compacted: false,
                compacted_range: None,
            });
            let repair_supported_tool_ids =
                worker_action_repair_supported_tool_ids(worker, &registered_mcp_tool_ids);
            let repair_prompt = build_worker_action_repair_prompt(
                &result.content,
                &error,
                &repair_supported_tool_ids,
            );
            let repaired = execute_worker_text_turn(
                state,
                session,
                worker,
                &repair_conversation,
                &repair_prompt,
                &[],
                cancel_rx,
            )
            .await?;
            let action = parse_worker_action_with_registered_mcp_tools(
                &repaired.content,
                registered_mcp_tool_ids.iter().map(String::as_str),
            )
            .with_context(|| {
                format!(
                    "worker returned invalid Nucleus action after repair retry; original response: {}; repaired response: {}",
                    excerpt(&result.content, 220),
                    excerpt(&repaired.content, 220)
                )
            })?;
            return Ok(ModelResponse {
                action,
                raw: repaired.content,
                provider_session_id: if repaired.provider_session_id.is_empty() {
                    result.provider_session_id
                } else {
                    repaired.provider_session_id
                },
            });
        }
        Err(error) => {
            return Err(anyhow!(
                "{}; response excerpt: {}",
                error,
                excerpt(&result.content, 500)
            ));
        }
    };

    Ok(ModelResponse {
        action,
        raw: result.content,
        provider_session_id: result.provider_session_id,
    })
}

fn worker_supports_action_contract_repair(worker: &WorkerSummary) -> bool {
    // The OpenAI-compatible adapter is currently the worker path where Nucleus owns
    // the prompt envelope and JSON-object response hint end to end.
    worker.provider == "openai_compatible"
}

pub(crate) async fn execute_worker_text_turn(
    state: &AppState,
    session: Option<&SessionSummary>,
    worker: &WorkerSummary,
    conversation: &[CheckpointMessage],
    prompt: &str,
    images: &[SessionTurnImage],
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<ProviderTurnResult> {
    let (events, mut receiver) = mpsc::unbounded_channel();
    let execution = build_execution_session(worker);
    let history = checkpoint_history(conversation, &execution.id);
    let prompt_body = build_worker_prompt_input(worker, conversation, prompt);
    let runtimes = state.runtimes.clone();
    let execution_clone = execution.clone();
    let history_clone = history.clone();
    let images = images.to_vec();
    let cancel_for_runtime = cancel_rx.clone();
    // Compile the real session's context (memory layers, prompt includes,
    // skill layers, tool/mcp catalogs) so the provider call sees the same
    // system prompt the daemon advertises in debug summaries. Without this
    // wiring `execute_prompt_stream` would rebuild an empty CompiledTurn
    // (see issue #232).
    let compiled_turn = session
        .and_then(|sess| {
            crate::compile_session_turn(state, sess, &history, &prompt_body, &images, "utility")
                .inspect_err(|error| {
                    warn!(
                        ?error,
                        session_id = sess.id.as_str(),
                        "session-aware prompt compile failed; falling back to layered-empty CompiledTurn",
                    );
                })
                .ok()
        });

    let handle = tokio::spawn(async move {
        match compiled_turn {
            Some(turn) => {
                runtimes
                    .execute_compiled_turn_stream_cancellable(
                        &execution_clone,
                        std::sync::Arc::new(turn),
                        events,
                        Some(cancel_for_runtime),
                    )
                    .await
            }
            None => {
                runtimes
                    .execute_prompt_stream_cancellable(
                        &execution_clone,
                        &history_clone,
                        &prompt_body,
                        &images,
                        "utility",
                        events,
                        Some(cancel_for_runtime),
                    )
                    .await
            }
        }
    });

    let mut reasoning_buffer = String::new();
    let mut last_reasoning = String::new();
    while let Some(event) = receiver.recv().await {
        match event {
            PromptStreamEvent::ReasoningSnapshot { text } => {
                let excerpted = append_reasoning_snapshot(&mut reasoning_buffer, &text);
                if excerpted != last_reasoning {
                    last_reasoning = excerpted.clone();
                    match state.store.record_worker_reasoning(
                        &worker.id,
                        &excerpted,
                        unix_timestamp(),
                    ) {
                        Ok(summary) => {
                            publish_worker_updated(state, &summary).await;
                            if let Ok(detail) = state.store.get_job(&summary.job_id) {
                                publish_job_updated(state, &detail.job).await;
                            }
                            let _ = publish_overview_event(state).await;
                        }
                        Err(error) => warn!(
                            ?error,
                            worker_id = worker.id.as_str(),
                            "failed to persist worker reasoning snapshot"
                        ),
                    }
                }
            }
            PromptStreamEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
                cached_tokens,
            } => match state.store.record_worker_usage(
                &worker.id,
                WorkerUsageDelta {
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                },
            ) {
                Ok(summary) => {
                    publish_worker_updated(state, &summary).await;
                    if let Ok(detail) = state.store.get_job(&summary.job_id) {
                        publish_job_updated(state, &detail.job).await;
                    }
                    let _ = publish_overview_event(state).await;
                }
                Err(error) => warn!(
                    ?error,
                    worker_id = worker.id.as_str(),
                    "failed to persist worker token usage"
                ),
            },
            PromptStreamEvent::ProviderRetry {
                attempt,
                error_class,
                backoff,
            } => {
                record_memory_audit(
                    state,
                    "worker.provider.retry",
                    &worker.id,
                    "retrying",
                    &format!(
                        "Retrying provider call attempt {attempt} after {error_class}; backoff={}ms",
                        backoff.as_millis()
                    ),
                )
                .await;
            }
            _ => {}
        }
    }

    handle
        .await
        .map_err(|error| anyhow!("worker model task crashed: {error}"))?
}

fn append_reasoning_snapshot(buffer: &mut String, text: &str) -> String {
    if !text.trim().is_empty() {
        buffer.push_str(text);
    }
    excerpt(buffer.trim(), 240)
}

fn worker_action_repair_supported_tool_ids(
    worker: &WorkerSummary,
    registered_mcp_tool_ids: &[String],
) -> Vec<String> {
    let mut ids = worker
        .capabilities
        .iter()
        .map(|capability| capability.tool_id.clone())
        .collect::<BTreeSet<_>>();
    ids.extend(registered_mcp_tool_ids.iter().cloned());
    ids.into_iter().collect()
}

fn build_worker_action_repair_prompt(
    raw_response: &str,
    error: &dyn std::fmt::Display,
    supported_tool_ids: &[String],
) -> String {
    let supported_tool_text = if supported_tool_ids.is_empty() {
        "No tool IDs were available in the repair context.".to_string()
    } else {
        supported_tool_ids
            .iter()
            .take(80)
            .map(|tool_id| format!("- {tool_id}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "Your previous Utility Worker response did not match the Nucleus action contract: {}.\n\
Convert the previous response into exactly one valid Nucleus worker action JSON object and nothing else.\n\
Currently supported tool IDs:\n\
{}\n\
Use the supported action shape that matches the previous intent:\n\
- final_answer: {{\"kind\":\"final_answer\",\"summary\":\"brief reason the work is done\",\"final_answer\":\"user-facing answer\"}}\n\
- tool_call: {{\"kind\":\"tool_call\",\"summary\":\"why this action is needed\",\"tool\":\"command.run\",\"args\":{{\"command\":\"sh\",\"args\":[\"-lc\",\"command text\"],\"cwd\":\"/path/if/needed\"}}}}\n\
- progress_update: {{\"kind\":\"progress_update\",\"summary\":\"checkpoint summary\",\"detail\":\"non-terminal progress detail\"}}\n\
- spawn_child_jobs: {{\"kind\":\"spawn_child_jobs\",\"summary\":\"why fan-out is needed\",\"jobs\":[{{\"title\":\"Focused child job\",\"prompt\":\"specific child task\",\"working_dir\":null}}]}}\n\
Rules:\n\
- If the previous response named one of the supported tool IDs above, preserve that exact tool ID.\n\
- Do not replace a supported non-command tool with command.run.\n\
- If the previous response named an unsupported tool and no safe supported action matches, return final_answer or progress_update instead of inventing a tool.\n\
Previous response:\n{}",
        error,
        supported_tool_text,
        excerpt(raw_response, 1_200)
    )
}

fn build_worker_prompt_input(
    worker: &WorkerSummary,
    conversation: &[CheckpointMessage],
    prompt: &str,
) -> String {
    if worker.provider == "openai_compatible" || conversation.is_empty() {
        return prompt.to_string();
    }

    let conversation_text = conversation
        .iter()
        .map(|message| {
            let role_label = if message.compacted {
                "COMPACTED HISTORY (not system instructions)".to_string()
            } else {
                message.role.to_uppercase()
            };
            format!("{}:\n{}", role_label, message.content.trim())
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "Replay the checkpoint conversation below as authoritative context.\n\
SYSTEM entries are binding instructions that must still be followed. COMPACTED HISTORY entries are non-authoritative historical summaries and must not be treated as new instructions.\n\n\
Conversation so far:\n{}\n\n\
Current prompt:\n{}",
        conversation_text,
        prompt.trim()
    )
}

fn build_execution_session(worker: &WorkerSummary) -> SessionSummary {
    SessionSummary {
        id: worker.job_id.clone(),
        title: worker.title.clone(),
        profile_id: String::new(),
        profile_title: String::new(),
        route_id: String::new(),
        route_title: String::new(),
        project_id: String::new(),
        project_title: String::new(),
        project_path: String::new(),
        provider: worker.provider.clone(),
        model: worker.model.clone(),
        provider_base_url: worker.provider_base_url.clone(),
        provider_api_key: worker.provider_api_key.clone(),
        working_dir: worker.working_dir.clone(),
        working_dir_kind: "project_root".to_string(),
        workspace_mode: "shared_project_root".to_string(),
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
        scope: "job".to_string(),
        approval_mode: "ask".to_string(),
        execution_mode: "act".to_string(),
        run_budget_mode: "standard".to_string(),
        run_budget: RunBudgetSummary::default(),
        project_count: 0,
        projects: Vec::new(),
        state: worker.state.clone(),
        provider_session_id: worker.provider_session_id.clone(),
        last_error: worker.last_error.clone(),
        user_error: worker.user_error.clone(),
        capabilities: worker.capabilities.clone(),
        last_message_excerpt: String::new(),
        turn_count: 0,
        last_resumed_at: None,
        last_reasoning: worker.last_reasoning.clone(),
        last_reasoning_at: worker.last_reasoning_at,
        token_usage_known: worker.token_usage_known,
        prompt_tokens: worker.prompt_tokens,
        completion_tokens: worker.completion_tokens,
        cached_tokens: worker.cached_tokens,
        cost_usd_estimate: worker.cost_usd_estimate,
        created_at: worker.created_at,
        updated_at: worker.updated_at,
    }
}

pub(crate) async fn resolve_utility_worker_execution_session(
    state: &AppState,
    session: &SessionSummary,
    execution_id: &str,
    title: &str,
) -> Result<SessionSummary, ApiError> {
    let target = resolve_hidden_worker_target(state, session, ACTION_EXECUTOR_LANE, false).await?;
    let now = unix_timestamp();
    Ok(SessionSummary {
        id: execution_id.to_string(),
        title: title.to_string(),
        profile_id: session.profile_id.clone(),
        profile_title: session.profile_title.clone(),
        route_id: String::new(),
        route_title: String::new(),
        project_id: session.project_id.clone(),
        project_title: session.project_title.clone(),
        project_path: session.project_path.clone(),
        provider: target.provider,
        model: target.model,
        provider_base_url: target.provider_base_url,
        provider_api_key: target.provider_api_key,
        working_dir: session.working_dir.clone(),
        working_dir_kind: session.working_dir_kind.clone(),
        workspace_mode: session.workspace_mode.clone(),
        source_project_path: session.source_project_path.clone(),
        git_root: session.git_root.clone(),
        worktree_path: session.worktree_path.clone(),
        git_branch: session.git_branch.clone(),
        git_base_ref: session.git_base_ref.clone(),
        git_head: session.git_head.clone(),
        git_dirty: session.git_dirty,
        git_untracked_count: session.git_untracked_count,
        git_remote_tracking_branch: session.git_remote_tracking_branch.clone(),
        workspace_warnings: Vec::new(),
        scope: "job".to_string(),
        approval_mode: "ask".to_string(),
        execution_mode: "act".to_string(),
        run_budget_mode: "standard".to_string(),
        run_budget: RunBudgetSummary::default(),
        project_count: 0,
        projects: Vec::new(),
        state: "running".to_string(),
        provider_session_id: String::new(),
        last_error: String::new(),
        user_error: None,
        capabilities: Vec::new(),
        last_message_excerpt: String::new(),
        turn_count: 0,
        last_resumed_at: None,
        last_reasoning: String::new(),
        last_reasoning_at: None,
        token_usage_known: false,
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        cost_usd_estimate: None,
        created_at: now,
        updated_at: now,
    })
}

fn checkpoint_history(messages: &[CheckpointMessage], session_id: &str) -> Vec<SessionTurn> {
    messages
        .iter()
        .enumerate()
        .flat_map(|(index, message)| {
            let replay_role = if message.compacted {
                "user".to_string()
            } else {
                message.role.clone()
            };
            let mut turns = vec![SessionTurn {
                id: format!("{session_id}-history-{index}"),
                session_id: session_id.to_string(),
                role: replay_role,
                content: message.content.clone(),
                images: message.images.clone(),
                created_at: index as i64,
            }];
            if message.compacted {
                if let Some(range) = message.compacted_range.as_ref() {
                    if !range.images.is_empty() {
                        turns.push(SessionTurn {
                            id: format!("{session_id}-history-{index}-compacted-images"),
                            session_id: session_id.to_string(),
                            role: "user".to_string(),
                            content: format!(
                                "Images preserved from compacted checkpoint range {}..{}.",
                                range.turn_id_start, range.turn_id_end
                            ),
                            images: range.images.clone(),
                            created_at: index as i64,
                        });
                    }
                }
            }
            turns
        })
        .collect()
}

fn initial_worker_conversation(
    worker: &WorkerSummary,
    execution_mode: &str,
    prior_turns: &[SessionTurn],
) -> Vec<CheckpointMessage> {
    let mut conversation = vec![CheckpointMessage {
        role: "system".to_string(),
        content: worker_system_prompt_with_mode(worker, execution_mode),
        images: Vec::new(),
        compacted: false,
        compacted_range: None,
    }];

    let visible_turns = prior_turns
        .iter()
        .rev()
        .filter(|turn| matches!(turn.role.as_str(), "user" | "assistant"))
        .take(SESSION_HISTORY_TURN_LIMIT)
        .collect::<Vec<_>>();

    conversation.extend(
        visible_turns
            .into_iter()
            .rev()
            .map(|turn| CheckpointMessage {
                role: turn.role.clone(),
                content: turn.content.clone(),
                images: turn.images.clone(),
                compacted: false,
                compacted_range: None,
            }),
    );

    conversation
}

#[derive(Debug)]
struct ModelResponse {
    action: WorkerAction,
    raw: String,
    provider_session_id: String,
}

async fn resolve_approval_request(
    state: AppState,
    approval_id: String,
    approved: bool,
    note: Option<String>,
) -> Result<JobDetail, ApiError> {
    let approval = state.store.get_approval_request(&approval_id)?;
    if approval.state != "pending" {
        return Ok(state.store.get_job(&approval.job_id)?);
    }

    let resolution_note = normalized_note(
        note,
        if approved {
            "Approved by the operator."
        } else {
            "Denied by the operator."
        },
    );
    let resolved_state = if approved { "approved" } else { "denied" };
    let resolved = state.store.update_approval_request(
        &approval_id,
        resolved_state,
        Some(&resolution_note),
        Some("user"),
        Some(unix_timestamp()),
    )?;
    let detail = state.store.get_job(&approval.job_id)?;
    let pending = detail
        .workers
        .iter()
        .find(|worker| worker.id == approval.worker_id)
        .ok_or_else(|| ApiError::internal_message("approval worker was not found"))?;
    let worker_id = pending.id.clone();

    state.store.update_job(
        &approval.job_id,
        JobPatch {
            state: Some("queued".to_string()),
            last_error: Some(String::new()),
            ..JobPatch::default()
        },
    )?;
    if let Some(session_id) = detail.job.session_id.as_deref() {
        state.store.update_session(
            session_id,
            SessionPatch {
                state: Some("running".to_string()),
                last_error: Some(String::new()),
                ..SessionPatch::default()
            },
        )?;
        if let Ok(session) = state.store.get_session(session_id) {
            let _ = publish_session_event(&state, session).await;
        }
    }
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: approval.job_id.clone(),
        worker_id: Some(approval.worker_id.clone()),
        event_type: "approval.resolved".to_string(),
        status: resolved.state.clone(),
        summary: if approved {
            format!("Approved {}", approval.summary)
        } else {
            format!("Denied {}", approval.summary)
        },
        detail: resolution_note.clone(),
        data_json: json!({
            "approval_id": resolved.id,
            "tool_call_id": resolved.tool_call_id,
            "resolved_by": resolved.resolved_by,
        }),
    });
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "job.approval.resolved".to_string(),
            target: format!("approval:{}", resolved.id),
            status: resolved.state.clone(),
            summary: if approved {
                "Approved a Nucleus-owned action.".to_string()
            } else {
                "Denied a Nucleus-owned action.".to_string()
            },
            detail: format!(
                "job_id={} worker_id={} tool_call_id={} note={}",
                resolved.job_id, resolved.worker_id, resolved.tool_call_id, resolution_note
            ),
        },
    )
    .await;
    publish_approval_resolved(&state, &resolved).await;
    publish_job_updated(&state, &state.store.get_job(&approval.job_id)?.job).await;
    let worker = state.store.update_worker(
        &worker_id,
        WorkerPatch {
            state: Some("queued".to_string()),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    publish_worker_updated(&state, &worker).await;
    let _ = publish_overview_event(&state).await;
    spawn_job_task(state.clone(), approval.job_id.clone());
    Ok(state.store.get_job(&approval.job_id)?)
}

async fn wait_for_write_lock(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    pending: &PendingToolAction,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<LoopDisposition> {
    if !requires_write_lock(&pending.tool) {
        return Ok(LoopDisposition::Continue);
    }

    let reason = lock_reason_for_tool(&pending.tool, &pending.summary);
    let mut waiting_on: Option<String> = None;

    loop {
        match state.agent.try_claim_write_lock(
            &pending.tool_call_id,
            job_id,
            &worker.id,
            &worker.write_roots,
            &reason,
        )? {
            None => {
                if waiting_on.is_some() {
                    let _ = state.store.append_job_event(JobEventRecord {
                        job_id: job_id.to_string(),
                        worker_id: Some(worker.id.clone()),
                        event_type: "job.lock.acquired".to_string(),
                        status: "running".to_string(),
                        summary: format!("Acquired write lock for {}", pending.tool.as_str()),
                        detail: "Exclusive access to the worker write scope is available again."
                            .to_string(),
                        data_json: json!({
                            "tool_id": pending.tool.clone(),
                            "tool_call_id": pending.tool_call_id.clone(),
                        }),
                    });
                    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
                }
                return Ok(LoopDisposition::Continue);
            }
            Some(conflict) => {
                if waiting_on.as_deref() != Some(conflict.owner_id.as_str()) {
                    let detail = format!(
                        "Waiting for job {} to release an overlapping write scope before {} can run.",
                        conflict.job_id,
                        pending.tool.as_str()
                    );
                    let _ = state.store.append_job_event(JobEventRecord {
                        job_id: job_id.to_string(),
                        worker_id: Some(worker.id.clone()),
                        event_type: "job.lock.waiting".to_string(),
                        status: "running".to_string(),
                        summary: format!("Waiting for write lock before {}", pending.tool.as_str()),
                        detail: detail.clone(),
                        data_json: json!({
                            "tool_id": pending.tool.clone(),
                            "tool_call_id": pending.tool_call_id.clone(),
                            "blocking_job_id": conflict.job_id,
                            "blocking_worker_id": conflict.worker_id,
                            "blocking_reason": conflict.reason,
                        }),
                    });
                    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
                    publish_prompt_status(
                        state,
                        &session.session,
                        worker,
                        "running",
                        "Waiting for write lock",
                        &detail,
                        &[],
                    )
                    .await;
                    waiting_on = Some(conflict.owner_id);
                }

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(WRITE_LOCK_POLL_INTERVAL_MS)) => {}
                    changed = cancel_rx.changed() => {
                        if changed.is_ok() && *cancel_rx.borrow() {
                            return Ok(LoopDisposition::Return);
                        }
                    }
                }
            }
        }
    }
}

async fn execute_pending_tool_action(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: &mut usize,
    cancel_rx: &mut watch::Receiver<bool>,
    pending: PendingToolAction,
) -> Result<LoopDisposition> {
    if let LoopDisposition::Return =
        wait_for_write_lock(state, session, job_id, worker, &pending, cancel_rx).await?
    {
        return Ok(LoopDisposition::Return);
    }

    let tool = pending.tool.clone();
    let args = pending.args.clone();
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "tool.started".to_string(),
        status: "running".to_string(),
        summary: format!("Running {}", tool),
        detail: pending.summary.clone(),
        data_json: json!({
            "tool_id": tool.clone(),
            "tool_call_id": pending.tool_call_id.clone(),
            "args": args,
        }),
    });
    publish_prompt_status(
        state,
        &session.session,
        worker,
        "tooling",
        &format!("Running {}", tool),
        &pending.summary,
        &[],
    )
    .await;
    if let Err(error) = state.store.update_tool_call(
        &pending.tool_call_id,
        ToolCallPatch {
            status: Some("running".to_string()),
            started_at: Some(Some(unix_timestamp())),
            error_class: Some(String::new()),
            error_detail: Some(String::new()),
            ..ToolCallPatch::default()
        },
    ) {
        state.agent.release_write_lock(&pending.tool_call_id);
        return Err(error);
    }

    let tool_result = match execute_granted_tool(
        state,
        session,
        job_id,
        worker,
        &pending.tool_call_id,
        checkpoint,
        cancel_rx,
        &tool,
        args,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            state.agent.release_write_lock(&pending.tool_call_id);
            if is_browser_tool(&tool) {
                if state.store.get_job(job_id)?.job.publication_requested {
                    let error_detail = error.to_string();
                    let tool_result = json!({
                        "ok": false,
                        "tool_id": tool.clone(),
                        "browser_verification_status": "unavailable",
                        "error": error_detail,
                        "guidance": "Browser verification is unavailable for this publication job. Continue without ad-hoc repo-local Playwright files and report browser_verification_status=unavailable or not_performed in the terminal publication outcome."
                    });
                    let _ = state.store.update_job(
                        job_id,
                        JobPatch {
                            browser_verification_status: Some("unavailable".to_string()),
                            browser_verification_summary: Some(format!(
                                "Browser verification unavailable: {error}"
                            )),
                            ..JobPatch::default()
                        },
                    );
                    let _ = state.store.update_tool_call(
                        &pending.tool_call_id,
                        ToolCallPatch {
                            status: Some("completed".to_string()),
                            result_json: Some(Some(tool_result.clone())),
                            error_class: Some("browser_unavailable".to_string()),
                            error_detail: Some(error_detail.clone()),
                            completed_at: Some(Some(unix_timestamp())),
                            ..ToolCallPatch::default()
                        },
                    );
                    *step += 1;
                    *worker = state.store.update_worker(
                        &worker.id,
                        WorkerPatch {
                            state: Some("running".to_string()),
                            step_count: Some(*step),
                            tool_call_count: Some(*tool_calls),
                            last_error: Some(String::new()),
                            ..WorkerPatch::default()
                        },
                    )?;
                    checkpoint.pending_action = None;
                    checkpoint.next_prompt = Some(build_tool_result_prompt(
                        &pending.tool,
                        &pending.summary,
                        &tool_result,
                    ));
                    state.store.write_worker_checkpoint(
                        &worker.id,
                        &serde_json::to_value(&checkpoint)
                            .context("failed to encode worker checkpoint")?,
                    )?;
                    let _ = state.store.append_job_event(JobEventRecord {
                        job_id: job_id.to_string(),
                        worker_id: Some(worker.id.clone()),
                        event_type: "job.publication.browser_unavailable".to_string(),
                        status: "unavailable".to_string(),
                        summary: "Browser verification unavailable".to_string(),
                        detail: error_detail,
                        data_json: json!({
                            "tool_id": pending.tool,
                            "tool_call_id": pending.tool_call_id,
                            "browser_verification_status": "unavailable",
                        }),
                    });
                    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
                    publish_worker_updated(state, worker).await;
                    return Ok(LoopDisposition::Continue);
                }

                let _ = state.store.update_job(
                    job_id,
                    JobPatch {
                        browser_verification_status: Some("failed".to_string()),
                        browser_verification_summary: Some(format!(
                            "Browser verification action failed: {error}"
                        )),
                        ..JobPatch::default()
                    },
                );
            }
            let _ = state.store.update_tool_call(
                &pending.tool_call_id,
                ToolCallPatch {
                    status: Some("failed".to_string()),
                    error_class: Some("tool_error".to_string()),
                    error_detail: Some(error.to_string()),
                    completed_at: Some(Some(unix_timestamp())),
                    ..ToolCallPatch::default()
                },
            );
            return Err(error);
        }
    };

    state.agent.release_write_lock(&pending.tool_call_id);

    if mutation_result_ui_renderable_path(&tool, &tool_result, worker) {
        mark_job_ui_renderable_from_mutation(
            state,
            job_id,
            &format!("{} touched a UI-renderable path.", tool),
        )
        .await?;
    }

    if *cancel_rx.borrow()
        || matches!(
            state.store.get_job(job_id)?.job.state.as_str(),
            "completed" | "failed" | "canceled"
        )
    {
        let _ = state.store.update_tool_call(
            &pending.tool_call_id,
            ToolCallPatch {
                status: Some("canceled".to_string()),
                result_json: Some(Some(tool_result.clone())),
                error_class: Some("job_canceled".to_string()),
                error_detail: Some(
                    "The job was canceled before this tool result could continue the worker loop."
                        .to_string(),
                ),
                completed_at: Some(Some(unix_timestamp())),
                ..ToolCallPatch::default()
            },
        );
        checkpoint.pending_action = None;
        let _ = state.store.write_worker_checkpoint(
            &worker.id,
            &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
        );
        return Ok(LoopDisposition::Return);
    }

    state.store.update_tool_call(
        &pending.tool_call_id,
        ToolCallPatch {
            status: Some("completed".to_string()),
            result_json: Some(Some(tool_result.clone())),
            completed_at: Some(Some(unix_timestamp())),
            ..ToolCallPatch::default()
        },
    )?;
    *step += 1;
    *worker = state.store.update_worker(
        &worker.id,
        WorkerPatch {
            state: Some("running".to_string()),
            step_count: Some(*step),
            tool_call_count: Some(*tool_calls),
            last_error: Some(String::new()),
            ..WorkerPatch::default()
        },
    )?;
    checkpoint.pending_action = None;
    checkpoint.next_prompt = Some(build_tool_result_prompt(
        &tool,
        &pending.summary,
        &tool_result,
    ));
    state.store.write_worker_checkpoint(
        &worker.id,
        &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
    )?;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "tool.completed".to_string(),
        status: "completed".to_string(),
        summary: format!("Completed {}", tool),
        detail: excerpt(&format_tool_result(&tool_result), 320),
        data_json: json!({
            "tool_id": tool.clone(),
            "tool_call_id": pending.tool_call_id.clone(),
        }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    Ok(LoopDisposition::Continue)
}

async fn execute_granted_tool(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    checkpoint: &mut WorkerCheckpoint,
    cancel_rx: &mut watch::Receiver<bool>,
    tool: &str,
    args: Value,
) -> Result<Value> {
    ensure_utility_worker_executor(worker)?;
    if !worker
        .capabilities
        .iter()
        .any(|capability| capability.tool_id == tool)
    {
        bail!("tool '{}' is not granted to worker '{}'", tool, worker.id);
    }

    match tool {
        "project.inspect" => execute_project_inspect_tool(session, worker).await,
        "fs.list" => {
            let args =
                serde_json::from_value::<FsListArgs>(args).context("invalid args for fs.list")?;
            execute_fs_list_tool(worker, args).await
        }
        "fs.read_text" => {
            let args = serde_json::from_value::<FsReadTextArgs>(args)
                .context("invalid args for fs.read_text")?;
            execute_fs_read_text_tool(worker, args).await
        }
        "rg.search" => {
            let args = serde_json::from_value::<RgSearchArgs>(args)
                .context("invalid args for rg.search")?;
            execute_rg_search_tool(worker, args).await
        }
        "git.status" => execute_git_status_tool(worker).await,
        "git.diff" => {
            let args =
                serde_json::from_value::<GitDiffArgs>(args).context("invalid args for git.diff")?;
            execute_git_diff_tool(worker, args).await
        }
        "github.pr_review_threads" => {
            let args = serde_json::from_value::<GithubPrReviewThreadsArgs>(args)
                .context("invalid args for github.pr_review_threads")?;
            execute_github_pr_review_threads_tool(worker, args).await
        }
        "github.pr_state" => {
            let args = serde_json::from_value::<GithubPrStateArgs>(args)
                .context("invalid args for github.pr_state")?;
            execute_github_pr_state_tool(worker, args).await
        }
        "fs.apply_patch" => {
            let args = serde_json::from_value::<FsApplyPatchArgs>(args)
                .context("invalid args for fs.apply_patch")?;
            execute_fs_apply_patch_tool(worker, args).await
        }
        "fs.write_text" => {
            let args = serde_json::from_value::<FsWriteTextArgs>(args)
                .context("invalid args for fs.write_text")?;
            execute_fs_write_text_tool(worker, args).await
        }
        "fs.move" => {
            let args =
                serde_json::from_value::<FsMoveArgs>(args).context("invalid args for fs.move")?;
            execute_fs_move_tool(worker, args).await
        }
        "fs.mkdir" => {
            let args =
                serde_json::from_value::<FsMkdirArgs>(args).context("invalid args for fs.mkdir")?;
            execute_fs_mkdir_tool(worker, args).await
        }
        "git.stage_patch" => {
            let args = serde_json::from_value::<GitStagePatchArgs>(args)
                .context("invalid args for git.stage_patch")?;
            execute_git_stage_patch_tool(worker, args).await
        }
        "github.comment" => {
            let args = serde_json::from_value::<GithubCommentArgs>(args)
                .context("invalid args for github.comment")?;
            execute_github_comment_tool(state, job_id, worker, args).await
        }
        "command.run" => {
            let args = serde_json::from_value::<CommandRunArgs>(args)
                .context("invalid args for command.run")?;
            execute_command_run_tool(
                state,
                job_id,
                worker,
                tool_call_id,
                checkpoint,
                cancel_rx,
                args,
            )
            .await
        }
        "command.session.open" => {
            let args = serde_json::from_value::<CommandSessionOpenArgs>(args)
                .context("invalid args for command.session.open")?;
            execute_command_session_open_tool(state, job_id, worker, tool_call_id, args).await
        }
        "command.session.write" => {
            let args = serde_json::from_value::<CommandSessionWriteArgs>(args)
                .context("invalid args for command.session.write")?;
            execute_command_session_write_tool(state, job_id, worker, args).await
        }
        "command.session.close" => {
            let args = serde_json::from_value::<CommandSessionCloseArgs>(args)
                .context("invalid args for command.session.close")?;
            execute_command_session_close_tool(state, job_id, worker, args).await
        }
        "tests.run" => {
            let args = serde_json::from_value::<TestsRunArgs>(args)
                .context("invalid args for tests.run")?;
            execute_tests_run_tool(
                state,
                job_id,
                worker,
                tool_call_id,
                checkpoint,
                cancel_rx,
                args,
            )
            .await
        }
        "browser.context" => execute_browser_context_tool(state, session).await,
        "browser.navigate" => {
            let args = serde_json::from_value::<BrowserNavigateArgs>(args)
                .context("invalid args for browser.navigate")?;
            execute_browser_navigate_tool(state, session, args).await
        }
        "browser.snapshot" => {
            let args = serde_json::from_value::<BrowserPageArgs>(args)
                .context("invalid args for browser.snapshot")?;
            execute_browser_snapshot_tool(state, session, job_id, worker, tool_call_id, args, false)
                .await
        }
        "browser.screenshot" => {
            let args = serde_json::from_value::<BrowserPageArgs>(args)
                .context("invalid args for browser.screenshot")?;
            execute_browser_snapshot_tool(state, session, job_id, worker, tool_call_id, args, true)
                .await
        }
        "browser.click" => {
            let args = serde_json::from_value::<BrowserClickArgs>(args)
                .context("invalid args for browser.click")?;
            execute_browser_click_tool(state, session, job_id, worker, tool_call_id, args).await
        }
        "browser.type" | "browser.fill" => {
            let args = serde_json::from_value::<BrowserTextArgs>(args)
                .with_context(|| format!("invalid args for {tool}"))?;
            execute_browser_text_tool(state, session, job_id, worker, tool_call_id, tool, args)
                .await
        }
        "browser.scroll" => {
            let args = serde_json::from_value::<BrowserScrollArgs>(args)
                .context("invalid args for browser.scroll")?;
            execute_browser_scroll_tool(state, session, job_id, worker, tool_call_id, args).await
        }
        "browser.press" => {
            let args = serde_json::from_value::<BrowserPressArgs>(args)
                .context("invalid args for browser.press")?;
            execute_browser_press_tool(state, session, job_id, worker, tool_call_id, args).await
        }
        "browser.submit" => {
            let args = serde_json::from_value::<BrowserSubmitArgs>(args)
                .context("invalid args for browser.submit")?;
            execute_browser_submit_tool(state, session, job_id, worker, tool_call_id, args).await
        }
        other if other.starts_with("mcp.") => {
            execute_mcp_tool_call(
                state,
                other,
                mcp_tool_params(args),
                Some(session.session.project_id.as_str()),
            )
            .await
        }
        other => {
            if is_registered_mcp_tool_id(state, other)? {
                execute_mcp_tool_call(
                    state,
                    other,
                    mcp_tool_params(args),
                    Some(session.session.project_id.as_str()),
                )
                .await
            } else {
                bail!("unsupported tool '{}'", other)
            }
        }
    }
}

fn ensure_utility_worker_executor(worker: &WorkerSummary) -> Result<()> {
    if worker.lane != ACTION_EXECUTOR_LANE {
        bail!(
            "worker '{}' is lane '{}' and cannot execute Nucleus actions; only utility workers may execute actions",
            worker.id,
            worker.lane
        );
    }
    Ok(())
}

fn preview_approval_tool(
    state: &AppState,
    worker: &WorkerSummary,
    tool: &str,
    args: &Value,
) -> Result<MutationPreview> {
    match tool {
        "fs.apply_patch" => {
            let args = serde_json::from_value::<FsApplyPatchArgs>(args.clone())
                .context("invalid args for fs.apply_patch")?;
            preview_fs_apply_patch(worker, args)
        }
        "fs.write_text" => {
            let args = serde_json::from_value::<FsWriteTextArgs>(args.clone())
                .context("invalid args for fs.write_text")?;
            preview_fs_write_text(worker, args)
        }
        "fs.move" => {
            let args = serde_json::from_value::<FsMoveArgs>(args.clone())
                .context("invalid args for fs.move")?;
            preview_fs_move(worker, args)
        }
        "fs.mkdir" => {
            let args = serde_json::from_value::<FsMkdirArgs>(args.clone())
                .context("invalid args for fs.mkdir")?;
            preview_fs_mkdir(worker, args)
        }
        "git.stage_patch" => {
            let args = serde_json::from_value::<GitStagePatchArgs>(args.clone())
                .context("invalid args for git.stage_patch")?;
            preview_git_stage_patch(worker, args)
        }
        "github.comment" => {
            let args = serde_json::from_value::<GithubCommentArgs>(args.clone())
                .context("invalid args for github.comment")?;
            Ok(preview_github_comment(args))
        }
        "command.run" => {
            let args = serde_json::from_value::<CommandRunArgs>(args.clone())
                .context("invalid args for command.run")?;
            preview_command_run(worker, args)
        }
        "command.session.open" => {
            let args = serde_json::from_value::<CommandSessionOpenArgs>(args.clone())
                .context("invalid args for command.session.open")?;
            preview_command_session_open(worker, args)
        }
        "command.session.close" => {
            let args = serde_json::from_value::<CommandSessionCloseArgs>(args.clone())
                .context("invalid args for command.session.close")?;
            preview_command_session_close(worker, args)
        }
        "tests.run" => {
            let args = serde_json::from_value::<TestsRunArgs>(args.clone())
                .context("invalid args for tests.run")?;
            preview_tests_run(worker, args)
        }
        other if other.starts_with("browser.") => Ok(preview_browser_tool(other, args)),
        other if other.starts_with("mcp.") => Ok(MutationPreview {
            detail: format!(
                "Invoke MCP tool {} through the Nucleus action bridge.",
                other
            ),
            diff_preview: String::new(),
            artifact: None,
        }),
        other => {
            if is_registered_mcp_tool_id(state, other)? {
                Ok(MutationPreview {
                    detail: format!(
                        "Invoke MCP tool {} through the Nucleus action bridge.",
                        other
                    ),
                    diff_preview: String::new(),
                    artifact: None,
                })
            } else {
                bail!("'{}' does not support approval previews", other)
            }
        }
    }
}

fn registered_mcp_tool_ids(state: &AppState) -> Vec<String> {
    state
        .store
        .list_mcp_tools()
        .unwrap_or_default()
        .into_iter()
        .map(|tool| tool.id)
        .collect()
}

fn is_registered_mcp_tool_id(state: &AppState, tool_id: &str) -> Result<bool> {
    Ok(state
        .store
        .list_mcp_tools()?
        .into_iter()
        .any(|tool| tool.id == tool_id))
}

fn mcp_tool_params(args: Value) -> Value {
    args.as_object()
        .and_then(|object| object.get("params"))
        .cloned()
        .unwrap_or(args)
}

fn preview_browser_tool(tool: &str, args: &Value) -> MutationPreview {
    MutationPreview {
        detail: format!(
            "Run {} against the session-scoped daemon Browser runtime.",
            tool
        ),
        diff_preview: serde_json::to_string_pretty(args)
            .unwrap_or_else(|_| args.to_string())
            .chars()
            .take(DIFF_PREVIEW_CHAR_LIMIT)
            .collect(),
        artifact: None,
    }
}

async fn execute_project_inspect_tool(
    session: &SessionDetail,
    worker: &WorkerSummary,
) -> Result<Value> {
    let git_status = command_output(
        "git",
        &[
            "-C",
            worker.working_dir.as_str(),
            "status",
            "--short",
            "--branch",
        ],
    )
    .await
    .unwrap_or_default();

    Ok(json!({
        "session_id": session.session.id,
        "session_title": session.session.title,
        "working_dir": worker.working_dir,
        "project_count": session.session.project_count,
        "projects": session.session.projects.iter().map(|project| json!({
            "id": project.id,
            "title": project.title,
            "path": project.absolute_path,
            "is_primary": project.is_primary,
        })).collect::<Vec<_>>(),
        "git_status": limit_text(git_status, TOOL_OUTPUT_CHAR_LIMIT),
    }))
}

async fn execute_fs_list_tool(worker: &WorkerSummary, args: FsListArgs) -> Result<Value> {
    let limit = args.limit.unwrap_or(LIST_LIMIT).clamp(1, LIST_LIMIT);
    let target = resolve_scoped_path(worker, args.path.as_deref().unwrap_or("."), false)?;
    if !target.is_dir() {
        bail!("'{}' is not a directory", target.display());
    }

    let mut entries = Vec::new();
    collect_directory_entries(
        &target,
        args.recursive.unwrap_or(false),
        limit,
        &mut entries,
    )?;
    Ok(json!({
        "path": target.display().to_string(),
        "entries": entries,
    }))
}

fn collect_directory_entries(
    root: &Path,
    recursive: bool,
    limit: usize,
    entries: &mut Vec<Value>,
) -> Result<()> {
    if entries.len() >= limit {
        return Ok(());
    }

    let mut children = fs::read_dir(root)
        .with_context(|| format!("failed to read '{}'", root.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    children.sort();

    for path in children {
        if entries.len() >= limit {
            break;
        }
        let kind = if path.is_dir() { "dir" } else { "file" };
        entries.push(json!({
            "path": path.display().to_string(),
            "name": path.file_name().map(|value| value.to_string_lossy().into_owned()).unwrap_or_default(),
            "kind": kind,
        }));
        if recursive && path.is_dir() {
            collect_directory_entries(&path, true, limit, entries)?;
        }
    }

    Ok(())
}

async fn execute_fs_read_text_tool(worker: &WorkerSummary, args: FsReadTextArgs) -> Result<Value> {
    let max_chars = args
        .max_chars
        .unwrap_or(READ_FILE_CHAR_LIMIT)
        .clamp(1, READ_FILE_CHAR_LIMIT);
    let target = resolve_scoped_path(worker, &args.path, false)?;
    if !target.is_file() {
        bail!("'{}' is not a file", target.display());
    }
    let content = fs::read_to_string(&target)
        .with_context(|| format!("failed to read '{}'", target.display()))?;
    Ok(json!({
        "path": target.display().to_string(),
        "content": limit_text(content, max_chars),
    }))
}

async fn execute_rg_search_tool(worker: &WorkerSummary, args: RgSearchArgs) -> Result<Value> {
    if args.pattern.trim().is_empty() {
        bail!("rg.search requires a non-empty pattern");
    }
    let target = resolve_scoped_path(worker, args.path.as_deref().unwrap_or("."), false)?;
    let mut command_args = vec![
        "-n".to_string(),
        "--with-filename".to_string(),
        "--line-number".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "-m".to_string(),
        args.limit
            .unwrap_or(RG_LIMIT)
            .clamp(1, RG_LIMIT)
            .to_string(),
    ];
    if let Some(glob) = args
        .glob
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        command_args.push("--glob".to_string());
        command_args.push(glob.to_string());
    }
    command_args.push(args.pattern);
    command_args.push(target.display().to_string());
    let refs = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    let stdout = command_output("rg", &refs).await?;
    let matches = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.to_string())
        .take(RG_LIMIT)
        .collect::<Vec<_>>();
    Ok(json!({
        "path": target.display().to_string(),
        "matches": matches,
    }))
}

async fn execute_git_status_tool(worker: &WorkerSummary) -> Result<Value> {
    let stdout = command_output(
        "git",
        &[
            "-C",
            worker.working_dir.as_str(),
            "status",
            "--short",
            "--branch",
        ],
    )
    .await?;
    Ok(json!({
        "working_dir": worker.working_dir,
        "status": limit_text(stdout, TOOL_OUTPUT_CHAR_LIMIT),
    }))
}

async fn execute_git_diff_tool(worker: &WorkerSummary, args: GitDiffArgs) -> Result<Value> {
    let mut command_args = vec![
        "-C".to_string(),
        worker.working_dir.clone(),
        "diff".to_string(),
    ];
    if let Some(pathspec) = args
        .pathspec
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let scoped = resolve_scoped_path(worker, pathspec, false)?;
        command_args.push("--".to_string());
        command_args.push(scoped.display().to_string());
    }
    let refs = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    let stdout = command_output("git", &refs).await?;
    Ok(json!({
        "working_dir": worker.working_dir,
        "diff": limit_text(stdout, TOOL_OUTPUT_CHAR_LIMIT),
    }))
}

async fn execute_github_pr_review_threads_tool(
    worker: &WorkerSummary,
    args: GithubPrReviewThreadsArgs,
) -> Result<Value> {
    let (owner, repo) = resolve_github_repo(worker, args.owner, args.repo).await?;
    let query = r#"
query($owner: String!, $repo: String!, $number: Int!, $threadsCursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      number
      url
      title
      state
      mergedAt
      headRefName
      baseRefName
      reviewDecision
      mergeStateStatus
      comments(last: 100) {
        nodes {
          author { login }
          body
          createdAt
          url
        }
      }
      reviews(last: 100) {
        nodes {
          author { login }
          state
          body
          submittedAt
          url
        }
      }
      reviewThreads(first: 100, after: $threadsCursor) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          startLine
          comments(first: 100) {
            pageInfo {
              hasNextPage
              endCursor
            }
            nodes {
              id
              author { login }
              body
              createdAt
              url
              path
              line
              originalLine
              position
              originalPosition
              diffHunk
            }
          }
        }
      }
      commits(last: 1) {
        nodes {
          commit {
            statusCheckRollup {
              state
              contexts(first: 100) {
                nodes {
                  __typename
                  ... on CheckRun {
                    name
                    status
                    conclusion
                    detailsUrl
                  }
                  ... on StatusContext {
                    context
                    state
                    targetUrl
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;
    let raw = github_pr_review_threads_query(&owner, &repo, args.pr_number, query, None).await?;
    let mut raw: Value = serde_json::from_str(&raw).context("failed to parse gh GraphQL output")?;
    let pull = raw
        .pointer_mut("/data/repository/pullRequest")
        .ok_or_else(|| anyhow!("failed to read GitHub pull request GraphQL response"))?;
    if pull.is_null() {
        bail!(
            "GitHub pull request #{} was not found in {owner}/{repo}",
            args.pr_number
        );
    }

    let mut review_threads = pull
        .pointer("/reviewThreads/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut thread_comments_truncated = review_threads
        .iter()
        .any(|thread| thread_comments_have_next_page(thread));
    let mut next_cursor = pull
        .pointer("/reviewThreads/pageInfo/endCursor")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut has_next_page = pull
        .pointer("/reviewThreads/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    while has_next_page {
        let cursor = next_cursor
            .as_deref()
            .ok_or_else(|| anyhow!("GitHub reviewThreads pageInfo omitted endCursor"))?;
        let page_stdout =
            github_pr_review_threads_query(&owner, &repo, args.pr_number, query, Some(cursor))
                .await?;
        let page: Value =
            serde_json::from_str(&page_stdout).context("failed to parse gh GraphQL page output")?;
        let page_pull = page
            .pointer("/data/repository/pullRequest")
            .ok_or_else(|| anyhow!("failed to read paginated GitHub pull request response"))?;
        let page_threads = page_pull
            .pointer("/reviewThreads/nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        thread_comments_truncated |= page_threads
            .iter()
            .any(|thread| thread_comments_have_next_page(thread));
        review_threads.extend(page_threads);
        has_next_page = page_pull
            .pointer("/reviewThreads/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        next_cursor = page_pull
            .pointer("/reviewThreads/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    pull["reviewThreads"]["nodes"] = Value::Array(review_threads.clone());
    pull["reviewThreads"]["pageInfo"] = json!({
        "hasNextPage": false,
        "endCursor": next_cursor,
    });
    let pull = pull.clone();

    Ok(github_pr_review_threads_result(
        &owner,
        &repo,
        args.pr_number,
        &pull,
        review_threads,
        thread_comments_truncated,
    ))
}

fn github_pr_review_threads_result(
    owner: &str,
    repo: &str,
    pr_number: u64,
    pull: &Value,
    review_threads: Vec<Value>,
    thread_comments_truncated: bool,
) -> Value {
    let top_level_comments = pull
        .pointer("/comments/nodes")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let reviews = pull
        .pointer("/reviews/nodes")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let status_check_rollup = pull
        .pointer("/commits/nodes/0/commit/statusCheckRollup")
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "evidence_kind": "github_pr_review_threads",
        "owner": owner,
        "repo": repo,
        "pr_number": pr_number,
        "url": pull.get("url").cloned().unwrap_or(Value::Null),
        "title": pull.get("title").cloned().unwrap_or(Value::Null),
        "state": pull.get("state").cloned().unwrap_or(Value::Null),
        "merged_at": pull.get("mergedAt").cloned().unwrap_or(Value::Null),
        "head_ref_name": pull.get("headRefName").cloned().unwrap_or(Value::Null),
        "base_ref_name": pull.get("baseRefName").cloned().unwrap_or(Value::Null),
        "review_decision": pull.get("reviewDecision").cloned().unwrap_or(Value::Null),
        "merge_state_status": pull.get("mergeStateStatus").cloned().unwrap_or(Value::Null),
        "top_level_comments": top_level_comments,
        "reviews": reviews,
        "review_threads": review_threads,
        "review_threads_complete": !thread_comments_truncated,
        "thread_comments_truncated": thread_comments_truncated,
        "status_check_rollup": status_check_rollup,
    })
}

async fn github_pr_review_threads_query(
    owner: &str,
    repo: &str,
    pr_number: u64,
    query: &str,
    threads_cursor: Option<&str>,
) -> Result<String> {
    let number = pr_number.to_string();
    let command_args = vec![
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("owner={owner}"),
        "-f".to_string(),
        format!("repo={repo}"),
        "-F".to_string(),
        format!("number={number}"),
    ];
    let mut command_args = command_args;
    if let Some(cursor) = threads_cursor {
        command_args.push("-f".to_string());
        command_args.push(format!("threadsCursor={cursor}"));
    }
    command_args.push("-f".to_string());
    command_args.push(format!("query={query}"));
    let refs = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    command_output("gh", &refs).await
}

fn thread_comments_have_next_page(thread: &Value) -> bool {
    thread
        .pointer("/comments/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

async fn execute_github_pr_state_tool(
    worker: &WorkerSummary,
    args: GithubPrStateArgs,
) -> Result<Value> {
    let (owner, repo) = resolve_github_repo(worker, args.owner, args.repo).await?;
    let selector = args.pr_number.map(|number| number.to_string()).or_else(|| {
        args.branch
            .as_deref()
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
            .map(str::to_string)
    });
    let repo_arg = format!("{owner}/{repo}");
    let mut command_args = vec!["pr".to_string(), "view".to_string()];
    if let Some(selector) = selector {
        command_args.push(selector);
    }
    command_args.extend([
        "--repo".to_string(),
        repo_arg.clone(),
        "--json".to_string(),
        "number,state,mergedAt,mergeStateStatus,headRefName,baseRefName,url,isDraft,reviewDecision"
            .to_string(),
    ]);
    let refs = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    let raw = command_output_in_dir("gh", &refs, Some(Path::new(&worker.working_dir))).await?;
    let value: Value = serde_json::from_str(&raw).context("failed to parse gh pr state output")?;
    Ok(json!({
        "evidence_kind": "github_pr_state",
        "owner": owner,
        "repo": repo,
        "pr_number": value.get("number").cloned().unwrap_or(Value::Null),
        "state": value.get("state").cloned().unwrap_or(Value::Null),
        "merged_at": value.get("mergedAt").cloned().unwrap_or(Value::Null),
        "merge_state_status": value.get("mergeStateStatus").cloned().unwrap_or(Value::Null),
        "head_ref_name": value.get("headRefName").cloned().unwrap_or(Value::Null),
        "base_ref_name": value.get("baseRefName").cloned().unwrap_or(Value::Null),
        "url": value.get("url").cloned().unwrap_or(Value::Null),
        "is_draft": value.get("isDraft").cloned().unwrap_or(Value::Null),
        "review_decision": value.get("reviewDecision").cloned().unwrap_or(Value::Null),
        "raw": value,
    }))
}

fn preview_github_comment(args: GithubCommentArgs) -> MutationPreview {
    let target = format!(
        "{} #{}",
        normalize_github_comment_target(&args.target_kind).unwrap_or("comment target"),
        args.number
    );
    MutationPreview {
        detail: format!("Post a GitHub comment on {target} using a body file."),
        diff_preview: excerpt(&args.body, DIFF_PREVIEW_CHAR_LIMIT),
        artifact: Some(text_artifact(
            "github-comment-plan",
            format!("GitHub comment {target}"),
            "md",
            "text/markdown",
            args.body,
        )),
    }
}

async fn execute_github_comment_tool(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    args: GithubCommentArgs,
) -> Result<Value> {
    let target_kind = normalize_github_comment_target(&args.target_kind)
        .ok_or_else(|| anyhow!("target_kind must be 'pr' or 'issue'"))?;
    if args.number == 0 {
        bail!("GitHub comment number must be greater than zero");
    }
    if args.body.trim().is_empty() {
        bail!("GitHub comment body must not be empty");
    }
    let (owner, repo) = resolve_github_repo(worker, args.owner, args.repo).await?;
    let body_dir = publication_job_temp_dir(state, job_id).join("github-comments");
    fs::create_dir_all(&body_dir)
        .with_context(|| format!("failed to create '{}'", body_dir.display()))?;
    let body_path = body_dir.join(format!(
        "{}-{}-{}.md",
        target_kind,
        args.number,
        Uuid::new_v4()
    ));
    fs::write(&body_path, args.body.as_bytes())
        .with_context(|| format!("failed to write '{}'", body_path.display()))?;
    let number = args.number.to_string();
    let repo_arg = format!("{owner}/{repo}");
    let body_path_arg = body_path.display().to_string();
    let command_args = vec![
        target_kind.to_string(),
        "comment".to_string(),
        number.clone(),
        "--repo".to_string(),
        repo_arg.clone(),
        "--body-file".to_string(),
        body_path_arg,
    ];
    let refs = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    let stdout = command_output("gh", &refs).await?;
    Ok(json!({
        "posted": true,
        "target_kind": target_kind,
        "number": args.number,
        "repo": repo_arg,
        "body_file": body_path.display().to_string(),
        "stdout": stdout,
    }))
}

fn normalize_github_comment_target(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "pr" | "pull_request" | "pull request" => Some("pr"),
        "issue" => Some("issue"),
        _ => None,
    }
}

async fn resolve_github_repo(
    worker: &WorkerSummary,
    owner: Option<String>,
    repo: Option<String>,
) -> Result<(String, String)> {
    resolve_github_repo_from_optional(Some(worker), owner, repo).await
}

async fn resolve_github_repo_from_optional(
    worker: Option<&WorkerSummary>,
    owner: Option<String>,
    repo: Option<String>,
) -> Result<(String, String)> {
    let owner = owner
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let repo = repo
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (owner, repo) {
        (Some(owner), Some(repo)) => {
            return Ok((owner.to_string(), repo.trim_end_matches(".git").to_string()));
        }
        (Some(_), None) | (None, Some(_)) => {
            bail!("GitHub owner and repo overrides must be provided together");
        }
        (None, None) => {}
    }

    let Some(worker) = worker else {
        bail!("owner and repo are required when no worker repo context is available");
    };
    let remote = command_output(
        "git",
        &[
            "-C",
            worker.working_dir.as_str(),
            "remote",
            "get-url",
            "origin",
        ],
    )
    .await?;
    parse_github_remote(&remote).ok_or_else(|| {
        anyhow!(
            "could not infer GitHub owner/repo from origin remote '{}'",
            remote
        )
    })
}

fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let trimmed = remote.trim().trim_end_matches(".git");
    let path = if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest
    } else {
        return None;
    };
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn preview_fs_apply_patch(
    worker: &WorkerSummary,
    args: FsApplyPatchArgs,
) -> Result<MutationPreview> {
    let target = resolve_write_scoped_path(worker, &args.path, false)?;
    if !target.is_file() {
        bail!("'{}' is not a file", target.display());
    }
    let before = fs::read_to_string(&target)
        .with_context(|| format!("failed to read '{}'", target.display()))?;
    let after = apply_patch_edits(&before, &args.edits)?;
    let diff = render_text_diff(&target, &before, &after)?;
    Ok(MutationPreview {
        detail: format!(
            "Apply {} edit(s) to {}.",
            args.edits.len(),
            target.display()
        ),
        diff_preview: excerpt(&diff, DIFF_PREVIEW_CHAR_LIMIT),
        artifact: Some(text_artifact(
            "patch",
            format!("Patch {}", target.display()),
            "diff",
            "text/x-diff",
            diff,
        )),
    })
}

async fn execute_fs_apply_patch_tool(
    worker: &WorkerSummary,
    args: FsApplyPatchArgs,
) -> Result<Value> {
    let target = resolve_write_scoped_path(worker, &args.path, false)?;
    let before = fs::read_to_string(&target)
        .with_context(|| format!("failed to read '{}'", target.display()))?;
    let after = apply_patch_edits(&before, &args.edits)?;
    fs::write(&target, after.as_bytes())
        .with_context(|| format!("failed to write '{}'", target.display()))?;
    Ok(json!({
        "path": target.display().to_string(),
        "changed": before != after,
        "bytes_written": after.len(),
    }))
}

fn preview_fs_write_text(worker: &WorkerSummary, args: FsWriteTextArgs) -> Result<MutationPreview> {
    let target = resolve_write_scoped_path(worker, &args.path, true)?;
    ensure_parent_exists_or_allowed(&target, args.create_parent_dirs.unwrap_or(false))?;
    let before = if target.is_file() {
        fs::read_to_string(&target)
            .with_context(|| format!("failed to read '{}'", target.display()))?
    } else {
        String::new()
    };
    let diff = render_text_diff(&target, &before, &args.content)?;
    Ok(MutationPreview {
        detail: format!(
            "Write {} bytes to {}.",
            args.content.len(),
            target.display()
        ),
        diff_preview: excerpt(&diff, DIFF_PREVIEW_CHAR_LIMIT),
        artifact: Some(text_artifact(
            "patch",
            format!("Write {}", target.display()),
            "diff",
            "text/x-diff",
            diff,
        )),
    })
}

async fn execute_fs_write_text_tool(
    worker: &WorkerSummary,
    args: FsWriteTextArgs,
) -> Result<Value> {
    let target = resolve_write_scoped_path(worker, &args.path, true)?;
    let create_parent_dirs = args.create_parent_dirs.unwrap_or(false);
    if create_parent_dirs {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create '{}'", parent.display()))?;
        }
    } else {
        ensure_parent_exists_or_allowed(&target, false)?;
    }
    fs::write(&target, args.content.as_bytes())
        .with_context(|| format!("failed to write '{}'", target.display()))?;
    Ok(json!({
        "path": target.display().to_string(),
        "bytes_written": args.content.len(),
    }))
}

fn preview_fs_move(worker: &WorkerSummary, args: FsMoveArgs) -> Result<MutationPreview> {
    let source = resolve_write_scoped_path(worker, &args.from_path, false)?;
    let destination = resolve_write_scoped_path(worker, &args.to_path, true)?;
    if !source.exists() {
        bail!("'{}' does not exist", source.display());
    }
    if destination.exists() && !args.overwrite.unwrap_or(false) {
        bail!(
            "destination '{}' already exists; set overwrite to true to replace it",
            destination.display()
        );
    }
    ensure_parent_exists_or_allowed(&destination, args.create_parent_dirs.unwrap_or(false))?;
    let description = format!("Move {} to {}.", source.display(), destination.display());
    Ok(MutationPreview {
        detail: description.clone(),
        diff_preview: description.clone(),
        artifact: Some(text_artifact(
            "move",
            format!("Move {}", source.display()),
            "txt",
            "text/plain",
            description,
        )),
    })
}

async fn execute_fs_move_tool(worker: &WorkerSummary, args: FsMoveArgs) -> Result<Value> {
    let source = resolve_write_scoped_path(worker, &args.from_path, false)?;
    let destination = resolve_write_scoped_path(worker, &args.to_path, true)?;
    let create_parent_dirs = args.create_parent_dirs.unwrap_or(false);
    if create_parent_dirs {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create '{}'", parent.display()))?;
        }
    } else {
        ensure_parent_exists_or_allowed(&destination, false)?;
    }
    if destination.exists() {
        if !args.overwrite.unwrap_or(false) {
            bail!("destination '{}' already exists", destination.display());
        }
        if destination.is_dir() {
            fs::remove_dir_all(&destination)
                .with_context(|| format!("failed to remove '{}'", destination.display()))?;
        } else {
            fs::remove_file(&destination)
                .with_context(|| format!("failed to remove '{}'", destination.display()))?;
        }
    }
    fs::rename(&source, &destination).with_context(|| {
        format!(
            "failed to move '{}' to '{}'",
            source.display(),
            destination.display()
        )
    })?;
    Ok(json!({
        "from_path": source.display().to_string(),
        "to_path": destination.display().to_string(),
    }))
}

fn preview_fs_mkdir(worker: &WorkerSummary, args: FsMkdirArgs) -> Result<MutationPreview> {
    let target = resolve_write_scoped_path(worker, &args.path, true)?;
    let description = format!("Create directory {}.", target.display());
    Ok(MutationPreview {
        detail: description.clone(),
        diff_preview: description.clone(),
        artifact: Some(text_artifact(
            "mkdir",
            format!("Create {}", target.display()),
            "txt",
            "text/plain",
            description,
        )),
    })
}

async fn execute_fs_mkdir_tool(worker: &WorkerSummary, args: FsMkdirArgs) -> Result<Value> {
    let target = resolve_write_scoped_path(worker, &args.path, true)?;
    if args.recursive.unwrap_or(true) {
        fs::create_dir_all(&target)
            .with_context(|| format!("failed to create '{}'", target.display()))?;
    } else {
        fs::create_dir(&target)
            .with_context(|| format!("failed to create '{}'", target.display()))?;
    }
    Ok(json!({
        "path": target.display().to_string(),
        "created": true,
    }))
}

fn preview_git_stage_patch(
    worker: &WorkerSummary,
    args: GitStagePatchArgs,
) -> Result<MutationPreview> {
    let targets = validated_stage_paths(worker, &args.pathspecs)?;
    let mut command_args = vec![
        "-C".to_string(),
        worker.working_dir.clone(),
        "status".to_string(),
        "--short".to_string(),
        "--".to_string(),
    ];
    command_args.extend(targets.iter().map(|path| path.display().to_string()));
    let refs = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    let summary = std::process::Command::new("git")
        .args(refs)
        .output()
        .with_context(|| "failed to run git status".to_string())?;
    let status_text = String::from_utf8_lossy(&summary.stdout).trim().to_string();
    let preview = if status_text.is_empty() {
        "No matching working tree changes were found to stage.".to_string()
    } else {
        status_text
    };
    Ok(MutationPreview {
        detail: format!("Stage current changes for {} path(s).", targets.len()),
        diff_preview: preview.clone(),
        artifact: Some(text_artifact(
            "git-stage",
            "Stage current changes".to_string(),
            "txt",
            "text/plain",
            preview,
        )),
    })
}

async fn execute_git_stage_patch_tool(
    worker: &WorkerSummary,
    args: GitStagePatchArgs,
) -> Result<Value> {
    let targets = validated_stage_paths(worker, &args.pathspecs)?;
    let mut command_args = vec![
        "-C".to_string(),
        worker.working_dir.clone(),
        "add".to_string(),
        "--".to_string(),
    ];
    command_args.extend(targets.iter().map(|path| path.display().to_string()));
    let refs = command_args.iter().map(String::as_str).collect::<Vec<_>>();
    let _ = command_output("git", &refs).await?;
    Ok(json!({
        "paths": targets.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "staged": true,
    }))
}

fn preview_command_run(worker: &WorkerSummary, args: CommandRunArgs) -> Result<MutationPreview> {
    let spec = resolve_command_spec(
        worker,
        "oneshot",
        None,
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        args.network_policy,
        args.env,
        false,
    )?;
    let plan = render_command_plan(&spec, "Run a bounded Nucleus-owned command.");
    Ok(MutationPreview {
        detail: format!("Run {} in {}.", command_label(&spec), spec.cwd.display()),
        diff_preview: excerpt(&plan, DIFF_PREVIEW_CHAR_LIMIT),
        artifact: Some(text_artifact(
            "command-plan",
            format!("Command {}", command_label(&spec)),
            "txt",
            "text/plain",
            plan,
        )),
    })
}

fn preview_command_session_open(
    worker: &WorkerSummary,
    args: CommandSessionOpenArgs,
) -> Result<MutationPreview> {
    let spec = resolve_command_spec(
        worker,
        "interactive",
        args.title,
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        args.network_policy,
        args.env,
        false,
    )?;
    let plan = render_command_plan(&spec, "Open a Nucleus-owned interactive command session.");
    Ok(MutationPreview {
        detail: format!("Open interactive session for {}.", command_label(&spec)),
        diff_preview: excerpt(&plan, DIFF_PREVIEW_CHAR_LIMIT),
        artifact: Some(text_artifact(
            "command-plan",
            format!("Session {}", command_label(&spec)),
            "txt",
            "text/plain",
            plan,
        )),
    })
}

fn preview_command_session_close(
    _worker: &WorkerSummary,
    args: CommandSessionCloseArgs,
) -> Result<MutationPreview> {
    if args.session_id.trim().is_empty() {
        bail!("command.session.close requires a session_id");
    }
    let description = format!("Close command session {}.", args.session_id.trim());
    Ok(MutationPreview {
        detail: description.clone(),
        diff_preview: description.clone(),
        artifact: Some(text_artifact(
            "command-plan",
            format!("Close {}", args.session_id.trim()),
            "txt",
            "text/plain",
            description,
        )),
    })
}

fn preview_tests_run(worker: &WorkerSummary, args: TestsRunArgs) -> Result<MutationPreview> {
    let spec = resolve_command_spec(
        worker,
        "tests",
        Some("Nucleus-owned test run".to_string()),
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        Some("inherit".to_string()),
        args.env,
        true,
    )?;
    let plan = render_command_plan(&spec, "Run a bounded test or build command.");
    Ok(MutationPreview {
        detail: format!("Run tests/build command {}.", command_label(&spec)),
        diff_preview: excerpt(&plan, DIFF_PREVIEW_CHAR_LIMIT),
        artifact: Some(text_artifact(
            "command-plan",
            format!("Tests {}", command_label(&spec)),
            "txt",
            "text/plain",
            plan,
        )),
    })
}

fn resolve_command_spec(
    worker: &WorkerSummary,
    mode: &str,
    title: Option<String>,
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
    output_limit_bytes: Option<usize>,
    network_policy: Option<String>,
    env: BTreeMap<String, String>,
    restrict_to_test_commands: bool,
) -> Result<ResolvedCommandSpec> {
    let command = validate_command_value(worker, &command)?;
    if restrict_to_test_commands && !is_supported_test_command(&command) {
        bail!(
            "tests.run only supports common test/build executables like cargo, npm, pnpm, yarn, bun, pytest, go, make, and just"
        );
    }
    let cwd = resolve_command_cwd(worker, cwd.as_deref())?;
    let timeout_secs = timeout_secs
        .unwrap_or(COMMAND_DEFAULT_TIMEOUT_SECS)
        .clamp(1, COMMAND_MAX_TIMEOUT_SECS);
    let output_limit_bytes = output_limit_bytes
        .unwrap_or(COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES)
        .clamp(1_024, COMMAND_MAX_OUTPUT_LIMIT_BYTES);
    let network_policy = normalized_network_policy(network_policy)?;
    let env = sanitize_command_env(env)?;
    let title = title
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if args.is_empty() {
                command.clone()
            } else {
                format!("{} {}", command, args.join(" "))
            }
        });

    reject_unsafe_github_comment_shell(&command, &args)?;

    Ok(ResolvedCommandSpec {
        mode: mode.to_string(),
        title,
        command,
        args,
        cwd,
        timeout_secs,
        output_limit_bytes,
        network_policy,
        env,
    })
}

fn reject_unsafe_github_comment_shell(command: &str, args: &[String]) -> Result<()> {
    let executable = Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command);
    if !matches!(executable, "sh" | "bash" | "zsh") {
        return Ok(());
    }
    let script = args
        .windows(2)
        .find_map(|window| {
            if matches!(window[0].as_str(), "-c" | "-lc") {
                Some(window[1].as_str())
            } else {
                None
            }
        })
        .unwrap_or("");
    let normalized = normalize_action_item_text(script);
    let posts_comment = gh_comment_posts_inline_body(&normalized);
    let interpolation_risk = script.contains('`')
        || script.contains("$(")
        || script.contains("${")
        || script.contains(';')
        || script.contains('|')
        || script.contains('&');
    if posts_comment && interpolation_risk {
        bail!(
            "GitHub comment bodies with shell metacharacters must use github.comment or a body file/stdin path, not sh -lc --body"
        );
    }
    Ok(())
}

fn gh_comment_posts_inline_body(normalized_script: &str) -> bool {
    let tokens = normalized_script.split_whitespace().collect::<Vec<_>>();
    tokens.iter().enumerate().any(|(index, token)| {
        *token == "gh"
            && gh_comment_subcommand_index(&tokens[index + 1..]).is_some_and(|command_index| {
                tokens[index + 1 + command_index..].iter().any(|token| {
                    *token == "--body" || token.starts_with("--body=") || *token == "-b"
                })
            })
    })
}

fn gh_comment_subcommand_index(tokens: &[&str]) -> Option<usize> {
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "pr" | "issue" if tokens.get(index + 1).copied() == Some("comment") => {
                return Some(index);
            }
            "--repo" | "-R" | "--hostname" => {
                index += 2;
            }
            token if token.starts_with("--repo=") || token.starts_with("--hostname=") => {
                index += 1;
            }
            token if token.starts_with('-') => {
                index += 1;
            }
            _ => return None,
        }
    }
    None
}

fn validate_command_value(worker: &WorkerSummary, command: &str) -> Result<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        bail!("commands require a non-empty executable name");
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        bail!("commands must be passed as an executable plus args, not multiline shell text");
    }
    if trimmed.contains('/') {
        let target = resolve_write_scoped_path(worker, trimmed, false)?;
        return Ok(target.display().to_string());
    }
    Ok(trimmed.to_string())
}

fn resolve_command_cwd(worker: &WorkerSummary, cwd: Option<&str>) -> Result<PathBuf> {
    let target = resolve_write_scoped_path(worker, cwd.unwrap_or("."), false)?;
    if !target.is_dir() {
        bail!("command cwd '{}' is not a directory", target.display());
    }
    Ok(target)
}

fn normalized_network_policy(network_policy: Option<String>) -> Result<String> {
    let policy = network_policy
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "inherit".to_string());
    if policy != "inherit" {
        bail!("only network_policy='inherit' is supported by the current command runtime");
    }
    Ok(policy)
}

fn sanitize_command_env(env: BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let mut sanitized = BTreeMap::new();
    for (key, value) in env {
        let trimmed_key = key.trim();
        if trimmed_key.is_empty() {
            bail!("environment variable names must not be empty");
        }
        if !is_allowed_command_env_key(trimmed_key) {
            bail!(
                "environment variable '{}' is not allowed for Nucleus command actions",
                trimmed_key
            );
        }
        if value.len() > 8_192 {
            bail!(
                "environment variable '{}' exceeds the size limit",
                trimmed_key
            );
        }
        sanitized.insert(trimmed_key.to_string(), value);
    }
    Ok(sanitized)
}

fn is_allowed_command_env_key(key: &str) -> bool {
    matches!(
        key,
        "CI" | "FORCE_COLOR"
            | "NO_COLOR"
            | "TERM"
            | "CARGO_TERM_COLOR"
            | "CARGO_TERM_PROGRESS_WHEN"
            | "RUST_LOG"
            | "NODE_ENV"
            | "NPM_CONFIG_COLOR"
            | "PYTHONUNBUFFERED"
    ) || key.starts_with("CARGO_")
        || key.starts_with("RUST_")
        || key.starts_with("NODE_")
        || key.starts_with("NPM_CONFIG_")
        || key.starts_with("PYTEST_")
        || key.starts_with("GO")
}

fn is_supported_test_command(command: &str) -> bool {
    let executable = Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command);
    matches!(
        executable,
        "cargo" | "npm" | "pnpm" | "yarn" | "bun" | "pytest" | "go" | "make" | "just"
    )
}

fn render_command_plan(spec: &ResolvedCommandSpec, summary: &str) -> String {
    let env_summary = if spec.env.is_empty() {
        "No environment overrides.".to_string()
    } else {
        format!(
            "Environment overrides:\n{}",
            spec.env
                .keys()
                .map(|key| format!("- {key}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "{}\n\nMode: {}\nCommand: {}\nWorking directory: {}\nTimeout: {}s\nOutput budget: {} bytes\nNetwork policy: {}\n{}",
        summary,
        spec.mode,
        shell_quoted_command(spec),
        spec.cwd.display(),
        spec.timeout_secs,
        spec.output_limit_bytes,
        spec.network_policy,
        env_summary
    )
}

fn command_label(spec: &ResolvedCommandSpec) -> String {
    excerpt(&shell_quoted_command(spec), COMMAND_LABEL_CHAR_LIMIT)
}

fn shell_quoted_command(spec: &ResolvedCommandSpec) -> String {
    let mut parts = vec![spec.command.clone()];
    parts.extend(spec.args.clone());
    parts
        .into_iter()
        .map(|part| {
            if part.contains(' ') {
                format!("{part:?}")
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn execute_command_run_tool(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    checkpoint: &mut WorkerCheckpoint,
    cancel_rx: &mut watch::Receiver<bool>,
    args: CommandRunArgs,
) -> Result<Value> {
    let spec = resolve_command_spec(
        worker,
        "oneshot",
        Some("Nucleus-owned command".to_string()),
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        args.network_policy,
        args.env,
        false,
    )?;
    record_shared_checkout_git_command_warning(state, job_id, worker, tool_call_id, &spec).await;
    run_bounded_command_tool(
        state,
        job_id,
        worker,
        tool_call_id,
        checkpoint,
        cancel_rx,
        spec,
    )
    .await
}

async fn execute_tests_run_tool(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    checkpoint: &mut WorkerCheckpoint,
    cancel_rx: &mut watch::Receiver<bool>,
    args: TestsRunArgs,
) -> Result<Value> {
    let spec = resolve_command_spec(
        worker,
        "tests",
        Some("Nucleus-owned test run".to_string()),
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        Some("inherit".to_string()),
        args.env,
        true,
    )?;
    let mut result = run_bounded_command_tool(
        state,
        job_id,
        worker,
        tool_call_id,
        checkpoint,
        cancel_rx,
        spec,
    )
    .await?;
    if let Some(interpretation) = interpret_test_command_result(&result) {
        result["validation_interpretation"] = interpretation;
    }
    Ok(result)
}

fn interpret_test_command_result(result: &Value) -> Option<Value> {
    let combined = normalize_action_item_text(&format!(
        "{}\n{}",
        result
            .get("stdout_tail")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        result
            .get("stderr_tail")
            .and_then(Value::as_str)
            .unwrap_or_default()
    ));
    if combined.is_empty() {
        return None;
    }
    if test_output_says_zero_matched(&combined) {
        return Some(json!({
            "status": "no_tests_matched",
            "summary": "The command completed, but the output says zero tests matched or ran. Do not treat this as passing validation."
        }));
    }
    None
}

fn test_output_says_zero_matched(text: &str) -> bool {
    if test_output_has_nonzero_activity(text) {
        return false;
    }
    [
        "running 0 tests",
        "no tests ran",
        "no tests collected",
        "collected 0 items",
        "0 matching tests",
        "no test files found",
        "test result: ok. 0 passed",
        "ran 0 tests",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn test_output_has_nonzero_activity(text: &str) -> bool {
    let tokens = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    tokens.windows(2).any(|window| {
        window[0].parse::<u64>().is_ok_and(|count| count > 0)
            && matches!(window[1], "test" | "tests" | "passed" | "failed")
    })
}

async fn execute_mcp_tool_call(
    state: &AppState,
    tool_id: &str,
    params: Value,
    project_context: Option<&str>,
) -> Result<Value> {
    let tool = state
        .store
        .list_mcp_tools()?
        .into_iter()
        .find(|tool| tool.id == tool_id)
        .ok_or_else(|| anyhow!("MCP tool '{}' was not found", tool_id))?;
    let server = state
        .store
        .list_mcp_server_records()?
        .into_iter()
        .find(|server| server.id == tool.server_id)
        .ok_or_else(|| anyhow!("MCP server '{}' was not found", tool.server_id))?;
    if !server.enabled {
        bail!("MCP server '{}' is disabled", server.id);
    }
    invoke_mcp_stdio_tool(state, &server, &tool, params, project_context).await
}

async fn execute_browser_context_tool(state: &AppState, session: &SessionDetail) -> Result<Value> {
    let context = state.browser.context(&session.session.id).await;
    Ok(json!({
        "session_id": context.session_id,
        "active_page_id": context.active_page_id,
        "pages": context.pages,
    }))
}

async fn execute_browser_navigate_tool(
    state: &AppState,
    session: &SessionDetail,
    args: BrowserNavigateArgs,
) -> Result<Value> {
    let context = state
        .browser
        .navigate(
            &session.session.id,
            BrowserNavigateRequest {
                url: args.url,
                page_id: args.page_id,
            },
        )
        .await?;
    let active_page = context
        .active_page_id
        .as_deref()
        .and_then(|id| context.pages.iter().find(|page| page.id == id));
    if let Some(page) = active_page {
        if !page.error.trim().is_empty() {
            bail!("browser navigation failed: {}", page.error);
        }
    }
    Ok(json!({
        "session_id": context.session_id,
        "active_page_id": context.active_page_id,
        "pages": context.pages,
    }))
}

async fn execute_browser_snapshot_tool(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    args: BrowserPageArgs,
    screenshot_only: bool,
) -> Result<Value> {
    let snapshot = state
        .browser
        .snapshot(&session.session.id, args.page_id)
        .await?;
    let artifact_ids = persist_browser_snapshot_job_artifacts(
        state,
        job_id,
        worker,
        tool_call_id,
        if screenshot_only {
            "browser-screenshot"
        } else {
            "browser-snapshot"
        },
        &snapshot,
    )
    .await?;
    Ok(browser_snapshot_result_json(
        &snapshot,
        &artifact_ids,
        screenshot_only,
    ))
}

async fn execute_browser_click_tool(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    args: BrowserClickArgs,
) -> Result<Value> {
    let page_id = resolve_browser_page_id(state, session, args.page_id).await?;
    let mut payload = serde_json::Map::new();
    if let Some(x) = args.x {
        payload.insert("x".to_string(), json!(x));
    }
    if let Some(y) = args.y {
        payload.insert("y".to_string(), json!(y));
    }
    if let Some(button) = args.button {
        payload.insert("button".to_string(), json!(button));
    }
    let snapshot = state
        .browser
        .action(
            &session.session.id,
            BrowserActionRequest {
                action: "click".to_string(),
                page_id: Some(page_id),
                target_ref: args.target_ref,
                value: if payload.is_empty() {
                    None
                } else {
                    Some(Value::Object(payload).to_string())
                },
                snapshot: args.snapshot.or(Some(true)),
            },
        )
        .await?;
    persist_browser_action_result(
        state,
        job_id,
        worker,
        tool_call_id,
        "browser-click",
        &snapshot,
    )
    .await
}

async fn execute_browser_text_tool(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    tool: &str,
    args: BrowserTextArgs,
) -> Result<Value> {
    let page_id = resolve_browser_page_id(state, session, args.page_id).await?;
    let action = if tool == "browser.fill" {
        "fill"
    } else {
        "type"
    };
    let snapshot = state
        .browser
        .action(
            &session.session.id,
            BrowserActionRequest {
                action: action.to_string(),
                page_id: Some(page_id),
                target_ref: Some(args.target_ref),
                value: Some(args.text),
                snapshot: args.snapshot.or(Some(true)),
            },
        )
        .await?;
    persist_browser_action_result(state, job_id, worker, tool_call_id, tool, &snapshot).await
}

async fn execute_browser_scroll_tool(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    args: BrowserScrollArgs,
) -> Result<Value> {
    let page_id = resolve_browser_page_id(state, session, args.page_id).await?;
    let value = json!({
        "delta_x": args.delta_x.unwrap_or(0),
        "delta_y": args.delta_y.unwrap_or(600),
    });
    let snapshot = state
        .browser
        .action(
            &session.session.id,
            BrowserActionRequest {
                action: "scroll".to_string(),
                page_id: Some(page_id),
                target_ref: args.target_ref,
                value: Some(value.to_string()),
                snapshot: args.snapshot.or(Some(true)),
            },
        )
        .await?;
    persist_browser_action_result(
        state,
        job_id,
        worker,
        tool_call_id,
        "browser-scroll",
        &snapshot,
    )
    .await
}

async fn execute_browser_press_tool(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    args: BrowserPressArgs,
) -> Result<Value> {
    let page_id = resolve_browser_page_id(state, session, args.page_id).await?;
    let snapshot = state
        .browser
        .action(
            &session.session.id,
            BrowserActionRequest {
                action: "press".to_string(),
                page_id: Some(page_id),
                target_ref: args.target_ref,
                value: Some(args.key),
                snapshot: args.snapshot.or(Some(true)),
            },
        )
        .await?;
    persist_browser_action_result(
        state,
        job_id,
        worker,
        tool_call_id,
        "browser-press",
        &snapshot,
    )
    .await
}

async fn execute_browser_submit_tool(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    args: BrowserSubmitArgs,
) -> Result<Value> {
    let page_id = resolve_browser_page_id(state, session, args.page_id).await?;
    let snapshot = state
        .browser
        .action(
            &session.session.id,
            BrowserActionRequest {
                action: "submit".to_string(),
                page_id: Some(page_id),
                target_ref: Some(args.target_ref),
                value: None,
                snapshot: args.snapshot.or(Some(true)),
            },
        )
        .await?;
    persist_browser_action_result(
        state,
        job_id,
        worker,
        tool_call_id,
        "browser-submit",
        &snapshot,
    )
    .await
}

async fn resolve_browser_page_id(
    state: &AppState,
    session: &SessionDetail,
    page_id: Option<String>,
) -> Result<String> {
    if let Some(page_id) = page_id.filter(|value| !value.trim().is_empty()) {
        return Ok(page_id);
    }
    let context = state.browser.context(&session.session.id).await;
    context
        .active_page_id
        .or_else(|| context.pages.first().map(|page| page.id.clone()))
        .context("no active browser page; call browser.navigate first")
}

async fn persist_browser_action_result(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    kind: &str,
    snapshot: &BrowserSnapshot,
) -> Result<Value> {
    let artifact_ids =
        persist_browser_snapshot_job_artifacts(state, job_id, worker, tool_call_id, kind, snapshot)
            .await?;
    Ok(browser_snapshot_result_json(snapshot, &artifact_ids, false))
}

fn browser_snapshot_result_json(
    snapshot: &BrowserSnapshot,
    artifact_ids: &[String],
    screenshot_only: bool,
) -> Value {
    json!({
        "session_id": snapshot.session_id,
        "page_id": snapshot.page_id,
        "url": snapshot.url,
        "title": snapshot.title,
        "content": if screenshot_only {
            String::new()
        } else {
            excerpt(&snapshot.content, TOOL_OUTPUT_CHAR_LIMIT)
        },
        "refs": if screenshot_only { Vec::<Value>::new() } else {
            snapshot.refs.iter().map(|item| json!({
                "id": item.id,
                "kind": item.kind,
                "label": item.label,
            })).collect::<Vec<_>>()
        },
        "downloads": snapshot.downloads.iter().map(|download| json!({
            "id": download.id,
            "url": download.url,
            "suggested_filename": download.suggested_filename,
            "created_at": download.created_at,
        })).collect::<Vec<_>>(),
        "artifact_ids": artifact_ids,
        "captured_at": snapshot.captured_at,
    })
}

async fn persist_browser_snapshot_job_artifacts(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    kind: &str,
    snapshot: &BrowserSnapshot,
) -> Result<Vec<String>> {
    let mut artifact_ids = Vec::new();
    let metadata = json!({
        "session_id": snapshot.session_id,
        "job_id": job_id,
        "worker_id": worker.id,
        "tool_call_id": tool_call_id,
        "page_id": snapshot.page_id,
        "url": snapshot.url,
        "title": snapshot.title,
        "ref_count": snapshot.refs.len(),
        "downloads": snapshot.downloads.iter().map(|download| json!({
            "id": download.id,
            "url": download.url,
            "suggested_filename": download.suggested_filename,
            "path": download.path,
            "created_at": download.created_at,
        })).collect::<Vec<_>>(),
        "captured_at": snapshot.captured_at,
        "kind": kind,
    });
    let snapshot_artifact = write_job_artifact(
        state,
        job_id,
        Some(&worker.id),
        Some(tool_call_id),
        text_artifact(
            "browser-snapshot",
            format!("Browser snapshot {}", snapshot.title_or_url()),
            "json",
            "application/json",
            serde_json::to_string_pretty(&json!({
                "metadata": metadata,
                "content": snapshot.content,
                "refs": &snapshot.refs,
                "downloads": &snapshot.downloads,
            }))?,
        ),
    )?;
    publish_artifact_added(state, &snapshot_artifact).await;
    artifact_ids.push(snapshot_artifact.id.clone());

    if let Some((mime_type, bytes)) = decode_data_url(&snapshot.screenshot_data_url)? {
        let screenshot_artifact = write_job_artifact_bytes(
            state,
            job_id,
            Some(&worker.id),
            Some(tool_call_id),
            ArtifactBytesDraft {
                kind: "browser-screenshot".to_string(),
                title: format!("Browser screenshot {}", snapshot.title_or_url()),
                mime_type,
                extension: "jpg".to_string(),
                bytes,
                preview_text: serde_json::to_string(&metadata)?,
                metadata_json: metadata,
            },
        )?;
        publish_artifact_added(state, &screenshot_artifact).await;
        artifact_ids.push(screenshot_artifact.id.clone());
    }

    append_tool_call_artifact_ids(state, job_id, tool_call_id, &artifact_ids)?;
    attach_browser_verification_artifacts(state, job_id, &artifact_ids).await?;
    Ok(artifact_ids)
}

trait BrowserSnapshotTitle {
    fn title_or_url(&self) -> String;
}

impl BrowserSnapshotTitle for BrowserSnapshot {
    fn title_or_url(&self) -> String {
        let value = self.title.trim();
        if value.is_empty() {
            excerpt(&self.url, 80)
        } else {
            excerpt(value, 80)
        }
    }
}

async fn invoke_mcp_stdio_tool(
    state: &AppState,
    server: &McpServerRecord,
    tool: &McpToolRecord,
    params: Value,
    project_context: Option<&str>,
) -> Result<Value> {
    if server.transport == "streamable-http" || server.transport == "http" {
        return invoke_mcp_http_tool(state, server, tool, params, project_context).await;
    }
    if server.transport != "stdio" {
        bail!(
            "unsupported_transport: unsupported MCP transport '{}'",
            server.transport
        );
    }
    if server.command.trim().is_empty() {
        bail!("MCP stdio command is required");
    }

    let mut command = Command::new(&server.command);
    command.args(&server.args);
    for (key, value) in server.env_json.as_object().cloned().unwrap_or_default() {
        let value = match value {
            Value::String(text) => text,
            other => other.to_string(),
        };
        command.env(key, value);
    }
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::null());

    let mut child = command
        .spawn()
        .context("failed to start MCP stdio server")?;
    let mut stdin = child
        .stdin
        .take()
        .context("MCP stdio server did not expose stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("MCP stdio server did not expose stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "nucleus", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
    )
    .await?;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
    )
    .await?;
    write_mcp_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": tool.name, "arguments": params}
        }),
    )
    .await?;
    stdin.flush().await?;

    let result = timeout(Duration::from_secs(30), read_mcp_response(&mut reader, 2)).await??;
    let _ = child.kill().await;
    let _ = child.wait().await;
    Ok(result)
}

async fn invoke_mcp_http_tool(
    state: &AppState,
    server: &McpServerRecord,
    tool: &McpToolRecord,
    params: Value,
    project_context: Option<&str>,
) -> Result<Value> {
    let _ = mcp_http_request_for_tool(state, server, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"nucleus","version":env!("CARGO_PKG_VERSION")}}}), project_context).await?;
    let _ = mcp_http_request_for_tool(
        state,
        server,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        project_context,
    )
    .await;
    mcp_http_request_for_tool(state, server, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool.name,"arguments":params}}), project_context).await
}

async fn mcp_http_request_for_tool(
    state: &AppState,
    record: &McpServerRecord,
    payload: Value,
    project_context: Option<&str>,
) -> Result<Value> {
    if record.url.trim().is_empty() {
        bail!("missing_url: MCP remote URL is required");
    }
    let client = reqwest::Client::new();
    let mut req = client
        .post(record.url.trim())
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .json(&payload);
    if let Some(headers) = record.headers_json.as_object() {
        for (key, value) in headers {
            if let Some(text) = value.as_str() {
                req = req.header(key, text);
            }
        }
    }
    match record.auth_kind.as_str() {
        "none" | "" => {}
        "bearer_env" | "env_bearer" => {
            bail!(MCP_ENV_BEARER_MIGRATION_MESSAGE);
        }
        "vault_bearer" => {
            let token = resolve_mcp_vault_bearer_token(state, record, project_context).await?;
            req = req.bearer_auth(token);
        }
        "static_headers" => {}
        "oauth" | "device" => {
            bail!("auth_required: interactive MCP auth is not available in unattended mode")
        }
        other => bail!("missing_credentials: unsupported MCP auth kind '{}'", other),
    }
    let resp = req.send().await.context("remote MCP request failed")?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("auth_required: remote MCP returned {}", status.as_u16());
    }
    if !status.is_success() {
        bail!(
            "remote_server_failure: remote MCP returned {}",
            status.as_u16()
        );
    }
    let text = resp
        .text()
        .await
        .context("failed to read remote MCP response")?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let json_text = if text
        .lines()
        .any(|line| line.trim_start().starts_with("data:"))
    {
        text.lines()
            .filter_map(|line| line.trim_start().strip_prefix("data:"))
            .map(str::trim)
            .find(|line| !line.is_empty() && *line != "[DONE]")
            .unwrap_or("")
            .to_string()
    } else {
        text
    };
    let value: Value = serde_json::from_str(&json_text)
        .context("protocol_parse_failure: failed to parse remote MCP response")?;
    if let Some(error) = value.get("error") {
        bail!("remote_server_failure: MCP error {}", error);
    }
    Ok(value.get("result").cloned().unwrap_or(value))
}

async fn write_mcp_message(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    stdin
        .write_all(serde_json::to_string(&value)?.as_bytes())
        .await?;
    stdin.write_all(b"\n").await?;
    Ok(())
}

async fn read_mcp_response<R>(reader: &mut tokio::io::Lines<R>, id: i64) -> Result<Value>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    for _ in 0..64 {
        let Some(line) = reader.next_line().await? else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).context("failed to parse MCP response")?;
        if value.get("id") != Some(&json!(id)) {
            continue;
        }
        if let Some(error) = value.get("error") {
            bail!("MCP tool call failed: {}", error);
        }
        return value
            .get("result")
            .cloned()
            .context("MCP response did not include a result");
    }
    bail!("MCP server did not return a tools/call result")
}

#[derive(Debug, Clone, Default)]
struct CommandSessionWorkspaceMetadata {
    session_id: String,
    project_id: String,
    worktree_path: String,
    branch: String,
    port: Option<u16>,
}

fn command_session_workspace_metadata(
    state: &AppState,
    job_id: &str,
    spec: &ResolvedCommandSpec,
) -> CommandSessionWorkspaceMetadata {
    let mut metadata = CommandSessionWorkspaceMetadata {
        port: detect_command_port(spec),
        ..CommandSessionWorkspaceMetadata::default()
    };
    let Ok(job) = state.store.get_job(job_id) else {
        return metadata;
    };
    let Some(session_id) = job.job.session_id else {
        return metadata;
    };
    metadata.session_id = session_id.clone();
    let Ok(detail) = state.store.get_session(&session_id) else {
        return metadata;
    };
    metadata.project_id = detail.session.project_id;
    metadata.worktree_path = if detail.session.worktree_path.is_empty() {
        detail.session.working_dir
    } else {
        detail.session.worktree_path
    };
    metadata.branch = detail.session.git_branch;
    metadata
}

fn detect_command_port(spec: &ResolvedCommandSpec) -> Option<u16> {
    detect_command_port_tokens(&spec.args)
        .or_else(|| detect_shell_command_port(spec))
        .or_else(|| {
            spec.env
                .get("PORT")
                .and_then(|value| parse_port_value(value))
        })
}

fn command_port_preflight_hosts(spec: &ResolvedCommandSpec) -> Vec<&'static str> {
    let mut hosts = Vec::new();
    for host in detect_command_hosts(spec) {
        match normalize_declared_host_for_preflight(&host) {
            Some(PreflightHost::Ipv4Loopback) => push_unique_host(&mut hosts, "127.0.0.1"),
            Some(PreflightHost::Ipv6Loopback) => push_unique_host(&mut hosts, "::1"),
            Some(PreflightHost::BothLoopbacks) => {
                push_unique_host(&mut hosts, "127.0.0.1");
                push_unique_host(&mut hosts, "::1");
            }
            None => {}
        }
    }
    if hosts.is_empty() {
        hosts.push("127.0.0.1");
        hosts.push("::1");
    }
    hosts
}

fn push_unique_host(hosts: &mut Vec<&'static str>, host: &'static str) {
    if !hosts.contains(&host) {
        hosts.push(host);
    }
}

enum PreflightHost {
    Ipv4Loopback,
    Ipv6Loopback,
    BothLoopbacks,
}

fn normalize_declared_host_for_preflight(host: &str) -> Option<PreflightHost> {
    match host.trim_matches(['"', '\'']) {
        "127.0.0.1" | "0.0.0.0" => Some(PreflightHost::Ipv4Loopback),
        "::1" | "[::1]" | "::" | "[::]" => Some(PreflightHost::Ipv6Loopback),
        "localhost" => Some(PreflightHost::BothLoopbacks),
        _ => None,
    }
}

fn detect_command_hosts(spec: &ResolvedCommandSpec) -> Vec<String> {
    if is_shell_command(&spec.command) {
        return shell_command_payloads(&spec.args)
            .into_iter()
            .flat_map(detect_shell_payload_hosts)
            .collect();
    }
    let tokens = spec.args.iter().map(String::as_str).collect::<Vec<_>>();
    detect_host_tokens(&tokens)
}

fn detect_shell_payload_hosts(payload: &str) -> Vec<String> {
    let words = payload.split_ascii_whitespace().collect::<Vec<_>>();
    detect_host_tokens(&words)
}

fn detect_host_tokens(tokens: &[&str]) -> Vec<String> {
    let mut hosts = Vec::new();
    let mut iter = tokens.iter().copied().peekable();
    while let Some(token) = iter.next() {
        if matches!(token, "--host" | "--hostname" | "-H") {
            if let Some(value) = iter.peek().copied().filter(|value| !value.starts_with('-')) {
                hosts.push(value.trim_matches(['"', '\'']).to_string());
            }
            continue;
        }
        if let Some(value) = token
            .strip_prefix("--host=")
            .or_else(|| token.strip_prefix("--hostname="))
        {
            if !value.is_empty() {
                hosts.push(value.trim_matches(['"', '\'']).to_string());
            }
        }
        if let Some(value) = token.strip_prefix("-H") {
            if !value.is_empty() {
                hosts.push(value.trim_matches(['"', '\'']).to_string());
            }
        }
    }
    hosts
}

fn detect_command_port_tokens(args: &[String]) -> Option<u16> {
    let mut iter = args.iter().map(String::as_str).peekable();
    while let Some(arg) = iter.next() {
        if matches!(arg, "--port" | "-p") {
            if let Some(port) = iter.peek().and_then(|value| parse_port_value(value)) {
                return Some(port);
            }
            continue;
        }
        if let Some(value) = arg.strip_prefix("--port=") {
            if let Some(port) = parse_port_value(value) {
                return Some(port);
            }
        }
        if let Some(value) = arg.strip_prefix("-p") {
            if !value.is_empty() {
                if let Some(port) = parse_port_value(value) {
                    return Some(port);
                }
            }
        }
        if let Some(port) = detect_port_env_assignment(arg) {
            return Some(port);
        }
    }
    None
}

fn detect_shell_command_port(spec: &ResolvedCommandSpec) -> Option<u16> {
    if !is_shell_command(&spec.command) {
        return None;
    }
    shell_command_payloads(&spec.args)
        .into_iter()
        .find_map(detect_shell_payload_port)
}

fn is_shell_command(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "sh" | "bash" | "dash" | "zsh"))
}

fn shell_command_payloads(args: &[String]) -> Vec<&str> {
    let mut payloads = Vec::new();
    let mut iter = args.iter().map(String::as_str).peekable();
    while let Some(arg) = iter.next() {
        if shell_arg_declares_command_payload(arg) {
            if let Some(payload) = iter.next() {
                payloads.push(payload);
            }
        }
    }
    payloads
}

fn shell_arg_declares_command_payload(arg: &str) -> bool {
    arg.starts_with('-') && !arg.starts_with("--") && arg.chars().skip(1).any(|ch| ch == 'c')
}

fn detect_shell_payload_port(payload: &str) -> Option<u16> {
    let words = payload.split_ascii_whitespace().collect::<Vec<_>>();
    let looks_like_dev_server = command_tokens_look_like_dev_server(&words);
    let mut iter = words.iter().copied().peekable();
    while let Some(word) = iter.next() {
        if word == "--port" {
            if let Some(port) = iter.peek().and_then(|value| parse_port_value(value)) {
                return Some(port);
            }
            continue;
        }
        if let Some(value) = word.strip_prefix("--port=") {
            if let Some(port) = parse_port_value(value) {
                return Some(port);
            }
        }
        if looks_like_dev_server && word == "-p" {
            if let Some(port) = iter.peek().and_then(|value| parse_port_value(value)) {
                return Some(port);
            }
            continue;
        }
        if looks_like_dev_server {
            if let Some(value) = word.strip_prefix("-p") {
                if !value.is_empty() {
                    if let Some(port) = parse_port_value(value) {
                        return Some(port);
                    }
                }
            }
        }
        if let Some(port) = detect_port_env_assignment(word) {
            return Some(port);
        }
    }
    None
}

fn detect_port_env_assignment(word: &str) -> Option<u16> {
    word.strip_prefix("PORT=").and_then(parse_port_value)
}

fn parse_port_value(value: &str) -> Option<u16> {
    (!value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()))
        .then(|| value.parse::<u16>().ok())
        .flatten()
}

fn command_declared_port_is_likely_listener(spec: &ResolvedCommandSpec) -> bool {
    if is_shell_command(&spec.command) {
        return shell_command_payloads(&spec.args)
            .into_iter()
            .any(shell_payload_looks_like_dev_server);
    }
    command_tokens_look_like_dev_server(
        std::iter::once(spec.command.as_str())
            .chain(spec.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .as_slice(),
    )
}

fn shell_payload_looks_like_dev_server(payload: &str) -> bool {
    let words = payload.split_ascii_whitespace().collect::<Vec<_>>();
    command_tokens_look_like_dev_server(&words)
}

fn command_tokens_look_like_dev_server(tokens: &[&str]) -> bool {
    let names = tokens
        .iter()
        .map(|token| command_token_name(token))
        .collect::<Vec<_>>();
    names.iter().any(|name| {
        matches!(
            *name,
            "vite" | "astro" | "next" | "nuxt" | "webpack-dev-server"
        )
    }) || package_manager_runs_dev_script(&names)
}

fn package_manager_runs_dev_script(tokens: &[&str]) -> bool {
    tokens.windows(3).any(|window| {
        matches!(window[0], "npm" | "pnpm" | "yarn" | "bun")
            && window[1] == "run"
            && window[2] == "dev"
    }) || tokens
        .windows(2)
        .any(|window| matches!(window[0], "pnpm" | "yarn" | "bun") && window[1] == "dev")
}

fn command_token_name(token: &str) -> &str {
    Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(token)
}

fn ensure_declared_command_port_available(spec: &ResolvedCommandSpec) -> Result<()> {
    let Some(port) = detect_command_port(spec) else {
        return Ok(());
    };
    if port == 0 {
        return Ok(());
    }
    if !command_declared_port_is_likely_listener(spec) {
        return Ok(());
    }

    for host in command_port_preflight_hosts(spec) {
        match StdTcpListener::bind((host, port)) {
            Ok(listener) => {
                drop(listener);
            }
            Err(error) if host == "::1" && error.kind() == ErrorKind::AddrNotAvailable => {
                continue;
            }
            Err(error) => {
                let owner = describe_port_owner(port)
                    .filter(|detail| !detail.trim().is_empty())
                    .map(|detail| format!(" Listener detail: {detail}"))
                    .unwrap_or_default();
                bail!(
                    "requested command port {port} is already in use ({error}). Stop the existing listener or choose another port before starting this dev server.{owner}"
                );
            }
        }
    }
    Ok(())
}

fn describe_port_owner(port: u16) -> Option<String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "ss -H -ltnp 'sport = :{port}' 2>/dev/null | head -n 3"
        ))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let summary = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    (!summary.is_empty()).then_some(summary)
}

async fn record_shared_checkout_git_command_warning(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    spec: &ResolvedCommandSpec,
) {
    if !is_risky_git_command(spec) {
        return;
    }
    let Ok(job) = state.store.get_job(job_id) else {
        return;
    };
    let Some(session_id) = job.job.session_id.as_deref() else {
        return;
    };
    let Ok(detail) = state.store.get_session(session_id) else {
        return;
    };
    if detail.session.workspace_mode != "shared_project_root" {
        return;
    }
    let shared_count = state
        .store
        .list_sessions()
        .map(|sessions| {
            sessions
                .into_iter()
                .filter(|session| {
                    session.state == "active"
                        && session.id != session_id
                        && session.working_dir == detail.session.working_dir
                })
                .count()
        })
        .unwrap_or(0);
    if shared_count == 0 {
        return;
    }
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "workspace.warning".to_string(),
        status: "warning".to_string(),
        summary: "Risky git command in shared checkout".to_string(),
        detail: format!(
            "Command '{}' may change branch or discard changes while {shared_count} other active session(s) share {}.",
            command_label(spec),
            detail.session.working_dir
        ),
        data_json: json!({
            "tool_call_id": tool_call_id,
            "session_id": session_id,
            "workspace_mode": detail.session.workspace_mode,
            "working_dir": detail.session.working_dir,
            "shared_session_count": shared_count,
        }),
    });
}

fn is_risky_git_command(spec: &ResolvedCommandSpec) -> bool {
    let mut parts = Vec::with_capacity(spec.args.len() + 1);
    parts.push(spec.command.as_str());
    parts.extend(spec.args.iter().map(String::as_str));
    let line = parts.join(" ");
    let normalized = line.trim();
    normalized.starts_with("git checkout")
        || normalized.starts_with("git switch")
        || normalized.starts_with("git reset")
        || normalized.starts_with("git clean")
        || normalized.contains(" git checkout")
        || normalized.contains(" git switch")
        || normalized.contains(" git reset")
        || normalized.contains(" git clean")
}

async fn run_bounded_command_tool(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    checkpoint: &mut WorkerCheckpoint,
    cancel_rx: &mut watch::Receiver<bool>,
    spec: ResolvedCommandSpec,
) -> Result<Value> {
    let started = start_command_session(state, job_id, worker, tool_call_id, &spec, false).await?;
    state
        .agent
        .transfer_write_lock(tool_call_id, &started.id)
        .context("failed to transfer the command write lock")?;
    if let Some(pending) = checkpoint.pending_action.as_mut() {
        pending.command_session_id = Some(started.id.clone());
        state.store.write_worker_checkpoint(
            &worker.id,
            &serde_json::to_value(&checkpoint).context("failed to encode worker checkpoint")?,
        )?;
    }
    let completed =
        wait_for_command_session_completion(state, &started.id, cancel_rx, "command.run").await?;
    Ok(command_session_result_json(
        &completed,
        &artifact_snapshot_from_summary(state, &completed)?,
    ))
}

async fn execute_command_session_open_tool(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    args: CommandSessionOpenArgs,
) -> Result<Value> {
    let wait_for_output_ms = args
        .wait_for_output_ms
        .unwrap_or(COMMAND_DEFAULT_WAIT_FOR_OUTPUT_MS)
        .clamp(0, COMMAND_MAX_WAIT_FOR_OUTPUT_MS);
    let spec = resolve_command_spec(
        worker,
        "interactive",
        args.title,
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        args.network_policy,
        args.env,
        false,
    )?;
    let started = start_command_session(state, job_id, worker, tool_call_id, &spec, true).await?;
    state
        .agent
        .transfer_write_lock(tool_call_id, &started.id)
        .context("failed to transfer the command write lock")?;
    let snapshot = snapshot_command_session(state, &started.id, wait_for_output_ms).await?;
    let latest = load_latest_command_session(state, &started.id).await?;
    Ok(command_session_result_json(&latest, &snapshot))
}

async fn execute_command_session_write_tool(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    args: CommandSessionWriteArgs,
) -> Result<Value> {
    let summary = state.store.get_command_session(&args.session_id)?;
    validate_command_session_scope(job_id, worker, &summary)?;
    let wait_for_output_ms = args
        .wait_for_output_ms
        .unwrap_or(COMMAND_DEFAULT_WAIT_FOR_OUTPUT_MS)
        .clamp(0, COMMAND_MAX_WAIT_FOR_OUTPUT_MS);
    let Some(handle) = state.agent.get_command_session(&summary.id).await else {
        bail!("command session '{}' is not running", summary.id);
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .control
        .send(CommandControl::Write {
            input: args.input,
            append_newline: args.append_newline.unwrap_or(true),
            wait_for_output_ms,
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow!("command session '{}' is no longer available", summary.id))?;
    let snapshot = reply_rx
        .await
        .map_err(|_| anyhow!("command session '{}' did not reply", summary.id))?
        .map_err(anyhow::Error::msg)?;
    let latest = state.store.get_command_session(&summary.id)?;
    Ok(command_session_result_json(&latest, &snapshot))
}

async fn execute_command_session_close_tool(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    args: CommandSessionCloseArgs,
) -> Result<Value> {
    let summary = state.store.get_command_session(&args.session_id)?;
    validate_command_session_scope(job_id, worker, &summary)?;
    let wait_for_exit_secs = args.wait_for_exit_secs.unwrap_or(5).clamp(1, 30);
    let Some(handle) = state.agent.get_command_session(&summary.id).await else {
        let snapshot = artifact_snapshot_from_summary(state, &summary)?;
        return Ok(command_session_result_json(&summary, &snapshot));
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .control
        .send(CommandControl::Close {
            wait_for_exit_secs,
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow!("command session '{}' is no longer available", summary.id))?;
    let close_result = reply_rx
        .await
        .map_err(|_| anyhow!("command session '{}' did not reply", summary.id))?
        .map_err(anyhow::Error::msg)?;
    let latest = state.store.get_command_session(&summary.id)?;
    Ok(json!({
        "id": latest.id,
        "state": close_result.state,
        "exit_code": close_result.exit_code,
        "last_error": close_result.last_error,
        "stdout_tail": close_result.stdout_tail,
        "stderr_tail": close_result.stderr_tail,
        "truncated": close_result.truncated,
        "stdout_artifact_id": latest.stdout_artifact_id,
        "stderr_artifact_id": latest.stderr_artifact_id,
        "completed_at": latest.completed_at,
    }))
}

async fn start_command_session(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    spec: &ResolvedCommandSpec,
    interactive: bool,
) -> Result<CommandSessionSummary> {
    let command_session_id = Uuid::new_v4().to_string();
    let command_summary = spec.title.clone();
    let log_dir = state
        .store
        .artifacts_dir_path()
        .join(job_id)
        .join("commands");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create '{}'", log_dir.display()))?;
    let stdout_path = log_dir.join(format!("{command_session_id}-stdout.log"));
    let stderr_path = log_dir.join(format!("{command_session_id}-stderr.log"));
    fs::write(&stdout_path, b"")
        .with_context(|| format!("failed to prepare '{}'", stdout_path.display()))?;
    fs::write(&stderr_path, b"")
        .with_context(|| format!("failed to prepare '{}'", stderr_path.display()))?;

    let command_session_workspace = command_session_workspace_metadata(state, job_id, spec);

    state.store.create_command_session(CommandSessionRecord {
        id: command_session_id.clone(),
        job_id: job_id.to_string(),
        worker_id: worker.id.clone(),
        tool_call_id: Some(tool_call_id.to_string()),
        mode: spec.mode.clone(),
        title: spec.title.clone(),
        state: "starting".to_string(),
        command: spec.command.clone(),
        args: spec.args.clone(),
        cwd: spec.cwd.display().to_string(),
        session_id: command_session_workspace.session_id,
        project_id: command_session_workspace.project_id,
        worktree_path: command_session_workspace.worktree_path,
        branch: command_session_workspace.branch,
        port: command_session_workspace.port,
        env_json: serde_json::to_value(&spec.env).context("failed to encode command env")?,
        network_policy: spec.network_policy.clone(),
        timeout_secs: spec.timeout_secs,
        output_limit_bytes: spec.output_limit_bytes,
        last_error: String::new(),
        exit_code: None,
        stdout_artifact_id: None,
        stderr_artifact_id: None,
        started_at: None,
        completed_at: None,
    })?;

    let stdout_artifact = match create_command_log_artifact(
        state,
        job_id,
        worker,
        tool_call_id,
        &command_session_id,
        "stdout",
        &spec.title,
        &stdout_path,
    ) {
        Ok(artifact) => artifact,
        Err(error) => {
            fail_command_session_start(
                state,
                job_id,
                &worker.id,
                tool_call_id,
                &command_session_id,
                &command_summary,
                &stderr_path,
                None,
                None,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    let stderr_artifact = match create_command_log_artifact(
        state,
        job_id,
        worker,
        tool_call_id,
        &command_session_id,
        "stderr",
        &spec.title,
        &stderr_path,
    ) {
        Ok(artifact) => artifact,
        Err(error) => {
            fail_command_session_start(
                state,
                job_id,
                &worker.id,
                tool_call_id,
                &command_session_id,
                &command_summary,
                &stderr_path,
                Some(&stdout_artifact),
                None,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    let _ = state.store.update_tool_call(
        tool_call_id,
        ToolCallPatch {
            artifact_ids: Some(vec![stdout_artifact.id.clone(), stderr_artifact.id.clone()]),
            ..ToolCallPatch::default()
        },
    )?;

    if let Err(error) = ensure_declared_command_port_available(spec) {
        fail_command_session_start(
            state,
            job_id,
            &worker.id,
            tool_call_id,
            &command_session_id,
            &command_summary,
            &stderr_path,
            Some(&stdout_artifact),
            Some(&stderr_artifact),
            &error,
        )
        .await;
        return Err(error);
    }

    let mut command = Command::new(&spec.command);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if interactive {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .kill_on_drop(true);
    if let Some(path) = command_path_env() {
        command.env("PATH", path);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    #[cfg(unix)]
    {
        command.process_group(0);
    }

    let mut child = match command
        .spawn()
        .with_context(|| format!("failed to start '{}'", spec.command))
    {
        Ok(child) => child,
        Err(error) => {
            fail_command_session_start(
                state,
                job_id,
                &worker.id,
                tool_call_id,
                &command_session_id,
                &command_summary,
                &stderr_path,
                Some(&stdout_artifact),
                Some(&stderr_artifact),
                &error,
            )
            .await;
            return Err(error);
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let error = anyhow!("failed to capture stdout for '{}'", spec.command);
            let _ = terminate_command_process(&mut child).await;
            let _ = child.wait().await;
            fail_command_session_start(
                state,
                job_id,
                &worker.id,
                tool_call_id,
                &command_session_id,
                &command_summary,
                &stderr_path,
                Some(&stdout_artifact),
                Some(&stderr_artifact),
                &error,
            )
            .await;
            return Err(error);
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let error = anyhow!("failed to capture stderr for '{}'", spec.command);
            let _ = terminate_command_process(&mut child).await;
            let _ = child.wait().await;
            fail_command_session_start(
                state,
                job_id,
                &worker.id,
                tool_call_id,
                &command_session_id,
                &command_summary,
                &stderr_path,
                Some(&stdout_artifact),
                Some(&stderr_artifact),
                &error,
            )
            .await;
            return Err(error);
        }
    };
    let stdin = child.stdin.take();

    let live_output = Arc::new(StdMutex::new(LiveCommandOutput::default()));
    let stdout_task = tokio::spawn(drain_command_output(
        stdout,
        stdout_path,
        true,
        live_output.clone(),
        spec.output_limit_bytes,
    ));
    let stderr_task = tokio::spawn(drain_command_output(
        stderr,
        stderr_path,
        false,
        live_output.clone(),
        spec.output_limit_bytes,
    ));
    let (control_tx, control_rx) = mpsc::channel(8);
    let (done_tx, done_rx) = watch::channel(false);

    let running = state.store.update_command_session(
        &command_session_id,
        CommandSessionPatch {
            state: Some("running".to_string()),
            stdout_artifact_id: Some(Some(stdout_artifact.id.clone())),
            stderr_artifact_id: Some(Some(stderr_artifact.id.clone())),
            started_at: Some(Some(unix_timestamp())),
            ..CommandSessionPatch::default()
        },
    )?;

    state
        .agent
        .register_command_session(&command_session_id, job_id, control_tx, done_rx.clone())
        .await;
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "command.session.started".to_string(),
        status: "running".to_string(),
        summary: format!("Started {}", spec.title),
        detail: render_command_plan(spec, "Nucleus-owned command session started."),
        data_json: json!({
            "command_session_id": command_session_id,
            "tool_call_id": tool_call_id,
            "mode": spec.mode,
        }),
    });
    publish_artifact_added(state, &stdout_artifact).await;
    publish_artifact_added(state, &stderr_artifact).await;
    publish_command_session_updated(state, &running).await;
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;

    tokio::spawn(run_command_session_controller(
        state.clone(),
        worker.id.clone(),
        running.clone(),
        stdin,
        child,
        live_output,
        stdout_task,
        stderr_task,
        control_rx,
        done_tx,
    ));

    Ok(running)
}

async fn wait_for_command_session_completion(
    state: &AppState,
    command_session_id: &str,
    cancel_rx: &mut watch::Receiver<bool>,
    label: &str,
) -> Result<CommandSessionSummary> {
    let Some(handle) = state.agent.get_command_session(command_session_id).await else {
        return state.store.get_command_session(command_session_id);
    };
    let mut done = handle.done.clone();

    loop {
        if *done.borrow() {
            break;
        }

        tokio::select! {
            changed = done.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = cancel_rx.changed() => {
                if changed.is_ok() && *cancel_rx.borrow() {
                    let _ = handle.control.send(CommandControl::Terminate {
                        reason: format!("{label} was canceled by Nucleus."),
                        final_state: "canceled".to_string(),
                    }).await;
                }
            }
        }
    }

    state.store.get_command_session(command_session_id)
}

async fn load_latest_command_session(
    state: &AppState,
    command_session_id: &str,
) -> Result<CommandSessionSummary> {
    if let Some(handle) = state.agent.get_command_session(command_session_id).await {
        let mut done = handle.done.clone();
        if !*done.borrow() {
            let _ = timeout(
                Duration::from_millis(COMMAND_STATE_SETTLE_WAIT_MS),
                done.changed(),
            )
            .await;
        }
    }

    state.store.get_command_session(command_session_id)
}

async fn snapshot_command_session(
    state: &AppState,
    command_session_id: &str,
    wait_for_output_ms: u64,
) -> Result<CommandInteractionResult> {
    let Some(handle) = state.agent.get_command_session(command_session_id).await else {
        let summary = state.store.get_command_session(command_session_id)?;
        return artifact_snapshot_from_summary(state, &summary);
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .control
        .send(CommandControl::Snapshot {
            wait_for_output_ms,
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            anyhow!(
                "command session '{}' is no longer available",
                command_session_id
            )
        })?;
    reply_rx
        .await
        .map_err(|_| anyhow!("command session '{}' did not reply", command_session_id))?
        .map_err(anyhow::Error::msg)
}

fn validate_command_session_scope(
    job_id: &str,
    worker: &WorkerSummary,
    summary: &CommandSessionSummary,
) -> Result<()> {
    if summary.job_id != job_id {
        bail!(
            "command session '{}' does not belong to this job",
            summary.id
        );
    }
    if summary.worker_id != worker.id {
        bail!(
            "command session '{}' is not owned by this worker",
            summary.id
        );
    }
    Ok(())
}

fn command_session_result_json(
    summary: &CommandSessionSummary,
    snapshot: &CommandInteractionResult,
) -> Value {
    json!({
        "id": summary.id,
        "mode": summary.mode,
        "title": summary.title,
        "state": summary.state,
        "command": summary.command,
        "args": summary.args,
        "cwd": summary.cwd,
        "network_policy": summary.network_policy,
        "timeout_secs": summary.timeout_secs,
        "output_limit_bytes": summary.output_limit_bytes,
        "last_error": summary.last_error,
        "exit_code": summary.exit_code,
        "stdout_tail": snapshot.stdout_tail,
        "stderr_tail": snapshot.stderr_tail,
        "truncated": snapshot.truncated,
        "stdout_artifact_id": summary.stdout_artifact_id,
        "stderr_artifact_id": summary.stderr_artifact_id,
        "started_at": summary.started_at,
        "completed_at": summary.completed_at,
    })
}

fn create_command_log_artifact(
    state: &AppState,
    job_id: &str,
    worker: &WorkerSummary,
    tool_call_id: &str,
    command_session_id: &str,
    stream: &str,
    title: &str,
    path: &Path,
) -> Result<ArtifactSummary> {
    state.store.create_job_artifact(JobArtifactRecord {
        id: Uuid::new_v4().to_string(),
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        tool_call_id: Some(tool_call_id.to_string()),
        command_session_id: Some(command_session_id.to_string()),
        kind: "command-log".to_string(),
        title: format!("{title} {stream}"),
        path: path.display().to_string(),
        mime_type: "text/plain".to_string(),
        size_bytes: 0,
        preview_text: format!("Waiting for {stream} output."),
        metadata_json: json!({}),
    })
}

fn load_artifact_preview_from_summary(
    state: &AppState,
    artifact_id: Option<&str>,
) -> Result<String> {
    let Some(artifact_id) = artifact_id else {
        return Ok(String::new());
    };
    Ok(state.store.get_job_artifact(artifact_id)?.preview_text)
}

fn artifact_snapshot_from_summary(
    state: &AppState,
    summary: &CommandSessionSummary,
) -> Result<CommandInteractionResult> {
    let stdout_tail =
        load_artifact_preview_from_summary(state, summary.stdout_artifact_id.as_deref())?;
    let stderr_tail =
        load_artifact_preview_from_summary(state, summary.stderr_artifact_id.as_deref())?;
    let truncated = stdout_tail.contains(COMMAND_TRUNCATED_NOTE)
        || stderr_tail.contains(COMMAND_TRUNCATED_NOTE);
    Ok(CommandInteractionResult {
        stdout_tail,
        stderr_tail,
        truncated,
    })
}

async fn fail_command_session_start(
    state: &AppState,
    job_id: &str,
    worker_id: &str,
    tool_call_id: &str,
    command_session_id: &str,
    title: &str,
    stderr_path: &Path,
    stdout_artifact: Option<&ArtifactSummary>,
    stderr_artifact: Option<&ArtifactSummary>,
    error: &anyhow::Error,
) {
    let note = format!("failed to start command session: {error}\n");
    let _ = fs::write(stderr_path, note.as_bytes());

    let artifact_ids = stdout_artifact
        .into_iter()
        .chain(stderr_artifact.into_iter())
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    if !artifact_ids.is_empty() {
        let _ = state.store.update_tool_call(
            tool_call_id,
            ToolCallPatch {
                artifact_ids: Some(artifact_ids),
                ..ToolCallPatch::default()
            },
        );
    }

    if let Some(artifact) = stderr_artifact {
        let _ = state.store.update_job_artifact(
            &artifact.id,
            JobArtifactPatch {
                size_bytes: Some(note.len() as u64),
                preview_text: Some(excerpt(&note, COMMAND_PREVIEW_CHAR_LIMIT)),
                ..JobArtifactPatch::default()
            },
        );
    }

    if let Some(artifact) = stdout_artifact {
        publish_artifact_added(state, artifact).await;
    }
    if let Some(artifact) = stderr_artifact {
        publish_artifact_added(state, artifact).await;
    }

    if let Ok(summary) = state.store.update_command_session(
        command_session_id,
        CommandSessionPatch {
            state: Some("failed".to_string()),
            last_error: Some(error.to_string()),
            stdout_artifact_id: Some(stdout_artifact.map(|artifact| artifact.id.clone())),
            stderr_artifact_id: Some(stderr_artifact.map(|artifact| artifact.id.clone())),
            completed_at: Some(Some(unix_timestamp())),
            ..CommandSessionPatch::default()
        },
    ) {
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job_id.to_string(),
            worker_id: Some(worker_id.to_string()),
            event_type: "command.session.updated".to_string(),
            status: "failed".to_string(),
            summary: format!("Failed {title}"),
            detail: excerpt(&note, 240),
            data_json: json!({
                "command_session_id": command_session_id,
                "tool_call_id": tool_call_id,
            }),
        });
        publish_command_session_updated(state, &summary).await;
        if let Ok(detail) = state.store.get_job(job_id) {
            publish_job_updated(state, &detail.job).await;
        }
    }
}

async fn terminate_command_process(child: &mut tokio::process::Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let result = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            if result == 0 {
                return Ok(());
            }

            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(error);
        }
    }

    child.kill().await
}

async fn terminate_command_process_and_wait(
    child: &mut tokio::process::Child,
    wait_for_exit: Duration,
) -> Option<ExitStatus> {
    let _ = terminate_command_process(child).await;
    match timeout(wait_for_exit, child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(_)) => None,
        Err(_) => {
            let _ = child.kill().await;
            child.wait().await.ok()
        }
    }
}

async fn run_command_session_controller(
    state: AppState,
    worker_id: String,
    summary: CommandSessionSummary,
    mut stdin: Option<tokio::process::ChildStdin>,
    mut child: tokio::process::Child,
    live_output: Arc<StdMutex<LiveCommandOutput>>,
    stdout_task: tokio::task::JoinHandle<Result<()>>,
    stderr_task: tokio::task::JoinHandle<Result<()>>,
    mut control_rx: mpsc::Receiver<CommandControl>,
    done_tx: watch::Sender<bool>,
) {
    let mut final_state = summary.state.clone();
    let mut last_error = String::new();
    let mut exit_code = None;
    let mut close_reply: Option<oneshot::Sender<Result<CommandCloseResult, String>>> = None;
    let timeout_window = tokio::time::sleep(Duration::from_secs(summary.timeout_secs));
    tokio::pin!(timeout_window);

    loop {
        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(status) => {
                        apply_command_exit_status(
                            &mut final_state,
                            &mut last_error,
                            &mut exit_code,
                            status,
                        );
                    }
                    Err(error) => {
                        final_state = "failed".to_string();
                        last_error = error.to_string();
                    }
                }
                break;
            }
            _ = &mut timeout_window => {
                final_state = "timed_out".to_string();
                last_error = format!(
                    "command exceeded the {} second Nucleus timeout",
                    summary.timeout_secs
                );
                if let Some(status) =
                    terminate_command_process_and_wait(&mut child, Duration::from_secs(2)).await
                {
                    exit_code = status.code();
                }
                break;
            }
            Some(control) = control_rx.recv() => {
                match control {
                    CommandControl::Snapshot { wait_for_output_ms, reply } => {
                        if wait_for_output_ms > 0 {
                            tokio::time::sleep(Duration::from_millis(wait_for_output_ms)).await;
                        }
                        let snapshot = snapshot_live_command_output(&live_output);
                        let maybe_status = child.try_wait();
                        let _ = reply.send(Ok(snapshot));
                        match maybe_status {
                            Ok(Some(status)) => {
                                apply_command_exit_status(
                                    &mut final_state,
                                    &mut last_error,
                                    &mut exit_code,
                                    status,
                                );
                                break;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                final_state = "failed".to_string();
                                last_error = error.to_string();
                                break;
                            }
                        }
                    }
                    CommandControl::Write {
                        input,
                        append_newline,
                        wait_for_output_ms,
                        reply,
                    } => {
                        let result = async {
                            let stdin = stdin
                                .as_mut()
                                .ok_or_else(|| "command session is not accepting input".to_string())?;
                            stdin
                                .write_all(input.as_bytes())
                                .await
                                .map_err(|error| error.to_string())?;
                            if append_newline {
                                stdin.write_all(b"\n").await.map_err(|error| error.to_string())?;
                            }
                            stdin.flush().await.map_err(|error| error.to_string())?;
                            if wait_for_output_ms > 0 {
                                tokio::time::sleep(Duration::from_millis(wait_for_output_ms)).await;
                            }
                            Ok(snapshot_live_command_output(&live_output))
                        }
                        .await;
                        let _ = reply.send(result);
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                apply_command_exit_status(
                                    &mut final_state,
                                    &mut last_error,
                                    &mut exit_code,
                                    status,
                                );
                                break;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                final_state = "failed".to_string();
                                last_error = error.to_string();
                                break;
                            }
                        }
                    }
                    CommandControl::Close {
                        wait_for_exit_secs,
                        reply,
                    } => {
                        stdin.take();
                        final_state = "closed".to_string();
                        close_reply = Some(reply);
                        match timeout(Duration::from_secs(wait_for_exit_secs), child.wait()).await {
                            Ok(Ok(status)) => {
                                exit_code = status.code();
                                if !status.success() {
                                    last_error = format_command_exit_error(status);
                                }
                            }
                            Ok(Err(error)) => {
                                last_error = error.to_string();
                            }
                            Err(_) => {
                                if let Some(status) = terminate_command_process_and_wait(
                                    &mut child,
                                    Duration::from_secs(2),
                                )
                                .await
                                {
                                    exit_code = status.code();
                                    if !status.success() {
                                        last_error = format_command_exit_error(status);
                                    }
                                }
                            }
                        }
                        break;
                    }
                    CommandControl::Terminate { reason, final_state: requested_state } => {
                        stdin.take();
                        final_state = requested_state;
                        last_error = reason;
                        if let Some(status) =
                            terminate_command_process_and_wait(&mut child, Duration::from_secs(2))
                                .await
                        {
                            exit_code = status.code();
                        }
                        break;
                    }
                }
            }
            else => {
                break;
            }
        }
    }

    let stdout_result = stdout_task.await;
    let stderr_result = stderr_task.await;
    if last_error.is_empty() {
        match stdout_result {
            Err(error) => last_error = format!("stdout task crashed: {error}"),
            Ok(Err(error)) => last_error = error.to_string(),
            Ok(Ok(())) => {}
        }
    }
    if last_error.is_empty() {
        match stderr_result {
            Err(error) => last_error = format!("stderr task crashed: {error}"),
            Ok(Err(error)) => last_error = error.to_string(),
            Ok(Ok(())) => {}
        }
    }

    let output = read_live_command_output(&live_output);
    let _ = refresh_command_log_artifacts(&state, &summary, &output);
    let final_summary = match state.store.update_command_session(
        &summary.id,
        CommandSessionPatch {
            state: Some(final_state.clone()),
            last_error: Some(last_error.clone()),
            exit_code: Some(exit_code),
            completed_at: Some(Some(unix_timestamp())),
            ..CommandSessionPatch::default()
        },
    ) {
        Ok(updated) => updated,
        Err(error) => {
            warn!(command_session_id = %summary.id, error = %error, "failed to finalize command session");
            let _ = done_tx.send(true);
            state.agent.release_write_lock(&summary.id);
            state.agent.finish_command_session(&summary.id).await;
            return;
        }
    };

    let _ = state.store.append_job_event(JobEventRecord {
        job_id: final_summary.job_id.clone(),
        worker_id: Some(worker_id),
        event_type: "command.session.updated".to_string(),
        status: final_summary.state.clone(),
        summary: format!(
            "{} {}",
            format_state_prefix(&final_summary.state),
            final_summary.title
        ),
        detail: if final_summary.last_error.is_empty() {
            shell_command_summary(&final_summary)
        } else {
            format!(
                "{}\n{}",
                shell_command_summary(&final_summary),
                excerpt(&final_summary.last_error, 240)
            )
        },
        data_json: json!({
            "command_session_id": final_summary.id,
            "mode": final_summary.mode,
            "exit_code": final_summary.exit_code,
        }),
    });
    publish_command_session_updated(&state, &final_summary).await;
    if let Ok(detail) = state.store.get_job(&final_summary.job_id) {
        publish_job_updated(&state, &detail.job).await;
    }

    if let Some(reply) = close_reply {
        let _ = reply.send(Ok(CommandCloseResult {
            state: final_summary.state.clone(),
            exit_code: final_summary.exit_code,
            last_error: final_summary.last_error.clone(),
            stdout_tail: render_output_preview(&output.stdout_tail, output.truncated),
            stderr_tail: render_output_preview(&output.stderr_tail, output.truncated),
            truncated: output.truncated,
        }));
    }

    let _ = done_tx.send(true);
    state.agent.release_write_lock(&summary.id);
    state.agent.finish_command_session(&summary.id).await;
}

async fn drain_command_output<R>(
    mut reader: R,
    path: PathBuf,
    is_stdout: bool,
    live_output: Arc<StdMutex<LiveCommandOutput>>,
    output_limit_bytes: usize,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut file = tokio::fs::File::create(&path)
        .await
        .with_context(|| format!("failed to open '{}'", path.display()))?;
    let mut buffer = vec![0u8; 4096];

    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .await
            .with_context(|| format!("failed to read '{}'", path.display()))?;
        if bytes_read == 0 {
            break;
        }

        let capture = {
            let mut output = live_output
                .lock()
                .expect("live command output mutex poisoned");
            let remaining = output_limit_bytes.saturating_sub(output.total_captured_bytes);
            if remaining == 0 {
                output.truncated = true;
                Vec::new()
            } else {
                let take = remaining.min(bytes_read);
                if take < bytes_read {
                    output.truncated = true;
                }
                output.total_captured_bytes += take;
                let text = String::from_utf8_lossy(&buffer[..take]).to_string();
                if is_stdout {
                    output.stdout_bytes += take as u64;
                    append_tail(&mut output.stdout_tail, &text, COMMAND_PREVIEW_CHAR_LIMIT);
                } else {
                    output.stderr_bytes += take as u64;
                    append_tail(&mut output.stderr_tail, &text, COMMAND_PREVIEW_CHAR_LIMIT);
                }
                buffer[..take].to_vec()
            }
        };

        if !capture.is_empty() {
            file.write_all(&capture)
                .await
                .with_context(|| format!("failed to write '{}'", path.display()))?;
        }
    }

    file.flush()
        .await
        .with_context(|| format!("failed to flush '{}'", path.display()))?;
    Ok(())
}

fn read_live_command_output(live_output: &Arc<StdMutex<LiveCommandOutput>>) -> LiveCommandOutput {
    live_output
        .lock()
        .expect("live command output mutex poisoned")
        .clone()
}

fn snapshot_live_command_output(
    live_output: &Arc<StdMutex<LiveCommandOutput>>,
) -> CommandInteractionResult {
    let output = read_live_command_output(live_output);
    CommandInteractionResult {
        stdout_tail: render_output_preview(&output.stdout_tail, output.truncated),
        stderr_tail: render_output_preview(&output.stderr_tail, output.truncated),
        truncated: output.truncated,
    }
}

fn refresh_command_log_artifacts(
    state: &AppState,
    summary: &CommandSessionSummary,
    output: &LiveCommandOutput,
) -> Result<()> {
    if let Some(artifact_id) = summary.stdout_artifact_id.as_deref() {
        let _ = state.store.update_job_artifact(
            artifact_id,
            JobArtifactPatch {
                size_bytes: Some(output.stdout_bytes),
                preview_text: Some(render_output_preview(&output.stdout_tail, output.truncated)),
                ..JobArtifactPatch::default()
            },
        )?;
    }
    if let Some(artifact_id) = summary.stderr_artifact_id.as_deref() {
        let _ = state.store.update_job_artifact(
            artifact_id,
            JobArtifactPatch {
                size_bytes: Some(output.stderr_bytes),
                preview_text: Some(render_output_preview(&output.stderr_tail, output.truncated)),
                ..JobArtifactPatch::default()
            },
        )?;
    }
    Ok(())
}

fn render_output_preview(value: &str, truncated: bool) -> String {
    let mut preview = excerpt(value, COMMAND_PREVIEW_CHAR_LIMIT);
    if truncated {
        if !preview.is_empty() {
            preview.push_str("\n\n");
        }
        preview.push_str(COMMAND_TRUNCATED_NOTE);
    }
    preview
}

fn append_tail(target: &mut String, chunk: &str, limit: usize) {
    target.push_str(chunk);
    let overflow = target.chars().count().saturating_sub(limit);
    if overflow == 0 {
        return;
    }
    *target = target.chars().skip(overflow).collect();
}

fn apply_command_exit_status(
    final_state: &mut String,
    last_error: &mut String,
    exit_code: &mut Option<i32>,
    status: ExitStatus,
) {
    *exit_code = status.code();
    if final_state.as_str() != "running" {
        return;
    }

    if status.success() {
        *final_state = "completed".to_string();
    } else {
        *final_state = "failed".to_string();
        *last_error = format_command_exit_error(status);
    }
}

fn format_command_exit_error(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("command exited with status {code}"),
        None => "command exited due to signal".to_string(),
    }
}

fn shell_command_summary(summary: &CommandSessionSummary) -> String {
    let spec = ResolvedCommandSpec {
        mode: summary.mode.clone(),
        title: summary.title.clone(),
        command: summary.command.clone(),
        args: summary.args.clone(),
        cwd: PathBuf::from(&summary.cwd),
        timeout_secs: summary.timeout_secs,
        output_limit_bytes: summary.output_limit_bytes,
        network_policy: summary.network_policy.clone(),
        env: BTreeMap::new(),
    };
    shell_quoted_command(&spec)
}

fn format_state_prefix(state: &str) -> &'static str {
    match state {
        "completed" => "Completed",
        "closed" => "Closed",
        "canceled" => "Canceled",
        "orphaned" => "Orphaned",
        "failed" => "Failed",
        _ => "Updated",
    }
}

fn apply_patch_edits(content: &str, edits: &[PatchEditArgs]) -> Result<String> {
    let mut next = content.to_string();
    for edit in edits {
        if edit.find.is_empty() {
            bail!("patch edits require a non-empty 'find' value");
        }
        if edit.replace_all.unwrap_or(false) {
            let matches = next.matches(&edit.find).count();
            if matches == 0 {
                bail!("patch edit did not match any content");
            }
            next = next.replace(&edit.find, &edit.replace);
        } else {
            let matches = next.match_indices(&edit.find).count();
            if matches == 0 {
                bail!("patch edit did not match any content");
            }
            if matches > 1 {
                bail!("patch edit matched multiple locations; use replace_all to replace them all");
            }
            next = next.replacen(&edit.find, &edit.replace, 1);
        }
    }
    Ok(next)
}

fn ensure_parent_exists_or_allowed(target: &Path, create_parent_dirs: bool) -> Result<()> {
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    if parent.exists() || create_parent_dirs {
        return Ok(());
    }
    bail!(
        "parent directory '{}' does not exist; enable create_parent_dirs to create it",
        parent.display()
    );
}

fn validated_stage_paths(worker: &WorkerSummary, pathspecs: &[String]) -> Result<Vec<PathBuf>> {
    if pathspecs.is_empty() {
        bail!("git.stage_patch requires at least one pathspec");
    }
    pathspecs
        .iter()
        .map(|pathspec| resolve_write_scoped_path(worker, pathspec, true))
        .collect()
}

fn text_artifact(
    kind: &str,
    title: String,
    extension: &str,
    mime_type: &str,
    content: String,
) -> ArtifactDraft {
    text_artifact_with_metadata(kind, title, extension, mime_type, content, json!({}))
}

fn text_artifact_with_metadata(
    kind: &str,
    title: String,
    extension: &str,
    mime_type: &str,
    content: String,
    metadata_json: Value,
) -> ArtifactDraft {
    ArtifactDraft {
        kind: kind.to_string(),
        title,
        mime_type: mime_type.to_string(),
        extension: extension.to_string(),
        preview_text: excerpt(&content, DIFF_PREVIEW_CHAR_LIMIT),
        content,
        metadata_json,
    }
}

fn render_text_diff(path: &Path, before: &str, after: &str) -> Result<String> {
    if before == after {
        return Ok(format!("No changes for {}.", path.display()));
    }

    let temp_dir = std::env::temp_dir().join(format!("nucleus-diff-{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create '{}'", temp_dir.display()))?;
    let before_path = temp_dir.join("before.txt");
    let after_path = temp_dir.join("after.txt");
    fs::write(&before_path, before)
        .with_context(|| format!("failed to write '{}'", before_path.display()))?;
    fs::write(&after_path, after)
        .with_context(|| format!("failed to write '{}'", after_path.display()))?;

    let output = std::process::Command::new("git")
        .args([
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--",
            before_path.to_string_lossy().as_ref(),
            after_path.to_string_lossy().as_ref(),
        ])
        .output()
        .with_context(|| "failed to render a text diff".to_string())?;
    let status = output.status.code().unwrap_or(-1);
    if status != 0 && status != 1 {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let _ = fs::remove_dir_all(&temp_dir);
        bail!(
            "git diff exited with {}{}",
            status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {}", excerpt(&stderr, 240))
            }
        );
    }

    let mut diff = String::from_utf8_lossy(&output.stdout).to_string();
    diff = diff.replace(
        before_path.to_string_lossy().as_ref(),
        &format!("a/{}", path.display()),
    );
    diff = diff.replace(
        after_path.to_string_lossy().as_ref(),
        &format!("b/{}", path.display()),
    );
    let _ = fs::remove_dir_all(&temp_dir);
    Ok(diff.trim().to_string())
}

fn write_job_artifact(
    state: &AppState,
    job_id: &str,
    worker_id: Option<&str>,
    tool_call_id: Option<&str>,
    draft: ArtifactDraft,
) -> Result<ArtifactSummary> {
    let artifact_id = Uuid::new_v4().to_string();
    let artifact_dir = state.store.artifacts_dir_path().join(job_id);
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create '{}'", artifact_dir.display()))?;
    let artifact_path = artifact_dir.join(format!("{}.{}", artifact_id, draft.extension));
    fs::write(&artifact_path, draft.content.as_bytes())
        .with_context(|| format!("failed to write '{}'", artifact_path.display()))?;
    state.store.create_job_artifact(JobArtifactRecord {
        id: artifact_id,
        job_id: job_id.to_string(),
        worker_id: worker_id.map(ToOwned::to_owned),
        tool_call_id: tool_call_id.map(ToOwned::to_owned),
        command_session_id: None,
        kind: draft.kind,
        title: draft.title,
        path: artifact_path.display().to_string(),
        mime_type: draft.mime_type,
        size_bytes: draft.content.len() as u64,
        preview_text: draft.preview_text,
        metadata_json: draft.metadata_json,
    })
}

fn write_job_artifact_bytes(
    state: &AppState,
    job_id: &str,
    worker_id: Option<&str>,
    tool_call_id: Option<&str>,
    draft: ArtifactBytesDraft,
) -> Result<ArtifactSummary> {
    let artifact_id = Uuid::new_v4().to_string();
    let artifact_dir = state.store.artifacts_dir_path().join(job_id);
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create '{}'", artifact_dir.display()))?;
    let artifact_path = artifact_dir.join(format!("{}.{}", artifact_id, draft.extension));
    fs::write(&artifact_path, &draft.bytes)
        .with_context(|| format!("failed to write '{}'", artifact_path.display()))?;
    state.store.create_job_artifact(JobArtifactRecord {
        id: artifact_id,
        job_id: job_id.to_string(),
        worker_id: worker_id.map(ToOwned::to_owned),
        tool_call_id: tool_call_id.map(ToOwned::to_owned),
        command_session_id: None,
        kind: draft.kind,
        title: draft.title,
        path: artifact_path.display().to_string(),
        mime_type: draft.mime_type,
        size_bytes: draft.bytes.len() as u64,
        preview_text: excerpt(&draft.preview_text, DIFF_PREVIEW_CHAR_LIMIT),
        metadata_json: draft.metadata_json,
    })
}

fn append_tool_call_artifact_ids(
    state: &AppState,
    job_id: &str,
    tool_call_id: &str,
    artifact_ids: &[String],
) -> Result<()> {
    if artifact_ids.is_empty() {
        return Ok(());
    }
    let mut current = state
        .store
        .get_job(job_id)?
        .tool_calls
        .into_iter()
        .find(|tool_call| tool_call.id == tool_call_id)
        .map(|tool_call| tool_call.artifact_ids)
        .unwrap_or_default();
    for artifact_id in artifact_ids {
        if !current.iter().any(|candidate| candidate == artifact_id) {
            current.push(artifact_id.clone());
        }
    }
    let _ = state.store.update_tool_call(
        tool_call_id,
        ToolCallPatch {
            artifact_ids: Some(current),
            ..ToolCallPatch::default()
        },
    )?;
    Ok(())
}

fn decode_data_url(value: &str) -> Result<Option<(String, Vec<u8>)>> {
    let Some(rest) = value.strip_prefix("data:") else {
        return Ok(None);
    };
    let Some((metadata, encoded)) = rest.split_once(',') else {
        return Ok(None);
    };
    if !metadata.ends_with(";base64") {
        return Ok(None);
    }
    let mime_type = metadata.trim_end_matches(";base64").to_string();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("failed to decode browser screenshot data URL")?;
    Ok(Some((mime_type, bytes)))
}

fn resolve_scoped_path(
    worker: &WorkerSummary,
    input: &str,
    allow_missing: bool,
) -> Result<PathBuf> {
    resolve_scoped_path_in_roots(worker, input, &worker.read_roots, allow_missing, "read")
}

async fn command_output(command: &str, args: &[&str]) -> Result<String> {
    command_output_in_dir(command, args, None).await
}

async fn command_output_in_dir(command: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut child = Command::new(command);
    child.args(args);
    if let Some(cwd) = cwd {
        child.current_dir(cwd);
    }
    if let Some(path) = command_path_env() {
        child.env("PATH", path);
    }
    let output = child
        .output()
        .await
        .with_context(|| format!("failed to start '{}'", command))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!(
            "'{}' exited with {}{}",
            command,
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {}", excerpt(&detail, 240))
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn normalized_note(note: Option<String>, default: &str) -> String {
    note.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn fallback_note(note: &str, default: &str) -> String {
    let trimmed = note.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
fn policy_for_tool(tool: &str) -> PolicyDecisionRecord {
    policy_for_tool_with_mode(tool, "ask")
}

fn policy_for_tool_with_mode(tool: &str, approval_mode: &str) -> PolicyDecisionRecord {
    if approval_mode == "trusted" && requires_approval_for_tool(tool) {
        return PolicyDecisionRecord {
            decision: "allow".to_string(),
            reason: "session allows Nucleus to run actions without per-step approval".to_string(),
            matched_rule: format!("session-trusted-actions:{tool}"),
            scope_kind: if is_browser_tool(tool) {
                "browser"
            } else if is_mutating_tool(tool) {
                "path"
            } else {
                "process"
            }
            .to_string(),
            risk_level: if is_browser_tool(tool) || is_mutating_tool(tool) {
                "medium".to_string()
            } else {
                "high".to_string()
            },
        };
    }

    if requires_approval_for_tool(tool) {
        PolicyDecisionRecord {
            decision: "require_approval".to_string(),
            reason: if is_browser_tool(tool) {
                "browser navigation and page interaction require explicit operator approval"
                    .to_string()
            } else if is_mutating_tool(tool) {
                "repo mutations require explicit operator approval".to_string()
            } else {
                "Nucleus-owned command launches require explicit operator approval".to_string()
            },
            matched_rule: if is_browser_tool(tool) {
                format!("approval:browser:{tool}")
            } else if is_mutating_tool(tool) {
                format!("approval:mutation:{tool}")
            } else {
                format!("approval:command:{tool}")
            },
            scope_kind: if is_browser_tool(tool) {
                "browser"
            } else if is_mutating_tool(tool) {
                "path"
            } else {
                "process"
            }
            .to_string(),
            risk_level: if is_browser_tool(tool) || is_mutating_tool(tool) {
                "medium".to_string()
            } else {
                "high".to_string()
            },
        }
    } else {
        PolicyDecisionRecord {
            decision: "allow".to_string(),
            reason: if is_command_follow_up_tool(tool) {
                "continuing an already-approved Nucleus command session".to_string()
            } else if is_browser_read_tool(tool) {
                "read-only browser inspection inside the session scope".to_string()
            } else {
                "read-only tool inside the session scope".to_string()
            },
            matched_rule: if is_command_follow_up_tool(tool) {
                format!("auto-command-follow-up:{tool}")
            } else if is_browser_read_tool(tool) {
                format!("auto-browser-read:{tool}")
            } else {
                format!("auto-readonly:{tool}")
            },
            scope_kind: if is_command_follow_up_tool(tool) {
                "process"
            } else if is_browser_read_tool(tool) {
                "browser"
            } else {
                "path"
            }
            .to_string(),
            risk_level: if is_command_follow_up_tool(tool) {
                "medium".to_string()
            } else {
                "low".to_string()
            },
        }
    }
}

fn requires_approval_for_tool(tool: &str) -> bool {
    is_mutating_tool(tool)
        || is_browser_action_tool(tool)
        || matches!(tool, "command.run" | "command.session.open" | "tests.run")
}

fn is_mutating_tool(tool: &str) -> bool {
    matches!(
        tool,
        "fs.apply_patch"
            | "fs.write_text"
            | "fs.move"
            | "fs.mkdir"
            | "git.stage_patch"
            | "github.comment"
    )
}

fn is_command_follow_up_tool(tool: &str) -> bool {
    matches!(tool, "command.session.write" | "command.session.close")
}

fn is_browser_read_tool(tool: &str) -> bool {
    matches!(
        tool,
        "browser.context" | "browser.snapshot" | "browser.screenshot"
    )
}

fn is_browser_action_tool(tool: &str) -> bool {
    matches!(
        tool,
        "browser.navigate"
            | "browser.click"
            | "browser.type"
            | "browser.fill"
            | "browser.scroll"
            | "browser.press"
            | "browser.submit"
    )
}

fn is_browser_tool(tool: &str) -> bool {
    is_browser_read_tool(tool) || is_browser_action_tool(tool)
}

fn requires_write_lock(tool: &str) -> bool {
    is_mutating_tool(tool) || matches!(tool, "command.run" | "command.session.open" | "tests.run")
}

fn lock_reason_for_tool(tool: &str, summary: &str) -> String {
    let detail = summary.trim();
    if detail.is_empty() {
        format!("Nucleus-owned {tool}")
    } else {
        format!("{tool}: {detail}")
    }
}

fn normalize_lock_roots(roots: &[String]) -> Result<Vec<PathBuf>> {
    let mut normalized = roots
        .iter()
        .map(|root| normalize_lock_root(root))
        .collect::<Result<Vec<_>>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalize_lock_root(root: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(root);
    if candidate.exists() {
        return fs::canonicalize(&candidate)
            .with_context(|| format!("failed to resolve write root '{}'", candidate.display()));
    }
    Ok(normalize_lexical_path(&candidate))
}

fn write_lock_roots_conflict(left: &[PathBuf], right: &[PathBuf]) -> bool {
    left.iter().any(|left_root| {
        right.iter().any(|right_root| {
            left_root.starts_with(right_root) || right_root.starts_with(left_root)
        })
    })
}

fn resolve_write_scoped_path(
    worker: &WorkerSummary,
    input: &str,
    allow_missing: bool,
) -> Result<PathBuf> {
    resolve_scoped_path_in_roots(worker, input, &worker.write_roots, allow_missing, "write")
}

fn resolve_scoped_path_in_roots(
    worker: &WorkerSummary,
    input: &str,
    roots: &[String],
    allow_missing: bool,
    scope_label: &str,
) -> Result<PathBuf> {
    let raw = PathBuf::from(input);
    let candidate = if raw.is_absolute() {
        raw
    } else {
        Path::new(&worker.working_dir).join(raw)
    };
    let normalized = normalize_lexical_path(&candidate);
    let resolved = if allow_missing {
        normalized
    } else {
        fs::canonicalize(&normalized)
            .with_context(|| format!("failed to resolve '{}'", normalized.display()))?
    };
    let allowed_roots = roots
        .iter()
        .map(|root| {
            fs::canonicalize(root)
                .with_context(|| format!("failed to resolve scope root '{}'", root))
        })
        .collect::<Result<Vec<_>>>()?;
    let allowed = allowed_roots.iter().any(|root| resolved.starts_with(root));
    if !allowed {
        bail!(
            "path '{}' is outside the worker {} scope",
            resolved.display(),
            scope_label
        );
    }

    Ok(resolved)
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn format_tool_result(result: &Value) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
}

fn limit_text(value: String, max_chars: usize) -> String {
    excerpt(&value, max_chars)
}

async fn fail_job(state: &AppState, job_id: &str, error: &str) -> Result<()> {
    let detail = state.store.get_job(job_id)?;
    let is_root_job = detail.job.parent_job_id.is_none();
    for child in &detail.child_jobs {
        if is_non_terminal_job_state(&child.state) {
            let _ = cancel_job(state.clone(), child.id.clone()).await;
        }
    }
    state
        .agent
        .terminate_job_command_sessions(
            job_id,
            "The job failed and closed any remaining Nucleus-owned command sessions.",
            "canceled",
        )
        .await;
    reconcile_failed_job_command_sessions(
        state,
        job_id,
        "The job failed and closed any remaining Nucleus-owned command sessions.",
        "canceled",
    )
    .await;
    state.store.update_job(
        job_id,
        JobPatch {
            state: Some("failed".to_string()),
            last_error: Some(error.to_string()),
            publication_status: detail
                .job
                .publication_requested
                .then(|| "failed".to_string()),
            publication_summary: detail
                .job
                .publication_requested
                .then(|| excerpt(error, 320)),
            ..JobPatch::default()
        },
    )?;
    for worker in &detail.workers {
        let _ = state.store.update_worker(
            &worker.id,
            WorkerPatch {
                state: Some("failed".to_string()),
                last_error: Some(error.to_string()),
                ..WorkerPatch::default()
            },
        );
    }
    if is_root_job {
        if let Some(session_id) = detail.job.session_id.as_deref() {
            let _ = state.store.update_session(
                session_id,
                SessionPatch {
                    state: Some("error".to_string()),
                    last_error: Some(error.to_string()),
                    ..SessionPatch::default()
                },
            );
            if let Ok(session) = state.store.get_session(session_id) {
                let _ = publish_session_event(state, session).await;
            }
        }
    }
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: detail.job.root_worker_id.clone(),
        event_type: "job.failed".to_string(),
        status: "failed".to_string(),
        summary: "Utility Worker job failed.".to_string(),
        detail: excerpt(error, 320),
        data_json: json!({ "error": error }),
    });
    if detail.job.publication_requested {
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job_id.to_string(),
            worker_id: detail.job.root_worker_id.clone(),
            event_type: "job.publication.blocked".to_string(),
            status: "failed".to_string(),
            summary: "Publication job failed.".to_string(),
            detail: excerpt(error, 320),
            data_json: json!({
                "publication_requested": true,
                "publication_status": "failed",
                "publication_summary": excerpt(error, 320),
            }),
        });
    }
    publish_job_failed(state, &state.store.get_job(job_id)?.job).await;
    if let Some(parent_job_id) = detail.job.parent_job_id.as_deref() {
        publish_job_updated(state, &state.store.get_job(parent_job_id)?.job).await;
    }
    // Run the post-turn memory decision classifier even when the worker errored.
    // The user's prompt is already persisted as a session turn at this point, so
    // an explicit "remember X" still saves even though no assistant reply exists.
    if is_root_job {
        if let Some(session_id) = detail.job.session_id.as_deref() {
            let _ = crate::extract_memory_decisions_after_turn(state, session_id, None).await;
        }
    }
    let _ = publish_overview_event(state).await;
    Ok(())
}

fn is_non_terminal_job_state(state: &str) -> bool {
    matches!(state, "queued" | "running" | "paused" | "waiting")
}

async fn reconcile_failed_job_command_sessions(
    state: &AppState,
    job_id: &str,
    reason: &str,
    final_state: &str,
) {
    let Ok(detail) = state.store.get_job(job_id) else {
        return;
    };
    for command_session in detail.command_sessions {
        if !is_non_terminal_command_session_state(&command_session.state) {
            continue;
        }
        let Ok(updated) = state.store.update_command_session(
            &command_session.id,
            CommandSessionPatch {
                state: Some(final_state.to_string()),
                last_error: Some(reason.to_string()),
                completed_at: Some(Some(unix_timestamp())),
                ..CommandSessionPatch::default()
            },
        ) else {
            continue;
        };
        if let Some(tool_call_id) = command_session.tool_call_id.as_deref() {
            if detail.tool_calls.iter().any(|tool_call| {
                tool_call.id == tool_call_id && is_non_terminal_tool_call_status(&tool_call.status)
            }) {
                let _ = state.store.update_tool_call(
                    tool_call_id,
                    ToolCallPatch {
                        status: Some("failed".to_string()),
                        error_class: Some("job_failed".to_string()),
                        error_detail: Some(reason.to_string()),
                        completed_at: Some(Some(unix_timestamp())),
                        ..ToolCallPatch::default()
                    },
                );
            }
        }
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job_id.to_string(),
            worker_id: Some(command_session.worker_id),
            event_type: "command.session.updated".to_string(),
            status: updated.state.clone(),
            summary: format!("{} {}", format_state_prefix(&updated.state), updated.title),
            detail: reason.to_string(),
            data_json: json!({
                "command_session_id": updated.id,
                "mode": updated.mode,
                "reason": "job_failed",
            }),
        });
        publish_command_session_updated(state, &updated).await;
    }
}

fn is_non_terminal_command_session_state(state: &str) -> bool {
    matches!(state, "starting" | "running")
}

async fn resolve_hidden_worker_target(
    state: &AppState,
    session: &SessionSummary,
    compiler_role: &str,
    needs_vision_tools: bool,
) -> Result<HiddenWorkerTarget, ApiError> {
    if compiler_role == "main" {
        if !session.route_id.trim().is_empty() {
            let route_profiles = load_router_profiles(state, false).await?;
            let route = route_profiles
                .iter()
                .find(|profile| profile.id == session.route_id)
                .ok_or_else(|| {
                    ApiError::bad_request(format!("unknown router profile '{}'", session.route_id))
                })?;

            if !route.enabled {
                return Err(ApiError::bad_request(format!(
                    "router profile '{}' is disabled",
                    route.title
                )));
            }

            let targets = resolve_profile_targets(state, route, false)
                .await?
                .into_iter()
                .map(|target| HiddenWorkerTargetCandidate {
                    target: HiddenWorkerTarget {
                        provider: target.provider,
                        model: target.model,
                        provider_base_url: target.provider_base_url,
                        provider_api_key: target.provider_api_key,
                    },
                    runtime_ready: target.runtime_ready,
                })
                .collect::<Vec<_>>();
            let mut target =
                select_hidden_worker_target(targets, needs_vision_tools).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "router profile '{}' has no usable targets",
                        route.title
                    ))
                })?;
            if target.provider == session.provider && !session.model.trim().is_empty() {
                target.model = session.model.clone();
            }
            ensure_hidden_worker_target_ready(state, &target, needs_vision_tools).await?;
            return Ok(target);
        }

        let target = HiddenWorkerTarget {
            provider: session.provider.clone(),
            model: session.model.clone(),
            provider_base_url: session.provider_base_url.clone(),
            provider_api_key: session.provider_api_key.clone(),
        };
        ensure_hidden_worker_target_ready(state, &target, needs_vision_tools).await?;
        return Ok(target);
    }

    let workspace = state.store.workspace()?;
    let profile = resolve_hidden_worker_profile(&workspace, session).ok_or_else(|| {
        let requested = if session.profile_id.trim().is_empty() {
            workspace.default_profile_id.as_str()
        } else {
            session.profile_id.as_str()
        };
        ApiError::bad_request(format!(
            "Utility Worker route is not configured: workspace profile '{requested}' was not found"
        ))
    })?;
    let target = HiddenWorkerTarget {
        provider: profile.utility.adapter.clone(),
        model: profile.utility.model.clone(),
        provider_base_url: profile.utility.base_url.clone(),
        provider_api_key: profile.utility.api_key.clone(),
    };
    ensure_hidden_worker_target_ready(state, &target, needs_vision_tools).await?;
    Ok(target)
}

#[derive(Clone)]
struct HiddenWorkerTargetCandidate {
    target: HiddenWorkerTarget,
    runtime_ready: bool,
}

fn select_hidden_worker_target(
    targets: Vec<HiddenWorkerTargetCandidate>,
    needs_vision_tools: bool,
) -> Option<HiddenWorkerTarget> {
    if needs_vision_tools {
        let ready_vision_target = targets
            .iter()
            .filter(|candidate| candidate.runtime_ready)
            .map(|candidate| &candidate.target)
            .find(|target| target_supports_vision_with_tools(target))
            .cloned();
        if let Some(target) = ready_vision_target {
            return Some(target);
        }
    }

    targets.into_iter().next().map(|candidate| candidate.target)
}

async fn ensure_hidden_worker_target_ready(
    state: &AppState,
    target: &HiddenWorkerTarget,
    needs_vision_tools: bool,
) -> Result<(), ApiError> {
    if needs_vision_tools && !target_supports_vision_with_tools(target) {
        return Ok(());
    }

    ensure_prompting_runtime(state, &target.provider, false).await
}

fn resolve_hidden_worker_profile<'a>(
    workspace: &'a WorkspaceSummary,
    session: &SessionSummary,
) -> Option<&'a WorkspaceProfileSummary> {
    let preferred_id = if session.profile_id.trim().is_empty() {
        workspace.default_profile_id.as_str()
    } else {
        session.profile_id.as_str()
    };
    workspace
        .profiles
        .iter()
        .find(|profile| profile.id == preferred_id)
}

fn normalize_playbook_title(value: &str) -> Result<String, ApiError> {
    let title = value.trim();
    if title.is_empty() {
        return Err(ApiError::bad_request("playbook title is required"));
    }
    Ok(title.to_string())
}

fn normalize_playbook_description(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

fn normalize_playbook_prompt(value: &str) -> Result<String, ApiError> {
    let prompt = value.trim();
    if prompt.is_empty() {
        return Err(ApiError::bad_request("playbook prompt is required"));
    }
    Ok(prompt.to_string())
}

fn normalize_playbook_policy_bundle(value: &str) -> Result<String, ApiError> {
    let bundle = value.trim();
    match bundle {
        "read_only" | "repo_mutation" | "command_runner" | "full_agent" => Ok(bundle.to_string()),
        _ => Err(ApiError::bad_request(format!(
            "unknown playbook policy bundle '{}'",
            value
        ))),
    }
}

fn normalize_playbook_trigger(
    trigger_kind: &str,
    schedule_interval_secs: Option<u64>,
    event_kind: Option<&str>,
) -> Result<(String, Option<u64>, Option<String>), ApiError> {
    match trigger_kind.trim() {
        "manual" => Ok(("manual".to_string(), None, None)),
        "schedule" => {
            let interval = schedule_interval_secs.ok_or_else(|| {
                ApiError::bad_request("scheduled playbooks require schedule_interval_secs")
            })?;
            if !(PLAYBOOK_MIN_INTERVAL_SECS..=PLAYBOOK_MAX_INTERVAL_SECS).contains(&interval) {
                return Err(ApiError::bad_request(format!(
                    "schedule_interval_secs must be between {} and {} seconds",
                    PLAYBOOK_MIN_INTERVAL_SECS, PLAYBOOK_MAX_INTERVAL_SECS
                )));
            }
            Ok(("schedule".to_string(), Some(interval), None))
        }
        "event" => {
            let event_kind = match event_kind.map(str::trim).filter(|value| !value.is_empty()) {
                Some("daemon_started") => "daemon_started".to_string(),
                Some("workspace_projects_synced") => "workspace_projects_synced".to_string(),
                Some(other) => {
                    return Err(ApiError::bad_request(format!(
                        "unknown playbook event trigger '{}'",
                        other
                    )));
                }
                None => {
                    return Err(ApiError::bad_request(
                        "event-triggered playbooks require event_kind",
                    ));
                }
            };
            Ok(("event".to_string(), None, Some(event_kind)))
        }
        other => Err(ApiError::bad_request(format!(
            "unknown playbook trigger kind '{}'",
            other
        ))),
    }
}

async fn create_playbook_session(
    state: &AppState,
    session_id: &str,
    title: &str,
    profile_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<SessionDetail, ApiError> {
    let workspace = state.store.workspace()?;
    let profile = match profile_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(profile_id) => resolve_workspace_profile(&workspace, profile_id)?,
        None => resolve_workspace_profile(&workspace, &workspace.default_profile_id)?,
    };
    let target = resolve_workspace_profile_target(state, profile, "main").await?;
    let projects =
        resolve_session_projects(state, project_id, project_id, None, Some(session_id), None)?;

    state.store.create_session(SessionRecord {
        id: session_id.to_string(),
        profile_id: target.profile_id,
        profile_title: target.profile_title,
        route_id: target.route_id,
        route_title: target.route_title,
        scope: "automation".to_string(),
        project_id: projects.primary_project_id.clone(),
        project_title: projects.primary_project_title.clone(),
        project_path: projects.primary_project_path.clone(),
        project_ids: projects.project_ids.clone(),
        title: format!("Playbook {}", title),
        provider: target.provider,
        model: target.model,
        provider_base_url: target.provider_base_url,
        provider_api_key: target.provider_api_key,
        working_dir: projects.working_dir,
        working_dir_kind: projects.working_dir_kind,
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
    })?;

    Ok(state.store.get_session(session_id)?)
}

async fn update_playbook_session(
    state: &AppState,
    session: &SessionSummary,
    title: &str,
    profile_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<SessionDetail, ApiError> {
    let workspace = state.store.workspace()?;
    let profile = match profile_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(profile_id) => resolve_workspace_profile(&workspace, profile_id)?,
        None => resolve_workspace_profile(&workspace, &workspace.default_profile_id)?,
    };
    let target = resolve_workspace_profile_target(state, profile, "main").await?;
    let projects = resolve_session_projects(
        state,
        project_id,
        project_id,
        None,
        Some(&session.id),
        Some(session),
    )?;

    state.store.update_session(
        &session.id,
        SessionPatch {
            title: Some(format!("Playbook {}", title)),
            profile_id: Some(target.profile_id),
            profile_title: Some(target.profile_title),
            route_id: Some(target.route_id),
            route_title: Some(target.route_title),
            scope: Some("automation".to_string()),
            project_id: Some(projects.primary_project_id),
            project_title: Some(projects.primary_project_title),
            project_path: Some(projects.primary_project_path),
            project_ids: Some(projects.project_ids),
            provider: Some(target.provider),
            model: Some(target.model),
            provider_base_url: Some(target.provider_base_url),
            provider_api_key: Some(target.provider_api_key),
            working_dir: Some(projects.working_dir),
            working_dir_kind: Some(projects.working_dir_kind),
            provider_session_id: Some(String::new()),
            last_error: Some(String::new()),
            ..SessionPatch::default()
        },
    )?;

    Ok(state.store.get_session(&session.id)?)
}

fn ensure_no_active_playbook_jobs(state: &AppState, playbook_id: &str) -> Result<(), ApiError> {
    let active = state.store.list_jobs_for_template_by_state(
        playbook_id,
        &["queued", "running", "paused", "waiting"],
    )?;
    if let Some(job) = active.first() {
        return Err(ApiError::bad_request(format!(
            "playbook '{}' already has an active job ({})",
            playbook_id, job.id
        )));
    }
    Ok(())
}

fn read_playbook_prompt(state: &AppState, playbook_id: &str) -> Result<String, ApiError> {
    Ok(state.store.get_playbook(playbook_id)?.prompt)
}

async fn run_scheduled_playbooks(state: &AppState) -> Result<()> {
    let now = unix_timestamp();
    for playbook in state.store.list_playbooks()? {
        if !playbook.enabled || playbook.trigger_kind != "schedule" {
            continue;
        }

        if state
            .store
            .list_jobs_for_template_by_state(
                &playbook.id,
                &["queued", "running", "paused", "waiting"],
            )?
            .is_empty()
        {
            let latest_scheduled = state
                .store
                .list_jobs_for_template(&playbook.id, 20)?
                .into_iter()
                .find(|job| job.trigger_kind == "playbook_schedule");
            let should_run = latest_scheduled.map_or(true, |job| {
                now.saturating_sub(job.created_at)
                    >= playbook.schedule_interval_secs.unwrap_or(0) as i64
            });
            if should_run {
                if let Err(error) =
                    queue_playbook_job(state, &playbook.id, "playbook_schedule", "system").await
                {
                    let _ = try_record_audit_event(
                        state,
                        AuditEventRecord {
                            kind: "playbook.schedule.failed".to_string(),
                            target: format!("playbook:{}", playbook.id),
                            status: "warning".to_string(),
                            summary: format!(
                                "Scheduled playbook '{}' did not start.",
                                playbook.title
                            ),
                            detail: error.message,
                        },
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

async fn dispatch_playbook_event_inner(state: &AppState, event_kind: &str) -> Result<()> {
    for playbook in state.store.list_playbooks()? {
        if !playbook.enabled || playbook.trigger_kind != "event" {
            continue;
        }
        if playbook.event_kind.as_deref() != Some(event_kind) {
            continue;
        }
        if !state
            .store
            .list_jobs_for_template_by_state(
                &playbook.id,
                &["queued", "running", "paused", "waiting"],
            )?
            .is_empty()
        {
            continue;
        }

        if let Err(error) =
            queue_playbook_job(state, &playbook.id, "playbook_event", "system").await
        {
            let _ = try_record_audit_event(
                state,
                AuditEventRecord {
                    kind: "playbook.event.failed".to_string(),
                    target: format!("playbook:{}", playbook.id),
                    status: "warning".to_string(),
                    summary: format!("Event playbook '{}' did not start.", playbook.title),
                    detail: error.message,
                },
            )
            .await;
        }
    }
    Ok(())
}

fn worker_read_roots(session: &SessionSummary) -> Vec<String> {
    let mut roots = Vec::new();
    push_unique_root(&mut roots, &session.working_dir);
    for project in &session.projects {
        push_unique_root(&mut roots, &project.absolute_path);
    }
    roots
}

fn worker_write_roots(session: &SessionSummary) -> Vec<String> {
    if session.working_dir_kind == "managed_git_worktree"
        || session.workspace_mode == "isolated_worktree"
        || session.projects.is_empty()
    {
        return worker_session_root(session);
    }
    worker_read_roots(session)
}

fn worker_session_root(session: &SessionSummary) -> Vec<String> {
    let mut roots = Vec::new();
    push_unique_root(&mut roots, &session.working_dir);
    roots
}

fn push_unique_root(roots: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if !trimmed.is_empty() && !roots.iter().any(|root| root == trimmed) {
        roots.push(trimmed.to_string());
    }
}

fn root_worker_capabilities() -> Vec<ToolCapabilityGrantRecord> {
    capabilities_for_policy_bundle("full_agent")
}

fn capabilities_for_policy_bundle(bundle: &str) -> Vec<ToolCapabilityGrantRecord> {
    match bundle {
        "read_only" => read_only_capabilities(),
        "repo_mutation" => {
            let mut capabilities = read_only_capabilities();
            capabilities.extend(mutating_capabilities());
            capabilities
        }
        "command_runner" => {
            let mut capabilities = read_only_capabilities();
            capabilities.extend(execution_capabilities());
            capabilities.extend(browser_capabilities());
            capabilities
        }
        _ => {
            let mut capabilities = read_only_capabilities();
            capabilities.extend(mutating_capabilities());
            capabilities.extend(execution_capabilities());
            capabilities.extend(browser_capabilities());
            capabilities
        }
    }
}

async fn queue_playbook_job(
    state: &AppState,
    playbook_id: &str,
    trigger_kind: &str,
    requested_by: &str,
) -> Result<JobDetail, ApiError> {
    ensure_no_active_playbook_jobs(state, playbook_id)?;

    let playbook = state.store.get_playbook(playbook_id)?;
    let session_id = playbook.session.id.clone();
    let prompt_excerpt = excerpt(&playbook.prompt, 160);
    let job_id = Uuid::new_v4().to_string();
    let root_worker_id = Uuid::new_v4().to_string();
    let target = resolve_hidden_worker_target(state, &playbook.session, "utility", false).await?;
    let worker_capabilities = capabilities_for_policy_bundle(&playbook.playbook.policy_bundle);
    let browser_tools_granted = worker_capabilities
        .iter()
        .any(|capability| capability.tool_id.starts_with("browser."));
    let ui_renderable = classify_prompt_ui_renderable(&playbook.prompt, 0);

    state.store.update_session(
        &session_id,
        SessionPatch {
            state: Some("running".to_string()),
            last_error: Some(String::new()),
            ..SessionPatch::default()
        },
    )?;
    state.store.append_session_turn(
        &session_id,
        &Uuid::new_v4().to_string(),
        "user",
        playbook.prompt.as_str(),
        &[],
    )?;

    let _job = state.store.create_job(JobRecord {
        id: job_id.clone(),
        session_id: Some(session_id.clone()),
        parent_job_id: None,
        template_id: Some(playbook.playbook.id.clone()),
        title: format!("Playbook {}", playbook.playbook.title),
        purpose: if playbook.playbook.description.is_empty() {
            playbook.playbook.title.clone()
        } else {
            playbook.playbook.description.clone()
        },
        trigger_kind: trigger_kind.to_string(),
        state: "queued".to_string(),
        requested_by: requested_by.to_string(),
        prompt_excerpt: prompt_excerpt.clone(),
        publication_intent_text: Some(playbook.prompt.clone()),
    })?;
    let job = state.store.update_job(
        &job_id,
        browser_verification_initial_patch(
            &ui_renderable,
            crate::browser::BrowserRuntime::availability_error(),
            browser_tools_granted,
        ),
    )?;
    if job.publication_requested {
        record_publication_git_hygiene_baseline(state, &job, &playbook.session.working_dir)?;
    }

    let _created_worker = state.store.create_worker(WorkerRecord {
        id: root_worker_id.clone(),
        job_id: job_id.clone(),
        parent_worker_id: None,
        title: "Utility automation worker".to_string(),
        lane: "utility".to_string(),
        state: "queued".to_string(),
        provider: target.provider.clone(),
        model: target.model.clone(),
        provider_base_url: target.provider_base_url.clone(),
        provider_api_key: target.provider_api_key.clone(),
        provider_session_id: String::new(),
        working_dir: playbook.session.working_dir.clone(),
        read_roots: worker_read_roots(&playbook.session),
        write_roots: worker_write_roots(&playbook.session),
        max_steps: playbook.session.run_budget.max_steps,
        max_tool_calls: playbook.session.run_budget.max_tool_calls,
        max_wall_clock_secs: playbook.session.run_budget.max_wall_clock_secs,
    })?;
    state.store.update_job(
        &job_id,
        JobPatch {
            root_worker_id: Some(root_worker_id.clone()),
            ..JobPatch::default()
        },
    )?;
    state
        .store
        .replace_tool_capability_grants(&root_worker_id, &worker_capabilities)?;
    let worker = state
        .store
        .get_job(&job_id)?
        .workers
        .into_iter()
        .find(|item| item.id == root_worker_id)
        .ok_or_else(|| ApiError::internal_message("failed to reload hidden automation worker"))?;

    let checkpoint = WorkerCheckpoint {
        session_id: session_id.clone(),
        prompt_text: playbook.prompt.clone(),
        images: Vec::new(),
        conversation: vec![CheckpointMessage {
            role: "system".to_string(),
            content: worker_system_prompt(&worker),
            images: Vec::new(),
            compacted: false,
            compacted_range: None,
        }],
        next_prompt: None,
        pending_action: None,
        browser_verification_final_answer_rejected: false,
        patch_loop_guardrail_triggered: false,
    };
    state
        .store
        .write_worker_checkpoint(&root_worker_id, &serde_json::to_value(checkpoint).unwrap())?;

    if let Ok(updated) = state.store.get_session(&session_id) {
        let _ = publish_session_event(state, updated).await;
    }
    publish_job_created(state, &state.store.get_job(&job_id)?.job).await;
    publish_worker_updated(state, &worker).await;
    let _ = publish_overview_event(state).await;
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: "playbook.job.created".to_string(),
            target: format!("job:{job_id}"),
            status: "success".to_string(),
            summary: format!("Queued playbook '{}' for execution.", playbook.playbook.title),
            detail: format!(
                "playbook_id={} session_id={} trigger_kind={} requested_by={} utility_provider={} utility_model={}",
                playbook.playbook.id,
                session_id,
                trigger_kind,
                requested_by,
                target.provider,
                target.model
            ),
        },
    )
    .await;

    spawn_job_task(state.clone(), job_id.clone());
    Ok(state.store.get_job(&job_id)?)
}

fn child_worker_capabilities() -> Vec<ToolCapabilityGrantRecord> {
    read_only_capabilities()
}

fn read_only_capabilities() -> Vec<ToolCapabilityGrantRecord> {
    vec![
        ToolCapabilityGrantRecord {
            tool_id: "project.inspect".to_string(),
            summary: "Inspect the active workspace and repo status.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "none".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "repo-read".to_string(),
            scope_kind: "workspace".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "fs.list".to_string(),
            summary: "List files or directories inside the allowed read scope.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "none".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "fs-read".to_string(),
            scope_kind: "path".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "fs.read_text".to_string(),
            summary: "Read a UTF-8 text file inside the allowed read scope.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "none".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "fs-read".to_string(),
            scope_kind: "path".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "rg.search".to_string(),
            summary: "Search the repo with ripgrep inside the allowed read scope.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "none".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "repo-read".to_string(),
            scope_kind: "path".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "git.status".to_string(),
            summary: "Read the current git status for the active working tree.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "none".to_string(),
            timeout_secs: 20,
            max_output_bytes: 16_384,
            supports_streaming: false,
            concurrency_group: "git-read".to_string(),
            scope_kind: "repo".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "git.diff".to_string(),
            summary: "Read the current git diff for the active working tree.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "none".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "git-read".to_string(),
            scope_kind: "repo".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "github.pr_review_threads".to_string(),
            summary: "Fetch thread-aware GitHub pull request review data, including inline review threads, unresolved and outdated state, paths, lines, and comment bodies.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "network".to_string(),
            timeout_secs: 30,
            max_output_bytes: 65_536,
            supports_streaming: false,
            concurrency_group: "github-read".to_string(),
            scope_kind: "repo".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "github.pr_state".to_string(),
            summary: "Fetch direct GitHub pull request lifecycle state, including state, mergedAt, mergeability, head branch, base branch, and URL.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "network".to_string(),
            timeout_secs: 30,
            max_output_bytes: 16_384,
            supports_streaming: false,
            concurrency_group: "github-read".to_string(),
            scope_kind: "repo".to_string(),
        },
    ]
}

fn mutating_capabilities() -> Vec<ToolCapabilityGrantRecord> {
    vec![
        ToolCapabilityGrantRecord {
            tool_id: "fs.apply_patch".to_string(),
            summary: "Apply scoped find-and-replace edits to a UTF-8 text file.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "write".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "fs-write".to_string(),
            scope_kind: "path".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "fs.write_text".to_string(),
            summary: "Create or overwrite a UTF-8 text file inside the write scope.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "write".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "fs-write".to_string(),
            scope_kind: "path".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "fs.move".to_string(),
            summary: "Move or rename a file or directory inside the write scope.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "write".to_string(),
            timeout_secs: 20,
            max_output_bytes: 16_384,
            supports_streaming: false,
            concurrency_group: "fs-write".to_string(),
            scope_kind: "path".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "fs.mkdir".to_string(),
            summary: "Create a directory inside the write scope.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "write".to_string(),
            timeout_secs: 20,
            max_output_bytes: 8_192,
            supports_streaming: false,
            concurrency_group: "fs-write".to_string(),
            scope_kind: "path".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "git.stage_patch".to_string(),
            summary: "Stage current working tree changes for selected paths.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "repo".to_string(),
            timeout_secs: 20,
            max_output_bytes: 16_384,
            supports_streaming: false,
            concurrency_group: "git-write".to_string(),
            scope_kind: "repo".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "github.comment".to_string(),
            summary: "Post a GitHub issue or PR comment using argv and a temporary body file so shell metacharacters in the body are not interpreted.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "external".to_string(),
            timeout_secs: 30,
            max_output_bytes: 16_384,
            supports_streaming: false,
            concurrency_group: "github-write".to_string(),
            scope_kind: "repo".to_string(),
        },
    ]
}

fn execution_capabilities() -> Vec<ToolCapabilityGrantRecord> {
    vec![
        ToolCapabilityGrantRecord {
            tool_id: "command.run".to_string(),
            summary: "Run a bounded Nucleus-owned command and capture logs as artifacts."
                .to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "high".to_string(),
            side_effect_level: "process".to_string(),
            timeout_secs: COMMAND_DEFAULT_TIMEOUT_SECS,
            max_output_bytes: COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES,
            supports_streaming: false,
            concurrency_group: "process".to_string(),
            scope_kind: "process".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "command.session.open".to_string(),
            summary: "Open a bounded interactive command session owned by Nucleus.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "high".to_string(),
            side_effect_level: "process".to_string(),
            timeout_secs: COMMAND_DEFAULT_TIMEOUT_SECS,
            max_output_bytes: COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES,
            supports_streaming: true,
            concurrency_group: "process".to_string(),
            scope_kind: "process".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "command.session.write".to_string(),
            summary: "Send input to an approved Nucleus-owned command session.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "process".to_string(),
            timeout_secs: 30,
            max_output_bytes: COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES,
            supports_streaming: true,
            concurrency_group: "process".to_string(),
            scope_kind: "process".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "command.session.close".to_string(),
            summary: "Close an approved Nucleus-owned command session.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "process".to_string(),
            timeout_secs: 30,
            max_output_bytes: COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES,
            supports_streaming: false,
            concurrency_group: "process".to_string(),
            scope_kind: "process".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "tests.run".to_string(),
            summary: "Run a bounded test or build command and capture logs as artifacts."
                .to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "high".to_string(),
            side_effect_level: "process".to_string(),
            timeout_secs: COMMAND_DEFAULT_TIMEOUT_SECS,
            max_output_bytes: COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES,
            supports_streaming: false,
            concurrency_group: "process".to_string(),
            scope_kind: "process".to_string(),
        },
    ]
}

fn browser_capabilities() -> Vec<ToolCapabilityGrantRecord> {
    vec![
        ToolCapabilityGrantRecord {
            tool_id: "browser.context".to_string(),
            summary: "List session Browser pages, active URL, titles, and page ids.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "none".to_string(),
            timeout_secs: 20,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.navigate".to_string(),
            summary: "Navigate the session Browser to a URL.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "network".to_string(),
            timeout_secs: 45,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.snapshot".to_string(),
            summary:
                "Read page text and Nucleus-generated actionable refs; stores snapshot evidence."
                    .to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "artifact".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.screenshot".to_string(),
            summary: "Capture a Browser screenshot as a session/job artifact.".to_string(),
            approval_mode: "auto".to_string(),
            risk_level: "low".to_string(),
            side_effect_level: "artifact".to_string(),
            timeout_secs: 30,
            max_output_bytes: 16_384,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.click".to_string(),
            summary: "Click a Browser element by ref or coordinate.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "browser".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.type".to_string(),
            summary: "Type text into a Browser field by ref.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "browser".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.fill".to_string(),
            summary: "Fill a Browser input by ref.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "browser".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.scroll".to_string(),
            summary: "Scroll the Browser page or scroll an element ref into view.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "browser".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.press".to_string(),
            summary: "Press a key in the Browser, optionally targeted at a ref.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "browser".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
        ToolCapabilityGrantRecord {
            tool_id: "browser.submit".to_string(),
            summary: "Submit a Browser form or input by ref.".to_string(),
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "browser".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "browser".to_string(),
            scope_kind: "browser".to_string(),
        },
    ]
}

fn mcp_tool_capabilities(state: &AppState) -> Vec<ToolCapabilityGrantRecord> {
    let Ok(servers) = state.store.list_mcp_servers() else {
        return Vec::new();
    };
    let enabled_servers = servers
        .into_iter()
        .filter(|server| server.enabled)
        .map(|server| server.id)
        .collect::<BTreeSet<_>>();
    if enabled_servers.is_empty() {
        return Vec::new();
    }

    state
        .store
        .list_mcp_tools()
        .unwrap_or_default()
        .into_iter()
        .filter(|tool| enabled_servers.contains(&tool.server_id))
        .map(|tool| ToolCapabilityGrantRecord {
            tool_id: tool.id,
            summary: if tool.description.trim().is_empty() {
                format!("Invoke MCP tool {} via Nucleus.", tool.name)
            } else {
                tool.description
            },
            approval_mode: "explicit".to_string(),
            risk_level: "medium".to_string(),
            side_effect_level: "external".to_string(),
            timeout_secs: 30,
            max_output_bytes: 32_768,
            supports_streaming: false,
            concurrency_group: "mcp".to_string(),
            scope_kind: "mcp".to_string(),
        })
        .collect()
}

fn worker_system_prompt(worker: &WorkerSummary) -> String {
    worker_system_prompt_with_mode(worker, "act")
}

fn worker_system_prompt_with_mode(worker: &WorkerSummary, execution_mode: &str) -> String {
    if execution_mode == "plan" {
        return format!(
            "You are the Utility Nucleus {} worker for a Nucleus-owned job.\n\
Return exactly one JSON object and nothing else.\n\
Plan mode is enabled for this session.\n\
Allowed response shape:\n\
{{\"kind\":\"final_answer\",\"summary\":\"what the plan covers\",\"final_answer\":\"concise user-facing plan\"}}\n\
Rules:\n\
- Do not call tools.\n\
- Do not spawn Utility Subworkers.\n\
- Do not run commands, inspect files, edit files, or assume action results.\n\
- You may reason from the user's prompt and existing visible context only.\n\
- The visible chat will receive final_answer.\n\
- Do not wrap JSON in markdown fences.\n\
Available tools: disabled in Plan mode.\n\
Worker lane: {}\n\
Working directory: {}\n",
            worker.lane, worker.lane, worker.working_dir
        );
    }

    let is_root_worker = worker.parent_worker_id.is_none();
    let action_shapes = if is_root_worker {
        "{\"kind\":\"tool_call\",\"summary\":\"inspect the active project\",\"tool\":\"project.inspect\",\"args\":{}}\n\
{\"kind\":\"tool_call\",\"summary\":\"list likely project directories\",\"tool\":\"fs.list\",\"args\":{\"path\":\".\",\"recursive\":false,\"limit\":100}}\n\
{\"kind\":\"tool_call\",\"summary\":\"fetch inline PR review threads\",\"tool\":\"github.pr_review_threads\",\"args\":{\"pr_number\":123}}\n\
{\"kind\":\"tool_call\",\"summary\":\"fetch direct PR lifecycle state\",\"tool\":\"github.pr_state\",\"args\":{\"pr_number\":123}}\n\
{\"kind\":\"tool_call\",\"summary\":\"check running dev processes\",\"tool\":\"command.run\",\"args\":{\"command\":\"sh\",\"args\":[\"-lc\",\"ps -ef | grep -iE 'stfr|vite|next|webpack|dev server' | grep -v grep\"],\"cwd\":\".\",\"timeout_secs\":20}}\n\
{\"kind\":\"tool_call\",\"summary\":\"verify the UI in Browser\",\"tool\":\"browser.navigate\",\"args\":{\"url\":\"http://127.0.0.1:5299\"}}\n\
{\"kind\":\"tool_call\",\"summary\":\"read Browser refs\",\"tool\":\"browser.snapshot\",\"args\":{}}\n\
{\"kind\":\"tool_call\",\"summary\":\"click a Browser control by ref\",\"tool\":\"browser.click\",\"args\":{\"target_ref\":\"ref-1\"}}\n\
{\"kind\":\"spawn_child_jobs\",\"summary\":\"why parallel exploration helps\",\"jobs\":[{\"title\":\"focused subtask\",\"prompt\":\"precise child prompt\",\"working_dir\":\"optional/path/inside/scope\"}]}\n\
{\"kind\":\"progress_update\",\"summary\":\"durable checkpoint, not done\",\"detail\":\"completed evidence and exact continuation point\"}\n\
{\"kind\":\"wait\",\"summary\":\"park until an external condition is ready\",\"until\":{\"kind\":\"delay_seconds\",\"delay_seconds\":60},\"max_wait_seconds\":1800,\"wake_note\":\"optional wake-up context\"}\n\
{\"kind\":\"wait\",\"summary\":\"park until memory classification finishes\",\"until\":{\"kind\":\"audit_event\",\"event_kind\":\"memory.classifier.completed\",\"target_pattern\":\"session:\",\"status\":\"success\"},\"max_wait_seconds\":1800}\n\
{\"kind\":\"final_answer\",\"summary\":\"why the work is done\",\"final_answer\":\"clean user-facing answer\",\"browser_verification\":{\"status\":\"passed|failed|not_performed|unavailable\",\"summary\":\"concise Browser verification result\",\"artifact_ids\":[\"artifact-id\"]}}"
    } else {
        "{\"kind\":\"tool_call\",\"summary\":\"inspect the active project\",\"tool\":\"project.inspect\",\"args\":{}}\n\
{\"kind\":\"tool_call\",\"summary\":\"list likely project directories\",\"tool\":\"fs.list\",\"args\":{\"path\":\".\",\"recursive\":false,\"limit\":100}}\n\
{\"kind\":\"tool_call\",\"summary\":\"fetch inline PR review threads\",\"tool\":\"github.pr_review_threads\",\"args\":{\"pr_number\":123}}\n\
{\"kind\":\"tool_call\",\"summary\":\"fetch direct PR lifecycle state\",\"tool\":\"github.pr_state\",\"args\":{\"pr_number\":123}}\n\
{\"kind\":\"progress_update\",\"summary\":\"durable checkpoint, not done\",\"detail\":\"completed evidence and exact continuation point\"}\n\
{\"kind\":\"wait\",\"summary\":\"park until an external condition is ready\",\"until\":{\"kind\":\"delay_seconds\",\"delay_seconds\":60},\"max_wait_seconds\":1800,\"wake_note\":\"optional wake-up context\"}\n\
{\"kind\":\"wait\",\"summary\":\"park until a child report exists\",\"until\":{\"kind\":\"artifact_kind\",\"job_id\":\"job-id\",\"artifact_kind\":\"child-report\"},\"max_wait_seconds\":1800}\n\
{\"kind\":\"final_answer\",\"summary\":\"why the work is done\",\"final_answer\":\"clean user-facing answer\"}"
    };
    let tool_help = worker
        .capabilities
        .iter()
        .map(|capability| {
            format!(
                "- {}: {} (approval={}, risk={})",
                capability.tool_id,
                capability.summary,
                capability.approval_mode,
                capability.risk_level
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let child_job_rules = if is_root_worker {
        "- Only root workers may fan out child jobs, and child jobs must stay read-only.\n\
- Use at most 5 child jobs in a single spawn_child_jobs action.\n"
    } else {
        ""
    };

    format!(
        "You are the Utility Nucleus {} worker for a Nucleus-owned job.\n\
Return exactly one JSON object and nothing else.\n\
Allowed response shapes:\n\
{}\n\
Rules:\n\
- Choose and execute the smallest useful next action.\n\
- Use tools only when they materially improve the answer.\n\
- Never invent tool output.\n\
- Stay inside the granted repo scope.\n\
{}\
- For UI or visual changes, strongly prefer Browser verification before final_answer when Browser tools are available: navigate to the matched local app, inspect refs/snapshot or screenshot, interact as needed, and cite artifact/result evidence.\n\
- For UI-renderable work, include browser_verification in final_answer with status passed, failed, not_performed, or unavailable. Typecheck/build success alone is not rendered-UI verification.\n\
- Prefer daemon-generated Browser refs for browser.click/browser.type/browser.fill/browser.scroll/browser.press/browser.submit. Do not invent selectors when a ref is available.\n\
- The visible chat will only receive final_answer, not your intermediate reasoning.\n\
- progress_update records a non-terminal checkpoint for Nucleus; it does not complete the job.\n\
- wait parks the worker until delay_seconds, absolute_unix, audit_event, child_jobs_completed, or artifact_kind is satisfied. It does not spend step/action budget but still spends wall-clock budget.\n\
- Do not put plans, next-step instructions, progress updates, partial completion notes, or descriptions of future actions in final_answer.\n\
- If the requested work is incomplete and you are not blocked or out of budget, continue with a tool_call instead of returning final_answer.\n\
- For PR feedback, latest review, requested changes, Codex review, or unresolved comment tasks, do not rely on flat gh pr view comments alone. Fetch thread-aware inline review data with github.pr_review_threads before saying there is no actionable feedback.\n\
- For PR lifecycle claims like merged, open, closed, ready, mergeable, or approved, fetch direct GitHub PR state with github.pr_state. Local git state can supplement, but it is never enough to say a PR is already merged or that nothing is left to merge. Only say that when direct PR state is MERGED or mergedAt is non-empty. If PR identity is unclear, resolve it from the session branch or ask a bounded follow-up.\n\
- If a user challenges or repeats a prior clean/no-action answer, treat it as a grounding failure signal and use a deeper or different evidence path before repeating the conclusion.\n\
- Treat zero matched tests as no tests matched, not validation success.\n\
- When posting GitHub comments, use github.comment or a body file/stdin path. Do not put comment bodies with backticks or shell metacharacters inside sh -lc strings.\n\
- Use final_answer only as the terminal completion action when the requested task is complete and validated, or when you are genuinely blocked.\n\
- Do not use provider-native tool wrappers such as tool_call/tool_name/shell; use the exact Nucleus JSON shapes above.\n\
- Do not wrap JSON in markdown fences.\n\
Available tools:\n{}\n\
Worker lane: {}\n\
Working directory: {}\n",
        worker.lane, action_shapes, child_job_rules, tool_help, worker.lane, worker.working_dir
    )
}

fn command_path_env() -> Option<std::ffi::OsString> {
    const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();

    let current = env::var_os("PATH")
        .filter(|value| !value.is_empty())
        .or_else(|| Some(FALLBACK_PATH.into()));

    if let Some(current) = current {
        for path in env::split_paths(&current) {
            if !path.as_os_str().is_empty() && seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }

    if let Some(home) = dirs::home_dir() {
        for suffix in [".local/bin", ".cargo/bin", ".bun/bin", "bin"] {
            let path = home.join(suffix);
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }

    if paths.is_empty() {
        return None;
    }

    env::join_paths(paths).ok()
}

async fn publish_job_created(state: &AppState, summary: &JobSummary) {
    let _ = state
        .events
        .send(DaemonEvent::JobCreated(publishable_job_summary(summary)));
    let _ = record_job_log(state, "info", "job.created", summary).await;
}

async fn publish_job_updated(state: &AppState, summary: &JobSummary) {
    let _ = state
        .events
        .send(DaemonEvent::JobUpdated(publishable_job_summary(summary)));
}

async fn publish_job_failed(state: &AppState, summary: &JobSummary) {
    let _ = state
        .events
        .send(DaemonEvent::JobFailed(publishable_job_summary(summary)));
    let _ = record_job_log(state, "error", "job.failed", summary).await;
}

async fn publish_job_completed(state: &AppState, summary: &JobSummary) {
    let _ = state
        .events
        .send(DaemonEvent::JobCompleted(publishable_job_summary(summary)));
    let _ = record_job_log(state, "info", "job.completed", summary).await;
}

async fn record_job_log(
    state: &AppState,
    level: &str,
    event: &str,
    summary: &JobSummary,
) -> Option<nucleus_protocol::InstanceLogEntry> {
    record_instance_log(
        state,
        level,
        "job",
        "agent",
        event,
        format!("{}: {}", summary.title, summary.state),
        json!({
            "job_id": summary.id,
            "session_id": summary.session_id,
            "parent_job_id": summary.parent_job_id,
        }),
        json!({
            "trigger_kind": summary.trigger_kind,
            "requested_by": summary.requested_by,
        }),
    )
    .await
}

async fn publish_worker_updated(state: &AppState, summary: &WorkerSummary) {
    let _ = state
        .events
        .send(DaemonEvent::WorkerUpdated(publishable_worker_summary(
            summary,
        )));
}

fn publishable_job_summary(summary: &JobSummary) -> JobSummary {
    let mut summary = summary.clone();
    let redactor = security::RedactionSet::new();
    summary.last_error = redactor.redact_text(&summary.last_error);
    summary.user_error = error_display::classify_user_error(&summary.last_error);
    summary
}

fn publishable_worker_summary(summary: &WorkerSummary) -> WorkerSummary {
    let mut summary = summary.clone();
    let redactor = security::RedactionSet::new().with_secret(summary.provider_api_key.clone());
    summary.provider_api_key.clear();
    summary.last_error = redactor.redact_text(&summary.last_error);
    summary.user_error = error_display::classify_user_error(&summary.last_error);
    summary
}

async fn publish_approval_requested(state: &AppState, summary: &ApprovalRequestSummary) {
    let _ = state
        .events
        .send(DaemonEvent::ApprovalRequested(summary.clone()));
}

async fn publish_approval_resolved(state: &AppState, summary: &ApprovalRequestSummary) {
    let _ = state
        .events
        .send(DaemonEvent::ApprovalResolved(summary.clone()));
}

async fn publish_artifact_added(state: &AppState, summary: &ArtifactSummary) {
    let _ = state
        .events
        .send(DaemonEvent::ArtifactAdded(summary.clone()));
}

async fn publish_command_session_updated(state: &AppState, summary: &CommandSessionSummary) {
    let _ = state
        .events
        .send(DaemonEvent::CommandSessionUpdated(summary.clone()));
}

async fn publish_prompt_status(
    state: &AppState,
    session: &SessionSummary,
    worker: &WorkerSummary,
    status: &str,
    label: &str,
    detail: &str,
    memory_outcomes: &[MemoryOutcome],
) {
    let _ = publish_prompt_progress_event(
        state,
        PromptProgressUpdate {
            session_id: session.id.clone(),
            status: status.to_string(),
            label: label.to_string(),
            detail: detail.to_string(),
            provider: worker.provider.clone(),
            model: worker.model.clone(),
            profile_id: session.profile_id.clone(),
            profile_title: session.profile_title.clone(),
            route_id: session.route_id.clone(),
            route_title: session.route_title.clone(),
            attempt: 0,
            attempt_count: 0,
            memory_outcomes: memory_outcomes.to_vec(),
            created_at: unix_timestamp(),
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault;
    use nucleus_protocol::{SessionProjectSummary, WorkspaceModelConfig};

    #[test]
    fn interrupted_restart_recovery_only_rewrites_non_terminal_tool_calls() {
        for status in ["queued", "starting", "running"] {
            assert!(is_non_terminal_tool_call_status(status));
        }

        for status in ["completed", "failed", "canceled", "denied"] {
            assert!(!is_non_terminal_tool_call_status(status));
        }
    }

    #[test]
    fn reasoning_snapshots_accumulate_streamed_chunks() {
        let mut buffer = String::new();

        assert_eq!(
            append_reasoning_snapshot(&mut buffer, "checking "),
            "checking"
        );
        assert_eq!(
            append_reasoning_snapshot(&mut buffer, "the next result"),
            "checking the next result"
        );
    }

    use crate::{
        host::HostEngine,
        runtime::RuntimeManager,
        updates::{InstanceRuntime, UpdateManager},
    };
    use nucleus_storage::{
        JobArtifactRecord, JobRecord, SessionRecord, StateStore, ToolCallRecord, WorkerRecord,
    };
    use std::{
        env, fs,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex as TestMutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use tokio::sync::broadcast;

    #[test]
    fn apply_patch_edits_replaces_one_match() {
        let result = apply_patch_edits(
            "alpha\nbeta\n",
            &[PatchEditArgs {
                find: "beta".to_string(),
                replace: "gamma".to_string(),
                replace_all: Some(false),
            }],
        )
        .expect("patch edit should succeed");

        assert_eq!(result, "alpha\ngamma\n");
    }

    #[test]
    fn apply_patch_edits_rejects_ambiguous_single_replace() {
        let error = apply_patch_edits(
            "match\nmatch\n",
            &[PatchEditArgs {
                find: "match".to_string(),
                replace: "next".to_string(),
                replace_all: Some(false),
            }],
        )
        .expect_err("patch edit should reject ambiguous replacements");

        assert!(
            error.to_string().contains("matched multiple locations"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn worker_roots_use_worktree_for_isolated_session_writes() {
        let worktree = "/state/worktrees/nucleus/session";
        let source = "/home/eba/dev-projects/nucleus";
        let session = scope_test_session(
            worktree,
            "managed_git_worktree",
            "isolated_worktree",
            vec![scope_test_project("nucleus", source, true)],
        );

        assert_eq!(
            worker_read_roots(&session),
            vec![worktree.to_string(), source.to_string()]
        );
        assert_eq!(worker_write_roots(&session), vec![worktree.to_string()]);
    }

    #[test]
    fn worker_roots_keep_attached_projects_writable_for_shared_sessions() {
        let primary = "/home/eba/dev-projects/nucleus";
        let secondary = "/home/eba/dev-projects/other";
        let session = scope_test_session(
            primary,
            "project_root",
            "shared_project_root",
            vec![
                scope_test_project("nucleus", primary, true),
                scope_test_project("other", secondary, false),
            ],
        );

        assert_eq!(
            worker_read_roots(&session),
            vec![primary.to_string(), secondary.to_string()]
        );
        assert_eq!(
            worker_write_roots(&session),
            vec![primary.to_string(), secondary.to_string()]
        );
    }

    fn scope_test_project(
        id: &str,
        absolute_path: &str,
        is_primary: bool,
    ) -> SessionProjectSummary {
        SessionProjectSummary {
            id: id.to_string(),
            title: id.to_string(),
            slug: id.to_string(),
            relative_path: id.to_string(),
            absolute_path: absolute_path.to_string(),
            is_primary,
        }
    }

    fn scope_test_session(
        working_dir: &str,
        working_dir_kind: &str,
        workspace_mode: &str,
        projects: Vec<SessionProjectSummary>,
    ) -> SessionSummary {
        let primary = projects.iter().find(|project| project.is_primary);
        SessionSummary {
            id: "session".to_string(),
            title: "Session".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            project_id: primary
                .map(|project| project.id.clone())
                .unwrap_or_default(),
            project_title: primary
                .map(|project| project.title.clone())
                .unwrap_or_default(),
            project_path: primary
                .map(|project| project.absolute_path.clone())
                .unwrap_or_default(),
            provider: "openai_compatible".to_string(),
            model: "cx/gpt-5.4".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            working_dir: working_dir.to_string(),
            working_dir_kind: working_dir_kind.to_string(),
            workspace_mode: workspace_mode.to_string(),
            source_project_path: primary
                .map(|project| project.absolute_path.clone())
                .unwrap_or_default(),
            git_root: working_dir.to_string(),
            worktree_path: working_dir.to_string(),
            git_branch: String::new(),
            git_base_ref: String::new(),
            git_head: String::new(),
            git_dirty: false,
            git_untracked_count: 0,
            git_remote_tracking_branch: String::new(),
            workspace_warnings: Vec::new(),
            scope: if projects.len() > 1 {
                "multi_project".to_string()
            } else {
                "project".to_string()
            },
            approval_mode: "ask".to_string(),
            execution_mode: "act".to_string(),
            run_budget_mode: "standard".to_string(),
            run_budget: RunBudgetSummary::default(),
            project_count: projects.len(),
            projects,
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            last_resumed_at: None,
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn mutating_tools_require_approval() {
        assert_eq!(
            policy_for_tool("fs.write_text").decision,
            "require_approval"
        );
        assert_eq!(policy_for_tool("command.run").decision, "require_approval");
        assert_eq!(
            policy_for_tool("command.session.open").decision,
            "require_approval"
        );
        assert_eq!(policy_for_tool("command.session.write").decision, "allow");
        assert_eq!(policy_for_tool("fs.read_text").decision, "allow");
        assert_eq!(
            policy_for_tool("github.comment").decision,
            "require_approval"
        );
        assert_eq!(
            policy_for_tool("github.pr_review_threads").decision,
            "allow"
        );
        assert_eq!(policy_for_tool("github.pr_state").decision, "allow");
    }

    #[test]
    fn browser_tools_have_session_scoped_policy() {
        let read_policy = policy_for_tool("browser.snapshot");
        assert_eq!(read_policy.decision, "allow");
        assert_eq!(
            read_policy.matched_rule,
            "auto-browser-read:browser.snapshot"
        );
        assert_eq!(read_policy.scope_kind, "browser");
        assert_eq!(read_policy.risk_level, "low");

        let action_policy = policy_for_tool("browser.click");
        assert_eq!(action_policy.decision, "require_approval");
        assert_eq!(action_policy.matched_rule, "approval:browser:browser.click");
        assert_eq!(action_policy.scope_kind, "browser");
        assert_eq!(action_policy.risk_level, "medium");
    }

    #[test]
    fn trusted_session_approval_mode_allows_action_tools() {
        let command_policy = policy_for_tool_with_mode("command.run", "trusted");
        assert_eq!(command_policy.decision, "allow");
        assert_eq!(
            command_policy.matched_rule,
            "session-trusted-actions:command.run"
        );
        assert_eq!(command_policy.risk_level, "high");

        let mutation_policy = policy_for_tool_with_mode("fs.write_text", "trusted");
        assert_eq!(mutation_policy.decision, "allow");
        assert_eq!(
            mutation_policy.matched_rule,
            "session-trusted-actions:fs.write_text"
        );
        assert_eq!(mutation_policy.risk_level, "medium");
    }

    #[test]
    fn budget_guidance_is_added_on_final_available_step() {
        let worker = test_worker_summary("root", 10, 20);
        let prompt = add_budget_guidance("Return one action.".to_string(), &worker, 9, 2);

        assert!(prompt.contains("Budget note"));
        assert!(prompt.contains("Prefer final_answer now"));
    }

    #[test]
    fn budget_guidance_is_not_added_with_room_remaining() {
        let worker = test_worker_summary("root", 10, 20);
        let prompt = add_budget_guidance("Return one action.".to_string(), &worker, 4, 2);

        assert_eq!(prompt, "Return one action.");
    }

    #[test]
    fn budget_checkpoint_answer_includes_latest_checkpoint() {
        let worker = test_worker_summary("root", 10, 20);
        let session = SessionDetail {
            session: build_execution_session(&worker),
            turns: Vec::new(),
        };
        let checkpoint = WorkerCheckpoint {
            session_id: session.session.id.clone(),
            prompt_text: "do useful work".to_string(),
            images: Vec::new(),
            conversation: Vec::new(),
            next_prompt: Some(
                "Tool result: seed completed; sqlite3 command was missing.".to_string(),
            ),
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        };

        let answer = build_budget_checkpoint_answer(&session, &worker, &checkpoint, 10, 8, "step");

        assert!(answer.contains("reached the current step budget"));
        assert!(answer.contains("seed completed"));
        assert!(answer.contains("continue from the checkpoint"));
    }

    #[test]
    fn playbook_trigger_validation_rejects_invalid_inputs() {
        let (trigger_kind, schedule_interval_secs, event_kind) =
            normalize_playbook_trigger("schedule", Some(300), None)
                .expect("scheduled playbook should validate");
        assert_eq!(trigger_kind, "schedule");
        assert_eq!(schedule_interval_secs, Some(300));
        assert_eq!(event_kind, None);

        let error = normalize_playbook_trigger("schedule", Some(30), None)
            .expect_err("short schedule should be rejected");
        assert!(error.message.contains("between 60 and 86400"));

        let error = normalize_playbook_trigger("event", None, None)
            .expect_err("event playbook should require an event kind");
        assert!(error.message.contains("require event_kind"));

        let error = normalize_playbook_trigger("event", None, Some("push_received"))
            .expect_err("unknown event kind should be rejected");
        assert!(error.message.contains("unknown playbook event trigger"));
    }

    #[test]
    fn policy_bundles_select_expected_capabilities() {
        let read_only = capabilities_for_policy_bundle("read_only");
        assert!(
            read_only
                .iter()
                .any(|grant| grant.tool_id == "fs.read_text")
        );
        assert!(
            read_only
                .iter()
                .any(|grant| grant.tool_id == "github.pr_review_threads")
        );
        assert!(
            read_only
                .iter()
                .any(|grant| grant.tool_id == "github.pr_state")
        );
        assert!(
            !read_only
                .iter()
                .any(|grant| grant.tool_id == "fs.write_text")
        );
        assert!(!read_only.iter().any(|grant| grant.tool_id == "command.run"));

        let repo_mutation = capabilities_for_policy_bundle("repo_mutation");
        assert!(
            repo_mutation
                .iter()
                .any(|grant| grant.tool_id == "fs.write_text")
        );
        assert!(
            repo_mutation
                .iter()
                .any(|grant| grant.tool_id == "github.comment")
        );
        assert!(
            !repo_mutation
                .iter()
                .any(|grant| grant.tool_id == "command.run")
        );

        let command_runner = capabilities_for_policy_bundle("command_runner");
        assert!(
            !command_runner
                .iter()
                .any(|grant| grant.tool_id == "fs.write_text")
        );
        assert!(
            command_runner
                .iter()
                .any(|grant| grant.tool_id == "command.run")
        );
        assert!(
            command_runner
                .iter()
                .any(|grant| grant.tool_id == "browser.navigate")
        );

        let full_agent = capabilities_for_policy_bundle("full_agent");
        assert!(
            full_agent
                .iter()
                .any(|grant| grant.tool_id == "fs.write_text")
        );
        assert!(
            full_agent
                .iter()
                .any(|grant| grant.tool_id == "command.run")
        );
        assert!(
            full_agent
                .iter()
                .any(|grant| grant.tool_id == "browser.click")
        );
    }

    #[test]
    fn browser_capabilities_cover_ref_based_actions() {
        let ids = browser_capabilities()
            .into_iter()
            .map(|grant| {
                assert_eq!(grant.concurrency_group, "browser");
                assert_eq!(grant.scope_kind, "browser");
                (grant.tool_id, grant.approval_mode, grant.risk_level)
            })
            .collect::<BTreeSet<_>>();

        assert!(ids.contains(&(
            "browser.context".to_string(),
            "auto".to_string(),
            "low".to_string()
        )));
        assert!(ids.contains(&(
            "browser.snapshot".to_string(),
            "auto".to_string(),
            "low".to_string()
        )));
        for tool in [
            "browser.navigate",
            "browser.click",
            "browser.type",
            "browser.fill",
            "browser.scroll",
            "browser.press",
            "browser.submit",
        ] {
            assert!(
                ids.contains(&(
                    tool.to_string(),
                    "explicit".to_string(),
                    "medium".to_string()
                )),
                "missing browser capability for {tool}"
            );
        }
    }

    #[test]
    fn write_lock_conflicts_on_overlapping_roots() {
        assert!(write_lock_roots_conflict(
            &[PathBuf::from("/tmp/repo")],
            &[PathBuf::from("/tmp/repo/src")]
        ));
        assert!(!write_lock_roots_conflict(
            &[PathBuf::from("/tmp/repo-a")],
            &[PathBuf::from("/tmp/repo-b")]
        ));
    }

    #[test]
    fn agent_runtime_transfers_write_locks_between_tool_and_command_owners() {
        let runtime = AgentRuntime::default();

        assert!(
            runtime
                .try_claim_write_lock(
                    "tool-call",
                    "job-a",
                    "worker-a",
                    &[String::from("/tmp/repo")],
                    "fs.write_text: update file",
                )
                .expect("first claim should succeed")
                .is_none()
        );

        let conflict = runtime
            .try_claim_write_lock(
                "other-owner",
                "job-b",
                "worker-b",
                &[String::from("/tmp/repo/src")],
                "command.run: cargo test",
            )
            .expect("conflict check should succeed")
            .expect("second owner should conflict");
        assert_eq!(conflict.job_id, "job-a");

        runtime
            .transfer_write_lock("tool-call", "command-session")
            .expect("lock transfer should succeed");

        let conflict = runtime
            .try_claim_write_lock(
                "other-owner",
                "job-b",
                "worker-b",
                &[String::from("/tmp/repo/src")],
                "command.run: cargo test",
            )
            .expect("conflict check should succeed")
            .expect("transferred owner should still conflict");
        assert_eq!(conflict.owner_id, "command-session");

        runtime.release_write_lock("command-session");

        assert!(
            runtime
                .try_claim_write_lock(
                    "other-owner",
                    "job-b",
                    "worker-b",
                    &[String::from("/tmp/repo/src")],
                    "command.run: cargo test",
                )
                .expect("claim after release should succeed")
                .is_none()
        );
    }

    #[test]
    fn worker_prompt_limits_child_job_fanout_to_root_workers() {
        let root_worker = WorkerSummary {
            id: "root".to_string(),
            job_id: "job".to_string(),
            parent_worker_id: None,
            title: "Root worker".to_string(),
            lane: "utility".to_string(),
            state: "queued".to_string(),
            provider: "test".to_string(),
            model: "test".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            provider_session_id: String::new(),
            working_dir: "/tmp".to_string(),
            read_roots: vec!["/tmp".to_string()],
            write_roots: vec!["/tmp".to_string()],
            max_steps: 10,
            max_tool_calls: 10,
            max_wall_clock_secs: 30,
            step_count: 0,
            tool_call_count: 0,
            wait_until_json: None,
            wait_started_at: None,
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };
        let child_worker = WorkerSummary {
            id: "child".to_string(),
            parent_worker_id: Some("root".to_string()),
            ..root_worker.clone()
        };

        let root_prompt = worker_system_prompt(&root_worker);
        let child_prompt = worker_system_prompt(&child_worker);

        assert!(root_prompt.contains("spawn_child_jobs"));
        assert!(!child_prompt.contains("spawn_child_jobs"));
        assert!(root_prompt.contains("{\"kind\":\"final_answer\""));
        assert!(
            !root_prompt.contains("{{\"kind\""),
            "worker prompt must show valid single-object JSON examples"
        );
        assert!(
            root_prompt.contains("Do not put plans, next-step instructions"),
            "worker prompt should prevent internal plans from becoming visible answers"
        );
        assert!(root_prompt.contains("{\"kind\":\"progress_update\""));
        assert!(root_prompt.contains("progress_update records a non-terminal checkpoint"));
        assert!(
            root_prompt.contains("\"tool\":\"command.run\""),
            "worker prompt should include concrete command.run action shape"
        );
        assert!(
            root_prompt.contains("Do not use provider-native tool wrappers"),
            "worker prompt should reject provider-native tool-call shapes"
        );
    }

    #[test]
    fn plan_mode_worker_prompt_disables_actions() {
        let worker = WorkerSummary {
            id: "root".to_string(),
            job_id: "job".to_string(),
            parent_worker_id: None,
            title: "Root worker".to_string(),
            lane: "utility".to_string(),
            state: "queued".to_string(),
            provider: "test".to_string(),
            model: "test".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            provider_session_id: String::new(),
            working_dir: "/tmp".to_string(),
            read_roots: vec!["/tmp".to_string()],
            write_roots: vec!["/tmp".to_string()],
            max_steps: 10,
            max_tool_calls: 10,
            max_wall_clock_secs: 30,
            step_count: 0,
            tool_call_count: 0,
            wait_until_json: None,
            wait_started_at: None,
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };

        let prompt = worker_system_prompt_with_mode(&worker, "plan");
        assert!(prompt.contains("Plan mode is enabled"));
        assert!(prompt.contains("Do not call tools"));
        assert!(prompt.contains("Available tools: disabled in Plan mode"));
        assert!(prompt.contains("{\"kind\":\"final_answer\""));
        assert!(!prompt.contains("\"kind\":\"tool_call\""));
        assert!(!prompt.contains("spawn_child_jobs"));
    }

    #[test]
    fn plan_mode_retry_prompt_requires_final_answer() {
        let prompt = build_plan_mode_retry_prompt("inspect the repo", "run command.run");
        assert!(prompt.contains("Plan mode is enabled"));
        assert!(prompt.contains("kind=\"final_answer\""));
        assert!(prompt.contains("Do not call tools"));
        assert!(prompt.contains("run command.run"));
    }

    #[test]
    fn internal_action_item_final_answers_retry_before_any_tool_call() {
        assert!(should_retry_internal_action_item_final_answer(
            "Next single step: inspect the workspace to find the `stfr` project.",
            0
        ));
        assert!(should_retry_internal_action_item_final_answer(
            "Check whether the STFR server process is currently running.",
            0
        ));
        assert!(!should_retry_internal_action_item_final_answer(
            "I found the STFR project in `/home/eba/dev-projects/dga-clients/stfr`.",
            0
        ));
        assert!(
            !should_retry_internal_action_item_final_answer("Next step: inspect the workspace.", 1),
            "after an action has run, concise follow-up guidance can be a valid answer"
        );
    }

    #[test]
    fn action_jobs_retry_zero_tool_final_answer_that_only_echoes_merge_task() {
        let worker = test_worker_summary("zero-tool-merge", 100, 100);
        let mut job = test_publication_job_summary("zero-tool-merge");
        job.publication_requested = false;
        job.prompt_excerpt =
            "we got a thumbs up from codex so it looks like we can merge".to_string();

        assert!(should_retry_zero_tool_action_final_answer(
            &job,
            "Ready to merge",
            "Merge PR #207 into dev, then delete the short-lived branch `fix-206-mobile-transcript-overflow` after confirming the merge completed.",
            "act",
            &worker,
            1,
            0,
            0,
        ));
        assert!(!should_retry_zero_tool_action_final_answer(
            &job,
            "Need confirmation",
            "Please confirm that you want me to merge PR #207 into dev and delete the source branch.",
            "act",
            &worker,
            1,
            0,
            0,
        ));
        assert!(!should_retry_zero_tool_action_final_answer(
            &job,
            "Blocked",
            "I cannot merge PR #207 because the required review is still pending.",
            "act",
            &worker,
            1,
            0,
            0,
        ));
        assert!(!should_retry_zero_tool_action_final_answer(
            &job,
            "Merged by child jobs",
            "The approved PR was merged and the source branch was deleted.",
            "act",
            &worker,
            1,
            0,
            2,
        ));
    }

    #[test]
    fn zero_tool_guard_allows_text_only_generated_artifacts() {
        let worker = test_worker_summary("zero-tool-draft", 100, 100);
        let mut job = test_publication_job_summary("zero-tool-draft");
        job.publication_requested = false;
        job.title = "Text-only request".to_string();
        job.purpose = "Session prompt".to_string();
        job.prompt_excerpt = "Draft a commit message and release notes for this diff.".to_string();

        assert!(!should_retry_zero_tool_action_final_answer(
            &job,
            "Drafted commit text",
            "fix: normalize daemon final responses\n\nRelease notes: final response metadata is now structured.",
            "act",
            &worker,
            1,
            0,
            0,
        ));

        job.prompt_excerpt = "Write an issue comment summarizing the proposed fix.".to_string();
        assert!(!should_retry_zero_tool_action_final_answer(
            &job,
            "Drafted issue comment",
            "Here is a comment body ready to post.",
            "act",
            &worker,
            1,
            0,
            0,
        ));

        job.prompt_excerpt = "Write a PR summary for this fix.".to_string();
        assert!(!should_retry_zero_tool_action_final_answer(
            &job,
            "Drafted PR summary",
            "This PR normalizes final responses and keeps job metadata structured.",
            "act",
            &worker,
            1,
            0,
            0,
        ));

        job.prompt_excerpt = "Post a comment on issue #209 with the validation result.".to_string();
        assert!(should_retry_zero_tool_action_final_answer(
            &job,
            "Comment ready",
            "Validation passed and the fix is ready.",
            "act",
            &worker,
            1,
            0,
            0,
        ));

        job.prompt_excerpt = "Prepare a short note for tomorrow's design review.".to_string();
        assert!(!should_retry_zero_tool_action_final_answer(
            &job,
            "Prepared note",
            "Here is a concise note for the design review.",
            "act",
            &worker,
            1,
            0,
            0,
        ));
    }

    #[test]
    fn pr_review_feedback_does_not_accept_clean_answer_without_inline_thread_evidence() {
        let worker = test_worker_summary("pr-feedback-evidence", 100, 100);
        let detail = test_job_detail_with_prompt(
            "Review latest PR feedback on PR #217, including Codex review comments and unresolved requested changes.",
        );
        let checkpoint = test_checkpoint_with_prompt(
            "Review latest PR feedback on PR #217, including Codex review comments.",
        );

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback and the PR looks clean.",
            "act",
            &checkpoint,
            &worker,
            3,
            1,
        ));
    }

    #[test]
    fn pr_review_feedback_compact_pr_ref_requires_thread_aware_evidence() {
        let worker = test_worker_summary("compact-pr-feedback-evidence", 100, 100);
        let detail = test_job_detail_with_prompt(
            "Review PR#217 latest feedback, including unresolved requested changes.",
        );
        let checkpoint = test_checkpoint_with_prompt("Review PR#217 latest feedback.");

        assert!(requires_pr_review_thread_evidence(
            "review pr#217 latest feedback"
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            1,
        ));
    }

    #[test]
    fn pr_review_feedback_plural_pr_refs_require_thread_aware_evidence() {
        let worker = test_worker_summary("plural-pr-feedback-evidence", 100, 100);
        let detail = test_job_detail_with_prompt(
            "Check PRs #217 and #219 review feedback and unresolved comments.",
        );
        let checkpoint = test_checkpoint_with_prompt("Check PRs #217 and #219 review feedback.");

        assert!(requires_pr_review_thread_evidence(
            "check prs #217 and #219 review feedback"
        ));
        assert!(!requires_pr_review_thread_evidence(
            "review issue #218 latest feedback"
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            1,
        ));
    }

    #[test]
    fn pr_review_feedback_accepts_clean_answer_after_thread_aware_evidence() {
        let worker = test_worker_summary("pr-feedback-thread-aware", 100, 100);
        let mut detail = test_job_detail_with_prompt(
            "Check latest PR feedback on PR #217 and unresolved Codex review comments.",
        );
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_review_threads",
            json!({
                "evidence_kind": "github_pr_review_threads",
                "pr_number": 217,
                "review_threads": [
                    {
                        "isResolved": true,
                        "isOutdated": false,
                        "path": "crates/daemon/src/agent.rs",
                        "line": 42,
                        "comments": {"nodes": []}
                    }
                ],
                "review_threads_complete": true,
                "thread_comments_truncated": false
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Check latest PR feedback.");

        assert!(has_thread_aware_pr_review_evidence(
            &detail,
            &evidence_task_text(&detail, &checkpoint),
            "No actionable feedback",
            "There is no actionable feedback.",
        ));
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn pr_review_feedback_evidence_must_match_referenced_pr() {
        let worker = test_worker_summary("wrong-pr-feedback-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt(
            "Check latest PR feedback on PR #217 and unresolved Codex review comments.",
        );
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_review_threads",
            json!({
                "evidence_kind": "github_pr_review_threads",
                "pr_number": 219,
                "review_threads": [],
                "review_threads_complete": true,
                "thread_comments_truncated": false
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Check latest PR feedback on PR #217.");

        assert!(!has_thread_aware_pr_review_evidence(
            &detail,
            &evidence_task_text(&detail, &checkpoint),
            "No actionable feedback",
            "There is no actionable feedback.",
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));

        detail.tool_calls.clear();
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_review_threads",
            json!({
                "evidence_kind": "github_pr_review_threads",
                "pr_number": 217,
                "review_threads": [],
                "review_threads_complete": true,
                "thread_comments_truncated": false
            }),
        ));
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn pr_review_threads_result_reports_incomplete_when_comments_truncated() {
        let pull = json!({
            "url": "https://github.com/WebLime-agency/nucleus/pull/219",
            "title": "fix: improve evidence grounding",
            "state": "OPEN",
            "comments": {"nodes": []},
            "reviews": {"nodes": []},
            "commits": {
                "nodes": [
                    {
                        "commit": {
                            "statusCheckRollup": {
                                "state": "SUCCESS"
                            }
                        }
                    }
                ]
            }
        });
        let truncated = github_pr_review_threads_result(
            "WebLime-agency",
            "nucleus",
            219,
            &pull,
            vec![json!({"id": "thread-1"})],
            true,
        );
        assert_eq!(
            truncated
                .get("thread_comments_truncated")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            truncated
                .get("review_threads_complete")
                .and_then(Value::as_bool),
            Some(false)
        );

        let complete = github_pr_review_threads_result(
            "WebLime-agency",
            "nucleus",
            219,
            &pull,
            vec![json!({"id": "thread-1"})],
            false,
        );
        assert_eq!(
            complete
                .get("review_threads_complete")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn pr_review_feedback_retries_when_thread_completeness_is_unknown() {
        let worker = test_worker_summary("pr-feedback-unknown-thread-completeness", 100, 100);
        let mut detail = test_job_detail_with_prompt(
            "Check latest PR feedback on PR #217 and unresolved Codex review comments.",
        );
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_review_threads",
            json!({
                "evidence_kind": "github_pr_review_threads",
                "review_threads": [
                    {
                        "isResolved": true,
                        "isOutdated": false,
                        "path": "crates/daemon/src/agent.rs",
                        "line": 42,
                        "comments": {"nodes": []}
                    }
                ]
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Check latest PR feedback.");

        assert!(!has_thread_aware_pr_review_evidence(
            &detail,
            &evidence_task_text(&detail, &checkpoint),
            "No actionable feedback",
            "There is no actionable feedback.",
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn pr_review_feedback_retries_when_thread_evidence_is_incomplete() {
        let worker = test_worker_summary("pr-feedback-incomplete-thread-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt(
            "Check latest PR feedback on PR #217 and unresolved Codex review comments.",
        );
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_review_threads",
            json!({
                "evidence_kind": "github_pr_review_threads",
                "review_threads": [
                    {
                        "isResolved": false,
                        "isOutdated": false,
                        "path": "crates/daemon/src/agent.rs",
                        "line": 42,
                        "comments": {"nodes": []}
                    }
                ],
                "review_threads_complete": false,
                "thread_comments_truncated": true
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Check latest PR feedback.");

        assert!(!has_thread_aware_pr_review_evidence(
            &detail,
            &evidence_task_text(&detail, &checkpoint),
            "No actionable feedback",
            "There is no actionable feedback.",
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn repeated_user_challenge_blocks_repeated_unsupported_clean_answer() {
        let worker = test_worker_summary("repeated-grounding-challenge", 100, 100);
        let detail = test_job_detail_with_prompt(
            "Are you sure? I can see a screenshot showing Codex review comments on PR #217. Check again.",
        );
        let checkpoint = test_checkpoint_with_prompt(
            "Are you sure? I can see a screenshot showing Codex review comments on PR #217. Check again.",
        );

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Still clean",
            "There is nothing actionable.",
            "act",
            &checkpoint,
            &worker,
            4,
            2,
        ));
    }

    #[test]
    fn non_pr_challenge_does_not_require_pr_review_threads() {
        let worker = test_worker_summary("non-pr-grounding-challenge", 100, 100);
        let detail =
            test_job_detail_with_prompt("Are you sure? Check again whether this note is concise.");
        let checkpoint =
            test_checkpoint_with_prompt("Are you sure? Check again whether this note is concise.");

        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Still clean",
            "There is nothing actionable.",
            "act",
            &checkpoint,
            &worker,
            4,
            2,
        ));
    }

    #[test]
    fn codex_review_without_pr_signal_does_not_require_pr_threads() {
        let worker = test_worker_summary("non-pr-codex-review", 100, 100);
        let detail = test_job_detail_with_prompt(
            "Review this local commit and tag @codex review when done.",
        );
        let checkpoint = test_checkpoint_with_prompt(
            "Review this local commit and tag @codex review when done.",
        );

        assert!(!requires_pr_review_thread_evidence(
            "review this local commit and tag @codex review when done"
        ));
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No actionable feedback",
            "There is no actionable feedback.",
            "act",
            &checkpoint,
            &worker,
            3,
            1,
        ));
    }

    #[test]
    fn compact_pr_review_requests_require_thread_evidence() {
        assert!(requires_pr_review_thread_evidence(
            "review PR#217 latest feedback"
        ));
        assert!(requires_pr_review_thread_evidence(
            "check PRs #217 and #219 for unresolved comments"
        ));
        assert!(requires_pr_review_thread_evidence(
            "review pull requests #217 and #219 requested changes"
        ));
    }

    #[test]
    fn pr_merged_claim_requires_direct_matching_pr_state() {
        let worker = test_worker_summary("pr-lifecycle-evidence", 100, 100);
        let mut detail =
            test_job_detail_with_prompt("looks like we got the clear to merge PR #217 to dev");
        detail.job.title = "Merge PR".to_string();
        let checkpoint =
            test_checkpoint_with_prompt("looks like we got the clear to merge PR #217 to dev");

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Already merged",
            "PR #217 is already merged into dev; nothing is left to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            1,
        ));

        let mut wrong_pr_detail = detail.clone();
        wrong_pr_detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 219,
                "state": "MERGED",
                "merged_at": "2026-05-20T16:00:00Z",
                "head_ref_name": "issue-218-grounding-evidence",
                "base_ref_name": "dev"
            }),
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &wrong_pr_detail,
            "Already merged",
            "PR #217 is already merged into dev; nothing is left to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));

        let mut open_detail = detail.clone();
        open_detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 217,
                "state": "OPEN",
                "merged_at": null,
                "head_ref_name": "work/nucleus/f8eec90e",
                "base_ref_name": "dev"
            }),
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &open_detail,
            "Already merged",
            "PR #217 is already merged into dev; nothing is left to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));

        let mut merged_detail = detail.clone();
        merged_detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 217,
                "state": "MERGED",
                "merged_at": "2026-05-20T16:00:00Z",
                "head_ref_name": "work/nucleus/f8eec90e",
                "base_ref_name": "dev"
            }),
        ));
        assert!(has_direct_pr_merged_evidence_for_claim(
            &merged_detail,
            &evidence_task_text(&merged_detail, &checkpoint),
            "Already merged",
            "PR #217 is already merged into dev; nothing is left to merge.",
        ));
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &merged_detail,
            "Already merged",
            "PR #217 is already merged into dev; nothing is left to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn pr_lifecycle_claim_matches_compact_pr_reference() {
        let worker = test_worker_summary("compact-pr-lifecycle-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt("looks like PR#217 is already merged");
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 219,
                "state": "MERGED",
                "merged_at": "2026-05-20T16:00:00Z"
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("looks like PR#217 is already merged");

        assert_eq!(
            extract_pr_numbers("PR#217").into_iter().collect::<Vec<_>>(),
            vec![217]
        );
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Already merged",
            "PR#217 is already merged into dev; nothing is left to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn pr_lifecycle_claim_ignores_unqualified_issue_references() {
        let worker = test_worker_summary("issue-reference-lifecycle-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt(
            "Check whether PR #217 is ready to merge. This also closes #218.",
        );
        detail.job.title = "PR lifecycle".to_string();
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 217,
                "state": "OPEN",
                "merged_at": null,
                "merge_state_status": "CLEAN"
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt(
            "Check whether PR #217 is ready to merge. This also closes #218.",
        );

        assert_eq!(
            extract_pr_numbers("Check PR #217 and close #218")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![217]
        );
        assert_eq!(
            extract_pr_numbers("Check PR #217 and #219, closes #218")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![217, 219]
        );
        assert_eq!(
            extract_pr_numbers("Check PRs #217 and #219, closes #218")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![217, 219]
        );
        assert_eq!(
            extract_pr_numbers("Check pull requests #217 and #219, closes #218")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![217, 219]
        );
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Ready to merge",
            "PR #217 is ready to merge and closes #218.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn pr_lifecycle_claim_requires_state_for_every_referenced_pr() {
        let worker = test_worker_summary("multi-pr-lifecycle-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt("Check whether PR #217 and #219 are ready.");
        detail.job.title = "PR lifecycle".to_string();
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 217,
                "state": "OPEN",
                "merged_at": null,
                "merge_state_status": "CLEAN"
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Check whether PR #217 and #219 are ready.");

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Ready to merge",
            "PR #217 and #219 are ready to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));

        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 219,
                "state": "OPEN",
                "merged_at": null,
                "merge_state_status": "CLEAN"
            }),
        ));
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Ready to merge",
            "PR #217 and #219 are ready to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            3,
        ));
    }

    #[test]
    fn pr_merged_claim_requires_merged_state_for_every_referenced_pr() {
        let worker = test_worker_summary("multi-pr-merged-evidence", 100, 100);
        let mut detail =
            test_job_detail_with_prompt("Check whether PR #217 and PR #219 are already merged.");
        detail.job.title = "PR lifecycle".to_string();
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 217,
                "state": "MERGED",
                "merged_at": "2026-05-20T16:00:00Z"
            }),
        ));
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 219,
                "state": "OPEN",
                "merged_at": null
            }),
        ));
        let checkpoint =
            test_checkpoint_with_prompt("Check whether PR #217 and PR #219 are already merged.");

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Already merged",
            "PR #217 and PR #219 are already merged; nothing is left to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            3,
        ));

        detail.tool_calls.pop();
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 219,
                "state": "MERGED",
                "merged_at": "2026-05-20T16:05:00Z"
            }),
        ));
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Already merged",
            "PR #217 and PR #219 are already merged; nothing is left to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            3,
        ));
    }

    #[test]
    fn pr_ready_claim_requires_direct_pr_state() {
        let worker = test_worker_summary("pr-ready-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt("Check whether PR #217 is ready to merge.");
        detail.job.title = "Merge readiness".to_string();
        let checkpoint = test_checkpoint_with_prompt("Check whether PR #217 is ready to merge.");

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Ready to merge",
            "PR #217 is ready to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            1,
        ));

        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_state",
            json!({
                "evidence_kind": "github_pr_state",
                "pr_number": 217,
                "state": "OPEN",
                "merged_at": null,
                "merge_state_status": "CLEAN",
                "head_ref_name": "work/nucleus/f8eec90e",
                "base_ref_name": "dev"
            }),
        ));
        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Ready to merge",
            "PR #217 is ready to merge.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn zero_test_output_is_not_treated_as_passing_validation() {
        let zero_result = json!({
            "stdout_tail": "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored",
            "stderr_tail": "",
            "exit_code": 0,
        });
        let interpretation =
            interpret_test_command_result(&zero_result).expect("zero tests should be detected");
        assert_eq!(interpretation["status"], "no_tests_matched");

        let passing_result = json!({
            "stdout_tail": "running 1 test\n\ntest result: ok. 1 passed; 0 failed; 0 ignored",
            "stderr_tail": "",
            "exit_code": 0,
        });
        assert!(interpret_test_command_result(&passing_result).is_none());

        let mixed_cargo_result = json!({
            "stdout_tail": "running 1 test\n\ntest result: ok. 1 passed; 0 failed; 0 ignored\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored",
            "stderr_tail": "",
            "exit_code": 0,
        });
        assert!(interpret_test_command_result(&mixed_cargo_result).is_none());
    }

    #[test]
    fn test_success_claim_requires_test_or_check_evidence() {
        let worker = test_worker_summary("test-evidence", 100, 100);
        let detail = test_job_detail_with_prompt("Check failed tests and validation for this PR.");
        let checkpoint = test_checkpoint_with_prompt("Check failed tests and validation.");

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "No failed tests",
            "No failed tests; validation passed.",
            "act",
            &checkpoint,
            &worker,
            3,
            1,
        ));
    }

    #[test]
    fn failing_test_run_does_not_support_validation_passed_claim() {
        let worker = test_worker_summary("failed-test-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt("Run tests and validate the fix.");
        detail.tool_calls.push(test_tool_call_summary(
            "tests.run",
            json!({
                "stdout_tail": "test result: FAILED. 0 passed; 1 failed",
                "stderr_tail": "",
                "exit_code": 1
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Run tests and validate the fix.");

        assert!(!has_test_validation_evidence(
            &detail,
            &evidence_task_text(&detail, &checkpoint),
            "Validation passed",
            "Tests passed.",
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Validation passed",
            "Tests passed.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn status_check_evidence_must_match_referenced_pr() {
        let worker = test_worker_summary("wrong-pr-status-evidence", 100, 100);
        let mut detail =
            test_job_detail_with_prompt("Check whether tests passed for PR #217 before merge.");
        detail.tool_calls.push(test_tool_call_summary(
            "tests.run",
            json!({
                "stdout_tail": "running 1 test\n\ntest result: ok. 1 passed; 0 failed",
                "exit_code": 0
            }),
        ));
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_review_threads",
            json!({
                "evidence_kind": "github_pr_review_threads",
                "pr_number": 219,
                "status_check_rollup": {
                    "state": "SUCCESS"
                }
            }),
        ));
        let checkpoint =
            test_checkpoint_with_prompt("Check whether tests passed for PR #217 before merge.");

        assert!(!has_test_validation_evidence(
            &detail,
            &evidence_task_text(&detail, &checkpoint),
            "Checks passed",
            "PR #217 checks passed.",
        ));
        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Checks passed",
            "PR #217 checks passed.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));

        detail.tool_calls.clear();
        detail.tool_calls.push(test_tool_call_summary(
            "github.pr_review_threads",
            json!({
                "evidence_kind": "github_pr_review_threads",
                "pr_number": 217,
                "status_check_rollup": {
                    "state": "SUCCESS"
                }
            }),
        ));
        assert!(has_test_validation_evidence(
            &detail,
            &evidence_task_text(&detail, &checkpoint),
            "Checks passed",
            "PR #217 checks passed.",
        ));
    }

    #[test]
    fn zero_matched_test_evidence_blocks_passing_validation_claim() {
        let worker = test_worker_summary("zero-test-evidence", 100, 100);
        let mut detail = test_job_detail_with_prompt("Run tests and validate the fix.");
        detail.tool_calls.push(test_tool_call_summary(
            "tests.run",
            json!({
                "stdout_tail": "running 0 tests",
                "validation_interpretation": {
                    "status": "no_tests_matched"
                }
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Run tests and validate the fix.");

        assert!(should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Validation passed",
            "Tests passed.",
            "act",
            &checkpoint,
            &worker,
            3,
            2,
        ));
    }

    #[test]
    fn later_successful_test_run_supersedes_zero_matched_evidence() {
        let worker = test_worker_summary("zero-test-later-success", 100, 100);
        let mut detail = test_job_detail_with_prompt("Run tests and validate the fix.");
        detail.tool_calls.push(test_tool_call_summary(
            "tests.run",
            json!({
                "stdout_tail": "running 0 tests",
                "exit_code": 0,
                "validation_interpretation": {
                    "status": "no_tests_matched"
                }
            }),
        ));
        detail.tool_calls.push(test_tool_call_summary(
            "tests.run",
            json!({
                "stdout_tail": "running 1 test\n\ntest result: ok. 1 passed; 0 failed",
                "exit_code": 0
            }),
        ));
        let checkpoint = test_checkpoint_with_prompt("Run tests and validate the fix.");

        assert!(!should_retry_unsupported_confident_negative_final_answer(
            &detail,
            "Validation passed",
            "Tests passed.",
            "act",
            &checkpoint,
            &worker,
            3,
            3,
        ));
    }

    #[test]
    fn github_comment_shell_body_with_backticks_is_rejected() {
        let result = reject_unsafe_github_comment_shell(
            "sh",
            &[
                "-lc".to_string(),
                "gh pr comment 218 --body \"Fixed `danger`\"".to_string(),
            ],
        );
        assert!(result.is_err());

        let short_body_result = reject_unsafe_github_comment_shell(
            "sh",
            &[
                "-lc".to_string(),
                "gh pr comment 218 -b \"Fixed `danger`\"".to_string(),
            ],
        );
        assert!(short_body_result.is_err());

        let global_repo_result = reject_unsafe_github_comment_shell(
            "sh",
            &[
                "-lc".to_string(),
                "gh --repo WebLime-agency/nucleus pr comment 218 --body \"Fixed `danger`\""
                    .to_string(),
            ],
        );
        assert!(global_repo_result.is_err());

        let preview = preview_github_comment(GithubCommentArgs {
            owner: Some("WebLime-agency".to_string()),
            repo: Some("nucleus".to_string()),
            target_kind: "pr".to_string(),
            number: 218,
            body: "Fixed `danger` without shell interpolation.".to_string(),
        });
        assert!(preview.detail.contains("body file"));
        assert!(preview.diff_preview.contains("`danger`"));
    }

    #[test]
    fn github_comment_shell_body_file_with_metacharacters_is_allowed() {
        let result = reject_unsafe_github_comment_shell(
            "sh",
            &[
                "-lc".to_string(),
                "printf '%s' 'Fixed `danger`' > /tmp/body.md; gh pr comment 218 --body-file /tmp/body.md".to_string(),
            ],
        );
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn github_repo_overrides_require_owner_and_repo_together() {
        assert_eq!(
            resolve_github_repo_from_optional(
                None,
                Some("WebLime-agency".to_string()),
                Some("nucleus.git".to_string()),
            )
            .await
            .expect("complete override should resolve"),
            ("WebLime-agency".to_string(), "nucleus".to_string())
        );

        assert!(
            resolve_github_repo_from_optional(None, Some("WebLime-agency".to_string()), None)
                .await
                .is_err()
        );
        assert!(
            resolve_github_repo_from_optional(None, None, Some("nucleus".to_string()))
                .await
                .is_err()
        );
    }

    #[test]
    fn incomplete_progress_final_answers_retry_when_budget_remains() {
        let worker = test_worker_summary("retry-incomplete", 100, 100);

        assert!(should_retry_incomplete_progress_final_answer(
            "Phase 4 is not complete yet",
            "Done and tested: composer extraction. Remaining work: sidebar refactor and docs.",
            "act",
            &worker,
            24,
            23,
        ));
        assert!(should_retry_incomplete_progress_final_answer(
            "Progress validated",
            "Phase 4 is not finished; remaining refactors are still required.",
            "act",
            &worker,
            24,
            23,
        ));
    }

    #[test]
    fn incomplete_progress_final_answers_do_not_retry_when_blocked_plan_or_out_of_budget() {
        let worker = test_worker_summary("no-retry-incomplete", 25, 25);

        assert!(!should_retry_incomplete_progress_final_answer(
            "Phase 4 is not complete yet",
            "Remaining work exists, but I am blocked by a missing credential.",
            "act",
            &worker,
            20,
            20,
        ));
        assert!(!should_retry_incomplete_progress_final_answer(
            "Phase 4 is not complete yet",
            "Remaining work: implement the sidebar.",
            "plan",
            &worker,
            20,
            20,
        ));
        assert!(!should_retry_incomplete_progress_final_answer(
            "Phase 4 is not complete yet",
            "Remaining work: implement the sidebar.",
            "act",
            &worker,
            24,
            24,
        ));
    }

    #[test]
    fn blocked_without_browser_verification_final_answer_is_terminal() {
        let worker = test_worker_summary("blocked-browser-verification", 100, 100);
        let final_answer = "Status: blocked_without_browser_verification\n\
Summary: Code validation passed but browser verification was unavailable.\n\
Validation:\n\
- cargo test -p nucleus-daemon worker_action passed\n\
Remaining:\n\
- Verify the UI through the daemon-owned Browser runtime";

        assert!(!should_retry_incomplete_progress_final_answer(
            "Code validation passed but browser verification was unavailable.",
            final_answer,
            "act",
            &worker,
            10,
            5,
        ));
    }

    #[test]
    fn final_answer_terminal_metadata_marks_blocked_browser_outcome() {
        let metadata = final_answer_terminal_metadata(
            "Code validation passed but browser verification was unavailable.",
            "Status: blocked_without_browser_verification\nBrowser verification status: unavailable",
            &json!({}),
            &[],
            12,
            7,
            &PublicationOutcomePatch::default(),
        );

        assert_eq!(metadata["terminal_status"], "blocked");
        assert_eq!(metadata["blocked"], true);
        assert_eq!(metadata["browser_verification_status"], "unavailable");
        assert_eq!(metadata["step_count"], 12);
        assert_eq!(metadata["tool_call_count"], 7);
    }

    #[test]
    fn terminal_metadata_uses_structured_blocked_publication_outcome() {
        let metadata = final_answer_terminal_metadata(
            "Publication blocked",
            "I could not open the PR yet.",
            &json!({
                "publication_status": "blocked",
                "validation_status": "passed",
                "browser_verification_status": "unavailable",
                "cleanup_status": "clean"
            }),
            &[],
            8,
            4,
            &PublicationOutcomePatch {
                publication_requested: Some(true),
                publication_status: Some("blocked".to_string()),
                validation_status: Some("passed".to_string()),
                browser_verification_status: Some("unavailable".to_string()),
                cleanup_status: Some("clean".to_string()),
                ..PublicationOutcomePatch::default()
            },
        );

        assert_eq!(metadata["terminal_status"], "blocked");
        assert_eq!(metadata["blocked"], true);
        assert_eq!(metadata["publication_status"], "blocked");
        assert_eq!(
            metadata["final_response_metadata"]["publication_status"],
            "blocked"
        );
    }

    #[test]
    fn terminal_metadata_does_not_treat_generic_reached_current_text_as_blocked() {
        let metadata = final_answer_terminal_metadata(
            "Completed the requested cleanup.",
            "The implementation reached the current target state and validation passed.",
            &json!({}),
            &[],
            4,
            2,
            &PublicationOutcomePatch::default(),
        );

        assert_eq!(metadata["terminal_status"], "completed");
        assert_eq!(metadata["blocked"], false);

        let unable_to_reproduce_metadata = final_answer_terminal_metadata(
            "Validated fix.",
            "I was unable to reproduce the issue after the fix, and validation passed.",
            &json!({}),
            &[],
            5,
            3,
            &PublicationOutcomePatch::default(),
        );
        assert_eq!(unable_to_reproduce_metadata["terminal_status"], "completed");
        assert_eq!(unable_to_reproduce_metadata["blocked"], false);

        let budget_metadata = final_answer_terminal_metadata(
            "Checkpoint saved.",
            "Nucleus reached the current step budget for this run.",
            &json!({}),
            &[],
            100,
            20,
            &PublicationOutcomePatch::default(),
        );
        assert_eq!(budget_metadata["terminal_status"], "blocked");
        assert_eq!(budget_metadata["blocked"], true);
    }

    #[test]
    fn browser_verification_text_accepts_browser_verified_phrase() {
        assert_eq!(
            status_from_browser_verification_text("Completed, browser-verified with screenshots."),
            Some("passed")
        );
        assert_eq!(
            status_from_browser_verification_text("Completed, not browser-verified."),
            Some("not_performed")
        );
    }

    #[test]
    fn publication_outcome_patch_extracts_structured_terminal_fields() {
        let job = test_publication_job_summary("publication-extract");
        let patch = publication_outcome_patch(
            &job,
            "Opened PR",
            "Publication status: opened\n\
Publication summary: Ready for review.\n\
PR URL: https://github.com/WebLime-agency/nucleus/pull/200\n\
Source branch: feat/example\n\
Target branch: dev\n\
Validation status: passed\n\
Browser verification status: unavailable\n\
Cleanup status: cleanup_required\n\
Cleanup paths: .tmp-playwright.",
            8,
            4,
        );

        assert_eq!(patch.publication_requested, Some(true));
        assert_eq!(patch.publication_status.as_deref(), Some("opened"));
        assert_eq!(
            patch.pr_url.as_deref(),
            Some("https://github.com/WebLime-agency/nucleus/pull/200")
        );
        assert_eq!(patch.source_branch.as_deref(), Some("feat/example"));
        assert_eq!(patch.target_branch.as_deref(), Some("dev"));
        assert_eq!(patch.validation_status.as_deref(), Some("passed"));
        assert_eq!(
            patch.browser_verification_status.as_deref(),
            Some("unavailable")
        );
        assert_eq!(patch.cleanup_status.as_deref(), Some("cleanup_required"));
        assert_eq!(
            patch.cleanup_paths.as_deref(),
            Some(&[".tmp-playwright".to_string()][..])
        );
    }

    #[test]
    fn publication_outcome_patch_extracts_nested_section_statuses() {
        let job = test_publication_job_summary("publication-nested-statuses");
        let final_answer = "Publication:\n\
- Status: not_opened\n\
- Summary: Branch remained unpublished\n\
Validation:\n\
- Status: passed\n\
- Summary: cargo test -p nucleus-daemon passed\n\
Browser verification:\n\
- Status: unavailable\n\
- Summary: Browser runtime was unavailable\n\
Cleanup:\n\
- Status: clean";
        let patch = publication_outcome_patch(&job, "Done", final_answer, 8, 4);

        assert_eq!(patch.publication_status.as_deref(), Some("not_opened"));
        assert_eq!(patch.validation_status.as_deref(), Some("passed"));
        assert_eq!(
            patch.browser_verification_status.as_deref(),
            Some("unavailable")
        );
        assert_eq!(patch.cleanup_status.as_deref(), Some("clean"));
        assert!(publication_final_answer_has_required_facts(
            "Opened PR",
            final_answer
        ));
    }

    #[test]
    fn publication_outcome_patch_blocks_missing_publication_status_for_requested_jobs() {
        let mut job = test_publication_job_summary("publication-missing-status");
        job.publication_status = "not_requested".to_string();
        let final_answer = "Validation status: passed\n\
Browser verification status: not_performed\n\
Cleanup status: clean";
        let patch = publication_outcome_patch(&job, "Done", final_answer, 10, 5);

        assert_eq!(patch.publication_status.as_deref(), Some("blocked"));
        assert_eq!(patch.validation_status.as_deref(), Some("passed"));
        assert_eq!(
            patch.browser_verification_status.as_deref(),
            Some("not_performed")
        );
        assert_eq!(patch.cleanup_status.as_deref(), Some("clean"));
    }

    #[test]
    fn required_browser_verification_uses_structured_metadata_status() {
        let mut job = test_publication_job_summary("publication-browser-required");
        job.browser_verification_required = true;
        job.browser_verification_status = "not_performed".to_string();
        let metadata = json!({
            "publication_status": "opened",
            "validation_status": "passed",
            "browser_verification_status": "passed",
            "cleanup_status": "clean"
        });

        let patch = publication_outcome_patch_with_metadata(
            &job,
            "Opened PR",
            "Published the PR.",
            &metadata,
            8,
            4,
        );
        assert_eq!(patch.browser_verification_status.as_deref(), Some("passed"));
        assert!(publication_final_answer_has_required_facts_with_metadata(
            "Opened PR",
            "Published the PR.",
            &metadata,
            None,
        ));

        let nested_metadata = json!({
            "publication_status": "opened",
            "validation_status": "passed",
            "browser_verification": {
                "status": "unavailable",
                "summary": "Browser runtime was unavailable."
            },
            "cleanup_status": "clean"
        });
        let patch = publication_outcome_patch_with_metadata(
            &job,
            "Opened PR",
            "Published the PR.",
            &nested_metadata,
            8,
            4,
        );
        assert_eq!(
            patch.browser_verification_status.as_deref(),
            Some("unavailable")
        );
        assert!(publication_final_answer_has_required_facts_with_metadata(
            "Opened PR",
            "Published the PR.",
            &nested_metadata,
            None,
        ));

        assert!(publication_final_answer_has_required_facts_with_metadata(
            "Opened PR",
            "Published the PR.",
            &metadata,
            Some(&BrowserVerificationClaim {
                status: "passed".to_string(),
                summary: "Verified in Browser.".to_string(),
                artifact_ids: vec!["artifact-1".to_string()],
            }),
        ));

        let patch = publication_outcome_patch_with_metadata(
            &job,
            "Opened PR",
            "Published the PR. Browser verification: passed.",
            &metadata,
            8,
            4,
        );
        assert_eq!(patch.browser_verification_status.as_deref(), Some("passed"));
    }

    #[test]
    fn optional_browser_verification_uses_preserved_final_answer_metadata() {
        let mut job = test_publication_job_summary("publication-browser-optional");
        job.browser_verification_required = false;
        job.browser_verification_status = "not_required".to_string();

        let action = parse_worker_action(
            r#"{"kind":"final_answer","summary":"published","final_answer":"Published the PR.","publication_status":"opened","validation_status":"passed","cleanup_status":"clean","browser_verification":{"status":"passed","summary":"Clicked through the published UI.","artifact_ids":["artifact-1"]}}"#,
        )
        .expect("final answer with optional browser verification should parse");
        let WorkerAction::FinalAnswer {
            final_answer,
            metadata,
            browser_verification: Some(claim),
            ..
        } = action
        else {
            panic!("expected final answer with browser verification claim");
        };

        let patch = publication_outcome_patch_with_metadata(
            &job,
            "Opened PR",
            &final_answer,
            &metadata,
            8,
            4,
        );

        assert_eq!(claim.status, "passed");
        assert_eq!(metadata["browser_verification"]["status"], "passed");
        assert_eq!(metadata["browser_verification_status"], "passed");
        assert_eq!(patch.browser_verification_status.as_deref(), Some("passed"));
    }

    #[test]
    fn publication_outcome_patch_ignores_nested_non_publication_summaries() {
        let job = test_publication_job_summary("publication-summary");
        let final_answer = "Publication status: opened\n\
Validation:\n\
- Status: passed\n\
- Summary: cargo test passed\n\
Browser verification:\n\
- Status: unavailable\n\
- Summary: browser runtime unavailable\n\
Publication:\n\
- Summary: PR opened for review\n\
Cleanup status: clean";
        let patch = publication_outcome_patch(&job, "Opened PR", final_answer, 8, 4);

        assert_eq!(
            patch.publication_summary.as_deref(),
            Some("PR opened for review")
        );
    }

    #[test]
    fn publication_outcome_patch_extracts_matching_inline_nested_labels_only() {
        let job = test_publication_job_summary("publication-inline-labels");
        let final_answer = "Publication: status: opened, summary: Ready for review\n\
Validation: status: passed, summary: cargo test passed\n\
Browser verification: status: not_performed, summary: no UI surface changed\n\
Cleanup: status: clean";
        let patch = publication_outcome_patch(&job, "Opened PR", final_answer, 8, 4);

        assert_eq!(patch.publication_status.as_deref(), Some("opened"));
        assert_eq!(
            patch.publication_summary.as_deref(),
            Some("Ready for review")
        );
        assert_eq!(patch.validation_status.as_deref(), Some("passed"));
        assert_eq!(
            patch.browser_verification_status.as_deref(),
            Some("not_performed")
        );
        assert_eq!(patch.cleanup_status.as_deref(), Some("clean"));
    }

    #[test]
    fn publication_completion_guard_requires_terminal_facts_once() {
        assert!(!publication_final_answer_has_required_facts(
            "Opened PR",
            "Opened the PR and ran checks."
        ));
        assert!(publication_final_answer_has_required_facts(
            "Opened PR",
            "Publication status: opened\n\
Validation status: passed\n\
Browser verification status: not_performed\n\
Cleanup status: clean"
        ));
        assert!(publication_final_answer_has_required_facts(
            "Opened PR",
            "Publication status: opened\n\
Validation status: passed\n\
Browser verification: status: passed, summary: clicked through the changed flow\n\
Cleanup status: clean"
        ));
        assert!(publication_final_answer_has_required_facts(
            "Done",
            "Publication:\n\
- Status: opened\n\
Validation status: passed\n\
Browser verification status: not_performed\n\
Cleanup status: clean"
        ));
    }

    #[test]
    fn publication_outcome_patch_treats_explicit_opened_text_as_opened() {
        let job = test_publication_job_summary("publication-opened-text");
        let final_answer = "Pull request opened.\n\
Validation status: passed\n\
Browser verification status: not_performed\n\
Cleanup status: clean";
        let patch = publication_outcome_patch(&job, "Pull request opened", final_answer, 6, 3);

        assert_eq!(patch.publication_status.as_deref(), Some("opened"));
        assert!(publication_final_answer_has_required_facts(
            "Pull request opened",
            final_answer
        ));
    }

    #[test]
    fn child_job_result_preserves_structured_outcome_metadata() {
        let mut job = test_publication_job_summary("child-publication-outcome");
        job.state = "completed".to_string();
        job.result_summary = "Opened PR".to_string();
        job.publication_status = "opened".to_string();
        job.publication_summary = "Opened a ready PR against dev.".to_string();
        job.pr_url = "https://github.com/WebLime-agency/nucleus/pull/210".to_string();
        job.source_branch = "fix-209-final-response-contract".to_string();
        job.target_branch = "dev".to_string();
        job.validation_status = "passed".to_string();
        job.browser_verification_status = "not_performed".to_string();
        job.cleanup_status = "clean".to_string();
        let detail = JobDetail {
            job,
            workers: Vec::new(),
            child_jobs: Vec::new(),
            tool_calls: Vec::new(),
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: vec![nucleus_protocol::JobEvent {
                id: 1,
                job_id: "child-publication-outcome".to_string(),
                worker_id: Some("worker-child-publication-outcome".to_string()),
                event_type: "job.completed".to_string(),
                status: "completed".to_string(),
                summary: "Opened PR".to_string(),
                detail: "Published the PR.".to_string(),
                data_json: json!({
                    "publication_status": "opened",
                    "pr_url": "https://github.com/WebLime-agency/nucleus/pull/210",
                    "validation_status": "passed",
                    "final_response_metadata": {
                        "cleanup_status": "clean"
                    }
                }),
                created_at: 0,
            }],
        };

        let result = child_job_result_json(&detail).expect("child result should serialize");

        assert_eq!(result["outcome"]["publication_status"], "opened");
        assert_eq!(
            result["outcome"]["pr_url"],
            "https://github.com/WebLime-agency/nucleus/pull/210"
        );
        assert_eq!(result["outcome"]["validation_status"], "passed");
        assert_eq!(result["outcome"]["cleanup_status"], "clean");
        assert_eq!(
            result["events"][0]["data_json"]["publication_status"],
            "opened"
        );
        assert_eq!(
            result["events"][0]["data_json"]["final_response_metadata"]["cleanup_status"],
            "clean"
        );
    }

    #[tokio::test]
    async fn completion_persists_clean_turn_and_structured_outcome_metadata() {
        let state_dir = test_state_dir("clean-final-response-completion");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session_id = "session-clean-final-response";
        let job_id = "job-clean-final-response";
        let worker_id = "worker-clean-final-response";

        state
            .store
            .create_session(test_session_record(
                session_id,
                "Clean final response",
                &workspace_root,
            ))
            .expect("session should persist");
        state
            .store
            .create_job(JobRecord {
                id: job_id.to_string(),
                session_id: Some(session_id.to_string()),
                parent_job_id: None,
                template_id: None,
                title: "Open PR".to_string(),
                purpose: "Session prompt".to_string(),
                trigger_kind: "session_prompt".to_string(),
                state: "running".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "open a PR to merge to dev".to_string(),
                publication_intent_text: Some("open a PR to merge to dev".to_string()),
            })
            .expect("job should persist");
        let mut worker = state
            .store
            .create_worker(WorkerRecord {
                id: worker_id.to_string(),
                job_id: job_id.to_string(),
                parent_worker_id: None,
                title: "Root utility worker".to_string(),
                lane: "utility".to_string(),
                state: "running".to_string(),
                provider: "openai_compatible".to_string(),
                model: "test-model".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                provider_session_id: String::new(),
                working_dir: workspace_root.display().to_string(),
                read_roots: vec![workspace_root.display().to_string()],
                write_roots: vec![workspace_root.display().to_string()],
                max_steps: 10,
                max_tool_calls: 10,
                max_wall_clock_secs: 30,
            })
            .expect("worker should persist");
        state
            .store
            .update_job(
                job_id,
                JobPatch {
                    root_worker_id: Some(worker_id.to_string()),
                    browser_verification_required: Some(true),
                    browser_verification_status: Some("not_performed".to_string()),
                    ..JobPatch::default()
                },
            )
            .expect("job should update");
        let session = state
            .store
            .get_session(session_id)
            .expect("session should load");
        let metadata = json!({
            "publication_status": "opened",
            "publication_summary": "Opened a ready PR against dev.",
            "pr_url": "https://github.com/WebLime-agency/nucleus/pull/209",
            "source_branch": "fix-209-final-response-contract",
            "target_branch": "dev",
            "validation_status": "passed",
            "browser_verification_status": "not_performed",
            "cleanup_status": "clean"
        });
        let artifacts = vec![FinalAnswerArtifact {
            kind: "implementation_prompt".to_string(),
            title: "Implementation prompt".to_string(),
            content: "Implement the daemon-owned final-response contract.".to_string(),
            metadata: json!({"target": "issue-209"}),
        }];

        complete_job_with_final_answer(
            &state,
            &session,
            job_id,
            &mut worker,
            3,
            1,
            "Opened PR",
            "Published the PR.",
            &metadata,
            &artifacts,
        )
        .await
        .expect("job should complete");

        let session = state
            .store
            .get_session(session_id)
            .expect("session should reload");
        let assistant_turn = session
            .turns
            .iter()
            .find(|turn| turn.role == "assistant")
            .expect("assistant turn should persist");
        assert_eq!(assistant_turn.content, "Published the PR.");
        assert!(!assistant_turn.content.contains("publication_status"));
        assert!(!assistant_turn.content.contains("Browser verification"));
        assert!(!assistant_turn.content.contains("Result:"));
        assert!(!assistant_turn.content.contains("Implementation prompt:"));

        let detail = state.store.get_job(job_id).expect("job should load");
        assert_eq!(detail.job.publication_status, "opened");
        assert_eq!(
            detail.job.pr_url,
            "https://github.com/WebLime-agency/nucleus/pull/209"
        );
        assert_eq!(detail.job.validation_status, "passed");
        assert_eq!(detail.job.browser_verification_status, "not_performed");
        assert_eq!(detail.job.cleanup_status, "clean");
        assert_eq!(detail.artifacts.len(), 1);
        assert_eq!(detail.artifacts[0].kind, "implementation_prompt");
        assert_eq!(detail.artifacts[0].metadata_json["target"], "issue-209");
        let completed = detail
            .events
            .iter()
            .find(|event| event.event_type == "job.completed")
            .expect("completion event should persist");
        assert_eq!(
            completed.data_json["final_response_metadata"]["publication_status"],
            "opened"
        );
        assert_eq!(
            completed.data_json["final_response_artifacts"][0]["kind"],
            "implementation_prompt"
        );
        assert_eq!(
            completed.data_json["final_response_artifacts"][0]["metadata_json"]["target"],
            "issue-209"
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn cleanup_inference_prefers_success_signal_over_temp_path_keyword() {
        assert_eq!(
            infer_cleanup_status("Cleaned up .tmp-playwright after browser verification."),
            Some("cleaned".to_string())
        );
        assert_eq!(
            infer_cleanup_status("Cleanup status: clean; branch clean after removing .tmp-check."),
            Some("clean".to_string())
        );
        assert_eq!(
            infer_cleanup_status("Cleanup required: .tmp-playwright remains."),
            Some("cleanup_required".to_string())
        );
        assert_eq!(
            infer_cleanup_status("Temp files were not cleaned up."),
            Some("cleanup_required".to_string())
        );
        assert_eq!(
            infer_cleanup_status("Cleanup status: cleanup_required"),
            Some("cleanup_required".to_string())
        );
    }

    #[test]
    fn publication_browser_status_reconciliation_preserves_terminal_fallback() {
        let mut job = test_publication_job_summary("publication-browser-terminal");
        job.browser_verification_status = "not_performed".to_string();
        let mut patch = PublicationOutcomePatch {
            publication_requested: Some(true),
            browser_verification_status: Some("pending".to_string()),
            ..PublicationOutcomePatch::default()
        };

        reconcile_publication_browser_status_with_completion(&job, &mut patch);

        assert_eq!(
            patch.browser_verification_status.as_deref(),
            Some("not_performed")
        );

        patch.browser_verification_status = Some("unavailable".to_string());
        reconcile_publication_browser_status_with_completion(&job, &mut patch);
        assert_eq!(
            patch.browser_verification_status.as_deref(),
            Some("unavailable")
        );

        job.browser_verification_status = "passed".to_string();
        reconcile_publication_browser_status_with_completion(&job, &mut patch);
        assert_eq!(patch.browser_verification_status.as_deref(), Some("passed"));
    }

    #[test]
    fn publication_temp_paths_detect_nested_components() {
        assert!(is_publication_temp_path(
            "apps/web/.tmp-playwright/probe.js"
        ));
        assert_eq!(
            publication_temp_root_path("apps/web/.tmp-playwright/probe.js").as_deref(),
            Some("apps/web/.tmp-playwright")
        );
        assert_eq!(
            publication_temp_root_path("./pkg/.playwright-check/log.txt").as_deref(),
            Some("pkg/.playwright-check")
        );
        assert!(!is_publication_temp_path(
            "apps/web/tmp-playwright/probe.js"
        ));
    }

    #[test]
    fn collect_repo_temp_paths_includes_nested_and_ignored_paths() {
        let root = test_state_dir("publication-temp-path-collection");
        fs::create_dir_all(root.join("apps/web/.tmp-playwright"))
            .expect("nested temp dir should exist");
        fs::write(root.join("apps/web/.tmp-playwright/probe.txt"), "temp")
            .expect("nested temp file should exist");
        fs::create_dir_all(root.join("ignored/.tmp-check")).expect("ignored temp dir should exist");
        fs::write(root.join("ignored/.tmp-check/probe.txt"), "temp")
            .expect("ignored temp file should exist");
        fs::write(root.join(".gitignore"), "ignored/.tmp-check/\n")
            .expect("gitignore should exist");

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .expect("git init should run");
        assert!(status.success());

        let paths = collect_repo_temp_paths(&root.display().to_string());

        assert!(paths.iter().any(|path| path == "apps/web/.tmp-playwright"));
        assert!(paths.iter().any(|path| path == "ignored/.tmp-check"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collect_repo_temp_paths_includes_staged_and_tracked_paths() {
        let root = test_state_dir("publication-temp-path-tracked-collection");
        fs::create_dir_all(root.join("apps/web/.tmp-playwright"))
            .expect("nested temp dir should exist");
        fs::write(root.join("apps/web/.tmp-playwright/probe.txt"), "temp")
            .expect("nested temp file should exist");
        fs::create_dir_all(root.join("pkg/.tmp-check")).expect("tracked temp dir should exist");
        fs::write(root.join("pkg/.tmp-check/probe.txt"), "temp")
            .expect("tracked temp file should exist");

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .expect("git init should run");
        assert!(status.success());

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args([
                "add",
                "apps/web/.tmp-playwright/probe.txt",
                "pkg/.tmp-check/probe.txt",
            ])
            .status()
            .expect("git add should run");
        assert!(status.success());

        let paths = collect_repo_temp_paths(&root.display().to_string());

        assert!(paths.iter().any(|path| path == "apps/web/.tmp-playwright"));
        assert!(paths.iter().any(|path| path == "pkg/.tmp-check"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collect_repo_temp_paths_includes_renamed_temp_destinations() {
        let root = test_state_dir("publication-temp-path-renamed-collection");
        fs::create_dir_all(root.join("src")).expect("src dir should exist");
        fs::create_dir_all(root.join("apps/web/.tmp-playwright"))
            .expect("nested temp dir should exist");
        fs::write(root.join("src/probe.txt"), "temp").expect("tracked file should exist");

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .expect("git init should run");
        assert!(status.success());

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "src/probe.txt"])
            .status()
            .expect("git add should run");
        assert!(status.success());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.local",
            ])
            .args(["commit", "-qm", "seed"])
            .status()
            .expect("git commit should run");
        assert!(status.success());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["mv", "src/probe.txt", "apps/web/.tmp-playwright/probe.txt"])
            .status()
            .expect("git mv should run");
        assert!(status.success());

        let paths = collect_repo_temp_paths(&root.display().to_string());

        assert!(paths.iter().any(|path| path == "apps/web/.tmp-playwright"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collect_repo_temp_paths_resolves_git_paths_from_repo_root() {
        let root = test_state_dir("publication-temp-path-scoped-workdir");
        let scoped_root = root.join("apps/web");
        fs::create_dir_all(scoped_root.join(".tmp-playwright"))
            .expect("scoped temp dir should exist");
        fs::write(scoped_root.join(".tmp-playwright/probe.txt"), "temp")
            .expect("scoped temp file should exist");

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .expect("git init should run");
        assert!(status.success());

        let paths = collect_repo_temp_paths(&scoped_root.display().to_string());

        assert!(paths.iter().any(|path| path == "apps/web/.tmp-playwright"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collect_repo_temp_paths_resolves_tracked_paths_from_repo_root() {
        let root = test_state_dir("publication-temp-path-scoped-tracked-workdir");
        let scoped_root = root.join("apps/web");
        fs::create_dir_all(scoped_root.join("src/.tmp-check"))
            .expect("scoped tracked temp dir should exist");
        fs::write(scoped_root.join("src/.tmp-check/probe.txt"), "temp")
            .expect("scoped tracked temp file should exist");

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .expect("git init should run");
        assert!(status.success());

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "apps/web/src/.tmp-check/probe.txt"])
            .status()
            .expect("git add should run");
        assert!(status.success());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.local",
            ])
            .args(["commit", "-qm", "seed"])
            .status()
            .expect("git commit should run");
        assert!(status.success());

        let paths = collect_repo_temp_paths(&scoped_root.display().to_string());

        assert!(paths.iter().any(|path| path == "apps/web/src/.tmp-check"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn publication_temp_hygiene_marks_new_repo_tmp_leftovers() {
        let root = test_state_dir("publication-temp-hygiene");
        fs::create_dir_all(root.join(".tmp-before")).expect("baseline temp dir should exist");
        fs::create_dir_all(root.join(".tmp-playwright")).expect("new temp dir should exist");

        let detail = JobDetail {
            job: test_publication_job_summary("publication-temp-hygiene"),
            workers: Vec::new(),
            child_jobs: Vec::new(),
            tool_calls: Vec::new(),
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: vec![nucleus_protocol::JobEvent {
                id: 1,
                job_id: "publication-temp-hygiene".to_string(),
                worker_id: None,
                event_type: "job.publication.git_baseline".to_string(),
                status: "captured".to_string(),
                summary: "Captured baseline".to_string(),
                detail: String::new(),
                data_json: json!({ "temp_paths": [".tmp-before"] }),
                created_at: 0,
            }],
        };
        let mut patch = PublicationOutcomePatch {
            publication_requested: Some(true),
            cleanup_status: Some("clean".to_string()),
            cleanup_paths: Some(Vec::new()),
            ..PublicationOutcomePatch::default()
        };

        apply_publication_temp_hygiene(&detail, &root.display().to_string(), &mut patch);

        assert_eq!(patch.cleanup_status.as_deref(), Some("cleanup_required"));
        assert_eq!(
            patch.cleanup_paths,
            Some(vec![".tmp-playwright".to_string()])
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn publication_temp_hygiene_uses_baseline_working_dir() {
        let root = test_state_dir("publication-temp-hygiene-working-dir");
        let baseline_root = root.join("baseline-root");
        let completion_root = root.join("completion-root");
        fs::create_dir_all(baseline_root.join(".tmp-before"))
            .expect("baseline temp dir should exist");
        fs::create_dir_all(baseline_root.join(".tmp-playwright"))
            .expect("new temp dir should exist");
        fs::create_dir_all(&completion_root).expect("completion root should exist");

        let detail = JobDetail {
            job: test_publication_job_summary("publication-temp-hygiene-working-dir"),
            workers: Vec::new(),
            child_jobs: Vec::new(),
            tool_calls: Vec::new(),
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: vec![nucleus_protocol::JobEvent {
                id: 1,
                job_id: "publication-temp-hygiene-working-dir".to_string(),
                worker_id: None,
                event_type: "job.publication.git_baseline".to_string(),
                status: "captured".to_string(),
                summary: "Captured baseline".to_string(),
                detail: String::new(),
                data_json: json!({
                    "working_dir": baseline_root.display().to_string(),
                    "temp_paths": [".tmp-before"],
                }),
                created_at: 0,
            }],
        };
        let mut patch = PublicationOutcomePatch {
            publication_requested: Some(true),
            cleanup_status: Some("clean".to_string()),
            cleanup_paths: Some(Vec::new()),
            ..PublicationOutcomePatch::default()
        };

        apply_publication_temp_hygiene(&detail, &completion_root.display().to_string(), &mut patch);

        assert_eq!(patch.cleanup_status.as_deref(), Some("cleanup_required"));
        assert_eq!(
            patch.cleanup_paths,
            Some(vec![".tmp-playwright".to_string()])
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn publication_temp_hygiene_skips_without_baseline_event() {
        let root = test_state_dir("publication-temp-hygiene-no-baseline");
        fs::create_dir_all(root.join(".tmp-playwright")).expect("existing temp dir should exist");

        let detail = JobDetail {
            job: test_publication_job_summary("publication-temp-hygiene-no-baseline"),
            workers: Vec::new(),
            child_jobs: Vec::new(),
            tool_calls: Vec::new(),
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: Vec::new(),
        };
        let mut patch = PublicationOutcomePatch {
            publication_requested: Some(true),
            cleanup_status: Some("clean".to_string()),
            cleanup_paths: Some(Vec::new()),
            ..PublicationOutcomePatch::default()
        };

        apply_publication_temp_hygiene(&detail, &root.display().to_string(), &mut patch);

        assert_eq!(patch.cleanup_status.as_deref(), Some("clean"));
        assert_eq!(patch.cleanup_paths, Some(Vec::new()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incomplete_progress_retry_prompt_requires_continuation() {
        let prompt = build_incomplete_progress_retry_prompt(
            "Phase 4 is not complete yet",
            "Remaining work: split the detail sidebar.",
        );

        assert!(prompt.contains("progress report rather than a completion answer"));
        assert!(prompt.contains("Continue with the next smallest useful tool_call"));
        assert!(prompt.contains(
            "Only return final_answer when the user's requested phase/task is fully complete"
        ));
    }

    #[test]
    fn progress_update_continuation_prompt_keeps_job_running() {
        let prompt = build_progress_update_continuation_prompt(
            "checkpoint saved",
            "Composer extraction is complete; continue with sidebar extraction.",
        );

        assert!(prompt.contains("non-terminal progress checkpoint"));
        assert!(prompt.contains("Continue working from this checkpoint"));
        assert!(prompt.contains("Use final_answer only when the requested task is complete"));
    }

    #[test]
    fn internal_action_item_retry_prompt_requires_an_action_or_real_answer() {
        let prompt = build_internal_action_item_retry_prompt(
            "Provided the next single step requested by the user",
            "Next single step: inspect the workspace.",
        );

        assert!(prompt.contains("not a user-facing answer"));
        assert!(prompt.contains("return a tool_call"));
        assert!(
            prompt.contains("Only return final_answer when the text directly answers the user")
        );
    }

    #[test]
    fn detects_command_ports() {
        let mut spec = ResolvedCommandSpec {
            mode: "interactive".to_string(),
            title: "Dev server".to_string(),
            command: "npm".to_string(),
            args: vec![
                "run".to_string(),
                "dev".to_string(),
                "--".to_string(),
                "--port".to_string(),
                "5173".to_string(),
            ],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };
        assert_eq!(detect_command_port(&spec), Some(5173));

        spec.args = vec!["-lc".to_string(), "PORT=5202 npm run dev".to_string()];
        spec.command = "sh".to_string();
        assert_eq!(detect_command_port(&spec), Some(5202));

        spec.args = vec![
            "run".to_string(),
            "dev".to_string(),
            "--port=5174".to_string(),
        ];
        spec.command = "npm".to_string();
        assert_eq!(detect_command_port(&spec), Some(5174));

        spec.args = vec!["run".to_string(), "dev".to_string(), "-p5175".to_string()];
        assert_eq!(detect_command_port(&spec), Some(5175));

        spec.env.insert("PORT".to_string(), "5176".to_string());
        spec.args = vec!["run".to_string(), "dev".to_string()];
        assert_eq!(detect_command_port(&spec), Some(5176));

        spec.env.clear();
        spec.command = "sh".to_string();
        spec.args = vec!["-lc".to_string(), "npm run dev -- --port 5177".to_string()];
        assert_eq!(detect_command_port(&spec), Some(5177));

        spec.args = vec!["-lc".to_string(), "npm run dev -- --port=5178".to_string()];
        assert_eq!(detect_command_port(&spec), Some(5178));

        spec.args = vec!["-lc".to_string(), "PORT=5179 npm run dev".to_string()];
        assert_eq!(detect_command_port(&spec), Some(5179));

        spec.args = vec!["-lc".to_string(), "npm run dev -- -p 5180".to_string()];
        assert_eq!(detect_command_port(&spec), Some(5180));

        spec.args = vec!["-lc".to_string(), "next dev -p 3000".to_string()];
        assert_eq!(detect_command_port(&spec), Some(3000));

        spec.args = vec![
            "-lc".to_string(),
            "vite --host 127.0.0.1 -p5181".to_string(),
        ];
        assert_eq!(detect_command_port(&spec), Some(5181));

        spec.command = "bash".to_string();
        spec.args = vec![
            "--norc".to_string(),
            "-lc".to_string(),
            "npm run dev -- -p 5182".to_string(),
        ];
        assert_eq!(detect_command_port(&spec), Some(5182));
    }

    #[test]
    fn command_port_detection_ignores_non_port_flag_substrings() {
        let spec = ResolvedCommandSpec {
            mode: "interactive".to_string(),
            title: "Shell command".to_string(),
            command: "sh".to_string(),
            args: vec![
                "-lc".to_string(),
                "printf ready && grep -p 5192 package.json".to_string(),
            ],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };
        assert_eq!(detect_command_port(&spec), None);
    }

    #[test]
    fn declared_client_ports_do_not_trigger_listener_preflight() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
        let port = listener
            .local_addr()
            .expect("listener addr should be available")
            .port();

        let client = ResolvedCommandSpec {
            mode: "interactive".to_string(),
            title: "Database client".to_string(),
            command: "psql".to_string(),
            args: vec!["--port".to_string(), port.to_string()],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };
        assert_eq!(detect_command_port(&client), Some(port));
        assert!(ensure_declared_command_port_available(&client).is_ok());

        let shell_client = ResolvedCommandSpec {
            mode: "interactive".to_string(),
            title: "Database client".to_string(),
            command: "sh".to_string(),
            args: vec!["-lc".to_string(), format!("psql --port {port}")],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };
        assert_eq!(detect_command_port(&shell_client), Some(port));
        assert!(ensure_declared_command_port_available(&shell_client).is_ok());
    }

    #[test]
    fn declared_ipv6_loopback_command_port_fails_preflight() {
        let listener = match std::net::TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::AddrNotAvailable => return,
            Err(error) => panic!("test IPv6 listener should bind or be unavailable: {error}"),
        };
        let port = listener
            .local_addr()
            .expect("listener addr should be available")
            .port();
        let spec = ResolvedCommandSpec {
            mode: "interactive".to_string(),
            title: "Dev server".to_string(),
            command: "sh".to_string(),
            args: vec!["-lc".to_string(), format!("npm run dev -- --port {port}")],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };

        assert_eq!(detect_command_port(&spec), Some(port));
        let error = ensure_declared_command_port_available(&spec)
            .expect_err("occupied IPv6 loopback port should fail preflight");
        assert!(error.to_string().contains("already in use"));
    }

    #[test]
    fn declared_ipv4_loopback_host_ignores_occupied_ipv6_loopback() {
        let listener = match std::net::TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::AddrNotAvailable => return,
            Err(error) => panic!("test IPv6 listener should bind or be unavailable: {error}"),
        };
        let port = listener
            .local_addr()
            .expect("listener addr should be available")
            .port();
        let ipv4_probe = match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => listener,
            Err(_) => return,
        };
        drop(ipv4_probe);

        let spec = ResolvedCommandSpec {
            mode: "interactive".to_string(),
            title: "Dev server".to_string(),
            command: "sh".to_string(),
            args: vec![
                "-lc".to_string(),
                format!("vite --host 127.0.0.1 --port {port}"),
            ],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };

        assert_eq!(detect_command_port(&spec), Some(port));
        assert_eq!(command_port_preflight_hosts(&spec), vec!["127.0.0.1"]);
        assert!(ensure_declared_command_port_available(&spec).is_ok());
    }

    #[test]
    fn declared_next_hostname_ignores_occupied_ipv6_loopback() {
        let listener = match std::net::TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::AddrNotAvailable => return,
            Err(error) => panic!("test IPv6 listener should bind or be unavailable: {error}"),
        };
        let port = listener
            .local_addr()
            .expect("listener addr should be available")
            .port();
        let ipv4_probe = match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => listener,
            Err(_) => return,
        };
        drop(ipv4_probe);

        let spec = ResolvedCommandSpec {
            mode: "interactive".to_string(),
            title: "Dev server".to_string(),
            command: "sh".to_string(),
            args: vec![
                "-lc".to_string(),
                format!("next dev -H 127.0.0.1 -p {port}"),
            ],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };

        assert_eq!(detect_command_port(&spec), Some(port));
        assert_eq!(command_port_preflight_hosts(&spec), vec!["127.0.0.1"]);
        assert!(ensure_declared_command_port_available(&spec).is_ok());
    }

    #[test]
    fn detects_risky_git_commands() {
        let mut spec = ResolvedCommandSpec {
            mode: "oneshot".to_string(),
            title: "Command".to_string(),
            command: "git".to_string(),
            args: vec!["switch".to_string(), "feature".to_string()],
            cwd: PathBuf::from("/tmp"),
            timeout_secs: 30,
            output_limit_bytes: 1024,
            network_policy: "inherit".to_string(),
            env: BTreeMap::new(),
        };
        assert!(is_risky_git_command(&spec));

        spec.command = "sh".to_string();
        spec.args = vec!["-lc".to_string(), "git reset --hard".to_string()];
        assert!(is_risky_git_command(&spec));

        spec.args = vec!["-lc".to_string(), "git status".to_string()];
        assert!(!is_risky_git_command(&spec));
    }

    #[test]
    fn parses_provider_native_shell_tool_call_as_command_run() {
        let action = parse_worker_action(
            r#"{"tool_call":{"tool_name":"shell","arguments":{"command":"pwd && ps -ef | grep -i stfr","cwd":"stfr","timeout_secs":20}}}"#,
        )
        .expect("provider-native shell call should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "sh");
        assert_eq!(args["args"][0], "-lc");
        assert_eq!(args["args"][1], "pwd && ps -ef | grep -i stfr");
        assert_eq!(args["cwd"], "stfr");
        assert_eq!(args["timeout_secs"], 20);
    }

    #[test]
    fn parses_provider_native_inline_shell_argv_tool_call_as_command_run() {
        let action = parse_worker_action(
            r#"{"tool_call":{"tool":"shell","command":["bash","-lc","rg -n \"uhm|UHM|Uhm\" ."],"workdir":"/home/eba/dev-projects/dga-clients"}}"#,
        )
        .expect("inline provider-native shell argv call should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "bash");
        assert_eq!(args["args"][0], "-lc");
        assert_eq!(args["args"][1], "rg -n \"uhm|UHM|Uhm\" .");
        assert_eq!(args["cwd"], "/home/eba/dev-projects/dga-clients");
    }

    #[test]
    fn parses_provider_native_inline_shell_command_string_as_command_run() {
        let action = parse_worker_action(
            r#"{"tool_call":{"tool":"shell","command":"cd /home/eba/dev-projects/dga-clients && for d in dga-stfr dga-uhm; do echo \"$d\"; done"}}"#,
        )
        .expect("inline provider-native shell command string should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "sh");
        assert_eq!(args["args"][0], "-lc");
        assert_eq!(
            args["args"][1],
            "cd /home/eba/dev-projects/dga-clients && for d in dga-stfr dga-uhm; do echo \"$d\"; done"
        );
    }

    #[test]
    fn parses_provider_native_shell_command_with_unescaped_find_parentheses() {
        let action = parse_worker_action(
            r#"{"tool_call":{"tool":"shell","arguments":{"command":["bash","-lc","find dga-uhm/src -maxdepth 3 \( -type f -o -type d \) | sort"]}}}"#,
        )
        .expect("shell command with invalid JSON shell escapes should normalize");

        let WorkerAction::ToolCall { tool, args, .. } = action else {
            panic!("expected tool call");
        };

        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "bash");
        assert_eq!(args["args"][0], "-lc");
        assert_eq!(
            args["args"][1],
            r#"find dga-uhm/src -maxdepth 3 \( -type f -o -type d \) | sort"#
        );
    }

    #[test]
    fn parses_provider_native_shell_command_with_literal_newlines() {
        let action = parse_worker_action(
            "{\"tool_call\":{\"tool\":\"shell\",\"arguments\":{\"command\":[\"bash\",\"-lc\",\"printf '\n--- package.json ---\n' && cat dga-uhm/package.json\"]}}}",
        )
        .expect("shell command with literal newlines should normalize");

        let WorkerAction::ToolCall { tool, args, .. } = action else {
            panic!("expected tool call");
        };

        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "bash");
        assert_eq!(args["args"][0], "-lc");
        assert_eq!(
            args["args"][1],
            "printf '\n--- package.json ---\n' && cat dga-uhm/package.json"
        );
    }

    #[test]
    fn parses_provider_native_action_tool_input_shell_call_as_command_run() {
        let action = parse_worker_action(
            r#"{"action":"tool_call","tool":"shell","input":"cd /home/eba/dev-projects/dga-clients && pwd && ls -la"}"#,
        )
        .expect("provider-native action/tool/input shell call should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "sh");
        assert_eq!(args["args"][0], "-lc");
        assert_eq!(
            args["args"][1],
            "cd /home/eba/dev-projects/dga-clients && pwd && ls -la"
        );
    }

    #[test]
    fn parses_provider_native_type_tool_input_object_shell_call_as_command_run() {
        let action = parse_worker_action(
            r#"{"type":"tool_call","tool":"shell","input":{"command":["bash","-lc","rg -n \"normalize_worker_tool_call_value\" crates/daemon/src"],"workdir":"/home/eba/dev-projects/nucleus"}}"#,
        )
        .expect("provider-native type/tool/input shell call should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "bash");
        assert_eq!(args["args"][0], "-lc");
        assert_eq!(
            args["args"][1],
            "rg -n \"normalize_worker_tool_call_value\" crates/daemon/src"
        );
        assert_eq!(args["cwd"], "/home/eba/dev-projects/nucleus");
    }

    #[test]
    fn worker_action_repair_prompt_targets_contract_shape() {
        let prompt = build_worker_action_repair_prompt(
            r#"{"message":"I should inspect the repo next"}"#,
            &crate::worker_action::WorkerActionParseError::InvalidActionShape,
            &[
                "cloudflare-api.search".to_string(),
                "command.run".to_string(),
            ],
        );

        assert!(prompt.contains("Nucleus action contract"));
        assert!(prompt.contains("\"kind\":\"final_answer\""));
        assert!(prompt.contains("\"tool\":\"command.run\""));
        assert!(prompt.contains("\"kind\":\"progress_update\""));
        assert!(prompt.contains("\"kind\":\"spawn_child_jobs\""));
        assert!(prompt.contains("exactly one valid Nucleus worker action"));
        assert!(prompt.contains("cloudflare-api.search"));
        assert!(prompt.contains("preserve that exact tool ID"));
        assert!(prompt.contains("Do not replace a supported non-command tool with command.run"));
    }

    #[tokio::test]
    async fn call_worker_model_repairs_invalid_action_shape_on_second_turn() {
        let state_dir = test_state_dir("worker-action-repair-flow");
        let state = initialize_test_state(&state_dir);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let request_count = Arc::new(AtomicUsize::new(0));
        let request_bodies = Arc::new(TestMutex::new(Vec::new()));
        let server_request_count = request_count.clone();
        let server_request_bodies = request_bodies.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("test request should connect");
                let body = read_test_http_body(&mut socket).await;
                server_request_bodies
                    .lock()
                    .expect("request bodies lock should not be poisoned")
                    .push(body);
                let index = server_request_count.fetch_add(1, Ordering::SeqCst);
                let content = if index == 0 {
                    r#"{"message":"I should inspect the repo next"}"#
                } else {
                    r#"{"kind":"final_answer","summary":"repaired action","final_answer":"Done."}"#
                };
                write_test_openai_sse_response(&mut socket, &format!("turn-{index}"), content)
                    .await;
            }
        });

        let mut worker = test_worker_summary("repair-worker", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = state_dir.display().to_string();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let response = call_worker_model(
            &state,
            None,
            &worker,
            &[],
            "Inspect the repo.",
            &[],
            &mut cancel_rx,
        )
        .await
        .expect("repair turn should produce a valid action");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            ..
        } = response.action
        else {
            panic!("expected repaired final answer");
        };
        assert_eq!(summary, "repaired action");
        assert_eq!(final_answer, "Done.");
        assert_eq!(response.provider_session_id, "turn-1");

        server.await.expect("test server should finish");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let bodies = request_bodies
            .lock()
            .expect("request bodies lock should not be poisoned");
        assert_eq!(bodies.len(), 2);
        assert!(bodies[0].contains("Inspect the repo."));
        assert!(bodies[1].contains("Nucleus action contract"));
        assert!(bodies[1].contains("I should inspect the repo next"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn call_worker_model_retries_500_once_and_records_audit() {
        let state_dir = test_state_dir("worker-provider-retry-500-once");
        let state = initialize_test_state(&state_dir);
        let (base_url, request_count, server) = spawn_retry_openai_server(vec![
            TestOpenAiProviderResponse {
                status: 500,
                retry_after_secs: None,
                body: r#"{"error":"temporary"}"#,
                content: None,
            },
            TestOpenAiProviderResponse {
                status: 200,
                retry_after_secs: None,
                body: "",
                content: Some(
                    r#"{"kind":"final_answer","summary":"retried","final_answer":"Done."}"#,
                ),
            },
        ])
        .await;

        let mut worker = test_worker_summary("provider-retry-500", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = state_dir.display().to_string();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let response =
            call_worker_model(&state, None, &worker, &[], "Try once.", &[], &mut cancel_rx)
                .await
                .expect("transient 500 should retry and succeed");

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert!(matches!(
            response.action,
            WorkerAction::FinalAnswer { final_answer, .. } if final_answer == "Done."
        ));
        let audits = state.store.list_audit_events(20).expect("audit lists");
        assert!(
            audits.iter().any(|event| {
                event.kind == "worker.provider.retry"
                    && event.target == worker.id
                    && event.summary.contains("http_500")
            }),
            "expected worker.provider.retry audit event, got {audits:?}"
        );

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn call_worker_model_honors_retry_after_header() {
        let state_dir = test_state_dir("worker-provider-retry-after");
        let state = initialize_test_state(&state_dir);
        let (base_url, request_count, server) = spawn_retry_openai_server(vec![
            TestOpenAiProviderResponse {
                status: 429,
                retry_after_secs: Some(5),
                body: r#"{"error":"rate limited"}"#,
                content: None,
            },
            TestOpenAiProviderResponse {
                status: 200,
                retry_after_secs: None,
                body: "",
                content: Some(
                    r#"{"kind":"final_answer","summary":"retried","final_answer":"Done."}"#,
                ),
            },
        ])
        .await;

        let mut worker = test_worker_summary("provider-retry-after", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = state_dir.display().to_string();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let started = Instant::now();
        call_worker_model(
            &state,
            None,
            &worker,
            &[],
            "Try later.",
            &[],
            &mut cancel_rx,
        )
        .await
        .expect("429 with retry-after should retry and succeed");

        assert!(
            started.elapsed() >= Duration::from_secs(5),
            "retry-after backoff should delay the second attempt"
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 2);

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn call_worker_model_does_not_retry_401() {
        let state_dir = test_state_dir("worker-provider-retry-401");
        let state = initialize_test_state(&state_dir);
        let (base_url, request_count, server) =
            spawn_retry_openai_server(vec![TestOpenAiProviderResponse {
                status: 401,
                retry_after_secs: None,
                body: r#"{"error":"bad key"}"#,
                content: None,
            }])
            .await;

        let mut worker = test_worker_summary("provider-retry-401", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = state_dir.display().to_string();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let error = call_worker_model(&state, None, &worker, &[], "Try auth.", &[], &mut cancel_rx)
            .await
            .expect_err("401 should fail immediately");

        assert!(error.to_string().contains("401"));
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert!(
            !state
                .store
                .list_audit_events(20)
                .expect("audit lists")
                .iter()
                .any(|event| event.kind == "worker.provider.retry")
        );

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn call_worker_model_gives_up_after_max_retries() {
        let state_dir = test_state_dir("worker-provider-retry-max");
        let state = initialize_test_state(&state_dir);
        let responses = vec![
            TestOpenAiProviderResponse {
                status: 500,
                retry_after_secs: None,
                body: r#"{"error":"still down"}"#,
                content: None,
            };
            (crate::retry::MAX_RETRY_ATTEMPTS + 1) as usize
        ];
        let (base_url, request_count, server) = spawn_retry_openai_server(responses).await;

        let mut worker = test_worker_summary("provider-retry-max", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = state_dir.display().to_string();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let error = call_worker_model(
            &state,
            None,
            &worker,
            &[],
            "Try until cap.",
            &[],
            &mut cancel_rx,
        )
        .await
        .expect_err("permanent 500 should give up");

        assert!(error.to_string().contains("500"));
        assert_eq!(
            request_count.load(Ordering::SeqCst),
            (crate::retry::MAX_RETRY_ATTEMPTS + 1) as usize
        );
        let audits = state.store.list_audit_events(20).expect("audit lists");
        assert_eq!(
            audits
                .iter()
                .filter(|event| event.kind == "worker.provider.retry")
                .count(),
            crate::retry::MAX_RETRY_ATTEMPTS as usize
        );

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn call_worker_model_cancellation_aborts_retry_backoff() {
        let state_dir = test_state_dir("worker-provider-retry-cancel");
        let state = initialize_test_state(&state_dir);
        let (base_url, request_count, server) =
            spawn_retry_openai_server(vec![TestOpenAiProviderResponse {
                status: 429,
                retry_after_secs: Some(5),
                body: r#"{"error":"rate limited"}"#,
                content: None,
            }])
            .await;

        let mut worker = test_worker_summary("provider-retry-cancel", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = state_dir.display().to_string();
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let model_call = call_worker_model(
            &state,
            None,
            &worker,
            &[],
            "Try then cancel.",
            &[],
            &mut cancel_rx,
        );
        let cancel_after_first_retry = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_tx.send(true).expect("cancel signal should send");
        };
        let (result, _) = tokio::join!(model_call, cancel_after_first_retry);

        let error = result.expect_err("cancel should abort retry backoff");
        assert!(error.to_string().contains("canceled"));
        assert_eq!(request_count.load(Ordering::SeqCst), 1);

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn call_worker_model_accepts_sse_content_without_done_marker() {
        let state_dir = test_state_dir("worker-provider-sse-no-done");
        let state = initialize_test_state(&state_dir);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("test request should connect");
            let _ = read_test_http_body(&mut socket).await;
            write_test_openai_sse_response_without_done(
                &mut socket,
                "no-done-turn",
                r#"{"kind":"final_answer","summary":"answered","final_answer":"Done."}"#,
            )
            .await;
        });

        let mut worker = test_worker_summary("provider-sse-no-done", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = state_dir.display().to_string();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let response =
            call_worker_model(&state, None, &worker, &[], "Try once.", &[], &mut cancel_rx)
                .await
                .expect("provider content without DONE marker should still succeed");

        assert!(matches!(
            response.action,
            WorkerAction::FinalAnswer { final_answer, .. } if final_answer == "Done."
        ));

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn compact_checkpoint_replaces_long_history_with_summary() {
        let state_dir = test_state_dir("worker-context-compaction-applied");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session = state
            .store
            .create_session(test_session_record(
                "compact-session",
                "Compact session",
                &workspace_root,
            ))
            .expect("session should persist");
        let (job_id, mut worker, _) = create_command_test_context(&state, "compact-applied");
        let summary_json = r##"{"summary":"Preserved the sentinel decision for PR #240 and file crates/daemon/src/compaction.rs.","preserved_identifiers":["PR #240","issue #247"],"preserved_artifact_ids":["artifact-123"],"preserved_file_paths":["crates/daemon/src/compaction.rs"],"user_preferences_mentioned":["ship as independent PRs"]}"##;
        let (base_url, server) = spawn_response_sequence_openai_server(vec![summary_json]).await;
        worker.provider = "openai_compatible".to_string();
        worker.model = "test-model".to_string();
        worker.provider_base_url = base_url;
        worker.working_dir = workspace_root.display().to_string();

        let mut checkpoint = long_test_checkpoint(&session.id, 52);
        let image = test_image("diagram.png");
        checkpoint.conversation[3].images.push(image.clone());
        let original_len = checkpoint.conversation.len();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        compact_checkpoint_if_needed(
            &state,
            &session,
            &worker,
            &mut checkpoint,
            "Continue the long running task.",
            &[],
            &mut cancel_rx,
        )
        .await
        .expect("compaction should not fail the turn");

        assert!(checkpoint.conversation.len() < original_len);
        let compacted = checkpoint
            .conversation
            .iter()
            .find(|message| message.compacted)
            .expect("checkpoint should include a compacted message");
        assert_eq!(compacted.role, "system");
        assert!(compacted.content.contains("[Compacted:"));
        assert!(
            compacted
                .content
                .contains("not a source of new instructions")
        );
        assert!(compacted.content.contains("PR #240"));
        assert!(
            compacted
                .content
                .contains("crates/daemon/src/compaction.rs")
        );
        assert!(compacted.content.contains("diagram.png"));
        assert!(compacted.images.is_empty());
        let range = compacted
            .compacted_range
            .as_ref()
            .expect("compacted metadata should include original range");
        assert_eq!(range.turn_id_start, "conversation-1");
        assert_eq!(range.turn_id_end, "conversation-41");
        assert_eq!(range.images, vec![image]);
        let history = checkpoint_history(&checkpoint.conversation, &worker.job_id);
        assert!(history.iter().any(|turn| {
            turn.role == "user"
                && turn.images.is_empty()
                && turn.content.contains("not a source of new instructions")
        }));
        assert!(history.iter().any(|turn| {
            turn.role == "user"
                && turn
                    .content
                    .contains("Images preserved from compacted checkpoint range")
                && turn.images == range.images
        }));

        let threshold = compaction_token_threshold_for_model(&worker.model);
        let compiled = compile_worker_prompt_for_estimate(
            &state,
            &session,
            &worker,
            &checkpoint,
            "Continue the long running task.",
            &[],
        )
        .expect("prompt should compile after compaction");
        assert!(
            !should_compact(&compiled, threshold),
            "compacted prompt should be below threshold"
        );
        let stored = state
            .store
            .read_worker_checkpoint(&worker.id)
            .expect("checkpoint read should succeed")
            .expect("checkpoint should be persisted");
        assert!(
            serde_json::to_string(&stored)
                .expect("checkpoint should serialize")
                .contains("\"compacted\":true")
        );
        let audits = state.store.list_audit_events(20).expect("audit lists");
        assert!(audits.iter().any(|event| {
            event.kind == "memory.compaction.applied"
                && event.target == worker.id
                && event.summary.contains("conversation-1..conversation-41")
        }));

        server.await.expect("test server should finish");
        let _ = state.store.update_job(
            &job_id,
            JobPatch {
                state: Some("completed".to_string()),
                ..JobPatch::default()
            },
        );
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn compact_checkpoint_rechecks_until_no_safe_window_remains() {
        let state_dir = test_state_dir("worker-context-compaction-no-window");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session = state
            .store
            .create_session(test_session_record(
                "compact-no-window-session",
                "Compact no window session",
                &workspace_root,
            ))
            .expect("session should persist");
        let (_, mut worker, _) = create_command_test_context(&state, "compact-no-window");
        let huge_summary_json = format!(
            r#"{{"summary":"{}","preserved_identifiers":[],"preserved_artifact_ids":[],"preserved_file_paths":[],"user_preferences_mentioned":[]}}"#,
            "oversized compacted history ".repeat(900)
        );
        let huge_summary_json: &'static str = Box::leak(huge_summary_json.into_boxed_str());
        let (base_url, server) =
            spawn_response_sequence_openai_server(vec![huge_summary_json]).await;
        worker.provider = "openai_compatible".to_string();
        worker.model = "test-model".to_string();
        worker.provider_base_url = base_url;
        worker.working_dir = workspace_root.display().to_string();

        let mut checkpoint = long_test_checkpoint(&session.id, 52);
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        compact_checkpoint_if_needed(
            &state,
            &session,
            &worker,
            &mut checkpoint,
            "Continue the long running task.",
            &[],
            &mut cancel_rx,
        )
        .await
        .expect("compaction should stop cleanly when no further safe window remains");

        assert!(
            checkpoint
                .conversation
                .iter()
                .any(|message| message.compacted)
        );
        assert!(
            crate::compaction::select_compaction_window(&checkpoint).is_none(),
            "second pass should discover that no safe compaction window remains"
        );
        let threshold = compaction_token_threshold_for_model(&worker.model);
        let compiled = compile_worker_prompt_for_estimate(
            &state,
            &session,
            &worker,
            &checkpoint,
            "Continue the long running task.",
            &[],
        )
        .expect("prompt should compile after oversized compaction");
        assert!(
            should_compact(&compiled, threshold),
            "oversized summary should still exceed the threshold"
        );
        let audits = state.store.list_audit_events(20).expect("audit lists");
        assert!(audits.iter().any(|event| {
            event.kind == "memory.compaction.failed"
                && event.target == worker.id
                && event.summary.contains("no safe compaction window")
        }));

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn compact_checkpoint_malformed_summary_preserves_history_and_audits() {
        let state_dir = test_state_dir("worker-context-compaction-malformed");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session = state
            .store
            .create_session(test_session_record(
                "compact-malformed-session",
                "Compact malformed session",
                &workspace_root,
            ))
            .expect("session should persist");
        let (_, mut worker, _) = create_command_test_context(&state, "compact-malformed");
        let (base_url, server) = spawn_response_sequence_openai_server(vec!["not-json"]).await;
        worker.provider = "openai_compatible".to_string();
        worker.model = "test-model".to_string();
        worker.provider_base_url = base_url;
        worker.working_dir = workspace_root.display().to_string();

        let mut checkpoint = long_test_checkpoint(&session.id, 52);
        let original = checkpoint.conversation.clone();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        compact_checkpoint_if_needed(
            &state,
            &session,
            &worker,
            &mut checkpoint,
            "Continue the long running task.",
            &[],
            &mut cancel_rx,
        )
        .await
        .expect("malformed compaction output should not fail the turn");

        assert_eq!(checkpoint.conversation, original);
        assert!(
            !checkpoint
                .conversation
                .iter()
                .any(|message| message.compacted)
        );
        let audits = state.store.list_audit_events(20).expect("audit lists");
        assert!(audits.iter().any(|event| {
            event.kind == "memory.compaction.failed"
                && event.target == worker.id
                && event.summary.contains("malformed")
        }));

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    /// Regression for #232: until now `start_prompt_job` built a CompiledTurn
    /// for debug summaries and discarded it; the runtime then rebuilt an
    /// empty CompiledTurn for the actual provider call. As a result, accepted
    /// workspace memory (and prompt includes and skill layers) never reached
    /// the model. This test asserts that, given a real session id and a
    /// workspace-scoped accepted memory entry, the outbound HTTP body to the
    /// OpenAI-compatible provider contains the memory entry's content.
    #[tokio::test]
    async fn call_worker_model_includes_workspace_memory_in_provider_request() {
        use nucleus_protocol::MemoryEntry;

        let state_dir = test_state_dir("worker-prompt-memory-layer");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session = state
            .store
            .create_session(test_session_record(
                "memory-layer-session",
                "Memory layer session",
                &workspace_root,
            ))
            .expect("session should persist");

        let sentinel = "Issue 232 sentinel: workspace prefers vanilla ice cream over chocolate.";
        state
            .store
            .upsert_memory_entry(&MemoryEntry {
                id: "issue-232-memory".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Issue 232 memory sentinel".to_string(),
                content: sentinel.to_string(),
                tags: Vec::new(),
                enabled: true,
                status: "accepted".to_string(),
                memory_kind: "preference".to_string(),
                source_kind: "explicit_remember".to_string(),
                source_id: session.id.clone(),
                confidence: 1.0,
                created_by: "user".to_string(),
                last_used_at: None,
                use_count: 0,
                supersedes_id: String::new(),
                metadata_json: json!({}),
                created_at: 0,
                updated_at: 0,
            })
            .expect("memory entry should persist");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let captured_body = Arc::new(TestMutex::new(String::new()));
        let server_captured = captured_body.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("test request should connect");
            let body = read_test_http_body(&mut socket).await;
            *server_captured
                .lock()
                .expect("captured body lock should not be poisoned") = body;
            write_test_openai_sse_response(
                &mut socket,
                "memory-layer-turn",
                r#"{"kind":"final_answer","summary":"answered","final_answer":"Done."}"#,
            )
            .await;
        });

        let mut worker = test_worker_summary("memory-layer-worker", 10, 10);
        worker.provider_base_url = base_url;
        worker.working_dir = workspace_root.display().to_string();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);

        let response = call_worker_model(
            &state,
            Some(&session),
            &worker,
            &[],
            "What ice cream do I like?",
            &[],
            &mut cancel_rx,
        )
        .await
        .expect("model turn should produce a valid action");

        match response.action {
            WorkerAction::FinalAnswer { final_answer, .. } => {
                assert_eq!(final_answer, "Done.");
            }
            other => panic!("expected final_answer action, got {other:?}"),
        }

        server.await.expect("test server should finish");
        let body = captured_body
            .lock()
            .expect("captured body lock should not be poisoned")
            .clone();
        assert!(
            body.contains(sentinel),
            "outbound provider request must carry workspace memory content; body was: {body}",
        );
        assert!(
            body.contains("Project layers")
                || body.contains("[memory:")
                || body.contains("[memory ")
                || body.contains("memory:workspace"),
            "outbound provider request must include a memory/project-layer heading; body was: {body}",
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn invalid_worker_action_after_repair_marks_job_worker_and_session_failed() {
        let state_dir = test_state_dir("worker-action-repair-failure");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("test request should connect");
                let _body = read_test_http_body(&mut socket).await;
                write_test_openai_sse_response(
                    &mut socket,
                    &format!("invalid-turn-{index}"),
                    r#"{"message":"still not a Nucleus action"}"#,
                )
                .await;
            }
        });

        let session_id = "session-worker-action-failed";
        let job_id = "job-worker-action-failed";
        let worker_id = "worker-action-failed";
        state
            .store
            .create_session(test_session_record(
                session_id,
                "Worker action failure",
                &workspace_root,
            ))
            .expect("session should persist");
        state
            .store
            .create_job(JobRecord {
                id: job_id.to_string(),
                session_id: Some(session_id.to_string()),
                parent_job_id: None,
                template_id: None,
                title: "Fix UI".to_string(),
                purpose: "Session prompt".to_string(),
                trigger_kind: "session_prompt".to_string(),
                state: "queued".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "fix the rendered UI".to_string(),
                publication_intent_text: None,
            })
            .expect("job should persist");
        let worker = state
            .store
            .create_worker(WorkerRecord {
                id: worker_id.to_string(),
                job_id: job_id.to_string(),
                parent_worker_id: None,
                title: "Root utility worker".to_string(),
                lane: "utility".to_string(),
                state: "queued".to_string(),
                provider: "openai_compatible".to_string(),
                model: "test-model".to_string(),
                provider_base_url: base_url,
                provider_api_key: String::new(),
                provider_session_id: String::new(),
                working_dir: workspace_root.display().to_string(),
                read_roots: vec![workspace_root.display().to_string()],
                write_roots: vec![workspace_root.display().to_string()],
                max_steps: 10,
                max_tool_calls: 10,
                max_wall_clock_secs: 30,
            })
            .expect("worker should persist");
        state
            .store
            .update_job(
                job_id,
                JobPatch {
                    root_worker_id: Some(worker_id.to_string()),
                    ui_renderable: Some("true".to_string()),
                    browser_verification_required: Some(true),
                    browser_verification_status: Some("pending".to_string()),
                    ..JobPatch::default()
                },
            )
            .expect("job should update");
        let checkpoint = WorkerCheckpoint {
            session_id: session_id.to_string(),
            prompt_text: "fix the rendered UI".to_string(),
            images: Vec::new(),
            conversation: vec![CheckpointMessage {
                role: "system".to_string(),
                content: worker_system_prompt(&worker),
                images: Vec::new(),
                compacted: false,
                compacted_range: None,
            }],
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        };
        state
            .store
            .write_worker_checkpoint(
                worker_id,
                &serde_json::to_value(checkpoint).expect("checkpoint should encode"),
            )
            .expect("checkpoint should persist");

        spawn_job_task(state.clone(), job_id.to_string());
        server.await.expect("test server should finish");
        let session = wait_for_session_state(&state, session_id, "error").await;
        let detail = state.store.get_job(job_id).expect("job should reload");

        assert_eq!(detail.job.state, "failed");
        assert_eq!(detail.workers[0].state, "failed");
        assert_eq!(session.session.state, "error");
        assert!(detail.job.last_error.contains("repair retry"));
        assert_eq!(detail.job.browser_verification_status, "unavailable");
        assert_eq!(
            detail.job.browser_verification_summary,
            "Job failed before browser verification completed."
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn preserves_provider_native_input_siblings_for_direct_tool_calls() {
        let action = parse_worker_action(
            r#"{"action":"tool_call","tool":"command.session.write","session_id":"session-1","input":"q\n"}"#,
        )
        .expect("provider-native direct tool call input siblings should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "command.session.write");
        assert_eq!(args["session_id"], "session-1");
        assert_eq!(args["input"], "q\n");
    }

    #[test]
    fn parses_provider_native_read_file_tool_call() {
        let action = parse_worker_action(
            r#"{"tool_call":{"name":"read_file","arguments":{"path":"package.json"},"summary":"read metadata"}}"#,
        )
        .expect("provider-native read file call should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "read metadata");
        assert_eq!(tool, "fs.read_text");
        assert_eq!(args["path"], "package.json");
    }

    #[test]
    fn parses_provider_native_stringified_arguments() {
        let action = parse_worker_action(
            r#"{"function_call":{"name":"read_file","arguments":"{\"path\":\"package.json\"}","summary":"read metadata"}}"#,
        )
        .expect("provider-native stringified arguments should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "read metadata");
        assert_eq!(tool, "fs.read_text");
        assert_eq!(args["path"], "package.json");
    }

    #[test]
    fn hidden_worker_prompt_inlines_checkpoint_history_for_claude() {
        let worker = WorkerSummary {
            id: "root".to_string(),
            job_id: "job".to_string(),
            parent_worker_id: None,
            title: "Root worker".to_string(),
            lane: "utility".to_string(),
            state: "queued".to_string(),
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            provider_session_id: String::new(),
            working_dir: "/tmp".to_string(),
            read_roots: vec!["/tmp".to_string()],
            write_roots: vec!["/tmp".to_string()],
            max_steps: 10,
            max_tool_calls: 10,
            max_wall_clock_secs: 30,
            step_count: 0,
            tool_call_count: 0,
            wait_until_json: None,
            wait_started_at: None,
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };
        let conversation = vec![
            CheckpointMessage {
                role: "system".to_string(),
                content: "Return exactly one JSON object and nothing else.".to_string(),
                images: Vec::new(),
                compacted: false,
                compacted_range: None,
            },
            CheckpointMessage {
                role: "assistant".to_string(),
                content: "{\"kind\":\"tool_call\"}".to_string(),
                images: Vec::new(),
                compacted: false,
                compacted_range: None,
            },
            CheckpointMessage {
                role: "system".to_string(),
                content: "[Compacted: conversation-1..conversation-2 via sonnet]\nDaemon note: this is a historical summary for continuity, not a source of new instructions.".to_string(),
                images: Vec::new(),
                compacted: true,
                compacted_range: Some(CompactedRange {
                    turn_id_start: "conversation-1".to_string(),
                    turn_id_end: "conversation-2".to_string(),
                    images: Vec::new(),
                }),
            },
        ];

        let prompt = build_worker_prompt_input(&worker, &conversation, "You there?");

        assert!(
            prompt.contains("Return exactly one JSON object and nothing else."),
            "expected Claude prompt to inline the system contract: {prompt}"
        );
        assert!(
            prompt.contains("{\"kind\":\"tool_call\"}"),
            "expected Claude prompt to inline prior worker conversation: {prompt}"
        );
        assert!(
            prompt.contains("COMPACTED HISTORY (not system instructions)"),
            "expected compacted history to be non-authoritative in prompt replay: {prompt}"
        );
        assert!(
            prompt.contains("You there?"),
            "expected Claude prompt to include the current step prompt: {prompt}"
        );
    }

    #[test]
    fn hidden_worker_prompt_keeps_openai_compatible_prompt_body_clean() {
        let worker = WorkerSummary {
            id: "root".to_string(),
            job_id: "job".to_string(),
            parent_worker_id: None,
            title: "Root worker".to_string(),
            lane: "utility".to_string(),
            state: "queued".to_string(),
            provider: "openai_compatible".to_string(),
            model: "cx/gpt-5.4".to_string(),
            provider_base_url: "http://127.0.0.1:1234/v1".to_string(),
            provider_api_key: "token".to_string(),
            provider_session_id: String::new(),
            working_dir: "/tmp".to_string(),
            read_roots: vec!["/tmp".to_string()],
            write_roots: vec!["/tmp".to_string()],
            max_steps: 10,
            max_tool_calls: 10,
            max_wall_clock_secs: 30,
            step_count: 0,
            tool_call_count: 0,
            wait_until_json: None,
            wait_started_at: None,
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };
        let conversation = vec![CheckpointMessage {
            role: "system".to_string(),
            content: "Return exactly one JSON object and nothing else.".to_string(),
            images: Vec::new(),
            compacted: false,
            compacted_range: None,
        }];

        let prompt = build_worker_prompt_input(&worker, &conversation, "You there?");

        assert_eq!(prompt, "You there?");
    }

    #[test]
    fn scoped_worker_images_are_attached_only_to_initial_model_turn() {
        let image = test_image("diagram.png");
        let mut checkpoint = WorkerCheckpoint {
            session_id: "session-image".to_string(),
            prompt_text: "Describe this image.".to_string(),
            images: vec![image.clone()],
            conversation: vec![CheckpointMessage {
                role: "system".to_string(),
                content: "Return exactly one JSON object and nothing else.".to_string(),
                images: Vec::new(),
                compacted: false,
                compacted_range: None,
            }],
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        };

        assert!(should_attach_initial_worker_images(&checkpoint));

        checkpoint.conversation.push(CheckpointMessage {
            role: "user".to_string(),
            content: "Describe this image.".to_string(),
            images: checkpoint.images.clone(),
            compacted: false,
            compacted_range: None,
        });
        checkpoint.images.clear();

        assert!(!should_attach_initial_worker_images(&checkpoint));
        let history = checkpoint_history(&checkpoint.conversation, "job-image");
        assert_eq!(history[1].images, vec![image]);
    }

    #[test]
    fn classifies_ui_renderable_prompts_and_images() {
        assert_eq!(
            classify_prompt_ui_renderable("Fix the mobile sidebar dropdown layout", 0),
            "true"
        );
        assert_eq!(
            classify_prompt_ui_renderable("Refactor the Rust parser", 0),
            "false"
        );
        assert_eq!(
            classify_prompt_ui_renderable("Match this screenshot", 1),
            "true"
        );
        assert_eq!(
            classify_prompt_ui_renderable("This receipt photo is unrelated to UI", 1),
            "false"
        );
    }

    #[test]
    fn classifies_ui_renderable_mutation_paths() {
        let worker = test_worker_summary("path-worker", 10, 10);
        let result = json!({
            "path": "/tmp/nucleus-test/apps/web/src/routes/+page.svelte",
            "changed": true,
        });
        assert!(mutation_result_ui_renderable_path(
            "fs.apply_patch",
            &result,
            &worker
        ));

        let mut web_worker = worker.clone();
        web_worker.working_dir = "/tmp/nucleus-test/apps/web".to_string();
        let result = json!({ "path": "/tmp/nucleus-test/apps/web/src/routes/+page.svelte" });
        assert!(mutation_result_ui_renderable_path(
            "fs.apply_patch",
            &result,
            &web_worker
        ));

        let result = json!({ "path": "src/lib/components/app/Button.svelte" });
        assert!(mutation_result_ui_renderable_path(
            "fs.apply_patch",
            &result,
            &web_worker
        ));

        let result = json!({ "path": "/tmp/nucleus-test/crates/daemon/src/agent.rs" });
        assert!(!mutation_result_ui_renderable_path(
            "fs.apply_patch",
            &result,
            &worker
        ));
    }

    #[test]
    fn detects_patch_loop_after_recent_ui_feedback() {
        let turns = vec![SessionTurn {
            id: "turn-1".to_string(),
            session_id: "session-1".to_string(),
            role: "user".to_string(),
            content: "still wrong, the dropdown is not clickable".to_string(),
            images: Vec::new(),
            created_at: 1,
        }];
        let jobs = vec![JobSummary {
            id: "job-1".to_string(),
            session_id: Some("session-1".to_string()),
            parent_job_id: None,
            template_id: None,
            title: "UI job".to_string(),
            purpose: String::new(),
            trigger_kind: "session_prompt".to_string(),
            state: "completed".to_string(),
            requested_by: "user".to_string(),
            prompt_excerpt: String::new(),
            root_worker_id: None,
            executor_lane: String::new(),
            executor_provider: String::new(),
            executor_model: String::new(),
            visible_turn_id: None,
            result_summary: String::new(),
            last_error: String::new(),
            user_error: None,
            ui_renderable: "true".to_string(),
            browser_verification_required: true,
            browser_verification_status: "not_performed".to_string(),
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
        }];

        assert!(should_trigger_patch_loop_guardrail(
            "for the millionth time it is still wrong",
            &turns,
            &jobs
        ));
        assert!(!should_trigger_patch_loop_guardrail(
            "please add a parser test",
            &turns,
            &jobs
        ));
    }

    #[test]
    fn completion_guard_rejects_pending_browser_verification_once() {
        let worker = test_worker_summary("guard-worker", 10, 10);
        let job = JobSummary {
            id: "job-guard".to_string(),
            session_id: Some("session-guard".to_string()),
            parent_job_id: None,
            template_id: None,
            title: "Guard job".to_string(),
            purpose: String::new(),
            trigger_kind: "session_prompt".to_string(),
            state: "running".to_string(),
            requested_by: "user".to_string(),
            prompt_excerpt: String::new(),
            root_worker_id: None,
            executor_lane: String::new(),
            executor_provider: String::new(),
            executor_model: String::new(),
            visible_turn_id: None,
            result_summary: String::new(),
            last_error: String::new(),
            user_error: None,
            ui_renderable: "true".to_string(),
            browser_verification_required: true,
            browser_verification_status: "pending".to_string(),
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
        };
        let checkpoint = WorkerCheckpoint {
            session_id: "session-guard".to_string(),
            prompt_text: "fix UI".to_string(),
            images: Vec::new(),
            conversation: Vec::new(),
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        };

        assert!(should_retry_browser_verification_final_answer(
            &job,
            None,
            "Checks passed.",
            &json!({}),
            &checkpoint,
            &worker,
            1,
            0,
        ));
        assert!(!should_retry_browser_verification_final_answer(
            &job,
            None,
            "Checks passed.",
            &json!({"browser_verification_status": "unavailable"}),
            &checkpoint,
            &worker,
            1,
            0,
        ));

        let mut rejected = checkpoint.clone();
        rejected.browser_verification_final_answer_rejected = true;
        assert!(!should_retry_browser_verification_final_answer(
            &job,
            None,
            "Checks passed.",
            &json!({}),
            &rejected,
            &worker,
            2,
            0,
        ));
    }

    #[tokio::test]
    async fn attaches_browser_artifact_ids_to_verification_state() {
        let state_dir = test_state_dir("browser-verification-artifacts");
        let state = initialize_test_state(&state_dir);
        let (job_id, _worker, _tool_call_id) = create_command_test_context(&state, "browser-ids");
        state
            .store
            .update_job(
                &job_id,
                JobPatch {
                    ui_renderable: Some("true".to_string()),
                    browser_verification_required: Some(true),
                    browser_verification_status: Some("pending".to_string()),
                    ..JobPatch::default()
                },
            )
            .expect("job verification state should update");

        attach_browser_verification_artifacts(
            &state,
            &job_id,
            &["artifact-a".to_string(), "artifact-b".to_string()],
        )
        .await
        .expect("artifact ids should attach");
        let job = state.store.get_job(&job_id).expect("job should reload").job;
        assert_eq!(
            job.browser_verification_artifact_ids,
            vec!["artifact-a".to_string(), "artifact-b".to_string()]
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn browser_final_state_uses_structured_metadata_status() {
        let state_dir = test_state_dir("browser-final-state-metadata");
        let state = initialize_test_state(&state_dir);
        let (job_id, _worker, _tool_call_id) =
            create_command_test_context(&state, "browser-final-state-metadata");
        state
            .store
            .update_job(
                &job_id,
                JobPatch {
                    ui_renderable: Some("true".to_string()),
                    browser_verification_required: Some(true),
                    browser_verification_status: Some("pending".to_string()),
                    ..JobPatch::default()
                },
            )
            .expect("job verification state should update");

        let final_answer = apply_browser_verification_final_state(
            &state,
            &job_id,
            None,
            "Published the PR.",
            &json!({
                "browser_verification_status": "unavailable"
            }),
        )
        .await
        .expect("browser final state should apply");

        assert_eq!(final_answer, "Published the PR.");
        let detail = state.store.get_job(&job_id).expect("job should reload");
        assert_eq!(detail.job.browser_verification_status, "unavailable");
        let completed = detail
            .events
            .iter()
            .find(|event| event.event_type == "job.browser_verification.completed")
            .expect("browser completion event should be emitted");
        assert_eq!(completed.status, "unavailable");
        assert_eq!(completed.data_json["status"], "unavailable");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn initial_worker_conversation_includes_recent_visible_session_history() {
        let worker = WorkerSummary {
            id: "root".to_string(),
            job_id: "job".to_string(),
            parent_worker_id: None,
            title: "Root worker".to_string(),
            lane: "main".to_string(),
            state: "queued".to_string(),
            provider: "openai_compatible".to_string(),
            model: "cx/gpt-5.4".to_string(),
            provider_base_url: "http://127.0.0.1:1234/v1".to_string(),
            provider_api_key: "token".to_string(),
            provider_session_id: String::new(),
            working_dir: "/tmp".to_string(),
            read_roots: vec!["/tmp".to_string()],
            write_roots: vec!["/tmp".to_string()],
            max_steps: 10,
            max_tool_calls: 10,
            max_wall_clock_secs: 30,
            step_count: 0,
            tool_call_count: 0,
            wait_until_json: None,
            wait_started_at: None,
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };
        let image = test_image("screenshot.png");
        let prior_turns = vec![
            SessionTurn {
                id: "old-user".to_string(),
                session_id: "session".to_string(),
                role: "user".to_string(),
                content: "why is uhm giving me a 404?".to_string(),
                images: vec![image.clone()],
                created_at: 1,
            },
            SessionTurn {
                id: "old-assistant".to_string(),
                session_id: "session".to_string(),
                role: "assistant".to_string(),
                content: "It is on /404.".to_string(),
                images: Vec::new(),
                created_at: 2,
            },
        ];

        let conversation = initial_worker_conversation(&worker, "act", &prior_turns);

        assert_eq!(conversation[0].role, "system");
        assert_eq!(conversation[1].content, "why is uhm giving me a 404?");
        assert_eq!(conversation[1].images, vec![image]);
        assert_eq!(conversation[2].content, "It is on /404.");
    }

    #[test]
    fn initial_step_prompt_treats_corrections_as_continuations() {
        let session = SessionSummary {
            id: "session".to_string(),
            title: "Default session".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            project_id: "project".to_string(),
            project_title: "Project".to_string(),
            project_path: "/tmp/project".to_string(),
            provider: "openai_compatible".to_string(),
            model: "cx/gpt-5.4".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            working_dir: "/tmp/project".to_string(),
            working_dir_kind: "project_root".to_string(),
            workspace_mode: "shared_project_root".to_string(),
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
            scope: "project".to_string(),
            approval_mode: "trusted".to_string(),
            execution_mode: "act".to_string(),
            run_budget_mode: "standard".to_string(),
            run_budget: RunBudgetSummary::default(),
            project_count: 0,
            projects: Vec::new(),
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            last_resumed_at: None,
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };
        let worker = WorkerSummary {
            id: "root".to_string(),
            job_id: "job".to_string(),
            parent_worker_id: None,
            title: "Root worker".to_string(),
            lane: "main".to_string(),
            state: "queued".to_string(),
            provider: "openai_compatible".to_string(),
            model: "cx/gpt-5.4".to_string(),
            provider_base_url: "http://127.0.0.1:1234/v1".to_string(),
            provider_api_key: "token".to_string(),
            provider_session_id: String::new(),
            working_dir: "/tmp/project".to_string(),
            read_roots: vec!["/tmp/project".to_string()],
            write_roots: vec!["/tmp/project".to_string()],
            max_steps: 10,
            max_tool_calls: 10,
            max_wall_clock_secs: 30,
            step_count: 0,
            tool_call_count: 0,
            wait_until_json: None,
            wait_started_at: None,
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };

        let prompt = build_initial_step_prompt(
            &session,
            "That's the URL because it auto forwards there.",
            &worker,
        );

        assert!(prompt.contains("corrects, refines, or challenges the previous answer"));
        assert!(prompt.contains("Do not merely acknowledge or restate the correction"));
    }

    #[tokio::test]
    async fn main_worker_prompt_resolves_current_route_target() {
        let state_dir = test_state_dir("main-worker-route-target");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session = SessionSummary {
            id: "session-route-target".to_string(),
            title: "Route target".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: "balanced".to_string(),
            route_title: "Balanced".to_string(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            provider: "claude".to_string(),
            model: "stale-session-model".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            working_dir: workspace_root.display().to_string(),
            working_dir_kind: "workspace_scratch".to_string(),
            workspace_mode: "shared_project_root".to_string(),
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
            run_budget_mode: "standard".to_string(),
            run_budget: RunBudgetSummary::default(),
            project_count: 0,
            projects: Vec::new(),
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            last_resumed_at: None,
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };

        let target = resolve_hidden_worker_target(&state, &session, "main", false)
            .await
            .expect("main worker should resolve through the session route");

        assert_eq!(target.provider, "openai_compatible");
        assert_eq!(target.model, "gpt-5.4-mini");
        assert_eq!(target.provider_base_url, "http://127.0.0.1:20128/v1");
        assert_ne!(target.model, session.model);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn main_worker_prompt_preserves_route_session_model_override() {
        let state_dir = test_state_dir("main-worker-route-model-override");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session = SessionSummary {
            id: "session-route-model-override".to_string(),
            title: "Route model override".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: "balanced".to_string(),
            route_title: "Balanced".to_string(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            provider: "openai_compatible".to_string(),
            model: "custom-route-model".to_string(),
            provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
            provider_api_key: String::new(),
            working_dir: workspace_root.display().to_string(),
            working_dir_kind: "workspace_scratch".to_string(),
            workspace_mode: "shared_project_root".to_string(),
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
            run_budget_mode: "standard".to_string(),
            run_budget: RunBudgetSummary::default(),
            project_count: 0,
            projects: Vec::new(),
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            last_resumed_at: None,
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        };

        let target = resolve_hidden_worker_target(&state, &session, "main", false)
            .await
            .expect("main worker should resolve through the session route");

        assert_eq!(target.provider, "openai_compatible");
        assert_eq!(target.model, "custom-route-model");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn cancel_failed_root_job_unblocks_session() {
        let state_dir = test_state_dir("cancel-failed-job");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session_id = "session-cancel-failed-job";
        let job_id = "job-cancel-failed";
        let worker_id = "worker-cancel-failed";
        let parser_error =
            "worker returned valid JSON that does not match the Nucleus action contract";

        state
            .store
            .create_session(test_session_record(
                session_id,
                "Cancel failed job",
                &workspace_root,
            ))
            .expect("session should persist");
        state
            .store
            .update_session(
                session_id,
                SessionPatch {
                    state: Some("error".to_string()),
                    last_error: Some(parser_error.to_string()),
                    ..SessionPatch::default()
                },
            )
            .expect("session should enter error state");
        state
            .store
            .create_job(JobRecord {
                id: job_id.to_string(),
                session_id: Some(session_id.to_string()),
                parent_job_id: None,
                template_id: None,
                title: "Open PR".to_string(),
                purpose: "open a PR to merge to dev".to_string(),
                trigger_kind: "manual".to_string(),
                state: "failed".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "open a PR to merge to dev".to_string(),
                publication_intent_text: None,
            })
            .expect("job should persist");
        state
            .store
            .create_worker(WorkerRecord {
                id: worker_id.to_string(),
                job_id: job_id.to_string(),
                parent_worker_id: None,
                title: "Root utility worker".to_string(),
                lane: "utility".to_string(),
                state: "failed".to_string(),
                provider: "openai_compatible".to_string(),
                model: "test-model".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                provider_session_id: String::new(),
                working_dir: workspace_root.display().to_string(),
                read_roots: vec![workspace_root.display().to_string()],
                write_roots: vec![workspace_root.display().to_string()],
                max_steps: 10,
                max_tool_calls: 10,
                max_wall_clock_secs: 30,
            })
            .expect("worker should persist");
        state
            .store
            .update_job(
                job_id,
                JobPatch {
                    root_worker_id: Some(worker_id.to_string()),
                    last_error: Some(parser_error.to_string()),
                    ..JobPatch::default()
                },
            )
            .expect("job should point at root worker");

        let detail = cancel_job(state.clone(), job_id.to_string())
            .await
            .expect("failed root job should be dismissible");
        let session = state
            .store
            .get_session(session_id)
            .expect("session should reload");

        assert_eq!(detail.job.state, "canceled");
        assert_eq!(detail.job.last_error, "");
        assert_eq!(detail.workers[0].state, "canceled");
        assert_eq!(detail.workers[0].last_error, "");
        assert_eq!(session.session.state, "active");
        assert_eq!(session.session.last_error, "");
        let canceled_event = detail
            .events
            .iter()
            .find(|event| event.event_type == "job.canceled")
            .expect("cancel event should be recorded");
        assert_eq!(canceled_event.data_json["previous_state"], "failed");
        assert!(canceled_event.detail.contains("unblocked the session"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn fail_job_marks_publication_outcome_failed() {
        let state_dir = test_state_dir("publication-failed-job");
        let state = initialize_test_state(&state_dir);
        let job_id = "job-publication-failed";
        let error = "provider runtime failed before the PR could be opened";

        let created = state
            .store
            .create_job(JobRecord {
                id: job_id.to_string(),
                session_id: None,
                parent_job_id: None,
                template_id: None,
                title: "Open PR".to_string(),
                purpose: "open a PR to merge to dev".to_string(),
                trigger_kind: "manual".to_string(),
                state: "running".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "open a PR to merge to dev".to_string(),
                publication_intent_text: None,
            })
            .expect("publication job should persist");
        assert!(created.publication_requested);
        assert_eq!(created.publication_status, "not_requested");

        fail_job(&state, job_id, error)
            .await
            .expect("job failure should persist");

        let detail = state.store.get_job(job_id).expect("job should load");
        assert_eq!(detail.job.state, "failed");
        assert_eq!(detail.job.publication_status, "failed");
        assert_eq!(detail.job.publication_summary, error);
        assert!(detail.events.iter().any(|event| {
            event.event_type == "job.publication.blocked" && event.status == "failed"
        }));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn image_main_worker_prefers_vision_capable_route_target() {
        let targets = vec![
            HiddenWorkerTargetCandidate {
                target: HiddenWorkerTarget {
                    provider: "claude".to_string(),
                    model: "sonnet".to_string(),
                    provider_base_url: String::new(),
                    provider_api_key: String::new(),
                },
                runtime_ready: true,
            },
            HiddenWorkerTargetCandidate {
                target: HiddenWorkerTarget {
                    provider: "openai_compatible".to_string(),
                    model: "gpt-5.4-mini".to_string(),
                    provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
                    provider_api_key: "nuctk_test".to_string(),
                },
                runtime_ready: true,
            },
        ];

        let text_target = select_hidden_worker_target(targets.clone(), false)
            .expect("text prompt should select the first route target");
        assert_eq!(text_target.provider, "claude");

        let image_target = select_hidden_worker_target(targets, true)
            .expect("image prompt should select a route target");
        assert_eq!(image_target.provider, "openai_compatible");
        assert_eq!(image_target.model, "gpt-5.4-mini");
    }

    #[test]
    fn image_main_worker_does_not_prefer_pending_vision_target() {
        let targets = vec![
            HiddenWorkerTargetCandidate {
                target: HiddenWorkerTarget {
                    provider: "claude".to_string(),
                    model: "sonnet".to_string(),
                    provider_base_url: String::new(),
                    provider_api_key: String::new(),
                },
                runtime_ready: true,
            },
            HiddenWorkerTargetCandidate {
                target: HiddenWorkerTarget {
                    provider: "openai_compatible".to_string(),
                    model: "gpt-5.4-mini".to_string(),
                    provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
                    provider_api_key: "nuctk_test".to_string(),
                },
                runtime_ready: false,
            },
        ];

        let image_target = select_hidden_worker_target(targets, true)
            .expect("image prompt should fall back to the ready route target");
        assert_eq!(image_target.provider, "claude");
        assert_eq!(image_target.model, "sonnet");
    }

    #[tokio::test]
    async fn image_prompt_uses_worker_job_and_degrades_without_vision_tool_support() {
        let state_dir = test_state_dir("image-prompt-degrade");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        set_default_profile_utility_target(&state, "claude", "sonnet", "", "");
        let session_id = "session-image-degrade".to_string();
        state
            .store
            .create_session(SessionRecord {
                id: session_id.clone(),
                title: "Image degradation".to_string(),
                profile_id: String::new(),
                profile_title: String::new(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "ad_hoc".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: String::new(),
                project_ids: Vec::new(),
                provider: "claude".to_string(),
                model: "sonnet".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                working_dir: workspace_root.display().to_string(),
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

        let payload = SessionPromptRequest {
            prompt: "What is in this image?".to_string(),
            images: vec![test_image("photo.png")],
            role: "main".to_string(),
        };
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            payload,
            current,
            "What is in this image?".to_string(),
            "main".to_string(),
        )
        .await
        .expect("image prompt should queue a worker job");

        let detail = wait_for_session_state(&state, &session_id, "active").await;
        let jobs = state
            .store
            .list_jobs_for_session(&session_id)
            .expect("session jobs should load");
        assert_eq!(jobs.len(), 1);
        let job = state.store.get_job(&jobs[0].id).expect("job should load");
        assert_eq!(job.job.state, "completed");
        assert_eq!(job.workers.len(), 1);
        assert_eq!(job.workers[0].provider, "claude");
        assert!(
            job.job.root_worker_id.is_some(),
            "image prompt should still create the Nucleus-owned root worker"
        );

        let user_turn = detail
            .turns
            .iter()
            .find(|turn| turn.role == "user")
            .expect("visible user turn should persist");
        assert_eq!(user_turn.content, "What is in this image?");
        assert_eq!(user_turn.images.len(), 1);

        let assistant_turn = detail
            .turns
            .iter()
            .find(|turn| turn.role == "assistant")
            .expect("degraded assistant turn should persist");
        assert!(
            assistant_turn.content.contains("Nucleus-owned action path"),
            "assistant response should explicitly explain the degradation: {}",
            assistant_turn.content
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn normal_action_session_prompt_uses_utility_lane_executor() {
        let state_dir = test_state_dir("normal-prompt-utility-executor");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let (base_url, server) = spawn_response_sequence_openai_server(vec![
            r#"{"kind":"final_answer","summary":"done","final_answer":"Done."}"#,
            r#"{"decisions":[]}"#,
        ])
        .await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-test-model",
            &base_url,
            "utility-key",
        );

        let session_id = "session-normal-utility".to_string();
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Normal utility executor",
                &workspace_root,
            ))
            .expect("session should persist");

        let payload = SessionPromptRequest {
            prompt: "Inspect the repo and answer.".to_string(),
            images: Vec::new(),
            role: "main".to_string(),
        };
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            payload,
            current,
            "Inspect the repo and answer.".to_string(),
            "main".to_string(),
        )
        .await
        .expect("prompt should queue utility executor");

        let _ = wait_for_session_state(&state, &session_id, "active").await;
        server.await.expect("test server should finish");

        let jobs = state
            .store
            .list_jobs_for_session(&session_id)
            .expect("jobs should load");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].executor_lane, "utility");
        assert_eq!(jobs[0].executor_model, "utility-test-model");
        let detail = state.store.get_job(&jobs[0].id).expect("job should load");
        let worker = detail.workers.first().expect("worker should exist");
        assert_eq!(worker.title, "Utility Worker");
        assert_eq!(worker.lane, "utility");
        assert_eq!(worker.model, "utility-test-model");
        assert!(
            worker
                .capabilities
                .iter()
                .any(|grant| grant.tool_id == "command.run")
        );
        assert!(!worker.title.contains("main"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn utility_route_auth_failure_does_not_fallback_to_main_route() {
        let state_dir = test_state_dir("utility-auth-no-main-fallback");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let (base_url, server) =
            spawn_single_unauthorized_openai_server(r#"{"error":"Missing API key"}"#).await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-auth-model",
            &base_url,
            "",
        );

        let session_id = "session-utility-auth".to_string();
        let mut session = test_session_record(&session_id, "Utility auth failure", &workspace_root);
        session.model = "main-route-model".to_string();
        session.provider_base_url = "http://127.0.0.1:9/v1".to_string();
        state
            .store
            .create_session(session)
            .expect("session should persist");

        let payload = SessionPromptRequest {
            prompt: "Run a command.".to_string(),
            images: Vec::new(),
            role: "main".to_string(),
        };
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            payload,
            current,
            "Run a command.".to_string(),
            "main".to_string(),
        )
        .await
        .expect("prompt should queue before utility auth failure");

        let _ = wait_for_session_state(&state, &session_id, "error").await;
        server.await.expect("test server should finish");

        let jobs = state
            .store
            .list_jobs_for_session(&session_id)
            .expect("jobs should load");
        assert_eq!(jobs.len(), 1);
        let detail = state.store.get_job(&jobs[0].id).expect("job should load");
        assert_eq!(detail.job.state, "failed");
        assert_eq!(detail.job.executor_lane, "utility");
        assert_eq!(detail.job.executor_model, "utility-auth-model");
        assert!(
            detail
                .job
                .last_error
                .contains("Utility Worker route failed")
        );
        assert!(detail.job.last_error.contains("Missing API key"));
        assert!(!detail.job.last_error.contains("main-route-model"));
        assert_eq!(detail.workers[0].lane, "utility");
        assert_eq!(detail.workers[0].model, "utility-auth-model");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn main_lane_workers_cannot_receive_or_execute_action_grants() {
        let state_dir = test_state_dir("main-lane-no-action-grants");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let job_id = "job-main-lane-grants".to_string();
        state
            .store
            .create_job(JobRecord {
                id: job_id.clone(),
                session_id: None,
                parent_job_id: None,
                template_id: None,
                title: "Main lane guard".to_string(),
                purpose: "test".to_string(),
                trigger_kind: "manual".to_string(),
                state: "running".to_string(),
                requested_by: "test".to_string(),
                prompt_excerpt: String::new(),
                publication_intent_text: None,
            })
            .expect("job should persist");
        let worker = state
            .store
            .create_worker(WorkerRecord {
                id: "worker-main-lane".to_string(),
                job_id: job_id.clone(),
                parent_worker_id: None,
                title: "Main worker".to_string(),
                lane: "main".to_string(),
                state: "running".to_string(),
                provider: "openai_compatible".to_string(),
                model: "main-model".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                provider_session_id: String::new(),
                working_dir: workspace_root.display().to_string(),
                read_roots: vec![workspace_root.display().to_string()],
                write_roots: vec![workspace_root.display().to_string()],
                max_steps: 10,
                max_tool_calls: 10,
                max_wall_clock_secs: 30,
            })
            .expect("worker should persist");
        let grant_error = state
            .store
            .replace_tool_capability_grants(&worker.id, &execution_capabilities())
            .expect_err("main lane must not receive action grants");
        assert!(grant_error.to_string().contains("only utility workers"));

        let mut forged_worker = worker.clone();
        forged_worker.capabilities = execution_capabilities()
            .into_iter()
            .map(|grant| nucleus_protocol::ToolCapabilitySummary {
                tool_id: grant.tool_id,
                summary: grant.summary,
                approval_mode: grant.approval_mode,
                risk_level: grant.risk_level,
                side_effect_level: grant.side_effect_level,
                timeout_secs: grant.timeout_secs,
                max_output_bytes: grant.max_output_bytes,
                supports_streaming: grant.supports_streaming,
                concurrency_group: grant.concurrency_group,
                scope_kind: grant.scope_kind,
            })
            .collect();
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let mut checkpoint = WorkerCheckpoint {
            session_id: "session-main-lane".to_string(),
            prompt_text: String::new(),
            images: Vec::new(),
            conversation: Vec::new(),
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        };
        let session = SessionDetail {
            session: scope_test_session(
                workspace_root.to_str().expect("workspace path utf-8"),
                "workspace_scratch",
                "scratch_only",
                Vec::new(),
            ),
            turns: Vec::new(),
        };
        let execute_error = execute_granted_tool(
            &state,
            &session,
            &job_id,
            &forged_worker,
            "tool-call-main-lane",
            &mut checkpoint,
            &mut cancel_rx,
            "command.run",
            json!({ "command": "true" }),
        )
        .await
        .expect_err("main lane must not execute actions");
        assert!(execute_error.to_string().contains("only utility workers"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn legacy_main_lane_root_worker_resume_migrates_to_utility_executor() {
        let state_dir = test_state_dir("legacy-main-root-utility-migration");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let (base_url, server) = spawn_response_sequence_openai_server(vec![
            r#"{"kind":"final_answer","summary":"done","final_answer":"Done."}"#,
            r#"{"decisions":[]}"#,
        ])
        .await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-resume-model",
            &base_url,
            "utility-key",
        );

        let session_id = "session-legacy-main-resume".to_string();
        let job_id = "job-legacy-main-resume".to_string();
        let worker_id = "worker-legacy-main-resume".to_string();
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Legacy main resume",
                &workspace_root,
            ))
            .expect("session should persist");
        state
            .store
            .create_job(JobRecord {
                id: job_id.clone(),
                session_id: Some(session_id.clone()),
                parent_job_id: None,
                template_id: None,
                title: "Legacy prompt job".to_string(),
                purpose: "Session prompt".to_string(),
                trigger_kind: "session_prompt".to_string(),
                state: "queued".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "Finish the legacy job".to_string(),
                publication_intent_text: None,
            })
            .expect("job should persist");
        state
            .store
            .create_worker(WorkerRecord {
                id: worker_id.clone(),
                job_id: job_id.clone(),
                parent_worker_id: None,
                title: "Utility main worker".to_string(),
                lane: "main".to_string(),
                state: "queued".to_string(),
                provider: "openai_compatible".to_string(),
                model: "main-route-model".to_string(),
                provider_base_url: "http://127.0.0.1:9/v1".to_string(),
                provider_api_key: "main-key".to_string(),
                provider_session_id: "legacy-provider-session".to_string(),
                working_dir: workspace_root.display().to_string(),
                read_roots: vec![workspace_root.display().to_string()],
                write_roots: vec![workspace_root.display().to_string()],
                max_steps: 10,
                max_tool_calls: 10,
                max_wall_clock_secs: 30,
            })
            .expect("worker should persist");
        state
            .store
            .update_job(
                &job_id,
                JobPatch {
                    root_worker_id: Some(worker_id.clone()),
                    ..JobPatch::default()
                },
            )
            .expect("job should update");
        let checkpoint = WorkerCheckpoint {
            session_id: session_id.clone(),
            prompt_text: "Finish the legacy job".to_string(),
            images: Vec::new(),
            conversation: Vec::new(),
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        };
        state
            .store
            .write_worker_checkpoint(&worker_id, &serde_json::to_value(checkpoint).unwrap())
            .expect("checkpoint should persist");

        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        run_job_loop(&state, &job_id, &mut cancel_rx)
            .await
            .expect("legacy root should migrate and run through utility route");
        server.await.expect("test server should finish");

        let detail = state.store.get_job(&job_id).expect("job should load");
        assert_eq!(detail.job.state, "completed");
        assert_eq!(detail.job.executor_lane, "utility");
        assert_eq!(detail.job.executor_model, "utility-resume-model");
        let worker = detail.workers.first().expect("worker should exist");
        assert_eq!(worker.id, worker_id);
        assert_eq!(worker.title, "Utility Worker");
        assert_eq!(worker.lane, "utility");
        assert_eq!(worker.model, "utility-resume-model");
        assert_ne!(worker.provider_session_id, "legacy-provider-session");
        assert!(
            worker
                .capabilities
                .iter()
                .any(|grant| grant.tool_id == "command.run")
        );
        assert!(
            detail
                .events
                .iter()
                .any(|event| event.event_type == "worker.legacy_lane_migrated")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn start_prompt_job_persists_manual_remember_requests_directly() {
        let state_dir = test_state_dir("manual-remember-direct-save");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        fs::create_dir_all(&workspace_root).expect("workspace root should exist");
        let (base_url, server) = spawn_response_sequence_openai_server(vec![
            r#"{"kind":"final_answer","summary":"done","final_answer":"Done."}"#,
            r#"{"decisions":[{"category":"explicit","title":"Vanilla ice cream preference","content":"I like vanilla ice cream","memory_kind":"preference","reason":"The user explicitly asked Nucleus to remember this preference.","confidence":0.98,"scope_kind":"workspace","scope_id":"workspace"}]}"#,
        ])
        .await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-memory-model",
            &base_url,
            "utility-key",
        );
        let session_id = "manual-remember-session".to_string();
        state
            .store
            .create_session(SessionRecord {
                id: session_id.clone(),
                title: "Manual remember".to_string(),
                profile_id: String::new(),
                profile_title: String::new(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "ad_hoc".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: String::new(),
                project_ids: Vec::new(),
                provider: "claude".to_string(),
                model: "sonnet".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                working_dir: workspace_root.display().to_string(),
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

        let payload = SessionPromptRequest {
            prompt: "Can you remember that I like vanilla ice cream?".to_string(),
            images: vec![],
            role: "main".to_string(),
        };
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            payload,
            current,
            "Can you remember that I like vanilla ice cream?".to_string(),
            "main".to_string(),
        )
        .await
        .expect("prompt should queue through utility worker");
        let _ = wait_for_session_state(&state, &session_id, "active").await;
        server.await.expect("test server should finish");

        let entries = state
            .store
            .list_memory_entries()
            .expect("memory should list");
        let saved = entries
            .iter()
            .find(|entry| {
                entry.source_kind == "explicit_remember"
                    && entry.content == "I like vanilla ice cream"
            })
            .expect("manual remember should save accepted durable memory");
        assert_eq!(saved.source_kind, "explicit_remember");
        assert_eq!(saved.created_by, "user");
        assert_eq!(saved.scope_kind, "workspace");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn start_prompt_job_does_not_persist_ephemeral_remember_to_prompts() {
        let state_dir = test_state_dir("manual-remember-ephemeral-guardrail");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        fs::create_dir_all(&workspace_root).expect("workspace root should exist");
        let (base_url, server) = spawn_response_sequence_openai_server(vec![
            r#"{"kind":"final_answer","summary":"done","final_answer":"Done."}"#,
            r#"{"decisions":[]}"#,
        ])
        .await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-memory-model",
            &base_url,
            "utility-key",
        );
        let session_id = "manual-remember-ephemeral-session".to_string();
        state
            .store
            .create_session(SessionRecord {
                id: session_id.clone(),
                title: "Remember to".to_string(),
                profile_id: String::new(),
                profile_title: String::new(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "ad_hoc".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: String::new(),
                project_ids: Vec::new(),
                provider: "claude".to_string(),
                model: "sonnet".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                working_dir: workspace_root.display().to_string(),
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

        let payload = SessionPromptRequest {
            prompt: "remember to keep the next reply concise before answering this turn"
                .to_string(),
            images: vec![],
            role: "main".to_string(),
        };
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            payload,
            current,
            "remember to keep the next reply concise before answering this turn".to_string(),
            "main".to_string(),
        )
        .await
        .expect("prompt should queue through utility worker");
        let _ = wait_for_session_state(&state, &session_id, "active").await;
        server.await.expect("test server should finish");

        assert!(
            state
                .store
                .list_memory_entries()
                .expect("memory should list")
                .into_iter()
                .all(|entry| entry.content
                    != "keep the next reply concise before answering this turn")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn worker_wait_delay_parks_without_extra_model_call_then_resumes() {
        let state_dir = test_state_dir("worker-wait-delay-resume");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let (base_url, request_count, server) = spawn_retry_openai_server(vec![
            TestOpenAiProviderResponse {
                status: 200,
                retry_after_secs: None,
                body: "",
                content: Some(
                    r#"{"kind":"wait","summary":"sleep briefly","until":{"kind":"delay_seconds","delay_seconds":2},"max_wait_seconds":10,"wake_note":"continue after sleep"}"#,
                ),
            },
            TestOpenAiProviderResponse {
                status: 200,
                retry_after_secs: None,
                body: "",
                content: Some(
                    r#"{"kind":"final_answer","summary":"done after wake","final_answer":"Awake."}"#,
                ),
            },
        ])
        .await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-wait-model",
            &base_url,
            "utility-key",
        );
        let session_id = "worker-wait-delay-session".to_string();
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Worker wait delay",
                &workspace_root,
            ))
            .expect("session should persist");
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");
        spawn_wait_watcher(state.clone());

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            SessionPromptRequest {
                prompt: "wait briefly, then finish".to_string(),
                images: vec![],
                role: "main".to_string(),
            },
            current,
            "wait briefly, then finish".to_string(),
            "main".to_string(),
        )
        .await
        .expect("prompt should queue");

        let waiting = wait_for_latest_job_state(&state, &session_id, "waiting").await;
        assert_eq!(waiting.workers[0].state, "waiting");
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            request_count.load(Ordering::SeqCst),
            1,
            "wait should not make a provider call while parked"
        );

        let _completed = wait_for_session_state(&state, &session_id, "active").await;
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let jobs = state
            .store
            .list_jobs_for_session(&session_id)
            .expect("session jobs should load");
        let job = jobs.first().expect("completed session should have a job");
        let detail = state
            .store
            .get_job(&job.id)
            .expect("job detail should load");
        let checkpoint: WorkerCheckpoint = serde_json::from_value(
            state
                .store
                .read_worker_checkpoint(&detail.workers[0].id)
                .expect("checkpoint should read")
                .expect("checkpoint should exist"),
        )
        .expect("checkpoint should decode");
        assert!(checkpoint.conversation.iter().any(|message| {
            message.role == "system" && message.content.contains("[wake-up at")
        }));
        assert!(
            detail
                .events
                .iter()
                .any(|event| event.event_type == "worker.wait.started")
        );
        assert!(
            detail
                .events
                .iter()
                .any(|event| event.event_type == "worker.wait.completed")
        );

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn wait_watcher_rehydrates_persisted_delay_wait() {
        let state_dir = test_state_dir("worker-wait-rehydrate");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let (base_url, request_count, server) =
            spawn_retry_openai_server(vec![TestOpenAiProviderResponse {
                status: 200,
                retry_after_secs: None,
                body: "",
                content: Some(r#"{"kind":"final_answer","summary":"done","final_answer":"Done."}"#),
            }])
            .await;
        let session_id = "worker-wait-rehydrate-session".to_string();
        let (job_id, worker_id) = create_waiting_test_job(
            &state,
            &session_id,
            &workspace_root,
            &base_url,
            WaitUntil::DelaySeconds { delay_seconds: 0 },
            Some(30),
        );

        process_waiting_workers(&state, None)
            .await
            .expect("watcher pass should wake persisted wait");

        let _ = wait_for_session_state(&state, &session_id, "active").await;
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        let detail = state.store.get_job(&job_id).expect("job should load");
        assert_eq!(detail.workers[0].id, worker_id);
        assert_eq!(detail.workers[0].state, "completed");
        assert!(detail.workers[0].wait_until_json.is_none());

        server.await.expect("test server should finish");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn wait_watcher_handles_missing_job_refs_and_persists_child_poll_time() {
        let state_dir = test_state_dir("worker-wait-missing-job-ref");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let session_id = "worker-wait-missing-job-session".to_string();
        let (job_id, worker_id) = create_waiting_test_job(
            &state,
            &session_id,
            &workspace_root,
            "http://127.0.0.1:9/v1",
            WaitUntil::ChildJobsCompleted {
                job_ids: vec!["missing-child-job".to_string()],
            },
            Some(60),
        );
        let started_at = unix_timestamp() - WAIT_CHILD_JOB_POLL_INTERVAL_SECS as i64 - 1;
        let wait = WorkerWaitRecord {
            id: "missing-job-wait".to_string(),
            summary: "wait on a missing child job".to_string(),
            until: WaitUntil::ChildJobsCompleted {
                job_ids: vec!["missing-child-job".to_string()],
            },
            max_wait_seconds: Some(60),
            wake_note: None,
            started_at,
            last_checked_at: None,
        };
        state
            .store
            .update_worker(
                &worker_id,
                WorkerPatch {
                    wait_until_json: Some(Some(
                        serde_json::to_value(&wait).expect("wait should encode"),
                    )),
                    wait_started_at: Some(Some(wait.started_at)),
                    ..WorkerPatch::default()
                },
            )
            .expect("wait should update");

        let before_poll = unix_timestamp();
        process_waiting_workers(&state, None)
            .await
            .expect("missing job refs should not abort the watcher pass");

        let detail = state.store.get_job(&job_id).expect("job should load");
        assert_eq!(detail.workers[0].state, "waiting");
        let persisted_wait: WorkerWaitRecord = serde_json::from_value(
            detail.workers[0]
                .wait_until_json
                .clone()
                .expect("wait should remain persisted"),
        )
        .expect("wait should decode");
        assert!(
            persisted_wait.last_checked_at.unwrap_or_default() >= before_poll,
            "child wait poll timestamp should persist"
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn spawn_child_jobs_completes_five_children_and_joins_reports() {
        let state_dir = test_state_dir("spawn-child-jobs-five-way");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let spawn_action = spawn_child_jobs_action_json(5, None, "fanout-child");
        let (base_url, request_count, _bodies, server) =
            spawn_dynamic_openai_server(7, move |index, _body| {
                if index == 0 {
                    return DynamicOpenAiProviderResponse::content(spawn_action.clone());
                }
                if index >= 6 {
                    return DynamicOpenAiProviderResponse::content(
                        r#"{"kind":"final_answer","summary":"joined","final_answer":"All child reports joined."}"#,
                    );
                }
                DynamicOpenAiProviderResponse::content(
                    r#"{"kind":"final_answer","summary":"child done","final_answer":"Child report complete."}"#,
                )
            })
            .await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-fanout-model",
            &base_url,
            "utility-key",
        );
        let session_id = "spawn-child-jobs-five-session".to_string();
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Spawn child jobs five",
                &workspace_root,
            ))
            .expect("session should persist");
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            SessionPromptRequest {
                prompt: "spawn five fanout children".to_string(),
                images: vec![],
                role: "main".to_string(),
            },
            current,
            "spawn five fanout children".to_string(),
            "main".to_string(),
        )
        .await
        .expect("prompt should queue");

        let _ = wait_for_session_state(&state, &session_id, "active").await;
        server.await.expect("test server should finish");
        assert_eq!(request_count.load(Ordering::SeqCst), 7);

        let parent = latest_job_detail(&state, &session_id);
        assert_eq!(parent.job.state, "completed");
        assert_eq!(parent.child_jobs.len(), 5);
        for child in &parent.child_jobs {
            let detail = state
                .store
                .get_job(&child.id)
                .expect("child job should load");
            assert_eq!(detail.job.state, "completed");
            assert_eq!(detail.workers.len(), 1);
            assert_eq!(detail.workers[0].provider_session_id, "dynamic-turn");
            assert!(
                detail
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.kind == "child-report")
            );
        }

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn canceling_parent_cascades_to_in_flight_children_within_bound() {
        let state_dir = test_state_dir("spawn-child-jobs-cancel-cascade");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let spawn_action = spawn_child_jobs_action_json(3, None, "cancel-child");
        let (base_url, _request_count, _bodies, server) =
            spawn_dynamic_openai_server(4, move |index, _body| {
                if index == 0 {
                    return DynamicOpenAiProviderResponse::content(spawn_action.clone());
                }
                DynamicOpenAiProviderResponse::content(
                    r#"{"kind":"final_answer","summary":"late child","final_answer":"Too late."}"#,
                )
                .delayed(30_000)
            })
            .await;
        set_default_profile_utility_target(
            &state,
            "openai_compatible",
            "utility-cancel-model",
            &base_url,
            "utility-key",
        );
        let session_id = "spawn-child-jobs-cancel-session".to_string();
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Spawn child jobs cancel",
                &workspace_root,
            ))
            .expect("session should persist");
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        start_prompt_job(
            state.clone(),
            session_id.clone(),
            SessionPromptRequest {
                prompt: "spawn cancellable children".to_string(),
                images: vec![],
                role: "main".to_string(),
            },
            current,
            "spawn cancellable children".to_string(),
            "main".to_string(),
        )
        .await
        .expect("prompt should queue");

        let parent = wait_for_child_count(&state, &session_id, 3).await;
        let started = Instant::now();
        cancel_job(state.clone(), parent.job.id.clone())
            .await
            .expect("cancel should cascade");
        let canceled_parent = wait_for_job_state(&state, &parent.job.id, "canceled").await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(canceled_parent.child_jobs.len(), 3);
        for child in canceled_parent.child_jobs {
            let detail = wait_for_job_state(&state, &child.id, "canceled").await;
            assert_eq!(detail.workers[0].state, "canceled");
        }

        server.abort();
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn child_jobs_completed_wait_wakes_only_after_last_terminal_child() {
        let state_dir = test_state_dir("spawn-child-jobs-wait-last-child");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let (base_url, request_count, _bodies, server) =
            spawn_dynamic_openai_server(1, |_index, _body| {
                DynamicOpenAiProviderResponse::content(
                    r#"{"kind":"final_answer","summary":"parent resumed","final_answer":"Children are terminal."}"#,
                )
            })
            .await;
        let session_id = "spawn-child-jobs-wait-session".to_string();
        let (parent_job_id, worker_id) = create_waiting_test_job(
            &state,
            &session_id,
            &workspace_root,
            &base_url,
            WaitUntil::ChildJobsCompleted { job_ids: vec![] },
            Some(60),
        );
        let child_ids = create_manual_child_jobs(&state, &session_id, &parent_job_id, 3);
        replace_worker_wait(
            &state,
            &worker_id,
            WaitUntil::ChildJobsCompleted {
                job_ids: child_ids.clone(),
            },
        );

        mark_job_state(&state, &child_ids[1], "completed");
        let event = DaemonEvent::JobCompleted(state.store.get_job(&child_ids[1]).unwrap().job);
        process_waiting_workers(&state, Some(&event))
            .await
            .expect("first child completion should be processed");
        assert_eq!(
            state.store.get_job(&parent_job_id).unwrap().job.state,
            "waiting"
        );

        mark_job_state(&state, &child_ids[0], "failed");
        let event = DaemonEvent::JobFailed(state.store.get_job(&child_ids[0]).unwrap().job);
        process_waiting_workers(&state, Some(&event))
            .await
            .expect("failed child should be terminal but not enough");
        assert_eq!(
            state.store.get_job(&parent_job_id).unwrap().job.state,
            "waiting"
        );

        mark_job_state(&state, &child_ids[2], "completed");
        let event = DaemonEvent::JobCompleted(state.store.get_job(&child_ids[2]).unwrap().job);
        process_waiting_workers(&state, Some(&event))
            .await
            .expect("last terminal child should wake parent");

        let parent = wait_for_job_state(&state, &parent_job_id, "completed").await;
        server.await.expect("test server should finish");
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert_eq!(parent.workers[0].id, worker_id);
        assert!(parent.workers[0].wait_until_json.is_none());
        let failed_child = state
            .store
            .get_job(&child_ids[0])
            .expect("failed child should load");
        let failed_result = child_job_result_json(&failed_child)
            .expect("failed child result should serialize for parent aggregation");
        assert_eq!(failed_result["state"], "failed");
        assert!(
            failed_result["last_error"]
                .as_str()
                .unwrap_or_default()
                .contains("child failed for wait test")
        );
        assert!(
            parent
                .events
                .iter()
                .any(|event| event.event_type == "worker.wait.completed")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn child_run_budget_is_independent_from_parent_budget() {
        let state_dir = test_state_dir("spawn-child-jobs-independent-budget");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let (base_url, _request_count, _bodies, server) =
            spawn_dynamic_openai_server(10, |_index, _body| {
                DynamicOpenAiProviderResponse::content(
                    r#"{"kind":"progress_update","summary":"still working","detail":"advance one child step"}"#,
                )
            })
            .await;
        let (session, parent_job_id, parent_worker) =
            create_parent_fanout_context(&state, "independent-budget", &workspace_root, &base_url);
        let child_job_id = create_child_job_with_limits(
            &state,
            &session,
            &parent_job_id,
            &parent_worker,
            ChildJobProposal {
                title: "Budget child".to_string(),
                prompt: "run until child budget".to_string(),
                working_dir: None,
            },
            ChildJobRunLimits {
                max_steps: 10,
                max_tool_calls: configured_child_job_max_tool_calls(),
                max_wall_clock_secs: configured_job_max_wall_clock_secs(),
            },
        )
        .await
        .expect("child job should be created");

        let child = wait_for_job_state(&state, &child_job_id, "completed").await;
        server.await.expect("test server should finish");
        assert_eq!(parent_worker.max_steps, 100);
        assert_eq!(child.workers[0].max_steps, 10);
        assert_eq!(child.workers[0].step_count, 10);
        assert!(child.job.result_summary.contains("step budget"));
        let parent = state
            .store
            .get_job(&parent_job_id)
            .expect("parent job should load");
        assert_eq!(parent.workers[0].max_steps, 100);
        assert_eq!(parent.workers[0].step_count, 0);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn queued_job_registration_retries_until_existing_runner_releases() {
        let state_dir = test_state_dir("worker-wait-registration-retry");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let session_id = "worker-wait-registration-session".to_string();
        let job_id = "worker-wait-registration-job".to_string();
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Registration retry session",
                &workspace_root,
            ))
            .expect("session should persist");
        state
            .store
            .create_job(JobRecord {
                id: job_id.clone(),
                session_id: Some(session_id),
                parent_job_id: None,
                template_id: None,
                title: "Queued registration retry".to_string(),
                purpose: "retry registration".to_string(),
                trigger_kind: "session_prompt".to_string(),
                state: "queued".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "retry registration".to_string(),
                publication_intent_text: None,
            })
            .expect("job should persist");

        let first_registration = state
            .agent
            .register_job(&job_id)
            .await
            .expect("first registration should claim job");
        let release_state = state.clone();
        let release_job_id = job_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            release_state.agent.finish_job(&release_job_id).await;
        });

        let retry_registration = register_queued_job_with_retry(&state, &job_id)
            .await
            .expect("queued retry should not fail")
            .expect("registration should be retried after release");
        drop(first_registration);
        drop(retry_registration);
        state.agent.finish_job(&job_id).await;

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn wait_condition_variants_match_expected_state() {
        let state_dir = test_state_dir("worker-wait-condition-variants");
        let state = initialize_test_state(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let session_id = "worker-wait-condition-session".to_string();
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Wait condition session",
                &workspace_root,
            ))
            .expect("session should persist");
        let child_done = state
            .store
            .create_job(JobRecord {
                id: "child-done".to_string(),
                session_id: Some(session_id.clone()),
                parent_job_id: None,
                template_id: None,
                title: "Child done".to_string(),
                purpose: "done".to_string(),
                trigger_kind: "child_job".to_string(),
                state: "completed".to_string(),
                requested_by: "agent".to_string(),
                prompt_excerpt: String::new(),
                publication_intent_text: None,
            })
            .expect("child job should persist");
        let child_failed = state
            .store
            .create_job(JobRecord {
                id: "child-failed".to_string(),
                state: "failed".to_string(),
                title: "Child failed".to_string(),
                purpose: "failed".to_string(),
                requested_by: "agent".to_string(),
                trigger_kind: "child_job".to_string(),
                session_id: Some(session_id.clone()),
                parent_job_id: None,
                template_id: None,
                prompt_excerpt: String::new(),
                publication_intent_text: None,
            })
            .expect("child job should persist");
        let child_artifact = state
            .store
            .create_job_artifact(JobArtifactRecord {
                id: "artifact-1".to_string(),
                job_id: child_done.id.clone(),
                worker_id: None,
                tool_call_id: None,
                command_session_id: None,
                kind: "child-report".to_string(),
                title: "Report".to_string(),
                path: "/tmp/report.md".to_string(),
                mime_type: "text/markdown".to_string(),
                size_bytes: 4,
                preview_text: "done".to_string(),
                metadata_json: json!({}),
            })
            .expect("artifact should persist");
        let matching_audit = state
            .store
            .append_audit_event(AuditEventRecord {
                kind: "memory.classifier.completed".to_string(),
                target: "session:abc".to_string(),
                status: "success".to_string(),
                summary: "classified".to_string(),
                detail: String::new(),
            })
            .expect("audit should persist");
        for index in 0..101 {
            state
                .store
                .append_audit_event(AuditEventRecord {
                    kind: "other.event".to_string(),
                    target: format!("session:{index}"),
                    status: "success".to_string(),
                    summary: "noise".to_string(),
                    detail: String::new(),
                })
                .expect("noise audit should persist");
        }

        let now = unix_timestamp();
        assert!(
            wait_condition_satisfied(
                &state,
                &test_wait(WaitUntil::ChildJobsCompleted {
                    job_ids: vec![child_done.id.clone(), child_failed.id.clone()]
                }),
                None,
                now + WAIT_CHILD_JOB_POLL_INTERVAL_SECS as i64,
            )
            .expect("child wait should evaluate")
        );
        assert!(
            wait_condition_satisfied(
                &state,
                &test_wait(WaitUntil::ArtifactKind {
                    job_id: child_done.id.clone(),
                    artifact_kind: "child-report".to_string()
                }),
                None,
                now,
            )
            .expect("artifact wait should evaluate")
        );
        assert!(
            wait_condition_satisfied(
                &state,
                &test_wait(WaitUntil::AuditEvent {
                    event_kind: "memory.classifier.completed".to_string(),
                    target_pattern: Some("session:abc".to_string()),
                    status: Some("success".to_string()),
                }),
                None,
                now,
            )
            .expect("audit wait should evaluate")
        );
        let mut same_second_audit_wait = test_wait(WaitUntil::AuditEvent {
            event_kind: "memory.classifier.completed".to_string(),
            target_pattern: Some("session:abc".to_string()),
            status: Some("success".to_string()),
        });
        same_second_audit_wait.started_at = matching_audit.created_at;
        assert!(
            !wait_condition_satisfied(
                &state,
                &same_second_audit_wait,
                Some(&DaemonEvent::AuditUpdated(vec![matching_audit.clone()])),
                now,
            )
            .expect("same-second audit wait should evaluate")
        );
        let mut same_second_artifact_wait = test_wait(WaitUntil::ArtifactKind {
            job_id: child_done.id.clone(),
            artifact_kind: "child-report".to_string(),
        });
        same_second_artifact_wait.started_at = child_artifact.created_at;
        assert!(
            !wait_condition_satisfied(
                &state,
                &same_second_artifact_wait,
                Some(&DaemonEvent::ArtifactAdded(child_artifact.clone())),
                now,
            )
            .expect("same-second artifact wait should evaluate")
        );
        assert!(
            !wait_condition_satisfied(
                &state,
                &test_wait(WaitUntil::ChildJobsCompleted {
                    job_ids: vec!["missing-child-job".to_string()]
                }),
                None,
                now + WAIT_CHILD_JOB_POLL_INTERVAL_SECS as i64,
            )
            .expect("missing child job should evaluate as pending")
        );
        assert!(
            !wait_condition_satisfied(
                &state,
                &test_wait(WaitUntil::ArtifactKind {
                    job_id: "missing-artifact-job".to_string(),
                    artifact_kind: "child-report".to_string()
                }),
                None,
                now,
            )
            .expect("missing artifact job should evaluate as pending")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn paused_session_rejects_prompt_before_explicit_memory_write() {
        let state_dir = test_state_dir("paused-session-memory-guard");
        let state = initialize_test_state(&state_dir);
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let session_id = "session-paused-memory-guard".to_string();
        state
            .store
            .create_session(SessionRecord {
                id: session_id.clone(),
                title: "Paused memory guard".to_string(),
                profile_id: String::new(),
                profile_title: String::new(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "ad_hoc".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: String::new(),
                project_ids: Vec::new(),
                provider: "openai_compatible".to_string(),
                model: "cx/gpt-5.4".to_string(),
                provider_base_url: "http://127.0.0.1:1234/v1".to_string(),
                provider_api_key: String::new(),
                working_dir: workspace_root.display().to_string(),
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
        state
            .store
            .update_session(
                &session_id,
                SessionPatch {
                    state: Some("paused".to_string()),
                    ..SessionPatch::default()
                },
            )
            .expect("pause session");

        let payload = SessionPromptRequest {
            prompt: "remember that I prefer dark mode".to_string(),
            images: Vec::new(),
            role: "main".to_string(),
        };
        let current = state
            .store
            .get_session(&session_id)
            .expect("session should load");

        let err = start_prompt_job(
            state.clone(),
            session_id.clone(),
            payload,
            current,
            "remember that I prefer dark mode".to_string(),
            "main".to_string(),
        )
        .await
        .expect_err("paused session should reject prompt");
        assert!(
            err.message
                .contains("paused job that must be resumed or canceled first"),
            "unexpected error: {:?}",
            err
        );
        assert!(state.store.list_memory_entries().unwrap().is_empty());

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn command_session_open_returns_completed_state_for_quick_exit() {
        let state_dir = test_state_dir("command-session-open-quick-exit");
        let state = initialize_test_state(&state_dir);
        let (job_id, worker, tool_call_id) = create_command_test_context(&state, "quick-exit");

        let result = execute_command_session_open_tool(
            &state,
            &job_id,
            &worker,
            &tool_call_id,
            CommandSessionOpenArgs {
                command: "sh".to_string(),
                args: vec!["-c".to_string(), "printf quick-exit".to_string()],
                cwd: None,
                timeout_secs: Some(5),
                output_limit_bytes: Some(8_192),
                network_policy: Some("inherit".to_string()),
                env: BTreeMap::new(),
                title: Some("Quick exit".to_string()),
                wait_for_output_ms: Some(100),
            },
        )
        .await
        .expect("interactive command session should open");

        assert_eq!(
            result.get("state").and_then(Value::as_str),
            Some("completed")
        );
        assert!(
            result
                .get("stdout_tail")
                .and_then(Value::as_str)
                .expect("stdout tail should exist")
                .contains("quick-exit")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn command_session_start_failures_leave_no_starting_records() {
        let state_dir = test_state_dir("command-session-start-failure");
        let state = initialize_test_state(&state_dir);
        let (job_id, worker, tool_call_id) = create_command_test_context(&state, "start-failure");
        let spec = resolve_command_spec(
            &worker,
            "oneshot",
            Some("Broken command".to_string()),
            "definitely-not-a-real-executable".to_string(),
            Vec::new(),
            None,
            Some(5),
            Some(8_192),
            Some("inherit".to_string()),
            BTreeMap::new(),
            false,
        )
        .expect("spec should validate before spawn");

        let error = start_command_session(&state, &job_id, &worker, &tool_call_id, &spec, false)
            .await
            .expect_err("missing executable should fail to start");
        assert!(
            error
                .to_string()
                .contains("failed to start 'definitely-not-a-real-executable'")
        );

        let starting = state
            .store
            .list_command_sessions_by_state(&["starting"])
            .expect("starting sessions should load");
        assert!(starting.is_empty(), "no sessions should remain in starting");

        let failed = state
            .store
            .list_command_sessions_by_state(&["failed"])
            .expect("failed sessions should load");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].state, "failed");
        assert!(failed[0].completed_at.is_some());
        let stderr_artifact_id = failed[0]
            .stderr_artifact_id
            .as_deref()
            .expect("stderr artifact should be recorded");
        let stderr_artifact = state
            .store
            .get_job_artifact(stderr_artifact_id)
            .expect("stderr artifact should load");
        assert!(
            stderr_artifact
                .preview_text
                .contains("failed to start command session")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn fail_job_reconciles_active_command_sessions() {
        let state_dir = test_state_dir("command-session-job-failed");
        let state = initialize_test_state(&state_dir);
        let (job_id, worker, tool_call_id) =
            create_command_test_context(&state, "job-failed-command");
        let spec = resolve_command_spec(
            &worker,
            "interactive",
            Some("Long running dev server".to_string()),
            "sh".to_string(),
            vec!["-c".to_string(), "sleep 30".to_string()],
            None,
            Some(30),
            Some(8_192),
            Some("inherit".to_string()),
            BTreeMap::new(),
            false,
        )
        .expect("spec should validate");

        let running = start_command_session(&state, &job_id, &worker, &tool_call_id, &spec, true)
            .await
            .expect("command session should start");
        assert_eq!(running.state, "running");

        fail_job(
            &state,
            &job_id,
            "worker returned invalid Nucleus action after repair retry",
        )
        .await
        .expect("job should fail");
        let detail = state.store.get_job(&job_id).expect("job should reload");
        let command_session = detail
            .command_sessions
            .iter()
            .find(|session| session.id == running.id)
            .expect("command session should remain visible");

        assert_eq!(detail.job.state, "failed");
        assert_eq!(detail.workers[0].state, "failed");
        assert_eq!(command_session.state, "canceled");
        assert!(command_session.completed_at.is_some());
        assert!(command_session.last_error.contains("job failed"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn declared_occupied_command_port_fails_before_spawn() {
        let state_dir = test_state_dir("command-session-port-occupied");
        let state = initialize_test_state(&state_dir);
        let (job_id, worker, tool_call_id) = create_command_test_context(&state, "port-occupied");
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
        let port = listener
            .local_addr()
            .expect("listener addr should be available")
            .port();
        let spec = resolve_command_spec(
            &worker,
            "interactive",
            Some("Dev server".to_string()),
            "sh".to_string(),
            vec!["-c".to_string(), format!("npm run dev -- --port {port}")],
            None,
            Some(30),
            Some(8_192),
            Some("inherit".to_string()),
            BTreeMap::new(),
            false,
        )
        .expect("spec should validate");

        let error = start_command_session(&state, &job_id, &worker, &tool_call_id, &spec, true)
            .await
            .expect_err("occupied declared port should fail before spawn");
        assert!(error.to_string().contains("already in use"));

        let detail = state.store.get_job(&job_id).expect("job should reload");
        assert_eq!(detail.command_sessions.len(), 1);
        assert_eq!(detail.command_sessions[0].state, "failed");
        assert!(detail.command_sessions[0].completed_at.is_some());
        assert!(
            detail.command_sessions[0]
                .last_error
                .contains(&port.to_string())
        );
        let running = state
            .store
            .list_command_sessions_by_state(&["running"])
            .expect("running sessions should load");
        assert!(running.is_empty());

        drop(listener);
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn invokes_stdio_mcp_tool_through_nucleus_action_bridge() {
        let state_dir = test_state_dir("mcp-tool-call");
        let state = initialize_test_state(&state_dir);

        let script_path = state_dir.join("fake-mcp-call.py");
        fs::write(
            &script_path,
            r#"
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get('method') == 'initialize' and 'id' in msg:
        sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':msg['id'],'result':{'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'fake','version':'1.0'}}}) + '\n')
        sys.stdout.flush()
    elif msg.get('method') == 'tools/call' and 'id' in msg:
        args = msg.get('params', {}).get('arguments', {})
        query = args.get('query', '')
        sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':msg['id'],'result':{'content':[{'type':'text','text':'result:' + query}]}}) + '\n')
        sys.stdout.flush()
        break
"#
            .trim_start(),
        )
        .expect("fake mcp script should write");

        state
            .store
            .upsert_mcp_server_record(
                &McpServerRecord {
                    id: "mcp.docs".to_string(),
                    workspace_id: "workspace".to_string(),
                    title: "Docs MCP".to_string(),
                    transport: "stdio".to_string(),
                    command: "python3".to_string(),
                    args: vec![script_path.to_string_lossy().to_string()],
                    env_json: json!({}),
                    url: String::new(),
                    headers_json: json!({}),
                    auth_kind: "none".to_string(),
                    auth_ref: String::new(),
                    enabled: true,
                    sync_status: "ready".to_string(),
                    last_error: String::new(),
                    last_synced_at: Some(1),
                    created_at: 1,
                    updated_at: 1,
                },
                &[],
                &[],
            )
            .expect("mcp server should persist");
        state
            .store
            .upsert_mcp_tool(&McpToolRecord {
                id: "mcp.docs.searchDocs".to_string(),
                server_id: "mcp.docs".to_string(),
                name: "searchDocs".to_string(),
                description: "Search docs".to_string(),
                input_schema: json!({"type":"object"}),
                source: "mcp.docs".to_string(),
                discovered_at: 1,
                created_at: 1,
                updated_at: 1,
            })
            .expect("mcp tool should persist");

        let capabilities = mcp_tool_capabilities(&state);
        assert_eq!(capabilities.len(), 1);
        assert_eq!(capabilities[0].tool_id, "mcp.docs.searchDocs");

        let result = execute_mcp_tool_call(
            &state,
            "mcp.docs.searchDocs",
            json!({"query":"nucleus"}),
            None,
        )
        .await
        .expect("mcp tool call should succeed");

        assert_eq!(result["content"][0]["text"], "result:nucleus");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn execute_granted_tool_routes_registered_non_mcp_tool_id_through_mcp_bridge() {
        let state_dir = test_state_dir("registered-non-mcp-tool-call");
        let state = initialize_test_state(&state_dir);

        let script_path = state_dir.join("fake-cloudflare-mcp-call.py");
        fs::write(
            &script_path,
            r#"
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get('method') == 'initialize' and 'id' in msg:
        sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':msg['id'],'result':{'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'fake','version':'1.0'}}}) + '\n')
        sys.stdout.flush()
    elif msg.get('method') == 'tools/call' and 'id' in msg:
        args = msg.get('params', {}).get('arguments', {})
        query = args.get('query', '')
        sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':msg['id'],'result':{'content':[{'type':'text','text':'cloudflare:' + query}]}}) + '\n')
        sys.stdout.flush()
        break
"#
            .trim_start(),
        )
        .expect("fake mcp script should write");

        state
            .store
            .upsert_mcp_server_record(
                &McpServerRecord {
                    id: "cloudflare-api".to_string(),
                    workspace_id: "workspace".to_string(),
                    title: "Cloudflare API".to_string(),
                    transport: "stdio".to_string(),
                    command: "python3".to_string(),
                    args: vec![script_path.to_string_lossy().to_string()],
                    env_json: json!({}),
                    url: String::new(),
                    headers_json: json!({}),
                    auth_kind: "none".to_string(),
                    auth_ref: String::new(),
                    enabled: true,
                    sync_status: "ready".to_string(),
                    last_error: String::new(),
                    last_synced_at: Some(1),
                    created_at: 1,
                    updated_at: 1,
                },
                &[],
                &[],
            )
            .expect("mcp server should persist");
        state
            .store
            .upsert_mcp_tool(&McpToolRecord {
                id: "cloudflare-api.search".to_string(),
                server_id: "cloudflare-api".to_string(),
                name: "search".to_string(),
                description: "Search Cloudflare docs".to_string(),
                input_schema: json!({"type":"object"}),
                source: "cloudflare-api".to_string(),
                discovered_at: 1,
                created_at: 1,
                updated_at: 1,
            })
            .expect("mcp tool should persist");

        let capabilities = mcp_tool_capabilities(&state);
        assert_eq!(capabilities.len(), 1);
        assert_eq!(capabilities[0].tool_id, "cloudflare-api.search");

        let (job_id, worker, tool_call_id) =
            create_command_test_context(&state, "registered-non-mcp-tool-call");
        state
            .store
            .replace_tool_capability_grants(&worker.id, &capabilities)
            .expect("worker mcp capability should persist");
        let worker = state
            .store
            .get_job(&job_id)
            .expect("job should reload")
            .workers
            .into_iter()
            .find(|candidate| candidate.id == worker.id)
            .expect("worker should reload with mcp capability");
        let mut checkpoint = WorkerCheckpoint {
            session_id: "session-registered-non-mcp".to_string(),
            prompt_text: String::new(),
            images: Vec::new(),
            conversation: Vec::new(),
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        };
        let session = SessionDetail {
            session: scope_test_session(
                worker.working_dir.as_str(),
                "workspace_scratch",
                "scratch_only",
                Vec::new(),
            ),
            turns: Vec::new(),
        };
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);

        let preview = preview_approval_tool(
            &state,
            &worker,
            "cloudflare-api.search",
            &json!({"query":"workers ai"}),
        )
        .expect("registered non-mcp MCP tool should have an approval preview");
        assert!(
            preview
                .detail
                .contains("cloudflare-api.search through the Nucleus action bridge")
        );

        let result = execute_granted_tool(
            &state,
            &session,
            &job_id,
            &worker,
            &tool_call_id,
            &mut checkpoint,
            &mut cancel_rx,
            "cloudflare-api.search",
            json!({"query":"workers ai"}),
        )
        .await
        .expect("registered non-mcp MCP tool id should execute through MCP bridge");

        assert_eq!(result["content"][0]["text"], "cloudflare:workers ai");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn env_bearer_mcp_tool_invocation_fails_closed() {
        let state_dir = test_state_dir("mcp-env-bearer-tool-call");
        let state = initialize_test_state(&state_dir);
        let server = McpServerRecord {
            id: "mcp.env".to_string(),
            workspace_id: "workspace".to_string(),
            title: "Env MCP".to_string(),
            transport: "streamable-http".to_string(),
            command: String::new(),
            args: Vec::new(),
            env_json: json!({}),
            url: "http://127.0.0.1:9/mcp".to_string(),
            headers_json: json!({}),
            auth_kind: "env_bearer".to_string(),
            auth_ref: "NUCLEUS_TEST_MCP_ENV_TOKEN".to_string(),
            enabled: true,
            sync_status: "ready".to_string(),
            last_error: String::new(),
            last_synced_at: Some(1),
            created_at: 1,
            updated_at: 1,
        };
        let tool = McpToolRecord {
            id: "mcp.env.lookup".to_string(),
            server_id: server.id.clone(),
            name: "lookup".to_string(),
            description: "Lookup".to_string(),
            input_schema: json!({"type":"object"}),
            source: server.id.clone(),
            discovered_at: 1,
            created_at: 1,
            updated_at: 1,
        };

        unsafe {
            env::set_var("NUCLEUS_TEST_MCP_ENV_TOKEN", "would-not-be-used");
        }
        let result = invoke_mcp_http_tool(&state, &server, &tool, json!({}), None)
            .await
            .expect_err("env bearer invocation fails closed even when env var exists");
        assert!(result.to_string().contains("auth_migration_required"));
        unsafe {
            env::remove_var("NUCLEUS_TEST_MCP_ENV_TOKEN");
        }

        let _ = fs::remove_dir_all(&state_dir);
    }

    fn initialize_test_state(state_dir: &Path) -> AppState {
        let workspace_root = state_dir.join("workspace");
        if let Some(default_root) = dirs::home_dir().map(|path| path.join("dev-projects")) {
            fs::create_dir_all(default_root).expect("default workspace root should exist");
        }
        fs::create_dir_all(&workspace_root).expect("workspace root should exist");

        let store =
            Arc::new(StateStore::initialize_at(state_dir).expect("store should initialize"));
        store
            .update_workspace(
                Some(
                    workspace_root
                        .to_str()
                        .expect("workspace root should serialize as utf-8"),
                ),
                None,
                None,
                None,
                None,
            )
            .expect("workspace root should update");

        let (events, _) = broadcast::channel(8);
        AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store)),
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(AgentRuntime::default()),
            browser: Arc::new(crate::browser::BrowserRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        }
    }

    fn set_default_profile_utility_target(
        state: &AppState,
        provider: &str,
        model: &str,
        base_url: &str,
        api_key: &str,
    ) {
        let workspace = state.store.workspace().expect("workspace should load");
        let profile = workspace
            .profiles
            .into_iter()
            .find(|profile| profile.id == workspace.default_profile_id)
            .expect("default profile should exist");
        state
            .store
            .update_workspace_profile(
                &profile.id,
                nucleus_storage::WorkspaceProfilePatch {
                    title: profile.title,
                    main: profile.main,
                    utility: WorkspaceModelConfig {
                        adapter: provider.to_string(),
                        model: model.to_string(),
                        base_url: base_url.to_string(),
                        api_key: api_key.to_string(),
                    },
                    is_default: true,
                },
            )
            .expect("default utility profile should update");
    }

    async fn spawn_response_sequence_openai_server(
        contents: Vec<&'static str>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let server = tokio::spawn(async move {
            for (index, content) in contents.into_iter().enumerate() {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("test request should connect");
                let _ = read_test_http_body(&mut socket).await;
                write_test_openai_sse_response(&mut socket, &format!("turn-{index}"), content)
                    .await;
            }
        });
        (base_url, server)
    }

    async fn spawn_single_unauthorized_openai_server(
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("test request should connect");
            let _ = read_test_http_body(&mut socket).await;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("test response should write");
        });
        (base_url, server)
    }

    #[derive(Clone, Copy)]
    struct TestOpenAiProviderResponse {
        status: u16,
        retry_after_secs: Option<u64>,
        body: &'static str,
        content: Option<&'static str>,
    }

    async fn spawn_retry_openai_server(
        responses: Vec<TestOpenAiProviderResponse>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = request_count.clone();
        let server = tokio::spawn(async move {
            for (index, response) in responses.into_iter().enumerate() {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("test request should connect");
                let _ = read_test_http_body(&mut socket).await;
                server_request_count.fetch_add(1, Ordering::SeqCst);
                if response.status == 200 {
                    write_test_openai_sse_response(
                        &mut socket,
                        &format!("retry-turn-{index}"),
                        response.content.expect("successful response needs content"),
                    )
                    .await;
                } else {
                    write_test_http_status_response(
                        &mut socket,
                        response.status,
                        response.retry_after_secs,
                        response.body,
                    )
                    .await;
                }
            }
        });
        (base_url, request_count, server)
    }

    struct DynamicOpenAiProviderResponse {
        status: u16,
        body: String,
        content: Option<String>,
        delay_ms: u64,
    }

    impl DynamicOpenAiProviderResponse {
        fn content(content: impl Into<String>) -> Self {
            Self {
                status: 200,
                body: String::new(),
                content: Some(content.into()),
                delay_ms: 0,
            }
        }

        fn delayed(mut self, delay_ms: u64) -> Self {
            self.delay_ms = delay_ms;
            self
        }
    }

    async fn spawn_dynamic_openai_server<F>(
        max_requests: usize,
        responder: F,
    ) -> (
        String,
        Arc<AtomicUsize>,
        Arc<TestMutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    )
    where
        F: Fn(usize, &str) -> DynamicOpenAiProviderResponse + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let base_url = format!(
            "http://{}/v1",
            listener
                .local_addr()
                .expect("listener addr should be available")
        );
        let request_count = Arc::new(AtomicUsize::new(0));
        let request_bodies = Arc::new(TestMutex::new(Vec::new()));
        let responder = Arc::new(responder);
        let server_request_count = request_count.clone();
        let server_request_bodies = request_bodies.clone();
        let server = tokio::spawn(async move {
            let mut handlers = Vec::with_capacity(max_requests);
            for _ in 0..max_requests {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .expect("test request should connect");
                let responder = responder.clone();
                let request_count = server_request_count.clone();
                let request_bodies = server_request_bodies.clone();
                handlers.push(tokio::spawn(async move {
                    let body = read_test_http_body(&mut socket).await;
                    request_bodies
                        .lock()
                        .expect("request bodies lock should not be poisoned")
                        .push(body.clone());
                    let index = request_count.fetch_add(1, Ordering::SeqCst);
                    let response = responder(index, &body);
                    if response.delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(response.delay_ms)).await;
                    }
                    if response.status == 200 {
                        write_test_openai_sse_response(
                            &mut socket,
                            "dynamic-turn",
                            response
                                .content
                                .as_deref()
                                .expect("successful response needs content"),
                        )
                        .await;
                    } else {
                        write_test_http_status_response(
                            &mut socket,
                            response.status,
                            None,
                            &response.body,
                        )
                        .await;
                    }
                }));
            }

            for handler in handlers {
                handler.await.expect("test request handler should finish");
            }
        });
        (base_url, request_count, request_bodies, server)
    }

    fn create_command_test_context(
        state: &AppState,
        label: &str,
    ) -> (String, WorkerSummary, String) {
        let workspace_root = PathBuf::from(
            state
                .store
                .workspace()
                .expect("workspace should load")
                .root_path,
        );
        let working_dir = workspace_root.join(label);
        fs::create_dir_all(&working_dir).expect("working dir should exist");

        let job_id = format!("job-{label}");
        state
            .store
            .create_job(JobRecord {
                id: job_id.clone(),
                session_id: None,
                parent_job_id: None,
                template_id: None,
                title: format!("Job {label}"),
                purpose: "test".to_string(),
                trigger_kind: "manual".to_string(),
                state: "running".to_string(),
                requested_by: "test".to_string(),
                prompt_excerpt: String::new(),
                publication_intent_text: None,
            })
            .expect("job should persist");

        let worker = state
            .store
            .create_worker(WorkerRecord {
                id: format!("worker-{label}"),
                job_id: job_id.clone(),
                parent_worker_id: None,
                title: format!("Worker {label}"),
                lane: "utility".to_string(),
                state: "running".to_string(),
                provider: "test".to_string(),
                model: "test".to_string(),
                provider_base_url: String::new(),
                provider_api_key: String::new(),
                provider_session_id: String::new(),
                working_dir: working_dir.display().to_string(),
                read_roots: vec![working_dir.display().to_string()],
                write_roots: vec![working_dir.display().to_string()],
                max_steps: 10,
                max_tool_calls: 10,
                max_wall_clock_secs: 30,
            })
            .expect("worker should persist");
        state
            .store
            .replace_tool_capability_grants(&worker.id, &execution_capabilities())
            .expect("worker capabilities should persist");
        let worker = state
            .store
            .get_job(&job_id)
            .expect("job should reload")
            .workers
            .into_iter()
            .find(|candidate| candidate.id == worker.id)
            .expect("worker should reload with capabilities");

        let tool_call_id = format!("tool-call-{label}");
        state
            .store
            .create_tool_call(ToolCallRecord {
                id: tool_call_id.clone(),
                job_id: job_id.clone(),
                worker_id: worker.id.clone(),
                tool_id: "command.session.open".to_string(),
                status: "pending".to_string(),
                summary: "Open a command session".to_string(),
                args_json: json!({}),
                result_json: None,
                policy_decision: None,
                artifact_ids: Vec::new(),
                error_class: String::new(),
                error_detail: String::new(),
                started_at: None,
                completed_at: None,
            })
            .expect("tool call should persist");

        (job_id, worker, tool_call_id)
    }

    fn test_image(display_name: &str) -> SessionTurnImage {
        SessionTurnImage {
            display_name: display_name.to_string(),
            mime_type: "image/png".to_string(),
            data_url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
        }
    }

    async fn read_test_http_body(socket: &mut tokio::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = socket
                .read(&mut chunk)
                .await
                .expect("test request should read");
            assert!(read > 0, "test request closed before headers");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(index) = http_header_end(&buffer) {
                break index;
            }
        };
        let content_start = header_end + 4;
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);

        while buffer.len() < content_start + content_length {
            let read = socket
                .read(&mut chunk)
                .await
                .expect("test request body should read");
            assert!(read > 0, "test request closed before body");
            buffer.extend_from_slice(&chunk[..read]);
        }

        String::from_utf8_lossy(&buffer[content_start..content_start + content_length]).to_string()
    }

    fn http_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    async fn write_test_openai_sse_response(
        socket: &mut tokio::net::TcpStream,
        id: &str,
        content: &str,
    ) {
        let chunk = serde_json::json!({
            "id": id,
            "choices": [
                {
                    "delta": {
                        "content": content,
                    },
                },
            ],
        });
        let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("test response should write");
    }

    async fn write_test_openai_sse_response_without_done(
        socket: &mut tokio::net::TcpStream,
        id: &str,
        content: &str,
    ) {
        let chunk = serde_json::json!({
            "id": id,
            "choices": [
                {
                    "delta": {
                        "content": content,
                    },
                },
            ],
        });
        let body = format!("data: {chunk}\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("test response should write");
    }

    async fn write_test_http_status_response(
        socket: &mut tokio::net::TcpStream,
        status: u16,
        retry_after_secs: Option<u64>,
        body: &str,
    ) {
        let reason = match status {
            401 => "Unauthorized",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            _ => "Error",
        };
        let retry_after = retry_after_secs
            .map(|seconds| format!("retry-after: {seconds}\r\n"))
            .unwrap_or_default();
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n{retry_after}content-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("test response should write");
    }

    fn test_wait(until: WaitUntil) -> WorkerWaitRecord {
        WorkerWaitRecord {
            id: "wait-test".to_string(),
            summary: "test wait".to_string(),
            until,
            max_wait_seconds: Some(60),
            wake_note: None,
            started_at: 0,
            last_checked_at: None,
        }
    }

    fn create_waiting_test_job(
        state: &AppState,
        session_id: &str,
        workspace_root: &Path,
        provider_base_url: &str,
        until: WaitUntil,
        max_wait_seconds: Option<u64>,
    ) -> (String, String) {
        state
            .store
            .create_session(test_session_record(
                session_id,
                "Waiting test session",
                workspace_root,
            ))
            .expect("session should persist");
        state
            .store
            .update_session(
                session_id,
                SessionPatch {
                    state: Some("running".to_string()),
                    ..SessionPatch::default()
                },
            )
            .expect("session should become running");
        let job_id = format!("{session_id}-job");
        let worker_id = format!("{session_id}-worker");
        state
            .store
            .create_job(JobRecord {
                id: job_id.clone(),
                session_id: Some(session_id.to_string()),
                parent_job_id: None,
                template_id: None,
                title: "Waiting job".to_string(),
                purpose: "wait".to_string(),
                trigger_kind: "session_prompt".to_string(),
                state: "waiting".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "wait".to_string(),
                publication_intent_text: Some("wait".to_string()),
            })
            .expect("job should persist");
        state
            .store
            .create_worker(WorkerRecord {
                id: worker_id.clone(),
                job_id: job_id.clone(),
                parent_worker_id: None,
                title: "Utility Worker".to_string(),
                lane: ACTION_EXECUTOR_LANE.to_string(),
                state: "queued".to_string(),
                provider: "openai_compatible".to_string(),
                model: "test-model".to_string(),
                provider_base_url: provider_base_url.to_string(),
                provider_api_key: "test-key".to_string(),
                provider_session_id: String::new(),
                working_dir: workspace_root.display().to_string(),
                read_roots: vec![workspace_root.display().to_string()],
                write_roots: vec![workspace_root.display().to_string()],
                max_steps: 10,
                max_tool_calls: 10,
                max_wall_clock_secs: 30,
            })
            .expect("worker should persist");
        state
            .store
            .update_job(
                &job_id,
                JobPatch {
                    root_worker_id: Some(worker_id.clone()),
                    ..JobPatch::default()
                },
            )
            .expect("job root worker should update");
        let wait = WorkerWaitRecord {
            id: "persisted-wait".to_string(),
            summary: "persisted wait".to_string(),
            until,
            max_wait_seconds,
            wake_note: Some("resume after persisted wait".to_string()),
            started_at: unix_timestamp(),
            last_checked_at: None,
        };
        state
            .store
            .update_worker(
                &worker_id,
                WorkerPatch {
                    state: Some("waiting".to_string()),
                    wait_until_json: Some(Some(
                        serde_json::to_value(&wait).expect("wait should encode"),
                    )),
                    wait_started_at: Some(Some(wait.started_at)),
                    ..WorkerPatch::default()
                },
            )
            .expect("worker wait should persist");
        state
            .store
            .write_worker_checkpoint(
                &worker_id,
                &serde_json::to_value(WorkerCheckpoint {
                    session_id: session_id.to_string(),
                    prompt_text: "wait".to_string(),
                    images: Vec::new(),
                    conversation: vec![CheckpointMessage {
                        role: "system".to_string(),
                        content: "Return exactly one JSON object and nothing else.".to_string(),
                        images: Vec::new(),
                        compacted: false,
                        compacted_range: None,
                    }],
                    next_prompt: None,
                    pending_action: None,
                    browser_verification_final_answer_rejected: false,
                    patch_loop_guardrail_triggered: false,
                })
                .expect("checkpoint should encode"),
            )
            .expect("checkpoint should persist");
        (job_id, worker_id)
    }

    async fn wait_for_latest_job_state(
        state: &AppState,
        session_id: &str,
        expected_state: &str,
    ) -> JobDetail {
        for _ in 0..200 {
            let jobs = state
                .store
                .list_jobs_for_session(session_id)
                .expect("session jobs should load while polling");
            if let Some(job) = jobs.first() {
                let detail = state.store.get_job(&job.id).expect("job should load");
                if detail.job.state == expected_state {
                    return detail;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!("latest session job did not reach state '{expected_state}'");
    }

    async fn wait_for_job_state(state: &AppState, job_id: &str, expected_state: &str) -> JobDetail {
        for _ in 0..500 {
            let detail = state.store.get_job(job_id).expect("job should load");
            if detail.job.state == expected_state {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!("job '{job_id}' did not reach state '{expected_state}'");
    }

    async fn wait_for_child_count(
        state: &AppState,
        session_id: &str,
        expected_count: usize,
    ) -> JobDetail {
        for _ in 0..500 {
            let detail = latest_job_detail(state, session_id);
            if detail.child_jobs.len() == expected_count {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!("latest session job did not record {expected_count} child jobs");
    }

    fn latest_job_detail(state: &AppState, session_id: &str) -> JobDetail {
        let jobs = state
            .store
            .list_jobs_for_session(session_id)
            .expect("session jobs should load");
        let job = jobs.first().expect("session should have a job");
        state
            .store
            .get_job(&job.id)
            .expect("job detail should load")
    }

    fn spawn_child_jobs_action_json(
        count: usize,
        failing_index: Option<usize>,
        prompt_prefix: &str,
    ) -> String {
        let jobs = (0..count)
            .map(|index| {
                let prompt = if Some(index) == failing_index {
                    format!("{prompt_prefix}-{index}: child-fail")
                } else {
                    format!("{prompt_prefix}-{index}: child-success")
                };
                json!({
                    "title": format!("Child {index}"),
                    "prompt": prompt,
                    "working_dir": null,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "kind": "spawn_child_jobs",
            "summary": format!("fan out to {count} child jobs"),
            "jobs": jobs,
        })
        .to_string()
    }

    fn create_manual_child_jobs(
        state: &AppState,
        session_id: &str,
        parent_job_id: &str,
        count: usize,
    ) -> Vec<String> {
        (0..count)
            .map(|index| {
                let id = format!("{parent_job_id}-manual-child-{index}");
                state
                    .store
                    .create_job(JobRecord {
                        id: id.clone(),
                        session_id: Some(session_id.to_string()),
                        parent_job_id: Some(parent_job_id.to_string()),
                        template_id: None,
                        title: format!("Manual child {index}"),
                        purpose: "wait test child".to_string(),
                        trigger_kind: "child_job".to_string(),
                        state: "running".to_string(),
                        requested_by: "agent".to_string(),
                        prompt_excerpt: String::new(),
                        publication_intent_text: None,
                    })
                    .expect("manual child should persist");
                id
            })
            .collect()
    }

    fn mark_job_state(state: &AppState, job_id: &str, next_state: &str) {
        state
            .store
            .update_job(
                job_id,
                JobPatch {
                    state: Some(next_state.to_string()),
                    last_error: (next_state == "failed")
                        .then(|| "child failed for wait test".to_string()),
                    ..JobPatch::default()
                },
            )
            .expect("job state should update");
    }

    fn replace_worker_wait(state: &AppState, worker_id: &str, until: WaitUntil) {
        let started_at = unix_timestamp();
        let wait = WorkerWaitRecord {
            id: format!("{worker_id}-wait"),
            summary: "wait for manual child jobs".to_string(),
            until,
            max_wait_seconds: Some(60),
            wake_note: Some("resume after children finish".to_string()),
            started_at,
            last_checked_at: None,
        };
        state
            .store
            .update_worker(
                worker_id,
                WorkerPatch {
                    wait_until_json: Some(Some(
                        serde_json::to_value(&wait).expect("wait should encode"),
                    )),
                    wait_started_at: Some(Some(started_at)),
                    last_error: Some(wait_status_text(&wait, started_at)),
                    ..WorkerPatch::default()
                },
            )
            .expect("worker wait should update");
    }

    fn create_parent_fanout_context(
        state: &AppState,
        label: &str,
        workspace_root: &Path,
        provider_base_url: &str,
    ) -> (SessionDetail, String, WorkerSummary) {
        let session_id = format!("{label}-session");
        state
            .store
            .create_session(test_session_record(
                &session_id,
                "Parent fanout context",
                workspace_root,
            ))
            .expect("session should persist");
        let job_id = format!("{label}-parent-job");
        state
            .store
            .create_job(JobRecord {
                id: job_id.clone(),
                session_id: Some(session_id.clone()),
                parent_job_id: None,
                template_id: None,
                title: "Parent fanout job".to_string(),
                purpose: "test parent".to_string(),
                trigger_kind: "session_prompt".to_string(),
                state: "running".to_string(),
                requested_by: "user".to_string(),
                prompt_excerpt: "parent".to_string(),
                publication_intent_text: None,
            })
            .expect("parent job should persist");
        let worker_id = format!("{label}-parent-worker");
        let worker = state
            .store
            .create_worker(WorkerRecord {
                id: worker_id.clone(),
                job_id: job_id.clone(),
                parent_worker_id: None,
                title: "Parent utility worker".to_string(),
                lane: "utility".to_string(),
                state: "running".to_string(),
                provider: "openai_compatible".to_string(),
                model: "test-model".to_string(),
                provider_base_url: provider_base_url.to_string(),
                provider_api_key: "test-key".to_string(),
                provider_session_id: String::new(),
                working_dir: workspace_root.display().to_string(),
                read_roots: vec![workspace_root.display().to_string()],
                write_roots: vec![workspace_root.display().to_string()],
                max_steps: 100,
                max_tool_calls: 100,
                max_wall_clock_secs: 300,
            })
            .expect("parent worker should persist");
        state
            .store
            .update_job(
                &job_id,
                JobPatch {
                    root_worker_id: Some(worker_id),
                    ..JobPatch::default()
                },
            )
            .expect("parent root worker should update");
        let session = state.store.get_session(&session_id).expect("session loads");
        (session, job_id, worker)
    }

    fn test_session_record(id: &str, title: &str, working_dir: &Path) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            title: title.to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            project_ids: Vec::new(),
            provider: "openai_compatible".to_string(),
            model: "test-model".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            working_dir: working_dir.display().to_string(),
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

    fn test_worker_summary(id: &str, max_steps: usize, max_tool_calls: usize) -> WorkerSummary {
        WorkerSummary {
            id: id.to_string(),
            job_id: format!("{id}-job"),
            parent_worker_id: None,
            title: "Root worker".to_string(),
            lane: "utility".to_string(),
            state: "running".to_string(),
            provider: "openai_compatible".to_string(),
            model: "test-model".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            provider_session_id: String::new(),
            working_dir: "/tmp/nucleus-test".to_string(),
            read_roots: vec!["/tmp/nucleus-test".to_string()],
            write_roots: vec!["/tmp/nucleus-test".to_string()],
            max_steps,
            max_tool_calls,
            max_wall_clock_secs: 300,
            step_count: 0,
            tool_call_count: 0,
            wait_until_json: None,
            wait_started_at: None,
            last_error: String::new(),
            user_error: None,
            capabilities: Vec::new(),
            last_reasoning: String::new(),
            last_reasoning_at: None,
            token_usage_known: false,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost_usd_estimate: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn long_test_checkpoint(session_id: &str, message_count: usize) -> WorkerCheckpoint {
        let mut conversation = vec![CheckpointMessage {
            role: "system".to_string(),
            content: "Return exactly one JSON object and nothing else.".to_string(),
            images: Vec::new(),
            compacted: false,
            compacted_range: None,
        }];
        conversation.extend((1..message_count).map(|index| CheckpointMessage {
            role: if index % 2 == 0 { "assistant" } else { "user" }.to_string(),
            content: format!(
                "long running session turn {index}: PR #240 sentinel {}",
                "x".repeat(1_000)
            ),
            images: Vec::new(),
            compacted: false,
            compacted_range: None,
        }));

        WorkerCheckpoint {
            session_id: session_id.to_string(),
            prompt_text: "Long running task".to_string(),
            images: Vec::new(),
            conversation,
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        }
    }

    fn test_publication_job_summary(id: &str) -> JobSummary {
        JobSummary {
            id: id.to_string(),
            session_id: Some(format!("{id}-session")),
            parent_job_id: None,
            template_id: None,
            title: "Prompt open a PR".to_string(),
            purpose: "Session prompt".to_string(),
            trigger_kind: "session_prompt".to_string(),
            state: "running".to_string(),
            requested_by: "user".to_string(),
            prompt_excerpt: "open a pr to merge to dev".to_string(),
            root_worker_id: Some(format!("{id}-worker")),
            executor_lane: "utility".to_string(),
            executor_provider: "openai_compatible".to_string(),
            executor_model: "gpt-5.4-mini".to_string(),
            visible_turn_id: None,
            result_summary: String::new(),
            last_error: String::new(),
            user_error: None,
            ui_renderable: "unknown".to_string(),
            browser_verification_required: false,
            browser_verification_summary: String::new(),
            browser_verification_artifact_ids: Vec::new(),
            publication_requested: true,
            publication_status: "not_opened".to_string(),
            publication_summary: String::new(),
            pr_url: String::new(),
            source_branch: String::new(),
            target_branch: String::new(),
            validation_status: "not_performed".to_string(),
            browser_verification_status: "not_performed".to_string(),
            cleanup_status: "unknown".to_string(),
            cleanup_paths: Vec::new(),
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
            created_at: 0,
            updated_at: 0,
        }
    }

    fn test_job_detail_with_prompt(prompt: &str) -> JobDetail {
        let mut job = test_publication_job_summary("evidence-job");
        job.title = "PR feedback".to_string();
        job.purpose = "Session prompt".to_string();
        job.prompt_excerpt = prompt.to_string();
        JobDetail {
            job,
            workers: Vec::new(),
            child_jobs: Vec::new(),
            tool_calls: Vec::new(),
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: Vec::new(),
        }
    }

    fn test_checkpoint_with_prompt(prompt: &str) -> WorkerCheckpoint {
        WorkerCheckpoint {
            session_id: "session-evidence".to_string(),
            prompt_text: prompt.to_string(),
            images: Vec::new(),
            conversation: Vec::new(),
            next_prompt: None,
            pending_action: None,
            browser_verification_final_answer_rejected: false,
            patch_loop_guardrail_triggered: false,
        }
    }

    fn test_tool_call_summary(
        tool_id: &str,
        result_json: Value,
    ) -> nucleus_protocol::ToolCallSummary {
        nucleus_protocol::ToolCallSummary {
            id: format!("{tool_id}-call"),
            job_id: "evidence-job".to_string(),
            worker_id: "worker-evidence".to_string(),
            tool_id: tool_id.to_string(),
            status: "completed".to_string(),
            summary: String::new(),
            args_json: json!({}),
            result_json: Some(result_json),
            policy_decision: None,
            artifact_ids: Vec::new(),
            error_class: String::new(),
            error_detail: String::new(),
            created_at: 0,
            started_at: Some(0),
            completed_at: Some(1),
        }
    }

    async fn wait_for_session_state(
        state: &AppState,
        session_id: &str,
        expected_state: &str,
    ) -> SessionDetail {
        for _ in 0..500 {
            let detail = state
                .store
                .get_session(session_id)
                .expect("session should load while polling");
            if detail.session.state == expected_state {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!("session '{session_id}' did not reach state '{expected_state}'");
    }

    fn test_instance_runtime() -> InstanceRuntime {
        InstanceRuntime::for_test(
            "Test",
            env::current_dir().expect("cwd should resolve"),
            "127.0.0.1:42241",
            "managed_release",
        )
    }

    fn test_state_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "nucleus-agent-{label}-{}-{suffix}",
            std::process::id()
        ))
    }
}
