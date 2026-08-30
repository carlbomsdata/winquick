//! What WinQuick knows about itself, as data rather than as printed lines.
//!
//! `info` and `doctor` used to build their answers straight into `println!`,
//! which is fine until something other than a terminal wants the same answers.
//! The MCP server does, and an agent needs fields it can branch on, not a table
//! it has to scrape. So the facts are gathered once, here, and the CLI renders
//! them while MCP serialises them. One source of truth, two presentations.

use anyhow::Result;
use serde::Serialize;
use std::path::Path;

use crate::{capability, desktop, helpers, paths, servicing, setup, state};

// ------------------------------------------------------------------- info

#[derive(Serialize)]
pub struct Capability {
    pub name: String,
    pub version: Option<String>,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct DesktopFacts {
    pub installed: bool,
    pub bytes: u64,
    /// A prepared session state is what makes a start take ~0.4 s.
    pub prepared: bool,
    pub prepared_bytes: u64,
    pub session_running: bool,
    pub session_pid: Option<u32>,
}

#[derive(Serialize)]
pub struct Info {
    pub version: &'static str,
    pub platform: String,
    pub runtime_installed: bool,
    pub runtime_bytes: u64,
    /// Whether a .NET Framework has been serviced into a second image, and
    /// therefore whether `winquick run` boots it. An agent deciding how to
    /// build a `net4xx` project needs this before it starts, not after a
    /// `0xC0000135`.
    pub dotnet_framework_installed: bool,
    pub dotnet_framework_bytes: u64,
    /// The image a command will actually boot, which is the serviced one when
    /// it exists and the pristine one otherwise.
    pub run_image: String,
    pub prepared: bool,
    pub prepared_bytes: u64,
    pub capabilities: Vec<Capability>,
    pub package_cache_bytes: Option<u64>,
    pub desktop: DesktopFacts,
    pub data_dir: String,
}

pub fn info() -> Result<Info> {
    let base = paths::base_image()?;
    let runtime_installed = base.exists();
    let netfx = paths::framework_image()?;
    let netfx_installed = netfx.exists();

    let (prepared, prepared_bytes) = match state::state_dir() {
        Ok(d) if d.join("ready.json").exists() => {
            (true, helpers::allocated(&d.join("ready.state")))
        }
        _ => (false, 0),
    };

    let installed = capability::installed()?;
    let capabilities = installed
        .iter()
        .filter(|c| capability::spec(&c.name).is_some())
        .map(|c| Capability {
            name: c.name.clone(),
            version: capability::spec(&c.name).map(|s| s.version.to_string()),
            bytes: helpers::allocated(&c.image),
        })
        .collect();
    // The package cache is an internal volume, not something you install, so it
    // is reported separately rather than as a capability.
    let package_cache_bytes = installed
        .iter()
        .find(|c| c.name == "nuget-cache")
        .map(|c| helpers::allocated(&c.image));

    let desk = desktop::base_image()?;
    let desk_installed = desk.exists();
    let dstate = state::desktop_state_dir()?;
    let desk_prepared = desk_installed && dstate.join("ready.json").exists();
    let session = desktop::running();

    Ok(Info {
        version: env!("CARGO_PKG_VERSION"),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        runtime_installed,
        runtime_bytes: if runtime_installed { helpers::allocated(&base) } else { 0 },
        dotnet_framework_installed: netfx_installed,
        dotnet_framework_bytes: if netfx_installed { helpers::allocated(&netfx) } else { 0 },
        run_image: paths::run_image()?.display().to_string(),
        prepared,
        prepared_bytes,
        capabilities,
        package_cache_bytes,
        desktop: DesktopFacts {
            installed: desk_installed,
            bytes: if desk_installed { helpers::allocated(&desk) } else { 0 },
            prepared: desk_prepared,
            prepared_bytes: if desk_prepared { dir_size(&dstate) } else { 0 },
            session_running: session.is_some(),
            session_pid: session.map(|s| s.pid),
        },
        data_dir: paths::root()?.display().to_string(),
    })
}

// ----------------------------------------------------------------- doctor

/// How a single check came out.
///
/// `Note` is deliberately distinct from `Fail`: "no prepared guest yet" is
/// normal on a fresh install and must not read as a fault, which is exactly the
/// distinction an agent needs in order to decide whether to act.
#[derive(Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Note,
    Fail,
}

