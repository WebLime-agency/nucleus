use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use nucleus_protocol::{
    ApprovalRequestSummary, ApprovalResolutionRequest, CommandSessionSummary, CreateSessionRequest,
    HealthResponse, JobDetail, JobEvent, JobSummary, SessionDetail, SessionPromptRequest,
    ToolCallSummary, WorkspaceSummary,
};

const READ_ONLY_PROBE_COMMAND: &str = "printf NUCLEUS_COMMAND_RUN_PROBE";
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::Value;
use tokio::time::sleep;
use url::Url;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:5202";
const DEFAULT_AUTH_TOKEN_PATH: &str = "/home/eba/.nucleus-dev-projects/local-auth-token";
const DEFAULT_PROJECT: &str = "nucleus";
const DEFAULT_TIMEOUT_SECS: u64 = 900;
const DEFAULT_OUTPUT_PATH: &str = "tools/dogfood-harness/reports/latest.json";
const MAX_HTTP_REQUEST_TIMEOUT_SECS: u64 = 30;
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const TERMINAL_STATES: &[&str] = &["completed", "blocked", "failed", "canceled"];
const CONTEXT_PRESSURE_BLOCKER_MARKER: &str = "NUCLEUS_CONTEXT_PRESSURE_BLOCKER";
const KEY_EVENT_TERMS: &[&str] = &[
    "delegat",
    "gate",
    "reuse",
    "publish",
    "publication",
    "approval",
    "child",
    "complete",
];

#[derive(Debug, Parser)]
#[command(about = "Run the Nucleus dogfood/regression ladder against a live install")]
struct Args {
    #[arg(long, env = "NUCLEUS_DOGFOOD_BASE_URL", default_value = DEFAULT_BASE_URL)]
    base_url: String,
    #[arg(long, env = "NUCLEUS_DOGFOOD_AUTH_TOKEN")]
    auth_token: Option<String>,
    #[arg(long, env = "NUCLEUS_DOGFOOD_AUTH_TOKEN_PATH", default_value = DEFAULT_AUTH_TOKEN_PATH)]
    auth_token_path: PathBuf,
    #[arg(long, env = "NUCLEUS_DOGFOOD_PROJECT", default_value = DEFAULT_PROJECT)]
    project: String,
    #[arg(long, env = "NUCLEUS_DOGFOOD_RUNGS", default_value = "all")]
    rungs: String,
    #[arg(long, env = "NUCLEUS_DOGFOOD_TIMEOUT_SECS", default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout_secs: u64,
    #[arg(long, env = "NUCLEUS_DOGFOOD_OUTPUT", default_value = DEFAULT_OUTPUT_PATH)]
    output: PathBuf,
    #[arg(long, env = "NUCLEUS_DOGFOOD_ALLOW_FAILURES", default_value_t = false)]
    allow_failures: bool,
}

#[derive(Debug, Clone)]
struct Rung {
    name: &'static str,
    prompt: &'static str,
    max_main_children: usize,
    acceptance: Acceptance,
}

#[derive(Debug, Clone, Copy)]
enum Acceptance {
    ReadOnlyProbe,
    EditAndTest,
    Feature161,
    Debug,
}

#[derive(Debug, Serialize)]
struct HarnessReport {
    install: InstallReport,
    rungs: Vec<RungReport>,
    overall: OverallReport,
}

#[derive(Debug, Serialize)]
struct InstallReport {
    url: String,
    version: String,
    routes: RoutesReport,
}

#[derive(Debug, Serialize)]
struct RoutesReport {
    default_profile_id: String,
    main_target: String,
    utility_target: String,
    profiles: Vec<ProfileRouteReport>,
}

#[derive(Debug, Serialize)]
struct ProfileRouteReport {
    id: String,
    title: String,
    is_default: bool,
    main: ModelRouteReport,
    utility: ModelRouteReport,
}

#[derive(Debug, Serialize)]
struct ModelRouteReport {
    adapter: String,
    model: String,
    base_url: String,
}

#[derive(Debug, Serialize)]
struct OverallReport {
    passed: usize,
    failed: usize,
    total: usize,
}

#[derive(Debug, Serialize)]
struct RungReport {
    name: String,
    session_id: String,
    root_job_id: String,
    status: String,
    reasons: Vec<String>,
    phase1: PhaseReport,
    phase2: PhaseReport,
    root_final: RootFinalReport,
    read_only_exact_probe_exit_0: Option<bool>,
    root_worker: Option<WorkerReport>,
    children: Vec<ChildReport>,
    counts: CountReport,
    approvals: ApprovalReport,
    cleanup: CleanupReport,
    key_events: Vec<EventReport>,
}

#[derive(Debug, Clone, Serialize)]
struct PhaseReport {
    passed: bool,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RootFinalReport {
    state: String,
    completion_status: String,
    result_summary: String,
    mutation_receipt_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
struct WorkerReport {
    lane: String,
    model: String,
    tool_calls: usize,
}

#[derive(Debug, Serialize)]
struct ChildReport {
    id: String,
    task_class: Option<String>,
    executor_lane: String,
    executor_model: String,
    state: String,
    completion_status: String,
    completion_blockers: Vec<String>,
    tool_calls: Vec<ToolCallReport>,
    tool_failures: Vec<ToolFailureReport>,
}

#[derive(Debug, Serialize)]
struct ToolCallReport {
    tool_id: String,
    args_summary: String,
    exit_code: Option<i64>,
    status: String,
}

#[derive(Debug, Serialize)]
struct ToolFailureReport {
    tool: String,
    status: String,
    error_class: String,
    error_detail: String,
}

#[derive(Debug, Serialize)]
struct CountReport {
    children: usize,
    main_children: usize,
    utility_children: usize,
    fanout_detected: bool,
}

#[derive(Debug, Default, Serialize)]
struct ApprovalReport {
    approved: Vec<ApprovalDecisionReport>,
    denied: Vec<ApprovalDecisionReport>,
}

#[derive(Debug, Default, Clone, Serialize)]
struct CleanupReport {
    root_cancel_attempted: bool,
    session_deleted: bool,
    evidence_preserved: bool,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ApprovalDecisionReport {
    id: String,
    job_id: String,
    tool: String,
    target_paths: Vec<String>,
    reason: String,
}

#[derive(Debug, Serialize)]
struct EventReport {
    job_id: String,
    event_type: String,
    status: String,
    summary: String,
}

#[derive(Debug)]
struct RungOutcome {
    report: RungReport,
}

#[derive(Debug)]
struct ApprovalVerdict {
    approve: bool,
    reason: String,
    target_paths: Vec<String>,
}

struct HarnessClient {
    base_url: Url,
    client: reqwest::Client,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = read_auth_token(&args)?;
    let client = HarnessClient::new(&args.base_url, &token, args.timeout_secs)?;
    let rungs = select_rungs(&args.rungs)?;

    let health: HealthResponse = client.get("/api/health").await?;
    if health.status != "ok" {
        bail!("target health check returned status {}", health.status);
    }
    let workspace: WorkspaceSummary = client.get("/api/workspace").await?;
    let project_id = resolve_project_id(&workspace, &args.project)?;

    let mut reports = Vec::new();
    for rung in rungs {
        println!("running {}...", rung.name);
        match run_rung(&client, &project_id, &rung, args.timeout_secs).await {
            Ok(outcome) => reports.push(outcome.report),
            Err(error) => reports.push(RungReport {
                name: rung.name.to_string(),
                session_id: String::new(),
                root_job_id: String::new(),
                status: "FAIL".to_string(),
                reasons: vec![error.to_string()],
                phase1: failed_phase("rung did not start"),
                phase2: failed_phase("rung did not start"),
                root_final: empty_root_final(),
                read_only_exact_probe_exit_0: read_only_probe_field(&rung, false),
                root_worker: None,
                children: Vec::new(),
                counts: CountReport {
                    children: 0,
                    main_children: 0,
                    utility_children: 0,
                    fanout_detected: false,
                },
                approvals: ApprovalReport::default(),
                cleanup: CleanupReport::default(),
                key_events: Vec::new(),
            }),
        }
    }

    let passed = reports.iter().filter(|rung| rung.status == "PASS").count();
    let failed = reports.len().saturating_sub(passed);
    let report = HarnessReport {
        install: InstallReport {
            url: args.base_url,
            version: health.version,
            routes: routes_report(&workspace),
        },
        overall: OverallReport {
            passed,
            failed,
            total: reports.len(),
        },
        rungs: reports,
    };

    write_report(&args.output, &report)?;
    print_summary(&report, &args.output);
    if failed > 0 && !args.allow_failures {
        bail!("dogfood harness reported {failed} failing rung(s)");
    }
    Ok(())
}

impl HarnessClient {
    fn new(base_url: &str, token: &str, timeout_secs: u64) -> Result<Self> {
        let base_url = Url::parse(base_url).context("invalid base URL")?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .context("auth token is not a valid HTTP header value")?,
        );
        let request_timeout = http_request_timeout(timeout_secs);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(request_timeout)
            .connect_timeout(request_timeout.min(Duration::from_secs(10)))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { base_url, client })
    }

    async fn get<T>(&self, path: &str) -> Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        self.request(reqwest::Method::GET, path, Option::<&Value>::None)
            .await
    }

    async fn post<T, B>(&self, path: &str, body: Option<&B>) -> Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        self.request(reqwest::Method::POST, path, body).await
    }

    async fn delete_empty(&self, path: &str) -> Result<()> {
        let url = self.url(path)?;
        let response = self
            .client
            .delete(url)
            .send()
            .await
            .context("request failed")?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(anyhow!("DELETE {path} failed with {status}: {text}"))
    }

    async fn request<T, B>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        let url = self.url(path)?;
        let mut request = self.client.request(method.clone(), url);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.context("request failed")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read response body")?;
        if !status.is_success() {
            return Err(anyhow!("{method} {path} failed with {status}: {text}"));
        }
        serde_json::from_str(&text).with_context(|| format!("invalid JSON from {path}: {text}"))
    }

    fn url(&self, path: &str) -> Result<Url> {
        let path = path.strip_prefix('/').unwrap_or(path);
        self.base_url
            .join(path)
            .with_context(|| format!("invalid API path {path}"))
    }
}

fn http_request_timeout(timeout_secs: u64) -> Duration {
    Duration::from_secs(timeout_secs.clamp(1, MAX_HTTP_REQUEST_TIMEOUT_SECS))
}

fn read_auth_token(args: &Args) -> Result<String> {
    if let Some(token) = args
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        return Ok(token.to_string());
    }
    if let Ok(token) = env::var("NUCLEUS_AUTH_TOKEN") {
        let token = token.trim();
        if token.is_empty() {
            bail!("NUCLEUS_AUTH_TOKEN is set but empty");
        }
        return Ok(token.to_string());
    }
    let token = fs::read_to_string(&args.auth_token_path).with_context(|| {
        format!(
            "failed to read auth token from {}",
            args.auth_token_path.display()
        )
    })?;
    let token = token.trim();
    if token.is_empty() {
        bail!("auth token file is empty");
    }
    Ok(token.to_string())
}

fn ladder() -> Vec<Rung> {
    vec![
        Rung {
            name: "read_only",
            max_main_children: 2,
            acceptance: Acceptance::ReadOnlyProbe,
            prompt: r#"Dogfood ladder rung: read_only.

Spawn exactly one main child with task_class delegated_subtask. Child task text:

Run exactly this one command in your worktree and report only raw output:

{"command":"printf","args":["NUCLEUS_COMMAND_RUN_PROBE"],"cwd":".","timeout_secs":20}

Root waits for that child, then answers with child job id, lane/model if known, exit code, and stdout."#,
        },
        Rung {
            name: "edit_and_test",
            max_main_children: 1,
            acceptance: Acceptance::EditAndTest,
            prompt: r#"Dogfood ladder rung: edit_and_test.

Use the Nucleus delegation path. The root worker must stay utility/orchestration-only and should not edit or run validation itself. Delegate one main child to make a tiny, well-scoped code change in its isolated worktree: add a small pure helper in crates/core/src/lib.rs and a focused unit test for it in the same crate. The helper should be Nucleus-themed and low-risk, for example formatting a short activity/status phrase. The child must use daemon file tools for edits and run a focused cargo test for the touched crate.

Do not publish. Do not push. Do not create branches manually. Do not stage changes. Do not commit. Do not open pull requests. Join the child result and report the changed file(s), test command, and validation result."#,
        },
        Rung {
            name: "feature_161",
            max_main_children: 1,
            acceptance: Acceptance::Feature161,
            prompt: r#"Dogfood ladder rung: feature_161.

Implement issue #161: subtle Nucleus-themed activity messages with rate-limited rotation. Use the Nucleus delegation path. The root worker must stay utility/orchestration-only. Delegate one main child to read issue #161 and the web client, add a small feature with a focused test, and validate it. If the child hits a daemon gate or missing contract, recover deterministically if possible; otherwise surface a precise blocker with evidence.

Do not publish. Do not push. Do not create branches manually. Do not stage changes. Do not commit. Do not open pull requests. Join the child result into one concise final answer with implementation/validation evidence or the precise blocker."#,
        },
        Rung {
            name: "debug",
            max_main_children: 2,
            acceptance: Acceptance::Debug,
            prompt: r#"Dogfood ladder rung: debug.

Use the Nucleus delegation path. The root worker must stay utility/orchestration-only. Delegate a bounded main child to diagnose and fix this small issue in its isolated worktree: find a focused unit test or helper in the Rust workspace that can be improved to reject empty human-facing status text, make the smallest reasonable fix, and run a focused test. If no appropriate fix is safe, return a precise blocker with the files inspected.

Do not publish. Do not push. Do not create branches manually. Do not stage changes. Do not commit. Do not open pull requests. Join the child result and report the diagnosis, changed file(s), and validation result or precise blocker."#,
        },
    ]
}

