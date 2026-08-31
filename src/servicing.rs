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

/// .NET Framework.
///
/// The package name is historical: it carries the OS's inbox
/// `C:\Windows\Microsoft.NET`, and 4.x is in-place, so one runtime serves every
/// 4.x target. It brings the classic build toolchain with it — an ARM64
/// `MSBuild.exe`, `Microsoft.Common.targets`, `Microsoft.CSharp.targets`,
/// `Microsoft.WinFX.targets` and `PresentationBuildTasks.dll` — which is the
/// only thing that can restore a `packages.config` project or markup-compile a
/// classic WPF one.
///
/// WinQuick's own notes used to say Validation OS "carries no .NET Framework
/// runtime at all". That is true of the stock image, and was read as meaning it
/// could not have one. It is on the media, in `cabs/Common`, beside the
/// graphics packages, and DISM takes it like any other. Without it a .NET
/// Framework application builds correctly and then dies on launch with
/// `0xC0000135` — measured, on a real WPF application.
const FRAMEWORK_PACKAGES: &[&str] = &[
    // `shell32.dll`, which `urlmon.dll` imports and the stock image does not
    // have. Nothing loads urlmon without it, and the chain below ends there.
    "Apps",
    "Apps-WOW64",
    // `System.Drawing` is not optional either: a .NET Framework without GDI+
    // throws `TypeInitializationException` on `new Bitmap(...)`, which is where
    // a great many Framework applications begin. Fonts come with it, because
    // GDI+ wants one the moment anything draws text.
    "Fonts",
    "GDIPlus",
    // COM next, and not optional. The Framework is built on it, and so are
    // MSBuild's own tasks: `GenerateResource` asks the shell for a file's
    // security zone before it will read a `.resx`, and without the COM package
    // that fails with `REGDB_E_CLASSNOTREG` and takes the build down with it.
    "COM",
    "COM-WOW64",
    // Carries `rasapi32.dll`, which `System.Net`'s proxy detection loads the
    // first time anything resolves a URL. Without it NuGet dies before it reads
    // a single package: "The type initializer for 'ProxyCache' threw".
    "WLAN",
    "NetFx45",
    // The 32-bit half, for an x86 application under emulation.
    "NetFx45-WOW64",
];

/// VirtIO drivers staged into the image. `viogpudo` is the display adapter;
/// `vioinput` is staged so the topology can grow input devices later.
const DRIVERS: &[(&str, &str)] = &[("viogpudo", "viogpudo.inf"), ("vioinput", "vioinput.inf")];

/// How long the DISM pass is allowed to take. A dozen or two packages against
/// a cold image is a few minutes; the ceiling is only there so a wedged guest
/// does not hang forever.
const SERVICING_TIMEOUT: Duration = Duration::from_secs(2400);

/// The name this capability answers to on the command line.
pub const FRAMEWORK_CAPABILITY: &str = "dotnet-framework";

pub struct Options {
    pub verbose: bool,
    pub force: bool,
    /// Red Hat's virtio-win ISO, which carries the display driver.
    pub virtio: Option<PathBuf>,
}

