pub const PRODUCT_NAME: &str = "Nucleus";
pub const PRODUCT_SLUG: &str = "nucleus";
pub const DEFAULT_WEB_DEV_PORT: u16 = 5201;
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:42240";
pub const DEFAULT_OPENAI_COMPATIBLE_BASE_URL: &str = "http://127.0.0.1:20128/v1";
pub const DEFAULT_OPENAI_COMPATIBLE_MODEL: &str = "gpt-5.4-mini";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterKind {
    Claude,
    Codex,
    OpenAiCompatible,
    System,
}

impl AdapterKind {
    pub const RUNTIME_PROBE_ALL: [AdapterKind; 4] = [
        AdapterKind::OpenAiCompatible,
        AdapterKind::Claude,
        AdapterKind::Codex,
        AdapterKind::System,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "openai_compatible" => Some(Self::OpenAiCompatible),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AdapterKind::Claude => "claude",
            AdapterKind::Codex => "codex",
            AdapterKind::OpenAiCompatible => "openai_compatible",
            AdapterKind::System => "system",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            AdapterKind::Claude => "Anthropic Claude protocol backend",
            AdapterKind::Codex => "OpenAI Codex protocol backend",
            AdapterKind::OpenAiCompatible => "OpenAI-compatible HTTP protocol backend",
            AdapterKind::System => "Host automation and observability adapter",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            AdapterKind::Claude => "sonnet",
            AdapterKind::Codex => "",
            AdapterKind::OpenAiCompatible => DEFAULT_OPENAI_COMPATIBLE_MODEL,
            AdapterKind::System => "",
        }
    }

    pub fn supports_sessions(self) -> bool {
        matches!(
            self,
            AdapterKind::Claude | AdapterKind::Codex | AdapterKind::OpenAiCompatible
        )
    }

    pub fn supports_prompting(self) -> bool {
        matches!(
            self,
            AdapterKind::Claude | AdapterKind::Codex | AdapterKind::OpenAiCompatible
        )
    }
}

pub fn product_banner() -> String {
    format!("{PRODUCT_NAME} local AI control plane")
}

pub fn render_compiled_turn_text(turn: &nucleus_protocol::CompiledTurn) -> String {
    let mut rendered = String::new();

    rendered.push_str(
        "Nucleus compiled turn. Treat this bundle as the sole source of context for this turn.\n",
    );
    rendered.push_str("Compiler role: ");
    rendered.push_str(&turn.role);
    rendered.push_str("\nProvider-neutral: true\n");

    render_layers("System layers", &turn.system_layers, &mut rendered);
    render_layers("Project layers", &turn.project_layers, &mut rendered);
    render_layers("Skill layers", &turn.skill_layers, &mut rendered);

    if !turn.tool_catalog.is_empty() {
        rendered.push_str("\n[Registered Nucleus tool metadata - daemon execution only]\n");
        for tool in &turn.tool_catalog {
            rendered.push_str("- ");
            rendered.push_str(&tool.id);
            rendered.push_str(": ");
            rendered.push_str(&tool.description);
            rendered.push('\n');
        }
    }

    render_mcp_catalog(&turn.mcp_catalog, &mut rendered);

    if !turn.history.is_empty() {
        rendered.push_str("\n[Conversation history]\n");
        for item in &turn.history {
            rendered.push_str(&item.role);
            rendered.push_str(":\n");
            rendered.push_str(&item.content);
            if !item.images.is_empty() {
                rendered.push_str("\n[");
                rendered.push_str(&item.images.len().to_string());
                rendered.push_str(" image attachment(s)]");
            }
            rendered.push_str("\n\n");
        }
    }

    rendered.push_str("\n[Current user turn]\n");
    rendered.push_str(&turn.user_turn.content);
    if !turn.user_turn.images.is_empty() {
        rendered.push_str("\n[");
        rendered.push_str(&turn.user_turn.images.len().to_string());
        rendered.push_str(" image attachment(s) supplied separately by transport]");
    }

    rendered
}

pub fn compiled_turn_openai_messages(
    turn: &nucleus_protocol::CompiledTurn,
) -> Vec<serde_json::Value> {
    let mut system_text = render_compiled_turn_system_text(turn);
    let mut history_messages = Vec::new();
    let mut conversation_started = false;

    for item in &turn.history {
        match item.role.as_str() {
            "system" if !conversation_started => {
                append_history_system_content(&mut system_text, &item.content);
            }
            "system" => {
                let content = format!("[system note]\n{}", item.content);
                history_messages.push(serde_json::json!({
                    "role": "user",
                    "content": openai_message_content(&content, &item.images),
                }));
            }
            "user" | "assistant" => {
                conversation_started = true;
                history_messages.push(serde_json::json!({
                    "role": item.role,
                    "content": openai_message_content(&item.content, &item.images),
                }));
            }
            _ => {}
        }
    }

    let mut messages = vec![serde_json::json!({
        "role": "system",
        "content": system_text,
    })];
    messages.extend(history_messages);

    messages.push(serde_json::json!({
        "role": "user",
        "content": openai_message_content(&turn.user_turn.content, &turn.user_turn.images),
    }));
    messages
}

