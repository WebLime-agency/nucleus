use std::{
    collections::BTreeMap,
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Command as StdCommand,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use nucleus_core::{DEFAULT_DAEMON_ADDR, PRODUCT_NAME, PRODUCT_SLUG};
use nucleus_protocol::{HealthResponse, SettingsSummary};
use nucleus_release::{
    DEFAULT_RELEASE_CHANNEL, ReleasePackageInput, activate_release, current_platform_target,
    current_release_binary_path, current_release_web_dir, default_channel_manifest_url,
    default_install_root, download_artifact_to_path, load_manifest, package_release_artifact,
    select_release, stage_release_archive, validate_channel, verify_sha256,
};
use nucleus_storage::{StateStore, StoredUpdateState};
use reqwest::header::AUTHORIZATION;
use serde_json::Value;

const DEFAULT_LOCAL_SETUP_BIND: &str = "127.0.0.1:5201";
const DEFAULT_SERVER_SETUP_BIND: &str = "127.0.0.1:5201";
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
    Instances(InstancesArgs),
    Auth(AuthArgs),
    Setup(SetupArgs),
    InstallService(InstallServiceArgs),
    Release(ReleaseArgs),
}

#[derive(Debug, Args)]
struct HealthArgs {
    #[arg(long)]
    server_url: Option<String>,
}

#[derive(Debug, Args)]
struct InstancesArgs {}

#[derive(Debug, Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    LocalToken(AuthSelectorArgs),
    RotateToken(AuthSelectorArgs),
}

#[derive(Debug, Clone, Args, Default)]
struct AuthSelectorArgs {
    #[arg(long)]
    instance: Option<String>,

    #[arg(long)]
    url: Option<String>,
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

    #[arg(long)]
    allow_unsafe_bind: bool,
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

    #[arg(long)]
    allow_unsafe_bind: bool,
}

#[derive(Debug, Args)]
struct ReleaseArgs {
    #[command(subcommand)]
    command: ReleaseCommand,
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    Package(ReleasePackageArgs),
    Install(ReleaseInstallArgs),
}

#[derive(Debug, Clone, Args)]
struct ReleasePackageArgs {
    #[arg(long)]
    release_id: String,

    #[arg(long)]
    version: Option<String>,

    #[arg(long, default_value = DEFAULT_RELEASE_CHANNEL)]
    channel: String,

    #[arg(long)]
    repo_root: Option<PathBuf>,

    #[arg(long)]
    daemon_binary: Option<PathBuf>,

    #[arg(long)]
    cli_binary: Option<PathBuf>,

    #[arg(long)]
    web_dist_dir: Option<PathBuf>,

    #[arg(long)]
    output_dir: Option<PathBuf>,

    #[arg(long)]
    artifact_base_url: Option<String>,

    #[arg(long)]
    manifest_path: Option<PathBuf>,

    #[arg(long)]
    target: Option<String>,

    #[arg(long)]
    minimum_client_version: Option<String>,

    #[arg(long)]
    minimum_server_version: Option<String>,

    #[arg(long = "capability-flag")]
    capability_flags: Vec<String>,
}

#[derive(Debug, Clone, Args)]
struct ReleaseInstallArgs {
    #[arg(long, env = "NUCLEUS_RELEASE_MANIFEST_URL")]
    manifest_url: Option<String>,

    #[arg(long, default_value = DEFAULT_RELEASE_CHANNEL)]
    channel: String,

    #[arg(long, env = "NUCLEUS_INSTALL_ROOT")]
    install_root: Option<PathBuf>,

    #[arg(long, default_value = DEFAULT_SERVER_SETUP_BIND)]
    bind: String,

    #[arg(long)]
    instance_name: Option<String>,

    #[arg(long, default_value = DEFAULT_SERVICE_NAME)]
    service_name: String,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    install_service: bool,

    #[arg(long)]
    enable: bool,

