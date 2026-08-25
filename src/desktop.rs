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
pub const IMAGE_NAME: &str = "desktop-arm64";

/// The capability name users type. Unlike the others it is built, not downloaded.
pub const CAPABILITY: &str = "desktop";

/// Marker identifying the volume carrying the bridge and the application.
const DESK_MARKER: &str = "WQDESK.TXT";

/// Windows boots to a usable desktop in about seven seconds, but a cold host
/// cache or a busy machine can stretch that.
const READY_TIMEOUT: Duration = Duration::from_secs(240);

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
fn files_path() -> Result<PathBuf> {
    Ok(dir()?.join("files.img"))
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
    // Signal 0 checks for existence and permission without delivering anything.
    unsafe { kill(pid as i32, 0) == 0 }
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
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

    let d = dir()?;
    // A dead session's leftovers would otherwise be reused as if they were live.
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d)?;

    let q = qemu::Qemu::locate()?;
    let overlay = overlay_path()?;
    q.create_overlay(&base, &overlay)
        .context("creating the session's disposable disk")?;

    let mbox = mailbox_path()?;
    mailbox::create_template(&mbox)?;

    let uefi_code = paths::uefi_code()
        .ok_or_else(|| anyhow!("QEMU's UEFI firmware is missing; reinstall QEMU"))?;
    let vars = vars_path()?;
    // The variable store must be writable or Windows will not boot at all.
    std::fs::File::create(&vars)?.set_len(64 * 1024 * 1024)?;

    let files = files_path()?;
    build_files_volume(&files, opts.app.as_deref())?;

    let control_disk = control_path()?;
    crate::control::create(&control_disk)?;

    let caps: Vec<PathBuf> = capability::installed()?.into_iter().map(|c| c.image).collect();
    if !caps.iter().any(|c| {
        c.file_stem().map(|s| s == "dotnet-sdk").unwrap_or(false)
    }) {
        bail!(
            "the desktop bridge needs the .NET SDK capability, which supplies the\n\
             Windows Desktop runtime the bridge and WPF applications run on.\n\n\
             Install it with:\n    winquick capability install dotnet-sdk"
        );
    }

    let cfg = qemu::DesktopBoot {
        uefi_code: &uefi_code,
        uefi_vars: &vars,
        root_disk: &overlay,
        mailbox: &mbox,
        files: &files,
        control: &control_disk,
        capabilities: &caps,
        memory_mb: opts.memory_mb,
        cpus: opts.cpus,
        serial_log: &log_path()?,
        qmp_socket: &qmp_path()?,
    };
    // Deliberately not registered with the interrupt handler: this process is
    // meant to outlive the CLI invocation that started it.
    let child = q.boot_desktop(&cfg)?;
    let pid = child.id();

    let session = Session {
        pid,
        started_unix: now_unix(),
        app: opts.app.as_ref().map(|p| p.display().to_string()),
    };
    std::fs::write(session_file()?, serde_json::to_vec_pretty(&session)?)?;

    if opts.verbose {
        eprintln!("winquick: desktop guest started as pid {pid}; waiting for the agent");
    }
    let started = Instant::now();
    let bring_up = || -> Result<()> {
        wait_ready(&mbox, pid, READY_TIMEOUT)?;
        if opts.verbose {
            eprintln!("winquick: guest booted in {:.1}s; starting the bridge",
                started.elapsed().as_secs_f64());
        }
        // One command through the mailbox, before anything else touches it: run
        // the bridge as a server on the control disk. The agent blocks inside it
        // and never polls the mailbox again, which is what keeps the host and
        // the guest from writing to that filesystem at the same time.
        mailbox::inject_command(&mbox, &bridge_command(&["serve".to_string()]), None, "serve")?;
        wait_serving(pid, READY_TIMEOUT)?;
        Ok(())
    };
    bring_up().inspect_err(|_| {
        let _ = stop();
    })?;
    if opts.verbose {
        eprintln!("winquick: desktop ready in {:.1}s", started.elapsed().as_secs_f64());
    }
    Ok(())
}

