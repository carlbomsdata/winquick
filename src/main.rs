//! WinQuick — run real Windows commands on an Apple Silicon Mac.

mod artifact;
mod capability;
mod helpers;
mod interrupt;
mod lock;
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

const AFTER_HELP: &str = "\
Examples:
  winquick run -- cmd /c ver
  winquick run -- pwsh -NoProfile -Command '$PSVersionTable'
  winquick run -w . -- dotnet test
  winquick run -w . -a \"bin/Release/**\" -- dotnet publish -c Release

Getting started:
  winquick setup                            install the Windows runtime
  winquick capability install powershell    add PowerShell 7
  winquick capability install dotnet-sdk    add the .NET SDK
  winquick doctor                           check the installation

Every run gets a clean Windows. Files, registry keys and environment
variables written by one run are gone in the next. Windows has no network
access; see `winquick cache --help` for offline package restore.";

#[derive(Parser)]
#[command(
    name = "winquick",
    version,
    about = "Run real Windows commands on an Apple Silicon Mac",
    long_about = "Run a command inside a real, disposable Windows environment on an Apple \
                  Silicon Mac.\n\nThink `docker run`, with a real Windows kernel on the other \
                  end. Each run starts from a clean Windows, executes the command, returns its \
                  stdout, stderr and exit code, and throws the environment away.",
    after_help = AFTER_HELP,
    after_long_help = AFTER_HELP
)]
struct Cli {
    /// Report what WinQuick is doing on stderr
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install the Windows runtime (run this once)
    #[command(after_help = "\
WinQuick needs Microsoft's Windows validation runtime, which Microsoft
distributes under its own licence. WinQuick cannot ship it for you.

  winquick setup --accept-microsoft-terms      download it (about 2.4 GB)
  winquick setup --from ~/Downloads/vos.iso    use a file you already have

Add optional tools at the same time:

  winquick setup --accept-microsoft-terms --with powershell dotnet-sdk")]
    Setup {
        /// Path to the Microsoft image (.iso, .vhdx, or a directory holding one)
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,
        /// Download the Microsoft image, accepting Microsoft's licence terms
        #[arg(long)]
        accept_microsoft_terms: bool,
        /// Rebuild even if a runtime is already installed
        #[arg(long)]
        force: bool,
        /// Also install these capabilities (powershell, dotnet-runtime, dotnet-sdk)
        #[arg(long, value_name = "NAME", num_args = 1..)]
        with: Vec<String>,
    },

    /// Run a command inside a throwaway Windows environment
    #[command(trailing_var_arg = true, after_help = "\
Arguments work like `docker run`: the program and its arguments are separate
words, and anything containing spaces stays one argument.

  winquick run -- cmd /c ver
  winquick run -- cmd /c \"echo A & echo B\"
  winquick run -- pwsh -NoProfile -Command 'Write-Output \"hello world\"'

With a project (-w), the directory appears inside Windows as C:\\workspace and
becomes the working directory. It is copied in, never copied back, so the guest
cannot change your source. Ask for output explicitly with --artifact.

  winquick run -w . -- dotnet test
  winquick run -w . -a \"TestResults/**\" -- dotnet test")]
    Run {
        /// Expose this host directory to Windows as C:\workspace
        #[arg(short = 'w', long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        /// Retrieve files matching this pattern afterwards (repeatable)
        #[arg(short = 'a', long = "artifact", value_name = "PATTERN")]
        artifacts: Vec<String>,
        /// Where retrieved files are written [default: ./winquick-artifacts]
        #[arg(long, value_name = "DIR")]
        artifacts_dir: Option<PathBuf>,
        /// Write artifacts into a directory that already has files in it
        #[arg(long)]
        artifact_overwrite: bool,
        /// Give up after this many seconds
        #[arg(long, default_value_t = 300, value_name = "SECONDS")]
        timeout: u64,
        /// Guest memory in MiB
        #[arg(long, default_value_t = 1024, value_name = "MIB")]
        memory: u32,
        /// Guest processors
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        /// Start Windows from scratch instead of resuming the prepared guest
        #[arg(long)]
        cold: bool,
        /// The command to run, after `--`
        #[arg(required = true, value_name = "COMMAND")]
        argv: Vec<String>,
    },

    /// Add or remove optional tools inside Windows
    Capability {
        #[command(subcommand)]
        action: CapabilityCmd,
    },

    /// Manage the offline package cache used by `dotnet`
    #[command(after_help = "\
Windows has no network access. Packages are restored on your Mac and shared with
Windows through a cache that persists between runs.

  cd MyProject
  winquick cache sync            restore this project's packages
  winquick run -w . -- dotnet test