    #[arg(long)]
    allow_unsafe_bind: bool,
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
struct ManagedReleaseInstallPlan {
    service_name: String,
    bind: String,
    install_root: PathBuf,
    instance_name: String,
    state_dir: PathBuf,
    daemon_binary: PathBuf,
    web_dist_dir: PathBuf,
    home_dir: PathBuf,
    manifest_url: String,
}

#[derive(Debug, Clone)]
struct ConnectionHints {
    local_url: String,
    hostname_url: Option<String>,
    tailscale_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalInstance {
    name: String,
    service_name: Option<String>,
    unit_path: Option<PathBuf>,
    state_dir: PathBuf,
    bind: Option<String>,
    url: Option<String>,
    install_kind: Option<String>,
    install_root: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = CliArgs::parse();

    match args.command {
        Command::Health(command) => run_health(command).await?,
        Command::Instances(command) => run_instances(command)?,
        Command::Auth(command) => run_auth(command, args.state_dir)?,
        Command::Setup(command) => run_setup(command, args.state_dir).await?,
        Command::InstallService(command) => run_install_service(command, args.state_dir)?,
        Command::Release(command) => run_release(command, args.state_dir).await?,
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

fn run_instances(_command: InstancesArgs) -> Result<()> {
    let instances = discover_local_instances()?;
    print_instances(&instances);
    Ok(())
}

fn run_auth(command: AuthArgs, state_dir: Option<PathBuf>) -> Result<()> {
    match command.command {
        AuthCommand::LocalToken(selector) => {
            let state_dir = resolve_auth_state_dir(state_dir, &selector)?;
            let store = open_store(Some(&state_dir))?;
            println!("{}", store.read_local_auth_token()?);
        }
        AuthCommand::RotateToken(selector) => {
            let state_dir = resolve_auth_state_dir(state_dir, &selector)?;
            let store = open_store(Some(&state_dir))?;
            let token = store.rotate_local_auth_token()?;
            eprintln!(
                "Rotated the local auth token for {}. Existing browser and client sessions using the old token must reconnect or re-authenticate.",
                state_dir.display()
            );
            println!("{token}");
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
    require_explicit_remote_bind(&bind, args.allow_unsafe_bind)?;
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
    require_explicit_remote_bind(&command.bind, command.allow_unsafe_bind)?;
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

async fn run_release(command: ReleaseArgs, state_dir: Option<PathBuf>) -> Result<()> {
    match command.command {
        ReleaseCommand::Package(args) => run_release_package(args),
        ReleaseCommand::Install(args) => run_release_install(args, state_dir).await,
    }
}

fn run_release_package(args: ReleasePackageArgs) -> Result<()> {
    let repo_root = resolve_repo_root(args.repo_root.as_deref())?;
    let release_id = trim_nonempty(&args.release_id, "release id")?;
    let channel = trim_nonempty(&args.channel, "release channel")?;
    validate_channel(channel)?;

    let daemon_binary = match args.daemon_binary.as_deref() {
        Some(path) => {
            if !path.is_file() {
                bail!("daemon binary '{}' was not found", path.display());
            }
            path.to_path_buf()
        }
        None => resolve_daemon_binary(&repo_root)?,
    };
    let cli_binary = match args.cli_binary.as_deref() {
        Some(path) => {
            if !path.is_file() {
                bail!("CLI binary '{}' was not found", path.display());
            }
            Some(path.to_path_buf())
        }
        None => resolve_cli_binary(&repo_root).ok(),
    };
    let web_dist_dir = resolve_web_dist_dir(args.web_dist_dir.as_deref(), &repo_root)?;
    let browser_sidecar_script = repo_root.join("scripts").join("browser-sidecar.mjs");
    let browser_sidecar_script = browser_sidecar_script
        .is_file()
        .then_some(browser_sidecar_script);
    let browser_node_module_dirs = ["playwright", "playwright-core"]
        .into_iter()
        .map(|module| repo_root.join("node_modules").join(module))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    let output_dir = args
        .output_dir
        .unwrap_or_else(|| repo_root.join("dist").join("releases"));
    let version = args
        .version
        .as_deref()
        .map(|value| trim_nonempty(value, "version"))
        .transpose()?
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_string();
    let packaged = package_release_artifact(ReleasePackageInput {
        release_id: release_id.to_string(),
        version: version.clone(),
        channel: channel.to_string(),
        daemon_binary,
        cli_binary,
        web_dist_dir,
        browser_sidecar_script,
        browser_node_module_dirs,
        output_dir,
        artifact_base_url: args
            .artifact_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        manifest_path: args.manifest_path,
        target: args
            .target
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        minimum_client_version: args
            .minimum_client_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        minimum_server_version: args
            .minimum_server_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        capability_flags: normalized_capability_flags(args.capability_flags),
    })?;

    println!("Managed release package complete");
    println!("Release ID: {}", packaged.release.release_id);
    println!("Version: {}", packaged.release.version);
    println!("Channel: {}", packaged.release.channel);
    println!("Target: {}", packaged.artifact.target);
    println!("Archive: {}", packaged.archive_path.display());
    println!("Manifest: {}", packaged.manifest_path.display());
    println!("Download URL: {}", packaged.artifact.download_url);
    println!("SHA256: {}", packaged.artifact.sha256);

    Ok(())
}

async fn run_release_install(
    args: ReleaseInstallArgs,
    explicit_state_dir: Option<PathBuf>,
) -> Result<()> {
    let channel = trim_nonempty(&args.channel, "release channel")?;
    validate_channel(channel)?;
    let manifest_url = args
        .manifest_url
        .as_deref()
        .map(|value| trim_nonempty(value, "manifest URL"))
        .transpose()?
        .map(ToOwned::to_owned)
        .unwrap_or(default_channel_manifest_url(channel)?);
    let install_root = resolve_install_root(args.install_root.as_deref())?;
    let state_dir = state_dir_path(explicit_state_dir.as_deref())?;
    let store = open_store(Some(&state_dir))?;
    let token = store.read_local_auth_token()?;
    let selected = {
        let manifest = load_manifest(&manifest_url).await?;
        select_release(&manifest, channel, &current_platform_target())?
    };

    let download_dir = store.artifacts_dir_path().join("managed-release-install");
    fs::create_dir_all(&download_dir)
        .with_context(|| format!("failed to create {}", download_dir.display()))?;
    let archive_path = download_dir.join(managed_release_archive_name(
        &selected.release.release_id,
        &selected.artifact.target,
    ));
    let (downloaded_size, _) =
        download_artifact_to_path(&selected.artifact.download_url, &archive_path).await?;
    let verified_size = verify_sha256(&archive_path, &selected.artifact.sha256)?;
    if verified_size != downloaded_size {
        bail!(
            "artifact size mismatch for {}: expected {} bytes, got {}",
            archive_path.display(),
            downloaded_size,
            verified_size
        );
    }
    if downloaded_size != selected.artifact.size_bytes {
        bail!(
            "artifact size mismatch for {}: manifest expected {} bytes, got {}",
            archive_path.display(),
            selected.artifact.size_bytes,
            downloaded_size
        );
    }
    let metadata =
        stage_release_archive(&archive_path, &install_root, &selected.release.release_id)?;
    let _ = activate_release(&install_root, &selected.release.release_id)?;
    let checked_at = unix_timestamp();
    store.write_update_state(&StoredUpdateState {
        tracked_channel: Some(channel.to_string()),
        tracked_ref: None,
        release_manifest_url: Some(manifest_url.clone()),
        pending_restart_release_id: None,
        update_available: false,
        last_successful_check_at: Some(checked_at),
        last_successful_target_version: Some(selected.release.version.clone()),
        last_successful_target_release_id: Some(selected.release.release_id.clone()),
        last_successful_target_commit: None,
        last_attempted_check_at: Some(checked_at),
        last_attempt_result: Some("success".to_string()),
        latest_error: None,
        latest_error_at: None,
        restart_required: false,
    })?;

    if args.install_service {
        require_explicit_remote_bind(&args.bind, args.allow_unsafe_bind)?;
        let plan = build_managed_release_install_plan(
            &args.service_name,
            &args.bind,
            &state_dir,
            &install_root,
            &manifest_url,
            args.instance_name,
        )?;
        let unit_path = install_managed_release_service_unit(&plan, args.enable)?;
        println!("Installed service unit: {}", unit_path.display());
        if !args.enable {
            println!(
                "Next: run `systemctl --user enable --now {}.service` when you want the managed release to start automatically.",
                plan.service_name
            );
        }
    }

    let hints = connection_hints(&args.bind);
    println!("Managed release install complete");
    println!("Release ID: {}", metadata.release_id);
    println!("Version: {}", metadata.version);
    println!("Channel: {}", metadata.channel);
    println!("Target: {}", metadata.target);
    println!("Install root: {}", install_root.display());
    println!(
        "Current binary: {}",
        current_release_binary_path(&install_root).display()
    );
    println!(
        "Web build: {}",
        current_release_web_dir(&install_root).display()
    );
    println!("State dir: {}", state_dir.display());
    println!("Manifest URL: {}", manifest_url);
    println!("Token: {token}");
    println!("Local URL: {}", hints.local_url);
    if let Some(url) = hints.hostname_url {
        println!("Host URL: {url}");
    }
    if let Some(url) = hints.tailscale_url {
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

fn resolve_auth_state_dir(
    explicit_state_dir: Option<PathBuf>,
    selector: &AuthSelectorArgs,
) -> Result<PathBuf> {
    if let Some(path) = explicit_state_dir {
        return Ok(path);
    }

    if selector.instance.is_some() && selector.url.is_some() {
        bail!("select either --instance or --url, not both");
    }

    let instances = discover_local_instances()?;
    if let Some(instance) = selector.instance.as_deref() {
        return select_instance_by_name(&instances, instance).map(|value| value.state_dir.clone());
    }
    if let Some(url) = selector.url.as_deref() {
        return select_instance_by_url(&instances, url).map(|value| value.state_dir.clone());
    }

    match instances.len() {
        0 => state_dir_path(None),
        1 => {
            let instance = &instances[0];
            eprintln!(
                "Using discovered Nucleus instance '{}' at {}.",
                instance.name,
                instance
                    .url
                    .as_deref()
                    .unwrap_or_else(|| instance.state_dir.to_str().unwrap_or("selected state dir"))
            );
            Ok(instance.state_dir.clone())
        }
        _ => {
            let mut message =
                "multiple Nucleus instances were found; select one explicitly".to_string();
            message.push_str(&format_instance_suggestions(&instances));
            bail!(message)
        }
    }
}

fn default_state_dir() -> Result<PathBuf> {
    let home_dir = home_dir()?;
    Ok(home_dir.join(".nucleus"))
}

fn resolve_install_root(explicit_install_root: Option<&Path>) -> Result<PathBuf> {
    match explicit_install_root {
        Some(path) => Ok(path.to_path_buf()),
        None => match env::var("NUCLEUS_INSTALL_ROOT") {
            Ok(path) => Ok(PathBuf::from(path)),
            Err(_) => default_install_root(),
        },
    }
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve the home directory"))
}

fn discover_local_instances() -> Result<Vec<LocalInstance>> {
    let systemd_dir = home_dir()?.join(".config/systemd/user");
    discover_local_instances_from_systemd_dir(&systemd_dir)
}

fn discover_local_instances_from_systemd_dir(systemd_dir: &Path) -> Result<Vec<LocalInstance>> {
    if !systemd_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut instances = Vec::new();
    for entry in fs::read_dir(systemd_dir).with_context(|| {
        format!(
            "failed to read user systemd directory '{}'",
            systemd_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("service") {
            continue;
        }

        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };

        if let Some(instance) = parse_local_instance_unit(Some(&path), &contents) {
            instances.push(instance);
        }
    }

    Ok(dedupe_instances(instances))
}

fn dedupe_instances(instances: Vec<LocalInstance>) -> Vec<LocalInstance> {
    let mut keyed = BTreeMap::new();
    for instance in instances {
        let service_key = instance.service_name.clone().unwrap_or_default();
        let key = format!("{}|{}", instance.state_dir.display(), service_key);
        keyed.entry(key).or_insert(instance);
    }
    keyed.into_values().collect()
}

fn parse_local_instance_unit(unit_path: Option<&Path>, contents: &str) -> Option<LocalInstance> {
    let env_values = parse_systemd_environment_values(contents);
    let state_dir = env_values.get("NUCLEUS_STATE_DIR")?;
    let service_name = unit_path
        .and_then(|path| path.file_name())
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned);
    let name = env_values
        .get("NUCLEUS_INSTANCE_NAME")
        .cloned()
        .or_else(|| {
            service_name
                .as_ref()
                .map(|value| value.trim_end_matches(".service").to_string())
        })
        .unwrap_or_else(|| PRODUCT_NAME.to_string());
    let bind = env_values.get("NUCLEUS_BIND").cloned();
    let url = bind
        .as_deref()
        .map(|value| connection_hints(value).local_url);
    let install_root = env_values.get("NUCLEUS_INSTALL_ROOT").map(PathBuf::from);

    Some(LocalInstance {
        name,
        service_name,
        unit_path: unit_path.map(Path::to_path_buf),
        state_dir: PathBuf::from(state_dir),
        bind,
        url,
        install_kind: env_values.get("NUCLEUS_INSTALL_KIND").cloned(),
        install_root,
    })
}

fn parse_systemd_environment_values(contents: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some(assignments) = trimmed.strip_prefix("Environment=") else {
            continue;
        };

        for assignment in assignments.split_whitespace() {
            let assignment = assignment.trim_matches('"').trim_matches('\'');
            let Some((key, value)) = assignment.split_once('=') else {
                continue;
            };
            values.insert(
                key.to_string(),
                value.trim_matches('"').trim_matches('\'').to_string(),
            );
        }
    }
    values
}

fn select_instance_by_name<'a>(
    instances: &'a [LocalInstance],
    instance_name: &str,
) -> Result<&'a LocalInstance> {
    let matches = instances
        .iter()
        .filter(|instance| {
            instance.name == instance_name
                || instance.service_name.as_deref() == Some(instance_name)
                || instance
                    .service_name
                    .as_deref()
                    .map(|value| value.trim_end_matches(".service") == instance_name)
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [instance] => Ok(instance),
        [] => {
            let mut message = format!("no Nucleus instance matched --instance '{instance_name}'");
            message.push_str(&format_instance_suggestions(instances));
            bail!(message)
        }
        _ => {
            let mut message =
                format!("multiple Nucleus instances matched --instance '{instance_name}'");
            message.push_str(&format_instance_suggestions(instances));
            bail!(message)
        }
    }
}

fn select_instance_by_url<'a>(
    instances: &'a [LocalInstance],
    instance_url: &str,
) -> Result<&'a LocalInstance> {
    let normalized = normalize_instance_url(instance_url);
    let matches = instances
        .iter()
        .filter(|instance| {
            instance.url.as_deref().map(normalize_instance_url).as_ref() == Some(&normalized)
                || instance
                    .bind
                    .as_deref()
                    .and_then(bind_port)
                    .map(|port| normalized.ends_with(&format!(":{port}")))
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [instance] => Ok(instance),
        [] => {
            let mut message = format!("no Nucleus instance matched --url '{instance_url}'");
            message.push_str(&format_instance_suggestions(instances));
            bail!(message)
        }
        _ => {
            let mut message = format!("multiple Nucleus instances matched --url '{instance_url}'");
            message.push_str(&format_instance_suggestions(instances));
            bail!(message)
        }
    }
}

fn normalize_instance_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn print_instances(instances: &[LocalInstance]) {
    if instances.is_empty() {
        println!("No installed local Nucleus instances found.");
        return;
    }

    print_table(
        &["NAME", "URL", "SERVICE", "STATE DIR"],
        &instances
            .iter()
            .map(|instance| {
                vec![
                    instance.name.clone(),
                    instance.url.clone().unwrap_or_else(|| "-".to_string()),
                    instance
                        .service_name
                        .clone()
                        .unwrap_or_else(|| "-".to_string()),
                    instance.state_dir.display().to_string(),
                ]
            })
            .collect::<Vec<_>>(),
    );
}

fn format_instance_suggestions(instances: &[LocalInstance]) -> String {
    if instances.is_empty() {
        return "\nNo installed local Nucleus instances were discovered. Use `nucleus --state-dir <state-dir> auth local-token`.".to_string();
    }

    let mut message = String::from("\n\nDiscovered instances:\n");
    for instance in instances {
        let service = instance
            .service_name
            .as_deref()
            .unwrap_or("unknown service");
        let url = instance.url.as_deref().unwrap_or("unknown URL");
        message.push_str(&format!(
            "- {} ({service}, {url}) state dir: {}\n",
            instance.name,
            instance.state_dir.display()
        ));
    }
    message.push_str("\nNext commands:\n");
    for instance in instances {
        message.push_str(&format!(
            "  nucleus auth local-token --instance {}\n",
            instance.name
        ));
    }
    message
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths = headers.iter().map(|value| value.len()).collect::<Vec<_>>();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(value.len());
            }
        }
    }

    for (index, header) in headers.iter().enumerate() {
        if index > 0 {
            print!("  ");
        }
        print!("{header:<width$}", width = widths[index]);
    }
    println!();

    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                print!("  ");
            }
            print!("{value:<width$}", width = widths[index]);
        }
        println!();
    }
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

fn resolve_cli_binary(repo_root: &Path) -> Result<PathBuf> {
    let current_exe = env::current_exe().context("failed to resolve the current executable")?;
    if current_exe.file_name().and_then(|value| value.to_str()) == Some("nucleus") {
        return Ok(current_exe);
    }

    for candidate in [
        repo_root.join("target/debug/nucleus"),
        repo_root.join("target/release/nucleus"),
    ] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    bail!("failed to locate `nucleus` CLI binary");
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
    let unit = render_dev_service_unit(plan);

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

fn build_managed_release_install_plan(
    service_name: &str,
    bind: &str,
    state_dir: &Path,
    install_root: &Path,
    manifest_url: &str,
    instance_name: Option<String>,
) -> Result<ManagedReleaseInstallPlan> {
    let home_dir = home_dir()?;
    Ok(ManagedReleaseInstallPlan {
        service_name: service_name.to_string(),
        bind: bind.to_string(),
        install_root: install_root.to_path_buf(),
        instance_name: instance_name.unwrap_or_else(|| PRODUCT_NAME.to_string()),
        state_dir: state_dir.to_path_buf(),
        daemon_binary: current_release_binary_path(install_root),
        web_dist_dir: current_release_web_dir(install_root),
        home_dir,
        manifest_url: manifest_url.to_string(),
    })
}

fn install_managed_release_service_unit(
    plan: &ManagedReleaseInstallPlan,
    enable: bool,
) -> Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        bail!(
            "managed release service install currently supports Linux systemd user services on this host"
        );
    }

