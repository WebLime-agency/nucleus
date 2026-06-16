use std::{sync::Arc, time::Duration};

use nucleus_protocol::{
    CompiledConversationTurn, CompiledPromptLayer, CompiledTurn, CompiledTurnCapabilities,
    CompiledTurnDebugSummary, MemoryEntry, SessionSummary,
};
use serde::Deserialize;
use tokio::{sync::mpsc, time::timeout};

use crate::{
    ApiError, AppState, ExtractedMemoryCandidate, agent, bounded_recent_turn_context,
    runtime::PromptStreamEvent,
};

#[cfg(not(test))]
const MEMORY_CLASSIFIER_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const MEMORY_CLASSIFIER_TIMEOUT: Duration = Duration::from_millis(200);
const MEMORY_CLASSIFIER_MEMORY_CHARS: usize = 8_000;

#[derive(Debug, Deserialize)]
struct MemoryClassifierResponse {
    decisions: Vec<ExtractedMemoryCandidate>,
}

pub(crate) async fn classify_memory_for_turn(
    state: &AppState,
    session_id: &str,
    _assistant_turn_id: Option<&str>,
) -> Result<Vec<ExtractedMemoryCandidate>, ApiError> {
    let detail = state
        .store
        .get_session(session_id)
        .map_err(ApiError::from)?;
    let existing_memory = collect_existing_memory_for_session(state, &detail.session)?;
    let system_text = memory_classifier_system_text(&existing_memory);
    let user_prompt = memory_classifier_user_prompt(&detail.turns);
    let execution = agent::resolve_utility_worker_execution_session(
        state,
        &detail.session,
        &format!("memory-classifier-{session_id}"),
        "Memory classifier",
    )
    .await?;

    let compiled_turn = compiled_classifier_turn(system_text, user_prompt);
    // The classifier ignores streamed `PromptStreamEvent`s; drop the receiver so
    // the runtime's `let _ = events.send(...)` calls fail-fast instead of
    // queueing chunk/snapshot clones that would never be drained.
    let (events, receiver) = mpsc::unbounded_channel::<PromptStreamEvent>();
    drop(receiver);
    let capabilities = agent::utility_runtime_model_capabilities(
        state,
        Some(&detail.session),
        &execution.provider,
        &execution.model,
        &execution.provider_base_url,
    );
    let result = timeout(
        MEMORY_CLASSIFIER_TIMEOUT,
        state
            .runtimes
            .execute_compiled_turn_stream_cancellable_with_capabilities(
                &execution,
                Arc::new(compiled_turn),
                capabilities,
                events,
                None,
            ),
    )
    .await
    .map_err(|_| ApiError::bad_request("memory classifier timed out"))?
    .map_err(|error| ApiError::bad_request(format!("memory classifier failed: {error}")))?;

    agent::record_successful_utility_transport_capability(
        state,
        Some(&detail.session),
        &execution.provider,
        &execution.model,
        &execution.provider_base_url,
        &result,
    );

    parse_memory_classifier_response(&result.content)
}

