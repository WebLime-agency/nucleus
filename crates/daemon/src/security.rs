use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use nucleus_protocol::{LocalInterfaceSummary, SecurityPostureSummary};
use serde_json::Value;
use url::Url;

const REDACTED: &str = "[REDACTED_SECRET]";
const SENSITIVE_FIELD_NAMES: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "api_key",
    "secret",
    "password",
    "private_key",
    "client_secret",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginSafety {
    pub origin: String,
    pub safe: bool,
    pub reason: String,
    pub https: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RedactionSet {
    exact_values: Vec<String>,
}

impl RedactionSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_secret(mut self, value: impl Into<String>) -> Self {
        self.register_secret(value);
        self
    }

    pub fn register_secret(&mut self, value: impl Into<String>) {
        let value = value.into();
        if value.trim().len() >= 4 && !self.exact_values.iter().any(|item| item == &value) {
            self.exact_values.push(value);
        }
    }

    pub fn redact_text(&self, input: &str) -> String {
        let mut redacted = redact_pem_blocks(input);
        redacted = redact_urls_with_credentials(&redacted);
        for secret in &self.exact_values {
            redacted = redacted.replace(secret, REDACTED);
        }
        redacted
    }

    pub fn redact_json(&self, value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        if is_sensitive_field_name(key) {
                            (key.clone(), Value::String(REDACTED.to_string()))
                        } else {
                            (key.clone(), self.redact_json(value))
                        }
                    })
                    .collect(),
            ),
            Value::Array(values) => {
                Value::Array(values.iter().map(|value| self.redact_json(value)).collect())
            }
            Value::String(value) => Value::String(self.redact_text(value)),
            other => other.clone(),
        }
    }

    pub fn redact_headers(&self, headers: &HeaderMap) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, value)| {
                let name_text = name.as_str().to_string();
                let value_text = value.to_str().unwrap_or("<non-utf8>");
                if is_sensitive_header_name(&name_text) {
                    (name_text, REDACTED.to_string())
                } else {
                    (name_text, self.redact_text(value_text))
                }
            })
            .collect()
    }
}

pub fn classify_request_origin(headers: &HeaderMap) -> OriginSafety {
    let origin = headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .or_else(|| headers.get("referer").and_then(|value| value.to_str().ok()))
        .unwrap_or("");

    if !origin.trim().is_empty() {
        return classify_origin(origin);
    }

    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if host.trim().is_empty() {
        return OriginSafety {
            origin: String::new(),
            safe: false,
            reason: "missing origin and host headers".to_string(),
            https: false,
        };
    }

    classify_origin(&format!("http://{host}"))
}

pub fn classify_origin(origin: &str) -> OriginSafety {
    let trimmed = origin.trim();
    let parsed = Url::parse(trimmed);
    let Ok(url) = parsed else {
        return OriginSafety {
            origin: trimmed.to_string(),
            safe: false,
            reason: "origin is not a valid URL".to_string(),
            https: false,
        };
    };

    let scheme = url.scheme();
    let https = scheme.eq_ignore_ascii_case("https");
    if https {
        return OriginSafety {
            origin: origin_without_path(&url),
            safe: true,
            reason: "HTTPS origin".to_string(),
            https: true,
        };
    }

    if !scheme.eq_ignore_ascii_case("http") {
        return OriginSafety {
            origin: origin_without_path(&url),
            safe: false,
            reason: "unsupported origin scheme".to_string(),
            https: false,
        };
    }

    let Some(host) = url.host_str() else {
        return OriginSafety {
            origin: origin_without_path(&url),
            safe: false,
            reason: "origin host is missing".to_string(),
            https: false,
        };
    };

    if is_loopback_host(host) {
        return OriginSafety {
            origin: origin_without_path(&url),
            safe: true,
            reason: "loopback HTTP origin".to_string(),
            https: false,
        };
    }

    OriginSafety {
        origin: origin_without_path(&url),
        safe: false,
        reason: "plain HTTP origin is not loopback".to_string(),
        https: false,
    }
}

pub fn build_security_posture(bind: &str, headers: &HeaderMap) -> SecurityPostureSummary {
    let origin = classify_request_origin(headers);
    let bind_ip = parse_bind_ip(bind);
    let exposure = classify_bind_exposure(bind, bind_ip);
    let mut warnings = Vec::new();

    if exposure == "all_interfaces" {
        warnings.push(
            "Daemon is bound to all interfaces; plain HTTP LAN access is not Vault-safe."
                .to_string(),
        );
    } else if exposure == "lan_or_private_interface" || exposure == "tailscale_or_private_interface"
    {
        warnings.push(
            "Daemon is reachable beyond loopback; use HTTPS before future Vault operations."
                .to_string(),
        );
    }

    if !origin.safe {
        warnings.push(format!(
            "Current origin is not Vault-safe: {}.",
            origin.reason
        ));
    }

    SecurityPostureSummary {
        configured_bind: bind.to_string(),
        exposure,
        https_active: origin.https,
        current_origin: if origin.origin.is_empty() {
            None
        } else {
            Some(origin.origin)
        },
        current_origin_vault_safe: origin.safe,
        current_origin_reason: origin.reason,
        local_interfaces: detected_interfaces(bind, bind_ip),
        warnings,
    }
}