    let user_systemd_dir = plan.home_dir.join(".config/systemd/user");
    fs::create_dir_all(&user_systemd_dir).with_context(|| {
        format!(
            "failed to create systemd user directory '{}'",
            user_systemd_dir.display()
        )
    })?;

    let unit_path = user_systemd_dir.join(format!("{}.service", plan.service_name));
    let unit = render_managed_release_service_unit(plan);

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

fn bind_mode_label(bind: &str) -> &'static str {
    match bind.parse::<SocketAddr>() {
        Ok(addr) if addr.ip().is_loopback() => "localhost only",
        Ok(addr) if addr.ip().is_unspecified() => "LAN/all interfaces",
        Ok(SocketAddr::V4(addr)) if is_tailscale_ipv4(*addr.ip()) => "Tailscale/private interface",
        Ok(SocketAddr::V4(addr)) if addr.ip().is_private() || addr.ip().is_link_local() => {
            "LAN/private interface"
        }
        Ok(SocketAddr::V6(addr)) if !addr.ip().is_loopback() => "LAN/private interface",
        Ok(_) => "custom/public",
        Err(_) if bind.starts_with("localhost:") => "localhost only",
        Err(_) => "custom/unknown",
    }
}

fn is_tailscale_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

fn require_explicit_remote_bind(bind: &str, allow_unsafe_bind: bool) -> Result<()> {
    if !bind_exposes_remote_access(bind) {
        return Ok(());
    }

    let message = format!(
        "bind '{bind}' exposes Nucleus beyond localhost ({mode}). Use --allow-unsafe-bind to confirm this explicit choice, and keep Vault plaintext operations on localhost or HTTPS.",
        mode = bind_mode_label(bind)
    );
    if allow_unsafe_bind {
        eprintln!("Warning: {message}");
        return Ok(());
    }
    bail!(message)
}

