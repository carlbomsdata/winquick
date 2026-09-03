//! WinQuick — run real Windows commands from macOS, Linux or Windows.

mod argv;
mod artifact;
mod artifact_patterns;
mod capability;
mod control;
mod desktop;
mod facts;
mod gpt;
mod helpers;
mod hostfs;
mod interrupt;
mod iso9660;
mod lock;
mod mailbox;
mod mcp;
mod paths;
mod platform;
mod proc;
mod qemu;
mod qmp;
mod runner;
mod servicing;
mod setup;
mod sha256;
mod state;
mod udf;
mod uiscript;

use anyhow::{Context, Result};
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
  winquick capability install desktop       add a real Windows desktop
  winquick doctor                           check the installation

Windows GUI applications:
  winquick ui-test MyApp.csproj --script my.uitest
  winquick desktop start --app ./publish
  winquick desktop launch app\\MyApp.exe
  winquick desktop screenshot app.png
  winquick desktop click --automation-id SaveButton

Every run gets a clean Windows. Files, registry keys and environment
variables written by one run are gone in the next. Windows has no network
access; see `winquick cache --help` for offline package restore.";

#[derive(Parser)]
#[command(
    name = "winquick",
    version,
    about = "Run commands inside a real, disposable Windows environment",
    long_about = "Run a command inside a real, disposable Windows environment.\n\nThink `docker run`, with a real Windows kernel on the other \
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
    #[command(
        trailing_var_arg = true,
        after_help = "\
Arguments work like `docker run`: the program and its arguments are separate
words, and anything containing spaces stays one argument.

  winquick run -- cmd /c ver
  winquick run -- cmd /c \"echo A & echo B\"
  winquick run -- pwsh -NoProfile -Command 'Write-Output \"hello world\"'

With a project (-w), the directory appears inside Windows as C:\\workspace and
becomes the working directory. It is copied in, never copied back, so the guest
cannot change your source. Ask for output explicitly with --artifact.

  winquick run -w . -- dotnet test
  winquick run -w . -a \"TestResults/**\" -- dotnet test"
    )]
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
        #[arg(long, default_value_t = runner::DEFAULT_MEMORY_MB, value_name = "MIB")]
        memory: u32,
        /// Guest processors
        #[arg(long, default_value_t = runner::DEFAULT_CPUS)]
        cpus: u32,
        /// Start Windows from scratch instead of resuming the prepared guest
        #[arg(long)]
        cold: bool,
        /// The command to run, after `--`
        #[arg(required = true, value_name = "COMMAND")]
        argv: Vec<String>,
    },

    /// Start a Windows session and leave it running
    #[command(after_help = "\
`run` boots Windows, runs one command and throws it away. A session boots it
once and leaves it up, so each following command is a round trip rather than a
boot. The session is disposable in exactly the same way: it writes to a
throwaway disk, and `winquick stop` deletes it.

  winquick start                      a session with nothing in it
  winquick start --app ./publish      with a publish directory inside Windows

Then drive it with the `winquick desktop` verbs, and finish with `winquick
stop`. `winquick status` says whether one is running.")]
    Start {
        /// Publish directory to make available inside Windows as the `app` folder
        #[arg(long, value_name = "DIR")]
        app: Option<PathBuf>,
        /// Guest memory in MiB
        #[arg(long, default_value_t = desktop::DEFAULT_MEMORY_MB, value_name = "MIB")]
        memory: u32,
        /// Guest processors
        #[arg(long, default_value_t = desktop::DEFAULT_CPUS)]
        cpus: u32,
    },

    /// Stop the running Windows session and discard it
    #[command(after_help = "\
Shuts Windows down and deletes the session's disposable disk. Nothing the
session wrote survives; copy anything you want out with `winquick desktop pull`
first.

Stopping when nothing is running is not an error.")]
    Stop,

    /// Report whether a Windows session is running
    #[command(after_help = "\
This is about the session. `winquick info` is about the installation, and
`winquick doctor` is about whether the installation works.")]
    Status,

    /// Drive a real Windows desktop: launch apps, inspect and click their UI
    #[command(after_help = "\
