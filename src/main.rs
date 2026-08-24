//! WinQuick — a tiny, disposable, real Windows command execution environment
//! for Apple Silicon Macs.
//!
//! Experimental. See README.md for scope.

mod capability;
mod mailbox;
mod paths;
mod qemu;
mod qmp;
mod runner;
mod setup;
mod state;

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
        /// Also build the PowerShell 7 capability volume
        #[arg(long)]
        with_powershell: bool,
        /// Use this PowerShell ARM64 ZIP instead of downloading one
        #[arg(long, value_name = "ZIP")]
        powershell_zip: Option<PathBuf>,
    },
    /// Run a command inside a throwaway Windows environment
    #[command(trailing_var_arg = true)]
    Run {
        /// Guest RAM in MiB
        #[arg(long, default_value_t = 1024, value_name = "MIB")]
        memory: u32,
        /// Guest vCPUs
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        /// Give up after this many seconds
        #[arg(long, default_value_t = 300, value_name = "SECS")]
        timeout: u64,
        /// Report phase timings and ready-state decisions on stderr
        #[arg(short, long)]
        verbose: bool,
        /// Boot Windows from scratch instead of resuming a prepared guest
        #[arg(long)]
        cold: bool,
        /// The Windows command, after `--`
        #[arg(required = true, value_name = "COMMAND")]
        argv: Vec<String>,
    },
    /// Show host, runtime and QEMU status
    Info,
    /// Discard the prepared guest so the next run rebuilds it
    Reset,
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
        Cmd::Setup { from, force, with_powershell, powershell_zip } => {
            setup::setup(from, force)?;
            if with_powershell || powershell_zip.is_some() {
                capability::install_powershell(powershell_zip, true)?;
                // The device topology changed, so any frozen guest is stale.
                state::discard()?;
            }
            Ok(0)
        }
        Cmd::Run {
            memory,
            cpus,
            timeout,
            verbose,
            cold,
            argv,
        } => runner::run(
            &join_argv(&argv),
            &runner::Options {
                memory_mb: memory,
                cpus,
                timeout: Duration::from_secs(timeout),
                verbose,
                force_cold: cold,
            },
        ),
        Cmd::Info => {
            info()?;
            Ok(0)
        }
        Cmd::Reset => {
            state::discard()?;
            println!("Prepared guest discarded; the next run will rebuild it.");
            Ok(0)
        }
    }
}

/// Turn argv back into a single Windows command line.
///
/// `winquick run -- a b c` runs program `a` with arguments `b` and `c`, the way
/// `docker run` does. An argument containing spaces has to stay one argument, so
/// it gets quoted — without this, `pwsh -Command 'Write-Output "hi"'` arrives at
/// PowerShell as several arguments and is re-parsed into something else.
///
/// Quoting follows the Windows C runtime rules, which `pwsh.exe` and most other
/// Windows programs use to split the command line back up: a backslash is only
/// special immediately before a quote, so a run of N backslashes before a quote
/// becomes 2N+1 (the extra one escaping the quote), and a run at the end of a
/// quoted argument becomes 2N so it does not escape the closing quote.
fn join_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for ch in a.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
            }
            '"' => {
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('"');
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::join_argv;

    fn j(v: &[&str]) -> String {
        join_argv(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn plain_arguments_are_untouched() {
        assert_eq!(j(&["cmd", "/c", "ver"]), "cmd /c ver");
    }

    #[test]
    fn arguments_with_spaces_stay_one_argument() {
        assert_eq!(
            j(&["pwsh", "-Command", "Write-Output hello"]),
            "pwsh -Command \"Write-Output hello\""
        );
    }

    #[test]
    fn embedded_quotes_are_escaped() {
        assert_eq!(
            j(&["pwsh", "-Command", "Write-Output \"hi\""]),
            "pwsh -Command \"Write-Output \\\"hi\\\"\""
        );
    }

    #[test]
    fn windows_paths_without_spaces_pass_through_verbatim() {
        assert_eq!(j(&["cmd", "/c", "dir", r"C:\Windows\System32"]), r"cmd /c dir C:\Windows\System32");
    }

    #[test]
    fn backslashes_before_a_quote_are_doubled() {
        // C-runtime rule: N backslashes then a quote becomes 2N+1 backslashes.
        assert_eq!(j(&[r#"a\"b c"#]), r#""a\\\"b c""#);
    }

    #[test]
    fn trailing_backslash_does_not_escape_the_closing_quote() {
        assert_eq!(j(&[r"c:\some path\"]), r#""c:\some path\\""#);
    }

    #[test]
    fn shell_metacharacters_are_preserved_when_unquoted() {
        assert_eq!(j(&["cmd", "/c", "a&b"]), "cmd /c a&b");
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

    match state::state_dir() {
        Ok(d) if d.join("ready.json").exists() => {
            let sz = std::fs::metadata(d.join("ready.state")).map(|m| m.len()).unwrap_or(0);
            println!("prepared: yes ({:.0} MiB frozen guest)", sz as f64 / (1024.0 * 1024.0));
        }
        _ => println!("prepared: no — first run will take longer"),
    }

    match capability::pwsh_image() {
        Ok(p) if p.exists() => println!(
            "powershell: {} ({:.0} MiB volume)",
            capability::PWSH_VERSION,
            std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) as f64 / (1024.0 * 1024.0)
        ),
        _ => println!("powershell: not installed — `winquick setup --with-powershell`"),
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
