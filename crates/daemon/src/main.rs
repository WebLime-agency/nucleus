mod agent;
mod host;
mod runtime;
mod updates;

use std::{
    collections::BTreeSet,
    env, fs,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    process::Command as StdCommand,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        Path, Query, Request, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::SinkExt;
use host::{DEFAULT_PROCESS_LIMIT, HostEngine, ProcessSort, resolve_process_limit};
use nucleus_core::{
    AdapterKind, DEFAULT_DAEMON_ADDR, DEFAULT_OPENAI_COMPATIBLE_BASE_URL, PRODUCT_NAME,
    product_banner,
};
use nucleus_protocol::{
    ActionParameter, ActionRunRequest, ActionRunResponse, ActionSummary, ApprovalRequestSummary,
    ApprovalResolutionRequest, AuditEvent, AuthSummary, CompatibilitySummary, ConnectionSummary,
    CreatePlaybookRequest, CreateSessionRequest, DaemonEvent, HealthResponse, HostStatus,
    JobDetail, JobSummary, McpServerSummary, NucleusToolDescriptor, PlaybookDetail,
    PlaybookSummary, ProcessKillRequest, ProcessKillResponse, ProcessListResponse,
    ProcessStreamUpdate, ProjectUpdateRequest, PromptProgressUpdate, RouterProfileSummary,
    RuntimeOverview, RuntimeSummary, SessionDetail, SessionPromptRequest, SessionSummary,
    SettingsSummary, SkillManifest, StreamConnected, SystemStats, UpdateConfigRequest,
    UpdatePlaybookRequest, UpdateSessionRequest, UpdateStatus, WorkspaceModelConfig,
    WorkspaceProfileSummary, WorkspaceProfileWriteRequest, WorkspaceSummary,
    WorkspaceUpdateRequest,
};
use nucleus_release::read_installed_release_metadata;
use nucleus_storage::{
    AuditEventRecord, ProjectPatch, SessionPatch, SessionRecord, StateStore, WorkspaceProfilePatch,
};
use runtime::RuntimeManager;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{
    sync::broadcast,
    time::{self, MissedTickBehavior},
};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::{info, warn};
use updates::{InstanceRuntime, UpdateManager};
use uuid::Uuid;

const STREAM_PROCESS_LIMIT: usize = DEFAULT_PROCESS_LIMIT;
const STREAM_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_AUDIT_LIMIT: usize = 20;
const MAX_AUDIT_LIMIT: usize = 100;
const MAX_PROMPT_INCLUDE_FILES: usize = 24;
const MAX_PROMPT_INCLUDE_FILE_CHARS: usize = 6_000;
const MAX_PROMPT_INCLUDE_TOTAL_CHARS: usize = 24_000;
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(900);
const INITIAL_UPDATE_CHECK_DELAY: Duration = Duration::from_secs(3);
const RESTART_AFTER_RESPONSE_DELAY: Duration = Duration::from_millis(800);

#[derive(Clone)]
struct AppState {
    version: String,
    store: Arc<StateStore>,
    host: Arc<HostEngine>,
    runtimes: Arc<RuntimeManager>,
    updates: Arc<UpdateManager>,
    agent: Arc<agent::AgentRuntime>,
    web_dist_dir: Option<PathBuf>,
    tailscale_dns_name: Option<String>,
    events: broadcast::Sender<DaemonEvent>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind = env::var("NUCLEUS_BIND").unwrap_or_else(|_| DEFAULT_DAEMON_ADDR.to_string());
    let instance = InstanceRuntime::detect(bind.clone());
    let web_dist_dir = resolve_web_dist_dir(&instance);
    let store = Arc::new(StateStore::initialize().context("failed to initialize state store")?);
    let updates = Arc::new(UpdateManager::new(instance.clone(), store.clone()));
    let (events, _) = broadcast::channel(32);
    let state = AppState {
        version: env!("CARGO_PKG_VERSION").to_string(),
        store,
        host: Arc::new(HostEngine::new()),
        runtimes: Arc::new(RuntimeManager::default()),
        updates,
        agent: Arc::new(agent::AgentRuntime::default()),
        web_dist_dir,
        tailscale_dns_name: detect_tailscale_dns_name(),
        events,
    };
    agent::recover_interrupted_jobs(&state).await?;
    spawn_event_publisher(state.clone());
    spawn_update_monitor(state.clone());
    agent::spawn_playbook_scheduler(state.clone());

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
    let protected_api = Router::new()
        .route("/overview", get(overview))
        .route("/runtimes", get(runtimes))
        .route("/settings", get(settings))
        .route(
            "/settings/update/check",
            axum::routing::post(check_for_updates),
        )
        .route(
            "/settings/update-config",
            axum::routing::patch(update_update_config),
        )
        .route("/settings/update/apply", axum::routing::post(apply_update))
        .route("/settings/restart", axum::routing::post(restart_daemon))
        .route("/workspace", get(workspace).patch(update_workspace))
        .route(
            "/workspace/profiles",
            axum::routing::post(create_workspace_profile),
        )
        .route(
            "/workspace/profiles/{profile_id}",
            axum::routing::patch(update_workspace_profile).delete(delete_workspace_profile),
        )
        .route(
            "/workspace/projects/sync",
            axum::routing::post(sync_projects),
        )
        .route(
            "/workspace/projects/{project_id}",
            axum::routing::patch(update_project),
        )
        .route("/router/profiles", get(router_profiles))
        .route("/skills", get(list_skills).post(upsert_skill))
        .route("/skills/{skill_id}", axum::routing::put(upsert_skill_by_id))
        .route("/mcps", get(list_mcp_servers).post(upsert_mcp_server))
        .route(
            "/mcps/{server_id}",
            axum::routing::put(upsert_mcp_server_by_id),
        )
        .route("/actions", get(actions))
        .route("/actions/{action_id}", get(action_detail))
        .route("/actions/{action_id}/run", axum::routing::post(run_action))
        .route("/audit", get(audit_events))
        .route("/approvals", get(pending_approvals))
        .route(
            "/approvals/{approval_id}/approve",
            axum::routing::post(approve_request),
        )
        .route(
            "/approvals/{approval_id}/deny",
            axum::routing::post(deny_request),
        )
        .route("/playbooks", get(playbooks).post(create_playbook))
        .route(
            "/playbooks/{playbook_id}",
            get(playbook_detail)
                .patch(update_playbook)
                .delete(delete_playbook),
        )
        .route(
            "/playbooks/{playbook_id}/run",
            axum::routing::post(run_playbook),
        )
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{session_id}/jobs", get(session_jobs))
        .route(
            "/sessions/{session_id}",
            get(session_detail)
                .patch(update_session)
                .delete(delete_session),
        )
        .route("/jobs/{job_id}", get(job_detail))
        .route("/jobs/{job_id}/cancel", axum::routing::post(cancel_job))
        .route("/jobs/{job_id}/resume", axum::routing::post(resume_job))
        .route(
            "/sessions/{session_id}/prompt",
            get(session_detail).post(prompt_session),
        )
        .route("/host-status", get(host_status))
        .route("/system", get(system_stats))
        .route("/system/processes", get(processes).post(kill_process))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_auth,
        ));

    let router = Router::new()
        .route("/health", get(health))
        .route("/api/health", get(health))
        .nest("/api", protected_api)
        .route("/ws", get(stream_socket))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    match &state.web_dist_dir {
        Some(web_dist_dir) => router.fallback_service(static_web_service(web_dist_dir)),
        None => router.route("/", get(root)),
    }
}