fn escape_env_value(value: &str) -> String {
    value.replace('\n', " ").trim().to_string()
}

fn render_dev_service_unit(plan: &InstallPlan) -> String {
    format!(
        "[Unit]\nDescription={} daemon\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory={}\nExecStart={}\nRestart=on-failure\nRestartSec=5\nEnvironment=HOME={}\nEnvironment=NUCLEUS_INSTANCE_NAME={}\nEnvironment=NUCLEUS_STATE_DIR={}\nEnvironment=NUCLEUS_BIND={}\nEnvironment=NUCLEUS_REPO_ROOT={}\nEnvironment=NUCLEUS_WEB_DIST_DIR={}\nEnvironment=NUCLEUS_INSTALL_KIND=dev_checkout\nEnvironment=NUCLEUS_SYSTEMD_UNIT={}.service\n\n[Install]\nWantedBy=default.target\n",
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
    )
}

fn render_managed_release_service_unit(plan: &ManagedReleaseInstallPlan) -> String {
    format!(
        "[Unit]\nDescription={} daemon\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory={}\nExecStart={}\nRestart=on-failure\nRestartSec=5\nEnvironment=HOME={}\nEnvironment=NUCLEUS_INSTANCE_NAME={}\nEnvironment=NUCLEUS_STATE_DIR={}\nEnvironment=NUCLEUS_BIND={}\nEnvironment=NUCLEUS_INSTALL_KIND=managed_release\nEnvironment=NUCLEUS_INSTALL_ROOT={}\nEnvironment=NUCLEUS_RELEASE_MANIFEST_URL={}\nEnvironment=NUCLEUS_WEB_DIST_DIR={}\nEnvironment=NUCLEUS_SYSTEMD_UNIT={}.service\n\n[Install]\nWantedBy=default.target\n",
        PRODUCT_NAME,
        plan.install_root.join("current").display(),
        plan.daemon_binary.display(),
        plan.home_dir.display(),
        escape_env_value(&plan.instance_name),
        plan.state_dir.display(),
        plan.bind,
        plan.install_root.display(),
        escape_env_value(&plan.manifest_url),
        plan.web_dist_dir.display(),
        plan.service_name,
    )
}

