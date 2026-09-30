use std::path::{Path, PathBuf};

use crate::APP_SLUG;
use crate::release::{Asset, Release};

/// How this particular installation gets updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Windows, installed by the setup program (its uninstaller `unins000.exe` sits next to the
    /// executable). The setup program is run silently over the existing installation.
    WindowsInstaller { arch: &'static str },
    /// Windows, unpacked from the portable zip: the executable is replaced in place.
    WindowsPortable { arch: &'static str },
    /// macOS `.app` bundle: the whole bundle is swapped for the one in the release zip.
    MacApp { app_path: PathBuf },
    /// Linux AppImage: the AppImage file is replaced.
    LinuxAppImage { path: PathBuf, arch: &'static str },
    /// Linux binary from the tarball, in a directory the user can write to.
    LinuxBinary { arch: &'static str },
}

impl Target {
    pub fn describe(&self) -> String {
        match self {
            Target::WindowsInstaller { arch } => format!("Windows {arch} installer"),
            Target::WindowsPortable { arch } => format!("Windows {arch} portable"),
            Target::MacApp { .. } => "macOS app".into(),
            Target::LinuxAppImage { arch, .. } => format!("Linux {arch} AppImage"),
            Target::LinuxBinary { arch } => format!("Linux {arch} tarball"),
        }
    }

    /// The release asset suitable for this target.
    pub fn pick_asset(&self, release: &Release) -> Option<Asset> {
        let suffix = match self {
            Target::WindowsInstaller { arch } => format!("-windows-{arch}-setup.exe"),
            Target::WindowsPortable { arch } => format!("-windows-{arch}-portable.zip"),
            Target::MacApp { .. } => "-macos-universal.zip".to_owned(),
            Target::LinuxAppImage { arch, .. } => format!("-linux-{arch}.AppImage"),
            Target::LinuxBinary { arch } => format!("-linux-{arch}.tar.gz"),
        };
        release.assets.iter().find(|a| a.name.starts_with(APP_SLUG) && a.name.ends_with(&suffix)).cloned()
    }
}

fn arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("aarch64"),
        _ => None,
    }
}

/// Work out how the running program was installed. `Err` explains why it cannot update itself.
pub fn detect_target() -> std::result::Result<Target, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the executable: {e}"))?;
    match std::env::consts::OS {
        "windows" => {
            let arch = arch().ok_or("unsupported CPU architecture")?;
            let installed = exe.parent().is_some_and(|d| d.join("unins000.exe").exists());
            Ok(if installed { Target::WindowsInstaller { arch } } else { Target::WindowsPortable { arch } })
        }
        "macos" => match enclosing_app(&exe) {
            Some(app_path) => Ok(Target::MacApp { app_path }),
            None => Err("not running from an .app bundle".into()),
        },
        "linux" => {
            let arch = arch().ok_or("unsupported CPU architecture")?;
            if let Some(p) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()) {
                return Ok(Target::LinuxAppImage { path: p, arch });
            }
            let managed = ["/usr/", "/opt/", "/snap/", "/nix/"].iter().any(|p| exe.starts_with(p));
            if managed || !dir_writable(exe.parent()) {
                return Err("installed by a system package or in a read-only location; use your package manager or download the new version".into());
            }
            Ok(Target::LinuxBinary { arch })
        }
        other => Err(format!("unsupported operating system: {other}")),
    }
}

/// The `.app` directory that contains `exe` (`Foo.app/Contents/MacOS/foo`).
pub(crate) fn enclosing_app(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
}

fn dir_writable(dir: Option<&Path>) -> bool {
    let Some(dir) = dir else { return false };
    let probe = dir.join(".ncm-update-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}
