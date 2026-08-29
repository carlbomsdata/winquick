//! Capability volumes: optional tooling attached to the guest as extra disks.
//!
//! The base runtime stays small and everything else is opt-in. Each capability
//! is one FAT32 image under `~/.winquick/capabilities/`, attached as its own NVMe
//! device and discovered by the guest agent, which puts it on `PATH`.
//!
//! Building an image instead of writing into the guest's NTFS system volume is
//! not just a size decision: `ntfscp` cannot create directories, and these
//! packages ship hundreds of them.
//!
//! Two rules learned the hard way, both in docs/research.md:
//!
//! * Volumes must be attached **writable**. Windows writes when it mounts a
//!   volume; a read-only NVMe makes those writes fail with `aio failed:
//!   Operation not permitted` and no volume appears at all.
//! * Volumes are cloned per run, never reformatted between runs, so the FAT
//!   volume identity the guest remembers stays valid.

use anyhow::{bail, Context, Result};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use fscommon::{BufStream, StreamSlice};
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use crate::helpers::which;
use crate::paths;

const SECTOR: u64 = 512;
const PART_START_LBA: u64 = 2048;
/// Package cache volume size. Sparse, so an empty one costs nothing, but large
/// enough that a realistic dependency set fits without resizing (which would
/// change the volume identity the guest remembers).
pub const NUGET_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// A capability that can be installed.
pub struct Spec {
    /// Name used on the command line and for the image filename.
    pub name: &'static str,
    pub version: &'static str,
    /// Where to get this capability for each guest architecture.
    pub arm64: Payload,
    pub x64: Payload,
    /// Directory the payload lands in on the volume; the agent probes for this.
    pub dest: &'static str,
    /// A file that must exist after unpacking, as a sanity check.
    pub sentinel: &'static str,
    pub description: &'static str,
}

/// One architecture's download.
///
/// A capability is not architecture-neutral: an ARM64 `pwsh.exe` will not run
/// in an x64 guest, and the guest architecture follows the host. Keeping both
/// here means the catalogue states the fact rather than the build hiding it.
pub struct Payload {
    pub url: &'static str,
    pub sha256: &'static str,
}

impl Spec {
    /// The download for the guest this host runs.
    pub fn payload(&self) -> &Payload {
        if crate::platform::GUEST_ARCH == "arm64" {
            &self.arm64
        } else {
            &self.x64
        }
    }
}

pub const SPECS: &[Spec] = &[
    Spec {
        name: "powershell",
        version: "7.6.5",
        arm64: Payload {
            url: "https://github.com/PowerShell/PowerShell/releases/download/v7.6.5/PowerShell-7.6.5-win-arm64.zip",
            sha256: "20514a755d16428dc4355c85e0883c859531e71cc3e122670aa1fccdbf96ba7e",
        },
        x64: Payload {
            url: "https://github.com/PowerShell/PowerShell/releases/download/v7.6.5/PowerShell-7.6.5-win-x64.zip",
            sha256: "32eb8f6cdce08f86e987d625a2733e54ac3e289ae7e1621b14c0b5bcec2434ea",
        },
        dest: "pwsh",
        sentinel: "pwsh.exe",
        description: "PowerShell 7 (pwsh)",
    },
    Spec {
        name: "dotnet-runtime",
        version: "10.0.5",
        arm64: Payload {
            url: "https://builds.dotnet.microsoft.com/dotnet/Runtime/10.0.5/dotnet-runtime-10.0.5-win-arm64.zip",
            sha256: "0368339d9ebd5e6d0a05e196fbe4c6d886e433373d772d41d9536cffe3e6e5f1",
        },
        x64: Payload {
            url: "https://builds.dotnet.microsoft.com/dotnet/Runtime/10.0.5/dotnet-runtime-10.0.5-win-x64.zip",
            sha256: "ba5d7ca9a366fe7955e25b3da92b3f95a67837514c4f76aad719df73a5fb18ed",
        },
        dest: "dotnet",
        sentinel: "dotnet.exe",
        description: ".NET 10 runtime (framework-dependent apps)",
    },
    Spec {
        name: "dotnet-sdk",
        version: "10.0.201",
        arm64: Payload {
            url: "https://builds.dotnet.microsoft.com/dotnet/Sdk/10.0.201/dotnet-sdk-10.0.201-win-arm64.zip",
            sha256: "4fde214de7b4f52ab0d10d02ec99ff7c8a0d6682ad8d9f0e67c5725e0624bfcf",
        },
        x64: Payload {
            url: "https://builds.dotnet.microsoft.com/dotnet/Sdk/10.0.201/dotnet-sdk-10.0.201-win-x64.zip",
            sha256: "56c346275e765767f335ce3df4468e5d471836e967a6cca0234ddf60ad9a6c80",
        },
        dest: "dotnet",
        sentinel: "dotnet.exe",
        description: ".NET 10 SDK (dotnet build / test)",
    },
];

