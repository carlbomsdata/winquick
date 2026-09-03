//! `winquick desktop` — a live Windows desktop you can drive from the shell.
//!
//! Unlike `winquick run`, which boots a guest per command, a desktop session
//! stays up. Windows starts once, an application is launched into a real
//! desktop, and every later verb is a round trip through the mailbox the guest
//! agent is already polling — about half a second, against the seven seconds a
//! boot costs. That is what makes iterating on a UI bearable.
//!
//! The session is still disposable: the guest runs on a copy-on-write overlay
//! over the desktop base image, and `winquick desktop stop` deletes it.
//!
//! # What the guest needs
//!
//! Three things beyond the ordinary runtime, all supplied by
//! `winquick capability install desktop`:
//!
//! * The desktop packages (WPF, GDI+, fonts, themes, UI Automation) applied
//!   offline to a copy of the Windows image with DISM.
//! * A display adapter. Validation OS has the BasicDisplay *service* but not
//!   `BasicDisplay.sys`, so there is no inbox driver for a plain framebuffer.
//!   WinQuick attaches a VirtIO GPU and stages Red Hat's `viogpudo` driver into
//!   the image's DriverStore; the guest's own PnP manager completes the install
//!   on first boot.
//! * `wqui.exe`, the guest-side bridge, built from `guest/wqui/`.
//!
//! # Why screenshots come from inside the guest
//!
//! The obvious host-side capture is QMP `screendump`, and WinQuick still offers
//! it (`--host`). It is not the default because on this stack it does not show
//! the desktop: tracing QEMU shows the guest setting a scanout and issuing a
//! steady stream of `TRANSFER_TO_HOST_2D`/`RESOURCE_FLUSH` pairs with correct
//! dirty rectangles, yet the only thing that ever lands in the host framebuffer
//! is `viogpudo`'s software-drawn mouse cursor. The desktop blit never reaches
//! the buffer the driver transfers. Capturing inside the guest sidesteps that
//! entirely, and gives per-window capture as a bonus. See docs/desktop.md.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{capability, mailbox, paths, qemu};

/// Image name for the serviced, desktop-capable Windows.
pub const IMAGE_NAME: &str =
    if cfg!(target_arch = "aarch64") { "desktop-arm64" } else { "desktop-x64" };

/// The capability name users type. Unlike the others it is built, not downloaded.
pub const CAPABILITY: &str = "desktop";

/// Marker identifying the volume carrying the bridge.
const DESK_MARKER: &str = "WQDESK.TXT";
/// Marker identifying the volume carrying the application under test.
const APP_MARKER: &str = "WQAPP.TXT";
/// The application volume is a fixed size so its filesystem identity survives
/// being refilled between sessions — the guest remembers that identity across
/// the freeze.
const APP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Windows boots to a usable desktop in about ten seconds, but a cold host
/// cache or a busy machine can stretch that. Only the one-off preparation pays
/// this; a session restores in well under a second.
const READY_TIMEOUT: Duration = Duration::from_secs(240);

/// Defaults for a desktop session.
///
/// Measured rather than guessed. Four processors is no faster than two for
/// anything a desktop session does — start, launch, UI automation, capture are
/// all within noise of each other — while taking twice as much of the host away
/// from whatever else is running. And RAM costs twice: it is the session's
/// resident size *and* most of the prepared state, which has to be read back on
/// every start. Halving 4096 to 2048 took a start from 507ms to 349ms.
pub const DEFAULT_MEMORY_MB: u32 = 2048;
pub const DEFAULT_CPUS: u32 = 2;

pub fn base_image() -> Result<PathBuf> {
    Ok(paths::root()?.join("images").join(IMAGE_NAME).join("base.qcow2"))
}

/// Everything belonging to the running session.
pub fn dir() -> Result<PathBuf> {
    Ok(paths::root()?.join("desktop"))
}

fn session_file() -> Result<PathBuf> {
    Ok(dir()?.join("session.json"))
}
fn overlay_path() -> Result<PathBuf> {
    Ok(dir()?.join("disk.qcow2"))
}
fn mailbox_path() -> Result<PathBuf> {
    Ok(dir()?.join("mailbox.img"))
}
fn vars_path() -> Result<PathBuf> {
    Ok(dir()?.join("vars.fd"))
}
fn bridge_img_path() -> Result<PathBuf> {
    Ok(dir()?.join("bridge.img"))
}
fn app_img_path() -> Result<PathBuf> {
    Ok(dir()?.join("app.img"))
}
fn control_path() -> Result<PathBuf> {
    Ok(dir()?.join("control.img"))
}
fn qmp_path() -> Result<PathBuf> {
    Ok(dir()?.join("qmp.sock"))
}
fn log_path() -> Result<PathBuf> {
    Ok(dir()?.join("session.log"))
}

/// What `winquick desktop status` reports, and how a later invocation finds the
/// running guest.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub pid: u32,
    /// Seconds since the epoch, so `status` can report an age without a clock
    /// dependency in the file format.
    pub started_unix: u64,
    pub app: Option<String>,
}