A desktop session boots Windows once and stays up, so each verb is a round
trip rather than a boot.

  winquick desktop start --app ./publish
  winquick desktop launch app/MyApp.exe
  winquick desktop wait-window --title \"My App\"
  winquick desktop screenshot before.png
  winquick desktop tree --title \"My App\"
  winquick desktop type --automation-id NameBox --text Tobias
  winquick desktop select --automation-id DeptCombo --item Design
  winquick desktop toggle --automation-id AdvancedCheck --state on
  winquick desktop click --automation-id SaveButton
  winquick desktop get --automation-id StatusText
  winquick desktop screenshot after.png
  winquick desktop stop

Elements are addressed by AutomationId first, then Name, ClassName or
ControlType. A selector matching more than one element is an error rather
than a guess.

Verbs other than start/stop/status/screenshot are passed to the guest bridge
unchanged: windows, display, launch, wait-window, focus, tree, find, get,
click, type, key, select, toggle, mouse.

`winquick desktop <verb> --help` describes any one of them, and answers
without a session running.")]
    Desktop {
        #[command(subcommand)]
        action: DesktopCmd,
    },

    /// Build a Windows UI application, drive it, and bring back screenshots
    #[command(after_help = "\
Takes a project file or an already-published directory, starts a desktop
session, runs a script of UI steps against the real application, and writes
the screenshots to this machine.

  winquick ui-test examples/WpfDemo/DemoApp.csproj --script examples/WpfDemo/demo.uitest
  winquick ui-test ./publish --script smoke.uitest --out ./shots

Script lines are the `winquick desktop` verbs, plus `screenshot <file>`,
`sleep <ms>` and `expect`. An expect line takes a selector and exactly one
assertion:

  --expect-name <text>            the element's name is exactly this
  --expect-name-contains <text>   ...or contains this
  --expect-value <text>           its value is exactly this
  --expect-contains <text>        ...or contains this
  --expect-toggle On|Off          a check box's state
  --expect-enabled true|false     whether the control can be used

For example:

  launch app\\DemoApp.exe
  wait-window --title \"WinQuick Demo\"
  screenshot before.png
  type --automation-id NameBox --text \"Tobias\"
  click --automation-id SaveButton
  expect --automation-id SaveButton --expect-enabled true
  expect --automation-id StatusText --expect-name-contains Saved
  screenshot after.png")]
    UiTest {
        /// A .csproj to build, or a directory that has already been published
        #[arg(value_name = "PROJECT_OR_DIR")]
        app: PathBuf,
        /// Script of UI steps [default: launch and screenshot]
        #[arg(long, value_name = "FILE")]
        script: Option<PathBuf>,
        /// Where screenshots are written
        #[arg(long, default_value = "winquick-ui", value_name = "DIR")]
        out: PathBuf,
        /// Leave the desktop session running afterwards
        #[arg(long)]
        keep: bool,
        /// Guest memory in MiB
        #[arg(long, default_value_t = desktop::DEFAULT_MEMORY_MB, value_name = "MIB")]
        memory: u32,
    },

    /// Add or remove optional tools inside Windows
    Capability {
        #[command(subcommand)]
        action: CapabilityCmd,
    },

    /// Manage the offline package cache used by `dotnet`
    #[command(after_help = "\
Windows has no network access. Packages are restored here and shared with
Windows through a cache that persists between runs.

  cd MyProject
  winquick cache sync            restore this project's packages
  winquick run -w . -- dotnet test

A project that targets .NET Framework needs reference assemblies its `.csproj`
does not mention, because on Windows they come from a developer pack:

  winquick cache add Microsoft.NETFramework.ReferenceAssemblies.net472@1.0.3

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

    /// Native MCP server for AI agents
    #[command(after_help = "\
Communicates over JSON-RPC via stdin/stdout, so an MCP client starts it as a
child process rather than connecting to a port. Nothing is written to stdout
except protocol traffic.

Register it with Claude Code:

  claude mcp add winquick -- winquick mcp

