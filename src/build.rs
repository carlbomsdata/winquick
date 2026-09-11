//! `winquick build` — one command from a project to a Windows binary on the host.
//!
//! `winquick run` is `docker run`; this is the missing `docker build`. It reads
//! the project, picks the toolchain its *shape* implies, makes sure the guest
//! has what it needs, restores packages offline, builds in a disposable Windows,
//! and — unlike `run` — keeps the output by default.
//!
//! The rule it is built around, from real use: the smart command may be
//! *unhelpful*, never *wrong*. The one place it could be wrong is target
//! framework versus available compiler — a project targeting .NET ≤ 3.5 built by
//! the guest's v4 compiler yields a binary that looks perfect and silently
//! breaks the old-runtime support it promises. So it refuses that case with the
//! manual recipe rather than guessing.

use crate::{capability, runner};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub struct Options {
    /// The project or the directory holding it.
    pub project: PathBuf,
    pub config: String,
    /// Where retrieved output lands; defaults to `winquick-artifacts/`.
    pub out: Option<PathBuf>,
    /// Override the auto-detected output glob.
    pub keep: Option<String>,
    /// Build only; keep nothing (the old `run` default).
    pub no_keep: bool,
    /// Install a needed capability without prompting.
    pub yes: bool,
    /// Print the plan and stop.
    pub dry_run: bool,
    pub timeout: std::time::Duration,
    pub verbose: bool,
}

/// A project's shape decides its toolchain, and getting that wrong is a
/// confusing failure rather than a clean one — so it is read, not guessed.
#[derive(Debug, PartialEq)]
enum Shape {
    /// `<Project Sdk="...">` — `dotnet build`.
    Sdk,
    /// A classic `<Project ToolsVersion=...>` — `dotnet msbuild`.
    Classic,
}

struct Project {
    /// The `.csproj`, absolute.
    file: PathBuf,
    /// The directory mounted into the guest as the workspace.
    workspace: PathBuf,
    /// The project path relative to the workspace, forward-slashed for Windows.
    rel: String,
    shape: Shape,
    /// The target as written: `net8.0`, `net472`, `v3.5`, …
    target: String,
}

/// What the build will do, or why it will not.
enum Plan {
    Build {
        /// The command run inside Windows.
        command: String,
        /// The capability the build needs, if it is not already present.
        need: Option<Capability>,
        /// The glob copied back out afterwards.
        keep: String,
    },
    /// Refuse rather than emit a silently-wrong binary or a confusing error.
    Refuse(String),
}

#[derive(Clone, Copy)]
enum Capability {
    DotnetSdk,
}

impl Capability {
    fn name(self) -> &'static str {
        match self {
            Capability::DotnetSdk => "dotnet-sdk",
        }
    }
    fn present(self) -> bool {
        match self {
            Capability::DotnetSdk => {
                capability::image_path("dotnet-sdk").map(|p| p.exists()).unwrap_or(false)
            }
        }
    }
    /// Roughly how much installing it writes, for the announcement.
    fn size_hint(self) -> &'static str {
        match self {
            Capability::DotnetSdk => "about 840 MiB, one-time",
        }
    }
}

