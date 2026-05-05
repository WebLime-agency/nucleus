mod host;
mod runtime;
mod updates;

use std::{
    collections::BTreeSet,
    env, fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use futures_util::SinkExt;
use host::{DEFAULT_PROCESS_LIMIT, HostEngine, ProcessSort, resolve_process_limit};
use nucleus_core::{AdapterKind, DEFAULT_DAEMON_ADDR, PRODUCT_NAME, product_banner};
use nucleus_protocol::{
    ActionParameter, ActionRunRequest, ActionRunResponse, ActionSummary, AuditEvent,
    CreateSessionRequest, DaemonEvent, HealthResponse, HostStatus, ProcessKillRequest,
    ProcessKillResponse, ProcessListResponse, ProcessStreamUpdate, ProjectUpdateRequest,
    PromptProgressUpdate, RouterProfileSummary, RuntimeOverview, RuntimeSummary, SessionDetail,
    SessionPromptRequest, SessionSummary, SettingsSummary, StreamConnected, SystemStats,
    UpdateSessionRequest, UpdateStatus, WorkspaceSummary, WorkspaceUpdateRequest,
};
use nucleus_storage::{AuditEventRecord, ProjectPatch, SessionPatch, SessionRecord, StateStore};
use runtime::{PromptStreamEvent, RuntimeManager};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{
    sync::{broadcast, mpsc},
    time::{self, MissedTickBehavior},
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use updates::{InstanceRuntime, UpdateManager};
use uuid::Uuid;

const STREAM_PROCESS_LIMIT: usize = DEFAULT_PROCESS_LIMIT;
const STREAM_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_AUDIT_LIMIT: usize = 20;
const MAX_AUDIT_LIMIT: usize = 100;
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(90);
const MAX_PROMPT_INCLUDE_FILES: usize = 24;
const MAX_PROMPT_INCLUDE_FILE_CHARS: usize = 6_000;
const MAX_PROMPT_INCLUDE_TOTAL_CHARS: usize = 24_000;
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(900);
const INITIAL_UPDATE_CHECK_DELAY: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct AppState {
    version: String,
    store: Arc<StateStore>,
    host: Arc<HostEngine>,
    runtimes: Arc<RuntimeManager>,
    updates: Arc<UpdateManager>,
    events: broadcast::Sender<DaemonEvent>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind = env::var("NUCLEUS_BIND").unwrap_or_else(|_| DEFAULT_DAEMON_ADDR.to_string());
    let instance = InstanceRuntime::detect(bind.clone());
    let updates = Arc::new(UpdateManager::new(instance.clone()));
    let (events, _) = broadcast::channel(32);
    let state = AppState {
        version: env!("CARGO_PKG_VERSION").to_string(),
        store: Arc::new(StateStore::initialize().context("failed to initialize state store")?),
        host: Arc::new(HostEngine::new()),
        runtimes: Arc::new(RuntimeManager::default()),
        updates,
        events,
    };
    spawn_event_publisher(state.clone());
    spawn_update_monitor(state.clone());

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind daemon listener on {bind}"))?;

    info!(bind = %bind, banner = %product_banner(), "starting daemon");

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("daemon server exited unexpectedly")?;

    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api/overview", get(overview))
        .route("/api/runtimes", get(runtimes))
        .route("/api/settings", get(settings))
        .route(
            "/api/settings/update/check",
            axum::routing::post(check_for_updates),
        )
        .route(
            "/api/settings/update/apply",
            axum::routing::post(apply_update),
        )
        .route("/api/workspace", get(workspace).patch(update_workspace))
        .route(
            "/api/workspace/projects/sync",
            axum::routing::post(sync_projects),
        )
        .route(
            "/api/workspace/projects/{project_id}",
            axum::routing::patch(update_project),
        )
        .route("/api/router/profiles", get(router_profiles))
        .route("/api/actions", get(actions))
        .route("/api/actions/{action_id}", get(action_detail))
        .route(
            "/api/actions/{action_id}/run",
            axum::routing::post(run_action),
        )
        .route("/api/audit", get(audit_events))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{session_id}",
            get(session_detail)
                .patch(update_session)
                .delete(delete_session),
        )
        .route(
            "/api/sessions/{session_id}/prompt",
            get(session_detail).post(prompt_session),
        )
        .route("/api/host-status", get(host_status))
        .route("/api/system", get(system_stats))
        .route("/api/system/processes", get(processes).post(kill_process))
        .route("/ws", get(stream_socket))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn root() -> &'static str {
    "Nucleus daemon"
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse::ok(PRODUCT_NAME, state.version))
}

async fn overview(State(state): State<AppState>) -> Result<Json<RuntimeOverview>, ApiError> {
    Ok(Json(
        build_runtime_overview(&state, state.host.host_status(), false).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct RuntimeQuery {
    refresh: Option<bool>,
}

async fn runtimes(
    State(state): State<AppState>,
    Query(query): Query<RuntimeQuery>,
) -> Result<Json<Vec<RuntimeSummary>>, ApiError> {
    let force_refresh = query.refresh.unwrap_or(false);
    Ok(Json(load_runtimes(&state, force_refresh).await?))
}

async fn settings(State(state): State<AppState>) -> Result<Json<SettingsSummary>, ApiError> {
    Ok(Json(build_settings_summary(&state).await))
}

async fn check_for_updates(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    let result = state.updates.check().await;
    if result.changed {
        let _ = publish_update_event(&state, result.status.clone()).await;
    }
    Ok(Json(result.status))
}

async fn apply_update(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    let result = state.updates.apply().await;
    if result.changed {
        let _ = publish_update_event(&state, result.status.clone()).await;
    }
    Ok(Json(result.status))
}

async fn workspace(State(state): State<AppState>) -> Result<Json<WorkspaceSummary>, ApiError> {
    Ok(Json(state.store.workspace()?))
}

async fn update_workspace(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<WorkspaceSummary>, ApiError> {
    let payload = decode_json::<WorkspaceUpdateRequest>(&body)?;
    let root_path = match payload.root_path.as_deref() {
        Some(root_path) => Some(sanitize_workspace_root(root_path)?),
        None => None,
    };
    let route_profiles = load_router_profiles(&state, false).await?;
    let main_target = match payload.main_target.as_deref() {
        Some(target) => Some(sanitize_workspace_target(&state, &route_profiles, target).await?),
        None => None,
    };
    let utility_target = match payload.utility_target.as_deref() {
        Some(target) => Some(sanitize_workspace_target(&state, &route_profiles, target).await?),
        None => None,
    };
    let workspace = state.store.update_workspace(
        root_path.as_deref(),
        main_target.as_deref(),
        utility_target.as_deref(),
    )?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "workspace.updated".to_string(),
            target: "workspace:root".to_string(),
            status: "success".to_string(),
            summary: "Updated workspace settings.".to_string(),
            detail: format!(
                "root_path={} main_target={} utility_target={}",
                root_path.unwrap_or_else(|| workspace.root_path.clone()),
                main_target.unwrap_or_else(|| workspace.main_target.clone()),
                utility_target.unwrap_or_else(|| workspace.utility_target.clone())
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(workspace))
}

async fn sync_projects(State(state): State<AppState>) -> Result<Json<WorkspaceSummary>, ApiError> {
    let workspace = state.store.sync_projects()?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "workspace.synced".to_string(),
            target: "workspace:projects".to_string(),
            status: "success".to_string(),
            summary: "Synced projects from the workspace root.".to_string(),
            detail: format!("project_count={}", workspace.projects.len()),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(workspace))
}

async fn update_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    body: Bytes,
) -> Result<Json<WorkspaceSummary>, ApiError> {
    let payload = decode_json::<ProjectUpdateRequest>(&body)?;
    let project = state.store.update_project(
        &project_id,
        ProjectPatch {
            title: payload.title.map(|value| value.trim().to_string()),
        },
    )?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "project.updated".to_string(),
            target: format!("project:{project_id}"),
            status: "success".to_string(),
            summary: format!("Updated project '{}'.", project.title),
            detail: format!("relative_path={}", project.relative_path),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(state.store.workspace()?))
}

async fn router_profiles(
    State(state): State<AppState>,
) -> Result<Json<Vec<RouterProfileSummary>>, ApiError> {
    Ok(Json(load_router_profiles(&state, false).await?))
}

async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    Ok(Json(state.store.list_sessions()?))
}

async fn actions() -> Json<Vec<ActionSummary>> {
    Json(action_catalog())
}

async fn action_detail(Path(action_id): Path<String>) -> Result<Json<ActionSummary>, ApiError> {
    let action = action_catalog()
        .into_iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| ApiError::not_found(format!("action '{}' was not found", action_id)))?;
    Ok(Json(action))
}

