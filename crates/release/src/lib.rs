use std::{
    cmp::Reverse,
    fs::{self, File},
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use nucleus_core::{PRODUCT_NAME, PRODUCT_SLUG};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::{Archive, Builder, Header};
use tokio::io::AsyncWriteExt;
use url::Url;
use uuid::Uuid;

pub const INSTALL_KIND_DEV_CHECKOUT: &str = "dev_checkout";
pub const INSTALL_KIND_MANAGED_RELEASE: &str = "managed_release";
pub const DEFAULT_RELEASE_CHANNEL: &str = "stable";
pub const RELEASE_CHANNELS: [&str; 3] = ["stable", "beta", "nightly"];
pub const RELEASE_FORMAT_TAR_GZ: &str = "tar.gz";
pub const RELEASE_METADATA_FILE: &str = "release.json";
pub const DEFAULT_RELEASE_REPOSITORY: &str = "WebLime-agency/nucleus";
pub const RELEASE_CHANNEL_TAG_PREFIX: &str = "nucleus-channel-";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedReleaseManifest {
    pub product: String,
    pub channel: String,
    pub generated_at: i64,
    #[serde(default)]
    pub releases: Vec<ManagedReleaseVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedReleaseVersion {
    pub release_id: String,
    pub version: String,
    pub channel: String,
    pub published_at: i64,
    #[serde(default)]
    pub minimum_client_version: Option<String>,
    #[serde(default)]
    pub minimum_server_version: Option<String>,
    #[serde(default)]
    pub capability_flags: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<ManagedReleaseArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedReleaseArtifact {
    pub target: String,
    pub format: String,
    pub download_url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledReleaseMetadata {
    pub product: String,
    pub release_id: String,
    pub version: String,
    pub channel: String,
    pub target: String,
    pub built_at: i64,
    #[serde(default)]
    pub minimum_client_version: Option<String>,
    #[serde(default)]
    pub minimum_server_version: Option<String>,
    #[serde(default)]
    pub capability_flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedRelease {
    pub release: ManagedReleaseVersion,
    pub artifact: ManagedReleaseArtifact,
    pub archive_path: PathBuf,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedRelease {
    pub release: ManagedReleaseVersion,
    pub artifact: ManagedReleaseArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationResult {
    pub current_release_id: String,
    pub previous_release_id: Option<String>,
    pub current_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ReleasePackageInput {
    pub release_id: String,
    pub version: String,
    pub channel: String,
    pub daemon_binary: PathBuf,
    pub cli_binary: Option<PathBuf>,
    pub web_dist_dir: PathBuf,
    pub output_dir: PathBuf,
    pub artifact_base_url: Option<String>,
    pub manifest_path: Option<PathBuf>,
    pub target: Option<String>,
    pub minimum_client_version: Option<String>,
    pub minimum_server_version: Option<String>,
    pub capability_flags: Vec<String>,
}

pub fn default_install_root() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve the home directory"))?;
    Ok(home.join(".local/share").join(PRODUCT_SLUG).join("managed"))
}

pub fn current_platform_target() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

pub fn channel_release_tag(channel: &str) -> Result<String> {
    validate_channel(channel)?;
    Ok(format!("{RELEASE_CHANNEL_TAG_PREFIX}{channel}"))
}

pub fn default_channel_manifest_url(channel: &str) -> Result<String> {
    let tag = channel_release_tag(channel)?;
    Ok(format!(
        "https://github.com/{DEFAULT_RELEASE_REPOSITORY}/releases/download/{tag}/manifest-{channel}.json"
    ))
}

pub fn current_release_dir(install_root: &Path) -> PathBuf {
    install_root.join("current")
}

pub fn current_release_web_dir(install_root: &Path) -> PathBuf {
    current_release_dir(install_root).join("web")
}

pub fn current_release_binary_path(install_root: &Path) -> PathBuf {
    current_release_dir(install_root)
        .join("bin")
        .join("nucleus-daemon")
}

pub fn current_release_id(install_root: &Path) -> Result<Option<String>> {
    let link = current_release_dir(install_root);
    if !link.exists() {
        return Ok(None);
    }

    let target = fs::read_link(&link)
        .with_context(|| format!("failed to read current release link {}", link.display()))?;
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_string());
    Ok(name)
}

pub fn read_installed_release_metadata(
    install_root: &Path,
) -> Result<Option<InstalledReleaseMetadata>> {
    let path = current_release_dir(install_root).join(RELEASE_METADATA_FILE);
    if !path.is_file() {
        return Ok(None);
    }

    let payload = fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read installed release metadata {}",
            path.display()
        )
    })?;
    let metadata = serde_json::from_str(&payload)
        .with_context(|| format!("failed to decode release metadata {}", path.display()))?;
    Ok(Some(metadata))
}

pub async fn load_manifest(source: &str) -> Result<ManagedReleaseManifest> {
    let bytes = load_bytes(source).await?;
    serde_json::from_slice(&bytes).context("failed to decode managed release manifest")
}

pub async fn load_bytes(source: &str) -> Result<Vec<u8>> {
    match parse_source(source)? {
        SourceLocation::Http(url) => {
            let response = reqwest::get(url.clone())
                .await
                .with_context(|| format!("failed to reach {url}"))?
                .error_for_status()
                .with_context(|| format!("request failed for {url}"))?;
            let bytes = response
                .bytes()
                .await
                .with_context(|| format!("failed to read response bytes from {url}"))?;
            Ok(bytes.to_vec())
        }
        SourceLocation::File(path) => tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read {}", path.display())),
    }
}

pub fn select_release(
    manifest: &ManagedReleaseManifest,
    channel: &str,
    target: &str,
) -> Result<SelectedRelease> {
    validate_channel(channel)?;

    if manifest.channel != channel {
        bail!(
            "managed release manifest is for channel '{}', but this install tracks '{}'",
            manifest.channel,
            channel
        );
    }

    let mut matches = manifest
        .releases
        .iter()
        .filter(|release| release.channel == channel)
        .filter_map(|release| {
            release
                .artifacts
                .iter()
                .find(|artifact| {
                    artifact.target == target && artifact.format == RELEASE_FORMAT_TAR_GZ
                })
                .map(|artifact| SelectedRelease {
                    release: release.clone(),
                    artifact: artifact.clone(),
                })
        })
        .collect::<Vec<_>>();

    matches.sort_by_key(|selected| Reverse(selected.release.published_at));
    matches
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no managed release artifact was published for channel '{channel}' and target '{target}'"))
}

pub async fn download_artifact_to_path(source: &str, destination: &Path) -> Result<(u64, String)> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let bytes = load_bytes(source).await?;
    let sha = sha256_hex(&bytes);
    let mut file = tokio::fs::File::create(destination)
        .await
        .with_context(|| format!("failed to create {}", destination.display()))?;
    file.write_all(&bytes)
        .await
        .with_context(|| format!("failed to write {}", destination.display()))?;
    file.flush()
        .await
        .with_context(|| format!("failed to flush {}", destination.display()))?;
    Ok((bytes.len() as u64, sha))
}

pub fn verify_sha256(path: &Path, expected_sha256: &str) -> Result<u64> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }

    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256 {
        bail!(
            "artifact checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual
        );
    }

    Ok(size)
}