pub fn build(opts: &Options) -> Result<i32> {
    let project = resolve(&opts.project)?;
    let plan = plan_for(&project, opts)?;

    match &plan {
        Plan::Refuse(why) => {
            eprintln!("winquick: {why}");
            // A refusal is a deliberate stop, not a crash; exit non-zero so a
            // script notices, but say plainly what happened.
            Ok(2)
        }
        Plan::Build { command, need, keep } => {
            let dest = opts.out.clone().unwrap_or_else(crate::artifact::default_dest);
            print_plan(&project, command, *need, keep, &dest, opts);
            if opts.dry_run {
                return Ok(0);
            }
            if let Some(cap) = need {
                ensure_capability(*cap, opts)?;
            }
            // Restore packages on the host first: the guest has no network, and
            // this removes the NU1301 detour before it happens.
            eprintln!("winquick: syncing the package cache for this project…");
            let _ = capability::nuget_sync(&project.file, "", opts.verbose)?;

            let artifacts = if opts.no_keep { Vec::new() } else { vec![keep.clone()] };
            let code = runner::run(
                command,
                &runner::Options {
                    memory_mb: runner::DEFAULT_MEMORY_MB,
                    cpus: runner::DEFAULT_CPUS,
                    timeout: opts.timeout,
                    verbose: opts.verbose,
                    force_cold: false,
                    force_warm: false,
                    workspace: Some(project.workspace.clone()),
                    artifacts,
                    artifacts_dir: dest.clone(),
                    artifact_overwrite: true,
                },
            )?;
            if code == 0 {
                if opts.no_keep {
                    eprintln!(
                        "winquick: built, kept nothing (--no-keep). Drop the flag to copy \
                         the output back."
                    );
                } else {
                    // Say exactly where, absolute — the whole point is not to
                    // leave the user hunting for output that did land somewhere.
                    eprintln!("winquick: build output kept in {}", absolute(&dest).display());
                }
            }
            Ok(code)
        }
    }
}

/// Find the one project to build, or say why the choice is not obvious.
fn resolve(arg: &Path) -> Result<Project> {
    let arg = arg.canonicalize().with_context(|| format!("{} does not exist", arg.display()))?;

    // A directory the user names is the workspace they chose; a bare project
    // file needs its repo mounted, because real projects live in a subdirectory
    // and reference things above them — a `.snk`, `Directory.Build.props`, linked
    // sources. Mounting only the project's own folder loses those and the build
    // fails on a file that is right there in the repo.
    let (file, workspace) = if arg.is_dir() {
        let mut hits: Vec<PathBuf> = Vec::new();
        find_csproj(&arg, 0, &mut hits);
        let file = match hits.len() {
            0 => bail!(
                "no .csproj found under {}.\n\
                 Point `winquick build` at the project file itself.",
                arg.display()
            ),
            1 => hits.pop().unwrap(),
            _ => {
                let list = hits.iter().map(|p| format!("  {}", p.display())).collect::<Vec<_>>();
                bail!(
                    "more than one project under {} — name the one to build:\n{}",
                    arg.display(),
                    list.join("\n")
                );
            }
        };
        (file, arg.clone())
    } else {
        if arg.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("sln"))
            == Some(true)
        {
            bail!("solutions are not supported yet — point `winquick build` at a .csproj");
        }
        let ws = repo_root(&arg);
        (arg.clone(), ws)
    };

    let rel = file.strip_prefix(&workspace).unwrap_or(&file).to_string_lossy().replace('\\', "/");

    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let (shape, target) = classify(&text);
    Ok(Project { file, workspace, rel, shape, target })
}

/// The directory to mount for a bare project file: the repo it lives in.
///
/// Walk up from the project looking for the markers that mean "this is the top
/// of the thing" — a `.git`, a `.sln`, or a `Directory.Build.props`/`.targets`
/// that the project inherits. Fall back to the project's own directory when
/// there is no such marker (a loose `.csproj` in a plain folder).
fn repo_root(project: &Path) -> PathBuf {
    let start = project.parent().unwrap_or(Path::new("."));
    let mut best: Option<PathBuf> = None;
    let mut dir = Some(start);
    while let Some(d) = dir {
        let has_marker = d.join(".git").exists()
            || d.join("Directory.Build.props").exists()
            || d.join("Directory.Build.targets").exists()
            || std::fs::read_dir(d)
                .map(|rd| {
                    rd.flatten().any(|e| {
                        e.path()
                            .extension()
                            .and_then(|x| x.to_str())
                            .map(|x| x.eq_ignore_ascii_case("sln"))
                            == Some(true)
                    })
                })
                .unwrap_or(false);
        if has_marker {
            best = Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    best.unwrap_or_else(|| start.to_path_buf())
}

/// Depth-limited search, so pointing at a large repo does not walk the world.
fn find_csproj(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let skip = matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("bin" | "obj" | ".git" | "node_modules" | "winquick-artifacts")
            );
            if !skip {
                find_csproj(&p, depth + 1, out);
            }
        } else if p.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("csproj"))
            == Some(true)
        {
            out.push(p);
        }
    }
}

