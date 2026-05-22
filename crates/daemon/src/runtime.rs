use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use nucleus_core::{AdapterKind, compiled_turn_openai_messages};
use nucleus_protocol::{
    CompiledConversationTurn, CompiledPromptLayer, CompiledTurn, CompiledTurnCapabilities,
    CompiledTurnDebugSummary, McpServerSummary, NucleusToolDescriptor, RuntimeSummary,
    SessionSummary, SessionTurn, SessionTurnImage,
};
use reqwest::header::{AUTHORIZATION, HeaderMap, RETRY_AFTER};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, mpsc, watch};

const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
const RUNTIME_CACHE_TTL: Duration = Duration::from_secs(30);

use crate::retry::{
    ProviderTransportError, RetryDecision, classify_provider_error, provider_error_class,
    retry_after_from_error,
};

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
    ProviderSessionReady {
        provider_session_id: String,
    },
    AssistantChunk {
        text: String,
    },
    AssistantSnapshot {
        text: String,
    },
    ReasoningSnapshot {
        text: String,
    },
    TokenUsage {
        prompt_tokens: u64,
        completion_tokens: u64,
        cached_tokens: u64,
    },
    ProviderRetry {
        attempt: u32,
        error_class: String,
        backoff: Duration,
    },
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

        let refreshed = probe_runtimes(base_runtimes);
        *cache = Some(RuntimeCache {
            refreshed_at: Instant::now(),
            runtimes: refreshed.clone(),
        });

        Ok(refreshed)
    }

    pub async fn execute_prompt_stream_cancellable(
        &self,
        session: &SessionSummary,
        history: &[SessionTurn],
        prompt: &str,
        images: &[SessionTurnImage],
        compiler_role: &str,
        events: mpsc::UnboundedSender<PromptStreamEvent>,
        cancel_rx: Option<watch::Receiver<bool>>,
    ) -> Result<ProviderTurnResult> {
        let compiled_turn =
            compiled_turn_from_prompt(history, prompt, images, compiler_role, &[], &[], &[]);
        self.execute_compiled_turn_stream_cancellable(
            session,
            Arc::new(compiled_turn),
            events,
            cancel_rx,
        )
        .await
    }

    pub async fn execute_compiled_turn_stream(
        &self,
        session: &SessionSummary,
        compiled_turn: Arc<CompiledTurn>,
        events: mpsc::UnboundedSender<PromptStreamEvent>,
    ) -> Result<ProviderTurnResult> {
        self.execute_compiled_turn_stream_cancellable(session, compiled_turn, events, None)
            .await
    }

    pub async fn execute_compiled_turn_stream_cancellable(
        &self,
        session: &SessionSummary,
        compiled_turn: Arc<CompiledTurn>,
        events: mpsc::UnboundedSender<PromptStreamEvent>,
        cancel_rx: Option<watch::Receiver<bool>>,
    ) -> Result<ProviderTurnResult> {
        let runtime = AdapterKind::parse(&session.provider)
            .ok_or_else(|| anyhow!("unsupported provider '{}'", session.provider))?;

        match runtime {
            AdapterKind::OpenAiCompatible => {
                execute_openai_compatible_prompt(session, &compiled_turn, events, cancel_rx).await
            }
            AdapterKind::Claude | AdapterKind::Codex => bail!(
                "provider '{}' requires a protocol backend or loopback bridge; CLI model execution is disabled",
                session.provider
            ),
            AdapterKind::System => bail!(
                "provider '{}' does not support Nucleus-managed prompting yet",
                session.provider
            ),
        }
    }
}

fn probe_runtimes(base_runtimes: Vec<RuntimeSummary>) -> Vec<RuntimeSummary> {
    base_runtimes
        .into_iter()
        .map(|runtime| match AdapterKind::parse(&runtime.id) {
            Some(AdapterKind::OpenAiCompatible) => probe_openai_compatible_runtime(runtime),
            Some(AdapterKind::Claude) => probe_planned_protocol_runtime(
                runtime,
                "Claude requires a protocol backend or loopback bridge; CLI model execution is disabled.",
            ),
            Some(AdapterKind::Codex) => probe_planned_protocol_runtime(
                runtime,
                "Codex requires a protocol backend or loopback bridge; CLI model execution is disabled.",
            ),
            Some(AdapterKind::System) => probe_system_runtime(runtime),
            None => runtime,
        })
        .collect()
}

fn probe_openai_compatible_runtime(mut runtime: RuntimeSummary) -> RuntimeSummary {
    runtime.state = "ready".to_string();
    runtime.auth_state = "configured_per_target".to_string();
    runtime.executable_path.clear();
    runtime.version.clear();
    runtime.note =
        "Uses per-profile or per-route OpenAI-compatible HTTP transport settings.".to_string();
    runtime
}