pub fn stage_release_archive(
    archive_path: &Path,
    install_root: &Path,
    expected_release_id: &str,
) -> Result<InstalledReleaseMetadata> {
    let releases_dir = install_root.join("releases");
    fs::create_dir_all(&releases_dir)
        .with_context(|| format!("failed to create {}", releases_dir.display()))?;

    let final_dir = releases_dir.join(expected_release_id);
    if final_dir.exists() {
        let metadata_path = final_dir.join(RELEASE_METADATA_FILE);
        if metadata_path.is_file() {
            let payload = fs::read_to_string(&metadata_path).with_context(|| {
                format!(
                    "failed to read release metadata {}",
                    metadata_path.display()
                )
            })?;
            let metadata: InstalledReleaseMetadata =
                serde_json::from_str(&payload).with_context(|| {
                    format!(
                        "failed to decode release metadata {}",
                        metadata_path.display()
                    )
                })?;
            if metadata.release_id == expected_release_id {
                return Ok(metadata);
            }
        }

        bail!(
            "release directory {} already exists but does not match the expected release metadata",
            final_dir.display()
        );
    }

    let staging_dir =
        releases_dir.join(format!(".staging-{expected_release_id}-{}", Uuid::new_v4()));
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    fs::create_dir_all(&staging_dir)
        .with_context(|| format!("failed to create {}", staging_dir.display()))?;

    let archive_file = File::open(archive_path)
        .with_context(|| format!("failed to open {}", archive_path.display()))?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(&staging_dir)
        .with_context(|| format!("failed to extract {}", archive_path.display()))?;

    let metadata = validate_extracted_release(&staging_dir, expected_release_id)?;
    fs::rename(&staging_dir, &final_dir).with_context(|| {
        format!(
            "failed to promote staged release {} to {}",
            staging_dir.display(),
            final_dir.display()
        )
    })?;

    Ok(metadata)
}

