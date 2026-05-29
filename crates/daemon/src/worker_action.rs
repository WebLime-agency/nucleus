use std::{collections::BTreeSet, error::Error, fmt};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerAction {
    ToolCall {
        summary: String,
        tool: String,
        #[serde(default)]
        args: Value,
    },
    SpawnChildJobs {
        summary: String,
        jobs: Vec<ChildJobProposal>,
    },
    ProgressUpdate {
        summary: String,
        detail: String,
    },
    Wait {
        summary: String,
        until: WaitUntil,
        #[serde(default)]
        max_wait_seconds: Option<u64>,
        #[serde(default)]
        wake_note: Option<String>,
    },
    FinalAnswer {
        summary: String,
        final_answer: String,
        #[serde(default)]
        metadata: Value,
        #[serde(default)]
        artifacts: Vec<FinalAnswerArtifact>,
        #[serde(default)]
        browser_verification: Option<BrowserVerificationClaim>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitUntil {
    DelaySeconds {
        delay_seconds: u64,
    },
    AbsoluteUnix {
        absolute_unix: i64,
    },
    AuditEvent {
        event_kind: String,
        #[serde(default)]
        target_pattern: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },
    ChildJobsCompleted {
        job_ids: Vec<String>,
    },
    ArtifactKind {
        job_id: String,
        #[serde(alias = "artifact_kind")]
        artifact_kind: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ChildJobProposal {
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub task_class: Option<String>,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub route_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BrowserVerificationClaim {
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FinalAnswerArtifact {
    pub kind: String,
    pub title: String,
    pub content: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerActionParseError {
    NoJsonObject,
    MalformedJson { detail: String },
    InvalidActionShape,
    UnknownTool { tool: String },
}

impl WorkerActionParseError {
    pub fn is_repairable_contract_error(&self) -> bool {
        matches!(
            self,
            WorkerActionParseError::NoJsonObject
                | WorkerActionParseError::MalformedJson { .. }
                | WorkerActionParseError::InvalidActionShape
                | WorkerActionParseError::UnknownTool { .. }
        )
    }
}

impl fmt::Display for WorkerActionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkerActionParseError::NoJsonObject => {
                write!(f, "worker returned no JSON action object")
            }
            WorkerActionParseError::MalformedJson { detail } => {
                write!(f, "worker returned malformed JSON action: {detail}")
            }
            WorkerActionParseError::InvalidActionShape => {
                write!(
                    f,
                    "worker returned valid JSON that does not match the Nucleus action contract"
                )
            }
            WorkerActionParseError::UnknownTool { tool } => {
                write!(f, "worker requested unknown Nucleus action '{tool}'")
            }
        }
    }
}

impl Error for WorkerActionParseError {}

pub fn parse_worker_action(content: &str) -> Result<WorkerAction, WorkerActionParseError> {
    parse_worker_action_with_support(content, &WorkerToolSupport::default())
}

pub fn parse_worker_action_with_registered_mcp_tools<'a, I>(
    content: &str,
    registered_mcp_tool_ids: I,
) -> Result<WorkerAction, WorkerActionParseError>
where
    I: IntoIterator<Item = &'a str>,
{
    let support = WorkerToolSupport::from_registered_mcp_tools(registered_mcp_tool_ids);
    parse_worker_action_with_support(content, &support)
}

#[derive(Debug, Default)]
struct WorkerToolSupport {
    registered_mcp_tool_ids: BTreeSet<String>,
}

impl WorkerToolSupport {
    fn from_registered_mcp_tools<'a, I>(registered_mcp_tool_ids: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        Self {
            registered_mcp_tool_ids: registered_mcp_tool_ids
                .into_iter()
                .map(str::trim)
                .filter(|tool_id| !tool_id.is_empty())
                .map(ToString::to_string)
                .collect(),
        }
    }

    fn is_supported_tool(&self, tool: &str) -> bool {
        is_builtin_nucleus_tool(tool) || self.registered_mcp_tool_ids.contains(tool)
    }
}

fn parse_worker_action_with_support(
    content: &str,
    support: &WorkerToolSupport,
) -> Result<WorkerAction, WorkerActionParseError> {
    let trimmed = content.trim();
    if let Some(action) = recover_provider_tool_action(trimmed, support)? {
        return validate_worker_action(action, support);
    }

    let start = trimmed
        .find('{')
        .ok_or(WorkerActionParseError::NoJsonObject)?;
    let end = trimmed
        .rfind('}')
        .ok_or(WorkerActionParseError::NoJsonObject)?;
    let candidate = &trimmed[start..=end];

    parse_worker_action_json_candidate(candidate, support).or_else(|error| {
        if let Some(action) = recover_provider_shell_action(candidate, support)? {
            return validate_worker_action(action, support);
        }
        Err(error)
    })
}

fn parse_worker_action_json_candidate(
    candidate: &str,
    support: &WorkerToolSupport,
) -> Result<WorkerAction, WorkerActionParseError> {
    let value = parse_worker_action_value(candidate)?;
    if let Some(action) = normalize_worker_action_value(&value, support)? {
        return validate_worker_action(action, support);
    }

    match serde_json::from_str::<WorkerAction>(candidate) {
        Ok(parsed) => validate_worker_action(parsed, support),
        Err(_error) if serde_json::from_str::<Value>(candidate).is_ok() => {
            Err(WorkerActionParseError::InvalidActionShape)
        }
        Err(error) => Err(WorkerActionParseError::MalformedJson {
            detail: excerpt(&error.to_string(), 220),
        }),
    }
}

fn recover_provider_tool_action(
    content: &str,
    support: &WorkerToolSupport,
) -> Result<Option<WorkerAction>, WorkerActionParseError> {
    if !contains_provider_tool_marker(content) {
        return Ok(None);
    }

    let mut recovered = Vec::new();
    let mut unknown_tool = None;

    collect_xml_provider_tool_actions(content, support, &mut recovered, &mut unknown_tool)?;
    collect_json_provider_tool_actions(content, support, &mut recovered, &mut unknown_tool)?;

    match recovered.len() {
        0 => {
            if let Some(error) = unknown_tool {
                Err(error)
            } else {
                Ok(None)
            }
        }
        1 => Ok(recovered.pop()),
        _ => Err(WorkerActionParseError::InvalidActionShape),
    }
}

fn contains_provider_tool_marker(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    lower.contains("tool_call") || lower.contains("function_call")
}

fn collect_xml_provider_tool_actions(
    content: &str,
    support: &WorkerToolSupport,
    recovered: &mut Vec<WorkerAction>,
    unknown_tool: &mut Option<WorkerActionParseError>,
) -> Result<(), WorkerActionParseError> {
    let mut search_start = 0;
    while let Some((tag_start, tag_name)) = find_next_provider_tool_tag(content, search_start) {
        let after_tag_name = tag_start + tag_name.len() + 1;
        let Some(relative_tag_end) = content[after_tag_name..].find('>') else {
            break;
        };
        let tag_end = after_tag_name + relative_tag_end;
        let attributes = &content[after_tag_name..tag_end];
        let body_start = tag_end + 1;
        let closing = format!("</{tag_name}>");
        let body_end = find_ascii_case_insensitive(&content[body_start..], &closing)
            .map(|relative| body_start + relative)
            .unwrap_or(content.len());
        search_start = if body_end < content.len() {
            body_end + closing.len()
        } else {
            body_end
        };

        let Some(tool) = xml_attribute(attributes, "name")
            .or_else(|| xml_attribute(attributes, "tool"))
            .or_else(|| xml_attribute(attributes, "tool_name"))
        else {
            continue;
        };
        let tool = tool.trim();
        if tool.is_empty() {
            continue;
        }

        let args = xml_tool_call_body_args(&content[body_start..body_end], tool);
        let value = json!({
            "name": tool,
            "arguments": args,
        });
        match normalize_worker_tool_call_value(&value, support) {
            Ok(action) => recovered.push(action),
            Err(error @ WorkerActionParseError::UnknownTool { .. }) => {
                unknown_tool.get_or_insert(error);
            }
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

fn find_next_provider_tool_tag(
    content: &str,
    search_start: usize,
) -> Option<(usize, &'static str)> {
    let tool_call = find_ascii_case_insensitive(&content[search_start..], "<tool_call")
        .map(|index| (search_start + index, "tool_call"));
    let function_call = find_ascii_case_insensitive(&content[search_start..], "<function_call")
        .map(|index| (search_start + index, "function_call"));

    match (tool_call, function_call) {
        (Some(left), Some(right)) if left.0 <= right.0 => Some(left),
        (Some(_), Some(right)) => Some(right),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn xml_attribute(attributes: &str, name: &str) -> Option<String> {
    let mut search_start = 0;
    while search_start < attributes.len() {
        let relative_name_start = attributes[search_start..].find(name)?;
        let name_start = search_start + relative_name_start;
        let name_end = name_start + name.len();
        let before_name = attributes[..name_start].chars().last();
        let after_name = attributes[name_end..].chars().next();
        if before_name.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            || after_name.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            search_start = name_end;
            continue;
        }

        let after_name_text = attributes[name_end..].trim_start();
        if !after_name_text.starts_with('=') {
            search_start = name_end;
            continue;
        }
        let after_equals = after_name_text[1..].trim_start();
        let quote = after_equals.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let value_start = quote.len_utf8();
        let value_end = after_equals[value_start..].find(quote)?;
        return Some(after_equals[value_start..value_start + value_end].to_string());
    }

    None
}

fn xml_tool_call_body_args(body: &str, raw_tool: &str) -> Value {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Value::Object(Map::new());
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }

    let normalized = raw_tool.trim().to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "shell" | "bash" | "terminal" | "command" | "run_command"
    ) {
        return Value::String(trimmed.to_string());
    }

    Value::Object(Map::new())
}

fn collect_json_provider_tool_actions(
    content: &str,
    support: &WorkerToolSupport,
    recovered: &mut Vec<WorkerAction>,
    unknown_tool: &mut Option<WorkerActionParseError>,
) -> Result<(), WorkerActionParseError> {
    for candidate in json_object_candidates(content) {
        match parse_worker_action_json_candidate(candidate, support) {
            Ok(action @ WorkerAction::ToolCall { .. }) => recovered.push(action),
            Ok(_) => {}
            Err(error @ WorkerActionParseError::UnknownTool { .. }) => {
                unknown_tool.get_or_insert(error);
            }
            Err(WorkerActionParseError::MalformedJson { .. })
            | Err(WorkerActionParseError::NoJsonObject)
            | Err(WorkerActionParseError::InvalidActionShape) => {}
        }
    }

    Ok(())
}

fn json_object_candidates(content: &str) -> Vec<&str> {
    let mut candidates = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in content.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = start.take() {
                        candidates.push(&content[start..index + ch.len_utf8()]);
                    }
                }
            }
            _ => {}
        }
    }

    candidates
}

