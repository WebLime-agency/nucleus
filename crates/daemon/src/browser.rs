use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use nucleus_protocol::{
    BrowserActionRequest, BrowserContextSummary, BrowserNavigateRequest, BrowserPageSummary,
    BrowserSnapshot, BrowserSnapshotRef,
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Default)]
pub struct BrowserRuntime {
    contexts: Mutex<HashMap<String, BrowserContextState>>,
    client: reqwest::Client,
}

#[derive(Clone)]
struct BrowserContextState {
    session_id: String,
    active_page_id: Option<String>,
    pages: Vec<BrowserPageState>,
}

#[derive(Clone)]
struct BrowserPageState {
    id: String,
    url: String,
    title: String,
    loading: bool,
    error: String,
    content: String,
    refs: Vec<BrowserSnapshotRef>,
    updated_at: i64,
}

impl BrowserRuntime {
    pub async fn context(&self, session_id: &str) -> BrowserContextSummary {
        let mut contexts = self.contexts.lock().await;
        let state = contexts
            .entry(session_id.to_owned())
            .or_insert_with(|| BrowserContextState {
                session_id: session_id.to_owned(),
                active_page_id: None,
                pages: Vec::new(),
            });
        state.summary()
    }

    pub async fn navigate(
        &self,
        session_id: &str,
        request: BrowserNavigateRequest,
    ) -> anyhow::Result<BrowserContextSummary> {
        let url = normalize_url(&request.url)?;
        let page_id = request
            .page_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let fetched = self.fetch_page(&url).await;
        let (title, content, refs, error) = match fetched {
            Ok((title, content, refs)) => (title, content, refs, String::new()),
            Err(err) => (String::new(), String::new(), Vec::new(), err.to_string()),
        };

        let mut contexts = self.contexts.lock().await;
        let state = contexts
            .entry(session_id.to_owned())
            .or_insert_with(|| BrowserContextState {
                session_id: session_id.to_owned(),
                active_page_id: None,
                pages: Vec::new(),
            });
        if let Some(page) = state.pages.iter_mut().find(|page| page.id == page_id) {
            page.url = url;
            page.title = title;
            page.loading = false;
            page.error = error;
            page.content = content;
            page.refs = refs;
            page.updated_at = now_ts();
        } else {
            state.pages.push(BrowserPageState {
                id: page_id.clone(),
                url,
                title,
                loading: false,
                error,
                content,
                refs,
                updated_at: now_ts(),
            });
        }
        state.active_page_id = Some(page_id);
        Ok(state.summary())
    }

    pub async fn snapshot(
        &self,
        session_id: &str,
        page_id: Option<String>,
    ) -> anyhow::Result<BrowserSnapshot> {
        let contexts = self.contexts.lock().await;
        let state = contexts
            .get(session_id)
            .context("browser context not found")?;
        let page = state.resolve_page(page_id.as_deref())?;
        Ok(BrowserSnapshot {
            session_id: session_id.to_owned(),
            page_id: page.id.clone(),
            url: page.url.clone(),
            title: page.title.clone(),
            content: page.content.clone(),
            refs: page.refs.clone(),
            captured_at: now_ts(),
        })
    }

    pub async fn action(
        &self,
        session_id: &str,
        request: BrowserActionRequest,
    ) -> anyhow::Result<BrowserSnapshot> {
        // MVP records intent and returns a fresh readable snapshot. A Playwright sidecar can replace this
        // implementation without changing the daemon/web contract.
        let snapshot = self.snapshot(session_id, request.page_id).await?;
        Ok(BrowserSnapshot {
            content: format!(
                "{}\n\nLast browser action: {} {}",
                snapshot.content,
                request.action,
                request.value.unwrap_or_default()
            ),
            ..snapshot
        })
    }

    async fn fetch_page(
        &self,
        url: &str,
    ) -> anyhow::Result<(String, String, Vec<BrowserSnapshotRef>)> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to load {url}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read browser response")?;
        if !status.is_success() {
            bail!("browser navigation returned HTTP {status}");
        }
        let title = extract_title(&body).unwrap_or_else(|| url.to_owned());
        let refs = extract_refs(&body);
        let content = html_to_text(&body);
        Ok((title, content, refs))
    }
}

impl BrowserContextState {
    fn summary(&self) -> BrowserContextSummary {
        BrowserContextSummary {
            session_id: self.session_id.clone(),
            active_page_id: self.active_page_id.clone(),
            pages: self
                .pages
                .iter()
                .map(|page| BrowserPageSummary {
                    id: page.id.clone(),
                    url: page.url.clone(),
                    title: page.title.clone(),
                    loading: page.loading,
                    error: page.error.clone(),
                    updated_at: page.updated_at,
                })
                .collect(),
        }
    }

