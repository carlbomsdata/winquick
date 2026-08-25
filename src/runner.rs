//! `winquick run` — the whole product, in one function.
//!
//! Two paths to the same guarantee:
//!
//! * **warm** — clone a frozen, already-booted guest, resume it, hand it the
//!   command. About a fifth of a second.
//! * **cold** — boot Windows from the base image. About eight and a half
//!   seconds, and used only when there is no usable frozen guest, or when the
//!   warm path fails.
//!
//! The cold path also *builds* the frozen guest, so the slow route happens once
//! and pays for every run after it. Nothing about this is visible to the user:
//! both paths take a command and return stdout, stderr and an exit code.

use crate::{mailbox, paths, qemu, qmp, state};
use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long to wait for a freshly booted guest to announce itself.
const READY_TIMEOUT: Duration = Duration::from_secs(90);

/// Fixed size for the workspace volume. Constant across runs so the FAT volume
/// identity the guest remembers keeps resolving; sparse, so an unused one costs
/// almost nothing.
const WORKSPACE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Artifact volume. Sparse, so an unused one costs nothing.
const ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Defaults shared by the CLI and by everything that runs a command on its
/// behalf. They are part of the prepared guest's fingerprint, so a caller that
/// picks different ones silently forces a rebuild for everyone else.
pub const DEFAULT_MEMORY_MB: u32 = 1024;
pub const DEFAULT_CPUS: u32 = 4;

pub struct Options {
    pub memory_mb: u32,
    pub cpus: u32,
    pub timeout: Duration,
    pub verbose: bool,
    /// Skip the warm path entirely. For benchmarking and for `--cold`.
    pub force_cold: bool,
    /// Host directory to expose to the guest at `C:\workspace`.
    pub workspace: Option<PathBuf>,
    /// Patterns, relative to the workspace root, to retrieve after the command.
    pub artifacts: Vec<String>,
    pub artifacts_dir: PathBuf,
    pub artifact_overwrite: bool,
}

/// Deletes the run directory no matter how we leave — normal exit, error, or
/// panic. A run that leaves state behind is a bug: the promise is that the
/// environment is discarded.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("WINQUICK_KEEP").is_some() {
            eprintln!("winquick: keeping {}", self.0.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Ctx {
    q: qemu::Qemu,
    base: PathBuf,
    uefi_code: PathBuf,
    /// Capability volumes to attach, in a deterministic order.
    capabilities: Vec<crate::capability::Installed>,
    opts_memory: u32,
    opts_cpus: u32,
    timeout: Duration,
    verbose: bool,
    workspace: Option<PathBuf>,
    artifacts: Vec<String>,
    artifacts_dir: PathBuf,
}

impl Ctx {
    fn vlog(&self, msg: impl std::fmt::Display) {
        if self.verbose {
            eprintln!("winquick: {msg}");
        }
    }
}

pub struct Outcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub warm: bool,
    /// The command line as sent to the guest, for diagnostics.
    pub command: String,
}

/// Pull requested files off the artifact volume. Runs whether or not the command
/// succeeded — a failed build's logs are usually exactly what is wanted — and
/// before the run directory is deleted.
fn collect_artifacts(ctx: &Ctx, image: &Path) -> Result<()> {
    if ctx.artifacts.is_empty() {
        return Ok(());
    }
    let t = Instant::now();
    let got = crate::artifact::extract(image, &ctx.artifacts_dir)?;
    if got.log.contains("winquick-artifact-status=1") {
        bail!(
            "the guest could not copy some requested artifacts:\n{}",
            got.log.trim()
        );
    }
    if got.files == 0 {
        eprintln!(
            "winquick: no files matched {} — nothing written to {}",
            ctx.artifacts.join(", "),
            ctx.artifacts_dir.display()
        );
    } else {
        eprintln!(
            "winquick: retrieved {} file{} ({:.1} MiB) into {}",
            got.files,
            if got.files == 1 { "" } else { "s" },
            got.bytes as f64 / (1024.0 * 1024.0),
            ctx.artifacts_dir.display()
        );
        ctx.vlog(format!(
            "artifact extraction {:.0}ms",
            t.elapsed().as_secs_f64() * 1000.0
        ));
    }
    Ok(())
}

