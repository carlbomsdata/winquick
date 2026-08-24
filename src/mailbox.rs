//! The mailbox disk: how a command and its results cross the VM boundary.
//!
//! A 64 MiB MBR-partitioned FAT32 image attached as a second NVMe device. Both
//! halves use only inbox components — NVMe and FAT are built into Validation OS
//! — so nothing has to be installed in the guest.
//!
//! # Protocol v1
//!
//! | File | Direction | Meaning |
//! |---|---|---|
//! | `WQMARK.TXT`  | host → guest | marks this volume as the mailbox |
//! | `WQREADY.TXT` | guest → host | the agent has booted and is waiting |
//! | `WQCMD.CMD`   | host → guest | the command, as a batch file |
//! | `WQGO.TXT`    | host → guest | run `WQCMD.CMD` now |
//! | `WQOUT.TXT`   | guest → host | stdout |
//! | `WQERR.TXT`   | guest → host | stderr |
//! | `WQCODE.TXT`  | guest → host | exit code, written last |
//!
//! `WQCODE.TXT` appearing is the completion signal, so the agent writes it after
//! the output files.
//!
//! # Filesystem identity is load-bearing
//!
//! The guest re-reads this volume by dismounting it and re-creating the mount
//! point from its volume GUID, which is derived from the filesystem. Reformatting
//! the image between runs changes the GUID and the guest can never mount it
//! again. So the mailbox is formatted exactly once, when the ready state is
//! built; every later run clones that image and rewrites files *inside* it.

use anyhow::{Context, Result};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use fscommon::{BufStream, StreamSlice};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::Path;

pub const MARKER: &str = "WQMARK.TXT";
pub const READY: &str = "WQREADY.TXT";
pub const CMD_FILE: &str = "WQCMD.CMD";
pub const GO: &str = "WQGO.TXT";
pub const OUT_FILE: &str = "WQOUT.TXT";
pub const ERR_FILE: &str = "WQERR.TXT";
pub const CODE_FILE: &str = "WQCODE.TXT";
/// Script the agent runs after the command, to collect artifacts.
pub const ART_SCRIPT: &str = "WQART.CMD";
/// Per-run token, echoed back with the exit code to prove the guest read this
/// run's command rather than a stale cached view of the volume.
pub const NONCE: &str = "WQNONCE.TXT";

const SECTOR: u64 = 512;
const PART_START_LBA: u64 = 2048;
const SIZE_BYTES: u64 = 64 * 1024 * 1024;

pub struct Results {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    /// The token the guest echoed back, if any.
    pub nonce: Option<String>,
}

fn open_fs(path: &Path, write: bool) -> Result<FileSystem<BufStream<StreamSlice<File>>>> {
    let img = OpenOptions::new()
        .read(true)
        .write(write)
        .open(path)
        .with_context(|| format!("opening mailbox {}", path.display()))?;
    let slice = StreamSlice::new(img, PART_START_LBA * SECTOR, SIZE_BYTES)?;
    let buf = BufStream::new(slice);
    FileSystem::new(buf, FsOptions::new()).context("reading mailbox filesystem")
}

/// Format a fresh mailbox. Called once per ready state, and once per cold
/// fallback run — never between warm runs.
pub fn create_template(path: &Path) -> Result<()> {
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
    let fs = FileSystem::new(&mut buf, FsOptions::new())?;
    fs.root_dir().create_file(MARKER)?.write_all(b"winquick\r\n")?;
    fs.unmount()?;
    buf.flush()?;
    Ok(())
}