/// Unpack a capability archive.
///
/// macOS ships `unzip`. Windows does not, but it has shipped bsdtar as
/// `tar.exe` since Windows 10 1803, and bsdtar reads zip archives perfectly
/// well -- so neither host needs anything installed. `-o` on macOS overwrites
/// without prompting, which matters because a prompt would hang a
/// non-interactive install.
fn unzip(archive: &Path, into: &Path) -> Result<()> {
    let mut c = if cfg!(windows) {
        let mut c = std::process::Command::new("tar");
        c.arg("-xf").arg(archive).arg("-C").arg(into);
        c
    } else {
        let mut c = std::process::Command::new("/usr/bin/unzip");
        c.args(["-q", "-o"]).arg(archive).arg("-d").arg(into);
        c
    };
    let st = c.status().context("unpacking the capability archive")?;
    if !st.success() {
        bail!("could not unpack {}", archive.display());
    }
    Ok(())
}

pub fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

#[derive(Clone)]
pub struct Installed {
    pub name: String,
    pub image: PathBuf,
}

pub fn dir() -> Result<PathBuf> {
    Ok(paths::root()?.join("capabilities"))
}

pub fn image_path(name: &str) -> Result<PathBuf> {
    Ok(dir()?.join(format!("{name}.img")))
}

