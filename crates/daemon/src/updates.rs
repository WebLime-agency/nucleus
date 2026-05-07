use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use nucleus_core::PRODUCT_NAME;
use nucleus_protocol::{InstanceSummary, UpdateStatus};
use nucleus_release::{
    DEFAULT_RELEASE_CHANNEL, INSTALL_KIND_DEV_CHECKOUT, INSTALL_KIND_MANAGED_RELEASE,
    ManagedReleaseManifest, SelectedRelease, activate_release, current_platform_target,
    current_release_binary_path, current_release_dir, current_release_id, current_release_web_dir,
    default_channel_manifest_url, load_manifest, read_installed_release_metadata, select_release,
    stage_release_archive, verify_sha256,
};
use nucleus_storage::{StateStore, StoredUpdateState};
use tokio::{
    process::Command,
    sync::Mutex,
    time::{Instant, timeout},
};

const DEFAULT_REMOTE_NAME: &str = "origin";
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const BUILD_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelaunchBehavior {
    detach_stdio: bool,
    detach_process_group: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RestartMode {
    Systemd { unit: String },
    SelfReexec,
    Unsupported,
}

impl RestartMode {
    fn label(&self) -> String {
        match self {
            Self::Systemd { .. } => "systemd".to_string(),
            Self::SelfReexec => "self-reexec".to_string(),
            Self::Unsupported => "unsupported".to_string(),
        }
    }

    fn supported(&self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

#[derive(Debug, Clone)]
pub struct InstanceRuntime {
    pub name: String,
    pub repo_root: PathBuf,
    pub daemon_bind: String,
    pub install_kind: String,
    pub install_root: Option<PathBuf>,
    pub release_manifest_url: Option<String>,
    pub state_dir: Option<PathBuf>,
    pub daemon_binary: PathBuf,
    pub managed_web_dist_dir: Option<PathBuf>,
    restart_mode: RestartMode,
}

impl InstanceRuntime {
    pub fn detect(daemon_bind: String) -> Self {
        let repo_root = env::var("NUCLEUS_REPO_ROOT")
            .map(PathBuf::from)
            .ok()
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let state_dir = env::var("NUCLEUS_STATE_DIR").ok().map(PathBuf::from);
        let daemon_binary = env::current_exe().unwrap_or_else(|_| {
            repo_root
                .join("target")
                .join("debug")
                .join("nucleus-daemon")
        });
        let install_kind = detect_install_kind(&repo_root);
        let install_root = env::var("NUCLEUS_INSTALL_ROOT").ok().map(PathBuf::from);
        let release_manifest_url = env::var("NUCLEUS_RELEASE_MANIFEST_URL").ok();
        let explicit_web_dist_dir = env::var("NUCLEUS_WEB_DIST_DIR").ok().map(PathBuf::from);
        let managed_web_dist_dir = if install_kind == INSTALL_KIND_MANAGED_RELEASE {
            install_root
                .as_ref()
                .map(|path| current_release_web_dir(path))
                .or(explicit_web_dist_dir)
        } else {
            explicit_web_dist_dir
        };
        let restart_mode = detect_restart_mode(
            &daemon_binary,
            state_dir.as_deref(),
            &daemon_bind,
            &repo_root,
        );

        Self {
            name: env::var("NUCLEUS_INSTANCE_NAME").unwrap_or_else(|_| PRODUCT_NAME.to_string()),
            install_kind,
            install_root,
            release_manifest_url,
            repo_root,
            daemon_bind,
            state_dir,
            daemon_binary,
            managed_web_dist_dir,
            restart_mode,
        }
    }

    pub fn summary(&self) -> InstanceSummary {
        InstanceSummary {
            name: self.name.clone(),
            repo_root: self
                .is_dev_checkout()
                .then(|| display_path(&self.repo_root)),
            daemon_bind: self.daemon_bind.clone(),
            install_kind: self.install_kind.clone(),
            restart_mode: self.restart_mode.label(),
            restart_supported: self.restart_mode.supported(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        name: impl Into<String>,
        repo_root: PathBuf,
        daemon_bind: impl Into<String>,
        install_kind: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            repo_root: repo_root.clone(),
            daemon_bind: daemon_bind.into(),
            install_kind: install_kind.into(),
            install_root: None,
            release_manifest_url: None,
            state_dir: None,
            daemon_binary: repo_root.join("target/debug/nucleus-daemon"),
            managed_web_dist_dir: None,
            restart_mode: RestartMode::Unsupported,
        }
    }

    fn is_dev_checkout(&self) -> bool {
        self.install_kind == INSTALL_KIND_DEV_CHECKOUT
    }

    fn is_managed_release(&self) -> bool {
        self.install_kind == INSTALL_KIND_MANAGED_RELEASE
    }
}

#[derive(Debug, Clone)]
pub struct UpdateRefresh {
    pub status: UpdateStatus,
    pub changed: bool,
    pub restart_requested: bool,
}

pub struct UpdateManager {
    instance: InstanceRuntime,
    store: Arc<StateStore>,
    state: Mutex<UpdateStatus>,
    operation_lock: Mutex<()>,
}

impl UpdateManager {
    pub fn new(instance: InstanceRuntime, store: Arc<StateStore>) -> Self {
        let stored = store.read_update_state().unwrap_or_default();
        let initial = initial_status(&instance, &stored);
        let mut reconciled = stored_state_from_status(&initial);
        if reconciled.release_manifest_url.is_none() {
            reconciled.release_manifest_url = stored.release_manifest_url.clone();
        }
        if initial.restart_required {
            reconciled.pending_restart_release_id = stored.pending_restart_release_id.clone();
        }
        if reconciled != stored
            && let Err(error) = store.write_update_state(&reconciled)
        {
            tracing::warn!(error = %error, "failed to reconcile persisted update state");
        }
        Self {
            instance,
            store,
            state: Mutex::new(initial),
            operation_lock: Mutex::new(()),
        }
    }

    pub fn instance_summary(&self) -> InstanceSummary {
        self.instance.summary()
    }

    pub async fn current(&self) -> UpdateStatus {
        self.state.lock().await.clone()
    }

    pub fn auto_check_enabled(&self) -> bool {
        self.instance.is_dev_checkout() || self.managed_release_manifest_url().is_some()
    }

    pub async fn configure(
        &self,
        tracked_channel: Option<String>,
        tracked_ref: Option<String>,
    ) -> Result<UpdateRefresh> {
        let _guard = self.operation_lock.lock().await;
        let mut stored = self.store.read_update_state()?;

        if self.instance.is_managed_release() {
            if tracked_ref.is_some() {
                bail!("managed releases do not track git refs");
            }

            if let Some(tracked_channel) = tracked_channel {
                let tracked_channel = tracked_channel.trim();
                nucleus_release::validate_channel(tracked_channel)?;
                stored.tracked_channel = Some(tracked_channel.to_string());
                clear_update_history_for_retarget(&mut stored);
            }
        } else {
            if tracked_channel.is_some() {
                bail!("dev checkouts do not track release channels");
            }

            if let Some(tracked_ref) = tracked_ref {
                let tracked_ref = tracked_ref.trim();
                if tracked_ref.is_empty() {
                    bail!("tracked ref cannot be empty");
                }
                stored.tracked_ref = Some(tracked_ref.to_string());
                clear_update_history_for_retarget(&mut stored);
            }
        }

        let next = initial_status(&self.instance, &stored);
        Ok(self.replace_state(next, false).await)
    }

    pub async fn check(&self) -> UpdateRefresh {
        let _guard = self.operation_lock.lock().await;
        self.transition_state("checking", "Checking for updates...")
            .await;

        let previous = self.current().await;
        let next = if self.instance.is_dev_checkout() {
            inspect_checkout(&self.instance, &previous, true, previous.restart_required)
                .await
                .unwrap_or_else(|error| error_status(&self.instance, &previous, error.to_string()))
        } else {
            inspect_managed_release(
                &self.instance,
                &previous,
                self.managed_release_manifest_url(),
                previous.restart_required,
            )
            .await
            .unwrap_or_else(|error| error_status(&self.instance, &previous, error.to_string()))
        };

        self.replace_state(next, false).await
    }

    pub async fn apply(&self) -> UpdateRefresh {
        let _guard = self.operation_lock.lock().await;
        let apply_message = if self.instance.is_managed_release() {
            "Applying managed release artifact and preparing restart..."
        } else {
            "Applying update and rebuilding Nucleus..."
        };
        self.transition_state("applying", apply_message).await;

        let previous = self.current().await;
        let inspected = if self.instance.is_dev_checkout() {
            inspect_checkout(&self.instance, &previous, true, previous.restart_required)
                .await
                .unwrap_or_else(|error| error_status(&self.instance, &previous, error.to_string()))
        } else {
            inspect_managed_release(
                &self.instance,
                &previous,
                self.managed_release_manifest_url(),
                previous.restart_required,
            )
            .await
            .unwrap_or_else(|error| error_status(&self.instance, &previous, error.to_string()))
        };

        if self.instance.is_managed_release() {
            return self.apply_managed_release(inspected).await;
        }

        if inspected.dirty_worktree {
            let mut next = inspected;
            next.state = "error".to_string();
            next.message =
                "Cannot update this checkout while the working tree has local changes.".to_string();
            next.last_attempted_check_at = Some(unix_timestamp());
            next.last_attempt_result = Some("error".to_string());
            next.latest_error = Some(next.message.clone());
            next.latest_error_at = next.last_attempted_check_at;
            return self.replace_state(next, false).await;
        }

        if !inspected.update_available {
            let mut next = inspected;
            next.state = "ready".to_string();
            next.message = "Nucleus is already up to date.".to_string();
            return self.replace_state(next, false).await;
        }

        let Some(tracked_ref) = inspected.tracked_ref.clone() else {
            let next = error_status(
                &self.instance,
                &inspected,
                "No tracked git ref is configured for this checkout.".to_string(),
            );
            return self.replace_state(next, false).await;
        };

        let before_commit = inspected.current_commit.clone();
        let pull_args = [
            "pull",
            "--ff-only",
            DEFAULT_REMOTE_NAME,
            tracked_ref.as_str(),
        ];
        let pull_result = run_git(&self.instance.repo_root, &pull_args, GIT_TIMEOUT)
            .await
            .map(|_| ());

        let refreshed = match pull_result {
            Ok(()) => {
                let build_result = match rebuild_daemon(&self.instance).await {
                    Ok(()) => rebuild_managed_web(&self.instance).await,
                    Err(error) => Err(error),
                };

                match build_result {
                    Ok(()) => inspect_checkout(&self.instance, &inspected, false, false)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                }
            }
            Err(error) => Err(error.to_string()),
        };

        match refreshed {
            Ok(mut refreshed) => {
                if before_commit.is_some() && refreshed.current_commit == before_commit {
                    refreshed.state = "ready".to_string();
                    refreshed.message = "Nucleus is already up to date.".to_string();
                    return self.replace_state(refreshed, false).await;
                }

                let next = finalize_updated_status(&self.instance, refreshed);
                let restart_requested = next.state == "restarting";
                self.replace_state(next, restart_requested).await
            }
            Err(message) => {
                let next = error_status(&self.instance, &inspected, message);
                self.replace_state(next, false).await
            }
        }
    }

    pub async fn request_restart(&self) -> UpdateRefresh {
        let _guard = self.operation_lock.lock().await;
        let current = self.current().await;

        if !self.instance.restart_mode.supported() {
            let next = error_status(
                &self.instance,
                &current,
                "This install cannot restart itself from the UI.".to_string(),
            );
            return self.replace_state(next, false).await;
        }

        let mut next = current;
        next.state = "restarting".to_string();
        next.message =
            "Restarting Nucleus now. The connection will return automatically.".to_string();
        next.restart_required = false;
        next.latest_error = None;
        next.latest_error_at = None;

        self.replace_state(next, true).await
    }

    pub async fn mark_restart_failure(&self, message: String) -> UpdateRefresh {
        let current = self.current().await;
        let mut next = current;
        next.state = "error".to_string();
        next.message = message.clone();
        next.latest_error = Some(message);
        next.latest_error_at = Some(unix_timestamp());
        next.restart_required = true;
        self.replace_state(next, false).await
    }

    pub async fn perform_restart(&self) -> Result<()> {
        match &self.instance.restart_mode {
            RestartMode::Systemd { unit } => restart_systemd_unit(unit).await,
            RestartMode::SelfReexec => relaunch_current_daemon(&self.instance).await,
            RestartMode::Unsupported => {
                anyhow::bail!("This install does not support daemon restarts from the UI.")
            }
        }
    }

    async fn replace_state(&self, next: UpdateStatus, restart_requested: bool) -> UpdateRefresh {
        let mut stored_state = stored_state_from_status(&next);
        if let Ok(existing) = self.store.read_update_state() {
            if stored_state.release_manifest_url.is_none() {
                stored_state.release_manifest_url = existing.release_manifest_url.clone();
            }

            if next.install_kind == INSTALL_KIND_MANAGED_RELEASE {
                stored_state.pending_restart_release_id =
                    next_pending_restart_release_id(&next, &existing);
            }
        }

        if let Err(error) = self.store.write_update_state(&stored_state) {
            tracing::warn!(error = %error, "failed to persist update state");
        }

        let mut state = self.state.lock().await;
        let changed = *state != next;
        *state = next.clone();

        UpdateRefresh {
            status: next,
            changed,
            restart_requested,
        }
    }

    async fn transition_state(&self, next_state: &str, message: &str) {
        let mut state = self.state.lock().await;
        state.state = next_state.to_string();
        state.message = message.to_string();
    }

    fn managed_release_manifest_url(&self) -> Option<String> {
        let stored = self.store.read_update_state().ok();
        let stored_url = stored
            .as_ref()
            .and_then(|state| state.release_manifest_url.clone());
        let tracked_channel = stored
            .and_then(|state| state.tracked_channel)
            .unwrap_or_else(|| DEFAULT_RELEASE_CHANNEL.to_string());

        stored_url
            .or_else(|| self.instance.release_manifest_url.clone())
            .or_else(|| default_channel_manifest_url(&tracked_channel).ok())
    }

    async fn apply_managed_release(&self, inspected: UpdateStatus) -> UpdateRefresh {
        if !inspected.update_available {
            let mut next = inspected;
            next.state = "ready".to_string();
            next.message = "Nucleus is already up to date.".to_string();
            return self.replace_state(next, false).await;
        }

        let Some(install_root) = self.instance.install_root.clone() else {
            let next = error_status(
                &self.instance,
                &inspected,
                "Managed release install root is not configured.".to_string(),
            );
            return self.replace_state(next, false).await;
        };

        let Some(manifest_url) = self.managed_release_manifest_url() else {
            let next = error_status(
                &self.instance,
                &inspected,
                "Managed release manifest URL is not configured.".to_string(),
            );
            return self.replace_state(next, false).await;
        };

        let Some(tracked_channel) = inspected.tracked_channel.clone() else {
            let next = error_status(
                &self.instance,
                &inspected,
                "Managed release channel is not configured.".to_string(),
            );
            return self.replace_state(next, false).await;
        };

        let selected = match load_manifest(&manifest_url).await.and_then(|manifest| {
            select_release(&manifest, &tracked_channel, &current_platform_target())
        }) {
            Ok(selected) => selected,
            Err(error) => {
                let next = error_status(&self.instance, &inspected, error.to_string());
                return self.replace_state(next, false).await;
            }
        };

        let download_dir = self
            .store
            .artifacts_dir_path()
            .join("managed-release-downloads");
        let archive_path = download_dir.join(artifact_download_name(&selected));
        let download = nucleus_release::download_artifact_to_path(
            &selected.artifact.download_url,
            &archive_path,
        )
        .await;
        let (downloaded_size, _) = match download {
            Ok(values) => values,
            Err(error) => {
                let next = error_status(&self.instance, &inspected, error.to_string());
                return self.replace_state(next, false).await;
            }
        };

        if let Err(error) =
            verify_sha256(&archive_path, &selected.artifact.sha256).and_then(|verified_size| {
                if verified_size != downloaded_size {
                    bail!(
                        "artifact size mismatch for {}: downloaded {} bytes, verified {} bytes",
                        archive_path.display(),
                        downloaded_size,
                        verified_size
                    );
                }

                if verified_size != selected.artifact.size_bytes {
                    bail!(
                        "artifact size mismatch for {}: manifest expected {} bytes, got {}",
                        archive_path.display(),
                        selected.artifact.size_bytes,
                        verified_size
                    );
                }

                Ok(())
            })
        {
            let next = error_status(&self.instance, &inspected, error.to_string());
            return self.replace_state(next, false).await;
        }

        if let Err(error) =
            stage_release_archive(&archive_path, &install_root, &selected.release.release_id)
                .and_then(|_| activate_release(&install_root, &selected.release.release_id))
        {
            let next = error_status(&self.instance, &inspected, error.to_string());
            return self.replace_state(next, false).await;
        }

        let mut refreshed = inspected;
        refreshed.latest_version = Some(selected.release.version.clone());
        refreshed.latest_release_id = Some(selected.release.release_id.clone());
        refreshed.latest_commit = None;
        refreshed.latest_commit_short = None;
        let next = finalize_updated_status(&self.instance, refreshed);
        let restart_requested = next.state == "restarting";
        self.replace_state(next, restart_requested).await
    }
}

fn finalize_updated_status(
    instance: &InstanceRuntime,
    mut refreshed: UpdateStatus,
) -> UpdateStatus {
    refreshed.update_available = false;
    refreshed.last_attempt_result = Some("success".to_string());
    refreshed.latest_error = None;
    refreshed.latest_error_at = None;

    if instance.restart_mode.supported() {
        refreshed.restart_required = false;
        refreshed.state = "restarting".to_string();
        refreshed.message =
            "Update applied. Restarting Nucleus now. The connection will return automatically."
                .to_string();
        return refreshed;
    }

    refreshed.restart_required = true;
    refreshed.state = "ready".to_string();
    refreshed.message = "Update applied. Restart Nucleus to load the new code.".to_string();
    refreshed
}

fn detect_restart_mode(
    daemon_binary: &Path,
    state_dir: Option<&Path>,
    bind: &str,
    repo_root: &Path,
) -> RestartMode {
    if let Ok(unit) = env::var("NUCLEUS_SYSTEMD_UNIT") {
        let unit = unit.trim();
        if !unit.is_empty() {
            return RestartMode::Systemd {
                unit: unit.to_string(),
            };
        }
    }

    if let Some(unit) = detect_systemd_unit(daemon_binary, state_dir, bind, repo_root) {
        return RestartMode::Systemd { unit };
    }

    if daemon_binary.is_file() {
        return RestartMode::SelfReexec;
    }

    RestartMode::Unsupported
}

fn detect_systemd_unit(
    daemon_binary: &Path,
    state_dir: Option<&Path>,
    bind: &str,
    repo_root: &Path,
) -> Option<String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (daemon_binary, state_dir, bind, repo_root);
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        let systemd_dir = dirs::home_dir()?.join(".config/systemd/user");
        let entries = fs::read_dir(systemd_dir).ok()?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("service") {
                continue;
            }

            let contents = match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(_) => continue,
            };

            if systemd_unit_matches(&contents, daemon_binary, state_dir, bind, repo_root) {
                return path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_string());
            }
        }

        None
    }
}

