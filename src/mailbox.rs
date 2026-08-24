//! The mailbox disk: how a command and its results cross the VM boundary.
//!
//! A small MBR-partitioned FAT32 image is attached to the guest as a second
//! NVMe device. The host writes the command into it before boot; the guest
//! agent writes stdout, stderr and the exit code back into it and shuts down;
//! the host reads the results once QEMU has exited.
//!
//! Doing it this way means the guest needs no third-party drivers at all — FAT
//! and NVMe are both inbox on Validation OS. The cost is that results arrive at
//! the end of the run rather than streaming; see docs/architecture.md.

use anyhow::{Context, Result};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use fscommon::{BufStream, StreamSlice};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

/// Marker the guest agent looks for when hunting for the mailbox volume.
pub const MARKER: &str = "WQMARK.TXT";
pub const CMD_FILE: &str = "WQCMD.CMD";
pub const OUT_FILE: &str = "WQOUT.TXT";
pub const ERR_FILE: &str = "WQERR.TXT";
pub const CODE_FILE: &str = "WQCODE.TXT";

const SECTOR: u64 = 512;
/// Conventional first-partition alignment.
const PART_START_LBA: u64 = 2048;
/// 64 MiB total: above the FAT32 minimum, with room for a lot of output.
const SIZE_BYTES: u64 = 64 * 1024 * 1024;

pub struct Results {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// `None` when the guest never wrote an exit code — it crashed, hung, or
    /// the agent failed before running the command.
    pub exit_code: Option<i32>,
}

/// Create the mailbox image and place the command in it.
pub fn create(path: &Path, command: &str) -> Result<()> {
    let img = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("creating mailbox at {}", path.display()))?;
    img.set_len(SIZE_BYTES)?;
    write_mbr(&img)?;

    let slice = StreamSlice::new(img, PART_START_LBA * SECTOR, SIZE_BYTES)?;
    let mut buf = BufStream::new(slice);
    fatfs::format_volume(
        &mut buf,
        FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .volume_label(*b"WQMAILBOX  "),
    )
    .context("formatting mailbox as FAT32")?;

    let fs = FileSystem::new(&mut buf, FsOptions::new()).context("opening mailbox filesystem")?;
    {
        let root = fs.root_dir();
        let mut m = root.create_file(MARKER)?;
        m.write_all(b"winquick\r\n")?;

        // `@echo off` keeps the child cmd.exe from echoing the command line into
        // captured stdout. CRLF because cmd.exe is unforgiving about bare LF.
        let mut c = root.create_file(CMD_FILE)?;
        c.write_all(b"@echo off\r\n")?;
        c.write_all(command.as_bytes())?;
        c.write_all(b"\r\n")?;
    }
    fs.unmount()?;
    buf.flush()?;
    Ok(())
}

/// Windows will not mount a partitionless ("superfloppy") volume on a fixed
/// disk — it looks for a partition table first and, finding none, exposes no
/// volume at all. This cost an afternoon; see docs/research.md.
fn write_mbr(mut img: &File) -> Result<()> {
    let total_sectors = SIZE_BYTES / SECTOR;
    let part_sectors = (total_sectors - PART_START_LBA) as u32;

    let mut mbr = [0u8; 512];
    let e = 446;
    mbr[e] = 0x00; // not bootable; nothing here is ever booted
    // CHS fields are meaningless at this size — the canonical "use LBA instead"
    // filler, which is what every modern formatter writes.
    mbr[e + 1..e + 4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    mbr[e + 4] = 0x0C; // FAT32 with LBA
    mbr[e + 5..e + 8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    mbr[e + 8..e + 12].copy_from_slice(&(PART_START_LBA as u32).to_le_bytes());
    mbr[e + 12..e + 16].copy_from_slice(&part_sectors.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    use std::io::Seek;
    img.seek(std::io::SeekFrom::Start(0))?;
    img.write_all(&mbr)?;
    img.flush()?;
    Ok(())
}

/// Read back whatever the guest left behind.
pub fn read_results(path: &Path) -> Result<Results> {
    let img = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let slice = StreamSlice::new(img, PART_START_LBA * SECTOR, SIZE_BYTES)?;
    let mut buf = BufStream::new(slice);
    let fs = FileSystem::new(&mut buf, FsOptions::new()).context("reading mailbox filesystem")?;
    let root = fs.root_dir();

    let slurp = |name: &str| -> Vec<u8> {
        match root.open_file(name) {
            Ok(mut f) => {
                let mut v = Vec::new();
                let _ = f.read_to_end(&mut v);
                v
            }
            Err(_) => Vec::new(),
        }
    };

    let stdout = slurp(OUT_FILE);
    let stderr = slurp(ERR_FILE);
    let exit_code = String::from_utf8_lossy(&slurp(CODE_FILE))
        .trim()
        .parse::<i32>()
        .ok();

    Ok(Results { stdout, stderr, exit_code })
}