/// Windows will not mount a partitionless ("superfloppy") volume on a fixed
/// disk — it looks for a partition table first and, finding none, exposes no
/// volume at all. See docs/research.md.
fn write_mbr(mut img: &File) -> Result<()> {
    let total_sectors = SIZE_BYTES / SECTOR;
    let part_sectors = (total_sectors - PART_START_LBA) as u32;
    let mut mbr = [0u8; 512];
    let e = 446;
    mbr[e] = 0x00;
    mbr[e + 1..e + 4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    mbr[e + 4] = 0x0C; // FAT32 with LBA
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

/// Place a command into an existing mailbox without touching the filesystem
/// identity, and clear any stale results.
/// Everything the host writes into the mailbox happens here, in a single
/// open/flush cycle. Opening the volume twice in a row left the FAT inconsistent
/// and the guest then read nothing at all.
pub fn inject_command(path: &Path, command: &str, artifact_script: Option<&str>, nonce: &str) -> Result<()> {
    let fs = open_fs(path, true)?;
    {
        let root = fs.root_dir();
        for stale in [OUT_FILE, ERR_FILE, CODE_FILE] {
            let _ = root.remove(stale);
        }
        if let Some(script) = artifact_script {
            let mut a = root.create_file(ART_SCRIPT)?;
            a.truncate()?;
            a.write_all(script.as_bytes())?;
        }
        // `@echo off` keeps the child cmd.exe from echoing the command line into
        // captured stdout. CRLF because cmd.exe is unforgiving about bare LF.
        let mut c = root.create_file(CMD_FILE)?;
        c.truncate()?;
        c.write_all(b"@echo off\r\n")?;
        c.write_all(command.as_bytes())?;
        c.write_all(b"\r\n")?;
        let mut n = root.create_file(NONCE)?;
        n.truncate()?;
        n.write_all(nonce.as_bytes())?;
        // written last: this is what the agent waits on
        let mut g = root.create_file(GO)?;
        g.truncate()?;
        g.write_all(b"go\r\n")?;
    }
    fs.unmount()?;
    Ok(())
}

/// Read one file if it exists. Used to poll for readiness and completion, so it
/// stays cheap: FAT32 metadata for a handful of files is a few sectors.
pub fn probe(path: &Path, name: &str) -> Option<Vec<u8>> {
    let fs = open_fs(path, false).ok()?;
    let mut f = fs.root_dir().open_file(name).ok()?;
    let mut v = Vec::new();
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

pub fn read_results(path: &Path) -> Result<Results> {
    let fs = open_fs(path, false)?;
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
    // "<code> <nonce>"
    let raw = String::from_utf8_lossy(&slurp(CODE_FILE)).trim().to_string();
    let mut parts = raw.split_whitespace();
    let exit_code = parts.next().and_then(|c| c.parse::<i32>().ok());
    let nonce = parts.next().map(str::to_string);
    Ok(Results { stdout, stderr, exit_code, nonce })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host writes several files into the mailbox before every run. This
    /// caught a corruption bug where a second write cycle left the FAT in a state
    /// the guest read as empty.
    #[test]
    fn command_and_script_round_trip() {
        let dir = std::env::temp_dir().join(format!("wq-mbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("mailbox.img");
        create_template(&img).unwrap();

        inject_command(&img, "cmd /c echo hello", Some("@echo off\r\nrem art\r\n"), "tok1").unwrap();
        assert_eq!(
            String::from_utf8(probe(&img, CMD_FILE).expect("command file")).unwrap(),
            "@echo off\r\ncmd /c echo hello\r\n"
        );
        assert_eq!(
            String::from_utf8(probe(&img, ART_SCRIPT).expect("artifact script")).unwrap(),
            "@echo off\r\nrem art\r\n"
        );
        assert!(probe(&img, GO).is_some(), "go flag must be present");
        assert!(probe(&img, MARKER).is_some(), "marker must survive injection");

        // A second injection, as a warm run does after cloning the template.
        inject_command(&img, "cmd /c echo second", None, "tok2").unwrap();
        assert_eq!(
            String::from_utf8(probe(&img, CMD_FILE).expect("command file")).unwrap(),
            "@echo off\r\ncmd /c echo second\r\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Stale results from a previous run must never be read back as this run's.
    #[test]
    fn stale_results_are_cleared() {
        let dir = std::env::temp_dir().join(format!("wq-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("mailbox.img");
        create_template(&img).unwrap();
        {
            let fs = open_fs(&img, true).unwrap();
            for (n, d) in [(OUT_FILE, "old out"), (ERR_FILE, "old err"), (CODE_FILE, "7")] {
                fs.root_dir().create_file(n).unwrap().write_all(d.as_bytes()).unwrap();
            }
            fs.unmount().unwrap();
        }
        inject_command(&img, "cmd /c echo x", None, "tok").unwrap();
        let r = read_results(&img).unwrap();
        assert!(r.stdout.is_empty(), "stdout not cleared: {:?}", r.stdout);
        assert!(r.stderr.is_empty(), "stderr not cleared");
        assert_eq!(r.exit_code, None, "stale exit code survived injection");
        std::fs::remove_dir_all(&dir).ok();
    }
}