fn systemd_unit_matches(
    contents: &str,
    daemon_binary: &Path,
    state_dir: Option<&Path>,
    bind: &str,
    repo_root: &Path,
) -> bool {
    let exec_start = format!("ExecStart={}", display_path(daemon_binary));
    if !contents.lines().any(|line| line.trim() == exec_start) {
        return false;
    }

    let expected_bind = format!("Environment=NUCLEUS_BIND={bind}");
    if !contents.lines().any(|line| line.trim() == expected_bind) {
        return false;
    }

    let expected_repo_root = format!("Environment=NUCLEUS_REPO_ROOT={}", display_path(repo_root));
    if !contents
        .lines()
        .any(|line| line.trim() == expected_repo_root)
    {
        return false;
    }

    match state_dir {
        Some(state_dir) => {
            let expected_state =
                format!("Environment=NUCLEUS_STATE_DIR={}", display_path(state_dir));
            contents.lines().any(|line| line.trim() == expected_state)
        }
        None => true,
    }
}

async fn rebuild_daemon(instance: &InstanceRuntime) -> Result<()> {
    run_shell(
        &instance.repo_root,
        "source ~/.cargo/env >/dev/null 2>&1 || true\ncargo build -p nucleus-daemon",
        BUILD_TIMEOUT,
    )
    .await
    .map(|_| ())
}