pub(crate) fn memory_classifier_system_text(existing_memory_rows: &[MemoryEntry]) -> String {
    let memory_excerpt = render_existing_memory(existing_memory_rows);
    format!(
        r#"You are a memory classifier. You will not address the user. You will only output JSON.

Return exactly one JSON object and nothing else.

Task:
- Read the recent conversation.
- Decide whether the user's latest durable information should be saved as memory.
- Use category `explicit` when the user explicitly asked Nucleus to remember something.
- Use category `auto_save` for high-confidence durable user facts, preferences, project decisions, constraints, or stable operational notes that should be accepted now without operator review. To update or replace an existing memory, emit a single `auto_save` decision and set `supersedes_id` to the existing memory id.
- Use category `candidate` for plausibly durable information that should be reviewed by an operator before acceptance.
- For greetings, temporary instructions, ordinary questions, and meta questions about memory such as "will you remember it if I open a new session?", return an empty decisions array (`{{"decisions":[]}}`).
- Only emit `category` values from this set: `explicit`, `auto_save`, `candidate`, `none`. Anything else is rejected and the whole response is discarded.
- Do not store secrets, credentials, tokens, cookies, private keys, passwords, authorization headers, or raw secret values.

Output schema:
{{"decisions":[{{"category":"explicit|auto_save|candidate|none","title":"short title","content":"durable memory text","memory_kind":"note|fact|preference|decision|project_note|solution|constraint|todo","tags":["optional"],"evidence":["brief quote or paraphrase"],"reason":"why this is durable","confidence":0.0,"scope_kind":"workspace|project|session","scope_id":"workspace or concrete id","supersedes_id":"existing memory id when updating"}}]}}

Category semantics:
- explicit: the user explicitly asked Nucleus to remember this.
- auto_save: high-confidence durable memory that should be accepted now.
- candidate: useful but lower-confidence memory that needs operator review.
- none: no write; prefer returning an empty decisions array.

Existing accepted memory:
{memory_excerpt}

Examples:
- User: "I love vanilla icea cream can you rememebr that?" -> {{"decisions":[{{"category":"explicit","title":"Ice cream preference","content":"The user likes vanilla ice cream.","memory_kind":"preference","tags":["preference"],"evidence":["User said they love vanilla ice cream and asked to remember it."],"reason":"Explicit durable preference despite typos.","confidence":0.96,"scope_kind":"workspace","scope_id":"workspace"}}]}}
- Existing memory id "pref-flavor": "The user prefers vanilla ice cream." User: "Actually I prefer chocolate now." -> {{"decisions":[{{"category":"auto_save","title":"Ice cream preference","content":"The user prefers chocolate ice cream.","memory_kind":"preference","tags":["preference"],"evidence":["User corrected their preference."],"reason":"Updates an existing durable preference.","confidence":0.94,"scope_kind":"workspace","scope_id":"workspace","supersedes_id":"pref-flavor"}}]}}
- User: "will you remember it if i go to a new session?" -> {{"decisions":[]}}"#
    )
}

pub(crate) fn parse_memory_classifier_response(
    content: &str,
) -> Result<Vec<ExtractedMemoryCandidate>, ApiError> {
    let response: MemoryClassifierResponse =
        serde_json::from_str(content.trim()).map_err(|error| {
            ApiError::bad_request(format!("memory classifier returned invalid JSON: {error}"))
        })?;
    for decision in &response.decisions {
        validate_classifier_decision(decision)?;
    }
    Ok(response.decisions)
}

fn validate_classifier_decision(decision: &ExtractedMemoryCandidate) -> Result<(), ApiError> {
    let category = decision
        .category
        .as_deref()
        .map(str::trim)
        .unwrap_or("candidate");
    if !matches!(category, "explicit" | "auto_save" | "candidate" | "none") {
        return Err(ApiError::bad_request(format!(
            "memory classifier returned unsupported category '{category}'"
        )));
    }
    if category != "none" && decision.content.trim().is_empty() {
        return Err(ApiError::bad_request(
            "memory classifier returned a decision without content",
        ));
    }
    if let Some(confidence) = decision.confidence {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(ApiError::bad_request(
                "memory classifier returned confidence outside 0..1",
            ));
        }
    }
    Ok(())
}