async fn create_session(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SessionDetail>, ApiError> {
    let payload = decode_json::<CreateSessionRequest>(&body)?;
    let session_id = Uuid::new_v4().to_string();
    let projects = resolve_session_projects(
        &state,
        payload.project_id.as_deref(),
        payload.primary_project_id.as_deref(),
        payload.project_ids.as_deref(),
        Some(&session_id),
        None,
    )?;
    let workspace = state.store.workspace()?;
    let default_target = parse_target_selector(&workspace.main_target);
    let route_profiles = load_router_profiles(&state, false).await?;
    let requested_route_id = payload
        .route_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let requested_provider = payload
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let selection = resolve_session_target(
        &state,
        &route_profiles,
        requested_route_id.or_else(|| {
            if requested_provider.is_none() {
                default_target.route_id.as_deref()
            } else {
                None
            }
        }),
        requested_provider.or(default_target.provider.as_deref()),
        payload.model.as_deref(),
    )
    .await?;
    let provider = resolve_provider(&selection.provider)?;
    let title = payload
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if !selection.route_title.is_empty() {
                format!("{} session", selection.route_title)
            } else {
                default_session_title(provider)
            }
        });
    let route_title = selection.route_title.clone();

    state.store.create_session(SessionRecord {
        id: session_id.clone(),
        route_id: selection.route_id,
        route_title,
        scope: projects.scope.clone(),
        project_id: projects.primary_project_id.clone(),
        project_title: projects.primary_project_title.clone(),
        project_path: projects.primary_project_path.clone(),
        project_ids: projects.project_ids.clone(),
        title,
        provider: selection.provider,
        model: selection.model,
        working_dir: projects.working_dir.clone(),
        working_dir_kind: projects.working_dir_kind.clone(),
    })?;

    let detail = state.store.get_session(&session_id)?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "session.created".to_string(),
            target: format!("session:{session_id}"),
            status: "success".to_string(),
            summary: format!(
                "Created {} session '{}'.",
                detail.session.provider, detail.session.title
            ),
            detail: format!(
                "provider={} model={} working_dir={} scope={} project_count={}",
                detail.session.provider,
                detail.session.model,
                detail.session.working_dir,
                detail.session.scope,
                detail.session.project_count
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(detail))
}

