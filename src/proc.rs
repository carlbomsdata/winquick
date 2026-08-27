//! Controlling another process, on both hosts.
//!
//! WinQuick needs three things of a process it did not spawn, or spawned and
//! then let go of: is it still alive, please stop, and stop now. Unix says all
//! three with `kill`; Windows needs a handle and three different calls. Both
//! spellings live here so the callers can stay platform-free.

/// Is this process still running?
///
/// A pid that has exited but not been reaped still answers yes on Unix, which
/// is correct: it is still in the process table and still ours to clean up.
#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    // Signal 0 performs the permission and existence checks and sends nothing.
    unsafe { libc_kill(pid as i32, 0) == 0 }
}

#[cfg(unix)]
pub fn terminate(pid: u32) {
    unsafe { libc_kill(pid as i32, 15) };
}

#[cfg(unix)]
pub fn force_kill(pid: u32) {
    unsafe { libc_kill(pid as i32, 9) };
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
mod win {
    pub const PROCESS_TERMINATE: u32 = 0x0001;
    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    pub const STILL_ACTIVE: u32 = 259;

    extern "system" {
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        pub fn TerminateProcess(handle: isize, exit_code: u32) -> i32;
        pub fn GetExitCodeProcess(handle: isize, code: *mut u32) -> i32;
        pub fn CloseHandle(handle: isize) -> i32;
    }
}

/// Windows has no signals, so "alive" is asked of a handle: a process that has
/// exited reports its exit code instead of `STILL_ACTIVE`. A pid we cannot open
/// at all is treated as gone, which is the useful answer for a caller deciding
/// whether to clean up after it.
#[cfg(windows)]
pub fn is_alive(pid: u32) -> bool {
    unsafe {
        let h = win::OpenProcess(win::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return false;
        }
        let mut code: u32 = 0;
        let ok = win::GetExitCodeProcess(h, &mut code) != 0;
        win::CloseHandle(h);
        ok && code == win::STILL_ACTIVE
    }
}

/// Windows offers nothing between "ask politely" and "stop now" for a process
/// with no message loop, and QEMU running headless has none. Both spellings
/// therefore terminate; the distinction is kept because it is meaningful on the
/// other host and the callers read better for it.
#[cfg(windows)]
pub fn terminate(pid: u32) {
    force_kill(pid);
}

#[cfg(windows)]
pub fn force_kill(pid: u32) {
    unsafe {
        let h = win::OpenProcess(win::PROCESS_TERMINATE, 0, pid);
        if h != 0 {
            win::TerminateProcess(h, 1);
            win::CloseHandle(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    /// A pid that cannot exist must not be reported as running, or cleanup
    /// would wait forever on a process that was never there.
    #[test]
    fn an_impossible_pid_is_not_alive() {
        assert!(!is_alive(0xFFFF_FFFE));
    }
}