/// Every installed capability, in a stable order so the device topology — and
/// therefore the prepared-guest fingerprint — is deterministic.
pub fn installed() -> Result<Vec<Installed>> {
    let d = match dir() {
        Ok(d) if d.exists() => d,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for e in std::fs::read_dir(d)? {
        let p = e?.path();
        if p.extension().map(|x| x == "img").unwrap_or(false) {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                out.push(Installed { name: stem.to_string(), image: p.clone() });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn remove(name: &str) -> Result<bool> {
    let p = image_path(name)?;
    if p.exists() {
        std::fs::remove_file(&p)?;
        return Ok(true);
    }
    Ok(false)
}

/// Download (or reuse), verify, unpack and build a capability volume.
pub fn install(name: &str, zip: Option<PathBuf>, verbose: bool) -> Result<u64> {
    let sp = spec(name)
        .ok_or_else(|| anyhow::anyhow!("unknown capability `{name}`"))?;
    let cache = paths::cache()?;
    std::fs::create_dir_all(&cache)?;

    let archive = match zip {
        Some(p) => p,
        None => {
            let file = sp.payload().url.rsplit('/').next().unwrap();
            let p = cache.join(file);
            if !p.exists() {
                println!("Downloading {} {} from Microsoft...", sp.description, sp.version);
                let st = std::process::Command::new(
                    crate::helpers::which("curl").unwrap_or_else(|| PathBuf::from("curl")),
                )
                    .args(["-sSL", "-o"])
                    .arg(&p)
                    .arg(sp.payload().url)
                    .status()
                    .context("running curl")?;
                if !st.success() {
                    let _ = std::fs::remove_file(&p);
                    bail!("download failed");
                }
            }
            p
        }
    };

    if !sp.payload().sha256.is_empty() {
        let got = sha256_file(&archive)?;
        if got != sp.payload().sha256 {
            bail!(
                "checksum mismatch for {}\n  expected {}\n  got      {got}",
                archive.display(),
                sp.payload().sha256
            );
        }
        if verbose {
            eprintln!("winquick: sha256 verified against the publisher's digest");
        }
    } else if verbose {
        eprintln!("winquick: no pinned checksum for {name}; trusting HTTPS from Microsoft");
    }

    let work = paths::root()?.join("work").join(name);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    unzip(&archive, &work)?;
    if !work.join(sp.sentinel).exists() {
        bail!(
            "{} does not look like {}: no {} inside",
            archive.display(),
            sp.description,
            sp.sentinel
        );
    }

    let image = image_path(name)?;
    std::fs::create_dir_all(image.parent().unwrap())?;
    println!("Building the {name} volume...");
    let size = build(&image, &work, sp.dest)?;
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "{} {} ready ({:.0} MiB volume)",
        sp.description,
        sp.version,
        size as f64 / (1024.0 * 1024.0)
    );
    Ok(size)
}

/// Build a FAT32 capability image containing `src_dir` at `dest_name`.
pub fn build(image: &Path, src_dir: &Path, dest_name: &str) -> Result<u64> {
    let content = dir_size(src_dir)?;
    // FAT32 needs headroom for its tables and per-file cluster slack; 25% over
    // the payload has been comfortable, with a 64 MiB floor for the format.
    let size = (((content * 5) / 4) + 64 * 1024 * 1024).next_multiple_of(SECTOR);
    build_sized(image, src_dir, dest_name, size)
}

/// Build a capability image of an explicit size. Used for the workspace volume,
/// where the size has to stay constant across runs so the FAT volume identity
/// the guest remembers keeps resolving.
pub fn build_sized(image: &Path, src_dir: &Path, dest_name: &str, size: u64) -> Result<u64> {
    build_inner(image, src_dir, Some(dest_name), size)
}

/// Build an image whose root *is* `src_dir`, with no wrapping directory.
///
/// The desktop session volume carries several top-level directories plus a
/// marker file, so there is nothing sensible to nest it under.
pub fn build_flat(image: &Path, src_dir: &Path) -> Result<u64> {
    let content = dir_size(src_dir)?;
    let size = (((content * 5) / 4) + 64 * 1024 * 1024).next_multiple_of(SECTOR);
    build_inner(image, src_dir, None, size)
}

fn build_inner(image: &Path, src_dir: &Path, dest_name: Option<&str>, size: u64) -> Result<u64> {
    let img = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(image)
        .with_context(|| format!("creating {}", image.display()))?;
    crate::hostfs::set_sparse_len(&img, size)?;
    write_mbr(&img, size)?;

    let slice = StreamSlice::new(img, PART_START_LBA * SECTOR, size)?;
    let mut buf = BufStream::new(slice);
    fatfs::format_volume(
        &mut buf,
        FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .volume_label(*b"WQCAPS     "),
    )
    .context("formatting capability volume")?;
    let fs = FileSystem::new(&mut buf, FsOptions::new())?;
    {
        let root = fs.root_dir();
        if src_dir.exists() {
            match dest_name {
                Some(name) => copy_tree(src_dir, &root.create_dir(name)?)?,
                None => copy_tree(src_dir, &root)?,
            }
        }
    }
    fs.unmount()?;
    buf.flush()?;
    Ok(size)
}

/// Where the canonical, host-managed NuGet package cache lives, and the volume
/// built from it. Only host-side tooling ever writes the canonical copy; the
/// guest gets a throwaway clone.
pub fn nuget_dir() -> Result<PathBuf> {
    Ok(paths::root()?.join("caches").join("nuget"))
}
pub fn nuget_image() -> Result<PathBuf> {
    // Lives with the other capability volumes so it is attached and fingerprinted
    // like them. That matters: the guest never re-reads a volume after the frozen
    // image was captured, so a changed cache has to invalidate the frozen guest.
    Ok(dir()?.join("nuget-cache.img"))
}

/// Write the marker the guest agent looks for, plus an empty payload directory.
pub fn mark(image: &Path, marker: &str, payload_dir: &str) -> Result<()> {
    let img = OpenOptions::new().read(true).write(true).open(image)?;
    let len = img.metadata()?.len();
    let slice = StreamSlice::new(img, PART_START_LBA * SECTOR, len)?;
    let mut buf = BufStream::new(slice);
    let fs = FileSystem::new(&mut buf, FsOptions::new())?;
    {
        let root = fs.root_dir();
        let mut f = root.create_file(marker)?;
        f.truncate()?;
        f.write_all(b"winquick\r\n")?;
        if root.open_dir(payload_dir).is_err() {
            root.create_dir(payload_dir)?;
        }
    }
    fs.unmount()?;
    buf.flush()?;
    Ok(())
}

/// Replace the contents of `dest_name` inside an existing image, without
/// reformatting — the volume identity has to survive.
pub fn refill(image: &Path, src_dir: &Path, dest_name: &str) -> Result<()> {
    let img = OpenOptions::new().read(true).write(true).open(image)?;
    let len = img.metadata()?.len();
    let slice = StreamSlice::new(img, PART_START_LBA * SECTOR, len)?;
    let mut buf = BufStream::new(slice);
    let fs = FileSystem::new(&mut buf, FsOptions::new())?;
    {
        let root = fs.root_dir();
        // fatfs has no recursive delete; clear what is there, then refill.
        let dest = match root.open_dir(dest_name) {
            Ok(d) => {
                purge(&d)?;
                d
            }
            Err(_) => root.create_dir(dest_name)?,
        };
        copy_tree(src_dir, &dest)?;
    }
    fs.unmount()?;
    buf.flush()?;
    Ok(())
}

fn purge<T: fatfs::ReadWriteSeek>(d: &fatfs::Dir<T>) -> Result<()> {
    let names: Vec<(String, bool)> = d
        .iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            n != "." && n != ".."
        })
        .map(|e| (e.file_name(), e.is_dir()))
        .collect();
    for (n, is_dir) in names {
        if is_dir {
            let sub = d.open_dir(&n)?;
            purge(&sub)?;
        }
        d.remove(&n)?;
    }
    Ok(())
}

fn dir_size(p: &Path) -> Result<u64> {
    let mut total = 0;
    for e in std::fs::read_dir(p)? {
        let e = e?;
        let m = e.metadata()?;
        total += if m.is_dir() { dir_size(&e.path())? } else { m.len() };
    }
    Ok(total)
}

/// Names the FAT volume cannot hold, found before anything is copied.
///
/// The filesystem crate WinQuick builds these volumes with accepts characters
/// up to `U+FFFF` only, so a name containing an emoji — or anything else
/// outside the basic multilingual plane, which needs a surrogate pair — is
/// rejected. Everyday non-ASCII is fine: accents, CJK, Cyrillic and Greek all
/// work.
///
/// The point of checking first is the error message. Failing part-way through
/// copying reported only "File name contains unsupported characters", leaving
/// the user to find the offending file in a tree of thousands.
pub fn unsupported_names(root: &Path) -> Vec<PathBuf> {
    let mut bad = Vec::new();
    collect_unsupported(root, &mut bad);
    bad
}

fn collect_unsupported(dir: &Path, bad: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if !name_fits_fat(&e.file_name().to_string_lossy()) {
            bad.push(path.clone());
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_unsupported(&path, bad);
        }
    }
}

