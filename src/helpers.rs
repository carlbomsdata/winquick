//! External programs WinQuick drives during setup.
//!
//! Building a Windows runtime means writing two files into an NTFS volume and
//! setting one registry value, and macOS can do neither: it has no NTFS write
//! support at all, and no notion of Windows registry hives. Rather than
//! implement a filesystem driver, WinQuick shells out to small, well-established
//! tools.
//!
//! | Tool | From | Why |
//! |---|---|---|
//! | `qemu-system-aarch64`, `qemu-img` | QEMU | runs and builds the guest |
//! | `ntfscp`, `ntfscat` | ntfsprogs | read/write files inside the Windows volume |
//! | `hivexsh` | hivex | set one value in a registry hive |
//!
//! All three stay separate executables, invoked as child processes. That is a
//! licensing boundary as much as a design one — see THIRD_PARTY_NOTICES.md.
//!
//! `ntfscp`/`ntfscat` ship with WinQuick because Homebrew's `ntfs-3g` formula is
//! Linux-only. QEMU and hivex are ordinary Homebrew packages.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Only `setup` needs these; `run` needs none of them.
pub struct SetupTools {
    pub ntfscp: PathBuf,
    pub ntfscat: PathBuf,
    pub hivexsh: PathBuf,
}

/// Directories that may hold helpers shipped alongside the CLI.
///
/// Covers the Homebrew layout (`bin/winquick` + `libexec/winquick/`), a plain
/// release archive (helpers next to the binary), and a source checkout.
fn bundled_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(bin) = exe.parent() {
            dirs.push(bin.to_path_buf());
            dirs.push(bin.join("winquick-helpers"));
            if let Some(prefix) = bin.parent() {
                dirs.push(prefix.join("libexec").join("winquick"));
                // cargo target/<profile>/winquick -> repo root
                if let Some(root) = prefix.parent() {
                    dirs.push(root.join("vendor").join("ntfsprogs"));
                    // hivexsh comes from Homebrew on macOS, but Windows has no
                    // package for it, so it is built and shipped alongside.
                    dirs.push(root.join("vendor").join("hivex"));
                }
            }
        }
    }
    dirs
}

/// A helper's file name on this host: `ntfscp` on macOS, `ntfscp.exe` on Windows.
fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn find(name: &str, env_override: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_override).map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let file = exe_name(name);
    for d in bundled_dirs() {
        let c = d.join(&file);
        if c.is_file() {
            return Some(c);
        }
    }
    which(name)
}

/// Find an executable on `PATH`.
///
/// Done here rather than by shelling out, because `which` is not a program on
/// Windows and the separator differs; `split_paths` knows both conventions.
pub fn which(bin: &str) -> Option<PathBuf> {
    let file = exe_name(bin);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(&file)).find(|c| is_executable(c))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Windows has no execute bit; the extension is what makes a file runnable, and
/// [`exe_name`] has already applied it.
#[cfg(windows)]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