/// Read shape and target from the `.csproj` text — cheap, no XML dependency.
fn classify(text: &str) -> (Shape, String) {
    // SDK-style is the `Sdk=` attribute on the `<Project>` element.
    let sdk = text
        .split_once("<Project")
        .map(|(_, rest)| {
            let head = rest.split('>').next().unwrap_or("");
            head.contains("Sdk=")
        })
        .unwrap_or(false);
    let shape = if sdk { Shape::Sdk } else { Shape::Classic };

    let target = tag(text, "TargetFramework")
        .or_else(|| tag(text, "TargetFrameworks").map(|s| first_target(&s)))
        .or_else(|| tag(text, "TargetFrameworkVersion"))
        .unwrap_or_default();
    (shape, target)
}

fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let val = text[start..end].trim().to_string();
    // Real projects set the target in Directory.Build.props or via an MSBuild
    // property; a `$(...)` value is a reference this text scan cannot resolve, so
    // treat it as unknown rather than printing the variable. Harmless for the
    // dispatch (SDK-style builds the same whatever the target), and the pre-v4
    // refusal keys on a literal version, which a property is not.
    if val.is_empty() || val.contains("$(") {
        None
    } else {
        Some(val)
    }
}

fn first_target(s: &str) -> String {
    s.split(';').next().unwrap_or(s).trim().to_string()
}

/// Turn a project into a plan, or a refusal.
fn plan_for(p: &Project, opts: &Options) -> Result<Plan> {
    if targets_pre_v4(&p.target) {
        return Ok(Plan::Refuse(format!(
            "this project targets {tfm}, and the guest has only the .NET Framework 4 \
             compiler.\n\
             Building it with that compiler would produce a v4 binary that looks fine and \
             silently loses the ≤3.5 / XP support the project targets — so winquick build \
             will not guess here.\n\n\
             Build it explicitly instead: v4 `csc.exe` with `/noconfig /nostdlib+` and \
             `/r:` pointed at the cached .NET 3.5 reference assemblies. See docs/dotnet.md \
             (\"Building for Windows XP-era targets\"). This case is planned for a later \
             winquick build.",
            tfm = p.target
        )));
    }

    let config = shell_quote(&opts.config);
    let proj = shell_quote(&p.rel);
    let command = match p.shape {
        // The SDK reads its own project directly.
        Shape::Sdk => format!("dotnet build {proj} -c {config} --nologo"),
        // A classic project builds through MSBuild; `dotnet msbuild` is the more
        // direct route than `dotnet build`, which adds a restore it does not need.
        Shape::Classic => {
            format!("dotnet msbuild {proj} -p:Configuration={config} -nologo")
        }
    };

    let need = (!Capability::DotnetSdk.present()).then_some(Capability::DotnetSdk);

    // Output lives under the project's own bin/<Config>; keep that subtree.
    let keep = opts.keep.clone().unwrap_or_else(|| {
        let dir = Path::new(&p.rel).parent().map(|d| d.to_string_lossy().to_string());
        match dir.as_deref() {
            Some("") | None => format!("bin/{}/**", opts.config),
            Some(d) => format!("{d}/bin/{}/**", opts.config),
        }
    });

    Ok(Plan::Build { command, need, keep })
}

/// Does the target predate the .NET Framework 4 runtime the guest ships?
///
/// Classic projects write `<TargetFrameworkVersion>v3.5</TargetFrameworkVersion>`;
/// v2.0, v3.0 and v3.5 all run on CLR 2.0, which the guest does not have.
fn targets_pre_v4(target: &str) -> bool {
    matches!(target.trim(), "v2.0" | "v3.0" | "v3.5")
}