fn probe_planned_protocol_runtime(mut runtime: RuntimeSummary, note: &str) -> RuntimeSummary {
    runtime.state = "planned".to_string();
    runtime.auth_state = "not_configured".to_string();
    runtime.executable_path.clear();
    runtime.version.clear();
    runtime.note = note.to_string();
    runtime
}

fn probe_system_runtime(mut runtime: RuntimeSummary) -> RuntimeSummary {
    runtime.state = "ready".to_string();
    runtime.auth_state = "not_required".to_string();
    runtime.version = env!("CARGO_PKG_VERSION").to_string();
    runtime.note = "Built into Nucleus.".to_string();
    runtime.supports_sessions = false;
    runtime.supports_prompting = false;
    runtime
}

pub(crate) fn compiled_turn_from_prompt(
    history: &[SessionTurn],
    prompt: &str,
    images: &[SessionTurnImage],
    compiler_role: &str,
    skill_layers: &[CompiledPromptLayer],
    tool_catalog: &[NucleusToolDescriptor],
    mcp_catalog: &[McpServerSummary],
) -> CompiledTurn {
    let role = match compiler_role.trim() {
        "utility" => "utility",
        _ => "main",
    };

    let compiled_history = history
        .iter()
        .filter(|turn| matches!(turn.role.as_str(), "user" | "assistant" | "system"))
        .map(|turn| CompiledConversationTurn {
            role: turn.role.clone(),
            content: turn.content.clone(),
            images: turn.images.clone(),
        })
        .collect::<Vec<_>>();

    CompiledTurn {
        id: uuid::Uuid::new_v4().to_string(),
        role: role.to_string(),
        provider_neutral: true,
        system_layers: vec![CompiledPromptLayer {
            id: "platform:nucleus-runtime".to_string(),
            kind: "platform".to_string(),
            scope: "nucleus".to_string(),
            title: "Nucleus runtime contract".to_string(),
            source_path: String::new(),
            content: "Nucleus owns prompt assembly, project context, skills, tools, and turn execution semantics. Provider-native project memory, skills, and MCP configuration are not authoritative for this turn.".to_string(),
        }],
        project_layers: Vec::new(),
        skill_layers: skill_layers.to_vec(),
        tool_catalog: tool_catalog.to_vec(),
        mcp_catalog: mcp_catalog.to_vec(),
        history: compiled_history.clone(),
        user_turn: CompiledConversationTurn {
            role: "user".to_string(),
            content: prompt.to_string(),
            images: images.to_vec(),
        },
        capabilities: CompiledTurnCapabilities {
            needs_images: !images.is_empty(),
            needs_tools: !tool_catalog.is_empty(),
            needs_mcp: !mcp_catalog.is_empty(),
        },
        debug_summary: CompiledTurnDebugSummary {
            include_count: 0,
            memory_count: 0,
            memory_included_count: 0,
            memory_skipped_count: 0,
            memory_truncated_count: 0,
            skill_count: skill_layers.len(),
            mcp_server_count: mcp_catalog.len(),
            tool_count: tool_catalog.len(),
            layer_count: skill_layers.len(),
            summary: format!(
                "Compiled {} history turns for {} provider-neutral prompt with {} skill layers, {} MCP servers, and {} tools.",
                compiled_history.len(), role, skill_layers.len(), mcp_catalog.len(), tool_catalog.len()
            ),
            skill_diagnostics: Vec::new(),
        },
    }
}

async fn execute_openai_compatible_prompt(
    session: &SessionSummary,
    compiled_turn: &CompiledTurn,
    events: mpsc::UnboundedSender<PromptStreamEvent>,
    mut cancel_rx: Option<watch::Receiver<bool>>,
) -> Result<ProviderTurnResult> {
    validate_working_directory(&session.working_dir)?;

    let base_url = session.provider_base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        bail!("OpenAI-compatible sessions require a base URL");
    }

    if session.model.trim().is_empty() {
        bail!("OpenAI-compatible sessions require a model name");
    }

    let client = reqwest::Client::builder()
        .timeout(PROMPT_TIMEOUT)
        .build()
        .context("failed to build OpenAI-compatible HTTP client")?;

    let mut payload = json!({
        "model": session.model,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": compiled_turn_openai_messages(compiled_turn),
    });
    if compiled_turn_requires_json_object(compiled_turn) {
        payload["response_format"] = json!({ "type": "json_object" });
    }

    let mut attempt = 0_u32;
    loop {
        if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            bail!("worker canceled before provider call");
        }

        match execute_openai_compatible_prompt_once(session, base_url, &client, &payload, &events)
            .await
        {
            Ok(result) => return Ok(result),
            Err(error) => {
                attempt = attempt.saturating_add(1);
                let retry_after = retry_after_from_error(&error);
                match classify_provider_error(&error, attempt, retry_after) {
                    RetryDecision::Retry { backoff } => {
                        let _ = events.send(PromptStreamEvent::ProviderRetry {
                            attempt,
                            error_class: provider_error_class(&error),
                            backoff,
                        });
                        wait_for_retry_backoff(backoff, cancel_rx.as_mut()).await?;
                    }
                    RetryDecision::GiveUp { reason } => {
                        let detail = error.to_string();
                        return Err(error).with_context(|| {
                            format!("provider retry policy gave up: {reason}; {detail}")
                        });
                    }
                }
            }
        }
    }
}

