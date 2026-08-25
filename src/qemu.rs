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
    /// Capability volumes, attached writable in a deterministic order.
    pub capabilities: &'a [PathBuf],
    /// Workspace, artifact and package-cache volumes. All three are always
    /// attached so the device topology does not depend on what a given run asked
    /// for — topology is part of the prepared-guest fingerprint.
    pub workspace: &'a Path,
    pub artifacts: &'a Path,
    pub memory_mb: u32,
    pub cpus: u32,
    pub serial_log: &'a Path,
    pub qmp_socket: &'a Path,
    /// When set, restore RAM and device state from this file instead of booting.
    pub incoming: Option<&'a Path>,
}

/// Canonical description of the device topology, recorded in the ready-state
/// fingerprint. Migration state is only meaningful against the same machine.
pub fn device_signature(memory_mb: u32, cpus: u32, capability_count: usize) -> String {
    let caps: String = (0..capability_count)
        .map(|i| format!(";nvme:cap{i}=wqcap{i}"))
        .collect();
    format!(
        "machine={MACHINE};accel=hvf;cpu=host;smp={cpus};mem={memory_mb};\
         nvme:root=wqroot;nvme:mbox=wqmbox;nvme:work=wqwork;nvme:arts=wqarts{caps};pflash:code,vars(rw);ramfb;display=none;rtc=localtime"
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
            .args(["-device", "nvme,drive=mbox,serial=wqmbox"]);
        c.arg("-drive")
            .arg(format!(
                "if=none,id=work,file={},format=raw,cache=writethrough",
                cfg.workspace.display()
            ))
            .args(["-device", "nvme,drive=work,serial=wqwork"])
            .arg("-drive")
            .arg(format!(
                "if=none,id=arts,file={},format=raw,cache=writethrough",
                cfg.artifacts.display()
            ))
            .args(["-device", "nvme,drive=arts,serial=wqarts"]);
        for (i, cap) in cfg.capabilities.iter().enumerate() {
            // Writable on purpose: Windows writes when mounting a volume, and a
            // read-only NVMe makes those fail so no volume appears at all.
            c.arg("-drive")
                .arg(format!(
                    "if=none,id=cap{i},file={},format=raw,cache=writethrough",
                    cap.display()
                ))
                .args(["-device", &format!("nvme,drive=cap{i},serial=wqcap{i}")]);
        }
        c
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
    crate::helpers::which(bin).ok_or_else(|| {
        anyhow!("QEMU is not installed.\n\nInstall it with:\n    brew install qemu")
    })
}

/// Copy-on-write clone where the filesystem supports it. On APFS this is
/// effectively free regardless of file size, which is what keeps per-run setup
/// in the tens of milliseconds.
pub fn clone_file(src: &Path, dst: &Path) -> Result<()> {
    let _ = std::fs::remove_file(dst);
    // Captured, not inherited: a message from `cp` must never end up mixed into
    // the guest's stdout, which is the caller's actual result.
    let out = Command::new("/bin/cp")
        .arg("-c")
        .arg(src)
        .arg(dst)
        .output()
        .context("running cp -c")?;
    if !out.status.success() {
        std::fs::copy(src, dst).with_context(|| {
            format!(
                "copying {} to {} ({})",
                src.display(),
                dst.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })?;
    }
    Ok(())
}

/// A long-lived desktop guest.
///
/// Two differences from a `run` guest, both forced by needing a real desktop:
/// a VirtIO GPU instead of `ramfb`, because Validation OS has no driver for a
/// plain framebuffer; and USB keyboard and tablet, so synthetic input has real
/// devices to come from.
pub struct DesktopBoot<'a> {
    pub uefi_code: &'a Path,
    pub uefi_vars: &'a Path,
    pub root_disk: &'a Path,
    pub mailbox: &'a Path,
    /// Volume carrying the bridge and the application under test.
    pub files: &'a Path,
    /// Raw disk the session's control channel runs on. Deliberately has no
    /// partition table, so Windows never mounts or caches it.
    pub control: &'a Path,
    pub capabilities: &'a [PathBuf],
    pub memory_mb: u32,
    pub cpus: u32,
    pub serial_log: &'a Path,
    pub qmp_socket: &'a Path,
}

impl Qemu {
    /// Spawn a desktop guest that outlives this process.
    ///
    /// The child is deliberately detached: `winquick desktop start` returns as
    /// soon as the guest is ready, and every later verb finds it by pid.
    pub fn boot_desktop(&self, cfg: &DesktopBoot) -> Result<Child> {
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
            .arg("-drive")
            .arg(format!(
                "if=none,id=mbox,file={},format=raw,cache=writethrough",
                cfg.mailbox.display()
            ))
            .args(["-device", "nvme,drive=mbox,serial=wqmbox"])
            .arg("-drive")
            .arg(format!(
                "if=none,id=files,file={},format=raw,cache=writethrough",
                cfg.files.display()
            ))
            .args(["-device", "nvme,drive=files,serial=wqfiles"])
            // writethrough, like every other volume: the host writes through
            // its page cache and QEMU reads through the same one, so both sides
            // see each other's bytes. `cache=none` makes QEMU bypass that cache
            // and the two halves stop seeing each other entirely.
            .arg("-drive")
            .arg(format!(
                "if=none,id=ctl,file={},format=raw,cache=writethrough",
                cfg.control.display()
            ))
            .args(["-device", "nvme,drive=ctl,serial=wqctl"]);
        for (i, cap) in cfg.capabilities.iter().enumerate() {
            c.arg("-drive")
                .arg(format!(
                    "if=none,id=cap{i},file={},format=raw,cache=writethrough",
                    cap.display()
                ))
                .args(["-device", &format!("nvme,drive=cap{i},serial=wqcap{i}")]);
        }
        c.args(["-device", "qemu-xhci"])
            .args(["-device", "usb-kbd"])
            .args(["-device", "usb-tablet"])
            .args(["-device", "virtio-gpu-pci,id=wqgpu"])
            .args(["-display", "none", "-vga", "none"])
            .args(["-rtc", "base=localtime", "-no-reboot"])
            .arg("-serial")
            .arg(format!("file:{}", cfg.serial_log.display()))
            .arg("-qmp")
            .arg(format!(
                "unix:{},server=on,wait=off",
                cfg.qmp_socket.display()
            ));
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        c.spawn().context("spawning the desktop guest")
    }
}

/// A guest that services another Windows image.
///
/// The distinguishing part is `target`: a *copy* of the Windows disk attached
/// as an ordinary data device, which DISM writes into offline. It must be raw,
/// because the host rewrites its partition tables directly before and after.
pub struct ServicingBoot<'a> {
    pub uefi_code: &'a Path,
    pub uefi_vars: &'a Path,
    pub root_disk: &'a Path,
    pub mailbox: &'a Path,
    /// Volume holding DISM, the packages and the drivers.
    pub servicing: &'a Path,
    /// The image being serviced.
    pub target: &'a Path,
    pub serial_log: &'a Path,
}