async fn session_detail(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionDetail>, ApiError> {
    Ok(Json(state.store.get_session(&session_id)?))
}

async fn update_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<SessionDetail>, ApiError> {
    let payload = decode_json::<UpdateSessionRequest>(&body)?;
    let before = state.store.get_session(&session_id)?;
    let project_selection = if payload.project_ids.is_some()
        || payload.primary_project_id.is_some()
        || payload.project_id.is_some()
    {
        Some(resolve_session_projects(
            &state,
            payload.project_id.as_deref(),
            payload.primary_project_id.as_deref(),
            payload.project_ids.as_deref(),
            Some(&session_id),
            Some(&before.session),
        )?)
    } else {
        None
    };
    let route_id = payload.route_id.as_deref().map(str::trim);
    let provider = payload.provider.as_deref().map(str::trim);
    let next_target = if route_id.is_some_and(|value| !value.is_empty())
        || provider.is_some_and(|value| !value.is_empty())
    {
        let profiles = load_router_profiles(&state, false).await?;
        Some(
            resolve_session_target(
                &state,
                &profiles,
                route_id,
                provider,
                payload
                    .model
                    .as_deref()
                    .or(Some(before.session.model.as_str())),
            )
            .await?,
        )
    } else {
        None
    };
    let reset_provider_session = project_selection.is_some() || next_target.is_some();
    let patch = SessionPatch {
        title: payload.title.map(|value| value.trim().to_string()),
        route_id: next_target
            .as_ref()
            .map(|selection| selection.route_id.clone()),
        route_title: next_target
            .as_ref()
            .map(|selection| selection.route_title.clone()),
        scope: project_selection
            .as_ref()
            .map(|selection| selection.scope.clone()),
        project_id: project_selection
            .as_ref()
            .map(|selection| selection.primary_project_id.clone()),
        project_title: project_selection
            .as_ref()
            .map(|selection| selection.primary_project_title.clone()),
        project_path: project_selection
            .as_ref()
            .map(|selection| selection.primary_project_path.clone()),
        project_ids: project_selection
            .as_ref()
            .map(|selection| selection.project_ids.clone()),
        provider: next_target
            .as_ref()
            .map(|selection| selection.provider.clone()),
        model: match next_target {
            Some(ref selection) => Some(selection.model.clone()),
            None => payload.model.map(|value| value.trim().to_string()),
        },
        working_dir: project_selection
            .as_ref()
            .map(|selection| selection.working_dir.clone()),
        working_dir_kind: project_selection
            .as_ref()
            .map(|selection| selection.working_dir_kind.clone()),
        state: match payload.state {
            Some(value) => Some(normalize_session_state(&value)?),
            None => None,
        },
        provider_session_id: if reset_provider_session {
            Some(String::new())
        } else {
            None
        },
        last_error: None,
    };

    state.store.update_session(&session_id, patch)?;
    let detail = state.store.get_session(&session_id)?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "session.updated".to_string(),
            target: format!("session:{session_id}"),
            status: "success".to_string(),
            summary: describe_session_update(&before.session, &detail.session),
            detail: format!(
                "provider={} model={} working_dir={} state={} scope={} project_count={}",
                detail.session.provider,
                detail.session.model,
                detail.session.working_dir,
                detail.session.state,
                detail.session.scope,
                detail.session.project_count
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(detail))
}

async fn delete_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let current = state.store.get_session(&session_id)?;
    state.store.delete_session(&session_id)?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "session.deleted".to_string(),
            target: format!("session:{session_id}"),
            status: "success".to_string(),
            summary: format!(
                "Deleted {} session '{}'.",
                current.session.provider, current.session.title
            ),
            detail: format!(
                "provider={} model={} working_dir={}",
                current.session.provider, current.session.model, current.session.working_dir
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn prompt_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<SessionDetail>, ApiError> {
    let payload = decode_json::<SessionPromptRequest>(&body)?;
    let prompt = payload.prompt.trim();
    let execution_prompt = effective_prompt_text(prompt, payload.images.len());

    if prompt.is_empty() && payload.images.is_empty() {
        return Err(ApiError::bad_request(
            "prompt cannot be empty unless images are attached",
        ));
    }

    let current = state.store.get_session(&session_id)?;

    if current.session.state == "archived" {
        return Err(ApiError::bad_request(
            "archived sessions cannot accept new prompts",
        ));
    }

    if current.session.state == "running" {
        return Err(ApiError::bad_request(
            "this session is already processing a prompt",
        ));
    }

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
        prompt,
        &payload.images,
    )?;
    let started = state.store.get_session(&session_id)?;
    let _ = publish_session_event(&state, started).await;
    let _ = publish_prompt_progress_event(
        &state,
        PromptProgressUpdate {
            session_id: session_id.clone(),
            status: "queued".to_string(),
            label: "Queued".to_string(),
            detail: if payload.images.is_empty() {
                "Prompt accepted by the daemon.".to_string()
            } else {
                format!(
                    "Prompt accepted with {} image attachment(s).",
                    payload.images.len()
                )
            },
            provider: current.session.provider.clone(),
            model: current.session.model.clone(),
            route_id: current.session.route_id.clone(),
            route_title: current.session.route_title.clone(),
            attempt: 0,
            attempt_count: 0,
            created_at: unix_timestamp(),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;

    spawn_prompt_job(
        state.clone(),
        session_id.clone(),
        payload,
        current,
        execution_prompt,
    );

    Ok(Json(state.store.get_session(&session_id)?))
}

fn spawn_prompt_job(
    state: AppState,
    session_id: String,
    payload: SessionPromptRequest,
    current: SessionDetail,
    execution_prompt: String,
) {
    let prompt_excerpt = if payload.prompt.trim().is_empty() {
        format!("[{} image attachment(s)]", payload.images.len())
    } else {
        payload.prompt.trim().to_string()
    };

    tokio::spawn(async move {
        if let Err(error) = process_prompt_job(
            state.clone(),
            &session_id,
            &payload,
            &current,
            &execution_prompt,
        )
        .await
        {
            warn!(session_id = %session_id, error = %error, "prompt job failed");
            finalize_prompt_job_failure(
                &state,
                &session_id,
                &current.session,
                &prompt_excerpt,
                &error,
            )
            .await;
        }
    });
}

async fn process_prompt_job(
    state: AppState,
    session_id: &str,
    payload: &SessionPromptRequest,
    current: &SessionDetail,
    execution_prompt: &str,
) -> Result<(), String> {
    let prompt_assembly = assemble_prompt_input(&state, &current.session, execution_prompt)
        .map_err(|error| error.message)?;
    let _ = publish_prompt_progress_event(
        &state,
        PromptProgressUpdate {
            session_id: session_id.to_string(),
            status: "assembling".to_string(),
            label: "Built prompt context".to_string(),
            detail: prompt_assembly.detail.clone(),
            provider: current.session.provider.clone(),
            model: current.session.model.clone(),
            route_id: current.session.route_id.clone(),
            route_title: current.session.route_title.clone(),
            attempt: 0,
            attempt_count: 0,
            created_at: unix_timestamp(),
        },
    )
    .await;

    let targets = resolve_prompt_targets(&state, &current.session, !payload.images.is_empty())
        .await
        .map_err(|error| error.message)?;
    let target_count = targets.len();
    let _ = publish_prompt_progress_event(
        &state,
        PromptProgressUpdate {
            session_id: session_id.to_string(),
            status: "routing".to_string(),
            label: if current.session.route_id.is_empty() {
                "Prepared provider".to_string()
            } else {
                "Resolved route targets".to_string()
            },
            detail: if current.session.route_id.is_empty() {
                format!(
                    "Using {} / {} directly.",
                    current.session.provider, current.session.model
                )
            } else {
                format!(
                    "{} target(s) available on route '{}'.",
                    target_count, current.session.route_title
                )
            },
            provider: current.session.provider.clone(),
            model: current.session.model.clone(),
            route_id: current.session.route_id.clone(),
            route_title: current.session.route_title.clone(),
            attempt: 0,
            attempt_count: target_count,
            created_at: unix_timestamp(),
        },
    )
    .await;

    let mut stream_state = PromptStreamState::default();
    let mut last_error = None;

    for (index, target) in targets.into_iter().enumerate() {
        let attempt = index + 1;
        let execution = build_prompt_execution_session(&current.session, &target);
        let prompt_body = if target.provider == current.session.provider {
            prompt_assembly.prompt.clone()
        } else {
            build_reroute_prompt(
                &current.turns,
                &prompt_assembly.prompt,
                payload.images.len(),
            )
        };
        let _ = publish_prompt_progress_event(
            &state,
            PromptProgressUpdate {
                session_id: session_id.to_string(),
                status: "calling".to_string(),
                label: format!("Calling {}", target.provider),
                detail: format!(
                    "Attempt {} of {} on {} / {}.",
                    attempt, target_count, target.provider, target.model
                ),
                provider: target.provider.clone(),
                model: target.model.clone(),
                route_id: target.route_id.clone(),
                route_title: target.route_title.clone(),
                attempt,
                attempt_count: target_count,
                created_at: unix_timestamp(),
            },
        )
        .await;

        match run_prompt_attempt(
            &state,
            session_id,
            &execution,
            &prompt_body,
            &payload.images,
            &target,
            attempt,
            target_count,
            &mut stream_state,
        )
        .await
        {
            Ok(result) => {
                let provider_session_id = if result.provider_session_id.is_empty() {
                    stream_state.provider_session_id.clone()
                } else {
                    result.provider_session_id.clone()
                };
                state
                    .store
                    .update_session(
                        session_id,
                        SessionPatch {
                            provider: Some(target.provider.clone()),
                            state: Some("active".to_string()),
                            route_id: Some(target.route_id.clone()),
                            route_title: Some(target.route_title.clone()),
                            provider_session_id: Some(provider_session_id),
                            model: Some(target.model.clone()),
                            last_error: Some(String::new()),
                            ..SessionPatch::default()
                        },
                    )
                    .map_err(|error| error.to_string())?;
                if current.session.provider != target.provider {
                    let _ = try_record_audit_event(
                        &state,
                        AuditEventRecord {
                            kind: "session.rerouted".to_string(),
                            target: format!("session:{session_id}"),
                            status: "success".to_string(),
                            summary: format!(
                                "Rerouted session '{}' from {} to {}.",
                                current.session.title, current.session.provider, target.provider
                            ),
                            detail: format!("route_id={} model={}", target.route_id, target.model),
                        },
                    )
                    .await;
                }

                let detail = state
                    .store
                    .get_session(session_id)
                    .map_err(|error| error.to_string())?;
                let _ = publish_session_event(&state, detail.clone()).await;
                let _ = publish_prompt_progress_event(
                    &state,
                    PromptProgressUpdate {
                        session_id: session_id.to_string(),
                        status: "completed".to_string(),
                        label: "Response received".to_string(),
                        detail: format!("{} returned a response.", target.provider),
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        route_id: target.route_id.clone(),
                        route_title: target.route_title.clone(),
                        attempt,
                        attempt_count: target_count,
                        created_at: unix_timestamp(),
                    },
                )
                .await;
                let _ = try_record_audit_event(
                    &state,
                    AuditEventRecord {
                        kind: "session.prompted".to_string(),
                        target: format!("session:{session_id}"),
                        status: "success".to_string(),
                        summary: format!(
                            "Prompted {} session '{}'.",
                            detail.session.provider, detail.session.title
                        ),
                        detail: format!(
                            "model={} prompt={} turns={}",
                            detail.session.model,
                            excerpt(&payload.prompt, 160),
                            detail.session.turn_count
                        ),
                    },
                )
                .await;
                let _ = publish_overview_event(&state).await;
                return Ok(());
            }
            Err(error) => {
                let _ = publish_prompt_progress_event(
                    &state,
                    PromptProgressUpdate {
                        session_id: session_id.to_string(),
                        status: if attempt < target_count {
                            "retrying".to_string()
                        } else {
                            "failed".to_string()
                        },
                        label: if attempt < target_count {
                            format!("Retrying after {}", target.provider)
                        } else {
                            format!("{} failed", target.provider)
                        },
                        detail: excerpt(&error, 200),
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        route_id: target.route_id.clone(),
                        route_title: target.route_title.clone(),
                        attempt,
                        attempt_count: target_count,
                        created_at: unix_timestamp(),
                    },
                )
                .await;
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "all prompt targets failed".to_string()))
}

async fn finalize_prompt_job_failure(
    state: &AppState,
    session_id: &str,
    session: &SessionSummary,
    prompt_excerpt: &str,
    error: &str,
) {
    let _ = state.store.update_session(
        session_id,
        SessionPatch {
            state: Some("error".to_string()),
            last_error: Some(error.to_string()),
            ..SessionPatch::default()
        },
    );
    let _ = publish_prompt_progress_event(
        state,
        PromptProgressUpdate {
            session_id: session_id.to_string(),
            status: "failed".to_string(),
            label: "Prompt failed".to_string(),
            detail: excerpt(error, 200),
            provider: session.provider.clone(),
            model: session.model.clone(),
            route_id: session.route_id.clone(),
            route_title: session.route_title.clone(),
            attempt: 0,
            attempt_count: 0,
            created_at: unix_timestamp(),
        },
    )
    .await;
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: "session.prompt.failed".to_string(),
            target: format!("session:{session_id}"),
            status: "error".to_string(),
            summary: format!(
                "Prompt failed for {} session '{}'.",
                session.provider, session.title
            ),
            detail: format!(
                "prompt={} error={}",
                excerpt(prompt_excerpt, 160),
                excerpt(error, 240)
            ),
        },
    )
    .await;

    if let Ok(detail) = state.store.get_session(session_id) {
        let _ = publish_session_event(state, detail).await;
    }
    let _ = publish_overview_event(state).await;
}

async fn run_prompt_attempt(
    state: &AppState,
    session_id: &str,
    execution: &SessionSummary,
    prompt_body: &str,
    images: &[nucleus_protocol::SessionTurnImage],
    target: &PromptTarget,
    attempt: usize,
    attempt_count: usize,
    stream_state: &mut PromptStreamState,
) -> Result<runtime::ProviderTurnResult, String> {
    stream_state.reset_for_attempt();

    let (events, mut receiver) = mpsc::unbounded_channel();
    let runtimes = state.runtimes.clone();
    let execution = execution.clone();
    let prompt_body = prompt_body.to_string();
    let images = images.to_vec();
    let handle = tokio::spawn(async move {
        runtimes
            .execute_prompt_stream(&execution, &prompt_body, &images, events)
            .await
    });

    while let Some(event) = receiver.recv().await {
        apply_prompt_stream_event(
            state,
            session_id,
            target,
            attempt,
            attempt_count,
            stream_state,
            event,
        )
        .await
        .map_err(|error| error.message)?;
    }

    let result = handle
        .await
        .map_err(|error| format!("prompt worker crashed: {error}"))?
        .map_err(|error| error.to_string())?;

    if !result.provider_session_id.is_empty() {
        stream_state.provider_session_id = result.provider_session_id.clone();
    }

    if stream_state.assistant_turn_id.is_none()
        || stream_state.last_persisted_content != result.content
    {
        stream_state.assistant_content = result.content.clone();
        persist_streaming_assistant(state, session_id, stream_state, true)
            .await
            .map_err(|error| error.message)?;
    }

    Ok(result)
}

async fn apply_prompt_stream_event(
    state: &AppState,
    session_id: &str,
    target: &PromptTarget,
    attempt: usize,
    attempt_count: usize,
    stream_state: &mut PromptStreamState,
    event: PromptStreamEvent,
) -> Result<(), ApiError> {
    match event {
        PromptStreamEvent::ProviderSessionReady {
            provider_session_id,
        } => {
            stream_state.provider_session_id = provider_session_id;
        }
        PromptStreamEvent::ReasoningSnapshot { text } => {
            let detail = excerpt(&text, 220);
            if detail != stream_state.last_reasoning_excerpt {
                stream_state.last_reasoning_excerpt = detail.clone();
                let _ = publish_prompt_progress_event(
                    state,
                    PromptProgressUpdate {
                        session_id: session_id.to_string(),
                        status: "thinking".to_string(),
                        label: format!("{} is thinking", target.provider),
                        detail,
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        route_id: target.route_id.clone(),
                        route_title: target.route_title.clone(),
                        attempt,
                        attempt_count,
                        created_at: unix_timestamp(),
                    },
                )
                .await;
            }
        }
        PromptStreamEvent::AssistantChunk { text } => {
            if !stream_state.streaming_announced {
                stream_state.streaming_announced = true;
                let _ = publish_prompt_progress_event(
                    state,
                    PromptProgressUpdate {
                        session_id: session_id.to_string(),
                        status: "streaming".to_string(),
                        label: format!("Streaming from {}", target.provider),
                        detail: format!(
                            "Receiving output from {} / {}.",
                            target.provider, target.model
                        ),
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        route_id: target.route_id.clone(),
                        route_title: target.route_title.clone(),
                        attempt,
                        attempt_count,
                        created_at: unix_timestamp(),
                    },
                )
                .await;
            }

            stream_state.assistant_content.push_str(&text);
            persist_streaming_assistant(state, session_id, stream_state, false).await?;
        }
        PromptStreamEvent::AssistantSnapshot { text } => {
            if !stream_state.streaming_announced {
                stream_state.streaming_announced = true;
                let _ = publish_prompt_progress_event(
                    state,
                    PromptProgressUpdate {
                        session_id: session_id.to_string(),
                        status: "streaming".to_string(),
                        label: format!("Streaming from {}", target.provider),
                        detail: format!(
                            "Receiving output from {} / {}.",
                            target.provider, target.model
                        ),
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        route_id: target.route_id.clone(),
                        route_title: target.route_title.clone(),
                        attempt,
                        attempt_count,
                        created_at: unix_timestamp(),
                    },
                )
                .await;
            }

            stream_state.assistant_content = text;
            persist_streaming_assistant(state, session_id, stream_state, true).await?;
        }
    }

    Ok(())
}

async fn persist_streaming_assistant(
    state: &AppState,
    session_id: &str,
    stream_state: &mut PromptStreamState,
    force: bool,
) -> Result<(), ApiError> {
    if stream_state.assistant_content.is_empty() {
        return Ok(());
    }

    if !force {
        if stream_state.assistant_content == stream_state.last_persisted_content {
            return Ok(());
        }

        if let Some(last_flush_at) = stream_state.last_flush_at {
            if last_flush_at.elapsed() < STREAM_FLUSH_INTERVAL
                && !stream_state.assistant_content.ends_with('\n')
            {
                return Ok(());
            }
        }
    }

    if let Some(turn_id) = stream_state.assistant_turn_id.as_deref() {
        state.store.update_session_turn_content(
            session_id,
            turn_id,
            &stream_state.assistant_content,
        )?;
    } else {
        let turn = state.store.append_session_turn(
            session_id,
            &Uuid::new_v4().to_string(),
            "assistant",
            &stream_state.assistant_content,
            &[],
        )?;
        stream_state.assistant_turn_id = Some(turn.id);
    }

    stream_state.last_persisted_content = stream_state.assistant_content.clone();
    stream_state.last_flush_at = Some(Instant::now());
    let detail = state.store.get_session(session_id)?;
    let _ = publish_session_event(state, detail).await;
    Ok(())
}

async fn host_status(State(state): State<AppState>) -> Json<nucleus_protocol::HostStatus> {
    Json(state.host.host_status())
}

async fn system_stats(State(state): State<AppState>) -> Json<SystemStats> {
    Json(state.host.system_stats())
}

#[derive(Debug, Deserialize)]
struct ProcessQuery {
    sort: Option<String>,
    limit: Option<usize>,
}

async fn processes(
    State(state): State<AppState>,
    Query(query): Query<ProcessQuery>,
) -> Result<Json<ProcessListResponse>, ApiError> {
    let sort = ProcessSort::parse(query.sort.as_deref())?;
    let limit = resolve_process_limit(query.limit)?;

    Ok(Json(state.host.processes(sort, limit)?))
}

async fn kill_process(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ProcessKillResponse>, ApiError> {
    let payload = decode_json::<ProcessKillRequest>(&body)?;
    let (response, _) =
        terminate_process_with_audit(&state, payload.pid, "system.processes").await?;
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
}

async fn audit_events(
    State(state): State<AppState>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEvent>>, ApiError> {
    let limit = resolve_audit_limit(query.limit)?;
    Ok(Json(state.store.list_audit_events(limit)?))
}

async fn run_action(
    State(state): State<AppState>,
    Path(action_id): Path<String>,
    body: Bytes,
) -> Result<Json<ActionRunResponse>, ApiError> {
    let payload = decode_json::<ActionRunRequest>(&body)?;
    Ok(Json(execute_action(&state, &action_id, payload).await?))
}

async fn stream_socket(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_stream_socket(socket, state))
}

async fn handle_stream_socket(mut socket: WebSocket, state: AppState) {
    if let Err(error) = send_event(
        &mut socket,
        DaemonEvent::Connected(StreamConnected {
            service: PRODUCT_NAME.to_string(),
            version: state.version.clone(),
        }),
    )
    .await
    {
        warn!(error = %error, "failed to send websocket connect event");
        return;
    }

    if let Err(error) = send_initial_stream_snapshot(&mut socket, &state).await {
        warn!(error = %error, "failed to send websocket initial snapshot");
        let _ = socket.close().await;
        return;
    }

    let mut receiver = state.events.subscribe();

    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        warn!(error = %error, "websocket receive error");
                        break;
                    }
                }
            }
            event = receiver.recv() => {
                match event {
                    Ok(event) => {
                        if let Err(error) = send_event(&mut socket, event).await {
                            warn!(error = %error, "failed to publish websocket event");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "websocket client lagged behind stream");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn send_initial_stream_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
) -> anyhow::Result<()> {
    let frame = state.host.telemetry_frame(STREAM_PROCESS_LIMIT);
    let overview = build_runtime_overview(state, frame.host_status.clone(), false).await?;
    let audit = state.store.list_audit_events(DEFAULT_AUDIT_LIMIT)?;

    send_event(socket, DaemonEvent::OverviewUpdated(overview)).await?;
    send_event(socket, DaemonEvent::AuditUpdated(audit)).await?;
    send_event(socket, DaemonEvent::SystemUpdated(frame.system_stats)).await?;
    send_event(
        socket,
        DaemonEvent::ProcessesUpdated(ProcessStreamUpdate {
            sort: ProcessSort::Cpu.as_str().to_string(),
            response: frame.processes_cpu,
        }),
    )
    .await?;
    send_event(
        socket,
        DaemonEvent::ProcessesUpdated(ProcessStreamUpdate {
            sort: ProcessSort::Memory.as_str().to_string(),
            response: frame.processes_memory,
        }),
    )
    .await?;
    send_event(
        socket,
        DaemonEvent::UpdateUpdated(state.updates.current().await),
    )
    .await?;

    Ok(())
}

async fn send_event(socket: &mut WebSocket, event: DaemonEvent) -> anyhow::Result<()> {
    let payload = serde_json::to_string(&event).context("failed to serialize websocket event")?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .context("failed to send websocket frame")
}

fn spawn_event_publisher(state: AppState) {
    tokio::spawn(async move {
        let mut interval = time::interval(STREAM_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            if state.events.receiver_count() == 0 {
                continue;
            }

            if let Err(error) = publish_stream_snapshot(&state).await {
                warn!(error = %error, "failed to publish daemon stream snapshot");
            }
        }
    });
}

fn spawn_update_monitor(state: AppState) {
    tokio::spawn(async move {
        time::sleep(INITIAL_UPDATE_CHECK_DELAY).await;

        let initial = state.updates.check().await;
        if initial.changed {
            let _ = publish_update_event(&state, initial.status).await;
        }

        let mut interval = time::interval(UPDATE_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            interval.tick().await;
            let next = state.updates.check().await;

            if next.changed {
                let _ = publish_update_event(&state, next.status).await;
            }
        }
    });
}

async fn publish_stream_snapshot(state: &AppState) -> anyhow::Result<()> {
    let frame = state.host.telemetry_frame(STREAM_PROCESS_LIMIT);
    let overview = build_runtime_overview(state, frame.host_status.clone(), false).await?;

    let _ = state.events.send(DaemonEvent::OverviewUpdated(overview));
    let _ = state
        .events
        .send(DaemonEvent::SystemUpdated(frame.system_stats));
    let _ = state
        .events
        .send(DaemonEvent::ProcessesUpdated(ProcessStreamUpdate {
            sort: ProcessSort::Cpu.as_str().to_string(),
            response: frame.processes_cpu,
        }));
    let _ = state
        .events
        .send(DaemonEvent::ProcessesUpdated(ProcessStreamUpdate {
            sort: ProcessSort::Memory.as_str().to_string(),
            response: frame.processes_memory,
        }));

    Ok(())
}

async fn publish_overview_event(state: &AppState) -> anyhow::Result<()> {
    let overview = build_runtime_overview(state, state.host.host_status(), false).await?;
    let _ = state.events.send(DaemonEvent::OverviewUpdated(overview));
    Ok(())
}

async fn publish_session_event(state: &AppState, detail: SessionDetail) -> anyhow::Result<()> {
    let _ = state.events.send(DaemonEvent::SessionUpdated(detail));
    Ok(())
}

async fn publish_prompt_progress_event(
    state: &AppState,
    progress: PromptProgressUpdate,
) -> anyhow::Result<()> {
    let _ = state.events.send(DaemonEvent::PromptProgress(progress));
    Ok(())
}

async fn publish_audit_event(state: &AppState) -> anyhow::Result<()> {
    let audit = state.store.list_audit_events(DEFAULT_AUDIT_LIMIT)?;
    let _ = state.events.send(DaemonEvent::AuditUpdated(audit));
    Ok(())
}

async fn publish_update_event(state: &AppState, update: UpdateStatus) -> anyhow::Result<()> {
    let _ = state.events.send(DaemonEvent::UpdateUpdated(update));
    Ok(())
}

async fn build_settings_summary(state: &AppState) -> SettingsSummary {
    SettingsSummary {
        product: PRODUCT_NAME.to_string(),
        version: state.version.clone(),
        instance: state.updates.instance_summary(),
        storage: state.store.storage_summary(),
        update: state.updates.current().await,
    }
}

async fn build_runtime_overview(
    state: &AppState,
    host: HostStatus,
    force_runtime_refresh: bool,
) -> anyhow::Result<RuntimeOverview> {
    let runtimes = load_runtimes(state, force_runtime_refresh).await?;
    Ok(RuntimeOverview {
        product: PRODUCT_NAME.to_string(),
        version: state.version.clone(),
        runtimes: runtimes.clone(),
        router_profiles: enrich_router_profiles(state.store.list_router_profiles()?, &runtimes),
        workspace: state.store.workspace()?,
        sessions: state.store.list_sessions()?,
        host,
        storage: state.store.storage_summary(),
    })
}

async fn load_runtimes(
    state: &AppState,
    force_refresh: bool,
) -> anyhow::Result<Vec<RuntimeSummary>> {
    let base = state.store.list_runtimes()?;
    state.runtimes.list_runtimes(base, force_refresh).await
}

async fn load_router_profiles(
    state: &AppState,
    force_runtime_refresh: bool,
) -> anyhow::Result<Vec<RouterProfileSummary>> {
    let runtimes = load_runtimes(state, force_runtime_refresh).await?;
    Ok(enrich_router_profiles(
        state.store.list_router_profiles()?,
        &runtimes,
    ))
}

fn enrich_router_profiles(
    profiles: Vec<RouterProfileSummary>,
    runtimes: &[RuntimeSummary],
) -> Vec<RouterProfileSummary> {
    profiles
        .into_iter()
        .map(|mut profile| {
            profile.state = if !profile.enabled {
                "disabled".to_string()
            } else if profile.targets.iter().any(|target| {
                runtimes
                    .iter()
                    .any(|runtime| runtime.id == target.provider && runtime.state == "ready")
            }) {
                "ready".to_string()
            } else if profile.targets.iter().any(|target| {
                runtimes
                    .iter()
                    .any(|runtime| runtime.id == target.provider && runtime.supports_prompting)
            }) {
                "degraded".to_string()
            } else {
                "unavailable".to_string()
            };
            profile
        })
        .collect()
}

async fn ensure_prompting_runtime(
    state: &AppState,
    provider: &str,
    force_refresh: bool,
) -> Result<(), ApiError> {
    let runtimes = load_runtimes(state, force_refresh).await?;
    let runtime = runtimes
        .into_iter()
        .find(|runtime| runtime.id == provider)
        .ok_or_else(|| ApiError::bad_request(format!("unknown provider '{provider}'")))?;

    if !runtime.supports_prompting {
        return Err(ApiError::bad_request(format!(
            "provider '{provider}' does not support prompting yet",
        )));
    }

    if runtime.state != "ready" {
        return Err(ApiError::bad_request(format!(
            "{}",
            if runtime.note.is_empty() {
                format!("provider '{provider}' is not ready")
            } else {
                runtime.note
            }
        )));
    }

    Ok(())
}

fn resolve_provider(value: &str) -> Result<AdapterKind, ApiError> {
    AdapterKind::parse(value)
        .ok_or_else(|| ApiError::bad_request(format!("unsupported provider '{value}'")))
}

fn sanitize_workspace_root(value: &str) -> Result<String, ApiError> {
    let path = value.trim();

    if path.is_empty() {
        return Err(ApiError::bad_request("workspace root cannot be empty"));
    }

    let metadata = std::fs::metadata(&path)
        .map_err(|_| ApiError::bad_request(format!("workspace root '{path}' does not exist")))?;

    if !metadata.is_dir() {
        return Err(ApiError::bad_request(format!(
            "workspace root '{path}' is not a directory",
        )));
    }

    Ok(path.to_string())
}

#[derive(Debug, Clone)]
struct SessionTargetSelection {
    route_id: String,
    route_title: String,
    provider: String,
    model: String,
}

#[derive(Debug, Clone)]
struct SessionProjectSelection {
    scope: String,
    primary_project_id: String,
    primary_project_title: String,
    primary_project_path: String,
    project_ids: Vec<String>,
    working_dir: String,
    working_dir_kind: String,
}

#[derive(Debug, Clone, Default)]
struct WorkspaceTargetSelection {
    route_id: Option<String>,
    provider: Option<String>,
}

#[derive(Debug, Clone)]
struct PromptTarget {
    provider: String,
    model: String,
    route_id: String,
    route_title: String,
}

#[derive(Debug, Clone)]
struct PromptIncludeSource {
    scope: &'static str,
    path: PathBuf,
    content: String,
}

#[derive(Debug, Clone)]
struct PromptAssembly {
    prompt: String,
    detail: String,
}

#[derive(Debug, Default)]
struct PromptStreamState {
    assistant_turn_id: Option<String>,
    assistant_content: String,
    last_persisted_content: String,
    last_flush_at: Option<Instant>,
    provider_session_id: String,
    last_reasoning_excerpt: String,
    streaming_announced: bool,
}

impl PromptStreamState {
    fn reset_for_attempt(&mut self) {
        self.assistant_content.clear();
        self.last_persisted_content.clear();
        self.last_flush_at = None;
        self.provider_session_id.clear();
        self.last_reasoning_excerpt.clear();
        self.streaming_announced = false;
    }
}

fn resolve_session_projects(
    state: &AppState,
    project_id: Option<&str>,
    primary_project_id: Option<&str>,
    project_ids: Option<&[String]>,
    scratch_session_id: Option<&str>,
    fallback: Option<&SessionSummary>,
) -> Result<SessionProjectSelection, ApiError> {
    let has_explicit_project_set = project_ids.is_some();
    let mut requested_ids = project_ids.map(|items| items.to_vec()).unwrap_or_else(|| {
        fallback
            .map(|session| {
                session
                    .projects
                    .iter()
                    .map(|project| project.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    });

    if requested_ids.is_empty() {
        if let Some(project_id) = project_id.map(str::trim).filter(|value| !value.is_empty()) {
            requested_ids.push(project_id.to_string());
        }
    }

    let requested_primary = primary_project_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            project_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            if has_explicit_project_set {
                None
            } else {
                fallback
                    .map(|session| session.project_id.clone())
                    .filter(|value| !value.is_empty())
            }
        });

    if let Some(primary_project_id) = requested_primary.as_ref() {
        if !requested_ids
            .iter()
            .any(|project_id| project_id == primary_project_id)
        {
            requested_ids.insert(0, primary_project_id.clone());
        }
    }

    let resolved = state
        .store
        .resolve_projects(&requested_ids)
        .map_err(ApiError::from)?;

    if resolved.is_empty() {
        let scratch_dir = state
            .store
            .scratch_dir_for_session(
                scratch_session_id
                    .or_else(|| fallback.map(|session| session.id.as_str()))
                    .unwrap_or("ad-hoc-preview"),
            )
            .map_err(ApiError::from)?;

        return Ok(SessionProjectSelection {
            scope: "ad_hoc".to_string(),
            primary_project_id: String::new(),
            primary_project_title: String::new(),
            primary_project_path: String::new(),
            project_ids: Vec::new(),
            working_dir: scratch_dir,
            working_dir_kind: "workspace_scratch".to_string(),
        });
    }

    let primary_project = match requested_primary {
        Some(primary_project_id) => resolved
            .iter()
            .find(|project| project.id == primary_project_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "primary project '{}' is not part of the selected workspace projects",
                    primary_project_id
                ))
            })?,
        None => resolved
            .first()
            .cloned()
            .ok_or_else(|| ApiError::bad_request("at least one project is required"))?,
    };

    Ok(SessionProjectSelection {
        scope: match resolved.len() {
            0 => "ad_hoc",
            1 => "project",
            _ => "multi_project",
        }
        .to_string(),
        primary_project_id: primary_project.id.clone(),
        primary_project_title: primary_project.title.clone(),
        primary_project_path: primary_project.absolute_path.clone(),
        project_ids: resolved.into_iter().map(|project| project.id).collect(),
        working_dir: primary_project.absolute_path,
        working_dir_kind: "project_root".to_string(),
    })
}

fn parse_target_selector(value: &str) -> WorkspaceTargetSelection {
    let value = value.trim();

    if let Some(route_id) = value.strip_prefix("route:").map(str::trim) {
        if !route_id.is_empty() {
            return WorkspaceTargetSelection {
                route_id: Some(route_id.to_string()),
                provider: None,
            };
        }
    }

    if let Some(provider) = value.strip_prefix("provider:").map(str::trim) {
        if !provider.is_empty() {
            return WorkspaceTargetSelection {
                route_id: None,
                provider: Some(provider.to_string()),
            };
        }
    }

    WorkspaceTargetSelection::default()
}

async fn sanitize_workspace_target(
    state: &AppState,
    route_profiles: &[RouterProfileSummary],
    value: &str,
) -> Result<String, ApiError> {
    let selection = parse_target_selector(value);

    let resolved = resolve_session_target(
        state,
        route_profiles,
        selection.route_id.as_deref(),
        selection.provider.as_deref(),
        None,
    )
    .await?;

    if !resolved.route_id.is_empty() {
        Ok(format!("route:{}", resolved.route_id))
    } else {
        Ok(format!("provider:{}", resolved.provider))
    }
}

async fn resolve_session_target(
    state: &AppState,
    route_profiles: &[RouterProfileSummary],
    route_id: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<SessionTargetSelection, ApiError> {
    if let Some(route_id) = route_id.map(str::trim).filter(|value| !value.is_empty()) {
        let route = route_profiles
            .iter()
            .find(|profile| profile.id == route_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown router profile '{route_id}'")))?;

        if !route.enabled {
            return Err(ApiError::bad_request(format!(
                "router profile '{}' is disabled",
                route.title
            )));
        }

        let targets = resolve_profile_targets(state, &route, false).await?;
        let target = targets.into_iter().next().ok_or_else(|| {
            ApiError::bad_request(format!(
                "router profile '{}' has no usable targets",
                route.title
            ))
        })?;

        return Ok(SessionTargetSelection {
            route_id: route.id,
            route_title: route.title,
            provider: target.provider,
            model: if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
                model.to_string()
            } else {
                target.model
            },
        });
    }

    let provider = provider
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("either route_id or provider is required"))?;
    let provider = resolve_provider(provider)?;

    if !provider.supports_sessions() {
        return Err(ApiError::bad_request(format!(
            "provider '{}' cannot create daemon-managed sessions yet",
            provider.as_str()
        )));
    }

    ensure_prompting_runtime(state, provider.as_str(), false).await?;

    Ok(SessionTargetSelection {
        route_id: String::new(),
        route_title: String::new(),
        provider: provider.as_str().to_string(),
        model: model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| provider.default_model().to_string()),
    })
}