#[derive(Serialize)]
pub struct Check {
    pub section: &'static str,
    pub name: String,
    pub status: Status,
    pub message: String,
}

#[derive(Serialize)]
pub struct Doctor {
    pub healthy: bool,
    pub checks: Vec<Check>,
    /// What to actually do about it, in the order it has to be done.
    pub problems: Vec<String>,
}

struct Builder {
    checks: Vec<Check>,
    problems: Vec<String>,
}

impl Builder {
    fn add(&mut self, section: &'static str, name: &str, status: Status, message: String) {
        self.checks.push(Check { section, name: name.to_string(), status, message });
    }
    fn ok(&mut self, section: &'static str, name: &str, message: impl Into<String>) {
        self.add(section, name, Status::Ok, message.into());
    }
    fn note(&mut self, section: &'static str, name: &str, message: impl Into<String>) {
        self.add(section, name, Status::Note, message.into());
    }
    fn fail(&mut self, section: &'static str, name: &str, message: impl Into<String>, problem: impl Into<String>) {
        self.add(section, name, Status::Fail, message.into());
        self.problems.push(problem.into());
    }
}

pub fn doctor() -> Result<Doctor> {
    let mut b = Builder { checks: Vec::new(), problems: Vec::new() };

    // -- host
    //
    // Two hosts are supported, and each one only in the shape that gives real
    // hardware virtualisation: an Apple Silicon Mac running an ARM64 guest
    // through Hypervisor.framework, and an x86_64 PC running an x64 guest
    // through the Windows Hypervisor Platform. Emulating a guest of the other
    // architecture would work and would not be this product.
    let arch = std::env::consts::ARCH;
    match (std::env::consts::OS, arch) {
        ("macos", "aarch64") => b.ok("Host", "cpu", format!("Apple Silicon ({arch})")),
        ("windows", "x86_64") => b.ok("Host", "cpu", format!("x86_64 ({arch})")),
        // Linux hosts the same product against KVM. Both architectures work the
        // same way -- the guest follows the host -- so neither is special-cased.
        ("linux", "x86_64") | ("linux", "aarch64") => {
            b.ok("Host", "cpu", format!("{arch} (kvm)"))
        }
        ("linux", _) => b.fail(
            "Host",
            "cpu",
            format!("unsupported architecture ({arch})"),
            "On Linux, WinQuick needs x86_64 or aarch64.",
        ),
        ("macos", _) => b.fail(
            "Host",
            "cpu",
            format!("not Apple Silicon ({arch})"),
            "On macOS, WinQuick needs an Apple Silicon Mac.",
        ),
        ("windows", _) => b.fail(
            "Host",
            "cpu",
            format!("not x86_64 ({arch})"),
            "On Windows, WinQuick needs an x86_64 PC.",
        ),
        (os, _) => b.fail(
            "Host",
            "cpu",
            format!("unsupported host ({os} {arch})"),
            "WinQuick runs on Apple Silicon macOS, x86_64 Windows and Linux.",
        ),
    }
    host_version(&mut b);

    // -- tools
    let have_runtime = paths::base_image()?.exists();
    for t in helpers::survey() {
        match &t.path {
            Some(p) => b.ok("Tools", t.name, p.display().to_string()),
            // Only needed to build a runtime, and one is already built.
            None if t.needed_for == "setup only" && have_runtime => {
                b.ok("Tools", t.name, "not installed (only needed by `winquick setup`)")
            }
            None => b.fail(
                "Tools",
                t.name,
                format!("missing ({})", t.needed_for),
                format!("{} is missing. {}", t.name, t.install_hint),
            ),
        }
    }
    match helpers::uefi_firmware() {
        Some(p) => b.ok("Tools", "uefi firmware", p.display().to_string()),
        None => b.fail("Tools", "uefi firmware", "missing",
                       if cfg!(target_os = "linux") {
                           "QEMU's UEFI firmware is missing. apt install qemu-efi-aarch64 (or ovmf on x86_64)"
                       } else {
                           "QEMU's UEFI firmware is missing. brew reinstall qemu"
                       }),
    }

    // -- runtime
    let base = paths::base_image()?;
    if base.exists() {
        b.ok("Runtime", "windows runtime", helpers::human(helpers::allocated(&base)));
        if let Err(e) = state::check_base_meta(&base, setup::AGENT) {
            b.fail("Runtime", "runtime version",
                   "runtime is from a different WinQuick version", format!("{e:#}"));
        }
        // Which image a command actually boots is the first thing to know when
        // a build behaves differently from the one you remember.
        let netfx = paths::framework_image()?;
        if netfx.exists() {
            b.ok(
                "Runtime",
                ".NET Framework",
                format!("{}, and `run` boots it", helpers::human(helpers::allocated(&netfx))),
            );
            if let Err(e) = state::check_base_meta(&netfx, setup::AGENT) {
                b.fail("Runtime", ".NET Framework image",
                       "serviced from a different WinQuick version", format!("{e:#}"));
            }
        }
    } else {
        b.fail("Runtime", "windows runtime", "not installed",
               "No Windows runtime. Run `winquick setup`.");
    }
    let prepared = state::state_dir().map(|d| d.join("ready.json").exists()).unwrap_or(false);
    let restore_off = state::restore_note().map(|p| p.exists()).unwrap_or(false);
    if restore_off {
        // Not a fault, and not something the user did: this accelerator cannot
        // resume a saved guest, so WinQuick stopped trying. Saying so here is
        // the difference between "runs are slow" and "runs are slow, and here
        // is exactly why, and here is what changes it".
        b.note(
            "Runtime",
            "prepared guest",
            "disabled: this QEMU restores a guest that never resumes, so every run boots cold",
        );
    } else if prepared {
        b.ok("Runtime", "prepared guest", "ready (runs are fast)");
    } else {
        b.note("Runtime", "prepared guest", "not built yet; the first run will build it");
    }
    if crate::platform::NEEDS_PATCHED_QEMU && !restore_off && !prepared {
        b.note(
            "Runtime",
            "fast path",
            "needs a QEMU carrying patches/whpx-stop-and-copy.patch; stock QEMU boots cold",
        );
    }
    let caps = capability::installed()?;
    let names = if caps.is_empty() {
        "none".to_string()
    } else {
        caps.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ")
    };
    b.note("Runtime", "capabilities", names);

    // -- desktop
    let desk = desktop::base_image()?;
    if desk.exists() {
        b.ok("Desktop", "desktop image", helpers::human(helpers::allocated(&desk)));
    } else {
        b.note("Desktop", "desktop image",
               "not installed (winquick capability install desktop)");
    }
    match desktop::running() {
        Some(s) => b.ok("Desktop", "session", format!("running as pid {}", s.pid)),
        None => b.ok("Desktop", "session", "none running"),
    }
    let dstate = state::desktop_state_dir()?;
    if dstate.join("ready.json").exists() {
        b.ok("Desktop", "session state", format!("prepared ({})", helpers::human(dir_size(&dstate))));
    } else if desk.exists() {
        b.note("Desktop", "session state", "not prepared yet (the first start takes ~20s)");
    }
    // The bridge is built from source inside Windows at install time, so an
    // installation that lost these files fails at the very last step of
    // `capability install desktop`.
    if desk.exists() {
        let built = desktop::bridge_dir()?;
        if built.join("wqui.exe").exists() {
            b.ok("Desktop", "guest bridge", "built");
        } else {
            b.fail(
                "Desktop",
                "guest bridge",
                format!("missing from {}", built.display()),
                "The desktop capability is installed but its guest bridge is missing. \
                 Rebuild it with `winquick capability install desktop --force`.",
            );
        }
    }
    match servicing::bridge_source() {
        Ok(p) => b.ok("Desktop", "bridge sources", p.display().to_string()),
        Err(_) => b.add("Desktop", "bridge sources", Status::Fail,
                        "missing (the installation is incomplete)".into()),
    }

    // -- disk
    let root = paths::root()?;
    let free = free_bytes(&root).unwrap_or(0);
    if free > 8 * 1024 * 1024 * 1024 {
        b.ok("Disk", "free space", format!("{} free in {}", helpers::human(free), root.display()));
    } else {
        b.fail("Disk", "free space",
               format!("{} free in {}", helpers::human(free), root.display()),
               "Less than 8 GiB free. Setup and capabilities need room.");
    }

    Ok(Doctor { healthy: b.problems.is_empty(), checks: b.checks, problems: b.problems })
}