async fn root() -> &'static str {
    "Nucleus"
}

async fn require_api_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    authorize_access(&state, request.headers(), None)?;
    Ok(next.run(request).await)
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

#[derive(Debug, Deserialize, Default)]
struct WebSocketAuthQuery {
    token: Option<String>,
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

async fn update_update_config(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<UpdateStatus>, ApiError> {
    let payload = decode_json::<UpdateConfigRequest>(&body)?;
    let result = state
        .updates
        .configure(payload.tracked_channel, payload.tracked_ref)
        .await?;
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
    if result.restart_requested {
        schedule_daemon_restart(state.clone());
    }
    Ok(Json(result.status))
}

async fn restart_daemon(State(state): State<AppState>) -> Result<Json<UpdateStatus>, ApiError> {
    let result = state.updates.request_restart().await;
    if result.changed {
        let _ = publish_update_event(&state, result.status.clone()).await;
    }
    if result.restart_requested {
        schedule_daemon_restart(state.clone());
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
    let default_profile_id = payload
        .default_profile_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let workspace = state.store.update_workspace(
        root_path.as_deref(),
        default_profile_id.as_deref(),
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
                "root_path={} default_profile_id={} main_target={} utility_target={}",
                root_path.unwrap_or_else(|| workspace.root_path.clone()),
                default_profile_id.unwrap_or_else(|| workspace.default_profile_id.clone()),
                main_target.unwrap_or_else(|| workspace.main_target.clone()),
                utility_target.unwrap_or_else(|| workspace.utility_target.clone())
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(workspace))
}

async fn create_workspace_profile(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<WorkspaceProfileSummary>, ApiError> {
    let payload = decode_json::<WorkspaceProfileWriteRequest>(&body)?;
    let patch = sanitize_workspace_profile_patch(&payload)?;
    let profile = state.store.create_workspace_profile(patch)?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "workspace.profile.created".to_string(),
            target: format!("workspace_profile:{}", profile.id),
            status: "success".to_string(),
            summary: format!("Created workspace profile '{}'.", profile.title),
            detail: format!(
                "default={} main_adapter={} utility_adapter={}",
                profile.is_default, profile.main.adapter, profile.utility.adapter
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(profile))
}

async fn update_workspace_profile(
    State(state): State<AppState>,
    Path(profile_id): Path<String>,
    body: Bytes,
) -> Result<Json<WorkspaceProfileSummary>, ApiError> {
    let payload = decode_json::<WorkspaceProfileWriteRequest>(&body)?;
    let patch = sanitize_workspace_profile_patch(&payload)?;
    let profile = state.store.update_workspace_profile(&profile_id, patch)?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "workspace.profile.updated".to_string(),
            target: format!("workspace_profile:{profile_id}"),
            status: "success".to_string(),
            summary: format!("Updated workspace profile '{}'.", profile.title),
            detail: format!(
                "default={} main_adapter={} utility_adapter={}",
                profile.is_default, profile.main.adapter, profile.utility.adapter
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(profile))
}

async fn delete_workspace_profile(
    State(state): State<AppState>,
    Path(profile_id): Path<String>,
) -> Result<Json<WorkspaceSummary>, ApiError> {
    let workspace = state.store.delete_workspace_profile(&profile_id)?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "workspace.profile.deleted".to_string(),
            target: format!("workspace_profile:{profile_id}"),
            status: "success".to_string(),
            summary: "Deleted workspace profile.".to_string(),
            detail: format!(
                "deleted_profile_id={} remaining_profiles={}",
                profile_id,
                workspace.profiles.len()
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
    if let Err(error) =
        agent::dispatch_playbook_event(state.clone(), "workspace_projects_synced").await
    {
        warn!(
            error = error.message.as_str(),
            "failed to dispatch workspace_projects_synced playbooks"
        );
    }
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

async fn list_skills(State(state): State<AppState>) -> Result<Json<Vec<SkillManifest>>, ApiError> {
    Ok(Json(state.store.list_skill_manifests()?))
}

async fn upsert_skill(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SkillManifest>, ApiError> {
    let payload = sanitize_skill_manifest(decode_json::<SkillManifest>(&body)?)?;
    Ok(Json(state.store.upsert_skill_manifest(&payload)?))
}

async fn upsert_skill_by_id(
    State(state): State<AppState>,
    Path(skill_id): Path<String>,
    body: Bytes,
) -> Result<Json<SkillManifest>, ApiError> {
    let mut payload = sanitize_skill_manifest(decode_json::<SkillManifest>(&body)?)?;
    payload.id = sanitize_registry_id(&skill_id, "skill id")?;
    Ok(Json(state.store.upsert_skill_manifest(&payload)?))
}

async fn list_mcp_servers(
    State(state): State<AppState>,
) -> Result<Json<Vec<McpServerSummary>>, ApiError> {
    Ok(Json(state.store.list_mcp_servers()?))
}

async fn upsert_mcp_server(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<McpServerSummary>, ApiError> {
    let payload = sanitize_mcp_server(decode_json::<McpServerSummary>(&body)?)?;
    Ok(Json(state.store.upsert_mcp_server(&payload)?))
}

async fn upsert_mcp_server_by_id(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
    body: Bytes,
) -> Result<Json<McpServerSummary>, ApiError> {
    let mut payload = sanitize_mcp_server(decode_json::<McpServerSummary>(&body)?)?;
    payload.id = sanitize_registry_id(&server_id, "MCP server id")?;
    Ok(Json(state.store.upsert_mcp_server(&payload)?))
}

async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    Ok(Json(state.store.list_sessions()?))
}

async fn playbooks(State(state): State<AppState>) -> Result<Json<Vec<PlaybookSummary>>, ApiError> {
    Ok(Json(agent::list_playbooks(state).await?))
}

async fn playbook_detail(
    State(state): State<AppState>,
    Path(playbook_id): Path<String>,
) -> Result<Json<PlaybookDetail>, ApiError> {
    Ok(Json(agent::get_playbook(state, playbook_id).await?))
}

async fn create_playbook(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<PlaybookDetail>, ApiError> {
    let payload = decode_json::<CreatePlaybookRequest>(&body)?;
    Ok(Json(agent::create_playbook(state, payload).await?))
}

async fn update_playbook(
    State(state): State<AppState>,
    Path(playbook_id): Path<String>,
    body: Bytes,
) -> Result<Json<PlaybookDetail>, ApiError> {
    let payload = decode_json::<UpdatePlaybookRequest>(&body)?;
    Ok(Json(
        agent::update_playbook(state, playbook_id, payload).await?,
    ))
}

async fn delete_playbook(
    State(state): State<AppState>,
    Path(playbook_id): Path<String>,
) -> Result<Json<PlaybookDetail>, ApiError> {
    Ok(Json(agent::delete_playbook(state, playbook_id).await?))
}

async fn run_playbook(
    State(state): State<AppState>,
    Path(playbook_id): Path<String>,
) -> Result<Json<JobDetail>, ApiError> {
    Ok(Json(agent::run_playbook(state, playbook_id).await?))
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
    let route_profiles = load_router_profiles(&state, false).await?;
    let requested_profile_id = payload
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
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
    let selection = if let Some(profile_id) = requested_profile_id.or_else(|| {
        if requested_route_id.is_none() && requested_provider.is_none() {
            Some(workspace.default_profile_id.as_str())
        } else {
            None
        }
    }) {
        let profile = resolve_workspace_profile(&workspace, profile_id)?;
        resolve_workspace_profile_target(&state, profile, "main").await?
    } else {
        let default_target = parse_target_selector(&workspace.main_target);
        resolve_session_target(
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
        .await?
    };
    let provider = resolve_provider(&selection.provider)?;
    let title = payload
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if !selection.profile_title.is_empty() {
                format!("{} session", selection.profile_title)
            } else if !selection.route_title.is_empty() {
                format!("{} session", selection.route_title)
            } else {
                default_session_title(provider)
            }
        });
    let route_title = selection.route_title.clone();

    state.store.create_session(SessionRecord {
        id: session_id.clone(),
        profile_id: selection.profile_id,
        profile_title: selection.profile_title,
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
        provider_base_url: selection.provider_base_url,
        provider_api_key: selection.provider_api_key,
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
    let workspace = if payload.profile_id.is_some() {
        Some(state.store.workspace()?)
    } else {
        None
    };
    let requested_profile_id = payload
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let route_id = payload.route_id.as_deref().map(str::trim);
    let provider = payload.provider.as_deref().map(str::trim);
    let next_target = if let Some(profile_id) = requested_profile_id {
        let workspace = workspace
            .as_ref()
            .ok_or_else(|| ApiError::internal_message("workspace state was not available"))?;
        let profile = resolve_workspace_profile(workspace, profile_id)?;
        Some(resolve_workspace_profile_target(&state, profile, "main").await?)
    } else if route_id.is_some_and(|value| !value.is_empty())
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
        profile_id: next_target
            .as_ref()
            .map(|selection| selection.profile_id.clone()),
        profile_title: next_target
            .as_ref()
            .map(|selection| selection.profile_title.clone()),
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
        provider_base_url: next_target
            .as_ref()
            .map(|selection| selection.provider_base_url.clone()),
        provider_api_key: next_target
            .as_ref()
            .map(|selection| selection.provider_api_key.clone()),
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

async fn session_jobs(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<JobSummary>>, ApiError> {
    Ok(Json(state.store.list_jobs_for_session(&session_id)?))
}

async fn job_detail(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobDetail>, ApiError> {
    Ok(Json(state.store.get_job(&job_id)?))
}

async fn cancel_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobDetail>, ApiError> {
    Ok(Json(agent::cancel_job(state, job_id).await?))
}

async fn resume_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobDetail>, ApiError> {
    Ok(Json(agent::resume_job(state, job_id).await?))
}

async fn pending_approvals(
    State(state): State<AppState>,
) -> Result<Json<Vec<ApprovalRequestSummary>>, ApiError> {
    Ok(Json(agent::list_pending_approvals(state).await?))
}

async fn approve_request(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
    body: Bytes,
) -> Result<Json<JobDetail>, ApiError> {
    let payload = if body.is_empty() {
        ApprovalResolutionRequest::default()
    } else {
        decode_json::<ApprovalResolutionRequest>(&body)?
    };
    Ok(Json(
        agent::approve_request(state, approval_id, payload.note).await?,
    ))
}

async fn deny_request(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
    body: Bytes,
) -> Result<Json<JobDetail>, ApiError> {
    let payload = if body.is_empty() {
        ApprovalResolutionRequest::default()
    } else {
        decode_json::<ApprovalResolutionRequest>(&body)?
    };
    Ok(Json(
        agent::deny_request(state, approval_id, payload.note).await?,
    ))
}

async fn prompt_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<SessionDetail>, ApiError> {
    let payload = decode_json::<SessionPromptRequest>(&body)?;
    let prompt = payload.prompt.trim();
    let execution_prompt = effective_prompt_text(prompt, payload.images.len());
    let compiler_role = normalize_compiler_role(&payload.role)?;

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

    Ok(Json(
        agent::start_prompt_job(
            state,
            session_id,
            payload,
            current,
            execution_prompt,
            compiler_role,
        )
        .await?,
    ))
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

async fn stream_socket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WebSocketAuthQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    match authorize_access(&state, &headers, query.token.as_deref()) {
        Ok(()) => ws.on_upgrade(move |socket| handle_stream_socket(socket, state)),
        Err(error) => {
            let reason = error.message.clone();
            ws.on_upgrade(move |socket| handle_unauthorized_stream_socket(socket, reason))
        }
    }
}

async fn handle_stream_socket(mut socket: WebSocket, state: AppState) {
    if let Err(error) = send_event(
        &mut socket,
        DaemonEvent::Connected(StreamConnected {
            service: PRODUCT_NAME.to_string(),
            version: state.version.clone(),
            compatibility: build_compatibility_summary(
                &state.version,
                &state.updates.instance_summary(),
            ),
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

async fn handle_unauthorized_stream_socket(mut socket: WebSocket, reason: String) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 4401,
            reason: reason.into(),
        })))
        .await;
    let _ = socket.close().await;
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
    if !state.updates.auto_check_enabled() {
        return;
    }

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

fn schedule_daemon_restart(state: AppState) {
    tokio::spawn(async move {
        time::sleep(RESTART_AFTER_RESPONSE_DELAY).await;

        if let Err(error) = state.updates.perform_restart().await {
            warn!(error = %error, "daemon restart request failed");
            let result = state.updates.mark_restart_failure(error.to_string()).await;
            if result.changed {
                let _ = publish_update_event(&state, result.status).await;
            }
        }
    });
}

async fn build_settings_summary(state: &AppState) -> SettingsSummary {
    let instance = state.updates.instance_summary();
    let hostname = state.host.host_status().hostname;

    SettingsSummary {
        product: PRODUCT_NAME.to_string(),
        version: state.version.clone(),
        instance: instance.clone(),
        storage: state.store.storage_summary(),
        auth: AuthSummary {
            enabled: true,
            token_path: state.store.local_auth_token_path(),
        },
        connection: build_connection_summary(
            &instance.daemon_bind,
            &hostname,
            state.tailscale_dns_name.as_deref(),
            state.web_dist_dir.as_ref(),
        ),
        compatibility: build_compatibility_summary(
            &state.version,
            &state.updates.instance_summary(),
        ),
        update: state.updates.current().await,
    }
}

fn build_compatibility_summary(
    version: &str,
    instance: &nucleus_protocol::InstanceSummary,
) -> CompatibilitySummary {
    let mut capability_flags = BTreeSet::from([
        "daemon-owned-update-state".to_string(),
        "embedded-web-build".to_string(),
        "install-kind-contract".to_string(),
    ]);
    let mut minimum_client_version = Some(version.to_string());
    let mut minimum_server_version = Some(version.to_string());

    if instance.install_kind == nucleus_release::INSTALL_KIND_MANAGED_RELEASE {
        if let Ok(install_root) = env::var("NUCLEUS_INSTALL_ROOT") {
            if let Ok(Some(metadata)) =
                read_installed_release_metadata(&PathBuf::from(install_root))
            {
                minimum_client_version = metadata
                    .minimum_client_version
                    .or_else(|| Some(version.to_string()));
                minimum_server_version = metadata
                    .minimum_server_version
                    .or_else(|| Some(version.to_string()));
                capability_flags.extend(metadata.capability_flags);
            }
        }
    }

    CompatibilitySummary {
        server_version: version.to_string(),
        minimum_client_version,
        minimum_server_version,
        surface_version: "2026-05-managed-release-v1".to_string(),
        capability_flags: capability_flags.into_iter().collect(),
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
    if matches!(
        AdapterKind::parse(provider),
        Some(AdapterKind::OpenAiCompatible)
    ) {
        return Ok(());
    }

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

fn normalize_compiler_role(value: &str) -> Result<String, ApiError> {
    match value.trim() {
        "" | "main" => Ok("main".to_string()),
        "utility" => Ok("utility".to_string()),
        other => Err(ApiError::bad_request(format!(
            "unsupported compiler role '{other}'"
        ))),
    }
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

fn sanitize_workspace_profile_patch(
    payload: &WorkspaceProfileWriteRequest,
) -> Result<WorkspaceProfilePatch, ApiError> {
    let title = payload.title.trim();
    if title.is_empty() {
        return Err(ApiError::bad_request("profile title is required"));
    }

    Ok(WorkspaceProfilePatch {
        title: title.to_string(),
        main: sanitize_workspace_model_config(&payload.main, "main")?,
        utility: sanitize_workspace_model_config(&payload.utility, "utility")?,
        is_default: payload.is_default.unwrap_or(false),
    })
}

fn sanitize_workspace_model_config(
    config: &WorkspaceModelConfig,
    role: &str,
) -> Result<WorkspaceModelConfig, ApiError> {
    let adapter = resolve_provider(config.adapter.trim())?;
    if !adapter.supports_prompting() {
        return Err(ApiError::bad_request(format!(
            "{role} model adapter '{}' cannot prompt sessions",
            adapter.as_str()
        )));
    }

    let mut model = config.model.trim().to_string();
    let mut base_url = config.base_url.trim().trim_end_matches('/').to_string();
    let api_key = config.api_key.trim().to_string();

    match adapter {
        AdapterKind::Claude => {
            if model.is_empty() {
                model = "sonnet".to_string();
            }
            base_url.clear();
        }
        AdapterKind::Codex => {
            base_url.clear();
        }
        AdapterKind::OpenAiCompatible => {
            if base_url.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "{role} model base URL is required for OpenAI-compatible adapters",
                )));
            }
            if model.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "{role} model name is required for OpenAI-compatible adapters",
                )));
            }
        }
        AdapterKind::System => {
            return Err(ApiError::bad_request(format!(
                "{role} model cannot use the system adapter",
            )));
        }
    }

    Ok(WorkspaceModelConfig {
        adapter: adapter.as_str().to_string(),
        model,
        base_url,
        api_key,
    })
}

fn sanitize_skill_manifest(mut manifest: SkillManifest) -> Result<SkillManifest, ApiError> {
    manifest.id = sanitize_registry_id(&manifest.id, "skill id")?;
    manifest.title = required_trimmed(manifest.title, "skill title")?;
    manifest.description = manifest.description.trim().to_string();
    manifest.activation_mode = match manifest.activation_mode.trim() {
        "always" | "auto" | "manual" => manifest.activation_mode.trim().to_string(),
        _ => {
            return Err(ApiError::bad_request(
                "skill activation_mode must be always, auto, or manual",
            ));
        }
    };
    manifest.triggers = sanitize_string_list(manifest.triggers);
    manifest.include_paths = sanitize_string_list(manifest.include_paths);
    manifest.required_tools = sanitize_string_list(manifest.required_tools);
    manifest.required_mcps = sanitize_string_list(manifest.required_mcps);
    manifest.project_filters = sanitize_string_list(manifest.project_filters);
    Ok(manifest)
}

fn sanitize_mcp_server(mut server: McpServerSummary) -> Result<McpServerSummary, ApiError> {
    server.id = sanitize_registry_id(&server.id, "MCP server id")?;
    server.title = required_trimmed(server.title, "MCP server title")?;
    server.resources = sanitize_string_list(server.resources);
    server.tools = server
        .tools
        .into_iter()
        .map(sanitize_tool_descriptor)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(server)
}

fn sanitize_tool_descriptor(
    mut tool: NucleusToolDescriptor,
) -> Result<NucleusToolDescriptor, ApiError> {
    tool.id = sanitize_registry_id(&tool.id, "tool id")?;
    tool.title = required_trimmed(tool.title, "tool title")?;
    tool.description = tool.description.trim().to_string();
    tool.source = tool.source.trim().to_string();
    Ok(tool)
}

fn required_trimmed(value: String, label: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::bad_request(format!("{label} is required")));
    }
    Ok(value.to_string())
}

fn sanitize_registry_id(value: &str, label: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::bad_request(format!("{label} is required")));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(ApiError::bad_request(format!(
            "{label} may only contain ASCII letters, numbers, '.', '_', or '-'",
        )));
    }
    Ok(value.to_string())
}

fn sanitize_string_list(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

#[derive(Debug, Clone)]
struct SessionTargetSelection {
    profile_id: String,
    profile_title: String,
    route_id: String,
    route_title: String,
    provider: String,
    model: String,
    provider_base_url: String,
    provider_api_key: String,
}

#[derive(Debug, Clone)]
struct PromptTarget {
    provider: String,
    model: String,
    provider_base_url: String,
    provider_api_key: String,
    runtime_ready: bool,
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
struct PromptIncludeSource {
    scope: &'static str,
    path: PathBuf,
    content: String,
}

#[derive(Debug, Clone)]
struct PromptAssembly {
    prompt: String,
}

fn resolve_workspace_profile<'a>(
    workspace: &'a WorkspaceSummary,
    profile_id: &str,
) -> Result<&'a WorkspaceProfileSummary, ApiError> {
    workspace
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| ApiError::bad_request(format!("unknown workspace profile '{profile_id}'")))
}

async fn resolve_workspace_profile_target(
    state: &AppState,
    profile: &WorkspaceProfileSummary,
    compiler_role: &str,
) -> Result<SessionTargetSelection, ApiError> {
    let compiler_role = normalize_compiler_role(compiler_role)?;
    let config = match compiler_role.as_str() {
        "utility" => &profile.utility,
        _ => &profile.main,
    };
    let provider = resolve_provider(&config.adapter)?;
    ensure_prompting_runtime(state, provider.as_str(), false).await?;

    let model = if config.model.trim().is_empty() {
        provider.default_model().to_string()
    } else {
        config.model.trim().to_string()
    };

    Ok(SessionTargetSelection {
        profile_id: profile.id.clone(),
        profile_title: profile.title.clone(),
        route_id: String::new(),
        route_title: String::new(),
        provider: provider.as_str().to_string(),
        model,
        provider_base_url: config.base_url.trim().to_string(),
        provider_api_key: config.api_key.trim().to_string(),
    })
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
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: route.id,
            route_title: route.title,
            provider: target.provider,
            model: if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
                model.to_string()
            } else {
                target.model
            },
            provider_base_url: target.provider_base_url,
            provider_api_key: target.provider_api_key,
        });
    }

    let provider = provider
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("either route_id or provider is required"))?;
    let provider = resolve_provider(provider)?;

    if !provider.supports_sessions() {
        return Err(ApiError::bad_request(format!(
            "provider '{}' cannot create Nucleus-managed sessions yet",
            provider.as_str()
        )));
    }

    ensure_prompting_runtime(state, provider.as_str(), false).await?;

    let provider_base_url = if provider == AdapterKind::OpenAiCompatible {
        DEFAULT_OPENAI_COMPATIBLE_BASE_URL.to_string()
    } else {
        String::new()
    };

    Ok(SessionTargetSelection {
        profile_id: String::new(),
        profile_title: String::new(),
        route_id: String::new(),
        route_title: String::new(),
        provider: provider.as_str().to_string(),
        model: model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| provider.default_model().to_string()),
        provider_base_url,
        provider_api_key: String::new(),
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

        match runtime {
            Some(runtime) if runtime.supports_prompting && runtime.state == "ready" => {
                ready.push(PromptTarget {
                    provider: target.provider.clone(),
                    model: target.model.clone(),
                    provider_base_url: target.base_url.trim().to_string(),
                    provider_api_key: target.api_key.trim().to_string(),
                    runtime_ready: true,
                })
            }
            Some(runtime) if runtime.supports_prompting => pending.push(PromptTarget {
                provider: target.provider.clone(),
                model: target.model.clone(),
                provider_base_url: target.base_url.trim().to_string(),
                provider_api_key: target.api_key.trim().to_string(),
                runtime_ready: false,
            }),
            _ => {}
        }
    }

    ready.extend(pending);
    Ok(ready)
}

