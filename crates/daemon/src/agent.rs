use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use nucleus_protocol::{
    ApprovalRequestSummary, ArtifactSummary, DaemonEvent, JobDetail, JobSummary,
    PromptProgressUpdate, SessionDetail, SessionPromptRequest, SessionSummary, SessionTurn,
    SessionTurnImage, WorkerSummary, WorkspaceProfileSummary, WorkspaceSummary,
};
use nucleus_storage::{
    ApprovalRequestRecord, AuditEventRecord, JobArtifactRecord, JobEventRecord, JobPatch,
    JobRecord, PolicyDecisionRecord, SessionPatch, ToolCallPatch, ToolCallRecord,
    ToolCapabilityGrantRecord, WorkerPatch, WorkerRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    process::Command,
    sync::{Mutex, mpsc, watch},
};
use tracing::warn;
use uuid::Uuid;

use super::{
    ApiError, AppState, assemble_prompt_input, ensure_prompting_runtime, excerpt,
    publish_overview_event, publish_prompt_progress_event, publish_session_event,
    try_record_audit_event, unix_timestamp,
};
use crate::runtime::PromptStreamEvent;

const JOB_MAX_STEPS: usize = 10;
const JOB_MAX_TOOL_CALLS: usize = 20;
const JOB_MAX_WALL_CLOCK_SECS: u64 = 300;
const TOOL_OUTPUT_CHAR_LIMIT: usize = 8_000;
const READ_FILE_CHAR_LIMIT: usize = 12_000;
const LIST_LIMIT: usize = 120;
const RG_LIMIT: usize = 80;
const DIFF_PREVIEW_CHAR_LIMIT: usize = 12_000;

#[derive(Default)]
pub struct AgentRuntime {
    running_jobs: Mutex<BTreeSet<String>>,
    cancel_tokens: Mutex<BTreeMap<String, watch::Sender<bool>>>,
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
    tool_call_id: String,
    approval_id: Option<String>,
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
    FinalAnswer {
        summary: String,
        final_answer: String,
    },
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

#[derive(Debug, Clone)]
struct MutationPreview {
    detail: String,
    diff_preview: String,
    artifact: Option<ArtifactDraft>,
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

pub async fn cancel_job(state: AppState, job_id: String) -> Result<JobDetail, ApiError> {
    let detail = state.store.get_job(&job_id)?;
    match detail.job.state.as_str() {
        "completed" | "failed" | "canceled" => {
            return Ok(detail);
        }
        _ => {}
    }

    if let Some(sender) = state.agent.cancel_tokens.lock().await.get(&job_id).cloned() {
        let _ = sender.send(true);
    }

    state.store.update_job(
        &job_id,
        JobPatch {
            state: Some("canceled".to_string()),
            last_error: Some(String::new()),
            ..JobPatch::default()
        },
    )?;
    for worker in detail.workers {
        let _ = state.store.update_worker(
            &worker.id,
            WorkerPatch {
                state: Some("canceled".to_string()),
                ..WorkerPatch::default()
            },
        );
    }
    for approval in detail.approvals {
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
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.clone(),
        worker_id: None,
        event_type: "job.canceled".to_string(),
        status: "canceled".to_string(),
        summary: "Canceled hidden worker job.".to_string(),
        detail: "The daemon stopped the job before it finished.".to_string(),
        data_json: json!({}),
    });
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

