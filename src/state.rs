//! Ready-state lifecycle.
//!
//! A ready state is a Windows guest that has already booted and is sitting in
//! the agent's wait loop, frozen. It is four files plus a fingerprint:
//!
//! ```text
//! ~/.winquick/states/validation-arm64/
//!     ready.state      RAM + device state, from QEMU migration
//!     ready-disk.qcow2 the root overlay exactly as it was at that instant
//!     ready-vars.fd    the UEFI variable store at that instant
//!     ready-mailbox.img the mailbox at that instant (fixes the volume GUID)
//!     ready.json       the fingerprint below
//! ```
//!
//! All four have to be restored together. RAM restored against a different disk
//! is not a slightly-wrong VM, it is an undefined one.

use crate::paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bumped whenever the host and guest halves stop understanding each other:
/// mailbox filenames, agent handshake, anything wire-visible.
pub const PROTOCOL_VERSION: u32 = 1;

/// Cheap identity for a file we do not want to hash on every invocation.
///
/// Hashing `base.qcow2` costs about a second; the whole warm run is supposed to
/// take a fifth of that. Length plus mtime plus inode changes whenever `setup`
/// rewrites the image, which is the case that actually matters.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct FileId {
    pub path: String,
    pub len: u64,
    pub mtime_ns: i64,
    pub inode: u64,
}

impl FileId {
    pub fn of(p: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(p).with_context(|| format!("stat {}", p.display()))?;
        Ok(FileId {
            path: p.display().to_string(),
            len: m.len(),
            mtime_ns: m.mtime() * 1_000_000_000 + m.mtime_nsec(),
            inode: m.ino(),
        })
    }
}

/// Non-cryptographic; this only has to notice that the agent text changed.
pub fn fnv1a(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Fingerprint {
    pub winquick_version: String,
    pub protocol_version: u32,
    pub base_image: FileId,
    pub agent_hash: String,
    pub qemu_binary: FileId,
    pub qemu_version: String,
    pub firmware: FileId,
    pub memory_mb: u32,
    pub cpus: u32,
    pub machine: String,
    /// Identity of the attached capability volume, or `None` when there is none.
    /// Attaching or rebuilding one changes the device topology, so the frozen
    /// guest has to be rebuilt.
    pub capability: Option<FileId>,
    /// Canonical description of the device topology. Migration state is only
    /// meaningful against the exact machine it came from.
    pub devices: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReadyMeta {
    pub fingerprint: Fingerprint,
    pub created_unix: u64,
    pub state_bytes: u64,
}

pub struct ReadyState {
    pub dir: PathBuf,
    pub meta: ReadyMeta,
}

impl ReadyState {
    pub fn state_file(&self) -> PathBuf {
        self.dir.join("ready.state")
    }
    pub fn disk(&self) -> PathBuf {
        self.dir.join("ready-disk.qcow2")
    }
    pub fn vars(&self) -> PathBuf {
        self.dir.join("ready-vars.fd")
    }
    pub fn mailbox(&self) -> PathBuf {
        self.dir.join("ready-mailbox.img")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BaseMeta {
    pub protocol_version: u32,
    pub agent_hash: String,
}

fn base_meta_path(base: &Path) -> PathBuf {
    base.with_extension("json")
}

pub fn write_base_meta(base: &Path, agent: &str) -> Result<()> {
    let m = BaseMeta { protocol_version: PROTOCOL_VERSION, agent_hash: fnv1a(agent.as_bytes()) };
    std::fs::write(base_meta_path(base), serde_json::to_vec_pretty(&m)?)
        .context("writing base image metadata")?;
    Ok(())
}

/// Confirm the agent baked into the base image is the one this binary expects.
pub fn check_base_meta(base: &Path, agent: &str) -> Result<()> {
    let p = base_meta_path(base);
    let stale = "The Windows runtime was built by a different version of winquick.\n\nRebuild it with:  winquick setup --force";
    let text = std::fs::read_to_string(&p).map_err(|_| anyhow::anyhow!("{stale}"))?;
    let m: BaseMeta = serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("{stale}"))?;
    if m.protocol_version != PROTOCOL_VERSION || m.agent_hash != fnv1a(agent.as_bytes()) {
        anyhow::bail!("{stale}");
    }
    Ok(())
}

pub fn state_dir() -> Result<PathBuf> {
    Ok(paths::root()?.join("states").join(paths::IMAGE_NAME))
}

fn meta_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("ready.json"))
}

/// Load the ready state, but only if every piece is present and the fingerprint
/// still matches the world we are about to run in.
pub fn load_valid(want: &Fingerprint) -> Result<Option<ReadyState>> {
    let dir = state_dir()?;
    let mp = meta_path()?;
    if !mp.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&mp).context("reading ready.json")?;
    let meta: ReadyMeta = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            return Err(anyhow::anyhow!("ready.json is unreadable: {e}"));
        }
    };
    let rs = ReadyState { dir, meta };
    for f in [rs.state_file(), rs.disk(), rs.vars(), rs.mailbox()] {
        if !f.exists() {
            anyhow::bail!("ready state incomplete: {} is missing", f.display());
        }
    }
    if std::fs::metadata(rs.state_file())?.len() != rs.meta.state_bytes {
        anyhow::bail!("ready.state size does not match ready.json");
    }
    if &rs.meta.fingerprint != want {
        anyhow::bail!("{}", describe_mismatch(&rs.meta.fingerprint, want));
    }
    Ok(Some(rs))
}

/// Say *what* changed, so `--verbose` explains the rebuild instead of just
/// announcing one.
fn describe_mismatch(have: &Fingerprint, want: &Fingerprint) -> String {
    let mut d = Vec::new();
    macro_rules! chk {
        ($f:ident, $label:expr) => {
            if have.$f != want.$f {
                d.push(format!("{} changed", $label));
            }
        };
    }
    chk!(winquick_version, "winquick version");
    chk!(protocol_version, "mailbox protocol");
    chk!(base_image, "base image");
    chk!(agent_hash, "guest agent");
    chk!(qemu_binary, "qemu binary");
    chk!(qemu_version, "qemu version");
    chk!(firmware, "uefi firmware");
    chk!(memory_mb, "guest memory");
    chk!(cpus, "vcpu count");
    chk!(machine, "machine type");
    chk!(capability, "capability volume");
    chk!(devices, "device configuration");
    if d.is_empty() {
        "ready state fingerprint differs".into()
    } else {
        format!("ready state stale: {}", d.join(", "))
    }
}

pub fn save(meta: &ReadyMeta) -> Result<()> {
    let mp = meta_path()?;
    std::fs::create_dir_all(mp.parent().unwrap())?;
    std::fs::write(&mp, serde_json::to_vec_pretty(meta)?).context("writing ready.json")?;
    Ok(())
}

/// Throw the whole thing away. Deliberately best-effort: discarding runs on the
/// failure path, and failing to discard must not mask the original problem.
pub fn discard() -> Result<()> {
    if let Ok(d) = state_dir() {
        let _ = std::fs::remove_dir_all(&d);
    }
    Ok(())
}