fn assemble_prompt_input(
    state: &AppState,
    session: &SessionSummary,
    prompt: &str,
) -> Result<PromptAssembly, ApiError> {
    let sources = discover_prompt_sources(state, session, prompt)?;

    if sources.is_empty() {
        return Ok(PromptAssembly {
            prompt: prompt.to_string(),
        });
    }

    Ok(PromptAssembly {
        prompt: render_prompt_with_sources(prompt, &sources),
    })
}

fn discover_prompt_sources(
    state: &AppState,
    session: &SessionSummary,
    prompt: &str,
) -> Result<Vec<PromptIncludeSource>, ApiError> {
    let workspace = state.store.workspace()?;
    let mut roots = Vec::new();

    if let Some(home_dir) = dirs::home_dir() {
        roots.push(("global", home_dir));
    }

    let workspace_root = PathBuf::from(&workspace.root_path);
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

    collect_skill_prompt_sources(
        state,
        &workspace,
        session,
        prompt,
        &mut seen_files,
        &mut sources,
        &mut total_chars,
    )?;

    Ok(sources)
}

fn collect_skill_prompt_sources(
    state: &AppState,
    workspace: &WorkspaceSummary,
    session: &SessionSummary,
    prompt: &str,
    seen_files: &mut BTreeSet<PathBuf>,
    sources: &mut Vec<PromptIncludeSource>,
    total_chars: &mut usize,
) -> Result<(), ApiError> {
    if sources.len() >= MAX_PROMPT_INCLUDE_FILES || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS {
        return Ok(());
    }

    let workspace_root = PathBuf::from(&workspace.root_path);
    if !workspace_root.is_dir() {
        return Ok(());
    }
    let workspace_root = fs::canonicalize(&workspace_root).map_err(|error| {
        ApiError::internal_message(format!(
            "failed to resolve workspace root '{}': {error}",
            workspace.root_path
        ))
    })?;

    for skill in state.store.list_skill_manifests()? {
        if !skill_is_active_for_prompt(&skill, session, prompt) {
            continue;
        }

        for include_path in &skill.include_paths {
            let Some(path) = resolve_skill_include_path(&workspace_root, include_path) else {
                continue;
            };

            push_prompt_source("skill", &path, seen_files, sources, total_chars)?;

            if sources.len() >= MAX_PROMPT_INCLUDE_FILES
                || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS
            {
                return Ok(());
            }
        }
    }

    Ok(())
}

