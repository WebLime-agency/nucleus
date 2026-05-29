use std::{error::Error, fmt, time::Duration};

pub(crate) const MAX_RETRY_ATTEMPTS: u32 = 5;
#[cfg(not(test))]
pub(crate) const BASE_RETRY_BACKOFF: Duration = Duration::from_secs(1);
#[cfg(test)]
pub(crate) const BASE_RETRY_BACKOFF: Duration = Duration::from_millis(10);
pub(crate) const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetryDecision {
    Retry { backoff: Duration },
    GiveUp { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderTransportError {
    Http {
        status: u16,
        detail: String,
        retry_after: Option<Duration>,
    },
    Stream {
        detail: String,
    },
}

impl ProviderTransportError {
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        match self {
            ProviderTransportError::Http { retry_after, .. } => *retry_after,
            ProviderTransportError::Stream { .. } => None,
        }
    }
}

impl fmt::Display for ProviderTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderTransportError::Http { status, detail, .. } => {
                write!(
                    formatter,
                    "OpenAI-compatible endpoint failed (HTTP {status}): {detail}"
                )
            }
            ProviderTransportError::Stream { detail } => {
                write!(formatter, "OpenAI-compatible stream failed: {detail}")
            }
        }
    }
}

impl Error for ProviderTransportError {}

pub(crate) fn classify_provider_error(
    err: &anyhow::Error,
    attempt: u32,
    retry_after: Option<Duration>,
) -> RetryDecision {
    if attempt > MAX_RETRY_ATTEMPTS {
        return RetryDecision::GiveUp {
            reason: format!("reached max retry attempts ({MAX_RETRY_ATTEMPTS})"),
        };
    }

    if let Some(provider_error) = err.downcast_ref::<ProviderTransportError>() {
        match provider_error {
            ProviderTransportError::Http { status, .. } => {
                return classify_http_status(*status, attempt, retry_after);
            }
            ProviderTransportError::Stream { detail } => {
                if is_retryable_transport_text(detail) {
                    return RetryDecision::Retry {
                        backoff: exponential_backoff(attempt, retry_after),
                    };
                }
                return RetryDecision::GiveUp {
                    reason: "provider stream returned malformed data".to_string(),
                };
            }
        }
    }

    for cause in err.chain() {
        if let Some(reqwest_error) = cause.downcast_ref::<reqwest::Error>() {
            if reqwest_error.is_timeout()
                || reqwest_error.is_connect()
                || reqwest_error.is_request()
            {
                return RetryDecision::Retry {
                    backoff: exponential_backoff(attempt, retry_after),
                };
            }
            if let Some(status) = reqwest_error.status() {
                return classify_http_status(status.as_u16(), attempt, retry_after);
            }
        }
    }

    let text = err.to_string();
    if is_retryable_transport_text(&text) {
        return RetryDecision::Retry {
            backoff: exponential_backoff(attempt, retry_after),
        };
    }

    RetryDecision::GiveUp {
        reason: "provider error is not retryable".to_string(),
    }
}

pub(crate) fn provider_error_class(err: &anyhow::Error) -> String {
    if let Some(provider_error) = err.downcast_ref::<ProviderTransportError>() {
        return match provider_error {
            ProviderTransportError::Http { status, .. } => format!("http_{status}"),
            ProviderTransportError::Stream { detail } if is_retryable_transport_text(detail) => {
                "stream_eof".to_string()
            }
            ProviderTransportError::Stream { .. } => "stream_malformed".to_string(),
        };
    }

    for cause in err.chain() {
        if let Some(reqwest_error) = cause.downcast_ref::<reqwest::Error>() {
            if reqwest_error.is_timeout() {
                return "timeout".to_string();
            }
            if reqwest_error.is_connect() {
                return "connection".to_string();
            }
            if let Some(status) = reqwest_error.status() {
                return format!("http_{}", status.as_u16());
            }
        }
    }

    let text = err.to_string().to_ascii_lowercase();
    if text.contains("timeout") || text.contains("timed out") {
        "timeout".to_string()
    } else if text.contains("eof") || text.contains("stream") {
        "stream_eof".to_string()
    } else if text.contains("connection") || text.contains("connect") {
        "connection".to_string()
    } else {
        "unknown".to_string()
    }
}