async fn rebuild_managed_web(instance: &InstanceRuntime) -> Result<()> {
    if instance.managed_web_dist_dir.is_none() {
        return Ok(());
    }

    run_shell(
        &instance.repo_root,
        "source ~/.nvm/nvm.sh >/dev/null 2>&1 || true\nnpm run build:web",
        BUILD_TIMEOUT,
    )
    .await
    .map(|_| ())
}

async fn restart_systemd_unit(unit: &str) -> Result<()> {
    let started_at = Instant::now();
    let output = Command::new("systemctl")
        .args(["--user", "restart", unit])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start systemctl restart for {unit}"))?;

    let output = match timeout(
        BUILD_TIMEOUT.min(Duration::from_secs(30)),
        output.wait_with_output(),
    )
    .await
    {
        Ok(result) => {
            result.with_context(|| format!("systemctl restart {unit} failed to execute"))?
        }
        Err(_) => anyhow::bail!(
            "systemctl --user restart {unit} timed out after {}s",
            started_at.elapsed().as_secs()
        ),
    };

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    anyhow::bail!(
        "systemctl --user restart {unit} failed{}",
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    );
}

async fn relaunch_current_daemon(instance: &InstanceRuntime) -> Result<()> {
    let behavior = relaunch_behavior();
    let launch_path = if instance.is_managed_release() {
        instance
            .install_root
            .as_ref()
            .map(|root| current_release_binary_path(root))
            .unwrap_or_else(|| instance.daemon_binary.clone())
    } else {
        instance.daemon_binary.clone()
    };
    let mut command = Command::new(&launch_path);
    command
        .stdin(std::process::Stdio::null())
        .stdout(if behavior.detach_stdio {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::inherit()
        })
        .stderr(if behavior.detach_stdio {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::inherit()
        })
        .current_dir(if instance.is_managed_release() {
            instance
                .install_root
                .as_ref()
                .map(|root| current_release_dir(root))
                .unwrap_or_else(|| instance.repo_root.clone())
        } else {
            instance.repo_root.clone()
        })
        .env("NUCLEUS_INSTANCE_NAME", &instance.name)
        .env("NUCLEUS_BIND", &instance.daemon_bind)
        .env("NUCLEUS_INSTALL_KIND", &instance.install_kind);

    if instance.is_dev_checkout() {
        command.env("NUCLEUS_REPO_ROOT", &instance.repo_root);
    }

    if let Some(state_dir) = &instance.state_dir {
        command.env("NUCLEUS_STATE_DIR", state_dir);
    }

    if let Some(install_root) = &instance.install_root {
        command.env("NUCLEUS_INSTALL_ROOT", install_root);
    }

    if let Some(release_manifest_url) = &instance.release_manifest_url {
        command.env("NUCLEUS_RELEASE_MANIFEST_URL", release_manifest_url);
    }

    let web_dist_dir = if instance.is_managed_release() {
        instance
            .install_root
            .as_ref()
            .map(|root| current_release_web_dir(root))
            .or_else(|| instance.managed_web_dist_dir.clone())
    } else {
        instance.managed_web_dist_dir.clone()
    };

    if let Some(web_dist_dir) = web_dist_dir {
        command.env("NUCLEUS_WEB_DIST_DIR", web_dist_dir);
    }

    #[cfg(unix)]
    if behavior.detach_process_group {
        command.process_group(0);
    }

    command.spawn().with_context(|| {
        format!(
            "failed to relaunch {} from {}",
            PRODUCT_NAME,
            display_path(&launch_path)
        )
    })?;

    std::process::exit(0);
}

