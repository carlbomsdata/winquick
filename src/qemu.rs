//! Everything that knows QEMU exists.
//!
//! QEMU is always a child process. WinQuick never links against it — that is a
//! licensing boundary (QEMU is GPLv2) as much as a design one.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub use crate::platform::MACHINE;

pub struct Qemu {
    pub system: PathBuf,
    pub img: PathBuf,
}

impl Qemu {
    pub fn locate() -> Result<Self> {
        Ok(Self {
            system: which(crate::platform::QEMU_SYSTEM)?,
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
    /// Print the command line before starting. A VM that will not boot is
    /// almost always diagnosed by reading the arguments it was given.
    pub verbose: bool,
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
        "{backend};smp={cpus};mem={memory_mb};\
         nvme:root=wqroot;nvme:mbox=wqmbox;nvme:work=wqwork;nvme:arts=wqarts{caps};pflash:code,vars(rw);ramfb;display=none;rtc=localtime",
        backend = crate::platform::backend_signature()
    )
}

impl Qemu {
    /// Spawn the VM headlessly. `-display none` means no window ever appears;
    /// `ramfb` is still present because Windows expects a display device.
    pub fn boot(&self, cfg: &BootConfig) -> Result<Child> {
        let mut c = Command::new(&self.system);
        c.args(["-M", MACHINE, "-accel", crate::platform::ACCEL, "-cpu", crate::platform::CPU_MODEL])
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
            .arg(qmp_arg(cfg.qmp_socket)?);
        if let Some(state) = cfg.incoming {
            c.arg("-incoming").arg(format!("file:{}", state.display()));
        }
        if cfg.verbose {
            eprintln!("winquick: {}", describe(&c));
        }
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        c.spawn().context("spawning QEMU")
    }
}

/// A command line as a person would have to type it, for `--verbose` and for
/// bug reports. Arguments containing spaces are quoted so the result can be
/// pasted back into a shell and reproduced.
fn describe(c: &Command) -> String {
    let quote = |s: &str| {
        if s.contains(' ') {
            format!("\"{s}\"")
        } else {
            s.to_string()
        }
    };
    let mut out = quote(&c.get_program().to_string_lossy());
    for a in c.get_args() {
        out.push(' ');
        out.push_str(&quote(&a.to_string_lossy()));
    }
    out
}

/// How QEMU should publish its monitor on this host.
///
/// macOS gets a Unix socket at `endpoint`, which QEMU creates. Windows has no
/// Unix sockets, so the monitor is a loopback TCP port instead and `endpoint`
/// becomes a small file naming it -- the same rendezvous point, spelled the
/// only way the platform allows. [`crate::hostfs::ControlStream`] reads
/// whichever of the two it finds.
#[cfg(unix)]
fn qmp_arg(endpoint: &Path) -> Result<String> {
    Ok(format!("unix:{},server=on,wait=off", endpoint.display()))
}

#[cfg(windows)]
fn qmp_arg(endpoint: &Path) -> Result<String> {
    // The port is chosen by binding one and immediately dropping it, so the
    // kernel picks something free. QEMU claims it a moment later. Nothing else
    // on the machine is likely to take it in between, and if it does, the boot
    // fails loudly rather than talking to the wrong process.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .context("reserving a port for QEMU's monitor")?
        .local_addr()
        .context("reserving a port for QEMU's monitor")?
        .port();
    std::fs::write(endpoint, format!("127.0.0.1:{port}"))
        .with_context(|| format!("writing {}", endpoint.display()))?;
    Ok(format!("tcp:127.0.0.1:{port},server=on,wait=off"))
}

pub fn which(bin: &str) -> Result<PathBuf> {
    crate::helpers::which(bin).ok_or_else(|| {
        if cfg!(target_os = "macos") {
            anyhow!("QEMU is not installed.\n\nInstall it with:\n    brew install qemu")
        } else {
            anyhow!(
                "QEMU is not installed, or {bin} is not on PATH.\n\n\
                 Install QEMU for Windows and make sure its directory is on PATH."
            )
        }
    })
}

/// Copy-on-write clone where the filesystem supports it.
///
/// On APFS this is effectively free regardless of file size, which is what
/// keeps per-run setup in the tens of milliseconds. NTFS has no equivalent --
/// block cloning on Windows needs ReFS -- so there the copy is real, and the
/// prepared-guest build is correspondingly slower. A plain copy is always the
/// fallback, so this is never a correctness question, only a speed one.
#[cfg(target_os = "macos")]
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

/// Copy a volume image, skipping the parts of it that are nothing.
///
/// The workspace and artifact volumes are two gigabytes each, and about a
/// quarter of one percent of that is real: a FAT boot sector, two allocation
/// tables and a nearly empty root directory. macOS gets the copy for free from
/// APFS cloning. Everywhere else WinQuick was moving 4.3 GB per run to deliver
/// ten megabytes, and that -- not the restore, which is under two hundred
/// milliseconds -- was where a "warm" run spent its time.
///
/// The destination is fresh and sparse, so a run of zeroes is already there;
/// writing it again would only allocate it.
#[cfg(not(target_os = "macos"))]
pub fn clone_file(src: &Path, dst: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let _ = std::fs::remove_file(dst);
    let mut r = std::fs::File::open(src)
        .with_context(|| format!("opening {}", src.display()))?;
    let len = r.metadata()?.len();
    let w = std::fs::File::create(dst)
        .with_context(|| format!("creating {}", dst.display()))?;
    crate::hostfs::set_sparse_len(&w, len)
        .with_context(|| format!("sizing {}", dst.display()))?;
    let mut w = w;

    const BLOCK: usize = 1 << 20;
    let mut buf = vec![0u8; BLOCK];

    for (start, length) in crate::hostfs::allocated_ranges(&r, len) {
        let mut off = start;
        let end = (start + length).min(len);
        r.seek(SeekFrom::Start(start))
            .with_context(|| format!("seeking {}", src.display()))?;
        while off < end {
            let want = ((end - off) as usize).min(BLOCK);
            // Short reads are ordinary, and a block that is only partly filled
            // would otherwise be judged on stale bytes from the last one.
            let mut filled = 0;
            while filled < want {
                match r.read(&mut buf[filled..want]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("reading {}", src.display()))
                    }
                }
            }
            if filled == 0 {
                break;
            }
            // A filesystem that could not name its holes hands back the whole
            // file, so still skip what is plainly nothing.
            if buf[..filled].iter().any(|b| *b != 0) {
                w.seek(SeekFrom::Start(off))?;
                w.write_all(&buf[..filled])
                    .with_context(|| format!("writing {}", dst.display()))?;
            }
            off += filled as u64;
        }
    }
    w.flush()?;
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
    /// Volume carrying the bridge. Frozen into the prepared state and never
    /// rewritten, because `wqui.exe` is executing from it.
    pub bridge: &'a Path,
    /// Volume carrying the application under test. Refilled per session, which
    /// is why it cannot share a volume with the bridge: refreshing the guest's
    /// view of it means dismounting it.
    pub app: &'a Path,
    /// Raw disk the session's control channel runs on. Deliberately has no
    /// partition table, so Windows never mounts or caches it.
    pub control: &'a Path,
    pub capabilities: &'a [PathBuf],
    pub memory_mb: u32,
    pub cpus: u32,
    pub serial_log: &'a Path,
    pub qmp_socket: &'a Path,
    /// When set, restore RAM and devices from this file instead of booting.
    pub incoming: Option<&'a Path>,
}

