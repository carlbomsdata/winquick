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
/// Directory on the volume the guest copies into.
pub const DIR: &str = "artifacts";
/// Whatever the guest's copy commands printed, kept for diagnostics.
pub const LOG: &str = "WQARTLOG.TXT";

/// Turn `--artifact` patterns into the batch script the agent runs.
///
/// # Pattern semantics
///
/// Patterns are **relative to the workspace root** (`C:\workspace`) and are
/// resolved **in the guest**, by Windows, because that is where the files are.
/// Forward and backward slashes are both accepted and normalised, so the same
/// pattern works whether it was typed on macOS or copied from a Windows script.
///
/// Three forms, deliberately not a glob engine:
///
/// | Pattern | Meaning |
/// |---|---|
/// | `bin/Release/**` | that directory, recursively, hierarchy preserved |
/// | `*.log`, `logs/*.txt` | wildcard match within one directory |
/// | `logs/build.log` | one named file or directory |
pub fn script(patterns: &[String]) -> String {
    let mut s = String::from("@echo off\r\n");
    s.push_str("set WQ_ART_FAIL=0\r\n");
    s.push_str(&format!("if not exist %WQART%\\{DIR} mkdir %WQART%\\{DIR}\r\n"));
    for p in patterns {
        let norm = p.replace('/', "\\");
        let norm = norm.trim_start_matches('\\').to_string();
        if let Some(dir) = norm.strip_suffix("\\**").or_else(|| norm.strip_suffix("\\*")) {
            // A whole directory tree, keeping its position under the workspace.
            s.push_str(&format!(
                "if exist \"C:\\workspace\\{dir}\" (\r\n  \
                   xcopy \"C:\\workspace\\{dir}\" \"%WQART%\\{DIR}\\{dir}\\\" /E /I /Y /Q\r\n  \
                   if errorlevel 1 set WQ_ART_FAIL=1\r\n\
                 ) else (\r\n  echo winquick: no match for {dir}\\**\r\n)\r\n"
            ));
        } else if norm == "**" {
            s.push_str(&format!(
                "xcopy \"C:\\workspace\" \"%WQART%\\{DIR}\\\" /E /I /Y /Q\r\n\
                 if errorlevel 1 set WQ_ART_FAIL=1\r\n"
            ));
        } else {
            let parent = match norm.rfind('\\') {
                Some(i) => &norm[..i],
                None => "",
            };
            let dest = if parent.is_empty() {
                format!("%WQART%\\{DIR}\\")
            } else {
                format!("%WQART%\\{DIR}\\{parent}\\")
            };
            if !parent.is_empty() {
                s.push_str(&format!("if not exist \"{dest}\" mkdir \"{dest}\"\r\n"));
            }
            // /I keeps xcopy from asking whether the destination is a directory.
            s.push_str(&format!(
                "if exist \"C:\\workspace\\{norm}\" (\r\n  \
                   xcopy \"C:\\workspace\\{norm}\" \"{dest}\" /E /I /Y /Q\r\n  \
                   if errorlevel 1 set WQ_ART_FAIL=1\r\n\
                 ) else (\r\n  echo winquick: no match for {norm}\r\n)\r\n"
            ));
        }
    }
    s.push_str("echo winquick-artifact-status=%WQ_ART_FAIL%\r\n");
    s
}

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