    state.store.update_job(
        &job_id,
        JobPatch {
            state: Some("queued".to_string()),
            ..JobPatch::default()
        },
    )?;
    for worker in detail.workers {
        let _ = state.store.update_worker(
            &worker.id,
            WorkerPatch {
                state: Some("queued".to_string()),
                ..WorkerPatch::default()
            },
        );
    }
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
    spawn_job_task(state.clone(), job_id.clone());
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
            WorkerAction::FinalAnswer {
                summary,
                final_answer,
            } => {
                let final_turn_id = Uuid::new_v4().to_string();
                state.store.append_session_turn(
                    &session_id,
                    &final_turn_id,
                    "assistant",
                    &final_answer,
                    &[],
                )?;
                state.store.update_job(
                    job_id,
                    JobPatch {
                        state: Some("completed".to_string()),
                        visible_turn_id: Some(final_turn_id),
                        result_summary: Some(summary.clone()),
                        last_error: Some(String::new()),
                        ..JobPatch::default()
                    },
                )?;
                worker = state.store.update_worker(
                    &worker.id,
                    WorkerPatch {
                        state: Some("completed".to_string()),
                        step_count: Some(step + 1),
                        tool_call_count: Some(tool_calls),
                        last_error: Some(String::new()),
                        ..WorkerPatch::default()
                    },
                )?;
                state.store.update_session(
                    &session_id,
                    SessionPatch {
                        state: Some("active".to_string()),
                        last_error: Some(String::new()),
                        ..SessionPatch::default()
                    },
                )?;
                let _ = state.store.append_job_event(JobEventRecord {
                    job_id: job_id.to_string(),
                    worker_id: Some(worker.id.clone()),
                    event_type: "job.completed".to_string(),
                    status: "completed".to_string(),
                    summary: summary.clone(),
                    detail: excerpt(&final_answer, 320),
                    data_json: json!({ "step_count": step + 1, "tool_call_count": tool_calls }),
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
                            session_id,
                            worker.provider,
                            worker.model,
                            step + 1,
                            tool_calls
                        ),
                    },
                )
                .await;
                if let Ok(updated) = state.store.get_session(&session_id) {
                    let _ = publish_session_event(state, updated).await;
                }
                publish_job_completed(state, &state.store.get_job(job_id)?.job).await;
                publish_worker_updated(state, &worker).await;
                publish_prompt_status(
                    state,
                    &session.session,
                    &worker,
                    "completed",
                    "Hidden worker completed",
                    "The daemon persisted a clean assistant turn from the worker result.",
                )
                .await;
                let _ = publish_overview_event(state).await;
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
) -> Result<LoopDisposition> {
    let Some(pending) = checkpoint.pending_action.clone() else {
        return Ok(LoopDisposition::Continue);
    };

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

    execute_pending_tool_action(
        state, session, job_id, worker, checkpoint, step, tool_calls, pending,
    )
    .await?;
    Ok(LoopDisposition::Continue)
}

async fn handle_tool_call_proposal(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: &mut usize,
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
            "running".to_string()
        },
        summary: summary.clone(),
        args_json: args.clone(),
        result_json: None,
        policy_decision: Some(policy.clone()),
        artifact_ids: Vec::new(),
        error_class: String::new(),
        error_detail: String::new(),
        started_at: Some(unix_timestamp()),
        completed_at: None,
    })?;

    if requires_approval {
        let preview = preview_mutating_tool(state, worker, &tool, &args)?;
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
            tool_call_id: tool_call_id.clone(),
            approval_id: Some(approval.id.clone()),
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

    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "tool.started".to_string(),
        status: "running".to_string(),
        summary: format!("Running {}", tool),
        detail: summary.clone(),
        data_json: json!({ "tool_id": tool, "args": args }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_prompt_status(
        state,
        &session.session,
        worker,
        "tooling",
        &format!("Running {}", tool),
        &summary,
    )
    .await;

    let pending = PendingToolAction {
        tool_call_id,
        approval_id: None,
        summary,
        tool,
        args,
    };
    execute_pending_tool_action(
        state, session, job_id, worker, checkpoint, step, tool_calls, pending,
    )
    .await?;
    Ok(LoopDisposition::Continue)
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
    let prompt_body = prompt.to_string();
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

async fn execute_pending_tool_action(
    state: &AppState,
    session: &SessionDetail,
    job_id: &str,
    worker: &mut WorkerSummary,
    checkpoint: &mut WorkerCheckpoint,
    step: &mut usize,
    tool_calls: &mut usize,
    pending: PendingToolAction,
) -> Result<Value> {
    let tool = pending.tool.clone();
    let args = pending.args.clone();
    let _ = state.store.append_job_event(JobEventRecord {
        job_id: job_id.to_string(),
        worker_id: Some(worker.id.clone()),
        event_type: "tool.started".to_string(),
        status: "running".to_string(),
        summary: format!("Running {}", tool),
        detail: pending.summary.clone(),
        data_json: json!({ "tool_id": tool, "tool_call_id": pending.tool_call_id, "args": args }),
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
    state.store.update_tool_call(
        &pending.tool_call_id,
        ToolCallPatch {
            status: Some("running".to_string()),
            error_class: Some(String::new()),
            error_detail: Some(String::new()),
            ..ToolCallPatch::default()
        },
    )?;

    let tool_result = match execute_granted_tool(state, session, worker, &tool, args).await {
        Ok(result) => result,
        Err(error) => {
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
        data_json: json!({ "tool_id": tool, "tool_call_id": pending.tool_call_id }),
    });
    publish_job_updated(state, &state.store.get_job(job_id)?.job).await;
    publish_worker_updated(state, worker).await;
    Ok(tool_result)
}

async fn execute_granted_tool(
    _state: &AppState,
    session: &SessionDetail,
    worker: &WorkerSummary,
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
        other => bail!("unsupported tool '{}'", other),
    }
}

fn preview_mutating_tool(
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
        other => bail!("'{}' is not a mutating tool", other),
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
    let output = Command::new(command)
        .args(args)
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
    if is_mutating_tool(tool) {
        PolicyDecisionRecord {
            decision: "require_approval".to_string(),
            reason: "repo mutations require explicit operator approval".to_string(),
            matched_rule: format!("approval:mutation:{tool}"),
            scope_kind: "path".to_string(),
            risk_level: "medium".to_string(),
        }
    } else {
        PolicyDecisionRecord {
            decision: "allow".to_string(),
            reason: "read-only tool inside the session scope".to_string(),
            matched_rule: format!("auto-readonly:{tool}"),
            scope_kind: "path".to_string(),
            risk_level: "low".to_string(),
        }
    }
}

fn is_mutating_tool(tool: &str) -> bool {
    matches!(
        tool,
        "fs.apply_patch" | "fs.write_text" | "fs.move" | "fs.mkdir" | "git.stage_patch"
    )
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
    let mut capabilities = read_only_capabilities();
    capabilities.extend(mutating_capabilities());
    capabilities
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

fn worker_system_prompt(worker: &WorkerSummary) -> String {
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

    format!(
        "You are the hidden Nucleus utility worker for a daemon-owned job.\n\
Return exactly one JSON object and nothing else.\n\
Allowed response shapes:\n\
{{\"kind\":\"tool_call\",\"summary\":\"why this tool is next\",\"tool\":\"tool.id\",\"args\":{{...}}}}\n\
{{\"kind\":\"final_answer\",\"summary\":\"why the work is done\",\"final_answer\":\"clean user-facing answer\"}}\n\
Rules:\n\
- Prefer the smallest useful next step.\n\
- Use tools only when they materially improve the answer.\n\
- Never invent tool output.\n\
- Stay inside the granted repo scope.\n\
- The visible chat will only receive final_answer, not your intermediate reasoning.\n\
- Do not wrap JSON in markdown fences.\n\
Available tools:\n{}\n\
Worker lane: {}\n\
Working directory: {}\n",
        tool_help, worker.lane, worker.working_dir
    )
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
        assert_eq!(policy_for_tool("fs.read_text").decision, "allow");
    }
}
