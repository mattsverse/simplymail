//! How this copy of SimplyMail was installed, and putting a release in its place.
//!
//! Only copies SimplyMail manages itself are replaced: an app bundle on macOS, or a binary
//! unpacked from a release archive on Linux and Windows. Homebrew, apt and dnf track the files
//! they installed, so for those the update is left to them.

#[cfg(any(target_os = "linux", windows))]
mod archive;
#[cfg(target_os = "macos")]
mod macos;

use std::path::{Path, PathBuf};

use crate::{Error, Result, Version};

/// The operating systems SimplyMail ships for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    /// The platform this build runs on, if SimplyMail ships for it.
    pub(crate) const CURRENT: Option<Self> = if cfg!(target_os = "macos") {
        Some(Self::MacOs)
    } else if cfg!(target_os = "linux") {
        Some(Self::Linux)
    } else if cfg!(windows) {
        Some(Self::Windows)
    } else {
        None
    };
}

/// The Rust target the Linux and Windows release archives are named after, for this build.
const TARGET: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("x86_64-unknown-linux-gnu")
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some("aarch64-unknown-linux-gnu")
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some("x86_64-pc-windows-msvc")
} else if cfg!(all(windows, target_arch = "aarch64")) {
    Some("aarch64-pc-windows-msvc")
} else {
    None
};

/// How this copy of SimplyMail was installed, which decides how it's updated.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Install {
    /// A macOS app bundle outside Homebrew, such as one dragged from the DMG to Applications.
    /// SimplyMail replaces it.
    App { bundle: PathBuf },
    /// A binary unpacked from a release archive. SimplyMail replaces it.
    Binary { exe: PathBuf },
    /// Installed by Homebrew, which updates it.
    Homebrew,
    /// Installed from a .deb or .rpm, which the system's package manager updates.
    Package,
    /// Built from source, or for a platform with no releases: updates aren't installed.
    Unsupported,
}

impl Install {
    /// How the running copy was installed.
    #[must_use]
    pub fn detect() -> Self {
        // A build from source would be replaced by a release, so it's never updated.
        if cfg!(debug_assertions) {
            return Self::Unsupported;
        }
        let (Some(platform), Ok(exe)) = (Platform::CURRENT, std::env::current_exe()) else {
            return Self::Unsupported;
        };
        // Through any symlink, such as Homebrew's in its bin directory.
        let exe = exe.canonicalize().unwrap_or(exe);
        classify(&exe, platform)
    }

    /// Whether SimplyMail can put an update in place itself.
    #[must_use]
    pub const fn can_install(&self) -> bool {
        matches!(self, Self::App { .. } | Self::Binary { .. })
    }

    /// What the user does to update, when SimplyMail can't do it for them.
    #[must_use]
    pub const fn instructions(&self) -> Option<&'static str> {
        match self {
            Self::App { .. } | Self::Binary { .. } => None,
            Self::Homebrew => Some(
                "SimplyMail was installed with Homebrew. To update, run: brew upgrade simplymail",
            ),
            Self::Package => Some(
                "SimplyMail was installed from a package. Download the new .deb or .rpm from the \
                 release page and install it as you did the first.",
            ),
            Self::Unsupported => Some(
                "This copy of SimplyMail can't update itself. Download the new version from the \
                 release page.",
            ),
        }
    }
}

/// How the executable at `exe`, with symlinks resolved, was installed on `platform`.
fn classify(exe: &Path, platform: Platform) -> Install {
    if exe.components().any(|c| c.as_os_str() == "Cellar") {
        return Install::Homebrew;
    }
    match platform {
        Platform::MacOs => app_bundle(exe).map_or(Install::Unsupported, |bundle| Install::App {
            bundle: bundle.to_owned(),
        }),
        // The packages install to /usr/bin; /usr/local is the administrator's.
        Platform::Linux if exe.starts_with("/usr") && !exe.starts_with("/usr/local") => {
            Install::Package
        }
        Platform::Linux | Platform::Windows => Install::Binary {
            exe: exe.to_owned(),
        },
    }
}

/// The `.app` an executable in `Contents/MacOS` belongs to.
fn app_bundle(exe: &Path) -> Option<&Path> {
    let macos = exe.parent().filter(|p| p.ends_with("Contents/MacOS"))?;
    macos
        .parent()?
        .parent()
        .filter(|b| b.extension().is_some_and(|e| e == "app"))
}