fn select_rungs(spec: &str) -> Result<Vec<Rung>> {
    let all = ladder();
    if spec.trim().eq_ignore_ascii_case("all") {
        return Ok(all);
    }
    let requested = spec
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if requested.is_empty() {
        bail!("--rungs must be 'all' or a comma-separated rung list");
    }
    let mut selected = Vec::new();
    for name in requested {
        let rung = all
            .iter()
            .find(|rung| rung.name == name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown rung '{name}'"))?;
        selected.push(rung);
    }
    Ok(selected)
}

fn resolve_project_id(workspace: &WorkspaceSummary, requested: &str) -> Result<String> {
    workspace
        .projects
        .iter()
        .find(|project| {
            project.id == requested
                || project.slug == requested
                || project.title.eq_ignore_ascii_case(requested)
        })
        .map(|project| project.id.clone())
        .ok_or_else(|| anyhow!("project '{requested}' was not found in /api/workspace"))
}

async fn run_rung(
    client: &HarnessClient,
    project_id: &str,
    rung: &Rung,
    timeout_secs: u64,
) -> Result<RungOutcome> {
    let session = create_rung_session(client, project_id, rung).await?;
    let session_id = session.session.id.clone();
    let mut approval_report = ApprovalReport::default();
    let mut root_job_id = String::new();
    let result = run_rung_inner(
        client,
        rung,
        timeout_secs,
        &session_id,
        &mut root_job_id,
        &mut approval_report,
    )
    .await;

    let mut report = match result {
        Ok(report) => report,
        Err(error) => failed_rung_report(rung, &session_id, root_job_id.as_str(), error),
    };
    let cleanup_mode = cleanup_mode_for_status(&report.status);
    let cleanup_report = cleanup(
        client,
        &session_id,
        root_job_id.as_str(),
        &mut approval_report,
        cleanup_mode,
    )
    .await;

    report.approvals.approved.extend(approval_report.approved);
    report.approvals.denied.extend(approval_report.denied);
    if !cleanup_report.reasons.is_empty() {
        report.reasons.extend(
            cleanup_report
                .reasons
                .iter()
                .map(|reason| format!("cleanup: {reason}")),
        );
        report.status = "FAIL".to_string();
    }
    report.cleanup = cleanup_report;
    Ok(RungOutcome { report })
}

fn failed_rung_report(
    rung: &Rung,
    session_id: &str,
    root_job_id: &str,
    error: anyhow::Error,
) -> RungReport {
    RungReport {
        name: rung.name.to_string(),
        session_id: session_id.to_string(),
        root_job_id: root_job_id.to_string(),
        status: "FAIL".to_string(),
        reasons: vec![error.to_string()],
        phase1: failed_phase("rung failed before evaluation"),
        phase2: failed_phase("rung failed before evaluation"),
        root_final: empty_root_final(),
        read_only_exact_probe_exit_0: read_only_probe_field(rung, false),
        root_worker: None,
        children: Vec::new(),
        counts: CountReport {
            children: 0,
            main_children: 0,
            utility_children: 0,
            fanout_detected: false,
        },
        approvals: ApprovalReport::default(),
        cleanup: CleanupReport::default(),
        key_events: Vec::new(),
    }
}

fn failed_phase(reason: &str) -> PhaseReport {
    PhaseReport {
        passed: false,
        reasons: vec![reason.to_string()],
    }
}

fn empty_root_final() -> RootFinalReport {
    RootFinalReport {
        state: String::new(),
        completion_status: String::new(),
        result_summary: String::new(),
        mutation_receipt_ids: Vec::new(),
    }
}

async fn create_rung_session(
    client: &HarnessClient,
    project_id: &str,
    rung: &Rung,
) -> Result<SessionDetail> {
    let request = CreateSessionRequest {
        profile_id: None,
        route_id: None,
        provider: None,
        title: Some(format!("Dogfood harness: {}", rung.name)),
        model: None,
        project_id: Some(project_id.to_string()),
        primary_project_id: Some(project_id.to_string()),
        project_ids: Some(vec![project_id.to_string()]),
        approval_mode: Some("ask".to_string()),
        execution_mode: Some("act".to_string()),
        run_budget_mode: Some("inherit".to_string()),
        attachment_mode: Some("new_worktree".to_string()),
        workspace_mode: None,
        branch_name: None,
    };
    client.post("/api/sessions", Some(&request)).await
}

async fn run_rung_inner(
    client: &HarnessClient,
    rung: &Rung,
    timeout_secs: u64,
    session_id: &str,
    root_job_id: &mut String,
    approvals: &mut ApprovalReport,
) -> Result<RungReport> {
    let prompt = SessionPromptRequest {
        prompt: rung.prompt.to_string(),
        images: Vec::new(),
        task_class: Some("local_project".to_string()),
        role: "main".to_string(),
    };
    let _: SessionDetail = client
        .post(&format!("/api/sessions/{session_id}/prompt"), Some(&prompt))
        .await?;
    *root_job_id = wait_for_root_job_id(client, session_id).await?;

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut latest = client
        .get::<JobDetail>(&format!("/api/jobs/{root_job_id}"))
        .await?;
    while Instant::now() < deadline {
        handle_pending_approvals(client, root_job_id, &latest, approvals).await?;
        latest = client
            .get::<JobDetail>(&format!("/api/jobs/{root_job_id}"))
            .await?;
        if TERMINAL_STATES.contains(&latest.job.state.as_str()) {
            break;
        }
        sleep(POLL_INTERVAL).await;
    }

    if !TERMINAL_STATES.contains(&latest.job.state.as_str()) {
        latest = client
            .get::<JobDetail>(&format!("/api/jobs/{root_job_id}"))
            .await?;
    }

    let child_details = fetch_child_details(client, &latest).await;
    Ok(build_rung_report(
        rung,
        session_id,
        root_job_id,
        latest,
        child_details,
        ApprovalReport::default(),
    ))
}

async fn wait_for_root_job_id(client: &HarnessClient, session_id: &str) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let jobs: Vec<JobSummary> = client
            .get(&format!("/api/sessions/{session_id}/jobs"))
            .await?;
        if let Some(job) = jobs
            .iter()
            .filter(|job| job.parent_job_id.is_none())
            .max_by_key(|job| job.created_at)
        {
            return Ok(job.id.clone());
        }
        sleep(Duration::from_millis(500)).await;
    }
    bail!("timed out waiting for root job id");
}

async fn fetch_child_details(client: &HarnessClient, root: &JobDetail) -> Vec<JobDetail> {
    let mut details = Vec::new();
    for child in &root.child_jobs {
        if let Ok(detail) = client
            .get::<JobDetail>(&format!("/api/jobs/{}", child.id))
            .await
        {
            details.push(detail);
        }
    }
    details
}

async fn handle_pending_approvals(
    client: &HarnessClient,
    root_job_id: &str,
    root: &JobDetail,
    report: &mut ApprovalReport,
) -> Result<()> {
    let pending: Vec<ApprovalRequestSummary> = client.get("/api/approvals").await?;
    let job_ids = current_tree_job_ids(root_job_id, root);
    for approval in pending
        .into_iter()
        .filter(|approval| approval.state == "pending" && job_ids.contains(&approval.job_id))
    {
        let job: JobDetail = client
            .get(&format!("/api/jobs/{}", approval.job_id))
            .await?;
        let decision = evaluate_approval(&approval, &job);
        let action = approval_action_report(&approval, &job, &decision);
        let note = ApprovalResolutionRequest {
            note: Some(decision.reason.clone()),
        };
        if decision.approve {
            let _: JobDetail = client
                .post(
                    &format!("/api/approvals/{}/approve", approval.id),
                    Some(&note),
                )
                .await?;
            report.approved.push(action);
        } else {
            let _: JobDetail = client
                .post(&format!("/api/approvals/{}/deny", approval.id), Some(&note))
                .await?;
            report.denied.push(action);
        }
    }
    Ok(())
}

fn current_tree_job_ids(root_job_id: &str, root: &JobDetail) -> BTreeSet<String> {
    let mut ids = BTreeSet::from([root_job_id.to_string()]);
    ids.extend(root.child_jobs.iter().map(|job| job.id.clone()));
    ids
}

fn evaluate_approval(approval: &ApprovalRequestSummary, job: &JobDetail) -> ApprovalVerdict {
    let Some(call) = job
        .tool_calls
        .iter()
        .find(|call| call.id == approval.tool_call_id)
    else {
        return deny("tool call for approval was not found", Vec::new());
    };
    let Some(worker) = job
        .workers
        .iter()
        .find(|worker| worker.id == approval.worker_id)
    else {
        return deny("worker for approval was not found", Vec::new());
    };
    let worktree = Path::new(&worker.working_dir);
    if worker.working_dir.trim().is_empty() || !worktree.is_absolute() {
        return deny("worker working_dir is missing or not absolute", Vec::new());
    }

    match call.tool_id.as_str() {
        "project.inspect" => approve("safe project inspection", Vec::new()),
        "fs.list" | "fs.read_text" | "rg.search" | "git.status" | "git.diff" => {
            approve_if_paths_scoped(
                call,
                worktree,
                read_path_fields(&call.tool_id),
                "safe read/inspect tool",
            )
        }
        "fs.apply_patch" | "fs.write_text" | "fs.mkdir" => {
            approve_if_paths_scoped(call, worktree, &["path"], "worktree-local file mutation")
        }
        "fs.move" => approve_if_paths_scoped(
            call,
            worktree,
            &["from_path", "to_path"],
            "worktree-local file move",
        ),
        "command.run" => evaluate_command_run(call, worktree),
        "python.run" => evaluate_python_run(call, worktree),
        "tests.run" => evaluate_tests_run(call, worktree),
        other if other.starts_with("github.") => {
            deny("GitHub tools are denied by the harness", Vec::new())
        }
        "git.stage_patch" => deny(
            "git.stage_patch is denied to prevent publication flow",
            Vec::new(),
        ),
        other if tool_name_is_publication(other) => {
            deny("publication/release tool is denied", Vec::new())
        }
        other => deny(
            &format!("tool '{other}' is not in the harness allow-list"),
            Vec::new(),
        ),
    }
}

fn read_path_fields(tool_id: &str) -> &'static [&'static str] {
    match tool_id {
        "fs.read_text" => &["path"],
        "fs.list" | "rg.search" | "git.diff" => &["path", "pathspec"],
        _ => &[],
    }
}

fn approve_if_paths_scoped(
    call: &ToolCallSummary,
    worktree: &Path,
    fields: &[&str],
    reason: &str,
) -> ApprovalVerdict {
    let paths = extract_paths(&call.args_json, fields);
    for path in &paths {
        if !path_is_inside_worktree(worktree, path) {
            return deny(
                &format!(
                    "path '{}' resolves outside the worker worktree",
                    path.display()
                ),
                stringify_paths(paths),
            );
        }
    }
    approve(reason, stringify_paths(paths))
}