fn append_history_system_content(system_text: &mut String, content: &str) {
    system_text.push_str("\n\n[History system message]\n");
    system_text.push_str(content);
}

pub fn render_compiled_turn_system_text(turn: &nucleus_protocol::CompiledTurn) -> String {
    let mut rendered = String::new();
    rendered
        .push_str("Nucleus compiled context. Provider features must not replace this context.\n");
    rendered.push_str("Compiler role: ");
    rendered.push_str(&turn.role);
    rendered.push('\n');

    render_layers("System layers", &turn.system_layers, &mut rendered);
    render_layers("Project layers", &turn.project_layers, &mut rendered);
    render_layers("Skill layers", &turn.skill_layers, &mut rendered);

    if !turn.tool_catalog.is_empty() {
        rendered.push_str("\n[Registered Nucleus tool metadata - daemon execution only]\n");
        for tool in &turn.tool_catalog {
            rendered.push_str("- ");
            rendered.push_str(&tool.id);
            rendered.push_str(": ");
            rendered.push_str(&tool.description);
            rendered.push('\n');
        }
    }

    render_mcp_catalog(&turn.mcp_catalog, &mut rendered);

    rendered
}

fn render_mcp_catalog(catalog: &[nucleus_protocol::McpServerSummary], rendered: &mut String) {
    if catalog.is_empty() {
        return;
    }

    rendered.push_str(
        "\n[Registered MCP metadata - daemon action bridge execution]\n\
MCP tool descriptors are available to this turn when granted. Invocation still runs through the Nucleus daemon and may be blocked by runtime credential state.\n",
    );
    for server in catalog {
        rendered.push_str("- mcp/");
        rendered.push_str(&server.id);
        rendered.push_str(": ");
        rendered.push_str(&server.title);
        rendered.push_str("; enabled=");
        rendered.push_str(if server.enabled { "true" } else { "false" });
        rendered.push_str("; discovery_status=");
        rendered.push_str(&server.sync_status);
        rendered.push_str("; ");
        rendered.push_str(&server.tools.len().to_string());
        rendered.push_str(" registered tool descriptor(s)");
        if !server.invocation_status.trim().is_empty() && server.invocation_status != "unknown" {
            rendered.push_str("; invocation_status=");
            rendered.push_str(&server.invocation_status);
        }
        if !server.invocation_message.trim().is_empty() {
            rendered.push_str("; ");
            rendered.push_str(&server.invocation_message);
        }
        rendered.push('\n');
        for tool in &server.tools {
            rendered.push_str("  - ");
            rendered.push_str(&tool.id);
            rendered.push_str(": ");
            rendered.push_str(&tool.description);
            rendered.push('\n');
        }
    }
}

fn render_layers(
    heading: &str,
    layers: &[nucleus_protocol::CompiledPromptLayer],
    rendered: &mut String,
) {
    if layers.is_empty() {
        return;
    }

    rendered.push('\n');
    rendered.push_str(heading);
    rendered.push('\n');
    for layer in layers {
        rendered.push('[');
        rendered.push_str(&layer.kind);
        rendered.push_str(": ");
        rendered.push_str(&layer.scope);
        if !layer.source_path.is_empty() {
            rendered.push(' ');
            rendered.push_str(&layer.source_path);
        }
        rendered.push_str("]\n");
        rendered.push_str(&layer.content);
        rendered.push_str("\n\n");
    }
}

