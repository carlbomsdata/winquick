//! Building the desktop image, without a Windows host.
//!
//! Validation OS ships as a deliberately minimal Windows: no WPF, no GDI+, no
//! themes, no UI Automation, and — importantly — no display driver at all. The
//! optional packages that add those are on Microsoft's own media as CABs, and
//! the supported way to apply them is DISM.
//!
//! DISM only runs on Windows. WinQuick's answer is to run it inside WinQuick:
//! the existing Windows runtime boots with a *copy* of its own disk attached as
//! a second device, and services that copy offline with
//! `dism /Image:<drive>\ /Add-Package`. Nothing is downloaded, nothing
//! Microsoft-licensed is redistributed, and the user needs no Windows machine.
//!
//! Three things about this are not obvious, and each one failed silently before
//! it was understood:
//!
//! * **`/Online` does not work.** Every package returns 0x80070032
//!   (`ERROR_NOT_SUPPORTED`) against a running Validation OS. Offline servicing
//!   of a mounted image works for all of them.
//! * **The copy needs its own identity.** Two disks with the same GPT GUIDs make
//!   Windows mount the duplicate read-only and *discard writes without an
//!   error*, so DISM reports success and changes nothing. See [`crate::gpt`].
//! * **The identity has to be put back.** The bootloader records the partition
//!   GUID it boots from, so an image left with fresh GUIDs fails to boot with
//!   `0xc000000e`.
//!
//! The display driver needs no such care. Staging it with
//! `dism /Add-Driver` is enough: the guest's own PnP manager finishes the
//! installation on first boot, binding the device, creating the class key and
//! starting the miniport. Earlier versions of this code hand-wrote service and
//! CriticalDeviceDatabase entries; inspecting the serviced hive afterwards
//! showed the device bound to DISM's service with a class instance PnP had
//! built, and the hand-written entries unused.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{capability, desktop, gpt, mailbox, paths, qemu, runner, setup};

/// The optional packages a desktop needs, in dependency order.
///
/// Each is applied twice where the media has both: the neutral package and its
/// en-us companion. Trimming this list is tempting but each entry earned its
/// place — for instance a WPF window will start and then die with
/// `DllNotFoundException: UIAutomationCore.dll` without `WPF-Support`.
const PACKAGES: &[&str] = &[
    "COM",
    "Windows-Runtime-Metadata",
    "Fonts",
    "GDIPlus",
    "Graphics",
    "Graphics-UXTheme",
    "Apps",
    "PnP",
    "Driver-Support",
    "Connectivity",
    "WPF-Support",
    "DeveloperTools",
];

/// VirtIO drivers staged into the image. `viogpudo` is the display adapter;
/// `vioinput` is staged so the topology can grow input devices later.
const DRIVERS: &[(&str, &str)] = &[("viogpudo", "viogpudo.inf"), ("vioinput", "vioinput.inf")];

/// How long the DISM pass is allowed to take. Twelve packages against a cold
/// image is a few minutes; the ceiling is only there so a wedged guest does not
/// hang forever.
const SERVICING_TIMEOUT: Duration = Duration::from_secs(2400);

pub struct Options {
    pub verbose: bool,
    pub force: bool,
    /// Red Hat's virtio-win ISO, which carries the display driver.
    pub virtio: Option<PathBuf>,
}

