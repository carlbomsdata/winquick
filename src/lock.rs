//! Serialising operations that touch shared state.
//!
//! Two `winquick run` invocations can safely proceed at once: each gets its own
//! run directory, its own QEMU, and its own clones of everything. What cannot
//! overlap is anything that *writes* shared state — setup, capability changes,
//! cache syncs, cleanup, and rebuilding the prepared guest.
//!
//! So `run` takes no lock, and the operations that mutate take an exclusive one.
//! Rebuilding the prepared guest is the interesting case: several concurrent
//! runs can each discover there is no prepared guest, and only one should build
//! it. The rest wait, then find it ready.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Guard {
    _file: File,
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // flock is released when the descriptor closes; the file itself is just
        // a rendezvous point and can stay.
        let _ = &self.path;
    }
}

fn lock_path(name: &str) -> Result<PathBuf> {
    let dir = crate::paths::root()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!(".{name}.lock")))
}

fn try_flock(f: &File, exclusive: bool, block: bool) -> std::io::Result<bool> {
    let mut op = if exclusive { libc_lock_ex() } else { libc_lock_sh() };
    if !block {
        op |= libc_lock_nb();
    }
    let rc = unsafe { flock(f.as_raw_fd(), op) };
    if rc == 0 {
        Ok(true)
    } else {
        let e = std::io::Error::last_os_error();
        if !block && e.raw_os_error() == Some(35) {
            // EWOULDBLOCK
            Ok(false)
        } else {
            Err(e)
        }
    }
}

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
fn libc_lock_sh() -> i32 {
    1
}
fn libc_lock_ex() -> i32 {
    2
}
fn libc_lock_nb() -> i32 {
    4
}

fn open_lock(name: &str) -> Result<(File, PathBuf)> {
    let path = lock_path(name)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening lock {}", path.display()))?;
    Ok((file, path))
}

/// Take an exclusive lock, telling the user if we have to wait for someone else.
pub fn acquire_blocking(what: &str) -> Result<Guard> {
    let (file, path) = open_lock("winquick")?;
    if !try_flock(&file, true, false)? {
        eprintln!("winquick: waiting for another winquick {what} to finish...");
        try_flock(&file, true, true)?;
    }
    Ok(Guard { _file: file, path })
}

/// Take the prepared-guest build lock, waiting up to `timeout`.
///
/// Returns `None` if someone else holds it and finished within the timeout —
/// the caller should then re-check whether the prepared guest now exists rather
/// than building a second one.
pub fn acquire_build(timeout: Duration) -> Result<Option<Guard>> {
    let (file, path) = open_lock("prepare")?;
    if try_flock(&file, true, false)? {
        return Ok(Some(Guard { _file: file, path }));
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if try_flock(&file, true, false)? {
            // We got it, but someone may have built it while we waited; the
            // caller re-checks.
            return Ok(Some(Guard { _file: file, path }));
        }
    }
    Ok(None)
}