fn relaunch_behavior() -> RelaunchBehavior {
    RelaunchBehavior {
        detach_stdio: true,
        detach_process_group: true,
    }
}

fn detect_install_kind(repo_root: &Path) -> String {
    match env::var("NUCLEUS_INSTALL_KIND") {
        Ok(value)
            if value == INSTALL_KIND_DEV_CHECKOUT || value == INSTALL_KIND_MANAGED_RELEASE =>
        {
            value
        }
        _ if repo_root.join(".git").exists() => INSTALL_KIND_DEV_CHECKOUT.to_string(),
        _ => INSTALL_KIND_MANAGED_RELEASE.to_string(),
    }
}

fn initial_status(instance: &InstanceRuntime, stored: &StoredUpdateState) -> UpdateStatus {
    let tracked_ref = if instance.is_dev_checkout() {
        stored
            .tracked_ref
            .clone()
            .or_else(|| default_tracked_ref(instance))
    } else {
        None
    };
    let tracked_channel = if instance.is_dev_checkout() {
        None
    } else {
        Some(
            stored
                .tracked_channel
                .clone()
                .unwrap_or_else(|| DEFAULT_RELEASE_CHANNEL.to_string()),
        )
    };
    let current_ref = if instance.is_managed_release() {
        instance
            .install_root
            .as_deref()
            .and_then(|root| current_release_id(root).ok().flatten())
    } else {
        None
    };
    let restart_required = managed_restart_required_on_boot(
        instance,
        stored.restart_required,
        current_ref.as_deref(),
        stored.pending_restart_release_id.as_deref(),
    );

    UpdateStatus {
        install_kind: instance.install_kind.clone(),
        tracked_channel,
        tracked_ref,
        repo_root: instance
            .is_dev_checkout()
            .then(|| display_path(&instance.repo_root)),
        current_ref,
        remote_name: instance
            .is_dev_checkout()
            .then(|| DEFAULT_REMOTE_NAME.to_string()),
        remote_url: None,
        current_commit: None,
        current_commit_short: None,
        latest_commit: stored.last_successful_target_commit.clone(),
        latest_commit_short: stored
            .last_successful_target_commit
            .as_deref()
            .map(short_commit),
        latest_version: stored.last_successful_target_version.clone(),
        latest_release_id: stored.last_successful_target_release_id.clone(),
        update_available: stored.update_available,
        dirty_worktree: false,
        restart_required,
        last_successful_check_at: stored.last_successful_check_at,
        last_attempted_check_at: stored.last_attempted_check_at,
        last_attempt_result: stored.last_attempt_result.clone(),
        latest_error: stored.latest_error.clone(),
        latest_error_at: stored.latest_error_at,
        state: if instance.is_dev_checkout() {
            "idle".to_string()
        } else {
            "ready".to_string()
        },
        message: initial_message(
            instance,
            restart_required,
            stored.release_manifest_url.as_deref(),
        ),
    }
}

fn managed_restart_required_on_boot(
    instance: &InstanceRuntime,
    restart_required: bool,
    current_ref: Option<&str>,
    pending_restart_release_id: Option<&str>,
) -> bool {
    if !instance.is_managed_release() || !restart_required {
        return restart_required;
    }

    match (
        current_ref.and_then(non_empty),
        pending_restart_release_id.and_then(non_empty),
    ) {
        (Some(current_ref), Some(pending_restart_release_id))
            if current_ref == pending_restart_release_id =>
        {
            false
        }
        _ => true,
    }
}

fn initial_message(
    instance: &InstanceRuntime,
    restart_required: bool,
    _release_manifest_url: Option<&str>,
) -> String {
    if restart_required {
        return "Update applied. Restart Nucleus to load the new code.".to_string();
    }

    if instance.is_dev_checkout() {
        return "Automatic update checks are ready for this checkout.".to_string();
    }

    "This managed release tracks a release channel and fetches verified release artifacts."
        .to_string()
}

