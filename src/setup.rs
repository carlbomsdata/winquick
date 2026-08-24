//! `winquick setup` — turn a Microsoft-supplied image into a WinQuick runtime.
//!
//! WinQuick ships no Microsoft software. The user downloads Validation OS ARM64
//! from Microsoft and accepts Microsoft's licence themselves; this step only
//! transforms that image, locally, into something bootable and controllable.
//! The result stays under `~/.winquick` and must not be redistributed.
//!
//! The transformation is deliberately tiny: drop the agent script into the
//! image and point cmd.exe's AutoRun at it. Two writes, nothing else.

use crate::{paths, qemu};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const AGENT: &str = include_str!("../guest/agent.cmd");

const DOWNLOAD_URL: &str = "https://aka.ms/DownloadValidationOS_arm64";

/// External tools `setup` shells out to. See docs/research.md for why these are
/// still external — none of them has a usable macOS 26 Homebrew bottle for the
/// NTFS side, and reimplementing an NTFS writer is not a v0.1 problem.
struct Tools {
    ntfscp: PathBuf,
    hivexsh: PathBuf,
}

fn find_tools() -> Result<Tools> {
    let ntfscp = std::env::var_os("WINQUICK_NTFSCP")
        .map(PathBuf::from)
        .or_else(|| qemu::which("ntfscp").ok())
        .ok_or_else(|| {
            anyhow!(
                "ntfscp not found.\n\n\
                 `winquick setup` needs ntfsprogs to write into the Windows image.\n\
                 Homebrew's ntfs-3g formula is Linux-only, so build it from source:\n\n  \
                 curl -LO https://tuxera.com/opensource/ntfs-3g_ntfsprogs-2022.10.3.tgz\n  \
                 tar xzf ntfs-3g_ntfsprogs-2022.10.3.tgz && cd ntfs-3g_ntfsprogs-2022.10.3\n  \
                 ./configure --disable-ntfs-3g --enable-ntfsprogs --disable-plugins \\\n    \
                   --without-uuid --without-hd && make\n\n\
                 then point WINQUICK_NTFSCP at ntfsprogs/ntfscp."
            )
        })?;
    let hivexsh = qemu::which("hivexsh")
        .map_err(|_| anyhow!("hivexsh not found. Install it with `brew install hivex`."))?;
    Ok(Tools { ntfscp, hivexsh })
}

pub fn setup(source: Option<PathBuf>, force: bool) -> Result<()> {
    let base = paths::base_image()?;
    if base.exists() && !force {
        println!("Runtime already present at {}", base.display());
        println!("Use `winquick setup --force` to rebuild it.");
        return Ok(());
    }

    let vhdx = locate_vhdx(source)?;
    let tools = find_tools()?;
    let q = qemu::Qemu::locate()?;

    let work = paths::root()?.join("work");
    std::fs::create_dir_all(&work)?;
    std::fs::create_dir_all(base.parent().unwrap())?;
    let raw = work.join("build.raw");

    println!("[1/5] expanding {}", vhdx.display());
    q.convert(&vhdx, &raw, "raw")?;

    println!("[2/5] attaching image");
    let dev = attach(&raw)?;
    let ntfs = format!("{dev}s4");
    let result = (|| -> Result<()> {
        println!("[3/5] installing guest agent");
        let agent = work.join("agent.cmd");
        std::fs::write(&agent, AGENT.replace('\n', "\r\n"))?;
        run_ok(
            Command::new(&tools.ntfscp)
                .arg(&ntfs)
                .arg(&agent)
                .arg("/Windows/System32/wqagent.cmd"),
            "ntfscp agent",
        )?;

        println!("[4/5] pointing cmd.exe AutoRun at the agent");
        let hive = work.join("SOFTWARE");
        let cat = tools.ntfscp.with_file_name("ntfscat");
        let out = Command::new(&cat)
            .arg(&ntfs)
            .arg("/Windows/System32/config/SOFTWARE")
            .output()
            .context("running ntfscat")?;
        if !out.status.success() || out.stdout.is_empty() {
            bail!("could not read the SOFTWARE registry hive out of the image");
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
            // hivexsh wants the script before the hive: `hivexsh -w -f <script> <hive>`
            Command::new(&tools.hivexsh)
                .arg("-w")
                .arg("-f")
                .arg(&sf)
                .arg(&hive),
            "hivexsh",
        )?;
        run_ok(
            Command::new(&tools.ntfscp)
                .arg(&ntfs)
                .arg(&hive)
                .arg("/Windows/System32/config/SOFTWARE"),
            "ntfscp hive",
        )?;
        Ok(())
    })();
    detach(&dev)?;
    result?;

    println!("[5/5] writing base image");
    let _ = std::fs::remove_file(&base);
    q.convert(&raw, &base, "qcow2")?;
    let _ = std::fs::remove_dir_all(&work);

    let size = std::fs::metadata(&base)?.len();
    println!(
        "\nRuntime ready: {} ({:.0} MiB)",
        base.display(),
        size as f64 / (1024.0 * 1024.0)
    );
    println!("Try:  winquick run -- cmd /c ver");
    Ok(())
}