pub fn read_session() -> Option<Session> {
    let raw = std::fs::read(session_file().ok()?).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Whether the recorded process is still alive. A crashed or killed QEMU leaves
/// the session file behind, and reporting it as running would be a lie.
pub fn alive(pid: u32) -> bool {
    crate::proc::is_alive(pid)
}

pub fn running() -> Option<Session> {
    read_session().filter(|s| alive(s.pid))
}

// ---------------------------------------------------------------- lifecycle

pub struct StartOptions {
    pub app: Option<PathBuf>,
    pub memory_mb: u32,
    pub cpus: u32,
    pub verbose: bool,
}

pub fn start(opts: &StartOptions) -> Result<()> {
    let t0 = Instant::now();
    let phase = |what: &str| {
        if opts.verbose {
            eprintln!("winquick: [{:>7.0}ms] {what}", t0.elapsed().as_secs_f64() * 1000.0);
        }
    };

    let base = base_image()?;
    if !base.exists() {
        bail!(
            "the desktop capability is not installed.\n\n\
             Install it with:\n    winquick capability install desktop\n\n\
             It builds a desktop-capable Windows image from the Microsoft media you \
             already provided to `winquick setup`."
        );
    }
    if let Some(s) = running() {
        bail!(
            "a desktop session is already running (pid {}).\n\n\
             Use it, or replace it with:\n    winquick desktop stop",
            s.pid
        );
    }

    // Say everything that is still missing at once, in the order it has to be
    // done. Discovering prerequisites one failed command at a time is the kind
    // of ceremony that makes a capability feel harder than it is.
    let installed = capability::installed()?;
    let mut missing: Vec<&str> = Vec::new();
    if !installed.iter().any(|c| c.name == "dotnet-sdk") {
        missing.push(
            "    winquick capability install dotnet-sdk    # supplies the Windows Desktop runtime",
        );
    }
    if !bridge_dir()?.join("wqui.exe").exists() {
        missing
            .push("    winquick capability install desktop --force   # rebuilds the guest bridge");
    }
    if !missing.is_empty() {
        bail!(
            "a desktop session needs a little more set up first:\n\n{}\n\n\
             Then run `winquick desktop start` again.",
            missing.join("\n")
        );
    }

    // A desktop session always resumes a prepared state -- there is no cold
    // path here -- so the host's restore limit applies unconditionally.
    crate::platform::check_prepared_cpus(opts.cpus)?;

    let ctx = Ctx::new(opts, &base, &installed)?;
    let want = ctx.fingerprint()?;

    // A prepared state that no longer matches the world is discarded and
    // rebuilt rather than run: restoring RAM against a different disk is not a
    // slightly-wrong virtual machine, it is an undefined one.
    let ready = match crate::state::load_desktop_valid(&want) {
        Ok(Some(r)) => Some(r),
        Ok(None) => None,
        Err(e) => {
            if opts.verbose {
                eprintln!("winquick: {e:#}");
            }
            let _ = crate::state::discard_desktop();
            None
        }
    };

    let ready = match ready {
        Some(r) => r,
        None => {
            println!("Preparing the desktop environment. This happens once, and takes about half a minute.");
            build_prepared_state(&ctx, &want)?
        }
    };
    phase("prepared state ready");

    let d = dir()?;
    // A dead session's leftovers would otherwise be reused as if they were live.
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d)?;

    // Clones, not copies: on APFS these cost nothing whatever the size.
    let overlay = overlay_path()?;
    let vars = vars_path()?;
    let mbox = mailbox_path()?;
    let bridge_img = bridge_img_path()?;
    let app_img = app_img_path()?;
    let control_disk = control_path()?;
    qemu::clone_file(&ready.disk(), &overlay)?;
    qemu::clone_file(&ready.vars(), &vars)?;
    qemu::clone_file(&ready.mailbox(), &mbox)?;
    qemu::clone_file(&ready.bridge(), &bridge_img)?;
    qemu::clone_file(&ready.app(), &app_img)?;
    qemu::clone_file(&ready.control(), &control_disk)?;
    // This session's application goes into the volume the frozen guest already
    // knows the identity of; only the contents differ.
    if let Some(app) = opts.app.as_deref() {
        if !app.is_dir() {
            bail!("{} is not a directory", app.display());
        }
        capability::refill(&app_img, app, "app")?;
    }
    let caps = clone_capabilities(&ctx.capabilities, &d)?;
    phase("volumes cloned");

    let child = ctx.q.boot_desktop(&qemu::DesktopBoot {
        uefi_code: &ctx.uefi_code,
        uefi_vars: &vars,
        root_disk: &overlay,
        mailbox: &mbox,
        bridge: &bridge_img,
        app: &app_img,
        control: &control_disk,
        capabilities: &caps,
        memory_mb: opts.memory_mb,
        cpus: opts.cpus,
        serial_log: &log_path()?,
        qmp_socket: &qmp_path()?,
        incoming: Some(&ready.state_file()),
    })?;
    let pid = child.id();
    phase("qemu spawned");

    let session = Session {
        pid,
        started_unix: now_unix(),
        app: opts.app.as_ref().map(|p| p.display().to_string()),
    };
    std::fs::write(session_file()?, serde_json::to_vec_pretty(&session)?)?;

    let bring_up = || -> Result<()> {
        let mut q = crate::qmp::Qmp::connect(&qmp_path()?, Duration::from_secs(20))?;
        q.wait_incoming(Duration::from_secs(60))?;
        q.cont()?;
        phase("guest restored");

        // The guest was frozen with a different application volume attached, so
        // it is holding a cached directory for contents that have since been
        // replaced. This is what makes it look again.
        let r = call(&["remount".to_string()], Duration::from_secs(60))?;
        if r.exit_code != 0 {
            bail!("{}", describe_failure(&r));
        }
        phase("session ready");
        Ok(())
    };
    bring_up().inspect_err(|_| {
        let _ = stop();
    })?;

    // Ctrl-C during startup used to exit 0 and leave a session running, which
    // reads as "nothing happened" when a virtual machine is in fact up. The
    // session itself is fine, so it is kept — but say so, and exit as an
    // interrupted command should.
    if crate::interrupt::interrupted() {
        eprintln!(
            "winquick: interrupted during startup; the desktop session is running.\n\
             Stop it with:  winquick desktop stop"
        );
        std::process::exit(130);
    }

    if opts.verbose {
        eprintln!("winquick: desktop ready in {:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(())
}

/// Everything the desktop path needs to describe the machine it runs on.
struct Ctx {
    q: qemu::Qemu,
    base: PathBuf,
    uefi_code: PathBuf,
    capabilities: Vec<capability::Installed>,
    memory_mb: u32,
    cpus: u32,
    verbose: bool,
}

impl Ctx {
    fn new(opts: &StartOptions, base: &Path, installed: &[capability::Installed]) -> Result<Self> {
        Ok(Self {
            q: qemu::Qemu::locate()?,
            base: base.to_path_buf(),
            uefi_code: paths::uefi_code()
                .ok_or_else(|| anyhow!("QEMU's UEFI firmware is missing; reinstall QEMU"))?,
            capabilities: installed.to_vec(),
            memory_mb: opts.memory_mb,
            cpus: opts.cpus,
            verbose: opts.verbose,
        })
    }

    fn fingerprint(&self) -> Result<crate::state::DesktopFingerprint> {
        use crate::state::{fnv1a, FileId};
        let mut caps = Vec::new();
        for c in &self.capabilities {
            caps.push((c.name.clone(), FileId::of(&c.image)?));
        }
        Ok(crate::state::DesktopFingerprint {
            winquick_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: crate::state::PROTOCOL_VERSION,
            control_protocol_version: crate::control::PROTOCOL_VERSION,
            desktop_image: FileId::of(&self.base)?,
            agent_hash: fnv1a(crate::setup::AGENT.as_bytes()),
            bridge_hash: bridge_identity()?,
            qemu_binary: FileId::of(&self.q.system)?,
            qemu_version: self.q.version()?,
            firmware: FileId::of(&self.uefi_code)?,
            memory_mb: self.memory_mb,
            cpus: self.cpus,
            machine: qemu::MACHINE.to_string(),
            capabilities: caps,
            devices: qemu::desktop_device_signature(
                self.memory_mb,
                self.cpus,
                self.capabilities.len(),
            ),
        })
    }
}

/// Identity of the built guest bridge: every file's name and size.
///
/// A rebuilt bridge is a different program, and a prepared state is a frozen
/// guest already running the old one.
fn bridge_identity() -> Result<String> {
    let dir = bridge_dir()?;
    let mut entries: Vec<(String, u64)> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let len = e.metadata().ok()?.len();
            Some((name, len))
        })
        .collect();
    entries.sort();
    let joined: String = entries.iter().map(|(n, l)| format!("{n}:{l};")).collect();
    Ok(crate::state::fnv1a(joined.as_bytes()))
}