impl Qemu {
    pub fn boot_servicing(&self, cfg: &ServicingBoot) -> Result<Child> {
        let mut c = Command::new(&self.system);
        c.args(["-M", MACHINE, "-accel", "hvf", "-cpu", "host"])
            .args(["-smp", "4", "-m", "3072"])
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
            .arg("-drive")
            .arg(format!(
                "if=none,id=mbox,file={},format=raw,cache=writethrough",
                cfg.mailbox.display()
            ))
            .args(["-device", "nvme,drive=mbox,serial=wqmbox"])
            .arg("-drive")
            .arg(format!(
                "if=none,id=svc,file={},format=raw,cache=writethrough",
                cfg.servicing.display()
            ))
            .args(["-device", "nvme,drive=svc,serial=wqsvc"])
            // writethrough so DISM's writes reach the host file as the guest
            // makes them, rather than at some point QEMU chooses
            .arg("-drive")
            .arg(format!(
                "if=none,id=tgt,file={},format=raw,cache=writethrough",
                cfg.target.display()
            ))
            .args(["-device", "nvme,drive=tgt,serial=wqtgt"])
            .args(["-device", "ramfb", "-display", "none", "-vga", "none"])
            .args(["-rtc", "base=localtime", "-no-reboot"])
            .arg("-serial")
            .arg(format!("file:{}", cfg.serial_log.display()));
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        c.spawn().context("spawning the servicing guest")
    }
}