pub fn activate_release(install_root: &Path, release_id: &str) -> Result<ActivationResult> {
    #[cfg(not(unix))]
    {
        let _ = (install_root, release_id);
        bail!("managed release activation currently requires unix-style symlinks");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let releases_dir = install_root.join("releases");
        let release_dir = releases_dir.join(release_id);
        if !release_dir.is_dir() {
            bail!(
                "release directory '{}' was not found",
                release_dir.display()
            );
        }

        fs::create_dir_all(install_root)
            .with_context(|| format!("failed to create {}", install_root.display()))?;

        let current_link = current_release_dir(install_root);
        let previous_link = install_root.join("previous");
        let previous_release_id = current_release_id(install_root)?;

        let current_tmp = install_root.join(format!(".current-{release_id}.tmp"));
        if current_tmp.exists() {
            let _ = fs::remove_file(&current_tmp);
        }
        symlink(&release_dir, &current_tmp).with_context(|| {
            format!(
                "failed to create current release link {} -> {}",
                current_tmp.display(),
                release_dir.display()
            )
        })?;
        fs::rename(&current_tmp, &current_link).with_context(|| {
            format!(
                "failed to activate release {} via {}",
                release_dir.display(),
                current_link.display()
            )
        })?;

        if let Some(previous_release_id) = previous_release_id.clone() {
            let previous_target = releases_dir.join(&previous_release_id);
            let previous_tmp = install_root.join(format!(".previous-{previous_release_id}.tmp"));
            if previous_tmp.exists() {
                let _ = fs::remove_file(&previous_tmp);
            }
            symlink(&previous_target, &previous_tmp).with_context(|| {
                format!(
                    "failed to create previous release link {} -> {}",
                    previous_tmp.display(),
                    previous_target.display()
                )
            })?;
            fs::rename(&previous_tmp, &previous_link).with_context(|| {
                format!(
                    "failed to update previous release link {}",
                    previous_link.display()
                )
            })?;
        }

        Ok(ActivationResult {
            current_release_id: release_id.to_string(),
            previous_release_id,
            current_path: current_link,
        })
    }
}