/// Execute and return the outcome without printing it. Used by `setup`'s smoke
/// test, which wants to inspect the result rather than emit it.
pub fn run_capture(command: &str, opts: &Options) -> Result<Outcome> {
    execute(command, opts)
}

pub fn run(command: &str, opts: &Options) -> Result<i32> {
    let t_start = Instant::now();
    let o = execute(command, opts)?;
    emit(o, t_start, opts.verbose)
}

fn execute(command: &str, opts: &Options) -> Result<Outcome> {
    let t_start = Instant::now();
    let base = paths::base_image()?;
    if !base.exists() {
        bail!(
            "No Windows runtime is installed yet.\n\nSet one up with:\n    winquick setup"
        );
    }
    let uefi_code = paths::uefi_code()
        .ok_or_else(|| anyhow!("could not find edk2-aarch64-code.fd next to QEMU"))?;
    let capabilities = crate::capability::installed()?;
    let ctx = Ctx {
        q: qemu::Qemu::locate()?,
        base,
        uefi_code,
        capabilities,
        opts_memory: opts.memory_mb,
        opts_cpus: opts.cpus,
        timeout: opts.timeout,
        verbose: opts.verbose,
        workspace: opts.workspace.clone(),
        artifacts: opts.artifacts.clone(),
        artifacts_dir: opts.artifacts_dir.clone(),
    };
    if !ctx.artifacts.is_empty() {
        crate::artifact::prepare_dest(&ctx.artifacts_dir, opts.artifact_overwrite)?;
    }
    state::check_base_meta(&ctx.base, crate::setup::AGENT)?;
    let want = fingerprint(&ctx)?;
    ctx.vlog(format!("host startup {:.0}ms", t_start.elapsed().as_secs_f64() * 1000.0));

    if !opts.force_cold {
        match state::load_valid(&want) {
            Ok(Some(ready)) => {
                ctx.vlog("using existing ready state");
                match warm_execute(&ctx, &ready, command) {
                    Ok(o) => return Ok(o),
                    Err(e) if crate::interrupt::interrupted() => return Err(e),
                    Err(e) => {
                        ctx.vlog(format!("warm path failed: {e:#}"));
                        ctx.vlog("discarding ready state and falling back to cold boot");
                        let _ = state::discard();
                    }
                }
            }
            Ok(None) => ctx.vlog("no ready state yet"),
            Err(e) => {
                ctx.vlog(format!("{e:#}"));
                let _ = state::discard();
            }
        }
    } else {
        ctx.vlog("--cold: skipping the warm path");
    }

    // Cold. Build a ready state first so future runs are fast, then use it — which
    // doubles as verification that the state we just wrote actually works.
    //
    // The lock is held across both the re-check and the build: several runs can
    // start at once with nothing prepared, and a run must never read a ready
    // state that another process is still writing.
    if !opts.force_cold {
        match crate::lock::acquire_build(Duration::from_secs(600))? {
            Some(_guard) => {
                // Someone may have built it while we waited.
                if let Ok(Some(ready)) = state::load_valid(&want) {
                    ctx.vlog("another run prepared the guest while we waited");
                    match warm_execute(&ctx, &ready, command) {
                        Ok(o) => return Ok(o),
                        Err(e) if crate::interrupt::interrupted() => return Err(e),
                        Err(e) => ctx.vlog(format!("that prepared guest did not work: {e:#}")),
                    }
                }
                match build_ready_state(&ctx, &want) {
                    Ok(ready) => match warm_execute(&ctx, &ready, command) {
                        Ok(o) => return Ok(o),
                        Err(e) if crate::interrupt::interrupted() => return Err(e),
                        Err(e) => {
                            ctx.vlog(format!("newly built ready state did not work: {e:#}"));
                            let _ = state::discard();
                        }
                    },
                    Err(e) if crate::interrupt::interrupted() => return Err(e),
                    Err(e) => ctx.vlog(format!("could not build a ready state: {e:#}")),
                }
            }
            None => ctx.vlog("gave up waiting for another run to prepare the guest"),
        }
    }

    // Last resort: boot and run, no state involved. This is the path that must
    // never fail for reasons of its own.
    cold_execute(&ctx, command)
}