/// Wait until the bridge answers on the control channel.
fn wait_serving(pid: u32, timeout: Duration) -> Result<()> {
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
        match call(&["windows".to_string()], Duration::from_secs(5)) {
            Ok(r) if r.exit_code == 0 => return Ok(()),
            Ok(r) => last = Some(describe_failure(&r)),
            Err(e) => last = Some(format!("{e:#}")),
        }
    }
    bail!(
        "the desktop bridge did not start within {}s{}",
        timeout.as_secs(),
        match last {
            Some(m) => format!("\n\nLast error: {m}"),
            None => String::new(),
        }
    )
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

pub fn stop() -> Result<bool> {
    let existed = match read_session() {
        Some(s) => {
            if alive(s.pid) {
                unsafe { kill(s.pid as i32, 15) };
                // Give QEMU a moment to go on its own before insisting.
                for _ in 0..50 {
                    if !alive(s.pid) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if alive(s.pid) {
                    unsafe { kill(s.pid as i32, 9) };
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

/// Run one bridge verb in the live session and return its JSON.
pub fn call(argv: &[String], timeout: Duration) -> Result<CallResult> {
    let session = running().ok_or_else(|| {
        anyhow!(
            "no desktop session is running.\n\nStart one with:\n    winquick desktop start"
        )
    })?;
    let mut channel = crate::control::Channel::open(&control_path()?)?;
    let pid = session.pid;
    let r = channel.call(argv, timeout, move || alive(pid))?;
    let json = serde_json::from_slice::<serde_json::Value>(&r.body).ok();
    Ok(CallResult {
        json,
        stdout: r.body,
        stderr: Vec::new(),
        exit_code: r.exit_code,
    })
}

/// Wrap a bridge invocation in the volume probe the guest needs.
///
/// Drive letters are not stable, so the command finds the volume by its marker
/// rather than assuming one, and then makes that volume the working directory
/// so `launch app\\MyApp.exe` means what it looks like it means.
fn bridge_command(argv: &[String]) -> String {
    let quoted = crate::join_argv(argv);
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

/// Build the volume carrying the bridge and, optionally, an application.
///
/// The bridge is copied out of the installed desktop capability so a session
/// never depends on anything outside `~/.winquick`.
fn build_files_volume(image: &Path, app: Option<&Path>) -> Result<()> {
    let staging = dir()?.join("files");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    let bridge_src = bridge_dir()?;
    if !bridge_src.join("wqui.exe").exists() {
        bail!(
            "the desktop bridge is missing from {}.\n\n\
             Rebuild it with:\n    winquick capability install desktop --force",
            bridge_src.display()
        );
    }
    copy_tree(&bridge_src, &staging.join("bridge"))?;

    if let Some(app) = app {
        if !app.is_dir() {
            bail!("{} is not a directory", app.display());
        }
        copy_tree(app, &staging.join("app"))?;
    }
    std::fs::write(staging.join(DESK_MARKER), b"winquick-desktop\r\n")?;

    capability::build_flat(image, &staging)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
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
            std::fs::copy(&from, &to)
                .with_context(|| format!("copying {}", from.display()))?;
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
        anyhow!("no desktop session is running.\n\nStart one with:\n    winquick desktop start")
    })?;
    let _ = session;

    let mut qmp = crate::qmp::Qmp::connect(&qmp_path()?, Duration::from_secs(10))?;
    let target = std::fs::canonicalize(dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")))
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
        qmp.command(
            "screendump",
            serde_json::json!({ "filename": target.to_string_lossy() }),
        )
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
                        let actual = r
                            .json
                            .as_ref()
                            .and_then(|j| j.get("element"))
                            .and_then(|e| e.get(check.field.json_key()))
                            .map(|v| match v.as_str() {
                                Some(s) => s.to_string(),
                                None => v.to_string(),
                            })
                            .unwrap_or_default();
                        let ok = if check.contains {
                            actual.contains(&check.expected)
                        } else {
                            actual == check.expected
                        };
                        if ok {
                            println!("{label}expect {what} {} = {:?}  OK",
                                check.field.json_key(), actual);
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