fn record_unsupported_attempt(
    instance: &InstanceRuntime,
    previous: &UpdateStatus,
    message: String,
) -> UpdateStatus {
    let mut next = previous.clone();
    next.install_kind = instance.install_kind.clone();
    next.state = "ready".to_string();
    next.message = message;
    next.last_attempted_check_at = Some(unix_timestamp());
    next.last_attempt_result = Some("unsupported".to_string());
    next.latest_error = None;
    next.latest_error_at = None;
    next
}

fn clear_update_history_for_retarget(state: &mut StoredUpdateState) {
    state.pending_restart_release_id = None;
    state.update_available = false;
    state.last_successful_check_at = None;
    state.last_successful_target_version = None;
    state.last_successful_target_release_id = None;
    state.last_successful_target_commit = None;
    state.last_attempted_check_at = None;
    state.last_attempt_result = None;
    state.latest_error = None;
    state.latest_error_at = None;
    state.restart_required = false;
}

fn stored_state_from_status(status: &UpdateStatus) -> StoredUpdateState {
    StoredUpdateState {
        tracked_channel: status.tracked_channel.clone(),
        tracked_ref: status.tracked_ref.clone(),
        release_manifest_url: None,
        pending_restart_release_id: None,
        update_available: status.update_available,
        last_successful_check_at: status.last_successful_check_at,
        last_successful_target_version: status.latest_version.clone(),
        last_successful_target_release_id: status.latest_release_id.clone(),
        last_successful_target_commit: status.latest_commit.clone(),
        last_attempted_check_at: status.last_attempted_check_at,
        last_attempt_result: status.last_attempt_result.clone(),
        latest_error: status.latest_error.clone(),
        latest_error_at: status.latest_error_at,
        restart_required: status.restart_required,
    }
}

fn next_pending_restart_release_id(
    status: &UpdateStatus,
    existing: &StoredUpdateState,
) -> Option<String> {
    if !status.restart_required && status.state != "restarting" {
        return None;
    }

    existing
        .pending_restart_release_id
        .clone()
        .or_else(|| status.latest_release_id.clone())
}

fn error_status(
    instance: &InstanceRuntime,
    previous: &UpdateStatus,
    message: String,
) -> UpdateStatus {
    let attempted_at = unix_timestamp();

    UpdateStatus {
        install_kind: instance.install_kind.clone(),
        tracked_channel: previous.tracked_channel.clone(),
        tracked_ref: previous.tracked_ref.clone(),
        repo_root: previous.repo_root.clone(),
        current_ref: previous.current_ref.clone(),
        remote_name: previous.remote_name.clone(),
        remote_url: previous.remote_url.clone(),
        current_commit: previous.current_commit.clone(),
        current_commit_short: previous.current_commit_short.clone(),
        latest_commit: previous.latest_commit.clone(),
        latest_commit_short: previous.latest_commit_short.clone(),
        latest_version: previous.latest_version.clone(),
        latest_release_id: previous.latest_release_id.clone(),
        update_available: previous.update_available,
        dirty_worktree: previous.dirty_worktree,
        restart_required: previous.restart_required,
        last_successful_check_at: previous.last_successful_check_at,
        last_attempted_check_at: Some(attempted_at),
        last_attempt_result: Some("error".to_string()),
        latest_error: Some(message.clone()),
        latest_error_at: Some(attempted_at),
        state: "error".to_string(),
        message,
    }
}

fn artifact_download_name(selected: &SelectedRelease) -> String {
    format!(
        "{}-{}-{}.tar.gz",
        nucleus_core::PRODUCT_SLUG,
        selected.release.release_id,
        selected.artifact.target
    )
}

async fn inspect_checkout(
    instance: &InstanceRuntime,
    previous: &UpdateStatus,
    fetch_remote: bool,
    restart_required: bool,
) -> Result<UpdateStatus> {
    if !instance.is_dev_checkout() {
        return Ok(initial_status(
            instance,
            &StoredUpdateState {
                tracked_channel: previous.tracked_channel.clone(),
                tracked_ref: previous.tracked_ref.clone(),
                release_manifest_url: None,
                pending_restart_release_id: None,
                update_available: previous.update_available,
                last_successful_check_at: previous.last_successful_check_at,
                last_successful_target_version: previous.latest_version.clone(),
                last_successful_target_release_id: previous.latest_release_id.clone(),
                last_successful_target_commit: previous.latest_commit.clone(),
                last_attempted_check_at: previous.last_attempted_check_at,
                last_attempt_result: previous.last_attempt_result.clone(),
                latest_error: previous.latest_error.clone(),
                latest_error_at: previous.latest_error_at,
                restart_required,
            },
        ));
    }

    let current_ref = run_git(
        &instance.repo_root,
        &["branch", "--show-current"],
        GIT_TIMEOUT,
    )
    .await?
    .trim()
    .to_string();
    let remote_url = run_git(
        &instance.repo_root,
        &["remote", "get-url", DEFAULT_REMOTE_NAME],
        GIT_TIMEOUT,
    )
    .await?;
    let current_commit = run_git(&instance.repo_root, &["rev-parse", "HEAD"], GIT_TIMEOUT).await?;
    let dirty_worktree = !run_git(&instance.repo_root, &["status", "--porcelain"], GIT_TIMEOUT)
        .await?
        .trim()
        .is_empty();
    let tracked_ref = previous
        .tracked_ref
        .clone()
        .or_else(|| default_tracked_ref(instance));

    if tracked_ref
        .as_deref()
        .zip(non_empty(&current_ref))
        .is_some_and(|(tracked, current)| tracked != current)
    {
        return Ok(checkout_mismatch_status(
            instance,
            previous,
            tracked_ref,
            non_empty(&current_ref).map(ToOwned::to_owned),
            Some(current_commit),
            dirty_worktree,
            restart_required,
        ));
    }

    if tracked_ref.is_some() && current_ref.trim().is_empty() {
        return Ok(checkout_mismatch_status(
            instance,
            previous,
            tracked_ref,
            None,
            Some(current_commit),
            dirty_worktree,
            restart_required,
        ));
    }

    let Some(tracked_ref) = tracked_ref else {
        return Ok(error_status(
            instance,
            previous,
            "This checkout does not have a tracked git ref yet.".to_string(),
        ));
    };

    if fetch_remote {
        let fetch_args = [
            "fetch",
            "--quiet",
            DEFAULT_REMOTE_NAME,
            tracked_ref.as_str(),
        ];
        run_git(&instance.repo_root, &fetch_args, GIT_TIMEOUT).await?;
    }

    let remote_ref = format!("{DEFAULT_REMOTE_NAME}/{tracked_ref}");
    let latest_commit = run_git(
        &instance.repo_root,
        &["rev-parse", remote_ref.as_str()],
        GIT_TIMEOUT,
    )
    .await?;
    let update_available =
        !current_commit.is_empty() && !latest_commit.is_empty() && current_commit != latest_commit;
    let checked_at = unix_timestamp();

    Ok(UpdateStatus {
        install_kind: instance.install_kind.clone(),
        tracked_channel: None,
        tracked_ref: Some(tracked_ref),
        repo_root: Some(display_path(&instance.repo_root)),
        current_ref: non_empty(&current_ref).map(ToOwned::to_owned),
        remote_name: Some(DEFAULT_REMOTE_NAME.to_string()),
        remote_url: Some(remote_url),
        current_commit_short: Some(short_commit(&current_commit)),
        current_commit: Some(current_commit),
        latest_commit_short: Some(short_commit(&latest_commit)),
        latest_commit: Some(latest_commit),
        latest_version: previous.latest_version.clone(),
        latest_release_id: previous.latest_release_id.clone(),
        update_available,
        dirty_worktree,
        restart_required,
        last_successful_check_at: Some(checked_at),
        last_attempted_check_at: Some(checked_at),
        last_attempt_result: Some("success".to_string()),
        latest_error: None,
        latest_error_at: None,
        state: "ready".to_string(),
        message: update_message(update_available, dirty_worktree, restart_required),
    })
}