/// Mirrors what the filesystem layer will accept, so the check and the failure
/// agree.
fn name_fits_fat(name: &str) -> bool {
    name.chars().all(|c| match c {
        'a'..='z' | 'A'..='Z' | '0'..='9' => true,
        '\u{80}'..='\u{FFFF}' => true,
        '$' | '%' | '\'' | '-' | '_' | '@' | '~' | '`' | '!' | '(' | ')' | '{' | '}' | '.'
        | ' ' | '+' | ',' | ';' | '=' | '[' | ']' | '^' | '#' | '&' => true,
        _ => false,
    })
}

/// Refuse a tree the volume cannot represent, saying exactly which files.
pub fn reject_unsupported_names(root: &Path, what: &str) -> Result<()> {
    let bad = unsupported_names(root);
    if bad.is_empty() {
        return Ok(());
    }
    let listed: Vec<String> = bad
        .iter()
        .take(10)
        .map(|p| format!("  {}", p.strip_prefix(root).unwrap_or(p).display()))
        .collect();
    let more = if bad.len() > 10 {
        format!("\n  ...and {} more", bad.len() - 10)
    } else {
        String::new()
    };
    bail!(
        "{what} contains {} file name(s) the Windows volume cannot hold:\n{}{more}\n\n\
         Names may use accents, CJK, Cyrillic and Greek, but not characters outside the\n\
         basic multilingual plane — emoji are the usual cause. Rename or exclude them.",
        bad.len(),
        listed.join("\n")
    )
}

