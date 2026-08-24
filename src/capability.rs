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
    pub url: &'static str,
    pub sha256: &'static str,
    /// Directory the payload lands in on the volume; the agent probes for this.
    pub dest: &'static str,
    /// A file that must exist after unpacking, as a sanity check.
    pub sentinel: &'static str,
    pub description: &'static str,
}

pub const SPECS: &[Spec] = &[
    Spec {
        name: "powershell",
        version: "7.6.5",
        url: "https://github.com/PowerShell/PowerShell/releases/download/v7.6.5/PowerShell-7.6.5-win-arm64.zip",
        sha256: "20514a755d16428dc4355c85e0883c859531e71cc3e122670aa1fccdbf96ba7e",
        dest: "pwsh",
        sentinel: "pwsh.exe",
        description: "PowerShell 7 (pwsh)",
    },
    Spec {
        name: "dotnet-runtime",
        version: "10.0.5",
        url: "https://builds.dotnet.microsoft.com/dotnet/Runtime/10.0.5/dotnet-runtime-10.0.5-win-arm64.zip",
        sha256: "0368339d9ebd5e6d0a05e196fbe4c6d886e433373d772d41d9536cffe3e6e5f1",
        dest: "dotnet",
        sentinel: "dotnet.exe",
        description: ".NET 10 runtime (framework-dependent apps)",
    },
    Spec {
        name: "dotnet-sdk",
        version: "10.0.201",
        url: "https://builds.dotnet.microsoft.com/dotnet/Sdk/10.0.201/dotnet-sdk-10.0.201-win-arm64.zip",
        sha256: "4fde214de7b4f52ab0d10d02ec99ff7c8a0d6682ad8d9f0e67c5725e0624bfcf",
        dest: "dotnet",
        sentinel: "dotnet.exe",
        description: ".NET 10 SDK (dotnet build / test)",
    },
];

pub fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

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
            let file = sp.url.rsplit('/').next().unwrap();
            let p = cache.join(file);
            if !p.exists() {
                println!("Downloading {} {} from Microsoft...", sp.description, sp.version);
                let st = std::process::Command::new("/usr/bin/curl")
                    .args(["-sSL", "-o"])
                    .arg(&p)
                    .arg(sp.url)
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

    if !sp.sha256.is_empty() {
        let got = sha256_file(&archive)?;
        if got != sp.sha256 {
            bail!(
                "checksum mismatch for {}\n  expected {}\n  got      {got}",
                archive.display(),
                sp.sha256
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
    let st = std::process::Command::new("/usr/bin/unzip")
        .args(["-q", "-o"])
        .arg(&archive)
        .arg("-d")
        .arg(&work)
        .status()
        .context("running unzip")?;
    if !st.success() {
        bail!("could not unpack {}", archive.display());
    }
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
    let img = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(image)
        .with_context(|| format!("creating {}", image.display()))?;
    img.set_len(size)?;
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
        let dest = root.create_dir(dest_name)?;
        if src_dir.exists() {
            copy_tree(src_dir, &dest)?;
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
    let out = std::process::Command::new("dotnet")
        .arg("restore")
        .arg(project)
        .args(["-r", rid])
        .arg("--packages")
        .arg(&cache)
        .args(["-v", "q", "--nologo"])
        .output()
        .context("running `dotnet restore` on the host — is the .NET SDK installed?")?;
    if !out.status.success() {
        bail!(
            "restoring packages on this Mac failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let after = count_packages(&cache);
    let image = nuget_image()?;
    // Rebuilding the volume changes its identity, which invalidates the prepared
    // guest. Skip it entirely when nothing was actually added.
    if after == before && image.exists() {
        use std::os::unix::fs::MetadataExt;
        return Ok(SyncResult {
            packages: after,
            added: 0,
            bytes: std::fs::metadata(&image)?.blocks() * 512,
            rebuilt: false,
        });
    }
    let (bytes, packages) = rebuild_nuget_image(verbose)?;
    Ok(SyncResult { packages, added: after.saturating_sub(before), bytes, rebuilt: true })
}

fn count_packages(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}

/// Rebuild the package-cache volume from the canonical directory.
pub fn rebuild_nuget_image(verbose: bool) -> Result<(u64, usize)> {
    let cache = nuget_dir()?;
    let image = nuget_image()?;
    std::fs::create_dir_all(image.parent().unwrap())?;
    let packages = std::fs::read_dir(&cache)
        .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count())
        .unwrap_or(0);
    if verbose {
        eprintln!("winquick: building the package-cache volume ({packages} packages)");
    }
    build_sized(&image, &cache, "packages", NUGET_BYTES)?;
    mark(&image, "WQNUGET.TXT", "packages")?;
    use std::os::unix::fs::MetadataExt;
    let allocated = std::fs::metadata(&image)?.blocks() * 512;
    Ok((allocated, packages))
}