pub fn package_release_artifact(input: ReleasePackageInput) -> Result<PackagedRelease> {
    validate_channel(&input.channel)?;

    if !input.daemon_binary.is_file() {
        bail!(
            "daemon binary '{}' was not found",
            input.daemon_binary.display()
        );
    }

    if let Some(cli_binary) = &input.cli_binary {
        if !cli_binary.is_file() {
            bail!("CLI binary '{}' was not found", cli_binary.display());
        }
    }

    if !input.web_dist_dir.join("index.html").is_file() {
        bail!(
            "web build '{}' is missing index.html",
            input.web_dist_dir.display()
        );
    }

    fs::create_dir_all(&input.output_dir)
        .with_context(|| format!("failed to create {}", input.output_dir.display()))?;

    let target = input.target.unwrap_or_else(current_platform_target);
    let published_at = unix_timestamp();
    let archive_name = format!("{}-{}-{}.tar.gz", PRODUCT_SLUG, input.release_id, target);
    let archive_path = input.output_dir.join(&archive_name);
    let manifest_path = input.manifest_path.unwrap_or_else(|| {
        input
            .output_dir
            .join(format!("manifest-{}.json", input.channel))
    });

    let file = File::create(&archive_path)
        .with_context(|| format!("failed to create {}", archive_path.display()))?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);

    builder
        .append_path_with_name(&input.daemon_binary, "bin/nucleus-daemon")
        .with_context(|| {
            format!(
                "failed to append daemon binary {}",
                input.daemon_binary.display()
            )
        })?;
    if let Some(cli_binary) = &input.cli_binary {
        builder
            .append_path_with_name(cli_binary, "bin/nucleus")
            .with_context(|| format!("failed to append CLI binary {}", cli_binary.display()))?;
    }
    append_directory_recursive(&mut builder, &input.web_dist_dir, &PathBuf::from("web"))?;
    let minimum_client_version = input
        .minimum_client_version
        .clone()
        .unwrap_or_else(|| input.version.clone());
    let minimum_server_version = input
        .minimum_server_version
        .clone()
        .unwrap_or_else(|| input.version.clone());

    let release_metadata = InstalledReleaseMetadata {
        product: PRODUCT_NAME.to_string(),
        release_id: input.release_id.clone(),
        version: input.version.clone(),
        channel: input.channel.clone(),
        target: target.clone(),
        built_at: published_at,
        minimum_client_version: Some(minimum_client_version.clone()),
        minimum_server_version: Some(minimum_server_version.clone()),
        capability_flags: input.capability_flags.clone(),
    };
    append_json_file(
        &mut builder,
        Path::new(RELEASE_METADATA_FILE),
        &release_metadata,
    )?;
    let encoder = builder
        .into_inner()
        .context("failed to finalize release archive stream")?;
    let file = encoder
        .finish()
        .context("failed to finish managed release gzip stream")?;
    file.sync_all()
        .with_context(|| format!("failed to flush {}", archive_path.display()))?;
    drop(file);

    let size_bytes = fs::metadata(&archive_path)
        .with_context(|| format!("failed to stat {}", archive_path.display()))?
        .len();
    let sha256 = sha256_file(&archive_path)?;
    let artifact_url = input
        .artifact_base_url
        .map(|base| format!("{}/{}", base.trim_end_matches('/'), archive_name))
        .unwrap_or_else(|| format!("file://{}", archive_path.display()));
    let artifact = ManagedReleaseArtifact {
        target: target.clone(),
        format: RELEASE_FORMAT_TAR_GZ.to_string(),
        download_url: artifact_url,
        sha256,
        size_bytes,
    };
    let release = ManagedReleaseVersion {
        release_id: input.release_id.clone(),
        version: input.version.clone(),
        channel: input.channel.clone(),
        published_at,
        minimum_client_version: Some(minimum_client_version),
        minimum_server_version: Some(minimum_server_version),
        capability_flags: input.capability_flags,
        artifacts: vec![artifact.clone()],
    };

    let mut manifest = if manifest_path.is_file() {
        let payload = fs::read_to_string(&manifest_path).with_context(|| {
            format!(
                "failed to read managed release manifest {}",
                manifest_path.display()
            )
        })?;
        let mut current: ManagedReleaseManifest =
            serde_json::from_str(&payload).with_context(|| {
                format!(
                    "failed to decode managed release manifest {}",
                    manifest_path.display()
                )
            })?;
        if current.channel != input.channel {
            bail!(
                "managed release manifest {} is for channel '{}', not '{}'",
                manifest_path.display(),
                current.channel,
                input.channel
            );
        }
        current
            .releases
            .retain(|item| item.release_id != release.release_id);
        current
    } else {
        ManagedReleaseManifest {
            product: PRODUCT_NAME.to_string(),
            channel: input.channel.clone(),
            generated_at: published_at,
            releases: Vec::new(),
        }
    };
    manifest.generated_at = published_at;
    manifest.releases.push(release.clone());
    manifest
        .releases
        .sort_by_key(|item| Reverse(item.published_at));

    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).context("failed to serialize manifest")?,
    )
    .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    Ok(PackagedRelease {
        release,
        artifact,
        archive_path,
        manifest_path,
    })
}

pub fn validate_channel(channel: &str) -> Result<()> {
    if RELEASE_CHANNELS.contains(&channel) {
        Ok(())
    } else {
        bail!(
            "unsupported release channel '{}'. Expected one of: {}",
            channel,
            RELEASE_CHANNELS.join(", ")
        )
    }
}