Any other MCP client wants the equivalent of:

  command: winquick
  args:    [\"mcp\"]

The tools cover disposable Windows commands, WPF and WinForms applications, UI
Automation and real Windows screenshots. See docs/mcp.md.")]
    Mcp,

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
enum DesktopCmd {
    /// Boot the desktop guest and leave it running
    Start {
        /// Publish directory to make available inside Windows as the `app` folder
        #[arg(long, value_name = "DIR")]
        app: Option<PathBuf>,
        /// Guest memory in MiB
        #[arg(long, default_value_t = desktop::DEFAULT_MEMORY_MB, value_name = "MIB")]
        memory: u32,
        /// Guest processors
        #[arg(long, default_value_t = desktop::DEFAULT_CPUS)]
        cpus: u32,
    },
    /// Shut the desktop guest down and delete its disposable disk
    Stop,
    /// Report whether a desktop session is running
    Status,
    /// Capture the screen, or one window, as a PNG on this machine
    Screenshot {
        /// Where to write the PNG
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Capture only the window whose title contains this
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// Capture the window with this handle, from `winquick desktop windows`
        /// (use when two windows share a title)
        #[arg(long, value_name = "HWND")]
        hwnd: Option<i64>,
        /// Capture a screen rectangle: x,y,width,height
        #[arg(long, value_name = "X,Y,W,H")]
        rect: Option<String>,
        /// Capture QEMU's framebuffer instead of the guest's desktop
        #[arg(long)]
        host: bool,
    },
    /// Copy a file the application produced back to this machine
    Pull {
        /// The file inside Windows, e.g. `app\out\page.png`
        #[arg(value_name = "GUEST-PATH")]
        guest_path: String,
        /// Where to write it on this machine
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Any other verb, passed to the guest bridge unchanged
    #[command(external_subcommand)]
    Bridge(Vec<String>),
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
        /// Rebuild even if it is already installed (desktop only)
        #[arg(long)]
        force: bool,
        /// Red Hat's virtio-win ISO, for the desktop display driver
        #[arg(long, value_name = "PATH")]
        virtio: Option<PathBuf>,
    },
    /// Remove a capability
    Remove { name: String },
}

#[derive(Subcommand)]
enum CacheCmd {
    /// Restore a project's packages here and share them with Windows
    Sync {
        /// Project or solution directory [default: .]
        path: Option<PathBuf>,
        /// Runtime identifier to restore for
        #[arg(long, default_value = "win-arm64")]
        rid: String,
    },
    /// Add named packages to the cache, without touching any project
    #[command(after_help = "\
`sync` answers \"what does this project need\". This answers \"this project
needs something its author never declared\" — which is the normal situation for
a .NET Framework project, because on Windows its reference assemblies come from
a developer pack rather than from NuGet.

  winquick cache add Microsoft.NETFramework.ReferenceAssemblies.net472@1.0.3
  winquick cache add Newtonsoft.Json

A version pins exactly what is fetched; without one you get the latest.")]
    Add {
        /// Package names, as `Name` or `Name@1.2.3`
        #[arg(required = true)]
        packages: Vec<String>,
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
        } => {
            artifact_patterns::validate(&artifacts)?;
            runner::run(
                &argv::join(&argv),
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
            )
        }

        Cmd::Desktop { action } => desktop_cmd(action, verbose),
        // The session lifecycle is one implementation with two spellings: the
        // top-level verbs are what a new user reaches for, and the `desktop`
        // ones keep working for anything already written against them.
        Cmd::Start { app, memory, cpus } => {
            desktop_cmd(DesktopCmd::Start { app, memory, cpus }, verbose)
        }
        Cmd::Stop => desktop_cmd(DesktopCmd::Stop, verbose),
        Cmd::Status => desktop_cmd(DesktopCmd::Status, verbose),
        Cmd::UiTest { app, script, out, keep, memory } => {
            ui_test(&app, script.as_deref(), &out, keep, memory, verbose)
        }
        Cmd::Capability { action } => capability_cmd(action, verbose),
        Cmd::Cache { action } => cache_cmd(action, verbose),
        Cmd::Doctor { smoke } => doctor(smoke),
        Cmd::Info => info(),
        Cmd::Mcp => mcp::serve(),
        Cmd::Reset => {
            state::discard()?;
            state::discard_desktop()?;
            println!("Prepared guest and desktop discarded; the next run rebuilds them.");
            Ok(0)
        }
        Cmd::Clean { all, dry_run } => clean(all, dry_run),
    }
}

