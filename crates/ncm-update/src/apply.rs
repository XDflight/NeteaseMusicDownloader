use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use semver::Version;

use crate::target::Target;
use crate::{APP_SLUG, Result, UpdateError};

/// A verified download, ready to be applied.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub path: PathBuf,
    pub target: Target,
    pub version: Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The update is staged or already in place; the application must exit now so that it can
    /// finish (installer) or so that the new version is the one that keeps running.
    ExitNow,
}

/// Apply the update. On success the caller should exit the process immediately.
pub fn apply(prepared: Prepared) -> Result<ApplyOutcome> {
    match &prepared.target {
        Target::WindowsInstaller { .. } => run_windows_installer(&prepared.path)?,
        Target::WindowsPortable { .. } => {
            let new_exe = extract_zip_file(&prepared.path, &format!("{APP_SLUG}.exe"))?;
            replace_current_exe(&new_exe)?;
        }
        Target::LinuxBinary { .. } => {
            let new_bin = extract_tar_gz_file(&prepared.path, APP_SLUG)?;
            replace_current_exe(&new_bin)?;
        }
        Target::MacApp { app_path } => swap_mac_app(&prepared.path, app_path)?,
        Target::LinuxAppImage { path, .. } => replace_appimage(&prepared.path, path)?,
    }
    Ok(ApplyOutcome::ExitNow)
}

// ---------------------------------------------------------------------------- Windows

#[cfg(windows)]
fn run_windows_installer(setup: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    // NSIS flags: `/S` installs silently over the existing installation, `/UPDATE` makes the
    // installer wait for this process to exit and start the new version when it is done.
    Command::new(setup)
        .args(["/S", "/UPDATE"])
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn run_windows_installer(_setup: &Path) -> Result<()> {
    Err(UpdateError::Unsupported("the Windows installer cannot run on this OS".into()))
}

// ------------------------------------------------------------------ in-place replacement

fn replace_current_exe(new_exe: &Path) -> Result<()> {
    self_replace::self_replace(new_exe)?;
    let _ = fs::remove_file(new_exe);
    relaunch(&std::env::current_exe()?)
}

fn relaunch(exe: &Path) -> Result<()> {
    Command::new(exe).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    Ok(())
}

pub(crate) fn extract_zip_file(zip_path: &Path, wanted: &str) -> Result<PathBuf> {
    let mut archive = zip::ZipArchive::new(fs::File::open(zip_path)?).map_err(|e| UpdateError::Archive(e.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| UpdateError::Archive(e.to_string()))?;
        let is_match = entry.is_file() && Path::new(entry.name()).file_name().is_some_and(|n| n == wanted);
        if is_match {
            let out = zip_path.with_file_name(format!("{wanted}.new"));
            io::copy(&mut entry, &mut fs::File::create(&out)?)?;
            set_executable(&out)?;
            return Ok(out);
        }
    }
    Err(UpdateError::Archive(format!("{wanted} not found in the archive")))
}

pub(crate) fn extract_tar_gz_file(tar_path: &Path, wanted: &str) -> Result<PathBuf> {
    let gz = flate2::read::GzDecoder::new(fs::File::open(tar_path)?);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let is_match = entry.header().entry_type().is_file() && entry.path()?.file_name().is_some_and(|n| n == wanted);
        if is_match {
            let out = tar_path.with_file_name(format!("{wanted}.new"));
            io::copy(&mut entry, &mut fs::File::create(&out)?)?;
            set_executable(&out)?;
            return Ok(out);
        }
    }
    Err(UpdateError::Archive(format!("{wanted} not found in the archive")))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

// ------------------------------------------------------------------------------- macOS

fn swap_mac_app(zip_path: &Path, app_path: &Path) -> Result<()> {
    let parent = app_path.parent().ok_or_else(|| UpdateError::Unsupported("app has no parent folder".into()))?;
    let staging = parent.join(format!(".{APP_SLUG}-update"));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(no_permission)?;

    let result = (|| -> Result<()> {
        let mut archive = zip::ZipArchive::new(fs::File::open(zip_path)?).map_err(|e| UpdateError::Archive(e.to_string()))?;
        archive.extract(&staging).map_err(|e| UpdateError::Archive(e.to_string()))?;
        let new_app = fs::read_dir(&staging)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or_else(|| UpdateError::Archive("no .app bundle in the archive".into()))?;

        let backup = app_path.with_extension("app.old");
        let _ = fs::remove_dir_all(&backup);
        fs::rename(app_path, &backup).map_err(no_permission)?;
        if let Err(e) = fs::rename(&new_app, app_path) {
            // Put the old version back before reporting the failure.
            let _ = fs::rename(&backup, app_path);
            return Err(e.into());
        }
        let _ = fs::remove_dir_all(&backup);
        Ok(())
    })();
    let _ = fs::remove_dir_all(&staging);
    result?;

    Command::new("open").arg("-n").arg(app_path).spawn()?;
    Ok(())
}

fn no_permission(e: io::Error) -> UpdateError {
    if e.kind() == io::ErrorKind::PermissionDenied {
        UpdateError::Unsupported("no permission to modify the installed app; download the new version manually".into())
    } else {
        e.into()
    }
}

// ------------------------------------------------------------------------------- Linux

fn replace_appimage(downloaded: &Path, current: &Path) -> Result<()> {
    let staged = current.with_extension("AppImage.new");
    fs::copy(downloaded, &staged).map_err(no_permission)?;
    set_executable(&staged)?;
    // Renaming over a running AppImage is fine: the old inode stays alive until we exit.
    fs::rename(&staged, current).map_err(no_permission)?;
    relaunch(current)
}