async fn resolve_profile_targets(
    state: &AppState,
    route: &RouterProfileSummary,
    force_runtime_refresh: bool,
) -> Result<Vec<PromptTarget>, ApiError> {
    let runtimes = load_runtimes(state, force_runtime_refresh).await?;
    let mut ready = Vec::new();
    let mut pending = Vec::new();

    for target in &route.targets {
        let runtime = runtimes
            .iter()
            .find(|runtime| runtime.id == target.provider);

        let item = PromptTarget {
            provider: target.provider.clone(),
            model: target.model.clone(),
            route_id: route.id.clone(),
            route_title: route.title.clone(),
        };

        match runtime {
            Some(runtime) if runtime.supports_prompting && runtime.state == "ready" => {
                ready.push(item)
            }
            Some(runtime) if runtime.supports_prompting => pending.push(item),
            _ => {}
        }
    }

    ready.extend(pending);
    Ok(ready)
}

async fn resolve_prompt_targets(
    state: &AppState,
    session: &SessionSummary,
    requires_image_support: bool,
) -> Result<Vec<PromptTarget>, ApiError> {
    if !session.route_id.is_empty() {
        let route = load_router_profiles(state, false)
            .await?
            .into_iter()
            .find(|profile| profile.id == session.route_id)
            .ok_or_else(|| {
                ApiError::bad_request(format!("unknown router profile '{}'", session.route_id))
            })?;
        let mut targets = resolve_profile_targets(state, &route, false).await?;

        targets.sort_by_key(|target| {
            let matches_active_target =
                target.provider == session.provider && target.model == session.model;
            let supports_images = provider_supports_images(&target.provider);

            if requires_image_support {
                (!supports_images, !matches_active_target)
            } else {
                (!matches_active_target, false)
            }
        });

        if requires_image_support {
            targets.retain(|target| provider_supports_images(&target.provider));
        }

        if requires_image_support && targets.is_empty() {
            return Err(ApiError::bad_request(format!(
                "router profile '{}' has no image-capable targets ready to accept this prompt",
                route.title
            )));
        }

        return Ok(targets);
    }

    if requires_image_support && !provider_supports_images(&session.provider) {
        return Err(ApiError::bad_request(format!(
            "provider '{}' does not support image attachments in Nucleus yet",
            session.provider
        )));
    }

    ensure_prompting_runtime(state, &session.provider, false).await?;

    Ok(vec![PromptTarget {
        provider: session.provider.clone(),
        model: session.model.clone(),
        route_id: String::new(),
        route_title: String::new(),
    }])
}

