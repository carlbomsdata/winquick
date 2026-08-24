//! WinQuick — a tiny, disposable, real Windows command execution environment
//! for Apple Silicon Macs.
//!
//! Experimental. See README.md for scope.

mod mailbox;
mod paths;
mod qemu;
mod runner;
mod setup;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "winquick",
    version,
    about = "Run a command in a throwaway Windows environment",
    long_about = "Run a command inside a real, disposable Windows environment on an \
                  Apple Silicon Mac.\n\nEach run boots a clean guest from a pristine base \
                  image, executes the command, returns its stdout, stderr and exit code, \
                  and destroys the environment."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the Windows runtime from a Microsoft-supplied Validation OS image
    Setup {
        /// Path to the Validation OS ARM64 ISO, its VHDX, or a directory holding one
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,
        /// Rebuild even if a runtime already exists
        #[arg(long)]
        force: bool,
    },
    /// Run a command inside a throwaway Windows environment
    #[command(trailing_var_arg = true)]
    Run {
        /// Guest RAM in MiB
        #[arg(long, default_value_t = 2048, value_name = "MIB")]
        memory: u32,
        /// Guest vCPUs
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        /// Give up after this many seconds
        #[arg(long, default_value_t = 300, value_name = "SECS")]
        timeout: u64,
        /// Report boot and teardown timings on stderr
        #[arg(short, long)]
        verbose: bool,
        /// The Windows command, after `--`
        #[arg(required = true, value_name = "COMMAND")]
        argv: Vec<String>,
    },
    /// Show host, runtime and QEMU status
    Info,
}

fn main() {
    let cli = Cli::parse();
    let code = match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("winquick: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Cmd::Setup { from, force } => {
            setup::setup(from, force)?;
            Ok(0)
        }
        Cmd::Run {
            memory,
            cpus,
            timeout,
            verbose,
            argv,
        } => runner::run(
            &argv.join(" "),
            &runner::Options {
                memory_mb: memory,
                cpus,
                timeout: Duration::from_secs(timeout),
                verbose,
            },
        ),
        Cmd::Info => {
            info()?;
            Ok(0)
        }
    }
}

fn info() -> Result<()> {
    println!("winquick {}", env!("CARGO_PKG_VERSION"));

    match qemu::Qemu::locate() {
        Ok(q) => {
            println!("qemu:     {}", q.version().unwrap_or_default());
            println!("          {}", q.system.display());
        }
        Err(e) => println!("qemu:     MISSING ({e})"),
    }

    match paths::uefi_code() {
        Some(p) => println!("uefi:     {}", p.display()),
        None => println!("uefi:     MISSING (edk2-aarch64-code.fd)"),
    }

    let base = paths::base_image()?;
    if base.exists() {
        let sz = std::fs::metadata(&base)?.len();
        println!(
            "runtime:  {} ({:.0} MiB)",
            base.display(),
            sz as f64 / (1024.0 * 1024.0)
        );
    } else {
        println!("runtime:  not built — run `winquick setup`");
    }
    Ok(())
}
