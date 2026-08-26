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
/// take a fifth of that. Length plus modification time changes whenever `setup`
/// rewrites the image, which is the case that actually matters. The inode used
/// to be part of this and was dropped: it has no portable counterpart on
/// Windows and caught nothing the other two miss.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct FileId {
    pub path: String,
    pub len: u64,
    pub mtime_ns: i128,
}

impl FileId {
    pub fn of(p: &Path) -> Result<Self> {
        let (len, mtime_ns) =
            crate::hostfs::identity(p).with_context(|| format!("stat {}", p.display()))?;
        Ok(FileId { path: p.display().to_string(), len, mtime_ns })
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
    /// Identity of every attached capability volume, in attach order. Adding,
    /// removing or rebuilding one changes the device topology, so the frozen
    /// guest has to be rebuilt.
    pub capabilities: Vec<(String, FileId)>,
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
    pub fn workspace(&self) -> PathBuf {
        self.dir.join("ready-workspace.img")
    }
    pub fn artifacts(&self) -> PathBuf {
        self.dir.join("ready-artifacts.img")
    }

}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BaseMeta {
    pub protocol_version: u32,
    pub agent_hash: String,
}

pub fn base_meta_path(base: &Path) -> Result<PathBuf> {
    Ok(base.with_extension("json"))
}

pub fn write_base_meta(base: &Path, agent: &str) -> Result<()> {
    let m = BaseMeta { protocol_version: PROTOCOL_VERSION, agent_hash: fnv1a(agent.as_bytes()) };
    std::fs::write(base_meta_path(base)?, serde_json::to_vec_pretty(&m)?)
        .context("writing base image metadata")?;
    Ok(())
}

/// Confirm the agent baked into the base image is the one this binary expects.
pub fn check_base_meta(base: &Path, agent: &str) -> Result<()> {
    let p = base_meta_path(base)?;
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
    for f in [rs.state_file(), rs.disk(), rs.vars(), rs.mailbox(), rs.workspace(), rs.artifacts()] {
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
    chk!(capabilities, "installed capabilities");
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

// ---------------------------------------------------------- desktop state

/// A prepared desktop guest: Windows booted, the desktop stack up, and the
/// bridge already answering on the control channel — frozen at that instant.
///
/// It exists for the same reason the command ready state does. Booting Windows
/// takes about nine seconds and every one of those seconds is spent doing
/// exactly what the last session did. Restoring RAM and devices instead takes a
/// few hundred milliseconds.
///
/// Freezing *after* the bridge answers is the part that matters. A state frozen
/// at the login prompt would still need the desktop stack and the bridge to
/// come up on every restore, which is most of the cost.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct DesktopFingerprint {
    pub winquick_version: String,
    /// Mailbox protocol, used once per session to start the bridge.
    pub protocol_version: u32,
    /// Control-disk layout the frozen guest speaks.
    pub control_protocol_version: u32,
    pub desktop_image: FileId,
    pub agent_hash: String,
    /// Identity of the built guest bridge. A rebuilt bridge is a different
    /// program and the frozen guest is running the old one.
    pub bridge_hash: String,
    pub qemu_binary: FileId,
    pub qemu_version: String,
    pub firmware: FileId,
    pub memory_mb: u32,
    pub cpus: u32,
    pub machine: String,
    pub capabilities: Vec<(String, FileId)>,
    pub devices: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DesktopMeta {
    pub fingerprint: DesktopFingerprint,
    pub created_unix: u64,
    pub state_bytes: u64,
}

pub struct DesktopReady {
    pub dir: PathBuf,
    pub meta: DesktopMeta,
}

impl DesktopReady {
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
    pub fn bridge(&self) -> PathBuf {
        self.dir.join("ready-bridge.img")
    }
    pub fn app(&self) -> PathBuf {
        self.dir.join("ready-app.img")
    }
    pub fn control(&self) -> PathBuf {
        self.dir.join("ready-control.img")
    }
    pub fn files(&self) -> [PathBuf; 6] {
        [
            self.state_file(),
            self.disk(),
            self.vars(),
            self.mailbox(),
            self.bridge(),
            self.app(),
        ]
    }
}

pub fn desktop_state_dir() -> Result<PathBuf> {
    Ok(paths::root()?.join("states").join(crate::desktop::IMAGE_NAME))
}

fn desktop_meta_path() -> Result<PathBuf> {
    Ok(desktop_state_dir()?.join("ready.json"))
}

/// Load the prepared desktop state, but only if it is complete and still
/// describes the world we are about to run in.
pub fn load_desktop_valid(want: &DesktopFingerprint) -> Result<Option<DesktopReady>> {
    let dir = desktop_state_dir()?;
    let mp = desktop_meta_path()?;
    if !mp.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&mp).context("reading the desktop ready.json")?;
    let meta: DesktopMeta = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("the desktop ready.json is unreadable: {e}"))?;
    let rs = DesktopReady { dir, meta };
    for f in rs.files() {
        if !f.exists() {
            anyhow::bail!("prepared desktop state incomplete: {} is missing", f.display());
        }
    }
    if std::fs::metadata(rs.state_file())?.len() != rs.meta.state_bytes {
        anyhow::bail!("ready.state size does not match ready.json");
    }
    if &rs.meta.fingerprint != want {
        anyhow::bail!("{}", describe_desktop_mismatch(&rs.meta.fingerprint, want));
    }
    Ok(Some(rs))
}

/// Say *what* changed, so a rebuild explains itself instead of just happening.
pub fn describe_desktop_mismatch(have: &DesktopFingerprint, want: &DesktopFingerprint) -> String {
    let mut d = Vec::new();
    macro_rules! chk {
        ($f:ident, $label:expr) => {
            if have.$f != want.$f {
                d.push($label);
            }
        };
    }
    chk!(winquick_version, "winquick version");
    chk!(protocol_version, "mailbox protocol");
    chk!(control_protocol_version, "control protocol");
    chk!(desktop_image, "desktop image");
    chk!(agent_hash, "guest agent");
    chk!(bridge_hash, "guest bridge");
    chk!(qemu_binary, "qemu binary");
    chk!(qemu_version, "qemu version");
    chk!(firmware, "uefi firmware");
    chk!(memory_mb, "guest memory");
    chk!(cpus, "vcpu count");
    chk!(machine, "machine type");
    chk!(capabilities, "installed capabilities");
    chk!(devices, "device configuration");
    if d.is_empty() {
        "prepared desktop state fingerprint differs".into()
    } else {
        format!("prepared desktop state stale: {}", d.join(", "))
    }
}

pub fn save_desktop(meta: &DesktopMeta) -> Result<()> {
    let mp = desktop_meta_path()?;
    std::fs::create_dir_all(mp.parent().unwrap())?;
    std::fs::write(&mp, serde_json::to_vec_pretty(meta)?)
        .context("writing the desktop ready.json")?;
    Ok(())
}

pub fn discard_desktop() -> Result<()> {
    if let Ok(d) = desktop_state_dir() {
        let _ = std::fs::remove_dir_all(&d);
    }
    Ok(())
}

#[cfg(test)]
mod desktop_tests {
    use super::*;

