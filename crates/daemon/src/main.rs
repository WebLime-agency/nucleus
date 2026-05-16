mod agent;
mod browser;
mod host;
mod runtime;
mod security;
mod updates;
mod vault;
mod worker_action;

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Path as FsPath, PathBuf},
    process::Command as StdCommand,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
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
use base64::Engine as _;
use flate2::read::GzDecoder;
use futures_util::SinkExt;
use host::{DEFAULT_PROCESS_LIMIT, HostEngine, ProcessSort, resolve_process_limit};
use nucleus_core::{
    AdapterKind, DEFAULT_DAEMON_ADDR, DEFAULT_OPENAI_COMPATIBLE_BASE_URL, PRODUCT_NAME,
    product_banner,
};
use nucleus_protocol::{
    ActionParameter, ActionRunRequest, ActionRunResponse, ActionSummary, ApprovalRequestSummary,
    ApprovalResolutionRequest, AuditEvent, AuthSummary, BrowserActionRequest,
    BrowserContextSummary, BrowserNavigateRequest, BrowserSnapshot, CompatibilitySummary,
    CompiledPromptLayer, CompiledTurn, ConnectionSummary, CreatePlaybookRequest,
    CreateSessionRequest, DaemonEvent, HealthResponse, HostStatus, InstanceLogCategoriesResponse,
    InstanceLogListResponse, JobDetail, JobSummary, MAX_CONFIGURED_JOB_STEPS,
    MAX_CONFIGURED_JOB_TOOL_CALLS, MAX_CONFIGURED_JOB_WALL_CLOCK_SECS, McpServerRecord,
    McpServerSummary, McpToolRecord, MemoryCandidate, MemoryCandidateAcceptRequest,
    MemoryCandidateListResponse, MemoryCandidateUpsertRequest, MemoryEntry,
    MemoryEntryUpsertRequest, MemorySearchResponse, MemorySummary, NucleusToolDescriptor,
    PlaybookDetail, PlaybookSummary, ProcessKillRequest, ProcessKillResponse, ProcessListResponse,
    ProcessStreamUpdate, ProjectUpdateRequest, PromptProgressUpdate, RouterProfileSummary,
    RunBudgetSummary, RuntimeOverview, RuntimeSummary, SessionDetail, SessionPromptRequest,
    SessionSummary, SettingsSummary, SkillImportRequest, SkillImportResponse, SkillInstallResult,
    SkillInstallVerification, SkillInstallationRecord, SkillInstallationUpsertRequest,
    SkillManifest, SkillPackageRecord, SkillPackageUpsertRequest, SkillReconcileCandidate,
    SkillReconcileRequest, SkillReconcileScanResponse, StreamConnected, SystemStats,
    UpdateConfigRequest, UpdatePlaybookRequest, UpdateSessionRequest, UpdateStatus,
    VaultInitRequest, VaultSecretListResponse, VaultSecretPolicyListResponse,
    VaultSecretPolicySummary, VaultSecretPolicyUpsertRequest, VaultSecretSummary,
    VaultSecretUpdateRequest, VaultSecretUpsertRequest, VaultStatusSummary, VaultUnlockRequest,
    WorkspaceModelConfig, WorkspaceProfileSummary, WorkspaceProfileWriteRequest, WorkspaceSummary,
    WorkspaceUpdateRequest,
};
use nucleus_release::read_installed_release_metadata;
use nucleus_storage::{
    AuditEventRecord, INSTANCE_LOG_MAX_ROWS, INSTANCE_LOG_RETENTION_DAYS, InstanceLogRecord,
    ProjectPatch, SessionPatch, SessionRecord, StateStore, VaultSecretPolicyRecord,
    VaultSecretRecord, WorkspaceProfilePatch,
};
use runtime::RuntimeManager;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
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
const DEFAULT_LOG_LIMIT: usize = 100;
const MAX_LOG_LIMIT: usize = 500;
const INSTANCE_LOG_MESSAGE_LIMIT: usize = 500;
const INSTANCE_LOG_METADATA_STRING_LIMIT: usize = 240;
const INSTANCE_LOG_METADATA_KEYS_LIMIT: usize = 24;
const INSTANCE_LOG_REDACTED: &str = "[REDACTED_SECRET]";
const MAX_PROMPT_INCLUDE_FILES: usize = 24;
const MAX_PROMPT_INCLUDE_FILE_CHARS: usize = 6_000;
const MAX_PROMPT_INCLUDE_TOTAL_CHARS: usize = 24_000;
const MAX_MEMORY_CONTEXT_CHARS: usize = 12_000;
const MAX_MEMORY_SEARCH_RESULTS: usize = 50;
const SESSION_MEMORY_SEARCH_CANDIDATE_LIMIT: usize = 50;
const MEMORY_TRUNCATION_NOTICE: &str = "\n[Memory entry truncated by Nucleus context budget]";
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
    vault: Arc<tokio::sync::Mutex<vault::VaultRuntime>>,
    agent: Arc<agent::AgentRuntime>,
    browser: Arc<browser::BrowserRuntime>,
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
        vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
        agent: Arc::new(agent::AgentRuntime::default()),
        browser: Arc::new(browser::BrowserRuntime::default()),
        web_dist_dir,
        tailscale_dns_name: detect_tailscale_dns_name(),
        events,
    };
    let _ = record_instance_log(
        &state,
        "info",
        "system",
        "daemon",
        "daemon.started",
        format!("Nucleus daemon started on {bind}."),
        json!({}),
        json!({
            "install_kind": state.updates.instance_summary().install_kind,
            "version": state.version,
        }),
    )
    .await;
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
        .route("/skills/import", axum::routing::post(import_skills))
        .route("/skills/reconcile", axum::routing::post(reconcile_skills))
        .route("/skills/reconcile/scan", get(scan_reconcile_skills))
        .route(
            "/skills/check-updates",
            axum::routing::post(check_skill_updates),
        )
        .route(
            "/skills/{skill_id}",
            axum::routing::put(upsert_skill_by_id).delete(delete_skill),
        )
        .route(
            "/skills/{skill_id}/check-update",
            axum::routing::post(check_skill_update),
        )
        .route(
            "/skill-packages",
            get(list_skill_packages).post(upsert_skill_package),
        )
        .route(
            "/skill-packages/{package_id}",
            axum::routing::put(upsert_skill_package_by_id),
        )
        .route(
            "/skill-installations",
            get(list_skill_installations).post(upsert_skill_installation),
        )
        .route(
            "/skill-installations/{installation_id}",
            axum::routing::put(upsert_skill_installation_by_id),
        )
        .route("/mcps", get(list_mcp_servers).post(upsert_mcp_server))
        .route(
            "/mcps/{server_id}",
            axum::routing::put(upsert_mcp_server_by_id).delete(delete_mcp_server),
        )
        .route(
            "/mcps/{server_id}/discover",
            axum::routing::post(discover_mcp_server_tools),
        )
        .route(
            "/mcps/{server_id}/tools/{tool_name}/call",
            axum::routing::post(call_mcp_server_tool),
        )
        .route("/memory", get(list_memory).post(upsert_memory))
        .route(
            "/memory/candidates",
            get(list_memory_candidates).post(upsert_memory_candidate),
        )
        .route(
            "/memory/candidates/{candidate_id}",
            axum::routing::put(upsert_memory_candidate_by_id)
                .patch(upsert_memory_candidate_by_id)
                .delete(delete_memory_candidate),
        )
        .route(
            "/memory/candidates/{candidate_id}/accept",
            axum::routing::post(accept_memory_candidate),
        )
        .route(
            "/memory/candidates/{candidate_id}/reject",
            axum::routing::post(reject_memory_candidate),
        )
        .route("/memory/remember", axum::routing::post(explicit_remember))
        .route("/memory/search", get(search_memory))
        .route(
            "/memory/{memory_id}",
            axum::routing::put(upsert_memory_by_id).delete(delete_memory),
        )
        .route("/vault/status", get(vault_status))
        .route("/vault/init", axum::routing::post(vault_init))
        .route("/vault/unlock", axum::routing::post(vault_unlock))
        .route("/vault/lock", axum::routing::post(vault_lock))
        .route(
            "/vault/secrets",
            get(list_vault_secrets).post(create_vault_secret),
        )
        .route(
            "/vault/secrets/{secret_id}",
            axum::routing::patch(update_vault_secret).delete(delete_vault_secret),
        )
        .route(
            "/vault/secrets/{secret_id}/policies",
            get(list_vault_secret_policies).post(upsert_vault_secret_policy),
        )
        .route(
            "/vault/secrets/{secret_id}/policies/{policy_id}",
            axum::routing::delete(delete_vault_secret_policy),
        )
        .route("/actions", get(actions))
        .route("/actions/{action_id}", get(action_detail))
        .route("/actions/{action_id}/run", axum::routing::post(run_action))
        .route("/audit", get(audit_events))
        .route("/logs", get(instance_logs))
        .route("/logs/categories", get(instance_log_categories))
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
        .route("/sessions/{session_id}/browser", get(browser_context))
        .route(
            "/sessions/{session_id}/browser/navigate",
            axum::routing::post(browser_navigate),
        )
        .route(
            "/sessions/{session_id}/browser/open",
            axum::routing::post(browser_open_tab),
        )
        .route(
            "/sessions/{session_id}/browser/select",
            axum::routing::post(browser_select_page),
        )
        .route(
            "/sessions/{session_id}/browser/command",
            axum::routing::post(browser_command),
        )
        .route(
            "/sessions/{session_id}/browser/annotation",
            axum::routing::post(browser_annotation),
        )
        .route(
            "/sessions/{session_id}/browser/snapshot",
            axum::routing::post(browser_snapshot),
        )
        .route(
            "/sessions/{session_id}/browser/action",
            axum::routing::post(browser_action),
        )
        .route(
            "/sessions/{session_id}/browser/stream/start",
            axum::routing::post(browser_stream_start),
        )
        .route(
            "/sessions/{session_id}/browser/stream/stop",
            axum::routing::post(browser_stream_stop),
        )
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

async fn browser_context(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<BrowserContextSummary>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    Ok(Json(state.browser.context(&session_id).await))
}

async fn browser_navigate(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<BrowserContextSummary>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    let payload = decode_json::<BrowserNavigateRequest>(&body)?;
    Ok(Json(state.browser.navigate(&session_id, payload).await?))
}

async fn browser_open_tab(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<BrowserContextSummary>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    Ok(Json(state.browser.open_tab(&session_id).await?))
}

async fn browser_select_page(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<BrowserContextSummary>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    #[derive(Deserialize)]
    struct SelectRequest {
        page_id: String,
    }
    let payload = decode_json::<SelectRequest>(&body)?;
    Ok(Json(
        state
            .browser
            .select_page(&session_id, &payload.page_id)
            .await?,
    ))
}

async fn browser_command(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<BrowserContextSummary>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    #[derive(Deserialize)]
    struct CommandRequest {
        page_id: String,
        command: String,
        #[serde(default)]
        args: serde_json::Value,
    }
    let payload = decode_json::<CommandRequest>(&body)?;
    Ok(Json(
        state
            .browser
            .command(
                &session_id,
                &payload.page_id,
                &payload.command,
                payload.args,
            )
            .await?,
    ))
}

async fn browser_annotation(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    #[derive(Deserialize)]
    struct AnnotationRequest {
        page_id: String,
        #[serde(default)]
        payload: serde_json::Value,
    }
    let payload = decode_json::<AnnotationRequest>(&body)?;
    let annotation = state
        .browser
        .annotation(&session_id, &payload.page_id, payload.payload.clone())
        .await?;
    let snapshot = state
        .browser
        .snapshot(&session_id, Some(payload.page_id.clone()))
        .await?;
    let artifact_paths = persist_browser_session_artifacts(
        &state,
        &session_id,
        "browser-annotation",
        &snapshot,
        Some(&annotation),
    )?;
    if let Some(comment) = annotation
        .get("comment")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let turn_id = Uuid::new_v4().to_string();
        let annotation_metadata = annotation.clone();
        state.store.append_session_turn(
            &session_id,
            &turn_id,
            "user",
            &format!(
                "Browser annotation on {} ({})\n\nComment: {}\n\nAnnotation metadata:\n{}",
                snapshot.title,
                snapshot.url,
                comment,
                serde_json::to_string_pretty(&annotation_metadata)
                    .unwrap_or_else(|_| annotation_metadata.to_string())
            ),
            &[],
        )?;
        if let Ok(updated) = state.store.get_session(&session_id) {
            let _ = publish_session_event(&state, updated).await;
        }
    }
    let mut response = annotation;
    if let Some(object) = response.as_object_mut() {
        object.insert("artifact_paths".to_string(), json!(artifact_paths));
        object.insert("screenshot_captured".to_string(), json!(true));
    }
    Ok(Json(response))
}

async fn browser_snapshot(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<BrowserSnapshot>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    #[derive(Deserialize)]
    struct SnapshotRequest {
        page_id: Option<String>,
    }
    let payload = if body.is_empty() {
        SnapshotRequest { page_id: None }
    } else {
        decode_json::<SnapshotRequest>(&body)?
    };
    let snapshot = state.browser.snapshot(&session_id, payload.page_id).await?;
    let _ = persist_browser_session_artifacts(
        &state,
        &session_id,
        "browser-snapshot",
        &snapshot,
        None,
    )?;
    Ok(Json(snapshot))
}

async fn browser_action(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<BrowserSnapshot>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    let payload = decode_json::<BrowserActionRequest>(&body)?;
    Ok(Json(state.browser.action(&session_id, payload).await?))
}

async fn browser_stream_start(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<BrowserContextSummary>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    #[derive(Deserialize)]
    struct StreamRequest {
        page_id: Option<String>,
    }
    let payload = if body.is_empty() {
        StreamRequest { page_id: None }
    } else {
        decode_json::<StreamRequest>(&body)?
    };
    state
        .browser
        .start_stream(session_id.clone(), payload.page_id, state.events.clone())
        .await?;
    Ok(Json(state.browser.context(&session_id).await))
}

async fn browser_stream_stop(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<BrowserContextSummary>, ApiError> {
    ensure_session_exists(&state, &session_id)?;
    #[derive(Deserialize)]
    struct StreamRequest {
        page_id: String,
    }
    let payload = decode_json::<StreamRequest>(&body)?;
    state
        .browser
        .stop_stream(&session_id, &payload.page_id)
        .await;
    Ok(Json(state.browser.context(&session_id).await))
}

fn persist_browser_session_artifacts(
    state: &AppState,
    session_id: &str,
    kind: &str,
    snapshot: &BrowserSnapshot,
    annotation: Option<&Value>,
) -> Result<Vec<String>, ApiError> {
    let artifact_id = Uuid::new_v4().to_string();
    let artifact_dir = state
        .store
        .artifacts_dir_path()
        .join("sessions")
        .join(session_id)
        .join("browser");
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create '{}'", artifact_dir.display()))?;
    let metadata = json!({
        "artifact_id": artifact_id,
        "kind": kind,
        "session_id": session_id,
        "page_id": snapshot.page_id,
        "url": snapshot.url,
        "title": snapshot.title,
        "viewport": null,
        "captured_at": snapshot.captured_at,
        "ref_count": snapshot.refs.len(),
        "downloads": snapshot.downloads,
        "annotation": annotation,
        "storage": "session_artifact",
        "memory": "not_promoted",
    });
    let json_path = artifact_dir.join(format!("{artifact_id}.json"));
    fs::write(
        &json_path,
        serde_json::to_vec_pretty(&json!({
            "metadata": metadata,
            "content": snapshot.content,
            "refs": &snapshot.refs,
            "downloads": &snapshot.downloads,
        }))
        .context("failed to encode browser session artifact")?,
    )
    .with_context(|| format!("failed to write '{}'", json_path.display()))?;
    let mut paths = vec![json_path.display().to_string()];
    if let Some((extension, bytes)) = decode_browser_data_url(&snapshot.screenshot_data_url)? {
        let image_path = artifact_dir.join(format!("{artifact_id}.{extension}"));
        fs::write(&image_path, bytes)
            .with_context(|| format!("failed to write '{}'", image_path.display()))?;
        paths.push(image_path.display().to_string());
    }
    Ok(paths)
}

fn decode_browser_data_url(value: &str) -> Result<Option<(String, Vec<u8>)>, ApiError> {
    let Some(rest) = value.strip_prefix("data:") else {
        return Ok(None);
    };
    let Some((metadata, encoded)) = rest.split_once(',') else {
        return Ok(None);
    };
    if !metadata.ends_with(";base64") {
        return Ok(None);
    }
    let mime_type = metadata.trim_end_matches(";base64");
    let extension = match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        _ => "bin",
    }
    .to_string();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("failed to decode browser screenshot data URL")?;
    Ok(Some((extension, bytes)))
}

