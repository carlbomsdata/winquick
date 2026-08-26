//! Getting files back out of a run.
//!
//! The workspace is deliberately one-way: the guest sees a throwaway copy of the
//! project and nothing it writes reaches the host. That is the isolation property
//! worth keeping, so retrieving build output is a separate, explicit request.
//!
//! A dedicated FAT32 volume is attached to every run. When `--artifact` patterns
//! are given, WinQuick writes a small batch script into the mailbox; the agent
//! runs it after the command, copying matches onto that volume and dismounting it
//! to flush. Once QEMU has exited the host reads the volume and writes the files
//! into `./winquick-artifacts/`.

use anyhow::{bail, Context, Result};
use fatfs::{FileSystem, FsOptions};
use fscommon::{BufStream, StreamSlice};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

const SECTOR: u64 = 512;
const PART_START_LBA: u64 = 2048;

/// Marker the guest agent looks for when hunting for the artifact volume.
pub const MARKER: &str = "WQARTS.TXT";
/// Whatever the guest's copy commands printed, kept for diagnostics.
pub const LOG: &str = "WQARTLOG.TXT";

pub use crate::artifact_patterns::{script, DIR};

fn open_fs(path: &Path) -> Result<FileSystem<BufStream<StreamSlice<File>>>> {
    let img = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("opening artifact volume {}", path.display()))?;
    let len = img.metadata()?.len();
    let slice = StreamSlice::new(img, PART_START_LBA * SECTOR, len)?;
    FileSystem::new(BufStream::new(slice), FsOptions::new())
        .context("reading artifact volume")
}

pub struct Extracted {
    pub files: usize,
    pub bytes: u64,
    pub log: String,
}

/// Copy everything the guest left on the artifact volume into `dest`.
pub fn extract(image: &Path, dest: &Path) -> Result<Extracted> {
    let fs = open_fs(image)?;
    let root = fs.root_dir();

    let mut log = String::new();
    if let Ok(mut f) = root.open_file(LOG) {
        let mut v = Vec::new();
        let _ = f.read_to_end(&mut v);
        log = String::from_utf8_lossy(&v).replace('\r', "");
    }

    let dir = match root.open_dir(DIR) {
        Ok(d) => d,
        Err(_) => return Ok(Extracted { files: 0, bytes: 0, log }),
    };
    let mut files = 0;
    let mut bytes = 0;
    copy_out(&dir, dest, &mut files, &mut bytes)?;
    Ok(Extracted { files, bytes, log })
}

/// Names come off a filesystem the guest could have written, so treat them as
/// hostile: reject anything that is not a single, ordinary path component.
/// Without this, an entry called `..` or `../../.ssh/authorized_keys` would let a
/// run write outside the artifacts directory.
fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !Path::new(name).is_absolute()
        && Path::new(name).components().count() == 1
}

fn copy_out<T: fatfs::ReadWriteSeek>(
    src: &fatfs::Dir<T>,
    dest: &Path,
    files: &mut usize,
    bytes: &mut u64,
) -> Result<()> {
    for e in src.iter() {
        let e = e?;
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        if !safe_component(&name) {
            eprintln!("winquick: skipping artifact with an unsafe name: {name:?}");
            continue;
        }
        let out = dest.join(&name);
        if e.is_dir() {
            std::fs::create_dir_all(&out)?;
            copy_out(&src.open_dir(&name)?, &out, files, bytes)?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = src.open_file(&name)?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            std::fs::write(&out, &buf)
                .with_context(|| format!("writing {}", out.display()))?;
            *files += 1;
            *bytes += buf.len() as u64;
        }
    }
    Ok(())
}

/// Where artifacts land by default. Never inside the project itself, so a run
/// cannot quietly overwrite source files.
pub fn default_dest() -> PathBuf {
    PathBuf::from("winquick-artifacts")
}

/// Refuse to write artifacts somewhere that would clobber a source tree unless
/// the user asked for it explicitly.
pub fn prepare_dest(dest: &Path, overwrite: bool) -> Result<()> {
    if dest.exists() {
        let non_empty = std::fs::read_dir(dest)?.next().is_some();
        if non_empty && !overwrite {
            bail!(
                "{} already exists and is not empty.\n\
                 Pass --artifact-overwrite to write into it anyway, or choose another\n\
                 directory with --artifacts-dir.",
                dest.display()
            );
        }
    }
    std::fs::create_dir_all(dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    Ok(())
}
