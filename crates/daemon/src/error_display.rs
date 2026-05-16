use nucleus_protocol::UserFacingErrorSummary;
use serde_json::Value;

const OPEN_PROFILES: &str = "open_profiles";
const RETRY_JOB: &str = "retry_job";
const CANCEL_JOB: &str = "cancel_job";
const OPEN_JOB_DETAILS: &str = "open_job_details";

pub(crate) fn classify_user_error(detail: &str) -> Option<UserFacingErrorSummary> {
    let detail = detail.trim();
    if detail.is_empty() {
        return None;
    }

    let signal = ErrorSignal::from_detail(detail);
    let lower = signal.combined_lowercase();

    if contains_missing_credential(&lower) {
        return Some(model_credentials_missing(detail));
    }

    if contains_invalid_credential(&lower) {
        return Some(model_credentials_invalid(detail));
    }

    if contains_auth_failure(&lower) {
        return Some(model_provider_auth_failed(detail));
    }

    if lower.contains("openai-compatible sessions require a base url")
        || lower.contains("model base url is required for openai-compatible adapters")
    {
        return Some(model_config_incomplete(
            detail,
            "Nucleus needs an OpenAI-compatible base URL before it can run this session.",
        ));
    }

    if lower.contains("openai-compatible sessions require a model name")
        || lower.contains("model name is required for openai-compatible adapters")
    {
        return Some(model_config_incomplete(
            detail,
            "Nucleus needs a model name before it can run this session.",
        ));
    }

    if lower.contains("unknown workspace profile")
        || lower.contains("workspace profile") && lower.contains("was not found")
    {
        return Some(model_profile_missing(detail));
    }

    if lower.contains("failed to reach the openai-compatible endpoint")
        || lower.contains("connection refused")
        || lower.contains("dns error")
        || lower.contains("timed out")
    {
        return Some(model_endpoint_unreachable(detail));
    }

    None
}

fn model_credentials_missing(detail: &str) -> UserFacingErrorSummary {
    UserFacingErrorSummary {
        code: "model_credentials_missing".to_string(),
        title: "Nucleus needs model credentials".to_string(),
        message:
            "Set up your Base model and Utility model credentials in Profiles, then retry this job."
                .to_string(),
        actions: model_setup_actions(),
        technical_detail: redact_likely_secret_tokens(detail),
    }
}

fn model_credentials_invalid(detail: &str) -> UserFacingErrorSummary {
    UserFacingErrorSummary {
        code: "model_credentials_invalid".to_string(),
        title: "Model credentials were rejected".to_string(),
        message:
            "Check the Base model and Utility model credentials in Profiles, then retry this job."
                .to_string(),
        actions: model_setup_actions(),
        technical_detail: redact_likely_secret_tokens(detail),
    }
}

fn model_provider_auth_failed(detail: &str) -> UserFacingErrorSummary {
    UserFacingErrorSummary {
        code: "model_provider_auth_failed".to_string(),
        title: "The model provider rejected the request".to_string(),
        message:
            "Check the selected profile credentials for the Base model and Utility model, then retry this job."
                .to_string(),
        actions: model_setup_actions(),
        technical_detail: redact_likely_secret_tokens(detail),
    }
}

fn model_profile_missing(detail: &str) -> UserFacingErrorSummary {
    UserFacingErrorSummary {
        code: "model_profile_missing".to_string(),
        title: "The selected model profile is missing".to_string(),
        message:
            "Choose or recreate the Base and Utility model profile in Profiles, then retry this job."
                .to_string(),
        actions: model_setup_actions(),
        technical_detail: redact_likely_secret_tokens(detail),
    }
}

fn model_config_incomplete(detail: &str, lead: &str) -> UserFacingErrorSummary {
    UserFacingErrorSummary {
        code: "model_config_incomplete".to_string(),
        title: "Model profile setup is incomplete".to_string(),
        message: format!(
            "{lead} Update your Base model and Utility model in Profiles, then retry this job."
        ),
        actions: model_setup_actions(),
        technical_detail: redact_likely_secret_tokens(detail),
    }
}

fn model_endpoint_unreachable(detail: &str) -> UserFacingErrorSummary {
    UserFacingErrorSummary {
        code: "model_endpoint_unreachable".to_string(),
        title: "Nucleus could not reach the model endpoint".to_string(),
        message:
            "Check the profile base URL for the Base model and Utility model, then retry this job."
                .to_string(),
        actions: model_setup_actions(),
        technical_detail: redact_likely_secret_tokens(detail),
    }
}

