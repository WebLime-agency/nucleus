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

const READ_ONLY_PROBE_COMMAND: &str = "sh -lc printf NUCLEUS_COMMAND_RUN_PROBE";
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
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const TERMINAL_STATES: &[&str] = &["completed", "blocked", "failed", "canceled"];
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
    root_worker: Option<WorkerReport>,
    children: Vec<ChildReport>,
    counts: CountReport,
    approvals: ApprovalReport,
    key_events: Vec<EventReport>,
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
    lane: String,
    model: String,
    state: String,
    completion_status: String,
    completion_blockers: Vec<String>,
    tool_failures: Vec<ToolFailureReport>,
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
    let client = HarnessClient::new(&args.base_url, &token)?;
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
                root_worker: None,
                children: Vec::new(),
                counts: CountReport {
                    children: 0,
                    main_children: 0,
                    utility_children: 0,
                    fanout_detected: false,
                },
                approvals: ApprovalReport::default(),
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
    Ok(())
}

impl HarnessClient {
    fn new(base_url: &str, token: &str) -> Result<Self> {
        let base_url = Url::parse(base_url).context("invalid base URL")?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .context("auth token is not a valid HTTP header value")?,
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
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

Use the Nucleus delegation path. The root worker must stay utility/orchestration-only and should not run local tools itself. Delegate exactly one main child whose task is to run this exact tool call in its own worktree and report the command result:

{"command":"sh","args":["-lc","printf NUCLEUS_COMMAND_RUN_PROBE"],"cwd":".","timeout_secs":20}

Do not edit files. Do not publish, push, create branches manually, or create a PR. After the child reports, join the result and give a concise final answer with the child job id, lane/model if known, exit status, and stdout."#,
        },
        Rung {
            name: "edit_and_test",
            max_main_children: 1,
            acceptance: Acceptance::EditAndTest,
            prompt: r#"Dogfood ladder rung: edit_and_test.

Use the Nucleus delegation path. The root worker must stay utility/orchestration-only and should not edit or run validation itself. Delegate one main child to make a tiny, well-scoped code change in its isolated worktree: add a small pure helper in crates/core/src/lib.rs and a focused unit test for it in the same crate. The helper should be Nucleus-themed and low-risk, for example formatting a short activity/status phrase. The child must use daemon file tools for edits and run a focused cargo test for the touched crate.

Do not publish, push, create branches manually, stage changes, commit, or create a PR. Join the child result and report the changed file(s), test command, and validation result."#,
        },
        Rung {
            name: "feature_161",
            max_main_children: 1,
            acceptance: Acceptance::Feature161,
            prompt: r#"Dogfood ladder rung: feature_161.

Implement issue #161: subtle Nucleus-themed activity messages with rate-limited rotation. Use the Nucleus delegation path. The root worker must stay utility/orchestration-only. Delegate one main child to read issue #161 and the web client, add a small feature with a focused test, and validate it. If the child hits a daemon gate or missing contract, recover deterministically if possible; otherwise surface a precise blocker with evidence.

Do not publish, push, create branches manually, stage changes, commit, or create a PR. Join the child result into one concise final answer with implementation/validation evidence or the precise blocker."#,
        },
        Rung {
            name: "debug",
            max_main_children: 2,
            acceptance: Acceptance::Debug,
            prompt: r#"Dogfood ladder rung: debug.

Use the Nucleus delegation path. The root worker must stay utility/orchestration-only. Delegate a bounded main child to diagnose and fix this small issue in its isolated worktree: find a focused unit test or helper in the Rust workspace that can be improved to reject empty human-facing status text, make the smallest reasonable fix, and run a focused test. If no appropriate fix is safe, return a precise blocker with the files inspected.

Do not publish, push, create branches manually, stage changes, commit, or create a PR. Join the child result and report the diagnosis, changed file(s), and validation result or precise blocker."#,
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

    let cleanup_reasons = cleanup(
        client,
        &session_id,
        root_job_id.as_str(),
        &mut approval_report,
    )
    .await;

    let mut report = result?;
    report.approvals.approved.extend(approval_report.approved);
    report.approvals.denied.extend(approval_report.denied);
    if !cleanup_reasons.is_empty() {
        report.reasons.extend(
            cleanup_reasons
                .into_iter()
                .map(|reason| format!("cleanup: {reason}")),
        );
        report.status = "FAIL".to_string();
    }
    Ok(RungOutcome { report })
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
    let command = command_text(&call.args_json);
    if command.trim().is_empty() {
        return deny("command.run command is missing", vec![cwd.to_string()]);
    }
    if command_is_publication_or_network_mutating(&command) {
        return deny(
            "command looks publication, git-mutating, or network-mutating",
            vec![cwd.to_string()],
        );
    }
    if command_has_shell_escape_or_write(&command) {
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
    if !command_looks_like_read_build_or_test(&command) {
        return deny(
            "command is not clearly read/build/test scoped",
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
    let command = command_text(&call.args_json);
    if command.trim().is_empty() {
        return deny("tests.run command is missing", vec![cwd.to_string()]);
    }
    if command_is_publication_or_network_mutating(&command) {
        return deny(
            "tests.run command looks publication, git-mutating, or network-mutating",
            vec![cwd.to_string()],
        );
    }
    if command_has_shell_escape_or_write(&command) {
        return deny(
            "tests.run command contains shell control, path traversal, or write-like operations",
            vec![cwd.to_string()],
        );
    }
    if command_has_external_path_reference(&call.args_json, worktree) {
        return deny(
            "tests.run command references a path outside the worker worktree",
            vec![cwd.to_string()],
        );
    }
    if !command_looks_like_validation(&command) {
        return deny(
            "tests.run command is not clearly test scoped",
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

fn command_text(args: &Value) -> String {
    let command = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let argv = args
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    format!("{command} {argv}")
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

fn command_has_external_path_reference(args: &Value, worktree: &Path) -> bool {
    command_tokens(args)
        .iter()
        .filter_map(|token| path_like_command_token(token))
        .any(|path| !path_is_inside_worktree(worktree, &path))
}

fn command_tokens(args: &Value) -> Vec<String> {
    let mut tokens = Vec::new();
    for field in ["command", "cmd"] {
        if let Some(command) = args.get(field).and_then(Value::as_str) {
            tokens.extend(command.split_whitespace().map(clean_command_token));
        }
    }
    if let Some(values) = args.get("args").and_then(Value::as_array) {
        for value in values.iter().filter_map(Value::as_str) {
            tokens.extend(value.split_whitespace().map(clean_command_token));
        }
    }
    tokens
}

fn clean_command_token(token: &str) -> String {
    token
        .trim_matches(|ch| matches!(ch, '\'' | '"' | ',' | ';' | '(' | ')' | '[' | ']'))
        .to_string()
}

fn path_like_command_token(token: &str) -> Option<PathBuf> {
    let raw = token.trim();
    let token = if raw.starts_with('-') {
        raw.split_once('=').map(|(_, value)| value.trim())?
    } else {
        raw
    };
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
    let lower = command.to_lowercase();
    let allowed = [
        "printf ",
        "pwd",
        "ls",
        "find",
        "rg ",
        "grep ",
        "cargo test",
        "cargo check",
        "cargo build",
        "npm run check",
        "npm run build",
        "npm test",
        "pnpm test",
        "pnpm run check",
        "pnpm run build",
        "pnpm run test",
        "yarn test",
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
    allowed.iter().any(|needle| lower.contains(needle))
}

fn command_looks_like_validation(command: &str) -> bool {
    let lower = command.to_lowercase();
    let allowed = [
        "cargo test",
        "cargo check",
        "cargo build",
        "npm run check",
        "npm run build",
        "npm test",
        "pnpm test",
        "pnpm run check",
        "pnpm run build",
        "pnpm run test",
        "yarn test",
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
    allowed.iter().any(|needle| lower.contains(needle))
}

fn script_contains_external_or_network_mutation(script: &str) -> bool {
    let compact: String = script.chars().filter(|ch| !ch.is_whitespace()).collect();
    let denied = [
        "requests.",
        "urllib",
        "http://",
        "https://",
        "subprocess",
        "socket",
        "os.system",
        "os.popen",
        "os.listdir",
        "shutil.",
        "git push",
        "git commit",
        "gh pr",
        "npm publish",
        "cargo publish",
        "../",
        "..\\",
        "'/",
        "\"/",
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

async fn cleanup(
    client: &HarnessClient,
    session_id: &str,
    root_job_id: &str,
    approvals: &mut ApprovalReport,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let session_detail = client
        .get::<SessionDetail>(&format!("/api/sessions/{session_id}"))
        .await
        .ok();
    if !root_job_id.is_empty() {
        let _ = client
            .post::<JobDetail, Value>(&format!("/api/jobs/{root_job_id}/cancel"), None)
            .await;
        wait_for_job_terminal(client, root_job_id, Duration::from_secs(60)).await;
        if let Ok(root) = client
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
    if let Err(error) = clean_harness_worktree(session_id, session_detail.as_ref()) {
        reasons.push(error.to_string());
    }
    if let Err(first_error) = client
        .delete_empty(&format!("/api/sessions/{session_id}"))
        .await
    {
        if let Err(error) = clean_harness_worktree(session_id, session_detail.as_ref()) {
            reasons.push(error.to_string());
        }
        sleep(Duration::from_secs(2)).await;
        if let Err(second_error) = client
            .delete_empty(&format!("/api/sessions/{session_id}"))
            .await
        {
            reasons.push(format!(
                "failed to delete session after retry: first={first_error}; second={second_error}"
            ));
        }
    }
    reasons
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
    let path_text = path.display().to_string();
    if !path_text.contains(session_id) || !path_text.contains("/.nucleus-") {
        return Ok(());
    }
    if git_command(path, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return Ok(());
    }
    git_command(path, &["reset", "--hard"])?;
    git_command(path, &["clean", "-fd"])?;
    Ok(())
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
    let reasons = acceptance_reasons(rung, &root, &child_details, &counts, &root_worker);
    RungReport {
        name: rung.name.to_string(),
        session_id: session_id.to_string(),
        root_job_id: root_job_id.to_string(),
        status: if reasons.is_empty() { "PASS" } else { "FAIL" }.to_string(),
        reasons,
        root_worker,
        children,
        counts,
        approvals,
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
            let tool_failures = detail
                .map(|detail| tool_failures(&detail.tool_calls))
                .unwrap_or_default();
            ChildReport {
                id: child.id.clone(),
                lane: child.executor_lane.clone(),
                model: child.executor_model.clone(),
                state: child.state.clone(),
                completion_status: child.completion_status.clone(),
                completion_blockers: child.completion_blockers.clone(),
                tool_failures,
            }
        })
        .collect()
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

fn acceptance_reasons(
    rung: &Rung,
    root: &JobDetail,
    child_details: &[JobDetail],
    counts: &CountReport,
    root_worker: &Option<WorkerReport>,
) -> Vec<String> {
    let mut reasons = common_reasons(rung, root, counts, root_worker);
    match rung.acceptance {
        Acceptance::ReadOnlyProbe => read_only_reasons(root, child_details, &mut reasons),
        Acceptance::EditAndTest => edit_and_test_reasons(root, child_details, &mut reasons),
        Acceptance::Feature161 => feature_reasons(root, child_details, &mut reasons),
        Acceptance::Debug => debug_reasons(root, child_details, &mut reasons),
    }
    external_artifact_reasons(root, child_details, &mut reasons);
    reasons
}

fn common_reasons(
    rung: &Rung,
    root: &JobDetail,
    counts: &CountReport,
    root_worker: &Option<WorkerReport>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if root.job.state != "completed" && root.job.state != "blocked" {
        reasons.push(format!("root job ended in state {}", root.job.state));
    }
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
    if counts.main_children == 0 {
        reasons.push("no main child was observed".to_string());
    }
    if counts.main_children > rung.max_main_children {
        reasons.push(format!(
            "fanout detected: {} main children exceeds limit {}",
            counts.main_children, rung.max_main_children
        ));
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

fn read_only_reasons(root: &JobDetail, child_details: &[JobDetail], reasons: &mut Vec<String>) {
    if root.job.state != "completed" {
        reasons.push("root did not converge with a completed result".to_string());
    }
    let command_ok = child_details.iter().any(child_ran_read_only_probe);
    if !command_ok {
        reasons.push("no child ran the exact read_only probe command with exit 0".to_string());
    }
    if !child_details.iter().any(child_accepted) {
        reasons.push("no child reached accepted/completed completion state".to_string());
    }
    if child_details
        .iter()
        .any(child_blocked_on_validation_evidence)
    {
        reasons.push("child blocked on validation evidence".to_string());
    }
}

fn edit_and_test_reasons(root: &JobDetail, child_details: &[JobDetail], reasons: &mut Vec<String>) {
    if root.job.state != "completed" {
        reasons.push("root did not converge with a completed result".to_string());
    }
    if !child_details.iter().any(child_has_mutation) {
        reasons.push("no child made an approved worktree edit".to_string());
    }
    if !child_details.iter().any(child_has_successful_test_run) {
        reasons.push("no child ran a focused validation command".to_string());
    }
    if !child_details.iter().any(child_accepted) {
        reasons.push("no child reached accepted/completed completion state".to_string());
    }
}

fn feature_reasons(root: &JobDetail, child_details: &[JobDetail], reasons: &mut Vec<String>) {
    let has_implementation_validation = child_details.iter().any(|child| {
        child_accepted(child) && child_has_mutation(child) && child_has_successful_test_run(child)
    });
    let has_terminal_blocker =
        root_has_precise_blocker(root) && all_children_terminal(child_details);
    if root.job.state != "completed" && !has_terminal_blocker {
        reasons
            .push("feature rung did not converge to completion or a precise blocker".to_string());
    }
    if !has_implementation_validation && !has_terminal_blocker {
        reasons.push(
            "feature rung lacked implementation+validation evidence or a precise blocker"
                .to_string(),
        );
    }
}

fn debug_reasons(root: &JobDetail, child_details: &[JobDetail], reasons: &mut Vec<String>) {
    let has_terminal_blocker =
        root_has_precise_blocker(root) && all_children_terminal(child_details);
    if root.job.state != "completed" && !has_terminal_blocker {
        reasons.push("debug rung did not converge to completion or a precise blocker".to_string());
    }
    if child_details.is_empty() {
        reasons.push("debug rung produced no child diagnosis".to_string());
    }
}

fn all_children_terminal(child_details: &[JobDetail]) -> bool {
    !child_details.is_empty()
        && child_details
            .iter()
            .all(|child| TERMINAL_STATES.contains(&child.job.state.as_str()))
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

fn tool_result_exit_zero(call: &ToolCallSummary) -> bool {
    call.result_json
        .as_ref()
        .and_then(|value| value.get("exit_code"))
        .and_then(Value::as_i64)
        == Some(0)
}

fn child_accepted(child: &JobDetail) -> bool {
    child.job.state == "completed" && child.job.completion_status != "blocked"
}

fn child_blocked_on_validation_evidence(child: &JobDetail) -> bool {
    child.job.completion_blockers.iter().any(|blocker| {
        let lower = blocker.to_lowercase();
        lower.contains("validation evidence")
            || lower.contains("completion was claimed without successful validation")
    })
}

fn child_has_mutation(child: &JobDetail) -> bool {
    child.tool_calls.iter().any(|call| {
        matches!(
            call.tool_id.as_str(),
            "fs.apply_patch" | "fs.write_text" | "fs.move" | "fs.mkdir"
        ) && call.status == "completed"
    })
}

fn child_has_successful_test_run(child: &JobDetail) -> bool {
    child
        .tool_calls
        .iter()
        .any(tool_call_is_successful_test_run)
        || child.command_sessions.iter().any(|session| {
            command_session_success(session)
                && command_looks_like_validation(&command_session_text(session))
        })
}

fn tool_call_is_successful_test_run(call: &ToolCallSummary) -> bool {
    (call.tool_id == "tests.run" || call.tool_id == "command.run")
        && call.status == "completed"
        && tool_result_exit_zero(call)
        && command_looks_like_validation(&command_text(&call.args_json))
}

fn command_session_text(session: &CommandSessionSummary) -> String {
    format!("{} {}", session.command, session.args.join(" "))
}

fn root_has_precise_blocker(root: &JobDetail) -> bool {
    !root.job.completion_blockers.is_empty()
        || !root.job.last_error.trim().is_empty()
        || root.job.result_summary.to_lowercase().contains("blocker")
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
    if let Some(parent) = path.parent() {
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
    println!("\n{:<16} {:<6} {}", "rung", "status", "reason");
    for rung in &report.rungs {
        let reason = rung
            .reasons
            .first()
            .cloned()
            .unwrap_or_else(|| "ok".to_string());
        println!("{:<16} {:<6} {}", rung.name, rung.status, reason);
    }
    println!(
        "\noverall: {}/{} passed, {} failed",
        report.overall.passed, report.overall.total, report.overall.failed
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_policy_rejects_compound_shell_mutation() {
        let command = "sh -lc cargo test; rm -rf ../other";

        assert!(command_looks_like_read_build_or_test(command));
        assert!(command_has_shell_escape_or_write(command));
    }

    #[test]
    fn command_policy_rejects_destructive_find() {
        let command = "find . -delete";

        assert!(command_looks_like_read_build_or_test(command));
        assert!(command_has_shell_escape_or_write(command));
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
    fn command_policy_rejects_unbounded_package_scripts() {
        let deploy = "pnpm run deploy";
        let check = "pnpm run check";

        assert!(!command_looks_like_read_build_or_test(deploy));
        assert!(command_looks_like_read_build_or_test(check));
    }

    #[test]
    fn validation_tool_call_requires_zero_exit() {
        let failed = tool_call("tests.run", "completed", Some(1));
        let passed = tool_call("tests.run", "completed", Some(0));

        assert!(!tool_call_is_successful_test_run(&failed));
        assert!(tool_call_is_successful_test_run(&passed));
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
            "sh -lc printf NUCLEUS_COMMAND_RUN_PROBE"
        ));
        assert!(!command_is_exact_read_only_probe("pwd"));
        assert!(!command_is_exact_read_only_probe(
            "sh -lc printf WRONG_PROBE"
        ));
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
}