/// The guest has no network on purpose, so a package that is not in the cache
/// fails with a DNS error that says nothing useful about how to fix it.
fn nuget_hint(o: &Outcome) -> Option<String> {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    if !text.contains("NU1301") {
        return None;
    }
    let missing: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split("Unable to find package ").nth(1))
        .filter_map(|l| l.split('.').next())
        .collect();
    let what = if missing.is_empty() {
        "A required NuGet package is not in the cache".to_string()
    } else {
        format!("NuGet package {} is not in the cache", missing[0].trim())
    };
    Some(format!(
        "\nwinquick: {what}, and the guest has no network by design.\n\
         winquick: populate the cache from this Mac, then run again:\n\
         winquick:     winquick cache sync <project>\n"
    ))
}

/// Windows says "not recognized" for a program that is not there. When that
/// program is one WinQuick can install, say so rather than leaving the user to
/// guess which capability provides it.
fn capability_hint(command: &str, o: &Outcome) -> Option<String> {
    let text = String::from_utf8_lossy(&o.stderr);
    if !text.contains("is not recognized as an internal or external command") {
        return None;
    }
    let program = command
        .split_whitespace()
        .next()?
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()?
        .trim_end_matches(".exe")
        .to_lowercase();

    let (what, cap) = match program.as_str() {
        "pwsh" | "powershell" => ("PowerShell", "powershell"),
        "dotnet" => (".NET", "dotnet-sdk"),
        _ => return None,
    };
    let installed = crate::capability::installed().unwrap_or_default();
    if installed.iter().any(|c| c.name == cap || c.name.starts_with("dotnet")) {
        // It is installed; something else is wrong, and a wrong hint is worse
        // than none.
        return None;
    }
    let extra = if cap == "dotnet-sdk" {
        "\nwinquick: (use dotnet-runtime instead if you only need to run built apps)"
    } else {
        ""
    };
    Some(format!(
        "\nwinquick: {what} is not installed in this Windows environment.\n\
         winquick: Install it with:\n\
         winquick:     winquick capability install {cap}{extra}\n"
    ))
}

/// `run -- <command>` takes the program and its arguments as separate words, the
/// way `docker run` does. Passing the whole command line as one quoted string is
/// an easy mistake, and cmd.exe's complaint about it is not obvious.
fn argv_shape_hint(command: &str, o: &Outcome) -> Option<String> {
    let text = String::from_utf8_lossy(&o.stderr);
    if !text.contains("is not recognized as an internal or external command") {
        return None;
    }
    // The whole thing arrived quoted, so the first word carries a space.
    let looks_quoted = command.starts_with('"') && command.trim_end().ends_with('"');
    if !looks_quoted {
        return None;
    }
    let inner = command.trim().trim_matches('"');
    Some(format!(
        "\nwinquick: `run` takes the program and its arguments as separate words,\n\
         winquick: like `docker run`. Try:\n\
         winquick:     winquick run -- {inner}\n"
    ))
}

