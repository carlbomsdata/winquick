//! Deciding which files a run should hand back.
//!
//! The workspace is one-way on purpose: the guest gets a throwaway copy and
//! nothing it writes reaches the Mac. Retrieving build output is therefore a
//! separate, explicit request, and this module is what `--artifact` patterns
//! mean.
//!
//! Matching happens **in the guest**, by Windows, because that is where the
//! files are — the host never sees the tree as the build left it. Everything
//! here compiles to `cmd` built-ins and `xcopy`, both inbox, so artifact
//! extraction needs no capability and no compiled guest code.
//!
//! v0.2.0 supported three fixed shapes. This is a real subset of glob:
//!
//! | Pattern | Meaning |
//! |---|---|
//! | `bin/Release/**` | that directory, recursively, hierarchy preserved |
//! | `**/*.dll` | every `.dll` anywhere under the workspace |
//! | `bin/**/*.exe` | every `.exe` anywhere under `bin` |
//! | `*.log` / `logs/*.txt` | wildcard within one directory |
//! | `foo?.txt` | `?` matches exactly one character |
//! | `out/report.txt` | one named file or directory |
//!
//! Slashes may lean either way, so a pattern copied from a Windows script works
//! unchanged.

use anyhow::{bail, Result};

/// Directory on the artifact volume the guest copies into.
pub const DIR: &str = "artifacts";

/// What a single `--artifact` pattern asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Pattern {
    /// A whole directory tree: `bin/Release/**`.
    Tree { dir: String },
    /// A wildcard applied at every depth below a directory: `bin/**/*.dll`.
    Recursive { dir: String, glob: String },
    /// A wildcard applied in one directory only: `logs/*.txt`.
    Shallow { dir: String, glob: String },
    /// One named file or directory: `out/report.txt`.
    Exact { path: String },
}

/// Parse one pattern, refusing anything that could reach outside the workspace.
pub fn parse(raw: &str) -> Result<Pattern> {
    let norm = raw.replace('/', "\\");
    let norm = norm.trim_start_matches('\\').trim_end_matches('\\').to_string();
    if norm.is_empty() {
        bail!("an artifact pattern cannot be empty");
    }
    // Patterns name things inside the workspace, and stay there. Rejecting this
    // here means a traversal attempt never reaches the guest, let alone the
    // extraction step.
    if norm.split('\\').any(|p| p == "..") {
        bail!("artifact pattern {raw:?} must not contain `..`");
    }
    if norm.chars().nth(1) == Some(':') {
        bail!("artifact pattern {raw:?} must be relative to the workspace, not an absolute path");
    }

    let parts: Vec<&str> = norm.split('\\').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        bail!("an artifact pattern cannot be empty");
    }

    // `**` decides the shape, wherever it appears. A trailing `**` is the tree
    // above it; a `**` further left applies the final element at any depth.
    // A single `*` stays one level deep, as it does in every other glob.
    if parts.last() == Some(&"**") {
        return Ok(Pattern::Tree { dir: parts[..parts.len() - 1].join("\\") });
    }
    if let Some(i) = parts.iter().position(|p| *p == "**") {
        if parts[i + 1..parts.len() - 1].iter().any(|p| *p == "**") {
            bail!("artifact pattern {raw:?}: use `**` once");
        }
        if parts[i + 1..parts.len() - 1].iter().any(|p| p.contains(['*', '?'])) {
            bail!("artifact pattern {raw:?}: `**` must be followed by the file pattern only");
        }
        return Ok(Pattern::Recursive {
            dir: parts[..i].join("\\"),
            glob: parts[parts.len() - 1].to_string(),
        });
    }

    let last = parts[parts.len() - 1];
    let dir = parts[..parts.len() - 1].join("\\");
    if last.contains(['*', '?']) {
        return Ok(Pattern::Shallow { dir, glob: last.to_string() });
    }
    Ok(Pattern::Exact { path: parts.join("\\") })
}