pub fn install(opts: &Options) -> Result<()> {
    let base = paths::base_image()?;
    if !base.exists() {
        bail!(
            "the Windows runtime is not installed yet.\n\n\
             Run this first:\n    winquick setup --accept-microsoft-terms"
        );
    }
    let out = desktop::base_image()?;
    if out.exists() && !opts.force {
        println!("The desktop capability is already installed.");
        println!("Rebuild it with:  winquick capability install desktop --force");
        return Ok(());
    }
    if capability::image_path("dotnet-sdk")?.exists() {
        // Needed later, to build the bridge. Checking now avoids doing twenty
        // minutes of servicing and then failing.
    } else {
        bail!(
            "building the desktop bridge needs the .NET SDK capability.\n\n\
             Install it first with:\n    winquick capability install dotnet-sdk"
        );
    }

    let work = paths::root()?.join("work").join("desktop");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;

    println!("Building the desktop image. This takes several minutes.");

    // ---- 1. the servicing volume -----------------------------------------
    println!("  [1/5] collecting Microsoft packages and drivers");
    let svc_img = work.join("servicing.img");
    let payload = build_servicing_payload(&work, opts)?;
    capability::build_flat(&svc_img, &payload)?;
    let _ = std::fs::remove_dir_all(&payload);

    // ---- 2. the target copy ----------------------------------------------
    println!("  [2/5] preparing a copy of the Windows image to service");
    let q = qemu::Qemu::locate()?;
    let target_raw = work.join("target.raw");
    q.convert(&base, &target_raw, "raw")
        .context("making a raw copy of the Windows image to service")?;

    // Captured before the identity is changed, and put back after servicing.
    let original = gpt::snapshot(&target_raw)?;
    gpt::randomize(&target_raw)
        .context("giving the servicing target its own disk identity")?;

    // ---- 3. run DISM inside Windows --------------------------------------
    println!("  [3/5] applying packages with DISM inside Windows");
    service(&q, &work, &svc_img, &target_raw, opts)?;

    // ---- 4. put the identity back ----------------------------------------
    println!("  [4/5] restoring the boot identity");
    gpt::restore(&target_raw, &original)?;
    std::fs::create_dir_all(out.parent().unwrap())?;
    let staged = work.join("base.qcow2");
    q.convert(&target_raw, &staged, "qcow2")?;
    let _ = std::fs::remove_file(&target_raw);
    // Rename last, so an interrupted build never leaves a half-written image
    // that looks installed.
    std::fs::rename(&staged, &out)?;

    // ---- 5. the guest-side bridge ----------------------------------------
    println!("  [5/5] building the guest bridge");
    build_bridge(opts.verbose)?;

    let _ = std::fs::remove_dir_all(&work);
    println!(
        "Desktop capability ready ({:.1} GiB image).",
        crate::helpers::allocated(&out) as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!("Start a session with:  winquick desktop start");
    Ok(())
}

/// Assemble everything the servicing guest needs on one volume.
fn build_servicing_payload(work: &Path, opts: &Options) -> Result<PathBuf> {
    let payload = work.join("payload");
    std::fs::create_dir_all(payload.join("cabs"))?;

    let media = setup::mount_microsoft_image(None)?;
    let dism_src = media.join("GenImage").join("Tools").join("DISM").join("arm64");
    if !dism_src.join("dism.exe").exists() {
        bail!(
            "no arm64 DISM on the Microsoft media at {}.\n\n\
             The desktop capability needs the full Validation OS image, not just the VHDX.",
            dism_src.display()
        );
    }
    desktop::copy_tree(&dism_src, &payload.join("dism"))?;

    let cabs = media.join("cabs");
    let mut missing = Vec::new();
    for pkg in PACKAGES {
        let file = format!("Microsoft-WinVOS-{pkg}-Package.cab");
        match find_cab(&cabs, &file, "neutral") {
            Some(p) => {
                std::fs::copy(&p, payload.join("cabs").join(&file))?;
            }
            None => missing.push(*pkg),
        }
        // The en-us companion carries the localised resources. It is optional:
        // the neutral package alone is enough for the API surface.
        if let Some(p) = find_cab(&cabs, &file, "en-us") {
            std::fs::copy(&p, payload.join("cabs").join(format!("en-us-{file}")))?;
        }
    }
    if !missing.is_empty() {
        bail!(
            "the Microsoft media is missing these packages: {}.\n\n\
             They are part of the Validation OS ISO; a VHDX on its own does not have them.",
            missing.join(", ")
        );
    }

    let virtio = mount_virtio(opts)?;
    for (name, inf) in DRIVERS {
        let src = find_driver(&virtio, name, inf).ok_or_else(|| {
            anyhow!(
                "no ARM64 `{name}` driver on {}.\n\n\
                 WinQuick needs Red Hat's virtio-win ISO for the display driver, \
                 because Validation OS has no inbox one.",
                virtio.display()
            )
        })?;
        desktop::copy_tree(&src, &payload.join("drivers").join(name))?;
    }
    Ok(payload)
}

/// Packages live under `cabs/<set>/<language>/`, and which set a package is in
/// varies, so search rather than hard-code the layout.
fn find_cab(cabs: &Path, file: &str, language: &str) -> Option<PathBuf> {
    let sets = std::fs::read_dir(cabs).ok()?;
    for set in sets.flatten() {
        let p = set.path().join(language).join(file);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// virtio-win lays drivers out as `<driver>/<windows version>/ARM64/`. Prefer
/// the newest Windows directory that actually has an ARM64 build.
fn find_driver(root: &Path, name: &str, inf: &str) -> Option<PathBuf> {
    let dir = root.join(name);
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join("ARM64"))
        .filter(|p| p.join(inf).is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}

fn mount_virtio(opts: &Options) -> Result<PathBuf> {
    if let Some(p) = &opts.virtio {
        if p.is_dir() {
            return Ok(p.clone());
        }
        return setup::mount_iso_at(p);
    }
    let cached = newest_virtio_iso(&paths::cache()?);
    match cached {
        Some(p) => setup::mount_iso_at(&p),
        None => bail!(
            "the desktop capability needs Red Hat's virtio-win ISO, which carries the\n\
             ARM64 display driver Validation OS does not have.\n\n\
             Download it from\n    \
             https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/\n\
             and point WinQuick at it:\n    \
             winquick capability install desktop --virtio ~/Downloads/virtio-win.iso"
        ),
    }
}

fn newest_virtio_iso(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("virtio-win") && n.ends_with(".iso"))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found.pop()
}

/// Boot Windows with the target attached, run DISM, and wait for it to finish.
fn service(
    q: &qemu::Qemu,
    work: &Path,
    svc_img: &Path,
    target: &Path,
    opts: &Options,
) -> Result<()> {
    let root = work.join("servicing-root.qcow2");
    q.create_overlay(&paths::base_image()?, &root)?;

    let mbox = work.join("mailbox.img");
    mailbox::create_template(&mbox)?;
    mailbox::inject_command(&mbox, &servicing_script(), None, "servicing")?;

    let vars = work.join("vars.fd");
    std::fs::File::create(&vars)?.set_len(64 * 1024 * 1024)?;
    let log = work.join("servicing.log");

    let uefi = paths::uefi_code()
        .ok_or_else(|| anyhow!("QEMU's UEFI firmware is missing; reinstall QEMU"))?;
    let mut child = q.boot_servicing(&qemu::ServicingBoot {
        uefi_code: &uefi,
        uefi_vars: &vars,
        root_disk: &root,
        mailbox: &mbox,
        servicing: svc_img,
        target,
        serial_log: &log,
    })?;
    crate::interrupt::watch_child(child.id());

    let deadline = Instant::now() + SERVICING_TIMEOUT;
    let done = loop {
        if let Some(raw) = mailbox::probe(&mbox, mailbox::CODE_FILE) {
            if !String::from_utf8_lossy(&raw).trim().is_empty() {
                break true;
            }
        }
        if child.try_wait()?.is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(500));
    };

    // The guest force-dismounts the target as its last act, which is what puts
    // its writes on the host's disk. Only then is it safe to stop QEMU.
    let _ = child.kill();
    let _ = child.wait();
    crate::interrupt::clear_child();

    let results = mailbox::read_results(&mbox)?;
    let out = String::from_utf8_lossy(&results.stdout).replace('\r', "");
    if opts.verbose {
        for line in out.lines() {
            eprintln!("winquick: {line}");
        }
    }
    if !done || !out.contains("SERVICING-DONE") {
        bail!(
            "servicing did not complete.\n\n{}\n\nThe guest's console output is in {}",
            out.trim(),
            log.display()
        );
    }
    // DISM reports per-package failures in its own exit codes, which the script
    // echoes. A non-zero one means the image is missing something it needs.
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("pkg ") {
            if let Some((name, code)) = rest.rsplit_once(" rc=") {
                if code.trim() != "0" {
                    bail!("DISM could not apply the {name} package (exit code {code})");
                }
            }
        }
    }
    Ok(())
}