/// Boot a desktop guest once, wait until the bridge answers, and freeze it.
fn build_prepared_state(
    ctx: &Ctx,
    want: &crate::state::DesktopFingerprint,
) -> Result<crate::state::DesktopReady> {
    let t0 = Instant::now();
    let work = paths::work()?.join("desktop-state");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;

    let overlay = work.join("root.qcow2");
    let vars = work.join("uefi-vars.fd");
    let mbox = work.join("mailbox.img");
    let bridge_img = work.join("bridge.img");
    let app_img = work.join("app.img");
    let control_disk = work.join("control.img");
    let qmp_sock = work.join("qmp.sock");
    let serial = work.join("serial.log");

    ctx.q.create_overlay(&ctx.base, &overlay)?;
    crate::helpers::fresh_uefi_vars(&vars)?;
    mailbox::create_template(&mbox)?;
    build_bridge_volume(&bridge_img)?;
    // Empty, but the right size: sessions refill it without reformatting, so
    // the filesystem identity the frozen guest remembers stays valid.
    capability::build_sized(&app_img, Path::new("/nonexistent"), "app", APP_BYTES)?;
    capability::mark(&app_img, APP_MARKER, "app")?;
    crate::control::create(&control_disk)?;
    let caps = clone_capabilities(&ctx.capabilities, &work)?;

    let mut child = ctx.q.boot_desktop(&qemu::DesktopBoot {
        uefi_code: &ctx.uefi_code,
        uefi_vars: &vars,
        root_disk: &overlay,
        mailbox: &mbox,
        bridge: &bridge_img,
        app: &app_img,
        control: &control_disk,
        capabilities: &caps,
        memory_mb: ctx.memory_mb,
        cpus: ctx.cpus,
        serial_log: &serial,
        qmp_socket: &qmp_sock,
        incoming: None,
    })?;
    crate::interrupt::watch_child(child.id());
    let pid = child.id();

    let sdir = crate::state::desktop_state_dir()?;
    let build = (|| -> Result<crate::state::DesktopMeta> {
        let mut q = crate::qmp::Qmp::connect(&qmp_sock, Duration::from_secs(30))?;
        wait_ready(&mbox, pid, READY_TIMEOUT)?;
        if ctx.verbose {
            eprintln!("winquick: guest booted in {:.1}s", t0.elapsed().as_secs_f64());
        }
        mailbox::inject_command(&mbox, &bridge_command(&["serve".to_string()]), None, "serve")?;
        wait_serving_on(&control_disk, pid, READY_TIMEOUT)?;
        if ctx.verbose {
            eprintln!("winquick: bridge answering after {:.1}s", t0.elapsed().as_secs_f64());
        }

        q.stop()?;
        std::fs::create_dir_all(&sdir)?;
        let state_file = sdir.join("ready.state");
        let _ = std::fs::remove_file(&state_file);
        q.migrate_to_file(&state_file, Duration::from_secs(180))?;
        // Quit cleanly rather than killing: the block layer has to flush before
        // the disks we are about to clone are trustworthy.
        let _ = q.command("quit", serde_json::json!({}));
        let _ = child.wait();

        qemu::clone_file(&overlay, &sdir.join("ready-disk.qcow2"))?;
        qemu::clone_file(&vars, &sdir.join("ready-vars.fd"))?;
        qemu::clone_file(&mbox, &sdir.join("ready-mailbox.img"))?;
        qemu::clone_file(&bridge_img, &sdir.join("ready-bridge.img"))?;
        qemu::clone_file(&app_img, &sdir.join("ready-app.img"))?;
        qemu::clone_file(&control_disk, &sdir.join("ready-control.img"))?;

        let meta = crate::state::DesktopMeta {
            fingerprint: want.clone(),
            created_unix: now_unix(),
            state_bytes: std::fs::metadata(&state_file)?.len(),
        };
        crate::state::save_desktop(&meta)?;
        if ctx.verbose {
            eprintln!(
                "winquick: prepared desktop state built in {:.1}s ({:.0} MiB)",
                t0.elapsed().as_secs_f64(),
                meta.state_bytes as f64 / (1024.0 * 1024.0)
            );
        }
        Ok(meta)
    })();

    let _ = child.kill();
    let _ = child.wait();
    crate::interrupt::clear_child();
    let _ = std::fs::remove_dir_all(&work);

    match build {
        Ok(meta) => Ok(crate::state::DesktopReady { dir: sdir, meta }),
        Err(e) => {
            let _ = crate::state::discard_desktop();
            Err(e)
        }
    }
}