    fn resolve_page(&self, page_id: Option<&str>) -> anyhow::Result<&BrowserPageState> {
        let target = page_id
            .or(self.active_page_id.as_deref())
            .context("no active browser page")?;
        self.pages
            .iter()
            .find(|page| page.id == target)
            .context("browser page not found")
    }
}

fn normalize_url(input: &str) -> anyhow::Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("URL is required");
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let parsed = url::Url::parse(&with_scheme).context("invalid browser URL")?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        scheme => bail!("unsupported browser URL scheme: {scheme}"),
    }
}

fn extract_title(html: &str) -> Option<String> {
    let mut scan_start = 0;
    let mut title_start = None;

    for (index, _) in html.char_indices() {
        if index < scan_start {
            continue;
        }
        let rest = &html[index..];
        if starts_with_ignore_ascii_case(rest, "<title") {
            let after_tag = rest.find('>')?;
            title_start = Some(index + after_tag + 1);
            break;
        }
        scan_start = index + 1;
    }

    let title_start = title_start?;
    let rest = &html[title_start..];
    let title_end = find_ignore_ascii_case(rest, "</title>")? + title_start;
    Some(html[title_start..title_end].trim().to_owned()).filter(|value| !value.is_empty())
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .char_indices()
        .find(|(index, _)| starts_with_ignore_ascii_case(&haystack[*index..], needle))
        .map(|(index, _)| index)
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    let Some(candidate) = value.get(..prefix.len()) else {
        return false;
    };
    candidate.eq_ignore_ascii_case(prefix)
}

fn extract_refs(html: &str) -> Vec<BrowserSnapshotRef> {
    let mut refs = Vec::new();
    for tag in ["a", "button", "input", "textarea", "select"] {
        let needle = format!("<{tag}");
        let mut offset = 0;
        let lower = html.to_lowercase();
        while let Some(pos) = lower[offset..].find(&needle) {
            let start = offset + pos;
            let end = lower[start..]
                .find('>')
                .map(|v| start + v + 1)
                .unwrap_or(html.len());
            let snippet = &html[start..end.min(html.len())];
            let label = attr(snippet, "aria-label")
                .or_else(|| attr(snippet, "title"))
                .or_else(|| attr(snippet, "placeholder"))
                .unwrap_or_else(|| tag.to_owned());
            refs.push(BrowserSnapshotRef {
                id: format!("{} {}", tag, refs.len() + 1),
                kind: tag.to_owned(),
                label,
                selector: String::new(),
            });
            offset = end;
            if refs.len() >= 80 {
                return refs;
            }
        }
    }
    refs
}

fn attr(snippet: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{name}={quote}");
        if let Some(start) = snippet.find(&needle) {
            let value_start = start + needle.len();
            if let Some(end) = snippet[value_start..].find(quote) {
                return Some(snippet[value_start..value_start + end].trim().to_owned())
                    .filter(|v| !v.is_empty());
            }
        }
    }
    None
}

fn html_to_text(html: &str) -> String {
    let visible_html = strip_invisible_html_blocks(html);
    let mut out = String::new();
    let mut in_tag = false;

    for ch in visible_html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    decode_basic_entities(&out)
        .split_whitespace()
        .take(1200)
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_invisible_html_blocks(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;

    while cursor < html.len() {
        let rest = &html[cursor..];
        let Some(relative_start) = find_next_invisible_block_start(rest) else {
            output.push_str(rest);
            break;
        };

        let start = cursor + relative_start;
        output.push_str(&html[cursor..start]);

        let tag_name = invisible_block_tag_name(&html[start..]).unwrap_or("script");
        let close_tag = format!("</{tag_name}>");
        let after_open = html[start..]
            .find('>')
            .map(|index| start + index + 1)
            .unwrap_or(html.len());

        if let Some(relative_end) = find_ignore_ascii_case(&html[after_open..], &close_tag) {
            cursor = after_open + relative_end + close_tag.len();
        } else {
            break;
        }
    }

    output
}

fn find_next_invisible_block_start(value: &str) -> Option<usize> {
    ["<script", "<style", "<noscript", "<svg", "<template"]
        .into_iter()
        .filter_map(|needle| find_ignore_ascii_case(value, needle))
        .min()
}

fn invisible_block_tag_name(value: &str) -> Option<&'static str> {
    if starts_with_ignore_ascii_case(value, "<script") {
        Some("script")
    } else if starts_with_ignore_ascii_case(value, "<style") {
        Some("style")
    } else if starts_with_ignore_ascii_case(value, "<noscript") {
        Some("noscript")
    } else if starts_with_ignore_ascii_case(value, "<svg") {
        Some("svg")
    } else if starts_with_ignore_ascii_case(value, "<template") {
        Some("template")
    } else {
        None
    }
}

fn decode_basic_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
