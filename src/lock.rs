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
use std::fs::File;
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
    crate::hostfs::try_lock(f, exclusive, block)
}

/// Try to take the lock once.
///
/// The two hosts refuse at different moments, and both refusals mean the same
/// thing. Unix always opens the file and then finds out from `flock`; Windows
/// opens it exclusively, so a busy lock is refused at open time and never
/// reaches `flock` at all. `None` means somebody else holds it -- which is an
/// answer, not a failure, and the callers below are built around waiting for it.
fn try_acquire(name: &str) -> Result<Option<Guard>> {
    let path = lock_path(name)?;
    let Some(file) = crate::hostfs::open_lock_file(&path)
        .with_context(|| format!("opening lock {}", path.display()))?
    else {
        return Ok(None);
    };
    if !try_flock(&file, true, false)? {
        return Ok(None);
    }
    Ok(Some(Guard { _file: file, path }))
}

/// Take an exclusive lock, telling the user if we have to wait for someone else.
pub fn acquire_blocking(what: &str) -> Result<Guard> {
    if let Some(g) = try_acquire("winquick")? {
        return Ok(g);
    }
    eprintln!("winquick: waiting for another winquick {what} to finish...");
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(g) = try_acquire("winquick")? {
            return Ok(g);
        }
    }
}

/// Take the prepared-guest build lock, waiting up to `timeout`.
///
/// Returns `None` if someone else holds it and finished within the timeout —
/// the caller should then re-check whether the prepared guest now exists rather
/// than building a second one.
pub fn acquire_build(timeout: Duration) -> Result<Option<Guard>> {
    let deadline = Instant::now() + timeout;
    loop {
        // Re-opened on every attempt, not just re-locked: on Windows the open
        // is the lock, so holding a handle from a failed attempt would be
        // holding nothing and waiting forever.
        if let Some(g) = try_acquire("prepare")? {
            // We have it, but someone may have built the prepared guest while
            // we waited; the caller re-checks.
            return Ok(Some(g));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