fn copy_tree<T: fatfs::ReadWriteSeek>(src: &Path, dst: &fatfs::Dir<T>) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let path = e.path();
        let md = e.metadata()?;
        if md.is_dir() {
            let sub = dst.create_dir(&name)?;
            copy_tree(&path, &sub)?;
        } else if md.is_file() {
            let mut f = dst.create_file(&name)?;
            f.truncate()?;
            let data = std::fs::read(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            f.write_all(&data)
                .with_context(|| format!("writing {name} into the volume"))?;
        }
    }
    Ok(())
}

/// Windows will not mount a partitionless volume on a fixed disk.
fn write_mbr(mut img: &File, size: u64) -> Result<()> {
    let total_sectors = size / SECTOR;
    let part_sectors = (total_sectors - PART_START_LBA) as u32;
    let mut mbr = [0u8; 512];
    let e = 446;
    mbr[e] = 0x00;
    mbr[e + 1..e + 4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    mbr[e + 4] = 0x0C;
    mbr[e + 5..e + 8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    mbr[e + 8..e + 12].copy_from_slice(&(PART_START_LBA as u32).to_le_bytes());
    mbr[e + 12..e + 16].copy_from_slice(&part_sectors.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    img.seek(std::io::SeekFrom::Start(0))?;
    img.write_all(&mbr)?;
    img.flush()?;
    Ok(())
}

fn sha256_file(p: &Path) -> Result<String> {
    let out = std::process::Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(p)
        .output()
        .context("running shasum")?;
    if !out.status.success() {
        bail!("could not checksum {}", p.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string())
}

/// Populate the canonical NuGet cache from macOS, then rebuild the volume the
/// guest sees.
///
/// Only host-side tooling ever writes the canonical cache. The guest gets a
/// throwaway clone of the resulting image, so a build script cannot leave
/// anything behind that a later run would pick up.
pub struct SyncResult {
    pub packages: usize,
    pub added: usize,
    pub bytes: u64,
    /// Whether the volume the guest sees had to be rebuilt.
    pub rebuilt: bool,
}

/// A throwaway copy of a project tree, used so that host-side tooling never
/// writes into the user's source.
///
/// `dotnet restore` is not read-only: it drops `obj/project.assets.json` and
/// two generated MSBuild files beside every project file it touches. WinQuick
/// tells users their source is never written to, and that has to hold on the
/// Mac as well as inside the guest.
///
/// The copy skips `.git`, `bin` and `obj`, which restore never reads. What is
/// left is what restore needs: solution and project files, `Directory.Build.*`,
/// `NuGet.config`, `global.json`, `packages.config` and any imported props.
struct ProjectCopy {
    /// Root of the copy; deleted on drop.
    root: PathBuf,
    /// The path to hand to `dotnet restore` — the copy of what the user named.
    target: PathBuf,
    /// Absolute forms of the copy root, longest first, and the user's original
    /// directory, so error text can be put back into their terms.
    from: Vec<String>,
    to: String,
}

impl ProjectCopy {
    fn new(project: &Path) -> Result<Self> {
        if !project.exists() {
            bail!("no such project or directory: {}", project.display());
        }
        // The user may name a directory, a `.sln` or a `.csproj`. Whatever they
        // named, the directory holding it is the unit that gets copied: sibling
        // projects reached by `ProjectReference` have to come along.
        let (src_dir, leaf) = if project.is_dir() {
            (project.to_path_buf(), None)
        } else {
            let parent = project.parent().filter(|p| !p.as_os_str().is_empty());
            (
                parent.unwrap_or(Path::new(".")).to_path_buf(),
                project.file_name().map(|n| n.to_os_string()),
            )
        };
        let work = paths::root()?.join("work");
        std::fs::create_dir_all(&work)?;
        // Unique per call, not just per process: nothing stops two syncs, or two
        // tests, from staging at once, and one deleting the other's copy would
        // be a maddening way to fail.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = work.join(format!("restore-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        copy_for_restore(&src_dir, &root)
            .with_context(|| format!("staging {} for restore", src_dir.display()))?;
        let target = match &leaf {
            Some(name) => root.join(name),
            None => root.clone(),
        };
        let mut from = vec![root.to_string_lossy().into_owned()];
        if let Ok(c) = root.canonicalize() {
            let c = c.to_string_lossy().into_owned();
            if !from.contains(&c) {
                from.push(c);
            }
        }
        // Longest first, so `/private/tmp/...` is not half-replaced by `/tmp/...`.
        from.sort_by_key(|s| std::cmp::Reverse(s.len()));
        Ok(Self { root, target, from, to: src_dir.to_string_lossy().into_owned() })
    }

    fn path(&self) -> &Path {
        &self.target
    }

    /// Put the user's own paths back into a message written about the copy.
    fn unstage(&self, text: &str) -> String {
        let mut out = text.to_string();
        for f in &self.from {
            out = out.replace(f.as_str(), &self.to);
        }
        out
    }
}

impl Drop for ProjectCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Copy a project tree for a restore, skipping what restore never reads.
fn copy_for_restore(src: &Path, dst: &Path) -> Result<()> {
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let name = e.file_name();
        // `file_type` does not follow links, which is the point: a link out of
        // the tree would let restore write back into the source it is meant to
        // be shielded from.
        let md = e.file_type()?;
        if md.is_dir() {
            let n = name.to_string_lossy();
            // Build output and history are large and restore reads neither. A
            // copied `obj` would also make restore think it is already done.
            if n == ".git" || n == "bin" || n == "obj" || n == "node_modules" {
                continue;
            }
            let sub = dst.join(&name);
            std::fs::create_dir_all(&sub)?;
            copy_for_restore(&e.path(), &sub)?;
        } else if md.is_file() {
            std::fs::copy(e.path(), dst.join(&name))?;
        }
    }
    Ok(())
}

pub fn nuget_sync(project: &Path, rid: &str, verbose: bool) -> Result<SyncResult> {
    let cache = nuget_dir()?;
    std::fs::create_dir_all(&cache)?;
    if which("dotnet").is_none() {
        bail!(
            "The .NET SDK is not installed on this Mac, so packages cannot be restored here.\n\n\
             Install it from https://dotnet.microsoft.com/download, or `brew install dotnet-sdk`."
        );
    }
    let before = count_packages(&cache);
    if verbose {
        eprintln!("winquick: restoring {} into {}", project.display(), cache.display());
    }
    // The restore happens on a throwaway copy of the project, never on the
    // project itself. `dotnet restore` writes `obj/project.assets.json` and the
    // generated `.nuget.g.props`/`.targets` next to every project file, and
    // WinQuick's whole promise is that it does not touch your source. Doing it
    // in a copy also means a half-finished sync leaves nothing behind.
    let staged = ProjectCopy::new(project)?;
    let out = std::process::Command::new("dotnet")
        .arg("restore")
        .arg(staged.path())
        .args(["-r", rid])
        .arg("--packages")
        .arg(&cache)
        // The host is macOS and every project WinQuick exists to build targets
        // Windows. Without this the SDK refuses any `net*-windows` target with
        // NETSDK1100 before it resolves a single package.
        .arg("-p:EnableWindowsTargeting=true")
        .args(["-v", "q", "--nologo"])
        .output()
        .context("running `dotnet restore` on the host — is the .NET SDK installed?")?;
    if !out.status.success() {
        bail!(
            "restoring packages on this Mac failed:\n{}{}",
            staged.unstage(&String::from_utf8_lossy(&out.stdout)).trim(),
            staged.unstage(&String::from_utf8_lossy(&out.stderr)).trim()
        );
    }
    drop(staged);

    let after = count_packages(&cache);
    let image = nuget_image()?;
    // Rebuilding the volume changes its identity, which invalidates the prepared
    // guest, so skip it when the volume already holds what the cache holds.
    //
    // The question is whether the *image* is current, not whether this
    // particular restore added anything. Comparing before/after got that wrong:
    // packages can reach the cache another way — an earlier sync whose rebuild
    // failed, or a `dotnet restore --packages` run by hand — and then every
    // later sync reported "already up to date" while the guest never saw them.
    if image.exists() && image_package_count(&image) == Some(after) {
        return Ok(SyncResult {
            packages: after,
            added: 0,
            bytes: crate::hostfs::allocated(&image),
            rebuilt: false,
        });
    }
    let (bytes, packages) = rebuild_nuget_image(verbose)?;
    Ok(SyncResult { packages, added: after.saturating_sub(before), bytes, rebuilt: true })
}

/// Where the package count for a built volume is recorded.
fn image_stamp(image: &Path) -> PathBuf {
    image.with_extension("packages")
}

/// How many packages the volume was built from, if we know.
///
/// `None` for a volume built by an older WinQuick, which is treated as stale so
/// that the first sync after upgrading rebuilds it once and records the count.
fn image_package_count(image: &Path) -> Option<usize> {
    std::fs::read_to_string(image_stamp(image)).ok()?.trim().parse().ok()
}

/// How many package *versions* the cache holds.
///
/// A NuGet global-packages folder nests as `<id>/<version>/`, and counting only
/// the ids misses the case that matters here: restoring a second version of a
/// package already present — `microsoft.netcore.app.ref/8.0.25` next to
/// `9.0.14` — leaves the id count unchanged while the contents differ. Counting
/// id/version pairs is both the honest number to report and a staleness signal
/// that actually moves.
fn count_packages(dir: &Path) -> usize {
    let Ok(ids) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for id in ids.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
        match std::fs::read_dir(id.path()) {
            Ok(versions) => {
                n += versions.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count()
            }
            Err(_) => n += 1,
        }
    }
    n
}

/// Rebuild the package-cache volume from the canonical directory.
pub fn rebuild_nuget_image(verbose: bool) -> Result<(u64, usize)> {
    let cache = nuget_dir()?;
    let image = nuget_image()?;
    std::fs::create_dir_all(image.parent().unwrap())?;
    let packages = count_packages(&cache);
    if verbose {
        eprintln!("winquick: building the package-cache volume ({packages} packages)");
    }
    build_sized(&image, &cache, "packages", NUGET_BYTES)?;
    mark(&image, "WQNUGET.TXT", "packages")?;
    // What the volume was built from, so a later sync can tell whether it is
    // still current without opening it.
    let _ = std::fs::write(image_stamp(&image), packages.to_string());
    let allocated = crate::hostfs::allocated(&image);
    Ok((allocated, packages))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_dir(p: &Path) {
        std::fs::create_dir_all(p).unwrap();
    }

    /// A NuGet cache nests as `<id>/<version>/`, and the version is the unit
    /// that matters. Counting only ids missed a second version of a package
    /// already present, so `cache sync` reported "already up to date" while the
    /// guest never received the new one.
    #[test]
    fn packages_are_counted_per_version_not_per_id() {
        let root = std::env::temp_dir().join(format!("wq-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        touch_dir(&root.join("microsoft.netcore.app.ref/8.0.25"));
        assert_eq!(count_packages(&root), 1);

        // The same id, a second version: the id count does not move, but the
        // cache genuinely changed and the volume must be rebuilt.
        touch_dir(&root.join("microsoft.netcore.app.ref/9.0.14"));
        assert_eq!(count_packages(&root), 2, "a new version must change the count");

        touch_dir(&root.join("newtonsoft.json/13.0.3"));
        assert_eq!(count_packages(&root), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn counting_an_absent_cache_is_zero() {
        assert_eq!(count_packages(Path::new("/nonexistent/winquick/cache")), 0);
    }

    /// A volume built by an older WinQuick has no recorded count, and must be
    /// treated as stale so the first sync after upgrading rebuilds it once.
    #[test]
    fn a_volume_without_a_stamp_is_stale() {
        let img = std::env::temp_dir().join(format!("wq-stamp-{}.img", std::process::id()));
        let _ = std::fs::remove_file(image_stamp(&img));
        assert_eq!(image_package_count(&img), None);
        std::fs::write(image_stamp(&img), "42").unwrap();
        assert_eq!(image_package_count(&img), Some(42));
        let _ = std::fs::remove_file(image_stamp(&img));
    }

    /// A sandbox that looks like a real repository: a solution, two projects,
    /// build output that must not be carried over, and a git directory.
    fn sample_repo(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("wq-copy-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        touch_dir(&root.join("App"));
        touch_dir(&root.join("Lib"));
        touch_dir(&root.join("App/bin/Release"));
        touch_dir(&root.join("App/obj"));
        touch_dir(&root.join(".git"));
        std::fs::write(root.join("App.sln"), "sln").unwrap();
        std::fs::write(root.join("NuGet.config"), "<configuration/>").unwrap();
        std::fs::write(root.join("App/App.csproj"), "<Project/>").unwrap();
        std::fs::write(root.join("Lib/Lib.csproj"), "<Project/>").unwrap();
        std::fs::write(root.join("App/bin/Release/App.exe"), "stale").unwrap();
        std::fs::write(root.join("App/obj/project.assets.json"), "stale").unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        root
    }

    /// `dotnet restore` writes `obj/` beside every project it touches, so it is
    /// pointed at a copy. The copy has to carry the sibling projects a solution
    /// reaches through `ProjectReference`, and the imported files beside them.
    #[test]
    fn a_restore_copy_carries_the_whole_solution() {
        let root = sample_repo("whole");
        let staged = ProjectCopy::new(&root).unwrap();
        let c = staged.path().to_path_buf();
        assert!(c.join("App.sln").is_file());
        assert!(c.join("NuGet.config").is_file(), "restore reads NuGet.config");
        assert!(c.join("App/App.csproj").is_file());
        assert!(c.join("Lib/Lib.csproj").is_file(), "a sibling project must come along");
        // Build output is large, is never read by restore, and a copied `obj`
        // would make restore believe it had already run.
        assert!(!c.join("App/bin").exists(), "bin must not be copied");
        assert!(!c.join("App/obj").exists(), "obj must not be copied");
        assert!(!c.join(".git").exists(), ".git must not be copied");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The copy is deleted when the sync ends, and — the point of the whole
    /// exercise — the user's own tree is left exactly as it was.
    #[test]
    fn a_restore_copy_leaves_the_source_untouched_and_cleans_up() {
        let root = sample_repo("clean");
        let before = listing(&root);
        let copy_root = {
            let staged = ProjectCopy::new(&root).unwrap();
            // Stand in for what restore does to whatever it is pointed at.
            std::fs::create_dir_all(staged.path().join("App/obj")).unwrap();
            std::fs::write(staged.path().join("App/obj/project.assets.json"), "{}").unwrap();
            staged.root.clone()
        };
        assert!(!copy_root.exists(), "the copy is thrown away with the sync");
        assert_eq!(before, listing(&root), "the user's project must not gain a file");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Naming a project file rather than a directory still copies the whole
    /// directory, because that is where its neighbours live.
    #[test]
    fn naming_a_project_file_still_stages_its_directory() {
        let root = sample_repo("leaf");
        let staged = ProjectCopy::new(&root.join("App/App.csproj")).unwrap();
        assert!(staged.path().is_file());
        assert_eq!(staged.path().file_name().unwrap(), "App.csproj");
        assert!(
            staged.path().parent().unwrap().join("bin").exists() == false,
            "bin is skipped here too"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Restore failures are reported about the copy, which the user has never
    /// heard of. Their own path goes back into the message.
    #[test]
    fn restore_errors_name_the_users_path_not_the_copy() {
        let root = sample_repo("msg");
        let staged = ProjectCopy::new(&root).unwrap();
        let raw = format!("{}/App/App.csproj(1,1): error NU1101", staged.path().display());
        let shown = staged.unstage(&raw);
        assert!(shown.starts_with(&format!("{}/App/App.csproj", root.display())), "{shown}");
        assert!(!shown.contains("restore-"), "no trace of the copy: {shown}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn listing(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
                if p.is_dir() {
                    stack.push(p);
                }
            }
        }
        out.sort();
        out
    }
}
