//! Replacing the running app bundle with a release's, once its signature checks out.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result};

/// Unpacks the release's zipped `SimplyMail.app` next to `bundle`, checks that Apple accepts it
/// and that it's signed by the same developer as the running app, then swaps it in.
pub(super) fn replace_bundle(bundle: &Path, zip: &[u8]) -> Result<()> {
    let parent = bundle.parent().ok_or(Error::CantInstall)?;
    // Next to the bundle, so the swap is a rename on one volume.
    let staging = parent.join(format!(".SimplyMail-update-{}", std::process::id()));
    let result = stage_and_swap(bundle, zip, &staging);
    // Holds the unpacked download, or after the swap the old app; neither is needed now.
    if let Err(e) = std::fs::remove_dir_all(&staging)
        && e.kind() != std::io::ErrorKind::NotFound
        && result.is_ok()
    {
        return Err(Error::Io(e));
    }
    result
}

fn stage_and_swap(bundle: &Path, zip: &[u8], staging: &Path) -> Result<()> {
    if staging.exists() {
        std::fs::remove_dir_all(staging)?;
    }
    std::fs::create_dir(staging).map_err(|e| Error::not_writable(staging, e))?;
    let archive = staging.join("update.zip");
    std::fs::write(&archive, zip)?;
    let unpacked = staging.join("unpacked");
    // ditto keeps the bundle's signature, extended attributes and symlinks intact.
    run(
        Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(&archive)
            .arg(&unpacked),
        "unpack the update",
    )?;
    let new = unpacked.join("SimplyMail.app");
    if !new.join("Contents/MacOS/simplymail").is_file() {
        return Err(Error::Install(
            "the download doesn't hold SimplyMail.app".into(),
        ));
    }
    verify(&new, bundle)?;

    let aside: PathBuf = staging.join("previous.app");
    std::fs::rename(bundle, &aside).map_err(|e| Error::not_writable(bundle, e))?;
    if let Err(e) = std::fs::rename(&new, bundle) {
        // Put the running app back where it was.
        std::fs::rename(&aside, bundle)?;
        return Err(Error::Io(e));
    }
    Ok(())
}

/// Fails unless `new` is intact, accepted by Gatekeeper (Developer ID signed and notarized), and,
/// when the running app is signed by a team, signed by the same team.
fn verify(new: &Path, running: &Path) -> Result<()> {
    run(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(new),
        "verify the update's signature",
    )?;
    run(
        Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute"])
            .arg(new),
        "confirm Apple notarized the update",
    )?;
    let expected = team(running)?;
    if let Some(expected) = expected
        && team(new)?.as_deref() != Some(expected.as_str())
    {
        return Err(Error::Install(
            "the update isn't signed by SimplyMail's developer".into(),
        ));
    }
    Ok(())
}

/// The team that signed `bundle`, or `None` for an ad hoc signature, such as a local build's.
fn team(bundle: &Path) -> Result<Option<String>> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=2"])
        .arg(bundle)
        .output()?;
    // codesign describes the signature on stderr.
    let description = String::from_utf8_lossy(&output.stderr);
    Ok(description
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set")
        .map(str::to_owned))
}

fn run(command: &mut Command, doing: &str) -> Result<()> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let why = stderr.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
    Err(Error::Install(format!("couldn't {doing}: {}", why.trim())))
}
