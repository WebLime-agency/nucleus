use std::{
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use nucleus_core::{DEFAULT_DAEMON_ADDR, PRODUCT_NAME};
use nucleus_protocol::{HealthResponse, SettingsSummary};
use nucleus_storage::StateStore;
use reqwest::header::AUTHORIZATION;
use serde_json::Value;

const DEFAULT_LOCAL_SETUP_BIND: &str = "127.0.0.1:5201";
const DEFAULT_SERVER_SETUP_BIND: &str = "0.0.0.0:5201";
const DEFAULT_SERVICE_NAME: &str = "nucleus-daemon";

#[derive(Debug, Parser)]
#[command(name = "nucleus")]
#[command(about = "Nucleus operator CLI")]
struct CliArgs {
    #[arg(long, global = true, env = "NUCLEUS_STATE_DIR")]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Health(HealthArgs),
    Auth(AuthArgs),
    Setup(SetupArgs),
    InstallService(InstallServiceArgs),
}

#[derive(Debug, Args)]
struct HealthArgs {
    #[arg(long)]
    server_url: Option<String>,
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    LocalToken,
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[command(subcommand)]
    command: SetupCommand,
}

#[derive(Debug, Subcommand)]
enum SetupCommand {
    Local(SetupRuntimeArgs),
    Server(SetupRuntimeArgs),
    Client(SetupClientArgs),
}

#[derive(Debug, Clone, Args)]
struct SetupRuntimeArgs {
    #[arg(long)]
    bind: Option<String>,

    #[arg(long)]
    repo_root: Option<PathBuf>,

    #[arg(long)]
    web_dist_dir: Option<PathBuf>,

    #[arg(long)]
    instance_name: Option<String>,

    #[arg(long, default_value = DEFAULT_SERVICE_NAME)]
    service_name: String,

    #[arg(long)]
    install_service: bool,

    #[arg(long)]
    enable: bool,
}

#[derive(Debug, Args)]
struct SetupClientArgs {
    #[arg(long)]
    server_url: String,

    #[arg(long, env = "NUCLEUS_TOKEN")]
    token: String,
}

#[derive(Debug, Clone, Args)]
struct InstallServiceArgs {
    #[arg(long, default_value = DEFAULT_SERVER_SETUP_BIND)]
    bind: String,

    #[arg(long)]
    repo_root: Option<PathBuf>,

    #[arg(long)]
    web_dist_dir: Option<PathBuf>,

    #[arg(long)]
    instance_name: Option<String>,

    #[arg(long, default_value = DEFAULT_SERVICE_NAME)]
    service_name: String,

    #[arg(long)]
    enable: bool,
}

#[derive(Debug, Clone)]
struct InstallPlan {
    service_name: String,
    bind: String,
    repo_root: PathBuf,
    web_dist_dir: PathBuf,
    instance_name: String,
    state_dir: PathBuf,
    daemon_binary: PathBuf,
    home_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct ConnectionHints {
    local_url: String,
    hostname_url: Option<String>,
    tailscale_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = CliArgs::parse();

    match args.command {
        Command::Health(command) => run_health(command).await?,
        Command::Auth(command) => run_auth(command, args.state_dir)?,
        Command::Setup(command) => run_setup(command, args.state_dir).await?,
        Command::InstallService(command) => run_install_service(command, args.state_dir)?,
    }

    Ok(())
}

async fn run_health(command: HealthArgs) -> Result<()> {
    let server_url = command.server_url.unwrap_or_else(default_server_url);
    let response = reqwest::get(format!("{server_url}/health"))
        .await
        .with_context(|| format!("failed to reach {server_url}"))?
        .error_for_status()
        .with_context(|| format!("health endpoint returned an error for {server_url}"))?
        .json::<HealthResponse>()
        .await
        .context("failed to decode health response")?;

    println!(
        "{} {} {}",
        response.service, response.version, response.status
    );

    Ok(())
}

fn run_auth(command: AuthArgs, state_dir: Option<PathBuf>) -> Result<()> {
    match command.command {
        AuthCommand::LocalToken => {
            let store = open_store(state_dir.as_deref())?;
            println!("{}", store.read_local_auth_token()?);
        }
    }

    Ok(())
}

async fn run_setup(command: SetupArgs, state_dir: Option<PathBuf>) -> Result<()> {
    match command.command {
        SetupCommand::Local(args) => {
            run_setup_runtime("local", DEFAULT_LOCAL_SETUP_BIND, args, state_dir).await
        }
        SetupCommand::Server(args) => {
            run_setup_runtime("server", DEFAULT_SERVER_SETUP_BIND, args, state_dir).await
        }
        SetupCommand::Client(args) => run_setup_client(args).await,
    }
}

async fn run_setup_runtime(
    mode: &str,
    default_bind: &str,
    args: SetupRuntimeArgs,
    state_dir: Option<PathBuf>,
) -> Result<()> {
    let store = open_store(state_dir.as_deref())?;
    let token = store.read_local_auth_token()?;
    let bind = args.bind.unwrap_or_else(|| default_bind.to_string());
    let repo_root = resolve_repo_root(args.repo_root.as_deref())?;
    let web_dist_dir = resolve_web_dist_dir(args.web_dist_dir.as_deref(), &repo_root)?;
    let instance_name = args
        .instance_name
        .unwrap_or_else(|| PRODUCT_NAME.to_string());
    let hints = connection_hints(&bind);

    if args.install_service {
        let plan = build_install_plan(
            &args.service_name,
            &bind,
            state_dir.as_deref(),
            Some(&repo_root),
            Some(&web_dist_dir),
            Some(instance_name.clone()),
        )?;
        let unit_path = install_service_unit(&plan, args.enable)?;
        println!("Installed service unit: {}", unit_path.display());
    }

    println!("{PRODUCT_NAME} setup complete");
    println!("Mode: {mode}");
    println!(
        "State dir: {}",
        state_dir_path(state_dir.as_deref())?.display()
    );
    println!("Bind: {bind}");
    println!("Web build: {}", web_dist_dir.display());
    println!("Token: {token}");
    println!("Local URL: {}", hints.local_url);

    if let Some(url) = hints.hostname_url {
        println!("Host URL: {url}");
    }

    if let Some(url) = hints.tailscale_url {
        println!("Tailscale URL: {url}");
    }

    if !args.install_service {
        println!(
            "Next: run `nucleus install-service --bind {bind} --enable` when you want this instance managed in systemd."
        );
    }

    println!(
        "Auth: use `Authorization: Bearer <token>` or enter the token in the web UI when prompted."
    );

    Ok(())
}

async fn run_setup_client(args: SetupClientArgs) -> Result<()> {
    let client = reqwest::Client::new();
    let settings = client
        .get(format!(
            "{}/api/settings",
            sanitize_server_url(&args.server_url)
        ))
        .header(AUTHORIZATION, format!("Bearer {}", args.token.trim()))
        .send()
        .await
        .with_context(|| format!("failed to reach {}", args.server_url))?
        .error_for_status()
        .context("server rejected the provided token")?
        .json::<SettingsSummary>()
        .await
        .context("failed to decode settings payload")?;

    println!(
        "Connected to {} {} at {}",
        settings.product,
        settings.version,
        sanitize_server_url(&args.server_url)
    );

    Ok(())
}

fn run_install_service(command: InstallServiceArgs, state_dir: Option<PathBuf>) -> Result<()> {
    let plan = build_install_plan(
        &command.service_name,
        &command.bind,
        state_dir.as_deref(),
        command.repo_root.as_deref(),
        command.web_dist_dir.as_deref(),
        command.instance_name,
    )?;
    let unit_path = install_service_unit(&plan, command.enable)?;

    println!("Installed service unit: {}", unit_path.display());
    println!("Local URL: {}", connection_hints(&plan.bind).local_url);

    if let Some(url) = connection_hints(&plan.bind).tailscale_url {
        println!("Tailscale URL: {url}");
    }

    Ok(())
}

fn open_store(state_dir: Option<&Path>) -> Result<StateStore> {
    match state_dir {
        Some(path) => StateStore::initialize_at(path),
        None => StateStore::initialize(),
    }
}

fn state_dir_path(explicit_state_dir: Option<&Path>) -> Result<PathBuf> {
    match explicit_state_dir {
        Some(path) => Ok(path.to_path_buf()),
        None => match env::var("NUCLEUS_STATE_DIR") {
            Ok(path) => Ok(PathBuf::from(path)),
            Err(_) => default_state_dir(),
        },
    }
}

fn default_state_dir() -> Result<PathBuf> {
    let home_dir = home_dir()?;
    Ok(home_dir.join(".nucleus"))
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve the home directory"))
}

fn resolve_repo_root(explicit_repo_root: Option<&Path>) -> Result<PathBuf> {
    explicit_repo_root
        .map(Path::to_path_buf)
        .or_else(|| env::var("NUCLEUS_REPO_ROOT").ok().map(PathBuf::from))
        .or_else(|| env::current_dir().ok())
        .ok_or_else(|| anyhow!("failed to resolve the repository root"))
}

fn resolve_web_dist_dir(explicit_web_dist_dir: Option<&Path>, repo_root: &Path) -> Result<PathBuf> {
    let candidate = explicit_web_dist_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo_root.join("apps/web/build"));

    if candidate.join("index.html").is_file() {
        return Ok(candidate);
    }

    bail!(
        "web build not found at '{}'. Run `source ~/.nvm/nvm.sh && npm run build:web` from the repo root first.",
        candidate.display()
    );
}

fn build_install_plan(
    service_name: &str,
    bind: &str,
    explicit_state_dir: Option<&Path>,
    explicit_repo_root: Option<&Path>,
    explicit_web_dist_dir: Option<&Path>,
    instance_name: Option<String>,
) -> Result<InstallPlan> {
    let repo_root = resolve_repo_root(explicit_repo_root)?;
    let web_dist_dir = resolve_web_dist_dir(explicit_web_dist_dir, &repo_root)?;
    let state_dir = state_dir_path(explicit_state_dir)?;
    let daemon_binary = resolve_daemon_binary(&repo_root)?;
    let home_dir = home_dir()?;

    Ok(InstallPlan {
        service_name: service_name.to_string(),
        bind: bind.to_string(),
        repo_root,
        web_dist_dir,
        instance_name: instance_name.unwrap_or_else(|| PRODUCT_NAME.to_string()),
        state_dir,
        daemon_binary,
        home_dir,
    })
}

fn resolve_daemon_binary(repo_root: &Path) -> Result<PathBuf> {
    if let Ok(path) = env::var("NUCLEUS_DAEMON_BIN") {
        let binary = PathBuf::from(path);
        if binary.is_file() {
            return Ok(binary);
        }
    }

    let current_exe = env::current_exe().context("failed to resolve the current executable")?;
    if let Some(parent) = current_exe.parent() {
        let sibling = parent.join("nucleus-daemon");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }

    for candidate in [
        repo_root.join("target/debug/nucleus-daemon"),
        repo_root.join("target/release/nucleus-daemon"),
    ] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    bail!(
        "failed to locate `nucleus-daemon`. Build the Rust workspace first so the daemon binary exists."
    );
}

fn install_service_unit(plan: &InstallPlan, enable: bool) -> Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        bail!("install-service currently supports Linux systemd user services on this host");
    }

    let user_systemd_dir = plan.home_dir.join(".config/systemd/user");
    fs::create_dir_all(&user_systemd_dir).with_context(|| {
        format!(
            "failed to create systemd user directory '{}'",
            user_systemd_dir.display()
        )
    })?;

    let unit_path = user_systemd_dir.join(format!("{}.service", plan.service_name));
    let unit = format!(
        "[Unit]\nDescription={} daemon\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory={}\nExecStart={}\nRestart=on-failure\nRestartSec=5\nEnvironment=HOME={}\nEnvironment=NUCLEUS_INSTANCE_NAME={}\nEnvironment=NUCLEUS_STATE_DIR={}\nEnvironment=NUCLEUS_BIND={}\nEnvironment=NUCLEUS_REPO_ROOT={}\nEnvironment=NUCLEUS_WEB_DIST_DIR={}\nEnvironment=NUCLEUS_SYSTEMD_UNIT={}.service\n\n[Install]\nWantedBy=default.target\n",
        PRODUCT_NAME,
        plan.repo_root.display(),
        plan.daemon_binary.display(),
        plan.home_dir.display(),
        escape_env_value(&plan.instance_name),
        plan.state_dir.display(),
        plan.bind,
        plan.repo_root.display(),
        plan.web_dist_dir.display(),
        plan.service_name,
    );

    fs::write(&unit_path, unit)
        .with_context(|| format!("failed to write service unit '{}'", unit_path.display()))?;

    run_systemctl(&["--user", "daemon-reload"])?;
    if enable {
        run_systemctl(&[
            "--user",
            "enable",
            "--now",
            &format!("{}.service", plan.service_name),
        ])?;
    }

    Ok(unit_path)
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = StdCommand::new("systemctl")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute `systemctl {}`", args.join(" ")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    bail!("systemctl {} failed: {}", args.join(" "), stderr);
}

