use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
};

use anyhow::{Context, Result, anyhow, bail};
use nucleus_protocol::{
    ApprovalRequestSummary, ArtifactSummary, CommandSessionSummary, CreatePlaybookRequest,
    DaemonEvent, JobDetail, JobSummary, PlaybookDetail, PlaybookSummary, PromptProgressUpdate,
    SessionDetail, SessionPromptRequest, SessionSummary, SessionTurn, SessionTurnImage,
    UpdatePlaybookRequest, WorkerSummary, WorkspaceProfileSummary, WorkspaceSummary,
};
use nucleus_storage::{
    ApprovalRequestRecord, AuditEventRecord, CommandSessionPatch, CommandSessionRecord,
    JobArtifactPatch, JobArtifactRecord, JobEventRecord, JobPatch, JobRecord, PlaybookPatch,
    PlaybookRecord, PolicyDecisionRecord, SessionPatch, SessionRecord, ToolCallPatch,
    ToolCallRecord, ToolCapabilityGrantRecord, WorkerPatch, WorkerRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Mutex, mpsc, oneshot, watch},
    time::{Duration, timeout},
};
use tracing::warn;
use uuid::Uuid;

use super::{
    ApiError, AppState, assemble_prompt_input, ensure_prompting_runtime, excerpt,
    publish_overview_event, publish_prompt_progress_event, publish_session_event,
    resolve_session_projects, resolve_workspace_profile, resolve_workspace_profile_target,
    try_record_audit_event, unix_timestamp,
};
use crate::runtime::PromptStreamEvent;

