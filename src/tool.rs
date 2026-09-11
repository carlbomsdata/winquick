//! Persistent, bring-your-own toolchains for the guest.
//!
//! WinQuick's guest is minimal and offline, so it ships no compilers beyond the
//! .NET capability. Anything else — a C toolchain, Go, Node, Python, a portable
//! CLI — a user brings in. Putting it in the workspace works but copies it into
//! a fresh image on every run: a 300 MiB toolchain turned a one-second build
//! into two minutes.
//!
//! A tool volume fixes that. `winquick tool add <name> --from <dir>` packs the
//! directory into a volume once; every later `run` and `build` attaches it — as
//! a capability volume, so the run path clones it (an APFS clone, effectively
//! free) and freezes it exactly like the .NET one, no re-copy — and the guest
//! agent puts its directories on `PATH`.
//!
//! It is deliberately generic: WinQuick does not care whether the volume holds a
//! compiler or a calculator. The manifest it writes, `WQPATH.TXT`, lists the
//! directories to prepend to `PATH`, one per line (`.` meaning the volume root).

use crate::capability;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// The PATH manifest the guest agent reads from each tool volume.
pub const MANIFEST: &str = "WQPATH.TXT";

pub struct Tool {
    pub name: String,
    pub image: PathBuf,
}

/// Tool volumes live among the capability volumes, so the run path clones,
/// attaches and fingerprints them with no changes of its own — they are just
/// another attached disk. The `tool-` prefix keeps them apart from the built-in
/// capabilities in listings.
const PREFIX: &str = "tool-";

fn image_path(name: &str) -> Result<PathBuf> {
    Ok(capability::dir()?.join(format!("{PREFIX}{name}.img")))
}

/// The tool name behind a capability image path, if it is a tool volume.
pub fn name_of(image: &Path) -> Option<String> {
    image
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix(PREFIX))
        .map(str::to_string)
}

/// Every registered tool, in a stable order so the device topology — and the
/// prepared-guest fingerprint — is deterministic.
pub fn installed() -> Result<Vec<Tool>> {
    let mut out: Vec<Tool> = capability::installed()?
        .into_iter()
        .filter_map(|c| name_of(&c.image).map(|name| Tool { name, image: c.image }))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A tool name has to be a clean single path segment: it becomes a filename, and
/// it is what the user types to remove it.
fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && name != "."
        && name != "..";
    if !ok {
        bail!("a tool name must be letters, digits, '-', '_' or '.' (got {name:?})");
    }
    Ok(())
}

/// Pack a host directory into a tool volume and record its PATH directories.
///
/// `paths` are directories inside the volume to put on `PATH`; empty means
/// auto-detect — a `bin` subdirectory if the toolchain has one, otherwise the
/// root, which is where a single portable exe usually sits.
pub fn add(name: &str, from: &Path, paths_in: &[String], verbose: bool) -> Result<()> {
    validate_name(name)?;
    if !from.is_dir() {
        bail!("--from must be a directory (got {})", from.display());
    }
    capability::reject_unsupported_names(from, "the tool directory")?;

    let entries: Vec<String> = if !paths_in.is_empty() {
        paths_in.iter().map(|p| p.replace('\\', "/").trim_matches('/').to_string()).collect()
    } else if from.join("bin").is_dir() {
        vec!["bin".to_string()]
    } else {
        vec![".".to_string()]
    };

    if capability::spec(name).is_some() {
        bail!("'{name}' is the name of a built-in capability; choose another tool name");
    }
    std::fs::create_dir_all(capability::dir()?)?;
    let image = image_path(name)?;
    if verbose {
        eprintln!("winquick: packing {} into a tool volume", from.display());
    }
    let bytes = capability::build_flat(&image, from)?;
    // The manifest the agent reads: one PATH directory per line.
    let manifest = entries.join("\n") + "\n";
    capability::write_root_file(&image, MANIFEST, manifest.as_bytes())?;

    // A changed tool set changes the device topology, so the prepared guest must
    // be rebuilt to match.
    crate::state::discard()?;

    println!(
        "Tool '{name}' ready ({}). It is on PATH inside `winquick run` and `winquick build`.",
        crate::helpers::human(bytes)
    );
    if entries == ["."] {
        println!("  PATH: the volume root");
    } else {
        println!("  PATH: {}", entries.join(", "));
    }
    Ok(())
}

pub fn remove(name: &str) -> Result<()> {
    validate_name(name)?;
    let p = image_path(name)?;
    if p.exists() {
        std::fs::remove_file(&p)?;
        crate::state::discard()?;
        println!("Removed tool '{name}'.");
    } else {
        println!("No tool named '{name}'.");
    }
    Ok(())
}

pub fn list() -> Result<()> {
    let tools = installed()?;
    if tools.is_empty() {
        println!("No tools registered.");
        println!("Add one with:  winquick tool add <name> --from <dir>");
        return Ok(());
    }
    println!("{:<20} {:>10}", "NAME", "SIZE");
    for t in &tools {
        let sz = crate::helpers::allocated(&t.image);
        println!("{:<20} {:>10}", t.name, crate::helpers::human(sz));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn names_are_validated() {
        for ok in ["zig", "go", "node-20", "python3.12", "my_tool"] {
            assert!(validate_name(ok).is_ok(), "{ok} should be valid");
        }
        for bad in ["", "..", ".", "a/b", "a b", "a;b", "a\\b"] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn only_tool_prefixed_images_are_tools() {
        assert_eq!(name_of(Path::new("/x/capabilities/tool-zig.img")).as_deref(), Some("zig"));
        assert_eq!(
            name_of(Path::new("/x/capabilities/tool-node-20.img")).as_deref(),
            Some("node-20")
        );
        assert_eq!(name_of(Path::new("/x/capabilities/dotnet-sdk.img")), None);
        assert_eq!(name_of(Path::new("/x/capabilities/nuget-cache.img")), None);
    }
}