fn parse_source(source: &str) -> Result<SourceLocation> {
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(SourceLocation::Http(source.to_string()));
    }

    if source.starts_with("file://") {
        let url = Url::parse(source).with_context(|| format!("invalid URL '{source}'"))?;
        let path = url
            .to_file_path()
            .map_err(|_| anyhow!("failed to decode file URL '{source}'"))?;
        return Ok(SourceLocation::File(path));
    }

    Ok(SourceLocation::File(PathBuf::from(source)))
}

fn append_directory_recursive(
    builder: &mut Builder<GzEncoder<File>>,
    source_dir: &Path,
    archive_root: &Path,
) -> Result<()> {
    for entry in fs::read_dir(source_dir)
        .with_context(|| format!("failed to read {}", source_dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to inspect {}", source_dir.display()))?;
        let path = entry.path();
        let archive_path = archive_root.join(entry.file_name());

        if path.is_dir() {
            builder
                .append_dir(&archive_path, &path)
                .with_context(|| format!("failed to append directory {}", path.display()))?;
            append_directory_recursive(builder, &path, &archive_path)?;
        } else if path.is_file() {
            builder
                .append_path_with_name(&path, &archive_path)
                .with_context(|| format!("failed to append file {}", path.display()))?;
        }
    }

    Ok(())
}

fn append_json_file<T: Serialize>(
    builder: &mut Builder<GzEncoder<File>>,
    archive_path: &Path,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("failed to serialize release metadata")?;
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, archive_path, Cursor::new(bytes))
        .with_context(|| format!("failed to append {}", archive_path.display()))?;
    Ok(())
}

