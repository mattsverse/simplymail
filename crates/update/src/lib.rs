//! Keeping SimplyMail up to date from its GitHub releases.
//!
//! - [`check`]: whether a release newer than the running version is out, with what changed in
//!   every release since ([`Available::changelog`]).
//! - [`Install`]: how the running copy was installed, which decides whether SimplyMail can
//!   update it or the user does, through Homebrew or their package manager.
//! - [`install`]: downloads the newest release, checks it against the release's `SHA256SUMS`
//!   (and on macOS, its Developer ID signature and notarization), and puts it in place of the
//!   running copy, which picks it up once restarted.

mod install;
mod release;
mod version;

use std::sync::Arc;

use rustls_platform_verifier::BuilderVerifierExt as _;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::crypto::ring;

pub use install::Install;
use install::Platform;
pub use release::{Asset, Available, Release};
pub use version::{InvalidVersion, Version};

/// Where releases are published.
pub const REPOSITORY: &str = "mattsverse/simplymail";

/// How many releases [`check`] reads: enough to gather every changelog since an old version.
const LISTED: usize = 30;

/// Why checking for or installing an update failed, phrased for the user.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("couldn't reach GitHub: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub sent releases SimplyMail can't read: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Tls(#[from] tokio_rustls::rustls::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("this release has no download for this computer")]
    NoAsset,
    #[error("the release has no checksum for {0}")]
    NoChecksum(String),
    #[error("the download was corrupted or tampered with: its checksum doesn't match")]
    ChecksumMismatch,
    #[error("can't write to {path}: {source}")]
    NotWritable {
        path: String,
        source: std::io::Error,
    },
    #[error("this copy of SimplyMail can't update itself")]
    CantInstall,
    #[error("{0}")]
    Install(String),
}

impl Error {
    fn not_writable(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::NotWritable {
            path: path.display().to_string(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An HTTPS client that checks certificates against the operating system's trust store, as
/// SimplyMail's mail connections do.
fn client() -> Result<reqwest::Client> {
    let tls = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    Ok(reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        // GitHub's API refuses requests without one.
        .user_agent(concat!("SimplyMail/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// The newest release if it's newer than `current`, with every changelog since `current`.
/// `None` when `current` is the newest.
///
/// # Errors
///
/// Returns an error when GitHub can't be reached or answers with something unexpected.
pub async fn check(current: Version) -> Result<Option<Available>> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page={LISTED}");
    let body = client()?
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(release::newer(serde_json::from_slice(&body)?, current))
}

/// Downloads `release` for this computer, checks it, and puts it in place of the running copy.
///
/// `install` says where the running copy is. `progress` hears the share downloaded so far, from
/// 0 to 100, when the download's size is known. Restart the app to run the new version.
///
/// # Errors
///
/// Returns an error when this copy can't update itself ([`Install::can_install`]), the release
/// has no download for this computer, the download fails its checks, or it can't be put in place.
pub async fn install(
    release: &Release,
    install: Install,
    progress: impl Fn(u8) + Send + 'static,
) -> Result<()> {
    if !install.can_install() {
        return Err(Error::CantInstall);
    }
    let name = Platform::CURRENT
        .and_then(|p| install::asset_name(release.version, p))
        .ok_or(Error::NoAsset)?;
    let asset = release.asset(&name).ok_or(Error::NoAsset)?;
    let sums = release
        .asset("SHA256SUMS")
        .ok_or_else(|| Error::NoChecksum(name.clone()))?;

    let client = client()?;
    let sums = client
        .get(&sums.url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let expected = install::expected_sum(&sums, &name)
        .ok_or_else(|| Error::NoChecksum(name.clone()))?
        .to_ascii_lowercase();

    let mut response = client.get(&asset.url).send().await?.error_for_status()?;
    let size = response.content_length().filter(|&n| n > 0);
    let mut archive = Vec::with_capacity(
        size.and_then(|n| usize::try_from(n).ok())
            .unwrap_or_default(),
    );
    let mut reported = None;
    while let Some(chunk) = response.chunk().await? {
        archive.extend_from_slice(&chunk);
        if let Some(size) = size {
            let done = u64::try_from(archive.len()).unwrap_or(u64::MAX);
            let percent = u8::try_from((done.saturating_mul(100) / size).min(100)).unwrap_or(100);
            if reported != Some(percent) {
                reported = Some(percent);
                progress(percent);
            }
        }
    }
    if install::sha256(&archive) != expected {
        return Err(Error::ChecksumMismatch);
    }
    tokio::task::spawn_blocking(move || install::replace(&install, &archive))
        .await
        .map_err(|e| Error::Install(format!("the installer stopped: {e}")))?
}
