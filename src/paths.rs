//! Where WinQuick keeps things on the host.
//!
//! Everything lives under `~/.winquick`. Nothing here is shared with other
//! users and nothing generated from Microsoft software ever leaves it.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

pub const IMAGE_NAME: &str = "validation-arm64";

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set"))
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