fn openai_message_content(
    text: &str,
    images: &[nucleus_protocol::SessionTurnImage],
) -> serde_json::Value {
    if images.is_empty() {
        return serde_json::Value::String(text.to_string());
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

    let mut parts = vec![serde_json::json!({
        "type": "text",
        "text": caption,
    })];

    for image in images {
        parts.push(serde_json::json!({
            "type": "image_url",
            "image_url": {
                "url": image.data_url,
            },
        }));
    }

    serde_json::Value::Array(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nucleus_protocol::{
        CompiledConversationTurn, CompiledPromptLayer, CompiledTurn, CompiledTurnCapabilities,
        CompiledTurnDebugSummary,
    };

    #[test]
    fn openai_messages_merge_leading_history_system_into_initial_system_message() {
        let turn = test_compiled_turn(vec![
            test_history_turn("system", "worker system prompt"),
            test_history_turn("user", "previous user"),
            test_history_turn("assistant", "previous assistant"),
        ]);

        let messages = compiled_turn_openai_messages(&turn);

        assert_single_leading_system_message(&messages);
        let system_content = message_content_text(&messages[0]);
        assert!(system_content.contains("platform system layer"));
        assert!(system_content.contains("worker system prompt"));
        assert_eq!(message_role(&messages[1]), "user");
        assert_eq!(message_content_text(&messages[1]), "previous user");
        assert_eq!(message_role(&messages[2]), "assistant");
        assert_eq!(message_content_text(&messages[2]), "previous assistant");
        assert_eq!(message_role(&messages[3]), "user");
        assert_eq!(message_content_text(&messages[3]), "current user");
    }

    #[test]
    fn openai_messages_relabel_mid_history_system_note_in_place() {
        let turn = test_compiled_turn(vec![
            test_history_turn("user", "previous user"),
            test_history_turn("system", "wake system note"),
            test_history_turn("assistant", "previous assistant"),
        ]);

        let messages = compiled_turn_openai_messages(&turn);

        assert_single_leading_system_message(&messages);
        assert_eq!(message_role(&messages[1]), "user");
        assert_eq!(message_content_text(&messages[1]), "previous user");
        assert_eq!(message_role(&messages[2]), "user");
        let relabeled_content = message_content_text(&messages[2]);
        assert!(relabeled_content.contains("[system note]"));
        assert!(relabeled_content.contains("wake system note"));
        assert_eq!(message_role(&messages[3]), "assistant");
        assert_eq!(message_content_text(&messages[3]), "previous assistant");
    }

    #[test]
    fn openai_messages_without_history_system_turns_are_unchanged() {
        let turn = test_compiled_turn(vec![
            test_history_turn("user", "previous user"),
            test_history_turn("assistant", "previous assistant"),
        ]);

        let messages = compiled_turn_openai_messages(&turn);

        assert_single_leading_system_message(&messages);
        assert_eq!(messages.len(), 4);
        assert_eq!(
            message_content_text(&messages[0]),
            render_compiled_turn_system_text(&turn)
        );
        assert_eq!(message_role(&messages[1]), "user");
        assert_eq!(message_content_text(&messages[1]), "previous user");
        assert_eq!(message_role(&messages[2]), "assistant");
        assert_eq!(message_content_text(&messages[2]), "previous assistant");
        assert_eq!(message_role(&messages[3]), "user");
        assert_eq!(message_content_text(&messages[3]), "current user");
    }

    fn assert_single_leading_system_message(messages: &[serde_json::Value]) {
        let system_indexes = messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| (message_role(message) == "system").then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(system_indexes, vec![0]);
    }

    fn message_role(message: &serde_json::Value) -> &str {
        message
            .get("role")
            .and_then(|value| value.as_str())
            .expect("message role should be a string")
    }

    fn message_content_text(message: &serde_json::Value) -> String {
        message
            .get("content")
            .and_then(|value| value.as_str())
            .expect("message content should be text")
            .to_string()
    }

    fn test_history_turn(role: &str, content: &str) -> CompiledConversationTurn {
        CompiledConversationTurn {
            role: role.to_string(),
            content: content.to_string(),
            images: Vec::new(),
        }
    }

    fn test_compiled_turn(history: Vec<CompiledConversationTurn>) -> CompiledTurn {
        CompiledTurn {
            id: "turn".to_string(),
            role: "assistant".to_string(),
            provider_neutral: true,
            system_layers: vec![CompiledPromptLayer {
                id: "system".to_string(),
                kind: "system".to_string(),
                scope: "workspace".to_string(),
                title: "System".to_string(),
                source_path: String::new(),
                content: "platform system layer".to_string(),
            }],
            project_layers: Vec::new(),
            skill_layers: Vec::new(),
            tool_catalog: Vec::new(),
            mcp_catalog: Vec::new(),
            history,
            user_turn: test_history_turn("user", "current user"),
            capabilities: CompiledTurnCapabilities {
                needs_images: false,
                needs_tools: false,
                needs_mcp: false,
            },
            debug_summary: CompiledTurnDebugSummary {
                include_count: 1,
                memory_count: 0,
                memory_included_count: 0,
                memory_skipped_count: 0,
                memory_truncated_count: 0,
                skill_count: 0,
                mcp_server_count: 0,
                tool_count: 0,
                layer_count: 1,
                summary: String::new(),
                skill_diagnostics: Vec::new(),
            },
        }
    }
}
