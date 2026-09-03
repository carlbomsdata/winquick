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

/// Is this pid a QEMU?
///
/// Asked before signalling a process WinQuick did not spawn and cannot prove it
/// owns. Pids are reused, and a run directory left behind by a killed run names
/// a pid that may since have become something else entirely -- so the name is
/// checked before anything is sent to it. Killing the wrong process because a
/// number came round again is far worse than leaving one QEMU running.
#[cfg(unix)]
pub fn looks_like_qemu(pid: u32) -> bool {
    let Ok(out) = std::process::Command::new("/bin/ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let name = String::from_utf8_lossy(&out.stdout);
    // `ps -o comm=` gives the full path on macOS and the bare name on Linux.
    name.trim().rsplit('/').next().unwrap_or("").starts_with("qemu-system")
}

/// Windows has no `ps`, and `tasklist` reports the image name but not the
/// command line, which is enough for this question.
#[cfg(windows)]
pub fn looks_like_qemu(pid: u32) -> bool {
    let Ok(out) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // `"qemu-system-x86_64.exe","1234",...`
    text.trim_start().trim_start_matches('"').starts_with("qemu-system")
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