// ------------------------------------------------------------ subcommands

/// Build or accept an application, drive its UI, and report.
fn ui_test(
    app: &std::path::Path,
    script: Option<&std::path::Path>,
    out: &std::path::Path,
    keep: bool,
    memory: u32,
    verbose: bool,
) -> Result<i32> {
    if desktop::running().is_some() {
        anyhow::bail!(
            "a desktop session is already running.\n\n\
             `ui-test` needs its own, because the application is baked into the\n\
             session's volume when it starts. Stop the current one with:\n    \
             winquick desktop stop"
        );
    }

    // A project has to be built first; a directory is taken as already published.
    let published = if app.is_dir() { app.to_path_buf() } else { build_project(app, verbose)? };
    let exe = sole_executable(&published)?;

    let text = match script {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
        }
        // Without a script the useful thing to prove is that it starts and
        // draws something.
        None => format!("launch app\\{exe}\nsleep 4000\nscreenshot launched.png\nwindows\n"),
    };
    let parsed = uiscript::parse(&text)?;

    println!("Starting a desktop session with {}...", published.display());
    desktop::start(&desktop::StartOptions {
        app: Some(published),
        memory_mb: memory,
        cpus: desktop::DEFAULT_CPUS,
        verbose,
    })?;

    let report = desktop::run_script(&parsed, out, Duration::from_secs(120));
    if !keep {
        let _ = desktop::stop();
    }
    let report = report?;

    println!();
    if report.failed.is_empty() {
        println!("{} steps passed.", report.passed);
    } else {
        println!("{} passed, {} failed:", report.passed, report.failed.len());
        for f in &report.failed {
            println!("  - {f}");
        }
    }
    if !report.screenshots.is_empty() {
        println!("Screenshots in {}:", out.display());
        for s in &report.screenshots {
            println!("  {}", s.display());
        }
    }
    if keep {
        println!("\nThe desktop session is still running; stop it with `winquick stop`.");
    }
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}

/// Publish a project inside Windows and bring the output back.
fn build_project(project: &std::path::Path, verbose: bool) -> Result<PathBuf> {
    let dir =
        project.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    let name = project
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("{} is not a project file", project.display()))?;
    let dest = std::env::temp_dir().join(format!("winquick-uitest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);

    println!("Building {} inside Windows...", project.display());
    let outcome = runner::run_capture(
        &format!("dotnet publish {name} -c Release -o publish --nologo"),
        &runner::Options {
            // The same shape as a plain `winquick run`, so this shares its
            // prepared guest instead of invalidating it and making the next
            // ordinary command pay for a rebuild.
            memory_mb: runner::DEFAULT_MEMORY_MB,
            cpus: runner::DEFAULT_CPUS,
            timeout: Duration::from_secs(900),
            verbose,
            force_cold: false,
            workspace: Some(dir.to_path_buf()),
            artifacts: vec!["publish/**".to_string()],
            artifacts_dir: dest.clone(),
            artifact_overwrite: true,
        },
    )?;
    if outcome.exit_code != 0 {
        anyhow::bail!(
            "the build failed:\n{}\n{}",
            String::from_utf8_lossy(&outcome.stdout).replace('\r', ""),
            String::from_utf8_lossy(&outcome.stderr).replace('\r', "")
        );
    }
    let nested = dest.join("publish");
    Ok(if nested.is_dir() { nested } else { dest })
}

/// The application to launch, when the script does not say.
fn sole_executable(dir: &std::path::Path) -> Result<String> {
    let mut exes: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.to_ascii_lowercase().ends_with(".exe"))
        .collect();
    exes.sort();
    match exes.len() {
        0 => anyhow::bail!("no .exe in {}", dir.display()),
        1 => Ok(exes.remove(0)),
        _ => anyhow::bail!(
            "{} has several executables ({}); say which to launch with a --script",
            dir.display(),
            exes.join(", ")
        ),
    }
}

