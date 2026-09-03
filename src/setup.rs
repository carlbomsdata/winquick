//! `winquick setup` — turn a Microsoft-supplied image into a WinQuick runtime.
//!
//! WinQuick ships no Microsoft software. You download Microsoft Validation OS
//! yourself and accept Microsoft's licence; this step only transforms that image,
//! locally, into something bootable and controllable. The result lives under
//! `~/.winquick` and must not be redistributed — see docs/licensing.md.
//!
//! The transformation is deliberately tiny: put the agent script into the image
//! and point cmd.exe's AutoRun at it. Two writes, nothing else. No drivers are
//! injected, no packages added, no firmware touched.

use crate::{capability, gpt, helpers, paths, platform, qemu, state};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const AGENT: &str = include_str!("../guest/agent.cmd");

/// Where the agent and the registry hive live inside the Windows volume.
///
/// No leading slash: the native Windows build of `ntfscp` rejects that form,
/// while both builds accept this one. Forward slashes work on both.
const GUEST_AGENT: &str = "Windows/System32/wqagent.cmd";
const GUEST_SOFTWARE: &str = "Windows/System32/config/SOFTWARE";

/// Where Microsoft publishes the image for this host's guest architecture.
///
/// The guest architecture follows the host: an ARM64 Mac runs an ARM64 guest,
/// an x86_64 PC runs an x64 one. Emulating the other way would throw away the
/// hardware acceleration the whole product depends on.
pub const VALIDATION_OS_URL: &str = if cfg!(target_arch = "aarch64") {
    "https://aka.ms/DownloadValidationOS_arm64"
} else {
    "https://aka.ms/DownloadValidationOS"
};
pub const VALIDATION_OS_PAGE: &str =
    "https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/validation-os-overview";

pub struct Options {
    /// Explicit path to the ISO, its VHDX, or a directory holding one.
    pub from: Option<PathBuf>,
    pub force: bool,
    /// Capabilities to install once the base runtime is ready.
    pub with: Vec<String>,
    /// Download the image without prompting, having accepted Microsoft's terms.
    pub accept_microsoft_terms: bool,
    pub verbose: bool,
}

