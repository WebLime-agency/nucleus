use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use nucleus_protocol::{
    BrowserActionRequest, BrowserContextSummary, BrowserFrameEvent, BrowserNavigateRequest,
    BrowserPageSummary, BrowserSnapshot, BrowserSnapshotRef, DaemonEvent,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Default)]
pub struct BrowserRuntime {
    contexts: Mutex<HashMap<String, BrowserContextState>>,
    client: reqwest::Client,
    sidecar: Mutex<Option<BrowserSidecar>>,
    streams: Mutex<HashMap<String, BrowserStreamHandle>>,
}

struct BrowserStreamHandle {
    stream_id: String,
    _task: tokio::task::JoinHandle<()>,
}

struct BrowserSidecar {
    base_url: String,
    _child: Child,
}

#[derive(serde::Deserialize)]
struct SidecarReady {
    port: u16,
}

#[derive(serde::Deserialize)]
struct SidecarPage {
    #[serde(default)]
    page_id: String,
    url: String,
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    refs: Vec<BrowserSnapshotRef>,
    #[serde(default)]
    screenshot_data_url: String,
}

#[derive(serde::Deserialize)]
struct SidecarStreamStart {
    stream_id: String,
}

#[derive(serde::Deserialize)]
struct SidecarFrameEnvelope {
    frame: Option<SidecarFrame>,
}

#[derive(serde::Deserialize)]
struct SidecarCommandResult {
    #[serde(default)]
    page: Option<SidecarPage>,
    #[serde(default)]
    pages: Vec<SidecarPage>,
}

