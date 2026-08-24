//! Everything that knows QEMU exists.
//!
//! QEMU is always a child process. WinQuick never links against it — that is a
//! licensing boundary (QEMU is GPLv2) as much as a design one.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub const MACHINE: &str = "virt";

pub struct Qemu {
    pub system: PathBuf,
    pub img: PathBuf,
}

impl Qemu {
    pub fn locate() -> Result<Self> {
        Ok(Self {
            system: which("qemu-system-aarch64")?,
            img: which("qemu-img")?,
        })
    }

    pub fn version(&self) -> Result<String> {
        let out = Command::new(&self.system).arg("--version").output()?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string())
    }

    /// Create a copy-on-write overlay. The base is opened read-only by QEMU and
    /// is never modified, so a run can do anything it likes to its own copy.
    pub fn create_overlay(&self, base: &Path, overlay: &Path) -> Result<()> {
        let st = Command::new(&self.img)
            .args(["create", "-q", "-f", "qcow2", "-b"])
            .arg(base)
            .args(["-F", "qcow2"])
            .arg(overlay)
            .status()
            .context("running qemu-img create")?;
        if !st.success() {
            return Err(anyhow!("qemu-img create failed"));
        }
        Ok(())
    }

    pub fn convert(&self, src: &Path, dst: &Path, format: &str) -> Result<()> {
        let st = Command::new(&self.img)
            .args(["convert", "-O", format])
            .arg(src)
            .arg(dst)
            .status()
            .context("running qemu-img convert")?;
        if !st.success() {
            return Err(anyhow!("qemu-img convert failed"));
        }
        Ok(())
    }
}

pub struct BootConfig<'a> {
    pub uefi_code: &'a Path,
    /// Must be writable. A read-only variable store stops Windows booting at
    /// all, silently, with a black framebuffer — see docs/research.md.
    pub uefi_vars: &'a Path,
    pub root_disk: &'a Path,
    pub mailbox: &'a Path,
    pub memory_mb: u32,
    pub cpus: u32,
    pub serial_log: &'a Path,
    pub qmp_socket: &'a Path,
    /// When set, restore RAM and device state from this file instead of booting.
    pub incoming: Option<&'a Path>,
}

/// Canonical description of the device topology, recorded in the ready-state
/// fingerprint. Migration state is only meaningful against the same machine.
pub fn device_signature(memory_mb: u32, cpus: u32) -> String {
    format!(
        "machine={MACHINE};accel=hvf;cpu=host;smp={cpus};mem={memory_mb};\
         nvme:root=wqroot;nvme:mbox=wqmbox;pflash:code,vars(rw);ramfb;display=none;rtc=localtime"
    )
}

impl Qemu {
    /// Spawn the VM headlessly. `-display none` means no window ever appears;
    /// `ramfb` is still present because Windows expects a display device.
    pub fn boot(&self, cfg: &BootConfig) -> Result<Child> {
        let mut c = Command::new(&self.system);
        c.args(["-M", MACHINE, "-accel", "hvf", "-cpu", "host"])
            .args(["-smp", &cfg.cpus.to_string()])
            .args(["-m", &cfg.memory_mb.to_string()])
            .arg("-drive")
            .arg(format!(
                "if=pflash,format=raw,readonly=on,file={}",
                cfg.uefi_code.display()
            ))
            .arg("-drive")
            .arg(format!("if=pflash,format=raw,file={}", cfg.uefi_vars.display()))
            .arg("-drive")
            .arg(format!(
                "if=none,id=root,file={},format=qcow2",
                cfg.root_disk.display()
            ))
            .args(["-device", "nvme,drive=root,serial=wqroot"])
            // writethrough so the guest's results are on the host's disk as soon
            // as the guest dismounts the volume
            .arg("-drive")
            .arg(format!(
                "if=none,id=mbox,file={},format=raw,cache=writethrough",
                cfg.mailbox.display()
            ))
            .args(["-device", "nvme,drive=mbox,serial=wqmbox"])
            .args(["-device", "ramfb", "-display", "none", "-vga", "none"])
            .args(["-rtc", "base=localtime", "-no-reboot"])
            .arg("-serial")
            .arg(format!("file:{}", cfg.serial_log.display()))
            .arg("-qmp")
            .arg(format!(
                "unix:{},server=on,wait=off",
                cfg.qmp_socket.display()
            ));
        if let Some(state) = cfg.incoming {
            c.arg("-incoming").arg(format!("file:{}", state.display()));
        }
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        c.spawn().context("spawning qemu-system-aarch64")
    }
}

pub fn which(bin: &str) -> Result<PathBuf> {
    let out = Command::new("/usr/bin/which").arg(bin).output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "{bin} not found on PATH. Install QEMU (e.g. `brew install qemu`)."
        ));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Copy-on-write clone where the filesystem supports it. On APFS this is
/// effectively free regardless of file size, which is what keeps per-run setup
/// in the tens of milliseconds.
pub fn clone_file(src: &Path, dst: &Path) -> Result<()> {
    let _ = std::fs::remove_file(dst);
    let st = Command::new("/bin/cp")
        .arg("-c")
        .arg(src)
        .arg(dst)
        .status()
        .context("running cp -c")?;
    if !st.success() {
        std::fs::copy(src, dst).with_context(|| {
            format!("copying {} to {}", src.display(), dst.display())
        })?;
    }
    Ok(())
}