pub fn setup(opts: &Options) -> Result<()> {
    let base = paths::base_image()?;

    if base.exists() && !opts.force {
        println!("A Windows runtime is already installed at");
        println!("  {}", base.display());
        println!();
        println!("Add optional tools with `winquick capability install <name>`,");
        println!("or rebuild from scratch with `winquick setup --force`.");
        return install_capabilities(&opts.with, opts.verbose);
    }

    // Everything setup needs, checked before anything is downloaded or written.
    let tools = helpers::setup_tools()?;
    let q = qemu::Qemu::locate()?;
    helpers::uefi_firmware()
        .ok_or_else(|| anyhow::anyhow!("QEMU is installed but its UEFI firmware is missing.\nReinstall QEMU: brew reinstall qemu"))?;

    let vhdx = acquire_image(opts)?;

    let work = paths::work()?;
    std::fs::create_dir_all(&work)?;
    std::fs::create_dir_all(base.parent().unwrap())?;
    // Build into a temporary file and move it into place at the end, so an
    // interrupted setup never leaves a half-written runtime behind.
    let raw = work.join("build.raw");
    let staged = work.join("base.partial.qcow2");
    let _ = std::fs::remove_file(&staged);

    println!("Building the Windows runtime. This takes a minute and happens once.");
    println!("  [1/4] expanding the image");
    q.convert(&vhdx, &raw, "raw")?;

    println!("  [2/4] installing the WinQuick agent");
    let volume = Volume::open(&raw)?;
    let agent = work.join("agent.cmd");
    std::fs::write(&agent, AGENT.replace('\n', "\r\n"))?;
    run_ok(
        volume.tool(&tools.ntfscp).arg(&agent).arg(GUEST_AGENT),
        "writing the agent into the image",
    )?;

    println!("  [3/4] configuring the guest");
    let hive = work.join("SOFTWARE");
    let out = volume
        .tool(&tools.ntfscat)
        .arg(GUEST_SOFTWARE)
        .output()
        .context("reading the guest registry")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("could not read the registry hive out of the image");
    }
    std::fs::write(&hive, &out.stdout)?;

    let script = "cd \\Microsoft\\Command Processor\n\
                  setval 1\n\
                  AutoRun\n\
                  string:call C:\\Windows\\System32\\wqagent.cmd\n\
                  commit\n\
                  close\n";
    let sf = work.join("hive.hvx");
    std::fs::write(&sf, script)?;
    run_ok(
        // hivexsh wants the script before the hive.
        Command::new(&tools.hivexsh).arg("-w").arg("-f").arg(&sf).arg(&hive),
        "editing the guest registry",
    )?;
    run_ok(
        volume.tool(&tools.ntfscp).arg(&hive).arg(GUEST_SOFTWARE),
        "writing the guest registry back",
    )?;

    println!("  [4/4] packing the runtime");
    q.convert(&raw, &staged, "qcow2")?;
    let _ = std::fs::remove_file(&raw);
    state::write_base_meta(&staged, AGENT)?;
    // Atomic-ish handover: the runtime only becomes visible once it is complete.
    let _ = std::fs::remove_file(&base);
    std::fs::rename(&staged, &base).context("installing the runtime")?;
    if let (Ok(a), Ok(b)) = (state::base_meta_path(&staged), state::base_meta_path(&base)) {
        let _ = std::fs::rename(a, b);
    }
    // The runtime changed, so any frozen guest from a previous install is stale.
    state::discard()?;
    let _ = std::fs::remove_dir_all(&work);

    println!("\nWindows runtime installed ({}).", helpers::human(helpers::allocated(&base)));

    install_capabilities(&opts.with, opts.verbose)?;
    smoke_test()
}

fn install_capabilities(names: &[String], verbose: bool) -> Result<()> {
    for name in names {
        println!();
        capability::install(name, None, verbose)?;
    }
    if !names.is_empty() {
        state::discard()?;
    }
    Ok(())
}

/// Never claim success without proving it: boot Windows and run a real command.
fn smoke_test() -> Result<()> {
    println!("\nTesting the runtime...");
    let out = crate::runner::run_capture(
        "cmd /c ver",
        &crate::runner::Options {
            // The defaults, deliberately: this run builds the prepared guest,
            // and a state built at some other size is one the next real run
            // would have to throw away and rebuild. Hardcoding four processors
            // also asked for more than a Windows host supports on this path.
            memory_mb: crate::runner::DEFAULT_MEMORY_MB,
            cpus: crate::runner::DEFAULT_CPUS,
            timeout: std::time::Duration::from_secs(300),
            verbose: false,
            force_cold: false,
            force_warm: false,
            workspace: None,
            artifacts: Vec::new(),
            artifacts_dir: crate::artifact::default_dest(),
            artifact_overwrite: false,
        },
    );
    match out {
        Ok(o) if o.exit_code == 0 && String::from_utf8_lossy(&o.stdout).contains("Windows") => {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if !line.trim().is_empty() {
                    println!("  {}", line.trim());
                }
            }
            println!("\nReady. Try:  winquick run -- cmd /c ver");
            Ok(())
        }
        Ok(o) => bail!(
            "the runtime was built but Windows did not respond as expected \
             (exit {}).\nRun `winquick doctor` for details.",
            o.exit_code
        ),
        Err(e) => bail!("the runtime was built but Windows failed to start: {e:#}"),
    }
}