async fn inspect_managed_release(
    instance: &InstanceRuntime,
    previous: &UpdateStatus,
    manifest_url: Option<String>,
    restart_required: bool,
) -> Result<UpdateStatus> {
    let Some(tracked_channel) = previous
        .tracked_channel
        .clone()
        .or_else(|| Some(DEFAULT_RELEASE_CHANNEL.to_string()))
    else {
        bail!("managed release channel is not configured");
    };

    let Some(manifest_url) = manifest_url else {
        return Ok(record_unsupported_attempt(
            instance,
            previous,
            "This managed release does not have a release manifest URL configured yet.".to_string(),
        ));
    };

    let install_root = instance
        .install_root
        .as_deref()
        .ok_or_else(|| anyhow!("managed release install root is not configured"))?;
    let manifest: ManagedReleaseManifest = load_manifest(&manifest_url).await?;
    let selected = select_release(&manifest, &tracked_channel, &current_platform_target())?;
    let current_release_id = current_release_id(install_root)?;
    let current_metadata = read_installed_release_metadata(install_root)?;
    let checked_at = unix_timestamp();
    let update_available = current_release_id
        .as_deref()
        .map(|current| current != selected.release.release_id)
        .unwrap_or(true);
    Ok(UpdateStatus {
        install_kind: instance.install_kind.clone(),
        tracked_channel: Some(tracked_channel.clone()),
        tracked_ref: None,
        repo_root: None,
        current_ref: current_release_id,
        remote_name: None,
        remote_url: None,
        current_commit: None,
        current_commit_short: None,
        latest_commit: None,
        latest_commit_short: None,
        latest_version: Some(selected.release.version.clone()),
        latest_release_id: Some(selected.release.release_id.clone()),
        update_available,
        dirty_worktree: false,
        restart_required,
        last_successful_check_at: Some(checked_at),
        last_attempted_check_at: Some(checked_at),
        last_attempt_result: Some("success".to_string()),
        latest_error: None,
        latest_error_at: None,
        state: "ready".to_string(),
        message: managed_release_message(
            update_available,
            restart_required,
            current_metadata.as_ref().map(|item| item.version.as_str()),
            &selected.release.version,
            &tracked_channel,
        ),
    })
}

fn checkout_mismatch_status(
    instance: &InstanceRuntime,
    previous: &UpdateStatus,
    tracked_ref: Option<String>,
    current_ref: Option<String>,
    current_commit: Option<String>,
    dirty_worktree: bool,
    restart_required: bool,
) -> UpdateStatus {
    let attempted_at = unix_timestamp();
    let message = match (&tracked_ref, &current_ref) {
        (Some(tracked), Some(current)) => format!(
            "This checkout is on '{current}' but tracks '{tracked}'. Switch back before checking or applying updates."
        ),
        (Some(tracked), None) => format!(
            "This checkout is detached from a branch. Switch to '{tracked}' before checking or applying updates."
        ),
        _ => "This checkout does not have a tracked git ref yet.".to_string(),
    };

    UpdateStatus {
        install_kind: instance.install_kind.clone(),
        tracked_channel: None,
        tracked_ref,
        repo_root: Some(display_path(&instance.repo_root)),
        current_ref,
        remote_name: previous
            .remote_name
            .clone()
            .or_else(|| Some(DEFAULT_REMOTE_NAME.to_string())),
        remote_url: previous.remote_url.clone(),
        current_commit_short: current_commit.as_deref().map(short_commit),
        current_commit,
        latest_commit: previous.latest_commit.clone(),
        latest_commit_short: previous.latest_commit_short.clone(),
        latest_version: previous.latest_version.clone(),
        latest_release_id: previous.latest_release_id.clone(),
        update_available: false,
        dirty_worktree,
        restart_required,
        last_successful_check_at: previous.last_successful_check_at,
        last_attempted_check_at: Some(attempted_at),
        last_attempt_result: Some("error".to_string()),
        latest_error: Some(message.clone()),
        latest_error_at: Some(attempted_at),
        state: "error".to_string(),
        message,
    }
}