fn print_plan(
    p: &Project,
    command: &str,
    need: Option<Capability>,
    keep: &str,
    dest: &Path,
    opts: &Options,
) {
    let shape = match p.shape {
        Shape::Sdk => "SDK-style",
        Shape::Classic => "classic (non-SDK)",
    };
    eprintln!("winquick build plan:");
    eprintln!("  project    {}", p.file.display());
    eprintln!(
        "  shape      {shape}, target {}",
        if p.target.is_empty() { "(unset)" } else { &p.target }
    );
    eprintln!("  command    {command}");
    match need {
        Some(c) => {
            eprintln!("  capability {} — not installed, will install ({})", c.name(), c.size_hint())
        }
        None => eprintln!("  capability dotnet-sdk — already installed"),
    }
    eprintln!("  cache      winquick cache sync (this project)");
    if opts.no_keep {
        eprintln!("  keep       nothing (--no-keep)");
    } else {
        eprintln!("  keep       -a \"{keep}\"  ->  {}", absolute(dest).display());
    }
}

/// Install a needed capability, but never silently — a serviced or downloaded
/// image is a big, visible thing, and doing it mid-build without a word is the
/// one thing the field feedback said not to do.
fn ensure_capability(cap: Capability, opts: &Options) -> Result<()> {
    if cap.present() {
        return Ok(());
    }
    eprintln!("winquick: this build needs the {} capability ({}).", cap.name(), cap.size_hint());
    if !opts.yes && !confirm() {
        bail!("declined. Install it yourself with:  winquick capability install {}", cap.name());
    }
    eprintln!("winquick: installing {}…", cap.name());
    match cap {
        Capability::DotnetSdk => {
            capability::install("dotnet-sdk", None, opts.verbose)?;
            crate::state::discard()?;
        }
    }
    Ok(())
}

/// A yes/no prompt on the real terminal. Not a TTY (a pipe, CI) means no —
/// automation should pass `--yes` rather than have a prompt answered for it.
fn confirm() -> bool {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        eprintln!("winquick: not a terminal; pass --yes to install without asking.");
        return false;
    }
    eprint!("Install it now? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}

/// cmd.exe quoting: wrap in double quotes when the token has a space.
/// A displayable absolute path, even for a directory that does not exist yet
/// (so the plan and the result name the same, unambiguous place).
fn absolute(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p),
        Err(_) => p.to_path_buf(),
    }
}

fn shell_quote(s: &str) -> String {
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_and_target_are_read_from_the_csproj() {
        let sdk = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net472</TargetFramework></PropertyGroup></Project>"#;
        assert_eq!(classify(sdk), (Shape::Sdk, "net472".to_string()));

        let classic = r#"<?xml version="1.0"?><Project ToolsVersion="4.0" xmlns="http://schemas.microsoft.com/developer/msbuild/2003"><PropertyGroup><TargetFrameworkVersion>v3.5</TargetFrameworkVersion></PropertyGroup></Project>"#;
        assert_eq!(classify(classic), (Shape::Classic, "v3.5".to_string()));

        let multi = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>net8.0;net472</TargetFrameworks></PropertyGroup></Project>"#;
        assert_eq!(classify(multi), (Shape::Sdk, "net8.0".to_string()));
    }

    #[test]
    fn a_property_valued_or_missing_target_reads_as_unknown() {
        // Serilog-style: TargetFrameworks is an MSBuild property, not a literal.
        let prop = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>$(TargetFrameworksLibrary)</TargetFrameworks></PropertyGroup></Project>"#;
        assert_eq!(classify(prop), (Shape::Sdk, String::new()));
        // No target element at all (set in Directory.Build.props).
        let none = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup></PropertyGroup></Project>"#;
        assert_eq!(classify(none), (Shape::Sdk, String::new()));
        // A property-valued target must NOT trip the pre-v4 refusal.
        assert!(!targets_pre_v4(""));
    }

    #[test]
    fn a_pre_v4_target_is_refused_not_guessed() {
        for t in ["v2.0", "v3.0", "v3.5"] {
            assert!(targets_pre_v4(t), "{t} runs on CLR 2.0 the guest lacks");
        }
        for t in ["v4.0", "net472", "net8.0", ""] {
            assert!(!targets_pre_v4(t), "{t} is fine on the v4 compiler");
        }
    }
}
