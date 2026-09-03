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

/// Give a freshly created file a length without allocating the space.
///
/// The mailbox, workspace and artifact volumes are tens or thousands of
/// megabytes of mostly nothing, and WinQuick makes a fresh copy of each per
/// prepared state. On APFS and on any Unix filesystem, `set_len` alone leaves a
/// sparse file and none of that is real. NTFS allocates eagerly unless the file
/// is explicitly marked sparse, which turned a prepared state into 4.5 GB on
/// disk and a multi-second copy. Marking it costs one call and makes both
/// disappear.
pub fn set_sparse_len(f: &File, len: u64) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        const FSCTL_SET_SPARSE: u32 = 0x000900C4;
        extern "system" {
            fn DeviceIoControl(
                handle: *mut std::ffi::c_void,
                control_code: u32,
                in_buffer: *const std::ffi::c_void,
                in_size: u32,
                out_buffer: *mut std::ffi::c_void,
                out_size: u32,
                returned: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
        }
        let mut returned: u32 = 0;
        // Best effort: a filesystem that does not support sparse files still
        // works, it just uses the space.
        unsafe {
            DeviceIoControl(
                f.as_raw_handle(),
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            );
        }
    }
    f.set_len(len)
}

/// Which parts of a file actually hold data.
///
/// The volume images WinQuick copies per run are sparse and almost entirely
/// hole: about a quarter of one percent of a two-gigabyte workspace is a FAT
/// boot sector, two allocation tables and a nearly empty root directory. The
/// fast way to copy one is to ask the filesystem where the content is, rather
/// than to read two gigabytes looking for it -- reading was itself most of what
/// a "warm" run spent its time on once the writing had been dealt with.
///
/// A filesystem that cannot answer gets the whole file back, which is correct
/// and merely slower.
// Only the non-macOS `clone_file` walks these ranges; macOS clones a file whole
// with `clonefile(2)` and never asks which parts are allocated.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn allocated_ranges(f: &File, len: u64) -> Vec<(u64, u64)> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        const FSCTL_QUERY_ALLOCATED_RANGES: u32 = 0x0009_40CF;
        const ERROR_MORE_DATA: u32 = 234;

        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct Range {
            offset: i64,
            length: i64,
        }

        extern "system" {
            fn DeviceIoControl(
                handle: *mut std::ffi::c_void,
                control_code: u32,
                in_buffer: *const std::ffi::c_void,
                in_size: u32,
                out_buffer: *mut std::ffi::c_void,
                out_size: u32,
                returned: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
            fn GetLastError() -> u32;
        }

        let whole = vec![(0u64, len)];
        let entry = std::mem::size_of::<Range>();
        let mut out: Vec<(u64, u64)> = Vec::new();
        let mut buf = vec![Range::default(); 512];
        let mut start: i64 = 0;

        while (start as u64) < len {
            let input = Range { offset: start, length: len as i64 - start };
            let mut returned: u32 = 0;
            let ok = unsafe {
                DeviceIoControl(
                    f.as_raw_handle(),
                    FSCTL_QUERY_ALLOCATED_RANGES,
                    &input as *const Range as *const std::ffi::c_void,
                    entry as u32,
                    buf.as_mut_ptr() as *mut std::ffi::c_void,
                    (buf.len() * entry) as u32,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            // A full output buffer is not a failure, it is a continuation.
            let more = ok == 0 && unsafe { GetLastError() } == ERROR_MORE_DATA;
            if ok == 0 && !more {
                return whole;
            }
            let n = returned as usize / entry;
            if n == 0 {
                break;
            }
            for r in &buf[..n] {
                if r.length > 0 {
                    out.push((r.offset as u64, r.length as u64));
                }
            }
            let last = buf[n - 1];
            start = last.offset + last.length;
            if !more {
                break;
            }
        }
        return out;
    }

    #[cfg(not(windows))]
    {
        let _ = f;
        vec![(0, len)]
    }
}

/// Fill a buffer with randomness from the operating system.
///
/// WinQuick needs this in exactly one place — giving a copied disk a fresh GPT
/// identity — where what matters is that two disks never collide, not that the
/// values resist an adversary. Both hosts have a proper source anyway, so it
/// uses one.
#[cfg(unix)]
pub fn fill_random(buf: &mut [u8]) -> Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .map_err(|e| anyhow::anyhow!("reading randomness: {e}"))?
        .read_exact(buf)
        .map_err(|e| anyhow::anyhow!("reading randomness: {e}"))
}

/// Windows has no `/dev/urandom`. `ProcessPrng` is the user-mode entry point
/// the system RNG exposes; it cannot fail and needs nothing set up first.
#[cfg(windows)]
pub fn fill_random(buf: &mut [u8]) -> Result<()> {
    #[link(name = "bcryptprimitives")]
    extern "system" {
        fn ProcessPrng(data: *mut u8, len: usize) -> i32;
    }
    let ok = unsafe { ProcessPrng(buf.as_mut_ptr(), buf.len()) };
    if ok == 0 {
        anyhow::bail!("the system random number generator refused");
    }
    Ok(())
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

    /// Give up rather than block forever on a QEMU that has stopped answering.
    ///
    /// Both underlying stream types support this and neither does it by
    /// default. Without it, a wedged QEMU wedges WinQuick too -- and because
    /// the prepared-guest build holds a lock, every other run on the machine
    /// then fails as well.
    pub fn set_read_timeout(&self, d: std::time::Duration) -> std::io::Result<()> {
        self.0.set_read_timeout(Some(d))
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

#[cfg(windows)]
pub fn open_lock_file(path: &Path) -> std::io::Result<Option<File>> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    // share_mode 0: no other process may open this file at all, which is
    // exactly the mutual exclusion the lock is for.
    // ERROR_SHARING_VIOLATION (32) and ERROR_LOCK_VIOLATION (33) are the two
    // ways Windows says "someone else has it". Neither maps to a stable
    // ErrorKind, so they are matched by number: treating them as failures
    // rather than as an answer turns a perfectly ordinary "another run is
    // holding the lock" into a crash.
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;

    match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
    {
        Ok(f) => Ok(Some(f)),
        Err(e)
            if e.kind() == std::io::ErrorKind::PermissionDenied
                || matches!(
                    e.raw_os_error(),
                    Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
                ) =>
        {
            Ok(None)
        }
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