fn default_tracked_ref(instance: &InstanceRuntime) -> Option<String> {
    if !instance.is_dev_checkout() {
        return None;
    }

    match run_git_sync(&instance.repo_root, &["branch", "--show-current"]) {
        Ok(current_ref) if !current_ref.trim().is_empty() => Some(current_ref.trim().to_string()),
        _ => match run_git_sync(
            &instance.repo_root,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        ) {
            Ok(remote_head) if !remote_head.trim().is_empty() => {
                Some(remote_head.trim().trim_start_matches("origin/").to_string())
            }
            _ => Some("main".to_string()),
        },
    }
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn update_message(update_available: bool, dirty_worktree: bool, restart_required: bool) -> String {
    if restart_required {
        return "Update applied. Restart Nucleus to load the new code.".to_string();
    }

    if update_available && dirty_worktree {
        return "Update available, but local changes must be resolved before applying it."
            .to_string();
    }

    if update_available {
        return "A newer version of Nucleus is available.".to_string();
    }

    "Nucleus is up to date.".to_string()
}

fn managed_release_message(
    update_available: bool,
    restart_required: bool,
    current_version: Option<&str>,
    latest_version: &str,
    tracked_channel: &str,
) -> String {
    if restart_required {
        return "Update applied. Restart Nucleus to load the new code.".to_string();
    }

    if update_available {
        return match current_version {
            Some(current_version) => format!(
                "A newer {tracked_channel} release is available: {current_version} -> {latest_version}."
            ),
            None => format!("A newer {tracked_channel} release is available: {latest_version}."),
        };
    }

    format!("Nucleus is up to date on the {tracked_channel} channel.")
}

async fn run_shell(repo_root: &Path, script: &str, timeout_window: Duration) -> Result<String> {
    let mut child = Command::new("bash");
    child
        .arg("-lc")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(repo_root);

    let child = child.spawn().with_context(|| {
        format!(
            "failed to start shell step in '{}'",
            display_path(repo_root)
        )
    })?;
    let output = match timeout(timeout_window, child.wait_with_output()).await {
        Ok(result) => result.context("shell step failed to execute")?,
        Err(_) => anyhow::bail!("shell step timed out after {}s", timeout_window.as_secs()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        anyhow::bail!(
            "shell step failed{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }

    Ok(stdout)
}

async fn run_git(repo_root: &Path, args: &[&str], timeout_window: Duration) -> Result<String> {
    let mut child = Command::new("git");
    child
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(repo_root);

    let child = child
        .spawn()
        .with_context(|| format!("failed to start git {:?}", args))?;
    let output = match timeout(timeout_window, child.wait_with_output()).await {
        Ok(result) => result.with_context(|| format!("git {:?} failed to execute", args))?,
        Err(_) => anyhow::bail!("git {:?} timed out", args),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        anyhow::bail!(
            "git {} failed{}",
            args.join(" "),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }

    Ok(stdout)
}

fn run_git_sync(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to start git {:?}", args))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        anyhow::bail!(
            "git {} failed{}",
            args.join(" "),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }

    Ok(stdout)
}

fn short_commit(value: &str) -> String {
    value.chars().take(7).collect()
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_RELEASE_CHANNEL, INSTALL_KIND_MANAGED_RELEASE, InstanceRuntime, RelaunchBehavior,
        RestartMode, UpdateStatus, finalize_updated_status, initial_status, relaunch_behavior,
        short_commit, systemd_unit_matches,
    };
    use nucleus_release::{
        ReleasePackageInput, activate_release, current_platform_target,
        current_release_binary_path, current_release_id, current_release_web_dir,
        package_release_artifact, stage_release_archive,
    };
    use nucleus_storage::{StateStore, StoredUpdateState};
    use std::{fs, path::PathBuf, sync::Arc, time::Duration};
    use uuid::Uuid;

    fn sample_instance(restart_mode: RestartMode) -> InstanceRuntime {
        InstanceRuntime {
            name: "Nucleus".to_string(),
            repo_root: PathBuf::from("/tmp/nucleus"),
            daemon_bind: "127.0.0.1:42240".to_string(),
            install_kind: "dev_checkout".to_string(),
            install_root: None,
            release_manifest_url: None,
            state_dir: Some(PathBuf::from("/tmp/.nucleus")),
            daemon_binary: PathBuf::from("/tmp/nucleus/target/debug/nucleus-daemon"),
            managed_web_dist_dir: Some(PathBuf::from("/tmp/nucleus/apps/web/build")),
            restart_mode,
        }
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nucleus-managed-release-{label}-{}",
            Uuid::new_v4()
        ))
    }

    fn write_release_inputs(root: &PathBuf, label: &str, web_body: &str) -> (PathBuf, PathBuf) {
        let bin_dir = root.join(format!("{label}-bin"));
        let web_dir = root.join(format!("{label}-web"));
        fs::create_dir_all(&bin_dir).expect("bin dir should exist");
        fs::create_dir_all(&web_dir).expect("web dir should exist");
        fs::write(bin_dir.join("nucleus-daemon"), format!("daemon-{label}"))
            .expect("daemon binary should write");
        fs::write(web_dir.join("index.html"), web_body).expect("web build should write");
        (bin_dir.join("nucleus-daemon"), web_dir)
    }

    fn sample_status() -> UpdateStatus {
        UpdateStatus {
            install_kind: "dev_checkout".to_string(),
            tracked_channel: None,
            tracked_ref: Some("main".to_string()),
            repo_root: Some("/tmp/nucleus".to_string()),
            current_ref: Some("main".to_string()),
            remote_name: Some("origin".to_string()),
            remote_url: Some("git@github.com:WebLime-agency/nucleus.git".to_string()),
            current_commit: Some("abcdef1234567890".to_string()),
            current_commit_short: Some("abcdef1".to_string()),
            latest_commit: Some("abcdef1234567890".to_string()),
            latest_commit_short: Some("abcdef1".to_string()),
            latest_version: None,
            latest_release_id: None,
            update_available: true,
            dirty_worktree: false,
            restart_required: false,
            last_successful_check_at: Some(1),
            last_attempted_check_at: Some(1),
            last_attempt_result: Some("success".to_string()),
            latest_error: None,
            latest_error_at: None,
            state: "ready".to_string(),
            message: "A newer version of Nucleus is available.".to_string(),
        }
    }

    #[test]
    fn short_commit_truncates_long_hashes() {
        assert_eq!(short_commit("1234567890abcdef"), "1234567");
        assert_eq!(short_commit("abc"), "abc");
    }

    #[test]
    fn initial_status_defaults_managed_releases_to_stable_channel() {
        let instance = InstanceRuntime {
            name: "Nucleus".to_string(),
            repo_root: PathBuf::from("/tmp/nucleus"),
            daemon_bind: "127.0.0.1:42240".to_string(),
            install_kind: INSTALL_KIND_MANAGED_RELEASE.to_string(),
            install_root: Some(PathBuf::from("/tmp/nucleus-managed")),
            release_manifest_url: None,
            state_dir: None,
            daemon_binary: PathBuf::from("/tmp/nucleus-daemon"),
            managed_web_dist_dir: None,
            restart_mode: RestartMode::Unsupported,
        };

        let status = initial_status(&instance, &StoredUpdateState::default());
        assert_eq!(
            status.tracked_channel.as_deref(),
            Some(DEFAULT_RELEASE_CHANNEL)
        );
        assert_eq!(status.state, "ready");
        assert!(!status.update_available);
    }

    #[test]
    fn managed_release_defaults_to_public_channel_manifest_url() {
        let root = test_root("default-manifest");
        let state_dir = root.join("state");
        let install_root = root.join("install");
        let store =
            Arc::new(StateStore::initialize_at(&state_dir).expect("store should initialize"));
        let instance = InstanceRuntime {
            name: "Nucleus".to_string(),
            repo_root: root.clone(),
            daemon_bind: "127.0.0.1:42240".to_string(),
            install_kind: INSTALL_KIND_MANAGED_RELEASE.to_string(),
            install_root: Some(install_root),
            release_manifest_url: None,
            state_dir: Some(state_dir),
            daemon_binary: PathBuf::from("/tmp/nucleus-daemon"),
            managed_web_dist_dir: None,
            restart_mode: RestartMode::Unsupported,
        };
        let manager = super::UpdateManager::new(instance, store);

        assert!(manager.auto_check_enabled());
        assert_eq!(
            manager.managed_release_manifest_url().as_deref(),
            Some(
                "https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-stable/manifest-stable.json"
            )
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn finalized_update_requests_restart_when_restart_control_exists() {
        let next =
            finalize_updated_status(&sample_instance(RestartMode::SelfReexec), sample_status());

        assert_eq!(next.state, "restarting");
        assert!(!next.restart_required);
        assert_eq!(
            next.message,
            "Update applied. Restarting Nucleus now. The connection will return automatically."
        );
    }

    #[test]
    fn finalized_update_falls_back_to_manual_restart_when_restart_control_is_missing() {
        let next =
            finalize_updated_status(&sample_instance(RestartMode::Unsupported), sample_status());

        assert_eq!(next.state, "ready");
        assert!(next.restart_required);
        assert_eq!(
            next.message,
            "Update applied. Restart Nucleus to load the new code."
        );
    }

    #[test]
    fn self_reexec_relaunch_detaches_from_parent_process() {
        let behavior = relaunch_behavior();

        assert_eq!(
            behavior,
            RelaunchBehavior {
                detach_stdio: true,
                detach_process_group: true,
            }
        );
    }

    #[test]
    fn systemd_unit_match_requires_binary_bind_and_repo_root() {
        let instance = sample_instance(RestartMode::Unsupported);
        let contents = format!(
            "[Service]\nExecStart={}\nEnvironment=NUCLEUS_BIND={}\nEnvironment=NUCLEUS_REPO_ROOT={}\nEnvironment=NUCLEUS_STATE_DIR={}\n",
            instance.daemon_binary.display(),
            instance.daemon_bind,
            instance.repo_root.display(),
            instance.state_dir.as_ref().unwrap().display()
        );

        assert!(systemd_unit_matches(
            &contents,
            &instance.daemon_binary,
            instance.state_dir.as_deref(),
            &instance.daemon_bind,
            &instance.repo_root,
        ));
    }

    #[tokio::test]
    async fn managed_release_check_and_apply_use_local_artifacts() {
        let root = test_root("apply");
        let output_dir = root.join("dist");
        let install_root = root.join("install");
        let state_dir = root.join("state");
        let manifest_path = output_dir.join("manifest-stable.json");
        let target = current_platform_target();

        let (current_binary, current_web) =
            write_release_inputs(&root, "current", "<html>current</html>");
        let packaged_current = package_release_artifact(ReleasePackageInput {
            release_id: "rel_current".to_string(),
            version: "0.1.0".to_string(),
            channel: DEFAULT_RELEASE_CHANNEL.to_string(),
            daemon_binary: current_binary,
            cli_binary: None,
            web_dist_dir: current_web,
            output_dir: output_dir.clone(),
            artifact_base_url: None,
            manifest_path: Some(manifest_path.clone()),
            target: Some(target.clone()),
            minimum_client_version: None,
            minimum_server_version: None,
            capability_flags: Vec::new(),
        })
        .expect("current package should succeed");
        stage_release_archive(&packaged_current.archive_path, &install_root, "rel_current")
            .expect("current release should stage");
        activate_release(&install_root, "rel_current").expect("current release should activate");
        std::thread::sleep(Duration::from_secs(1));

        let (next_binary, next_web) = write_release_inputs(&root, "next", "<html>next</html>");
        let packaged_next = package_release_artifact(ReleasePackageInput {
            release_id: "rel_next".to_string(),
            version: "0.2.0".to_string(),
            channel: DEFAULT_RELEASE_CHANNEL.to_string(),
            daemon_binary: next_binary,
            cli_binary: None,
            web_dist_dir: next_web,
            output_dir: output_dir.clone(),
            artifact_base_url: None,
            manifest_path: Some(manifest_path.clone()),
            target: Some(target),
            minimum_client_version: None,
            minimum_server_version: None,
            capability_flags: Vec::new(),
        })
        .expect("next package should succeed");

        let store =
            Arc::new(StateStore::initialize_at(&state_dir).expect("store should initialize"));
        store
            .write_update_state(&StoredUpdateState {
                tracked_channel: Some(DEFAULT_RELEASE_CHANNEL.to_string()),
                tracked_ref: None,
                release_manifest_url: Some(format!("file://{}", manifest_path.display())),
                pending_restart_release_id: None,
                update_available: false,
                last_successful_check_at: None,
                last_successful_target_version: None,
                last_successful_target_release_id: None,
                last_successful_target_commit: None,
                last_attempted_check_at: None,
                last_attempt_result: None,
                latest_error: None,
                latest_error_at: None,
                restart_required: false,
            })
            .expect("update state should write");

        let instance = InstanceRuntime {
            name: "Nucleus".to_string(),
            repo_root: root.clone(),
            daemon_bind: "127.0.0.1:42240".to_string(),
            install_kind: INSTALL_KIND_MANAGED_RELEASE.to_string(),
            install_root: Some(install_root.clone()),
            release_manifest_url: Some(format!("file://{}", manifest_path.display())),
            state_dir: Some(state_dir.clone()),
            daemon_binary: current_release_binary_path(&install_root),
            managed_web_dist_dir: Some(current_release_web_dir(&install_root)),
            restart_mode: RestartMode::Unsupported,
        };
        let manager = super::UpdateManager::new(instance.clone(), store.clone());

        let checked = manager.check().await;
        assert!(checked.status.update_available);
        assert_eq!(
            checked.status.tracked_channel.as_deref(),
            Some(DEFAULT_RELEASE_CHANNEL)
        );
        assert_eq!(checked.status.current_ref.as_deref(), Some("rel_current"));
        assert_eq!(
            checked.status.latest_release_id.as_deref(),
            Some("rel_next")
        );
        assert_eq!(checked.status.latest_version.as_deref(), Some("0.2.0"));

        let applied = manager.apply().await;
        assert_eq!(
            applied.status.latest_release_id.as_deref(),
            Some("rel_next")
        );
        assert_eq!(
            current_release_id(&install_root).expect("current release should read"),
            Some("rel_next".to_string()),
            "{:?}",
            applied.status
        );
        assert!(
            current_release_web_dir(&install_root)
                .join("index.html")
                .is_file()
        );
        assert!(packaged_next.archive_path.is_file());

        std::thread::sleep(Duration::from_secs(1));
        let (newest_binary, newest_web) =
            write_release_inputs(&root, "newest", "<html>newest</html>");
        let packaged_newest = package_release_artifact(ReleasePackageInput {
            release_id: "rel_newest".to_string(),
            version: "0.3.0".to_string(),
            channel: DEFAULT_RELEASE_CHANNEL.to_string(),
            daemon_binary: newest_binary,
            cli_binary: None,
            web_dist_dir: newest_web,
            output_dir: output_dir.clone(),
            artifact_base_url: None,
            manifest_path: Some(manifest_path.clone()),
            target: Some(current_platform_target()),
            minimum_client_version: None,
            minimum_server_version: None,
            capability_flags: Vec::new(),
        })
        .expect("newest package should succeed");
        assert!(packaged_newest.archive_path.is_file());

        let post_apply = manager.check().await;
        assert!(post_apply.status.update_available);
        assert_eq!(post_apply.status.current_ref.as_deref(), Some("rel_next"));
        assert_eq!(
            post_apply.status.latest_release_id.as_deref(),
            Some("rel_newest")
        );

        let persisted = store
            .read_update_state()
            .expect("persisted update state should read");
        assert!(persisted.restart_required);
        assert_eq!(
            persisted.pending_restart_release_id.as_deref(),
            Some("rel_next")
        );
        assert_eq!(
            persisted.last_successful_target_release_id.as_deref(),
            Some("rel_newest")
        );

        let restarted = super::UpdateManager::new(instance, store.clone());
        let restarted_status = restarted.current().await;
        assert_eq!(restarted_status.current_ref.as_deref(), Some("rel_next"));
        assert!(!restarted_status.restart_required, "{:?}", restarted_status);
        assert!(restarted_status.update_available, "{:?}", restarted_status);
        assert_eq!(
            restarted_status.latest_release_id.as_deref(),
            Some("rel_newest")
        );

        let reconciled = store
            .read_update_state()
            .expect("reconciled update state should read");
        assert!(!reconciled.restart_required, "{:?}", reconciled);
        assert_eq!(reconciled.pending_restart_release_id, None);

        let _ = fs::remove_dir_all(root);
    }
}