#[derive(serde::Deserialize)]
struct SidecarAnnotationResult {
    annotation: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct SidecarFrame {
    page_id: String,
    mime: String,
    image: String,
    #[serde(default)]
    state: Option<SidecarPage>,
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
        let requested_page_id = request
            .page_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();
        let fetched = self
            .navigate_sidecar(session_id, &requested_page_id, &url)
            .await;
        let (page_id, page_url, title, content, refs, error) = match fetched {
            Ok(page) => (
                page.page_id,
                page.url,
                page.title,
                page.content,
                page.refs,
                String::new(),
            ),
            Err(err) => (
                if requested_page_id.is_empty() {
                    Uuid::new_v4().to_string()
                } else {
                    requested_page_id
                },
                url,
                String::new(),
                String::new(),
                Vec::new(),
                err.to_string(),
            ),
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
            page.url = page_url;
            page.title = title;
            page.loading = false;
            page.error = error;
            page.content = content;
            page.refs = refs;
            page.updated_at = now_ts();
        } else {
            state.pages.push(BrowserPageState {
                id: page_id.clone(),
                url: page_url,
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
        let page = {
            let contexts = self.contexts.lock().await;
            let state = contexts
                .get(session_id)
                .context("browser context not found")?;
            state.resolve_page(page_id.as_deref())?.clone()
        };
        let sidecar_page = self.snapshot_sidecar(session_id, &page.id).await.ok();
        Ok(BrowserSnapshot {
            session_id: session_id.to_owned(),
            page_id: page.id.clone(),
            url: sidecar_page
                .as_ref()
                .map(|page| page.url.clone())
                .unwrap_or_else(|| page.url.clone()),
            title: sidecar_page
                .as_ref()
                .map(|page| page.title.clone())
                .unwrap_or_else(|| page.title.clone()),
            content: sidecar_page
                .as_ref()
                .map(|page| page.content.clone())
                .unwrap_or_else(|| page.content.clone()),
            refs: sidecar_page
                .as_ref()
                .map(|page| page.refs.clone())
                .unwrap_or_else(|| page.refs.clone()),
            screenshot_data_url: sidecar_page
                .map(|page| page.screenshot_data_url)
                .unwrap_or_default(),
            captured_at: now_ts(),
        })
    }

    pub async fn action(
        &self,
        session_id: &str,
        request: BrowserActionRequest,
    ) -> anyhow::Result<BrowserSnapshot> {
        let page_id = request.page_id.clone();
        if let Some(page_id) = page_id.as_deref() {
            let sidecar_page = self.input_sidecar(session_id, page_id, &request).await?;
            self.sync_sidecar_page(session_id, &sidecar_page).await?;
            if !request.snapshot.unwrap_or(true) {
                return Ok(BrowserSnapshot {
                    session_id: session_id.to_owned(),
                    page_id: sidecar_page.page_id,
                    url: sidecar_page.url,
                    title: sidecar_page.title,
                    content: sidecar_page.content,
                    refs: sidecar_page.refs,
                    screenshot_data_url: sidecar_page.screenshot_data_url,
                    captured_at: now_ts(),
                });
            }
        }
        self.snapshot(session_id, page_id).await
    }

    async fn sidecar_url(&self) -> anyhow::Result<String> {
        let mut sidecar = self.sidecar.lock().await;
        if let Some(sidecar) = sidecar.as_ref() {
            return Ok(sidecar.base_url.clone());
        }

        let script = std::env::current_dir()
            .context("failed to resolve current directory")?
            .join("scripts/browser-sidecar.mjs");
        let mut child = Command::new("node")
            .arg("--experimental-websocket")
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context("failed to start browser sidecar")?;
        let stdout = child
            .stdout
            .take()
            .context("browser sidecar stdout unavailable")?;
        let mut lines = BufReader::new(stdout).lines();
        let line = lines
            .next_line()
            .await
            .context("failed to read browser sidecar startup")?
            .context("browser sidecar exited before startup")?;
        let ready: SidecarReady =
            serde_json::from_str(&line).context("invalid browser sidecar startup payload")?;
        let handle = BrowserSidecar {
            base_url: format!("http://127.0.0.1:{}", ready.port),
            _child: child,
        };
        let base_url = handle.base_url.clone();
        *sidecar = Some(handle);
        Ok(base_url)
    }

    async fn navigate_sidecar(
        &self,
        session_id: &str,
        page_id: &str,
        url: &str,
    ) -> anyhow::Result<SidecarPage> {
        let base_url = self.sidecar_url().await?;
        Ok(self
            .client
            .post(format!("{base_url}/navigate"))
            .json(&serde_json::json!({ "session_id": session_id, "page_id": page_id, "url": url }))
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarPage>()
            .await?)
    }

    async fn snapshot_sidecar(
        &self,
        session_id: &str,
        page_id: &str,
    ) -> anyhow::Result<SidecarPage> {
        let base_url = self.sidecar_url().await?;
        Ok(self
            .client
            .post(format!("{base_url}/snapshot"))
            .json(&serde_json::json!({ "session_id": session_id, "page_id": page_id }))
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarPage>()
            .await?)
    }

    async fn input_sidecar(
        &self,
        session_id: &str,
        page_id: &str,
        request: &BrowserActionRequest,
    ) -> anyhow::Result<SidecarPage> {
        let base_url = self.sidecar_url().await?;
        let mut body = serde_json::json!({
            "session_id": session_id,
            "page_id": page_id,
            "action": request.action,
            "value": request.value,
        });
        if let Some(args) = request
            .value
            .as_deref()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        {
            if let Some(object) = body.as_object_mut() {
                if let Some(args_object) = args.as_object() {
                    for (key, value) in args_object {
                        object.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        Ok(self
            .client
            .post(format!("{base_url}/input"))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarPage>()
            .await?)
    }

    pub async fn open_tab(&self, session_id: &str) -> anyhow::Result<BrowserContextSummary> {
        let base_url = self.sidecar_url().await?;
        let result = self
            .client
            .post(format!("{base_url}/open"))
            .json(&serde_json::json!({ "session_id": session_id, "new_tab": true }))
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarCommandResult>()
            .await?;
        self.sync_sidecar_pages(
            session_id,
            result.pages,
            result.page.map(|page| page.page_id),
        )
        .await
    }

    pub async fn select_page(
        &self,
        session_id: &str,
        page_id: &str,
    ) -> anyhow::Result<BrowserContextSummary> {
        let base_url = self.sidecar_url().await?;
        let result = self
            .client
            .post(format!("{base_url}/select"))
            .json(&serde_json::json!({ "session_id": session_id, "page_id": page_id }))
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarCommandResult>()
            .await?;
        if !result.pages.is_empty() {
            return self
                .sync_sidecar_pages(
                    session_id,
                    result.pages,
                    result.page.map(|page| page.page_id),
                )
                .await;
        }

        let mut contexts = self.contexts.lock().await;
        let state = contexts
            .get_mut(session_id)
            .context("browser context not found")?;
        state.resolve_page(Some(page_id))?;
        state.active_page_id = Some(page_id.to_owned());
        Ok(state.summary())
    }

    pub async fn command(
        &self,
        session_id: &str,
        page_id: &str,
        command: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<BrowserContextSummary> {
        let base_url = self.sidecar_url().await?;
        let mut body =
            serde_json::json!({ "session_id": session_id, "page_id": page_id, "command": command });
        if let (Some(target), Some(source)) = (body.as_object_mut(), args.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        let result = self
            .client
            .post(format!("{base_url}/command"))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarCommandResult>()
            .await?;
        let active = result.page.as_ref().map(|page| page.page_id.clone());
        self.sync_sidecar_pages(session_id, result.pages, active)
            .await
    }

    pub async fn annotation(
        &self,
        session_id: &str,
        page_id: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let base_url = self.sidecar_url().await?;
        let result = self
            .client
            .post(format!("{base_url}/annotation"))
            .json(&serde_json::json!({ "session_id": session_id, "page_id": page_id, "payload": payload }))
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarAnnotationResult>()
            .await?;
        Ok(result.annotation)
    }

    async fn sync_sidecar_pages(
        &self,
        session_id: &str,
        pages: Vec<SidecarPage>,
        active_page_id: Option<String>,
    ) -> anyhow::Result<BrowserContextSummary> {
        let mut contexts = self.contexts.lock().await;
        let state = contexts
            .entry(session_id.to_owned())
            .or_insert_with(|| BrowserContextState {
                session_id: session_id.to_owned(),
                active_page_id: None,
                pages: Vec::new(),
            });
        let mut next_pages = Vec::new();
        let now = now_ts();
        for sidecar_page in pages {
            next_pages.push(BrowserPageState {
                id: sidecar_page.page_id,
                url: sidecar_page.url,
                title: sidecar_page.title,
                loading: false,
                error: String::new(),
                content: sidecar_page.content,
                refs: sidecar_page.refs,
                updated_at: now,
            });
        }
        if !next_pages.is_empty() {
            let active_id = active_page_id
                .filter(|id| next_pages.iter().any(|page| &page.id == id))
                .or_else(|| {
                    state
                        .active_page_id
                        .clone()
                        .filter(|id| next_pages.iter().any(|page| &page.id == id))
                })
                .or_else(|| next_pages.last().map(|page| page.id.clone()));
            state.pages = next_pages;
            state.active_page_id = active_id;
        }
        Ok(state.summary())
    }

    async fn sync_sidecar_page(
        &self,
        session_id: &str,
        sidecar_page: &SidecarPage,
    ) -> anyhow::Result<()> {
        let mut contexts = self.contexts.lock().await;
        let state = contexts
            .entry(session_id.to_owned())
            .or_insert_with(|| BrowserContextState {
                session_id: session_id.to_owned(),
                active_page_id: None,
                pages: Vec::new(),
            });
        let now = now_ts();
        if let Some(page) = state
            .pages
            .iter_mut()
            .find(|page| page.id == sidecar_page.page_id)
        {
            page.url = sidecar_page.url.clone();
            page.title = sidecar_page.title.clone();
            page.loading = false;
            page.error.clear();
            page.content = sidecar_page.content.clone();
            page.refs = sidecar_page.refs.clone();
            page.updated_at = now;
        } else {
            state.pages.push(BrowserPageState {
                id: sidecar_page.page_id.clone(),
                url: sidecar_page.url.clone(),
                title: sidecar_page.title.clone(),
                loading: false,
                error: String::new(),
                content: sidecar_page.content.clone(),
                refs: sidecar_page.refs.clone(),
                updated_at: now,
            });
        }
        state.active_page_id = Some(sidecar_page.page_id.clone());
        Ok(())
    }

    pub async fn start_stream(
        &self,
        session_id: String,
        page_id: Option<String>,
        events: tokio::sync::broadcast::Sender<DaemonEvent>,
    ) -> anyhow::Result<()> {
        let page = {
            let contexts = self.contexts.lock().await;
            let state = contexts
                .get(&session_id)
                .context("browser context not found")?;
            state.resolve_page(page_id.as_deref())?.clone()
        };
        let start = self.start_screencast_sidecar(&session_id, &page.id).await?;
        let base_url = self.sidecar_url().await?;
        let client = self.client.clone();
        let stream_id = start.stream_id.clone();
        let key = format!("{session_id}:{}", page.id);
        self.stop_stream(&session_id, &page.id).await;
        let task_session_id = session_id.clone();
        let task_page_id = page.id.clone();
        let task_stream_id = stream_id.clone();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(33));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let response = client
                    .post(format!("{base_url}/pop_frame"))
                    .json(&serde_json::json!({ "stream_id": task_stream_id }))
                    .send()
                    .await;
                let Ok(response) = response else { break };
                let Ok(response) = response.error_for_status() else {
                    break;
                };
                let Ok(envelope) = response.json::<SidecarFrameEnvelope>().await else {
                    break;
                };
                let Some(frame) = envelope.frame else {
                    continue;
                };
                let state = frame.state.as_ref();
                let _ = events.send(DaemonEvent::BrowserFrame(BrowserFrameEvent {
                    session_id: task_session_id.clone(),
                    page_id: frame.page_id.clone(),
                    mime: frame.mime,
                    image: frame.image,
                    url: state.map(|state| state.url.clone()).unwrap_or_default(),
                    title: state.map(|state| state.title.clone()).unwrap_or_default(),
                    captured_at: now_ts(),
                }));
                if frame.page_id != task_page_id {
                    break;
                }
            }
        });
        self.streams.lock().await.insert(
            key,
            BrowserStreamHandle {
                stream_id,
                _task: task,
            },
        );
        Ok(())
    }

    pub async fn stop_stream(&self, session_id: &str, page_id: &str) {
        let key = format!("{session_id}:{page_id}");
        if let Some(handle) = self.streams.lock().await.remove(&key) {
            handle._task.abort();
            let _ = self.stop_screencast_sidecar(&handle.stream_id).await;
        }
    }

    async fn start_screencast_sidecar(
        &self,
        session_id: &str,
        page_id: &str,
    ) -> anyhow::Result<SidecarStreamStart> {
        let base_url = self.sidecar_url().await?;
        Ok(self
            .client
            .post(format!("{base_url}/start_screencast"))
            .json(
                &serde_json::json!({ "session_id": session_id, "page_id": page_id, "quality": 82 }),
            )
            .send()
            .await?
            .error_for_status()?
            .json::<SidecarStreamStart>()
            .await?)
    }

    async fn stop_screencast_sidecar(&self, stream_id: &str) -> anyhow::Result<()> {
        let base_url = self.sidecar_url().await?;
        self.client
            .post(format!("{base_url}/stop_screencast"))
            .json(&serde_json::json!({ "stream_id": stream_id }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
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
    if trimmed.eq_ignore_ascii_case("about:blank") {
        return Ok("about:blank".to_owned());
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

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