fn connection_hints(bind: &str) -> ConnectionHints {
    let port = bind_port(bind).unwrap_or(80);
    let local_url = format!("http://127.0.0.1:{port}");
    let hostname = local_hostname();
    let hostname_url = if bind_exposes_remote_access(bind) && !hostname.is_empty() {
        Some(format!("http://{hostname}:{port}"))
    } else {
        None
    };
    let tailscale_url = if bind_exposes_remote_access(bind) {
        tailscale_dns_name().map(|value| format!("http://{value}:{port}"))
    } else {
        None
    };

    ConnectionHints {
        local_url,
        hostname_url,
        tailscale_url,
    }
}

fn local_hostname() -> String {
    env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let output = StdCommand::new("hostname").arg("-s").output().ok()?;
            if !output.status.success() {
                return None;
            }

            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if value.is_empty() { None } else { Some(value) }
        })
        .unwrap_or_else(|| "localhost".to_string())
}

fn tailscale_dns_name() -> Option<String> {
    let output = StdCommand::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let payload: Value = serde_json::from_slice(&output.stdout).ok()?;
    let dns_name = payload
        .get("Self")
        .and_then(|value| value.get("DNSName"))
        .and_then(Value::as_str)
        .map(|value| value.trim_end_matches('.').to_string())?;

    if dns_name.is_empty() {
        return None;
    }

    Some(dns_name)
}

fn default_server_url() -> String {
    env::var("NUCLEUS_SERVER_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| sanitize_server_url(&value))
        .or_else(|| {
            env::var("NUCLEUS_BIND")
                .ok()
                .map(|value| format!("http://127.0.0.1:{}", bind_port(&value).unwrap_or(5201)))
        })
        .unwrap_or_else(|| format!("http://{DEFAULT_DAEMON_ADDR}"))
}

fn sanitize_server_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn bind_port(bind: &str) -> Option<u16> {
    bind.parse::<SocketAddr>()
        .ok()
        .map(|addr| addr.port())
        .or_else(|| {
            bind.rsplit_once(':')
                .and_then(|(_, port)| port.parse::<u16>().ok())
        })
}

fn bind_exposes_remote_access(bind: &str) -> bool {
    if bind.starts_with("127.0.0.1:") || bind.starts_with("localhost:") {
        return false;
    }

    match bind.parse::<SocketAddr>() {
        Ok(addr) => match addr.ip() {
            IpAddr::V4(ip) => !ip.is_loopback(),
            IpAddr::V6(ip) => !ip.is_loopback(),
        },
        Err(_) => true,
    }
}

fn escape_env_value(value: &str) -> String {
    value.replace('\n', " ").trim().to_string()
}
