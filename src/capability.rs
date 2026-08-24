//! Capability volumes: extra tooling attached to the guest as its own disk.
//!
//! PowerShell is the first one. It is 271 MiB of files, and baking that into the
//! base image would grow a 763 MiB runtime by more than a third for something not
//! every run needs. Instead it lives in its own FAT32 image which is attached
//! only when it exists, cloned per run like everything else.
//!
//! This also sidesteps a practical problem: `ntfscp` cannot create directories,
//! and PowerShell ships 41 of them, so writing it into the NTFS system volume
//! from macOS is not straightforward. A FAT32 image we build ourselves is.
//!
//! The volume must be attached **writable**. Windows writes when it mounts a
//! volume; a read-only NVMe makes those writes fail with `aio failed: Operation
//! not permitted` and no volume appears at all. See docs/research.md.

use anyhow::{bail, Context, Result};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use fscommon::{BufStream, StreamSlice};
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use crate::paths;

const SECTOR: u64 = 512;
const PART_START_LBA: u64 = 2048;

/// Where the PowerShell capability volume lives, if it has been built.
pub fn pwsh_image() -> Result<PathBuf> {
    Ok(paths::root()?
        .join("images")
        .join(paths::IMAGE_NAME)
        .join("pwsh.img"))
}

/// Build a FAT32 capability image containing `src_dir` at `dest_name`.
pub fn build(image: &Path, src_dir: &Path, dest_name: &str) -> Result<u64> {
    let content = dir_size(src_dir)?;
    // FAT32 needs headroom for its tables and per-file cluster slack; 25% over
    // the payload has been comfortable, with a 64 MiB floor for the format.
    let size = (((content * 5) / 4) + 64 * 1024 * 1024).next_multiple_of(SECTOR);

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
        copy_tree(src_dir, &dest)?;
    }
    fs.unmount()?;
    buf.flush()?;
    Ok(size)
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
        if e.metadata()?.is_dir() {
            let sub = dst.create_dir(&name)?;
            copy_tree(&path, &sub)?;
        } else {
            let mut f = dst.create_file(&name)?;
            f.truncate()?;
            let data = std::fs::read(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            f.write_all(&data)
                .with_context(|| format!("writing {name} into the capability volume"))?;
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

/// The official Microsoft PowerShell 7 ARM64 portable ZIP.
pub const PWSH_URL: &str =
    "https://github.com/PowerShell/PowerShell/releases/download/v7.6.5/PowerShell-7.6.5-win-arm64.zip";
pub const PWSH_SHA256: &str =
    "20514a755d16428dc4355c85e0883c859531e71cc3e122670aa1fccdbf96ba7e";
pub const PWSH_VERSION: &str = "7.6.5";

/// Fetch (or reuse) the PowerShell ZIP, verify it, unpack it, and build the volume.
pub fn install_powershell(zip: Option<PathBuf>, verbose: bool) -> Result<()> {
    let cache = paths::cache()?;
    std::fs::create_dir_all(&cache)?;
    let archive = match zip {
        Some(p) => p,
        None => {
            let p = cache.join(format!("PowerShell-{PWSH_VERSION}-win-arm64.zip"));
            if !p.exists() {
                println!("Downloading PowerShell {PWSH_VERSION} for Windows ARM64 from Microsoft...");
                let st = std::process::Command::new("/usr/bin/curl")
                    .args(["-sSL", "-o"])
                    .arg(&p)
                    .arg(PWSH_URL)
                    .status()
                    .context("running curl")?;
                if !st.success() {
                    bail!("download failed");
                }
            }
            p
        }
    };

    let got = sha256_file(&archive)?;
    if got != PWSH_SHA256 {
        bail!(
            "checksum mismatch for {}\n  expected {PWSH_SHA256}\n  got      {got}",
            archive.display()
        );
    }
    if verbose {
        eprintln!("winquick: sha256 verified against Microsoft's published digest");
    }

    let work = paths::root()?.join("work").join("pwsh");
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
    if !work.join("pwsh.exe").exists() {
        bail!("{} does not look like a PowerShell ZIP", archive.display());
    }

    let image = pwsh_image()?;
    std::fs::create_dir_all(image.parent().unwrap())?;
    println!("Building the PowerShell volume...");
    let size = build(&image, &work, "pwsh")?;
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "PowerShell {PWSH_VERSION} ready ({:.0} MiB volume)",
        size as f64 / (1024.0 * 1024.0)
    );
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