/// Build the volume the bridge runs from.
fn build_bridge_volume(image: &Path) -> Result<()> {
    let src = bridge_dir()?;
    if !src.join("wqui.exe").exists() {
        bail!(
            "the desktop bridge is missing from {}.\n\n\
             Rebuild it with:\n    winquick capability install desktop --force",
            src.display()
        );
    }
    let staging = paths::work()?.join("bridge-volume");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    copy_tree(&src, &staging.join("bridge"))?;
    std::fs::write(staging.join(DESK_MARKER), b"winquick-desktop\r\n")?;
    capability::build_flat(image, &staging)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

fn wait_ready(mbox: &Path, pid: u32, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if mailbox::probe(mbox, mailbox::READY).is_some() {
            return Ok(());
        }
        if !alive(pid) {
            bail!(
                "the desktop guest exited before it was ready.\n\n\
                 Its console output is in {}",
                log_path()?.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!(
        "the desktop guest did not become ready within {}s.\n\n\
         Its console output is in {}",
        timeout.as_secs(),
        log_path()?.display()
    )
}

/// Wait until the bridge answers on a control disk.
///
/// Takes the disk rather than assuming the running session's, because it is
/// used both while preparing a state and while bringing a session up.
fn wait_serving_on(disk: &Path, pid: u32, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last: Option<String> = None;
    while Instant::now() < deadline {
        if !alive(pid) {
            bail!(
                "the desktop guest exited while the bridge was starting.\n\n\
                 Its console output is in {}",
                log_path()?.display()
            );
        }
        let attempt = crate::control::Channel::open(disk).and_then(|mut c| {
            c.call(&["windows".to_string()], Duration::from_secs(5), || alive(pid))
        });
        match attempt {
            Ok(r) if r.exit_code == 0 => return Ok(()),
            Ok(r) => last = Some(String::from_utf8_lossy(&r.body).trim().to_string()),
            Err(e) => last = Some(format!("{e:#}")),
        }
    }

    // The agent captured whatever the bridge printed before it gave up. Without
    // this the only symptom is silence, which says nothing about whether the
    // bridge crashed, never ran, or ran and could not find its control disk.
    let mut detail = String::new();
    if let Ok(mbox) = mailbox_path() {
        for (label, file) in [("stdout", mailbox::OUT_FILE), ("stderr", mailbox::ERR_FILE)] {
            if let Some(raw) = mailbox::probe(&mbox, file) {
                let text = String::from_utf8_lossy(&raw).replace('\r', "");
                if !text.trim().is_empty() {
                    detail.push_str(&format!("\n\nBridge {label}:\n{}", text.trim()));
                }
            }
        }
    }
    bail!(
        "the desktop bridge did not start within {}s{}{detail}\n\nThe guest's console output is in {}",
        timeout.as_secs(),
        match last {
            Some(m) => format!("\n\nLast error: {m}"),
            None => String::new(),
        },
        log_path()?.display()
    )
}

pub fn stop() -> Result<bool> {
    let existed = match read_session() {
        Some(s) => {
            if alive(s.pid) {
                crate::proc::terminate(s.pid);
                // Give QEMU a moment to go on its own before insisting.
                for _ in 0..50 {
                    if !alive(s.pid) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if alive(s.pid) {
                    crate::proc::force_kill(s.pid);
                }
            }
            true
        }
        None => false,
    };
    let _ = std::fs::remove_dir_all(dir()?);
    Ok(existed)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ------------------------------------------------------------------- verbs

pub struct CallResult {
    pub json: Option<Value>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

/// Verbs the guest bridge understands.
///
/// Kept here so an unrecognised verb is a syntax error the CLI can report
/// immediately, without first requiring a session to exist.
pub const VERBS: &[&str] = &[
    "windows",
    "display",
    "launch",
    "wait-window",
    "focus",
    "screenshot",
    "pull",
    "tree",
    "find",
    "get",
    "click",
    "type",
    "key",
    "select",
    "toggle",
    "mouse",
    "remount",
];

/// What each forwarded verb does, and the options it takes.
///
/// The bridge validates options against one shared table, so its error message
/// for a bad option lists every option of every verb. That is no use to someone
/// trying to find out what `toggle` accepts, and `--help` reached the bridge as
/// just another unknown option — so a verb's help was unobtainable without a
/// booted Windows, which is exactly backwards for the thing you read *before*
/// starting one.
///
/// Kept next to `VERBS` and pinned to it by a test, so a new verb cannot ship
/// undocumented.
pub const VERB_HELP: &[(&str, &str, &str)] = &[
    ("windows", "List the visible top-level windows", ""),
    ("display", "Report the screen size and colour depth", ""),
    ("launch", "Start a program inside the session", "<program> [args...] [--cwd <dir>]"),
    ("wait-window", "Wait until a window appears", "--title <text> [--timeout <ms>] [--poll <ms>]"),
    ("focus", "Bring a window to the front", "--title <text> | --hwnd <n>"),
    ("tree", "Print the UI Automation tree", "[<selector>] [--depth <n>]"),
    ("find", "List every element matching a selector", "<selector> [--all]"),
    (
        "screenshot",
        "Capture the screen, or one window, as a PNG",
        "<file> [--title <text>] [--hwnd <n>] [--rect x,y,w,h]",
    ),
    (
        "pull",
        "Copy a file the application produced back to this machine",
        "<guest-path> <local-file>   (e.g. app\\out\\page.png)",
    ),
    ("get", "Read one element", "<selector>"),
    ("click", "Click one element", "<selector> [--right] [--settle <ms>]"),
    ("type", "Type text into one element", "<selector> --text <text>"),
    ("key", "Send keystrokes to whatever has focus", "--key <combo>  (e.g. ctrl+a, enter, tab)"),
    ("select", "Choose an item in a list or combo box", "<selector> --item <text>"),
    ("toggle", "Set or flip a check box", "<selector> [--state on|off]"),
    ("mouse", "Move or click at raw screen coordinates", "--x <n> --y <n> [--move] [--right]"),
    ("remount", "Re-read the app volume after it changed", ""),
];

/// The selector vocabulary, shared by every verb that addresses an element.
const SELECTOR_HELP: &str = "\
A <selector> is one or more of:
    --automation-id <id>     the AutomationId, and the one to prefer
    --name <text>            the accessible name
    --class <name>           the class name
    --control-type <type>    Button, Edit, Text, CheckBox, ComboBox, List ...
    --title <text>           limit the search to one window
    --hwnd <n>               ...or to one window by handle

Combine them to narrow a match. A selector matching more than one element is
an error rather than a guess.";

/// Help for one forwarded verb, or `None` if it is not one.
pub fn verb_help(verb: &str) -> Option<String> {
    let (_, what, opts) = VERB_HELP.iter().find(|(v, _, _)| *v == verb)?;
    let mut s = format!("{what}\n\nUsage: winquick desktop {verb}");
    if !opts.is_empty() {
        s.push(' ');
        s.push_str(opts);
    }
    if opts.contains("<selector>") {
        s.push_str("\n\n");
        s.push_str(SELECTOR_HELP);
    }
    s.push_str("\n\nRuns against the session started by `winquick desktop start`.");
    Some(s)
}

/// Reject an unknown verb before anything looks at session state.
pub fn check_verb(verb: Option<&str>) -> Result<()> {
    let Some(v) = verb else {
        bail!("no desktop command given. Try:  winquick desktop --help");
    };
    if VERBS.contains(&v) {
        return Ok(());
    }
    let near: Vec<&str> =
        VERBS.iter().copied().filter(|k| k.starts_with(v.chars().next().unwrap_or('\0'))).collect();
    let hint = if near.is_empty() {
        String::new()
    } else {
        format!("\n\nDid you mean: {}?", near.join(", "))
    };
    bail!(
        "unknown desktop command `{v}`.\n\nAvailable: start, stop, status, {}{hint}",
        VERBS.join(", ")
    )
}

/// Run one bridge verb in the live session and return its JSON.
pub fn call(argv: &[String], timeout: Duration) -> Result<CallResult> {
    let session = running().ok_or_else(|| {
        anyhow!("no desktop session is running.\n\nStart one with:\n    winquick start")
    })?;
    let mut channel = crate::control::Channel::open(&control_path()?)?;
    let pid = session.pid;
    let r = channel.call(argv, timeout, move || alive(pid))?;
    let json = serde_json::from_slice::<serde_json::Value>(&r.body).ok();
    Ok(CallResult { json, stdout: r.body, stderr: Vec::new(), exit_code: r.exit_code })
}

/// Wrap a bridge invocation in the volume probe the guest needs.
///
/// Drive letters are not stable, so the command finds the volume by its marker
/// rather than assuming one, and then makes that volume the working directory
/// so `launch app\\MyApp.exe` means what it looks like it means.
fn bridge_command(argv: &[String]) -> String {
    let quoted = crate::argv::join(argv);
    format!(
        "set WQX=\r\n\
         for %%d in (D E F G H I J K L M N O P) do if not defined WQX \
         if exist %%d:\\{DESK_MARKER} set WQX=%%d:\r\n\
         if not defined WQX (echo {{\"ok\":false,\"error\":\"desktop volume not attached\"}} & exit /b 3)\r\n\
         cd /d %WQX%\\\r\n\
         \"%WQX%\\bridge\\wqui.exe\" {quoted}"
    )
}

/// Capture the screen (or one window) and write the PNG to the host.
///
/// The guest writes into the mailbox, which the agent dismounts after every
/// command, so the bytes are on the host's disk by the time this returns.
pub fn screenshot(dest: &Path, extra: &[String], timeout: Duration) -> Result<Value> {
    // "-" asks the bridge to hand the image back through the control channel;
    // a session has no shared filesystem to leave it on.
    let mut argv = vec!["screenshot".to_string(), "-".to_string()];
    argv.extend_from_slice(extra);
    let r = call(&argv, timeout)?;

    let mut json = r.json.clone().unwrap_or(Value::Null);
    if r.exit_code != 0 || json.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("{}", describe_failure(&r));
    }
    let encoded = json
        .get("pngBase64")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the guest reported a capture but sent no image"))?;
    let png = base64_decode(encoded)?;

    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(dest, &png).with_context(|| format!("writing {}", dest.display()))?;

    // The image itself is on disk now; leaving a megabyte of base64 in the
    // reported JSON helps nobody.
    if let Some(obj) = json.as_object_mut() {
        obj.remove("pngBase64");
        obj.insert("path".into(), Value::String(dest.display().to_string()));
        obj.insert("bytes".into(), Value::from(png.len()));
    }
    Ok(json)
}

/// Bring a file the application produced back to this machine.
///
/// A session could already show you a picture of what happened but not give you
/// the thing that happened — the converted page, the exported report, the log
/// the application wrote. There is no shared filesystem, so the bytes come back
/// the way a screenshot does.
///
/// A relative guest path is read exactly as `launch` reads a program name, so
/// `app\out\page.png` names a file the application wrote beside itself.
pub fn pull(guest_path: &str, dest: &Path, timeout: Duration) -> Result<Value> {
    let argv = vec!["pull".to_string(), guest_path.to_string()];
    let r = call(&argv, timeout)?;

    let mut json = r.json.clone().unwrap_or(Value::Null);
    if r.exit_code != 0 || json.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("{}", describe_failure(&r));
    }
    let encoded = json
        .get("contentBase64")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the guest reported the file but sent no contents"))?;
    let bytes = base64_decode(encoded)?;

    // The guest hashed what it read. Checking it here is the difference between
    // "a file arrived" and "this file arrived intact".
    if let Some(want) = json.get("sha256").and_then(Value::as_str) {
        let got = sha256_hex(&bytes);
        if got != want {
            bail!(
                "{guest_path} arrived corrupted: the guest hashed {want}, this machine has {got}"
            );
        }
    }

    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;

    if let Some(obj) = json.as_object_mut() {
        obj.remove("contentBase64");
        obj.insert("local".into(), Value::String(dest.display().to_string()));
    }
    Ok(json)
}

/// SHA-256 of what came back, to compare against what the guest hashed.
fn sha256_hex(data: &[u8]) -> String {
    crate::sha256::hex_of(data)
}

/// Standard base64, which is how the bridge sends an image back.
fn base64_decode(s: &str) -> Result<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = value(c).ok_or_else(|| anyhow!("the image the guest sent is not valid base64"))?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Turn a failed call into something worth reading.
pub fn describe_failure(r: &CallResult) -> String {
    if let Some(msg) = r.json.as_ref().and_then(|j| j.get("error")).and_then(Value::as_str) {
        return msg.to_string();
    }
    let err = String::from_utf8_lossy(&r.stderr);
    let out = String::from_utf8_lossy(&r.stdout);
    let detail = if !err.trim().is_empty() { err } else { out };
    if detail.trim().is_empty() {
        format!("the bridge failed with exit code {}", r.exit_code)
    } else {
        detail.trim().to_string()
    }
}

// ------------------------------------------------------- the files volume

/// Give the session its own copy of every capability volume.
///
/// Cloned, never shared. Windows writes to a volume when it mounts one, so
/// attaching the installed images directly would leave the session's
/// fingerprints on them. On APFS the clone is free regardless of size.
fn clone_capabilities(installed: &[capability::Installed], dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (i, c) in installed.iter().enumerate() {
        let dst = dir.join(format!("cap{i}.img"));
        qemu::clone_file(&c.image, &dst)
            .with_context(|| format!("cloning the {} volume", c.name))?;
        out.push(dst);
    }
    Ok(out)
}

/// Where the built bridge lives between installs.
pub fn bridge_dir() -> Result<PathBuf> {
    Ok(paths::root()?.join("desktop-bridge"))
}

pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
        }
    }
    Ok(())
}

/// Capture QEMU's own framebuffer through QMP.
///
/// Offered for completeness and for diagnosing the display path. On this stack
/// it does not show the Windows desktop — see the note at the top of this
/// module — so `winquick desktop screenshot` captures inside the guest unless
/// `--host` asks otherwise.
pub fn host_screenshot(dest: &Path) -> Result<u64> {
    let session = running().ok_or_else(|| {
        anyhow!("no desktop session is running.\n\nStart one with:\n    winquick start")
    })?;
    let _ = session;

    let mut qmp = crate::qmp::Qmp::connect(&qmp_path()?, Duration::from_secs(10))?;
    let target = std::fs::canonicalize(
        dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")),
    )
    .unwrap_or_else(|_| PathBuf::from("."))
    .join(dest.file_name().unwrap_or_else(|| std::ffi::OsStr::new("screen.ppm")));

    // Newer QEMU can encode PNG directly; older builds only write PPM. Ask for
    // PNG and fall back rather than silently producing a file with the wrong
    // contents for its extension.
    let png = qmp.command(
        "screendump",
        serde_json::json!({ "filename": target.to_string_lossy(), "format": "png" }),
    );
    if png.is_err() {
        qmp.command("screendump", serde_json::json!({ "filename": target.to_string_lossy() }))
            .context("asking QEMU for a framebuffer dump")?;
        eprintln!(
            "winquick: this QEMU cannot encode PNG; {} is a PPM despite its name",
            target.display()
        );
    }
    Ok(std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0))
}