/// The batch script the servicing guest runs.
///
/// It finds both volumes by content rather than by drive letter, because
/// Windows assigns those in an order that depends on how many volumes are
/// attached.
fn servicing_script() -> String {
    let mut s = String::from(
        "setlocal enabledelayedexpansion\r\n\
         set S=\r\n\
         set T=\r\n\
         for %%d in (D E F G H I J K) do if not defined S if exist %%d:\\dism\\dism.exe set S=%%d:\r\n\
         for %%d in (D E F G H I J K) do if not defined T if exist %%d:\\Windows\\System32\\config\\SOFTWARE set T=%%d:\r\n\
         echo servicing=%S% target=%T%\r\n\
         if not defined T (echo NO-TARGET & exit /b 9)\r\n\
         set DI=%S%\\dism\\dism.exe\r\n",
    );
    for pkg in PACKAGES {
        s.push_str(&format!(
            "%DI% /Image:%T%\\ /Add-Package /PackagePath:%S%\\cabs\\Microsoft-WinVOS-{pkg}-Package.cab >nul 2>&1\r\n\
             echo pkg {pkg} rc=!errorlevel!\r\n\
             if exist %S%\\cabs\\en-us-Microsoft-WinVOS-{pkg}-Package.cab \
             %DI% /Image:%T%\\ /Add-Package /PackagePath:%S%\\cabs\\en-us-Microsoft-WinVOS-{pkg}-Package.cab >nul 2>&1\r\n"
        ));
    }
    for (name, inf) in DRIVERS {
        s.push_str(&format!(
            "%DI% /Image:%T%\\ /Add-Driver /Driver:%S%\\drivers\\{name}\\{inf} >nul 2>&1\r\n\
             echo driver {name} rc=!errorlevel!\r\n"
        ));
    }
    // Windows only synchronises a volume with the disk underneath at dismount,
    // so without this the host would convert an image that still has none of
    // these changes in it.
    s.push_str("mountvol %T% /P\r\necho SERVICING-DONE\r\n");
    s
}

