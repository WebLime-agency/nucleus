use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use nucleus_core::PRODUCT_NAME;
use nucleus_protocol::{InstanceSummary, UpdateStatus};
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
    pub install_mode: String,
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
        let managed_web_dist_dir = env::var("NUCLEUS_WEB_DIST_DIR").ok().map(PathBuf::from);
        let restart_mode = detect_restart_mode(
            &daemon_binary,
            state_dir.as_deref(),
            &daemon_bind,
            &repo_root,
        );

        Self {
            name: env::var("NUCLEUS_INSTANCE_NAME").unwrap_or_else(|_| PRODUCT_NAME.to_string()),
            install_mode: if repo_root.join(".git").exists() {
                "git".to_string()
            } else {
                "unsupported".to_string()
            },
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
            repo_root: display_path(&self.repo_root),
            daemon_bind: self.daemon_bind.clone(),
            install_mode: self.install_mode.clone(),
            restart_mode: self.restart_mode.label(),
            restart_supported: self.restart_mode.supported(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        name: impl Into<String>,
        repo_root: PathBuf,
        daemon_bind: impl Into<String>,
        install_mode: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            repo_root: repo_root.clone(),
            daemon_bind: daemon_bind.into(),
            install_mode: install_mode.into(),
            state_dir: None,
            daemon_binary: repo_root.join("target/debug/nucleus-daemon"),
            managed_web_dist_dir: None,
            restart_mode: RestartMode::Unsupported,
        }
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
    state: Mutex<UpdateStatus>,
    operation_lock: Mutex<()>,
}