/// The desktop verbs.
///
/// Only the session's own lifecycle and screen capture are handled here. Every
/// other verb is forwarded to the guest bridge verbatim, which keeps this
/// command from drifting out of step with what the bridge actually supports.
fn desktop_cmd(action: DesktopCmd, verbose: bool) -> Result<i32> {
    const CALL_TIMEOUT: Duration = Duration::from_secs(120);

    match action {
        DesktopCmd::Start { app, memory, cpus } => {
            let _guard = lock::acquire_blocking("desktop start")?;
            desktop::start(&desktop::StartOptions {
                app: app.clone(),
                memory_mb: memory,
                cpus,
                verbose,
            })?;
            println!("Desktop session ready.");
            if app.is_some() {
                println!("Your publish directory is available inside Windows as `app`.");
            }
            println!("Launch something with:  winquick desktop launch app\\<YourApp>.exe");
            Ok(0)
        }

        DesktopCmd::Stop => {
            let _guard = lock::acquire_blocking("desktop stop")?;
            if desktop::stop()? {
                println!("Desktop session stopped.");
            } else {
                println!("No desktop session was running.");
            }
            Ok(0)
        }

        DesktopCmd::Status => match desktop::running() {
            Some(s) => {
                println!("Desktop session running (pid {}).", s.pid);
                if let Some(app) = &s.app {
                    println!("  app: {app}");
                }
                Ok(0)
            }
            None => {
                println!("No desktop session is running.");
                println!("Start one with:  winquick start");
                Ok(1)
            }
        },

        DesktopCmd::Pull { guest_path, file } => {
            let json = desktop::pull(&guest_path, &file, CALL_TIMEOUT)?;
            println!(
                "Wrote {} ({} bytes, sha256 {}).",
                file.display(),
                json.get("bytes").and_then(|v| v.as_i64()).unwrap_or(0),
                json.get("sha256").and_then(|v| v.as_str()).unwrap_or("?")
            );
            Ok(0)
        }
        DesktopCmd::Screenshot { file, title, hwnd, rect, host } => {
            if host {
                let png = desktop::host_screenshot(&file)?;
                println!("Wrote {} ({} bytes) from QEMU's framebuffer.", file.display(), png);
                return Ok(0);
            }
            let mut extra = Vec::new();
            if let Some(t) = title {
                extra.push("--title".into());
                extra.push(t);
            }
            if let Some(h) = hwnd {
                extra.push("--hwnd".into());
                extra.push(h.to_string());
            }
            if let Some(r) = rect {
                extra.push("--rect".into());
                extra.push(r);
            }
            let json = desktop::screenshot(&file, &extra, CALL_TIMEOUT)?;
            let non_black = json.get("nonBlackFraction").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let colors = json.get("distinctColors").and_then(|v| v.as_i64()).unwrap_or(0);
            println!(
                "Wrote {} ({}x{}, {:.1}% non-black, {colors} distinct colours).",
                file.display(),
                json.get("width").and_then(|v| v.as_i64()).unwrap_or(0),
                json.get("height").and_then(|v| v.as_i64()).unwrap_or(0),
                non_black * 100.0
            );
            Ok(0)
        }

        DesktopCmd::Bridge(argv) => {
            // Syntax before state: an unrecognised verb used to be reported as
            // "no desktop session is running", which sends the reader looking
            // in the wrong place entirely.
            desktop::check_verb(argv.first().map(String::as_str))?;
            // `--help` is a question about the CLI, not a request for the
            // guest. Answering it here means it works before Windows is
            // running, which is when someone actually asks.
            if argv[1..].iter().any(|a| a == "--help" || a == "-h") {
                if let Some(h) = desktop::verb_help(&argv[0]) {
                    println!("{h}");
                    return Ok(0);
                }
            }
            let r = desktop::call(&argv, CALL_TIMEOUT)?;
            // The bridge already speaks JSON; pretty-print it so a person can
            // read it and a script can still parse it.
            match &r.json {
                Some(v) => println!("{}", serde_json::to_string_pretty(v)?),
                None => {
                    std::io::Write::write_all(&mut std::io::stdout(), &r.stdout)?;
                    std::io::Write::write_all(&mut std::io::stderr(), &r.stderr)?;
                }
            }
            if r.exit_code != 0 {
                return Err(anyhow::anyhow!("{}", desktop::describe_failure(&r)));
            }
            Ok(0)
        }
    }
}