fn recover_provider_shell_action(
    candidate: &str,
    support: &WorkerToolSupport,
) -> Result<Option<WorkerAction>, WorkerActionParseError> {
    let lower = candidate.to_ascii_lowercase();
    if !lower.contains("tool_call") || !lower.contains("shell") || !lower.contains("command") {
        return Ok(None);
    }

    let Some(command) = extract_jsonish_string_field(candidate, "command", false) else {
        return Ok(None);
    };
    let command = command.trim();
    if command.is_empty() {
        return Ok(None);
    }

    let mut args = serde_json::Map::new();
    args.insert("command".to_string(), Value::String(command.to_string()));
    if let Some(cwd) = extract_jsonish_string_field(candidate, "cwd", true)
        .or_else(|| extract_jsonish_string_field(candidate, "workdir", true))
        .or_else(|| extract_jsonish_string_field(candidate, "working_dir", true))
    {
        let cwd = cwd.trim();
        if !cwd.is_empty() {
            args.insert("cwd".to_string(), Value::String(cwd.to_string()));
        }
    }

    normalize_worker_tool_name_and_args("shell", Value::Object(args), support).map(
        |(tool, args)| {
            Some(WorkerAction::ToolCall {
                summary: "Run the requested Nucleus action.".to_string(),
                tool,
                args,
            })
        },
    )
}

fn extract_jsonish_string_field(
    candidate: &str,
    field: &str,
    allow_comma_end: bool,
) -> Option<String> {
    let quoted_field = format!("\"{field}\"");
    let field_start = candidate.find(&quoted_field)?;
    let after_field = &candidate[field_start + quoted_field.len()..];
    let colon_offset = after_field.find(':')?;
    let after_colon = after_field[colon_offset + 1..].trim_start();
    let value_start_in_after_colon = after_field[colon_offset + 1..].len() - after_colon.len();
    if !after_colon.starts_with('"') {
        return None;
    }
    let absolute_value_start =
        field_start + quoted_field.len() + colon_offset + 1 + value_start_in_after_colon + 1;
    let rest = &candidate[absolute_value_start..];

    let end = find_jsonish_string_end(rest, allow_comma_end).unwrap_or(rest.len());
    Some(unescape_jsonish_string(&rest[..end]))
}

fn find_jsonish_string_end(value: &str, allow_comma_end: bool) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut escaped = false;
    let mut fallback = None;

    for (index, byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match *byte {
            b'\\' => escaped = true,
            b'"' => {
                fallback = Some(index);
                let tail = value[index + 1..].trim_start();
                if tail.starts_with('}')
                    || (allow_comma_end && tail.starts_with(','))
                    || tail.starts_with("]}")
                    || tail.starts_with("}}")
                    || tail.starts_with("}]}")
                {
                    return Some(index);
                }
            }
            _ => {}
        }
    }

    fallback
}

fn unescape_jsonish_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

fn validate_worker_action(
    action: WorkerAction,
    support: &WorkerToolSupport,
) -> Result<WorkerAction, WorkerActionParseError> {
    if let WorkerAction::ToolCall { tool, .. } = &action {
        if !support.is_supported_tool(tool) {
            return Err(WorkerActionParseError::UnknownTool { tool: tool.clone() });
        }
    }
    Ok(action)
}

fn parse_worker_action_value(candidate: &str) -> Result<Value, WorkerActionParseError> {
    serde_json::from_str::<Value>(candidate)
        .or_else(|_| serde_json::from_str::<Value>(&sanitize_worker_json_candidate(candidate)))
        .map_err(|error| WorkerActionParseError::MalformedJson {
            detail: excerpt(&error.to_string(), 220),
        })
}

