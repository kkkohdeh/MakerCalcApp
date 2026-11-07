use crate::i18n::t;
// Query public GitHub releases; the browser downloads and Inno Setup installs.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const DEFAULT_UPDATE_URL: &str =
    "https://api.github.com/repos/kkkohdeh/MakerCalcApp/releases?per_page=30";
const RELEASES_URL: &str = "https://github.com/kkkohdeh/MakerCalcApp/releases/tag/";
const MAX_RELEASE_BYTES: usize = 1024 * 1024;
const INSTALLER_ASSET: &str = "MakerCalc-Setup.exe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    Available(RemoteRelease),
    // Existing visual/restart states are not produced by the manual flow.
    #[allow(dead_code)]
    Downloading,
    #[allow(dead_code)]
    ReadyToRestart(String),
    UpToDate,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRelease {
    pub version: String,
    pub release_url: String,
    pub notes: Arc<str>,
}

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
    body: Option<String>,
}

#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    state: String,
    size: u64,
}

pub fn is_newer_version(remote: &str, current: &str) -> bool {
    match (
        semver::Version::parse(remote.trim().trim_start_matches('v')),
        semver::Version::parse(current.trim().trim_start_matches('v')),
    ) {
        (Ok(remote), Ok(current)) => remote.cmp_precedence(&current).is_gt(),
        _ => false,
    }
}

fn select_release(
    releases: Vec<GitHubRelease>,
    current: &str,
) -> Result<Option<RemoteRelease>, String> {
    let installed = semver::Version::parse(current)
        .map_err(|_| t!("settings.updates.errors.version").into_owned())?;
    let latest = releases
        .into_iter()
        .filter_map(|release| {
            if release.draft || release.tag_name.len() > 80 {
                return None;
            }
            let version = semver::Version::parse(
                release
                    .tag_name
                    .strip_prefix('v')
                    .unwrap_or(&release.tag_name),
            )
            .ok()?;
            // Stable never offers prereleases; alpha can advance to stable.
            if installed.pre.is_empty() && (release.prerelease || !version.pre.is_empty()) {
                return None;
            }
            if !release.assets.iter().any(|asset| {
                asset.name == INSTALLER_ASSET && asset.state == "uploaded" && asset.size > 0
            }) {
                return None;
            }
            Some((version, release.tag_name, release.body))
        })
        .max_by(|(a, _, _), (b, _, _)| a.cmp_precedence(b));
    Ok(latest.and_then(|(version, tag, body)| {
        is_newer_version(&version.to_string(), current).then(|| RemoteRelease {
            version: version.to_string(),
            // Do not open arbitrary URLs from JSON.
            release_url: format!("{RELEASES_URL}{tag}"),
            notes: body.unwrap_or_default().into(),
        })
    }))
}

/// Clean residual `.exe.old` files from previous updates on startup.
pub fn cleanup_previous_update() {
    if crate::desktop::package::is_packaged() {
        return; // The package directory is read-only and Store owns replacement.
    }
    if let Ok(current_exe) = std::env::current_exe() {
        let old_exe = current_exe.with_extension("exe.old");
        if old_exe.exists() {
            let _ = std::fs::remove_file(old_exe);
        }
    }
}

/// Bounded, credential-free query. Do not download or run the installer.
pub async fn check_for_update(
    client: &reqwest::Client,
    update_url: &str,
) -> Result<Option<RemoteRelease>, String> {
    let mut response = client
        .get(update_url)
        .header("User-Agent", "MakerCalc-Desktop")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| t!("settings.updates.errors.connection").into_owned())?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            403 | 429 => t!("settings.updates.errors.rate_limit").into_owned(),
            _ => t!(
                "settings.updates.errors.http",
                status = response.status().as_u16()
            )
            .into_owned(),
        });
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RELEASE_BYTES as u64)
    {
        return Err(t!("settings.updates.errors.response_size").into_owned());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| t!("settings.updates.errors.read_response").into_owned())?
    {
        if chunk.len() > MAX_RELEASE_BYTES - bytes.len() {
            return Err(t!("settings.updates.errors.response_size").into_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    let releases = serde_json::from_slice(&bytes)
        .map_err(|_| t!("settings.updates.errors.invalid_response").into_owned())?;
    select_release(releases, env!("CARGO_PKG_VERSION"))
}

static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

/// UI requests GPUI quit next; main launches only after shutdown and releasing the profile lock.
pub fn restart_app() {
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
}

pub(crate) fn launch_pending_restart() -> Result<(), String> {
    if RESTART_REQUESTED.swap(false, Ordering::SeqCst) {
        let exe = std::env::current_exe()
            .map_err(|_| t!("settings.updates.errors.find_executable").into_owned())?;
        std::process::Command::new(exe)
            .spawn()
            .map_err(|_| t!("settings.updates.errors.restart").into_owned())?;
    }
    Ok(())
}

/// Shared bounded check for startup and the explicit settings action.
pub struct AutoUpdater {
    pub status: Arc<Mutex<UpdateStatus>>,
    client: Result<reqwest::Client, String>,
}

impl AutoUpdater {
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(UpdateStatus::Idle)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| t!("settings.updates.errors.secure_connection").into_owned()),
        }
    }

    pub fn start_background_check(&self) -> Option<tokio::task::JoinHandle<UpdateStatus>> {
        if crate::desktop::package::is_packaged() {
            return None; // A Store installation must never offer the GitHub EXE installer.
        }
        let mut status = self.status.lock().unwrap();
        if *status == UpdateStatus::Checking {
            return None;
        }
        let client = match &self.client {
            Ok(client) => client.clone(),
            Err(error) => {
                *status = UpdateStatus::Failed(error.clone());
                log_check_failure(error);
                return None;
            }
        };
        *status = UpdateStatus::Checking;
        drop(status);
        let status = Arc::clone(&self.status);
        Some(tokio::spawn(async move {
            let next = match check_for_update(&client, DEFAULT_UPDATE_URL).await {
                Ok(Some(release)) => UpdateStatus::Available(release),
                Ok(None) => UpdateStatus::UpToDate,
                Err(error) => UpdateStatus::Failed(error),
            };
            if let UpdateStatus::Failed(error) = &next {
                log_check_failure(error);
            }
            *status.lock().unwrap() = next.clone();
            next
        }))
    }
}

pub(crate) fn log_check_failure(error: &str) {
    use crate::diagnostics::{LogArea, LogEvent, LogLevel};
    crate::diagnostics::log(
        LogEvent::new(LogLevel::Warn, LogArea::App, "app.update_check_failed").detail(error),
    );
}