fn capability_cmd(action: CapabilityCmd, verbose: bool) -> Result<i32> {
    match action {
        CapabilityCmd::List => {
            let installed = capability::installed()?;
            println!("{:<16} {:<10} {:<42} STATUS", "NAME", "VERSION", "WHAT IT ADDS");
            for sp in capability::SPECS {
                let status = match installed.iter().find(|i| i.name == sp.name) {
                    Some(i) => {
                        format!("installed, {}", helpers::human(helpers::allocated(&i.image)))
                    }
                    None => "not installed".to_string(),
                };
                println!("{:<16} {:<10} {:<42} {}", sp.name, sp.version, sp.description, status);
            }
            // Serviced into the Windows image rather than downloaded, so it is
            // not in SPECS.
            let netfx = paths::framework_image()?;
            println!(
                "{:<16} {:<10} {:<42} {}",
                servicing::FRAMEWORK_CAPABILITY,
                "serviced",
                ".NET Framework: run and build classic projects",
                if netfx.exists() {
                    format!("installed, {}", helpers::human(helpers::allocated(&netfx)))
                } else {
                    "not installed".to_string()
                }
            );
            // Built rather than downloaded, so it is not in SPECS.
            let desk = desktop::base_image().map(|p| p.exists()).unwrap_or(false);
            println!(
                "{:<16} {:<10} {:<42} {}",
                desktop::CAPABILITY,
                "built",
                "Windows desktop: WPF/WinForms, UI automation",
                if desk {
                    match desktop::base_image() {
                        Ok(p) => format!("installed, {}", helpers::human(helpers::allocated(&p))),
                        Err(_) => "installed".to_string(),
                    }
                } else {
                    "not installed".to_string()
                }
            );
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
        CapabilityCmd::Install { name, from, force, virtio } => {
            let _guard = lock::acquire_blocking("capability install")?;
            // `desktop` is not a downloadable archive like the others: it builds
            // a second Windows image by servicing a copy of the first.
            if name == desktop::CAPABILITY {
                servicing::install(&servicing::Options { verbose, force, virtio })?;
                return Ok(0);
            }
            // Also not a downloadable archive: .NET Framework is part of
            // Windows, and it arrives by servicing the image with Microsoft's
            // own package rather than by unzipping something.
            if name == servicing::FRAMEWORK_CAPABILITY {
                servicing::install_framework(&servicing::Options { verbose, force, virtio })?;
                return Ok(0);
            }
            capability::install(&name, from, verbose)?;
            state::discard()?;
            println!("\nWindows will pick this up on the next run.");
            Ok(0)
        }
        CapabilityCmd::Remove { name } => {
            let _guard = lock::acquire_blocking("capability remove")?;
            if name == servicing::FRAMEWORK_CAPABILITY {
                let img = paths::framework_image()?;
                let dir = img.parent().unwrap().to_path_buf();
                if img.exists() {
                    // The whole directory: the image and the runtime metadata
                    // beside it are one thing.
                    std::fs::remove_dir_all(&dir)?;
                    state::discard()?;
                    println!("Removed {name}. `winquick run` is back on the plain image.");
                } else {
                    println!("{name} is not installed.");
                }
                return Ok(0);
            }
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
        CacheCmd::Add { packages } => {
            let _guard = lock::acquire_blocking("cache add")?;
            let r = capability::nuget_add(&packages, verbose)?;
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
            println!(
                "Package cache: {packages} packages, {}",
                helpers::human(helpers::allocated(&img))
            );
            println!("  restored here into {}", dir.display());
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

/// Render the same facts the MCP server serialises.
fn info() -> Result<i32> {
    let i = facts::info()?;
    println!("winquick {}", i.version);
    if i.runtime_installed {
        println!("runtime      Windows, {}", helpers::human(i.runtime_bytes));
    } else {
        println!("runtime      not installed — run `winquick setup`");
    }
    if i.prepared {
        println!("prepared     yes, {} (makes runs fast)", helpers::human(i.prepared_bytes));
    } else {
        println!("prepared     no — the first run will take longer");
    }
    if i.capabilities.is_empty() {
        println!("capabilities none — see `winquick capability list`");
    } else {
        for c in &i.capabilities {
            println!(
                "capability   {} {} ({})",
                c.name,
                c.version.as_deref().unwrap_or(""),
                helpers::human(c.bytes)
            );
        }
    }
    if let Some(b) = i.package_cache_bytes {
        println!("packages     cached, {}", helpers::human(b));
    }
    {
        let netfx = paths::framework_image()?;
        if netfx.exists() {
            println!(
                "capability   dotnet-framework ({}), classic .NET Framework projects",
                helpers::human(helpers::allocated(&netfx))
            );
        }
    }
    if i.desktop.installed {
        println!(
            "capability   desktop ({}), WPF/WinForms and UI automation",
            helpers::human(i.desktop.bytes)
        );
        if i.desktop.prepared {
            println!(
                "desktop      prepared, {} (sessions start in ~0.4 s)",
                helpers::human(i.desktop.prepared_bytes)
            );
        } else {
            println!("desktop      not prepared yet (the first session takes ~20 s)");
        }
        match i.desktop.session_pid {
            Some(pid) => println!("session      running, pid {pid}"),
            None => println!("session      none running"),
        }
    } else {
        println!("capability   desktop not installed (winquick capability install desktop)");
    }
    println!("data         {}", i.data_dir);
    Ok(0)
}

fn doctor(smoke: bool) -> Result<i32> {
    let report = facts::doctor()?;
    let mut problems = report.problems.clone();

    let mut section = "";
    for c in &report.checks {
        if c.section != section {
            if !section.is_empty() {
                println!();
            }
            println!("{}", c.section);
            section = c.section;
        }
        let mark = match c.status {
            facts::Status::Ok => tick(true).to_string(),
            facts::Status::Fail => tick(false).to_string(),
            // A note is neither a pass nor a fault; it is context.
            facts::Status::Note => "·   ".to_string(),
        };
        println!("  {mark} {:<20} {}", c.name, c.message);
    }

    let have_base = paths::base_image()?.exists();
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
        memory_mb: runner::DEFAULT_MEMORY_MB,
        cpus: runner::DEFAULT_CPUS,
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
    if ok {
        "ok  "
    } else {
        "FAIL"
    }
}

fn clean(all: bool, dry_run: bool) -> Result<i32> {
    let root = paths::root()?;
    if !root.exists() {
        println!("Nothing to clean.");
        return Ok(0);
    }
    // A running desktop session holds its disk open; stopping it first keeps
    // `clean` from leaving an orphaned QEMU behind with no session file.
    if let Some(session) = desktop::running() {
        if dry_run {
            println!("  {:<28} {:>10}  pid {}", "running desktop session", "", session.pid);
        } else {
            desktop::stop()?;
            println!("Stopped the running desktop session.");
        }
    }

    let mut targets: Vec<(PathBuf, &str)> = vec![
        (state::state_dir()?, "prepared guest"),
        (state::desktop_state_dir()?, "prepared desktop"),
        (root.join("run"), "leftover run directories"),
        (root.join("work"), "temporary build files"),
        (desktop::dir()?, "desktop session"),
        (paths::cache()?, "downloaded installers"),
        // `clean` is the "forget what you worked out about this machine"
        // command, and installing a QEMU that can restore a prepared guest is
        // exactly the kind of change a user runs it after.
        (state::restore_note()?, "restore-unsupported note"),
        (state::restore_works_note()?, "restore-works note"),
    ];
    if all {
        targets.push((root.join("images"), "Windows runtime"));
        targets.push((capability::dir()?, "capabilities"));
        targets.push((root.join("caches"), "package cache"));
        targets.push((desktop::bridge_dir()?, "desktop bridge"));
    }

    let mut total = 0u64;
    let mut found = false;
    for (p, what) in &targets {
        if p.exists() {
            let sz = if p.is_dir() { facts::dir_size(p) } else { helpers::allocated(p) };
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
        let _ = std::fs::remove_file(p);
    }
    println!("\nRemoved {}.", helpers::human(total));
    if all {
        println!("Run `winquick setup` to install Windows again.");
    } else {
        println!(
            "The Windows runtime is still installed; the next run rebuilds the prepared guest."
        );
    }
    Ok(0)
}