fn sanitize_worker_json_candidate(candidate: &str) -> String {
    let mut sanitized = String::with_capacity(candidate.len());
    let mut chars = candidate.chars().peekable();
    let mut in_string = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_string = !in_string;
                sanitized.push(ch);
            }
            '\\' if in_string => match chars.peek().copied() {
                Some(next) if is_json_escape_character(next) => {
                    sanitized.push(ch);
                    sanitized.push(next);
                    chars.next();
                }
                Some(_) => {
                    sanitized.push('\\');
                    sanitized.push('\\');
                }
                None => {
                    sanitized.push('\\');
                    sanitized.push('\\');
                }
            },
            '\n' if in_string => sanitized.push_str("\\n"),
            '\r' if in_string => sanitized.push_str("\\r"),
            '\t' if in_string => sanitized.push_str("\\t"),
            _ => sanitized.push(ch),
        }
    }

    sanitized
}

fn is_json_escape_character(ch: char) -> bool {
    matches!(ch, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u')
}

fn normalize_worker_action_value(
    value: &Value,
    support: &WorkerToolSupport,
) -> Result<Option<WorkerAction>, WorkerActionParseError> {
    let object = value
        .as_object()
        .ok_or(WorkerActionParseError::InvalidActionShape)?;

    if object.contains_key("progress_update") || object.contains_key("progress") {
        return normalize_worker_progress_update_value(object).map(Some);
    }

    if object
        .get("action")
        .or_else(|| object.get("kind"))
        .or_else(|| object.get("type"))
        .and_then(Value::as_str)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "progress_update" || value == "checkpoint"
        })
        .unwrap_or(false)
    {
        return normalize_worker_progress_update_value(object).map(Some);
    }

    if object
        .get("action")
        .or_else(|| object.get("kind"))
        .or_else(|| object.get("type"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("final_answer"))
        .unwrap_or(false)
    {
        return normalize_worker_final_answer_value(object).map(Some);
    }

    if let Some(tool_call) = object.get("tool_call") {
        return normalize_worker_tool_call_value(tool_call, support).map(Some);
    }

    if let Some(function_call) = object.get("function_call") {
        return normalize_worker_tool_call_value(function_call, support).map(Some);
    }

    if object
        .get("action")
        .or_else(|| object.get("kind"))
        .or_else(|| object.get("type"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("tool_call"))
        .unwrap_or(false)
        && (object.contains_key("tool")
            || object.contains_key("tool_name")
            || object.contains_key("name"))
    {
        return normalize_worker_tool_call_value(value, support).map(Some);
    }

    if object
        .get("action")
        .or_else(|| object.get("kind"))
        .or_else(|| object.get("type"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("spawn_child_jobs"))
        .unwrap_or(false)
    {
        return Ok(None);
    }

    if object.contains_key("final_answer") {
        return normalize_worker_final_answer_value(object).map(Some);
    }

    if object.contains_key("tool")
        || object.contains_key("tool_name")
        || object.contains_key("name")
    {
        return normalize_worker_tool_call_value(value, support).map(Some);
    }

    Ok(None)
}

fn normalize_worker_progress_update_value(
    object: &serde_json::Map<String, Value>,
) -> Result<WorkerAction, WorkerActionParseError> {
    let nested = object
        .get("progress_update")
        .or_else(|| object.get("progress"))
        .and_then(Value::as_object);
    let detail = object
        .get("detail")
        .or_else(|| object.get("content"))
        .or_else(|| object.get("message"))
        .or_else(|| nested.and_then(|value| value.get("detail")))
        .or_else(|| nested.and_then(|value| value.get("message")))
        .or_else(|| nested.and_then(|value| value.get("content")))
        .or_else(|| {
            object
                .get("progress_update")
                .filter(|value| value.is_string())
        })
        .or_else(|| object.get("progress").filter(|value| value.is_string()))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| nested.map(format_progress_update_object))
        .ok_or(WorkerActionParseError::InvalidActionShape)?;
    let summary = object
        .get("summary")
        .or_else(|| nested.and_then(|value| value.get("summary")))
        .or_else(|| nested.and_then(|value| value.get("status")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Recorded a non-terminal progress checkpoint.")
        .to_string();

    Ok(WorkerAction::ProgressUpdate { summary, detail })
}

fn format_progress_update_object(object: &serde_json::Map<String, Value>) -> String {
    let mut lines = Vec::new();
    for key in [
        "status",
        "summary",
        "validated",
        "changed_files",
        "remaining",
        "next",
    ] {
        if let Some(value) = object.get(key) {
            lines.push(format!("{}: {}", key, format_progress_value(value)));
        }
    }

    for (key, value) in object {
        if [
            "status",
            "summary",
            "validated",
            "changed_files",
            "remaining",
            "next",
        ]
        .contains(&key.as_str())
        {
            continue;
        }
        lines.push(format!("{}: {}", key, format_progress_value(value)));
    }

    lines.join("\n")
}

fn format_progress_value(value: &Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_else(|_| value.to_string()))
}

fn normalize_worker_final_answer_value(
    object: &serde_json::Map<String, Value>,
) -> Result<WorkerAction, WorkerActionParseError> {
    let nested_final_answer = object.get("final_answer").and_then(Value::as_object);
    let final_answer = normalized_final_answer_message(object)
        .or_else(|| nested_final_answer.and_then(normalized_final_answer_message))
        .or_else(|| {
            object
                .get("final_answer")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed)
        });
    let mut metadata = Map::new();
    collect_final_answer_metadata(object, &mut metadata);
    if let Some(nested) = nested_final_answer {
        collect_final_answer_metadata(nested, &mut metadata);
    }
    let mut artifacts = Vec::new();
    collect_explicit_final_answer_artifacts(object, &mut artifacts);
    collect_final_answer_artifacts(object, &mut artifacts);
    if let Some(nested) = nested_final_answer {
        collect_explicit_final_answer_artifacts(nested, &mut artifacts);
        collect_final_answer_artifacts(nested, &mut artifacts);
    }
    artifacts.sort_by_key(|artifact| final_answer_artifact_priority(&artifact.kind));
    let summary = object
        .get("summary")
        .or_else(|| nested_final_answer.and_then(|value| value.get("summary")))
        .or_else(|| nested_final_answer.and_then(|value| value.get("status")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("The work is done.")
        .to_string();
    let final_answer = final_answer
        .or_else(|| {
            if !artifacts.is_empty() && !summary.trim().is_empty() {
                Some(summary.clone())
            } else {
                None
            }
        })
        .ok_or(WorkerActionParseError::InvalidActionShape)?;
    let browser_verification_value = object
        .get("browser_verification")
        .or_else(|| nested_final_answer.and_then(|value| value.get("browser_verification")))
        .filter(|value| !is_empty_json_value(value))
        .cloned();
    let browser_verification = browser_verification_value
        .as_ref()
        .and_then(|value| serde_json::from_value::<BrowserVerificationClaim>(value.clone()).ok());
    if let Some(value) = browser_verification_value {
        metadata
            .entry("browser_verification".to_string())
            .or_insert(value);
    }
    if let Some(claim) = browser_verification.as_ref() {
        if let Some(status) = non_empty_trimmed(&claim.status) {
            metadata
                .entry("browser_verification_status".to_string())
                .or_insert(Value::String(status));
        }
    }

    Ok(WorkerAction::FinalAnswer {
        summary,
        final_answer,
        metadata: Value::Object(metadata),
        artifacts,
        browser_verification,
    })
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalized_final_answer_message(object: &serde_json::Map<String, Value>) -> Option<String> {
    ["message", "content", "answer", "text"]
        .iter()
        .find_map(|key| object.get(*key).and_then(format_final_answer_body))
}

fn format_final_answer_body(value: &Value) -> Option<String> {
    if is_empty_json_value(value) {
        return None;
    }

    match value {
        Value::String(value) => non_empty_trimmed(value),
        Value::Array(items) => {
            let items = items
                .iter()
                .filter_map(format_final_answer_list_item)
                .collect::<Vec<_>>();
            if items.is_empty() {
                None
            } else {
                Some(
                    items
                        .into_iter()
                        .map(|item| format!("- {item}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
        }
        Value::Object(object) => normalized_final_answer_message(object),
        Value::Bool(_) | Value::Number(_) => Some(format_inline_final_answer_value(value)),
        Value::Null => None,
    }
}

fn collect_final_answer_metadata(
    object: &serde_json::Map<String, Value>,
    metadata: &mut Map<String, Value>,
) {
    for (key, value) in object {
        if is_empty_json_value(value)
            || is_final_answer_control_key(key)
            || final_answer_artifact_kind(key).is_some()
        {
            continue;
        }
        if key == "metadata" {
            if let Some(explicit) = value.as_object() {
                for (nested_key, nested_value) in explicit {
                    if !is_empty_json_value(nested_value) {
                        metadata.insert(nested_key.clone(), nested_value.clone());
                    }
                }
            }
            continue;
        }
        metadata.insert(key.clone(), value.clone());
    }
}

fn collect_final_answer_artifacts(
    object: &serde_json::Map<String, Value>,
    artifacts: &mut Vec<FinalAnswerArtifact>,
) {
    for (key, value) in object {
        let Some(kind) = final_answer_artifact_kind(key) else {
            continue;
        };
        push_final_answer_artifact(&kind, key, value, artifacts);
    }
}

fn push_final_answer_artifact(
    kind: &str,
    key: &str,
    value: &Value,
    artifacts: &mut Vec<FinalAnswerArtifact>,
) {
    if is_empty_json_value(value) {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                push_final_answer_artifact(kind, key, item, artifacts);
            }
        }
        Value::Object(object) => {
            let content = ["content", "body", "text", "message", "prompt", "comment"]
                .iter()
                .find_map(|field| object.get(*field).and_then(format_final_answer_body))
                .unwrap_or_else(|| {
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
                });
            if content.trim().is_empty() {
                return;
            }
            let mut metadata = Map::new();
            for (nested_key, nested_value) in object {
                if ["content", "body", "text", "message", "prompt", "comment"]
                    .contains(&nested_key.as_str())
                    || is_empty_json_value(nested_value)
                {
                    continue;
                }
                metadata.insert(nested_key.clone(), nested_value.clone());
            }
            artifacts.push(FinalAnswerArtifact {
                kind: kind.to_string(),
                title: final_answer_artifact_title(kind, key),
                content,
                metadata: Value::Object(metadata),
            });
        }
        _ => {
            let content = format_inline_final_answer_value(value);
            if content.trim().is_empty() {
                return;
            }
            artifacts.push(FinalAnswerArtifact {
                kind: kind.to_string(),
                title: final_answer_artifact_title(kind, key),
                content,
                metadata: json!({}),
            });
        }
    }
}

fn format_final_answer_list_item(value: &Value) -> Option<String> {
    if is_empty_json_value(value) {
        None
    } else {
        Some(format_inline_final_answer_value(value))
    }
}

fn format_inline_final_answer_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.trim().to_string(),
        Value::Array(items) => items
            .iter()
            .filter_map(format_final_answer_list_item)
            .collect::<Vec<_>>()
            .join(", "),
        value => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn is_empty_json_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(items) => items.iter().all(is_empty_json_value),
        Value::Object(object) => object.values().all(is_empty_json_value),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

fn is_final_answer_control_key(key: &str) -> bool {
    matches!(
        key,
        "action"
            | "kind"
            | "type"
            | "final_answer"
            | "message"
            | "content"
            | "answer"
            | "text"
            | "summary"
            | "artifacts"
            | "browser_verification"
    )
}

fn collect_explicit_final_answer_artifacts(
    object: &serde_json::Map<String, Value>,
    artifacts: &mut Vec<FinalAnswerArtifact>,
) {
    let Some(value) = object.get("artifacts") else {
        return;
    };
    push_explicit_final_answer_artifact(value, artifacts);
}

fn push_explicit_final_answer_artifact(value: &Value, artifacts: &mut Vec<FinalAnswerArtifact>) {
    if is_empty_json_value(value) {
        return;
    }

    match value {
        Value::Array(items) => {
            for item in items {
                push_explicit_final_answer_artifact(item, artifacts);
            }
        }
        Value::Object(object) => {
            let kind = object
                .get("kind")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed)
                .unwrap_or_else(|| "artifact".to_string());
            let content = ["content", "body", "text", "message", "prompt", "comment"]
                .iter()
                .find_map(|field| object.get(*field).and_then(format_final_answer_body))
                .unwrap_or_else(|| {
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
                });
            if content.trim().is_empty() {
                return;
            }

            let title = object
                .get("title")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed)
                .unwrap_or_else(|| final_answer_artifact_title(&kind, "artifact"));
            let mut metadata = Map::new();
            if let Some(explicit) = object.get("metadata").and_then(Value::as_object) {
                for (nested_key, nested_value) in explicit {
                    if !is_empty_json_value(nested_value) {
                        metadata.insert(nested_key.clone(), nested_value.clone());
                    }
                }
            }
            for (nested_key, nested_value) in object {
                if [
                    "kind", "title", "content", "body", "text", "message", "prompt", "comment",
                    "metadata",
                ]
                .contains(&nested_key.as_str())
                    || is_empty_json_value(nested_value)
                {
                    continue;
                }
                metadata.insert(nested_key.clone(), nested_value.clone());
            }
            artifacts.push(FinalAnswerArtifact {
                kind,
                title,
                content,
                metadata: Value::Object(metadata),
            });
        }
        _ => {
            let content = format_inline_final_answer_value(value);
            if content.trim().is_empty() {
                return;
            }
            artifacts.push(FinalAnswerArtifact {
                kind: "artifact".to_string(),
                title: "Artifact".to_string(),
                content,
                metadata: json!({}),
            });
        }
    }
}

fn final_answer_artifact_kind(key: &str) -> Option<String> {
    let normalized = key.trim().to_ascii_lowercase();
    let kind = match normalized.as_str() {
        "implementation_prompt"
        | "implementation_prompts"
        | "generated_prompt"
        | "generated_prompts" => "implementation_prompt",
        "issue_comment" | "issue_comments" | "comment" | "comments" => "issue_comment",
        "pr_summary" | "pull_request_summary" => "pr_summary",
        "validation_report" | "validation_reports" => "validation_report",
        "browser_verification_report" | "browser_verification_reports" => {
            "browser_verification_report"
        }
        _ => return None,
    };
    Some(kind.to_string())
}

fn final_answer_artifact_title(kind: &str, fallback_key: &str) -> String {
    match kind {
        "implementation_prompt" => "Implementation prompt".to_string(),
        "issue_comment" => "Issue comment".to_string(),
        "pr_summary" => "PR summary".to_string(),
        "validation_report" => "Validation report".to_string(),
        "browser_verification_report" => "Browser verification report".to_string(),
        _ => fallback_key.replace('_', " "),
    }
}

fn final_answer_artifact_priority(kind: &str) -> usize {
    match kind {
        "implementation_prompt" => 0,
        "issue_comment" => 1,
        "pr_summary" => 2,
        "validation_report" => 3,
        "browser_verification_report" => 4,
        _ => 5,
    }
}

fn normalize_worker_tool_call_value(
    value: &Value,
    support: &WorkerToolSupport,
) -> Result<WorkerAction, WorkerActionParseError> {
    let object = value
        .as_object()
        .ok_or(WorkerActionParseError::InvalidActionShape)?;
    let raw_tool = object
        .get("tool")
        .or_else(|| object.get("tool_name"))
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)
        .ok_or(WorkerActionParseError::InvalidActionShape)?
        .trim();
    if raw_tool.is_empty() {
        return Err(WorkerActionParseError::InvalidActionShape);
    }

    let args = object
        .get("args")
        .or_else(|| object.get("arguments"))
        .cloned()
        .unwrap_or_else(|| {
            let mut inline_args = object.clone();
            inline_args.remove("action");
            inline_args.remove("kind");
            if is_provider_tool_call_type(object.get("type")) {
                inline_args.remove("type");
            }
            inline_args.remove("tool");
            inline_args.remove("tool_name");
            inline_args.remove("name");
            inline_args.remove("summary");
            inline_args.remove("reason");
            if inline_args.len() == 1 && inline_args.contains_key("input") {
                inline_args.remove("input").unwrap_or(Value::Null)
            } else {
                Value::Object(inline_args)
            }
        });
    let args = decode_worker_tool_args(args);
    let summary = object
        .get("summary")
        .or_else(|| object.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Run the requested Nucleus action.")
        .to_string();

    let (tool, args) = normalize_worker_tool_name_and_args(raw_tool, args, support)?;
    Ok(WorkerAction::ToolCall {
        summary,
        tool,
        args,
    })
}

fn is_provider_tool_call_type(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "tool_call" | "function_call"
            )
        })
        .unwrap_or(false)
}

fn decode_worker_tool_args(args: Value) -> Value {
    match args {
        Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Value::Object(serde_json::Map::new());
            }

            serde_json::from_str::<Value>(trimmed).unwrap_or(Value::String(value))
        }
        value => value,
    }
}