fn evaluate_command_run(call: &ToolCallSummary, worktree: &Path) -> ApprovalVerdict {
    let cwd = call
        .args_json
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or(".");
    if !path_is_inside_worktree(worktree, Path::new(cwd)) {
        return deny(
            "command cwd resolves outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if network_policy_allows_network(&call.args_json) {
        return deny("command requests network access", vec![cwd.to_string()]);
    }
    if has_conflicting_command_aliases(&call.args_json) {
        return deny(
            "command.run specifies both command and cmd aliases",
            vec![cwd.to_string()],
        );
    }
    let Some(command) = normalize_command_run_like_args(&call.args_json) else {
        return deny("command.run command is missing", vec![cwd.to_string()]);
    };
    let policy_command = match command_policy_command(&command) {
        Ok(command) => command,
        Err(reason) => {
            return deny(
                &format!("command shell wrapper is not a single simple command: {reason}"),
                vec![cwd.to_string()],
            );
        }
    };
    if command_is_publication_or_network_mutating(&policy_command) {
        return deny(
            "command looks publication, git-mutating, or network-mutating",
            vec![cwd.to_string()],
        );
    }
    if command_uses_external_git_diff(&policy_command)
        || env_requests_external_git_diff(&call.args_json)
    {
        return deny(
            "command requests an external git diff helper",
            vec![cwd.to_string()],
        );
    }
    if command_uses_git_output_file(&policy_command)
        || command_uses_cargo_clippy_fix(&policy_command)
    {
        return deny(
            "command requests write-like output or auto-fix behavior",
            vec![cwd.to_string()],
        );
    }
    if git_diff_or_log_missing_helper_disables(&policy_command) {
        return deny(
            "command git diff/log must disable external diff helpers",
            vec![cwd.to_string()],
        );
    }
    if command_has_shell_escape_or_write(&policy_command) {
        return deny(
            "command contains shell control, path traversal, or write-like operations",
            vec![cwd.to_string()],
        );
    }
    if command_has_external_path_reference(&call.args_json, worktree) {
        return deny(
            "command references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if env_has_external_path_reference(&call.args_json, worktree) {
        return deny(
            "command env references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if !command_looks_like_read_build_or_test(&policy_command) {
        return deny(
            "command program is not on the safe read/build/test allow-list",
            vec![cwd.to_string()],
        );
    }
    approve(
        "bounded read/build/test command in worker worktree",
        vec![cwd.to_string()],
    )
}

fn evaluate_python_run(call: &ToolCallSummary, worktree: &Path) -> ApprovalVerdict {
    let cwd = call
        .args_json
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or(".");
    if !path_is_inside_worktree(worktree, Path::new(cwd)) {
        return deny(
            "python cwd resolves outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if network_policy_allows_network(&call.args_json) {
        return deny("python.run requests network access", vec![cwd.to_string()]);
    }
    let script = call
        .args_json
        .get("script")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    if script.trim().is_empty() {
        return deny("python.run script is missing", vec![cwd.to_string()]);
    }
    if args_have_external_path_reference(&call.args_json, worktree) {
        return deny(
            "python.run args reference a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if env_has_external_path_reference(&call.args_json, worktree) {
        return deny(
            "python.run env references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if script_contains_external_or_network_mutation(&script) {
        return deny(
            "python script is not clearly scoped to the worker worktree",
            vec![cwd.to_string()],
        );
    }
    approve(
        "bounded python.run in worker worktree",
        vec![cwd.to_string()],
    )
}

fn evaluate_tests_run(call: &ToolCallSummary, worktree: &Path) -> ApprovalVerdict {
    let cwd = call
        .args_json
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or(".");
    if !path_is_inside_worktree(worktree, Path::new(cwd)) {
        return deny(
            "tests.run cwd resolves outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if network_policy_allows_network(&call.args_json) {
        return deny("tests.run requests network access", vec![cwd.to_string()]);
    }
    if has_conflicting_command_aliases(&call.args_json) {
        return deny(
            "tests.run specifies both command and cmd aliases",
            vec![cwd.to_string()],
        );
    }
    let Some(command) = normalize_tests_run_like_args(&call.args_json) else {
        return deny("tests.run command is missing", vec![cwd.to_string()]);
    };
    let policy_command = match command_policy_command(&command) {
        Ok(command) => command,
        Err(reason) => {
            return deny(
                &format!("tests.run shell wrapper is not a single simple command: {reason}"),
                vec![cwd.to_string()],
            );
        }
    };
    if command_is_publication_or_network_mutating(&policy_command) {
        return deny(
            "tests.run command looks publication, git-mutating, or network-mutating",
            vec![cwd.to_string()],
        );
    }
    if command_uses_external_git_diff(&policy_command)
        || env_requests_external_git_diff(&call.args_json)
    {
        return deny(
            "tests.run command requests an external git diff helper",
            vec![cwd.to_string()],
        );
    }
    if command_uses_git_output_file(&policy_command)
        || command_uses_cargo_clippy_fix(&policy_command)
    {
        return deny(
            "tests.run command requests write-like output or auto-fix behavior",
            vec![cwd.to_string()],
        );
    }
    if git_diff_or_log_missing_helper_disables(&policy_command) {
        return deny(
            "tests.run git diff/log must disable external diff helpers",
            vec![cwd.to_string()],
        );
    }
    if command_has_shell_escape_or_write(&policy_command) {
        return deny(
            "tests.run command contains shell control, path traversal, or write-like operations",
            vec![cwd.to_string()],
        );
    }
    if normalized_command_has_external_path_reference(&command, &call.args_json, worktree) {
        return deny(
            "tests.run command references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if env_has_external_path_reference(&call.args_json, worktree) {
        return deny(
            "tests.run env references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if !command_looks_like_validation(&policy_command) {
        return deny(
            "tests.run command program is not on the safe test allow-list",
            vec![cwd.to_string()],
        );
    }
    approve(
        "bounded tests.run in worker worktree",
        vec![cwd.to_string()],
    )
}

fn approve(reason: &str, target_paths: Vec<String>) -> ApprovalVerdict {
    ApprovalVerdict {
        approve: true,
        reason: reason.to_string(),
        target_paths,
    }
}

fn deny(reason: &str, target_paths: Vec<String>) -> ApprovalVerdict {
    ApprovalVerdict {
        approve: false,
        reason: reason.to_string(),
        target_paths,
    }
}

fn extract_paths(args: &Value, fields: &[&str]) -> Vec<PathBuf> {
    fields
        .iter()
        .filter_map(|field| args.get(*field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn stringify_paths(paths: Vec<PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn path_is_inside_worktree(worktree: &Path, candidate: &Path) -> bool {
    let base = normalize_path(worktree);
    let joined = if candidate.is_absolute() {
        normalize_path(candidate)
    } else {
        normalize_path(&worktree.join(candidate))
    };
    joined == base || joined.starts_with(base)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedCommandRun {
    command: String,
    args: Vec<String>,
    timeout_secs: Option<u64>,
}

impl NormalizedCommandRun {
    fn display(&self) -> String {
        let mut parts = vec![self.command.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

fn normalize_command_run_like_args(args: &Value) -> Option<NormalizedCommandRun> {
    normalize_command_like_args(args, false)
}

fn normalize_tests_run_like_args(args: &Value) -> Option<NormalizedCommandRun> {
    normalize_command_like_args(args, true)
}

fn normalize_command_like_args(
    args: &Value,
    split_command_string: bool,
) -> Option<NormalizedCommandRun> {
    let object = args.as_object()?;
    let (command_key, command_value) = if let Some(value) = object.get("cmd") {
        ("cmd", value)
    } else {
        ("command", object.get("command")?)
    };
    let explicit_args = object.get("args").filter(|value| !value.is_null());
    let mut normalized_args = explicit_args
        .and_then(command_args_value)
        .unwrap_or_default();
    let (command, mut argv) = match command_value {
        Value::String(command) => {
            let command = command.trim();
            if command.is_empty() {
                return None;
            }
            if split_command_string {
                let mut parts = command_args_value(&Value::String(command.to_string()))?;
                let command = parts.first().cloned()?;
                let argv = parts.drain(1..).collect();
                (command, argv)
            } else if command_key == "cmd"
                || (explicit_args.is_none() && command_string_looks_like_shell(command))
            {
                (
                    "sh".to_string(),
                    vec!["-lc".to_string(), command.to_string()],
                )
            } else {
                (command.to_string(), Vec::new())
            }
        }
        Value::Array(values) => {
            let mut parts = command_args_value(&Value::Array(values.clone()))?;
            let command = parts.first().cloned()?;
            let argv = if explicit_args.is_some() && !split_command_string {
                Vec::new()
            } else {
                parts.drain(1..).collect()
            };
            (command, argv)
        }
        _ => return None,
    };

    if !normalized_args.is_empty() {
        argv.append(&mut normalized_args);
    }

    Some(NormalizedCommandRun {
        command,
        args: argv,
        timeout_secs: command_timeout_secs(args),
    })
}

fn command_args_value(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
            })
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.into_iter().map(str::to_string).collect()),
        Value::String(value) => Some(
            value
                .split_whitespace()
                .filter(|part| !part.trim().is_empty())
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

fn command_timeout_secs(args: &Value) -> Option<u64> {
    args.get("timeout_secs")
        .and_then(json_value_to_u64_lossy)
        .or_else(|| {
            args.get("timeout_seconds")
                .and_then(json_value_to_u64_lossy)
        })
        .or_else(|| {
            let millis = args.get("timeout_ms").and_then(json_value_to_u64_lossy)?;
            Some(millis.saturating_add(999) / 1_000).map(|seconds| seconds.max(1))
        })
}

fn json_value_to_u64_lossy(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn command_string_looks_like_shell(command: &str) -> bool {
    command.chars().any(char::is_whitespace)
        || command.chars().any(|character| {
            matches!(
                character,
                ';' | '|'
                    | '&'
                    | '<'
                    | '>'
                    | '$'
                    | '`'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '*'
                    | '?'
                    | '~'
            )
        })
}

fn command_text(args: &Value) -> String {
    normalize_command_run_like_args(args)
        .map(|command| command.display())
        .unwrap_or_default()
}

fn has_conflicting_command_aliases(args: &Value) -> bool {
    command_alias_present(args.get("command")) && command_alias_present(args.get("cmd"))
}

fn command_alias_present(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .any(|value| !value.trim().is_empty()),
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

fn network_policy_allows_network(args: &Value) -> bool {
    args.get("network_policy")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| !matches!(value, "inherit" | "none" | "disabled" | "off" | "deny"))
}

fn command_is_publication_or_network_mutating(command: &str) -> bool {
    let lower = command.to_lowercase();
    let denied_needles = [
        "git push",
        "git commit",
        "git tag",
        "git add",
        "git stage",
        "git switch -c",
        "git checkout -b",
        "gh pr",
        "gh release",
        "npm publish",
        "pnpm publish",
        "yarn publish",
        "cargo publish",
        "docker push",
        "curl ",
        "wget ",
        "scp ",
        "rsync ",
        "ssh ",
        "npm install",
        "pnpm install",
        "yarn install",
        "bun install",
    ];
    denied_needles.iter().any(|needle| lower.contains(needle))
}

fn command_uses_external_git_diff(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !normalized.starts_with("git ") {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let runs_external_diff_command = tokens.iter().any(|token| matches!(*token, "diff" | "log"));
    runs_external_diff_command
        && (tokens.iter().any(|token| {
            *token == "--ext-diff"
                || token.starts_with("--ext-diff=")
                || token.starts_with("--config=diff.external")
                || token.starts_with("diff.external")
        }) || tokens
            .windows(2)
            .any(|window| window[0] == "-c" && window[1].starts_with("diff.external")))
}

fn command_uses_git_output_file(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !normalized.starts_with("git ") {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let writes_output = tokens
        .iter()
        .any(|token| *token == "--output" || token.starts_with("--output="));
    writes_output && tokens.iter().any(|token| matches!(*token, "diff" | "log"))
}

fn git_diff_or_log_missing_helper_disables(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !normalized.starts_with("git ") {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let runs_diff_or_log = tokens.iter().any(|token| matches!(*token, "diff" | "log"));
    if !runs_diff_or_log {
        return false;
    }
    if tokens.iter().any(|token| *token == "--textconv") {
        return true;
    }
    !(tokens.iter().any(|token| *token == "--no-ext-diff")
        && tokens.iter().any(|token| *token == "--no-textconv"))
}

fn command_uses_cargo_clippy_fix(command: &str) -> bool {
    let lower = command.to_lowercase();
    let tokens = lower.split_whitespace().collect::<Vec<_>>();
    tokens.first() == Some(&"cargo")
        && tokens.iter().any(|token| *token == "clippy")
        && tokens.iter().any(|token| *token == "--fix")
}

fn env_requests_external_git_diff(args: &Value) -> bool {
    let Some(env) = args.get("env").and_then(Value::as_object) else {
        return false;
    };
    env.iter().any(|(key, value)| {
        let key = key.to_ascii_uppercase();
        if key == "GIT_EXTERNAL_DIFF" {
            return true;
        }
        let Some(value) = env_value_text(value) else {
            return false;
        };
        let value = value.to_ascii_lowercase();
        (key == "GIT_CONFIG_PARAMETERS" || key.starts_with("GIT_CONFIG_KEY_"))
            && value.contains("diff.external")
    })
}

fn env_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn command_has_shell_escape_or_write(command: &str) -> bool {
    let lower = command.to_lowercase();
    let denied_needles = [
        ";",
        "&&",
        "||",
        "|",
        ">",
        "<",
        "\n",
        "`",
        "$",
        "$(",
        "../",
        " rm ",
        " rm -",
        " mv ",
        " cp ",
        " tee ",
        " -delete",
        " -exec",
        " -execdir",
        "sed -i",
        "perl -pi",
        "python -c",
        "python3 -c",
    ];
    denied_needles.iter().any(|needle| lower.contains(needle))
}

fn command_policy_command(command: &NormalizedCommandRun) -> Result<String, String> {
    command_policy_command_inner(command, 0)
}

fn command_policy_command_inner(
    command: &NormalizedCommandRun,
    depth: usize,
) -> Result<String, String> {
    if depth > 2 {
        return Err("too many nested shell wrappers".to_string());
    }
    if !is_shell_program(&command.command) {
        return Ok(command.display());
    }
    let Some(flag) = command.args.first().map(String::as_str) else {
        return Err("shell wrapper missing -c/-lc".to_string());
    };
    if !matches!(flag, "-c" | "-lc") {
        return Err("shell wrapper must use -c or -lc".to_string());
    }
    if command.args.len() != 2 {
        return Err("shell wrapper must contain exactly one inner command".to_string());
    }
    let inner = command.args[1].trim();
    if inner.is_empty() {
        return Err("shell wrapper inner command is empty".to_string());
    }
    if shell_string_has_unsafe_control(inner) {
        return Err("inner command contains shell control".to_string());
    }
    let tokens = shell_words(inner)?;
    if tokens.is_empty() {
        return Err("inner command is empty".to_string());
    }
    let nested = NormalizedCommandRun {
        command: tokens[0].clone(),
        args: tokens.into_iter().skip(1).collect(),
        timeout_secs: command.timeout_secs,
    };
    command_policy_command_inner(&nested, depth + 1)
}

fn is_shell_program(program: &str) -> bool {
    matches!(
        program,
        "sh" | "bash" | "/bin/sh" | "/usr/bin/sh" | "/bin/bash" | "/usr/bin/bash"
    )
}

fn shell_string_has_unsafe_control(command: &str) -> bool {
    if command.contains('\\') {
        return true;
    }
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some(current) if ch == current => quote = None,
            Some(_) => {}
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                ';' | '|' | '>' | '<' | '`' | '$' | '{' | '}' | '\n' => return true,
                '&' => return true,
                _ => {}
            },
        }
    }
    quote.is_some()
}

fn shell_words(command: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in command.chars() {
        match quote {
            Some(current_quote) if ch == current_quote => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".to_string());
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

fn command_has_external_path_reference(args: &Value, worktree: &Path) -> bool {
    let Some(command) = normalize_command_run_like_args(args) else {
        return command_tokens_have_external_path_reference(command_tokens(args), worktree);
    };
    normalized_command_has_external_path_reference(&command, args, worktree)
}

fn normalized_command_has_external_path_reference(
    command: &NormalizedCommandRun,
    args: &Value,
    worktree: &Path,
) -> bool {
    let tokens = command_policy_command(command)
        .ok()
        .and_then(|command| shell_words(&command).ok())
        .unwrap_or_else(|| command_tokens(args));
    command_tokens_have_external_path_reference(tokens, worktree)
}

fn command_tokens_have_external_path_reference(tokens: Vec<String>, worktree: &Path) -> bool {
    tokens
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 0)
        .filter_map(|(_, token)| path_like_command_token(token))
        .any(|path| !path_is_inside_worktree(worktree, &path))
}

fn args_have_external_path_reference(args: &Value, worktree: &Path) -> bool {
    args.get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(|value| value.split_whitespace().map(clean_command_token))
        .filter_map(|token| path_like_command_token(&token))
        .any(|path| !path_is_inside_worktree(worktree, &path))
}

fn env_has_external_path_reference(args: &Value, worktree: &Path) -> bool {
    args.get("env")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|env| env.values())
        .filter_map(Value::as_str)
        .flat_map(env_path_tokens)
        .filter_map(|token| path_like_command_token(&token))
        .any(|path| !path_is_inside_worktree(worktree, &path))
}

fn env_path_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| ch.is_whitespace() || ch == ':')
        .filter(|token| !token.trim().is_empty())
        .map(clean_command_token)
        .collect()
}

fn command_tokens(args: &Value) -> Vec<String> {
    let mut tokens = Vec::new();
    for field in ["command", "cmd"] {
        if let Some(alias) = args.get(field) {
            tokens.extend(command_alias_tokens(alias));
        }
    }
    if let Some(values) = args.get("args").and_then(Value::as_array) {
        for value in values.iter().filter_map(Value::as_str) {
            tokens.extend(value.split_whitespace().map(clean_command_token));
        }
    }
    tokens
}

fn command_alias_tokens(value: &Value) -> Vec<String> {
    match value {
        Value::String(command) => command
            .split_whitespace()
            .map(clean_command_token)
            .collect(),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .flat_map(|value| value.split_whitespace().map(clean_command_token))
            .collect(),
        _ => Vec::new(),
    }
}

fn clean_command_token(token: &str) -> String {
    token
        .trim_matches(|ch| matches!(ch, '\'' | '"' | ',' | ';' | '(' | ')' | '[' | ']'))
        .to_string()
}

fn path_like_command_token(token: &str) -> Option<PathBuf> {
    let raw = token.trim();
    if raw.starts_with('-') {
        return raw
            .split('=')
            .skip(1)
            .find_map(|value| path_like_value(value.trim()));
    }
    path_like_value(raw)
}

fn path_like_value(token: &str) -> Option<PathBuf> {
    let token = token.trim();
    if token.is_empty() || token.starts_with('-') {
        return None;
    }
    if token.starts_with('~') {
        return Some(PathBuf::from("/~"));
    }
    if token.starts_with('/') || token.starts_with("../") || token == ".." {
        return Some(PathBuf::from(token));
    }
    if token.contains("/../") || token.ends_with("/..") {
        return Some(PathBuf::from(token));
    }
    None
}

fn command_looks_like_read_build_or_test(command: &str) -> bool {
    let allowed = [
        "printf",
        "/usr/bin/printf",
        "echo",
        "true",
        "pwd",
        "cat",
        "ls",
        "head",
        "tail",
        "wc",
        "find",
        "rg",
        "grep",
        "git status",
        "git diff",
        "git log",
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo fmt --check",
        "cargo clippy",
        "npm run check",
        "npm run check:web",
        "npm run build",
        "npm run build:web",
        "npm run test",
        "npm test",
        "npm build",
        "pnpm test",
        "pnpm build",
        "pnpm run check",
        "pnpm run build",
        "pnpm run test",
        "yarn test",
        "yarn build",
        "yarn run check",
        "yarn run build",
        "yarn run test",
        "bun test",
        "pytest",
        "go test",
        "make test",
        "just test",
        "just check",
    ];
    command_matches_allowed_prefix(command, &allowed)
}

fn command_looks_like_validation(command: &str) -> bool {
    let allowed = [
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo fmt --check",
        "cargo clippy",
        "npm run check",
        "npm run check:web",
        "npm run build",
        "npm run build:web",
        "npm run test",
        "npm test",
        "npm build",
        "pnpm test",
        "pnpm build",
        "pnpm run check",
        "pnpm run build",
        "pnpm run test",
        "yarn test",
        "yarn build",
        "yarn run check",
        "yarn run build",
        "yarn run test",
        "bun test",
        "pytest",
        "go test",
        "make test",
        "just test",
        "just check",
    ];
    command_matches_allowed_prefix(command, &allowed) || command_is_focused_node_test(command)
}

fn command_is_focused_node_test(command: &str) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    command_policy_candidates(&normalized)
        .iter()
        .filter_map(|candidate| shell_words(candidate).ok())
        .any(|tokens| focused_node_test_tokens(&tokens))
}

fn focused_node_test_tokens(tokens: &[String]) -> bool {
    if tokens.len() != 3 || tokens[0] != "node" || tokens[1] != "--test" {
        return false;
    }
    let path = tokens[2].as_str();
    if path.is_empty()
        || path.starts_with('-')
        || path
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return false;
    }
    let file_name = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let name = file_name.to_lowercase();
    let has_test_marker = name.contains(".test.") || name.contains(".spec.");
    let has_supported_extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|extension| {
            matches!(
                extension.to_lowercase().as_str(),
                "js" | "mjs" | "cjs" | "ts" | "mts" | "cts"
            )
        })
        .unwrap_or(false);
    has_test_marker && has_supported_extension
}

fn command_session_looks_like_check_validation(command: &str) -> bool {
    let allowed = [
        "cargo check",
        "cargo build",
        "cargo fmt --check",
        "cargo clippy",
        "npm run check",
        "npm run check:web",
        "npm run build",
        "npm run build:web",
        "npm build",
        "pnpm build",
        "pnpm run check",
        "pnpm run build",
        "yarn build",
        "yarn run check",
        "yarn run build",
        "just check",
    ];
    command_matches_allowed_prefix(command, &allowed)
}

fn command_matches_allowed_prefix(command: &str, allowed: &[&str]) -> bool {
    let normalized = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let candidates = command_policy_candidates(&normalized);
    allowed.iter().any(|prefix| {
        let prefix = prefix.to_lowercase();
        candidates
            .iter()
            .any(|candidate| candidate == &prefix || candidate.starts_with(&format!("{prefix} ")))
    })
}

fn command_policy_candidates(normalized: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    push_command_policy_candidate(&mut candidates, normalized);
    for prefix in ["sh -lc ", "sh -c ", "bash -lc ", "bash -c "] {
        if let Some(unwrapped) = normalized.strip_prefix(prefix) {
            push_command_policy_candidate(&mut candidates, unwrapped);
        }
    }
    candidates
}

fn push_command_policy_candidate(candidates: &mut Vec<String>, command: &str) {
    let command = command.to_string();
    if !candidates.contains(&command) {
        candidates.push(command.clone());
    }
    if let Some(stripped) = strip_npm_workspace_flags(&command) {
        if !candidates.contains(&stripped) {
            candidates.push(stripped);
        }
    }
}

fn strip_npm_workspace_flags(command: &str) -> Option<String> {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    if parts.first().copied() != Some("npm") {
        return None;
    }

    let mut stripped = vec!["npm"];
    let mut index = 1;
    let mut changed = false;
    while index < parts.len() {
        match parts[index] {
            "--workspace" | "-w" => {
                changed = true;
                index += 2;
            }
            "--workspaces" => {
                changed = true;
                index += 1;
            }
            part if part.starts_with("--workspace=") || part.starts_with("-w=") => {
                changed = true;
                index += 1;
            }
            part => {
                stripped.push(part);
                index += 1;
            }
        }
    }

    changed.then(|| stripped.join(" "))
}

fn script_contains_external_or_network_mutation(script: &str) -> bool {
    let compact: String = script.chars().filter(|ch| !ch.is_whitespace()).collect();
    let denied = [
        "requests.",
        "urllib",
        "http://",
        "https://",
        "subprocess",
        "__import__(",
        "importlib",
        "getattr(",
        "eval(",
        "exec(",
        "socket",
        "os.system",
        "os.popen",
        ".system(",
        ".popen(",
        "os.listdir",
        "shutil.",
        "git push",
        "git commit",
        "gh pr",
        "npm publish",
        "cargo publish",
        "../",
        "..\\",
        "path('..')",
        "path(\"..\")",
        "open('..')",
        "open(\"..\")",
        "'/",
        "\"/",
        "path.home",
        ".home()",
        "expanduser",
        "os.environ",
        "os.getenv",
        "getenv(",
        "environ[",
        ".parent",
        "parents[",
        "open(",
        "open('/",
        "path('/",
        "path(\"/",
        "write_text(",
        "write_bytes(",
        ".write(",
        "touch(",
        "mkdir(",
        "rename(",
        "replace(",
        "remove(",
        "unlink(",
        "rmdir(",
        "rmtree(",
        "unlink('/",
        "rmtree('/",
    ];
    denied
        .iter()
        .any(|needle| script.contains(needle) || compact.contains(needle))
}

fn tool_name_is_publication(tool: &str) -> bool {
    let lower = tool.to_lowercase();
    lower.contains("publish") || lower.contains("publication") || lower.contains("release")
}

fn approval_action_report(
    approval: &ApprovalRequestSummary,
    job: &JobDetail,
    decision: &ApprovalVerdict,
) -> ApprovalDecisionReport {
    let tool = job
        .tool_calls
        .iter()
        .find(|call| call.id == approval.tool_call_id)
        .map(|call| call.tool_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    ApprovalDecisionReport {
        id: approval.id.clone(),
        job_id: approval.job_id.clone(),
        tool,
        target_paths: decision.target_paths.clone(),
        reason: decision.reason.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupMode {
    DeleteSession,
    PreserveEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupOperation {
    CancelRoot,
    DenyPendingApprovals,
    CleanHarnessWorktree,
    DeleteSession,
}

impl CleanupMode {
    fn preserve_evidence(self) -> bool {
        matches!(self, CleanupMode::PreserveEvidence)
    }
}

fn cleanup_mode_for_status(status: &str) -> CleanupMode {
    if status == "PASS" {
        CleanupMode::DeleteSession
    } else {
        CleanupMode::PreserveEvidence
    }
}

fn cleanup_operations(mode: CleanupMode) -> Vec<CleanupOperation> {
    let mut operations = vec![
        CleanupOperation::CancelRoot,
        CleanupOperation::DenyPendingApprovals,
    ];
    if mode == CleanupMode::DeleteSession {
        operations.push(CleanupOperation::CleanHarnessWorktree);
        operations.push(CleanupOperation::DeleteSession);
    }
    operations
}

async fn cleanup(
    client: &HarnessClient,
    session_id: &str,
    root_job_id: &str,
    approvals: &mut ApprovalReport,
    mode: CleanupMode,
) -> CleanupReport {
    let operations = cleanup_operations(mode);
    let mut report = CleanupReport {
        evidence_preserved: mode.preserve_evidence(),
        ..CleanupReport::default()
    };
    let session_detail = client
        .get::<SessionDetail>(&format!("/api/sessions/{session_id}"))
        .await
        .ok();
    if operations.contains(&CleanupOperation::CancelRoot) && !root_job_id.is_empty() {
        report.root_cancel_attempted = true;
        let _ = client
            .post::<JobDetail, Value>(&format!("/api/jobs/{root_job_id}/cancel"), None)
            .await;
        wait_for_job_terminal(client, root_job_id, Duration::from_secs(60)).await;
        if operations.contains(&CleanupOperation::DenyPendingApprovals)
            && let Ok(root) = client
                .get::<JobDetail>(&format!("/api/jobs/{root_job_id}"))
                .await
        {
            if let Ok(pending) = client
                .get::<Vec<ApprovalRequestSummary>>("/api/approvals")
                .await
            {
                let job_ids = current_tree_job_ids(root_job_id, &root);
                for approval in pending.into_iter().filter(|approval| {
                    approval.state == "pending" && job_ids.contains(&approval.job_id)
                }) {
                    let note = ApprovalResolutionRequest {
                        note: Some("cleanup denied pending harness approval".to_string()),
                    };
                    let _ = client
                        .post::<JobDetail, _>(
                            &format!("/api/approvals/{}/deny", approval.id),
                            Some(&note),
                        )
                        .await;
                    approvals.denied.push(ApprovalDecisionReport {
                        id: approval.id,
                        job_id: approval.job_id,
                        tool: "unknown".to_string(),
                        target_paths: Vec::new(),
                        reason: "cleanup denied pending harness approval".to_string(),
                    });
                }
            }
        }
    }
    if !operations.contains(&CleanupOperation::DeleteSession) {
        return report;
    }
    if operations.contains(&CleanupOperation::CleanHarnessWorktree)
        && let Err(error) = clean_harness_worktree(session_id, session_detail.as_ref())
    {
        report.reasons.push(error.to_string());
    }
    if let Err(first_error) = client
        .delete_empty(&format!("/api/sessions/{session_id}"))
        .await
    {
        if operations.contains(&CleanupOperation::CleanHarnessWorktree)
            && let Err(error) = clean_harness_worktree(session_id, session_detail.as_ref())
        {
            report.reasons.push(error.to_string());
        }
        sleep(Duration::from_secs(2)).await;
        if let Err(second_error) = client
            .delete_empty(&format!("/api/sessions/{session_id}"))
            .await
        {
            report.reasons.push(format!(
                "failed to delete session after retry: first={first_error}; second={second_error}"
            ));
        } else {
            report.session_deleted = true;
        }
    } else {
        report.session_deleted = true;
    }
    report
}

async fn wait_for_job_terminal(client: &HarnessClient, job_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match client
            .get::<JobDetail>(&format!("/api/jobs/{job_id}"))
            .await
        {
            Ok(detail) if TERMINAL_STATES.contains(&detail.job.state.as_str()) => return,
            Ok(_) => sleep(Duration::from_secs(2)).await,
            Err(_) => return,
        }
    }
}

fn clean_harness_worktree(session_id: &str, detail: Option<&SessionDetail>) -> Result<()> {
    let Some(detail) = detail else {
        return Ok(());
    };
    let working_dir = detail.session.working_dir.trim();
    if working_dir.is_empty() {
        return Ok(());
    }
    let path = Path::new(working_dir);
    if !path.is_absolute() || !path.exists() {
        return Ok(());
    }
    if !is_harness_nucleus_worktree_path(path, session_id) {
        return Ok(());
    }
    if git_command(path, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return Ok(());
    }
    git_command(path, &["reset", "--hard"])?;
    git_command(path, &["clean", "-fd"])?;
    Ok(())
}

fn is_harness_nucleus_worktree_path(path: &Path, session_id: &str) -> bool {
    let path_text = path.display().to_string();
    path_text.contains(session_id)
        && path.components().any(|component| {
            component.as_os_str() == ".nucleus"
                || component
                    .as_os_str()
                    .to_string_lossy()
                    .starts_with(".nucleus-")
        })
}

fn git_command(worktree: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn build_rung_report(
    rung: &Rung,
    session_id: &str,
    root_job_id: &str,
    root: JobDetail,
    child_details: Vec<JobDetail>,
    approvals: ApprovalReport,
) -> RungReport {
    let root_worker = root_worker_report(&root);
    let children = child_reports(&root, &child_details);
    let counts = count_report(&root, rung.max_main_children);
    let key_events = key_events(&root, &child_details);
    let phase1 = phase1_report(&root, &child_details, &counts, rung.max_main_children);
    let phase2 = phase2_report(&root);
    let root_final = root_final_report(&root);
    let read_only_exact_probe_exit_0 =
        read_only_probe_field(rung, child_details.iter().any(child_ran_read_only_probe));
    let reasons = acceptance_reasons(
        rung,
        &root,
        &child_details,
        &root_worker,
        &phase1,
        &phase2,
        read_only_exact_probe_exit_0,
    );
    let status = if phase1.passed && phase2.passed && reasons.is_empty() {
        "PASS"
    } else {
        "FAIL"
    };
    RungReport {
        name: rung.name.to_string(),
        session_id: session_id.to_string(),
        root_job_id: root_job_id.to_string(),
        status: status.to_string(),
        reasons,
        phase1,
        phase2,
        root_final,
        read_only_exact_probe_exit_0,
        root_worker,
        children,
        counts,
        approvals,
        cleanup: CleanupReport::default(),
        key_events,
    }
}

fn root_worker_report(root: &JobDetail) -> Option<WorkerReport> {
    let worker = root
        .job
        .root_worker_id
        .as_deref()
        .and_then(|id| root.workers.iter().find(|worker| worker.id == id))
        .or_else(|| root.workers.first())?;
    Some(WorkerReport {
        lane: worker.lane.clone(),
        model: worker.model.clone(),
        tool_calls: worker.tool_call_count,
    })
}

fn child_reports(root: &JobDetail, child_details: &[JobDetail]) -> Vec<ChildReport> {
    let details_by_id = child_details
        .iter()
        .map(|detail| (detail.job.id.as_str(), detail))
        .collect::<HashMap<_, _>>();
    root.child_jobs
        .iter()
        .map(|child| {
            let detail = details_by_id.get(child.id.as_str()).copied();
            let job = detail.map(|detail| &detail.job).unwrap_or(child);
            let tool_calls = detail
                .map(|detail| tool_call_reports(&detail.tool_calls))
                .unwrap_or_default();
            let tool_failures = detail
                .map(|detail| tool_failures(&detail.tool_calls))
                .unwrap_or_default();
            ChildReport {
                id: child.id.clone(),
                task_class: job.task_class.clone(),
                executor_lane: child.executor_lane.clone(),
                executor_model: child.executor_model.clone(),
                state: job.state.clone(),
                completion_status: job.completion_status.clone(),
                completion_blockers: job.completion_blockers.clone(),
                tool_calls,
                tool_failures,
            }
        })
        .collect()
}

fn tool_call_reports(calls: &[ToolCallSummary]) -> Vec<ToolCallReport> {
    calls
        .iter()
        .map(|call| ToolCallReport {
            tool_id: call.tool_id.clone(),
            args_summary: tool_call_args_summary(call),
            exit_code: tool_result_exit_code(call),
            status: call.status.clone(),
        })
        .collect()
}

fn tool_call_args_summary(call: &ToolCallSummary) -> String {
    if call.tool_id == "command.run" || call.tool_id == "tests.run" {
        let command = command_text(&call.args_json);
        if !command.trim().is_empty() {
            return truncate_summary(&command);
        }
    }
    if !call.summary.trim().is_empty() {
        return truncate_summary(&call.summary);
    }
    let args = serde_json::to_string(&call.args_json).unwrap_or_default();
    truncate_summary(&args)
}

fn truncate_summary(value: &str) -> String {
    const MAX_LEN: usize = 180;
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= MAX_LEN {
        return value;
    }
    let mut truncated = value.chars().take(MAX_LEN - 3).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn tool_failures(calls: &[ToolCallSummary]) -> Vec<ToolFailureReport> {
    calls
        .iter()
        .filter(|call| {
            matches!(call.status.as_str(), "failed" | "blocked" | "denied")
                || !call.error_class.is_empty()
                || !call.error_detail.is_empty()
        })
        .map(|call| ToolFailureReport {
            tool: call.tool_id.clone(),
            status: call.status.clone(),
            error_class: call.error_class.clone(),
            error_detail: call.error_detail.clone(),
        })
        .collect()
}

fn count_report(root: &JobDetail, max_main_children: usize) -> CountReport {
    let main_children = root
        .child_jobs
        .iter()
        .filter(|child| child.executor_lane == "main")
        .count();
    let utility_children = root
        .child_jobs
        .iter()
        .filter(|child| child.executor_lane == "utility")
        .count();
    CountReport {
        children: root.child_jobs.len(),
        main_children,
        utility_children,
        fanout_detected: main_children > max_main_children,
    }
}

fn key_events(root: &JobDetail, child_details: &[JobDetail]) -> Vec<EventReport> {
    root.events
        .iter()
        .chain(child_details.iter().flat_map(|detail| detail.events.iter()))
        .filter(|event| is_key_event(event))
        .map(|event| EventReport {
            job_id: event.job_id.clone(),
            event_type: event.event_type.clone(),
            status: event.status.clone(),
            summary: event.summary.clone(),
        })
        .collect()
}

fn is_key_event(event: &JobEvent) -> bool {
    let haystack =
        format!("{} {} {}", event.event_type, event.status, event.summary).to_lowercase();
    KEY_EVENT_TERMS.iter().any(|term| haystack.contains(term))
}

fn root_final_report(root: &JobDetail) -> RootFinalReport {
    RootFinalReport {
        state: root.job.state.clone(),
        completion_status: root.job.completion_status.clone(),
        result_summary: root.job.result_summary.clone(),
        mutation_receipt_ids: mutation_receipt_ids(root),
    }
}

fn mutation_receipt_ids(job: &JobDetail) -> Vec<i64> {
    let mut ids = BTreeSet::new();
    collect_mutation_receipt_ids(&job.job.metadata_json, &mut ids);
    for evidence in [
        &job.job.command_session_cwd_evidence_json,
        &job.job.target_entity_evidence_json,
        &job.job.process_state_evidence_json,
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(value) = serde_json::from_str::<Value>(evidence) {
            collect_mutation_receipt_ids(&value, &mut ids);
        }
    }
    for event in &job.events {
        collect_mutation_receipt_ids(&event.data_json, &mut ids);
    }
    ids.into_iter().collect()
}

fn collect_mutation_receipt_ids(value: &Value, ids: &mut BTreeSet<i64>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key == "mutation_receipt_ids" {
                    collect_i64_values(value, ids);
                } else if key == "mutation_receipt_id" {
                    collect_i64_value(value, ids);
                }
                collect_mutation_receipt_ids(value, ids);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_mutation_receipt_ids(value, ids);
            }
        }
        _ => {}
    }
}

fn collect_i64_values(value: &Value, ids: &mut BTreeSet<i64>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_i64_value(value, ids);
            }
        }
        value => collect_i64_value(value, ids),
    }
}

fn collect_i64_value(value: &Value, ids: &mut BTreeSet<i64>) {
    if let Some(id) = value.as_i64() {
        ids.insert(id);
    } else if let Some(text) = value.as_str()
        && let Ok(id) = text.parse::<i64>()
    {
        ids.insert(id);
    }
}

fn phase1_report(
    root: &JobDetail,
    child_details: &[JobDetail],
    counts: &CountReport,
    max_main_children: usize,
) -> PhaseReport {
    let mut reasons = Vec::new();
    if !job_tree_has_accepted_main_child(root, child_details) {
        reasons.push("no main child reached completed/accepted completion state".to_string());
    }
    if counts.main_children > max_main_children {
        reasons.push(format!(
            "fanout detected: {} main children exceeds limit {}",
            counts.main_children, max_main_children
        ));
    }
    PhaseReport {
        passed: reasons.is_empty(),
        reasons,
    }
}

fn phase2_report(root: &JobDetail) -> PhaseReport {
    let mut reasons = Vec::new();
    if root.job.state != "completed" {
        reasons.push("root did not converge with a completed result".to_string());
    }
    if root.job.completion_status == "blocked" {
        reasons.push("root completion status was blocked".to_string());
    }
    PhaseReport {
        passed: reasons.is_empty(),
        reasons,
    }
}

fn acceptance_reasons(
    rung: &Rung,
    root: &JobDetail,
    child_details: &[JobDetail],
    root_worker: &Option<WorkerReport>,
    phase1: &PhaseReport,
    phase2: &PhaseReport,
    read_only_exact_probe_exit_0: Option<bool>,
) -> Vec<String> {
    let mut reasons = common_reasons(root, root_worker);
    reasons.extend(
        phase1
            .reasons
            .iter()
            .map(|reason| format!("phase1: {reason}")),
    );
    reasons.extend(
        phase2
            .reasons
            .iter()
            .map(|reason| format!("phase2: {reason}")),
    );
    match rung.acceptance {
        Acceptance::ReadOnlyProbe => {
            read_only_reasons(child_details, read_only_exact_probe_exit_0, &mut reasons)
        }
        Acceptance::EditAndTest => edit_and_test_reasons(child_details, &mut reasons),
        Acceptance::Feature161 => feature_reasons(child_details, &mut reasons),
        Acceptance::Debug => debug_reasons(child_details, &mut reasons),
    }
    external_artifact_reasons(root, child_details, &mut reasons);
    reasons
}

fn common_reasons(root: &JobDetail, root_worker: &Option<WorkerReport>) -> Vec<String> {
    let mut reasons = Vec::new();
    match root_worker {
        Some(worker) if worker.lane == "utility" => {}
        Some(worker) => reasons.push(format!("root worker lane was {}, not utility", worker.lane)),
        None => reasons.push("root worker was not reported".to_string()),
    }
    if let Some(worker) = root_worker {
        if worker.tool_calls > 1 {
            reasons.push(format!(
                "root worker made {} tool calls, expected approximately zero",
                worker.tool_calls
            ));
        }
    }
    for child in &root.child_jobs {
        if !TERMINAL_STATES.contains(&child.state.as_str()) {
            reasons.push(format!(
                "child {} was still {}, not terminal",
                child.id, child.state
            ));
        }
    }
    reasons
}

fn read_only_reasons(
    child_details: &[JobDetail],
    read_only_exact_probe_exit_0: Option<bool>,
    reasons: &mut Vec<String>,
) {
    if read_only_exact_probe_exit_0 != Some(true) {
        reasons.push("no child ran the exact read_only probe command with exit 0".to_string());
    }
    if child_details
        .iter()
        .any(child_blocked_on_validation_evidence)
    {
        reasons.push("child blocked on validation evidence".to_string());
    }
}

fn edit_and_test_reasons(child_details: &[JobDetail], reasons: &mut Vec<String>) {
    if !child_details.iter().any(child_has_mutation) {
        reasons.push("no child made an approved worktree edit".to_string());
    }
    if !child_details.iter().any(child_has_successful_test_run) {
        reasons.push("no child ran a focused validation command".to_string());
    }
}

fn feature_reasons(child_details: &[JobDetail], reasons: &mut Vec<String>) {
    let has_implementation_validation = child_details.iter().any(|child| {
        child_accepted(child) && child_has_mutation(child) && child_has_successful_test_run(child)
    });
    if !has_implementation_validation {
        reasons.push("feature rung lacked accepted implementation+validation evidence".to_string());
    }
}

fn debug_reasons(child_details: &[JobDetail], reasons: &mut Vec<String>) {
    if child_details.is_empty() {
        reasons.push("debug rung produced no child diagnosis".to_string());
    }
}

fn external_artifact_reasons(
    root: &JobDetail,
    child_details: &[JobDetail],
    reasons: &mut Vec<String>,
) {
    for job in std::iter::once(root).chain(child_details.iter()) {
        if !job.job.pr_url.trim().is_empty() {
            reasons.push(format!(
                "job {} reported PR URL {}",
                job.job.id, job.job.pr_url
            ));
        }
        if !matches!(job.job.publication_status.as_str(), "" | "not_requested") {
            reasons.push(format!(
                "job {} reported publication status {}",
                job.job.id, job.job.publication_status
            ));
        }
        for call in &job.tool_calls {
            if call.tool_id.starts_with("github.")
                || call.tool_id == "git.stage_patch"
                || tool_name_is_publication(&call.tool_id)
            {
                reasons.push(format!(
                    "job {} attempted external/publication tool {}",
                    job.job.id, call.tool_id
                ));
            }
        }
    }
}

fn child_ran_read_only_probe(child: &JobDetail) -> bool {
    child
        .command_sessions
        .iter()
        .any(command_session_is_successful_read_only_probe)
        || child.tool_calls.iter().any(|call| {
            call.tool_id == "command.run"
                && call.status == "completed"
                && tool_result_exit_zero(call)
                && command_is_exact_read_only_probe(&command_text(&call.args_json))
        })
}

fn command_session_is_successful_read_only_probe(session: &CommandSessionSummary) -> bool {
    command_session_success(session)
        && command_is_exact_read_only_probe(&command_session_text(session))
}

fn command_is_exact_read_only_probe(command: &str) -> bool {
    command.split_whitespace().collect::<Vec<_>>().join(" ") == READ_ONLY_PROBE_COMMAND
}

fn command_session_success(session: &CommandSessionSummary) -> bool {
    session.state == "completed" && session.exit_code == Some(0)
}

fn tool_result_exit_code(call: &ToolCallSummary) -> Option<i64> {
    call.result_json
        .as_ref()
        .and_then(|value| value.get("exit_code"))
        .and_then(Value::as_i64)
}

fn tool_result_exit_zero(call: &ToolCallSummary) -> bool {
    call.result_json.as_ref().is_some_and(|value| {
        tool_result_exit_code(call) == Some(0)
            && value
                .pointer("/validation_interpretation/status")
                .and_then(Value::as_str)
                != Some("no_tests_matched")
    })
}

fn read_only_probe_field(rung: &Rung, value: bool) -> Option<bool> {
    matches!(rung.acceptance, Acceptance::ReadOnlyProbe).then_some(value)
}

fn job_tree_has_accepted_main_child(root: &JobDetail, child_details: &[JobDetail]) -> bool {
    child_details
        .iter()
        .any(|child| child.job.executor_lane == "main" && child_accepted(child))
        || root.child_jobs.iter().any(|child| {
            child.executor_lane == "main"
                && child.state == "completed"
                && child.completion_status != "blocked"
        })
}

fn child_accepted(child: &JobDetail) -> bool {
    (child.job.state == "completed" && child.job.completion_status != "blocked")
        || child_failed_with_context_pressure_and_validation(child)
}

fn child_failed_with_context_pressure_and_validation(child: &JobDetail) -> bool {
    child.job.state == "failed"
        && child
            .job
            .completion_blockers
            .iter()
            .any(|blocker| blocker.contains(CONTEXT_PRESSURE_BLOCKER_MARKER))
        && child_has_mutation(child)
        && child_has_successful_test_run(child)
}

fn child_blocked_on_validation_evidence(child: &JobDetail) -> bool {
    child.job.completion_blockers.iter().any(|blocker| {
        let lower = blocker.to_lowercase();
        lower.contains("validation evidence")
            || lower.contains("completion was claimed without successful validation")
    })
}

fn child_has_mutation(child: &JobDetail) -> bool {
    child.tool_calls.iter().any(tool_call_is_file_mutation)
}

fn tool_call_is_file_mutation(call: &ToolCallSummary) -> bool {
    matches!(
        call.tool_id.as_str(),
        "fs.apply_patch" | "fs.write_text" | "fs.move"
    ) && call.status == "completed"
}

fn child_has_successful_test_run(child: &JobDetail) -> bool {
    child
        .tool_calls
        .iter()
        .any(tool_call_is_successful_test_run)
        || child.command_sessions.iter().any(|session| {
            command_session_success(session)
                && command_session_looks_like_check_validation(&command_session_text(session))
        })
}

fn tool_call_is_successful_test_run(call: &ToolCallSummary) -> bool {
    (call.tool_id == "tests.run" || call.tool_id == "command.run")
        && call.status == "completed"
        && tool_result_exit_zero(call)
        && command_looks_like_validation(&tool_call_command_text(call))
}

fn tool_call_command_text(call: &ToolCallSummary) -> String {
    let command = if call.tool_id == "tests.run" {
        normalize_tests_run_like_args(&call.args_json)
    } else {
        normalize_command_run_like_args(&call.args_json)
    };
    command
        .map(|command| command_policy_command(&command).unwrap_or_else(|_| command.display()))
        .unwrap_or_default()
}

fn command_session_text(session: &CommandSessionSummary) -> String {
    format!("{} {}", session.command, session.args.join(" "))
}

fn routes_report(workspace: &WorkspaceSummary) -> RoutesReport {
    let default_profile_id = workspace.default_profile_id.as_str();
    RoutesReport {
        default_profile_id: workspace.default_profile_id.clone(),
        main_target: workspace.main_target.clone(),
        utility_target: workspace.utility_target.clone(),
        profiles: workspace
            .profiles
            .iter()
            .filter(|profile| profile.id == default_profile_id || profile.is_default)
            .map(|profile| ProfileRouteReport {
                id: profile.id.clone(),
                title: profile.title.clone(),
                is_default: profile.is_default,
                main: ModelRouteReport {
                    adapter: profile.main.adapter.clone(),
                    model: profile.main.model.clone(),
                    base_url: profile.main.base_url.clone(),
                },
                utility: ModelRouteReport {
                    adapter: profile.utility.adapter.clone(),
                    model: profile.utility.model.clone(),
                    base_url: profile.utility.base_url.clone(),
                },
            })
            .collect(),
    }
}

fn write_report(path: &Path, report: &HarnessReport) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let payload = serde_json::to_string_pretty(report).context("failed to serialize report")?;
    fs::write(path, format!("{payload}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn print_summary(report: &HarnessReport, output: &Path) {
    println!("\nNucleus dogfood harness ({})", report.install.url);
    println!("version: {}", report.install.version);
    println!("report: {}", output.display());
    println!(
        "\n{:<16} {:<6} {:<8} {:<8} {}",
        "rung", "status", "phase1", "phase2", "reason"
    );
    for rung in &report.rungs {
        let reason = rung
            .reasons
            .first()
            .cloned()
            .unwrap_or_else(|| "ok".to_string());
        println!(
            "{:<16} {:<6} {:<8} {:<8} {}",
            rung.name,
            rung.status,
            pass_fail(rung.phase1.passed),
            pass_fail(rung.phase2.passed),
            reason
        );
        if rung.cleanup.evidence_preserved {
            println!(
                "  preserved evidence: session_id={} root_job_id={}",
                rung.session_id, rung.root_job_id
            );
        }
    }
    println!(
        "\noverall: {}/{} passed, {} failed",
        report.overall.passed, report.overall.total, report.overall.failed
    );
}

fn pass_fail(passed: bool) -> &'static str {
    if passed { "PASS" } else { "FAIL" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_policy_rejects_compound_shell_mutation() {
        let command = "sh -lc cargo test; rm -rf ../other";

        assert!(command_has_shell_escape_or_write(command));
    }

    #[test]
    fn command_policy_rejects_destructive_find() {
        let command = "find . -delete";

        assert!(command_looks_like_read_build_or_test(command));
        assert!(command_has_shell_escape_or_write(command));
    }

    #[test]
    fn command_policy_allows_benign_shell_wrapped_commands() {
        for args in [
            json!({"command":"sh","args":["-lc","printf NUCLEUS_COMMAND_RUN_PROBE"],"cwd":"."}),
            json!({"command":"bash","args":["-c","cargo test"],"cwd":"."}),
            json!({"command":"sh","args":["-lc","npm test"],"cwd":"."}),
        ] {
            let call = tool_call_with_args("command.run", args);

            let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(verdict.approve, "{}", verdict.reason);
        }
    }

    #[test]
    fn command_policy_allows_nested_cmd_alias_shell_probe_when_inner_is_safe() {
        let call = tool_call_with_args(
            "command.run",
            json!({"cmd":"sh -lc 'printf NUCLEUS_COMMAND_RUN_PROBE'","cwd":".","timeout_seconds":30}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(verdict.approve, "{}", verdict.reason);
    }

    #[test]
    fn command_policy_rejects_dangerous_shell_wrapped_commands() {
        for (args, expected_reason) in [
            (
                json!({"command":"sh","args":["-lc","rm -rf x"],"cwd":"."}),
                "allow-list",
            ),
            (
                json!({"command":"sh","args":["-lc","git push"],"cwd":"."}),
                "publication",
            ),
            (
                json!({"command":"sh","args":["-lc","curl http://example.invalid"],"cwd":"."}),
                "network",
            ),
            (
                json!({"command":"sh","args":["-lc","echo x > /etc/y"],"cwd":"."}),
                "single simple",
            ),
            (
                json!({"command":"sh","args":["-lc","a && b"],"cwd":"."}),
                "single simple",
            ),
            (
                json!({"command":"sh","args":["-lc","cat $(pwd)"],"cwd":"."}),
                "single simple",
            ),
            (
                json!({"command":"./sh","args":["-lc","cargo test"],"cwd":"."}),
                "allow-list",
            ),
            (
                json!({"command":"tools/bash","args":["-lc","cargo test"],"cwd":"."}),
                "allow-list",
            ),
            (
                json!({"command":"sh","args":["-lc","custom-tool check"],"cwd":"."}),
                "allow-list",
            ),
        ] {
            let call = tool_call_with_args("command.run", args);

            let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(
                !verdict.approve,
                "unexpected approval for {expected_reason}"
            );
            assert!(
                verdict.reason.contains(expected_reason),
                "reason '{}' did not contain '{expected_reason}'",
                verdict.reason
            );
        }
    }

    #[test]
    fn command_policy_rejects_external_path_arguments() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"rg","args":["token","/home/eba/.ssh"],"cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn command_policy_rejects_array_alias_external_paths() {
        let call = tool_call_with_args(
            "command.run",
            json!({"cmd":["rg","token","/home/eba/.ssh"],"cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn command_policy_rejects_conflicting_command_aliases() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"cargo test","cmd":"git push","cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("both command and cmd"));
    }

    #[test]
    fn command_policy_rejects_conflicting_array_aliases() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"cargo test","cmd":["sh","-lc","git push"],"cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("both command and cmd"));
    }

    #[test]
    fn command_text_prefers_array_cmd_alias() {
        let args = json!({"cmd":["sh","-lc","cargo test -p nucleus-core"],"cwd":"."});

        assert_eq!(command_text(&args), "sh -lc cargo test -p nucleus-core");
        assert!(command_looks_like_validation(&command_text(&args)));
    }

    #[test]
    fn command_policy_normalizes_cmd_and_timeout_aliases() {
        let args = json!({"cmd":"printf NUCLEUS_COMMAND_RUN_PROBE","cwd":".","timeout_ms":1500});
        let normalized = normalize_command_run_like_args(&args).expect("normalize command args");

        assert_eq!(normalized.command, "sh");
        assert_eq!(
            normalized.args,
            vec!["-lc", "printf NUCLEUS_COMMAND_RUN_PROBE"]
        );
        assert_eq!(normalized.timeout_secs, Some(2));

        let call = tool_call_with_args("command.run", args);
        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(verdict.approve, "{}", verdict.reason);
    }

    #[test]
    fn tests_run_policy_preserves_mixed_cmd_and_args_aliases() {
        let args = json!({"cmd":"cargo test","args":["-p","nucleus-daemon"],"cwd":".","timeout_seconds":120});
        let normalized = normalize_tests_run_like_args(&args).expect("normalize tests args");

        assert_eq!(normalized.command, "cargo");
        assert_eq!(normalized.args, vec!["test", "-p", "nucleus-daemon"]);
        assert_eq!(normalized.timeout_secs, Some(120));

        let call = tool_call_with_args("tests.run", args);
        let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(verdict.approve, "{}", verdict.reason);
    }

    #[test]
    fn command_policy_rejects_option_attached_external_paths() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"cargo","args":["test","--manifest-path=/tmp/other/Cargo.toml"],"cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn command_policy_rejects_nested_option_external_paths() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"cargo","args":["test","--config=build.rustc-wrapper=/tmp/evil"],"cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn command_policy_rejects_external_env_paths() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"cargo","args":["test"],"cwd":".","env":{"CARGO_TARGET_DIR":"/tmp/outside"}}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn command_policy_rejects_shell_home_paths() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"sh","args":["-lc","grep token ~/.ssh/id_rsa"],"cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn command_policy_rejects_shell_env_expansion_paths() {
        let call = tool_call_with_args(
            "command.run",
            json!({"command":"sh","args":["-lc","grep token $HOME/.ssh/id_rsa"],"cwd":"."}),
        );

        let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("shell control"));
    }

    #[test]
    fn command_policy_rejects_unbounded_package_scripts() {
        let deploy = "pnpm run deploy";
        let check = "pnpm run check";

        assert!(!command_looks_like_read_build_or_test(deploy));
        assert!(command_looks_like_read_build_or_test(check));
    }

    #[test]
    fn command_policy_rejects_raw_interpreters_and_installs() {
        for args in [
            json!({"command":"node","args":["scripts/check.js"],"cwd":"."}),
            json!({"command":"python","args":["scripts/check.py"],"cwd":"."}),
            json!({"command":"python3","args":["scripts/check.py"],"cwd":"."}),
            json!({"command":"npm","args":["ci"],"cwd":"."}),
            json!({"command":"pnpm","args":["ci"],"cwd":"."}),
            json!({"command":"yarn","args":["install","--immutable"],"cwd":"."}),
        ] {
            let call = tool_call_with_args("command.run", args);

            let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(!verdict.approve, "unexpected approval for {call:?}");
            assert!(verdict.reason.contains("allow-list") || verdict.reason.contains("network"));
        }
    }

    #[test]
    fn command_policy_rejects_external_git_diff_helpers() {
        for args in [
            json!({"command":"git","args":["diff","--ext-diff"],"cwd":"."}),
            json!({"command":"git","args":["log","--ext-diff","-p","-1"],"cwd":"."}),
            json!({"command":"git","args":["diff"],"cwd":".","env":{"GIT_EXTERNAL_DIFF":"sh -c echo"}}),
            json!({"command":"git","args":["diff"],"cwd":".","env":{"GIT_CONFIG_COUNT":"1","GIT_CONFIG_KEY_0":"diff.external","GIT_CONFIG_VALUE_0":"sh -c echo"}}),
            json!({"command":"git","args":["diff"],"cwd":".","env":{"GIT_CONFIG_PARAMETERS":"'diff.external=sh -c echo'"}}),
            json!({"command":"git","args":["diff","-c","diff.external=sh -c echo"],"cwd":"."}),
            json!({"command":"git","args":["-c","diff.external=sh -c echo","diff"],"cwd":"."}),
            json!({"cmd":"git -c diff.external=sh diff","cwd":"."}),
        ] {
            let call = tool_call_with_args("command.run", args);

            let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(!verdict.approve, "unexpected approval for {call:?}");
            assert!(verdict.reason.contains("external git diff"));
        }
    }

    #[test]
    fn command_policy_rejects_write_like_git_and_clippy_options() {
        for args in [
            json!({"command":"git","args":["diff","--output=probe.patch"],"cwd":"."}),
            json!({"command":"git","args":["log","--output","probe.log"],"cwd":"."}),
            json!({"command":"cargo","args":["clippy","--fix"],"cwd":"."}),
            json!({"command":"sh","args":["-c","cargo clippy --fi\\x"],"cwd":"."}),
            json!({"command":"bash","args":["-lc","cat {/etc/passwd,./Cargo.toml}"],"cwd":"."}),
        ] {
            let call = tool_call_with_args("command.run", args);

            let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(!verdict.approve, "unexpected approval for {call:?}");
            assert!(
                verdict.reason.contains("write-like")
                    || verdict.reason.contains("auto-fix")
                    || verdict.reason.contains("shell wrapper")
                    || verdict.reason.contains("shell control")
            );
        }
    }

    #[test]
    fn command_policy_requires_git_diff_log_helper_disables() {
        for args in [
            json!({"command":"git","args":["diff"],"cwd":"."}),
            json!({"command":"git","args":["log","-p","-1"],"cwd":"."}),
            json!({"command":"git","args":["diff","--no-ext-diff","--no-textconv","--textconv"],"cwd":"."}),
        ] {
            let call = tool_call_with_args("command.run", args);

            let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(!verdict.approve, "unexpected approval for {call:?}");
            assert!(verdict.reason.contains("disable external diff helpers"));
        }

        for args in [
            json!({"command":"git","args":["diff","--no-ext-diff","--no-textconv"],"cwd":"."}),
            json!({"command":"git","args":["log","--no-ext-diff","--no-textconv","-p","-1"],"cwd":"."}),
        ] {
            let call = tool_call_with_args("command.run", args);

            let verdict = evaluate_command_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(
                verdict.approve,
                "unexpected denial for {call:?}: {verdict:?}"
            );
        }
    }

    #[test]
    fn tests_run_policy_rejects_external_git_diff_helpers() {
        for args in [
            json!({"command":"git","args":["-c","diff.external=sh -c echo","diff"],"cwd":"."}),
            json!({"command":"git","args":["log","--ext-diff","-p","-1"],"cwd":"."}),
            json!({"command":"git","args":["diff"],"cwd":".","env":{"GIT_CONFIG_COUNT":"1","GIT_CONFIG_KEY_0":"diff.external","GIT_CONFIG_VALUE_0":"sh -c echo"}}),
        ] {
            let call = tool_call_with_args("tests.run", args);

            let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(!verdict.approve, "unexpected approval for {call:?}");
            assert!(verdict.reason.contains("external git diff"));
        }
    }

    #[test]
    fn tests_run_policy_rejects_write_like_git_and_clippy_options() {
        for args in [
            json!({"command":"git","args":["diff","--output=probe.patch"],"cwd":"."}),
            json!({"command":"cargo","args":["clippy","--fix"],"cwd":"."}),
            json!({"command":"sh","args":["-c","cargo clippy --fi\\x"],"cwd":"."}),
            json!({"command":"bash","args":["-lc","cat {/etc/passwd,./Cargo.toml}"],"cwd":"."}),
        ] {
            let call = tool_call_with_args("tests.run", args);

            let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(!verdict.approve, "unexpected approval for {call:?}");
            assert!(
                verdict.reason.contains("write-like")
                    || verdict.reason.contains("auto-fix")
                    || verdict.reason.contains("shell wrapper")
                    || verdict.reason.contains("shell control")
            );
        }
    }

    #[test]
    fn tests_run_policy_allows_array_command_with_explicit_args() {
        let call = tool_call_with_args(
            "tests.run",
            json!({"command":["cargo","test"],"args":["-p","nucleus-core"],"cwd":"."}),
        );

        let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(verdict.approve, "unexpected denial: {verdict:?}");
    }

    #[test]
    fn tests_run_policy_rejects_array_tail_external_paths() {
        let call = tool_call_with_args(
            "tests.run",
            json!({"command":["cargo","test","--manifest-path=/tmp/other/Cargo.toml"],"args":["-p","nucleus-core"],"cwd":"."}),
        );

        let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn command_policy_requires_cargo_fmt_check() {
        let bare_fmt = tool_call_with_args(
            "tests.run",
            json!({"command":"cargo","args":["fmt"],"cwd":"."}),
        );
        let checked_fmt = tool_call_with_args(
            "tests.run",
            json!({"command":"cargo","args":["fmt","--check"],"cwd":"."}),
        );

        let bare = evaluate_tests_run(&bare_fmt, Path::new("/tmp/nucleus-dogfood-worktree"));
        let checked = evaluate_tests_run(&checked_fmt, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!bare.approve);
        assert!(bare.reason.contains("allow-list"));
        assert!(checked.approve, "{}", checked.reason);
    }

    #[test]
    fn command_policy_allows_repo_web_validation_scripts() {
        assert!(command_looks_like_read_build_or_test("npm run check:web"));
        assert!(command_looks_like_read_build_or_test("npm run build:web"));
        assert!(command_looks_like_validation("npm run check:web"));
        assert!(command_looks_like_validation("npm run build:web"));
        assert!(command_session_looks_like_check_validation(
            "npm run check:web"
        ));
    }

    #[test]
    fn command_policy_allows_shell_wrapped_validation_commands() {
        assert!(command_looks_like_read_build_or_test(
            "sh -lc cargo test -p nucleus-core"
        ));
        assert!(command_looks_like_validation(
            "sh -lc cargo test -p nucleus-core"
        ));
        assert!(command_looks_like_validation("bash -lc npm run check:web"));
    }

    #[test]
    fn command_policy_allows_npm_workspace_validation_flags() {
        assert!(command_looks_like_validation(
            "npm --workspace @nucleus/web test -- src/activity.test.ts"
        ));
        assert!(command_looks_like_validation(
            "npm --workspace=@nucleus/web run check:web"
        ));
        assert!(command_looks_like_validation(
            "sh -lc npm -w @nucleus/web test -- src/activity.test.ts"
        ));
        assert!(!command_looks_like_validation(
            "npm --workspace @nucleus/web run deploy"
        ));
    }

    #[test]
    fn tests_run_policy_allows_focused_node_test_file() {
        let call = tool_call_with_args(
            "tests.run",
            json!({"command":"node","args":["--test","apps/web/src/lib/nucleus/session-ux.test.mjs"],"cwd":"."}),
        );

        let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(verdict.approve, "unexpected denial: {verdict:?}");
        assert!(command_looks_like_validation(
            "node --test apps/web/src/lib/nucleus/session-ux.test.mjs"
        ));
    }

    #[test]
    fn tests_run_policy_rejects_unbounded_node_and_npm_exec_shapes() {
        for args in [
            json!({"command":"node","args":["apps/web/src/lib/nucleus/session-ux.test.mjs"],"cwd":"."}),
            json!({"command":"node","args":["--test"],"cwd":"."}),
            json!({"command":"node","args":["--test","apps/web/src/lib/nucleus/*.test.mjs"],"cwd":"."}),
            json!({"command":"node","args":["--test","apps/web/src/lib/nucleus"],"cwd":"."}),
            json!({"command":"npm","args":["--workspace","@nucleus/web","exec","--","node","--test","src/lib/nucleus/session-ux.test.mjs"],"cwd":"."}),
        ] {
            let call = tool_call_with_args("tests.run", args);

            let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

            assert!(!verdict.approve, "unexpected approval for {call:?}");
            assert!(verdict.reason.contains("allow-list"));
        }
    }

    #[test]
    fn http_request_timeout_is_bounded_by_rung_timeout() {
        assert_eq!(http_request_timeout(0), Duration::from_secs(1));
        assert_eq!(http_request_timeout(5), Duration::from_secs(5));
        assert_eq!(http_request_timeout(900), Duration::from_secs(30));
    }

    #[test]
    fn command_policy_uses_anchored_allow_list_matches() {
        assert!(!command_looks_like_read_build_or_test(
            "eslint --fix apps/web/src"
        ));
        assert!(command_looks_like_read_build_or_test("ls apps/web/src"));
    }

    #[test]
    fn validation_tool_call_requires_zero_exit() {
        let failed = tool_call("tests.run", "completed", Some(1));
        let passed = tool_call("tests.run", "completed", Some(0));

        assert!(!tool_call_is_successful_test_run(&failed));
        assert!(tool_call_is_successful_test_run(&passed));
    }

    #[test]
    fn validation_tool_call_uses_tests_run_array_normalization() {
        let mut call = tool_call_with_args(
            "tests.run",
            json!({"command":["cargo","test"],"args":["-p","nucleus-core"],"cwd":"."}),
        );
        call.status = "completed".to_string();
        call.result_json = Some(json!({"exit_code": 0}));

        assert!(tool_call_is_successful_test_run(&call));
    }

    #[test]
    fn validation_tool_call_unwraps_command_run_shell_alias() {
        let mut call = tool_call_with_args(
            "command.run",
            json!({"cmd":"sh -lc 'cargo test'","cwd":"."}),
        );
        call.status = "completed".to_string();
        call.result_json = Some(json!({"exit_code": 0}));

        assert!(tool_call_is_successful_test_run(&call));
    }

    #[test]
    fn validation_tool_call_rejects_no_tests_matched() {
        let mut no_match = tool_call("tests.run", "completed", Some(0));
        no_match.result_json = Some(json!({
            "exit_code": 0,
            "validation_interpretation": {"status": "no_tests_matched"}
        }));

        assert!(!tool_call_is_successful_test_run(&no_match));
    }

    #[test]
    fn focused_node_test_evidence_is_accepted_when_exit_zero() {
        let mut call = tool_call_with_args(
            "tests.run",
            json!({"command":"node","args":["--test","apps/web/src/lib/nucleus/session-ux.test.mjs"],"cwd":"."}),
        );
        call.status = "completed".to_string();
        call.result_json = Some(json!({"exit_code": 0}));

        assert!(tool_call_is_successful_test_run(&call));
    }

    #[test]
    fn failed_broader_npm_test_is_not_accepted_as_validation() {
        let mut call = tool_call_with_args(
            "tests.run",
            json!({"command":"npm","args":["--workspace","@nucleus/web","test","--","src/lib/nucleus/session-ux.test.mjs"],"cwd":"."}),
        );
        call.status = "completed".to_string();
        call.result_json = Some(json!({"exit_code": 1}));

        assert!(!tool_call_is_successful_test_run(&call));
    }

    #[test]
    fn feature_161_accepts_delegated_focused_node_validation() {
        let rung = test_rung(Acceptance::Feature161, 1);
        let root = root_detail(
            "completed",
            "satisfied",
            vec![child_summary("child", "completed", "satisfied")],
        );
        let mutation = tool_call("fs.apply_patch", "completed", None);
        let mut validation = tool_call_with_args(
            "tests.run",
            json!({"command":"node","args":["--test","apps/web/src/lib/nucleus/session-ux.test.mjs"],"cwd":"."}),
        );
        validation.status = "completed".to_string();
        validation.result_json = Some(json!({"exit_code": 0}));
        let child = child_detail(
            "child",
            "completed",
            "satisfied",
            vec![mutation, validation],
        );
        let root_worker = Some(WorkerReport {
            lane: "utility".to_string(),
            model: "cx/gpt-mini".to_string(),
            tool_calls: 0,
        });

        let reasons = acceptance_reasons(
            &rung,
            &root,
            &[child],
            &root_worker,
            &PhaseReport {
                passed: true,
                reasons: Vec::new(),
            },
            &PhaseReport {
                passed: true,
                reasons: Vec::new(),
            },
            None,
        );

        assert!(
            reasons.is_empty(),
            "focused validation should satisfy feature_161: {reasons:?}"
        );
    }

    #[test]
    fn feature_161_rejects_root_tool_drift_even_with_child_validation() {
        let rung = test_rung(Acceptance::Feature161, 1);
        let root = root_detail(
            "completed",
            "satisfied",
            vec![child_summary("child", "completed", "satisfied")],
        );
        let mutation = tool_call("fs.apply_patch", "completed", None);
        let validation = tool_call("tests.run", "completed", Some(0));
        let child = child_detail(
            "child",
            "completed",
            "satisfied",
            vec![mutation, validation],
        );
        let root_worker = Some(WorkerReport {
            lane: "utility".to_string(),
            model: "cx/gpt-mini".to_string(),
            tool_calls: 2,
        });

        let reasons = acceptance_reasons(
            &rung,
            &root,
            &[child],
            &root_worker,
            &PhaseReport {
                passed: true,
                reasons: Vec::new(),
            },
            &PhaseReport {
                passed: true,
                reasons: Vec::new(),
            },
            None,
        );

        assert!(reasons.iter().any(|reason| {
            reason == "root worker made 2 tool calls, expected approximately zero"
        }));
        assert!(!reasons.iter().any(|reason| {
            reason == "feature rung lacked accepted implementation+validation evidence"
        }));
    }

    #[test]
    fn command_session_validation_fallback_excludes_test_commands() {
        assert!(!command_session_looks_like_check_validation("cargo test"));
        assert!(!command_session_looks_like_check_validation("npm test"));
        assert!(!command_session_looks_like_check_validation("npm run test"));
        assert!(!command_session_looks_like_check_validation("pnpm test"));
        assert!(!command_session_looks_like_check_validation("yarn test"));
        assert!(command_session_looks_like_check_validation("cargo check"));
        assert!(command_session_looks_like_check_validation(
            "npm run check:web"
        ));
    }

    #[test]
    fn mutation_evidence_requires_file_edit() {
        let mut mkdir = tool_call_with_args("fs.mkdir", json!({"path":"tmp"}));
        mkdir.status = "completed".to_string();
        let mut patch = tool_call_with_args("fs.apply_patch", json!({"path":"src/lib.rs"}));
        patch.status = "completed".to_string();

        assert!(!tool_call_is_file_mutation(&mkdir));
        assert!(tool_call_is_file_mutation(&patch));
    }

    #[test]
    fn validation_tool_call_requires_validation_command() {
        let mut read_only =
            tool_call_with_args("command.run", json!({"command":"pwd","args":[],"cwd":"."}));
        read_only.status = "completed".to_string();
        read_only.result_json = Some(json!({"exit_code": 0}));

        assert!(command_looks_like_read_build_or_test(&command_text(
            &read_only.args_json
        )));
        assert!(!command_looks_like_validation(&command_text(
            &read_only.args_json
        )));
        assert!(!tool_call_is_successful_test_run(&read_only));
    }

    #[test]
    fn tests_run_policy_rejects_publication_subcommand() {
        let call = tool_call_with_args(
            "tests.run",
            json!({"command":"npm","args":["publish"],"cwd":"."}),
        );

        let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("publication"));
    }

    #[test]
    fn tests_run_policy_rejects_external_env_paths() {
        let call = tool_call_with_args(
            "tests.run",
            json!({"command":"npm","args":["test"],"cwd":".","env":{"TMPDIR":"/tmp/outside"}}),
        );

        let verdict = evaluate_tests_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn python_policy_rejects_parent_path_writes() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"from pathlib import Path\nPath('../other-session/file').write_text('x')"}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("worker worktree"));
    }

    #[test]
    fn python_policy_rejects_split_parent_path_reads() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"from pathlib import Path\nprint((Path('..') / 'other-session' / 'secret').read_text())"}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("worker worktree"));
    }

    #[test]
    fn python_policy_rejects_absolute_path_writes() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"from pathlib import Path\nPath(\"/tmp/nucleus-dogfood-leak\").touch()"}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("worker worktree"));
    }

    #[test]
    fn python_policy_rejects_spaced_external_reads() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"from pathlib import Path\nPath (\"/etc/passwd\").read_text()"}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("worker worktree"));
    }

    #[test]
    fn python_policy_rejects_absolute_path_strings() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"import os\nprint(os.listdir('/home/eba/.ssh'))"}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("worker worktree"));
    }

    #[test]
    fn python_policy_rejects_home_path_helpers() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"from pathlib import Path\nprint((Path.home() / '.ssh/id_rsa').read_text())"}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("worker worktree"));
    }

    #[test]
    fn python_policy_rejects_dynamic_process_escape() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"__import__('os').system('touch /tmp/nucleus-dogfood-leak')"}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("worker worktree"));
    }

    #[test]
    fn write_report_allows_current_directory_output() {
        let path = Path::new("nucleus-dogfood-report-current-dir-test.json");
        let _ = fs::remove_file(path);

        write_report(path, &minimal_report()).expect("write report in current dir");

        assert!(path.exists());
        fs::remove_file(path).expect("remove current-dir report fixture");
    }

    #[test]
    fn python_policy_rejects_external_args() {
        let call = tool_call_with_args(
            "python.run",
            json!({"cwd":".","script":"from pathlib import Path\nprint(Path(__import__('sys').argv[1]).read_text())","args":["/home/eba/.ssh/id_rsa"]}),
        );

        let verdict = evaluate_python_run(&call, Path::new("/tmp/nucleus-dogfood-worktree"));

        assert!(!verdict.approve);
        assert!(verdict.reason.contains("outside the worker worktree"));
    }

    #[test]
    fn explicit_inherit_network_policy_matches_default() {
        assert!(!network_policy_allows_network(
            &json!({"network_policy":"inherit"})
        ));
        assert!(!network_policy_allows_network(&json!({})));
        assert!(network_policy_allows_network(
            &json!({"network_policy":"enabled"})
        ));
    }

    #[test]
    fn read_only_probe_requires_exact_command() {
        assert!(command_is_exact_read_only_probe(
            "printf NUCLEUS_COMMAND_RUN_PROBE"
        ));
        assert!(command_looks_like_read_build_or_test(
            "printf NUCLEUS_COMMAND_RUN_PROBE"
        ));
        assert!(!command_is_exact_read_only_probe("pwd"));
        assert!(!command_is_exact_read_only_probe("printf WRONG_PROBE"));
    }

    #[test]
    fn read_only_probe_prompt_uses_argv_and_avoids_narrow_trigger_words() {
        let read_only = ladder()
            .into_iter()
            .find(|rung| rung.name == "read_only")
            .expect("read_only rung");
        let prompt = read_only.prompt.to_lowercase();

        assert!(read_only.prompt.contains(
            r#"{"command":"printf","args":["NUCLEUS_COMMAND_RUN_PROBE"],"cwd":".","timeout_secs":20}"#
        ));
        for trigger in [
            "investigation",
            "editing",
            "files",
            "implement",
            "implementation",
            "test",
            "tests",
            "validation",
            "write",
            "create",
            "delete",
            "remove",
            "replace",
            "publish",
            "commit",
            "pull request",
            "pr ",
            "status",
        ] {
            assert!(
                !prompt.contains(trigger),
                "read_only prompt contains trigger word {trigger}"
            );
        }
    }

    #[test]
    fn cleanup_recognizes_default_nucleus_state_worktree() {
        assert!(is_harness_nucleus_worktree_path(
            Path::new("/home/eba/.nucleus/worktrees/session-123/work"),
            "session-123"
        ));
        assert!(is_harness_nucleus_worktree_path(
            Path::new("/home/eba/.nucleus-dev-projects/worktrees/session-123/work"),
            "session-123"
        ));
        assert!(!is_harness_nucleus_worktree_path(
            Path::new("/home/eba/projects/session-123/work"),
            "session-123"
        ));
    }

    #[test]
    fn rung_evaluator_splits_phase1_and_phase2() {
        let rung = test_rung(Acceptance::Debug, 1);

        let completed = build_rung_report(
            &rung,
            "session",
            "root",
            root_detail(
                "completed",
                "satisfied",
                vec![child_summary("child", "completed", "satisfied")],
            ),
            vec![child_detail("child", "completed", "satisfied", Vec::new())],
            ApprovalReport::default(),
        );
        assert_eq!(completed.status, "PASS");
        assert!(completed.phase1.passed);
        assert!(completed.phase2.passed);

        let mutation = tool_call("fs.apply_patch", "completed", None);
        let validation = tool_call("tests.run", "completed", Some(0));
        let mut failed_evidence_child = child_detail(
            "child",
            "failed",
            "blocked",
            vec![mutation.clone(), validation.clone()],
        );
        failed_evidence_child.job.completion_blockers.push(format!(
            "{CONTEXT_PRESSURE_BLOCKER_MARKER}: prompt remained above threshold after compaction"
        ));
        let recovered_context_pressure = build_rung_report(
            &rung,
            "session",
            "root",
            root_detail(
                "completed",
                "satisfied",
                vec![child_summary("child", "failed", "blocked")],
            ),
            vec![failed_evidence_child.clone()],
            ApprovalReport::default(),
        );
        assert_eq!(recovered_context_pressure.status, "PASS");
        assert!(recovered_context_pressure.phase1.passed);
        assert!(recovered_context_pressure.phase2.passed);

        let blocked_root_with_recovered_child = build_rung_report(
            &rung,
            "session",
            "root",
            root_detail(
                "blocked",
                "blocked",
                vec![child_summary("child", "failed", "blocked")],
            ),
            vec![failed_evidence_child],
            ApprovalReport::default(),
        );
        assert_eq!(blocked_root_with_recovered_child.status, "FAIL");
        assert!(blocked_root_with_recovered_child.phase1.passed);
        assert!(!blocked_root_with_recovered_child.phase2.passed);

        let root_not_converged = build_rung_report(
            &rung,
            "session",
            "root",
            root_detail(
                "blocked",
                "pending",
                vec![child_summary("child", "completed", "satisfied")],
            ),
            vec![child_detail("child", "completed", "satisfied", Vec::new())],
            ApprovalReport::default(),
        );
        assert!(root_not_converged.phase1.passed);
        assert!(!root_not_converged.phase2.passed);
        assert!(
            root_not_converged
                .phase2
                .reasons
                .iter()
                .any(|reason| reason.contains("root did not converge"))
        );

        let child_blocked = build_rung_report(
            &rung,
            "session",
            "root",
            root_detail(
                "completed",
                "satisfied",
                vec![child_summary("child", "blocked", "blocked")],
            ),
            vec![child_detail("child", "blocked", "blocked", Vec::new())],
            ApprovalReport::default(),
        );
        assert!(!child_blocked.phase1.passed);
        assert!(child_blocked.phase2.passed);

        let fanout = build_rung_report(
            &rung,
            "session",
            "root",
            root_detail(
                "completed",
                "satisfied",
                vec![
                    child_summary("child-1", "completed", "satisfied"),
                    child_summary("child-2", "completed", "satisfied"),
                ],
            ),
            vec![
                child_detail("child-1", "completed", "satisfied", Vec::new()),
                child_detail("child-2", "completed", "satisfied", Vec::new()),
            ],
            ApprovalReport::default(),
        );
        assert!(!fanout.phase1.passed);
        assert!(
            fanout
                .phase1
                .reasons
                .iter()
                .any(|reason| reason.contains("fanout detected"))
        );

        let utility_child_only = build_rung_report(
            &rung,
            "session",
            "root",
            root_detail(
                "completed",
                "satisfied",
                vec![utility_child_summary(
                    "utility-child",
                    "completed",
                    "satisfied",
                )],
            ),
            vec![utility_child_detail(
                "utility-child",
                "completed",
                "satisfied",
                Vec::new(),
            )],
            ApprovalReport::default(),
        );
        assert!(!utility_child_only.phase1.passed);
        assert!(
            utility_child_only
                .phase1
                .reasons
                .iter()
                .any(|reason| reason.contains("no main child"))
        );
    }

    #[test]
    fn report_serializer_includes_diagnostic_child_and_root_fields() {
        let rung = test_rung(Acceptance::ReadOnlyProbe, 2);
        let mut root = root_detail(
            "completed",
            "satisfied",
            vec![child_summary("child", "completed", "satisfied")],
        );
        root.job.result_summary = "root joined child result".to_string();
        root.job.metadata_json = json!({"context_integrity":{"mutation_receipt_ids":[7,"8"]}});
        root.events.push(JobEvent {
            id: 1,
            job_id: "root".to_string(),
            worker_id: None,
            event_type: "completion.gate".to_string(),
            status: "satisfied".to_string(),
            summary: "receipt explained mutation".to_string(),
            detail: String::new(),
            data_json: json!({"mutation_receipt_id": 9}),
            created_at: 0,
        });
        let mut probe = tool_call_with_args(
            "command.run",
            json!({"command":"printf","args":["NUCLEUS_COMMAND_RUN_PROBE"],"cwd":"."}),
        );
        probe.status = "completed".to_string();
        probe.result_json = Some(json!({"exit_code": 0}));

        let report = build_rung_report(
            &rung,
            "session",
            "root",
            root,
            vec![child_detail("child", "completed", "satisfied", vec![probe])],
            ApprovalReport::default(),
        );
        let value = serde_json::to_value(&report).expect("serialize report");

        assert_eq!(value["root_final"]["state"], "completed");
        assert_eq!(
            value["root_final"]["result_summary"],
            "root joined child result"
        );
        assert_eq!(
            value["root_final"]["mutation_receipt_ids"],
            json!([7, 8, 9])
        );
        assert_eq!(value["read_only_exact_probe_exit_0"], true);
        assert_eq!(value["children"][0]["task_class"], "delegated_subtask");
        assert_eq!(value["children"][0]["executor_lane"], "main");
        assert_eq!(value["children"][0]["executor_model"], "cx/gpt-test");
        assert_eq!(
            value["children"][0]["tool_calls"][0]["tool_id"],
            "command.run"
        );
        assert_eq!(value["children"][0]["tool_calls"][0]["exit_code"], 0);
        assert_eq!(value["children"][0]["tool_calls"][0]["status"], "completed");
    }

    #[test]
    fn cleanup_decision_preserves_failures_and_deletes_passes() {
        assert_eq!(
            cleanup_mode_for_status("FAIL"),
            CleanupMode::PreserveEvidence
        );
        assert_eq!(cleanup_mode_for_status("PASS"), CleanupMode::DeleteSession);
        let failure_operations = cleanup_operations(cleanup_mode_for_status("FAIL"));
        let pass_operations = cleanup_operations(cleanup_mode_for_status("PASS"));

        assert!(failure_operations.contains(&CleanupOperation::CancelRoot));
        assert!(failure_operations.contains(&CleanupOperation::DenyPendingApprovals));
        assert!(!failure_operations.contains(&CleanupOperation::DeleteSession));
        assert!(!failure_operations.contains(&CleanupOperation::CleanHarnessWorktree));
        assert!(pass_operations.contains(&CleanupOperation::DeleteSession));
        assert!(pass_operations.contains(&CleanupOperation::CleanHarnessWorktree));
    }

    fn tool_call(tool_id: &str, status: &str, exit_code: Option<i32>) -> ToolCallSummary {
        let mut call = tool_call_with_args(
            tool_id,
            json!({"command":"cargo","args":["test"],"cwd":"."}),
        );
        call.status = status.to_string();
        call.result_json = exit_code.map(|exit_code| json!({"exit_code": exit_code}));
        call
    }

    fn tool_call_with_args(tool_id: &str, args_json: Value) -> ToolCallSummary {
        ToolCallSummary {
            id: "tool-call".to_string(),
            job_id: "job".to_string(),
            worker_id: "worker".to_string(),
            tool_id: tool_id.to_string(),
            status: "pending".to_string(),
            summary: String::new(),
            args_json,
            result_json: None,
            policy_decision: None,
            artifact_ids: Vec::new(),
            error_class: String::new(),
            error_detail: String::new(),
            created_at: 0,
            started_at: None,
            completed_at: None,
        }
    }

    fn test_rung(acceptance: Acceptance, max_main_children: usize) -> Rung {
        Rung {
            name: "test",
            prompt: "test",
            max_main_children,
            acceptance,
        }
    }

    fn root_detail(state: &str, completion_status: &str, children: Vec<JobSummary>) -> JobDetail {
        JobDetail {
            job: job_summary(
                "root",
                state,
                completion_status,
                None,
                "utility",
                "cx/gpt-mini",
            ),
            workers: vec![worker_summary(
                "root-worker",
                "root",
                "utility",
                "cx/gpt-mini",
            )],
            child_jobs: children,
            tool_calls: Vec::new(),
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: Vec::new(),
        }
    }

    fn child_summary(id: &str, state: &str, completion_status: &str) -> JobSummary {
        job_summary(
            id,
            state,
            completion_status,
            Some("delegated_subtask"),
            "main",
            "cx/gpt-test",
        )
    }

    fn utility_child_summary(id: &str, state: &str, completion_status: &str) -> JobSummary {
        job_summary(
            id,
            state,
            completion_status,
            Some("delegated_subtask"),
            "utility",
            "cx/gpt-mini-test",
        )
    }

    fn child_detail(
        id: &str,
        state: &str,
        completion_status: &str,
        tool_calls: Vec<ToolCallSummary>,
    ) -> JobDetail {
        JobDetail {
            job: child_summary(id, state, completion_status),
            workers: vec![worker_summary("child-worker", id, "main", "cx/gpt-test")],
            child_jobs: Vec::new(),
            tool_calls,
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: Vec::new(),
        }
    }

    fn utility_child_detail(
        id: &str,
        state: &str,
        completion_status: &str,
        tool_calls: Vec<ToolCallSummary>,
    ) -> JobDetail {
        JobDetail {
            job: utility_child_summary(id, state, completion_status),
            workers: vec![worker_summary(
                "utility-child-worker",
                id,
                "utility",
                "cx/gpt-mini-test",
            )],
            child_jobs: Vec::new(),
            tool_calls,
            approvals: Vec::new(),
            artifacts: Vec::new(),
            command_sessions: Vec::new(),
            events: Vec::new(),
        }
    }

    fn worker_summary(
        id: &str,
        job_id: &str,
        lane: &str,
        model: &str,
    ) -> nucleus_protocol::WorkerSummary {
        nucleus_protocol::WorkerSummary {
            id: id.to_string(),
            job_id: job_id.to_string(),
            parent_worker_id: None,
            title: "Worker".to_string(),
            lane: lane.to_string(),
            state: "completed".to_string(),
            provider: "test".to_string(),
            model: model.to_string(),
            route_id: String::new(),
            route_title: String::new(),
            provider_base_url: String::new(),
            provider_api_key: String::new(),
            provider_session_id: String::new(),
            working_dir: "/tmp/nucleus-dogfood-worktree".to_string(),
            read_roots: Vec::new(),
            write_roots: Vec::new(),
            max_steps: 10,
            max_tool_calls: 10,
            max_wall_clock_secs: 60,
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

    fn job_summary(
        id: &str,
        state: &str,
        completion_status: &str,
        task_class: Option<&str>,
        lane: &str,
        model: &str,
    ) -> JobSummary {
        JobSummary {
            id: id.to_string(),
            session_id: Some("session".to_string()),
            parent_job_id: (id != "root").then(|| "root".to_string()),
            template_id: None,
            task_class: task_class.map(str::to_string),
            title: id.to_string(),
            purpose: String::new(),
            trigger_kind: "manual".to_string(),
            state: state.to_string(),
            requested_by: "test".to_string(),
            prompt_excerpt: String::new(),
            root_worker_id: (id == "root").then(|| "root-worker".to_string()),
            executor_lane: lane.to_string(),
            executor_provider: "test".to_string(),
            executor_model: model.to_string(),
            executor_route_id: String::new(),
            executor_route_title: String::new(),
            visible_turn_id: None,
            result_summary: String::new(),
            last_error: String::new(),
            user_error: None,
            ui_renderable: String::new(),
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
            completion_status: completion_status.to_string(),
            completion_gates: Vec::new(),
            completion_blockers: if completion_status == "blocked" {
                vec!["blocked by test fixture".to_string()]
            } else {
                Vec::new()
            },
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

    fn minimal_report() -> HarnessReport {
        HarnessReport {
            install: InstallReport {
                url: "http://127.0.0.1:5202".to_string(),
                version: "test".to_string(),
                routes: RoutesReport {
                    default_profile_id: String::new(),
                    main_target: String::new(),
                    utility_target: String::new(),
                    profiles: Vec::new(),
                },
            },
            rungs: Vec::new(),
            overall: OverallReport {
                passed: 0,
                failed: 0,
                total: 0,
            },
        }
    }
}