fn ensure_session_exists(state: &AppState, session_id: &str) -> Result<(), ApiError> {
    state.store.get_session(session_id)?;
    Ok(())
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

async fn settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SettingsSummary>, ApiError> {
    Ok(Json(build_settings_summary(&state, &headers).await))
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
    Ok(Json(redact_workspace_summary(state.store.workspace()?)))
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
    let run_budget = match payload.run_budget {
        Some(value) => Some(normalize_workspace_run_budget(value)?),
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
        run_budget.as_ref(),
    )?;
    let _ = try_record_audit_event(
        &state,
        AuditEventRecord {
            kind: "workspace.updated".to_string(),
            target: "workspace:root".to_string(),
            status: "success".to_string(),
            summary: "Updated workspace settings.".to_string(),
            detail: format!(
                "root_path={} default_profile_id={} main_target={} utility_target={} run_budget_steps={} run_budget_actions={} run_budget_wall_clock_secs={}",
                root_path.unwrap_or_else(|| workspace.root_path.clone()),
                default_profile_id.unwrap_or_else(|| workspace.default_profile_id.clone()),
                main_target.unwrap_or_else(|| workspace.main_target.clone()),
                utility_target.unwrap_or_else(|| workspace.utility_target.clone()),
                workspace.run_budget.max_steps,
                workspace.run_budget.max_tool_calls,
                workspace.run_budget.max_wall_clock_secs
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(redact_workspace_summary(workspace)))
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
    Ok(Json(redact_workspace_profile(profile)))
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
    Ok(Json(redact_workspace_profile(profile)))
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
    Ok(Json(redact_workspace_summary(workspace)))
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
    Ok(Json(redact_workspace_summary(workspace)))
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
    Ok(Json(redact_workspace_summary(state.store.workspace()?)))
}

async fn router_profiles(
    State(state): State<AppState>,
) -> Result<Json<Vec<RouterProfileSummary>>, ApiError> {
    Ok(Json(redact_router_profiles(
        load_router_profiles(&state, false).await?,
    )))
}

async fn list_skills(State(state): State<AppState>) -> Result<Json<Vec<SkillManifest>>, ApiError> {
    let mut skills = state.store.list_skill_manifests()?;
    for skill in &mut skills {
        hydrate_skill_instructions_from_include(skill);
    }
    Ok(Json(skills))
}

async fn upsert_skill(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SkillManifest>, ApiError> {
    let payload = sanitize_skill_manifest(decode_json::<SkillManifest>(&body)?)?;
    sync_skill_instructions_to_file(&payload);
    Ok(Json(state.store.upsert_skill_manifest(&payload)?))
}

async fn upsert_skill_by_id(
    State(state): State<AppState>,
    Path(skill_id): Path<String>,
    body: Bytes,
) -> Result<Json<SkillManifest>, ApiError> {
    let mut payload = sanitize_skill_manifest(decode_json::<SkillManifest>(&body)?)?;
    payload.id = sanitize_registry_id(&skill_id, "skill id")?;
    sync_skill_instructions_to_file(&payload);
    Ok(Json(state.store.upsert_skill_manifest(&payload)?))
}

async fn list_skill_packages(
    State(state): State<AppState>,
) -> Result<Json<Vec<SkillPackageRecord>>, ApiError> {
    Ok(Json(state.store.list_skill_packages()?))
}

async fn upsert_skill_package(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SkillPackageRecord>, ApiError> {
    let payload = decode_json::<SkillPackageUpsertRequest>(&body)?;
    let package = build_skill_package_record(payload, None)?;
    Ok(Json(state.store.upsert_skill_package(&package)?))
}

async fn upsert_skill_package_by_id(
    State(state): State<AppState>,
    Path(package_id): Path<String>,
    body: Bytes,
) -> Result<Json<SkillPackageRecord>, ApiError> {
    let payload = decode_json::<SkillPackageUpsertRequest>(&body)?;
    let package = build_skill_package_record(payload, Some(package_id))?;
    Ok(Json(state.store.upsert_skill_package(&package)?))
}

async fn list_skill_installations(
    State(state): State<AppState>,
) -> Result<Json<Vec<SkillInstallationRecord>>, ApiError> {
    Ok(Json(state.store.list_skill_installations()?))
}

async fn upsert_skill_installation(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SkillInstallationRecord>, ApiError> {
    let payload = decode_json::<SkillInstallationUpsertRequest>(&body)?;
    let installation = build_skill_installation_record(&state, payload, None)?;
    Ok(Json(state.store.upsert_skill_installation(&installation)?))
}

async fn upsert_skill_installation_by_id(
    State(state): State<AppState>,
    Path(installation_id): Path<String>,
    body: Bytes,
) -> Result<Json<SkillInstallationRecord>, ApiError> {
    let payload = decode_json::<SkillInstallationUpsertRequest>(&body)?;
    let installation = build_skill_installation_record(&state, payload, Some(installation_id))?;
    Ok(Json(state.store.upsert_skill_installation(&installation)?))
}

async fn list_mcp_servers(
    State(state): State<AppState>,
) -> Result<Json<Vec<McpServerSummary>>, ApiError> {
    Ok(Json(redact_mcp_servers(state.store.list_mcp_servers()?)))
}

async fn upsert_mcp_server(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<McpServerSummary>, ApiError> {
    let payload = sanitize_mcp_server(decode_json::<McpServerSummary>(&body)?)?;
    Ok(Json(redact_mcp_server(
        state.store.upsert_mcp_server(&payload)?,
    )))
}

async fn upsert_mcp_server_by_id(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
    body: Bytes,
) -> Result<Json<McpServerSummary>, ApiError> {
    let mut payload = sanitize_mcp_server(decode_json::<McpServerSummary>(&body)?)?;
    payload.id = sanitize_registry_id(&server_id, "MCP server id")?;
    Ok(Json(redact_mcp_server(
        state.store.upsert_mcp_server(&payload)?,
    )))
}

async fn list_memory(State(state): State<AppState>) -> Result<Json<MemorySummary>, ApiError> {
    let mut entries = state
        .store
        .list_memory_entries()
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    entries.sort_by(|a, b| {
        a.scope_kind
            .cmp(&b.scope_kind)
            .then(a.scope_id.cmp(&b.scope_id))
            .then(a.title.cmp(&b.title))
            .then(a.id.cmp(&b.id))
    });
    let enabled_count = entries.iter().filter(|entry| entry.enabled).count();
    let scope_count = entries
        .iter()
        .map(|entry| format!("{}:{}", entry.scope_kind, entry.scope_id))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    Ok(Json(MemorySummary {
        entries,
        enabled_count,
        scope_count,
    }))
}

#[derive(Debug, Deserialize)]
struct MemorySearchQuery {
    q: String,
    scope_kind: Option<String>,
    scope_id: Option<String>,
    session_id: Option<String>,
    limit: Option<usize>,
}

async fn search_memory(
    State(state): State<AppState>,
    Query(query): Query<MemorySearchQuery>,
) -> Result<Json<MemorySearchResponse>, ApiError> {
    let search_text = query.q.trim();
    if search_text.is_empty() {
        return Err(ApiError::bad_request("memory search query is required"));
    }

    let scope_kind = query
        .scope_kind
        .as_deref()
        .map(normalize_memory_scope_kind)
        .transpose()?;
    let scope_id = query
        .scope_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if scope_kind.is_some() != scope_id.is_some() {
        return Err(ApiError::bad_request(
            "memory search scope_kind and scope_id must be provided together",
        ));
    }

    let requested_limit = query
        .limit
        .unwrap_or(20)
        .clamp(1, MAX_MEMORY_SEARCH_RESULTS);
    let storage_limit = if query.session_id.is_some() {
        // Session applicability is enforced with the same prompt-context rules below. Pull a
        // bounded candidate set first so unrelated high-ranked scopes cannot consume the final
        // caller-requested limit before daemon-side session filtering runs.
        SESSION_MEMORY_SEARCH_CANDIDATE_LIMIT
    } else {
        requested_limit
    };

    let mut results = state
        .store
        .search_memory_entries(
            search_text,
            scope_kind.as_deref(),
            scope_id.as_deref(),
            storage_limit,
        )
        .map_err(ApiError::from)?;

    if let Some(session_id) = query.session_id.as_deref() {
        let detail = state
            .store
            .get_session(session_id)
            .map_err(ApiError::from)?;
        let project_ids = detail
            .session
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<BTreeSet<_>>();
        results.retain(|result| {
            memory_entry_applies_to_session(&result.entry, &detail.session, &project_ids)
        });
    }
    results.truncate(requested_limit);

    let used_ids = results
        .iter()
        .map(|result| result.entry.id.clone())
        .collect::<Vec<_>>();
    state
        .store
        .record_memory_entries_used(&used_ids)
        .map_err(ApiError::from)?;
    for result in &mut results {
        result.entry = state
            .store
            .get_memory_entry(&result.entry.id)
            .map_err(ApiError::from)?;
    }

    Ok(Json(MemorySearchResponse { results }))
}

async fn upsert_memory(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<MemoryEntry>, ApiError> {
    let payload = decode_json::<MemoryEntryUpsertRequest>(&body)?;
    upsert_memory_from_request(&state, payload, None).map(Json)
}

async fn upsert_memory_by_id(
    State(state): State<AppState>,
    Path(memory_id): Path<String>,
    body: Bytes,
) -> Result<Json<MemoryEntry>, ApiError> {
    let payload = decode_json::<MemoryEntryUpsertRequest>(&body)?;
    upsert_memory_from_request(&state, payload, Some(memory_id)).map(Json)
}

async fn delete_memory(
    State(state): State<AppState>,
    Path(memory_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let memory_id = sanitize_registry_id(&memory_id, "memory id")?;
    state
        .store
        .delete_memory_entry(&memory_id)
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    Ok(StatusCode::NO_CONTENT)
}

fn upsert_memory_from_request(
    state: &AppState,
    payload: MemoryEntryUpsertRequest,
    id_override: Option<String>,
) -> Result<MemoryEntry, ApiError> {
    let scope_kind = normalize_memory_scope_kind(&payload.scope_kind)?;
    let scope_id = payload.scope_id.trim();
    let title = payload.title.trim();
    let content = security::RedactionSet::new().redact_text(payload.content.trim());
    if scope_id.is_empty() || title.is_empty() || content.is_empty() {
        return Err(ApiError::bad_request(
            "memory scope, title, and content are required",
        ));
    }
    let id = match id_override {
        Some(value) => sanitize_registry_id(&value, "memory id")?,
        None => sanitize_registry_id(payload.id.as_deref().unwrap_or(title), "memory id")?,
    };
    let entry = MemoryEntry {
        id,
        scope_kind,
        scope_id: scope_id.to_string(),
        title: title.to_string(),
        content,
        tags: payload
            .tags
            .into_iter()
            .map(|tag: String| tag.trim().to_string())
            .filter(|tag: &String| !tag.is_empty())
            .collect(),
        enabled: payload.enabled.unwrap_or(true),
        status: normalize_memory_status(payload.status.as_deref())?,
        memory_kind: normalize_memory_kind(payload.memory_kind.as_deref())?,
        source_kind: normalize_memory_source_kind(payload.source_kind.as_deref())?,
        source_id: payload.source_id.unwrap_or_default().trim().to_string(),
        confidence: payload.confidence.unwrap_or(1.0).clamp(0.0, 1.0),
        created_by: normalize_memory_created_by(payload.created_by.as_deref())?,
        last_used_at: payload.last_used_at,
        use_count: payload.use_count.unwrap_or(0).max(0),
        supersedes_id: payload.supersedes_id.unwrap_or_default().trim().to_string(),
        metadata_json: security::RedactionSet::new()
            .redact_json(&payload.metadata_json.unwrap_or_else(|| json!({}))),
        created_at: 0,
        updated_at: 0,
    };
    state
        .store
        .upsert_memory_entry(&entry)
        .map_err(ApiError::from)
}

const MEMORY_EXTRACTION_CONTEXT_CHAR_BUDGET: usize = 8_000;

#[derive(Debug, Deserialize)]
struct ExtractedMemoryCandidate {
    title: String,
    content: String,
    #[serde(default = "default_extracted_candidate_kind")]
    candidate_kind: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    scope_kind: Option<String>,
    #[serde(default)]
    scope_id: Option<String>,
}

fn default_extracted_candidate_kind() -> String {
    "note".to_string()
}

pub(crate) async fn extract_memory_candidates_after_successful_turn(
    state: &AppState,
    session_id: &str,
    assistant_turn_id: &str,
) {
    record_memory_audit(
        state,
        "memory.candidate.extraction_started",
        session_id,
        "started",
        "Started automatic memory candidate extraction.",
    )
    .await;

    match extract_memory_candidates_after_successful_turn_inner(
        state,
        session_id,
        assistant_turn_id,
    )
    .await
    {
        Ok(count) => {
            record_memory_audit(
                state,
                "memory.candidate.extraction_completed",
                session_id,
                "completed",
                &format!("Completed automatic memory candidate extraction; stored {count} pending candidates."),
            )
            .await;
        }
        Err(error) => {
            warn!(error = ?error, session_id, assistant_turn_id, "automatic memory candidate extraction failed without affecting the user turn");
            record_memory_audit(
                state,
                "memory.candidate.extraction_failed",
                session_id,
                "failed",
                "Automatic memory candidate extraction failed without affecting the user turn.",
            )
            .await;
        }
    }
}

async fn extract_memory_candidates_after_successful_turn_inner(
    state: &AppState,
    session_id: &str,
    assistant_turn_id: &str,
) -> Result<usize, ApiError> {
    let detail = state
        .store
        .get_session(session_id)
        .map_err(ApiError::from)?;
    let Some(assistant_turn) = detail
        .turns
        .iter()
        .find(|turn| turn.id == assistant_turn_id && turn.role == "assistant")
    else {
        return Err(ApiError::bad_request(
            "assistant turn was not found for memory extraction",
        ));
    };
    let recent_context = bounded_recent_turn_context(&detail.turns);
    let extracted = parse_structured_memory_candidates(&assistant_turn.content, &recent_context)?;
    let mut stored_count = 0usize;
    for item in extracted {
        let scope_kind = item
            .scope_kind
            .unwrap_or_else(|| infer_session_memory_scope_kind(&detail.session));
        let scope_id = item
            .scope_id
            .unwrap_or_else(|| infer_session_memory_scope_id(&detail.session, &scope_kind));
        let payload = MemoryCandidateUpsertRequest {
            id: Some(format!(
                "auto-{}-{}",
                assistant_turn_id,
                stable_short_hash(&format!("{}:{}", item.title, item.content))
            )),
            scope_kind,
            scope_id,
            session_id: Some(session_id.to_string()),
            turn_id_start: detail.turns.first().map(|turn| turn.id.clone()),
            turn_id_end: Some(assistant_turn_id.to_string()),
            candidate_kind: Some(item.candidate_kind),
            title: item.title,
            content: item.content,
            tags: item.tags,
            evidence: item.evidence,
            reason: Some(item.reason),
            confidence: item.confidence,
            status: Some("pending".to_string()),
            dedupe_key: None,
            accepted_memory_id: None,
            created_by: Some("utility_worker".to_string()),
            metadata_json: Some(json!({
                "source": "automatic_extraction",
                "assistant_turn_id": assistant_turn_id,
                "context_char_budget": MEMORY_EXTRACTION_CONTEXT_CHAR_BUDGET,
                "context_chars_used": recent_context.len(),
                "format": "structured_json_candidates"
            })),
        };
        match upsert_memory_candidate_from_request(state, payload, None, false).await {
            Ok(_) => stored_count += 1,
            Err(error) if error.message.contains("duplicate pending memory candidate") => {}
            Err(error) if error.message.contains("credential-like") => {}
            Err(error) => return Err(error),
        }
    }
    Ok(stored_count)
}

fn parse_structured_memory_candidates(
    assistant_content: &str,
    _recent_context: &str,
) -> Result<Vec<ExtractedMemoryCandidate>, ApiError> {
    if assistant_content.contains("NUCLEUS_MEMORY_EXTRACT_FAIL") {
        return Err(ApiError::bad_request("simulated memory extraction failure"));
    }
    if let Some(json_text) = extract_tagged_json_array(assistant_content, "memory_candidates") {
        return serde_json::from_str::<Vec<ExtractedMemoryCandidate>>(json_text)
            .map_err(|_| ApiError::bad_request("memory extraction returned invalid JSON"));
    }

    let mut candidates = Vec::new();
    for line in assistant_content.lines() {
        let trimmed = line.trim();
        let Some(content) = trimmed
            .strip_prefix("Remember:")
            .or_else(|| trimmed.strip_prefix("Memory:"))
            .map(str::trim)
        else {
            continue;
        };
        if content.len() < 12 {
            continue;
        }
        candidates.push(ExtractedMemoryCandidate {
            title: excerpt(content, 64),
            content: content.to_string(),
            candidate_kind: "note".to_string(),
            tags: vec!["automatic".to_string()],
            evidence: vec![excerpt(trimmed, 160)],
            reason: "Assistant marked this as durable information to remember.".to_string(),
            confidence: Some(0.7),
            scope_kind: None,
            scope_id: None,
        });
    }
    Ok(candidates)
}

fn extract_tagged_json_array<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let start = text.find(&start_tag)? + start_tag.len();
    let end = text[start..].find(&end_tag)? + start;
    Some(text[start..end].trim())
}

fn bounded_recent_turn_context(turns: &[nucleus_protocol::SessionTurn]) -> String {
    let mut selected = Vec::new();
    let mut remaining = MEMORY_EXTRACTION_CONTEXT_CHAR_BUDGET;
    for turn in turns.iter().rev() {
        let rendered = format!("{}: {}", turn.role, turn.content);
        if rendered.len() > remaining {
            continue;
        }
        remaining -= rendered.len();
        selected.push(rendered);
    }
    selected.reverse();
    selected.join("\n")
}

fn infer_session_memory_scope_kind(session: &SessionSummary) -> String {
    if !session.project_id.is_empty() {
        "project".to_string()
    } else {
        "workspace".to_string()
    }
}

fn infer_session_memory_scope_id(session: &SessionSummary, scope_kind: &str) -> String {
    if scope_kind == "project" && !session.project_id.is_empty() {
        session.project_id.clone()
    } else {
        "workspace".to_string()
    }
}

fn stable_short_hash(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

#[derive(Debug, Deserialize)]
struct MemoryCandidateQuery {
    status: Option<String>,
    scope_kind: Option<String>,
    scope_id: Option<String>,
    session_id: Option<String>,
}

async fn list_memory_candidates(
    State(state): State<AppState>,
    Query(query): Query<MemoryCandidateQuery>,
) -> Result<Json<MemoryCandidateListResponse>, ApiError> {
    let mut candidates = state
        .store
        .list_memory_candidates()
        .map_err(ApiError::from)?;
    candidates.retain(|candidate| {
        query.status.as_ref().is_none_or(|v| candidate.status == *v)
            && query
                .scope_kind
                .as_ref()
                .is_none_or(|v| candidate.scope_kind == *v)
            && query
                .scope_id
                .as_ref()
                .is_none_or(|v| candidate.scope_id == *v)
            && query
                .session_id
                .as_ref()
                .is_none_or(|v| candidate.session_id == *v)
    });
    Ok(Json(MemoryCandidateListResponse { candidates }))
}

async fn upsert_memory_candidate(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<MemoryCandidate>, ApiError> {
    let payload = decode_json::<MemoryCandidateUpsertRequest>(&body)?;
    upsert_memory_candidate_from_request(&state, payload, None, true)
        .await
        .map(Json)
}

async fn upsert_memory_candidate_by_id(
    State(state): State<AppState>,
    Path(candidate_id): Path<String>,
    body: Bytes,
) -> Result<Json<MemoryCandidate>, ApiError> {
    let payload = decode_json::<MemoryCandidateUpsertRequest>(&body)?;
    upsert_memory_candidate_from_request(&state, payload, Some(candidate_id), true)
        .await
        .map(Json)
}

async fn accept_memory_candidate(
    State(state): State<AppState>,
    Path(candidate_id): Path<String>,
    body: Bytes,
) -> Result<Json<MemoryEntry>, ApiError> {
    let candidate_id = sanitize_registry_id(&candidate_id, "memory candidate id")?;
    let request = if body.is_empty() {
        MemoryCandidateAcceptRequest {
            title: None,
            content: None,
            tags: None,
            memory_kind: None,
            confidence: None,
            created_by: None,
            metadata_json: None,
        }
    } else {
        decode_json::<MemoryCandidateAcceptRequest>(&body)?
    };
    let mut candidate = state
        .store
        .load_memory_candidate(&candidate_id)
        .map_err(ApiError::from)?;
    if candidate.status == "accepted" && !candidate.accepted_memory_id.is_empty() {
        return state
            .store
            .upsert_memory_entry(
                &state
                    .store
                    .list_memory_entries()?
                    .into_iter()
                    .find(|entry| entry.id == candidate.accepted_memory_id)
                    .ok_or_else(|| ApiError::bad_request("accepted memory entry is missing"))?,
            )
            .map(Json)
            .map_err(ApiError::from);
    }
    let title = request
        .title
        .unwrap_or_else(|| candidate.title.clone())
        .trim()
        .to_string();
    let content = security::RedactionSet::new().redact_text(
        request
            .content
            .unwrap_or_else(|| candidate.content.clone())
            .trim(),
    );
    if title.is_empty() || content.is_empty() || contains_credential_like_value(&content) {
        return Err(ApiError::bad_request(
            "accepted memory content is empty or credential-like",
        ));
    }
    let memory_id = sanitize_registry_id(&format!("candidate-{}", candidate.id), "memory id")?;
    let entry = MemoryEntry { id: memory_id.clone(), scope_kind: candidate.scope_kind.clone(), scope_id: candidate.scope_id.clone(), title, content, tags: request.tags.unwrap_or_else(|| candidate.tags.clone()), enabled: true, status: "accepted".to_string(), memory_kind: normalize_memory_kind(request.memory_kind.as_deref().or(Some(candidate.candidate_kind.as_str())))?, source_kind: "candidate".to_string(), source_id: candidate.id.clone(), confidence: request.confidence.unwrap_or(candidate.confidence).clamp(0.0,1.0), created_by: normalize_memory_created_by(request.created_by.as_deref())?, last_used_at: None, use_count: 0, supersedes_id: String::new(), metadata_json: security::RedactionSet::new().redact_json(&request.metadata_json.unwrap_or_else(|| json!({"candidate_evidence": candidate.evidence, "candidate_reason": candidate.reason}))), created_at: 0, updated_at: 0 };
    let entry = state
        .store
        .upsert_memory_entry(&entry)
        .map_err(ApiError::from)?;
    candidate.status = "accepted".to_string();
    candidate.accepted_memory_id = entry.id.clone();
    state
        .store
        .upsert_memory_candidate(&candidate)
        .map_err(ApiError::from)?;
    record_memory_audit(
        &state,
        "memory.candidate.accepted",
        &candidate.id,
        "accepted",
        &format!("Accepted memory candidate '{}'.", candidate.title),
    )
    .await;
    Ok(Json(entry))
}

async fn reject_memory_candidate(
    State(state): State<AppState>,
    Path(candidate_id): Path<String>,
) -> Result<Json<MemoryCandidate>, ApiError> {
    let candidate_id = sanitize_registry_id(&candidate_id, "memory candidate id")?;
    let mut candidate = state
        .store
        .load_memory_candidate(&candidate_id)
        .map_err(ApiError::from)?;
    candidate.status = "rejected".to_string();
    let candidate = state
        .store
        .upsert_memory_candidate(&candidate)
        .map_err(ApiError::from)?;
    record_memory_audit(
        &state,
        "memory.candidate.rejected",
        &candidate.id,
        "rejected",
        &format!("Rejected memory candidate '{}'.", candidate.title),
    )
    .await;
    Ok(Json(candidate))
}

async fn delete_memory_candidate(
    State(state): State<AppState>,
    Path(candidate_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let candidate_id = sanitize_registry_id(&candidate_id, "memory candidate id")?;
    state
        .store
        .delete_memory_candidate(&candidate_id)
        .map_err(ApiError::from)?;
    record_memory_audit(
        &state,
        "memory.candidate.dismissed",
        &candidate_id,
        "dismissed",
        "Dismissed memory candidate.",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn explicit_remember(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<MemoryEntry>, ApiError> {
    let mut payload = decode_json::<MemoryEntryUpsertRequest>(&body)?;
    payload.source_kind = Some("explicit_remember".to_string());
    payload.created_by = Some(payload.created_by.unwrap_or_else(|| "user".to_string()));
    let entry = upsert_memory_from_request(&state, payload, None)?;
    record_memory_audit(
        &state,
        "memory.explicit.created",
        &entry.id,
        "created",
        &format!("Created explicit memory '{}'.", entry.title),
    )
    .await;
    Ok(Json(entry))
}

async fn upsert_memory_candidate_from_request(
    state: &AppState,
    payload: MemoryCandidateUpsertRequest,
    id_override: Option<String>,
    audit: bool,
) -> Result<MemoryCandidate, ApiError> {
    let scope_kind = normalize_memory_scope_kind(&payload.scope_kind)?;
    let scope_id = payload.scope_id.trim();
    let title = payload.title.trim();
    let redactor = security::RedactionSet::new();
    let content = redactor.redact_text(payload.content.trim());
    if scope_id.is_empty()
        || title.is_empty()
        || content.is_empty()
        || contains_credential_like_value(&content)
    {
        return Err(ApiError::bad_request(
            "candidate scope, title, and non-secret content are required",
        ));
    }
    let candidate_kind = normalize_memory_kind(payload.candidate_kind.as_deref())?;
    let dedupe_key = payload.dedupe_key.unwrap_or_else(|| {
        memory_dedupe_key(&scope_kind, scope_id, &candidate_kind, title, &content)
    });
    if !dedupe_key.is_empty()
        && state
            .store
            .list_memory_candidates()?
            .into_iter()
            .any(|existing| {
                existing.id != payload.id.clone().unwrap_or_default()
                    && existing.dedupe_key == dedupe_key
                    && existing.status == "pending"
            })
    {
        return Err(ApiError::bad_request("duplicate pending memory candidate"));
    }
    let id = match id_override {
        Some(value) => sanitize_registry_id(&value, "memory candidate id")?,
        None => sanitize_registry_id(
            payload.id.as_deref().unwrap_or(title),
            "memory candidate id",
        )?,
    };
    let candidate = MemoryCandidate {
        id,
        scope_kind,
        scope_id: scope_id.to_string(),
        session_id: payload.session_id.unwrap_or_default(),
        turn_id_start: payload.turn_id_start.unwrap_or_default(),
        turn_id_end: payload.turn_id_end.unwrap_or_default(),
        candidate_kind,
        title: title.to_string(),
        content,
        tags: payload
            .tags
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        evidence: payload
            .evidence
            .into_iter()
            .map(|e| redactor.redact_text(e.trim()))
            .filter(|e| !e.is_empty() && !contains_credential_like_value(e))
            .collect(),
        reason: redactor.redact_text(payload.reason.unwrap_or_default().trim()),
        confidence: payload.confidence.unwrap_or(0.0).clamp(0.0, 1.0),
        status: normalize_candidate_status(payload.status.as_deref())?,
        dedupe_key,
        accepted_memory_id: payload.accepted_memory_id.unwrap_or_default(),
        created_by: normalize_memory_created_by(
            payload.created_by.as_deref().or(Some("utility_worker")),
        )?,
        created_at: 0,
        updated_at: 0,
        metadata_json: redactor.redact_json(&payload.metadata_json.unwrap_or_else(|| json!({}))),
    };
    let candidate = state
        .store
        .upsert_memory_candidate(&candidate)
        .map_err(ApiError::from)?;
    if audit {
        record_memory_audit(
            state,
            "memory.candidate.created",
            &candidate.id,
            "created",
            &format!("Created memory candidate '{}'.", candidate.title),
        )
        .await;
    }
    Ok(candidate)
}

fn normalize_candidate_status(value: Option<&str>) -> Result<String, ApiError> {
    match value.unwrap_or("pending").trim() {
        "pending" | "accepted" | "rejected" | "dismissed" | "superseded" => {
            Ok(value.unwrap_or("pending").trim().to_string())
        }
        _ => Err(ApiError::bad_request(
            "candidate status must be pending, accepted, rejected, dismissed, or superseded",
        )),
    }
}
fn memory_dedupe_key(
    scope_kind: &str,
    scope_id: &str,
    kind: &str,
    title: &str,
    content: &str,
) -> String {
    let mut h = Sha256::new();
    h.update(format!(
        "{}:{}:{}:{}:{}",
        scope_kind,
        scope_id,
        kind,
        title.to_lowercase(),
        content.to_lowercase()
    ));
    format!("memory-candidate:{:x}", h.finalize())
}
fn contains_credential_like_value(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("authorization: bearer")
        || lower.contains("-----begin ")
        || lower.contains("password=")
        || lower.contains("api_key=")
        || lower.contains("access_token=")
        || lower.contains("refresh_token=")
        || lower.contains("://") && lower.contains('@')
}
async fn record_memory_audit(
    state: &AppState,
    kind: &str,
    target: &str,
    status: &str,
    summary: &str,
) {
    let redactor = security::RedactionSet::new();
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: kind.to_string(),
            target: target.to_string(),
            status: status.to_string(),
            summary: redactor.redact_text(summary),
            detail: "{}".to_string(),
        },
    )
    .await;
}

fn normalize_memory_scope_kind(value: &str) -> Result<String, ApiError> {
    match value.trim() {
        "workspace" | "project" | "session" => Ok(value.trim().to_string()),
        _ => Err(ApiError::bad_request(
            "memory scope kind must be workspace, project, or session",
        )),
    }
}

fn normalize_memory_status(value: Option<&str>) -> Result<String, ApiError> {
    let value = value.unwrap_or("accepted").trim();
    match value {
        "accepted" | "archived" => Ok(value.to_string()),
        _ => Err(ApiError::bad_request(
            "memory status must be accepted or archived",
        )),
    }
}

fn normalize_memory_kind(value: Option<&str>) -> Result<String, ApiError> {
    let value = value.unwrap_or("note").trim();
    match value {
        "note" | "fact" | "preference" | "decision" | "project_note" | "solution"
        | "constraint" | "todo" => Ok(value.to_string()),
        _ => Err(ApiError::bad_request(
            "memory kind must be note, fact, preference, decision, project_note, solution, constraint, or todo",
        )),
    }
}

fn normalize_memory_source_kind(value: Option<&str>) -> Result<String, ApiError> {
    let value = value.unwrap_or("manual").trim();
    match value {
        "manual" | "candidate" | "explicit_remember" | "import" | "system" => Ok(value.to_string()),
        _ => Err(ApiError::bad_request(
            "memory source kind must be manual, candidate, explicit_remember, import, or system",
        )),
    }
}

fn normalize_memory_created_by(value: Option<&str>) -> Result<String, ApiError> {
    let value = value.unwrap_or("user").trim();
    match value {
        "user" | "assistant" | "utility_worker" | "system" => Ok(value.to_string()),
        _ => Err(ApiError::bad_request(
            "memory created_by must be user, assistant, utility_worker, or system",
        )),
    }
}

#[derive(Debug, Deserialize)]
struct VaultSecretListQuery {
    scope_kind: Option<String>,
    scope_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VaultSecretPolicyScopeQuery {
    scope_kind: Option<String>,
    scope_id: Option<String>,
}

async fn vault_status(State(state): State<AppState>) -> Result<Json<VaultStatusSummary>, ApiError> {
    Ok(Json(vault_status_summary(&state).await?))
}

async fn vault_init(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<VaultStatusSummary>, ApiError> {
    require_vault_safe_origin(&headers)?;
    let payload = decode_json::<VaultInitRequest>(&body)?;
    let mut vault = state.vault.lock().await;
    let record = vault
        .initialize(&state.store, &payload.passphrase)
        .map_err(vault_api_error)?;
    drop(vault);
    record_vault_audit(
        &state,
        "vault.initialized",
        "vault:default",
        "Initialized local Vault.",
    )
    .await;
    Ok(Json(vault_status_from_record(Some(record), true)))
}

async fn vault_unlock(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<VaultStatusSummary>, ApiError> {
    require_vault_safe_origin(&headers)?;
    let payload = decode_json::<VaultUnlockRequest>(&body)?;
    let mut vault = state.vault.lock().await;
    match vault.unlock(&state.store, &payload.passphrase) {
        Ok(record) => {
            drop(vault);
            record_vault_audit(
                &state,
                "vault.unlocked",
                "vault:default",
                "Unlocked local Vault.",
            )
            .await;
            Ok(Json(vault_status_from_record(Some(record), true)))
        }
        Err(error) => {
            drop(vault);
            record_vault_audit(
                &state,
                "vault.unlock_failed",
                "vault:default",
                "Vault unlock failed.",
            )
            .await;
            Err(vault_api_error(error))
        }
    }
}

async fn vault_lock(State(state): State<AppState>) -> Result<Json<VaultStatusSummary>, ApiError> {
    state.vault.lock().await.lock();
    record_vault_audit(
        &state,
        "vault.locked",
        "vault:default",
        "Locked local Vault.",
    )
    .await;
    Ok(Json(vault_status_summary(&state).await?))
}

async fn list_vault_secrets(
    State(state): State<AppState>,
    Query(query): Query<VaultSecretListQuery>,
) -> Result<Json<VaultSecretListResponse>, ApiError> {
    let scope = validate_vault_scope_query(
        &state,
        query.scope_kind.as_deref(),
        query.scope_id.as_deref(),
    )?;
    let secrets = state
        .store
        .list_vault_secrets(
            scope.as_ref().map(|(kind, _)| kind.as_str()),
            scope.as_ref().map(|(_, id)| id.as_str()),
        )?
        .into_iter()
        .map(vault_secret_summary)
        .collect();
    Ok(Json(VaultSecretListResponse { secrets }))
}

async fn create_vault_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<VaultSecretSummary>, ApiError> {
    require_vault_safe_origin(&headers)?;
    let payload = decode_json::<VaultSecretUpsertRequest>(&body)?;
    let input = validate_vault_secret_create(&state, payload)?;
    let mut vault = state.vault.lock().await;
    let saved = vault
        .create_or_update_secret(&state.store, input)
        .map_err(vault_api_error)?;
    drop(vault);
    record_vault_audit(
        &state,
        "vault.secret.created",
        &format!("vault_secret:{}", saved.id),
        "Created Vault secret metadata.",
    )
    .await;
    Ok(Json(vault_secret_summary(saved)))
}

async fn update_vault_secret(
    State(state): State<AppState>,
    Path(secret_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<VaultSecretSummary>, ApiError> {
    require_vault_safe_origin(&headers)?;
    let secret_id = sanitize_registry_id(&secret_id, "vault secret id")?;
    let existing = state.store.load_vault_secret(&secret_id)?;
    let payload = decode_json::<VaultSecretUpdateRequest>(&body)?;
    let secret = payload
        .secret
        .ok_or_else(|| ApiError::bad_request("vault secret value is required for update"))?;
    let scope_id = match payload.scope_id {
        Some(scope_id) => non_empty_trimmed(scope_id, "vault scope id")?,
        None => existing.scope_id,
    };
    let scope_kind = normalize_vault_scope_kind(
        payload
            .scope_kind
            .as_deref()
            .unwrap_or(&existing.scope_kind),
    )?;
    validate_vault_scope(&state, &scope_kind, &scope_id)?;
    let input = vault::VaultSecretInput {
        id: Some(secret_id),
        scope_kind,
        scope_id,
        name: sanitize_vault_secret_name(payload.name.as_deref().unwrap_or(&existing.name))?,
        description: payload
            .description
            .unwrap_or(existing.description)
            .trim()
            .to_string(),
        secret,
    };
    let mut vault = state.vault.lock().await;
    let saved = vault
        .create_or_update_secret(&state.store, input)
        .map_err(vault_api_error)?;
    drop(vault);
    record_vault_audit(
        &state,
        "vault.secret.updated",
        &format!("vault_secret:{}", saved.id),
        "Updated Vault secret metadata.",
    )
    .await;
    Ok(Json(vault_secret_summary(saved)))
}

async fn delete_vault_secret(
    State(state): State<AppState>,
    Path(secret_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_vault_safe_origin(&headers)?;
    let secret_id = sanitize_registry_id(&secret_id, "vault secret id")?;
    if !state.vault.lock().await.is_unlocked() {
        return Err(ApiError::forbidden("vault is locked"));
    }
    state.store.delete_vault_secret(&secret_id)?;
    record_vault_audit(
        &state,
        "vault.secret.deleted",
        &format!("vault_secret:{secret_id}"),
        "Deleted Vault secret metadata.",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_vault_secret_policies(
    State(state): State<AppState>,
    Path(secret_id): Path<String>,
    Query(query): Query<VaultSecretPolicyScopeQuery>,
) -> Result<Json<VaultSecretPolicyListResponse>, ApiError> {
    let secret_id = sanitize_registry_id(&secret_id, "vault secret id")?;
    let secret = state.store.load_vault_secret(&secret_id)?;
    validate_vault_secret_policy_context(&state, &secret, &query)?;
    let policies = state
        .store
        .list_vault_secret_policies(&secret_id)?
        .into_iter()
        .map(vault_secret_policy_summary)
        .collect();
    Ok(Json(VaultSecretPolicyListResponse { policies }))
}

async fn upsert_vault_secret_policy(
    State(state): State<AppState>,
    Path(secret_id): Path<String>,
    Query(query): Query<VaultSecretPolicyScopeQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<VaultSecretPolicySummary>, ApiError> {
    require_vault_safe_origin(&headers)?;
    let secret_id = sanitize_registry_id(&secret_id, "vault secret id")?;
    let secret = state.store.load_vault_secret(&secret_id)?;
    validate_vault_secret_policy_context(&state, &secret, &query)?;
    if !state.vault.lock().await.is_unlocked() {
        return Err(ApiError::forbidden("vault is locked"));
    }
    let payload = decode_json::<VaultSecretPolicyUpsertRequest>(&body)?;
    let record = VaultSecretPolicyRecord {
        id: payload
            .id
            .map(|id| sanitize_registry_id(&id, "vault policy id"))
            .transpose()?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        secret_id: secret_id.clone(),
        consumer_kind: normalize_vault_consumer_kind(&payload.consumer_kind)?,
        consumer_id: sanitize_registry_id(&payload.consumer_id, "vault consumer id")?,
        permission: normalize_vault_permission(&payload.permission)?,
        approval_mode: normalize_vault_approval_mode(&payload.approval_mode)?,
        created_at: 0,
        updated_at: 0,
    };
    let saved = state.store.upsert_vault_secret_policy(&record)?;
    record_vault_audit(
        &state,
        "vault.secret.policy.updated",
        &format!("vault_secret:{}", secret_id),
        "Updated Vault secret access policy metadata.",
    )
    .await;
    Ok(Json(vault_secret_policy_summary(saved)))
}

async fn delete_vault_secret_policy(
    State(state): State<AppState>,
    Path((secret_id, policy_id)): Path<(String, String)>,
    Query(query): Query<VaultSecretPolicyScopeQuery>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_vault_safe_origin(&headers)?;
    let secret_id = sanitize_registry_id(&secret_id, "vault secret id")?;
    let policy_id = sanitize_registry_id(&policy_id, "vault policy id")?;
    let secret = state.store.load_vault_secret(&secret_id)?;
    validate_vault_secret_policy_context(&state, &secret, &query)?;
    if !state.vault.lock().await.is_unlocked() {
        return Err(ApiError::forbidden("vault is locked"));
    }
    state
        .store
        .delete_vault_secret_policy(&secret_id, &policy_id)?;
    record_vault_audit(
        &state,
        "vault.secret.policy.deleted",
        &format!("vault_secret:{}", secret_id),
        "Deleted Vault secret access policy metadata.",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn require_vault_safe_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = security::classify_request_origin(headers);
    if origin.safe {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!(
            "vault operation requires a safe origin: {}",
            origin.reason
        )))
    }
}

fn validate_vault_secret_policy_context(
    state: &AppState,
    secret: &VaultSecretRecord,
    query: &VaultSecretPolicyScopeQuery,
) -> Result<(), ApiError> {
    if secret.scope_kind == "workspace" {
        if let (Some(kind), Some(id)) = (query.scope_kind.as_deref(), query.scope_id.as_deref()) {
            let kind = normalize_vault_scope_kind(kind)?;
            let id = non_empty_trimmed(id.to_string(), "vault scope id")?;
            if kind != "workspace" || id != "workspace" {
                return Err(ApiError::forbidden(
                    "vault_project_context_mismatch: policy operation scope does not match secret scope",
                ));
            }
        }
        return Ok(());
    }

    if secret.scope_kind != "project" {
        return Err(ApiError::bad_request("unsupported Vault secret scope"));
    }

    let Some(kind) = query.scope_kind.as_deref() else {
        return Err(ApiError::forbidden(
            "vault_project_context_missing: project Vault policy operation requires matching project scope",
        ));
    };
    let Some(id) = query.scope_id.as_deref() else {
        return Err(ApiError::forbidden(
            "vault_project_context_missing: project Vault policy operation requires matching project scope",
        ));
    };
    let kind = normalize_vault_scope_kind(kind)?;
    let id = non_empty_trimmed(id.to_string(), "vault scope id")?;
    validate_vault_scope(state, &kind, &id)?;
    if kind != secret.scope_kind || id != secret.scope_id {
        return Err(ApiError::forbidden(
            "vault_project_context_mismatch: policy operation scope does not match secret scope",
        ));
    }
    Ok(())
}

fn validate_vault_secret_create(
    state: &AppState,
    payload: VaultSecretUpsertRequest,
) -> Result<vault::VaultSecretInput, ApiError> {
    let scope_kind = normalize_vault_scope_kind(&payload.scope_kind)?;
    let scope_id = non_empty_trimmed(payload.scope_id, "vault scope id")?;
    validate_vault_scope(state, &scope_kind, &scope_id)?;
    Ok(vault::VaultSecretInput {
        id: payload
            .id
            .map(|id| sanitize_registry_id(&id, "vault secret id"))
            .transpose()?,
        scope_kind,
        scope_id,
        name: sanitize_vault_secret_name(&payload.name)?,
        description: payload.description.trim().to_string(),
        secret: non_empty_secret(payload.secret, "vault secret value")?,
    })
}

fn validate_vault_scope_query(
    state: &AppState,
    scope_kind: Option<&str>,
    scope_id: Option<&str>,
) -> Result<Option<(String, String)>, ApiError> {
    match (scope_kind, scope_id) {
        (Some(kind), Some(id)) => {
            let kind = normalize_vault_scope_kind(kind)?;
            let id = non_empty_trimmed(id.to_string(), "vault scope id")?;
            validate_vault_scope(state, &kind, &id)?;
            Ok(Some((kind, id)))
        }
        (None, None) => Ok(None),
        _ => Err(ApiError::bad_request(
            "vault scope filters require both scope_kind and scope_id",
        )),
    }
}

fn validate_vault_scope(
    state: &AppState,
    scope_kind: &str,
    scope_id: &str,
) -> Result<(), ApiError> {
    match scope_kind {
        "workspace" if scope_id == "workspace" => Ok(()),
        "workspace" => Err(ApiError::bad_request(
            "workspace Vault scope_id must be workspace",
        )),
        "project" => state
            .store
            .resolve_project(scope_id)
            .map(|_| ())
            .map_err(|_| ApiError::not_found(format!("project '{scope_id}' was not found"))),
        _ => Err(ApiError::bad_request(
            "vault scope kind must be workspace or project",
        )),
    }
}

fn normalize_vault_scope_kind(value: &str) -> Result<String, ApiError> {
    match value.trim() {
        "workspace" | "project" => Ok(value.trim().to_string()),
        _ => Err(ApiError::bad_request(
            "vault scope kind must be workspace or project",
        )),
    }
}

fn normalize_vault_consumer_kind(value: &str) -> Result<String, ApiError> {
    match value.trim() {
        "mcp" | "action" | "tool" | "workspace" => Ok(value.trim().to_string()),
        _ => Err(ApiError::bad_request(
            "vault consumer kind must be mcp, action, tool, or workspace",
        )),
    }
}

fn normalize_vault_permission(value: &str) -> Result<String, ApiError> {
    match value.trim() {
        "read" => Ok(value.trim().to_string()),
        _ => Err(ApiError::bad_request("vault permission must be read")),
    }
}

fn normalize_vault_approval_mode(value: &str) -> Result<String, ApiError> {
    match value.trim() {
        "allow" | "ask" | "deny" => Ok(value.trim().to_string()),
        _ => Err(ApiError::bad_request(
            "vault approval mode must be allow, ask, or deny",
        )),
    }
}

fn sanitize_vault_secret_name(value: &str) -> Result<String, ApiError> {
    sanitize_registry_id(value, "vault secret name")
}

fn non_empty_trimmed(value: String, label: &str) -> Result<String, ApiError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err(ApiError::bad_request(format!("{label} is required")))
    } else {
        Ok(value)
    }
}

fn non_empty_secret(value: String, label: &str) -> Result<String, ApiError> {
    if value.trim().is_empty() {
        Err(ApiError::bad_request(format!("{label} is required")))
    } else {
        Ok(value)
    }
}

async fn vault_status_summary(state: &AppState) -> Result<VaultStatusSummary, ApiError> {
    let record = state.store.load_vault_state()?;
    let unlocked = state.vault.lock().await.is_unlocked();
    Ok(vault_status_from_record(record, unlocked))
}

fn vault_status_from_record(
    record: Option<nucleus_storage::VaultStateRecord>,
    unlocked: bool,
) -> VaultStatusSummary {
    match record {
        Some(record) => VaultStatusSummary {
            initialized: true,
            locked: !unlocked,
            state: if unlocked { "unlocked" } else { "locked" }.to_string(),
            vault_id: record.vault_id,
            cipher: record.cipher,
            kdf_algorithm: record.kdf_algorithm,
            created_at: Some(record.created_at),
            updated_at: Some(record.updated_at),
        },
        None => VaultStatusSummary {
            initialized: false,
            locked: true,
            state: "uninitialized".to_string(),
            vault_id: String::new(),
            cipher: String::new(),
            kdf_algorithm: String::new(),
            created_at: None,
            updated_at: None,
        },
    }
}

fn vault_secret_summary(secret: nucleus_storage::VaultSecretRecord) -> VaultSecretSummary {
    VaultSecretSummary {
        id: secret.id,
        scope_kind: secret.scope_kind,
        scope_id: secret.scope_id,
        name: secret.name,
        description: secret.description,
        configured: true,
        version: secret.version,
        created_at: secret.created_at,
        updated_at: secret.updated_at,
        last_used_at: secret.last_used_at,
    }
}

fn vault_secret_policy_summary(policy: VaultSecretPolicyRecord) -> VaultSecretPolicySummary {
    VaultSecretPolicySummary {
        id: policy.id,
        secret_id: policy.secret_id,
        consumer_kind: policy.consumer_kind,
        consumer_id: policy.consumer_id,
        permission: policy.permission,
        approval_mode: policy.approval_mode,
        created_at: policy.created_at,
        updated_at: policy.updated_at,
    }
}

fn vault_api_error(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    if message.contains("not initialized") {
        ApiError::bad_request(message)
    } else if message.contains("already initialized") {
        ApiError::bad_request(message)
    } else if message.contains("locked") {
        ApiError::forbidden(message)
    } else if message.contains("at least") {
        ApiError::bad_request(message)
    } else if message.contains("invalid vault passphrase") {
        ApiError::unauthorized("invalid vault passphrase")
    } else {
        ApiError::internal_message(message)
    }
}

async fn record_vault_audit(state: &AppState, kind: &str, target: &str, summary: &str) {
    let _ = try_record_audit_event(
        state,
        AuditEventRecord {
            kind: kind.to_string(),
            target: target.to_string(),
            status: "success".to_string(),
            summary: summary.to_string(),
            detail: String::new(),
        },
    )
    .await;
}

async fn delete_skill(
    State(state): State<AppState>,
    Path(skill_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let skill_id = sanitize_registry_id(&skill_id, "skill id")?;
    state
        .store
        .delete_skill_manifest(&skill_id)
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_mcp_server(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let server_id = sanitize_registry_id(&server_id, "MCP server id")?;
    state
        .store
        .delete_mcp_server(&server_id)
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn discover_mcp_server_tools(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<Json<McpServerSummary>, ApiError> {
    let server_id = sanitize_registry_id(&server_id, "MCP server id")?;
    let record = state
        .store
        .list_mcp_server_records()
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| ApiError::not_found(format!("MCP server '{server_id}' was not found")))?;

    let result = discover_mcp_server(&state, &record).await;
    match result {
        Ok(discovered) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time should be monotonic")
                .as_secs() as i64;
            let updated_record = McpServerRecord {
                sync_status: "ready".to_string(),
                last_error: String::new(),
                last_synced_at: Some(now),
                updated_at: now,
                ..record.clone()
            };
            let summary = McpServerSummary {
                id: record.id.clone(),
                title: record.title.clone(),
                enabled: record.enabled,
                transport: record.transport.clone(),
                command: record.command.clone(),
                args: record.args.clone(),
                env_json: record.env_json.clone(),
                url: record.url.clone(),
                headers_json: record.headers_json.clone(),
                auth_kind: record.auth_kind.clone(),
                auth_ref: record.auth_ref.clone(),
                sync_status: "ready".to_string(),
                last_error: String::new(),
                last_synced_at: Some(now),
                tools: discovered.tools.clone(),
                resources: discovered.resources.clone(),
            };
            state
                .store
                .upsert_mcp_server_record(&updated_record, &summary.tools, &summary.resources)
                .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
            for tool in &discovered.tool_records {
                state
                    .store
                    .upsert_mcp_tool(tool)
                    .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
            }
            Ok(Json(redact_mcp_server(
                state
                    .store
                    .list_mcp_servers()
                    .map_err(|error| ApiError::from(anyhow::Error::from(error)))?
                    .into_iter()
                    .find(|server| server.id == record.id)
                    .expect("discovered MCP server should be present"),
            )))
        }
        Err(error) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time should be monotonic")
                .as_secs() as i64;
            let message = error.to_string();
            let updated_record = McpServerRecord {
                sync_status: mcp_sync_status_for_error(&message).to_string(),
                last_error: message.clone(),
                last_synced_at: Some(now),
                updated_at: now,
                ..record.clone()
            };
            state
                .store
                .upsert_mcp_server_record(&updated_record, &[], &[])
                .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
            Err(ApiError::bad_request(message))
        }
    }
}

async fn call_mcp_server_tool(
    State(state): State<AppState>,
    Path((server_id, tool_name)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let server_id = sanitize_registry_id(&server_id, "MCP server id")?;
    let tool_name = tool_name.trim().to_string();
    if tool_name.is_empty() {
        return Err(ApiError::bad_request("MCP tool name is required"));
    }
    let params = if body.is_empty() {
        json!({})
    } else {
        decode_json::<Value>(&body)?
    };
    let record = state
        .store
        .list_mcp_server_records()
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| ApiError::not_found(format!("MCP server '{server_id}' was not found")))?;
    if record.transport != "streamable-http" && record.transport != "http" {
        return Err(ApiError::bad_request(format!(
            "unsupported_transport: tool call API currently supports native remote MCPs, got '{}'",
            record.transport
        )));
    }
    let _ = mcp_http_request(&state, &record, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"nucleus","version":env!("CARGO_PKG_VERSION")}}}), None)
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let _ = mcp_http_request(
        &state,
        &record,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        None,
    )
    .await;
    let result = mcp_http_request(&state, &record, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool_name,"arguments":params}}), None)
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(Json(result))
}

#[derive(Debug)]
struct DiscoveredMcpCatalog {
    tools: Vec<NucleusToolDescriptor>,
    resources: Vec<String>,
    tool_records: Vec<McpToolRecord>,
}

async fn discover_mcp_server(
    state: &AppState,
    record: &McpServerRecord,
) -> anyhow::Result<DiscoveredMcpCatalog> {
    match record.transport.as_str() {
        "stdio" => discover_mcp_stdio_server(record).await,
        "streamable-http" | "http" => discover_mcp_http_server(state, record).await,
        "sse" => bail!("unsupported_transport: SSE MCP transport is not implemented"),
        other => bail!(
            "unsupported_transport: unsupported MCP transport '{}'",
            other
        ),
    }
}

async fn discover_mcp_stdio_server(
    record: &McpServerRecord,
) -> anyhow::Result<DiscoveredMcpCatalog> {
    if record.transport != "stdio" {
        anyhow::bail!("unsupported MCP transport '{}'", record.transport);
    }
    if record.command.trim().is_empty() {
        anyhow::bail!("MCP stdio command is required");
    }

    let mut command = Command::new(&record.command);
    command.args(&record.args);
    for (key, value) in record.env_json.as_object().cloned().unwrap_or_default() {
        let value = match value {
            Value::String(text) => text,
            other => other.to_string(),
        };
        command.env(key, value);
    }
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::null());

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

    stdin
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "nucleus", "version": env!("CARGO_PKG_VERSION")}
                }
            }))?
            .as_bytes(),
        )
        .await?;
    stdin
        .write_all(
            b"
",
        )
        .await?;

    stdin
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))?
            .as_bytes(),
        )
        .await?;
    stdin
        .write_all(
            b"
",
        )
        .await?;

    stdin
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }))?
            .as_bytes(),
        )
        .await?;
    stdin
        .write_all(
            b"
",
        )
        .await?;
    stdin.flush().await?;

    let mut tool_list: Option<Value> = None;
    for _ in 0..16 {
        let Some(line) = reader.next_line().await? else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).context("failed to parse MCP response")?;
        if value.get("id") == Some(&json!(2)) {
            tool_list = value.get("result").cloned();
            break;
        }
    }

    let _ = child.kill().await;
    let _ = child.wait().await;

    let result = tool_list.context("MCP server did not return a tools/list result")?;
    let tools_value = result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be monotonic")
        .as_secs() as i64;
    let mut tools = Vec::new();
    let mut tool_records = Vec::new();
    for item in tools_value {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            continue;
        }
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let input_schema = item
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let source = record.id.clone();
        let tool_id = format!("{}.{}", record.id, name);
        tools.push(NucleusToolDescriptor {
            id: tool_id.clone(),
            title: name.to_string(),
            description: description.clone(),
            input_schema: input_schema.clone(),
            source: source.clone(),
        });
        tool_records.push(McpToolRecord {
            id: tool_id,
            server_id: record.id.clone(),
            name: name.to_string(),
            description,
            input_schema,
            source,
            discovered_at: now,
            created_at: now,
            updated_at: now,
        });
    }

    Ok(DiscoveredMcpCatalog {
        tools,
        resources: Vec::new(),
        tool_records,
    })
}