async fn execute_openai_compatible_prompt_once(
    session: &SessionSummary,
    base_url: &str,
    client: &reqwest::Client,
    payload: &serde_json::Value,
    events: &mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<ProviderTurnResult> {
    // The OpenAI-compatible adapter does not expose a portable idempotency key
    // today. If a future provider does, retries should reuse the same key here.
    let mut request = client
        .post(format!("{base_url}/chat/completions"))
        .json(payload);

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
        let retry_after = retry_after_from_headers(response.headers());
        let body = response.text().await.unwrap_or_default();
        let detail = if body.trim().is_empty() {
            format!("HTTP {}", status.as_u16())
        } else {
            truncate(body, 200)
        };
        return Err(anyhow!(ProviderTransportError::Http {
            status: status.as_u16(),
            detail,
            retry_after,
        }));
    }

    read_openai_compatible_stream(response, events.clone()).await
}

async fn wait_for_retry_backoff(
    backoff: Duration,
    cancel_rx: Option<&mut watch::Receiver<bool>>,
) -> Result<()> {
    let Some(cancel_rx) = cancel_rx else {
        tokio::time::sleep(backoff).await;
        return Ok(());
    };

    if *cancel_rx.borrow() {
        bail!("worker canceled during provider retry backoff");
    }

    tokio::select! {
        _ = tokio::time::sleep(backoff) => Ok(()),
        changed = cancel_rx.changed() => {
            if changed.is_ok() && *cancel_rx.borrow() {
                bail!("worker canceled during provider retry backoff");
            }
            Ok(())
        }
    }
}

async fn read_openai_compatible_stream(
    response: reqwest::Response,
    events: mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<ProviderTurnResult> {
    let mut provider_session_id = String::new();
    let mut content = String::new();
    let mut pending = String::new();
    let mut saw_done = false;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("failed while reading the response stream")?;
        pending.push_str(
            std::str::from_utf8(&bytes).context("OpenAI-compatible stream was not valid UTF-8")?,
        );

        while let Some(index) = pending.find('\n') {
            let line = pending[..index].trim().trim_end_matches('\r').to_string();
            pending = pending[index + 1..].to_string();
            if handle_openai_compatible_line(
                &line,
                &mut provider_session_id,
                &mut content,
                &events,
            )? {
                saw_done = true;
            }
        }
    }

    if !pending.trim().is_empty() {
        if handle_openai_compatible_line(
            pending.trim(),
            &mut provider_session_id,
            &mut content,
            &events,
        )? {
            saw_done = true;
        }
    }

    let content = content.trim().to_string();
    if !saw_done && content.is_empty() {
        return Err(anyhow!(ProviderTransportError::Stream {
            detail: "EOF before stream complete".to_string(),
        }));
    }
    if content.is_empty() {
        bail!("OpenAI-compatible endpoint returned an empty response.");
    }

    Ok(ProviderTurnResult {
        provider_session_id,
        content,
    })
}

fn handle_openai_compatible_line(
    line: &str,
    provider_session_id: &mut String,
    content: &mut String,
    events: &mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<bool> {
    if line.is_empty() || !line.starts_with("data:") {
        return Ok(false);
    }

    let payload = line["data:".len()..].trim();
    if payload == "[DONE]" {
        return Ok(true);
    }

    let chunk = serde_json::from_str::<OpenAiStreamChunk>(payload)
        .with_context(|| "failed to decode OpenAI-compatible stream chunk".to_string())?;

    if provider_session_id.is_empty() {
        *provider_session_id = chunk.id.clone().unwrap_or_default();
        if !provider_session_id.is_empty() {
            let _ = events.send(PromptStreamEvent::ProviderSessionReady {
                provider_session_id: provider_session_id.clone(),
            });
        }
    }

    if let Some(usage) = chunk.usage {
        let _ = events.send(PromptStreamEvent::TokenUsage {
            prompt_tokens: usage.prompt_tokens.unwrap_or(0),
            completion_tokens: usage.completion_tokens.unwrap_or(0),
            cached_tokens: usage
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens)
                .unwrap_or(0),
        });
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
            let _ = events.send(PromptStreamEvent::AssistantSnapshot {
                text: content.clone(),
            });
        }
    }

    Ok(false)
}

fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    let value = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)?;

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    httpdate::parse_http_date(value).ok().map(|deadline| {
        deadline
            .duration_since(SystemTime::now())
            .unwrap_or_default()
    })
}

fn compiled_turn_requires_json_object(compiled_turn: &CompiledTurn) -> bool {
    compiled_turn.history.iter().any(|turn| {
        turn.role == "system"
            && turn
                .content
                .contains("Return exactly one JSON object and nothing else.")
    })
}

fn validate_working_directory(path: &str) -> Result<()> {
    let path = Path::new(path);

    if !path.is_dir() {
        bail!("working directory '{}' is not available", path.display());
    }

    Ok(())
}

fn truncate(value: impl AsRef<str>, max_chars: usize) -> String {
    let value = value.as_ref();
    let mut result = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            result.push_str("...");
            return result;
        }
        result.push(ch);
    }
    result
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    id: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiPromptTokensDetails {
    cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    #[serde(default)]
    delta: OpenAiDelta,
    message: Option<OpenAiMessage>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
    reasoning: Option<String>,
    reasoning_content: Option<String>,
}

impl OpenAiDelta {
    fn reasoning_text(&self) -> Option<String> {
        self.reasoning
            .as_ref()
            .or(self.reasoning_content.as_ref())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_turn_preserves_requested_compiler_role() {
        let main = compiled_turn_from_prompt(&[], "Summarize.", &[], "main", &[], &[], &[]);
        let utility = compiled_turn_from_prompt(&[], "Summarize.", &[], "utility", &[], &[], &[]);
        let fallback =
            compiled_turn_from_prompt(&[], "Summarize.", &[], "unexpected", &[], &[], &[]);

        assert_eq!(main.role, "main");
        assert_eq!(utility.role, "utility");
        assert_eq!(fallback.role, "main");
    }

    #[test]
    fn openai_worker_turns_request_json_object_mode() {
        let history = vec![SessionTurn {
            id: "system".to_string(),
            session_id: "job".to_string(),
            role: "system".to_string(),
            content: "Return exactly one JSON object and nothing else.".to_string(),
            images: Vec::new(),
            created_at: 0,
        }];
        let compiled = compiled_turn_from_prompt(
            &history,
            "Decide the next step.",
            &[],
            "main",
            &[],
            &[],
            &[],
        );

        assert!(compiled_turn_requires_json_object(&compiled));
    }

    #[test]
    fn openai_regular_turns_do_not_request_json_object_mode() {
        let history = vec![SessionTurn {
            id: "user".to_string(),
            session_id: "session".to_string(),
            role: "user".to_string(),
            content: "Return exactly one JSON object and nothing else.".to_string(),
            images: Vec::new(),
            created_at: 0,
        }];
        let compiled =
            compiled_turn_from_prompt(&history, "Summarize.", &[], "main", &[], &[], &[]);

        assert!(!compiled_turn_requires_json_object(&compiled));
    }

    #[test]
    fn retry_after_header_accepts_delta_seconds_and_http_date() {
        let mut delta_headers = HeaderMap::new();
        delta_headers.insert(RETRY_AFTER, "5".parse().unwrap());
        assert_eq!(
            retry_after_from_headers(&delta_headers),
            Some(Duration::from_secs(5))
        );

        let deadline = SystemTime::now() + Duration::from_secs(60);
        let mut date_headers = HeaderMap::new();
        date_headers.insert(
            RETRY_AFTER,
            httpdate::fmt_http_date(deadline).parse().unwrap(),
        );
        let parsed =
            retry_after_from_headers(&date_headers).expect("HTTP-date Retry-After should parse");
        assert!(
            parsed > Duration::from_secs(55) && parsed <= Duration::from_secs(60),
            "expected roughly 60s retry-after, got {parsed:?}"
        );
    }

    #[test]
    fn openai_stream_usage_block_emits_token_usage() {
        let (events, mut receiver) = mpsc::unbounded_channel();
        let mut provider_session_id = String::new();
        let mut content = String::new();
        let done = handle_openai_compatible_line(
            r#"data: {"id":"chatcmpl-test","choices":[],"usage":{"prompt_tokens":120,"completion_tokens":45,"prompt_tokens_details":{"cached_tokens":30}}}"#,
            &mut provider_session_id,
            &mut content,
            &events,
        )
        .expect("usage chunk should decode");

        assert!(!done);
        assert_eq!(provider_session_id, "chatcmpl-test");
        let mut saw_usage = false;
        while let Ok(event) = receiver.try_recv() {
            if let PromptStreamEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
                cached_tokens,
            } = event
            {
                saw_usage = true;
                assert_eq!(prompt_tokens, 120);
                assert_eq!(completion_tokens, 45);
                assert_eq!(cached_tokens, 30);
            }
        }
        assert!(saw_usage);
    }
}
