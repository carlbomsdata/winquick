//! Where WinQuick keeps things on the host.
//!
//! Everything lives under `~/.winquick`. Nothing here is shared with other
//! users and nothing generated from Microsoft software ever leaves it.

use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;

/// The runtime directory name, which carries the guest architecture: an x64
/// runtime under an `arm64` name would be a trap for anyone reading the
/// directory, and the two are not interchangeable.
pub const IMAGE_NAME: &str =
    if cfg!(target_arch = "aarch64") { "validation-arm64" } else { "validation-x64" };

/// The user's home directory.
///
/// `HOME` is the Unix answer and is also what a shell sets on Windows, but a
/// `winquick.exe` started from cmd.exe or Explorer sees only `USERPROFILE`.
/// Preferring `HOME` keeps a deliberately overridden home working on both.
pub fn home() -> Result<PathBuf> {
    check_home(
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from),
    )
}

/// The rule [`home`] applies, separated from where the value comes from so it
/// can be tested without mutating the environment of a parallel test binary.
///
/// Every path WinQuick writes to or deletes is derived from the home directory.
/// An empty or relative one would put `~/.winquick` somewhere that depends on
/// the current directory, which would make `winquick clean` delete a `.winquick`
/// in whatever directory it happened to be run from. Refusing is the only safe
/// answer, and it is not a situation a working shell produces.
fn check_home(home: Option<PathBuf>) -> Result<PathBuf> {
    let home = home.ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))?;
    if home.as_os_str().is_empty() {
        bail!("HOME is set but empty, so WinQuick cannot tell where to keep its data");
    }
    if !home.is_absolute() {
        bail!(
            "HOME is not an absolute path ({}), so WinQuick cannot tell where to keep its data",
            home.display()
        );
    }
    Ok(home)
}

pub fn root() -> Result<PathBuf> {
    Ok(home()?.join(".winquick"))
}

/// Pristine base image. Never opened for writing after `setup`.
pub fn base_image() -> Result<PathBuf> {
    Ok(root()?.join("images").join(IMAGE_NAME).join("base.qcow2"))
}

/// The runtime name for the same guest with a .NET Framework serviced into it.
pub const FRAMEWORK_IMAGE_NAME: &str =
    if cfg!(target_arch = "aarch64") { "netfx-arm64" } else { "netfx-x64" };

/// The base image with a .NET Framework in it, whether or not it exists.
///
/// A second image rather than a modified one: the pristine base stays
/// byte-identical, and removing the capability is deleting a directory.
pub fn framework_image() -> Result<PathBuf> {
    Ok(root()?.join("images").join(FRAMEWORK_IMAGE_NAME).join("base.qcow2"))
}

/// The image `winquick run` should boot.
///
/// The serviced image when it is installed, the pristine one otherwise. The
/// ready-state fingerprint carries the image's identity, so switching either
/// way rebuilds the prepared guest without anything else having to notice.
pub fn run_image() -> Result<PathBuf> {
    let netfx = framework_image()?;
    if netfx.exists() {
        return Ok(netfx);
    }
    base_image()
}

/// User-obtained Microsoft media and other downloads.
pub fn cache() -> Result<PathBuf> {
    Ok(root()?.join("cache"))
}

/// Scratch space for work in progress: unpacked archives, staged volumes,
/// build output on its way somewhere else.
///
/// Deliberately here rather than in the system temporary directory. On a
/// multi-user Linux host `/tmp` is world-writable, so a fixed name under it can
/// be pre-created by someone else as a symlink pointing anywhere they like, and
/// WinQuick would write through it. `~/.winquick` belongs to one user, and
/// `winquick clean` already knows to empty this.
pub fn work() -> Result<PathBuf> {
    Ok(root()?.join("work"))
}

/// Transient per-run state. Deleted after every run.
pub fn run_dir(id: &str) -> Result<PathBuf> {
    Ok(root()?.join("run").join(id))
}

/// UEFI firmware shipped with QEMU.
pub fn uefi_code() -> Option<PathBuf> {
    crate::helpers::uefi_firmware()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The .NET Framework capability is a *second* image, never a modified
    /// base. If these ever named the same file, installing it would rewrite
    /// the pristine runtime and "the base image is never written to" would
    /// quietly stop being true.
    #[test]
    fn the_serviced_image_is_never_the_pristine_one() {
        assert_ne!(IMAGE_NAME, FRAMEWORK_IMAGE_NAME);
        let (base, netfx) = (base_image().unwrap(), framework_image().unwrap());
        assert_ne!(base, netfx);
        // Siblings under `images/`, so removing the capability is removing one
        // directory and nothing else.
        assert_eq!(base.parent().and_then(|p| p.parent()), netfx.parent().and_then(|p| p.parent()));
    }

    /// `winquick clean` deletes directories under `root()`, and `root()` is
    /// `home()` plus one component. A home that is empty or relative would aim
    /// that at the current directory, so it is refused outright.
    #[test]
    fn a_home_winquick_cannot_trust_is_refused() {
        assert!(check_home(None).is_err(), "an unset home is not usable");
        for bad in ["", "relative/home", ".", ".."] {
            assert!(
                check_home(Some(PathBuf::from(bad))).is_err(),
                "{bad:?} should not be accepted as a home"
            );
        }
        let good = if cfg!(windows) { "C:\\Users\\someone" } else { "/home/someone" };
        assert_eq!(check_home(Some(PathBuf::from(good))).unwrap(), PathBuf::from(good));
    }

    /// A run boots the serviced image when it is installed and the pristine
    /// one otherwise — and never anything else. Which of the two it is
    /// depends on the machine this runs on, so the test asserts the rule
    /// rather than the answer.
    #[test]
    fn a_run_boots_the_serviced_image_only_when_it_exists() {
        let (base, netfx) = (base_image().unwrap(), framework_image().unwrap());
        let chosen = run_image().unwrap();
        if netfx.exists() {
            assert_eq!(chosen, netfx);
        } else {
            assert_eq!(chosen, base);
        }
    }
}