fn validate_extracted_release(
    release_dir: &Path,
    expected_release_id: &str,
) -> Result<InstalledReleaseMetadata> {
    let metadata_path = release_dir.join(RELEASE_METADATA_FILE);
    let daemon_path = release_dir.join("bin").join("nucleus-daemon");
    let web_index = release_dir.join("web").join("index.html");

    if !metadata_path.is_file() {
        bail!(
            "release archive did not contain {}",
            metadata_path.display()
        );
    }
    if !daemon_path.is_file() {
        bail!("release archive did not contain {}", daemon_path.display());
    }
    if !web_index.is_file() {
        bail!("release archive did not contain {}", web_index.display());
    }

    let payload = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let metadata: InstalledReleaseMetadata = serde_json::from_str(&payload)
        .with_context(|| format!("failed to decode {}", metadata_path.display()))?;
    if metadata.release_id != expected_release_id {
        bail!(
            "release metadata mismatch: expected '{}', got '{}'",
            expected_release_id,
            metadata.release_id
        );
    }

    Ok(metadata)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

enum SourceLocation {
    Http(String),
    File(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_and_reads_manifest_with_file_urls() {
        let root = std::env::temp_dir().join(format!("nucleus-release-test-{}", Uuid::new_v4()));
        let bin_dir = root.join("bin");
        let web_dir = root.join("web");
        let output_dir = root.join("dist");
        fs::create_dir_all(&bin_dir).expect("bin dir should exist");
        fs::create_dir_all(&web_dir).expect("web dir should exist");
        fs::write(bin_dir.join("nucleus-daemon"), "daemon").expect("daemon file should exist");
        fs::write(bin_dir.join("nucleus"), "cli").expect("CLI file should exist");
        fs::write(web_dir.join("index.html"), "<html></html>").expect("web build should exist");

        let packaged = package_release_artifact(ReleasePackageInput {
            release_id: "rel_test".to_string(),
            version: "0.1.0".to_string(),
            channel: DEFAULT_RELEASE_CHANNEL.to_string(),
            daemon_binary: bin_dir.join("nucleus-daemon"),
            cli_binary: Some(bin_dir.join("nucleus")),
            web_dist_dir: web_dir,
            output_dir: output_dir.clone(),
            artifact_base_url: None,
            manifest_path: None,
            target: Some("x86_64-linux".to_string()),
            minimum_client_version: None,
            minimum_server_version: None,
            capability_flags: vec!["embedded-web-build".to_string()],
        })
        .expect("package should succeed");

        assert!(packaged.archive_path.is_file());
        assert!(packaged.manifest_path.is_file());

        let payload = fs::read_to_string(packaged.manifest_path).expect("manifest should read");
        let manifest: ManagedReleaseManifest =
            serde_json::from_str(&payload).expect("manifest should decode");
        let selected = select_release(&manifest, DEFAULT_RELEASE_CHANNEL, "x86_64-linux")
            .expect("release should select");
        assert_eq!(selected.release.release_id, "rel_test");
        assert_eq!(
            selected.release.minimum_client_version.as_deref(),
            Some("0.1.0")
        );
        assert_eq!(
            selected.release.minimum_server_version.as_deref(),
            Some("0.1.0")
        );
        assert!(selected.artifact.download_url.starts_with("file://"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn default_channel_manifest_url_uses_public_channel_release_asset() {
        let url = default_channel_manifest_url("beta").expect("channel should validate");

        assert_eq!(
            url,
            "https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-beta/manifest-beta.json"
        );
    }

    #[test]
    fn select_release_rejects_manifest_channel_mismatches() {
        let manifest = ManagedReleaseManifest {
            product: PRODUCT_NAME.to_string(),
            channel: "stable".to_string(),
            generated_at: 1,
            releases: Vec::new(),
        };

        let error = select_release(&manifest, "beta", "x86_64-linux")
            .expect_err("mismatched channel should fail");

        assert!(
            error
                .to_string()
                .contains("manifest is for channel 'stable'")
        );
    }

    #[test]
    fn select_release_rejects_missing_target_artifacts() {
        let manifest = ManagedReleaseManifest {
            product: PRODUCT_NAME.to_string(),
            channel: DEFAULT_RELEASE_CHANNEL.to_string(),
            generated_at: 1,
            releases: vec![ManagedReleaseVersion {
                release_id: "rel_missing_target".to_string(),
                version: "0.1.0".to_string(),
                channel: DEFAULT_RELEASE_CHANNEL.to_string(),
                published_at: 1,
                minimum_client_version: Some("0.1.0".to_string()),
                minimum_server_version: Some("0.1.0".to_string()),
                capability_flags: Vec::new(),
                artifacts: vec![ManagedReleaseArtifact {
                    target: "aarch64-linux".to_string(),
                    format: RELEASE_FORMAT_TAR_GZ.to_string(),
                    download_url: "file:///tmp/does-not-matter.tar.gz".to_string(),
                    sha256: "abc".to_string(),
                    size_bytes: 1,
                }],
            }],
        };

        let error = select_release(&manifest, DEFAULT_RELEASE_CHANNEL, "x86_64-linux")
            .expect_err("missing target should fail");

        assert!(
            error
                .to_string()
                .contains("no managed release artifact was published")
        );
    }

    #[test]
    fn package_rejects_existing_manifest_from_other_channel() {
        let root = std::env::temp_dir().join(format!("nucleus-release-channel-{}", Uuid::new_v4()));
        let bin_dir = root.join("bin");
        let web_dir = root.join("web");
        let output_dir = root.join("dist");
        let manifest_path = output_dir.join("manifest-stable.json");
        fs::create_dir_all(&bin_dir).expect("bin dir should exist");
        fs::create_dir_all(&web_dir).expect("web dir should exist");
        fs::create_dir_all(&output_dir).expect("output dir should exist");
        fs::write(bin_dir.join("nucleus-daemon"), "daemon").expect("daemon file should exist");
        fs::write(web_dir.join("index.html"), "<html></html>").expect("web build should exist");
        fs::write(
            &manifest_path,
            serde_json::to_string(&ManagedReleaseManifest {
                product: PRODUCT_NAME.to_string(),
                channel: "beta".to_string(),
                generated_at: 1,
                releases: Vec::new(),
            })
            .expect("manifest should serialize"),
        )
        .expect("manifest should write");

        let error = package_release_artifact(ReleasePackageInput {
            release_id: "rel_wrong_channel".to_string(),
            version: "0.1.0".to_string(),
            channel: DEFAULT_RELEASE_CHANNEL.to_string(),
            daemon_binary: bin_dir.join("nucleus-daemon"),
            cli_binary: None,
            web_dist_dir: web_dir,
            output_dir,
            artifact_base_url: None,
            manifest_path: Some(manifest_path),
            target: Some("x86_64-linux".to_string()),
            minimum_client_version: None,
            minimum_server_version: None,
            capability_flags: Vec::new(),
        })
        .expect_err("wrong channel manifest should fail");

        assert!(error.to_string().contains("is for channel 'beta'"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stages_and_activates_release_archive() {
        let root = std::env::temp_dir().join(format!("nucleus-release-stage-{}", Uuid::new_v4()));
        let bin_dir = root.join("bin");
        let web_dir = root.join("web");
        let output_dir = root.join("dist");
        let install_root = root.join("install");
        fs::create_dir_all(&bin_dir).expect("bin dir should exist");
        fs::create_dir_all(&web_dir).expect("web dir should exist");
        fs::write(bin_dir.join("nucleus-daemon"), "daemon").expect("daemon file should exist");
        fs::write(bin_dir.join("nucleus"), "cli").expect("CLI file should exist");
        fs::write(web_dir.join("index.html"), "<html></html>").expect("web build should exist");

        let packaged = package_release_artifact(ReleasePackageInput {
            release_id: "rel_stage".to_string(),
            version: "0.1.0".to_string(),
            channel: DEFAULT_RELEASE_CHANNEL.to_string(),
            daemon_binary: bin_dir.join("nucleus-daemon"),
            cli_binary: Some(bin_dir.join("nucleus")),
            web_dist_dir: web_dir,
            output_dir,
            artifact_base_url: None,
            manifest_path: None,
            target: Some("x86_64-linux".to_string()),
            minimum_client_version: None,
            minimum_server_version: None,
            capability_flags: Vec::new(),
        })
        .expect("package should succeed");

        let metadata = stage_release_archive(&packaged.archive_path, &install_root, "rel_stage")
            .expect("release should stage");
        assert_eq!(metadata.release_id, "rel_stage");

        let activation =
            activate_release(&install_root, "rel_stage").expect("release should activate");
        assert_eq!(activation.current_release_id, "rel_stage");
        assert!(current_release_binary_path(&install_root).exists());
        assert!(
            current_release_dir(&install_root)
                .join("bin/nucleus")
                .is_file()
        );
        assert!(
            current_release_web_dir(&install_root)
                .join("index.html")
                .is_file()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activation_tracks_previous_release_after_second_activation() {
        let root =
            std::env::temp_dir().join(format!("nucleus-release-previous-{}", Uuid::new_v4()));
        let output_dir = root.join("dist");
        let install_root = root.join("install");

        for release_id in ["rel_one", "rel_two"] {
            let bin_dir = root.join(format!("{release_id}-bin"));
            let web_dir = root.join(format!("{release_id}-web"));
            fs::create_dir_all(&bin_dir).expect("bin dir should exist");
            fs::create_dir_all(&web_dir).expect("web dir should exist");
            fs::write(
                bin_dir.join("nucleus-daemon"),
                format!("daemon-{release_id}"),
            )
            .expect("daemon file should exist");
            fs::write(
                web_dir.join("index.html"),
                format!("<html>{release_id}</html>"),
            )
            .expect("web build should exist");

            let packaged = package_release_artifact(ReleasePackageInput {
                release_id: release_id.to_string(),
                version: "0.1.0".to_string(),
                channel: DEFAULT_RELEASE_CHANNEL.to_string(),
                daemon_binary: bin_dir.join("nucleus-daemon"),
                cli_binary: None,
                web_dist_dir: web_dir,
                output_dir: output_dir.clone(),
                artifact_base_url: None,
                manifest_path: None,
                target: Some("x86_64-linux".to_string()),
                minimum_client_version: None,
                minimum_server_version: None,
                capability_flags: Vec::new(),
            })
            .expect("package should succeed");

            stage_release_archive(&packaged.archive_path, &install_root, release_id)
                .expect("release should stage");
        }

        let first =
            activate_release(&install_root, "rel_one").expect("first release should activate");
        assert_eq!(first.previous_release_id, None);

        let second =
            activate_release(&install_root, "rel_two").expect("second release should activate");
        assert_eq!(second.previous_release_id.as_deref(), Some("rel_one"));

        let previous_target =
            fs::read_link(install_root.join("previous")).expect("previous link should exist");
        assert_eq!(
            previous_target.file_name().and_then(|value| value.to_str()),
            Some("rel_one")
        );

        let _ = fs::remove_dir_all(root);
    }
}