impl UpdateManager {
    pub fn new(instance: InstanceRuntime) -> Self {
        let initial = idle_status(&instance);
        Self {
            instance,
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

    pub async fn check(&self) -> UpdateRefresh {
        let _guard = self.operation_lock.lock().await;
        self.transition_state("checking", "Checking for updates...")
            .await;

        let previous = self.current().await;
        let next = inspect_repo(&self.instance, true, previous.restart_required)
            .await
            .unwrap_or_else(|error| error_status(&self.instance, &previous, error.to_string()));

        self.replace_state(next, false).await
    }

    pub async fn apply(&self) -> UpdateRefresh {
        let _guard = self.operation_lock.lock().await;
        self.transition_state("applying", "Applying update and rebuilding Nucleus...")
            .await;

        let previous = self.current().await;
        let inspected = inspect_repo(&self.instance, true, previous.restart_required)
            .await
            .unwrap_or_else(|error| error_status(&self.instance, &previous, error.to_string()));

        if inspected.install_mode != "git" {
            return self.replace_state(inspected, false).await;
        }

        if inspected.dirty_worktree {
            let mut next = inspected;
            next.state = "error".to_string();
            next.message =
                "Cannot update this checkout while the working tree has local changes.".to_string();
            next.checked_at = Some(unix_timestamp());
            return self.replace_state(next, false).await;
        }

        if !inspected.update_available {
            let mut next = inspected;
            next.state = "ready".to_string();
            next.message = "Nucleus is already up to date.".to_string();
            next.checked_at = Some(unix_timestamp());
            return self.replace_state(next, false).await;
        }

        let before_commit = inspected.current_commit.clone();
        let pull_args = [
            "pull",
            "--ff-only",
            DEFAULT_REMOTE_NAME,
            inspected.branch.as_str(),
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
                    Ok(()) => inspect_repo(&self.instance, false, false)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                }
            }
            Err(error) => Err(error.to_string()),
        };

        match refreshed {
            Ok(mut refreshed) => {
                if !before_commit.is_empty() && refreshed.current_commit == before_commit {
                    refreshed.checked_at = Some(unix_timestamp());
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
        next.checked_at = Some(unix_timestamp());

        self.replace_state(next, true).await
    }

    pub async fn mark_restart_failure(&self, message: String) -> UpdateRefresh {
        let current = self.current().await;
        let next = error_status(&self.instance, &current, message);
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
}

fn finalize_updated_status(
    instance: &InstanceRuntime,
    mut refreshed: UpdateStatus,
) -> UpdateStatus {
    refreshed.checked_at = Some(unix_timestamp());
    refreshed.update_available = false;

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
    let mut command = Command::new(&instance.daemon_binary);
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
        .current_dir(&instance.repo_root)
        .env("NUCLEUS_INSTANCE_NAME", &instance.name)
        .env("NUCLEUS_BIND", &instance.daemon_bind)
        .env("NUCLEUS_REPO_ROOT", &instance.repo_root);

    if let Some(state_dir) = &instance.state_dir {
        command.env("NUCLEUS_STATE_DIR", state_dir);
    }

    if let Some(web_dist_dir) = &instance.managed_web_dist_dir {
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
            display_path(&instance.daemon_binary)
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

fn idle_status(instance: &InstanceRuntime) -> UpdateStatus {
    let supported = instance.install_mode == "git";

    UpdateStatus {
        install_mode: instance.install_mode.clone(),
        repo_root: display_path(&instance.repo_root),
        branch: String::new(),
        remote_name: DEFAULT_REMOTE_NAME.to_string(),
        remote_url: String::new(),
        current_commit: String::new(),
        current_commit_short: String::new(),
        remote_commit: String::new(),
        remote_commit_short: String::new(),
        update_available: false,
        dirty_worktree: false,
        restart_required: false,
        checked_at: None,
        state: if supported {
            "idle".to_string()
        } else {
            "unsupported".to_string()
        },
        message: if supported {
            "Automatic update checks are ready for this checkout.".to_string()
        } else {
            "Automatic updates require a git checkout with an origin remote.".to_string()
        },
    }
}

fn error_status(
    instance: &InstanceRuntime,
    previous: &UpdateStatus,
    message: String,
) -> UpdateStatus {
    UpdateStatus {
        install_mode: instance.install_mode.clone(),
        repo_root: display_path(&instance.repo_root),
        branch: previous.branch.clone(),
        remote_name: if previous.remote_name.is_empty() {
            DEFAULT_REMOTE_NAME.to_string()
        } else {
            previous.remote_name.clone()
        },
        remote_url: previous.remote_url.clone(),
        current_commit: previous.current_commit.clone(),
        current_commit_short: previous.current_commit_short.clone(),
        remote_commit: previous.remote_commit.clone(),
        remote_commit_short: previous.remote_commit_short.clone(),
        update_available: previous.update_available,
        dirty_worktree: previous.dirty_worktree,
        restart_required: previous.restart_required,
        checked_at: Some(unix_timestamp()),
        state: "error".to_string(),
        message,
    }
}

async fn inspect_repo(
    instance: &InstanceRuntime,
    fetch_remote: bool,
    restart_required: bool,
) -> Result<UpdateStatus> {
    if instance.install_mode != "git" {
        return Ok(idle_status(instance));
    }

    let branch = run_git(
        &instance.repo_root,
        &["branch", "--show-current"],
        GIT_TIMEOUT,
    )
    .await?;
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

    if fetch_remote {
        if branch.is_empty() {
            let fetch_args = ["fetch", "--quiet", DEFAULT_REMOTE_NAME];
            run_git(&instance.repo_root, &fetch_args, GIT_TIMEOUT).await?;
        } else {
            let fetch_args = ["fetch", "--quiet", DEFAULT_REMOTE_NAME, branch.as_str()];
            run_git(&instance.repo_root, &fetch_args, GIT_TIMEOUT).await?;
        }
    }

    let remote_ref = resolve_remote_ref(&instance.repo_root, &branch).await?;
    let remote_commit = run_git(
        &instance.repo_root,
        &["rev-parse", remote_ref.as_str()],
        GIT_TIMEOUT,
    )
    .await?;
    let update_available =
        !current_commit.is_empty() && !remote_commit.is_empty() && current_commit != remote_commit;

    Ok(UpdateStatus {
        install_mode: instance.install_mode.clone(),
        repo_root: display_path(&instance.repo_root),
        branch,
        remote_name: DEFAULT_REMOTE_NAME.to_string(),
        remote_url,
        current_commit_short: short_commit(&current_commit),
        current_commit,
        remote_commit_short: short_commit(&remote_commit),
        remote_commit,
        update_available,
        dirty_worktree,
        restart_required,
        checked_at: Some(unix_timestamp()),
        state: "ready".to_string(),
        message: update_message(update_available, dirty_worktree, restart_required),
    })
}

async fn resolve_remote_ref(repo_root: &Path, branch: &str) -> Result<String> {
    if !branch.is_empty() {
        return Ok(format!("{DEFAULT_REMOTE_NAME}/{branch}"));
    }

    let remote_head = run_git(
        repo_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        GIT_TIMEOUT,
    )
    .await?;

    if remote_head.is_empty() {
        return Ok("origin/main".to_string());
    }

    Ok(remote_head)
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
        InstanceRuntime, RelaunchBehavior, RestartMode, UpdateStatus, finalize_updated_status,
        idle_status, relaunch_behavior, short_commit, systemd_unit_matches,
    };
    use std::path::PathBuf;

    fn sample_instance(restart_mode: RestartMode) -> InstanceRuntime {
        InstanceRuntime {
            name: "Nucleus".to_string(),
            repo_root: PathBuf::from("/tmp/nucleus"),
            daemon_bind: "127.0.0.1:42240".to_string(),
            install_mode: "git".to_string(),
            state_dir: Some(PathBuf::from("/tmp/.nucleus")),
            daemon_binary: PathBuf::from("/tmp/nucleus/target/debug/nucleus-daemon"),
            managed_web_dist_dir: Some(PathBuf::from("/tmp/nucleus/apps/web/build")),
            restart_mode,
        }
    }

    fn sample_status() -> UpdateStatus {
        UpdateStatus {
            install_mode: "git".to_string(),
            repo_root: "/tmp/nucleus".to_string(),
            branch: "main".to_string(),
            remote_name: "origin".to_string(),
            remote_url: "git@github.com:WebLime-agency/nucleus.git".to_string(),
            current_commit: "abcdef1234567890".to_string(),
            current_commit_short: "abcdef1".to_string(),
            remote_commit: "abcdef1234567890".to_string(),
            remote_commit_short: "abcdef1".to_string(),
            update_available: true,
            dirty_worktree: false,
            restart_required: false,
            checked_at: Some(1),
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
    fn idle_status_marks_non_git_checkouts_as_unsupported() {
        let instance = InstanceRuntime {
            name: "Nucleus".to_string(),
            repo_root: PathBuf::from("/tmp/nucleus"),
            daemon_bind: "127.0.0.1:42240".to_string(),
            install_mode: "unsupported".to_string(),
            state_dir: None,
            daemon_binary: PathBuf::from("/tmp/nucleus-daemon"),
            managed_web_dist_dir: None,
            restart_mode: RestartMode::Unsupported,
        };

        let status = idle_status(&instance);
        assert_eq!(status.state, "unsupported");
        assert!(!status.update_available);
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
}