fn assemble_prompt_input(
    state: &AppState,
    session: &SessionSummary,
    prompt: &str,
) -> Result<PromptAssembly, ApiError> {
    let sources = discover_prompt_sources(state, session)?;

    if sources.is_empty() {
        return Ok(PromptAssembly {
            prompt: prompt.to_string(),
            detail: "No include context found. Using the raw prompt.".to_string(),
        });
    }

    let mut scopes = BTreeSet::new();
    let mut listed_paths = Vec::new();
    for source in &sources {
        scopes.insert(source.scope);
        if listed_paths.len() < 4 {
            listed_paths.push(compact_prompt_source_path(&source.path));
        }
    }

    Ok(PromptAssembly {
        prompt: render_prompt_with_sources(prompt, &sources),
        detail: format!(
            "Loaded {} include file(s) across {} scope(s): {}{}",
            sources.len(),
            scopes.len(),
            listed_paths.join(", "),
            if sources.len() > listed_paths.len() {
                format!(" +{} more", sources.len() - listed_paths.len())
            } else {
                String::new()
            }
        ),
    })
}

fn discover_prompt_sources(
    state: &AppState,
    session: &SessionSummary,
) -> Result<Vec<PromptIncludeSource>, ApiError> {
    let workspace = state.store.workspace()?;
    let mut roots = Vec::new();

    if let Some(home_dir) = dirs::home_dir() {
        roots.push(("global", home_dir));
    }

    let workspace_root = PathBuf::from(workspace.root_path);
    roots.push(("workspace", workspace_root));
    roots.push(("session", PathBuf::from(&session.working_dir)));

    for project in &session.projects {
        roots.push(("project", PathBuf::from(&project.absolute_path)));
    }

    let mut seen_files = BTreeSet::new();
    let mut sources = Vec::new();
    let mut total_chars = 0usize;

    for (scope, root) in roots {
        collect_prompt_sources_from_root(
            scope,
            &root,
            &mut seen_files,
            &mut sources,
            &mut total_chars,
        )?;

        if sources.len() >= MAX_PROMPT_INCLUDE_FILES
            || total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS
        {
            break;
        }
    }

    Ok(sources)
}