fn normalize_worker_tool_name_and_args(
    raw_tool: &str,
    args: Value,
    support: &WorkerToolSupport,
) -> Result<(String, Value), WorkerActionParseError> {
    let normalized = raw_tool.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "shell" | "bash" | "terminal" | "command" | "run_command" => {
            Ok(("command.run".to_string(), normalize_shell_tool_args(args)?))
        }
        "read_file" | "fs.read" => Ok(("fs.read_text".to_string(), args)),
        "list_files" | "ls" => Ok(("fs.list".to_string(), args)),
        "search" | "grep" | "ripgrep" => Ok(("rg.search".to_string(), args)),
        "inspect_repo"
        | "inspect_project"
        | "repo_inspect"
        | "project_inspect"
        | "workspace_inspect"
        | "workspace_inspection"
        | "workspace.inspect" => Ok((
            "project.inspect".to_string(),
            Value::Object(serde_json::Map::new()),
        )),
        "git_status" => Ok(("git.status".to_string(), args)),
        "git_diff" => Ok(("git.diff".to_string(), args)),
        tool if tool.contains('.')
            && (support.is_supported_tool(raw_tool.trim()) || support.is_supported_tool(tool)) =>
        {
            Ok((raw_tool.trim().to_string(), args))
        }
        _ => Err(WorkerActionParseError::UnknownTool {
            tool: raw_tool.to_string(),
        }),
    }
}

