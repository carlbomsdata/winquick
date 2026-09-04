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

    /// The capability volume as it stood at the freeze.
    ///
    /// Every other attached volume is snapshotted here for the same reason: the
    /// frozen guest has that volume *mounted*, so its RAM holds a filesystem
    /// cache describing those exact bytes. Handing the restored guest a
    /// different image -- even one differing only by the writes Windows itself
    /// made when it mounted the volume -- leaves the cache describing something
    /// that is no longer on the disk.
    pub fn capability(&self, i: usize) -> PathBuf {
        self.dir.join(format!("ready-cap{i}.img"))
    }

    /// How many capability volumes this state was frozen with.
    ///
    /// Read from the fingerprint rather than by counting files, so a state
    /// missing one of them fails the completeness check instead of silently
    /// running with fewer disks than it was frozen with.
    pub fn capability_count(&self) -> usize {
        self.meta.fingerprint.capabilities.len()
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
/// Where the "this host cannot restore a prepared guest" note lives.
///
/// Beside the state directories rather than inside one: whether a restore works
/// is a property of the QEMU and the accelerator, not of any particular image,
/// so it must outlive `setup` discarding a stale prepared guest.
pub fn restore_note() -> Result<PathBuf> {
    Ok(crate::paths::root()?.join("restore-unsupported"))
}

/// Record that a prepared guest was built here, restored, and then did nothing.
///
/// This is not a failure WinQuick can fix and not one it should keep paying
/// for. Without the note, every run rebuilds a prepared guest, restores it,
/// waits, gives up and boots cold -- three boots to run one command. With it,
/// the run goes straight to a cold boot, which works.
///
/// The backend signature is written alongside, so installing a QEMU that *can*
/// restore makes the note stop applying by itself.
pub fn mark_restore_unsupported(signature: &str) -> Result<()> {
    write_note(&restore_note()?, signature)
}

/// Where the "a prepared guest has restored on this host" note lives.
///
/// Beside the other one, and for the same reason: whether restore works is a
/// property of the QEMU and the accelerator, not of any particular image.
pub fn restore_works_note() -> Result<PathBuf> {
    Ok(crate::paths::root()?.join("restore-works"))
}

/// Write one of the two backend notes, if it does not already say this.
fn write_note(p: &Path, signature: &str) -> Result<()> {
    if note_says(p, signature) {
        return Ok(());
    }
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(p, signature)?;
    Ok(())
}

/// Whether a note exists and is about this backend.
///
/// A note left by a different QEMU is not about this one, and says nothing.
fn note_says(p: &Path, signature: &str) -> bool {
    std::fs::read_to_string(p).map(|s| s.trim() == signature).unwrap_or(false)
}

/// Record that a prepared guest restored here and ran a command.
///
/// This is the evidence that outranks a run of silent guests: where a prepared
/// guest gets frozen is partly luck, and three unlucky ones in a row say
/// nothing about a machine that has already done the thing.
pub fn mark_restore_works(signature: &str) -> Result<()> {
    write_note(&restore_works_note()?, signature)
}

/// Whether a prepared guest has ever restored here with this backend.
pub fn restore_works(signature: &str) -> bool {
    restore_works_note().map(|p| note_says(&p, signature)).unwrap_or(false)
}

/// Whether restoring is already known not to work with this backend.
pub fn restore_unsupported(signature: &str) -> bool {
    restore_note().map(|p| note_says(&p, signature)).unwrap_or(false)
}

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
    let mut required =
        vec![rs.state_file(), rs.disk(), rs.vars(), rs.mailbox(), rs.workspace(), rs.artifacts()];
    required.extend((0..rs.capability_count()).map(|i| rs.capability(i)));
    for f in required {
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

/// Withdraw the "there is a prepared guest" claim.
///
/// Called before a freeze starts overwriting the files a previous freeze
/// published. `ready.json` is the only thing that advertises a prepared guest,
/// so removing it first means an interrupted freeze leaves a guest that is
/// merely absent rather than one that claims to be ready and is not.
pub fn unpublish() -> Result<()> {
    if let Ok(mp) = meta_path() {
        let _ = std::fs::remove_file(mp);
    }
    Ok(())
}

/// What is structurally wrong with the prepared guest, if anything.
///
/// Cheap on purpose: it reads one small JSON file and stats the rest. It
/// answers "was this freeze finished and are its pieces still here", not "will
/// this guest restore" - the second question needs a hypervisor, and that is
/// what `doctor --smoke` is for.
///
/// `None` means nothing detectable is wrong. A prepared guest that does not
/// exist at all is not a problem, and is reported as `None` here; the caller
/// distinguishes absent from broken.
pub fn structural_problem() -> Option<String> {
    structural_problem_in(&state_dir().ok()?)
}

/// As [`structural_problem`], against a named directory, so the rules can be
/// tested without a real prepared guest on the machine running the tests.
pub fn structural_problem_in(dir: &Path) -> Option<String> {
    let mp = dir.join("ready.json");
    if !mp.exists() {
        return None;
    }
    let text = match std::fs::read_to_string(&mp) {
        Ok(t) => t,
        Err(e) => return Some(format!("ready.json cannot be read: {e}")),
    };
    let meta: ReadyMeta = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => return Some(format!("ready.json is unreadable: {e}")),
    };
    let rs = ReadyState { dir: dir.to_path_buf(), meta };
    let mut required =
        vec![rs.state_file(), rs.disk(), rs.vars(), rs.mailbox(), rs.workspace(), rs.artifacts()];
    required.extend((0..rs.capability_count()).map(|i| rs.capability(i)));
    for f in required {
        if !f.exists() {
            return Some(format!(
                "{} is missing",
                f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            ));
        }
    }
    match std::fs::metadata(rs.state_file()) {
        Err(e) => return Some(format!("ready.state cannot be read: {e}")),
        Ok(m) if m.len() != rs.meta.state_bytes => {
            return Some(format!(
                "ready.state is {} but ready.json says {}",
                crate::helpers::human(m.len()),
                crate::helpers::human(rs.meta.state_bytes)
            ))
        }
        Ok(m) if m.len() == 0 => return Some("ready.state is empty".to_string()),
        Ok(_) => {}
    }
    None
}

pub fn save(meta: &ReadyMeta) -> Result<()> {
    let mp = meta_path()?;
    std::fs::create_dir_all(mp.parent().unwrap())?;
    std::fs::write(&mp, serde_json::to_vec_pretty(meta)?).context("writing ready.json")?;
    Ok(())
}

/// Throw the whole thing away. Deliberately best-effort: discarding runs on the
/// failure path, and failing to discard must not mask the original problem.
/// A prepared guest that timed out without ever taking the command.
///
/// Kept beside the state it accuses, so discarding the state forgets it too.
fn strike_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("ready.strike"))
}

