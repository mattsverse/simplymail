//! Replacing the running binary with the one in a release archive: a .tar.gz on Linux, a .zip on
//! Windows, each holding `simplymail-<version>-<target>/simplymail[.exe]`.

use std::io::Read as _;
use std::path::Path;

use crate::{Error, Result};

/// The binary's name inside the archive.
const BINARY: &str = if cfg!(windows) {
    "simplymail.exe"
} else {
    "simplymail"
};

/// Takes the binary out of `archive` and puts it in place of `exe`, the running executable.
pub(super) fn replace_binary(exe: &Path, archive: &[u8]) -> Result<()> {
    let binary = unpack(archive)?;
    let dir = exe.parent().ok_or(Error::CantInstall)?;
    // In the same directory, so moving it into place is a rename on one volume.
    let staged = dir.join(format!(".{BINARY}.update"));
    std::fs::write(&staged, binary).map_err(|e| Error::not_writable(dir, e))?;
    let result = swap(&staged, exe);
    if result.is_err() {
        drop(std::fs::remove_file(&staged));
    }
    result
}

#[cfg(unix)]
fn swap(staged: &Path, exe: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))?;
    // The running process keeps the old file open until it exits.
    std::fs::rename(staged, exe).map_err(|e| Error::not_writable(exe, e))
}

#[cfg(windows)]
fn swap(staged: &Path, exe: &Path) -> Result<()> {
    // Windows won't overwrite a running .exe; self-replace moves it aside and deletes it later.
    self_replace::self_replace(staged).map_err(|e| Error::not_writable(exe, e))?;
    std::fs::remove_file(staged)?;
    Ok(())
}

/// The binary at the top of the archive's only directory.
#[cfg(target_os = "linux")]
fn unpack(archive: &[u8]) -> Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file() && is_binary(&entry.path()?) {
            let mut binary = Vec::new();
            entry.read_to_end(&mut binary)?;
            return Ok(binary);
        }
    }
    Err(missing())
}

/// The binary at the top of the archive's only directory.
#[cfg(windows)]
fn unpack(archive: &[u8]) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| Error::Install(format!("couldn't read the download: {e}")))?;
    for i in 0..zip.len() {
        let mut file = zip
            .by_index(i)
            .map_err(|e| Error::Install(format!("couldn't read the download: {e}")))?;
        if file.is_file() && file.enclosed_name().is_some_and(|p| is_binary(&p)) {
            let mut binary = Vec::new();
            file.read_to_end(&mut binary)?;
            return Ok(binary);
        }
    }
    Err(missing())
}

/// Whether `path`, inside an archive, is `<directory>/<BINARY>`.
fn is_binary(path: &Path) -> bool {
    let mut parts = path.components();
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(std::path::Component::Normal(_)), Some(std::path::Component::Normal(name)), None)
            if name == BINARY
    )
}

fn missing() -> Error {
    Error::Install(format!("the download doesn't hold {BINARY}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_binary_in_the_top_directory_counts() {
        let dir = "simplymail-0.2.0-target";
        assert!(is_binary(&Path::new(dir).join(BINARY)), "the binary");
        assert!(!is_binary(Path::new(BINARY)), "not in a directory");
        assert!(
            !is_binary(&Path::new(dir).join("nested").join(BINARY)),
            "too deep"
        );
        assert!(
            !is_binary(&Path::new(dir).join("README.md")),
            "another file"
        );
    }

    #[cfg(target_os = "linux")]
    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len().try_into().unwrap());
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(windows)]
    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (path, data) in files {
            zip.start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn the_binary_is_unpacked_and_nothing_else() {
        let dir = "simplymail-0.2.0-target";
        let binary = format!("{dir}/{BINARY}");
        let readme = format!("{dir}/README.md");
        let archive = archive(&[(&readme, b"readme"), (&binary, b"new binary")]);
        assert_eq!(unpack(&archive).unwrap(), b"new binary");
        let without = self::archive(&[(&readme, b"readme")]);
        assert!(unpack(&without).is_err(), "no binary");
    }

    #[test]
    fn the_binary_is_swapped_in() {
        let dir = std::env::temp_dir().join(format!("simplymail-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(BINARY);
        std::fs::write(&exe, b"old").unwrap();
        let archive = archive(&[(&format!("simplymail-0.2.0-target/{BINARY}"), b"new")]);
        // Not the running executable, so Windows takes the same path as a plain rename would.
        #[cfg(unix)]
        {
            replace_binary(&exe, &archive).unwrap();
            assert_eq!(std::fs::read(&exe).unwrap(), b"new");
            assert!(
                !dir.join(format!(".{BINARY}.update")).exists(),
                "nothing is left behind"
            );
        }
        #[cfg(windows)]
        assert_eq!(unpack(&archive).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
