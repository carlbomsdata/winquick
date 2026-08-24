//! `winquick run` — the whole product, in one function.

use crate::{mailbox, paths, qemu};
use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Options {
    pub memory_mb: u32,
    pub cpus: u32,
    pub timeout: Duration,
    pub verbose: bool,
}

/// Deletes the run directory no matter how we leave `run()` — normal exit,
/// error, or panic. A run that leaves state behind is a bug: the promise is
/// that the environment is discarded.
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

pub fn run(command: &str, opts: &Options) -> Result<i32> {
    let base = paths::base_image()?;
    if !base.exists() {
        return Err(anyhow!(
            "no Windows runtime found at {}\n\nRun `winquick setup` first.",
            base.display()
        ));
    }

    let q = qemu::Qemu::locate()?;
    let uefi_code = paths::uefi_code().ok_or_else(|| {
        anyhow!("could not find edk2-aarch64-code.fd next to QEMU; is QEMU installed?")
    })?;

    let id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
    );
    let dir = paths::run_dir(&id)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating run directory {}", dir.display()))?;
    let _scratch = Scratch(dir.clone());

    let overlay = dir.join("root.qcow2");
    let mbox = dir.join("mailbox.img");
    let vars = dir.join("uefi-vars.fd");
    let serial = dir.join("serial.log");
    let qmp = dir.join("qmp.sock");

    q.create_overlay(&base, &overlay)?;
    mailbox::create(&mbox, command)?;

    // Fresh UEFI variable store per run, so even firmware state is disposable.
    let vf = std::fs::File::create(&vars)?;
    vf.set_len(64 * 1024 * 1024)?;
    drop(vf);

    let started = Instant::now();
    if opts.verbose {
        eprintln!("winquick: booting ({} MiB, {} vCPU)", opts.memory_mb, opts.cpus);
    }

    let mut child = q.boot(&qemu::BootConfig {
        uefi_code: &uefi_code,
        uefi_vars: &vars,
        root_disk: &overlay,
        mailbox: &mbox,
        memory_mb: opts.memory_mb,
        cpus: opts.cpus,
        serial_log: &serial,
        qmp_socket: &qmp,
    })?;

    let mut timed_out = false;
    loop {
        match child.try_wait()? {
            Some(status) => {
                if !status.success() && opts.verbose {
                    eprintln!("winquick: qemu exited with {status}");
                }
                break;
            }
            None => {
                if started.elapsed() > opts.timeout {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    if opts.verbose {
        eprintln!("winquick: vm finished in {:.1}s", started.elapsed().as_secs_f64());
    }

    let results = mailbox::read_results(&mbox)?;

    // Pass the guest's streams through byte for byte, except for the CRLF that
    // every Windows program emits — a Unix caller piping this into `grep` should
    // not have to strip carriage returns.
    let mut out = std::io::stdout().lock();
    out.write_all(&strip_cr(&results.stdout))?;
    out.flush()?;
    let mut err = std::io::stderr().lock();
    err.write_all(&strip_cr(&results.stderr))?;
    err.flush()?;

    if timed_out {
        return Err(anyhow!(
            "timed out after {}s waiting for the guest",
            opts.timeout.as_secs()
        ));
    }

    results.exit_code.ok_or_else(|| {
        anyhow!("the guest never reported an exit code (it may have crashed or hung)")
    })
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