    fn id(name: &str, len: u64) -> FileId {
        FileId { path: name.into(), len, mtime_ns: 1 }
    }

    fn base() -> DesktopFingerprint {
        DesktopFingerprint {
            winquick_version: "0.2.0".into(),
            protocol_version: 1,
            control_protocol_version: 1,
            desktop_image: id("desktop.qcow2", 100),
            agent_hash: "aaaa".into(),
            bridge_hash: "bbbb".into(),
            qemu_binary: id("qemu", 10),
            qemu_version: "QEMU 11.1.0".into(),
            firmware: id("edk2.fd", 20),
            memory_mb: 2048,
            cpus: 2,
            machine: "virt".into(),
            capabilities: vec![("dotnet-sdk".into(), id("dotnet-sdk.img", 30))],
            devices: "machine=virt;...".into(),
        }
    }

    /// Restoring RAM against a machine it did not come from is undefined, so
    /// every input that can change the machine has to be part of the identity.
    /// This is a checklist as much as a test: each case is a way a stale state
    /// could otherwise be run.
    #[test]
    fn every_input_that_changes_the_machine_invalidates_the_state() {
        let cases: Vec<(&str, Box<dyn Fn(&mut DesktopFingerprint)>)> = vec![
            ("winquick version", Box::new(|f: &mut DesktopFingerprint| f.winquick_version = "0.3.0".into())),
            ("mailbox protocol", Box::new(|f: &mut DesktopFingerprint| f.protocol_version = 2)),
            ("control protocol", Box::new(|f: &mut DesktopFingerprint| f.control_protocol_version = 2)),
            ("desktop image", Box::new(|f: &mut DesktopFingerprint| f.desktop_image = id("desktop.qcow2", 101))),
            ("guest agent", Box::new(|f: &mut DesktopFingerprint| f.agent_hash = "cccc".into())),
            ("guest bridge", Box::new(|f: &mut DesktopFingerprint| f.bridge_hash = "dddd".into())),
            ("qemu binary", Box::new(|f: &mut DesktopFingerprint| f.qemu_binary = id("qemu", 11))),
            ("qemu version", Box::new(|f: &mut DesktopFingerprint| f.qemu_version = "QEMU 12".into())),
            ("uefi firmware", Box::new(|f: &mut DesktopFingerprint| f.firmware = id("edk2.fd", 21))),
            ("guest memory", Box::new(|f: &mut DesktopFingerprint| f.memory_mb = 4096)),
            ("vcpu count", Box::new(|f: &mut DesktopFingerprint| f.cpus = 4)),
            ("machine type", Box::new(|f: &mut DesktopFingerprint| f.machine = "virt-9".into())),
            ("installed capabilities", Box::new(|f: &mut DesktopFingerprint| f.capabilities.clear())),
            ("device configuration", Box::new(|f: &mut DesktopFingerprint| f.devices = "other".into())),
        ];

        for (label, mutate) in cases {
            let have = base();
            let mut want = base();
            mutate(&mut want);
            assert_ne!(have, want, "{label} did not change the fingerprint");
            let why = describe_desktop_mismatch(&have, &want);
            assert!(
                why.contains(label),
                "a stale state caused by {label} would be reported as {why:?}"
            );
        }
    }

    /// Rebuilding a capability changes its identity even at the same size, and
    /// the frozen guest has the old one mounted.
    #[test]
    fn a_rebuilt_capability_invalidates_the_state() {
        let have = base();
        let mut want = base();
        want.capabilities = vec![(
            "dotnet-sdk".into(),
            FileId { path: "dotnet-sdk.img".into(), len: 30, mtime_ns: 999 },
        )];
        assert_ne!(have, want);
        assert!(describe_desktop_mismatch(&have, &want).contains("installed capabilities"));
    }

    #[test]
    fn an_identical_fingerprint_is_reusable() {
        assert_eq!(base(), base());
    }
}