pub(crate) fn retry_after_from_error(err: &anyhow::Error) -> Option<Duration> {
    err.downcast_ref::<ProviderTransportError>()
        .and_then(ProviderTransportError::retry_after)
}

fn classify_http_status(status: u16, attempt: u32, retry_after: Option<Duration>) -> RetryDecision {
    match status {
        408 | 425 | 429 => RetryDecision::Retry {
            backoff: exponential_backoff(attempt, retry_after),
        },
        500..=599 => RetryDecision::Retry {
            backoff: exponential_backoff(attempt, retry_after),
        },
        401 | 403 => RetryDecision::GiveUp {
            reason: format!("provider rejected credentials or authorization (HTTP {status})"),
        },
        400..=499 => RetryDecision::GiveUp {
            reason: format!("provider returned non-retryable HTTP {status}"),
        },
        _ => RetryDecision::GiveUp {
            reason: format!("provider returned non-retryable HTTP {status}"),
        },
    }
}

fn exponential_backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(retry_after) = retry_after {
        return retry_after.min(MAX_RETRY_BACKOFF);
    }

    let multiplier = 1_u32
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    BASE_RETRY_BACKOFF
        .saturating_mul(multiplier)
        .min(MAX_RETRY_BACKOFF)
}

fn is_retryable_transport_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("connect")
        || lower.contains("eof")
        || lower.contains("stream ended before")
        || lower.contains("stream complete")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    fn http_error(status: u16, retry_after: Option<Duration>) -> anyhow::Error {
        anyhow!(ProviderTransportError::Http {
            status,
            detail: "test".to_string(),
            retry_after,
        })
    }

    #[test]
    fn retries_transient_http_statuses() {
        for status in [408, 425, 429, 500, 502, 503, 504] {
            assert!(matches!(
                classify_provider_error(&http_error(status, None), 1, None),
                RetryDecision::Retry { .. }
            ));
        }
    }

    #[test]
    fn honors_retry_after_for_rate_limit() {
        let backoff = Duration::from_secs(5);
        assert_eq!(
            classify_provider_error(&http_error(429, Some(backoff)), 1, Some(backoff)),
            RetryDecision::Retry { backoff }
        );
    }

    #[test]
    fn honors_retry_after_for_transient_server_errors() {
        let backoff = Duration::from_secs(12);
        assert_eq!(
            classify_provider_error(&http_error(503, Some(backoff)), 1, Some(backoff)),
            RetryDecision::Retry { backoff }
        );
    }

    #[test]
    fn clamps_retry_after_to_cap() {
        assert_eq!(
            classify_provider_error(
                &http_error(429, Some(Duration::from_secs(600))),
                1,
                Some(Duration::from_secs(600)),
            ),
            RetryDecision::Retry {
                backoff: MAX_RETRY_BACKOFF
            }
        );
    }

    #[test]
    fn gives_up_on_auth_and_other_client_errors() {
        for status in [400, 401, 403, 404, 422] {
            assert!(matches!(
                classify_provider_error(&http_error(status, None), 1, None),
                RetryDecision::GiveUp { .. }
            ));
        }
    }

    #[test]
    fn retries_connection_timeout_and_eof_text() {
        for message in [
            "connection reset by peer",
            "request timed out",
            "EOF before stream complete",
            "stream completed with empty response",
        ] {
            assert!(matches!(
                classify_provider_error(&anyhow!(message), 1, None),
                RetryDecision::Retry { .. }
            ));
        }
    }

    #[test]
    fn retries_empty_completed_provider_stream() {
        let error = anyhow!(ProviderTransportError::Stream {
            detail: "stream completed with empty response".to_string(),
        });

        assert!(matches!(
            classify_provider_error(&error, 1, None),
            RetryDecision::Retry { .. }
        ));
        assert_eq!(provider_error_class(&error), "stream_eof");
    }

    #[test]
    fn gives_up_after_max_retry_attempts() {
        assert!(matches!(
            classify_provider_error(&http_error(500, None), MAX_RETRY_ATTEMPTS + 1, None),
            RetryDecision::GiveUp { reason } if reason.contains("max retry")
        ));
    }
}