fn trim_nonempty<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} cannot be empty");
    }
    Ok(trimmed)
}

fn normalized_capability_flags(values: Vec<String>) -> Vec<String> {
    let mut flags = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    flags.sort();
    flags.dedup();
    flags
}

fn managed_release_archive_name(release_id: &str, target: &str) -> String {
    format!("{PRODUCT_SLUG}-{release_id}-{target}.tar.gz")
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
        AuthSelectorArgs, InstallPlan, LocalInstance, ManagedReleaseInstallPlan,
        discover_local_instances_from_systemd_dir, format_instance_suggestions,
        managed_release_archive_name, normalized_capability_flags, parse_local_instance_unit,
        render_dev_service_unit, render_managed_release_service_unit, require_explicit_remote_bind,
        resolve_auth_state_dir, select_instance_by_name, select_instance_by_url,
    };
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn renders_dev_checkout_service_unit_with_explicit_install_kind() {
        let unit = render_dev_service_unit(&InstallPlan {
            service_name: "nucleus-daemon".to_string(),
            bind: "127.0.0.1:5201".to_string(),
            repo_root: PathBuf::from("/tmp/nucleus"),
            web_dist_dir: PathBuf::from("/tmp/nucleus/apps/web/build"),
            instance_name: "Nucleus".to_string(),
            state_dir: PathBuf::from("/tmp/.nucleus"),
            daemon_binary: PathBuf::from("/tmp/nucleus/target/debug/nucleus-daemon"),
            home_dir: PathBuf::from("/tmp/home"),
        });

        assert!(unit.contains("Environment=NUCLEUS_INSTALL_KIND=dev_checkout"));
        assert!(unit.contains("Environment=NUCLEUS_REPO_ROOT=/tmp/nucleus"));
    }

    #[test]
    fn renders_managed_release_service_unit_with_release_env() {
        let unit = render_managed_release_service_unit(&ManagedReleaseInstallPlan {
            service_name: "nucleus-daemon".to_string(),
            bind: "0.0.0.0:5201".to_string(),
            install_root: PathBuf::from("/tmp/nucleus-managed"),
            instance_name: "Nucleus".to_string(),
            state_dir: PathBuf::from("/tmp/.nucleus"),
            daemon_binary: PathBuf::from("/tmp/nucleus-managed/current/bin/nucleus-daemon"),
            web_dist_dir: PathBuf::from("/tmp/nucleus-managed/current/web"),
            home_dir: PathBuf::from("/tmp/home"),
            manifest_url: "https://example.com/manifest-stable.json".to_string(),
        });

        assert!(unit.contains("Environment=NUCLEUS_INSTALL_KIND=managed_release"));
        assert!(unit.contains("Environment=NUCLEUS_INSTALL_ROOT=/tmp/nucleus-managed"));
        assert!(unit.contains(
            "Environment=NUCLEUS_RELEASE_MANIFEST_URL=https://example.com/manifest-stable.json"
        ));
    }

    #[test]
    fn parses_nucleus_service_unit_as_local_instance() {
        let unit = render_managed_release_service_unit(&ManagedReleaseInstallPlan {
            service_name: "nucleus-dev-projects".to_string(),
            bind: "127.0.0.1:5202".to_string(),
            install_root: PathBuf::from("/home/eba/tools/nucleus-dev-projects"),
            instance_name: "nucleus-dev-projects".to_string(),
            state_dir: PathBuf::from("/home/eba/.nucleus-dev-projects"),
            daemon_binary: PathBuf::from(
                "/home/eba/tools/nucleus-dev-projects/current/bin/nucleus-daemon",
            ),
            web_dist_dir: PathBuf::from("/home/eba/tools/nucleus-dev-projects/current/web"),
            home_dir: PathBuf::from("/home/eba"),
            manifest_url: "https://example.com/manifest-beta.json".to_string(),
        });

        let instance =
            parse_local_instance_unit(Some(&PathBuf::from("nucleus-dev-projects.service")), &unit)
                .expect("unit should parse");

        assert_eq!(instance.name, "nucleus-dev-projects");
        assert_eq!(
            instance.service_name.as_deref(),
            Some("nucleus-dev-projects.service")
        );
        assert_eq!(instance.url.as_deref(), Some("http://127.0.0.1:5202"));
        assert_eq!(
            instance.state_dir,
            PathBuf::from("/home/eba/.nucleus-dev-projects")
        );
        assert_eq!(instance.install_kind.as_deref(), Some("managed_release"));
        assert_eq!(
            instance.install_root,
            Some(PathBuf::from("/home/eba/tools/nucleus-dev-projects"))
        );
    }

    #[test]
    fn discovers_instances_from_systemd_user_dir() {
        let systemd_dir = test_dir("nucleus-cli-systemd");
        fs::create_dir_all(&systemd_dir).expect("systemd dir should be created");
        fs::write(
            systemd_dir.join("nucleus-a.service"),
            "Environment=NUCLEUS_INSTANCE_NAME=nucleus-a\nEnvironment=NUCLEUS_STATE_DIR=/tmp/a\nEnvironment=NUCLEUS_BIND=127.0.0.1:5202\n",
        )
        .expect("service unit should be written");
        fs::write(
            systemd_dir.join("not-nucleus.service"),
            "Environment=OTHER=value\n",
        )
        .expect("non-nucleus service should be written");

        let instances = discover_local_instances_from_systemd_dir(&systemd_dir)
            .expect("instances should be discovered");

        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].name, "nucleus-a");
        assert_eq!(
            instances[0].service_name.as_deref(),
            Some("nucleus-a.service")
        );
        assert_eq!(instances[0].url.as_deref(), Some("http://127.0.0.1:5202"));

        let _ = fs::remove_dir_all(&systemd_dir);
    }

    #[test]
    fn selects_instance_by_name_service_or_url() {
        let instances = vec![
            LocalInstance {
                name: "nucleus-dev-projects".to_string(),
                service_name: Some("nucleus-dev-projects.service".to_string()),
                unit_path: None,
                state_dir: PathBuf::from("/tmp/dev-projects"),
                bind: Some("127.0.0.1:5202".to_string()),
                url: Some("http://127.0.0.1:5202".to_string()),
                install_kind: Some("managed_release".to_string()),
                install_root: None,
            },
            LocalInstance {
                name: "nucleus-wbl-dga".to_string(),
                service_name: Some("nucleus-wbl-dga.service".to_string()),
                unit_path: None,
                state_dir: PathBuf::from("/tmp/wbl-dga"),
                bind: Some("127.0.0.1:5203".to_string()),
                url: Some("http://127.0.0.1:5203".to_string()),
                install_kind: Some("managed_release".to_string()),
                install_root: None,
            },
        ];

        assert_eq!(
            select_instance_by_name(&instances, "nucleus-dev-projects")
                .expect("name should match")
                .state_dir,
            PathBuf::from("/tmp/dev-projects")
        );
        assert_eq!(
            select_instance_by_name(&instances, "nucleus-wbl-dga.service")
                .expect("service should match")
                .state_dir,
            PathBuf::from("/tmp/wbl-dga")
        );
        assert_eq!(
            select_instance_by_url(&instances, "http://localhost:5203/")
                .expect("url port should match")
                .state_dir,
            PathBuf::from("/tmp/wbl-dga")
        );
    }

    #[test]
    fn explicit_state_dir_precedes_discovery_selectors() {
        let state_dir = PathBuf::from("/tmp/explicit-state");
        let selected = resolve_auth_state_dir(
            Some(state_dir.clone()),
            &AuthSelectorArgs {
                instance: Some("missing".to_string()),
                url: None,
            },
        )
        .expect("explicit state dir should win");

        assert_eq!(selected, state_dir);
    }

    #[test]
    fn instance_suggestions_do_not_include_tokens() {
        let suggestions = format_instance_suggestions(&[LocalInstance {
            name: "nucleus-dev-projects".to_string(),
            service_name: Some("nucleus-dev-projects.service".to_string()),
            unit_path: None,
            state_dir: PathBuf::from("/tmp/dev-projects"),
            bind: Some("127.0.0.1:5202".to_string()),
            url: Some("http://127.0.0.1:5202".to_string()),
            install_kind: Some("managed_release".to_string()),
            install_root: None,
        }]);

        assert!(suggestions.contains("nucleus auth local-token --instance nucleus-dev-projects"));
        assert!(!suggestions.contains("nuctk_"));
    }

    #[test]
    fn remote_bind_requires_explicit_confirmation() {
        assert!(require_explicit_remote_bind("127.0.0.1:5201", false).is_ok());
        assert!(require_explicit_remote_bind("localhost:5201", false).is_ok());
        let error = require_explicit_remote_bind("0.0.0.0:5201", false)
            .expect_err("all-interface bind should require confirmation")
            .to_string();
        assert!(error.contains("--allow-unsafe-bind"));
        assert!(error.contains("Vault plaintext operations"));
        assert!(require_explicit_remote_bind("0.0.0.0:5201", true).is_ok());
    }

    #[test]
    fn managed_install_default_bind_is_localhost_only() {
        assert_eq!(super::DEFAULT_SERVER_SETUP_BIND, "127.0.0.1:5201");
        assert!(require_explicit_remote_bind(super::DEFAULT_SERVER_SETUP_BIND, false).is_ok());
    }

    #[test]
    fn normalizes_capability_flags() {
        assert_eq!(
            normalized_capability_flags(vec![
                " embedded-web-build ".to_string(),
                "embedded-web-build".to_string(),
                "install-kind-contract".to_string(),
            ]),
            vec![
                "embedded-web-build".to_string(),
                "install-kind-contract".to_string(),
            ]
        );
    }

    #[test]
    fn builds_managed_release_archive_name() {
        assert_eq!(
            managed_release_archive_name("rel_123", "x86_64-linux"),
            "nucleus-rel_123-x86_64-linux.tar.gz"
        );
    }

    fn test_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("nucleus-{label}-{}-{suffix}", std::process::id()))
    }
}