fn classify_bind_exposure(bind: &str, bind_ip: Option<IpAddr>) -> String {
    match bind_ip {
        Some(IpAddr::V4(ip)) if ip.is_loopback() => "localhost_only".to_string(),
        Some(IpAddr::V6(ip)) if ip.is_loopback() => "localhost_only".to_string(),
        Some(IpAddr::V4(ip)) if ip.is_unspecified() => "all_interfaces".to_string(),
        Some(IpAddr::V6(ip)) if ip.is_unspecified() => "all_interfaces".to_string(),
        Some(IpAddr::V4(ip)) if is_tailscale_ipv4(ip) => {
            "tailscale_or_private_interface".to_string()
        }
        Some(IpAddr::V4(ip)) if ip.is_private() || ip.is_link_local() => {
            "lan_or_private_interface".to_string()
        }
        Some(IpAddr::V6(ip)) if !ip.is_loopback() => "lan_or_private_interface".to_string(),
        Some(_) => "public_interface".to_string(),
        None if bind.starts_with("localhost:") => "localhost_only".to_string(),
        None => "unknown".to_string(),
    }
}

fn detected_interfaces(_bind: &str, bind_ip: Option<IpAddr>) -> Vec<LocalInterfaceSummary> {
    let Some(ip) = bind_ip else {
        return Vec::new();
    };
    vec![LocalInterfaceSummary {
        name: "configured_bind".to_string(),
        address: ip.to_string(),
        is_loopback: ip.is_loopback(),
        is_private: match ip {
            IpAddr::V4(ip) => ip.is_private() || is_tailscale_ipv4(ip),
            IpAddr::V6(ip) => !ip.is_loopback(),
        },
    }]
}

fn parse_bind_ip(bind: &str) -> Option<IpAddr> {
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        return Some(addr.ip());
    }
    bind.rsplit_once(':')?.0.parse::<IpAddr>().ok()
}

fn is_loopback_host(host: &str) -> bool {
    let normalized = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if normalized == "localhost" {
        return true;
    }
    normalized
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn is_tailscale_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

fn origin_without_path(url: &Url) -> String {
    let Some(host) = url.host_str() else {
        return url.as_str().to_string();
    };
    match url.port() {
        Some(port) => format!("{}://{}:{port}", url.scheme(), host),
        None => format!("{}://{}", url.scheme(), host),
    }
}

fn is_sensitive_header_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "set-cookie" | "proxy-authorization"
    )
}

fn is_sensitive_field_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase().replace(['-', '.'], "_");
    SENSITIVE_FIELD_NAMES
        .iter()
        .any(|sensitive| normalized == *sensitive || normalized.ends_with(&format!("_{sensitive}")))
}

fn redact_pem_blocks(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut redacting = false;
    for line in input.lines() {
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            redacting = true;
            output.push_str(REDACTED);
            output.push('\n');
            continue;
        }
        if redacting {
            if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                redacting = false;
            }
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    if !input.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }
    output
}

fn redact_urls_with_credentials(input: &str) -> String {
    input
        .split_whitespace()
        .map(redact_url_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_url_token(token: &str) -> String {
    let trimmed = token.trim_matches(|c: char| matches!(c, ',' | ';' | ')' | '(' | '"' | '\''));
    let prefix_len = token.find(trimmed).unwrap_or(0);
    let suffix = &token[prefix_len + trimmed.len()..];
    let prefix = &token[..prefix_len];

    let Ok(mut url) = Url::parse(trimmed) else {
        return token.to_string();
    };
    if url.username().is_empty() && url.password().is_none() {
        return token.to_string();
    }
    let _ = url.set_username(REDACTED);
    let _ = url.set_password(Some(REDACTED));
    format!("{prefix}{url}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;

    #[test]
    fn classifies_loopback_http_as_safe() {
        assert!(classify_origin("http://localhost:5201").safe);
        assert!(classify_origin("http://127.0.0.1:5201").safe);
        assert!(classify_origin("http://[::1]:5201").safe);
    }

    #[test]
    fn classifies_https_as_safe() {
        let result = classify_origin("https://nucleus.example.test");
        assert!(result.safe);
        assert!(result.https);
    }

    #[test]
    fn classifies_plain_private_http_as_unsafe() {
        let result = classify_origin("http://192.168.1.20:5201");
        assert!(!result.safe);
        assert_eq!(result.reason, "plain HTTP origin is not loopback");
    }

    #[test]
    fn classifies_tailscale_http_as_unsafe() {
        let result = classify_origin("http://100.80.12.4:5201");
        assert!(!result.safe);
    }

    #[test]
    fn classifies_headers_from_origin() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", HeaderValue::from_static("http://localhost:5173"));
        assert!(classify_request_origin(&headers).safe);
    }

    #[test]
    fn redacts_sensitive_headers_and_exact_secret_values() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer abc123"));
        headers.insert(
            "x-note",
            HeaderValue::from_static("prefix secret-value suffix"),
        );
        let redactor = RedactionSet::new().with_secret("secret-value");
        let redacted = redactor.redact_headers(&headers);
        assert!(redacted.contains(&("authorization".to_string(), REDACTED.to_string())));
        assert!(redacted.contains(&("x-note".to_string(), format!("prefix {REDACTED} suffix"))));
    }

    #[test]
    fn redacts_sensitive_json_fields() {
        let redactor = RedactionSet::new();
        let redacted = redactor.redact_json(&json!({
            "access_token": "abc",
            "nested": { "client_secret": "def", "safe": "ok" }
        }));
        assert_eq!(redacted["access_token"], REDACTED);
        assert_eq!(redacted["nested"]["client_secret"], REDACTED);
        assert_eq!(redacted["nested"]["safe"], "ok");
    }

    #[test]
    fn redacts_urls_with_credentials_and_private_keys() {
        let redactor = RedactionSet::new();
        let text = "postgres://user:pass@example.test/db\n-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\ndone";
        let redacted = redactor.redact_text(text);
        assert!(!redacted.contains("user:pass"));
        assert!(!redacted.contains("abc"));
        assert!(redacted.contains(REDACTED));
    }
}
