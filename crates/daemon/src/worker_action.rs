use std::{error::Error, fmt};

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
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
    FinalAnswer {
        summary: String,
        final_answer: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChildJobProposal {
    pub title: String,
    pub prompt: String,
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerActionParseError {
    NoJsonObject,
    MalformedJson { detail: String },
    InvalidActionShape,
    UnknownTool { tool: String },
}

impl WorkerActionParseError {
    pub fn is_repairable_json_error(&self) -> bool {
        matches!(
            self,
            WorkerActionParseError::NoJsonObject | WorkerActionParseError::MalformedJson { .. }
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
    let trimmed = content.trim();
    if let Ok(parsed) = serde_json::from_str::<WorkerAction>(trimmed) {
        return validate_worker_action(parsed);
    }

    let start = trimmed
        .find('{')
        .ok_or(WorkerActionParseError::NoJsonObject)?;
    let end = trimmed
        .rfind('}')
        .ok_or(WorkerActionParseError::NoJsonObject)?;
    let candidate = &trimmed[start..=end];

    let value = match parse_worker_action_value(candidate) {
        Ok(value) => value,
        Err(error) => {
            if let Some(action) = recover_provider_shell_action(candidate)? {
                return validate_worker_action(action);
            }
            return Err(error);
        }
    };
    if let Some(action) = normalize_worker_action_value(&value)? {
        return validate_worker_action(action);
    }

    match serde_json::from_str::<WorkerAction>(candidate) {
        Ok(parsed) => validate_worker_action(parsed),
        Err(_error) if serde_json::from_str::<Value>(candidate).is_ok() => {
            Err(WorkerActionParseError::InvalidActionShape)
        }
        Err(error) => Err(WorkerActionParseError::MalformedJson {
            detail: excerpt(&error.to_string(), 220),
        }),
    }
}

fn recover_provider_shell_action(
    candidate: &str,
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

    normalize_worker_tool_name_and_args("shell", Value::Object(args)).map(|(tool, args)| {
        Some(WorkerAction::ToolCall {
            summary: "Run the requested Nucleus action.".to_string(),
            tool,
            args,
        })
    })
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

fn validate_worker_action(action: WorkerAction) -> Result<WorkerAction, WorkerActionParseError> {
    if let WorkerAction::ToolCall { tool, .. } = &action {
        if !is_supported_nucleus_tool(tool) {
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
        .and_then(Value::as_str)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "progress_update" || value == "checkpoint"
        })
        .unwrap_or(false)
    {
        return normalize_worker_progress_update_value(object).map(Some);
    }

    if object.contains_key("final_answer") {
        return normalize_worker_final_answer_value(object).map(Some);
    }

    if object
        .get("action")
        .or_else(|| object.get("kind"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("final_answer"))
        .unwrap_or(false)
    {
        return normalize_worker_final_answer_value(object).map(Some);
    }

    if let Some(tool_call) = object.get("tool_call") {
        return normalize_worker_tool_call_value(tool_call).map(Some);
    }

    if let Some(function_call) = object.get("function_call") {
        return normalize_worker_tool_call_value(function_call).map(Some);
    }

    if object
        .get("action")
        .or_else(|| object.get("kind"))
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("tool_call"))
        .unwrap_or(false)
        && (object.contains_key("tool")
            || object.contains_key("tool_name")
            || object.contains_key("name"))
    {
        return normalize_worker_tool_call_value(value).map(Some);
    }

    if object.contains_key("tool")
        || object.contains_key("tool_name")
        || object.contains_key("name")
    {
        return normalize_worker_tool_call_value(value).map(Some);
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
        .ok_or(WorkerActionParseError::InvalidActionShape)?
        .to_string();
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

fn normalize_worker_final_answer_value(
    object: &serde_json::Map<String, Value>,
) -> Result<WorkerAction, WorkerActionParseError> {
    let final_answer = object
        .get("final_answer")
        .or_else(|| object.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(WorkerActionParseError::InvalidActionShape)?
        .to_string();
    let summary = object
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("The work is done.")
        .to_string();

    Ok(WorkerAction::FinalAnswer {
        summary,
        final_answer,
    })
}

fn normalize_worker_tool_call_value(value: &Value) -> Result<WorkerAction, WorkerActionParseError> {
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

    let (tool, args) = normalize_worker_tool_name_and_args(raw_tool, args)?;
    Ok(WorkerAction::ToolCall {
        summary,
        tool,
        args,
    })
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
) -> Result<(String, Value), WorkerActionParseError> {
    let normalized = raw_tool.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "shell" | "bash" | "terminal" | "command" | "run_command" => {
            Ok(("command.run".to_string(), normalize_shell_tool_args(args)?))
        }
        "read_file" | "fs.read" => Ok(("fs.read_text".to_string(), args)),
        "list_files" | "ls" => Ok(("fs.list".to_string(), args)),
        "search" | "grep" | "ripgrep" => Ok(("rg.search".to_string(), args)),
        "inspect_repo" | "inspect_project" | "repo_inspect" | "project_inspect" => Ok((
            "project.inspect".to_string(),
            Value::Object(serde_json::Map::new()),
        )),
        "git_status" => Ok(("git.status".to_string(), args)),
        "git_diff" => Ok(("git.diff".to_string(), args)),
        tool if tool.contains('.') && is_supported_nucleus_tool(tool) => {
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

fn is_supported_nucleus_tool(tool: &str) -> bool {
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
            | "fs.apply_patch"
            | "fs.write_text"
            | "fs.move"
            | "fs.mkdir"
            | "git.stage_patch"
            | "command.run"
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
    fn classifies_unknown_provider_tool_without_repairing() {
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
        assert!(!error.is_repairable_json_error());
    }

    #[test]
    fn classifies_valid_json_wrong_shape_as_contract_error() {
        let error = parse_worker_action(r#"{"message":"I should inspect the repo next"}"#)
            .expect_err("valid JSON without Nucleus action shape should be rejected");

        assert_eq!(error, WorkerActionParseError::InvalidActionShape);
        assert!(!error.is_repairable_json_error());
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
        assert!(error.is_repairable_json_error());
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
    fn accepts_final_answer_without_kind_as_bounded_compatibility() {
        let action = parse_worker_action(
            r#"{"summary":"diagnosed homepage redirect","final_answer":"The homepage is redirecting because the CMS entry is missing."}"#,
        )
        .expect("final_answer-only object should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
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
    fn accepts_action_final_answer_content_as_bounded_compatibility() {
        let action = parse_worker_action(
            r#"{"action":"final_answer","content":"Yes—I’m here. How can I help?"}"#,
        )
        .expect("action/content final answer should normalize");

        let WorkerAction::FinalAnswer {
            summary,
            final_answer,
        } = action
        else {
            panic!("expected final answer");
        };

        assert_eq!(summary, "The work is done.");
        assert_eq!(final_answer, "Yes—I’m here. How can I help?");
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
}