fn collect_prompt_sources_from_root(
    scope: &'static str,
    root: &PathBuf,
    seen_files: &mut BTreeSet<PathBuf>,
    sources: &mut Vec<PromptIncludeSource>,
    total_chars: &mut usize,
) -> Result<(), ApiError> {
    if !root.is_dir() {
        return Ok(());
    }

    for include_dir in [
        root.join(".nucleus").join("include"),
        root.join("promptinclude"),
        root.join("include"),
    ] {
        collect_include_directory_sources(scope, &include_dir, seen_files, sources, total_chars)?;

        if sources.len() >= MAX_PROMPT_INCLUDE_FILES
            || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS
        {
            return Ok(());
        }
    }

    collect_legacy_promptinclude_sources(scope, root, seen_files, sources, total_chars)?;
    Ok(())
}

fn collect_include_directory_sources(
    scope: &'static str,
    include_dir: &PathBuf,
    seen_files: &mut BTreeSet<PathBuf>,
    sources: &mut Vec<PromptIncludeSource>,
    total_chars: &mut usize,
) -> Result<(), ApiError> {
    if !include_dir.is_dir() {
        return Ok(());
    }

    let mut markdown_files = Vec::new();
    collect_markdown_files(include_dir, &mut markdown_files)?;
    markdown_files.sort();

    for file in markdown_files {
        push_prompt_source(scope, &file, seen_files, sources, total_chars)?;

        if sources.len() >= MAX_PROMPT_INCLUDE_FILES
            || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS
        {
            break;
        }
    }

    Ok(())
}

fn collect_legacy_promptinclude_sources(
    scope: &'static str,
    root: &PathBuf,
    seen_files: &mut BTreeSet<PathBuf>,
    sources: &mut Vec<PromptIncludeSource>,
    total_chars: &mut usize,
) -> Result<(), ApiError> {
    let read_dir = fs::read_dir(root).map_err(|error| {
        ApiError::internal_message(format!(
            "failed to read prompt include root '{}': {error}",
            root.display()
        ))
    })?;

    let mut legacy_files = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|error| {
            ApiError::internal_message(format!(
                "failed to inspect prompt include root '{}': {error}",
                root.display()
            ))
        })?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.ends_with(".promptinclude.md") && path.is_file() {
            legacy_files.push(path);
        }
    }

    legacy_files.sort();
    for file in legacy_files {
        push_prompt_source(scope, &file, seen_files, sources, total_chars)?;

        if sources.len() >= MAX_PROMPT_INCLUDE_FILES
            || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS
        {
            break;
        }
    }

    Ok(())
}

