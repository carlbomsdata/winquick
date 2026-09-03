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
/// Four processors is what a prepared guest restores reliably on Apple Silicon
/// and is meaningfully faster for builds. Windows defaults to two because that
/// is the most its prepared-state restore supports — see
/// `platform::MAX_PREPARED_CPUS` for why.
pub const DEFAULT_CPUS: u32 = if cfg!(target_os = "windows") { 2 } else { 4 };

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

/// Total bytes QEMU has moved for this guest, across every block device.
///
/// Asked of QEMU rather than of the guest, on purpose: the question is whether
/// the guest is doing anything at all, and a guest that cannot answer is
/// exactly the case being diagnosed. `None` when the monitor will not say, in
/// which case the caller must not read anything into it.
fn guest_io(q: &mut qmp::Qmp) -> Option<u64> {
    let v = q.command("query-blockstats", serde_json::json!({})).ok()?;
    let mut total = 0u64;
    for dev in v.as_array()? {
        let Some(s) = dev.get("stats") else { continue };
        for field in ["rd_bytes", "wr_bytes"] {
            total = total.saturating_add(s.get(field).and_then(|n| n.as_u64()).unwrap_or(0));
        }
    }
    Some(total)
}

/// How much the guest moved between two readings.
///
/// Zero unless both readings exist and the second is the larger, so a monitor
/// that would not answer, or answered oddly, can never be mistaken for proof
/// that a halted guest is alive.
fn io_since(before: Option<u64>, after: Option<u64>) -> u64 {
    match (before, after) {
        (Some(a), Some(b)) => b.saturating_sub(a),
        _ => 0,
    }
}