/// The release asset holding `version` for `platform`, as `release.yml` names it.
pub(crate) fn asset_name(version: Version, platform: Platform) -> Option<String> {
    match platform {
        Platform::MacOs => Some(format!("SimplyMail-{version}-universal.zip")),
        Platform::Linux => TARGET.map(|t| format!("simplymail-{version}-{t}.tar.gz")),
        Platform::Windows => TARGET.map(|t| format!("simplymail-{version}-{t}.zip")),
    }
}

/// The SHA-256 of `name` in a `sha256sum` listing.
pub(crate) fn expected_sum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.trim().split_once(char::is_whitespace)?;
        // `sha256sum` marks binary-mode files with `*`.
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(sum)
    })
}

/// The lowercase hex SHA-256 of `data`.
pub(crate) fn sha256(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .iter()
        .flat_map(|b| [b >> 4, b & 0xf])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// Puts the downloaded, verified release `archive` in place of `install`. Blocking.
pub(crate) fn replace(install: &Install, archive: &[u8]) -> Result<()> {
    match install {
        #[cfg(target_os = "macos")]
        Install::App { bundle } => macos::replace_bundle(bundle, archive),
        #[cfg(any(target_os = "linux", windows))]
        Install::Binary { exe } => archive::replace_binary(exe, archive),
        _ => {
            let _ = archive;
            Err(Error::CantInstall)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homebrew_installs_are_left_to_homebrew() {
        for exe in [
            "/opt/homebrew/Cellar/simplymail/0.1.0/SimplyMail.app/Contents/MacOS/simplymail",
            "/usr/local/Cellar/simplymail/0.1.0/SimplyMail.app/Contents/MacOS/simplymail",
        ] {
            assert_eq!(classify(Path::new(exe), Platform::MacOs), Install::Homebrew);
        }
        let linuxbrew = "/home/linuxbrew/.linuxbrew/Cellar/simplymail/0.1.0/bin/simplymail";
        assert_eq!(
            classify(Path::new(linuxbrew), Platform::Linux),
            Install::Homebrew
        );
    }

    #[test]
    fn mac_apps_outside_homebrew_are_replaced_whole() {
        let exe = Path::new("/Applications/SimplyMail.app/Contents/MacOS/simplymail");
        assert_eq!(
            classify(exe, Platform::MacOs),
            Install::App {
                bundle: "/Applications/SimplyMail.app".into()
            }
        );
        assert_eq!(
            classify(Path::new("/Users/me/bin/simplymail"), Platform::MacOs),
            Install::Unsupported,
            "a bare binary isn't what macOS releases ship"
        );
    }

    #[test]
    fn linux_packages_are_left_to_the_package_manager() {
        assert_eq!(
            classify(Path::new("/usr/bin/simplymail"), Platform::Linux),
            Install::Package
        );
        for exe in [
            "/usr/local/bin/simplymail",
            "/home/me/.local/bin/simplymail",
        ] {
            assert_eq!(
                classify(Path::new(exe), Platform::Linux),
                Install::Binary { exe: exe.into() }
            );
        }
    }

    #[test]
    fn only_self_managed_copies_install_and_the_rest_say_how() {
        let app = Install::App {
            bundle: "/Applications/SimplyMail.app".into(),
        };
        assert!(app.can_install(), "an app bundle");
        assert_eq!(app.instructions(), None);
        for other in [Install::Homebrew, Install::Package, Install::Unsupported] {
            assert!(!other.can_install(), "{other:?}");
            assert!(other.instructions().is_some(), "{other:?}");
        }
    }

    #[test]
    fn assets_are_named_as_the_release_names_them() {
        let v = "0.2.0".parse().unwrap();
        assert_eq!(
            asset_name(v, Platform::MacOs).as_deref(),
            Some("SimplyMail-0.2.0-universal.zip")
        );
        if let Some(target) = TARGET {
            assert_eq!(
                asset_name(v, Platform::Linux),
                Some(format!("simplymail-0.2.0-{target}.tar.gz"))
            );
            assert_eq!(
                asset_name(v, Platform::Windows),
                Some(format!("simplymail-0.2.0-{target}.zip"))
            );
        }
    }

    #[test]
    fn sums_are_found_by_file_name() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let sums =
            format!("{a}  SimplyMail-0.2.0-universal.zip\n{b} *simplymail-0.2.0.zip\nshort  x\n");
        assert_eq!(
            expected_sum(&sums, "SimplyMail-0.2.0-universal.zip"),
            Some(a.as_str())
        );
        assert_eq!(
            expected_sum(&sums, "simplymail-0.2.0.zip"),
            Some(b.as_str())
        );
        assert_eq!(expected_sum(&sums, "x"), None, "not a SHA-256");
        assert_eq!(expected_sum(&sums, "missing"), None);
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