fn normalize_shell_tool_args(args: Value) -> Result<Value, WorkerActionParseError> {
    let mut normalized = serde_json::Map::new();
    let object = args.as_object();
    let command_value = object
        .and_then(|object| object.get("command").or_else(|| object.get("input")))
        .unwrap_or(&args);
    if let Some(command) = command_value.as_str().map(str::trim) {
        if command.is_empty() {
            return Err(WorkerActionParseError::InvalidActionShape);
        }

        normalized.insert("command".to_string(), Value::String("sh".to_string()));
        normalized.insert(
            "args".to_string(),
            Value::Array(vec![
                Value::String("-lc".to_string()),
                Value::String(command.to_string()),
            ]),
        );
    } else if let Some(parts) = command_value.as_array() {
        let mut command_parts = parts
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|part| !part.is_empty());
        let command = command_parts
            .next()
            .ok_or(WorkerActionParseError::InvalidActionShape)?;
        normalized.insert("command".to_string(), Value::String(command.to_string()));
        normalized.insert(
            "args".to_string(),
            Value::Array(
                command_parts
                    .map(|part| Value::String(part.to_string()))
                    .collect(),
            ),
        );
    } else {
        return Err(WorkerActionParseError::InvalidActionShape);
    }

    if let Some(object) = object {
        for key in [
            "cwd",
            "workdir",
            "working_dir",
            "timeout_secs",
            "output_limit_bytes",
            "network_policy",
            "env",
        ] {
            if let Some(value) = object.get(key) {
                let normalized_key = match key {
                    "workdir" | "working_dir" => "cwd",
                    _ => key,
                };
                normalized.insert(normalized_key.to_string(), value.clone());
            }
        }
    }

    Ok(Value::Object(normalized))
}

fn is_builtin_nucleus_tool(tool: &str) -> bool {
    if tool.starts_with("mcp.") {
        return true;
    }

    matches!(
        tool,
        "project.inspect"
            | "fs.list"
            | "fs.read_text"
            | "rg.search"
            | "git.status"
            | "git.diff"
            | "github.pr_review_threads"
            | "github.pr_state"
            | "github.comment"
            | "fs.apply_patch"
            | "fs.write_text"
            | "fs.move"
            | "fs.mkdir"
            | "git.stage_patch"
            | "command.run"
            | "python.run"
            | "command.session.open"
            | "command.session.write"
            | "command.session.close"
            | "tests.run"
    )
}