async fn discover_mcp_http_server(
    state: &AppState,
    record: &McpServerRecord,
) -> anyhow::Result<DiscoveredMcpCatalog> {
    let result = mcp_http_request(state, record, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"nucleus","version":env!("CARGO_PKG_VERSION")}}}), None).await?;
    let _ = result;
    let _ = mcp_http_request(
        state,
        record,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        None,
    )
    .await;
    let result = mcp_http_request(
        state,
        record,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        None,
    )
    .await?;
    let tools_value = result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be monotonic")
        .as_secs() as i64;
    let mut tools = Vec::new();
    let mut tool_records = Vec::new();
    for item in tools_value {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            continue;
        }
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let input_schema = item
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let source = record.id.clone();
        let tool_id = format!("{}.{}", record.id, name);
        tools.push(NucleusToolDescriptor {
            id: tool_id.clone(),
            title: name.to_string(),
            description: description.clone(),
            input_schema: input_schema.clone(),
            source: source.clone(),
        });
        tool_records.push(McpToolRecord {
            id: tool_id,
            server_id: record.id.clone(),
            name: name.to_string(),
            description,
            input_schema,
            source,
            discovered_at: now,
            created_at: now,
            updated_at: now,
        });
    }
    Ok(DiscoveredMcpCatalog {
        tools,
        resources: Vec::new(),
        tool_records,
    })
}

async fn mcp_http_request(
    state: &AppState,
    record: &McpServerRecord,
    payload: Value,
    project_context: Option<&str>,
) -> anyhow::Result<Value> {
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
            if key.trim().is_empty() {
                continue;
            }
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

fn mcp_sync_status_for_error(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("auth_migration_required") {
        "auth_migration_required"
    } else if lower.contains("vault_locked") {
        "vault_locked"
    } else if lower.contains("vault_secret_missing") {
        "vault_secret_missing"
    } else if lower.contains("vault_policy_denied") {
        "vault_policy_denied"
    } else if lower.contains("vault_project_context_missing") {
        "vault_project_context_missing"
    } else if lower.contains("vault_project_context_mismatch") {
        "vault_project_context_mismatch"
    } else if lower.contains("missing_credentials") {
        "missing_credentials"
    } else if lower.contains("auth_required") {
        "auth_required"
    } else if lower.contains("unsupported_transport") {
        "unsupported_transport"
    } else {
        "error"
    }
}

pub(crate) const MCP_ENV_BEARER_MIGRATION_MESSAGE: &str = "auth_migration_required: bearer env auth is no longer supported; move the token into Vault and select bearer from Vault";

pub(crate) async fn resolve_mcp_vault_bearer_token(
    state: &AppState,
    record: &McpServerRecord,
    project_context: Option<&str>,
) -> anyhow::Result<String> {
    let reference = parse_vault_reference(&record.auth_ref)?;
    enforce_vault_reference_context(&reference, project_context)?;
    let secret = state
        .store
        .list_vault_secrets(Some(&reference.scope_kind), Some(&reference.scope_id))?
        .into_iter()
        .find(|secret| secret.name == reference.name)
        .ok_or_else(|| {
            anyhow::anyhow!("vault_secret_missing: configured Vault reference was not found")
        })?;

    let allowed = state
        .store
        .list_vault_secret_policies(&secret.id)?
        .into_iter()
        .any(|policy| {
            policy.consumer_kind == "mcp"
                && policy.consumer_id == record.id
                && policy.permission == "read"
                && policy.approval_mode == "allow"
        });
    if !allowed {
        anyhow::bail!(
            "vault_policy_denied: MCP server is not allowed to read this Vault reference"
        );
    }

    let mut vault = state.vault.lock().await;
    let token = vault
        .decrypt_secret(&state.store, &secret)
        .map_err(|error| match error.to_string().as_str() {
            "vault is locked" => {
                anyhow::anyhow!("vault_locked: unlock Vault before using this MCP credential")
            }
            _ => anyhow::anyhow!(
                "vault_secret_missing: configured Vault reference could not be resolved"
            ),
        })?;
    drop(vault);
    state
        .store
        .record_vault_secret_usage(&secret.id, "mcp", &record.id, "bearer_auth")?;
    Ok(token)
}

struct ParsedVaultReference {
    scope_kind: String,
    scope_id: String,
    name: String,
}

fn enforce_vault_reference_context(
    reference: &ParsedVaultReference,
    project_context: Option<&str>,
) -> anyhow::Result<()> {
    if reference.scope_kind != "project" {
        return Ok(());
    }
    let Some(context) = project_context
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        anyhow::bail!(
            "vault_project_context_missing: project Vault reference requires matching project context"
        );
    };
    if context != reference.scope_id {
        anyhow::bail!(
            "vault_project_context_mismatch: project Vault reference does not match current project context"
        );
    }
    Ok(())
}

fn parse_vault_reference(value: &str) -> anyhow::Result<ParsedVaultReference> {
    let value = value.trim();
    let Some(rest) = value.strip_prefix("vault://") else {
        anyhow::bail!("missing_credentials: vault_bearer auth_ref must be a vault:// reference");
    };
    let mut parts = rest.split('/').filter(|part| !part.trim().is_empty());
    match parts.next() {
        Some("workspace") => {
            let name = parts.collect::<Vec<_>>().join("/");
            let name = sanitize_registry_id(&name, "vault secret name")
                .map_err(|error| anyhow::anyhow!(error.message))?;
            Ok(ParsedVaultReference {
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                name,
            })
        }
        Some("project") => {
            let project_id = parts.next().ok_or_else(|| {
                anyhow::anyhow!(
                    "missing_credentials: project Vault reference requires a project id"
                )
            })?;
            let project_id = sanitize_registry_id(project_id, "vault project id")
                .map_err(|error| anyhow::anyhow!(error.message))?;
            let name = parts.collect::<Vec<_>>().join("/");
            let name = sanitize_registry_id(&name, "vault secret name")
                .map_err(|error| anyhow::anyhow!(error.message))?;
            Ok(ParsedVaultReference {
                scope_kind: "project".to_string(),
                scope_id: project_id,
                name,
            })
        }
        _ => anyhow::bail!("missing_credentials: unsupported Vault reference scope"),
    }
}

async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    Ok(Json(redact_session_summaries(state.store.list_sessions()?)))
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
    Ok(Json(redact_job_detail(
        agent::run_playbook(state, playbook_id).await?,
    )))
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
    let approval_mode = normalize_session_approval_mode(payload.approval_mode.as_deref())?;
    let execution_mode = normalize_session_execution_mode(payload.execution_mode.as_deref())?;
    let run_budget_mode = normalize_session_run_budget_mode(payload.run_budget_mode.as_deref())?;
    let workspace = prepare_session_workspace(
        &state,
        &session_id,
        &projects,
        payload.workspace_mode.as_deref(),
        payload.branch_name.as_deref(),
    )?;

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
        working_dir: workspace.working_dir.clone(),
        working_dir_kind: workspace.working_dir_kind.clone(),
        workspace_mode: workspace.workspace_mode.clone(),
        source_project_path: workspace.source_project_path.clone(),
        git_root: workspace.git_root.clone(),
        worktree_path: workspace.worktree_path.clone(),
        git_branch: workspace.git_branch.clone(),
        git_base_ref: workspace.git_base_ref.clone(),
        git_head: workspace.git_head.clone(),
        git_dirty: workspace.git_dirty,
        git_untracked_count: workspace.git_untracked_count,
        git_remote_tracking_branch: workspace.git_remote_tracking_branch.clone(),
        workspace_warnings: workspace.workspace_warnings.clone(),
        approval_mode,
        execution_mode,
        run_budget_mode,
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
                "provider={} model={} working_dir={} scope={} approval_mode={} execution_mode={} project_count={}",
                detail.session.provider,
                detail.session.model,
                detail.session.working_dir,
                detail.session.scope,
                detail.session.approval_mode,
                detail.session.execution_mode,
                detail.session.project_count
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(redact_session_detail(detail)))
}

async fn session_detail(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionDetail>, ApiError> {
    let mut detail = state.store.get_session(&session_id)?;
    refresh_session_workspace_warnings(&mut detail.session);
    Ok(Json(redact_session_detail(detail)))
}

async fn update_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<Json<SessionDetail>, ApiError> {
    let payload = decode_json::<UpdateSessionRequest>(&body)?;
    let before = state.store.get_session(&session_id)?;
    let workspace_affecting_update = payload.project_ids.is_some()
        || payload.primary_project_id.is_some()
        || payload.project_id.is_some()
        || payload.workspace_mode.is_some()
        || payload.branch_name.is_some();
    let project_selection = if workspace_affecting_update {
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
    let prepared_workspace = if let Some(selection) = project_selection.as_ref() {
        Some(prepare_session_workspace(
            &state,
            &session_id,
            selection,
            payload
                .workspace_mode
                .as_deref()
                .or(Some(before.session.workspace_mode.as_str())),
            payload.branch_name.as_deref(),
        )?)
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
        working_dir: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.working_dir.clone()),
        working_dir_kind: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.working_dir_kind.clone()),
        workspace_mode: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.workspace_mode.clone()),
        source_project_path: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.source_project_path.clone()),
        git_root: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.git_root.clone()),
        worktree_path: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.worktree_path.clone()),
        git_branch: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.git_branch.clone()),
        git_base_ref: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.git_base_ref.clone()),
        git_head: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.git_head.clone()),
        git_dirty: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.git_dirty),
        git_untracked_count: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.git_untracked_count),
        git_remote_tracking_branch: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.git_remote_tracking_branch.clone()),
        workspace_warnings: prepared_workspace
            .as_ref()
            .map(|workspace| workspace.workspace_warnings.clone()),
        approval_mode: match payload.approval_mode {
            Some(value) => Some(normalize_session_approval_mode(Some(&value))?),
            None => None,
        },
        execution_mode: match payload.execution_mode {
            Some(value) => Some(normalize_session_execution_mode(Some(&value))?),
            None => None,
        },
        run_budget_mode: match payload.run_budget_mode {
            Some(value) => Some(normalize_session_run_budget_mode(Some(&value))?),
            None => None,
        },
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
                "provider={} model={} working_dir={} state={} scope={} approval_mode={} execution_mode={} project_count={}",
                detail.session.provider,
                detail.session.model,
                detail.session.working_dir,
                detail.session.state,
                detail.session.scope,
                detail.session.approval_mode,
                detail.session.execution_mode,
                detail.session.project_count
            ),
        },
    )
    .await;
    let _ = publish_overview_event(&state).await;
    Ok(Json(redact_session_detail(detail)))
}

async fn delete_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let current = state.store.get_session(&session_id)?;
    cleanup_session_worktree_before_delete(&current.session)?;
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
    Ok(Json(redact_job_detail(state.store.get_job(&job_id)?)))
}

async fn cancel_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobDetail>, ApiError> {
    Ok(Json(redact_job_detail(
        agent::cancel_job(state, job_id).await?,
    )))
}

async fn resume_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobDetail>, ApiError> {
    Ok(Json(redact_job_detail(
        agent::resume_job(state, job_id).await?,
    )))
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
    Ok(Json(redact_job_detail(
        agent::approve_request(state, approval_id, payload.note).await?,
    )))
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
    Ok(Json(redact_job_detail(
        agent::deny_request(state, approval_id, payload.note).await?,
    )))
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

    Ok(Json(redact_session_detail(
        agent::start_prompt_job(
            state,
            session_id,
            payload,
            current,
            execution_prompt,
            compiler_role,
        )
        .await?,
    )))
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

#[derive(Debug, Deserialize)]
struct InstanceLogQuery {
    category: Option<String>,
    level: Option<String>,
    before: Option<i64>,
    before_id: Option<i64>,
    limit: Option<usize>,
}

async fn instance_logs(
    State(state): State<AppState>,
    Query(query): Query<InstanceLogQuery>,
) -> Result<Json<InstanceLogListResponse>, ApiError> {
    let limit = resolve_log_limit(query.limit)?;
    let category = normalize_log_filter(query.category);
    let level = normalize_log_filter(query.level).map(|value| value.to_lowercase());
    let before = resolve_log_cursor(query.before, query.before_id)?;
    let records =
        state
            .store
            .list_instance_logs(category.as_deref(), level.as_deref(), before, limit)?;
    let categories = state.store.list_instance_log_categories()?;
    let next_cursor = records.last().map(|record| (record.timestamp, record.id));

    Ok(Json(InstanceLogListResponse {
        records,
        categories,
        logs_dir: state.store.logs_dir_path().display().to_string(),
        retention: format!(
            "{INSTANCE_LOG_RETENTION_DAYS} days, {INSTANCE_LOG_MAX_ROWS} recent records, JSONL rotated around 1 MiB with 3 history files"
        ),
        next_before: next_cursor.map(|(timestamp, _)| timestamp),
        next_before_id: next_cursor.map(|(_, id)| id),
    }))
}

async fn instance_log_categories(
    State(state): State<AppState>,
) -> Result<Json<InstanceLogCategoriesResponse>, ApiError> {
    Ok(Json(InstanceLogCategoriesResponse {
        categories: state.store.list_instance_log_categories()?,
    }))
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
    let _ = state
        .events
        .send(DaemonEvent::SessionUpdated(redact_session_detail(detail)));
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

async fn build_settings_summary(state: &AppState, headers: &HeaderMap) -> SettingsSummary {
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
        security: security::build_security_posture(&instance.daemon_bind, headers),
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
        router_profiles: redact_router_profiles(enrich_router_profiles(
            state.store.list_router_profiles()?,
            &runtimes,
        )),
        workspace: redact_workspace_summary(state.store.workspace()?),
        sessions: redact_session_summaries(state.store.list_sessions()?),
        host,
        storage: state.store.storage_summary(),
    })
}

fn redact_session_summaries(sessions: Vec<SessionSummary>) -> Vec<SessionSummary> {
    sessions.into_iter().map(redact_session_summary).collect()
}

fn redact_session_detail(mut detail: SessionDetail) -> SessionDetail {
    detail.session = redact_session_summary(detail.session);
    detail
}

fn redact_session_summary(mut session: SessionSummary) -> SessionSummary {
    session.provider_api_key.clear();
    session
}

fn redact_job_detail(mut detail: JobDetail) -> JobDetail {
    for worker in &mut detail.workers {
        worker.provider_api_key.clear();
    }

    let redactor = security::RedactionSet::new();
    for call in &mut detail.tool_calls {
        call.args_json = redactor.redact_json(&call.args_json);
        call.result_json = call
            .result_json
            .as_ref()
            .map(|value| redactor.redact_json(value));
        call.error_detail = redactor.redact_text(&call.error_detail);
    }
    for event in &mut detail.events {
        event.detail = redactor.redact_text(&event.detail);
        event.data_json = redactor.redact_json(&event.data_json);
    }
    detail
}

fn redact_workspace_summary(mut workspace: WorkspaceSummary) -> WorkspaceSummary {
    workspace.profiles = workspace
        .profiles
        .into_iter()
        .map(redact_workspace_profile)
        .collect();
    workspace
}

fn redact_workspace_profile(mut profile: WorkspaceProfileSummary) -> WorkspaceProfileSummary {
    profile.main = redact_workspace_model_config(profile.main);
    profile.utility = redact_workspace_model_config(profile.utility);
    profile
}

fn redact_workspace_model_config(mut config: WorkspaceModelConfig) -> WorkspaceModelConfig {
    config.api_key.clear();
    config
}

fn redact_router_profiles(profiles: Vec<RouterProfileSummary>) -> Vec<RouterProfileSummary> {
    profiles.into_iter().map(redact_router_profile).collect()
}

fn redact_router_profile(mut profile: RouterProfileSummary) -> RouterProfileSummary {
    for target in &mut profile.targets {
        target.api_key.clear();
    }
    profile
}

fn redact_mcp_servers(servers: Vec<McpServerSummary>) -> Vec<McpServerSummary> {
    servers.into_iter().map(redact_mcp_server).collect()
}

