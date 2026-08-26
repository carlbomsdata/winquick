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
                }
            }
        }
    }
    dirs
}

fn find(name: &str, env_override: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_override).map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    for d in bundled_dirs() {
        let c = d.join(name);
        if c.is_file() {
            return Some(c);
        }
    }
    which(name)
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("/usr/bin/which").arg(bin).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    p.is_file().then_some(p)
}

pub fn find_ntfscp() -> Option<PathBuf> {
    find("ntfscp", "WINQUICK_NTFSCP")
}
pub fn find_ntfscat() -> Option<PathBuf> {
    // Conventionally installed beside ntfscp; prefer that before searching PATH.
    if let Some(cp) = find_ntfscp() {
        let sibling = cp.with_file_name("ntfscat");
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
        msg.push_str("  hivex      brew install hivex\n");
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
            name: "qemu-system-aarch64",
            path: which("qemu-system-aarch64"),
            needed_for: "running Windows",
            install_hint: "brew install qemu",
        },
        ToolStatus {
            name: "qemu-img",
            path: which("qemu-img"),
            needed_for: "running Windows",
            install_hint: "brew install qemu",
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
            install_hint: "brew install hivex",
        },
        ToolStatus {
            name: "unzip",
            path: which("unzip"),
            needed_for: "installing capabilities",
            install_hint: "ships with macOS",
        },
    ]
}

/// The UEFI firmware QEMU ships. Located relative to the QEMU binary so it keeps
/// working under any Homebrew prefix.
pub fn uefi_firmware() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(q) = which("qemu-system-aarch64") {
        let q = q.canonicalize().unwrap_or(q);
        if let Some(prefix) = q.parent().and_then(|b| b.parent()) {
            roots.push(prefix.join("share").join("qemu"));
        }
    }
    roots.push(PathBuf::from("/opt/homebrew/share/qemu"));
    roots.push(PathBuf::from("/usr/local/share/qemu"));
    roots
        .into_iter()
        .map(|r| r.join("edk2-aarch64-code.fd"))
        .find(|p| p.is_file())
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