pub fn find_ntfscp() -> Option<PathBuf> {
    find("ntfscp", "WINQUICK_NTFSCP")
}
pub fn find_ntfscat() -> Option<PathBuf> {
    // Conventionally installed beside ntfscp; prefer that before searching PATH.
    if let Some(cp) = find_ntfscp() {
        let sibling = cp.with_file_name(exe_name("ntfscat"));
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    find("ntfscat", "WINQUICK_NTFSCAT")
}
pub fn find_hivexsh() -> Option<PathBuf> {
    find("hivexsh", "WINQUICK_HIVEXSH")
}

/// Resolve everything `setup` needs, or explain exactly what to install.
pub fn setup_tools() -> Result<SetupTools> {
    let ntfscp = find_ntfscp();
    let ntfscat = find_ntfscat();
    let hivexsh = find_hivexsh();

    let mut missing: Vec<&str> = Vec::new();
    if ntfscp.is_none() || ntfscat.is_none() {
        missing.push("ntfsprogs");
    }
    if hivexsh.is_none() {
        missing.push("hivex");
    }
    if missing.is_empty() {
        return Ok(SetupTools {
            ntfscp: ntfscp.unwrap(),
            ntfscat: ntfscat.unwrap(),
            hivexsh: hivexsh.unwrap(),
        });
    }

    let mut msg = String::from("winquick setup needs a few tools that are not installed.\n\n");
    if missing.contains(&"hivex") {
        if cfg!(windows) {
            msg.push_str(
                "  hivexsh    normally ships with WinQuick. If you are running from a\n\
                 \x20            source checkout, build it once with:\n\
                 \x20                ./scripts/build-hivex-windows.sh\n",
            );
        } else {
            msg.push_str("  hivex      brew install hivex\n");
        }
    }
    if missing.contains(&"ntfsprogs") {
        msg.push_str(
            "  ntfsprogs  normally ships with WinQuick. If you are running from a\n\
             \x20            source checkout, build it once with:\n\
             \x20                ./scripts/build-ntfs-helpers.sh\n",
        );
    }
    msg.push_str("\nThen run `winquick setup` again. `winquick doctor` shows what is missing.");
    bail!("{msg}");
}

/// How to get QEMU on this host. Homebrew is the only sensible answer on
/// macOS; Windows has no single one, so the hint says what is needed rather
/// than naming a package manager the user may not have.
const QEMU_HINT: &str = if cfg!(target_os = "macos") {
    "brew install qemu"
} else {
    "install QEMU for Windows and put it on PATH"
};

/// Report for `winquick doctor`.
pub struct ToolStatus {
    pub name: &'static str,
    pub path: Option<PathBuf>,
    pub needed_for: &'static str,
    pub install_hint: &'static str,
}

pub fn survey() -> Vec<ToolStatus> {
    vec![
        ToolStatus {
            name: crate::platform::QEMU_SYSTEM,
            path: which(crate::platform::QEMU_SYSTEM),
            needed_for: "running Windows",
            install_hint: QEMU_HINT,
        },
        ToolStatus {
            name: "qemu-img",
            path: which("qemu-img"),
            needed_for: "running Windows",
            install_hint: QEMU_HINT,
        },
        ToolStatus {
            name: "ntfscp",
            path: find_ntfscp(),
            needed_for: "setup only",
            install_hint: "ships with WinQuick; ./scripts/build-ntfs-helpers.sh from source",
        },
        ToolStatus {
            name: "ntfscat",
            path: find_ntfscat(),
            needed_for: "setup only",
            install_hint: "ships with WinQuick; ./scripts/build-ntfs-helpers.sh from source",
        },
        ToolStatus {
            name: "hivexsh",
            path: find_hivexsh(),
            needed_for: "setup only",
            // Windows has no package for it, so WinQuick ships one; if it is
            // missing there, the installation is incomplete rather than the
            // machine underprovisioned.
            install_hint: if cfg!(windows) {
                "should have shipped with WinQuick; reinstall"
            } else {
                "brew install hivex"
            },
        },
        ToolStatus {
            name: if cfg!(windows) { "tar" } else { "unzip" },
            path: which(if cfg!(windows) { "tar" } else { "unzip" }),
            needed_for: "installing capabilities",
            install_hint: if cfg!(windows) {
                "ships with Windows 10 1803 and later"
            } else {
                "ships with macOS"
            },
        },
    ]
}

/// The UEFI firmware QEMU ships. Located relative to the QEMU binary so it keeps
/// working under any Homebrew prefix.
pub fn uefi_firmware() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(q) = which(crate::platform::QEMU_SYSTEM) {
        let q = q.canonicalize().unwrap_or(q);
        if let Some(prefix) = q.parent().and_then(|b| b.parent()) {
            roots.push(prefix.join("share").join("qemu"));
        }
    }
    // Homebrew's prefixes on macOS; the Windows QEMU keeps its firmware beside
    // the executable, which the discovery above already covers.
    roots.push(PathBuf::from("/opt/homebrew/share/qemu"));
    roots.push(PathBuf::from("/usr/local/share/qemu"));
    roots.push(PathBuf::from("C:\\Program Files\\qemu\\share"));
    roots
        .into_iter()
        .map(|r| r.join(crate::platform::UEFI_CODE))
        .find(|p| p.is_file())
}

/// A blank UEFI variable store for one boot.
///
/// The two hosts want different things here, and getting it wrong is not
/// subtle. The aarch64 `virt` machine expects a 64 MiB pflash pair, so a file
/// of zeroes is exactly right. The x86_64 `q35` machine refuses any firmware
/// pair totalling more than 8 MiB, and its code image expects the variable
/// store QEMU ships beside it -- so that template is copied rather than
/// invented. A blank 64 MiB file there produces
/// `combined size of system firmware exceeds 8388608 bytes` and no boot at all.
pub fn fresh_uefi_vars(path: &Path) -> Result<()> {
    if let Some(name) = crate::platform::UEFI_VARS_TEMPLATE {
        let template = uefi_firmware()
            .and_then(|code| code.parent().map(|d| d.join(name)))
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "QEMU is installed but its UEFI variable template {name} is missing"
                )
            })?;
        std::fs::copy(&template, path).map_err(|e| {
            anyhow::anyhow!("copying {} to {}: {e}", template.display(), path.display())
        })?;
        return Ok(());
    }
    std::fs::File::create(path)?.set_len(64 * 1024 * 1024)?;
    Ok(())
}

/// Human-readable size, for status output.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

/// Bytes actually occupied on disk, which for our sparse images is the number
/// that matters.
pub fn allocated(p: &Path) -> u64 {
    crate::hostfs::allocated(p)
}