fn collect_existing_memory_for_session(
    state: &AppState,
    session: &SessionSummary,
) -> Result<Vec<MemoryEntry>, ApiError> {
    let mut entries = state
        .store
        .list_memory_entries()
        .map_err(ApiError::from)?
        .into_iter()
        .filter(|entry| entry.enabled && entry.status == "accepted")
        .filter(|entry| {
            (entry.scope_kind == "workspace" && entry.scope_id == "workspace")
                || (entry.scope_kind == "project"
                    && !session.project_id.is_empty()
                    && entry.scope_id == session.project_id)
                || (entry.scope_kind == "session" && entry.scope_id == session.id)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        memory_scope_rank(&left.scope_kind)
            .cmp(&memory_scope_rank(&right.scope_kind))
            .then_with(|| left.memory_kind.cmp(&right.memory_kind))
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(entries)
}

fn memory_scope_rank(scope_kind: &str) -> usize {
    match scope_kind {
        "workspace" => 0,
        "project" => 1,
        "session" => 2,
        _ => 9,
    }
}

fn render_existing_memory(entries: &[MemoryEntry]) -> String {
    if entries.is_empty() {
        return "- none".to_string();
    }

    let mut rendered = String::new();
    for entry in entries {
        let next = format!(
            "- id: {}\n  scope: {}/{}\n  kind: {}\n  title: {}\n  content: {}\n",
            entry.id,
            entry.scope_kind,
            entry.scope_id,
            entry.memory_kind,
            entry.title,
            entry.content
        );
        if rendered.len() + next.len() > MEMORY_CLASSIFIER_MEMORY_CHARS {
            rendered.push_str("- [remaining memory omitted by classifier prompt budget]\n");
            break;
        }
        rendered.push_str(&next);
    }
    rendered
}

fn memory_classifier_user_prompt(turns: &[nucleus_protocol::SessionTurn]) -> String {
    format!(
        "Recent conversation:\n{}\n\nClassify memory decisions for the latest user turn.",
        bounded_recent_turn_context(turns)
    )
}

fn compiled_classifier_turn(system_text: String, user_prompt: String) -> CompiledTurn {
    CompiledTurn {
        id: uuid::Uuid::new_v4().to_string(),
        role: "utility".to_string(),
        provider_neutral: true,
        system_layers: vec![CompiledPromptLayer {
            id: "platform:nucleus-memory-classifier".to_string(),
            kind: "platform".to_string(),
            scope: "nucleus".to_string(),
            title: "Memory classifier contract".to_string(),
            source_path: String::new(),
            content: system_text,
        }],
        project_layers: Vec::new(),
        skill_layers: Vec::new(),
        tool_catalog: Vec::new(),
        mcp_catalog: Vec::new(),
        history: vec![CompiledConversationTurn {
            role: "system".to_string(),
            content: "Return exactly one JSON object and nothing else.".to_string(),
            images: Vec::new(),
        }],
        user_turn: CompiledConversationTurn {
            role: "user".to_string(),
            content: user_prompt,
            images: Vec::new(),
        },
        capabilities: CompiledTurnCapabilities {
            needs_images: false,
            needs_tools: false,
            needs_mcp: false,
        },
        debug_summary: CompiledTurnDebugSummary {
            include_count: 0,
            memory_count: 0,
            memory_included_count: 0,
            memory_skipped_count: 0,
            memory_truncated_count: 0,
            skill_count: 0,
            mcp_server_count: 0,
            tool_count: 0,
            layer_count: 1,
            summary: "Compiled memory classifier prompt.".to_string(),
            skill_diagnostics: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn memory_entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            scope_kind: "workspace".to_string(),
            scope_id: "workspace".to_string(),
            title: "Preference".to_string(),
            content: content.to_string(),
            tags: Vec::new(),
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
        }
    }

    #[test]
    fn prompt_builder_includes_contract_examples_and_existing_memory_ids() {
        let prompt = memory_classifier_system_text(&[memory_entry(
            "pref-flavor",
            "The user prefers vanilla ice cream.",
        )]);
        assert!(prompt.contains("Return exactly one JSON object and nothing else."));
        assert!(prompt.contains("will you remember it if i go to a new session?"));
        assert!(prompt.contains("pref-flavor"));
        assert!(prompt.contains("supersedes_id"));
        assert!(prompt.contains("Do not store secrets"));
        // Task verbs must match the schema categories the parser accepts so a
        // model following the instructions cannot produce ADD/UPDATE responses
        // that get rejected wholesale.
        assert!(prompt.contains("category `explicit`"));
        assert!(prompt.contains("category `auto_save`"));
        assert!(prompt.contains("category `candidate`"));
        assert!(!prompt.contains("Use ADD"));
        assert!(!prompt.contains("Use UPDATE"));
    }

    #[test]
    fn parser_accepts_valid_decision_object() {
        let decisions = parse_memory_classifier_response(
            r#"{"decisions":[{"category":"auto_save","content":"The user prefers vanilla ice cream.","memory_kind":"preference","confidence":0.95}]}"#,
        )
        .expect("valid classifier response should parse");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].candidate_kind, "preference");
    }

    #[test]
    fn parser_rejects_malformed_or_schema_incompatible_responses() {
        assert!(parse_memory_classifier_response("not json").is_err());
        assert!(parse_memory_classifier_response(r#"{"items":[]}"#).is_err());
        assert!(
            parse_memory_classifier_response(
                r#"{"decisions":[{"category":"delete","content":"Nope","confidence":0.5}]}"#,
            )
            .is_err()
        );
        assert!(
            parse_memory_classifier_response(
                r#"{"decisions":[{"category":"auto_save","content":"Nope","confidence":2.0}]}"#,
            )
            .is_err()
        );
    }
}