/// Record one, and say whether this is the second in a row.
///
/// Neither of the two things this distinguishes can be told apart at the moment
/// they happen. A guest running a slow command and a guest that resumed wrong
/// both leave the command sitting unacknowledged in the mailbox -- the first
/// because a busy guest can leave that FAT write unflushed for a minute, the
/// second because it never got that far. Guessing has been wrong in both
/// directions: calling it a bad guest throws away a good prepared state on
/// every slow command, and calling it a slow command let one bad freeze wedge
/// the fast path for eight hours.
///
/// So it is not guessed. The first one is forgiven and the state kept, which is
/// right for a slow command and costs a wedged guest one more run. The second
/// in a row is not, because a slow command does not repeat by itself and a
/// wedged guest does.
pub fn record_strike() -> bool {
    let Ok(p) = strike_path() else { return false };
    if p.exists() {
        return true;
    }
    let _ = std::fs::write(&p, b"1");
    false
}

/// Forget them. Called whenever a warm run works, because whatever the last
/// timeout was about, this state is demonstrably fine now.
pub fn clear_strikes() {
    if let Ok(p) = strike_path() {
        let _ = std::fs::remove_file(p);
    }
}

pub fn discard() -> Result<()> {
    if let Ok(d) = state_dir() {
        let _ = std::fs::remove_dir_all(&d);
    }
    Ok(())
}

