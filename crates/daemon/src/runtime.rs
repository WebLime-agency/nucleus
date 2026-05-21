use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use nucleus_core::{AdapterKind, compiled_turn_openai_messages};
use nucleus_protocol::{
    CompiledConversationTurn, CompiledPromptLayer, CompiledTurn, CompiledTurnCapabilities,
    CompiledTurnDebugSummary, McpServerSummary, NucleusToolDescriptor, RuntimeSummary,
    SessionSummary, SessionTurn, SessionTurnImage,
};
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, mpsc};

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

    pub async fn execute_prompt_stream(
        &self,
        session: &SessionSummary,
        history: &[SessionTurn],
        prompt: &str,
        images: &[SessionTurnImage],
        compiler_role: &str,
        events: mpsc::UnboundedSender<PromptStreamEvent>,
    ) -> Result<ProviderTurnResult> {
        let compiled_turn =
            compiled_turn_from_prompt(history, prompt, images, compiler_role, &[], &[], &[]);
        self.execute_compiled_turn_stream(session, Arc::new(compiled_turn), events)
            .await
    }

    pub async fn execute_compiled_turn_stream(
        &self,
        session: &SessionSummary,
        compiled_turn: Arc<CompiledTurn>,
        events: mpsc::UnboundedSender<PromptStreamEvent>,
    ) -> Result<ProviderTurnResult> {
        let runtime = AdapterKind::parse(&session.provider)
            .ok_or_else(|| anyhow!("unsupported provider '{}'", session.provider))?;

        match runtime {
            AdapterKind::OpenAiCompatible => {
                execute_openai_compatible_prompt(session, &compiled_turn, events).await
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
        "messages": compiled_turn_openai_messages(compiled_turn),
    });
    if compiled_turn_requires_json_object(compiled_turn) {
        payload["response_format"] = json!({ "type": "json_object" });
    }

    let mut request = client
        .post(format!("{base_url}/chat/completions"))
        .json(&payload);

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
        bail!(
            "OpenAI-compatible endpoint failed (HTTP {}): {detail}",
            status.as_u16()
        );
    }

    read_openai_compatible_stream(response, events).await
}

async fn read_openai_compatible_stream(
    response: reqwest::Response,
    events: mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<ProviderTurnResult> {
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
            handle_openai_compatible_line(&line, &mut provider_session_id, &mut content, &events)?;
        }
    }

    if !pending.trim().is_empty() {
        handle_openai_compatible_line(
            pending.trim(),
            &mut provider_session_id,
            &mut content,
            &events,
        )?;
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

fn handle_openai_compatible_line(
    line: &str,
    provider_session_id: &mut String,
    content: &mut String,
    events: &mpsc::UnboundedSender<PromptStreamEvent>,
) -> Result<()> {
    if line.is_empty() || !line.starts_with("data:") {
        return Ok(());
    }

    let payload = line["data:".len()..].trim();
    if payload == "[DONE]" {
        return Ok(());
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

    Ok(())
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
}