fn emit(o: Outcome, t_start: Instant, verbose: bool) -> Result<i32> {
    // Pass the guest's streams through, except for the CRLF that every Windows
    // program emits — a Unix caller piping into `grep` should not have to strip
    // carriage returns.
    let mut out = std::io::stdout().lock();
    out.write_all(&strip_cr(&o.stdout))?;
    out.flush()?;
    let mut err = std::io::stderr().lock();
    err.write_all(&strip_cr(&o.stderr))?;
    if let Some(hint) = nuget_hint(&o) {
        err.write_all(hint.as_bytes())?;
    }
    if let Some(hint) = argv_shape_hint(&o.command, &o) {
        err.write_all(hint.as_bytes())?;
    } else if let Some(hint) = capability_hint(&o.command, &o) {
        err.write_all(hint.as_bytes())?;
    }
    err.flush()?;
    if verbose {
        eprintln!(
            "winquick: {} run, total {:.0}ms",
            if o.warm { "warm" } else { "cold" },
            t_start.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(o.exit_code)
}

fn fingerprint(ctx: &Ctx) -> Result<state::Fingerprint> {
    let agent = crate::setup::AGENT;
    Ok(state::Fingerprint {
        winquick_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: state::PROTOCOL_VERSION,
        base_image: state::FileId::of(&ctx.base)?,
        agent_hash: state::fnv1a(agent.as_bytes()),
        qemu_binary: state::FileId::of(&ctx.q.system)?,
        // Recorded from the binary's identity rather than by running it: shelling
        // out to `qemu-system-aarch64 --version` costs ~10ms, which is 5% of the
        // warm-run budget. The binary FileId already changes when QEMU is upgraded.
        qemu_version: format!("id:{}", state::fnv1a(ctx.q.system.display().to_string().as_bytes())),
        firmware: state::FileId::of(&ctx.uefi_code)?,
        memory_mb: ctx.opts_memory,
        cpus: ctx.opts_cpus,
        machine: qemu::MACHINE.to_string(),
        capabilities: ctx
            .capabilities
            .iter()
            .map(|c| Ok((c.name.clone(), state::FileId::of(&c.image)?)))
            .collect::<Result<Vec<_>>>()?,
        devices: qemu::device_signature(ctx.opts_memory, ctx.opts_cpus, ctx.capabilities.len()),
    })
}

/// Cheap unique-per-run token. Not a secret; it only has to differ between runs.
fn run_nonce() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("n{:x}{:x}", std::process::id(), t)
}

fn new_run_dir() -> Result<PathBuf> {
    let id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
    );
    let dir = paths::run_dir(&id)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating run directory {}", dir.display()))?;
    Ok(dir)
}

/// Clone the capability volume for this run, if there is one. Cloned rather than
/// shared because the guest writes to it when mounting.
fn clone_capability(ctx: &Ctx, dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (i, c) in ctx.capabilities.iter().enumerate() {
        let dst = dir.join(format!("cap{i}.img"));
        qemu::clone_file(&c.image, &dst)?;
        out.push(dst);
    }
    Ok(out)
}

/// Build the per-run workspace volume: clone the ready template so the FAT
/// identity is preserved, then write this run's project into it.
fn prepare_workspace(ctx: &Ctx, template: Option<&Path>, dst: &Path) -> Result<()> {
    match template {
        Some(t) => qemu::clone_file(t, dst)?,
        None => {
            crate::capability::build_sized(dst, Path::new("/nonexistent"), "workspace", WORKSPACE_BYTES)?;
        }
    }
    crate::capability::mark(dst, "WQWORK.TXT", "workspace")?;
    if let Some(src) = &ctx.workspace {
        let t = Instant::now();
        crate::capability::refill(dst, src, "workspace")?;
        ctx.vlog(format!(
            "workspace: {} staged in {:.0}ms",
            src.display(),
            t.elapsed().as_secs_f64() * 1000.0
        ));
    }
    Ok(())
}