Windows sees a throwaway copy of the cache, so a build cannot change it.")]
    Cache {
        #[command(subcommand)]
        action: CacheCmd,
    },

    /// Check the installation and report problems
    Doctor {
        /// Also run a real Windows command to prove it works
        #[arg(long)]
        smoke: bool,
    },

    /// Show what is installed
    Info,

    /// Discard the prepared guest; the next run rebuilds it
    Reset,

    /// Delete WinQuick's generated data (never touches your projects)
    #[command(after_help = "\
By default this removes the prepared guest, cached downloads and any leftover
run directories, keeping the Windows runtime and capabilities so you do not have
to set up again. Use --all to remove everything WinQuick generated.")]
    Clean {
        /// Also remove the Windows runtime, capabilities and package cache
        #[arg(long)]
        all: bool,
        /// Show what would be removed without removing it
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum CapabilityCmd {
    /// Show available and installed capabilities
    List,
    /// Install a capability
    Install {
        /// powershell, dotnet-runtime or dotnet-sdk
        name: String,
        /// Use this archive instead of downloading one
        #[arg(long, value_name = "ZIP")]
        from: Option<PathBuf>,
    },
    /// Remove a capability
    Remove { name: String },
}

#[derive(Subcommand)]
enum CacheCmd {
    /// Restore a project's packages on this Mac and share them with Windows
    Sync {
        /// Project or solution directory [default: .]
        path: Option<PathBuf>,
        /// Runtime identifier to restore for
        #[arg(long, default_value = "win-arm64")]
        rid: String,
    },
    /// Show what the cache holds
    Info,
    /// Empty the cache
    Clear,
}

fn main() {
    interrupt::install();
    let cli = Cli::parse();
    let verbose = cli.verbose;
    let code = match dispatch(cli) {
        Ok(code) => code,
        Err(e) if interrupt::interrupted() => {
            eprintln!("winquick: interrupted");
            let _ = e;
            130
        }
        Err(e) => {
            eprintln!("winquick: {}", if verbose { format!("{e:?}") } else { format!("{e:#}") });
            1
        }
    };
    // Anything setup mounted is released even on the error path.
    setup::release_mounts();
    std::process::exit(code);
}

fn dispatch(cli: Cli) -> Result<i32> {
    let verbose = cli.verbose;
    match cli.command {
        Cmd::Setup { from, accept_microsoft_terms, force, with } => {
            let _guard = lock::acquire_blocking("setup")?;
            setup::setup(&setup::Options { from, force, with, accept_microsoft_terms, verbose })?;
            Ok(0)
        }

        Cmd::Run {
            workspace,
            artifacts,
            artifacts_dir,
            artifact_overwrite,
            timeout,
            memory,
            cpus,
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
                workspace,
                artifacts,
                artifacts_dir: artifacts_dir.unwrap_or_else(artifact::default_dest),
                artifact_overwrite,
            },
        ),

        Cmd::Capability { action } => capability_cmd(action, verbose),
        Cmd::Cache { action } => cache_cmd(action, verbose),
        Cmd::Doctor { smoke } => doctor(smoke),
        Cmd::Info => info(),
        Cmd::Reset => {
            state::discard()?;
            println!("Prepared guest discarded; the next run will rebuild it.");
            Ok(0)
        }
        Cmd::Clean { all, dry_run } => clean(all, dry_run),
    }
}

// ------------------------------------------------------------------ argv

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
    argv.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")
}

fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for ch in a.chars() {
        match ch {
            '\\' => backslashes += 1,
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

// ------------------------------------------------------------ subcommands

fn capability_cmd(action: CapabilityCmd, verbose: bool) -> Result<i32> {
    match action {
        CapabilityCmd::List => {
            let installed = capability::installed()?;
            println!("{:<16} {:<10} {:<42} {}", "NAME", "VERSION", "WHAT IT ADDS", "STATUS");
            for sp in capability::SPECS {
                let status = match installed.iter().find(|i| i.name == sp.name) {
                    Some(i) => format!("installed, {}", helpers::human(helpers::allocated(&i.image))),
                    None => "not installed".to_string(),
                };
                println!("{:<16} {:<10} {:<42} {}", sp.name, sp.version, sp.description, status);
            }
            let unknown: Vec<&str> = installed
                .iter()
                .filter(|i| capability::spec(&i.name).is_none())
                .map(|i| i.name.as_str())
                .collect();
            if !unknown.is_empty() {
                println!("\nAlso installed, from an older WinQuick: {}", unknown.join(", "));
            }
            println!("\nInstall with:  winquick capability install <name>");
            Ok(0)
        }
        CapabilityCmd::Install { name, from } => {
            let _guard = lock::acquire_blocking("capability install")?;
            capability::install(&name, from, verbose)?;
            state::discard()?;
            println!("\nWindows will pick this up on the next run.");
            Ok(0)
        }
        CapabilityCmd::Remove { name } => {
            let _guard = lock::acquire_blocking("capability remove")?;
            if capability::remove(&name)? {
                state::discard()?;
                println!("Removed {name}.");
            } else {
                println!("{name} is not installed.");
            }
            Ok(0)
        }
    }
}

fn cache_cmd(action: CacheCmd, verbose: bool) -> Result<i32> {
    match action {
        CacheCmd::Sync { path, rid } => {
            let _guard = lock::acquire_blocking("cache sync")?;
            let p = path.unwrap_or_else(|| PathBuf::from("."));
            let r = capability::nuget_sync(&p, &rid, verbose)?;
            if r.added == 0 && !r.rebuilt {
                println!("Package cache already up to date ({} packages).", r.packages);
            } else {
                println!(
                    "Package cache updated: {} packages, {}.",
                    r.packages,
                    helpers::human(r.bytes)
                );
            }
            Ok(0)
        }
        CacheCmd::Info => {
            let dir = capability::nuget_dir()?;
            let img = capability::nuget_image()?;
            if !img.exists() {
                println!("No package cache yet.");
                println!("Populate it from a project with:  winquick cache sync");
                return Ok(0);
            }
            let packages = std::fs::read_dir(&dir)
                .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
                .unwrap_or(0);
            println!("Package cache: {packages} packages, {}", helpers::human(helpers::allocated(&img)));
            println!("  restored on this Mac into {}", dir.display());
            println!("  Windows sees a throwaway copy, so a build cannot change it");
            Ok(0)
        }
        CacheCmd::Clear => {
            let _guard = lock::acquire_blocking("cache clear")?;
            let _ = std::fs::remove_dir_all(capability::nuget_dir()?);
            let _ = std::fs::remove_file(capability::nuget_image()?);
            state::discard()?;
            println!("Package cache cleared.");
            Ok(0)
        }
    }
}

fn info() -> Result<i32> {
    println!("winquick {}", env!("CARGO_PKG_VERSION"));
    let base = paths::base_image()?;
    if base.exists() {
        println!("runtime      Windows, {}", helpers::human(helpers::allocated(&base)));
    } else {
        println!("runtime      not installed — run `winquick setup`");
    }
    match state::state_dir() {
        Ok(d) if d.join("ready.json").exists() => {
            let sz = helpers::allocated(&d.join("ready.state"));
            println!("prepared     yes, {} (makes runs fast)", helpers::human(sz));
        }
        _ => println!("prepared     no — the first run will take longer"),
    }
    let caps = capability::installed()?;
    if caps.is_empty() {
        println!("capabilities none — see `winquick capability list`");
    } else {
        for c in &caps {
            let v = capability::spec(&c.name).map(|s| s.version).unwrap_or("?");
            println!("capability   {} {} ({})", c.name, v, helpers::human(helpers::allocated(&c.image)));
        }
    }
    println!("data         {}", paths::root()?.display());
    Ok(0)
}

fn doctor(smoke: bool) -> Result<i32> {
    let mut problems: Vec<String> = Vec::new();
    println!("Host");
    let arch = std::env::consts::ARCH;
    let ok_arch = arch == "aarch64";
    println!("  {} Apple Silicon ({arch})", tick(ok_arch));
    if !ok_arch {
        problems.push("WinQuick only supports Apple Silicon Macs.".into());
    }
    let sw = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("  {} macOS {sw}", tick(!sw.is_empty()));

    println!("\nTools");
    let have_runtime = paths::base_image()?.exists();
    for t in helpers::survey() {
        match &t.path {
            Some(p) => println!("  {} {:<20} {}", tick(true), t.name, p.display()),
            None if t.needed_for == "setup only" && have_runtime => {
                // Only needed to build a runtime, and one is already built.
                println!("  {} {:<20} not installed (only needed by `winquick setup`)", tick(true), t.name);
            }
            None => {
                println!("  {} {:<20} missing ({})", tick(false), t.name, t.needed_for);
                problems.push(format!("{} is missing. {}", t.name, t.install_hint));
            }
        }
    }
    match helpers::uefi_firmware() {
        Some(p) => println!("  {} {:<20} {}", tick(true), "uefi firmware", p.display()),
        None => {
            println!("  {} {:<20} missing", tick(false), "uefi firmware");
            problems.push("QEMU's UEFI firmware is missing. brew reinstall qemu".into());
        }
    }

    println!("\nRuntime");
    let base = paths::base_image()?;
    let have_base = base.exists();
    println!(
        "  {} Windows runtime {}",
        tick(have_base),
        if have_base { helpers::human(helpers::allocated(&base)) } else { "not installed".into() }
    );
    if !have_base {
        problems.push("No Windows runtime. Run `winquick setup`.".into());
    } else if let Err(e) = state::check_base_meta(&base, setup::AGENT) {
        println!("  {} runtime is from a different WinQuick version", tick(false));
        problems.push(format!("{e:#}"));
    }
    let prepared = state::state_dir().map(|d| d.join("ready.json").exists()).unwrap_or(false);
    println!("  {} prepared guest {}", tick(prepared), if prepared { "ready" } else { "will be built on first run" });

    let caps = capability::installed()?;
    println!(
        "  · capabilities: {}",
        if caps.is_empty() { "none".to_string() } else { caps.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ") }
    );

    println!("\nDisk");
    let free = free_bytes(&paths::root()?).unwrap_or(0);
    let enough = free > 8 * 1024 * 1024 * 1024;
    println!("  {} {} free in {}", tick(enough), helpers::human(free), paths::root()?.display());
    if !enough {
        problems.push("Less than 8 GiB free. Setup and capabilities need room.".into());
    }

    if smoke && have_base && problems.is_empty() {
        println!("\nSmoke test");
        match runner::run_capture("cmd /c ver", &smoke_opts()) {
            Ok(o) if o.exit_code == 0 => {
                let s = String::from_utf8_lossy(&o.stdout);
                println!("  {} {}", tick(true), s.trim());
            }
            Ok(o) => {
                println!("  {} Windows exited {}", tick(false), o.exit_code);
                problems.push("Windows started but the test command failed.".into());
            }
            Err(e) => {
                println!("  {} {e:#}", tick(false));
                problems.push("Windows did not start.".into());
            }
        }
    }

    if problems.is_empty() {
        println!("\nEverything looks good.");
        Ok(0)
    } else {
        println!("\n{} problem(s):", problems.len());
        for p in &problems {
            println!("  - {p}");
        }
        Ok(1)
    }
}

fn smoke_opts() -> runner::Options {
    runner::Options {
        memory_mb: 1024,
        cpus: 4,
        timeout: Duration::from_secs(300),
        verbose: false,
        force_cold: false,
        workspace: None,
        artifacts: Vec::new(),
        artifacts_dir: artifact::default_dest(),
        artifact_overwrite: false,
    }
}

fn tick(ok: bool) -> &'static str {
    if ok { "ok  " } else { "FAIL" }
}

fn free_bytes(p: &std::path::Path) -> Option<u64> {
    let mut dir = p.to_path_buf();
    while !dir.exists() {
        dir = dir.parent()?.to_path_buf();
    }
    let out = std::process::Command::new("/bin/df").args(["-k"]).arg(&dir).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let avail: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail * 1024)
}

fn clean(all: bool, dry_run: bool) -> Result<i32> {
    let root = paths::root()?;
    if !root.exists() {
        println!("Nothing to clean.");
        return Ok(0);
    }
    let mut targets: Vec<(PathBuf, &str)> = vec![
        (state::state_dir()?, "prepared guest"),
        (root.join("run"), "leftover run directories"),
        (root.join("work"), "temporary build files"),
        (paths::cache()?, "downloaded installers"),
    ];
    if all {
        targets.push((root.join("images"), "Windows runtime"));
        targets.push((capability::dir()?, "capabilities"));
        targets.push((root.join("caches"), "package cache"));
    }

    let mut total = 0u64;
    let mut found = false;
    for (p, what) in &targets {
        if p.exists() {
            let sz = dir_size(p);
            total += sz;
            found = true;
            println!("  {:<28} {:>10}  {}", what, helpers::human(sz), p.display());
        }
    }
    if !found {
        println!("Nothing to clean.");
        return Ok(0);
    }
    println!("  {:<28} {:>10}", "total", helpers::human(total));

    if dry_run {
        println!("\nNothing removed (--dry-run).");
        return Ok(0);
    }
    let _guard = lock::acquire_blocking("clean")?;
    for (p, _) in &targets {
        let _ = std::fs::remove_dir_all(p);
    }
    println!("\nRemoved {}.", helpers::human(total));
    if all {
        println!("Run `winquick setup` to install Windows again.");
    } else {
        println!("The Windows runtime is still installed; the next run rebuilds the prepared guest.");
    }
    Ok(0)
}

fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() && !path.is_symlink() {
                total += dir_size(&path);
            } else {
                total += helpers::allocated(&path);
            }
        }
    }
    total
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