/// Build `wqui.exe` inside Windows and keep the result for later sessions.
fn build_bridge(verbose: bool) -> Result<()> {
    let src = bridge_source()?;
    let dest = desktop::bridge_dir()?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;

    let opts = runner::Options {
        memory_mb: 2048,
        cpus: 4,
        timeout: Duration::from_secs(900),
        verbose,
        force_cold: false,
        workspace: Some(src),
        artifacts: vec!["publish/**".to_string()],
        artifacts_dir: dest.clone(),
        artifact_overwrite: true,
    };
    let outcome = runner::run_capture(
        "dotnet publish wqui.csproj -c Release -o publish --nologo",
        &opts,
    )?;
    if outcome.exit_code != 0 {
        bail!(
            "could not build the guest bridge:\n{}\n{}",
            String::from_utf8_lossy(&outcome.stdout).replace('\r', ""),
            String::from_utf8_lossy(&outcome.stderr).replace('\r', "")
        );
    }
    // Artifacts arrive under the pattern's own directory; flatten so the volume
    // builder sees the executable at the top.
    let nested = dest.join("publish");
    if nested.is_dir() {
        for entry in std::fs::read_dir(&nested)? {
            let e = entry?;
            std::fs::rename(e.path(), dest.join(e.file_name()))?;
        }
        let _ = std::fs::remove_dir_all(&nested);
    }
    if !dest.join("wqui.exe").exists() {
        bail!("the bridge build produced no wqui.exe");
    }
    Ok(())
}

/// The bridge sources, either from an installed copy or a source checkout.
pub fn bridge_source() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(bin) = exe.parent() {
            if let Some(prefix) = bin.parent() {
                candidates.push(prefix.join("share").join("winquick").join("wqui"));
                if let Some(repo) = prefix.parent() {
                    candidates.push(repo.join("guest").join("wqui"));
                }
            }
        }
    }
    candidates.push(PathBuf::from("guest/wqui"));
    for c in candidates {
        if c.join("wqui.csproj").is_file() {
            return Ok(c);
        }
    }
    bail!("cannot find the guest bridge sources (guest/wqui)")
}