fn excerpt(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    value.chars().take(max_chars).collect::<String>() + "..."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_unknown_provider_tool_as_repairable_contract_error() {
        let error = parse_worker_action(
            r#"{"tool_call":{"tool":"nucleus_repo_search","arguments":{"path":"/tmp","query":"home 404","limit":20}}}"#,
        )
        .expect_err("invented provider-style action should be rejected");

        assert_eq!(
            error,
            WorkerActionParseError::UnknownTool {
                tool: "nucleus_repo_search".to_string()
            }
        );
        assert!(error.is_repairable_contract_error());
    }

    #[test]
    fn classifies_valid_json_wrong_shape_as_contract_error() {
        let error = parse_worker_action(r#"{"message":"I should inspect the repo next"}"#)
            .expect_err("valid JSON without Nucleus action shape should be rejected");

        assert_eq!(error, WorkerActionParseError::InvalidActionShape);
        assert!(error.is_repairable_contract_error());
    }

    #[test]
    fn classifies_malformed_json_as_repairable() {
        let error =
            parse_worker_action(r#"{"kind":"tool_call","summary":"x","tool":"rg.search",}"#)
                .expect_err("malformed JSON should be rejected");

        assert!(matches!(
            error,
            WorkerActionParseError::MalformedJson { .. }
        ));
        assert!(error.is_repairable_contract_error());
    }

    #[test]
    fn parses_canonical_nucleus_action() {
        let action = parse_worker_action(
            r#"{"kind":"tool_call","summary":"search source","tool":"rg.search","args":{"pattern":"home","path":"dga-uhm","limit":20}}"#,
        )
        .expect("canonical Nucleus action should parse");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "search source");
        assert_eq!(tool, "rg.search");
        assert_eq!(args["pattern"], "home");
    }

    #[test]
    fn accepts_registered_mcp_tool_id_without_mcp_prefix() {
        let action = parse_worker_action_with_registered_mcp_tools(
            r#"{"kind":"tool_call","summary":"search cloudflare","tool":"cloudflare-api.search","args":{"query":"workers ai"}}"#,
            ["cloudflare-api.search"],
        )
        .expect("registered non-mcp MCP tool id should parse");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "search cloudflare");
        assert_eq!(tool, "cloudflare-api.search");
        assert_eq!(args["query"], "workers ai");
    }

    #[test]
    fn normalizes_xml_style_registered_tool_call() {
        let action = parse_worker_action_with_registered_mcp_tools(
            r#"<tool_call name="cloudflare-api.search">async () => { return "liite.io"; }</tool_call>"#,
            ["cloudflare-api.search"],
        )
        .expect("xml-style provider-native registered tool call should normalize");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "cloudflare-api.search");
        assert_eq!(args, json!({}));
    }

    #[test]
    fn preserves_provider_native_registered_tool_id() {
        let action = parse_worker_action_with_registered_mcp_tools(
            r#"{"type":"tool_call","name":"cloudflare-api.search","arguments":{"q":"workers ai"}}"#,
            ["cloudflare-api.search"],
        )
        .expect("provider-native registered tool call should preserve the supported tool id");

        let WorkerAction::ToolCall { tool, args, .. } = action else {
            panic!("expected tool call");
        };

        assert_eq!(tool, "cloudflare-api.search");
        assert_eq!(args["q"], "workers ai");
    }

    #[test]
    fn rejects_unregistered_non_mcp_tool_id() {
        let error = parse_worker_action_with_registered_mcp_tools(
            r#"{"kind":"tool_call","summary":"invent","tool":"cloudflare-api.execute","args":{}}"#,
            ["cloudflare-api.search"],
        )
        .expect_err("unregistered non-mcp tool id should still be rejected");

        assert_eq!(
            error,
            WorkerActionParseError::UnknownTool {
                tool: "cloudflare-api.execute".to_string()
            }
        );
    }

    #[test]
    fn rejects_xml_style_unknown_tool_as_repairable_contract_error() {
        let error = parse_worker_action_with_registered_mcp_tools(
            r#"<tool_call name="cloudflare-api.execute">{"script":"lookup"}</tool_call>"#,
            ["cloudflare-api.search"],
        )
        .expect_err("unsupported xml-style provider tool call should fail closed");

        assert_eq!(
            error,
            WorkerActionParseError::UnknownTool {
                tool: "cloudflare-api.execute".to_string()
            }
        );
        assert!(error.is_repairable_contract_error());
    }

    #[test]
    fn recovers_single_supported_tool_call_from_mixed_output() {
        let action = parse_worker_action_with_registered_mcp_tools(
            r#"{"message":"I should check the site first."}
{"tool_call":{"name":"cloudflare-api.search","arguments":{"query":"liite.io dns"}}}"#,
            ["cloudflare-api.search"],
        )
        .expect("exactly one recoverable supported tool call should normalize");

        let WorkerAction::ToolCall { tool, args, .. } = action else {
            panic!("expected tool call");
        };

        assert_eq!(tool, "cloudflare-api.search");
        assert_eq!(args["query"], "liite.io dns");
    }

    #[test]
    fn rejects_ambiguous_mixed_supported_tool_calls() {
        let error = parse_worker_action_with_registered_mcp_tools(
            r#"{"tool_call":{"name":"cloudflare-api.search","arguments":{"query":"liite.io"}}}
{"tool_call":{"name":"rg.search","arguments":{"pattern":"liite","path":"."}}}"#,
            ["cloudflare-api.search"],
        )
        .expect_err("multiple recoverable supported tool calls should fail closed");

        assert_eq!(error, WorkerActionParseError::InvalidActionShape);
    }

    #[test]
    fn canonical_tool_call_with_extra_final_answer_stays_tool_call() {
        let action = parse_worker_action(
            r#"{"kind":"tool_call","summary":"search source","tool":"rg.search","args":{"pattern":"home","path":"dga-uhm","limit":20},"final_answer":"Search after this tool call."}"#,
        )
        .expect("canonical tool call should not be normalized as final answer");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "search source");
        assert_eq!(tool, "rg.search");
        assert_eq!(args["pattern"], "home");
    }

    #[test]
    fn accepts_final_answer_without_kind_as_bounded_compatibility() {
        let action = parse_worker_action(
            r#"{"summary":"diagnosed homepage redirect","final_answer":"The homepage is redirecting because the CMS entry is missing."}"#,
        )
        .expect("final_answer-only object should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "diagnosed homepage redirect");
        assert_eq!(
            final_answer,
            "The homepage is redirecting because the CMS entry is missing."
        );
    }

    #[test]
    fn canonical_final_answer_routes_through_metadata_normalization() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","summary":"published","final_answer":"Done.","publication_status":"opened","implementation_prompt":"Implement the reviewed change."}"#,
        )
        .expect("canonical final answer with extra fields should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            metadata,
            artifacts,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "published");
        assert_eq!(final_answer, "Done.");
        assert_eq!(metadata["publication_status"], "opened");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "implementation_prompt");
        assert_eq!(artifacts[0].content, "Implement the reviewed change.");
    }

    #[test]
    fn canonical_final_answer_artifacts_array_stays_artifacts() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","summary":"generated","final_answer":"Done.","artifacts":[{"kind":"implementation_prompt","title":"Implementation prompt","content":"Implement issue #209.","metadata":{"source":"worker"}},{"kind":"issue_comment","content":"Ready to post.","target":"issue-209"}]}"#,
        )
        .expect("canonical final answer artifacts should normalize");

        let WorkerAction::FinalAnswer {
            metadata,
            artifacts,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert!(metadata.get("artifacts").is_none());
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].kind, "implementation_prompt");
        assert_eq!(artifacts[0].title, "Implementation prompt");
        assert_eq!(artifacts[0].content, "Implement issue #209.");
        assert_eq!(artifacts[0].metadata["source"], "worker");
        assert_eq!(artifacts[1].kind, "issue_comment");
        assert_eq!(artifacts[1].title, "Issue comment");
        assert_eq!(artifacts[1].content, "Ready to post.");
        assert_eq!(artifacts[1].metadata["target"], "issue-209");
    }

    #[test]
    fn accepts_action_final_answer_content_as_bounded_compatibility() {
        let action = parse_worker_action(
            r#"{"action":"final_answer","content":"Yes—I’m here. How can I help?"}"#,
        )
        .expect("action/content final answer should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "The work is done.");
        assert_eq!(final_answer, "Yes—I’m here. How can I help?");
    }

    #[test]
    fn accepts_action_final_answer_answer_as_bounded_compatibility() {
        let action = parse_worker_action(
            r#"{"action":"final_answer","answer":"Yes — I can help with EmDash."}"#,
        )
        .expect("action/answer final answer should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "The work is done.");
        assert_eq!(final_answer, "Yes — I can help with EmDash.");
    }

    #[test]
    fn accepts_type_final_answer_content_as_bounded_compatibility() {
        let action = parse_worker_action(
            r#"{"type":"final_answer","content":"Yes—I can assist with EmDash."}"#,
        )
        .expect("type/content final answer should normalize");

        let WorkerAction::FinalAnswer { final_answer, .. } = action else {
            panic!("expected final answer");
        };

        assert_eq!(final_answer, "Yes—I can assist with EmDash.");
    }

    #[test]
    fn accepts_kind_final_answer_text_as_bounded_compatibility() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","text":"The requested work is in the local branch."}"#,
        )
        .expect("kind/text final answer should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "The work is done.");
        assert_eq!(final_answer, "The requested work is in the local branch.");
    }

    #[test]
    fn accepts_kind_final_answer_message_as_bounded_compatibility() {
        let action =
            parse_worker_action(r#"{"kind":"final_answer","message":"Completed and released."}"#)
                .expect("kind/message final answer should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "The work is done.");
        assert_eq!(final_answer, "Completed and released.");
    }

    #[test]
    fn final_answer_message_field_renders_as_primary_body() {
        let action = parse_worker_action(r#"{"kind":"final_answer","message":"Understood."}"#)
            .expect("kind/message final answer should normalize");

        let WorkerAction::FinalAnswer { final_answer, .. } = action else {
            panic!("expected final answer");
        };

        assert_eq!(final_answer, "Understood.");
        assert!(!final_answer.contains("Message:"));
    }

    #[test]
    fn nested_final_answer_message_field_renders_as_primary_body() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","final_answer":{"message":"Understood."}}"#,
        )
        .expect("nested message final answer should normalize");

        let WorkerAction::FinalAnswer { final_answer, .. } = action else {
            panic!("expected final answer");
        };

        assert_eq!(final_answer, "Understood.");
        assert!(!final_answer.contains("Message:"));
    }

    #[test]
    fn structured_final_answer_keeps_metadata_out_of_visible_body() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","final_answer":{"message":"Published the PR.","publication_status":"published","publication_summary":"Opened a ready PR against dev.","pr_url":"https://github.com/WebLime-agency/nucleus/pull/202","source_branch":"codex/issue-202-final-answer-normalization","target_branch":"dev","validation_status":"passed","browser_verification_status":"not_required","cleanup_status":"clean"}}"#,
        )
        .expect("structured final answer should normalize");

        let WorkerAction::FinalAnswer {
            final_answer,
            metadata,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(final_answer, "Published the PR.");
        assert!(!final_answer.contains("Message:"));
        assert_eq!(metadata["publication_status"], "published");
        assert_eq!(
            metadata["publication_summary"],
            "Opened a ready PR against dev."
        );
        assert_eq!(
            metadata["pr_url"],
            "https://github.com/WebLime-agency/nucleus/pull/202"
        );
        assert_eq!(
            metadata["source_branch"],
            "codex/issue-202-final-answer-normalization"
        );
        assert_eq!(metadata["target_branch"], "dev");
        assert_eq!(metadata["validation_status"], "passed");
        assert_eq!(metadata["browser_verification_status"], "not_required");
        assert_eq!(metadata["cleanup_status"], "clean");
    }

    #[test]
    fn structured_final_answer_array_fields_become_metadata() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","final_answer":{"message":"Validation complete.","validation":["cargo test -p nucleus-daemon worker_action passed","cargo fmt --all --check passed"],"remaining":["Wait for CI","Do not merge"],"next":["Open review","Address feedback if needed"]}}"#,
        )
        .expect("structured final answer with arrays should normalize");

        let WorkerAction::FinalAnswer {
            final_answer,
            metadata,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(final_answer, "Validation complete.");
        assert_eq!(
            metadata["validation"][0],
            "cargo test -p nucleus-daemon worker_action passed"
        );
        assert_eq!(metadata["remaining"][0], "Wait for CI");
        assert_eq!(metadata["next"][1], "Address feedback if needed");
    }

    #[test]
    fn accepts_nested_structured_final_answer_object() {
        let action = parse_worker_action(
            r#"{"final_answer":{"status":"blocked_without_browser_verification","summary":"Code validation passed but browser verification was unavailable.","message":"PR publication is blocked until rendered UI behavior is verified.","validation":["cargo test -p nucleus-daemon worker_action passed","cargo fmt --all --check passed"],"browser_verification_status":"unavailable","remaining":["Verify the UI through the daemon-owned Browser runtime"],"cleanup_status":"clean"}}"#,
        )
        .expect("nested final_answer object should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(
            summary,
            "Code validation passed but browser verification was unavailable."
        );
        assert_eq!(
            final_answer,
            "PR publication is blocked until rendered UI behavior is verified."
        );
        assert!(!final_answer.contains("Message:"));
    }

    #[test]
    fn accepts_kind_final_answer_with_nested_message_object() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","final_answer":{"status":"blocked","message":"I cannot honestly open the PR yet because Browser verification did not run.","pr_url":"","publication_status":"blocked","browser_verification_status":"not_performed"}}"#,
        )
        .expect("kind/final_answer object should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
            metadata,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "blocked");
        assert_eq!(
            final_answer,
            "I cannot honestly open the PR yet because Browser verification did not run."
        );
        assert_eq!(metadata["status"], "blocked");
        assert_eq!(metadata["publication_status"], "blocked");
        assert_eq!(metadata["browser_verification_status"], "not_performed");
        assert!(!final_answer.contains("PR URL:"));
    }

    #[test]
    fn structured_final_answer_prompts_and_comments_become_artifacts() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","summary":"Generated handoff text.","final_answer":{"message":"Generated the requested handoff.","implementation_prompt":"Implement issue #123 using the daemon contract.","comment":{"body":"Posted-ready issue comment.","target":"issue-123"}}}"#,
        )
        .expect("structured artifacts should normalize");

        let WorkerAction::FinalAnswer {
            final_answer,
            artifacts,
            ..
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(final_answer, "Generated the requested handoff.");
        assert!(!final_answer.contains("Implementation prompt:"));
        assert!(!final_answer.contains("Comment:"));
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].kind, "implementation_prompt");
        assert_eq!(
            artifacts[0].content,
            "Implement issue #123 using the daemon contract."
        );
        assert_eq!(artifacts[1].kind, "issue_comment");
        assert_eq!(artifacts[1].content, "Posted-ready issue comment.");
        assert_eq!(artifacts[1].metadata["target"], "issue-123");
    }

    #[test]
    fn parses_final_answer_browser_verification_claim() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","summary":"verified UI","final_answer":"Done.","browser_verification":{"status":"passed","summary":"Clicked the dropdown.","artifact_ids":["artifact-1"]}}"#,
        )
        .expect("browser verification claim should parse");

        let WorkerAction::FinalAnswer {
            browser_verification: Some(claim),
            metadata,
            ..
        } = action
        else {
            panic!("expected final answer with browser verification");
        };

        assert_eq!(claim.status, "passed");
        assert_eq!(claim.summary, "Clicked the dropdown.");
        assert_eq!(claim.artifact_ids, vec!["artifact-1".to_string()]);
        assert_eq!(metadata["browser_verification"]["status"], "passed");
        assert_eq!(
            metadata["browser_verification"]["summary"],
            "Clicked the dropdown."
        );
        assert_eq!(metadata["browser_verification_status"], "passed");
    }

    #[test]
    fn parses_nested_final_answer_browser_verification_claim() {
        let action = parse_worker_action(
            r#"{"kind":"final_answer","summary":"verified UI","final_answer":{"message":"Done.","browser_verification":{"status":"passed","summary":"Clicked the dropdown.","artifact_ids":["artifact-1"]}}}"#,
        )
        .expect("nested browser verification claim should parse");

        let WorkerAction::FinalAnswer {
            final_answer,
            browser_verification: Some(claim),
            metadata,
            ..
        } = action
        else {
            panic!("expected final answer with browser verification");
        };

        assert_eq!(final_answer, "Done.");
        assert_eq!(claim.status, "passed");
        assert_eq!(claim.summary, "Clicked the dropdown.");
        assert_eq!(claim.artifact_ids, vec!["artifact-1".to_string()]);
        assert_eq!(metadata["browser_verification"]["status"], "passed");
        assert_eq!(
            metadata["browser_verification"]["summary"],
            "Clicked the dropdown."
        );
        assert_eq!(metadata["browser_verification_status"], "passed");
    }

    #[test]
    fn parses_progress_update_as_non_terminal_action() {
        let action = parse_worker_action(
            r#"{"kind":"progress_update","summary":"checkpoint saved","detail":"Composer extraction is complete; sidebar extraction remains."}"#,
        )
        .expect("progress_update should parse");

        let WorkerAction::ProgressUpdate { summary, detail } = action else {
            panic!("expected progress update");
        };

        assert_eq!(summary, "checkpoint saved");
        assert_eq!(
            detail,
            "Composer extraction is complete; sidebar extraction remains."
        );
    }

    #[test]
    fn accepts_checkpoint_progress_compatibility() {
        let action = parse_worker_action(
            r#"{"action":"checkpoint","content":"Validated current slice; continue with job history."}"#,
        )
        .expect("checkpoint/content progress should normalize");

        let WorkerAction::ProgressUpdate { summary, detail } = action else {
            panic!("expected progress update");
        };

        assert_eq!(summary, "Recorded a non-terminal progress checkpoint.");
        assert_eq!(
            detail,
            "Validated current slice; continue with job history."
        );
    }

    #[test]
    fn accepts_object_progress_update_compatibility() {
        let action = parse_worker_action(
            r#"{"progress_update":{"status":"in_progress","summary":"checkpoint saved","message":"Phase 4 is not complete yet; continue with the next slice."}}"#,
        )
        .expect("object progress_update should normalize");

        let WorkerAction::ProgressUpdate { summary, detail } = action else {
            panic!("expected progress update");
        };

        assert_eq!(summary, "checkpoint saved");
        assert_eq!(
            detail,
            "Phase 4 is not complete yet; continue with the next slice."
        );
    }

    #[test]
    fn accepts_structured_progress_update_without_message() {
        let action = parse_worker_action(
            r#"{"progress_update":{"status":"partial_success","summary":"Validated another Phase 4 slice.","validated":["npm run check:web","npm run build:web"],"changed_files":["apps/web/src/lib/components/app/workspace/workspace-storage-path-card.svelte"],"next":"Continue with the session workspace decomposition."}}"#,
        )
        .expect("structured progress_update should normalize");

        let WorkerAction::ProgressUpdate { summary, detail } = action else {
            panic!("expected progress update");
        };

        assert_eq!(summary, "Validated another Phase 4 slice.");
        assert!(detail.contains("status: partial_success"));
        assert!(detail.contains("validated:"));
        assert!(detail.contains("Continue with the session workspace decomposition."));
    }

    #[test]
    fn accepts_inspect_repo_as_project_inspect_compatibility() {
        let action = parse_worker_action(
            r#"{"tool_call":{"name":"inspect_repo","arguments":{"cwd":"/tmp/project","targets":["apps/web/src/lib/components"]}}}"#,
        )
        .expect("inspect_repo should normalize to project.inspect");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "project.inspect");
        assert!(args.as_object().is_some_and(|object| object.is_empty()));
    }

    #[test]
    fn accepts_workspace_inspection_as_project_inspect_compatibility() {
        let action = parse_worker_action(
            r#"{"action":"tool_call","tool":"workspace_inspection","args":{"reason":"Need to inspect the repository state and issue context."}}"#,
        )
        .expect("workspace_inspection should normalize to project.inspect");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "project.inspect");
        assert!(args.as_object().is_some_and(|object| object.is_empty()));
    }

    #[test]
    fn recovers_provider_shell_action_with_raw_multiline_command() {
        let action = parse_worker_action(
            "{\"tool_call\":{\"tool\":\"shell\",\"args\":{\"command\":\"cd /tmp && python3 - <<'PY'\nfrom pathlib import Path\ntext = p.read_text()\ntext = text.replace(\n\"old\",\n\"new\"\n)\nPY\"}}}",
        )
        .expect("provider-native shell command should be recovered");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "command.run");
        assert_eq!(args["command"], "sh");
        assert_eq!(args["args"][0], "-lc");
        assert!(
            args["args"][1]
                .as_str()
                .is_some_and(|command| command.contains("text.replace") && command.contains("PY"))
        );
    }

    #[test]
    fn preserves_inline_type_arg_for_mcp_direct_tool_call() {
        let action = parse_worker_action(
            r#"{"tool":"mcp.issue_tracker.create","type":"issue","id":"123","title":"Fix login"}"#,
        )
        .expect("mcp direct tool call should preserve legitimate type arg");

        let WorkerAction::ToolCall {
            summary,
            tool,
            args,
        } = action
        else {
            panic!("expected tool call");
        };

        assert_eq!(summary, "Run the requested Nucleus action.");
        assert_eq!(tool, "mcp.issue_tracker.create");
        assert_eq!(args["type"], "issue");
        assert_eq!(args["id"], "123");
        assert_eq!(args["title"], "Fix login");
    }

    #[test]
    fn spawn_child_jobs_deserializes_optional_route_id() {
        let action = parse_worker_action(
            r#"{"kind":"spawn_child_jobs","summary":"fan out","jobs":[{"title":"Developer","prompt":"Implement the change","working_dir":null,"route_id":"developer-profile"}]}"#,
        )
        .expect("spawn_child_jobs with route_id should parse");

        let WorkerAction::SpawnChildJobs { jobs, .. } = action else {
            panic!("expected spawn_child_jobs");
        };

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].route_id.as_deref(), Some("developer-profile"));
    }

    #[test]
    fn spawn_child_jobs_without_route_id_remains_backward_compatible() {
        let action = parse_worker_action(
            r#"{"kind":"spawn_child_jobs","summary":"fan out","jobs":[{"title":"Inherited","prompt":"Inspect the repo","working_dir":null}]}"#,
        )
        .expect("spawn_child_jobs without route_id should parse");

        let WorkerAction::SpawnChildJobs { jobs, .. } = action else {
            panic!("expected spawn_child_jobs");
        };

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].route_id, None);
    }

    #[test]
    fn wait_until_variants_round_trip() {
        let variants = vec![
            WaitUntil::DelaySeconds { delay_seconds: 2 },
            WaitUntil::AbsoluteUnix {
                absolute_unix: 1_900_000_000,
            },
            WaitUntil::AuditEvent {
                event_kind: "memory.classifier.completed".to_string(),
                target_pattern: Some("session:abc".to_string()),
                status: Some("success".to_string()),
            },
            WaitUntil::ChildJobsCompleted {
                job_ids: vec!["job-a".to_string(), "job-b".to_string()],
            },
            WaitUntil::ArtifactKind {
                job_id: "job-a".to_string(),
                artifact_kind: "child-report".to_string(),
            },
        ];

        for until in variants {
            let encoded = serde_json::to_string(&until).expect("wait until should serialize");
            let decoded: WaitUntil =
                serde_json::from_str(&encoded).expect("wait until should deserialize");
            assert_eq!(decoded, until);
        }
    }
}