fn locate_vhdx(source: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(s) = source {
        if s.is_dir() {
            let v = s.join("ValidationOS.vhdx");
            if v.exists() {
                return Ok(v);
            }
            bail!("no ValidationOS.vhdx in {}", s.display());
        }
        if s.extension().map(|e| e == "iso").unwrap_or(false) {
            return mount_iso(&s);
        }
        if s.exists() {
            return Ok(s);
        }
        bail!("{} does not exist", s.display());
    }

    let cached = paths::cache()?.join("validationos-arm64.iso");
    if cached.exists() {
        return mount_iso(&cached);
    }

    bail!(
        "No Validation OS image found.\n\n\
         WinQuick does not ship Microsoft software. Download Validation OS ARM64\n\
         from Microsoft yourself, accepting Microsoft's licence terms:\n\n  \
         {DOWNLOAD_URL}\n\n\
         Then either save it as {}\n  or pass it explicitly:  winquick setup --from <path-to-iso>",
        cached.display()
    )
}

fn mount_iso(iso: &Path) -> Result<PathBuf> {
    // The user may already have the ISO mounted; macOS refuses to attach it
    // twice, so reuse the existing mount point rather than failing.
    if let Some(existing) = existing_mount(iso) {
        let v = existing.join("ValidationOS.vhdx");
        if v.exists() {
            return Ok(v);
        }
    }

    let mnt = Path::new("/tmp/winquick-vos");
    std::fs::create_dir_all(mnt)?;
    let v = mnt.join("ValidationOS.vhdx");
    if v.exists() {
        return Ok(v);
    }
    run_ok(
        Command::new("/usr/bin/hdiutil")
            .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
            .arg(mnt)
            .arg(iso),
        "hdiutil attach",
    )?;
    if !v.exists() {
        bail!(
            "{} does not look like a Validation OS ARM64 ISO (no ValidationOS.vhdx inside)",
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

fn attach(raw: &Path) -> Result<String> {
    let out = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-imagekey", "diskimage-class=CRawDiskImage", "-nomount"])
        .arg(raw)
        .output()
        .context("running hdiutil attach")?;
    if !out.status.success() {
        bail!("hdiutil attach failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let dev = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("could not parse hdiutil output"))?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(dev)
}

fn detach(dev: &str) -> Result<()> {
    let _ = Command::new("/usr/bin/hdiutil").args(["detach", dev]).output();
    Ok(())
}

fn run_ok(c: &mut Command, what: &str) -> Result<()> {
    let out = c.output().with_context(|| format!("running {what}"))?;
    if !out.status.success() {
        bail!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