fn skill_is_active_for_prompt(
    skill: &SkillManifest,
    session: &SessionSummary,
    prompt: &str,
) -> bool {
    if !skill.enabled || !skill_project_filter_matches(skill, session) {
        return false;
    }

    match skill.activation_mode.as_str() {
        "always" => true,
        "auto" => {
            let prompt = prompt.to_ascii_lowercase();
            skill
                .triggers
                .iter()
                .map(|trigger| trigger.trim().to_ascii_lowercase())
                .filter(|trigger| !trigger.is_empty())
                .any(|trigger| prompt.contains(&trigger))
        }
        _ => false,
    }
}

fn skill_project_filter_matches(skill: &SkillManifest, session: &SessionSummary) -> bool {
    if skill.project_filters.is_empty() {
        return true;
    }

    session.projects.iter().any(|project| {
        skill.project_filters.iter().any(|filter| {
            let filter = filter.trim();
            !filter.is_empty()
                && (filter == project.id
                    || filter == project.slug
                    || filter == project.relative_path
                    || filter == project.absolute_path
                    || filter == project.title)
        })
    })
}

fn resolve_skill_include_path(workspace_root: &PathBuf, include_path: &str) -> Option<PathBuf> {
    let include_path = include_path.trim();
    if include_path.is_empty() {
        return None;
    }

    let relative = PathBuf::from(include_path);
    if relative.is_absolute() {
        return None;
    }

    let path = workspace_root.join(relative);
    let canonical = fs::canonicalize(path).ok()?;
    if canonical.is_file() && canonical.starts_with(workspace_root) {
        Some(canonical)
    } else {
        None
    }
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

    // Shared repo context should land before local private overlays so the
    // public product truth stays visible and `.nucleus/include` can refine it.
    for include_dir in [root.join("include"), root.join("promptinclude")] {
        collect_include_directory_sources(scope, &include_dir, seen_files, sources, total_chars)?;

        if sources.len() >= MAX_PROMPT_INCLUDE_FILES
            || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS
        {
            return Ok(());
        }
    }

    collect_legacy_promptinclude_sources(scope, root, seen_files, sources, total_chars)?;

    if sources.len() >= MAX_PROMPT_INCLUDE_FILES || *total_chars >= MAX_PROMPT_INCLUDE_TOTAL_CHARS {
        return Ok(());
    }

    collect_include_directory_sources(
        scope,
        &root.join(".nucleus").join("include"),
        seen_files,
        sources,
        total_chars,
    )?;
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
        AdapterKind::OpenAiCompatible => "OpenAI session".to_string(),
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
            summary: "Send SIGTERM to a user-owned process by PID through Nucleus safety checks."
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

fn authorize_access(
    state: &AppState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), ApiError> {
    let token = bearer_token(headers)
        .or(query_token.map(str::trim).filter(|value| !value.is_empty()))
        .ok_or_else(|| {
            ApiError::unauthorized("Authentication required. Provide a bearer token.")
        })?;

    if state
        .store
        .validate_access_token(token)
        .map_err(ApiError::from)?
    {
        return Ok(());
    }

    Err(ApiError::unauthorized(
        "Authentication required. The provided bearer token is invalid.",
    ))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn static_web_service(web_dist_dir: &PathBuf) -> ServeDir<ServeFile> {
    let spa_fallback = if web_dist_dir.join("200.html").is_file() {
        web_dist_dir.join("200.html")
    } else {
        web_dist_dir.join("index.html")
    };

    ServeDir::new(web_dist_dir)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(spa_fallback))
}

fn resolve_web_dist_dir(instance: &InstanceRuntime) -> Option<PathBuf> {
    let mut candidates = if instance.install_kind == nucleus_release::INSTALL_KIND_MANAGED_RELEASE {
        vec![
            instance.managed_web_dist_dir.clone(),
            instance
                .install_root
                .as_ref()
                .map(|install_root| nucleus_release::current_release_web_dir(install_root)),
        ]
    } else {
        vec![
            env::var("NUCLEUS_WEB_DIST_DIR").ok().map(PathBuf::from),
            instance.managed_web_dist_dir.clone(),
        ]
    };

    if instance.install_kind == nucleus_release::INSTALL_KIND_DEV_CHECKOUT {
        candidates.push(Some(instance.repo_root.join("apps/web/build")));
    }

    candidates
        .into_iter()
        .flatten()
        .find(|candidate| candidate.join("index.html").is_file())
}

fn detect_tailscale_dns_name() -> Option<String> {
    let output = StdCommand::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let payload: Value = serde_json::from_slice(&output.stdout).ok()?;
    let dns_name = payload
        .get("Self")
        .and_then(|value| value.get("DNSName"))
        .and_then(Value::as_str)
        .map(|value| value.trim_end_matches('.').to_string())?;

    if dns_name.is_empty() {
        return None;
    }

    Some(dns_name)
}

fn build_connection_summary(
    bind: &str,
    hostname: &str,
    tailscale_dns_name: Option<&str>,
    web_dist_dir: Option<&PathBuf>,
) -> ConnectionSummary {
    let port = bind_port(bind).unwrap_or(80);
    let local_url = format!("http://127.0.0.1:{port}");
    let hostname_url = if bind_exposes_remote_access(bind) {
        Some(format!("http://{hostname}:{port}"))
    } else {
        None
    };
    let tailscale_url = if bind_exposes_remote_access(bind) {
        tailscale_dns_name.map(|value| format!("http://{value}:{port}"))
    } else {
        None
    };

    ConnectionSummary {
        local_url,
        hostname_url,
        tailscale_url,
        web_mode: if web_dist_dir.is_some() {
            "embedded_static".to_string()
        } else {
            "api_only".to_string()
        },
        web_root: web_dist_dir.map(|path| path.display().to_string()),
    }
}

fn bind_port(bind: &str) -> Option<u16> {
    bind.parse::<SocketAddr>()
        .ok()
        .map(|addr| addr.port())
        .or_else(|| {
            bind.rsplit_once(':')
                .and_then(|(_, port)| port.parse::<u16>().ok())
        })
}

fn bind_exposes_remote_access(bind: &str) -> bool {
    if bind.starts_with("127.0.0.1:") || bind.starts_with("localhost:") {
        return false;
    }

    match bind.parse::<SocketAddr>() {
        Ok(addr) => match addr.ip() {
            IpAddr::V4(ip) => !ip.is_loopback(),
            IpAddr::V6(ip) => !ip.is_loopback(),
        },
        Err(_) => true,
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

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
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
        time::{SystemTime, UNIX_EPOCH},
    };

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
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            scope: "project".to_string(),
            project_id: "project-1".to_string(),
            project_title: "Project One".to_string(),
            project_path: "/home/eba/dev-projects/project-one".to_string(),
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
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
        assert_eq!(sources[0].path, generic);
        assert_eq!(sources[1].path, legacy);
        assert_eq!(sources[2].path, namespaced.join("rules.md"));
        let rendered = render_prompt_with_sources("Ship it.", &sources);
        assert!(rendered.contains("Always do the right thing."));
        assert!(rendered.contains("Project-specific reminder."));
        assert!(rendered.contains("User request:\nShip it."));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn active_skills_contribute_workspace_include_files() {
        let state_dir = test_state_dir("skill-includes");
        let store = initialize_test_store(&state_dir);
        let workspace_root = state_dir.join("workspace");
        let skill_dir = workspace_root.join("skills");
        fs::create_dir_all(&skill_dir).expect("skill include directory should exist");
        fs::write(
            skill_dir.join("rust.md"),
            "# Rust Skill\nPrefer small focused patches.\n",
        )
        .expect("skill include should write");
        store
            .upsert_skill_manifest(&SkillManifest {
                id: "rust".to_string(),
                title: "Rust".to_string(),
                description: "Rust conventions".to_string(),
                activation_mode: "auto".to_string(),
                triggers: vec!["cargo".to_string()],
                include_paths: vec!["skills/rust.md".to_string()],
                required_tools: Vec::new(),
                required_mcps: Vec::new(),
                project_filters: Vec::new(),
                enabled: true,
            })
            .expect("skill manifest should persist");

        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            agent: Arc::new(agent::AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };
        let session = SessionSummary {
            id: "session-skill".to_string(),
            title: "Skill prompt".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            provider: "openai_compatible".to_string(),
            model: "gpt-5.4-mini".to_string(),
            provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
            provider_api_key: String::new(),
            working_dir: workspace_root.display().to_string(),
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

        let assembly = assemble_prompt_input(&state, &session, "Run cargo test.")
            .expect("skill includes should assemble");

        assert!(assembly.prompt.contains("Prefer small focused patches."));
        assert!(assembly.prompt.contains("[skill include:"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn route_targets_preserve_openai_transport_config_when_creating_sessions() {
        let state_dir = test_state_dir("session-target-cache");
        let store = initialize_test_store(&state_dir);
        let (events, _) = broadcast::channel(4);

        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            agent: Arc::new(agent::AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let profiles = vec![RouterProfileSummary {
            id: "gateway-route".to_string(),
            title: "Gateway Route".to_string(),
            summary: "Test OpenAI-compatible route".to_string(),
            enabled: true,
            state: "ready".to_string(),
            targets: vec![nucleus_protocol::RouteTarget {
                provider: "openai_compatible".to_string(),
                model: "route-model".to_string(),
                base_url: "http://127.0.0.1:20128/v1".to_string(),
                api_key: "nuctk_route".to_string(),
            }],
        }];

        let result = resolve_session_target(&state, &profiles, Some("gateway-route"), None, None)
            .await
            .expect("OpenAI-compatible route should resolve");

        assert_eq!(result.route_id, "gateway-route");
        assert_eq!(result.provider, "openai_compatible");
        assert_eq!(result.model, "route-model");
        assert_eq!(result.provider_base_url, "http://127.0.0.1:20128/v1");
        assert_eq!(result.provider_api_key, "nuctk_route");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn creates_sessions_from_fresh_default_protocol_profile() {
        let state_dir = test_state_dir("create-session-cache");
        let store = initialize_test_store(&state_dir);

        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            agent: Arc::new(agent::AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let detail = create_session(State(state), Bytes::from_static(b"{}"))
            .await
            .expect("fresh default profile should create a protocol session");
        assert_eq!(detail.0.session.provider, "openai_compatible");
        assert_eq!(detail.0.session.model, "gpt-5.4-mini");
        assert_eq!(
            detail.0.session.provider_base_url,
            "http://127.0.0.1:20128/v1"
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn creates_sessions_from_default_workspace_profile() {
        let state_dir = test_state_dir("create-session-default-profile");
        let store = initialize_test_store(&state_dir);

        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            agent: Arc::new(agent::AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let detail = create_session(State(state), Bytes::from_static(b"{}"))
            .await
            .expect("default workspace profile should allow session creation");
        assert_eq!(detail.0.session.profile_id, "default");
        assert_eq!(detail.0.session.profile_title, "Default");
        assert_eq!(detail.0.session.provider, "openai_compatible");
        assert_eq!(detail.0.session.model, "gpt-5.4-mini");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn creates_sessions_from_openai_compatible_workspace_profiles() {
        let state_dir = test_state_dir("create-session-openai-profile");
        let store = initialize_test_store(&state_dir);
        store
            .create_workspace_profile(nucleus_storage::WorkspaceProfilePatch {
                title: "Gateway".to_string(),
                main: WorkspaceModelConfig {
                    adapter: "openai_compatible".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                    base_url: "http://127.0.0.1:20128/v1".to_string(),
                    api_key: "nuctk_test".to_string(),
                },
                utility: WorkspaceModelConfig {
                    adapter: "openai_compatible".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                    base_url: "http://127.0.0.1:20129/v1".to_string(),
                    api_key: String::new(),
                },
                is_default: false,
            })
            .expect("workspace profile should create");

        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            agent: Arc::new(agent::AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let result = create_session(
            State(state.clone()),
            Bytes::from(
                serde_json::to_vec(&CreateSessionRequest {
                    profile_id: Some("gateway".to_string()),
                    route_id: None,
                    provider: None,
                    title: None,
                    model: None,
                    project_id: None,
                    primary_project_id: None,
                    project_ids: None,
                })
                .expect("session payload should serialize"),
            ),
        )
        .await
        .expect("OpenAI-compatible profile should allow session creation");

        assert_eq!(result.0.session.profile_id, "gateway");
        assert_eq!(result.0.session.provider, "openai_compatible");
        assert_eq!(result.0.session.model, "gpt-4.1-mini");
        assert_eq!(
            result.0.session.provider_base_url,
            "http://127.0.0.1:20128/v1"
        );
        assert_eq!(result.0.session.provider_api_key, "nuctk_test");

        let workspace = state.store.workspace().expect("workspace should load");
        let profile =
            resolve_workspace_profile(&workspace, "gateway").expect("gateway profile should load");
        let utility_target = resolve_workspace_profile_target(&state, profile, "utility")
            .await
            .expect("utility role should resolve from the profile utility config");
        assert_eq!(utility_target.provider, "openai_compatible");
        assert_eq!(utility_target.model, "gpt-4.1-mini");
        assert_eq!(
            utility_target.provider_base_url,
            "http://127.0.0.1:20129/v1"
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn resolves_direct_prompt_targets_without_cli_runtime_cache() {
        let state_dir = test_state_dir("prompt-target-direct-cache");
        let store = initialize_test_store(&state_dir);
        let (events, _) = broadcast::channel(4);

        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            agent: Arc::new(agent::AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let session = SessionSummary {
            id: "session-direct".to_string(),
            title: "Direct prompt".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            provider: "openai_compatible".to_string(),
            model: "direct-model".to_string(),
            provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
            provider_api_key: String::new(),
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

        let result = resolve_session_target(
            &state,
            &[],
            None,
            Some(&session.provider),
            Some(&session.model),
        )
        .await;

        let target =
            result.expect("OpenAI-compatible runtime should satisfy direct prompt routing");
        assert_eq!(target.provider, "openai_compatible");
        assert_eq!(target.model, "direct-model");
        assert_eq!(target.provider_base_url, "http://127.0.0.1:20128/v1");

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn resolves_route_prompt_targets_with_transport_config() {
        let state_dir = test_state_dir("prompt-target-route-cache");
        let store = initialize_test_store(&state_dir);
        let (events, _) = broadcast::channel(4);

        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            agent: Arc::new(agent::AgentRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let session = SessionSummary {
            id: "session-route".to_string(),
            title: "Route prompt".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: "balanced".to_string(),
            route_title: "Balanced".to_string(),
            scope: "ad_hoc".to_string(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: String::new(),
            provider: "openai_compatible".to_string(),
            model: "gpt-5.4-mini".to_string(),
            provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
            provider_api_key: String::new(),
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

        let route_profiles = load_router_profiles(&state, false)
            .await
            .expect("router profiles should load");
        let result =
            resolve_session_target(&state, &route_profiles, Some(&session.route_id), None, None)
                .await;

        let target = result.expect("OpenAI-compatible route should satisfy prompt routing");
        assert_eq!(target.provider, "openai_compatible");
        assert_eq!(target.provider_base_url, "http://127.0.0.1:20128/v1");

        let _ = fs::remove_dir_all(&state_dir);
    }

    fn test_state_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("nucleus-{label}-{}-{suffix}", std::process::id()))
    }

    fn initialize_test_store(state_dir: &std::path::Path) -> Arc<StateStore> {
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
        store
    }

    fn test_instance_runtime() -> InstanceRuntime {
        InstanceRuntime::for_test(
            "Test",
            env::current_dir().expect("cwd should resolve"),
            "127.0.0.1:42240",
            "managed_release",
        )
    }
}