fn redact_mcp_server(mut server: McpServerSummary) -> McpServerSummary {
    let redactor = security::RedactionSet::new();
    server.env_json = redactor.redact_json(&server.env_json);
    server.headers_json = redactor.redact_json(&server.headers_json);
    server.last_error = redactor.redact_text(&server.last_error);
    server
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

fn hydrate_skill_instructions_from_include(skill: &mut SkillManifest) {
    if !skill.instructions.trim().is_empty() {
        return;
    }
    let Some(path) = skill_instruction_path(skill) else {
        return;
    };
    if let Ok(content) = fs::read_to_string(path) {
        skill.instructions = content;
    }
}

fn sync_skill_instructions_to_file(skill: &SkillManifest) {
    if skill.instructions.trim().is_empty() {
        return;
    }
    let Some(path) = skill_instruction_path(skill) else {
        return;
    };
    let _ = fs::write(path, &skill.instructions);
}

fn skill_instruction_path(skill: &SkillManifest) -> Option<PathBuf> {
    skill.include_paths.iter().find_map(|include_path| {
        let path = PathBuf::from(include_path.trim());
        if path.is_absolute() && path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md")
        {
            Some(path)
        } else {
            None
        }
    })
}

#[derive(Debug, Clone)]
struct ImportSourceMeta {
    source_kind: String,
    source_url: String,
    source_repo_url: String,
    source_owner: String,
    source_repo: String,
    source_ref: String,
    source_parent_path: String,
    source_skill_path: String,
    source_commit: String,
}

async fn import_skills(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SkillImportResponse>, ApiError> {
    let payload = decode_json::<SkillImportRequest>(&body)?;
    Ok(Json(import_skills_from_source(&state, payload).await?))
}

async fn scan_reconcile_skills(
    State(state): State<AppState>,
) -> Result<Json<SkillReconcileScanResponse>, ApiError> {
    Ok(Json(scan_skill_folders(&state)?))
}

async fn reconcile_skills(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SkillImportResponse>, ApiError> {
    let payload = if body.is_empty() {
        SkillReconcileRequest::default()
    } else {
        decode_json::<SkillReconcileRequest>(&body)?
    };
    Ok(Json(reconcile_skill_folders(&state, &payload.skill_ids)?))
}

async fn check_skill_update(
    State(state): State<AppState>,
    Path(skill_id): Path<String>,
) -> Result<Json<SkillInstallResult>, ApiError> {
    let skill_id = sanitize_registry_id(&skill_id, "skill id")?;
    Ok(Json(check_one_skill_update(&state, &skill_id).await?))
}

async fn check_skill_updates(
    State(state): State<AppState>,
) -> Result<Json<SkillImportResponse>, ApiError> {
    let mut response = SkillImportResponse {
        installed: Vec::new(),
        errors: Vec::new(),
    };
    for skill in state.store.list_skill_manifests()? {
        match check_one_skill_update(&state, &skill.id).await {
            Ok(result) => response.installed.push(result),
            Err(error) => response
                .errors
                .push(format!("{}: {}", skill.id, error.message)),
        }
    }
    Ok(Json(response))
}

async fn import_skills_from_source(
    state: &AppState,
    payload: SkillImportRequest,
) -> Result<SkillImportResponse, ApiError> {
    let source = payload.source.trim();
    if source.is_empty() {
        return Err(ApiError::bad_request("skill import source is required"));
    }
    let scope_kind = default_scope_kind(&payload.scope_kind)?;
    let scope_id = default_scope_id(&payload.scope_id);
    let mut response = SkillImportResponse {
        installed: Vec::new(),
        errors: Vec::new(),
    };

    if source.starts_with("https://github.com/") {
        let mut github = parse_github_tree_url(source)?;
        if let Ok(commit) = resolve_github_commit(&github).await {
            github.source_commit = commit;
        }
        let temp = std::env::temp_dir().join(format!("nucleus-skill-import-{}", Uuid::new_v4()));
        let result = import_github_skill_dirs(state, &github, &temp, &scope_kind, &scope_id).await;
        let _ = fs::remove_dir_all(&temp);
        match result {
            Ok(installed) => response.installed.extend(installed),
            Err(error) => response.errors.push(error.message),
        }
    } else {
        let base = PathBuf::from(source);
        let meta = ImportSourceMeta {
            source_kind: "local".to_string(),
            source_url: source.to_string(),
            source_repo_url: String::new(),
            source_owner: String::new(),
            source_repo: String::new(),
            source_ref: String::new(),
            source_parent_path: source.to_string(),
            source_skill_path: String::new(),
            source_commit: String::new(),
        };
        import_discovered_dirs(
            state,
            &base,
            &base,
            &meta,
            &scope_kind,
            &scope_id,
            &mut response,
        )?;
    }

    if response.installed.is_empty() && response.errors.is_empty() {
        response
            .errors
            .push("no skill folders with SKILL.md were found".to_string());
    }
    Ok(response)
}

async fn import_github_skill_dirs(
    state: &AppState,
    github: &ImportSourceMeta,
    temp: &FsPath,
    scope_kind: &str,
    scope_id: &str,
) -> Result<Vec<SkillInstallResult>, ApiError> {
    fs::create_dir_all(temp).map_err(api_io_error)?;
    let archive = reqwest::get(github_codeload_url(github))
        .await
        .map_err(api_io_error)?
        .bytes()
        .await
        .map_err(api_io_error)?;
    safe_unpack_tar_gz(&archive, temp).map_err(api_io_error)?;
    let root = archive_root_dir(temp)?;
    let base = root.join(&github.source_parent_path);
    let mut installed = Vec::new();
    for dir in discover_skill_dirs(&base).map_err(api_io_error)? {
        let mut meta = github.clone();
        meta.source_skill_path = pathdiff(&root, &dir);
        installed.push(install_skill_dir(state, &dir, &meta, scope_kind, scope_id)?);
    }
    Ok(installed)
}

fn import_discovered_dirs(
    state: &AppState,
    base: &FsPath,
    root: &FsPath,
    meta: &ImportSourceMeta,
    scope_kind: &str,
    scope_id: &str,
    response: &mut SkillImportResponse,
) -> Result<(), ApiError> {
    for dir in discover_skill_dirs(base).map_err(api_io_error)? {
        let mut child_meta = meta.clone();
        child_meta.source_skill_path = pathdiff(root, &dir);
        match install_skill_dir(state, &dir, &child_meta, scope_kind, scope_id) {
            Ok(result) => response.installed.push(result),
            Err(error) => response.errors.push(error.message),
        }
    }
    Ok(())
}

fn scan_skill_folders(state: &AppState) -> Result<SkillReconcileScanResponse, ApiError> {
    let skills_dir = state.store.state_dir_path().join("skills");
    let mut response = SkillReconcileScanResponse {
        skills_dir: display(&skills_dir),
        candidates: Vec::new(),
        errors: Vec::new(),
    };
    if !skills_dir.exists() {
        return Ok(response);
    }
    let registered = state
        .store
        .list_skill_manifests()?
        .into_iter()
        .map(|skill| skill.id)
        .collect::<BTreeSet<_>>();
    for dir in discover_skill_dirs(&skills_dir).map_err(api_io_error)? {
        let parsed = match parse_skill_file(&dir) {
            Ok(parsed) => parsed,
            Err(error) => {
                response.errors.push(error.message);
                continue;
            }
        };
        let skill_id = match derive_skill_id(&dir, &parsed) {
            Ok(skill_id) => skill_id,
            Err(error) => {
                response.errors.push(error.message);
                continue;
            }
        };
        let title = parsed
            .get("name")
            .or_else(|| parsed.get("title"))
            .cloned()
            .unwrap_or_else(|| titleize(&skill_id));
        response.candidates.push(SkillReconcileCandidate {
            skill_id: skill_id.clone(),
            title,
            path: display(&dir),
            already_registered: registered.contains(&skill_id),
        });
    }
    Ok(response)
}

fn reconcile_skill_folders(
    state: &AppState,
    requested_skill_ids: &[String],
) -> Result<SkillImportResponse, ApiError> {
    let skills_dir = state.store.state_dir_path().join("skills");
    let mut response = SkillImportResponse {
        installed: Vec::new(),
        errors: Vec::new(),
    };
    if !skills_dir.exists() {
        return Ok(response);
    }
    let requested = requested_skill_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<BTreeSet<_>>();
    for dir in discover_skill_dirs(&skills_dir).map_err(api_io_error)? {
        let parsed = match parse_skill_file(&dir) {
            Ok(parsed) => parsed,
            Err(error) => {
                response.errors.push(error.message);
                continue;
            }
        };
        let id = match derive_skill_id(&dir, &parsed) {
            Ok(id) => id,
            Err(error) => {
                response.errors.push(error.message);
                continue;
            }
        };
        if !requested.is_empty() && !requested.contains(&id) {
            continue;
        }
        let meta = ImportSourceMeta {
            source_kind: "unknown".to_string(),
            source_url: String::new(),
            source_repo_url: String::new(),
            source_owner: String::new(),
            source_repo: String::new(),
            source_ref: String::new(),
            source_parent_path: String::new(),
            source_skill_path: id.clone(),
            source_commit: String::new(),
        };
        match register_skill_dir(state, &dir, Some(id), &meta, "workspace", "default", false) {
            Ok(result) => response.installed.push(result),
            Err(error) => response.errors.push(error.message),
        }
    }
    Ok(response)
}

fn discover_skill_dirs(base: &FsPath) -> anyhow::Result<Vec<PathBuf>> {
    if base.join("SKILL.md").is_file() {
        return Ok(vec![base.to_path_buf()]);
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(base).with_context(|| format!("failed to read {}", base.display()))? {
        let path = entry?.path();
        if path.is_dir() && path.join("SKILL.md").is_file() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn install_skill_dir(
    state: &AppState,
    source_dir: &FsPath,
    meta: &ImportSourceMeta,
    scope_kind: &str,
    scope_id: &str,
) -> Result<SkillInstallResult, ApiError> {
    let parsed = parse_skill_file(source_dir)?;
    let skill_id = derive_skill_id(source_dir, &parsed)?;
    let skills_dir = state.store.state_dir_path().join("skills");
    fs::create_dir_all(&skills_dir).map_err(api_io_error)?;
    let dest = skills_dir.join(&skill_id);
    let staging = skills_dir.join(format!(".{}.staging-{}", skill_id, Uuid::new_v4()));
    copy_dir_all(source_dir, &staging).map_err(api_io_error)?;
    if dest.exists() {
        let backup = skills_dir.join(format!(".{}.backup-{}", skill_id, Uuid::new_v4()));
        fs::rename(&dest, &backup).map_err(api_io_error)?;
        if let Err(error) = fs::rename(&staging, &dest) {
            let _ = fs::rename(&backup, &dest);
            let _ = fs::remove_dir_all(&staging);
            return Err(api_io_error(error));
        }
        let _ = fs::remove_dir_all(backup);
    } else {
        fs::rename(&staging, &dest).map_err(api_io_error)?;
    }
    register_skill_dir(
        state,
        &dest,
        Some(skill_id),
        meta,
        scope_kind,
        scope_id,
        true,
    )
}

fn register_skill_dir(
    state: &AppState,
    dir: &FsPath,
    forced_id: Option<String>,
    meta: &ImportSourceMeta,
    scope_kind: &str,
    scope_id: &str,
    files_copied: bool,
) -> Result<SkillInstallResult, ApiError> {
    let parsed = parse_skill_file(dir)?;
    let instructions = fs::read_to_string(dir.join("SKILL.md")).map_err(api_io_error)?;
    if instructions.trim().is_empty() {
        return Err(ApiError::bad_request(format!(
            "{} has an empty SKILL.md",
            dir.display()
        )));
    }
    let skill_id = match forced_id {
        Some(id) => sanitize_registry_id(&id, "skill id")?,
        None => derive_skill_id(dir, &parsed)?,
    };
    let existing_manifest = state
        .store
        .list_skill_manifests()?
        .into_iter()
        .find(|skill| skill.id == skill_id);
    let title = parsed
        .get("name")
        .or_else(|| parsed.get("title"))
        .cloned()
        .or_else(|| existing_manifest.as_ref().map(|skill| skill.title.clone()))
        .unwrap_or_else(|| titleize(&skill_id));
    let description = parsed
        .get("description")
        .cloned()
        .or_else(|| {
            existing_manifest
                .as_ref()
                .map(|skill| skill.description.clone())
        })
        .unwrap_or_default();
    let activation_mode = existing_manifest
        .as_ref()
        .map(|skill| skill.activation_mode.clone())
        .or_else(|| parsed.get("activation_mode").cloned())
        .unwrap_or_else(|| "manual".to_string());
    let manifest = sanitize_skill_manifest(SkillManifest {
        id: skill_id.clone(),
        title: title.clone(),
        description,
        instructions: instructions.clone(),
        activation_mode,
        triggers: existing_manifest
            .as_ref()
            .map(|skill| skill.triggers.clone())
            .unwrap_or_default(),
        include_paths: vec![format!("skills/{}/SKILL.md", skill_id)],
        required_tools: Vec::new(),
        required_mcps: Vec::new(),
        project_filters: Vec::new(),
        enabled: existing_manifest
            .as_ref()
            .map(|skill| skill.enabled)
            .unwrap_or(true),
    })?;
    state.store.upsert_skill_manifest(&manifest)?;

    let now = unix_timestamp();
    let checksum = checksum_dir(dir).map_err(api_io_error)?;
    let package_id = format!("nucleus.{}", skill_id);
    let installation_id = format!("workspace.nucleus.{}", skill_id);
    let existing_package = state
        .store
        .list_skill_packages()?
        .into_iter()
        .find(|package| package.id == package_id);
    let package = SkillPackageRecord {
        id: package_id.clone(),
        name: title,
        version: existing_package
            .as_ref()
            .map(|package| package.version.clone())
            .filter(|version| !version.is_empty())
            .unwrap_or_else(|| "source".to_string()),
        manifest_json: serde_json::to_value(&manifest).unwrap_or_else(|_| json!({})),
        instructions,
        source_kind: meta.source_kind.clone(),
        source_url: meta.source_url.clone(),
        source_repo_url: meta.source_repo_url.clone(),
        source_owner: meta.source_owner.clone(),
        source_repo: meta.source_repo.clone(),
        source_ref: meta.source_ref.clone(),
        source_parent_path: meta.source_parent_path.clone(),
        source_skill_path: meta.source_skill_path.clone(),
        source_commit: meta.source_commit.clone(),
        imported_at: existing_package
            .as_ref()
            .and_then(|package| package.imported_at)
            .or(Some(now)),
        last_checked_at: existing_package
            .as_ref()
            .and_then(|package| package.last_checked_at),
        latest_source_commit: existing_package
            .as_ref()
            .map(|package| package.latest_source_commit.clone())
            .unwrap_or_default(),
        update_status: "current".to_string(),
        content_checksum: checksum.clone(),
        dirty_status: "clean".to_string(),
        created_at: existing_package
            .as_ref()
            .map(|package| package.created_at)
            .unwrap_or(now),
        updated_at: now,
    };
    state.store.upsert_skill_package(&package)?;

    let existing_installation = state
        .store
        .list_skill_installations()?
        .into_iter()
        .find(|installation| installation.id == installation_id);
    state
        .store
        .upsert_skill_installation(&SkillInstallationRecord {
            id: installation_id.clone(),
            package_id: package_id.clone(),
            scope_kind: existing_installation
                .as_ref()
                .map(|installation| installation.scope_kind.clone())
                .unwrap_or_else(|| scope_kind.to_string()),
            scope_id: existing_installation
                .as_ref()
                .map(|installation| installation.scope_id.clone())
                .unwrap_or_else(|| scope_id.to_string()),
            enabled: existing_installation
                .as_ref()
                .map(|installation| installation.enabled)
                .unwrap_or(true),
            pinned_version: existing_installation
                .and_then(|installation| installation.pinned_version),
            created_at: now,
            updated_at: now,
        })?;

    let result = verify_install(
        state,
        &skill_id,
        &package_id,
        &installation_id,
        files_copied,
        &package,
    );
    if result.status != "installed" {
        return Err(ApiError::internal_message(format!(
            "skill {} was only partially installed",
            skill_id
        )));
    }
    Ok(result)
}

fn verify_install(
    state: &AppState,
    skill_id: &str,
    package_id: &str,
    installation_id: &str,
    files_copied: bool,
    package: &SkillPackageRecord,
) -> SkillInstallResult {
    let manifests = state.store.list_skill_manifests().unwrap_or_default();
    let packages = state.store.list_skill_packages().unwrap_or_default();
    let installs = state.store.list_skill_installations().unwrap_or_default();
    let manifest_registered = manifests.iter().any(|skill| skill.id == skill_id);
    let package_registered = packages.iter().any(|stored| stored.id == package_id);
    let installation_registered = installs
        .iter()
        .any(|installation| installation.id == installation_id);
    let files_exist = state
        .store
        .state_dir_path()
        .join("skills")
        .join(skill_id)
        .join("SKILL.md")
        .is_file();
    let installed =
        files_exist && manifest_registered && package_registered && installation_registered;
    SkillInstallResult {
        skill_id: skill_id.to_string(),
        package_id: package_id.to_string(),
        installation_id: installation_id.to_string(),
        source_kind: package.source_kind.clone(),
        source_url: package.source_url.clone(),
        source_repo: if package.source_owner.is_empty() {
            package.source_repo.clone()
        } else {
            format!("{}/{}", package.source_owner, package.source_repo)
        },
        source_ref: package.source_ref.clone(),
        source_skill_path: package.source_skill_path.clone(),
        source_commit: package.source_commit.clone(),
        content_checksum: package.content_checksum.clone(),
        dirty_status: package.dirty_status.clone(),
        update_status: package.update_status.clone(),
        status: if installed { "installed" } else { "partial" }.to_string(),
        verification: SkillInstallVerification {
            files_copied: files_copied || files_exist,
            manifest_registered,
            package_registered,
            installation_registered,
            instructions_non_empty: !package.instructions.trim().is_empty(),
            source_metadata_stored: package.source_kind != "github"
                || !package.source_url.is_empty(),
            checksum_recorded: !package.content_checksum.is_empty(),
        },
    }
}

fn parse_github_tree_url(source: &str) -> Result<ImportSourceMeta, ApiError> {
    let url = url::Url::parse(source).map_err(|_| ApiError::bad_request("invalid GitHub URL"))?;
    if url.host_str() != Some("github.com") {
        return Err(ApiError::bad_request("only github.com URLs are supported"));
    }
    let parts: Vec<_> = url
        .path_segments()
        .ok_or_else(|| ApiError::bad_request("invalid GitHub URL"))?
        .collect();
    if parts.len() < 2 {
        return Err(ApiError::bad_request(
            "GitHub URL must include owner and repo",
        ));
    }
    let owner = parts[0].to_string();
    let repo = parts[1].trim_end_matches(".git").to_string();
    let (reference, path) = if parts.get(2) == Some(&"tree") && parts.len() >= 4 {
        (parts[3].to_string(), parts[4..].join("/"))
    } else {
        ("main".to_string(), String::new())
    };
    validate_relative_archive_path(&path)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(ImportSourceMeta {
        source_kind: "github".to_string(),
        source_url: source.to_string(),
        source_repo_url: format!("https://github.com/{owner}/{repo}"),
        source_owner: owner,
        source_repo: repo,
        source_ref: reference,
        source_parent_path: path,
        source_skill_path: String::new(),
        source_commit: String::new(),
    })
}

fn parse_skill_file(dir: &FsPath) -> Result<BTreeMap<String, String>, ApiError> {
    let text = fs::read_to_string(dir.join("SKILL.md")).map_err(|_| {
        ApiError::bad_request(format!("{} does not contain SKILL.md", dir.display()))
    })?;
    let mut out = BTreeMap::new();
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((key, value)) = line.split_once(':') {
                    out.insert(
                        key.trim().to_string(),
                        value.trim().trim_matches('"').to_string(),
                    );
                }
            }
        }
    }
    if !out.contains_key("title") && !out.contains_key("name") {
        if let Some(line) = text.lines().find(|line| line.starts_with("# ")) {
            out.insert(
                "title".to_string(),
                line.trim_start_matches("# ").trim().to_string(),
            );
        }
    }
    Ok(out)
}

fn derive_skill_id(dir: &FsPath, parsed: &BTreeMap<String, String>) -> Result<String, ApiError> {
    let raw = parsed
        .get("id")
        .cloned()
        .or_else(|| {
            dir.file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "skill".to_string());
    sanitize_registry_id(&slugify_skill_id(&raw), "skill id")
}

fn slugify_skill_id(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if matches!(ch, '.' | '_' | '-') {
            slug.push(ch);
            previous_dash = ch == '-';
        } else if !previous_dash {
            slug.push('-');
            previous_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn copy_dir_all(src: &FsPath, dst: &FsPath) -> anyhow::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn checksum_dir(dir: &FsPath) -> anyhow::Result<String> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, path) in files {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(fs::read(path)?);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_files(
    root: &FsPath,
    dir: &FsPath,
    out: &mut Vec<(String, PathBuf)>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(root, &path, out)?;
        } else if entry.file_type()?.is_file() {
            out.push((pathdiff(root, &path), path));
        }
    }
    Ok(())
}

fn pathdiff(root: &FsPath, path: &FsPath) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string()
}

fn titleize(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn check_one_skill_update(
    state: &AppState,
    skill_id: &str,
) -> Result<SkillInstallResult, ApiError> {
    let package_id = format!("nucleus.{skill_id}");
    let mut package = state
        .store
        .list_skill_packages()?
        .into_iter()
        .find(|package| package.id == package_id)
        .ok_or_else(|| {
            ApiError::bad_request(format!("skill package {package_id} was not found"))
        })?;
    let dir = state.store.state_dir_path().join("skills").join(skill_id);
    package.dirty_status = if dir.exists() {
        if checksum_dir(&dir).map_err(api_io_error)? == package.content_checksum {
            "clean".to_string()
        } else {
            "modified".to_string()
        }
    } else {
        "unknown".to_string()
    };
    package.last_checked_at = Some(unix_timestamp());
    if package.source_kind == "github" {
        match fetch_github_skill_checksum(&package).await {
            Ok((latest_checksum, latest_commit)) => {
                package.latest_source_commit =
                    latest_commit.unwrap_or_else(|| latest_checksum.clone());
                package.update_status = if latest_checksum == package.content_checksum {
                    "current".to_string()
                } else {
                    "update_available".to_string()
                };
            }
            Err(error) => {
                package.update_status = "source_error".to_string();
                package.latest_source_commit = error.to_string();
            }
        }
    } else {
        package.update_status = "unknown".to_string();
    }
    package.updated_at = unix_timestamp();
    state.store.upsert_skill_package(&package)?;
    Ok(verify_install(
        state,
        skill_id,
        &package_id,
        &format!("workspace.nucleus.{skill_id}"),
        false,
        &package,
    ))
}

async fn fetch_github_skill_checksum(
    package: &SkillPackageRecord,
) -> anyhow::Result<(String, Option<String>)> {
    if package.source_owner.is_empty()
        || package.source_repo.is_empty()
        || package.source_ref.is_empty()
        || package.source_skill_path.is_empty()
    {
        bail!("missing GitHub source metadata");
    }
    let meta = ImportSourceMeta {
        source_kind: "github".to_string(),
        source_url: package.source_url.clone(),
        source_repo_url: package.source_repo_url.clone(),
        source_owner: package.source_owner.clone(),
        source_repo: package.source_repo.clone(),
        source_ref: package.source_ref.clone(),
        source_parent_path: package.source_parent_path.clone(),
        source_skill_path: package.source_skill_path.clone(),
        source_commit: package.source_commit.clone(),
    };
    let latest_commit = resolve_github_commit(&meta).await.ok();
    let temp = std::env::temp_dir().join(format!("nucleus-skill-check-{}", Uuid::new_v4()));
    let result = async {
        fs::create_dir_all(&temp)?;
        let archive = reqwest::get(github_codeload_url(&meta))
            .await?
            .bytes()
            .await?;
        safe_unpack_tar_gz(&archive, &temp)?;
        let root = archive_root_dir(&temp).map_err(|error| anyhow::anyhow!(error.message))?;
        let checksum = checksum_dir(&root.join(&package.source_skill_path))?;
        Ok::<_, anyhow::Error>((checksum, latest_commit))
    }
    .await;
    let _ = fs::remove_dir_all(&temp);
    result
}

fn github_codeload_url(meta: &ImportSourceMeta) -> String {
    format!(
        "https://codeload.github.com/{}/{}/tar.gz/{}",
        meta.source_owner, meta.source_repo, meta.source_ref
    )
}

async fn resolve_github_commit(meta: &ImportSourceMeta) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct CommitResponse {
        sha: String,
    }
    let url = format!(
        "https://api.github.com/repos/{}/{}/commits/{}",
        meta.source_owner, meta.source_repo, meta.source_ref
    );
    let response = reqwest::Client::new()
        .get(url)
        .header("User-Agent", "nucleus-skill-import")
        .send()
        .await?
        .error_for_status()?
        .json::<CommitResponse>()
        .await?;
    Ok(response.sha)
}

fn safe_unpack_tar_gz(bytes: &[u8], destination: &FsPath) -> anyhow::Result<()> {
    let decoder = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            if matches!(entry_type.as_byte(), b'g' | b'x' | b'1' | b'2') {
                continue;
            }
            bail!(
                "unsupported archive entry type for {}",
                entry.path()?.display()
            );
        }
        let path = entry.path()?.to_path_buf();
        validate_relative_archive_path(&path)?;
        let out = destination.join(&path);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(out)?;
    }
    Ok(())
}

fn validate_relative_archive_path(path: impl AsRef<FsPath>) -> anyhow::Result<()> {
    use std::path::Component;
    let path = path.as_ref();
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe archive path {}", path.display());
            }
        }
    }
    Ok(())
}

fn archive_root_dir(temp: &FsPath) -> Result<PathBuf, ApiError> {
    let mut roots = fs::read_dir(temp)
        .map_err(api_io_error)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    roots.sort();
    roots
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::bad_request("GitHub archive did not contain a repository root"))
}

fn default_scope_kind(scope_kind: &str) -> Result<String, ApiError> {
    let scope_kind = if scope_kind.trim().is_empty() {
        "workspace"
    } else {
        scope_kind.trim()
    };
    match scope_kind {
        "workspace" | "project" | "session" => Ok(scope_kind.to_string()),
        _ => Err(ApiError::bad_request(
            "skill import scope_kind must be workspace, project, or session",
        )),
    }
}

fn default_scope_id(scope_id: &str) -> String {
    if scope_id.trim().is_empty() {
        "default".to_string()
    } else {
        scope_id.trim().to_string()
    }
}

fn api_io_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::internal_message(error.to_string())
}

fn sanitize_skill_manifest(mut manifest: SkillManifest) -> Result<SkillManifest, ApiError> {
    manifest.id = sanitize_registry_id(&manifest.id, "skill id")?;
    manifest.title = required_trimmed(manifest.title, "skill title")?;
    manifest.description = manifest.description.trim().to_string();
    manifest.instructions = manifest.instructions.trim().to_string();
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
    server.auth_kind = normalize_mcp_auth_kind_for_write(&server.auth_kind)?;
    server.auth_ref = normalize_mcp_auth_ref_for_write(&server.auth_kind, &server.auth_ref)?;
    server.resources = sanitize_string_list(server.resources);
    server.tools = server
        .tools
        .into_iter()
        .map(sanitize_tool_descriptor)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(server)
}

fn normalize_mcp_auth_kind_for_write(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    match value {
        "" | "none" => Ok("none".to_string()),
        "vault_bearer" | "static_headers" | "oauth" | "device" => Ok(value.to_string()),
        "bearer_env" | "env_bearer" => Err(ApiError::bad_request(
            "auth_migration_required: bearer env auth is no longer supported; move the token into Vault and select bearer from Vault",
        )),
        _ => Err(ApiError::bad_request(
            "MCP auth_kind must be none, vault_bearer, static_headers, oauth, or device",
        )),
    }
}

fn normalize_mcp_auth_ref_for_write(auth_kind: &str, value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if auth_kind != "vault_bearer" || value.is_empty() {
        return Ok(value.to_string());
    }
    if value.starts_with("vault://") {
        parse_vault_reference(value).map_err(|error| ApiError::bad_request(error.to_string()))?;
        return Ok(value.to_string());
    }
    let name = sanitize_registry_id(value, "vault secret name")?;
    Ok(format!("vault://workspace/{name}"))
}

fn build_skill_package_record(
    payload: SkillPackageUpsertRequest,
    path_id: Option<String>,
) -> Result<SkillPackageRecord, ApiError> {
    let id = path_id
        .or(payload.id)
        .ok_or_else(|| ApiError::bad_request("skill package id is required"))?;
    let id = sanitize_registry_id(&id, "skill package id")?;
    let name = required_trimmed(payload.name, "skill package name")?;
    let version = required_trimmed(payload.version, "skill package version")?;
    let instructions = required_trimmed(payload.instructions, "skill package instructions")?;
    let now = unix_timestamp();
    Ok(SkillPackageRecord {
        id,
        name,
        version,
        manifest_json: payload.manifest_json,
        instructions,
        created_at: now,
        source_kind: if payload.source_kind.trim().is_empty() {
            "manual".to_string()
        } else {
            payload.source_kind.trim().to_string()
        },
        source_url: payload.source_url.trim().to_string(),
        source_repo_url: payload.source_repo_url.trim().to_string(),
        source_owner: payload.source_owner.trim().to_string(),
        source_repo: payload.source_repo.trim().to_string(),
        source_ref: payload.source_ref.trim().to_string(),
        source_parent_path: payload.source_parent_path.trim().to_string(),
        source_skill_path: payload.source_skill_path.trim().to_string(),
        source_commit: payload.source_commit.trim().to_string(),
        imported_at: Some(now),
        last_checked_at: None,
        latest_source_commit: String::new(),
        update_status: "unknown".to_string(),
        content_checksum: payload.content_checksum.trim().to_string(),
        dirty_status: "unknown".to_string(),
        updated_at: now,
    })
}