/// Artifact volume: always attached, always empty at the start of a run.
fn prepare_artifacts(template: Option<&Path>, dst: &Path) -> Result<()> {
    match template {
        Some(t) => qemu::clone_file(t, dst)?,
        None => {
            crate::capability::build_sized(dst, Path::new("/nonexistent"), crate::artifact::DIR, ARTIFACT_BYTES)?;
        }
    }
    crate::capability::mark(dst, crate::artifact::MARKER, crate::artifact::DIR)?;
    Ok(())
}

fn fresh_vars(path: &Path) -> Result<()> {
    let f = std::fs::File::create(path)?;
    f.set_len(64 * 1024 * 1024)?;
    Ok(())
}

/// Wait for a mailbox file to appear, polling the image directly.
///
/// Cheap on purpose: FAT32 metadata for a handful of files is a few sectors, so
/// a 2 ms poll does not distort the measurement it is part of.
fn wait_for(
    mbox: &Path,
    name: &str,
    child: &mut Child,
    deadline: Instant,
) -> Result<Vec<u8>> {
    loop {
        if crate::interrupt::interrupted() {
            bail!("interrupted");
        }
        if let Some(v) = mailbox::probe(mbox, name) {
            if !v.trim_ascii().is_empty() {
                return Ok(v);
            }
        }
        if let Some(st) = child.try_wait()? {
            bail!("qemu exited ({st}) before {name} appeared");
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {name} from the guest");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
    crate::interrupt::clear_child();
}

// ---------------------------------------------------------------- warm path

fn warm_execute(ctx: &Ctx, ready: &state::ReadyState, command: &str) -> Result<Outcome> {
    let t0 = Instant::now();
    let dir = new_run_dir()?;
    let _scratch = Scratch(dir.clone());
    let overlay = dir.join("root.qcow2");
    let vars = dir.join("uefi-vars.fd");
    let mbox = dir.join("mailbox.img");
    let serial = dir.join("serial.log");
    let qmp_sock = dir.join("qmp.sock");
    let caps = clone_capability(ctx, &dir)?;
    let workspace = dir.join("workspace.img");
    let artifacts_img = dir.join("artifacts.img");

    // Clones, not copies: on APFS these are effectively free whatever the size.
    qemu::clone_file(&ready.disk(), &overlay)?;
    qemu::clone_file(&ready.vars(), &vars)?;
    qemu::clone_file(&ready.mailbox(), &mbox)?;
    let art_script = (!ctx.artifacts.is_empty())
        .then(|| crate::artifact::script(&ctx.artifacts));
    let nonce = run_nonce();
    mailbox::inject_command(&mbox, command, art_script.as_deref(), &nonce)?;
    prepare_workspace(ctx, Some(&ready.workspace()), &workspace)?;
    prepare_artifacts(Some(&ready.artifacts()), &artifacts_img)?;
    let t_prep = t0.elapsed();

    let mut child = ctx.q.boot(&qemu::BootConfig {
        uefi_code: &ctx.uefi_code,
        uefi_vars: &vars,
        root_disk: &overlay,
        mailbox: &mbox,
        capabilities: &caps,
        workspace: &workspace,
        artifacts: &artifacts_img,
        memory_mb: ctx.opts_memory,
        cpus: ctx.opts_cpus,
        serial_log: &serial,
        qmp_socket: &qmp_sock,
        incoming: Some(&ready.state_file()),
    })?;
    crate::interrupt::watch_child(child.id());

    let result = (|| -> Result<Outcome> {
        let mut q = qmp::Qmp::connect(&qmp_sock, Duration::from_secs(10))?;
        let t_spawn = t0.elapsed();
        q.wait_incoming(Duration::from_secs(30))?;
        let t_restore = t0.elapsed();
        q.cont()?;
        let deadline = Instant::now() + ctx.timeout;
        wait_for(&mbox, mailbox::CODE_FILE, &mut child, deadline)?;
        let t_exec = t0.elapsed();
        let r = mailbox::read_results(&mbox)?;
        if r.nonce.as_deref() != Some(nonce.as_str()) {
            bail!(
                "the guest reported a result for a different run \
                 (expected token {nonce}, got {:?}) — it was holding a stale view \
                 of the mailbox",
                r.nonce
            );
        }
        let code = r
            .exit_code
            .ok_or_else(|| anyhow!("guest wrote no exit code"))?;
        collect_artifacts(ctx, &artifacts_img)?;
        ctx.vlog(format!(
            "warm phases: prep {:.0}ms | qemu spawn {:.0}ms | state restore {:.0}ms | guest exec + mailbox sync {:.0}ms",
            t_prep.as_secs_f64() * 1000.0,
            (t_spawn - t_prep).as_secs_f64() * 1000.0,
            (t_restore - t_spawn).as_secs_f64() * 1000.0,
            (t_exec - t_restore).as_secs_f64() * 1000.0
        ));
        Ok(Outcome { stdout: r.stdout, stderr: r.stderr, exit_code: code, warm: true, command: command.to_string() })
    })();

    let t_before_kill = Instant::now();
    kill(&mut child);
    ctx.vlog(format!(
        "teardown {:.0}ms",
        t_before_kill.elapsed().as_secs_f64() * 1000.0
    ));
    result
}

// ---------------------------------------------------------------- cold paths

/// Boot a clean guest, wait until the agent says it is ready, and freeze it.
fn build_ready_state(ctx: &Ctx, want: &state::Fingerprint) -> Result<state::ReadyState> {
    let t0 = Instant::now();
    ctx.vlog("preparing a reusable Windows image (one-off, takes a few seconds)");
    let dir = new_run_dir()?;
    let _scratch = Scratch(dir.clone());
    let overlay = dir.join("root.qcow2");
    let vars = dir.join("uefi-vars.fd");
    let mbox = dir.join("mailbox.img");
    let serial = dir.join("serial.log");
    let qmp_sock = dir.join("qmp.sock");
    let caps = clone_capability(ctx, &dir)?;
    let workspace = dir.join("workspace.img");
    let artifacts_img = dir.join("artifacts.img");

    ctx.q.create_overlay(&ctx.base, &overlay)?;
    fresh_vars(&vars)?;
    mailbox::create_template(&mbox)?;
    prepare_workspace(ctx, None, &workspace)?;
    prepare_artifacts(None, &artifacts_img)?;

    let mut child = ctx.q.boot(&qemu::BootConfig {
        uefi_code: &ctx.uefi_code,
        uefi_vars: &vars,
        root_disk: &overlay,
        mailbox: &mbox,
        capabilities: &caps,
        workspace: &workspace,
        artifacts: &artifacts_img,
        memory_mb: ctx.opts_memory,
        cpus: ctx.opts_cpus,
        serial_log: &serial,
        qmp_socket: &qmp_sock,
        incoming: None,
    })?;
    crate::interrupt::watch_child(child.id());

    let sdir = state::state_dir()?;
    let build = (|| -> Result<state::ReadyMeta> {
        let mut q = qmp::Qmp::connect(&qmp_sock, Duration::from_secs(30))?;
        // Bounded independently of --timeout: a guest that never reports ready is
        // broken, and burning the whole command timeout on it just delays the
        // cold fallback that would have worked.
        let deadline = Instant::now() + READY_TIMEOUT;
        wait_for(&mbox, mailbox::READY, &mut child, deadline)?;
        ctx.vlog(format!("guest ready after {:.1}s", t0.elapsed().as_secs_f64()));

        q.stop()?;
        std::fs::create_dir_all(&sdir)?;
        let state_file = sdir.join("ready.state");
        let _ = std::fs::remove_file(&state_file);
        q.migrate_to_file(&state_file, Duration::from_secs(120))?;
        // Quit cleanly rather than killing: the block layer has to flush before
        // the overlay we are about to copy is trustworthy.
        let _ = q.command("quit", serde_json::json!({}));
        let _ = child.wait();

        qemu::clone_file(&overlay, &sdir.join("ready-disk.qcow2"))?;
        qemu::clone_file(&vars, &sdir.join("ready-vars.fd"))?;
        qemu::clone_file(&mbox, &sdir.join("ready-mailbox.img"))?;
        qemu::clone_file(&workspace, &sdir.join("ready-workspace.img"))?;
        qemu::clone_file(&artifacts_img, &sdir.join("ready-artifacts.img"))?;
        let meta = state::ReadyMeta {
            fingerprint: want.clone(),
            created_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            state_bytes: std::fs::metadata(&state_file)?.len(),
        };
        state::save(&meta)?;
        ctx.vlog(format!(
            "ready state built in {:.1}s ({:.0} MiB)",
            t0.elapsed().as_secs_f64(),
            meta.state_bytes as f64 / (1024.0 * 1024.0)
        ));
        Ok(meta)
    })();

    kill(&mut child);
    match build {
        Ok(meta) => Ok(state::ReadyState { dir: sdir, meta }),
        Err(e) => {
            let _ = state::discard();
            Err(e)
        }
    }
}

/// Boot, run, read results, kill. No ready state involved — the safety net.
fn cold_execute(ctx: &Ctx, command: &str) -> Result<Outcome> {
    ctx.vlog("cold boot");
    let dir = new_run_dir()?;
    let _scratch = Scratch(dir.clone());
    let overlay = dir.join("root.qcow2");
    let vars = dir.join("uefi-vars.fd");
    let mbox = dir.join("mailbox.img");
    let serial = dir.join("serial.log");
    let qmp_sock = dir.join("qmp.sock");
    let caps = clone_capability(ctx, &dir)?;
    let workspace = dir.join("workspace.img");
    let artifacts_img = dir.join("artifacts.img");

    ctx.q.create_overlay(&ctx.base, &overlay)?;
    fresh_vars(&vars)?;
    mailbox::create_template(&mbox)?;
    let art_script = (!ctx.artifacts.is_empty())
        .then(|| crate::artifact::script(&ctx.artifacts));
    let nonce = run_nonce();
    mailbox::inject_command(&mbox, command, art_script.as_deref(), &nonce)?;
    prepare_workspace(ctx, None, &workspace)?;
    prepare_artifacts(None, &artifacts_img)?;
    prepare_artifacts(None, &artifacts_img)?;

    let mut child = ctx.q.boot(&qemu::BootConfig {
        uefi_code: &ctx.uefi_code,
        uefi_vars: &vars,
        root_disk: &overlay,
        mailbox: &mbox,
        capabilities: &caps,
        workspace: &workspace,
        artifacts: &artifacts_img,
        memory_mb: ctx.opts_memory,
        cpus: ctx.opts_cpus,
        serial_log: &serial,
        qmp_socket: &qmp_sock,
        incoming: None,
    })?;
    crate::interrupt::watch_child(child.id());

    let result = (|| -> Result<Outcome> {
        let deadline = Instant::now() + ctx.timeout;
        wait_for(&mbox, mailbox::CODE_FILE, &mut child, deadline)?;
        let r = mailbox::read_results(&mbox)?;
        if r.nonce.as_deref() != Some(nonce.as_str()) {
            bail!("the guest reported a result for a different run (token mismatch)");
        }
        let code = r.exit_code.ok_or_else(|| anyhow!("guest wrote no exit code"))?;
        collect_artifacts(ctx, &artifacts_img)?;
        Ok(Outcome { stdout: r.stdout, stderr: r.stderr, exit_code: code, warm: false, command: command.to_string() })
    })();

    kill(&mut child);
    result
}

fn strip_cr(b: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\r' && i + 1 < b.len() && b[i + 1] == b'\n' {
            i += 1;
            continue;
        }
        v.push(b[i]);
        i += 1;
    }
    v
}
