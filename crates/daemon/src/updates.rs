use std::{
    env,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use nucleus_core::PRODUCT_NAME;
use nucleus_protocol::{InstanceSummary, UpdateStatus};
use tokio::{process::Command, sync::Mutex, time::timeout};

const DEFAULT_REMOTE_NAME: &str = "origin";
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct InstanceRuntime {
    pub name: String,
    pub repo_root: PathBuf,
    pub daemon_bind: String,
    pub install_mode: String,
}

impl InstanceRuntime {
    pub fn detect(daemon_bind: String) -> Self {
        let repo_root = env::var("NUCLEUS_REPO_ROOT")
            .map(PathBuf::from)
            .ok()
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            name: env::var("NUCLEUS_INSTANCE_NAME").unwrap_or_else(|_| PRODUCT_NAME.to_string()),
            install_mode: if repo_root.join(".git").exists() {
                "git".to_string()
            } else {
                "unsupported".to_string()
            },
            repo_root,
            daemon_bind,
        }
    }

    pub fn summary(&self) -> InstanceSummary {
        InstanceSummary {
            name: self.name.clone(),
            repo_root: display_path(&self.repo_root),
            daemon_bind: self.daemon_bind.clone(),
            install_mode: self.install_mode.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpdateRefresh {
    pub status: UpdateStatus,
    pub changed: bool,
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

        self.replace_state(next).await
    }

    pub async fn apply(&self) -> UpdateRefresh {
        let _guard = self.operation_lock.lock().await;
        self.transition_state("applying", "Applying update...")
            .await;

        let previous = self.current().await;
        let inspected = inspect_repo(&self.instance, true, previous.restart_required)
            .await
            .unwrap_or_else(|error| error_status(&self.instance, &previous, error.to_string()));

        if inspected.install_mode != "git" {
            return self.replace_state(inspected).await;
        }

        if inspected.dirty_worktree {
            let mut next = inspected;
            next.state = "error".to_string();
            next.message =
                "Cannot update this checkout while the working tree has local changes.".to_string();
            next.checked_at = Some(unix_timestamp());
            return self.replace_state(next).await;
        }

        if !inspected.update_available {
            let mut next = inspected;
            next.state = "ready".to_string();
            next.message = "Nucleus is already up to date.".to_string();
            next.checked_at = Some(unix_timestamp());
            return self.replace_state(next).await;
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

        let next = match pull_result {
            Ok(()) => match inspect_repo(&self.instance, false, true).await {
                Ok(mut refreshed) => {
                    refreshed.restart_required = refreshed.restart_required
                        || (!before_commit.is_empty() && refreshed.current_commit != before_commit);
                    refreshed.checked_at = Some(unix_timestamp());
                    refreshed.state = "ready".to_string();
                    refreshed.message =
                        "Update applied. Restart Nucleus to load the new daemon.".to_string();
                    refreshed
                }
                Err(error) => error_status(&self.instance, &inspected, error.to_string()),
            },
            Err(error) => error_status(&self.instance, &inspected, error.to_string()),
        };

        self.replace_state(next).await
    }

    async fn replace_state(&self, next: UpdateStatus) -> UpdateRefresh {
        let mut state = self.state.lock().await;
        let changed = *state != next;
        *state = next.clone();

        UpdateRefresh {
            status: next,
            changed,
        }
    }

    async fn transition_state(&self, next_state: &str, message: &str) {
        let mut state = self.state.lock().await;
        state.state = next_state.to_string();
        state.message = message.to_string();
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
        return "Update applied. Restart Nucleus to load the new daemon.".to_string();
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
    use super::{InstanceRuntime, idle_status, short_commit};
    use std::path::PathBuf;

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
        };

        let status = idle_status(&instance);
        assert_eq!(status.state, "unsupported");
        assert!(!status.update_available);
    }
}
