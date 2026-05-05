use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::StreamExt;
use nucleus_core::AdapterKind;
use nucleus_protocol::{RuntimeSummary, SessionSummary, SessionTurn, SessionTurnImage};
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::{Mutex, mpsc},
    time::{Duration, Instant, timeout},
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
const RUNTIME_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct RuntimeManager {
    cache: Mutex<Option<RuntimeCache>>,
}

struct RuntimeCache {
    refreshed_at: Instant,
    runtimes: Vec<RuntimeSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTurnResult {
    pub provider_session_id: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptStreamEvent {
    ProviderSessionReady { provider_session_id: String },
    AssistantChunk { text: String },
    AssistantSnapshot { text: String },
    ReasoningSnapshot { text: String },
}

#[derive(Debug)]
struct PreparedPromptImages {
    file_paths: Vec<PathBuf>,
    temp_dir: Option<PathBuf>,
}

impl RuntimeManager {
    pub async fn list_runtimes(
        &self,
        base_runtimes: Vec<RuntimeSummary>,
        force_refresh: bool,
    ) -> Result<Vec<RuntimeSummary>> {
        let mut cache = self.cache.lock().await;

        if !force_refresh {
            if let Some(existing) = cache.as_ref() {
                if existing.refreshed_at.elapsed() < RUNTIME_CACHE_TTL {
                    return Ok(existing.runtimes.clone());
                }
            }
        }

        let refreshed = probe_runtimes(base_runtimes).await;
        *cache = Some(RuntimeCache {
            refreshed_at: Instant::now(),
            runtimes: refreshed.clone(),
        });

        Ok(refreshed)
    }

    pub async fn execute_prompt_stream(
        &self,
        session: &SessionSummary,
        history: &[SessionTurn],
        prompt: &str,
        images: &[SessionTurnImage],
        events: mpsc::UnboundedSender<PromptStreamEvent>,
    ) -> Result<ProviderTurnResult> {
        let runtime = AdapterKind::parse(&session.provider)
            .ok_or_else(|| anyhow!("unsupported provider '{}'", session.provider))?;

        match runtime {
            AdapterKind::Claude => execute_claude_prompt(session, prompt, images, events).await,
            AdapterKind::Codex => execute_codex_prompt(session, prompt, images, events).await,
            AdapterKind::OpenAiCompatible => {
                execute_openai_compatible_prompt(session, history, prompt, images, events).await
            }
            AdapterKind::System => bail!(
                "provider '{}' does not support daemon-managed prompting yet",
                session.provider
            ),
        }
    }
}

#[cfg(test)]
impl RuntimeManager {
    pub async fn seed_cache_for_test(&self, runtimes: Vec<RuntimeSummary>) {
        let mut cache = self.cache.lock().await;
        *cache = Some(RuntimeCache {
            refreshed_at: Instant::now(),
            runtimes,
        });
    }
}

async fn probe_runtimes(base_runtimes: Vec<RuntimeSummary>) -> Vec<RuntimeSummary> {
    let mut items = Vec::with_capacity(base_runtimes.len());

    for runtime in base_runtimes {
        let next = match AdapterKind::parse(&runtime.id) {
            Some(AdapterKind::Claude) => probe_claude_runtime(runtime).await,
            Some(AdapterKind::Codex) => probe_codex_runtime(runtime).await,
            Some(AdapterKind::OpenAiCompatible) => runtime,
            Some(AdapterKind::System) => probe_system_runtime(runtime),
            None => runtime,
        };

        items.push(next);
    }

    items
}

fn probe_system_runtime(mut runtime: RuntimeSummary) -> RuntimeSummary {
    runtime.state = "ready".to_string();
    runtime.auth_state = "not_required".to_string();
    runtime.version = env!("CARGO_PKG_VERSION").to_string();
    runtime.note = "Built into the Nucleus daemon.".to_string();
    runtime.supports_sessions = false;
    runtime.supports_prompting = false;
    runtime
}

async fn probe_claude_runtime(mut runtime: RuntimeSummary) -> RuntimeSummary {
    let Some(path) = which("claude").await else {
        runtime.state = "unavailable".to_string();
        runtime.auth_state = "missing".to_string();
        runtime.note = "Claude CLI is not installed on this machine.".to_string();
        return runtime;
    };

    runtime.executable_path = path.clone();
    runtime.version = command_stdout("claude", &["-v"], None, PROBE_TIMEOUT)
        .await
        .unwrap_or_default();

    match claude_auth_state().await {
        Ok(status) if status.logged_in => {
            runtime.state = "ready".to_string();
            runtime.auth_state = "ready".to_string();
            runtime.note = format!("Authenticated via {}.", status.auth_method);
        }
        Ok(_) => {
            runtime.state = "auth_required".to_string();
            runtime.auth_state = "missing".to_string();
            runtime.note = "Claude CLI is installed but not authenticated.".to_string();
        }
        Err(error) => {
            runtime.state = "degraded".to_string();
            runtime.auth_state = "unknown".to_string();
            runtime.note = truncate(error.to_string(), 160);
        }
    }

    runtime
}

async fn probe_codex_runtime(mut runtime: RuntimeSummary) -> RuntimeSummary {
    let Some(path) = which("codex").await else {
        runtime.state = "unavailable".to_string();
        runtime.auth_state = "missing".to_string();
        runtime.note = "Codex CLI is not installed on this machine.".to_string();
        return runtime;
    };

    runtime.executable_path = path.clone();
    runtime.version = command_stdout("codex", &["-V"], None, PROBE_TIMEOUT)
        .await
        .unwrap_or_default();

    match codex_auth_state().await {
        Ok(status) => {
            runtime.state = "ready".to_string();
            runtime.auth_state = "ready".to_string();
            runtime.note =
                format!("{status}. Leave the model blank to use the local Codex default.");
        }
        Err(error) => {
            runtime.state = "auth_required".to_string();
            runtime.auth_state = "missing".to_string();
            runtime.note = truncate(error.to_string(), 160);
        }
    }

    runtime
}

async fn execute_claude_prompt(
    session: &SessionSummary,
    prompt: &str,
    images: &[SessionTurnImage],
    events: mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<ProviderTurnResult> {
    validate_working_directory(&session.working_dir)?;

    if !images.is_empty() {
        bail!("Claude sessions cannot accept image attachments from Nucleus yet");
    }

    let mut args = vec![
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ];

    if !session.provider_session_id.is_empty() {
        args.push("--resume");
        args.push(session.provider_session_id.as_str());
    }

    if !session.model.is_empty() {
        args.push("--model");
        args.push(session.model.as_str());
    }

    args.push(prompt);

    let mut provider_session_id = String::new();
    let mut content = String::new();

    run_json_stream_command(
        "claude",
        &args,
        Some(&session.working_dir),
        PROMPT_TIMEOUT,
        |line| {
            let payload = serde_json::from_str::<Value>(line)
                .with_context(|| format!("failed to decode Claude JSONL line: {line}"))?;

            match payload.get("type").and_then(Value::as_str) {
                Some("system") => {
                    if payload.get("subtype").and_then(Value::as_str) == Some("init") {
                        if let Some(next_session_id) =
                            payload.get("session_id").and_then(Value::as_str)
                        {
                            provider_session_id = next_session_id.to_string();
                            let _ = events.send(PromptStreamEvent::ProviderSessionReady {
                                provider_session_id: provider_session_id.clone(),
                            });
                        }
                    }
                }
                Some("stream_event") => {
                    if let Some(delta) = extract_claude_text_delta(&payload) {
                        content.push_str(&delta);
                        let _ = events.send(PromptStreamEvent::AssistantChunk { text: delta });
                    }
                }
                Some("assistant") => {
                    if let Some(next_session_id) = payload.get("session_id").and_then(Value::as_str)
                    {
                        provider_session_id = next_session_id.to_string();
                    }

                    if let Some(snapshot) = extract_claude_assistant_text(&payload) {
                        content = snapshot.clone();
                        let _ =
                            events.send(PromptStreamEvent::AssistantSnapshot { text: snapshot });
                    }
                }
                Some("result") => {
                    if let Some(next_session_id) = payload.get("session_id").and_then(Value::as_str)
                    {
                        provider_session_id = next_session_id.to_string();
                    }

                    if payload
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        let failure = payload
                            .get("result")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .unwrap_or("Claude reported an unknown error.");
                        bail!(failure.to_string());
                    }

                    if content.trim().is_empty() {
                        if let Some(snapshot) = payload
                            .get("result")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(ToOwned::to_owned)
                        {
                            content = snapshot.clone();
                            let _ = events
                                .send(PromptStreamEvent::AssistantSnapshot { text: snapshot });
                        }
                    }
                }
                _ => {}
            }

            Ok(())
        },
    )
    .await?;

    let content = content.trim().to_string().chars().collect::<String>();

    if content.is_empty() {
        bail!("Claude returned an empty response.");
    }

    Ok(ProviderTurnResult {
        provider_session_id,
        content,
    })
}

async fn execute_codex_prompt(
    session: &SessionSummary,
    prompt: &str,
    images: &[SessionTurnImage],
    events: mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<ProviderTurnResult> {
    validate_working_directory(&session.working_dir)?;
    let prepared_images =
        prepare_prompt_images(images).context("failed to prepare image attachments for Codex")?;
    let mut args: Vec<String> = vec!["exec".to_string()];

    if session.provider_session_id.is_empty() {
        args.push("--json".to_string());
        args.push("--skip-git-repo-check".to_string());

        if !session.model.is_empty() {
            args.push("-m".to_string());
            args.push(session.model.clone());
        }

        for path in &prepared_images.file_paths {
            args.push("--image".to_string());
            args.push(path.display().to_string());
        }

        args.push("--".to_string());
        args.push(prompt.to_string());
    } else {
        args.push("resume".to_string());
        args.push("--json".to_string());
        args.push("--skip-git-repo-check".to_string());

        if !session.model.is_empty() {
            args.push("-m".to_string());
            args.push(session.model.clone());
        }

        for path in &prepared_images.file_paths {
            args.push("--image".to_string());
            args.push(path.display().to_string());
        }

        args.push("--".to_string());
        args.push(session.provider_session_id.clone());
        args.push(prompt.to_string());
    }

    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let result =
        execute_codex_prompt_stream(session, &arg_refs, &prepared_images, events, PROMPT_TIMEOUT)
            .await;
    cleanup_prepared_images(prepared_images);
    result
}

async fn execute_codex_prompt_stream(
    session: &SessionSummary,
    args: &[&str],
    _prepared_images: &PreparedPromptImages,
    events: mpsc::UnboundedSender<PromptStreamEvent>,
    timeout_window: Duration,
) -> Result<ProviderTurnResult> {
    let mut provider_session_id = String::new();
    let mut content = String::new();

    run_json_stream_command(
        "codex",
        args,
        Some(&session.working_dir),
        timeout_window,
        |line| {
            let Ok(payload) = serde_json::from_str::<Value>(line) else {
                return Ok(());
            };

            match payload.get("type").and_then(Value::as_str) {
                Some("thread.started") => {
                    if let Some(thread_id) = payload.get("thread_id").and_then(Value::as_str) {
                        provider_session_id = thread_id.to_string();
                        let _ = events.send(PromptStreamEvent::ProviderSessionReady {
                            provider_session_id: provider_session_id.clone(),
                        });
                    }
                }
                Some("item.completed") => {
                    if let Some(item) = payload.get("item") {
                        match item.get("type").and_then(Value::as_str) {
                            Some("reasoning") => {
                                if let Some(text) = item
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|value| !value.is_empty())
                                {
                                    let _ = events.send(PromptStreamEvent::ReasoningSnapshot {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            Some("agent_message") => {
                                if let Some(text) = item.get("text").and_then(Value::as_str) {
                                    content = text.trim().to_string();
                                    let _ = events.send(PromptStreamEvent::AssistantSnapshot {
                                        text: content.clone(),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }

            Ok(())
        },
    )
    .await?;

    if content.is_empty() {
        bail!("Codex returned an empty response.");
    }

    Ok(ProviderTurnResult {
        provider_session_id,
        content,
    })
}

async fn execute_openai_compatible_prompt(
    session: &SessionSummary,
    history: &[SessionTurn],
    prompt: &str,
    images: &[SessionTurnImage],
    events: mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<ProviderTurnResult> {
    validate_working_directory(&session.working_dir)?;

    let base_url = session.provider_base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        bail!("OpenAI-compatible sessions require a base URL");
    }

    if session.model.trim().is_empty() {
        bail!("OpenAI-compatible sessions require a model name");
    }

    let url = format!("{base_url}/chat/completions");
    let client = reqwest::Client::builder()
        .timeout(PROMPT_TIMEOUT)
        .build()
        .context("failed to build OpenAI-compatible HTTP client")?;

    let mut request = client.post(url).json(&json!({
        "model": session.model,
        "stream": true,
        "messages": build_openai_messages(history, prompt, images),
    }));

    if !session.provider_api_key.trim().is_empty() {
        request = request.header(
            AUTHORIZATION,
            format!("Bearer {}", session.provider_api_key.trim()),
        );
    }

    let response = request
        .send()
        .await
        .context("failed to reach the OpenAI-compatible endpoint")?;
    let status = response.status();

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = if body.trim().is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            truncate(body, 200)
        };
        bail!("OpenAI-compatible endpoint failed: {detail}");
    }

    let mut provider_session_id = String::new();
    let mut content = String::new();
    let mut pending = String::new();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("failed while reading the response stream")?;
        pending.push_str(
            std::str::from_utf8(&bytes).context("OpenAI-compatible stream was not valid UTF-8")?,
        );

        while let Some(index) = pending.find('\n') {
            let line = pending[..index].trim().trim_end_matches('\r').to_string();
            pending = pending[index + 1..].to_string();

            if line.is_empty() || !line.starts_with("data:") {
                continue;
            }

            let payload = line["data:".len()..].trim();
            if payload == "[DONE]" {
                continue;
            }

            let chunk = serde_json::from_str::<OpenAiStreamChunk>(payload)
                .with_context(|| "failed to decode OpenAI-compatible stream chunk".to_string())?;

            if provider_session_id.is_empty() {
                provider_session_id = chunk.id.clone().unwrap_or_default();
            }

            for choice in chunk.choices {
                if let Some(reasoning) = choice.delta.reasoning_text() {
                    let _ = events.send(PromptStreamEvent::ReasoningSnapshot { text: reasoning });
                }

                if let Some(delta) = choice
                    .delta
                    .content
                    .or(choice.message.and_then(|m| m.content))
                {
                    content.push_str(&delta);
                    let _ = events.send(PromptStreamEvent::AssistantChunk { text: delta });
                }
            }
        }
    }

    if !pending.trim().is_empty() {
        let payload = pending.trim().trim_start_matches("data:").trim();
        if payload != "[DONE]" {
            let chunk = serde_json::from_str::<OpenAiStreamChunk>(payload)
                .with_context(|| "failed to decode trailing OpenAI-compatible chunk".to_string())?;
            if provider_session_id.is_empty() {
                provider_session_id = chunk.id.unwrap_or_default();
            }
            for choice in chunk.choices {
                if let Some(reasoning) = choice.delta.reasoning_text() {
                    let _ = events.send(PromptStreamEvent::ReasoningSnapshot { text: reasoning });
                }
                if let Some(delta) = choice
                    .delta
                    .content
                    .or(choice.message.and_then(|m| m.content))
                {
                    content.push_str(&delta);
                    let _ = events.send(PromptStreamEvent::AssistantChunk { text: delta });
                }
            }
        }
    }

    let content = content.trim().to_string();
    if content.is_empty() {
        bail!("OpenAI-compatible endpoint returned an empty response.");
    }

    Ok(ProviderTurnResult {
        provider_session_id,
        content,
    })
}

fn build_openai_messages(
    history: &[SessionTurn],
    prompt: &str,
    images: &[SessionTurnImage],
) -> Vec<Value> {
    let mut messages = Vec::new();

    for turn in history {
        if !matches!(turn.role.as_str(), "user" | "assistant" | "system") {
            continue;
        }

        messages.push(json!({
            "role": turn.role,
            "content": openai_message_content(&turn.content, &turn.images),
        }));
    }

    messages.push(json!({
        "role": "user",
        "content": openai_message_content(prompt, images),
    }));

    messages
}

fn openai_message_content(text: &str, images: &[SessionTurnImage]) -> Value {
    if images.is_empty() {
        return Value::String(text.to_string());
    }

    let caption = if text.trim().is_empty() {
        if images.len() == 1 {
            "Review the attached image and respond with the most useful analysis.".to_string()
        } else {
            format!(
                "Review the {} attached images and respond with the most useful analysis.",
                images.len()
            )
        }
    } else {
        text.to_string()
    };

    let mut parts = vec![json!({
        "type": "text",
        "text": caption,
    })];

    for image in images {
        parts.push(json!({
            "type": "image_url",
            "image_url": {
                "url": image.data_url,
            },
        }));
    }

    Value::Array(parts)
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    #[serde(default)]
    delta: OpenAiDelta,
    #[serde(default)]
    message: Option<OpenAiMessage>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

impl OpenAiDelta {
    fn reasoning_text(&self) -> Option<String> {
        self.reasoning
            .as_deref()
            .or(self.reasoning_content.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    #[serde(default)]
    content: Option<String>,
}

async fn claude_auth_state() -> Result<ClaudeAuthStatus> {
    let stdout = command_stdout("claude", &["auth", "status"], None, PROBE_TIMEOUT).await?;
    serde_json::from_str::<ClaudeAuthStatus>(&stdout).context("failed to decode Claude auth status")
}

async fn codex_auth_state() -> Result<String> {
    let output = run_command("codex", &["login", "status"], None, PROBE_TIMEOUT).await?;
    let trimmed = if output.stdout.trim().is_empty() {
        output.stderr.trim()
    } else {
        output.stdout.trim()
    };

    if trimmed.is_empty() {
        bail!("Codex login status returned an empty result");
    }

    Ok(trimmed.to_string())
}

async fn which(command: &str) -> Option<String> {
    resolve_command_path(command).map(|path| path.display().to_string())
}

async fn command_stdout(
    command: &str,
    args: &[&str],
    cwd: Option<&str>,
    timeout_window: Duration,
) -> Result<String> {
    let output = run_command(command, args, cwd, timeout_window).await?;
    Ok(output.stdout)
}

async fn run_command(
    command: &str,
    args: &[&str],
    cwd: Option<&str>,
    timeout_window: Duration,
) -> Result<CommandOutput> {
    let resolved_command = resolve_command_path(command).unwrap_or_else(|| PathBuf::from(command));
    let mut child = Command::new(&resolved_command);
    child
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(path) = augmented_path_env() {
        child.env("PATH", path);
    }

    if let Some(path) = cwd {
        child.current_dir(path);
    }

    let child = child
        .spawn()
        .with_context(|| format!("failed to start '{command}'"))?;

    let output = match timeout(timeout_window, child.wait_with_output()).await {
        Ok(result) => result.with_context(|| format!("'{command}' failed to execute"))?,
        Err(_) => bail!(
            "'{command}' timed out after {} seconds",
            timeout_window.as_secs()
        ),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        let detail = if stderr.is_empty() {
            stdout.clone()
        } else {
            stderr.clone()
        };
        bail!(
            "'{command}' exited with {}{}",
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {}", truncate(detail, 200))
            }
        );
    }

    Ok(CommandOutput { stdout, stderr })
}

fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(command);
    if candidate.is_absolute() || command.contains(std::path::MAIN_SEPARATOR) {
        return is_executable_file(&candidate).then_some(candidate);
    }

    runtime_search_paths().into_iter().find_map(|directory| {
        let candidate = directory.join(command);
        is_executable_file(&candidate).then_some(candidate)
    })
}

fn augmented_path_env() -> Option<std::ffi::OsString> {
    let paths = runtime_search_paths();
    if paths.is_empty() {
        return None;
    }

    env::join_paths(paths).ok()
}

fn runtime_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();

    if let Some(current) = env::var_os("PATH") {
        for path in env::split_paths(&current) {
            push_unique_path(&mut paths, &mut seen, path);
        }
    }

    if let Some(home) = dirs::home_dir() {
        for suffix in [
            ".local/bin",
            ".cargo/bin",
            ".bun/bin",
            ".asdf/shims",
            ".npm-global/bin",
            ".volta/bin",
            "bin",
        ] {
            push_unique_path(&mut paths, &mut seen, home.join(suffix));
        }

        let nvm_root = home.join(".nvm/versions/node");
        let mut node_bins = fs::read_dir(&nvm_root)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.filter_map(Result::ok))
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .map(|path| path.join("bin"))
            .collect::<Vec<_>>();
        node_bins.sort();
        node_bins.reverse();

        for path in node_bins {
            push_unique_path(&mut paths, &mut seen, path);
        }
    }

    paths
}

fn push_unique_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() || !path.is_dir() {
        return;
    }

    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

async fn run_json_stream_command<F>(
    command: &str,
    args: &[&str],
    cwd: Option<&str>,
    timeout_window: Duration,
    mut on_stdout_line: F,
) -> Result<CommandOutput>
where
    F: FnMut(&str) -> Result<()>,
{
    let mut child = Command::new(command);
    child
        .kill_on_drop(true)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(path) = cwd {
        child.current_dir(path);
    }

    let mut child = child
        .spawn()
        .with_context(|| format!("failed to start '{command}'"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout for '{command}'"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr for '{command}'"))?;
    let stderr_task = tokio::spawn(read_pipe_to_string(stderr));

    let mut stdout_capture = String::new();
    let mut stdout_reader = BufReader::new(stdout).lines();
    let run = async {
        while let Some(line) = stdout_reader.next_line().await? {
            let trimmed = line.trim();

            if trimmed.is_empty() {
                continue;
            }

            if !stdout_capture.is_empty() {
                stdout_capture.push('\n');
            }
            stdout_capture.push_str(trimmed);
            on_stdout_line(trimmed)?;
        }

        child
            .wait()
            .await
            .with_context(|| format!("'{command}' failed to execute"))
    };

    let status = match timeout(timeout_window, run).await {
        Ok(result) => result?,
        Err(_) => bail!(
            "'{command}' timed out after {} seconds",
            timeout_window.as_secs()
        ),
    };
    let stderr = stderr_task
        .await
        .context("failed to join stderr reader task")??;

    if !status.success() {
        let detail = if stderr.is_empty() {
            stdout_capture.clone()
        } else {
            stderr.clone()
        };
        bail!(
            "'{command}' exited with {}{}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {}", truncate(detail, 200))
            }
        );
    }

    Ok(CommandOutput {
        stdout: stdout_capture,
        stderr,
    })
}

#[cfg(test)]
fn parse_codex_output(stdout: &str) -> Result<ProviderTurnResult> {
    let mut provider_session_id = String::new();
    let mut content = String::new();

    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(payload) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        match payload.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                if let Some(thread_id) = payload.get("thread_id").and_then(Value::as_str) {
                    provider_session_id = thread_id.to_string();
                }
            }
            Some("item.completed") => {
                if let Some(item) = payload.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            content = text.trim().to_string();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if content.is_empty() {
        bail!("Codex returned an empty response.");
    }

    Ok(ProviderTurnResult {
        provider_session_id,
        content,
    })
}

fn validate_working_directory(path: &str) -> Result<()> {
    if path.is_empty() {
        bail!("session working directory is required");
    }

    if !Path::new(path).is_dir() {
        bail!("working directory '{path}' does not exist");
    }

    Ok(())
}

fn prepare_prompt_images(images: &[SessionTurnImage]) -> Result<PreparedPromptImages> {
    if images.is_empty() {
        return Ok(PreparedPromptImages {
            file_paths: Vec::new(),
            temp_dir: None,
        });
    }

    let temp_dir = std::env::temp_dir()
        .join("nucleus")
        .join("prompt-images")
        .join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create '{}'", temp_dir.display()))?;

    let mut file_paths = Vec::with_capacity(images.len());

    for (index, image) in images.iter().enumerate() {
        let bytes = decode_data_url(&image.data_url)?;
        let extension = image_extension(&image.mime_type);
        let file_name = format!("image-{}.{}", index + 1, extension);
        let path = temp_dir.join(file_name);
        fs::write(&path, bytes).with_context(|| format!("failed to write '{}'", path.display()))?;
        file_paths.push(path);
    }

    Ok(PreparedPromptImages {
        file_paths,
        temp_dir: Some(temp_dir),
    })
}

fn cleanup_prepared_images(images: PreparedPromptImages) {
    if let Some(path) = images.temp_dir {
        let _ = fs::remove_dir_all(path);
    }
}

fn decode_data_url(data_url: &str) -> Result<Vec<u8>> {
    let (metadata, payload) = data_url
        .split_once(',')
        .ok_or_else(|| anyhow!("invalid data URL"))?;

    if !metadata.starts_with("data:") {
        bail!("invalid data URL");
    }

    if !metadata.ends_with(";base64") {
        bail!("only base64 data URLs are supported");
    }

    BASE64
        .decode(payload)
        .context("failed to decode image payload")
}

fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

fn truncate(value: String, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();

    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

async fn read_pipe_to_string<T>(pipe: T) -> Result<String>
where
    T: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(pipe).lines();
    let mut output = String::new();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(trimmed);
    }

    Ok(output)
}

fn extract_claude_text_delta(payload: &Value) -> Option<String> {
    let event = payload.get("event")?;
    match event.get("type").and_then(Value::as_str) {
        Some("content_block_start") => event
            .get("content_block")
            .and_then(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    block.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        Some("content_block_delta") => event
            .get("delta")
            .and_then(|delta| {
                if delta.get("type").and_then(Value::as_str) == Some("text_delta") {
                    delta.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn extract_claude_assistant_text(payload: &Value) -> Option<String> {
    let message = payload.get("message")?;
    let content = message.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                item.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect::<String>()
        .trim()
        .to_string();

    if text.is_empty() { None } else { Some(text) }
}

#[derive(Debug, Deserialize)]
struct ClaudeAuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
    #[serde(rename = "authMethod")]
    auth_method: String,
}

struct CommandOutput {
    stdout: String,
    stderr: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parses_codex_jsonl_output() {
        let payload = r#"{"type":"thread.started","thread_id":"abc-123"}
{"type":"item.completed","item":{"type":"agent_message","text":"Hello there"}}
{"type":"turn.completed"}"#;

        let parsed = parse_codex_output(payload).expect("codex output should parse");
        assert_eq!(parsed.provider_session_id, "abc-123");
        assert_eq!(parsed.content, "Hello there");
    }

    #[test]
    fn rejects_empty_codex_output() {
        let error = parse_codex_output("{}").expect_err("missing message should fail");
        assert!(error.to_string().contains("empty response"));
    }

    #[tokio::test]
    async fn command_stdout_uses_augmented_home_paths() {
        let _env_lock = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let root = test_dir("runtime-path");
        let home = root.join("home");
        let local_bin = home.join(".local/bin");
        let script = local_bin.join("mocktool");

        fs::create_dir_all(&local_bin).expect("local bin should exist");
        fs::write(&script, "#!/bin/sh\necho nucleus-runtime-ok\n").expect("mock tool should write");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script)
                .expect("script metadata should load")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).expect("script should be executable");
        }

        let original_home = env::var_os("HOME");
        let original_path = env::var_os("PATH");
        unsafe {
            env::set_var("HOME", &home);
            env::set_var("PATH", "/usr/bin:/bin");
        }

        let output = command_stdout("mocktool", &[], None, PROBE_TIMEOUT)
            .await
            .expect("augmented runtime path should find mocktool");

        match original_home {
            Some(value) => unsafe {
                env::set_var("HOME", value);
            },
            None => unsafe {
                env::remove_var("HOME");
            },
        }
        match original_path {
            Some(value) => unsafe {
                env::set_var("PATH", value);
            },
            None => unsafe {
                env::remove_var("PATH");
            },
        }

        assert_eq!(output.trim(), "nucleus-runtime-ok");

        let _ = fs::remove_dir_all(&root);
    }

    fn test_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        env::temp_dir().join(format!(
            "nucleus-runtime-{label}-{}-{suffix}",
            std::process::id()
        ))
    }
}