/// Canonical description of the desktop topology, recorded in the prepared
/// state's fingerprint. Migration state is only meaningful against the same
/// machine it came from.
pub fn desktop_device_signature(memory_mb: u32, cpus: u32, capability_count: usize) -> String {
    let caps: String = (0..capability_count)
        .map(|i| format!(";nvme:cap{i}=wqcap{i}"))
        .collect();
    format!(
        "{backend};smp={cpus};mem={memory_mb};\
         nvme:root=wqroot;nvme:mbox=wqmbox;nvme:bridge=wqbridge;nvme:app=wqapp;\
         nvme:ctl=wqctl{caps};pflash:code,vars(rw);xhci+kbd+tablet;virtio-gpu-pci;\
         display=none;rtc=localtime",
        backend = crate::platform::backend_signature()
    )
}

impl Qemu {
    /// Spawn a desktop guest that outlives this process.
    ///
    /// The child is deliberately detached: `winquick desktop start` returns as
    /// soon as the guest is ready, and every later verb finds it by pid.
    pub fn boot_desktop(&self, cfg: &DesktopBoot) -> Result<Child> {
        let mut c = Command::new(&self.system);
        c.args(["-M", MACHINE, "-accel", crate::platform::ACCEL, "-cpu", crate::platform::CPU_MODEL])
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
                "if=none,id=bridge,file={},format=raw,cache=writethrough",
                cfg.bridge.display()
            ))
            .args(["-device", "nvme,drive=bridge,serial=wqbridge"])
            .arg("-drive")
            .arg(format!(
                "if=none,id=app,file={},format=raw,cache=writethrough",
                cfg.app.display()
            ))
            .args(["-device", "nvme,drive=app,serial=wqapp"])
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
            .arg(qmp_arg(cfg.qmp_socket)?);
        if let Some(state) = cfg.incoming {
            c.arg("-incoming").arg(format!("file:{}", state.display()));
        }
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
        c.args(["-M", MACHINE, "-accel", crate::platform::ACCEL, "-cpu", crate::platform::CPU_MODEL])
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A clone is a copy. The whole point of the fast paths -- APFS cloning on
    /// macOS, skipping runs of zeroes everywhere else -- is that they are not
    /// visible in the result, and a volume image that came back subtly
    /// different would corrupt a guest filesystem rather than fail loudly.
    #[test]
    fn a_cloned_volume_is_byte_for_byte_its_source() {
        let dir = std::env::temp_dir().join(format!("wq-clone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.img");
        let dst = dir.join("dst.img");

        // Shaped like the volumes this actually copies: a little content at the
        // front, a long run of nothing, a little more at the end, and a tail
        // that is not a whole block.
        let mut content = vec![0u8; 5 << 20];
        for (i, b) in content[..4096].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        content[(3 << 20) + 17] = 0xAB;
        let n = content.len();
        content[n - 3..].copy_from_slice(&[1, 2, 3]);
        std::fs::write(&src, &content).unwrap();

        clone_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), content.len() as u64);

        // Cloning over an existing file replaces it rather than merging into
        // it, which is what every caller assumes.
        std::fs::write(&dst, vec![0xFFu8; 9 << 20]).unwrap();
        clone_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), content);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
