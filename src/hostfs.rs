//! The handful of filesystem facts WinQuick needs that Unix and Windows spell
//! differently.
//!
//! These are not abstractions over anything interesting — they are the four or
//! five places where "how big is this really", "is this the same file as
//! before" and "hold this lock" have no portable spelling in `std`. Keeping
//! them here means the rest of the code stays free of `cfg` branches, and the
//! platform differences are visible in one place where they can be reasoned
//! about together.

use anyhow::Result;
use std::fs::File;
use std::path::Path;

/// How much disk a file actually occupies.
///
/// On Unix this is the allocated block count, which is what makes a sparse
/// qcow2 report its real footprint rather than its virtual size. Windows has no
/// cheap stable equivalent in `std`, so the logical length is used; for
/// WinQuick's images the two agree closely enough for the sizes reported by
/// `info` and `doctor`.
#[cfg(unix)]
pub fn allocated(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).map(|m| m.blocks() * 512).unwrap_or(0)
}

#[cfg(windows)]
pub fn allocated(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Enough of a file's identity to notice that it changed underneath us.
///
/// Length plus modification time is portable and sufficient: this exists to
/// invalidate a prepared state when its backing image is rebuilt, not to defend
/// against a deliberate forgery. The Unix inode used to be part of this; it has
/// no portable counterpart and added nothing that the pair does not already
/// catch.
pub fn identity(p: &Path) -> Result<(u64, i128)> {
    let m = std::fs::metadata(p)?;
    let modified = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    Ok((m.len(), modified))
}

// ------------------------------------------------------------------- locking

/// Take an advisory lock on an open file, or report that someone else holds it.
///
/// Unix uses `flock`, which the kernel releases when the descriptor closes.
/// Windows has no advisory lock with the same shape, but an exclusive *share
/// mode* gives the same guarantee from the other direction: the second opener
/// simply cannot open the file. Both are released by closing, so neither can
/// strand a lock behind a crashed process.
#[cfg(unix)]
pub fn try_lock(f: &File, exclusive: bool, block: bool) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    const LOCK_SH: i32 = 1;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    let mut op = if exclusive { LOCK_EX } else { LOCK_SH };
    if !block {
        op |= LOCK_NB;
    }
    if unsafe { flock(f.as_raw_fd(), op) } == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    // EWOULDBLOCK: somebody else holds it, which is an answer rather than a fault.
    if !block && e.raw_os_error() == Some(35) {
        Ok(false)
    } else {
        Err(e)
    }
}

/// On Windows the lock is taken by *opening* the file exclusively, so by the
/// time a caller has a `File` the question is already settled.
#[cfg(windows)]
pub fn try_lock(_f: &File, _exclusive: bool, _block: bool) -> std::io::Result<bool> {
    Ok(true)
}

/// Open a lock file, taking the lock if the platform does that at open time.
///
/// `None` means another process holds it.
#[cfg(unix)]
pub fn open_lock_file(path: &Path) -> std::io::Result<Option<File>> {
    use std::fs::OpenOptions;
    OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path).map(Some)
}

#[cfg(windows)]
pub fn open_lock_file(path: &Path) -> std::io::Result<Option<File>> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    // share_mode 0: no other process may open this file at all, which is
    // exactly the mutual exclusion the lock is for.
    match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
    {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_reports_something_for_a_real_file() {
        let p = std::env::temp_dir().join(format!("wq-alloc-{}", std::process::id()));
        std::fs::write(&p, vec![0u8; 8192]).unwrap();
        assert!(allocated(&p) >= 4096, "a 8 KiB file should not report near zero");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn allocated_is_zero_for_a_missing_file() {
        assert_eq!(allocated(Path::new("/nonexistent/winquick/file")), 0);
    }

    /// Identity has to move when the file is rewritten, or a stale prepared
    /// state would be trusted after its image was rebuilt.
    #[test]
    fn identity_changes_when_the_file_does() {
        let p = std::env::temp_dir().join(format!("wq-id-{}", std::process::id()));
        std::fs::write(&p, b"one").unwrap();
        let a = identity(&p).unwrap();
        // Length alone is enough to distinguish these, independent of clock
        // granularity.
        std::fs::write(&p, b"one plus more").unwrap();
        let b = identity(&p).unwrap();
        assert_ne!(a, b, "rewriting the file must change its identity");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn identity_of_a_missing_file_is_an_error() {
        assert!(identity(Path::new("/nonexistent/winquick/file")).is_err());
    }
}

// ------------------------------------------------------- QEMU monitor socket

/// The connection to QEMU's monitor.
///
/// QEMU is asked for a Unix socket on macOS and a TCP port on Windows, which
/// has no Unix sockets. Both are byte streams with identical semantics once
/// connected, so the rest of the QMP client never has to know which it got.
/// The `endpoint` is a path on Unix and a file holding `127.0.0.1:<port>` on
/// Windows, written by whoever launched QEMU.
#[cfg(unix)]
pub struct ControlStream(std::os::unix::net::UnixStream);

#[cfg(windows)]
pub struct ControlStream(std::net::TcpStream);

impl ControlStream {
    #[cfg(unix)]
    pub fn connect(endpoint: &Path) -> std::io::Result<Self> {
        if !endpoint.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the monitor socket does not exist yet",
            ));
        }
        std::os::unix::net::UnixStream::connect(endpoint).map(ControlStream)
    }

    #[cfg(windows)]
    pub fn connect(endpoint: &Path) -> std::io::Result<Self> {
        let addr = std::fs::read_to_string(endpoint)?;
        std::net::TcpStream::connect(addr.trim()).map(ControlStream)
    }

    pub fn try_clone(&self) -> std::io::Result<Self> {
        self.0.try_clone().map(ControlStream)
    }
}

impl std::io::Read for ControlStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl std::io::Write for ControlStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