// ------------------------------------------------------------------ shared

pub fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else {
                total += helpers::allocated(&path);
            }
        }
    }
    total
}

/// The host operating system's own version, as a note rather than a check:
/// nothing depends on it, but it is the first thing worth knowing in a bug
/// report.
fn host_version(b: &mut Builder) {
    #[cfg(target_os = "macos")]
    {
        let sw = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if sw.is_empty() {
            b.note("Host", "macos", "version unknown");
        } else {
            b.ok("Host", "macos", format!("macOS {sw}"));
        }
    }
    #[cfg(target_os = "windows")]
    {
        let ver = std::process::Command::new("cmd")
            .args(["/c", "ver"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if ver.is_empty() {
            b.note("Host", "windows", "version unknown");
        } else {
            b.ok("Host", "windows", ver);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        b.note("Host", "os", std::env::consts::OS);
    }
}

#[cfg(unix)]
pub fn free_bytes(p: &Path) -> Option<u64> {
    let out = std::process::Command::new("/bin/df").arg("-k").arg(p).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let blocks: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(blocks * 1024)
}

/// Windows reports free space per directory, because a quota can make it differ
/// from the volume's own free space. That is the number that matters here.
#[cfg(windows)]
pub fn free_bytes(p: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }

    // The path has to exist for the query to mean anything; walk up until it
    // does, so a missing ~/.winquick still reports the volume it would live on.
    let mut dir = p;
    while !dir.exists() {
        dir = dir.parent()?;
    }
    let wide: Vec<u16> =
        dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut())
    };
    (ok != 0).then_some(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every check carries a section and a name, because the CLI groups by the
    /// first and an agent keys on the second.
    #[test]
    fn checks_are_labelled() {
        let d = doctor().expect("doctor runs on any host");
        assert!(!d.checks.is_empty());
        for c in &d.checks {
            assert!(!c.section.is_empty(), "check without a section");
            assert!(!c.name.is_empty(), "check without a name");
        }
    }

    /// `healthy` must mean exactly "nothing to fix", or an agent cannot trust it.
    #[test]
    fn healthy_agrees_with_the_problem_list() {
        let d = doctor().expect("doctor runs");
        assert_eq!(d.healthy, d.problems.is_empty());
        let failed = d.checks.iter().any(|c| c.status == Status::Fail);
        // A failing check must have produced a problem to act on. The bridge
        // sources check is the one deliberate exception: it reports a broken
        // install without adding a second, duplicate instruction.
        if failed && d.problems.is_empty() {
            assert!(
                d.checks.iter().filter(|c| c.status == Status::Fail).all(|c| c.name == "bridge sources"),
                "a failing check produced no problem to act on"
            );
        }
    }

    /// A note is advice, not a fault; it must never make the host unhealthy.
    #[test]
    fn notes_do_not_make_a_host_unhealthy() {
        let mut b = Builder { checks: Vec::new(), problems: Vec::new() };
        b.note("Runtime", "prepared guest", "not built yet");
        b.ok("Host", "cpu", "Apple Silicon");
        assert!(b.problems.is_empty());
    }

    #[test]
    fn info_reports_a_version_and_a_platform() {
        let i = info().expect("info runs");
        assert_eq!(i.version, env!("CARGO_PKG_VERSION"));
        assert!(i.platform.contains('-'), "platform should be os-arch: {}", i.platform);
        assert!(!i.data_dir.is_empty());
    }
}