fn model_setup_actions() -> Vec<String> {
    [OPEN_PROFILES, RETRY_JOB, CANCEL_JOB, OPEN_JOB_DETAILS]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn redact_likely_secret_tokens(detail: &str) -> String {
    detail
        .split_whitespace()
        .map(|token| {
            let trimmed = token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '"' | '\'' | ',' | ':' | ';' | ')' | '(' | ']' | '[' | '}' | '{'
                )
            });
            if is_likely_secret_token(trimmed) {
                token.replace(trimmed, "[REDACTED_SECRET]")
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_likely_secret_token(token: &str) -> bool {
    let lower = token.to_lowercase();
    token.len() >= 10
        && (lower.starts_with("sk-")
            || lower.starts_with("sk_")
            || lower.starts_with("xox")
            || lower.starts_with("ghp_")
            || lower.starts_with("github_pat_"))
}

fn contains_missing_credential(lower: &str) -> bool {
    (lower.contains("missing api key")
        || lower.contains("api key missing")
        || lower.contains("no api key"))
        && contains_auth_context(lower)
}

fn contains_invalid_credential(lower: &str) -> bool {
    lower.contains("invalid_api_key")
        || lower.contains("invalid api key")
        || lower.contains("incorrect api key")
        || lower.contains("api key is invalid")
}

fn contains_auth_failure(lower: &str) -> bool {
    lower.contains("authentication_error")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("http 401")
        || lower.contains("http 403")
        || lower.contains("status 401")
        || lower.contains("status 403")
}

fn contains_auth_context(lower: &str) -> bool {
    lower.contains("auth")
        || lower.contains("credential")
        || lower.contains("api key")
        || lower.contains("openai-compatible")
}

#[derive(Debug)]
struct ErrorSignal {
    detail: String,
    json_text: String,
}

impl ErrorSignal {
    fn from_detail(detail: &str) -> Self {
        let json_text = extract_json_value(detail)
            .map(|value| flatten_json_strings(&value).join(" "))
            .unwrap_or_default();
        Self {
            detail: detail.to_string(),
            json_text,
        }
    }

    fn combined_lowercase(&self) -> String {
        format!("{} {}", self.detail, self.json_text).to_lowercase()
    }
}

fn extract_json_value(detail: &str) -> Option<Value> {
    let start = detail
        .char_indices()
        .find_map(|(index, character)| matches!(character, '{' | '[').then_some(index))?;
    serde_json::from_str(&detail[start..]).ok()
}

fn flatten_json_strings(value: &Value) -> Vec<String> {
    match value {
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, value)| {
                let mut values = vec![key.clone()];
                values.extend(flatten_json_strings(value));
                values
            })
            .collect(),
        Value::Array(items) => items.iter().flat_map(flatten_json_strings).collect(),
        Value::String(value) => vec![value.clone()],
        Value::Number(value) => vec![value.to_string()],
        Value::Bool(value) => vec![value.to_string()],
        Value::Null => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_exact_missing_api_key_issue_example() {
        let raw = r#"OpenAI-compatible endpoint failed: {"error":{"message":"Missing API key","type":"authentication_error","code":"invalid_api_key"}}"#;

        let summary = classify_user_error(raw).expect("error should classify");

        assert_eq!(summary.code, "model_credentials_missing");
        assert!(summary.title.contains("model credentials"));
        assert!(summary.message.contains("Base model"));
        assert!(summary.message.contains("Utility model"));
        assert!(summary.actions.iter().any(|action| action == OPEN_PROFILES));
        assert!(summary.actions.iter().any(|action| action == RETRY_JOB));
        assert_eq!(summary.technical_detail, raw);
        assert!(!summary.title.contains('{'));
        assert!(!summary.message.contains("invalid_api_key"));
    }

    #[test]
    fn classifies_401_auth_json() {
        let raw = r#"OpenAI-compatible endpoint failed (HTTP 401): {"error":{"message":"Authentication failed","type":"authentication_error"}}"#;

        let summary = classify_user_error(raw).expect("error should classify");

        assert_eq!(summary.code, "model_provider_auth_failed");
        assert!(summary.message.contains("profile"));
        assert_eq!(summary.technical_detail, raw);
    }

    #[test]
    fn classifies_403_auth_json() {
        let raw =
            r#"OpenAI-compatible endpoint failed (HTTP 403): {"error":{"message":"Forbidden"}}"#;

        let summary = classify_user_error(raw).expect("error should classify");

        assert_eq!(summary.code, "model_provider_auth_failed");
        assert!(summary.actions.iter().any(|action| action == CANCEL_JOB));
    }

    #[test]
    fn classifies_missing_openai_base_url() {
        let raw = "OpenAI-compatible sessions require a base URL";

        let summary = classify_user_error(raw).expect("error should classify");

        assert_eq!(summary.code, "model_config_incomplete");
        assert!(summary.message.contains("base URL"));
        assert!(summary.message.contains("Profiles"));
    }

    #[test]
    fn classifies_missing_openai_model_name() {
        let raw = "OpenAI-compatible sessions require a model name";

        let summary = classify_user_error(raw).expect("error should classify");

        assert_eq!(summary.code, "model_config_incomplete");
        assert!(summary.message.contains("model name"));
        assert!(summary.message.contains("Profiles"));
    }

    #[test]
    fn friendly_copy_does_not_include_raw_json_or_secret_values() {
        let raw = r#"OpenAI-compatible endpoint failed (HTTP 401): {"error":{"message":"invalid_api_key sk-test-secret","type":"authentication_error","code":"invalid_api_key"}}"#;

        let summary = classify_user_error(raw).expect("error should classify");

        assert_eq!(summary.code, "model_credentials_invalid");
        assert!(!summary.title.contains("sk-test-secret"));
        assert!(!summary.message.contains("sk-test-secret"));
        assert!(!summary.title.contains('{'));
        assert!(!summary.message.contains('{'));
        assert!(!summary.technical_detail.contains("sk-test-secret"));
        assert!(summary.technical_detail.contains("[REDACTED_SECRET]"));
    }
}