// ------------------------------------------------------------- ui scripts

/// What a script run produced.
pub struct Report {
    pub passed: usize,
    pub failed: Vec<String>,
    pub screenshots: Vec<PathBuf>,
}

/// Run a parsed UI script against the live session.
///
/// Every step is reported as it happens, because a UI test that fails on step
/// nine is only useful if you can see what the first eight did.
pub fn run_script(
    script: &crate::uiscript::Script,
    out_dir: &Path,
    timeout: Duration,
) -> Result<Report> {
    use crate::uiscript::Step;

    std::fs::create_dir_all(out_dir)?;
    let mut report = Report { passed: 0, failed: Vec::new(), screenshots: Vec::new() };

    for (n, step) in script.steps.iter().enumerate() {
        let label = format!("{:>3}. ", n + 1);
        match step {
            Step::Sleep { ms } => {
                println!("{label}sleep {ms}ms");
                std::thread::sleep(Duration::from_millis(*ms));
                report.passed += 1;
            }

            Step::Screenshot { file, args } => {
                let dest = out_dir.join(file);
                match screenshot(&dest, args, timeout) {
                    Ok(json) => {
                        let non_black =
                            json.get("nonBlackFraction").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        println!(
                            "{label}screenshot {file} ({}x{}, {:.0}% non-black)",
                            json.get("width").and_then(|v| v.as_i64()).unwrap_or(0),
                            json.get("height").and_then(|v| v.as_i64()).unwrap_or(0),
                            non_black * 100.0
                        );
                        report.screenshots.push(dest);
                        report.passed += 1;
                    }
                    Err(e) => {
                        println!("{label}screenshot {file} FAILED: {e:#}");
                        report.failed.push(format!("screenshot {file}: {e:#}"));
                    }
                }
            }

            Step::Expect { selector, check } => {
                let mut argv = vec!["get".to_string()];
                argv.extend(selector.iter().cloned());
                let what = selector.join(" ");
                match call(&argv, timeout) {
                    Ok(r) if r.exit_code == 0 => {
                        let raw = r
                            .json
                            .as_ref()
                            .and_then(|j| j.get("element"))
                            .and_then(|e| e.get(check.field.json_key()))
                            .filter(|v| !v.is_null());
                        // Distinguish "the property says empty" from "the
                        // element has no such property"; the second reads as an
                        // application bug when it is a misaimed assertion.
                        let Some(raw) = raw else {
                            let msg = format!(
                                "expect {what}: no {} on this element ({})",
                                check.field.json_key(),
                                check.field.hint()
                            );
                            println!("{label}{msg}  FAILED");
                            report.failed.push(msg);
                            continue;
                        };
                        let actual = match raw.as_str() {
                            Some(s) => s.to_string(),
                            None => raw.to_string(),
                        };
                        let ok = if check.contains {
                            actual.contains(&check.expected)
                        } else {
                            actual == check.expected
                        };
                        if ok {
                            println!(
                                "{label}expect {what} {} = {:?}  OK",
                                check.field.json_key(),
                                actual
                            );
                            report.passed += 1;
                        } else {
                            let msg = format!(
                                "expect {what}: {} was {:?}, wanted {}{:?}",
                                check.field.json_key(),
                                actual,
                                if check.contains { "something containing " } else { "" },
                                check.expected
                            );
                            println!("{label}{msg}  FAILED");
                            report.failed.push(msg);
                        }
                    }
                    Ok(r) => {
                        let msg = format!("expect {what}: {}", describe_failure(&r));
                        println!("{label}{msg}  FAILED");
                        report.failed.push(msg);
                    }
                    Err(e) => {
                        let msg = format!("expect {what}: {e:#}");
                        println!("{label}{msg}  FAILED");
                        report.failed.push(msg);
                    }
                }
            }

            Step::Bridge(argv) => {
                let what = argv.join(" ");
                match call(argv, timeout) {
                    Ok(r) if r.exit_code == 0 => {
                        println!("{label}{what}  OK{}", summarise(&r));
                        report.passed += 1;
                    }
                    Ok(r) => {
                        let msg = format!("{what}: {}", describe_failure(&r));
                        println!("{label}{msg}  FAILED");
                        report.failed.push(msg);
                    }
                    Err(e) => {
                        let msg = format!("{what}: {e:#}");
                        println!("{label}{msg}  FAILED");
                        report.failed.push(msg);
                    }
                }
            }
        }
    }
    Ok(report)
}

/// A short tail for a successful step, so the log shows what actually happened.
fn summarise(r: &CallResult) -> String {
    let Some(j) = &r.json else { return String::new() };
    for key in ["pid", "waitedMs", "via", "typed", "selected", "count"] {
        if let Some(v) = j.get(key) {
            let text = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
            return format!(" ({key} {text})");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    /// Every forwarded verb must be documented, or `--help` falls through to
    /// the guest again and becomes unanswerable without a booted Windows.
    #[test]
    fn every_verb_has_help() {
        for v in super::VERBS {
            assert!(super::verb_help(v).is_some(), "verb `{v}` has no help");
        }
        for (v, _, _) in super::VERB_HELP {
            assert!(super::VERBS.contains(v), "`{v}` has help but is not a verb");
        }
    }

    /// A verb that takes a selector explains what a selector is.
    #[test]
    fn selector_verbs_explain_the_selector() {
        let h = super::verb_help("click").unwrap();
        assert!(h.contains("--automation-id"), "click help omits the selector: {h}");
        assert!(super::verb_help("display").unwrap().contains("winquick desktop display"));
        assert!(super::verb_help("nonsense").is_none());
    }
}