const JOB_MAX_STEPS: usize = 10;
const JOB_MAX_TOOL_CALLS: usize = 20;
const JOB_MAX_WALL_CLOCK_SECS: u64 = 300;
const JOB_MAX_CHILDREN_PER_FANOUT: usize = 3;
const CHILD_JOB_MAX_STEPS: usize = 6;
const CHILD_JOB_MAX_TOOL_CALLS: usize = 12;
const CHILD_JOB_POLL_INTERVAL_MS: u64 = 250;
const TOOL_OUTPUT_CHAR_LIMIT: usize = 8_000;
const READ_FILE_CHAR_LIMIT: usize = 12_000;
const LIST_LIMIT: usize = 120;
const RG_LIMIT: usize = 80;
const DIFF_PREVIEW_CHAR_LIMIT: usize = 12_000;
const COMMAND_PREVIEW_CHAR_LIMIT: usize = 4_000;
const COMMAND_DEFAULT_TIMEOUT_SECS: u64 = 300;
const COMMAND_MAX_TIMEOUT_SECS: u64 = 1_800;
const COMMAND_DEFAULT_OUTPUT_LIMIT_BYTES: usize = 131_072;
const COMMAND_MAX_OUTPUT_LIMIT_BYTES: usize = 524_288;
const COMMAND_DEFAULT_WAIT_FOR_OUTPUT_MS: u64 = 250;
const COMMAND_MAX_WAIT_FOR_OUTPUT_MS: u64 = 2_000;
const COMMAND_STATE_SETTLE_WAIT_MS: u64 = 50;
const WRITE_LOCK_POLL_INTERVAL_MS: u64 = 250;
const PLAYBOOK_SCHEDULER_INTERVAL_SECS: u64 = 30;
const PLAYBOOK_MIN_INTERVAL_SECS: u64 = 60;
const PLAYBOOK_MAX_INTERVAL_SECS: u64 = 86_400;
const COMMAND_TRUNCATED_NOTE: &str = "[output truncated by the daemon budget]";

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
struct WorkerCheckpoint {
    session_id: String,
    prompt_text: String,
    conversation: Vec<CheckpointMessage>,
    next_prompt: Option<String>,
    pending_action: Option<PendingToolAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingToolAction {
    #[serde(default)]
    action_kind: String,
    tool_call_id: String,
    approval_id: Option<String>,
    command_session_id: Option<String>,
    #[serde(default)]
    child_job_ids: Vec<String>,
    summary: String,
    tool: String,
    args: Value,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkerAction {
    ToolCall {
        summary: String,
        tool: String,
        #[serde(default)]
        args: Value,
    },
    SpawnChildJobs {
        summary: String,
        jobs: Vec<ChildJobProposal>,
    },
    FinalAnswer {
        summary: String,
        final_answer: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct ChildJobProposal {
    title: String,
    prompt: String,
    working_dir: Option<String>,
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
}

pub async fn start_text_prompt_job(
    state: AppState,
    session_id: String,
    payload: SessionPromptRequest,
    current: SessionDetail,
    execution_prompt: String,
) -> Result<SessionDetail, ApiError> {
    if current.session.state == "paused" {
        return Err(ApiError::bad_request(
            "this session has a paused job that must be resumed or canceled first",
        ));
    }

    let prompt_excerpt = excerpt(&execution_prompt, 160);
    let job_id = Uuid::new_v4().to_string();
    let root_worker_id = Uuid::new_v4().to_string();
    let target = resolve_hidden_worker_target(&state, &current.session).await?;

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
        execution_prompt.as_str(),
        &payload.images,
    )?;

    let job = state.store.create_job(JobRecord {
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
    })?;

    let _created_worker = state.store.create_worker(WorkerRecord {
        id: root_worker_id.clone(),
        job_id: job_id.clone(),
        parent_worker_id: None,
        title: "Hidden utility worker".to_string(),
        lane: "utility".to_string(),
        state: "queued".to_string(),
        provider: target.provider.clone(),
        model: target.model.clone(),
        provider_base_url: target.provider_base_url.clone(),
        provider_api_key: target.provider_api_key.clone(),
        provider_session_id: String::new(),
        working_dir: current.session.working_dir.clone(),
        read_roots: worker_read_roots(&current.session),
        write_roots: worker_write_roots(&current.session),
        max_steps: JOB_MAX_STEPS,
        max_tool_calls: JOB_MAX_TOOL_CALLS,
        max_wall_clock_secs: JOB_MAX_WALL_CLOCK_SECS,
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
        .replace_tool_capability_grants(&root_worker_id, &root_worker_capabilities())?;
    let worker = state
        .store
        .get_job(&job_id)?
        .workers
        .into_iter()
        .find(|item| item.id == root_worker_id)
        .ok_or_else(|| ApiError::internal_message("failed to reload hidden worker capabilities"))?;

    let checkpoint = WorkerCheckpoint {
        session_id: session_id.clone(),
        prompt_text: execution_prompt.clone(),
        conversation: vec![CheckpointMessage {
            role: "system".to_string(),
            content: worker_system_prompt(&worker),
        }],
        next_prompt: None,
        pending_action: None,
    };
    state
        .store
        .write_worker_checkpoint(&root_worker_id, &serde_json::to_value(checkpoint).unwrap())?;

    let started = state.store.get_session(&session_id)?;
    let _ = publish_session_event(&state, started).await;
    publish_job_created(&state, &job).await;
    publish_worker_updated(&state, &worker).await;
    publish_prompt_status(
        &state,
        &current.session,
        &worker,
        "queued",
        "Queued hidden worker",
        "The daemon accepted the prompt and created a hidden utility worker.",
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
                "Queued hidden worker job for session '{}'.",
                current.session.title
            ),
            detail: format!(
                "session_id={} utility_provider={} utility_model={}",
                session_id, target.provider, target.model
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
        "completed" | "failed" | "canceled" => {
            return Ok(detail);
        }
        _ => {}
    }

    let subtree = collect_job_subtree_ids(&state, &job_id)?;
    for child_job_id in subtree.iter().rev() {
        let child_detail = state.store.get_job(child_job_id)?;
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
            let _ = state.store.update_worker(
                &worker.id,
                WorkerPatch {
                    state: Some("canceled".to_string()),
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
            summary: "Canceled hidden worker job.".to_string(),
            detail: "The daemon stopped the job before it finished.".to_string(),
            data_json: json!({}),
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
    if detail.job.state != "paused" {
        return Err(ApiError::bad_request(
            "only paused hidden worker jobs can be resumed",
        ));
    }

    let subtree = collect_job_subtree_ids(&state, &job_id)?;
    for child_job_id in subtree.iter().rev() {
        let child_detail = state.store.get_job(child_job_id)?;
        if child_detail.job.state != "paused" {
            continue;
        }
        state.store.update_job(
            child_job_id,
            JobPatch {
                state: Some("queued".to_string()),
                ..JobPatch::default()
            },
        )?;
        for worker in child_detail.workers {
            let _ = state.store.update_worker(
                &worker.id,
                WorkerPatch {
                    state: Some("queued".to_string()),
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
    let jobs = state.store.list_jobs_by_state(&["queued", "running"])?;
    for job in jobs {
        let _ = state.store.update_job(
            &job.id,
            JobPatch {
                state: Some("paused".to_string()),
                last_error: Some("The daemon restarted before this job completed.".to_string()),
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
                        "The daemon restarted before this worker completed.".to_string(),
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
                    last_error: Some("Resume or cancel the paused hidden worker job.".to_string()),
                    ..SessionPatch::default()
                },
            );
        }
        let _ = state.store.append_job_event(JobEventRecord {
            job_id: job.id.clone(),
            worker_id: None,
            event_type: "job.paused".to_string(),
            status: "paused".to_string(),
            summary: "Paused a hidden worker job after daemon restart.".to_string(),
            detail:
                "The daemon recovered persisted job state and is waiting for an explicit resume."
                    .to_string(),
            data_json: json!({ "reason": "daemon_restart" }),
        });
        publish_job_updated(state, &state.store.get_job(&job.id)?.job).await;
    }
    for command_session in state
        .store
        .list_command_sessions_by_state(&["starting", "running"])?
    {
        let _ = state.store.update_command_session(
            &command_session.id,
            CommandSessionPatch {
                state: Some("orphaned".to_string()),
                last_error: Some(
                    "The daemon restarted before this command session completed.".to_string(),
                ),
                completed_at: Some(Some(unix_timestamp())),
                ..CommandSessionPatch::default()
            },
        );
    }
    Ok(())
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

        for handle in handles {
            let _ = handle
                .control
                .send(CommandControl::Terminate {
                    reason: reason.to_string(),
                    final_state: final_state.to_string(),
                })
                .await;
        }
    }
}

async fn run_job(state: AppState, job_id: String) -> Result<()> {
    let Some(mut cancel_rx) = state.agent.register_job(&job_id).await else {
        return Ok(());
    };
    let result = run_job_loop(&state, &job_id, &mut cancel_rx).await;
    state.agent.finish_job(&job_id).await;
    result
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
    let session = state.store.get_session(&session_id)?;
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
        "Hidden worker running",
        "The daemon is planning the next repo-inspection step.",
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

        if step >= worker.max_steps {
            bail!("hidden worker reached the maximum step budget");
        }

        if tool_calls >= worker.max_tool_calls {
            bail!("hidden worker reached the maximum tool-call budget");
        }

        let prompt = checkpoint.next_prompt.take().unwrap_or_else(|| {
            build_initial_step_prompt(&session.session, &assembled_prompt.prompt, &worker)
        });

        publish_prompt_status(
            state,
            &session.session,
            &worker,
            "thinking",
            "Planning the next step",
            "The hidden worker is deciding whether to inspect the repo or answer directly.",
        )
        .await;

        let response = call_worker_model(state, &worker, &checkpoint.conversation, &prompt).await?;
        checkpoint.conversation.push(CheckpointMessage {
            role: "user".to_string(),
            content: prompt.clone(),
        });
        checkpoint.conversation.push(CheckpointMessage {
            role: "assistant".to_string(),
            content: response.raw.clone(),
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

        match response.action {
            WorkerAction::SpawnChildJobs { summary, jobs } => {
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
            } => {
                complete_job_with_final_answer(
                    state,
                    &session,
                    job_id,
                    &mut worker,
                    step + 1,
                    tool_calls,
                    &summary,
                    &final_answer,
                )
                .await?;
                return Ok(());
            }
            WorkerAction::ToolCall {
                summary,
                tool,
                args,
            } => {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopDisposition {
    Continue,
    Return,
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

    if let Some(approval_id) = pending.approval_id.as_deref() {
        let approval = state.store.get_approval_request(approval_id)?;
        match approval.state.as_str() {
            "pending" => {
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
                    detail: "The daemon resumed with the persisted command-session result after restart."
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
    *tool_calls += 1;
    let policy = policy_for_tool(&tool);
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
        bail!("only the root hidden worker may spawn child jobs");
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
        "Spawning child workers",
        &summary,
    )
    .await;
    Ok(LoopDisposition::Continue)
}

async fn create_child_job(
    state: &AppState,
    session: &SessionDetail,
    parent_job_id: &str,
    parent_worker: &WorkerSummary,
    proposal: ChildJobProposal,
) -> Result<String> {
    let title = proposal.title.trim();
    if title.is_empty() {
        bail!("child job titles must not be empty");
    }
    let prompt = proposal.prompt.trim();
    if prompt.is_empty() {
        bail!("child job prompts must not be empty");
    }
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
    })?;
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
        working_dir: working_dir.display().to_string(),
        read_roots,
        write_roots: Vec::new(),
        max_steps: CHILD_JOB_MAX_STEPS,
        max_tool_calls: CHILD_JOB_MAX_TOOL_CALLS,
        max_wall_clock_secs: JOB_MAX_WALL_CLOCK_SECS,
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
                "child worker '{}' was not found after creation",
                child_worker_id
            )
        })?;

    let checkpoint = WorkerCheckpoint {
        session_id: session.session.id.clone(),
        prompt_text: prompt.to_string(),
        conversation: vec![CheckpointMessage {
            role: "system".to_string(),
            content: worker_system_prompt(&child_worker),
        }],
        next_prompt: None,
        pending_action: None,
    };
    state.store.write_worker_checkpoint(
        &child_worker.id,
        &serde_json::to_value(checkpoint).context("failed to encode child worker checkpoint")?,
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
Decide the next single step. If the work is done, return final_answer.",
        summary,
        serde_json::to_string_pretty(results)
            .unwrap_or_else(|_| Value::Array(results.to_vec()).to_string())
    )
}

fn is_pending_child_job_action(pending: &PendingToolAction) -> bool {
    pending.action_kind == "child_jobs" || !pending.child_job_ids.is_empty()
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
) -> Result<()> {
    let detail = state.store.get_job(job_id)?;
    state
        .agent
        .terminate_job_command_sessions(
            job_id,
            "The job completed and closed any remaining daemon-owned command sessions.",
            "closed",
        )
        .await;

    let mut visible_turn_id = None;
    let mut report_artifact = None;
    if detail.job.parent_job_id.is_none() {
        let final_turn_id = Uuid::new_v4().to_string();
        state.store.append_session_turn(
            &session.session.id,
            &final_turn_id,
            "assistant",
            final_answer,
            &[],
        )?;
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
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "job.completed".to_string(),
        status: "completed".to_string(),
        summary: summary.to_string(),
        detail: excerpt(final_answer, 320),
        data_json: json!({ "step_count": step_count, "tool_call_count": tool_call_count }),
    });
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: "session.job.completed".to_string(),
            target: format!("job:{job_id}"),
            status: "success".to_string(),
            summary: format!(
                "Completed hidden worker job for session '{}'.",
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
            "Hidden worker completed",
            "The daemon persisted a clean assistant turn from the worker result.",
        )
        .await;
    } else {
        if let Some(artifact) = report_artifact.as_ref() {
            publish_artifact_added(state, artifact).await;
        }
        if let Some(parent_job_id) = detail.job.parent_job_id.as_deref() {
            publish_job_updated(state, &state.store.get_job(parent_job_id)?.job).await;
        }
    }

    publish_job_completed(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    let _ = publish_overview_event(state).await;
    Ok(())
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
        "You are handling a daemon-owned session prompt.\n\
Session title: {}\n\
{}\n\
Visible provider: {} / {}\n\
Hidden worker provider: {} / {}\n\
Prompt-time context and user request:\n{}\n\
Decide the next single step.",
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

fn build_tool_result_prompt(tool: &str, summary: &str, result: &Value) -> String {
    format!(
        "Tool result for {}.\nReason for the call: {}\nStructured result:\n{}\n\
Decide the next single step. If the work is done, return final_answer.",
        tool,
        summary,
        format_tool_result(result)
    )
}

fn build_tool_denied_prompt(tool: &str, summary: &str, reason: &str) -> String {
    format!(
        "The daemon did not allow {}.\nReason for the proposed call: {}\nResolution detail: {}\n\
Choose the next best single step. If the work can still be completed without this mutation, return final_answer.",
        tool, summary, reason
    )
}

async fn call_worker_model(
    state: &AppState,
    worker: &WorkerSummary,
    conversation: &[CheckpointMessage],
    prompt: &str,
) -> Result<ModelResponse> {
    let (events, mut receiver) = mpsc::unbounded_channel();
    let execution = build_execution_session(worker);
    let history = checkpoint_history(conversation, &execution.id);
    let prompt_body = build_worker_prompt_input(worker, conversation, prompt);
    let runtimes = state.runtimes.clone();
    let execution_clone = execution.clone();
    let history_clone = history.clone();
    let handle = tokio::spawn(async move {
        runtimes
            .execute_prompt_stream(&execution_clone, &history_clone, &prompt_body, &[], events)
            .await
    });

    let mut last_reasoning = String::new();
    while let Some(event) = receiver.recv().await {
        if let PromptStreamEvent::ReasoningSnapshot { text } = event {
            let excerpted = excerpt(&text, 240);
            if excerpted != last_reasoning {
                last_reasoning = excerpted;
            }
        }
    }

    let result = handle
        .await
        .map_err(|error| anyhow!("worker model task crashed: {error}"))??;
    let action = parse_worker_action(&result.content)?;

    Ok(ModelResponse {
        action,
        raw: result.content,
        provider_session_id: result.provider_session_id,
    })
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
            format!(
                "{}:\n{}",
                message.role.to_uppercase(),
                message.content.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "Replay the checkpoint conversation below as authoritative context.\n\
SYSTEM entries are binding instructions that must still be followed.\n\n\
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
        scope: "job".to_string(),
        project_count: 0,
        projects: Vec::new(),
        state: worker.state.clone(),
        provider_session_id: worker.provider_session_id.clone(),
        last_error: worker.last_error.clone(),
        last_message_excerpt: String::new(),
        turn_count: 0,
        created_at: worker.created_at,
        updated_at: worker.updated_at,
    }
}

fn checkpoint_history(messages: &[CheckpointMessage], session_id: &str) -> Vec<SessionTurn> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| SessionTurn {
            id: format!("{session_id}-history-{index}"),
            session_id: session_id.to_string(),
            role: message.role.clone(),
            content: message.content.clone(),
            images: Vec::<SessionTurnImage>::new(),
            created_at: index as i64,
        })
        .collect()
}

fn parse_worker_action(content: &str) -> Result<WorkerAction> {
    let trimmed = content.trim();
    if let Ok(parsed) = serde_json::from_str::<WorkerAction>(trimmed) {
        return Ok(parsed);
    }

    let start = trimmed
        .find('{')
        .ok_or_else(|| anyhow!("worker returned no JSON object"))?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| anyhow!("worker returned no JSON object"))?;
    let candidate = &trimmed[start..=end];
    serde_json::from_str::<WorkerAction>(candidate).with_context(|| {
        format!(
            "worker returned invalid JSON action: {}",
            excerpt(trimmed, 220)
        )
    })
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
                "Approved a daemon-owned tool mutation.".to_string()
            } else {
                "Denied a daemon-owned tool mutation.".to_string()
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
        other => bail!("unsupported tool '{}'", other),
    }
}

fn preview_approval_tool(
    _state: &AppState,
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
        other => bail!("'{}' does not support approval previews", other),
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
    let plan = render_command_plan(&spec, "Run a bounded daemon-owned command.");
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
    let plan = render_command_plan(&spec, "Open a daemon-owned interactive command session.");
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
        Some("Daemon-owned test run".to_string()),
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
                "environment variable '{}' is not allowed for daemon command tools",
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
    shell_quoted_command(spec)
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
        Some("Daemon-owned command".to_string()),
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        args.network_policy,
        args.env,
        false,
    )?;
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
        Some("Daemon-owned test run".to_string()),
        args.command,
        args.args,
        args.cwd,
        args.timeout_secs,
        args.output_limit_bytes,
        Some("inherit".to_string()),
        args.env,
        true,
    )?;
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
        detail: render_command_plan(spec, "Daemon-owned command session started."),
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
                        reason: format!("{label} was canceled by the daemon."),
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
                        exit_code = status.code();
                        if final_state == "running" {
                            if status.success() {
                                final_state = "completed".to_string();
                            } else {
                                final_state = "failed".to_string();
                                last_error = format_command_exit_error(status);
                            }
                        }
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
                    "command exceeded the {} second daemon timeout",
                    summary.timeout_secs
                );
                let _ = terminate_command_process(&mut child).await;
                if let Ok(status) = child.wait().await {
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
                        let _ = reply.send(Ok(snapshot_live_command_output(&live_output)));
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
                                let _ = terminate_command_process(&mut child).await;
                                match child.wait().await {
                                    Ok(status) => {
                                        exit_code = status.code();
                                        if !status.success() {
                                            last_error = format_command_exit_error(status);
                                        }
                                    }
                                    Err(error) => {
                                        last_error = error.to_string();
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
                        let _ = terminate_command_process(&mut child).await;
                        if let Ok(status) = child.wait().await {
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
    ArtifactDraft {
        kind: kind.to_string(),
        title,
        mime_type: mime_type.to_string(),
        extension: extension.to_string(),
        preview_text: excerpt(&content, DIFF_PREVIEW_CHAR_LIMIT),
        content,
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
    })
}

fn resolve_scoped_path(
    worker: &WorkerSummary,
    input: &str,
    allow_missing: bool,
) -> Result<PathBuf> {
    resolve_scoped_path_in_roots(worker, input, &worker.read_roots, allow_missing, "read")
}

async fn command_output(command: &str, args: &[&str]) -> Result<String> {
    let mut child = Command::new(command);
    child.args(args);
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

fn policy_for_tool(tool: &str) -> PolicyDecisionRecord {
    if requires_approval_for_tool(tool) {
        PolicyDecisionRecord {
            decision: "require_approval".to_string(),
            reason: if is_mutating_tool(tool) {
                "repo mutations require explicit operator approval".to_string()
            } else {
                "daemon-owned command launches require explicit operator approval".to_string()
            },
            matched_rule: if is_mutating_tool(tool) {
                format!("approval:mutation:{tool}")
            } else {
                format!("approval:command:{tool}")
            },
            scope_kind: if is_mutating_tool(tool) {
                "path"
            } else {
                "process"
            }
            .to_string(),
            risk_level: if is_mutating_tool(tool) {
                "medium".to_string()
            } else {
                "high".to_string()
            },
        }
    } else {
        PolicyDecisionRecord {
            decision: "allow".to_string(),
            reason: if is_command_follow_up_tool(tool) {
                "continuing an already-approved daemon command session".to_string()
            } else {
                "read-only tool inside the session scope".to_string()
            },
            matched_rule: if is_command_follow_up_tool(tool) {
                format!("auto-command-follow-up:{tool}")
            } else {
                format!("auto-readonly:{tool}")
            },
            scope_kind: if is_command_follow_up_tool(tool) {
                "process"
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
    is_mutating_tool(tool) || matches!(tool, "command.run" | "command.session.open" | "tests.run")
}

fn is_mutating_tool(tool: &str) -> bool {
    matches!(
        tool,
        "fs.apply_patch" | "fs.write_text" | "fs.move" | "fs.mkdir" | "git.stage_patch"
    )
}

fn is_command_follow_up_tool(tool: &str) -> bool {
    matches!(tool, "command.session.write" | "command.session.close")
}

fn requires_write_lock(tool: &str) -> bool {
    is_mutating_tool(tool) || matches!(tool, "command.run" | "command.session.open" | "tests.run")
}

fn lock_reason_for_tool(tool: &str, summary: &str) -> String {
    let detail = summary.trim();
    if detail.is_empty() {
        format!("daemon-owned {tool}")
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
    state
        .agent
        .terminate_job_command_sessions(
            job_id,
            "The job failed and closed any remaining daemon-owned command sessions.",
            "canceled",
        )
        .await;
    state.store.update_job(
        job_id,
        JobPatch {
            state: Some("failed".to_string()),
            last_error: Some(error.to_string()),
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
        summary: "Hidden worker job failed.".to_string(),
        detail: excerpt(error, 320),
        data_json: json!({ "error": error }),
    });
    publish_job_failed(state, &state.store.get_job(job_id)?.job).await;
    if let Some(parent_job_id) = detail.job.parent_job_id.as_deref() {
        publish_job_updated(state, &state.store.get_job(parent_job_id)?.job).await;
    }
    let _ = publish_overview_event(state).await;
    Ok(())
}

async fn resolve_hidden_worker_target(
    state: &AppState,
    session: &SessionSummary,
) -> Result<HiddenWorkerTarget, ApiError> {
    let workspace = state.store.workspace()?;
    let profile = resolve_hidden_worker_profile(&workspace, session);

    if let Some(profile) = profile {
        ensure_prompting_runtime(state, &profile.utility.adapter, false).await?;
        return Ok(HiddenWorkerTarget {
            provider: profile.utility.adapter.clone(),
            model: profile.utility.model.clone(),
            provider_base_url: profile.utility.base_url.clone(),
            provider_api_key: profile.utility.api_key.clone(),
        });
    }

    ensure_prompting_runtime(state, &session.provider, false).await?;
    Ok(HiddenWorkerTarget {
        provider: session.provider.clone(),
        model: session.model.clone(),
        provider_base_url: session.provider_base_url.clone(),
        provider_api_key: session.provider_api_key.clone(),
    })
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
    let target = resolve_workspace_profile_target(state, profile).await?;
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
    let target = resolve_workspace_profile_target(state, profile).await?;
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
    let active = state
        .store
        .list_jobs_for_template_by_state(playbook_id, &["queued", "running", "paused"])?;
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
            .list_jobs_for_template_by_state(&playbook.id, &["queued", "running", "paused"])?
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
            .list_jobs_for_template_by_state(&playbook.id, &["queued", "running", "paused"])?
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
    if session.projects.is_empty() {
        return vec![session.working_dir.clone()];
    }

    session
        .projects
        .iter()
        .map(|project| project.absolute_path.clone())
        .collect()
}

fn worker_write_roots(session: &SessionSummary) -> Vec<String> {
    worker_read_roots(session)
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
            capabilities
        }
        _ => {
            let mut capabilities = read_only_capabilities();
            capabilities.extend(mutating_capabilities());
            capabilities.extend(execution_capabilities());
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
    let target = resolve_hidden_worker_target(state, &playbook.session).await?;

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

    let job = state.store.create_job(JobRecord {
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
    })?;

    let _created_worker = state.store.create_worker(WorkerRecord {
        id: root_worker_id.clone(),
        job_id: job_id.clone(),
        parent_worker_id: None,
        title: "Hidden automation worker".to_string(),
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
        max_steps: JOB_MAX_STEPS,
        max_tool_calls: JOB_MAX_TOOL_CALLS,
        max_wall_clock_secs: JOB_MAX_WALL_CLOCK_SECS,
    })?;
    state.store.update_job(
        &job_id,
        JobPatch {
            root_worker_id: Some(root_worker_id.clone()),
            ..JobPatch::default()
        },
    )?;
    state.store.replace_tool_capability_grants(
        &root_worker_id,
        &capabilities_for_policy_bundle(&playbook.playbook.policy_bundle),
    )?;
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
        conversation: vec![CheckpointMessage {
            role: "system".to_string(),
            content: worker_system_prompt(&worker),
        }],
        next_prompt: None,
        pending_action: None,
    };
    state
        .store
        .write_worker_checkpoint(&root_worker_id, &serde_json::to_value(checkpoint).unwrap())?;

    if let Ok(updated) = state.store.get_session(&session_id) {
        let _ = publish_session_event(state, updated).await;
    }
    publish_job_created(state, &job).await;
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
    ]
}

fn execution_capabilities() -> Vec<ToolCapabilityGrantRecord> {
    vec![
        ToolCapabilityGrantRecord {
            tool_id: "command.run".to_string(),
            summary: "Run a bounded daemon-owned command and capture logs as artifacts."
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
            summary: "Open a bounded interactive command session owned by the daemon.".to_string(),
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
            summary: "Send input to an approved daemon-owned command session.".to_string(),
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
            summary: "Close an approved daemon-owned command session.".to_string(),
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

fn worker_system_prompt(worker: &WorkerSummary) -> String {
    let is_root_worker = worker.parent_worker_id.is_none();
    let action_shapes = if is_root_worker {
        "{{\"kind\":\"tool_call\",\"summary\":\"why this tool is next\",\"tool\":\"tool.id\",\"args\":{{...}}}}\n\
{{\"kind\":\"spawn_child_jobs\",\"summary\":\"why parallel exploration helps\",\"jobs\":[{{\"title\":\"focused subtask\",\"prompt\":\"precise child prompt\",\"working_dir\":\"optional/path/inside/scope\"}}]}}\n\
{{\"kind\":\"final_answer\",\"summary\":\"why the work is done\",\"final_answer\":\"clean user-facing answer\"}}"
    } else {
        "{{\"kind\":\"tool_call\",\"summary\":\"why this tool is next\",\"tool\":\"tool.id\",\"args\":{{...}}}}\n\
{{\"kind\":\"final_answer\",\"summary\":\"why the work is done\",\"final_answer\":\"clean user-facing answer\"}}"
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
- Use at most 3 child jobs in a single spawn_child_jobs action.\n"
    } else {
        ""
    };

    format!(
        "You are the hidden Nucleus utility worker for a daemon-owned job.\n\
Return exactly one JSON object and nothing else.\n\
Allowed response shapes:\n\
{}\n\
Rules:\n\
- Prefer the smallest useful next step.\n\
- Use tools only when they materially improve the answer.\n\
- Never invent tool output.\n\
- Stay inside the granted repo scope.\n\
{}\
- The visible chat will only receive final_answer, not your intermediate reasoning.\n\
- Do not wrap JSON in markdown fences.\n\
Available tools:\n{}\n\
Worker lane: {}\n\
Working directory: {}\n",
        action_shapes, child_job_rules, tool_help, worker.lane, worker.working_dir
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
    let _ = state.events.send(DaemonEvent::JobCreated(summary.clone()));
}

async fn publish_job_updated(state: &AppState, summary: &JobSummary) {
    let _ = state.events.send(DaemonEvent::JobUpdated(summary.clone()));
}

async fn publish_job_failed(state: &AppState, summary: &JobSummary) {
    let _ = state.events.send(DaemonEvent::JobFailed(summary.clone()));
}

async fn publish_job_completed(state: &AppState, summary: &JobSummary) {
    let _ = state
        .events
        .send(DaemonEvent::JobCompleted(summary.clone()));
}

async fn publish_worker_updated(state: &AppState, summary: &WorkerSummary) {
    let _ = state
        .events
        .send(DaemonEvent::WorkerUpdated(summary.clone()));
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
            created_at: unix_timestamp(),
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        host::HostEngine,
        runtime::RuntimeManager,
        updates::{InstanceRuntime, UpdateManager},
    };
    use nucleus_storage::{JobRecord, StateStore, ToolCallRecord, WorkerRecord};
    use std::{
        env, fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
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
            last_error: String::new(),
            capabilities: Vec::new(),
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
            last_error: String::new(),
            capabilities: Vec::new(),
            created_at: 0,
            updated_at: 0,
        };
        let conversation = vec![
            CheckpointMessage {
                role: "system".to_string(),
                content: "Return exactly one JSON object and nothing else.".to_string(),
            },
            CheckpointMessage {
                role: "assistant".to_string(),
                content: "{\"kind\":\"tool_call\"}".to_string(),
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
            last_error: String::new(),
            capabilities: Vec::new(),
            created_at: 0,
            updated_at: 0,
        };
        let conversation = vec![CheckpointMessage {
            role: "system".to_string(),
            content: "Return exactly one JSON object and nothing else.".to_string(),
        }];

        let prompt = build_worker_prompt_input(&worker, &conversation, "You there?");

        assert_eq!(prompt, "You there?");
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
            )
            .expect("workspace root should update");

        let (events, _) = broadcast::channel(8);
        AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store)),
            agent: Arc::new(AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        }
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
