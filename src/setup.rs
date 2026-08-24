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

use crate::{capability, helpers, paths, qemu, state};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const AGENT: &str = include_str!("../guest/agent.cmd");

/// Where Microsoft publishes the image, and what it is called.
pub const VALIDATION_OS_URL: &str = "https://aka.ms/DownloadValidationOS_arm64";
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

    let work = paths::root()?.join("work");
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
    let dev = attach(&raw)?;
    let ntfs = format!("{dev}s4");
    let result = (|| -> Result<()> {
        let agent = work.join("agent.cmd");
        std::fs::write(&agent, AGENT.replace('\n', "\r\n"))?;
        run_ok(
            Command::new(&tools.ntfscp)
                .arg(&ntfs)
                .arg(&agent)
                .arg("/Windows/System32/wqagent.cmd"),
            "writing the agent into the image",
        )?;

        println!("  [3/4] configuring the guest");
        let hive = work.join("SOFTWARE");
        let out = Command::new(&tools.ntfscat)
            .arg(&ntfs)
            .arg("/Windows/System32/config/SOFTWARE")
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
            Command::new(&tools.ntfscp)
                .arg(&ntfs)
                .arg(&hive)
                .arg("/Windows/System32/config/SOFTWARE"),
            "writing the guest registry back",
        )?;
        Ok(())
    })();
    detach(&dev);
    result?;

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

    println!(
        "\nWindows runtime installed ({}).",
        helpers::human(helpers::allocated(&base))
    );

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
            memory_mb: 1024,
            cpus: 4,
            timeout: std::time::Duration::from_secs(300),
            verbose: false,
            force_cold: false,
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
                return mount_iso(&iso);
            }
            bail!("no Validation OS image found in {}", s.display());
        }
        if !s.exists() {
            bail!("{} does not exist", s.display());
        }
        return if s.extension().map(|e| e.eq_ignore_ascii_case("iso")).unwrap_or(false) {
            mount_iso(s)
        } else {
            Ok(s.clone())
        };
    }

    let cache = paths::cache()?;
    std::fs::create_dir_all(&cache)?;
    let cached = cache.join("validationos-arm64.iso");
    if cached.exists() {
        println!("Using the Validation OS image already downloaded to");
        println!("  {}", cached.display());
        return mount_iso(&cached);
    }
    // Somewhere obvious the user may have put it.
    for dir in [dirs_download(), Some(PathBuf::from("."))].into_iter().flatten() {
        if let Some(iso) = newest_iso(&dir) {
            println!("Found a Validation OS image at");
            println!("  {}", iso.display());
            return mount_iso(&iso);
        }
    }

    if !opts.accept_microsoft_terms {
        bail!("{}", acquisition_message(&cached));
    }

    println!("Downloading Microsoft Validation OS for ARM64 (about 2.4 GB)...");
    println!("  from {VALIDATION_OS_URL}");
    let tmp = cache.join("validationos-arm64.iso.part");
    let st = Command::new("/usr/bin/curl")
        .args(["-fL", "--progress-bar", "-C", "-", "-o"])
        .arg(&tmp)
        .arg(VALIDATION_OS_URL)
        .status()
        .context("running curl")?;
    if !st.success() {
        bail!("download failed. Re-run to resume, or download it yourself:\n  {VALIDATION_OS_URL}");
    }
    std::fs::rename(&tmp, &cached)?;
    mount_iso(&cached)
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
         Either way the image and everything built from it stay on this Mac.\n\
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

fn mount_iso(iso: &Path) -> Result<PathBuf> {
    if let Some(existing) = existing_mount(iso) {
        let v = existing.join("ValidationOS.vhdx");
        if v.exists() {
            return Ok(v);
        }
    }
    let mnt = paths::root()?.join("mnt");
    std::fs::create_dir_all(&mnt)?;
    let v = mnt.join("ValidationOS.vhdx");
    if v.exists() {
        return Ok(v);
    }
    run_ok(
        Command::new("/usr/bin/hdiutil")
            .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
            .arg(&mnt)
            .arg(iso),
        "opening the Microsoft image",
    )?;
    if !v.exists() {
        let _ = Command::new("/usr/bin/hdiutil").args(["detach"]).arg(&mnt).output();
        bail!(
            "{} does not look like a Validation OS ARM64 image.\n\
             Expected ValidationOS.vhdx inside it. Download the ARM64 edition from:\n  {VALIDATION_OS_URL}",
            iso.display()
        );
    }
    Ok(v)
}

/// Ask hdiutil where, if anywhere, this image is already attached.
fn existing_mount(image: &Path) -> Option<PathBuf> {
    let out = Command::new("/usr/bin/hdiutil").arg("info").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let want = image.canonicalize().ok()?;
    let mut matched = false;
    for line in text.lines() {
        if line.starts_with("image-path") {
            matched = line
                .split_once(':')
                .map(|(_, p)| Path::new(p.trim()) == want)
                .unwrap_or(false);
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
    if let Ok(mnt) = paths::root().map(|r| r.join("mnt")) {
        if mnt.exists() {
            let _ = Command::new("/usr/bin/hdiutil").args(["detach"]).arg(&mnt).output();
            let _ = std::fs::remove_dir(&mnt);
        }
    }
}

fn attach(raw: &Path) -> Result<String> {
    let out = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-imagekey", "diskimage-class=CRawDiskImage", "-nomount"])
        .arg(raw)
        .output()
        .context("opening the runtime image")?;
    if !out.status.success() {
        bail!("could not open the runtime image: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let dev = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("could not open the runtime image"))?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(dev)
}

fn detach(dev: &str) {
    let _ = Command::new("/usr/bin/hdiutil").args(["detach", dev]).output();
}

fn run_ok(c: &mut Command, what: &str) -> Result<()> {
    let out = c.output().with_context(|| format!("{what}"))?;
    if !out.status.success() {
        bail!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