#[cfg(test)]
mod strike_tests {
    /// One timeout is forgiven, two in a row are not. A slow command does not
    /// repeat by itself; a guest that resumed wrong does.
    #[test]
    fn the_second_strike_in_a_row_is_the_one_that_counts() {
        let dir = std::env::temp_dir().join(format!("wq-strike-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ready.strike");

        // Modelled on the real functions, which address the state directory the
        // process is configured for; the rule is what is under test.
        let record = |p: &std::path::Path| {
            if p.exists() {
                true
            } else {
                std::fs::write(p, b"1").unwrap();
                false
            }
        };
        assert!(!record(&p), "the first is forgiven");
        assert!(record(&p), "the second in a row is not");

        // A run that works clears the record, so an occasional slow command
        // never accumulates into a verdict.
        std::fs::remove_file(&p).unwrap();
        assert!(!record(&p), "after a working run it starts again from nothing");

        let _ = std::fs::remove_dir_all(&dir);
    }
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
        [self.state_file(), self.disk(), self.vars(), self.mailbox(), self.bridge(), self.app()]
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
        /// A named change to one field of the fingerprint.
        type Case = (&'static str, Box<dyn Fn(&mut DesktopFingerprint)>);
        let cases: Vec<Case> = vec![
            (
                "winquick version",
                Box::new(|f: &mut DesktopFingerprint| f.winquick_version = "0.3.0".into()),
            ),
            ("mailbox protocol", Box::new(|f: &mut DesktopFingerprint| f.protocol_version = 2)),
            (
                "control protocol",
                Box::new(|f: &mut DesktopFingerprint| f.control_protocol_version = 2),
            ),
            (
                "desktop image",
                Box::new(|f: &mut DesktopFingerprint| f.desktop_image = id("desktop.qcow2", 101)),
            ),
            ("guest agent", Box::new(|f: &mut DesktopFingerprint| f.agent_hash = "cccc".into())),
            ("guest bridge", Box::new(|f: &mut DesktopFingerprint| f.bridge_hash = "dddd".into())),
            ("qemu binary", Box::new(|f: &mut DesktopFingerprint| f.qemu_binary = id("qemu", 11))),
            (
                "qemu version",
                Box::new(|f: &mut DesktopFingerprint| f.qemu_version = "QEMU 12".into()),
            ),
            (
                "uefi firmware",
                Box::new(|f: &mut DesktopFingerprint| f.firmware = id("edk2.fd", 21)),
            ),
            ("guest memory", Box::new(|f: &mut DesktopFingerprint| f.memory_mb = 4096)),
            ("vcpu count", Box::new(|f: &mut DesktopFingerprint| f.cpus = 4)),
            ("machine type", Box::new(|f: &mut DesktopFingerprint| f.machine = "virt-9".into())),
            (
                "installed capabilities",
                Box::new(|f: &mut DesktopFingerprint| f.capabilities.clear()),
            ),
            (
                "device configuration",
                Box::new(|f: &mut DesktopFingerprint| f.devices = "other".into()),
            ),
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

#[cfg(test)]
mod tests {

    // ---- prepared-state validity -------------------------------------------
    //
    // An interrupted freeze used to leave `ready.json` advertising a guest
    // whose state file had already been deleted, and `doctor` believed it.

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A directory that looks like a finished freeze.
    fn prepared(name: &str, state_len: usize, caps: usize) -> Tmp {
        let d = std::env::temp_dir().join(format!("wq-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ready.state"), vec![7u8; state_len]).unwrap();
        for f in [
            "ready-disk.qcow2",
            "ready-vars.fd",
            "ready-mailbox.img",
            "ready-workspace.img",
            "ready-artifacts.img",
        ] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let mut cap_json = Vec::new();
        for i in 0..caps {
            std::fs::write(d.join(format!("ready-cap{i}.img")), b"x").unwrap();
            cap_json.push(serde_json::json!([
                format!("cap{i}"),
                { "path": "/x", "len": 1, "mtime_ns": 0 }
            ]));
        }
        let id = serde_json::json!({ "path": "/x", "len": 1, "mtime_ns": 0 });
        let meta = serde_json::json!({
            "fingerprint": {
                "winquick_version": "0.0.0", "protocol_version": PROTOCOL_VERSION,
                "base_image": id, "agent_hash": "0", "qemu_binary": id,
                "qemu_version": "id:0", "firmware": id, "memory_mb": 1024, "cpus": 4,
                "machine": "virt", "capabilities": cap_json, "devices": "d"
            },
            "created_unix": 0,
            "state_bytes": state_len,
        });
        std::fs::write(d.join("ready.json"), serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
        Tmp(d)
    }

    /// A freeze that finished is not a problem.
    #[test]
    fn a_complete_prepared_guest_reports_nothing_wrong() {
        let t = prepared("ok", 4096, 2);
        assert_eq!(structural_problem_in(&t.0), None);
    }

    /// No prepared guest at all is absence, not corruption. The caller says
    /// "not built yet"; it must not be reported as a fault.
    #[test]
    fn an_absent_prepared_guest_is_not_a_problem() {
        let d = std::env::temp_dir().join(format!("wq-state-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(structural_problem_in(&d), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The exact shape of the bug: `ready.json` survives, the state file it
    /// describes does not.
    #[test]
    fn a_missing_state_file_is_detected() {
        let t = prepared("gone", 4096, 0);
        std::fs::remove_file(t.0.join("ready.state")).unwrap();
        let why = structural_problem_in(&t.0).expect("must be reported");
        assert!(why.contains("ready.state"), "{why}");
        assert!(why.contains("missing"), "{why}");
    }

    /// A migration that stopped partway leaves fewer bytes than `ready.json`
    /// recorded.
    #[test]
    fn a_truncated_state_file_is_detected() {
        let t = prepared("short", 4096, 0);
        std::fs::write(t.0.join("ready.state"), vec![7u8; 100]).unwrap();
        let why = structural_problem_in(&t.0).expect("must be reported");
        assert!(why.contains("ready.state"), "{why}");
    }

    /// Any volume the guest needs, not just the state file.
    #[test]
    fn a_missing_volume_is_detected() {
        let t = prepared("vol", 4096, 0);
        std::fs::remove_file(t.0.join("ready-workspace.img")).unwrap();
        let why = structural_problem_in(&t.0).expect("must be reported");
        assert!(why.contains("ready-workspace.img"), "{why}");
    }

    /// A capability volume named by the metadata but absent on disk.
    #[test]
    fn a_missing_capability_volume_is_detected() {
        let t = prepared("cap", 4096, 2);
        std::fs::remove_file(t.0.join("ready-cap1.img")).unwrap();
        let why = structural_problem_in(&t.0).expect("must be reported");
        assert!(why.contains("ready-cap1.img"), "{why}");
    }

    #[test]
    fn unreadable_metadata_is_detected_rather_than_ignored() {
        let t = prepared("json", 4096, 0);
        std::fs::write(t.0.join("ready.json"), b"{ not json").unwrap();
        let why = structural_problem_in(&t.0).expect("must be reported");
        assert!(why.contains("ready.json"), "{why}");
    }

    /// The half-written file a killed migration leaves behind must never be
    /// mistaken for the published one.
    #[test]
    fn a_partial_state_file_is_not_the_published_one() {
        let t = prepared("part", 4096, 0);
        std::fs::write(t.0.join("ready.state.part"), vec![7u8; 12]).unwrap();
        assert_eq!(structural_problem_in(&t.0), None, "a leftover .part must not matter");
        std::fs::remove_file(t.0.join("ready.state")).unwrap();
        assert!(
            structural_problem_in(&t.0).is_some(),
            "a .part file must not stand in for the real state"
        );
    }
    use super::*;

    /// Evidence that a QEMU *can* restore outranks any amount of evidence that
    /// it sometimes does not. Where a prepared guest gets frozen is partly
    /// luck, so a run of silent ones says nothing about a machine that has
    /// already restored one and run a command on it. Both notes are read the
    /// same way, and both are about one backend rather than about any.
    #[test]
    fn a_note_is_about_the_backend_that_wrote_it() {
        let dir = std::env::temp_dir().join(format!("wq-note-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let note = dir.join("restore-works");

        assert!(!note_says(&note, "whpx|a"), "no note yet");
        write_note(&note, "whpx|a").unwrap();
        assert!(note_says(&note, "whpx|a"));
        assert!(
            !note_says(&note, "whpx|b"),
            "a note left by a different QEMU is not about this one"
        );

        // Writing the same thing twice is not an error and does not change it.
        write_note(&note, "whpx|a").unwrap();
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "whpx|a");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