/// Find the Microsoft image, guiding the user through getting it if necessary.
fn acquire_image(opts: &Options) -> Result<PathBuf> {
    if let Some(s) = &opts.from {
        if s.is_dir() {
            let v = s.join("ValidationOS.vhdx");
            if v.exists() {
                return Ok(v);
            }
            // A directory of downloads is a reasonable thing to point at.
            if let Some(iso) = newest_iso(s) {
                return extract_vhdx(&iso);
            }
            bail!("no Validation OS image found in {}", s.display());
        }
        if !s.exists() {
            bail!("{} does not exist", s.display());
        }
        return if s.extension().map(|e| e.eq_ignore_ascii_case("iso")).unwrap_or(false) {
            extract_vhdx(s)
        } else {
            Ok(s.clone())
        };
    }

    let cache = paths::cache()?;
    std::fs::create_dir_all(&cache)?;
    let cached = cache.join(format!("validationos-{}.iso", platform::GUEST_ARCH));
    if cached.exists() {
        println!("Using the Validation OS image already downloaded to");
        println!("  {}", cached.display());
        return extract_vhdx(&cached);
    }
    // Somewhere obvious the user may have put it.
    for dir in [dirs_download(), Some(PathBuf::from("."))].into_iter().flatten() {
        if let Some(iso) = newest_iso(&dir) {
            println!("Found a Validation OS image at");
            println!("  {}", iso.display());
            return extract_vhdx(&iso);
        }
    }

    if !opts.accept_microsoft_terms {
        bail!("{}", acquisition_message(&cached));
    }

    println!("Downloading Microsoft Validation OS for {} (about 2.4 GB)...", platform::GUEST_ARCH);
    println!("  from {VALIDATION_OS_URL}");
    let tmp = cache.join(format!("validationos-{}.iso.part", platform::GUEST_ARCH));
    let st = helpers::program("curl")
        // `--proto`: this download has no pinned checksum -- Microsoft revises
        // the image in place -- so HTTPS all the way through, including across
        // the aka.ms redirect, is the only integrity guarantee there is.
        .args([
            "-fL",
            "--progress-bar",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "-C",
            "-",
            "-o",
        ])
        .arg(&tmp)
        .arg(VALIDATION_OS_URL)
        .status()
        .context("running curl")?;
    if !st.success() {
        bail!("download failed. Re-run to resume, or download it yourself:\n  {VALIDATION_OS_URL}");
    }
    std::fs::rename(&tmp, &cached)?;
    extract_vhdx(&cached)
}

fn acquisition_message(cached: &Path) -> String {
    format!(
        "WinQuick needs Microsoft's Windows validation runtime, which Microsoft\n\
         distributes under its own licence. WinQuick cannot ship it for you.\n\n\
         Two ways to get it:\n\n\
         1. Let WinQuick download it, if you accept Microsoft's licence terms:\n\
         \x20      winquick setup --accept-microsoft-terms\n\n\
         2. Download it yourself (about 2.4 GB) and point WinQuick at the file:\n\
         \x20      {VALIDATION_OS_URL}\n\
         \x20      winquick setup --from ~/Downloads/<the-downloaded>.iso\n\n\
         Either way the image and everything built from it stay on this machine.\n\
         Microsoft's terms and background:\n\
         \x20  {VALIDATION_OS_PAGE}\n\n\
         If you download it manually you can also just save it as:\n\
         \x20  {}",
        cached.display()
    )
}

fn dirs_download() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads"))
}