fn collect_markdown_files(dir: &PathBuf, results: &mut Vec<PathBuf>) -> Result<(), ApiError> {
    let read_dir = fs::read_dir(dir).map_err(|error| {
        ApiError::internal_message(format!(
            "failed to read include directory '{}': {error}",
            dir.display()
        ))
    })?;

    for entry in read_dir {
        let entry = entry.map_err(|error| {
            ApiError::internal_message(format!(
                "failed to inspect include directory '{}': {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();

        if path.is_dir() {
            collect_markdown_files(&path, results)?;
            continue;
        }

        if !path.is_file() {
            continue;
        }

        let extension = path.extension().and_then(|extension| extension.to_str());
        if matches!(extension, Some("md") | Some("markdown")) {
            results.push(path);
        }
    }

    Ok(())
}

fn push_prompt_source(
    scope: &'static str,
    path: &PathBuf,
    seen_files: &mut BTreeSet<PathBuf>,
    sources: &mut Vec<PromptIncludeSource>,
    total_chars: &mut usize,
) -> Result<(), ApiError> {
    if sources.len() >= MAX_PROMPT_INCLUDE_FILES || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS {
        return Ok(());
    }

    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    if !seen_files.insert(canonical) {
        return Ok(());
    }

    let contents = fs::read_to_string(path).map_err(|error| {
        ApiError::internal_message(format!(
            "failed to read include file '{}': {error}",
            path.display()
        ))
    })?;
    let trimmed = contents.trim();

    if trimmed.is_empty() {
        return Ok(());
    }

    let remaining = MAX_PROMPT_INCLUDE_TOTAL_CHARS.saturating_sub(*total_chars);
    if remaining == 0 {
        return Ok(());
    }

    let truncated = excerpt(trimmed, MAX_PROMPT_INCLUDE_FILE_CHARS.min(remaining));
    *total_chars += truncated.chars().count();
    sources.push(PromptIncludeSource {
        scope,
        path: path.clone(),
        content: truncated,
    });
    Ok(())
}

fn render_prompt_with_sources(prompt: &str, sources: &[PromptIncludeSource]) -> String {
    let mut rendered = String::from(
        "Session context for this turn. Treat these files as always-on instructions and local knowledge.\n",
    );

    for source in sources {
        rendered.push_str("\n[");
        rendered.push_str(source.scope);
        rendered.push_str(" include: ");
        rendered.push_str(&compact_prompt_source_path(&source.path));
        rendered.push_str("]\n");
        rendered.push_str(&source.content);
        rendered.push('\n');
    }

    rendered.push_str("\nUser request:\n");
    rendered.push_str(prompt);
    rendered
}

fn compact_prompt_source_path(path: &PathBuf) -> String {
    let path_string = path.display().to_string();
    if let Some(home_dir) = dirs::home_dir() {
        let home_display = home_dir.display().to_string();
        if let Some(stripped) = path_string.strip_prefix(&home_display) {
            return format!("~{stripped}");
        }
    }

    path_string
}

fn provider_supports_images(provider: &str) -> bool {
    matches!(AdapterKind::parse(provider), Some(AdapterKind::Codex))
}

fn effective_prompt_text(prompt: &str, image_count: usize) -> String {
    if !prompt.trim().is_empty() {
        return prompt.trim().to_string();
    }

    if image_count == 0 {
        return String::new();
    }

    if image_count == 1 {
        "Review the attached image and respond with the most useful analysis.".to_string()
    } else {
        format!(
            "Review the {image_count} attached images and respond with the most useful analysis."
        )
    }
}

fn build_prompt_execution_session(
    session: &SessionSummary,
    target: &PromptTarget,
) -> SessionSummary {
    let provider_session_id =
        if target.provider == session.provider && target.model == session.model {
            session.provider_session_id.clone()
        } else {
            String::new()
        };

    SessionSummary {
        provider: target.provider.clone(),
        model: target.model.clone(),
        provider_session_id,
        ..session.clone()
    }
}

fn build_reroute_prompt(
    turns: &[nucleus_protocol::SessionTurn],
    prompt: &str,
    image_count: usize,
) -> String {
    let mut transcript =
        String::from("Continue this session using the following transcript as context.\n\n");

    for turn in turns.iter().rev().take(12).rev() {
        transcript.push_str(&format!("{}:\n{}\n\n", turn.role, turn.content));
    }

    if image_count > 0 {
        transcript.push_str(&format!(
            "The next user turn also includes {image_count} attached image(s). Inspect them directly.\n\n"
        ));
    }

    transcript.push_str("Next user turn:\n");
    transcript.push_str(prompt);
    transcript
}

fn normalize_session_state(value: &str) -> Result<String, ApiError> {
    match value.trim() {
        "active" | "archived" | "error" | "running" => Ok(value.trim().to_string()),
        other => Err(ApiError::bad_request(format!(
            "unsupported session state '{other}'",
        ))),
    }
}

fn default_session_title(provider: AdapterKind) -> String {
    match provider {
        AdapterKind::Claude => "Claude session".to_string(),
        AdapterKind::Codex => "Codex session".to_string(),
        AdapterKind::System => "System session".to_string(),
    }
}

fn action_catalog() -> Vec<ActionSummary> {
    vec![
        ActionSummary {
            id: "runtime.refresh".to_string(),
            title: "Refresh runtimes".to_string(),
            category: "runtime".to_string(),
            summary: "Probe Claude, Codex, and system runtime readiness immediately.".to_string(),
            risk: "safe".to_string(),
            requires_confirmation: false,
            parameters: Vec::new(),
        },
        ActionSummary {
            id: "system.process.terminate".to_string(),
            title: "Terminate process".to_string(),
            category: "system".to_string(),
            summary:
                "Send SIGTERM to a user-owned process by PID through the daemon safety checks."
                    .to_string(),
            risk: "caution".to_string(),
            requires_confirmation: true,
            parameters: vec![ActionParameter {
                name: "pid".to_string(),
                label: "PID".to_string(),
                value_type: "number".to_string(),
                required: true,
                description: "Target process ID to terminate with SIGTERM.".to_string(),
                default_value: String::new(),
            }],
        },
    ]
}

fn resolve_audit_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    let limit = limit.unwrap_or(DEFAULT_AUDIT_LIMIT);

    if limit == 0 {
        return Err(ApiError::bad_request(
            "audit limit must be greater than zero".to_string(),
        ));
    }

    if limit > MAX_AUDIT_LIMIT {
        return Err(ApiError::bad_request(format!(
            "audit limit must be {MAX_AUDIT_LIMIT} or lower"
        )));
    }

    Ok(limit)
}

async fn execute_action(
    state: &AppState,
    action_id: &str,
    payload: ActionRunRequest,
) -> Result<ActionRunResponse, ApiError> {
    match action_id {
        "runtime.refresh" => {
            let runtimes = load_runtimes(state, true).await?;
            let message = format!("Refreshed {} runtimes.", runtimes.len());
            let audit = try_record_audit_event(
                state,
                AuditEventRecord {
                    kind: "action.executed".to_string(),
                    target: "action:runtime.refresh".to_string(),
                    status: "success".to_string(),
                    summary: "Refreshed runtime health.".to_string(),
                    detail: format!("count={}", runtimes.len()),
                },
            )
            .await;
            let _ = publish_overview_event(state).await;

            Ok(ActionRunResponse {
                action_id: action_id.to_string(),
                status: "success".to_string(),
                message,
                result: json!(runtimes),
                audit_event_id: audit.as_ref().map(|event| event.id),
            })
        }
        "system.process.terminate" => {
            let pid = read_action_pid(&payload, "pid")?;
            let (response, audit_event_id) =
                terminate_process_with_audit(state, pid, "action:system.process.terminate").await?;

            Ok(ActionRunResponse {
                action_id: action_id.to_string(),
                status: "success".to_string(),
                message: format!(
                    "Sent {} to process {} ({}).",
                    response.signal, response.killed_pid, response.name
                ),
                result: json!(response),
                audit_event_id,
            })
        }
        _ => Err(ApiError::not_found(format!(
            "action '{}' was not found",
            action_id
        ))),
    }
}

async fn terminate_process_with_audit(
    state: &AppState,
    pid: u32,
    source: &str,
) -> Result<(ProcessKillResponse, Option<i64>), ApiError> {
    let response = state.host.terminate_process(pid)?;
    let audit = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: "process.terminated".to_string(),
            target: format!("process:{}", response.killed_pid),
            status: "success".to_string(),
            summary: format!(
                "Sent {} to process {} ({}).",
                response.signal, response.killed_pid, response.name
            ),
            detail: format!(
                "source={} pid={} signal={} name={}",
                source, response.killed_pid, response.signal, response.name
            ),
        },
    )
    .await;
    let _ = publish_stream_snapshot(state).await;

    Ok((response, audit.as_ref().map(|event| event.id)))
}

fn read_action_pid(payload: &ActionRunRequest, name: &str) -> Result<u32, ApiError> {
    let params = payload
        .params
        .as_object()
        .ok_or_else(|| ApiError::bad_request("action params must be a JSON object".to_string()))?;
    let value = params.get(name).ok_or_else(|| {
        ApiError::bad_request(format!("missing required action param '{}'", name))
    })?;

    let pid = match value {
        Value::Number(number) => number.as_u64().ok_or_else(|| {
            ApiError::bad_request(format!(
                "action param '{}' must be a positive integer",
                name
            ))
        })?,
        Value::String(text) => text.trim().parse::<u64>().map_err(|_| {
            ApiError::bad_request(format!(
                "action param '{}' must be a positive integer",
                name
            ))
        })?,
        _ => {
            return Err(ApiError::bad_request(format!(
                "action param '{}' must be a positive integer",
                name
            )));
        }
    };

    u32::try_from(pid).map_err(|_| {
        ApiError::bad_request(format!(
            "action param '{}' is out of range for a process id",
            name
        ))
    })
}

async fn try_record_audit_event(state: &AppState, record: AuditEventRecord) -> Option<AuditEvent> {
    match state.store.append_audit_event(record) {
        Ok(event) => {
            let _ = publish_audit_event(state).await;
            Some(event)
        }
        Err(error) => {
            warn!(error = %error, "failed to persist audit event");
            None
        }
    }
}

fn describe_session_update(before: &SessionSummary, after: &SessionSummary) -> String {
    if before.state != after.state {
        return match after.state.as_str() {
            "archived" => format!("Archived {} session '{}'.", after.provider, after.title),
            "active" if before.state == "archived" => {
                format!("Reactivated {} session '{}'.", after.provider, after.title)
            }
            other => format!(
                "Updated {} session '{}' to {}.",
                after.provider, after.title, other
            ),
        };
    }

    if before.model != after.model {
        return format!(
            "Updated {} session '{}' model to {}.",
            after.provider,
            after.title,
            if after.model.is_empty() {
                "the provider default".to_string()
            } else {
                format!("'{}'", after.model)
            }
        );
    }

    if before.route_id != after.route_id {
        return format!(
            "Updated {} session '{}' route to {}.",
            after.provider,
            after.title,
            if after.route_title.is_empty() {
                "direct provider mode".to_string()
            } else {
                format!("'{}'", after.route_title)
            }
        );
    }

    if before.project_count != after.project_count || before.project_id != after.project_id {
        if after.project_count == 0 {
            return format!(
                "Moved {} session '{}' into the workspace scratch area.",
                after.provider, after.title
            );
        }

        if after.project_count == 1 {
            return format!(
                "Attached {} session '{}' to project '{}'.",
                after.provider, after.title, after.project_title
            );
        }

        return format!(
            "Updated {} session '{}' to {} attached projects with '{}' as the working directory.",
            after.provider, after.title, after.project_count, after.project_title
        );
    }

    if before.title != after.title {
        return format!("Renamed {} session to '{}'.", after.provider, after.title);
    }

    format!("Updated {} session '{}'.", after.provider, after.title)
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

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn decode_json<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice::<T>(body)
        .map_err(|error| ApiError::bad_request(format!("invalid request body: {error}")))
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nucleus_daemon=info,tower_http=info".into()),
        )
        .try_init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut stream) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = stream.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
        }
    }

    fn internal_message(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        let message = error.to_string();

        if message.contains("was not found") {
            return Self::not_found(message);
        }

        Self::internal_message(message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(serde_json::json!({
            "error": self.code,
            "message": self.message,
        }));

        (self.status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        env, fs,
        path::PathBuf,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn rejects_zero_audit_limit() {
        let error = resolve_audit_limit(Some(0)).expect_err("limit 0 should fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parses_numeric_action_pid() {
        let pid = read_action_pid(
            &ActionRunRequest {
                params: json!({ "pid": 4242 }),
            },
            "pid",
        )
        .expect("numeric pid should parse");

        assert_eq!(pid, 4242);
    }

    #[test]
    fn describes_model_updates() {
        let before = SessionSummary {
            id: "session-1".to_string(),
            title: "Test".to_string(),
            route_id: String::new(),
            route_title: String::new(),
            scope: "project".to_string(),
            project_id: "project-1".to_string(),
            project_title: "Project One".to_string(),
            project_path: "/home/eba/dev-projects/project-one".to_string(),
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            working_dir: "/home/eba/dev-projects/project-one".to_string(),
            working_dir_kind: "project_root".to_string(),
            project_count: 1,
            projects: vec![nucleus_protocol::SessionProjectSummary {
                id: "project-1".to_string(),
                title: "Project One".to_string(),
                slug: "project-one".to_string(),
                relative_path: "project-one".to_string(),
                absolute_path: "/home/eba/dev-projects/project-one".to_string(),
                is_primary: true,
            }],
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            created_at: 0,
            updated_at: 0,
        };
        let after = SessionSummary {
            model: "opus".to_string(),
            ..before.clone()
        };

        let summary = describe_session_update(&before, &after);
        assert!(summary.contains("model"));
        assert!(summary.contains("opus"));
    }

    #[test]
    fn collects_directory_and_legacy_include_files() {
        let root = test_state_dir("prompt-includes");
        let namespaced = root.join(".nucleus").join("include");
        let legacy = root.join("ports.promptinclude.md");
        let generic = root.join("include").join("notes.md");

        fs::create_dir_all(&namespaced).expect("namespaced include directory should exist");
        fs::create_dir_all(
            generic
                .parent()
                .expect("generic include parent should exist"),
        )
        .expect("generic include directory should exist");
        fs::write(
            namespaced.join("rules.md"),
            "# Rules\nAlways do the right thing.\n",
        )
        .expect("namespaced include file should write");
        fs::write(&legacy, "# Ports\nUse the assigned port.\n")
            .expect("legacy include file should write");
        fs::write(&generic, "# Notes\nProject-specific reminder.\n")
            .expect("generic include file should write");

        let mut seen_files = std::collections::BTreeSet::new();
        let mut sources = Vec::new();
        let mut total_chars = 0usize;
        collect_prompt_sources_from_root(
            "project",
            &root,
            &mut seen_files,
            &mut sources,
            &mut total_chars,
        )
        .expect("prompt sources should collect");

        assert_eq!(sources.len(), 3);
        let rendered = render_prompt_with_sources("Ship it.", &sources);
        assert!(rendered.contains("Always do the right thing."));
        assert!(rendered.contains("Project-specific reminder."));
        assert!(rendered.contains("User request:\nShip it."));

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn resolves_route_targets_from_cached_runtimes_when_creating_sessions() {
        let state_dir = test_state_dir("session-target-cache");
        let store =
            Arc::new(StateStore::initialize_at(&state_dir).expect("store should initialize"));
        let (events, _) = broadcast::channel(4);
        let runtimes = Arc::new(RuntimeManager::default());
        runtimes
            .seed_cache_for_test(vec![RuntimeSummary {
                id: "synthetic".to_string(),
                summary: "Synthetic cached runtime".to_string(),
                state: "ready".to_string(),
                auth_state: "ready".to_string(),
                version: "test".to_string(),
                executable_path: String::new(),
                default_model: "fast".to_string(),
                note: String::new(),
                supports_sessions: true,
                supports_prompting: true,
            }])
            .await;

        let state = AppState {
            version: "test".to_string(),
            store,
            host: Arc::new(HostEngine::new()),
            runtimes,
            updates: Arc::new(UpdateManager::new(test_instance_runtime())),
            events,
        };

        let profiles = vec![RouterProfileSummary {
            id: "synthetic-route".to_string(),
            title: "Synthetic Route".to_string(),
            summary: "Test route".to_string(),
            enabled: true,
            state: "ready".to_string(),
            targets: vec![nucleus_protocol::RouteTarget {
                provider: "synthetic".to_string(),
                model: "fast".to_string(),
            }],
        }];

        let result = resolve_session_target(&state, &profiles, Some("synthetic-route"), None, None)
            .await
            .expect("cached runtime should satisfy route resolution");

        assert_eq!(result.route_id, "synthetic-route");
        assert_eq!(result.provider, "synthetic");
        assert_eq!(result.model, "fast");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn creates_sessions_from_cached_default_provider_without_forcing_runtime_refresh() {
        let _env_lock = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let state_dir = test_state_dir("create-session-cache");
        let store =
            Arc::new(StateStore::initialize_at(&state_dir).expect("store should initialize"));
        store
            .update_workspace(None, Some("provider:claude"), None)
            .expect("workspace target should update");

        let (events, _) = broadcast::channel(4);
        let runtimes = Arc::new(RuntimeManager::default());
        runtimes
            .seed_cache_for_test(vec![RuntimeSummary {
                id: "claude".to_string(),
                summary: "Claude Code".to_string(),
                state: "ready".to_string(),
                auth_state: "ready".to_string(),
                version: "test".to_string(),
                executable_path: "/tmp/claude".to_string(),
                default_model: "sonnet".to_string(),
                note: String::new(),
                supports_sessions: true,
                supports_prompting: true,
            }])
            .await;

        let state = AppState {
            version: "test".to_string(),
            store,
            host: Arc::new(HostEngine::new()),
            runtimes,
            updates: Arc::new(UpdateManager::new(test_instance_runtime())),
            events,
        };

        let original_path = env::var_os("PATH");
        unsafe {
            env::set_var("PATH", "");
        }

        let result = create_session(State(state), Bytes::from_static(b"{}")).await;

        match original_path {
            Some(value) => unsafe {
                env::set_var("PATH", value);
            },
            None => unsafe {
                env::remove_var("PATH");
            },
        }

        let detail = result.expect("cached runtime should allow session creation");
        assert_eq!(detail.0.session.provider, "claude");
        assert_eq!(detail.0.session.model, "sonnet");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn resolves_direct_prompt_targets_from_cached_runtime_without_forcing_refresh() {
        let _env_lock = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let state_dir = test_state_dir("prompt-target-direct-cache");
        let store =
            Arc::new(StateStore::initialize_at(&state_dir).expect("store should initialize"));
        let (events, _) = broadcast::channel(4);
        let runtimes = Arc::new(RuntimeManager::default());
        runtimes
            .seed_cache_for_test(vec![RuntimeSummary {
                id: "claude".to_string(),
                summary: "Claude Code".to_string(),
                state: "ready".to_string(),
                auth_state: "ready".to_string(),
                version: "test".to_string(),
                executable_path: "/tmp/claude".to_string(),
                default_model: "sonnet".to_string(),
                note: String::new(),
                supports_sessions: true,
                supports_prompting: true,
            }])
            .await;

        let state = AppState {
            version: "test".to_string(),
            store,
            host: Arc::new(HostEngine::new()),
            runtimes,
            updates: Arc::new(UpdateManager::new(test_instance_runtime())),
            events,
        };

        let session = SessionSummary {
            id: "session-direct".to_string(),
            title: "Direct prompt".to_string(),
            route_id: String::new(),
            route_title: String::new(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            working_dir: "/tmp".to_string(),
            working_dir_kind: "workspace_scratch".to_string(),
            project_count: 0,
            projects: Vec::new(),
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            created_at: 0,
            updated_at: 0,
        };

        let original_path = env::var_os("PATH");
        unsafe {
            env::set_var("PATH", "");
        }

        let result = resolve_prompt_targets(&state, &session, false).await;

        match original_path {
            Some(value) => unsafe {
                env::set_var("PATH", value);
            },
            None => unsafe {
                env::remove_var("PATH");
            },
        }

        let targets = result.expect("cached runtime should satisfy direct prompt routing");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].provider, "claude");
        assert_eq!(targets[0].model, "sonnet");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn resolves_route_prompt_targets_from_cached_runtime_without_forcing_refresh() {
        let _env_lock = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let state_dir = test_state_dir("prompt-target-route-cache");
        let store =
            Arc::new(StateStore::initialize_at(&state_dir).expect("store should initialize"));
        let (events, _) = broadcast::channel(4);
        let runtimes = Arc::new(RuntimeManager::default());
        runtimes
            .seed_cache_for_test(vec![RuntimeSummary {
                id: "claude".to_string(),
                summary: "Claude Code".to_string(),
                state: "ready".to_string(),
                auth_state: "ready".to_string(),
                version: "test".to_string(),
                executable_path: "/tmp/claude".to_string(),
                default_model: "sonnet".to_string(),
                note: String::new(),
                supports_sessions: true,
                supports_prompting: true,
            }])
            .await;

        let state = AppState {
            version: "test".to_string(),
            store,
            host: Arc::new(HostEngine::new()),
            runtimes,
            updates: Arc::new(UpdateManager::new(test_instance_runtime())),
            events,
        };

        let session = SessionSummary {
            id: "session-route".to_string(),
            title: "Route prompt".to_string(),
            route_id: "balanced".to_string(),
            route_title: "Balanced".to_string(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            working_dir: "/tmp".to_string(),
            working_dir_kind: "workspace_scratch".to_string(),
            project_count: 0,
            projects: Vec::new(),
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            created_at: 0,
            updated_at: 0,
        };

        let original_path = env::var_os("PATH");
        unsafe {
            env::set_var("PATH", "");
        }

        let result = resolve_prompt_targets(&state, &session, false).await;

        match original_path {
            Some(value) => unsafe {
                env::set_var("PATH", value);
            },
            None => unsafe {
                env::remove_var("PATH");
            },
        }

        let targets = result.expect("cached runtime should satisfy route prompt routing");
        assert!(!targets.is_empty());
        assert_eq!(targets[0].provider, "claude");

        let _ = fs::remove_dir_all(&state_dir);
    }

    fn test_state_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("nucleus-{label}-{}-{suffix}", std::process::id()))
    }

    fn test_instance_runtime() -> InstanceRuntime {
        InstanceRuntime {
            name: "Test".to_string(),
            repo_root: env::current_dir().expect("cwd should resolve"),
            daemon_bind: "127.0.0.1:42240".to_string(),
            install_mode: "unsupported".to_string(),
        }
    }
}