/// Everything a desktop image needs: the graphics and WPF packages, plus the
/// .NET Framework, so a session can run a .NET Framework application.
fn desktop_packages() -> Vec<&'static str> {
    // Deduplicated, in order. The two lists overlap on purpose -- a desktop
    // needs GDI+ for its own reasons and .NET Framework needs it for
    // `System.Drawing` -- and applying a package twice is not merely wasteful:
    // the second copy lands on the read-only file the first one left behind,
    // and the build stops with "Permission denied" before DISM has run at all.
    let mut out: Vec<&'static str> = Vec::new();
    for p in PACKAGES.iter().chain(FRAMEWORK_PACKAGES.iter()).copied() {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Service the `run` image so it has a .NET Framework.
///
/// Written to a second image rather than over the pristine one: the base stays
/// byte-identical, and removing this capability is deleting a file. The runner
/// prefers it when it is there, and the ready-state fingerprint already carries
/// the image's identity, so switching either way rebuilds the prepared guest by
/// itself.
///
/// Unlike the desktop image this needs no drivers and no bridge — it is the
/// same headless machine with more of Windows in it.
pub fn install_framework(opts: &Options) -> Result<()> {
    let base = paths::base_image()?;
    if !base.exists() {
        bail!(
            "the Windows runtime is not installed yet.\n\n\
             Run this first:\n    winquick setup --accept-microsoft-terms"
        );
    }
    let out = paths::framework_image()?;
    if out.exists() && !opts.force {
        println!("The .NET Framework capability is already installed.");
        println!("Rebuild it with:  winquick capability install dotnet-framework --force");
        return Ok(());
    }

    let work = paths::root()?.join("work").join("framework");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;

    println!("Adding the .NET Framework to the Windows image. This takes a few minutes.");

    println!("  [1/4] collecting Microsoft packages");
    let svc_img = work.join("servicing.img");
    let payload = build_servicing_payload(&work, opts, FRAMEWORK_PACKAGES, &[])?;
    capability::build_flat(&svc_img, &payload)?;
    let _ = std::fs::remove_dir_all(&payload);

    println!("  [2/4] preparing a copy of the Windows image to service");
    let q = qemu::Qemu::locate()?;
    let target_raw = work.join("target.raw");
    q.convert(&base, &target_raw, "raw")
        .context("making a raw copy of the Windows image to service")?;
    let original = gpt::snapshot(&target_raw)?;
    gpt::randomize(&target_raw)
        .context("giving the servicing target its own disk identity")?;

    println!("  [3/4] applying packages with DISM inside Windows");
    service(&q, &work, &svc_img, &target_raw, opts, FRAMEWORK_PACKAGES, &[])?;

    println!("  [4/4] restoring the boot identity");
    gpt::restore(&target_raw, &original)?;
    std::fs::create_dir_all(out.parent().unwrap())?;
    let staged = work.join("base.qcow2");
    q.convert(&target_raw, &staged, "qcow2")?;
    let _ = std::fs::remove_file(&target_raw);
    // The agent came along with the image, so its metadata comes along too.
    // Without this every run reports "built by a different version of winquick".
    std::fs::copy(
        crate::state::base_meta_path(&base)?,
        crate::state::base_meta_path(&staged)?,
    )
    .context("carrying the runtime metadata onto the serviced image")?;
    std::fs::rename(
        crate::state::base_meta_path(&staged)?,
        crate::state::base_meta_path(&out)?,
    )?;
    // Renamed last, so an interrupted build never leaves a half-written image
    // that looks installed.
    std::fs::rename(&staged, &out)?;

    // The prepared guest was frozen from the other image.
    let _ = crate::state::discard();

    let _ = std::fs::remove_dir_all(&work);
    println!(
        ".NET Framework ready ({:.1} GiB image).",
        crate::helpers::allocated(&out) as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!("`winquick run` uses it from now on.");
    Ok(())
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
    let payload = build_servicing_payload(&work, opts, &desktop_packages(), DRIVERS)?;
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
    service(&q, &work, &svc_img, &target_raw, opts, &desktop_packages(), DRIVERS)?;

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

    // A prepared desktop state is a frozen guest running the old bridge from the
    // old image. Both just changed.
    let _ = crate::state::discard_desktop();

    let _ = std::fs::remove_dir_all(&work);
    println!(
        "Desktop capability ready ({:.1} GiB image).",
        crate::helpers::allocated(&out) as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!("Start a session with:  winquick desktop start");
    Ok(())
}

/// Assemble everything the servicing guest needs on one volume.
fn build_servicing_payload(
    work: &Path,
    opts: &Options,
    packages: &[&str],
    drivers: &[(&str, &str)],
) -> Result<PathBuf> {
    let payload = work.join("payload");
    std::fs::create_dir_all(payload.join("cabs"))?;

    let media = setup::mount_microsoft_image(None)?;
    // The media carries a DISM per architecture; take the one that matches the
    // guest. Hardcoding arm64 meant an x64 host looked in a directory the x64
    // media does not have.
    let dism_arch = if crate::platform::GUEST_ARCH == "arm64" { "arm64" } else { "amd64" };
    let dism_src = media.join("GenImage").join("Tools").join("DISM").join(dism_arch);
    if !dism_src.join("dism.exe").exists() {
        bail!(
            "no {dism_arch} DISM on the Microsoft media at {}.\n\n\
             The desktop capability needs the full Validation OS image, not just the VHDX.",
            dism_src.display()
        );
    }
    desktop::copy_tree(&dism_src, &payload.join("dism"))?;

    let cabs = media.join("cabs");
    let mut missing = Vec::new();
    for pkg in packages {
        let file = format!("Microsoft-WinVOS-{pkg}-Package.cab");
        match find_cab(&cabs, &file, "neutral") {
            Some(p) => stage_cab(&p, &payload.join("cabs").join(&file))?,
            None => missing.push(*pkg),
        }
        // The en-us companion carries the localised resources. It is optional:
        // the neutral package alone is enough for the API surface.
        if let Some(p) = find_cab(&cabs, &file, "en-us") {
            stage_cab(&p, &payload.join("cabs").join(format!("en-us-{file}")))?;
        }
    }
    if !missing.is_empty() {
        bail!(
            "the Microsoft media is missing these packages: {}.\n\n\
             They are part of the Validation OS ISO; a VHDX on its own does not have them.",
            missing.join(", ")
        );
    }

    if drivers.is_empty() {
        return Ok(payload);
    }
    let virtio = mount_virtio(opts)?;
    for (name, inf) in drivers {
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

/// Copy one package onto the servicing volume.
///
/// The destination is removed first. Files on the mounted ISO are read-only and
/// `std::fs::copy` carries the mode across, so writing the same name twice
/// fails with `Permission denied` rather than overwriting.
fn stage_cab(src: &Path, dst: &Path) -> Result<()> {
    let _ = std::fs::remove_file(dst);
    std::fs::copy(src, dst).with_context(|| format!("staging {}", src.display()))?;
    Ok(())
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
/// Where the virtio-win disc keeps the driver for this guest.
///
/// The layout is `<driver>/<windows-release>/<arch>/`, and the arch directory
/// was hardcoded to `ARM64`. The disc carries ARM64 builds of these drivers
/// whatever guest you are running, so on an x64 image the staging *succeeded*
/// and installed a driver the guest could never bind: DISM was happy, the
/// capability reported ready, the desktop session started, UI Automation
/// worked -- and every screenshot came back a single flat black, because
/// nothing had a display adapter.
fn driver_arch() -> &'static str {
    if crate::platform::GUEST_ARCH == "arm64" { "ARM64" } else { "amd64" }
}

fn find_driver(root: &Path, name: &str, inf: &str) -> Option<PathBuf> {
    let dir = root.join(name);
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(driver_arch()))
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
    packages: &[&str],
    drivers: &[(&str, &str)],
) -> Result<()> {
    let root = work.join("servicing-root.qcow2");
    q.create_overlay(&paths::base_image()?, &root)?;

    let mbox = work.join("mailbox.img");
    mailbox::create_template(&mbox)?;
    mailbox::inject_command(&mbox, &servicing_script(packages, drivers), None, "servicing")?;

    let vars = work.join("vars.fd");
    crate::helpers::fresh_uefi_vars(&vars)?;
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
fn servicing_script(packages: &[&str], drivers: &[(&str, &str)]) -> String {
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
    for pkg in packages {
        s.push_str(&format!(
            "%DI% /Image:%T%\\ /Add-Package /PackagePath:%S%\\cabs\\Microsoft-WinVOS-{pkg}-Package.cab >nul 2>&1\r\n\
             echo pkg {pkg} rc=!errorlevel!\r\n\
             if exist %S%\\cabs\\en-us-Microsoft-WinVOS-{pkg}-Package.cab \
             %DI% /Image:%T%\\ /Add-Package /PackagePath:%S%\\cabs\\en-us-Microsoft-WinVOS-{pkg}-Package.cab >nul 2>&1\r\n"
        ));
    }
    for (name, inf) in drivers {
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
        // The same shape as a plain `winquick run`, so this shares its prepared
        // guest instead of invalidating it.
        memory_mb: runner::DEFAULT_MEMORY_MB,
        cpus: runner::DEFAULT_CPUS,
        timeout: Duration::from_secs(900),
        verbose,
        force_cold: false,
        workspace: Some(src),
        artifacts: vec!["publish/**".to_string()],
        artifacts_dir: dest.clone(),
        artifact_overwrite: true,
    };
    // The runtime identifier has to name the guest's architecture, not a fixed
    // one: an ARM64 apphost does not run in an x64 guest, and vice versa.
    let outcome = runner::run_capture(
        &format!(
            "dotnet publish wqui.csproj -c Release -r win-{} -o publish --nologo",
            crate::platform::GUEST_ARCH
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop list and the .NET Framework list overlap on purpose. Staging
    /// the same package twice copies a read-only file from the mounted ISO onto
    /// itself, and the whole build stops with "Permission denied" before DISM
    /// has run — which is a maddening way to learn that two lists share an
    /// entry.
    #[test]
    fn a_package_is_staged_once_even_when_two_lists_want_it() {
        let all = desktop_packages();
        let mut seen: Vec<&str> = Vec::new();
        for p in &all {
            assert!(!seen.contains(p), "{p} appears twice in the desktop package set");
            seen.push(p);
        }
        // Everything either list asks for is still there.
        for p in PACKAGES.iter().chain(FRAMEWORK_PACKAGES.iter()) {
            assert!(all.contains(p), "{p} was dropped");
        }
    }

    /// A desktop is a superset: it services everything a `run` image does, plus
    /// what it needs for a screen. Losing that would mean an application that
    /// runs headlessly and then dies in a session, which is exactly the failure
    /// this pairing exists to prevent.
    #[test]
    fn a_desktop_gets_everything_the_framework_image_gets() {
        let desk = desktop_packages();
        for p in FRAMEWORK_PACKAGES {
            assert!(desk.contains(p), "the desktop image would not have {p}");
        }
    }

    /// Each of these was added because a real build or launch failed without
    /// it, and each cost a full servicing pass to find. The list looks longer
    /// than "install .NET Framework" ought to be, which is exactly why someone
    /// will eventually try to trim it — so the reasons are asserted here
    /// rather than only written in a comment.
    #[test]
    fn the_framework_image_keeps_its_hard_won_prerequisites() {
        for (pkg, why) in [
            ("NetFx45", "the runtime itself; without it: 0xC0000135 on launch"),
            ("Apps", "shell32, which urlmon imports; GenerateResource needs it"),
            ("GDIPlus", "System.Drawing; `new Bitmap` throws without it"),
            ("Fonts", "GDI+ wants one the moment anything draws text"),
            ("COM", "MSBuild's own tasks; REGDB_E_CLASSNOTREG without it"),
            ("WLAN", "rasapi32, which NuGet's ProxyCache loads"),
        ] {
            assert!(FRAMEWORK_PACKAGES.contains(&pkg), "{pkg} was dropped — {why}");
        }
        // The 32-bit halves let an x86 application run under emulation. They
        // are not decoration either.
        for pkg in ["Apps-WOW64", "COM-WOW64", "NetFx45-WOW64"] {
            assert!(FRAMEWORK_PACKAGES.contains(&pkg), "{pkg} was dropped");
        }
    }

    /// The order matters to DISM, and the first list has to stay first.
    #[test]
    fn the_desktop_packages_keep_their_order() {
        let all = desktop_packages();
        assert_eq!(all[0], PACKAGES[0]);
        let com = all.iter().position(|p| *p == "COM").unwrap();
        let wpf = all.iter().position(|p| *p == "WPF-Support").unwrap();
        assert!(com < wpf, "COM must be applied before what depends on it");
    }
}