/// Reject bad patterns before the run starts, rather than after a build has
/// already been paid for.
pub fn validate(patterns: &[String]) -> Result<()> {
    for p in patterns {
        parse(p)?;
    }
    Ok(())
}

fn src_of(dir: &str) -> String {
    if dir.is_empty() {
        "C:\\workspace".to_string()
    } else {
        format!("C:\\workspace\\{dir}")
    }
}

fn dst_of(dir: &str) -> String {
    if dir.is_empty() {
        format!("%WQART%\\{DIR}\\")
    } else {
        format!("%WQART%\\{DIR}\\{dir}\\")
    }
}

/// The batch the agent runs after the command, one block per pattern.
pub fn script(patterns: &[String]) -> String {
    let mut s = String::from("@echo off\r\n");
    s.push_str("set WQ_ART_FAIL=0\r\n");
    s.push_str(&format!("if not exist %WQART%\\{DIR} mkdir %WQART%\\{DIR}\r\n"));
    for raw in patterns {
        if let Ok(p) = parse(raw) {
            s.push_str(&emit(&p, raw));
        }
    }
    s.push_str("echo winquick-artifact-status=%WQ_ART_FAIL%\r\n");
    s
}

fn emit(p: &Pattern, raw: &str) -> String {
    match p {
        Pattern::Tree { dir } => {
            let (src, dst) = (src_of(dir), dst_of(dir));
            format!(
                "if exist \"{src}\" (\r\n  \
                 xcopy \"{src}\" \"{dst}\" /E /I /Y /Q\r\n  \
                 if errorlevel 1 set WQ_ART_FAIL=1\r\n\
                 ) else (\r\n  echo winquick: no match for {raw}\r\n)\r\n"
            )
        }
        // xcopy given a wildcard source with /S walks every subdirectory and
        // keeps the hierarchy, which is precisely `**/<glob>`.
        Pattern::Recursive { dir, glob } => {
            let (src, dst) = (src_of(dir), dst_of(dir));
            format!(
                "if exist \"{src}\" (\r\n  \
                 xcopy \"{src}\\{glob}\" \"{dst}\" /S /I /Y /Q >nul 2>&1\r\n  \
                 if errorlevel 1 echo winquick: no match for {raw}\r\n\
                 ) else (\r\n  echo winquick: no match for {raw}\r\n)\r\n"
            )
        }
        Pattern::Shallow { dir, glob } => {
            let (src, dst) = (src_of(dir), dst_of(dir));
            format!(
                "if not exist \"{dst}\" mkdir \"{dst}\" 2>nul\r\n\
                 set WQ_ART_ANY=0\r\n\
                 for %%f in (\"{src}\\{glob}\") do @(\r\n  \
                 if exist \"%%~f\" (\r\n    \
                 copy /Y \"%%~f\" \"{dst}\" >nul\r\n    \
                 if errorlevel 1 set WQ_ART_FAIL=1\r\n    \
                 set WQ_ART_ANY=1\r\n  \
                 )\r\n\
                 )\r\n\
                 if \"%WQ_ART_ANY%\"==\"0\" echo winquick: no match for {raw}\r\n"
            )
        }
        Pattern::Exact { path } => {
            let parent = match path.rfind('\\') {
                Some(i) => &path[..i],
                None => "",
            };
            let dst = dst_of(parent);
            let mut out = String::new();
            if !parent.is_empty() {
                out.push_str(&format!("if not exist \"{dst}\" mkdir \"{dst}\"\r\n"));
            }
            out.push_str(&format!(
                "if exist \"C:\\workspace\\{path}\" (\r\n  \
                 xcopy \"C:\\workspace\\{path}\" \"{dst}\" /E /I /Y /Q\r\n  \
                 if errorlevel 1 set WQ_ART_FAIL=1\r\n\
                 ) else (\r\n  echo winquick: no match for {raw}\r\n)\r\n"
            ));
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Pattern {
        parse(s).unwrap()
    }

    #[test]
    fn a_whole_tree() {
        assert_eq!(p("bin/Release/**"), Pattern::Tree { dir: "bin\\Release".into() });
        assert_eq!(p("**"), Pattern::Tree { dir: String::new() });
    }

    /// One star is one level, two stars recurse — the same rule every other
    /// glob uses. v0.2.0 treated `dir/*` as the whole tree; it does not now.
    #[test]
    fn one_star_is_one_level() {
        assert_eq!(p("publish/*"), Pattern::Shallow { dir: "publish".into(), glob: "*".into() });
        assert_eq!(p("publish/**"), Pattern::Tree { dir: "publish".into() });
    }

    #[test]
    fn a_wildcard_at_every_depth() {
        assert_eq!(
            p("**/*.dll"),
            Pattern::Recursive { dir: String::new(), glob: "*.dll".into() }
        );
        assert_eq!(
            p("bin/**/*.exe"),
            Pattern::Recursive { dir: "bin".into(), glob: "*.exe".into() }
        );
        assert_eq!(
            p("publish/**/*"),
            Pattern::Recursive { dir: "publish".into(), glob: "*".into() }
        );
    }

    #[test]
    fn a_wildcard_in_one_directory() {
        assert_eq!(p("*.log"), Pattern::Shallow { dir: String::new(), glob: "*.log".into() });
        assert_eq!(p("logs/*.txt"), Pattern::Shallow { dir: "logs".into(), glob: "*.txt".into() });
        assert_eq!(p("foo?.txt"), Pattern::Shallow { dir: String::new(), glob: "foo?.txt".into() });
    }

    #[test]
    fn a_named_file() {
        assert_eq!(p("out/report.txt"), Pattern::Exact { path: "out\\report.txt".into() });
        assert_eq!(p("report.txt"), Pattern::Exact { path: "report.txt".into() });
    }

    #[test]
    fn slashes_lean_either_way() {
        assert_eq!(p("bin/Release/**"), p("bin\\Release\\**"));
        assert_eq!(p("logs/*.txt"), p("logs\\*.txt"));
    }

    /// A pattern names something inside the workspace. Anything that could
    /// climb out is refused here, before it reaches the guest at all.
    #[test]
    fn traversal_is_refused() {
        for bad in [
            "../outside.txt",
            "bin/../../etc/passwd",
            "..",
            "bin/**/../x",
            r"..\windows\system32",
        ] {
            assert!(parse(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn absolute_paths_are_refused() {
        for bad in [r"C:\Windows\System32\*", r"D:\x.txt"] {
            assert!(parse(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn an_empty_pattern_is_refused() {
        assert!(parse("").is_err());
        assert!(parse("/").is_err());
        assert!(parse("///").is_err());
    }

    #[test]
    fn a_leading_slash_is_just_the_workspace_root() {
        assert_eq!(p("/bin/**"), Pattern::Tree { dir: "bin".into() });
    }

    #[test]
    fn validate_reports_the_first_bad_pattern() {
        let good = vec!["bin/**".to_string(), "*.log".to_string()];
        assert!(validate(&good).is_ok());
        let bad = vec!["bin/**".to_string(), "../x".to_string()];
        assert!(validate(&bad).is_err());
    }

    /// The generated batch must never contain a path that leaves the artifact
    /// directory, whatever it was handed.
    #[test]
    fn the_generated_script_stays_inside_the_artifact_directory() {
        let s = script(&[
            "bin/Release/**".to_string(),
            "**/*.dll".to_string(),
            "logs/*.txt".to_string(),
            "out/report.txt".to_string(),
        ]);
        assert!(!s.contains(".."), "script contains a traversal: {s}");
        for line in s.lines().filter(|l| l.contains("%WQART%")) {
            assert!(
                line.contains(&format!("%WQART%\\{DIR}")),
                "a destination escaped the artifact directory: {line}"
            );
        }
    }

    #[test]
    fn every_pattern_shape_produces_a_guarded_block() {
        for pat in ["bin/**", "**/*.dll", "logs/*.txt", "out/report.txt"] {
            let s = script(&[pat.to_string()]);
            assert!(s.contains("no match for"), "{pat} has no not-found branch");
        }
    }
}
