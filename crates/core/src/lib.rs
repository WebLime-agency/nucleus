pub const PRODUCT_NAME: &str = "Nucleus";
pub const PRODUCT_SLUG: &str = "nucleus";
pub const DEFAULT_WEB_DEV_PORT: u16 = 5201;
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:42240";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterKind {
    Claude,
    Codex,
    OpenAiCompatible,
    System,
}

impl AdapterKind {
    pub const RUNTIME_PROBE_ALL: [AdapterKind; 3] =
        [AdapterKind::Claude, AdapterKind::Codex, AdapterKind::System];

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
            AdapterKind::Claude => "Anthropic Claude session adapter",
            AdapterKind::Codex => "OpenAI Codex session adapter",
            AdapterKind::OpenAiCompatible => "OpenAI-compatible HTTP session adapter",
            AdapterKind::System => "Host automation and observability adapter",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            AdapterKind::Claude => "sonnet",
            AdapterKind::Codex => "",
            AdapterKind::OpenAiCompatible => "",
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