/// Newest file in `dir` that looks like a Validation OS ISO.
fn newest_iso(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let name = p.file_name()?.to_string_lossy().to_lowercase();
        if name.ends_with(".iso") && name.contains("validationos") {
            let t = e.metadata().ok()?.modified().ok()?;
            if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                best = Some((t, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Get `ValidationOS.vhdx` out of Microsoft's ISO.
///
/// Read directly rather than mounted. The media is a UDF disc, and mounting one
/// means `hdiutil` on macOS and `Mount-DiskImage` on Windows -- the latter needs
/// elevation and is blocked outright by some endpoint security software. The
/// same reader works on both hosts, needs no privileges, and cannot leave a
/// mount behind for the next run to trip over.
///
/// The extracted copy is kept: it is about a gigabyte, and `setup --force`
/// should not have to read 2.4 GB of ISO again to get it.
fn extract_vhdx(iso: &Path) -> Result<PathBuf> {
    const NAME: &str = "ValidationOS.vhdx";
    let out = paths::cache()?.join(format!("ValidationOS-{}.vhdx", platform::GUEST_ARCH));
    if out.exists() {
        return Ok(out);
    }
    std::fs::create_dir_all(out.parent().unwrap())?;

    println!("  reading {NAME} from the Microsoft image");
    // Written beside the target and renamed, so an interrupted extract cannot
    // be mistaken for a finished one next time.
    let partial = out.with_extension("vhdx.partial");
    let _ = std::fs::remove_file(&partial);
    let written = crate::udf::extract_file(iso, NAME, &partial).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        anyhow::anyhow!(
            "{e}\n\nThis should be the Validation OS {} edition; download it from:\n  {VALIDATION_OS_URL}",
            platform::GUEST_ARCH
        )
    })?;
    if written == 0 {
        let _ = std::fs::remove_file(&partial);
        bail!("{} in {} is empty", NAME, iso.display());
    }
    std::fs::rename(&partial, &out)?;
    Ok(out)
}

/// Ask hdiutil where, if anywhere, this image is already attached.
fn existing_mount(image: &Path) -> Option<PathBuf> {
    let out = Command::new("/usr/bin/hdiutil").arg("info").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let want = image.canonicalize().ok()?;
    let mut matched = false;
    for line in text.lines() {
        if line.starts_with("image-path") {
            matched =
                line.split_once(':').map(|(_, p)| Path::new(p.trim()) == want).unwrap_or(false);
        } else if matched {
            if let Some(mp) = line.split_whitespace().last() {
                if mp.starts_with('/') && Path::new(mp).is_dir() {
                    return Some(PathBuf::from(mp));
                }
            }
        }
    }
    None
}

/// Release anything setup mounted, so an interrupted run leaves nothing attached.
pub fn release_mounts() {
    let Ok(mnt) = paths::root().map(|r| r.join("mnt")) else { return };
    if !mnt.exists() {
        return;
    }
    // `setup` mounts directly at mnt/; the generic mounter uses a subdirectory
    // per image. Detach whichever exist.
    if let Ok(entries) = std::fs::read_dir(&mnt) {
        for sub in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            let _ = Command::new("/usr/bin/hdiutil").args(["detach"]).arg(&sub).output();
            let _ = std::fs::remove_dir(&sub);
        }
    }
    let _ = Command::new("/usr/bin/hdiutil").args(["detach"]).arg(&mnt).output();
    let _ = std::fs::remove_dir(&mnt);
}

/// The Windows volume inside a disk image, addressed without mounting it.
///
/// `ntfscp` and `ntfscat` normally want a partition device node. Handing them
/// the image file and the offset the partition starts at does the same job and
/// is the only route that works on both hosts: macOS can produce a node with
/// `hdiutil attach -nomount`, but the Windows equivalent needs elevation and a
/// virtual-disk driver, and endpoint security software blocks it in practice.
/// Nothing here needs privileges, and nothing outside the file is touched.
struct Volume {
    image: PathBuf,
    offset: u64,
}

impl Volume {
    fn open(image: &Path) -> Result<Self> {
        Ok(Self { image: image.to_path_buf(), offset: gpt::windows_volume_offset(image)? })
    }

    /// One of the ntfs helpers, already pointed at this volume.
    fn tool(&self, exe: &Path) -> Command {
        let mut c = Command::new(exe);
        c.env("NTFS_IMAGE_OFFSET", self.offset.to_string()).arg(&self.image);
        c
    }
}

fn run_ok(c: &mut Command, what: &str) -> Result<()> {
    let out = c.output().with_context(|| what.to_string())?;
    if !out.status.success() {
        bail!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Mount the Microsoft media and return its root, for callers that need the
/// whole ISO rather than just the disk image inside it.
///
/// Building the desktop image needs DISM and the optional package CABs, and
/// both live beside the VHDX on the ISO — a bare `ValidationOS.vhdx` is not
/// enough.
pub fn mount_microsoft_image(from: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = from {
        if p.is_dir() {
            return Ok(p.to_path_buf());
        }
        return mount_iso_at(p);
    }
    let cached = paths::cache()?.join(format!("validationos-{}.iso", platform::GUEST_ARCH));
    if cached.exists() {
        return mount_iso_at(&cached);
    }
    for dir in [dirs_download(), Some(PathBuf::from("."))].into_iter().flatten() {
        if let Some(iso) = newest_iso(&dir) {
            return mount_iso_at(&iso);
        }
    }
    bail!(
        "cannot find the Validation OS ISO.\n\n\
         The desktop capability needs the full ISO, which carries DISM and the\n\
         optional packages. Point WinQuick at it with:\n    \
         winquick capability install desktop --from /path/to/validationos.iso"
    )
}

/// Attach any ISO read-only and return its mount point.
///
/// Unlike [`extract_vhdx`], this makes no assumption about what is inside, so it
/// works for the virtio-win media as well as Microsoft's. An image already
/// attached — by an earlier WinQuick command, or by the user — is reused rather
/// than attached a second time, which the kernel refuses as busy.
pub fn mount_iso_at(iso: &Path) -> Result<PathBuf> {
    if let Some(existing) = existing_mount(iso) {
        return Ok(existing);
    }
    let stem = iso
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image")
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.', "_");
    let mnt = paths::root()?.join("mnt").join(stem);
    std::fs::create_dir_all(&mnt)?;
    if std::fs::read_dir(&mnt).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Ok(mnt);
    }
    // macOS can attach the image and read it in place, which is free. Nothing
    // else can without root, so everywhere else the two trees the desktop
    // capability needs are copied out of the image with WinQuick's own UDF
    // reader -- the same one `setup` already uses to lift the VHDX out.
    if cfg!(target_os = "macos") {
        run_ok(
            Command::new("/usr/bin/hdiutil")
                .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
                .arg(&mnt)
                .arg(iso),
            &format!("opening {}", iso.display()),
        )?;
        return Ok(mnt);
    }
    extract_media_trees(iso, &mnt)?;
    Ok(mnt)
}

/// Copy the parts of the Microsoft media the desktop build reads.
///
/// Not the whole image: it is 2.3 GB and the build wants two directories out
/// of it. Extracting only those keeps this within a few hundred megabytes and
/// a few seconds.
fn extract_media_trees(iso: &Path, dest: &Path) -> Result<()> {
    // Two discs, two filesystems. Microsoft's Validation OS media is UDF; Red
    // Hat's virtio-win disc, which the desktop capability takes drivers from,
    // is plain ISO 9660. Try each, and only complain if neither reads.
    if let Ok(mut v) = crate::udf::Volume::open(iso) {
        let mut got = false;
        for want in ["GenImage", "cabs"] {
            if let Some(e) = v.find(want)? {
                if e.is_dir {
                    v.extract_tree(&e, &dest.join(want))
                        .with_context(|| format!("extracting {want} from {}", iso.display()))?;
                    got = true;
                }
            }
        }
        if got {
            return Ok(());
        }
    }

    let mut i = crate::iso9660::Image::open(iso)
        .with_context(|| format!("{} is neither UDF nor ISO 9660", iso.display()))?;
    // The virtio disc is drivers for every Windows there has ever been; the
    // caller picks two out of it, so the top-level directories are all that
    // need to exist on disk.
    for e in i.root()? {
        if e.is_dir {
            i.extract_tree(&e, &dest.join(&e.name))
                .with_context(|| format!("extracting {} from {}", e.name, iso.display()))?;
        }
    }
    Ok(())
}
