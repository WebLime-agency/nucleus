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

    if !turn.mcp_catalog.is_empty() {
        rendered.push_str("\n[Registered MCP metadata - not executable in this runtime]\n");
        for server in &turn.mcp_catalog {
            rendered.push_str("- mcp/");
            rendered.push_str(&server.id);
            rendered.push_str(": ");
            rendered.push_str(&server.title);
            rendered.push_str(" (");
            rendered.push_str(&server.tools.len().to_string());
            rendered.push_str(" registered tool descriptor(s))\n");
        }
    }

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
    let system_text = render_compiled_turn_system_text(turn);
    let mut messages = vec![serde_json::json!({
        "role": "system",
        "content": system_text,
    })];

    for item in &turn.history {
        if matches!(item.role.as_str(), "user" | "assistant" | "system") {
            messages.push(serde_json::json!({
                "role": item.role,
                "content": openai_message_content(&item.content, &item.images),
            }));
        }
    }

    messages.push(serde_json::json!({
        "role": "user",
        "content": openai_message_content(&turn.user_turn.content, &turn.user_turn.images),
    }));
    messages
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

    if !turn.mcp_catalog.is_empty() {
        rendered.push_str("\n[Registered MCP metadata - not executable in this runtime]\n");
        for server in &turn.mcp_catalog {
            rendered.push_str("- mcp/");
            rendered.push_str(&server.id);
            rendered.push_str(": ");
            rendered.push_str(&server.title);
            rendered.push_str(" (");
            rendered.push_str(&server.tools.len().to_string());
            rendered.push_str(" registered tool descriptor(s))\n");
        }
    }

    rendered
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