fn build_skill_installation_record(
    state: &AppState,
    payload: SkillInstallationUpsertRequest,
    path_id: Option<String>,
) -> Result<SkillInstallationRecord, ApiError> {
    let id = path_id
        .or(payload.id)
        .ok_or_else(|| ApiError::bad_request("skill installation id is required"))?;
    let id = sanitize_registry_id(&id, "skill installation id")?;
    let package_id = sanitize_registry_id(&payload.package_id, "skill package id")?;
    let package_ids = state
        .store
        .list_skill_packages()?
        .into_iter()
        .map(|pkg| pkg.id)
        .collect::<std::collections::BTreeSet<_>>();
    if !package_ids.contains(&package_id) {
        return Err(ApiError::bad_request(format!(
            "skill package '{package_id}' was not found"
        )));
    }
    let scope_kind = required_trimmed(payload.scope_kind, "skill installation scope_kind")?;
    let scope_id = required_trimmed(payload.scope_id, "skill installation scope_id")?;
    match scope_kind.as_str() {
        "workspace" | "project" | "session" => {}
        _ => {
            return Err(ApiError::bad_request(
                "skill installation scope_kind must be workspace, project, or session",
            ));
        }
    }
    let now = unix_timestamp();
    Ok(SkillInstallationRecord {
        id,
        package_id,
        scope_kind,
        scope_id,
        enabled: payload.enabled.unwrap_or(true),
        pinned_version: payload
            .pinned_version
            .map(|v: String| v.trim().to_string())
            .filter(|v: &String| !v.is_empty()),
        created_at: now,
        updated_at: now,
    })
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

#[derive(Debug, Clone)]
struct PreparedSessionWorkspace {
    working_dir: String,
    working_dir_kind: String,
    workspace_mode: String,
    source_project_path: String,
    git_root: String,
    worktree_path: String,
    git_branch: String,
    git_base_ref: String,
    git_head: String,
    git_dirty: bool,
    git_untracked_count: usize,
    git_remote_tracking_branch: String,
    workspace_warnings: Vec<String>,
}

fn prepare_session_workspace(
    state: &AppState,
    session_id: &str,
    projects: &SessionProjectSelection,
    requested_mode: Option<&str>,
    requested_branch: Option<&str>,
) -> Result<PreparedSessionWorkspace, ApiError> {
    let mut mode = requested_mode.unwrap_or("").trim().to_string();
    if mode.is_empty() {
        mode = if projects.primary_project_path.is_empty() {
            "scratch_only"
        } else {
            "isolated_worktree"
        }
        .to_string();
    }
    if !matches!(
        mode.as_str(),
        "shared_project_root" | "isolated_worktree" | "scratch_only"
    ) {
        return Err(ApiError::bad_request(format!(
            "unknown workspace mode '{mode}'"
        )));
    }
    if mode == "scratch_only" || projects.primary_project_path.is_empty() {
        let scratch_dir = state
            .store
            .scratch_dir_for_session(session_id)
            .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
        return Ok(PreparedSessionWorkspace::empty(
            scratch_dir,
            "workspace_scratch",
            "scratch_only",
        ));
    }
    let git = snapshot_git_state(&projects.primary_project_path).unwrap_or_default();
    if mode == "shared_project_root" || git.git_root.is_empty() {
        let mut prepared = PreparedSessionWorkspace::from_git(
            projects.working_dir.clone(),
            projects.working_dir_kind.clone(),
            &mode,
            &projects.primary_project_path,
            git,
        );
        let active = state
            .store
            .list_sessions()
            .map_err(|error| ApiError::from(anyhow::Error::from(error)))?
            .into_iter()
            .filter(|s| s.state == "active" && s.working_dir == projects.working_dir)
            .count();
        if mode == "shared_project_root" && active > 0 {
            prepared.workspace_warnings.push(format!("shared checkout already has {active} active session(s); branch and dirty state can cross session boundaries"));
        }
        if mode == "isolated_worktree" && prepared.git_root.is_empty() {
            prepared.workspace_warnings.push("requested isolated_worktree but project is not git-backed; using shared project directory".to_string());
            prepared.workspace_mode = "shared_project_root".to_string();
        }
        return Ok(prepared);
    }
    let git_root = git.git_root.clone();
    let base_ref = choose_session_base_ref(&git_root, &git);
    let slug = slugify(&projects.primary_project_title).if_empty("project");
    let short = session_id.chars().take(8).collect::<String>();
    let branch = requested_branch
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("work/{slug}/{short}"));
    if let Some(owner) = branch_worktree_path(&git_root, &branch)? {
        return Err(ApiError::bad_request(format!(
            "branch '{branch}' is already checked out at {owner}; attach to the owning session/worktree or choose a different branch"
        )));
    }
    let wt = state
        .store
        .state_dir_path()
        .join("worktrees")
        .join(&projects.primary_project_id)
        .join(session_id);
    fs::create_dir_all(wt.parent().unwrap())
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    let mut command = StdCommand::new("git");
    command
        .arg("-C")
        .arg(&git_root)
        .arg("worktree")
        .arg("add")
        .arg(&wt);
    if requested_branch
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        && branch_exists(&git_root, &branch)
    {
        command.arg(&branch);
    } else {
        command.arg("-b").arg(&branch).arg(&base_ref);
    }
    let output = command
        .output()
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&wt);
        return Err(ApiError::bad_request(format!(
            "failed to create git worktree: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut wt_git = snapshot_git_state(&display(&wt)).unwrap_or_default();
    wt_git.git_base_ref = base_ref.clone();
    Ok(PreparedSessionWorkspace::from_git(
        display(&wt),
        "managed_git_worktree".to_string(),
        "isolated_worktree",
        &projects.primary_project_path,
        wt_git,
    ))
}

trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}
impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

impl PreparedSessionWorkspace {
    fn empty(working_dir: String, working_dir_kind: &str, mode: &str) -> Self {
        Self {
            working_dir,
            working_dir_kind: working_dir_kind.to_string(),
            workspace_mode: mode.to_string(),
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
        }
    }
    fn from_git(
        working_dir: String,
        working_dir_kind: String,
        mode: &str,
        source: &str,
        git: GitSnapshot,
    ) -> Self {
        Self {
            working_dir: working_dir.clone(),
            working_dir_kind,
            workspace_mode: mode.to_string(),
            source_project_path: source.to_string(),
            git_root: git.git_root,
            worktree_path: working_dir,
            git_branch: git.git_branch,
            git_base_ref: git.git_base_ref,
            git_head: git.git_head,
            git_dirty: git.git_dirty,
            git_untracked_count: git.git_untracked_count,
            git_remote_tracking_branch: git.git_remote_tracking_branch,
            workspace_warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct GitSnapshot {
    git_root: String,
    git_branch: String,
    git_base_ref: String,
    git_head: String,
    git_dirty: bool,
    git_untracked_count: usize,
    git_remote_tracking_branch: String,
}

fn snapshot_git_state(path: &str) -> Result<GitSnapshot, ApiError> {
    let root = git_output(path, &["rev-parse", "--show-toplevel"])?;
    let branch = git_output(path, &["branch", "--show-current"]).unwrap_or_default();
    let head = git_output(path, &["rev-parse", "HEAD"]).unwrap_or_default();
    let upstream = git_output(
        path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .unwrap_or_default();
    let status = git_output(path, &["status", "--porcelain"]).unwrap_or_default();
    let untracked = status.lines().filter(|l| l.starts_with("??")).count();
    Ok(GitSnapshot {
        git_root: root,
        git_branch: branch,
        git_base_ref: String::new(),
        git_head: head,
        git_dirty: !status.trim().is_empty(),
        git_untracked_count: untracked,
        git_remote_tracking_branch: upstream,
    })
}

fn cleanup_session_worktree_before_delete(session: &SessionSummary) -> Result<(), ApiError> {
    if session.workspace_mode != "isolated_worktree" || session.worktree_path.is_empty() {
        return Ok(());
    }
    let current = snapshot_git_state(&session.worktree_path).map_err(|_| {
        ApiError::bad_request(format!(
            "managed worktree '{}' is missing or unreadable; archive the session or prune stale metadata before deleting",
            session.worktree_path
        ))
    })?;
    if current.git_dirty || current.git_untracked_count > 0 {
        return Err(ApiError::bad_request(format!(
            "refusing to delete dirty managed worktree '{}' ({} untracked file(s)); commit, clean, or archive it first",
            session.worktree_path, current.git_untracked_count
        )));
    }
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(&session.git_root)
        .args(["worktree", "remove", &session.worktree_path])
        .output()
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    if !output.status.success() {
        return Err(ApiError::bad_request(format!(
            "failed to remove managed worktree '{}': {}",
            session.worktree_path,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

fn refresh_session_workspace_warnings(session: &mut SessionSummary) {
    let current = snapshot_git_state(&session.working_dir).ok();
    let warnings = workspace_drift_warnings(session, current.as_ref());
    for warning in warnings {
        if !session
            .workspace_warnings
            .iter()
            .any(|existing| existing == &warning)
        {
            session.workspace_warnings.push(warning);
        }
    }
}

fn workspace_drift_warnings(
    session: &SessionSummary,
    current: Option<&GitSnapshot>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if session.git_root.is_empty() {
        return warnings;
    }
    let Some(current) = current else {
        warnings.push(format!(
            "workspace git state could not be read for {}; the worktree may have been removed",
            session.working_dir
        ));
        return warnings;
    };
    if !session.git_branch.is_empty() && current.git_branch != session.git_branch {
        warnings.push(format!(
            "branch changed since session start: was {}, now {}",
            session.git_branch,
            if current.git_branch.is_empty() {
                "detached HEAD"
            } else {
                &current.git_branch
            }
        ));
    }
    if !session.git_head.is_empty() && current.git_head != session.git_head {
        warnings.push(format!(
            "HEAD changed since session start: was {}, now {}",
            short_sha(&session.git_head),
            short_sha(&current.git_head)
        ));
    }
    if current.git_dirty != session.git_dirty {
        warnings.push(format!(
            "dirty state changed since session start: was {}, now {}",
            session.git_dirty, current.git_dirty
        ));
    }
    if current.git_untracked_count != session.git_untracked_count {
        warnings.push(format!(
            "untracked file count changed since session start: was {}, now {}",
            session.git_untracked_count, current.git_untracked_count
        ));
    }
    warnings
}

fn short_sha(value: &str) -> String {
    value.chars().take(12).collect()
}

fn choose_session_base_ref(git_root: &str, git: &GitSnapshot) -> String {
    for candidate in ["dev", "origin/dev", "main", "origin/main"] {
        if git_output(git_root, &["rev-parse", "--verify", candidate]).is_ok() {
            return candidate.to_string();
        }
    }
    if !git.git_branch.is_empty() {
        git.git_branch.clone()
    } else {
        "HEAD".to_string()
    }
}

fn branch_worktree_path(git_root: &str, branch: &str) -> Result<Option<String>, ApiError> {
    let output = git_output(git_root, &["worktree", "list", "--porcelain"])?;
    let mut current_path = String::new();
    let mut current_branch = String::new();
    for line in output.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if current_branch == format!("refs/heads/{branch}") {
                return Ok(Some(current_path));
            }
            current_path.clear();
            current_branch.clear();
        } else if let Some(path) = line.strip_prefix("worktree ") {
            current_path = path.to_string();
        } else if let Some(value) = line.strip_prefix("branch ") {
            current_branch = value.to_string();
        }
    }
    Ok(None)
}

fn branch_exists(git_root: &str, branch: &str) -> bool {
    git_output(
        git_root,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .is_ok()
}

fn git_output(path: &str, args: &[&str]) -> Result<String, ApiError> {
    let out = StdCommand::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(ApiError::bad_request(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

fn display(path: &FsPath) -> String {
    path.to_string_lossy().to_string()
}

fn slugify(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
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
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;

    if resolved.is_empty() {
        let scratch_dir = state
            .store
            .scratch_dir_for_session(
                scratch_session_id
                    .or_else(|| fallback.map(|session| session.id.as_str()))
                    .unwrap_or("ad-hoc-preview"),
            )
            .map_err(|error| ApiError::from(anyhow::Error::from(error)))?;

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
    let sources = discover_prompt_sources(state, session)?;
    let skill_layers = collect_compiled_skill_layers(state, session, prompt)?.layers;

    if sources.is_empty() && skill_layers.is_empty() {
        return Ok(PromptAssembly {
            prompt: prompt.to_string(),
        });
    }

    Ok(PromptAssembly {
        prompt: render_prompt_with_sources_and_skill_layers(prompt, &sources, &skill_layers),
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

    Ok(sources)
}

#[derive(Debug, Default)]
struct SkillLayerCollection {
    layers: Vec<CompiledPromptLayer>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
struct InstalledSkillPackage {
    package_id: String,
    name: String,
    instructions: String,
    manifest_json: Value,
}

fn canonical_workspace_root(workspace: &WorkspaceSummary) -> Result<PathBuf, ApiError> {
    let workspace_root = PathBuf::from(&workspace.root_path);
    if !workspace_root.is_dir() {
        return Ok(workspace_root);
    }
    fs::canonicalize(&workspace_root).map_err(|error| {
        ApiError::internal_message(format!(
            "failed to resolve workspace root '{}': {error}",
            workspace.root_path
        ))
    })
}

fn skill_activation_match(
    skill: &SkillManifest,
    packages: &[InstalledSkillPackage],
    prompt: &str,
) -> Option<String> {
    if !skill.enabled {
        return None;
    }
    match skill.activation_mode.as_str() {
        "always" => Some("always".to_string()),
        "auto" => skill_match_reason(skill, packages, prompt, false),
        "manual" => skill_match_reason(skill, packages, prompt, true),
        _ => None,
    }
}

fn skill_match_reason(
    skill: &SkillManifest,
    packages: &[InstalledSkillPackage],
    prompt: &str,
    exact_only: bool,
) -> Option<String> {
    let prompt_normalized = normalize_skill_match_text(prompt);
    let prompt_lower = prompt.to_ascii_lowercase();

    let exact_terms = [skill.id.as_str(), skill.title.as_str()]
        .into_iter()
        .map(normalize_skill_match_text)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    for term in &exact_terms {
        if prompt_normalized.contains(term) {
            return Some(format!("exact mention '{term}'"));
        }
    }

    let normalized_id = normalize_skill_match_text(&skill.id.replace(['-', '_', '.'], " "));
    if !normalized_id.is_empty() && prompt_normalized.contains(&normalized_id) {
        return Some(format!("normalized id '{normalized_id}'"));
    }

    if exact_only {
        return None;
    }

    for trigger in &skill.triggers {
        let trigger = normalize_skill_match_text(trigger);
        if !trigger.is_empty() && prompt_normalized.contains(&trigger) {
            return Some(format!("trigger '{trigger}'"));
        }
    }

    for package in packages {
        for value in [
            package.name.as_str(),
            package.package_id.as_str(),
            package
                .manifest_json
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(""),
            package
                .manifest_json
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(""),
            package
                .manifest_json
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ] {
            let term = normalize_skill_match_text(value);
            if !term.is_empty() && prompt_normalized.contains(&term) {
                return Some(format!("package metadata '{term}'"));
            }
        }
    }

    let description = skill.description.to_ascii_lowercase();
    description
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| token.len() >= 5)
        .find(|token| prompt_lower.contains(*token))
        .map(|token| format!("description token '{token}'"))
}

fn normalize_skill_match_text(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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

fn installed_skill_packages_by_skill_id(
    state: &AppState,
    session: &SessionSummary,
) -> Result<std::collections::BTreeMap<String, Vec<InstalledSkillPackage>>, ApiError> {
    let packages = state.store.list_skill_packages()?;
    let package_map = packages
        .into_iter()
        .map(|pkg| (pkg.id.clone(), pkg))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut installed = std::collections::BTreeMap::<String, Vec<InstalledSkillPackage>>::new();

    for installation in state.store.list_skill_installations()? {
        if !installation.enabled {
            continue;
        }
        let Some(package) = package_map.get(&installation.package_id) else {
            continue;
        };
        if !skill_installation_applies(&installation, session) {
            continue;
        }
        if let Some(pinned) = installation.pinned_version.as_deref() {
            if package.version != pinned {
                continue;
            }
        }
        let Some(skill_id) = package_skill_id(package) else {
            continue;
        };
        installed
            .entry(skill_id)
            .or_default()
            .push(InstalledSkillPackage {
                package_id: package.id.clone(),
                name: package.name.clone(),
                instructions: package.instructions.clone(),
                manifest_json: package.manifest_json.clone(),
            });
    }

    Ok(installed)
}

fn package_skill_id(package: &SkillPackageRecord) -> Option<String> {
    package
        .manifest_json
        .get("manifest_id")
        .and_then(Value::as_str)
        .or_else(|| package.manifest_json.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| package.id.strip_prefix("nucleus.").map(ToString::to_string))
}

fn skill_installation_applies(
    installation: &SkillInstallationRecord,
    session: &SessionSummary,
) -> bool {
    match installation.scope_kind.as_str() {
        "workspace" => installation.scope_id == "workspace",
        "project" => session
            .projects
            .iter()
            .any(|project| installation.scope_id == project.id),
        "session" => session.id == installation.scope_id,
        _ => false,
    }
}

fn resolve_skill_include_path(
    workspace_root: &std::path::Path,
    skill_id: &str,
    include_path: &str,
) -> Option<PathBuf> {
    let include_path = include_path.trim();
    if include_path.is_empty() {
        return None;
    }

    let raw_path = PathBuf::from(include_path);
    let path = if raw_path.is_absolute() {
        raw_path
    } else {
        workspace_root.join(raw_path)
    };
    let canonical = fs::canonicalize(path).ok()?;
    if !canonical.is_file() {
        return None;
    }
    if canonical.starts_with(workspace_root) || is_allowed_nucleus_skill_path(&canonical, skill_id)
    {
        Some(canonical)
    } else {
        None
    }
}

fn is_allowed_nucleus_skill_path(path: &std::path::Path, skill_id: &str) -> bool {
    if path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    if parent.file_name().and_then(|name| name.to_str()) != Some(skill_id) {
        return false;
    }
    let Some(skills_dir) = parent.parent() else {
        return false;
    };
    skills_dir.file_name().and_then(|name| name.to_str()) == Some("skills")
        && skills_dir
            .parent()
            .and_then(|state_dir| state_dir.file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".nucleus"))
}

#[derive(Debug, Clone, Default)]
struct MemoryLayerCollection {
    layers: Vec<CompiledPromptLayer>,
    total_count: usize,
    included_count: usize,
    skipped_count: usize,
    truncated_count: usize,
}

fn compiled_include_layers(sources: &[PromptIncludeSource]) -> Vec<CompiledPromptLayer> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| CompiledPromptLayer {
            id: format!("include:{}", index + 1),
            kind: "include".to_string(),
            scope: source.scope.to_string(),
            title: source
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("prompt include")
                .to_string(),
            source_path: source.path.display().to_string(),
            content: source.content.clone(),
        })
        .collect()
}

fn collect_memory_layers(
    state: &AppState,
    session: &SessionSummary,
) -> Result<MemoryLayerCollection, ApiError> {
    let mut collection = MemoryLayerCollection::default();
    let mut remaining = MAX_MEMORY_CONTEXT_CHARS;
    let mut layers = Vec::new();
    let project_ids = session
        .projects
        .iter()
        .map(|project| project.id.as_str())
        .collect::<BTreeSet<_>>();

    for entry in state.store.list_memory_entries()? {
        if !memory_entry_applies_to_session(&entry, session, &project_ids) {
            continue;
        }
        collection.total_count += 1;
        if !entry.enabled || entry.status != "accepted" {
            collection.skipped_count += 1;
            continue;
        }
        let mut content = format!(
            "Title: {}\nScope: {}/{}\nKind: {}\nTags: {}\n\n{}",
            entry.title,
            entry.scope_kind,
            entry.scope_id,
            entry.memory_kind,
            if entry.tags.is_empty() {
                "none".to_string()
            } else {
                entry.tags.join(", ")
            },
            entry.content
        );
        if content.len() > remaining {
            collection.truncated_count += 1;
            if remaining < 256 {
                collection.skipped_count += 1;
                continue;
            }
            let content_budget = remaining.saturating_sub(MEMORY_TRUNCATION_NOTICE.len());
            if content_budget == 0 {
                collection.skipped_count += 1;
                continue;
            }
            content = truncate_utf8_to_byte_budget(&content, content_budget);
            content.push_str(MEMORY_TRUNCATION_NOTICE);
        }
        remaining = remaining.saturating_sub(content.len());
        layers.push(CompiledPromptLayer {
            id: format!("memory:{}", entry.id),
            kind: "memory".to_string(),
            scope: format!("{}:{}", entry.scope_kind, entry.scope_id),
            title: entry.title,
            source_path: String::new(),
            content,
        });
        collection.included_count += 1;
    }
    collection.layers = layers;
    Ok(collection)
}

fn truncate_utf8_to_byte_budget(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut end = 0usize;
    for (index, character) in value.char_indices() {
        let next = index + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    value[..end].to_string()
}

fn memory_entry_applies_to_session(
    entry: &MemoryEntry,
    session: &SessionSummary,
    project_ids: &BTreeSet<&str>,
) -> bool {
    match entry.scope_kind.as_str() {
        "workspace" => entry.scope_id == "workspace",
        "project" => project_ids.contains(entry.scope_id.as_str()),
        "session" => entry.scope_id == session.id,
        _ => false,
    }
}

fn compile_session_turn(
    state: &AppState,
    session: &SessionSummary,
    history: &[nucleus_protocol::SessionTurn],
    prompt: &str,
    images: &[nucleus_protocol::SessionTurnImage],
    compiler_role: &str,
) -> Result<CompiledTurn, ApiError> {
    let prompt_sources = discover_prompt_sources(state, session)?;
    let mut project_layers = compiled_include_layers(&prompt_sources);
    let memory_collection = collect_memory_layers(state, session)?;
    project_layers.extend(memory_collection.layers.clone());
    let skill_collection = collect_compiled_skill_layers(state, session, prompt)?;
    let skill_layers = skill_collection.layers;
    let mut mcp_catalog = state.store.list_mcp_servers()?;
    mcp_catalog.retain(|server| server.enabled);
    mcp_catalog.sort_by(|a, b| a.id.cmp(&b.id));
    let tool_catalog = mcp_catalog
        .iter()
        .flat_map(|server| server.tools.clone())
        .collect::<Vec<_>>();

    let mut compiled = runtime::compiled_turn_from_prompt(
        history,
        prompt,
        images,
        compiler_role,
        &skill_layers,
        &tool_catalog,
        &mcp_catalog,
    );
    compiled.project_layers = project_layers;
    let include_count = prompt_sources.len();
    let skill_count = compiled.skill_layers.len();
    let mcp_server_count = compiled.mcp_catalog.len();
    let tool_count = compiled.tool_catalog.len();
    let layer_count =
        compiled.system_layers.len() + compiled.project_layers.len() + compiled.skill_layers.len();
    compiled.debug_summary = nucleus_protocol::CompiledTurnDebugSummary {
        include_count,
        memory_count: memory_collection.total_count,
        memory_included_count: memory_collection.included_count,
        memory_skipped_count: memory_collection.skipped_count,
        memory_truncated_count: memory_collection.truncated_count,
        skill_count,
        mcp_server_count,
        tool_count,
        layer_count,
        summary: format!(
            "Compiled {} history turns for {} provider-neutral prompt with {} prompt includes, {} accepted memory entries included, {} skill layers, {} MCP servers, and {} tools.",
            compiled.history.len(),
            compiled.role,
            include_count,
            memory_collection.included_count,
            skill_count,
            mcp_server_count,
            tool_count,
        ),
        skill_diagnostics: skill_collection.diagnostics,
    };
    Ok(compiled)
}

fn collect_compiled_skill_layers(
    state: &AppState,
    session: &SessionSummary,
    prompt: &str,
) -> Result<SkillLayerCollection, ApiError> {
    let workspace = state.store.workspace()?;
    let workspace_root = canonical_workspace_root(&workspace)?;
    let mut layers = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen_content = BTreeSet::new();
    let installed = installed_skill_packages_by_skill_id(state, session)?;
    for skill in state.store.list_skill_manifests()? {
        let packages = installed.get(&skill.id).cloned().unwrap_or_default();
        if !skill.enabled {
            diagnostics.push(format!("skill {} skipped: disabled", skill.id));
            continue;
        }
        if packages.is_empty() {
            diagnostics.push(format!(
                "skill {} skipped: no enabled installation for this session",
                skill.id
            ));
            continue;
        }
        if !skill_project_filter_matches(&skill, session) {
            diagnostics.push(format!(
                "skill {} skipped: project filter mismatch",
                skill.id
            ));
            continue;
        }
        let Some(reason) = skill_activation_match(&skill, &packages, prompt) else {
            diagnostics.push(format!(
                "skill {} skipped: activation mode '{}' did not match trigger/title/id metadata",
                skill.id, skill.activation_mode
            ));
            continue;
        };
        diagnostics.push(format!("skill {} selected: {}", skill.id, reason));
        for package in &packages {
            let content = package.instructions.trim();
            if content.is_empty() {
                diagnostics.push(format!(
                    "skill {} package {} skipped: no package instructions",
                    skill.id, package.package_id
                ));
                continue;
            }
            if seen_content.insert(content.to_string()) {
                layers.push(CompiledPromptLayer {
                    id: format!("skill:{}:package:{}", skill.id, package.package_id),
                    kind: "skill".to_string(),
                    scope: "workspace".to_string(),
                    title: skill.title.clone(),
                    source_path: format!("skill-package:{}", package.package_id),
                    content: package.instructions.clone(),
                });
                diagnostics.push(format!(
                    "skill {} package {} loaded: package instructions",
                    skill.id, package.package_id
                ));
            }
        }
        if !skill.instructions.trim().is_empty() {
            let content = skill.instructions.trim().to_string();
            if seen_content.insert(content) {
                layers.push(CompiledPromptLayer {
                    id: format!("skill:{}:instructions", skill.id),
                    kind: "skill".to_string(),
                    scope: "workspace".to_string(),
                    title: skill.title.clone(),
                    source_path: format!("skill:{}", skill.id),
                    content: skill.instructions.clone(),
                });
                diagnostics.push(format!("skill {} loaded: manifest instructions", skill.id));
            }
        }
        for include_path in &skill.include_paths {
            let Some(path) = resolve_skill_include_path(&workspace_root, &skill.id, include_path)
            else {
                diagnostics.push(format!(
                    "skill {} include rejected or missing: {}",
                    skill.id, include_path
                ));
                continue;
            };
            let Ok(content) = fs::read_to_string(&path) else {
                diagnostics.push(format!(
                    "skill {} include unreadable: {}",
                    skill.id,
                    path.display()
                ));
                continue;
            };
            if seen_content.insert(content.trim().to_string()) {
                layers.push(CompiledPromptLayer {
                    id: format!("skill:{}:{}", skill.id, include_path),
                    kind: "skill".to_string(),
                    scope: "workspace".to_string(),
                    title: skill.title.clone(),
                    source_path: path.display().to_string(),
                    content,
                });
                diagnostics.push(format!(
                    "skill {} loaded: include {}",
                    skill.id,
                    path.display()
                ));
            }
        }
    }
    layers.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(SkillLayerCollection {
        layers,
        diagnostics,
    })
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
    render_prompt_with_sources_and_skill_layers(prompt, sources, &[])
}

fn render_prompt_with_sources_and_skill_layers(
    prompt: &str,
    sources: &[PromptIncludeSource],
    skill_layers: &[CompiledPromptLayer],
) -> String {
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

    for layer in skill_layers {
        rendered.push_str("\n[skill layer: ");
        rendered.push_str(&layer.title);
        if !layer.source_path.is_empty() {
            rendered.push_str(" — ");
            rendered.push_str(&layer.source_path);
        }
        rendered.push_str("]\n");
        rendered.push_str(&layer.content);
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

fn normalize_session_approval_mode(value: Option<&str>) -> Result<String, ApiError> {
    let mode = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ask");
    match mode {
        "ask" | "trusted" => Ok(mode.to_string()),
        other => Err(ApiError::bad_request(format!(
            "unsupported session approval mode '{other}'",
        ))),
    }
}

fn normalize_session_execution_mode(value: Option<&str>) -> Result<String, ApiError> {
    let mode = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("act");
    match mode {
        "act" | "plan" => Ok(mode.to_string()),
        other => Err(ApiError::bad_request(format!(
            "unsupported session execution mode '{other}'",
        ))),
    }
}

fn normalize_session_run_budget_mode(value: Option<&str>) -> Result<String, ApiError> {
    let mode = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("inherit");
    match mode {
        "inherit" | "standard" | "extended" | "marathon" | "unbounded" => Ok(mode.to_string()),
        other => Err(ApiError::bad_request(format!(
            "unsupported session run budget mode '{other}'",
        ))),
    }
}

fn normalize_workspace_run_budget(
    mut value: RunBudgetSummary,
) -> Result<RunBudgetSummary, ApiError> {
    value.mode = "standard".to_string();
    value.max_steps = normalize_budget_usize(
        "run_budget.max_steps",
        value.max_steps,
        MAX_CONFIGURED_JOB_STEPS,
    )?;
    value.max_tool_calls = normalize_budget_usize(
        "run_budget.max_tool_calls",
        value.max_tool_calls,
        MAX_CONFIGURED_JOB_TOOL_CALLS,
    )?;
    value.max_wall_clock_secs = normalize_budget_u64(
        "run_budget.max_wall_clock_secs",
        value.max_wall_clock_secs,
        MAX_CONFIGURED_JOB_WALL_CLOCK_SECS,
    )?;
    Ok(value)
}

fn normalize_budget_usize(name: &str, value: usize, max: usize) -> Result<usize, ApiError> {
    if value == 0 {
        return Err(ApiError::bad_request(format!("{name} must be at least 1")));
    }
    if value > max {
        return Err(ApiError::bad_request(format!(
            "{name} must be no more than {max}"
        )));
    }
    Ok(value)
}

fn normalize_budget_u64(name: &str, value: u64, max: u64) -> Result<u64, ApiError> {
    if value == 0 {
        return Err(ApiError::bad_request(format!("{name} must be at least 1")));
    }
    if value > max {
        return Err(ApiError::bad_request(format!(
            "{name} must be no more than {max}"
        )));
    }
    Ok(value)
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

fn resolve_log_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    let limit = limit.unwrap_or(DEFAULT_LOG_LIMIT);

    if limit == 0 {
        return Err(ApiError::bad_request(
            "log limit must be greater than zero".to_string(),
        ));
    }

    if limit > MAX_LOG_LIMIT {
        return Err(ApiError::bad_request(format!(
            "log limit must be {MAX_LOG_LIMIT} or lower"
        )));
    }

    Ok(limit)
}

fn resolve_log_cursor(
    before: Option<i64>,
    before_id: Option<i64>,
) -> Result<Option<(i64, i64)>, ApiError> {
    match (before, before_id) {
        (Some(timestamp), Some(id)) => Ok(Some((timestamp, id))),
        (None, None) => Ok(None),
        (Some(_), None) => Err(ApiError::bad_request(
            "before_id is required when before is provided",
        )),
        (None, Some(_)) => Err(ApiError::bad_request("before_id requires before")),
    }
}

fn normalize_log_filter(value: Option<String>) -> Option<String> {
    value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
}

fn audit_status_to_log_level(status: &str) -> &'static str {
    match status {
        "error" | "failed" | "denied" => "error",
        "warning" | "warn" => "warn",
        "success" | "ok" | "info" => "info",
        _ => "info",
    }
}

fn category_for_event(event: &str) -> &'static str {
    match event.split('.').next().unwrap_or_default() {
        "vault" => "vault",
        "memory" => "memory",
        "mcp" => "mcp",
        "update" | "settings" => "update",
        "job" | "worker" | "approval" | "playbook" => "job",
        "session" => "session",
        "automation" => "automation",
        _ => "system",
    }
}

fn normalize_log_level(value: &str) -> String {
    match value.trim().to_lowercase().as_str() {
        "trace" => "debug".to_string(),
        "debug" => "debug".to_string(),
        "warn" | "warning" => "warn".to_string(),
        "error" | "failed" => "error".to_string(),
        _ => "info".to_string(),
    }
}

fn normalize_log_category(value: &str) -> String {
    match value.trim() {
        "system" | "session" | "job" | "automation" | "mcp" | "memory" | "vault" | "update" => {
            value.trim().to_string()
        }
        _ => "system".to_string(),
    }
}

fn normalize_log_event(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "event".to_string();
    }

    excerpt(trimmed, 100)
}

fn sanitize_log_json(redactor: &security::RedactionSet, value: &Value) -> Value {
    sanitize_log_json_depth(redactor, &redactor.redact_json(value), 0)
}

fn sanitize_log_json_depth(
    redactor: &security::RedactionSet,
    value: &Value,
    depth: usize,
) -> Value {
    if depth >= 3 {
        return json!("[TRUNCATED]");
    }

    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .take(INSTANCE_LOG_METADATA_KEYS_LIMIT)
                .map(|(key, value)| {
                    (
                        excerpt(&redact_instance_log_text(&redactor.redact_text(key)), 80),
                        sanitize_log_json_depth(redactor, value, depth + 1),
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(INSTANCE_LOG_METADATA_KEYS_LIMIT)
                .map(|value| sanitize_log_json_depth(redactor, value, depth + 1))
                .collect(),
        ),
        Value::String(value) => json!(excerpt(
            &redact_instance_log_text(&redactor.redact_text(value)),
            INSTANCE_LOG_METADATA_STRING_LIMIT
        )),
        other => other.clone(),
    }
}

fn redact_instance_log_text(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            redact_instance_log_assignments(&redact_instance_log_bearer_values(
                &redact_instance_log_sk_tokens(line),
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_instance_log_bearer_values(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(index) = remaining.to_ascii_lowercase().find("bearer ") {
        out.push_str(&remaining[..index + "bearer ".len()]);
        out.push_str(INSTANCE_LOG_REDACTED);
        remaining = &remaining[index + "bearer ".len()..];
        let consumed = remaining
            .char_indices()
            .find_map(|(position, character)| character.is_whitespace().then_some(position))
            .unwrap_or(remaining.len());
        remaining = &remaining[consumed..];
    }

    out.push_str(remaining);
    out
}

fn redact_instance_log_sk_tokens(input: &str) -> String {
    input
        .split_inclusive(char::is_whitespace)
        .map(|part| {
            let trimmed = part.trim_end();
            let suffix = &part[trimmed.len()..];
            let token = trimmed.trim_matches(|character: char| {
                matches!(character, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
            });
            if token.starts_with("sk-") && token.len() >= 12 {
                format!("{INSTANCE_LOG_REDACTED}{suffix}")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

fn redact_instance_log_assignments(input: &str) -> String {
    let mut output = input.to_string();
    for marker in [
        "api_key",
        "access_token",
        "refresh_token",
        "password",
        "passphrase",
        "client_secret",
        "cookie",
        "authorization",
    ] {
        output = redact_instance_log_assignment_for_marker(&output, marker);
    }
    output
}

fn redact_instance_log_assignment_for_marker(input: &str, marker: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(relative_index) = lower[cursor..].find(marker) {
        let index = cursor + relative_index;
        let after_marker = index + marker.len();
        let Some(separator_offset) =
            input[after_marker..].find(|character: char| matches!(character, '=' | ':'))
        else {
            break;
        };
        let value_start = after_marker + separator_offset + 1;
        let value_end = input[value_start..]
            .char_indices()
            .find_map(|(position, character)| {
                matches!(character, ',' | ';' | '&' | ' ' | '\t').then_some(value_start + position)
            })
            .unwrap_or(input.len());

        output.push_str(&input[cursor..value_start]);
        output.push_str(INSTANCE_LOG_REDACTED);
        cursor = value_end;
    }

    output.push_str(&input[cursor..]);
    output
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
    let log_level = audit_status_to_log_level(&record.status);
    let log_category = category_for_event(&record.kind);
    let log_event = record.kind.clone();
    let log_message = if record.summary.trim().is_empty() {
        record.detail.clone()
    } else {
        record.summary.clone()
    };
    let log_target = record.target.clone();

    match state.store.append_audit_event(record) {
        Ok(event) => {
            let _ = publish_audit_event(state).await;
            let _ = record_instance_log(
                state,
                log_level,
                log_category,
                "audit",
                log_event,
                log_message,
                json!({
                    "audit_event_id": event.id,
                    "target": log_target,
                }),
                json!({ "audit_status": event.status }),
            )
            .await;
            Some(event)
        }
        Err(error) => {
            warn!(error = %error, "failed to persist audit event");
            None
        }
    }
}

pub(crate) async fn record_instance_log(
    state: &AppState,
    level: impl AsRef<str>,
    category: impl AsRef<str>,
    source: impl AsRef<str>,
    event: impl AsRef<str>,
    message: impl AsRef<str>,
    related_ids: Value,
    metadata: Value,
) -> Option<nucleus_protocol::InstanceLogEntry> {
    let mut redactor = security::RedactionSet::new();
    if let Ok(token) = state.store.read_local_auth_token() {
        redactor.register_secret(token);
    }

    let level = normalize_log_level(level.as_ref());
    let category = normalize_log_category(category.as_ref());
    let event = normalize_log_event(event.as_ref());
    let source = excerpt(
        &redact_instance_log_text(&redactor.redact_text(source.as_ref().trim())),
        80,
    );
    let message = excerpt(
        &redact_instance_log_text(&redactor.redact_text(message.as_ref().trim())),
        INSTANCE_LOG_MESSAGE_LIMIT,
    );
    let related_ids = sanitize_log_json(&redactor, &related_ids);
    let metadata = sanitize_log_json(&redactor, &metadata);

    match state.store.append_instance_log(InstanceLogRecord {
        timestamp: unix_timestamp(),
        level,
        category,
        source,
        event,
        message,
        related_ids,
        metadata,
    }) {
        Ok(entry) => Some(entry),
        Err(error) => {
            warn!(error = %error, "failed to persist instance log");
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

    if before.approval_mode != after.approval_mode {
        return match after.approval_mode.as_str() {
            "trusted" => format!(
                "Allowed Nucleus to run actions without approval in {} session '{}'.",
                after.provider, after.title
            ),
            _ => format!(
                "Restored approval prompts in {} session '{}'.",
                after.provider, after.title
            ),
        };
    }

    if before.execution_mode != after.execution_mode {
        return match after.execution_mode.as_str() {
            "plan" => format!(
                "Enabled Plan mode in {} session '{}'.",
                after.provider, after.title
            ),
            _ => format!(
                "Enabled Action mode in {} session '{}'.",
                after.provider, after.title
            ),
        };
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
        .map_err(|error| ApiError::from(anyhow::Error::from(error)))?
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
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn api_session_response_redacts_provider_api_key() {
        let mut session = test_session(Path::new("/tmp/nucleus-redaction-test"));
        session.provider_api_key = "nuctk_super_secret".to_string();
        let detail = SessionDetail {
            session,
            turns: Vec::new(),
        };

        let redacted = redact_session_detail(detail);

        assert_eq!(redacted.session.provider_api_key, "");
    }

    #[test]
    fn router_profile_response_redacts_target_api_keys() {
        let profiles = redact_router_profiles(vec![RouterProfileSummary {
            id: "route".to_string(),
            title: "Route".to_string(),
            summary: String::new(),
            enabled: true,
            state: "ready".to_string(),
            targets: vec![nucleus_protocol::RouteTarget {
                provider: "openai-compatible".to_string(),
                model: "model".to_string(),
                base_url: "https://example.test".to_string(),
                api_key: "nuctk_route_secret".to_string(),
            }],
        }]);

        assert_eq!(profiles[0].targets[0].api_key, "");
    }

    #[test]
    fn mcp_response_redacts_env_and_header_secret_values() {
        let server = redact_mcp_server(McpServerSummary {
            id: "server".to_string(),
            title: "Server".to_string(),
            enabled: true,
            transport: "streamable-http".to_string(),
            command: String::new(),
            args: Vec::new(),
            env_json: json!({ "SUPABASE_ACCESS_TOKEN": "nuctk_env_secret", "SAFE": "ok" }),
            url: "https://example.test/mcp".to_string(),
            headers_json: json!({ "Authorization": "Bearer nuctk_header_secret" }),
            auth_kind: "none".to_string(),
            auth_ref: String::new(),
            sync_status: "ready".to_string(),
            last_error: String::new(),
            last_synced_at: None,
            tools: Vec::new(),
            resources: Vec::new(),
        });

        assert_eq!(
            server.env_json["SUPABASE_ACCESS_TOKEN"],
            "[REDACTED_SECRET]"
        );
        assert_eq!(server.env_json["SAFE"], "ok");
        assert_eq!(server.headers_json["Authorization"], "[REDACTED_SECRET]");
    }

    #[test]
    fn rejects_invalid_instance_log_limits() {
        assert!(resolve_log_limit(Some(1)).is_ok());
        assert!(resolve_log_limit(Some(0)).is_err());
        assert!(resolve_log_limit(Some(MAX_LOG_LIMIT + 1)).is_err());
    }

    #[test]
    fn rejects_partial_instance_log_cursors() {
        assert_eq!(resolve_log_cursor(None, None).unwrap(), None);
        assert_eq!(
            resolve_log_cursor(Some(10), Some(5)).unwrap(),
            Some((10, 5))
        );
        assert!(resolve_log_cursor(Some(10), None).is_err());
        assert!(resolve_log_cursor(None, Some(5)).is_err());
    }

    #[tokio::test]
    async fn instance_log_write_path_redacts_secret_like_values() {
        let (state_dir, state) = test_named_app_state("instance-log-redaction");
        let secret = state
            .store
            .read_local_auth_token()
            .expect("test token should exist");

        record_instance_log(
            &state,
            "info",
            "vault",
            "test",
            "vault.secret.checked",
            format!(
                "token={secret} api_key=first-leaked-key&api_key=second-leaked-key bearer phase6-vault-token sk-supersecretvalue"
            ),
            json!({
                "vault_secret_id": "secret-meta-id",
                "api_key": "plain-api-key-value",
                "safe_url": "https://example.test/path?api_key=third-leaked-key&api_key=fourth-leaked-key"
            }),
            json!({
                "Authorization": "Bearer another-secret",
                "safe": "metadata"
            }),
        )
        .await
        .expect("log should persist");

        let logs = state
            .store
            .list_instance_logs(Some("vault"), None, None, 10)
            .expect("logs should list");
        let serialized = serde_json::to_string(&logs).expect("logs serialize");
        assert!(serialized.contains("[REDACTED_SECRET]"));
        assert!(!serialized.contains(&secret));
        assert!(!serialized.contains("phase6-vault-token"));
        assert!(!serialized.contains("sk-supersecretvalue"));
        assert!(!serialized.contains("first-leaked-key"));
        assert!(!serialized.contains("second-leaked-key"));
        assert!(!serialized.contains("third-leaked-key"));
        assert!(!serialized.contains("fourth-leaked-key"));
        assert!(!serialized.contains("plain-api-key-value"));
        assert!(!serialized.contains("another-secret"));
        assert!(state_dir.join("logs/events.jsonl").is_file());

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn refuses_dirty_worktree_cleanup() {
        let root = test_state_dir("dirty-worktree-cleanup");
        fs::create_dir_all(&root).expect("repo dir should create");
        run_git(&root, &["init", "-b", "dev"]);
        run_git(&root, &["config", "user.email", "nucleus@example.test"]);
        run_git(&root, &["config", "user.name", "Nucleus Test"]);
        fs::write(root.join("README.md"), "hello").expect("file should write");
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-m", "initial"]);
        let worktree = root.with_file_name(format!(
            "{}-cleanup",
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo")
        ));
        run_git(
            &root,
            &[
                "worktree",
                "add",
                &worktree.display().to_string(),
                "-b",
                "work/test",
            ],
        );
        fs::write(worktree.join("dirty.txt"), "dirty").expect("dirty file should write");

        let mut session = test_session(&worktree);
        session.workspace_mode = "isolated_worktree".to_string();
        session.git_root = root.display().to_string();
        session.worktree_path = worktree.display().to_string();

        let error = cleanup_session_worktree_before_delete(&session)
            .expect_err("dirty worktree should be refused");
        assert!(
            error
                .message
                .contains("refusing to delete dirty managed worktree")
        );

        let _ = StdCommand::new("git")
            .arg("-C")
            .arg(&root)
            .args([
                "worktree",
                "remove",
                "--force",
                &worktree.display().to_string(),
            ])
            .output();
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&worktree);
    }

    #[test]
    fn reports_workspace_git_state_drift() {
        let mut session = test_session(std::path::Path::new("/tmp/example"));
        session.git_root = "/tmp/example".to_string();
        session.git_branch = "dev".to_string();
        session.git_head = "aaaaaaaaaaaabbbb".to_string();
        session.git_dirty = false;
        session.git_untracked_count = 0;

        let current = GitSnapshot {
            git_root: "/tmp/example".to_string(),
            git_branch: "feature".to_string(),
            git_base_ref: String::new(),
            git_head: "bbbbbbbbbbbbcccc".to_string(),
            git_dirty: true,
            git_untracked_count: 2,
            git_remote_tracking_branch: String::new(),
        };

        let warnings = workspace_drift_warnings(&session, Some(&current));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("branch changed"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("HEAD changed"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("dirty state changed"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("untracked file count changed"))
        );
    }

    #[test]
    fn detects_branch_checked_out_in_another_worktree() {
        let root = test_state_dir("branch-worktree-owner");
        fs::create_dir_all(&root).expect("repo dir should create");
        run_git(&root, &["init", "-b", "dev"]);
        run_git(&root, &["config", "user.email", "nucleus@example.test"]);
        run_git(&root, &["config", "user.name", "Nucleus Test"]);
        fs::write(root.join("README.md"), "hello").expect("file should write");
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-m", "initial"]);

        let feature = root.with_file_name(format!(
            "{}-feature",
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repo")
        ));
        run_git(
            &root,
            &[
                "worktree",
                "add",
                &feature.display().to_string(),
                "-b",
                "feature/test",
            ],
        );

        let owner = branch_worktree_path(&root.display().to_string(), "feature/test")
            .expect("worktree list should parse");
        assert_eq!(owner, Some(feature.display().to_string()));

        let missing = branch_worktree_path(&root.display().to_string(), "feature/missing")
            .expect("worktree list should parse");
        assert_eq!(missing, None);

        let _ = StdCommand::new("git")
            .arg("-C")
            .arg(&root)
            .args([
                "worktree",
                "remove",
                "--force",
                &feature.display().to_string(),
            ])
            .output();
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&feature);
    }

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
    fn compile_session_turn_populates_skill_layers_and_mcp_catalog() {
        let state_dir = test_state_dir("phase4-compiled-turn");
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
                instructions: String::new(),
                activation_mode: "auto".to_string(),
                triggers: vec!["cargo".to_string()],
                include_paths: vec!["skills/rust.md".to_string()],
                required_tools: Vec::new(),
                required_mcps: vec!["mcp.docs".to_string()],
                project_filters: Vec::new(),
                enabled: true,
            })
            .expect("skill manifest should persist");
        install_test_skill_package(
            &store,
            "rust",
            "Rust",
            "# Rust Skill\nPrefer small focused patches.\n",
        );
        store
            .upsert_mcp_server(&McpServerSummary {
                id: "mcp.docs".to_string(),
                title: "Docs MCP".to_string(),
                enabled: true,
                transport: "stdio".to_string(),
                command: String::new(),
                args: Vec::new(),
                env_json: json!({}),
                url: String::new(),
                headers_json: json!({}),
                auth_kind: "none".to_string(),
                auth_ref: String::new(),
                sync_status: "ready".to_string(),
                last_error: String::new(),
                last_synced_at: None,
                tools: vec![NucleusToolDescriptor {
                    id: "mcp.docs.searchDocs".to_string(),
                    title: "searchDocs".to_string(),
                    description: "Search docs".to_string(),
                    input_schema: json!({"type":"object"}),
                    source: "mcp.docs".to_string(),
                }],
                resources: Vec::new(),
            })
            .expect("mcp server should persist");

        store
            .update_workspace(
                Some(&workspace_root.display().to_string()),
                None,
                None,
                None,
                None,
            )
            .expect("workspace root should update");

        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let session = SessionSummary {
            id: "session-1".to_string(),
            title: "Phase 4".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: workspace_root.display().to_string(),
            provider: "openai_compatible".to_string(),
            model: "gpt-5.4-mini".to_string(),
            provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
            provider_api_key: String::new(),
            working_dir: workspace_root.display().to_string(),
            working_dir_kind: "workspace".to_string(),
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
            scope: "workspace".to_string(),
            approval_mode: "ask".to_string(),
            execution_mode: "act".to_string(),
            run_budget_mode: "inherit".to_string(),
            run_budget: RunBudgetSummary::default(),
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

        let compiled =
            compile_session_turn(&state, &session, &[], "Please run cargo test", &[], "main")
                .expect("compiled turn should build");

        assert_eq!(compiled.skill_layers.len(), 1);
        assert_eq!(compiled.skill_layers[0].title, "Rust");
        assert!(
            compiled.skill_layers[0]
                .content
                .contains("small focused patches")
        );
        assert_eq!(compiled.mcp_catalog.len(), 1);
        assert_eq!(compiled.mcp_catalog[0].id, "mcp.docs");
        assert_eq!(compiled.tool_catalog.len(), 1);
        assert_eq!(compiled.tool_catalog[0].id, "mcp.docs.searchDocs");
        assert_eq!(compiled.debug_summary.skill_count, 1);
        assert_eq!(compiled.debug_summary.mcp_server_count, 1);
        assert_eq!(compiled.debug_summary.tool_count, 1);
        assert!(compiled.capabilities.needs_mcp);
        assert!(compiled.capabilities.needs_tools);

        let _ = fs::remove_dir_all(&state_dir);
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
                instructions: String::new(),
                activation_mode: "auto".to_string(),
                triggers: vec!["cargo".to_string()],
                include_paths: vec!["skills/rust.md".to_string()],
                required_tools: Vec::new(),
                required_mcps: Vec::new(),
                project_filters: Vec::new(),
                enabled: true,
            })
            .expect("skill manifest should persist");
        install_test_skill_package(&store, "rust", "Rust", "");

        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
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
            last_message_excerpt: String::new(),
            turn_count: 0,
            created_at: 0,
            updated_at: 0,
        };

        let assembly = assemble_prompt_input(&state, &session, "Run cargo test.")
            .expect("skill includes should assemble");

        assert!(assembly.prompt.contains("Prefer small focused patches."));
        assert!(assembly.prompt.contains("[skill layer: Rust"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn skills_activate_on_title_id_manual_and_package_instructions() {
        let state_dir = test_state_dir("skill-activation");
        let store = initialize_test_store(&state_dir);
        let workspace_root = state_dir.join("workspace");
        store
            .upsert_skill_manifest(&SkillManifest {
                id: "emdash-site-architecture".to_string(),
                title: "EmDash Site Architecture".to_string(),
                description: "Design EmDash CMS sites".to_string(),
                instructions: String::new(),
                activation_mode: "auto".to_string(),
                triggers: Vec::new(),
                include_paths: Vec::new(),
                required_tools: Vec::new(),
                required_mcps: Vec::new(),
                project_filters: Vec::new(),
                enabled: true,
            })
            .expect("skill manifest should persist");
        install_test_skill_package(
            &store,
            "emdash-site-architecture",
            "EmDash Site Architecture",
            "EMDASH_PACKAGE_INSTRUCTIONS",
        );
        let state = test_app_state(&store);
        let session = test_session(&workspace_root);

        let compiled = compile_session_turn(
            &state,
            &session,
            &[],
            "I'd like to work on EmDash Site Architecture with a particular repo i have in mind",
            &[],
            "main",
        )
        .expect("compiled turn should build");
        assert_eq!(compiled.skill_layers.len(), 1);
        assert!(
            compiled.skill_layers[0]
                .content
                .contains("EMDASH_PACKAGE_INSTRUCTIONS")
        );
        assert!(compiled.debug_summary.skill_diagnostics.iter().any(|line| {
            line.contains("emdash-site-architecture selected") && line.contains("exact mention")
        }));

        let compiled = compile_session_turn(
            &state,
            &session,
            &[],
            "Use emdash-site-architecture for this repo",
            &[],
            "main",
        )
        .expect("compiled turn should build");
        assert_eq!(compiled.skill_layers.len(), 1);

        store
            .upsert_skill_manifest(&SkillManifest {
                id: "manual-skill".to_string(),
                title: "Manual Skill".to_string(),
                description: String::new(),
                instructions: String::new(),
                activation_mode: "manual".to_string(),
                triggers: vec!["loose".to_string()],
                include_paths: Vec::new(),
                required_tools: Vec::new(),
                required_mcps: Vec::new(),
                project_filters: Vec::new(),
                enabled: true,
            })
            .expect("manual manifest should persist");
        install_test_skill_package(
            &store,
            "manual-skill",
            "Manual Skill",
            "MANUAL_INSTRUCTIONS",
        );
        let compiled = compile_session_turn(
            &state,
            &session,
            &[],
            "Please use Manual Skill exactly",
            &[],
            "main",
        )
        .expect("compiled turn should build");
        assert!(
            compiled
                .skill_layers
                .iter()
                .any(|layer| layer.content.contains("MANUAL_INSTRUCTIONS"))
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn skill_include_resolution_allows_nucleus_skill_root_and_rejects_other_absolute_paths() {
        let state_dir = test_state_dir("skill-include-security");
        let workspace_root = state_dir.join("workspace");
        fs::create_dir_all(&workspace_root).expect("workspace should exist");
        let skill_dir = state_dir
            .join(".nucleus-eba")
            .join("skills")
            .join("safe-skill");
        fs::create_dir_all(&skill_dir).expect("nucleus skill dir should exist");
        let skill_file = skill_dir.join("SKILL.md");
        fs::write(&skill_file, "SAFE").expect("skill file should write");
        assert!(
            resolve_skill_include_path(
                &workspace_root,
                "safe-skill",
                &skill_file.display().to_string()
            )
            .is_some()
        );
        assert!(resolve_skill_include_path(&workspace_root, "safe-skill", "/etc/passwd").is_none());
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
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
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
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
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
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
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
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
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
                    approval_mode: None,
                    execution_mode: None,
                    run_budget_mode: None,
                    workspace_mode: None,
                    branch_name: None,
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
        assert_eq!(result.0.session.provider_api_key, "");

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
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
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
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
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

    #[tokio::test]
    async fn imports_local_single_skill_and_registers_records() {
        let state_dir = test_state_dir("skill-import-single");
        let store = initialize_test_store(&state_dir);
        let state = test_app_state(&store);
        let source = state_dir.join("source-skill");
        write_test_skill(&source, "Single Skill", "Use the single skill.");

        let response = import_skills_from_source(
            &state,
            SkillImportRequest {
                source: source.display().to_string(),
                scope_kind: String::new(),
                scope_id: String::new(),
            },
        )
        .await
        .expect("local import should succeed");

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        assert_eq!(response.installed.len(), 1);
        let result = &response.installed[0];
        assert_eq!(result.skill_id, "source-skill");
        assert_eq!(result.source_kind, "local");
        assert_eq!(result.status, "installed");
        assert!(result.verification.manifest_registered);
        assert!(result.verification.package_registered);
        assert!(result.verification.installation_registered);
        assert!(!result.content_checksum.is_empty());
        assert!(state_dir.join("skills/source-skill/SKILL.md").is_file());

        let manifests = store.list_skill_manifests().expect("manifests should list");
        let packages = store.list_skill_packages().expect("packages should list");
        let installations = store
            .list_skill_installations()
            .expect("installations should list");
        assert!(manifests.iter().any(|skill| skill.id == "source-skill"));
        let package = packages
            .iter()
            .find(|package| package.id == "nucleus.source-skill")
            .expect("package should exist");
        assert_eq!(
            package.instructions,
            "# Single Skill\n\nUse the single skill.\n"
        );
        assert_eq!(package.source_kind, "local");
        assert_eq!(package.dirty_status, "clean");
        assert!(
            installations
                .iter()
                .any(|installation| installation.id == "workspace.nucleus.source-skill")
        );

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn imports_local_parent_with_multiple_skills() {
        let state_dir = test_state_dir("skill-import-parent");
        let store = initialize_test_store(&state_dir);
        let state = test_app_state(&store);
        let parent = state_dir.join("skill-parent");
        write_test_skill(&parent.join("alpha"), "Alpha", "Use alpha.");
        write_test_skill(&parent.join("beta"), "Beta", "Use beta.");

        let response = import_skills_from_source(
            &state,
            SkillImportRequest {
                source: parent.display().to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "default".to_string(),
            },
        )
        .await
        .expect("parent import should succeed");

        assert!(
            response.errors.is_empty(),
            "unexpected errors: {:?}",
            response.errors
        );
        let installed = response
            .installed
            .iter()
            .map(|result| result.skill_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(installed, vec!["alpha", "beta"]);
        let packages = store.list_skill_packages().expect("packages should list");
        assert!(packages.iter().any(|package| package.id == "nucleus.alpha"));
        assert!(packages.iter().any(|package| package.id == "nucleus.beta"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn parses_github_tree_url_with_subpath() {
        let meta = parse_github_tree_url(
            "https://github.com/coreyhaines31/marketingskills/tree/main/skills",
        )
        .expect("GitHub tree URL should parse");
        assert_eq!(meta.source_kind, "github");
        assert_eq!(meta.source_owner, "coreyhaines31");
        assert_eq!(meta.source_repo, "marketingskills");
        assert_eq!(meta.source_ref, "main");
        assert_eq!(meta.source_parent_path, "skills");
        assert_eq!(
            meta.source_repo_url,
            "https://github.com/coreyhaines31/marketingskills"
        );
    }

    #[test]
    fn discovers_multi_skill_subpath() {
        let state_dir = test_state_dir("skill-discovery");
        let parent = state_dir.join("repo/skills");
        write_test_skill(
            &parent.join("copywriting"),
            "Copywriting",
            "Use copywriting.",
        );
        write_test_skill(
            &parent.join("marketing-ideas"),
            "Marketing Ideas",
            "Use ideas.",
        );
        fs::create_dir_all(parent.join("not-a-skill")).expect("non skill dir should create");

        let discovered = discover_skill_dirs(&parent).expect("discovery should succeed");
        let names = discovered
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["copywriting", "marketing-ideas"]);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn rejects_invalid_skill_folder_without_skill_md() {
        let state_dir = test_state_dir("skill-invalid");
        let store = initialize_test_store(&state_dir);
        let state = test_app_state(&store);
        let invalid = state_dir.join("invalid-skill");
        fs::create_dir_all(&invalid).expect("invalid dir should create");

        let error = install_skill_dir(
            &state,
            &invalid,
            &ImportSourceMeta {
                source_kind: "local".to_string(),
                source_url: invalid.display().to_string(),
                source_repo_url: String::new(),
                source_owner: String::new(),
                source_repo: String::new(),
                source_ref: String::new(),
                source_parent_path: invalid.display().to_string(),
                source_skill_path: String::new(),
                source_commit: String::new(),
            },
            "workspace",
            "default",
        )
        .expect_err("invalid skill should be rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn rejects_unsafe_archive_paths() {
        validate_relative_archive_path("repo/skills/copywriting/SKILL.md")
            .expect("relative archive path should be allowed");
        validate_relative_archive_path("../escape/SKILL.md")
            .expect_err("parent traversal should be rejected");
        validate_relative_archive_path("/tmp/escape/SKILL.md")
            .expect_err("absolute paths should be rejected");
    }

    #[test]
    fn safe_tar_unpack_extracts_safe_entries() {
        let state_dir = test_state_dir("skill-safe-tar");
        let mut tar_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let data = b"# Good\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "repo/skills/good/SKILL.md", &data[..])
                .expect("tar entry should append");
            let encoder = builder.into_inner().expect("tar should finish");
            encoder.finish().expect("gzip should finish");
        }

        safe_unpack_tar_gz(&tar_bytes, &state_dir).expect("safe tar should unpack");
        assert!(state_dir.join("repo/skills/good/SKILL.md").is_file());

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn reconcile_registers_existing_folders_and_is_idempotent() {
        let state_dir = test_state_dir("skill-reconcile");
        let store = initialize_test_store(&state_dir);
        let state = test_app_state(&store);
        write_test_skill(
            &state_dir.join("skills/copywriting"),
            "Copywriting",
            "Use copywriting.",
        );
        write_test_skill(
            &state_dir.join("skills/marketing-ideas"),
            "Marketing Ideas",
            "Use marketing ideas.",
        );

        let first = reconcile_skill_folders(&state, &[]).expect("reconcile should succeed");
        assert!(
            first.errors.is_empty(),
            "unexpected errors: {:?}",
            first.errors
        );
        assert_eq!(first.installed.len(), 2);
        let second = reconcile_skill_folders(&state, &[]).expect("second reconcile should succeed");
        assert!(
            second.errors.is_empty(),
            "unexpected errors: {:?}",
            second.errors
        );
        assert_eq!(second.installed.len(), 2);

        let manifests = store.list_skill_manifests().expect("manifests should list");
        let packages = store.list_skill_packages().expect("packages should list");
        let installations = store
            .list_skill_installations()
            .expect("installations should list");
        assert!(manifests.iter().any(|skill| skill.id == "copywriting"));
        assert!(manifests.iter().any(|skill| skill.id == "marketing-ideas"));
        assert!(
            packages
                .iter()
                .any(|package| package.id == "nucleus.copywriting"
                    && package.source_kind == "unknown"
                    && !package.content_checksum.is_empty())
        );
        assert!(
            packages
                .iter()
                .any(|package| package.id == "nucleus.marketing-ideas"
                    && package.instructions.contains("Use marketing ideas."))
        );
        assert_eq!(installations.len(), 2);

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn check_update_detects_local_dirty_modification() {
        let state_dir = test_state_dir("skill-dirty");
        let store = initialize_test_store(&state_dir);
        let state = test_app_state(&store);
        let source = state_dir.join("dirty-source");
        write_test_skill(&source, "Dirty Source", "Original instructions.");
        let response = import_skills_from_source(
            &state,
            SkillImportRequest {
                source: source.display().to_string(),
                scope_kind: String::new(),
                scope_id: String::new(),
            },
        )
        .await
        .expect("import should succeed");
        assert_eq!(response.installed[0].dirty_status, "clean");

        fs::write(state_dir.join("skills/dirty-source/notes.md"), "local edit")
            .expect("local edit should write");
        let result = check_one_skill_update(&state, "dirty-source")
            .await
            .expect("check update should succeed");
        assert_eq!(result.dirty_status, "modified");

        let _ = fs::remove_dir_all(&state_dir);
    }

    fn write_test_skill(dir: &std::path::Path, title: &str, body: &str) {
        fs::create_dir_all(dir).expect("skill dir should create");
        fs::write(dir.join("SKILL.md"), format!("# {title}\n\n{body}\n"))
            .expect("SKILL.md should write");
        fs::create_dir_all(dir.join("references")).expect("references dir should create");
        fs::write(dir.join("references/example.md"), "reference").expect("reference should write");
    }

    fn run_git(root: &FsPath, args: &[&str]) {
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_state_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("nucleus-{label}-{}-{suffix}", std::process::id()))
    }

    fn install_test_skill_package(
        store: &Arc<StateStore>,
        skill_id: &str,
        name: &str,
        instructions: &str,
    ) {
        let package_id = format!("nucleus.{skill_id}");
        store
            .upsert_skill_package(&SkillPackageRecord {
                id: package_id.clone(),
                name: name.to_string(),
                version: "1.0.0".to_string(),
                manifest_json: json!({"id": skill_id, "title": name, "name": name}),
                instructions: instructions.to_string(),
                source_kind: "manual".to_string(),
                source_url: String::new(),
                source_repo_url: String::new(),
                source_owner: String::new(),
                source_repo: String::new(),
                source_ref: String::new(),
                source_parent_path: String::new(),
                source_skill_path: String::new(),
                source_commit: String::new(),
                imported_at: Some(0),
                last_checked_at: None,
                latest_source_commit: String::new(),
                update_status: "unknown".to_string(),
                content_checksum: String::new(),
                dirty_status: "unknown".to_string(),
                created_at: 0,
                updated_at: 0,
            })
            .expect("skill package should persist");
        store
            .upsert_skill_installation(&SkillInstallationRecord {
                id: format!("workspace.{package_id}"),
                package_id,
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                enabled: true,
                pinned_version: None,
                created_at: 0,
                updated_at: 0,
            })
            .expect("skill installation should persist");
    }

    fn test_app_state(store: &Arc<StateStore>) -> AppState {
        let (events, _) = broadcast::channel(4);
        AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        }
    }

    fn test_named_app_state(name: &str) -> (PathBuf, AppState) {
        let state_dir = test_state_dir(name);
        let store = initialize_test_store(&state_dir);
        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };
        (state_dir, state)
    }

    #[test]
    fn memory_upsert_normalizes_phase2_defaults() {
        let (state_dir, state) = test_named_app_state("memory-defaults");
        let entry = upsert_memory_from_request(
            &state,
            MemoryEntryUpsertRequest {
                id: Some("phase2-defaults".to_string()),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Phase 2 defaults".to_string(),
                content: "Remember this preference.".to_string(),
                tags: vec![" docs ".to_string(), "".to_string()],
                enabled: None,
                status: None,
                memory_kind: None,
                source_kind: None,
                source_id: None,
                confidence: Some(1.8),
                created_by: None,
                last_used_at: None,
                use_count: Some(-3),
                supersedes_id: None,
                metadata_json: None,
            },
            None,
        )
        .expect("memory should save");

        assert_eq!(entry.status, "accepted");
        assert_eq!(entry.memory_kind, "note");
        assert_eq!(entry.source_kind, "manual");
        assert_eq!(entry.created_by, "user");
        assert_eq!(entry.confidence, 1.0);
        assert_eq!(entry.use_count, 0);
        assert_eq!(entry.tags, vec!["docs"]);
        assert_eq!(entry.metadata_json, json!({}));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn memory_upsert_redacts_sensitive_metadata() {
        let (state_dir, state) = test_named_app_state("memory-metadata-redaction");
        let entry = upsert_memory_from_request(
            &state,
            MemoryEntryUpsertRequest {
                id: Some("metadata-redaction".to_string()),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Metadata redaction".to_string(),
                content: "Safe visible memory.".to_string(),
                tags: Vec::new(),
                enabled: None,
                status: None,
                memory_kind: Some("decision".to_string()),
                source_kind: Some("explicit_remember".to_string()),
                source_id: None,
                confidence: None,
                created_by: Some("assistant".to_string()),
                last_used_at: None,
                use_count: None,
                supersedes_id: None,
                metadata_json: Some(json!({
                    "api_key": "nuctk_secret",
                    "nested": { "password": "also-secret", "token": "bearer-secret" },
                    "safe": "ok"
                })),
            },
            None,
        )
        .expect("memory should save");

        assert_eq!(entry.memory_kind, "decision");
        assert_eq!(entry.source_kind, "explicit_remember");
        assert_eq!(entry.created_by, "assistant");
        assert_eq!(entry.metadata_json["api_key"], "[REDACTED_SECRET]");
        assert_eq!(
            entry.metadata_json["nested"]["password"],
            "[REDACTED_SECRET]"
        );
        assert_eq!(entry.metadata_json["nested"]["token"], "[REDACTED_SECRET]");
        assert_eq!(entry.metadata_json["safe"], "ok");
        let _ = fs::remove_dir_all(&state_dir);
    }

    fn create_test_persisted_session(
        state: &AppState,
        session_id: &str,
        workspace_root: &std::path::Path,
    ) -> SessionSummary {
        state
            .store
            .create_session(SessionRecord {
                id: session_id.to_string(),
                title: "Test session".to_string(),
                profile_id: String::new(),
                profile_title: String::new(),
                route_id: String::new(),
                route_title: String::new(),
                scope: "workspace".to_string(),
                project_id: String::new(),
                project_title: String::new(),
                project_path: workspace_root.display().to_string(),
                project_ids: Vec::new(),
                provider: "openai_compatible".to_string(),
                model: "gpt-5.4-mini".to_string(),
                provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
                provider_api_key: String::new(),
                working_dir: workspace_root.display().to_string(),
                working_dir_kind: "workspace".to_string(),
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
                run_budget_mode: "inherit".to_string(),
            })
            .expect("test session should persist")
    }

    fn test_memory_entry(id: &str, scope_kind: &str, scope_id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            scope_kind: scope_kind.to_string(),
            scope_id: scope_id.to_string(),
            title: id.to_string(),
            content: content.to_string(),
            tags: Vec::new(),
            enabled: true,
            status: "accepted".to_string(),
            memory_kind: "note".to_string(),
            source_kind: "manual".to_string(),
            source_id: String::new(),
            confidence: 1.0,
            created_by: "user".to_string(),
            last_used_at: None,
            use_count: 0,
            supersedes_id: String::new(),
            metadata_json: json!({}),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[tokio::test]
    async fn automatic_memory_extraction_creates_pending_only_and_is_prompt_safe() {
        let (state_dir, state) = test_named_app_state("memory-auto-extraction");
        let workspace_root = state_dir.join("workspace");
        fs::create_dir_all(&workspace_root).unwrap();
        let session = create_test_persisted_session(&state, "auto-session", &workspace_root);
        state
            .store
            .append_session_turn(
                &session.id,
                "user-turn",
                "user",
                "Please capture durable preferences.",
                &[],
            )
            .unwrap();
        let assistant = r#"Done.
<memory_candidates>[{"title":"Preferred release notes","content":"The workspace prefers concise release notes with exact check results.","candidate_kind":"preference","tags":["release"],"evidence":["User asked for concise exact checks."],"reason":"Useful for future release work.","confidence":0.86}]</memory_candidates>"#;
        state
            .store
            .append_session_turn(&session.id, "assistant-turn", "assistant", assistant, &[])
            .unwrap();

        extract_memory_candidates_after_successful_turn(&state, &session.id, "assistant-turn")
            .await;
        let candidates = state.store.list_memory_candidates().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status, "pending");
        assert_eq!(
            state.store.list_memory_entries().unwrap().len(),
            0,
            "automatic extraction must not create accepted memory"
        );

        let compiled = compile_session_turn(&state, &session, &[], "future", &[], "main").unwrap();
        let rendered = nucleus_core::render_compiled_turn_system_text(&compiled);
        assert!(!rendered.contains("concise release notes"));

        let entry = accept_memory_candidate(
            State(state.clone()),
            Path(candidates[0].id.clone()),
            Bytes::new(),
        )
        .await
        .unwrap()
        .0;
        let compiled = compile_session_turn(&state, &session, &[], "future", &[], "main").unwrap();
        let rendered = nucleus_core::render_compiled_turn_system_text(&compiled);
        assert!(rendered.contains(&entry.content));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn automatic_memory_extraction_failure_secrets_dedupe_and_audit_are_safe() {
        let (state_dir, state) = test_named_app_state("memory-auto-guardrails");
        let workspace_root = state_dir.join("workspace");
        fs::create_dir_all(&workspace_root).unwrap();
        let session = create_test_persisted_session(&state, "auto-guardrails", &workspace_root);
        state
            .store
            .append_session_turn(
                &session.id,
                "user-turn",
                "user",
                "Remember operational context.",
                &[],
            )
            .unwrap();
        let secret = "sk-raw-secret-test-value";
        let assistant = format!(
            r#"Done.
<memory_candidates>[
{{"title":"Safe durable note","content":"The project prefers candidate review before durable memory acceptance.","candidate_kind":"decision","reason":"Future useful.","confidence":0.8}},
{{"title":"Unsafe token","content":"authorization: bearer {secret}","candidate_kind":"note","reason":"Must be skipped.","confidence":0.9}}
]</memory_candidates>"#
        );
        state
            .store
            .append_session_turn(&session.id, "assistant-safe", "assistant", &assistant, &[])
            .unwrap();
        extract_memory_candidates_after_successful_turn(&state, &session.id, "assistant-safe")
            .await;
        extract_memory_candidates_after_successful_turn(&state, &session.id, "assistant-safe")
            .await;
        let candidates = state.store.list_memory_candidates().unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "duplicate and secret-like extracted candidates should be skipped"
        );
        assert!(!format!("{candidates:?}").contains(secret));

        state
            .store
            .append_session_turn(
                &session.id,
                "assistant-fail",
                "assistant",
                "NUCLEUS_MEMORY_EXTRACT_FAIL",
                &[],
            )
            .unwrap();
        extract_memory_candidates_after_successful_turn(&state, &session.id, "assistant-fail")
            .await;
        assert!(
            state
                .store
                .get_session(&session.id)
                .unwrap()
                .turns
                .iter()
                .any(|turn| turn.id == "assistant-fail")
        );
        let audits = state.store.list_audit_events(50).unwrap();
        let rendered_audits = format!("{audits:?}");
        assert!(rendered_audits.contains("memory.candidate.extraction_failed"));
        assert!(!rendered_audits.contains(secret));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn memory_candidate_lifecycle_accepts_rejects_and_dismisses() {
        let (state_dir, state) = test_named_app_state("memory-candidate-lifecycle");
        let candidate = upsert_memory_candidate_from_request(
            &state,
            MemoryCandidateUpsertRequest {
                id: Some("candidate-one".to_string()),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                session_id: Some("session-one".to_string()),
                turn_id_start: Some("turn-a".to_string()),
                turn_id_end: Some("turn-b".to_string()),
                candidate_kind: Some("decision".to_string()),
                title: "Use compact memory".to_string(),
                content: "Nucleus should keep memory candidates review-only until accepted."
                    .to_string(),
                tags: vec!["memory".to_string()],
                evidence: vec!["User asked for candidate review.".to_string()],
                reason: Some("Durable product behavior.".to_string()),
                confidence: Some(0.9),
                status: None,
                dedupe_key: None,
                accepted_memory_id: None,
                created_by: None,
                metadata_json: None,
            },
            None,
            true,
        )
        .await
        .expect("candidate should save");
        assert_eq!(candidate.status, "pending");

        let entry = accept_memory_candidate(
            State(state.clone()),
            Path(candidate.id.clone()),
            Bytes::from_static(br#"{"content":"Edited accepted memory."}"#),
        )
        .await
        .expect("candidate should accept")
        .0;
        assert_eq!(entry.source_kind, "candidate");
        assert_eq!(entry.content, "Edited accepted memory.");
        let accepted = state.store.load_memory_candidate(&candidate.id).unwrap();
        assert_eq!(accepted.status, "accepted");
        assert_eq!(accepted.accepted_memory_id, entry.id);

        let rejected = upsert_memory_candidate_from_request(
            &state,
            MemoryCandidateUpsertRequest {
                id: Some("reject-me".to_string()),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Reject me".to_string(),
                content: "A non-secret pending candidate.".to_string(),
                session_id: None,
                turn_id_start: None,
                turn_id_end: None,
                candidate_kind: None,
                tags: vec![],
                evidence: vec![],
                reason: None,
                confidence: None,
                status: None,
                dedupe_key: None,
                accepted_memory_id: None,
                created_by: None,
                metadata_json: None,
            },
            None,
            false,
        )
        .await
        .unwrap();
        let rejected = reject_memory_candidate(State(state.clone()), Path(rejected.id))
            .await
            .unwrap()
            .0;
        assert_eq!(rejected.status, "rejected");

        let dismissed = upsert_memory_candidate_from_request(
            &state,
            MemoryCandidateUpsertRequest {
                id: Some("dismiss-me".to_string()),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Dismiss me".to_string(),
                content: "A candidate to dismiss.".to_string(),
                session_id: None,
                turn_id_start: None,
                turn_id_end: None,
                candidate_kind: None,
                tags: vec![],
                evidence: vec![],
                reason: None,
                confidence: None,
                status: None,
                dedupe_key: None,
                accepted_memory_id: None,
                created_by: None,
                metadata_json: None,
            },
            None,
            false,
        )
        .await
        .unwrap();
        delete_memory_candidate(State(state.clone()), Path(dismissed.id.clone()))
            .await
            .unwrap();
        assert_eq!(
            state
                .store
                .load_memory_candidate(&dismissed.id)
                .unwrap()
                .status,
            "dismissed"
        );
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn memory_candidate_prompt_visibility_and_dedupe_guardrails() {
        let (state_dir, state) = test_named_app_state("memory-candidate-prompt");
        let workspace_root = state_dir.join("workspace");
        fs::create_dir_all(&workspace_root).expect("workspace should exist");
        let session = test_session(&workspace_root);
        let payload = MemoryCandidateUpsertRequest {
            id: Some("pending-memory".to_string()),
            scope_kind: "workspace".to_string(),
            scope_id: "workspace".to_string(),
            title: "Pending invisible".to_string(),
            content: "Pending candidates must not enter compiled prompts.".to_string(),
            session_id: None,
            turn_id_start: None,
            turn_id_end: None,
            candidate_kind: None,
            tags: vec![],
            evidence: vec![],
            reason: None,
            confidence: None,
            status: None,
            dedupe_key: None,
            accepted_memory_id: None,
            created_by: None,
            metadata_json: None,
        };
        upsert_memory_candidate_from_request(&state, payload.clone(), None, false)
            .await
            .unwrap();
        let mut duplicate_payload = payload;
        duplicate_payload.id = Some("pending-memory-duplicate".to_string());
        upsert_memory_candidate_from_request(&state, duplicate_payload, None, false)
            .await
            .expect_err("duplicate pending candidates should be rejected");
        let compiled = compile_session_turn(&state, &session, &[], "prompt", &[], "main").unwrap();
        assert!(
            !nucleus_core::render_compiled_turn_system_text(&compiled)
                .contains("Pending candidates must not enter")
        );
        let entry = accept_memory_candidate(
            State(state.clone()),
            Path("pending-memory".to_string()),
            Bytes::new(),
        )
        .await
        .unwrap()
        .0;
        let compiled = compile_session_turn(&state, &session, &[], "prompt", &[], "main").unwrap();
        assert!(nucleus_core::render_compiled_turn_system_text(&compiled).contains(&entry.content));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn explicit_remember_and_candidates_reject_credential_like_content() {
        let (state_dir, state) = test_named_app_state("memory-secret-guardrails");
        let entry = explicit_remember(
            State(state.clone()),
            Bytes::from_static(br#"{"id":"remember-safely","scope_kind":"workspace","scope_id":"workspace","title":"Remember safely","content":"This project uses a project-scoped token stored in Vault."}"#),
        ).await.unwrap().0;
        assert_eq!(entry.source_kind, "explicit_remember");

        let bad = upsert_memory_candidate_from_request(
            &state,
            MemoryCandidateUpsertRequest {
                id: Some("secret-candidate".to_string()),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Secret".to_string(),
                content: "authorization: bearer sk-super-secret".to_string(),
                session_id: None,
                turn_id_start: None,
                turn_id_end: None,
                candidate_kind: None,
                tags: vec![],
                evidence: vec![],
                reason: None,
                confidence: None,
                status: None,
                dedupe_key: None,
                accepted_memory_id: None,
                created_by: None,
                metadata_json: None,
            },
            None,
            true,
        )
        .await;
        assert!(bad.is_err());
        let audits = state.store.list_audit_events(20).unwrap();
        assert!(!format!("{audits:?}").contains("sk-super-secret"));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn memory_search_returns_accepted_scoped_entries_only() {
        let (state_dir, state) = test_named_app_state("memory-fts-search");
        state
            .store
            .upsert_memory_entry(&MemoryEntry {
                id: "workspace-searchable".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Validation style".to_string(),
                content: "Use exact validation commands in reports.".to_string(),
                tags: vec!["reports".to_string()],
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
                metadata_json: json!({}),
                created_at: 0,
                updated_at: 0,
            })
            .expect("accepted memory should persist");
        state
            .store
            .upsert_memory_entry(&MemoryEntry {
                id: "archived-searchable".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Archived validation style".to_string(),
                content: "Archived exact validation commands must not return.".to_string(),
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
                metadata_json: json!({}),
                created_at: 0,
                updated_at: 0,
            })
            .expect("archived memory should persist");
        upsert_memory_candidate_from_request(
            &state,
            MemoryCandidateUpsertRequest {
                id: Some("pending-validation-candidate".to_string()),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Pending validation".to_string(),
                content: "Pending validation candidates must not return in search.".to_string(),
                session_id: None,
                turn_id_start: None,
                turn_id_end: None,
                candidate_kind: None,
                tags: Vec::new(),
                evidence: Vec::new(),
                reason: None,
                confidence: None,
                status: Some("pending".to_string()),
                dedupe_key: None,
                accepted_memory_id: None,
                created_by: None,
                metadata_json: None,
            },
            None,
            false,
        )
        .await
        .expect("pending candidate should persist");

        let response = search_memory(
            State(state.clone()),
            Query(MemorySearchQuery {
                q: "validation".to_string(),
                scope_kind: Some("workspace".to_string()),
                scope_id: Some("workspace".to_string()),
                session_id: None,
                limit: Some(10),
            }),
        )
        .await
        .expect("memory search should succeed")
        .0;

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].entry.id, "workspace-searchable");
        assert_eq!(response.results[0].entry.use_count, 1);
        assert!(response.results[0].entry.last_used_at.is_some());
        let debug = format!("{response:?}");
        assert!(!debug.contains("Pending validation candidates"));
        assert!(!debug.contains("Archived exact validation"));
        assert!(!debug.contains("sk-super-secret"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn memory_search_session_filtering_happens_before_final_limit() {
        let (state_dir, state) = test_named_app_state("memory-fts-session-limit");
        let workspace_root = state_dir.join("workspace");
        fs::create_dir_all(&workspace_root).expect("workspace root should exist");
        let session = create_test_persisted_session(&state, "search-session", &workspace_root);

        for index in 0..3 {
            state
                .store
                .upsert_memory_entry(&test_memory_entry(
                    &format!("other-project-{index}"),
                    "project",
                    "other-project",
                    "validation validation validation non-applicable project memory",
                ))
                .expect("non-applicable memory should persist");
        }
        state
            .store
            .upsert_memory_entry(&test_memory_entry(
                "applicable-session-memory",
                "session",
                &session.id,
                "validation applicable session memory should survive final limit",
            ))
            .expect("session memory should persist");
        state
            .store
            .upsert_memory_entry(&test_memory_entry(
                "second-applicable-session-memory",
                "session",
                &session.id,
                "validation second applicable session memory should be truncated by final limit",
            ))
            .expect("second session memory should persist");

        let response = search_memory(
            State(state.clone()),
            Query(MemorySearchQuery {
                q: "validation".to_string(),
                scope_kind: None,
                scope_id: None,
                session_id: Some(session.id.clone()),
                limit: Some(1),
            }),
        )
        .await
        .expect("session-filtered memory search should succeed")
        .0;

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].entry.scope_kind, "session");
        assert_eq!(response.results[0].entry.scope_id, session.id);
        assert!(
            response.results[0]
                .entry
                .id
                .contains("applicable-session-memory")
        );
        assert_eq!(response.results[0].entry.use_count, 1);
        assert!(response.results[0].entry.last_used_at.is_some());
        let debug = format!("{response:?}");
        assert!(!debug.contains("non-applicable project memory"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn compiled_turn_truncates_multibyte_memory_without_panic() {
        let (state_dir, state) = test_named_app_state("memory-emoji-truncation");
        let workspace_root = state_dir.join("workspace");
        state
            .store
            .upsert_memory_entry(&MemoryEntry {
                id: "emoji-memory".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Emoji memory".to_string(),
                content: "🚀".repeat(MAX_MEMORY_CONTEXT_CHARS),
                tags: Vec::new(),
                enabled: true,
                status: "accepted".to_string(),
                memory_kind: "note".to_string(),
                source_kind: "manual".to_string(),
                source_id: String::new(),
                confidence: 1.0,
                created_by: "user".to_string(),
                last_used_at: None,
                use_count: 0,
                supersedes_id: String::new(),
                metadata_json: json!({}),
                created_at: 0,
                updated_at: 0,
            })
            .expect("emoji memory should persist");

        let session = test_session(&workspace_root);
        let compiled = compile_session_turn(&state, &session, &[], "Hello", &[], "main")
            .expect("multibyte memory should compile without panic");
        let rendered = nucleus_core::render_compiled_turn_system_text(&compiled);
        assert!(rendered.contains("[Memory entry truncated by Nucleus context budget]"));
        assert_eq!(compiled.debug_summary.memory_truncated_count, 1);
        let memory_layer = compiled
            .project_layers
            .iter()
            .find(|layer| layer.kind == "memory")
            .expect("memory layer should be present");
        assert!(memory_layer.content.len() <= MAX_MEMORY_CONTEXT_CHARS);
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn compiled_turn_includes_prompt_includes_and_accepted_memory() {
        let (state_dir, state) = test_named_app_state("memory-compiled-context");
        let workspace_root = state_dir.join("workspace");
        let include_dir = workspace_root.join("include");
        fs::create_dir_all(&include_dir).expect("include dir should exist");
        fs::write(
            include_dir.join("phase2.md"),
            "Provider-visible include text.",
        )
        .expect("include should write");
        state
            .store
            .upsert_memory_entry(&MemoryEntry {
                id: "workspace-memory".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Workspace memory".to_string(),
                content: "Provider-visible accepted memory.".to_string(),
                tags: vec!["phase2".to_string()],
                enabled: true,
                status: "accepted".to_string(),
                memory_kind: "note".to_string(),
                source_kind: "manual".to_string(),
                source_id: String::new(),
                confidence: 1.0,
                created_by: "user".to_string(),
                last_used_at: None,
                use_count: 0,
                supersedes_id: String::new(),
                metadata_json: json!({}),
                created_at: 0,
                updated_at: 0,
            })
            .expect("memory should persist");
        state
            .store
            .upsert_memory_entry(&MemoryEntry {
                id: "archived-memory".to_string(),
                scope_kind: "workspace".to_string(),
                scope_id: "workspace".to_string(),
                title: "Archived memory".to_string(),
                content: "This archived memory must not appear.".to_string(),
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
                metadata_json: json!({}),
                created_at: 0,
                updated_at: 0,
            })
            .expect("archived memory should persist");

        let session = test_session(&workspace_root);
        let compiled = compile_session_turn(&state, &session, &[], "Hello", &[], "main")
            .expect("turn should compile");
        let rendered = nucleus_core::render_compiled_turn_system_text(&compiled);

        assert!(rendered.contains("Provider-visible include text."));
        assert!(rendered.contains("Provider-visible accepted memory."));
        assert!(!rendered.contains("This archived memory must not appear."));
        assert_eq!(compiled.debug_summary.include_count, 1);
        assert_eq!(compiled.debug_summary.memory_count, 2);
        assert_eq!(compiled.debug_summary.memory_included_count, 1);
        assert_eq!(compiled.debug_summary.memory_skipped_count, 1);
        let include_index = rendered.find("Provider-visible include text.").unwrap();
        let memory_index = rendered.find("Provider-visible accepted memory.").unwrap();
        assert!(include_index < memory_index);
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn vault_initializes_unlocks_and_encrypts_without_plaintext_storage() {
        let (state_dir, state) = test_named_app_state("vault-init-encrypt");
        {
            let mut vault = state.vault.lock().await;
            vault
                .initialize(&state.store, "correct horse battery staple")
                .expect("vault initializes");
            let secret = vault
                .create_or_update_secret(
                    &state.store,
                    vault::VaultSecretInput {
                        id: Some("cloudflare-token".to_string()),
                        scope_kind: "workspace".to_string(),
                        scope_id: "workspace".to_string(),
                        name: "CLOUDFLARE_API_TOKEN".to_string(),
                        description: "Cloudflare API token".to_string(),
                        secret: "super-secret-token".to_string(),
                    },
                )
                .expect("secret persists encrypted");
            let decrypted = vault::decrypt_secret_for_test(&mut vault, &state.store, &secret)
                .expect("secret decrypts while unlocked");
            assert_eq!(decrypted, "super-secret-token");
        }

        let raw = state
            .store
            .load_vault_secret("cloudflare-token")
            .expect("secret should load");
        assert_ne!(raw.ciphertext, b"super-secret-token");
        let ciphertext_text = String::from_utf8_lossy(&raw.ciphertext);
        assert!(!ciphertext_text.contains("super-secret-token"));
        {
            let mut vault = state.vault.lock().await;
            vault.lock();
            vault
                .unlock(&state.store, "correct horse battery staple")
                .expect("correct passphrase unlocks");
            let mut tampered = raw.clone();
            tampered.aad.push_str(":tampered");
            let error = vault::decrypt_secret_for_test(&mut vault, &state.store, &tampered)
                .expect_err("tampered AAD fails");
            assert!(error.to_string().contains("failed to decrypt vault secret"));
        }

        let listed = state
            .store
            .list_vault_secrets(None, None)
            .expect("secrets should list")
            .into_iter()
            .map(vault_secret_summary)
            .collect::<Vec<_>>();
        let serialized = serde_json::to_string(&listed).expect("summary serializes");
        assert!(serialized.contains("CLOUDFLARE_API_TOKEN"));
        assert!(!serialized.contains("super-secret-token"));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn vault_wrong_passphrase_fails_and_lock_blocks_secret_writes() {
        let (state_dir, state) = test_named_app_state("vault-lock-wrong-pass");
        {
            let mut vault = state.vault.lock().await;
            vault
                .initialize(&state.store, "correct horse battery staple")
                .expect("vault initializes");
            vault.lock();
        }
        {
            let mut vault = state.vault.lock().await;
            let error = vault
                .unlock(&state.store, "wrong horse battery staple")
                .expect_err("wrong passphrase fails");
            assert!(error.to_string().contains("invalid vault passphrase"));
            let error = vault
                .create_or_update_secret(
                    &state.store,
                    vault::VaultSecretInput {
                        id: Some("blocked".to_string()),
                        scope_kind: "workspace".to_string(),
                        scope_id: "workspace".to_string(),
                        name: "BLOCKED".to_string(),
                        description: String::new(),
                        secret: "must-not-store".to_string(),
                    },
                )
                .expect_err("locked vault blocks writes");
            assert!(error.to_string().contains("vault is locked"));
        }
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn vault_plaintext_operations_require_safe_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://192.168.1.2:5201"),
        );
        let error = require_vault_safe_origin(&headers).expect_err("LAN HTTP is unsafe");
        assert_eq!(error.status, StatusCode::FORBIDDEN);

        let mut safe_headers = HeaderMap::new();
        safe_headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        require_vault_safe_origin(&safe_headers).expect("localhost origin is vault-safe");
    }

    #[tokio::test]
    async fn vault_audit_payloads_do_not_include_secret_values() {
        let (state_dir, state) = test_named_app_state("vault-audit-redacted");
        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        let _ = vault_init(
            State(state.clone()),
            headers.clone(),
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("vault init succeeds");
        let _ = create_vault_secret(
            State(state.clone()),
            headers,
            Bytes::from(r#"{"id":"audit-token","scope_kind":"workspace","scope_id":"workspace","name":"AUDIT_TOKEN","secret":"audit-secret-value"}"#),
        )
        .await
        .expect("secret create succeeds");
        let audit = state.store.list_audit_events(20).expect("audit lists");
        let serialized = serde_json::to_string(&audit).expect("audit serializes");
        assert!(serialized.contains("vault.secret.created"));
        assert!(!serialized.contains("audit-secret-value"));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn vault_policy_api_is_metadata_only_and_requires_safe_unlocked_vault() {
        let (state_dir, state) = test_named_app_state("vault-policy-api");
        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        let _ = vault_init(
            State(state.clone()),
            headers.clone(),
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("vault init succeeds");
        let _ = create_vault_secret(
            State(state.clone()),
            headers.clone(),
            Bytes::from(r#"{"id":"policy-token","scope_kind":"workspace","scope_id":"workspace","name":"POLICY_TOKEN","secret":"policy-secret-value"}"#),
        )
        .await
        .expect("secret create succeeds");

        let policy = upsert_vault_secret_policy(
            State(state.clone()),
            Path("policy-token".to_string()),
            Query(VaultSecretPolicyScopeQuery { scope_kind: None, scope_id: None }),
            headers.clone(),
            Bytes::from(r#"{"consumer_kind":"mcp","consumer_id":"server-1","permission":"read","approval_mode":"ask"}"#),
        )
        .await
        .expect("policy upsert succeeds")
        .0;
        assert_eq!(policy.consumer_kind, "mcp");
        assert_eq!(policy.approval_mode, "ask");

        let listed = list_vault_secret_policies(
            State(state.clone()),
            Path("policy-token".to_string()),
            Query(VaultSecretPolicyScopeQuery {
                scope_kind: None,
                scope_id: None,
            }),
        )
        .await
        .expect("policies list")
        .0;
        assert_eq!(listed.policies.len(), 1);
        let serialized = serde_json::to_string(&listed).expect("policy list serializes");
        assert!(!serialized.contains("policy-secret-value"));

        let _ = vault_lock(State(state.clone()))
            .await
            .expect("lock succeeds");
        let locked_error = upsert_vault_secret_policy(
            State(state.clone()),
            Path("policy-token".to_string()),
            Query(VaultSecretPolicyScopeQuery { scope_kind: None, scope_id: None }),
            headers.clone(),
            Bytes::from(r#"{"consumer_kind":"mcp","consumer_id":"server-2","permission":"read","approval_mode":"ask"}"#),
        )
        .await
        .expect_err("locked vault rejects policy writes");
        assert_eq!(locked_error.status, StatusCode::FORBIDDEN);

        let audit = state.store.list_audit_events(20).expect("audit lists");
        let audit_serialized = serde_json::to_string(&audit).expect("audit serializes");
        assert!(audit_serialized.contains("vault.secret.policy.updated"));
        assert!(!audit_serialized.contains("policy-secret-value"));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn vault_persists_locked_across_restart_and_unlocks_metadata_only() {
        let state_dir = test_state_dir("vault-restart-persistence");
        let store = initialize_test_store(&state_dir);
        let state = test_app_state(&store);
        {
            let mut vault = state.vault.lock().await;
            vault
                .initialize(&state.store, "correct horse battery staple")
                .expect("vault initializes");
            vault
                .create_or_update_secret(
                    &state.store,
                    vault::VaultSecretInput {
                        id: Some("restart-token".to_string()),
                        scope_kind: "workspace".to_string(),
                        scope_id: "workspace".to_string(),
                        name: "RESTART_TOKEN".to_string(),
                        description: "Restart token".to_string(),
                        secret: "restart-secret-value".to_string(),
                    },
                )
                .expect("secret persists encrypted");
        }
        drop(state);
        drop(store);

        let reopened_store = Arc::new(
            StateStore::initialize_at(&state_dir).expect("store should reopen after restart"),
        );
        let restarted = test_app_state(&reopened_store);
        let status = vault_status(State(restarted.clone()))
            .await
            .expect("status should load")
            .0;
        assert!(status.initialized);
        assert!(status.locked);
        assert_eq!(status.state, "locked");

        let listed = list_vault_secrets(
            State(restarted.clone()),
            Query(VaultSecretListQuery {
                scope_kind: None,
                scope_id: None,
            }),
        )
        .await
        .expect("secrets should list")
        .0;
        assert_eq!(listed.secrets.len(), 1);
        assert_eq!(listed.secrets[0].id, "restart-token");
        let serialized = serde_json::to_string(&listed).expect("list serializes");
        assert!(!serialized.contains("restart-secret-value"));

        let raw = restarted
            .store
            .load_vault_secret("restart-token")
            .expect("secret should load");
        assert_ne!(raw.ciphertext, b"restart-secret-value");
        assert!(!String::from_utf8_lossy(&raw.ciphertext).contains("restart-secret-value"));

        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        let unlocked = vault_unlock(
            State(restarted.clone()),
            headers,
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("correct passphrase unlocks after restart")
        .0;
        assert!(!unlocked.locked);
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn vault_endpoints_report_status_lock_and_metadata_only_list() {
        let (state_dir, state) = test_named_app_state("vault-endpoint-status-list");
        let initial = vault_status(State(state.clone()))
            .await
            .expect("status should load")
            .0;
        assert!(!initial.initialized);
        assert!(initial.locked);
        assert_eq!(initial.state, "uninitialized");

        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        let initialized = vault_init(
            State(state.clone()),
            headers.clone(),
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("vault init succeeds")
        .0;
        assert!(initialized.initialized);
        assert!(!initialized.locked);

        let created = create_vault_secret(
            State(state.clone()),
            headers,
            Bytes::from(
                "{\"id\":\"status-token\",\"scope_kind\":\"workspace\",\"scope_id\":\"workspace\",\"name\":\"STATUS_TOKEN\",\"secret\":\"  status-secret-value\\n\"}",
            ),
        )
        .await
        .expect("secret create succeeds")
        .0;
        let created_serialized = serde_json::to_string(&created).expect("summary serializes");
        assert!(!created_serialized.contains("status-secret-value"));
        let raw = state
            .store
            .load_vault_secret("status-token")
            .expect("secret should load");
        let decrypted = {
            let mut vault = state.vault.lock().await;
            vault::decrypt_secret_for_test(&mut vault, &state.store, &raw)
                .expect("secret decrypts while unlocked")
        };
        assert_eq!(decrypted, "  status-secret-value\n");

        let listed = list_vault_secrets(
            State(state.clone()),
            Query(VaultSecretListQuery {
                scope_kind: None,
                scope_id: None,
            }),
        )
        .await
        .expect("secrets list")
        .0;
        let serialized = serde_json::to_string(&listed).expect("list serializes");
        assert!(serialized.contains("STATUS_TOKEN"));
        assert!(!serialized.contains("status-secret-value"));

        let locked = vault_lock(State(state.clone()))
            .await
            .expect("lock succeeds")
            .0;
        assert!(locked.initialized);
        assert!(locked.locked);
        assert_eq!(locked.state, "locked");
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn vault_update_endpoint_requires_safe_origin_unlocked_and_stays_metadata_only() {
        let (state_dir, state) = test_named_app_state("vault-endpoint-update");
        let mut safe_headers = HeaderMap::new();
        safe_headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        let _ = vault_init(
            State(state.clone()),
            safe_headers.clone(),
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("vault init succeeds");
        let _ = create_vault_secret(
            State(state.clone()),
            safe_headers.clone(),
            Bytes::from(r#"{"id":"update-token","scope_kind":"workspace","scope_id":"workspace","name":"UPDATE_TOKEN","secret":"old-secret-value"}"#),
        )
        .await
        .expect("secret create succeeds");
        let before = state
            .store
            .load_vault_secret("update-token")
            .expect("secret should load");

        let mut unsafe_headers = HeaderMap::new();
        unsafe_headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://192.168.1.2:5201"),
        );
        let unsafe_error = update_vault_secret(
            State(state.clone()),
            Path("update-token".to_string()),
            unsafe_headers,
            Bytes::from(r#"{"secret":"unsafe-secret-value"}"#),
        )
        .await
        .expect_err("unsafe origin should fail");
        assert_eq!(unsafe_error.status, StatusCode::FORBIDDEN);

        let blank_scope_error = update_vault_secret(
            State(state.clone()),
            Path("update-token".to_string()),
            safe_headers.clone(),
            Bytes::from(r#"{"scope_id":"   ","secret":"blank-scope-secret-value"}"#),
        )
        .await
        .expect_err("blank scope id should fail");
        assert_eq!(blank_scope_error.status, StatusCode::BAD_REQUEST);

        let _ = vault_lock(State(state.clone()))
            .await
            .expect("lock succeeds");
        let locked_error = update_vault_secret(
            State(state.clone()),
            Path("update-token".to_string()),
            safe_headers.clone(),
            Bytes::from(r#"{"secret":"locked-secret-value"}"#),
        )
        .await
        .expect_err("locked vault should block update");
        assert_eq!(locked_error.status, StatusCode::FORBIDDEN);

        let _ = vault_unlock(
            State(state.clone()),
            safe_headers.clone(),
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("unlock succeeds");
        let updated = update_vault_secret(
            State(state.clone()),
            Path("update-token".to_string()),
            safe_headers,
            Bytes::from(r#"{"description":"Updated token","secret":"new-secret-value"}"#),
        )
        .await
        .expect("update succeeds")
        .0;
        assert_eq!(updated.version, before.version + 1);
        assert_eq!(updated.description, "Updated token");
        let serialized = serde_json::to_string(&updated).expect("summary serializes");
        assert!(!serialized.contains("new-secret-value"));

        let after = state
            .store
            .load_vault_secret("update-token")
            .expect("updated secret should load");
        assert_ne!(after.ciphertext, before.ciphertext);
        assert!(!String::from_utf8_lossy(&after.ciphertext).contains("new-secret-value"));
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn vault_delete_endpoint_requires_safe_origin_and_removes_secret_from_list() {
        let (state_dir, state) = test_named_app_state("vault-endpoint-delete");
        let mut safe_headers = HeaderMap::new();
        safe_headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        let _ = vault_init(
            State(state.clone()),
            safe_headers.clone(),
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("vault init succeeds");
        let _ = create_vault_secret(
            State(state.clone()),
            safe_headers.clone(),
            Bytes::from(r#"{"id":"delete-token","scope_kind":"workspace","scope_id":"workspace","name":"DELETE_TOKEN","secret":"delete-secret-value"}"#),
        )
        .await
        .expect("secret create succeeds");

        let mut unsafe_headers = HeaderMap::new();
        unsafe_headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://192.168.1.2:5201"),
        );
        let unsafe_error = delete_vault_secret(
            State(state.clone()),
            Path("delete-token".to_string()),
            unsafe_headers,
        )
        .await
        .expect_err("unsafe origin should fail");
        assert_eq!(unsafe_error.status, StatusCode::FORBIDDEN);
        assert!(state.store.load_vault_secret("delete-token").is_ok());

        let _ = vault_lock(State(state.clone()))
            .await
            .expect("lock succeeds");
        let locked_error = delete_vault_secret(
            State(state.clone()),
            Path("delete-token".to_string()),
            safe_headers.clone(),
        )
        .await
        .expect_err("locked vault should block delete");
        assert_eq!(locked_error.status, StatusCode::FORBIDDEN);
        assert!(state.store.load_vault_secret("delete-token").is_ok());

        let _ = vault_unlock(
            State(state.clone()),
            safe_headers.clone(),
            Bytes::from(r#"{"passphrase":"correct horse battery staple"}"#),
        )
        .await
        .expect("unlock succeeds");

        let status = delete_vault_secret(
            State(state.clone()),
            Path("delete-token".to_string()),
            safe_headers,
        )
        .await
        .expect("delete succeeds");
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.store.load_vault_secret("delete-token").is_err());

        let listed = list_vault_secrets(
            State(state.clone()),
            Query(VaultSecretListQuery {
                scope_kind: None,
                scope_id: None,
            }),
        )
        .await
        .expect("secrets list")
        .0;
        assert!(listed.secrets.is_empty());
        let serialized = serde_json::to_string(&listed).expect("list serializes");
        assert!(!serialized.contains("delete-secret-value"));
        let _ = fs::remove_dir_all(&state_dir);
    }

    fn test_session(workspace_root: &std::path::Path) -> SessionSummary {
        SessionSummary {
            id: "session-test".to_string(),
            title: "Test".to_string(),
            profile_id: String::new(),
            profile_title: String::new(),
            route_id: String::new(),
            route_title: String::new(),
            project_id: String::new(),
            project_title: String::new(),
            project_path: workspace_root.display().to_string(),
            provider: "openai_compatible".to_string(),
            model: "gpt-5.4-mini".to_string(),
            provider_base_url: "http://127.0.0.1:20128/v1".to_string(),
            provider_api_key: String::new(),
            working_dir: workspace_root.display().to_string(),
            working_dir_kind: "workspace".to_string(),
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
            scope: "workspace".to_string(),
            approval_mode: "ask".to_string(),
            execution_mode: "act".to_string(),
            run_budget_mode: "inherit".to_string(),
            run_budget: RunBudgetSummary::default(),
            project_count: 0,
            projects: Vec::new(),
            state: "active".to_string(),
            provider_session_id: String::new(),
            last_error: String::new(),
            last_message_excerpt: String::new(),
            turn_count: 0,
            created_at: 0,
            updated_at: 0,
        }
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
                None,
            )
            .expect("workspace root should update");
        store
    }

    #[tokio::test]
    async fn discovers_stdio_mcp_tools_and_persists_sync_state() {
        let state_dir = test_state_dir("mcp-discover-success");
        let store = initialize_test_store(&state_dir);
        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let script_path = state_dir.join("fake-mcp.py");
        fs::write(&script_path, r#"
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get('method') == 'initialize' and 'id' in msg:
        sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':msg['id'],'result':{'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'fake','version':'1.0'}}}) + '\n')
        sys.stdout.flush()
    elif msg.get('method') == 'tools/list' and 'id' in msg:
        sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':msg['id'],'result':{'tools':[{'name':'searchDocs','description':'Search docs','inputSchema':{'type':'object','properties':{'query':{'type':'string'}}}}]}}) + '\n')
        sys.stdout.flush()
        break
"#.trim_start()).expect("fake mcp script should write");

        let record = McpServerRecord {
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
            sync_status: "pending".to_string(),
            last_error: String::new(),
            last_synced_at: None,
            created_at: 0,
            updated_at: 0,
        };
        store
            .upsert_mcp_server_record(&record, &[], &[])
            .expect("mcp record should persist");

        let summary = discover_mcp_server_tools(State(state.clone()), Path("mcp.docs".to_string()))
            .await
            .expect("discovery should succeed")
            .0;
        assert_eq!(summary.id, "mcp.docs");
        assert_eq!(summary.tools.len(), 1);
        assert_eq!(summary.tools[0].id, "mcp.docs.searchDocs");

        let tools = store.list_mcp_tools().expect("mcp tools should load");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "searchDocs");

        let records = store
            .list_mcp_server_records()
            .expect("mcp server records should load");
        let record = records
            .into_iter()
            .find(|row| row.id == "mcp.docs")
            .expect("record should exist");
        assert_eq!(record.sync_status, "ready");
        assert!(record.last_error.is_empty());
        assert!(record.last_synced_at.is_some());

        let _ = fs::remove_dir_all(&state_dir);
    }

    async fn start_auth_check_mcp_server(expected_token: &'static str) -> String {
        async fn handler(
            headers: HeaderMap,
            Json(payload): Json<Value>,
        ) -> Result<Json<Value>, StatusCode> {
            let expected = "Bearer phase6-vault-token";
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                != Some(expected)
            {
                return Err(StatusCode::UNAUTHORIZED);
            }
            let id = payload.get("id").cloned().unwrap_or_else(|| json!(null));
            let method = payload.get("method").and_then(Value::as_str).unwrap_or("");
            let result = if method == "tools/list" {
                json!({"tools":[{"name":"lookup","description":"Lookup docs","inputSchema":{"type":"object"}}]})
            } else {
                json!({"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake","version":"1.0"}})
            };
            Ok(Json(json!({"jsonrpc":"2.0","id":id,"result":result})))
        }

        assert_eq!(expected_token, "phase6-vault-token");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener binds");
        let addr = listener.local_addr().expect("listener has addr");
        let app = Router::new().route("/mcp", axum::routing::post(handler));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/mcp")
    }

    fn vault_http_mcp_record(url: String) -> McpServerRecord {
        McpServerRecord {
            id: "mcp.vault-docs".to_string(),
            workspace_id: "workspace".to_string(),
            title: "Vault Docs MCP".to_string(),
            transport: "streamable-http".to_string(),
            command: String::new(),
            args: Vec::new(),
            env_json: json!({}),
            url,
            headers_json: json!({}),
            auth_kind: "vault_bearer".to_string(),
            auth_ref: "vault://workspace/MCP_TOKEN".to_string(),
            enabled: true,
            sync_status: "pending".to_string(),
            last_error: String::new(),
            last_synced_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[tokio::test]
    async fn mcp_vault_bearer_discovery_resolves_daemon_side_and_fails_closed() {
        let (state_dir, state) = test_named_app_state("mcp-vault-bearer");
        let url = start_auth_check_mcp_server("phase6-vault-token").await;
        let record = vault_http_mcp_record(url);
        state
            .store
            .upsert_mcp_server_record(&record, &[], &[])
            .expect("mcp record persists");

        {
            let mut vault = state.vault.lock().await;
            vault
                .initialize(&state.store, "correct horse battery staple")
                .expect("vault initializes");
            let secret = vault
                .create_or_update_secret(
                    &state.store,
                    vault::VaultSecretInput {
                        id: Some("mcp-token".to_string()),
                        scope_kind: "workspace".to_string(),
                        scope_id: "workspace".to_string(),
                        name: "MCP_TOKEN".to_string(),
                        description: "MCP bearer token".to_string(),
                        secret: "phase6-vault-token".to_string(),
                    },
                )
                .expect("secret stores");
            state
                .store
                .upsert_vault_secret_policy(&VaultSecretPolicyRecord {
                    id: "policy-allow".to_string(),
                    secret_id: secret.id,
                    consumer_kind: "mcp".to_string(),
                    consumer_id: record.id.clone(),
                    permission: "read".to_string(),
                    approval_mode: "allow".to_string(),
                    created_at: 0,
                    updated_at: 0,
                })
                .expect("policy stores");
        }

        let summary = discover_mcp_server_tools(State(state.clone()), Path(record.id.clone()))
            .await
            .expect("vault-backed discovery succeeds")
            .0;
        assert_eq!(summary.sync_status, "ready");
        assert_eq!(summary.auth_kind, "vault_bearer");
        assert_eq!(summary.auth_ref, "vault://workspace/MCP_TOKEN");
        let serialized = serde_json::to_string(&summary).expect("summary serializes");
        assert!(!serialized.contains("phase6-vault-token"));

        state.vault.lock().await.lock();
        let locked = discover_mcp_server_tools(State(state.clone()), Path(record.id.clone()))
            .await
            .expect_err("locked vault blocks discovery");
        assert_eq!(locked.status, StatusCode::BAD_REQUEST);
        assert!(locked.message.contains("vault_locked"));
        let stored = state
            .store
            .list_mcp_server_records()
            .expect("records load")
            .into_iter()
            .find(|row| row.id == record.id)
            .expect("record exists");
        assert_eq!(stored.sync_status, "vault_locked");

        let missing = resolve_mcp_vault_bearer_token(
            &state,
            &McpServerRecord {
                auth_ref: "vault://workspace/NOPE".to_string(),
                ..record.clone()
            },
            None,
        )
        .await
        .expect_err("missing secret blocks");
        assert!(missing.to_string().contains("vault_secret_missing"));

        {
            let _ = state
                .vault
                .lock()
                .await
                .unlock(&state.store, "correct horse battery staple")
                .expect("vault unlocks");
        }
        state
            .store
            .upsert_vault_secret_policy(&VaultSecretPolicyRecord {
                id: "policy-deny".to_string(),
                secret_id: "mcp-token".to_string(),
                consumer_kind: "mcp".to_string(),
                consumer_id: "mcp.denied".to_string(),
                permission: "read".to_string(),
                approval_mode: "deny".to_string(),
                created_at: 0,
                updated_at: 0,
            })
            .expect("deny policy stores");
        let denied = resolve_mcp_vault_bearer_token(
            &state,
            &McpServerRecord {
                id: "mcp.denied".to_string(),
                ..record.clone()
            },
            None,
        )
        .await
        .expect_err("policy denied blocks");
        assert!(denied.to_string().contains("vault_policy_denied"));

        let audit = state.store.list_audit_events(50).expect("audit lists");
        let audit_serialized = serde_json::to_string(&audit).expect("audit serializes");
        assert!(!audit_serialized.contains("phase6-vault-token"));
        let records = state.store.list_mcp_server_records().expect("records load");
        let records_serialized = serde_json::to_string(&records).expect("records serialize");
        assert!(!records_serialized.contains("phase6-vault-token"));

        let mut env_record = record.clone();
        env_record.id = "mcp.env-legacy".to_string();
        env_record.auth_kind = "bearer_env".to_string();
        env_record.auth_ref = "NUCLEUS_TEST_PHASE6_MCP_TOKEN".to_string();
        state
            .store
            .upsert_mcp_server_record(&env_record, &[], &[])
            .expect("legacy env mcp record persists");
        unsafe {
            env::set_var("NUCLEUS_TEST_PHASE6_MCP_TOKEN", "phase6-vault-token");
        }
        let env_discovery =
            discover_mcp_server_tools(State(state.clone()), Path(env_record.id.clone()))
                .await
                .expect_err("env bearer discovery fails closed even when env var exists");
        assert_eq!(env_discovery.status, StatusCode::BAD_REQUEST);
        assert!(env_discovery.message.contains("auth_migration_required"));
        let stored_env_record = state
            .store
            .list_mcp_server_records()
            .expect("records load")
            .into_iter()
            .find(|row| row.id == env_record.id)
            .expect("legacy env record exists");
        assert_eq!(stored_env_record.sync_status, "auth_migration_required");

        let env_result = mcp_http_request(
            &state,
            &env_record,
            json!({"jsonrpc":"2.0","id":9,"method":"tools/list","params":{}}),
            None,
        )
        .await
        .expect_err("env bearer request fails closed even when env var exists");
        assert!(env_result.to_string().contains("auth_migration_required"));
        unsafe {
            env::remove_var("NUCLEUS_TEST_PHASE6_MCP_TOKEN");
        }
        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn mcp_api_rejects_env_bearer_and_normalizes_simple_vault_refs() {
        let (state_dir, state) = test_named_app_state("mcp-auth-validation");

        let rejected = upsert_mcp_server(
            State(state.clone()),
            Bytes::from(
                r#"{"id":"mcp.env","title":"Env MCP","enabled":true,"transport":"streamable-http","url":"https://example.test/mcp","auth_kind":"env_bearer","auth_ref":"MCP_TOKEN"}"#,
            ),
        )
        .await
        .expect_err("env bearer writes are rejected");
        assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
        assert!(rejected.message.contains("auth_migration_required"));

        let saved = upsert_mcp_server(
            State(state.clone()),
            Bytes::from(
                r#"{"id":"mcp.vault","title":"Vault MCP","enabled":true,"transport":"streamable-http","url":"https://example.test/mcp","auth_kind":"vault_bearer","auth_ref":"MCP_TOKEN"}"#,
            ),
        )
        .await
        .expect("simple Vault auth ref writes")
        .0;
        assert_eq!(saved.auth_kind, "vault_bearer");
        assert_eq!(saved.auth_ref, "vault://workspace/MCP_TOKEN");

        let malformed_ref = upsert_mcp_server(
            State(state.clone()),
            Bytes::from(
                r#"{"id":"mcp.bad-vault-ref","title":"Bad Vault MCP","enabled":true,"transport":"streamable-http","url":"https://example.test/mcp","auth_kind":"vault_bearer","auth_ref":"vault://project//MCP_TOKEN"}"#,
            ),
        )
        .await
        .expect_err("malformed Vault refs are rejected");
        assert_eq!(malformed_ref.status, StatusCode::BAD_REQUEST);
        assert!(malformed_ref.message.contains("vault secret name"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn project_vaults_are_isolated_and_mcp_project_context_is_enforced() {
        let (state_dir, state) = test_named_app_state("phase7-project-vaults");
        let workspace = state.store.workspace().expect("workspace loads");
        let alpha_dir = PathBuf::from(&workspace.root_path).join("alpha");
        let beta_dir = PathBuf::from(&workspace.root_path).join("beta");
        fs::create_dir_all(&alpha_dir).expect("alpha project dir creates");
        fs::create_dir_all(&beta_dir).expect("beta project dir creates");
        fs::write(alpha_dir.join("README.md"), "alpha").expect("alpha marker writes");
        fs::write(beta_dir.join("README.md"), "beta").expect("beta marker writes");
        let workspace = state.store.sync_projects().expect("projects sync");
        assert!(
            workspace
                .projects
                .iter()
                .any(|project| project.id == "alpha")
        );
        assert!(
            workspace
                .projects
                .iter()
                .any(|project| project.id == "beta")
        );

        let url = start_auth_check_mcp_server("phase6-vault-token").await;
        let mut project_record = vault_http_mcp_record(url.clone());
        project_record.id = "mcp.project".to_string();
        project_record.auth_ref = "vault://project/alpha/MCP_TOKEN".to_string();
        state
            .store
            .upsert_mcp_server_record(&project_record, &[], &[])
            .expect("project mcp record persists");
        let mut workspace_record = vault_http_mcp_record(url);
        workspace_record.id = "mcp.workspace".to_string();
        workspace_record.auth_ref = "vault://workspace/MCP_TOKEN".to_string();
        state
            .store
            .upsert_mcp_server_record(&workspace_record, &[], &[])
            .expect("workspace mcp record persists");

        {
            let mut vault = state.vault.lock().await;
            vault
                .initialize(&state.store, "correct horse battery staple")
                .expect("vault initializes");
            let workspace_secret = vault
                .create_or_update_secret(
                    &state.store,
                    vault::VaultSecretInput {
                        id: Some("workspace-token".to_string()),
                        scope_kind: "workspace".to_string(),
                        scope_id: "workspace".to_string(),
                        name: "MCP_TOKEN".to_string(),
                        description: String::new(),
                        secret: "phase6-vault-token".to_string(),
                    },
                )
                .expect("workspace secret stores");
            let alpha_secret = vault
                .create_or_update_secret(
                    &state.store,
                    vault::VaultSecretInput {
                        id: Some("alpha-token".to_string()),
                        scope_kind: "project".to_string(),
                        scope_id: "alpha".to_string(),
                        name: "MCP_TOKEN".to_string(),
                        description: String::new(),
                        secret: "phase6-vault-token".to_string(),
                    },
                )
                .expect("alpha secret stores");
            let beta_secret = vault
                .create_or_update_secret(
                    &state.store,
                    vault::VaultSecretInput {
                        id: Some("beta-token".to_string()),
                        scope_kind: "project".to_string(),
                        scope_id: "beta".to_string(),
                        name: "MCP_TOKEN".to_string(),
                        description: String::new(),
                        secret: "wrong-project-token".to_string(),
                    },
                )
                .expect("beta secret stores");
            for (id, consumer_id) in [
                (workspace_secret.id.as_str(), workspace_record.id.as_str()),
                (alpha_secret.id.as_str(), project_record.id.as_str()),
            ] {
                state
                    .store
                    .upsert_vault_secret_policy(&VaultSecretPolicyRecord {
                        id: format!("policy-{id}"),
                        secret_id: id.to_string(),
                        consumer_kind: "mcp".to_string(),
                        consumer_id: consumer_id.to_string(),
                        permission: "read".to_string(),
                        approval_mode: "allow".to_string(),
                        created_at: 0,
                        updated_at: 0,
                    })
                    .expect("allow policy stores");
            }
            state
                .store
                .upsert_vault_secret_policy(&VaultSecretPolicyRecord {
                    id: "policy-beta-denied".to_string(),
                    secret_id: beta_secret.id,
                    consumer_kind: "mcp".to_string(),
                    consumer_id: project_record.id.clone(),
                    permission: "read".to_string(),
                    approval_mode: "deny".to_string(),
                    created_at: 0,
                    updated_at: 0,
                })
                .expect("deny policy stores");
        }

        let workspace_secrets = state
            .store
            .list_vault_secrets(Some("workspace"), Some("workspace"))
            .expect("workspace secrets list");
        let alpha_secrets = state
            .store
            .list_vault_secrets(Some("project"), Some("alpha"))
            .expect("alpha secrets list");
        let beta_secrets = state
            .store
            .list_vault_secrets(Some("project"), Some("beta"))
            .expect("beta secrets list");
        assert_eq!(workspace_secrets.len(), 1);
        assert_eq!(alpha_secrets.len(), 1);
        assert_eq!(beta_secrets.len(), 1);
        assert_ne!(
            workspace_secrets[0].scope_key_id,
            alpha_secrets[0].scope_key_id
        );
        assert_ne!(alpha_secrets[0].scope_key_id, beta_secrets[0].scope_key_id);
        assert!(validate_vault_scope(&state, "project", "alpha").is_ok());
        assert!(validate_vault_scope(&state, "project", "missing-project").is_err());
        assert!(validate_vault_scope(&state, "workspace", "alpha").is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:5201"),
        );
        let alpha_scope = VaultSecretPolicyScopeQuery {
            scope_kind: Some("project".to_string()),
            scope_id: Some("alpha".to_string()),
        };
        let beta_scope = VaultSecretPolicyScopeQuery {
            scope_kind: Some("project".to_string()),
            scope_id: Some("beta".to_string()),
        };
        let missing_policy_context = list_vault_secret_policies(
            State(state.clone()),
            Path("alpha-token".to_string()),
            Query(VaultSecretPolicyScopeQuery {
                scope_kind: None,
                scope_id: None,
            }),
        )
        .await
        .expect_err("missing project policy context fails closed");
        assert_eq!(missing_policy_context.status, StatusCode::FORBIDDEN);
        assert!(
            missing_policy_context
                .message
                .contains("vault_project_context_missing")
        );
        let wrong_policy_context = list_vault_secret_policies(
            State(state.clone()),
            Path("alpha-token".to_string()),
            Query(beta_scope),
        )
        .await
        .expect_err("wrong project policy context fails closed");
        assert_eq!(wrong_policy_context.status, StatusCode::FORBIDDEN);
        assert!(
            wrong_policy_context
                .message
                .contains("vault_project_context_mismatch")
        );
        let endpoint_policy = upsert_vault_secret_policy(
            State(state.clone()),
            Path("alpha-token".to_string()),
            Query(VaultSecretPolicyScopeQuery {
                scope_kind: alpha_scope.scope_kind.clone(),
                scope_id: alpha_scope.scope_id.clone(),
            }),
            headers.clone(),
            Bytes::from(r#"{"id":"policy-alpha-endpoint","consumer_kind":"mcp","consumer_id":"mcp.project.extra","permission":"read","approval_mode":"allow"}"#),
        )
        .await
        .expect("matching project policy context allows upsert")
        .0;
        assert_eq!(endpoint_policy.consumer_id, "mcp.project.extra");
        let listed_project_policies = list_vault_secret_policies(
            State(state.clone()),
            Path("alpha-token".to_string()),
            Query(VaultSecretPolicyScopeQuery {
                scope_kind: alpha_scope.scope_kind.clone(),
                scope_id: alpha_scope.scope_id.clone(),
            }),
        )
        .await
        .expect("matching project policy context allows list")
        .0;
        assert!(
            listed_project_policies
                .policies
                .iter()
                .any(|policy| policy.id == endpoint_policy.id)
        );
        let wrong_delete = delete_vault_secret_policy(
            State(state.clone()),
            Path(("alpha-token".to_string(), endpoint_policy.id.clone())),
            Query(VaultSecretPolicyScopeQuery {
                scope_kind: Some("project".to_string()),
                scope_id: Some("beta".to_string()),
            }),
            headers.clone(),
        )
        .await
        .expect_err("wrong project policy context blocks delete");
        assert_eq!(wrong_delete.status, StatusCode::FORBIDDEN);
        let deleted = delete_vault_secret_policy(
            State(state.clone()),
            Path(("alpha-token".to_string(), endpoint_policy.id.clone())),
            Query(alpha_scope),
            headers.clone(),
        )
        .await
        .expect("matching project policy context allows delete");
        assert_eq!(deleted, StatusCode::NO_CONTENT);

        let project_token = resolve_mcp_vault_bearer_token(&state, &project_record, Some("alpha"))
            .await
            .expect("matching project context resolves");
        assert_eq!(project_token, "phase6-vault-token");
        let missing_context = resolve_mcp_vault_bearer_token(&state, &project_record, None)
            .await
            .expect_err("missing project context fails closed");
        assert!(
            missing_context
                .to_string()
                .contains("vault_project_context_missing")
        );
        let wrong_context = resolve_mcp_vault_bearer_token(&state, &project_record, Some("beta"))
            .await
            .expect_err("wrong project context fails closed");
        assert!(
            wrong_context
                .to_string()
                .contains("vault_project_context_mismatch")
        );

        let workspace_result = mcp_http_request(
            &state,
            &workspace_record,
            json!({"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}),
            None,
        )
        .await
        .expect("workspace vault reference still works");
        assert_eq!(workspace_result["tools"][0]["name"], "lookup");

        state.vault.lock().await.lock();
        let locked = resolve_mcp_vault_bearer_token(&state, &project_record, Some("alpha"))
            .await
            .expect_err("locked project vault fails closed");
        assert!(locked.to_string().contains("vault_locked"));

        let audit = state.store.list_audit_events(50).expect("audit lists");
        let audit_serialized = serde_json::to_string(&audit).expect("audit serializes");
        assert!(!audit_serialized.contains("phase6-vault-token"));
        let records = state.store.list_mcp_server_records().expect("records load");
        let records_serialized = serde_json::to_string(&records).expect("records serialize");
        assert!(!records_serialized.contains("phase6-vault-token"));
        let summaries = serde_json::to_string(
            &workspace_secrets
                .iter()
                .cloned()
                .map(vault_secret_summary)
                .collect::<Vec<_>>(),
        )
        .expect("secrets serialize");
        assert!(!summaries.contains("phase6-vault-token"));

        let _ = fs::remove_dir_all(&state_dir);
    }

    #[tokio::test]
    async fn records_stdio_mcp_discovery_failures() {
        let state_dir = test_state_dir("mcp-discover-failure");
        let store = initialize_test_store(&state_dir);
        let (events, _) = broadcast::channel(4);
        let state = AppState {
            version: "test".to_string(),
            store: store.clone(),
            host: Arc::new(HostEngine::new()),
            runtimes: Arc::new(RuntimeManager::default()),
            updates: Arc::new(UpdateManager::new(test_instance_runtime(), store.clone())),
            vault: Arc::new(tokio::sync::Mutex::new(vault::VaultRuntime::default())),
            agent: Arc::new(agent::AgentRuntime::default()),
            browser: Arc::new(browser::BrowserRuntime::default()),
            web_dist_dir: None,
            tailscale_dns_name: None,
            events,
        };

        let record = McpServerRecord {
            id: "mcp.broken".to_string(),
            workspace_id: "workspace".to_string(),
            title: "Broken MCP".to_string(),
            transport: "stdio".to_string(),
            command: String::new(),
            args: Vec::new(),
            env_json: json!({}),
            url: String::new(),
            headers_json: json!({}),
            auth_kind: "none".to_string(),
            auth_ref: String::new(),
            enabled: true,
            sync_status: "pending".to_string(),
            last_error: String::new(),
            last_synced_at: None,
            created_at: 0,
            updated_at: 0,
        };
        store
            .upsert_mcp_server_record(&record, &[], &[])
            .expect("mcp record should persist");

        let error = discover_mcp_server_tools(State(state.clone()), Path("mcp.broken".to_string()))
            .await
            .expect_err("discovery should fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        let records = store
            .list_mcp_server_records()
            .expect("mcp server records should load");
        let record = records
            .into_iter()
            .find(|row| row.id == "mcp.broken")
            .expect("record should exist");
        assert_eq!(record.sync_status, "error");
        assert!(record.last_error.contains("command is required"));

        let _ = fs::remove_dir_all(&state_dir);
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