/// The patterns the guest reported as matching nothing, in the order it tried
/// them.
///
/// The guest is the only side that can answer this: matching happens in
/// Windows, over a tree the host never sees.
fn unmatched_patterns(log: &str) -> Vec<&str> {
    log.lines()
        .filter_map(|l| l.trim().strip_prefix("winquick: no match for "))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
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
        bail!("the guest could not copy some requested artifacts:\n{}", got.log.trim());
    }
    // The guest reports each pattern that matched nothing. Only reporting the
    // all-or-nothing case hid the more common mistake: several patterns, one of
    // them wrong, a plausible "retrieved 1 file" and a missing artifact nobody
    // noticed until much later.
    if got.files > 0 {
        for pat in unmatched_patterns(&got.log) {
            eprintln!("winquick: nothing matched {pat}");
        }
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
        ctx.vlog(format!("artifact extraction {:.0}ms", t.elapsed().as_secs_f64() * 1000.0));
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

/// Refuse a workspace that cannot be staged, before anything boots.
///
/// Left to the copy, a missing directory surfaces as a bare
/// `No such file or directory (os error 2)` from deep inside the staging code,
/// naming neither the path nor the flag that carried it. The MCP surface
/// already answers this properly; the command line is the more common way in
/// and deserves the same answer.
fn check_workspace(ws: Option<&Path>) -> Result<()> {
    let Some(ws) = ws else { return Ok(()) };
    if !ws.exists() {
        bail!(
            "--workspace: {} does not exist.\n\n\
             That directory is copied into the guest and appears there as\n\
             C:\\workspace, so it has to exist on this machine first.",
            ws.display()
        );
    }
    if !ws.is_dir() {
        bail!(
            "--workspace: {} is a file, not a directory.\n\n\
             Give the directory to expose as C:\\workspace.",
            ws.display()
        );
    }
    Ok(())
}

fn execute(command: &str, opts: &Options) -> Result<Outcome> {
    let t_start = Instant::now();
    let base = paths::run_image()?;
    if !base.exists() {
        bail!("No Windows runtime is installed yet.\n\nSet one up with:\n    winquick setup");
    }
    check_workspace(opts.workspace.as_deref())?;
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
    // A fast run resumes a prepared guest. Some hosts cannot rebuild a partition
    // of any size from one, and saying so is better than restoring something
    // that hangs -- or than quietly cold-booting and letting the user believe
    // the fast path worked.
    if !opts.force_cold {
        crate::platform::check_prepared_cpus(ctx.opts_cpus)?;
    }
    let want = fingerprint(&ctx)?;
    ctx.vlog(format!("host startup {:.0}ms", t_start.elapsed().as_secs_f64() * 1000.0));

    // Keyed on the accelerator and the QEMU that would do the restoring, not
    // on the guest topology: whether restore works at all is a property of
    // those two, and re-testing it for every memory size would be noise.
    //
    // The binary's own identity is part of the key, not just its version
    // string: a QEMU rebuilt with a restore fix reports the same version as
    // the one that could not restore, and a note that outlived the fix would
    // keep the fast path switched off on a machine where it now works.
    let backend = format!(
        "{}|{}|{}",
        want.qemu_version,
        crate::platform::backend_signature(),
        qemu_binary_identity(&ctx.q.system),
    );
    let can_restore = !state::restore_unsupported(&backend);
    if !can_restore {
        ctx.vlog("prepared guests do not restore with this QEMU; booting cold");
    }

    if !opts.force_cold && can_restore {
        match state::load_valid(&want) {
            Ok(Some(ready)) => {
                ctx.vlog("using existing ready state");
                match warm_execute(&ctx, &ready, command) {
                    Ok(o) => {
                        let _ = state::mark_restore_works(&backend);
                        return Ok(o);
                    }
                    Err(e) if crate::interrupt::interrupted() => return Err(e),
                    // The guest took the command and then ran out of time on
                    // it. Nothing is wrong with this prepared guest, and
                    // running the same slow command again cold would only
                    // spend the timeout a second time.
                    Err(e) if command_timed_out(&e) => return Err(e),
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
    if !opts.force_cold && can_restore {
        match crate::lock::acquire_build(Duration::from_secs(600))? {
            Some(_guard) => {
                // Someone may have built it while we waited.
                if let Ok(Some(ready)) = state::load_valid(&want) {
                    ctx.vlog("another run prepared the guest while we waited");
                    match warm_execute(&ctx, &ready, command) {
                        Ok(o) => {
                            let _ = state::mark_restore_works(&backend);
                            return Ok(o);
                        }
                        Err(e) if crate::interrupt::interrupted() => return Err(e),
                        Err(e) if command_timed_out(&e) => return Err(e),
                        Err(e) => ctx.vlog(format!("that prepared guest did not work: {e:#}")),
                    }
                }
                // Where a prepared guest gets frozen is partly luck. The
                // agent's poll loop mounts the mailbox, looks and dismounts
                // again without ever going quiet, and a guest caught in the
                // wrong part of that comes back unable to poll at all. So a
                // silent guest is evidence about *this state*, and only a
                // string of them is evidence about the machine -- one bad
                // freeze must not switch the fast path off for good.
                for attempt in 1..=PREPARE_ATTEMPTS {
                    match build_ready_state(&ctx, &want) {
                        Ok(ready) => {
                            match warm_execute(&ctx, &ready, command) {
                                Ok(o) => {
                                    let _ = state::mark_restore_works(&backend);
                                    return Ok(o);
                                }
                                Err(e) if crate::interrupt::interrupted() => return Err(e),
                                Err(e) if command_timed_out(&e) => return Err(e),
                                Err(e) => {
                                    ctx.vlog(format!(
                                        "newly built ready state did not work \
                                         (attempt {attempt} of {PREPARE_ATTEMPTS}): {e:#}"
                                    ));
                                    let _ = state::discard();
                                    // Only a silent guest is worth another go,
                                    // and only then. Anything else -- QEMU
                                    // dying, a disk error, a killed process --
                                    // is an accident of this run, and neither
                                    // rebuilding nor remembering it is right.
                                    if !guest_was_silent(&e) {
                                        break;
                                    }
                                    if attempt == PREPARE_ATTEMPTS {
                                        // The note means "this QEMU cannot
                                        // restore a prepared guest". A QEMU
                                        // that has already restored one
                                        // demonstrably can, and a run of
                                        // unlucky freezes is not evidence
                                        // against it -- switching the fast
                                        // path off for good on that basis is
                                        // how a machine that works ends up
                                        // cold-booting for ever.
                                        if state::restore_works(&backend) {
                                            ctx.vlog(format!(
                                                "{PREPARE_ATTEMPTS} prepared guests in a row \
                                                 came back silent, but this QEMU has restored \
                                                 one before; leaving the fast path on"
                                            ));
                                        } else {
                                            let _ = state::mark_restore_unsupported(&backend);
                                            ctx.vlog(
                                                "this QEMU cannot restore a prepared guest; \
                                                 later runs will boot cold without trying",
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) if crate::interrupt::interrupted() => return Err(e),
                        Err(e) => {
                            ctx.vlog(format!("could not build a ready state: {e:#}"));
                            break;
                        }
                    }
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
    let text =
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
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
         winquick: populate the cache from this machine, then run again:\n\
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

/// Enough to tell one QEMU build from another without hashing 80 MB of it.
///
/// Size and modification time change together whenever the binary is replaced,
/// which is all this needs to decide: the answer only gates whether to retry
/// something cheap.
fn qemu_binary_identity(p: &Path) -> String {
    let Ok(m) = std::fs::metadata(p) else {
        return "unknown".into();
    };
    let secs = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}@{}", m.len(), secs)
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
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
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
///
/// `from_ready` is the prepared state, when there is one. It matters which
/// source is used: a restored guest resumes with these volumes still mounted
/// and a filesystem cache describing the bytes that were on them at the freeze,
/// which are the canonical image *plus* whatever Windows wrote when it mounted
/// it. Cloning the canonical image instead hands that guest a disk its own
/// cache disagrees with -- measured on Windows as a guest that restores, never
/// acknowledges, and hangs until the command times out, for `cmd /c echo` as
/// readily as for pwsh.
fn clone_capability(
    ctx: &Ctx,
    dir: &Path,
    from_ready: Option<&state::ReadyState>,
) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for (i, c) in ctx.capabilities.iter().enumerate() {
        let dst = dir.join(format!("cap{i}.img"));
        let src = match from_ready {
            Some(r) => r.capability(i),
            None => c.image.clone(),
        };
        qemu::clone_file(&src, &dst)?;
        out.push(dst);
    }
    Ok(out)
}

/// Build the per-run workspace volume: clone the ready template so the FAT
/// identity is preserved, then write this run's project into it.
fn prepare_workspace(ctx: &Ctx, template: Option<&Path>, dst: &Path) -> Result<()> {
    // Check the whole tree before copying any of it, so an unrepresentable name
    // is reported by path rather than as a bare failure part-way through.
    if let Some(src) = &ctx.workspace {
        crate::capability::reject_unsupported_names(src, "the workspace")?;
    }
    match template {
        Some(t) => qemu::clone_file(t, dst)?,
        None => {
            crate::capability::build_sized(
                dst,
                Path::new("/nonexistent"),
                "workspace",
                WORKSPACE_BYTES,
            )?;
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
            crate::capability::build_sized(
                dst,
                Path::new("/nonexistent"),
                crate::artifact::DIR,
                ARTIFACT_BYTES,
            )?;
        }
    }
    crate::capability::mark(dst, crate::artifact::MARKER, crate::artifact::DIR)?;
    Ok(())
}

fn fresh_vars(path: &Path) -> Result<()> {
    crate::helpers::fresh_uefi_vars(path)
}

/// The guest was resumed, said nothing, and ran out of time.
///
/// Distinguished from every other way a run can fail because it is the only one
/// that says something about the *host*: QEMU dying, an I/O error or a Ctrl-C
/// are all accidents of this run, while a guest that resumes and never executes
/// is a property of the accelerator. Only this one is worth remembering.
#[derive(Debug)]
struct GuestSilent(String);

impl std::fmt::Display for GuestSilent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timed out waiting for {} from the guest", self.0)
    }
}

impl std::error::Error for GuestSilent {}

/// Did this failure mean "the guest resumed and never executed"?
fn guest_was_silent(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<GuestSilent>().is_some())
}

/// The command was picked up and did not finish inside `--timeout`.
///
/// A different thing entirely from a silent guest, and the difference costs
/// real time: a silent guest is worth replacing and trying again, a slow
/// command is not. Treating the two alike ran a user's command once on the
/// prepared guest, threw that guest away, and then ran the whole thing again
/// cold -- up to `PREPARE_ATTEMPTS` more times, each paying the full timeout.
#[derive(Debug)]
struct CommandTimedOut(Duration);

impl std::fmt::Display for CommandTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the command did not finish within {} s — raise it with `--timeout`",
            self.0.as_secs()
        )
    }
}

impl std::error::Error for CommandTimedOut {}

fn command_timed_out(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<CommandTimedOut>().is_some())
}

/// Reinterpret the *second* of the two waits.
///
/// Both waits report silence the same way, because both are "the guest wrote
/// nothing". Only the first one is evidence about the guest: by the time the
/// command is running, the guest has demonstrably picked it up, and running out
/// of time after that says something about the command instead.
fn as_command_timeout(e: anyhow::Error, limit: Duration) -> anyhow::Error {
    if guest_was_silent(&e) {
        anyhow::Error::new(CommandTimedOut(limit))
    } else {
        e
    }
}

/// Wait for a mailbox file to appear, polling the image directly.
///
/// Cheap on purpose: FAT32 metadata for a handful of files is a few sectors, so
/// a 2 ms poll does not distort the measurement it is part of.
/// Wait for the guest to take something out of the mailbox.
///
/// The agent deletes the go flag as soon as it has read this run's token, so
/// the flag going away is the earliest and cheapest proof that a restored guest
/// is not only running but running *the agent*. Reported as a silent guest when
/// it does not happen, which is exactly what it is.
fn wait_until_gone(mbox: &Path, name: &str, child: &mut Child, deadline: Instant) -> Result<()> {
    loop {
        if crate::interrupt::interrupted() {
            bail!("interrupted");
        }
        if mailbox::probe(mbox, name).is_none() {
            return Ok(());
        }
        if let Some(st) = child.try_wait()? {
            bail!("qemu exited ({st}) before the guest took the command{}", qemu_complaint(child));
        }
        if Instant::now() > deadline {
            return Err(anyhow::Error::new(GuestSilent(name.to_string())));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for(mbox: &Path, name: &str, child: &mut Child, deadline: Instant) -> Result<Vec<u8>> {
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
            bail!("qemu exited ({st}) before {name} appeared{}", qemu_complaint(child));
        }
        if Instant::now() > deadline {
            return Err(anyhow::Error::new(GuestSilent(name.to_string())));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Whatever QEMU said on its way out.
///
/// An exit code on its own is not a diagnosis, and QEMU is usually explicit
/// about what it could not do -- a missing accelerator, a file it could not
/// open, an option this build does not support. Passing that through is the
/// difference between a report and a shrug.
fn qemu_complaint(child: &mut Child) -> String {
    use std::io::Read;
    let Some(mut err) = child.stderr.take() else { return String::new() };
    let mut text = String::new();
    let _ = err.read_to_string(&mut text);
    let text = text.trim();
    if text.is_empty() {
        String::new()
    } else {
        format!(":\n{text}")
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
    crate::interrupt::clear_child();
}

// ---------------------------------------------------------------- warm path

/// How long a restored guest gets to pick the command up.
///
/// A working one does it in about half a second -- the agent's poll loop turns
/// over in tens of milliseconds -- so this is twenty times the margin it needs,
/// and a guest that came back halted is recognised as such in seconds rather
/// than in whatever the user set `--timeout` to.
///
/// It is not a verdict on its own, though. See `PROOF_OF_LIFE_BYTES`.
const FIRST_CONTACT: Duration = Duration::from_secs(10);

/// How much guest I/O counts as "this guest is working, not halted".
///
/// The go flag disappearing is a FAT directory write, and the agent starts the
/// user's command the moment it has read the token -- so the acknowledgement
/// and the workload race, on the same volume, and the workload can win. A
/// build heavy enough to saturate the guest keeps that one directory write
/// from reaching the image for far longer than `FIRST_CONTACT`, and WinQuick
/// then threw away a perfectly good prepared guest and cold-booted.
///
/// Measured on `dotnet build` of a three-project solution: **122 s** every
/// time, five discarded prepared guests per run, against **11 s** for the same
/// build when the flag was simply waited for longer. The guest was never
/// halted; it was busy.
///
/// So when the deadline passes, ask a question the guest cannot lie about:
/// QEMU's own byte counters. A restored guest that came back halted moves
/// essentially nothing -- its poll loop is a few sectors of FAT metadata per
/// turn. One that is building moves tens of megabytes in the same window.
/// Sixteen megabytes sits well clear of both.
const PROOF_OF_LIFE_BYTES: u64 = 16 * 1024 * 1024;

/// How long to watch the counters when the total is not enough to decide.
///
/// `PROOF_OF_LIFE_BYTES` answers "is this guest working hard?", and a great
/// many perfectly healthy commands are not. `winquick run --timeout 2 -- cmd
/// /c "ping -n 30 127.0.0.1"` moves almost nothing, holds the go flag in the
/// guest's cache for the whole thirty seconds, and was therefore read as a
/// halted guest -- which cost a discarded prepared guest, five rebuilds and
/// **117 s** for a two-second timeout, measured.
///
/// So ask the smaller question the byte total cannot: not *how much* has this
/// guest moved, but *is it still moving*. A guest that came back halted stops
/// dead once the I/O in flight at resume has drained, and its counters never
/// change again. A live Windows guest never stops touching a disk for a second
/// and a half. Only paid on the fallback path, after the ten-second deadline
/// has already passed.
const STILL_MOVING_WINDOW: Duration = Duration::from_millis(1500);

/// Is the guest's I/O still advancing at all?
///
/// Conservative in the same way `io_since` is: a monitor that will not answer,
/// or counters that do not move, is never read as proof of life.
fn still_moving(q: &mut qmp::Qmp) -> bool {
    let before = guest_io(q);
    std::thread::sleep(STILL_MOVING_WINDOW);
    io_since(before, guest_io(q)) > 0
}

/// How many prepared guests to build before believing the machine cannot restore.
///
/// Each attempt costs one guest boot, and only a *silent* restored guest --
/// one that came back and then never polled -- counts as an attempt at all.
/// About half of them are unusable at two processors, so three attempts give up
/// roughly one time in ten. Five is affordable because a silent guest is now
/// recognised in seconds rather than in a minute, and it brings that down to
/// about one time in thirty.
const PREPARE_ATTEMPTS: usize = 5;

/// How long to let the guest settle after it announces itself, before freezing.
///
/// Long enough for the agent to finish dismounting the mailbox and get back to
/// its poll loop, which is the only state worth capturing. It is paid once per
/// prepared guest, never per run.
const SETTLE_BEFORE_FREEZE: Duration = Duration::from_millis(1500);

fn warm_execute(ctx: &Ctx, ready: &state::ReadyState, command: &str) -> Result<Outcome> {
    let t0 = Instant::now();
    let dir = new_run_dir()?;
    let _scratch = Scratch(dir.clone());
    let overlay = dir.join("root.qcow2");
    let vars = dir.join("uefi-vars.fd");
    let mbox = dir.join("mailbox.img");
    let serial = dir.join("serial.log");
    let qmp_sock = dir.join("qmp.sock");
    let caps = clone_capability(ctx, &dir, Some(ready))?;
    let workspace = dir.join("workspace.img");
    let artifacts_img = dir.join("artifacts.img");

    // Clones, not copies: on APFS these are effectively free whatever the size.
    qemu::clone_file(&ready.disk(), &overlay)?;
    qemu::clone_file(&ready.vars(), &vars)?;
    qemu::clone_file(&ready.mailbox(), &mbox)?;
    let art_script = (!ctx.artifacts.is_empty()).then(|| crate::artifact::script(&ctx.artifacts));
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
        verbose: ctx.verbose,
        incoming: Some(&ready.state_file()),
    })?;
    crate::interrupt::watch_child(child.id());

    let result = (|| -> Result<Outcome> {
        let mut q = qmp::Qmp::connect(&qmp_sock, Duration::from_secs(10))?;
        let t_spawn = t0.elapsed();
        q.wait_incoming(Duration::from_secs(30))?;
        let t_restore = t0.elapsed();
        q.cont()?;
        // Two waits, not one, because they are two different questions.
        //
        // "Did this guest come back alive?" is answered in about half a second:
        // the agent reads the go flag and deletes it before doing anything
        // else. A guest that came back halted never deletes it, and no amount
        // of waiting changes that. Asking the question with the command's
        // timeout made every unlucky restore cost a minute before falling back.
        //
        // "Has the command finished?" is the user's question, and it gets the
        // user's timeout, because a long build is a legitimate thing to wait
        // for.
        //
        // The first question has a second half. The flag disappearing is a FAT
        // directory write on the same volume the workload is about to hammer,
        // and the agent starts the workload the instant it has the token -- so
        // a busy guest can leave that write unflushed for a minute. QEMU's own
        // byte counters tell a busy guest from a halted one without asking the
        // guest anything.
        let io_at_resume = guest_io(&mut q);
        if let Err(e) =
            wait_until_gone(&mbox, mailbox::GO, &mut child, Instant::now() + FIRST_CONTACT)
        {
            if !guest_was_silent(&e) {
                return Err(e);
            }
            let moved = io_since(io_at_resume, guest_io(&mut q));
            let why = if moved >= PROOF_OF_LIFE_BYTES {
                format!("has moved {:.0} MiB", moved as f64 / (1024.0 * 1024.0))
            } else if still_moving(&mut q) {
                "is still moving data".to_string()
            } else {
                return Err(e);
            };
            ctx.vlog(format!(
                "the guest has not acknowledged the command yet but {why} — \
                 it is working, not halted; waiting for the command instead"
            ));
        }
        let deadline = Instant::now() + ctx.timeout;
        wait_for(&mbox, mailbox::CODE_FILE, &mut child, deadline)
            .map_err(|e| as_command_timeout(e, ctx.timeout))?;
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
        let code = r.exit_code.ok_or_else(|| anyhow!("guest wrote no exit code"))?;
        collect_artifacts(ctx, &artifacts_img)?;
        ctx.vlog(format!(
            "warm phases: prep {:.0}ms | qemu spawn {:.0}ms | state restore {:.0}ms | guest exec + mailbox sync {:.0}ms",
            t_prep.as_secs_f64() * 1000.0,
            (t_spawn - t_prep).as_secs_f64() * 1000.0,
            (t_restore - t_spawn).as_secs_f64() * 1000.0,
            (t_exec - t_restore).as_secs_f64() * 1000.0
        ));
        Ok(Outcome {
            stdout: r.stdout,
            stderr: r.stderr,
            exit_code: code,
            warm: true,
            command: command.to_string(),
        })
    })();

    let t_before_kill = Instant::now();
    kill(&mut child);
    ctx.vlog(format!("teardown {:.0}ms", t_before_kill.elapsed().as_secs_f64() * 1000.0));
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
    let caps = clone_capability(ctx, &dir, None)?;
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
        verbose: ctx.verbose,
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

        // The readiness flag becomes visible here the moment its directory
        // entry reaches the image, and that is the middle of the agent's work,
        // not the end of it: the agent writes the flag and then dismounts the
        // mailbox volume. Freezing on the flag captures a guest with mailbox
        // I/O still in flight, and on restore into a fresh process that
        // operation never completes -- the agent never reaches its poll loop
        // and never sees the next command. Measured on Windows: a guest frozen
        // this way performed zero poll iterations after restore.
        //
        // So wait for the guest to go quiet before taking its picture.
        std::thread::sleep(SETTLE_BEFORE_FREEZE);
        q.stop()?;
        std::fs::create_dir_all(&sdir)?;
        // Withdraw the claim before touching anything a previous freeze
        // published. From here until `state::save` at the end, the files in
        // this directory are half of one freeze and half of another, and an
        // interruption anywhere in between used to leave `ready.json` still
        // advertising a guest whose state file had already been deleted.
        state::unpublish()?;
        let state_file = sdir.join("ready.state");
        // Migrate into a temporary name and rename once QEMU says it finished,
        // so a killed migration cannot leave a truncated file under the name
        // everything else trusts.
        let partial = sdir.join("ready.state.part");
        let _ = std::fs::remove_file(&state_file);
        let _ = std::fs::remove_file(&partial);
        q.migrate_to_file(&partial, Duration::from_secs(120))?;
        std::fs::rename(&partial, &state_file).context("publishing the frozen guest state")?;
        // Quit cleanly rather than killing: the block layer has to flush before
        // the overlay we are about to copy is trustworthy.
        let _ = q.command("quit", serde_json::json!({}));
        let _ = child.wait();

        qemu::clone_file(&overlay, &sdir.join("ready-disk.qcow2"))?;
        qemu::clone_file(&vars, &sdir.join("ready-vars.fd"))?;
        qemu::clone_file(&mbox, &sdir.join("ready-mailbox.img"))?;
        qemu::clone_file(&workspace, &sdir.join("ready-workspace.img"))?;
        qemu::clone_file(&artifacts_img, &sdir.join("ready-artifacts.img"))?;
        // The capability volumes too. The guest mounts these at startup and --
        // unlike the mailbox, workspace and artifact volumes -- never dismounts
        // them, so they are still mounted when the picture is taken and the
        // frozen cache describes *these* bytes, not the canonical image's.
        for (i, cap) in caps.iter().enumerate() {
            qemu::clone_file(cap, &sdir.join(format!("ready-cap{i}.img")))?;
        }
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
    let caps = clone_capability(ctx, &dir, None)?;
    let workspace = dir.join("workspace.img");
    let artifacts_img = dir.join("artifacts.img");

    ctx.q.create_overlay(&ctx.base, &overlay)?;
    fresh_vars(&vars)?;
    mailbox::create_template(&mbox)?;
    let art_script = (!ctx.artifacts.is_empty()).then(|| crate::artifact::script(&ctx.artifacts));
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
        verbose: ctx.verbose,
        incoming: None,
    })?;
    crate::interrupt::watch_child(child.id());

    let result = (|| -> Result<Outcome> {
        let deadline = Instant::now() + ctx.timeout;
        wait_for(&mbox, mailbox::CODE_FILE, &mut child, deadline)
            .map_err(|e| as_command_timeout(e, ctx.timeout))?;
        let r = mailbox::read_results(&mbox)?;
        if r.nonce.as_deref() != Some(nonce.as_str()) {
            bail!("the guest reported a result for a different run (token mismatch)");
        }
        let code = r.exit_code.ok_or_else(|| anyhow!("guest wrote no exit code"))?;
        collect_artifacts(ctx, &artifacts_img)?;
        Ok(Outcome {
            stdout: r.stdout,
            stderr: r.stderr,
            exit_code: code,
            warm: false,
            command: command.to_string(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A mistyped `--workspace` is one of the easiest mistakes to make, and the
    /// answer has to name the path and the flag. It used to be a bare
    /// `No such file or directory (os error 2)` raised from inside the staging
    /// code, which said neither.
    #[test]
    fn a_missing_workspace_names_the_path_and_the_flag() {
        let e = super::check_workspace(Some(std::path::Path::new("/no/such/winquick/dir")))
            .unwrap_err()
            .to_string();
        assert!(e.contains("--workspace"), "{e}");
        assert!(e.contains("/no/such/winquick/dir"), "{e}");
        assert!(e.contains("does not exist"), "{e}");
        assert!(!e.contains("os error"), "the raw errno must not be the answer: {e}");
    }

    /// Pointing at a file is the other half of the same mistake.
    #[test]
    fn a_workspace_that_is_a_file_says_so() {
        let f = std::env::temp_dir().join(format!("wq-ws-file-{}", std::process::id()));
        std::fs::write(&f, b"x").unwrap();
        let e = super::check_workspace(Some(&f)).unwrap_err().to_string();
        let _ = std::fs::remove_file(&f);
        assert!(e.contains("is a file, not a directory"), "{e}");
        assert!(e.contains("--workspace"), "{e}");
    }

    /// The common cases must stay silent: no workspace at all, and a real one.
    #[test]
    fn a_real_directory_and_no_workspace_both_pass() {
        assert!(super::check_workspace(None).is_ok());
        assert!(super::check_workspace(Some(&std::env::temp_dir())).is_ok());
    }

    /// Recording "this host cannot restore" is a decision that makes every
    /// later run slower, so it must rest on the one failure that actually says
    /// something about the host. A killed QEMU or a disk error must not count:
    /// that is how a machine where restoring works fine ends up booting cold
    /// forever.
    #[test]
    fn only_a_silent_guest_counts_as_evidence_about_the_host() {
        let silent = anyhow::Error::new(GuestSilent("WQCODE.TXT".into()));
        assert!(guest_was_silent(&silent));

        let wrapped = silent.context("restoring the prepared guest");
        assert!(guest_was_silent(&wrapped), "context must not hide the cause");

        assert!(!guest_was_silent(&anyhow!("qemu exited (exit code: 1)")));
        assert!(!guest_was_silent(&anyhow!("interrupted")));
    }

    /// The two waits fail the same way and mean opposite things. A guest that
    /// never took the command is worth replacing; a command that took too long
    /// is not, and treating them alike ran the user's slow command once warm
    /// and then up to `PREPARE_ATTEMPTS` more times cold, each paying the whole
    /// timeout before giving up.
    #[test]
    fn a_slow_command_is_not_a_bad_guest() {
        let limit = Duration::from_secs(300);
        let e =
            as_command_timeout(anyhow::Error::new(GuestSilent(mailbox::CODE_FILE.into())), limit);
        assert!(command_timed_out(&e), "{e:#}");
        assert!(!guest_was_silent(&e), "a slow command must not look like a dead guest");
        let msg = e.to_string();
        assert!(msg.contains("300"), "{msg}");
        assert!(msg.contains("--timeout"), "the message must say what to change: {msg}");

        // Anything that is not silence passes straight through, so a QEMU that
        // died is still reported as a QEMU that died.
        let other = as_command_timeout(anyhow!("qemu exited (exit code: 1)"), limit);
        assert!(!command_timed_out(&other), "{other:#}");
        assert_eq!(other.to_string(), "qemu exited (exit code: 1)");

        // The first wait keeps its meaning: that one *is* evidence about the
        // guest, and nothing here may weaken it.
        let first = anyhow::Error::new(GuestSilent(mailbox::GO.into()));
        assert!(guest_was_silent(&first));
        assert!(!command_timed_out(&first));
    }

    /// The message is what a user sees; it should name what was waited for.
    #[test]
    fn a_silent_guest_says_what_it_was_waiting_for() {
        let e = GuestSilent("WQCODE.TXT".into()).to_string();
        assert!(e.contains("WQCODE.TXT"), "{e}");
    }

    /// Ask for four things, get two, and the run used to look like a success.
    /// Every pattern the guest could not match has to be named.
    #[test]
    fn a_pattern_that_matched_nothing_is_named() {
        let log = "\
winquick: no match for */bin/Release/*.dll\r\n\
2 File(s) copied\r\n\
winquick: no match for TestResults/**\r\n\
winquick-artifact-status=0\r\n";
        assert_eq!(unmatched_patterns(log), vec!["*/bin/Release/*.dll", "TestResults/**"]);
    }

    /// A guest that is building moves hundreds of megabytes while the host is
    /// still waiting for one directory write. Measured on a three-project
    /// solution: 210 MiB inside the first-contact window.
    #[test]
    fn a_busy_guest_is_told_from_a_halted_one_by_what_it_moved() {
        let mib = 1024 * 1024;
        assert!(io_since(Some(100 * mib), Some(310 * mib)) >= PROOF_OF_LIFE_BYTES);
        // A halted guest's poll loop is a few sectors of FAT metadata.
        assert!(io_since(Some(100 * mib), Some(100 * mib + 40_000)) < PROOF_OF_LIFE_BYTES);
        assert_eq!(io_since(Some(7), Some(7)), 0);
    }

    /// Not every healthy command is a heavy one. `ping -n 30` moves almost
    /// nothing and still holds the go flag in the guest's cache for half a
    /// minute, so "did it move sixteen megabytes" says no about a guest that is
    /// perfectly alive. "Is it moving at all" is the question that works for
    /// both, and a halted guest still answers no to it: its counters stop dead.
    #[test]
    fn a_guest_that_is_merely_alive_is_not_a_halted_one() {
        // The window compares two readings, so any advance at all counts.
        assert!(io_since(Some(1_000), Some(1_512)) > 0);
        // And a guest whose counters have stopped does not.
        assert_eq!(io_since(Some(1_000), Some(1_000)), 0);
        // The heavy case still short-circuits without paying for the window.
        assert!(io_since(Some(0), Some(64 * 1024 * 1024)) >= PROOF_OF_LIFE_BYTES);
        // The window is paid only after the ten-second deadline, so it has to
        // stay small next to it.
        assert!(STILL_MOVING_WINDOW < FIRST_CONTACT);
    }

    /// A monitor that would not answer, or answered oddly, must never be read
    /// as proof that a halted guest is alive -- that would trade a ten-second
    /// fallback for the whole command timeout.
    #[test]
    fn an_unanswered_monitor_is_never_proof_of_life() {
        assert_eq!(io_since(None, Some(u64::MAX)), 0);
        assert_eq!(io_since(Some(0), None), 0);
        assert_eq!(io_since(None, None), 0);
        // Counters that went backwards say nothing either.
        assert_eq!(io_since(Some(900), Some(10)), 0);
    }

    /// A run that matched everything says nothing extra.
    #[test]
    fn a_clean_extraction_reports_no_unmatched_patterns() {
        let log = "12 File(s) copied\r\nwinquick-artifact-status=0\r\n";
        assert!(unmatched_patterns(log).is_empty());
    }
}
