//! Where WinQuick keeps things on the host.
//!
//! Everything lives under `~/.winquick`. Nothing here is shared with other
//! users and nothing generated from Microsoft software ever leaves it.

use anyhow::{anyhow, Result};
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
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))
}

pub fn root() -> Result<PathBuf> {
    Ok(home()?.join(".winquick"))
}

/// Pristine base image. Never opened for writing after `setup`.
pub fn base_image() -> Result<PathBuf> {
    Ok(root()?.join("images").join(IMAGE_NAME).join("base.qcow2"))
}

/// User-obtained Microsoft media and other downloads.
pub fn cache() -> Result<PathBuf> {
    Ok(root()?.join("cache"))
}

/// Transient per-run state. Deleted after every run.
pub fn run_dir(id: &str) -> Result<PathBuf> {
    Ok(root()?.join("run").join(id))
}

/// UEFI firmware shipped with QEMU.
pub fn uefi_code() -> Option<PathBuf> {
    crate::helpers::uefi_firmware()
}

/// Transient per-run state. Everything here is deleted when a run ends.
pub fn run_root() -> Result<PathBuf> {
    Ok(root()?.join("run"))
}
