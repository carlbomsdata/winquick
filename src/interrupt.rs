//! Ctrl-C without leaving a virtual machine running.
//!
//! Rust's default handling of SIGINT terminates the process immediately, which
//! skips every `Drop` — so the QEMU child keeps running and the run directory
//! stays behind. Both are unacceptable: an abandoned VM holds a gigabyte of RAM
//! and the user has no idea it exists.
//!
//! The handler itself does only async-signal-safe work: it records that we were
//! interrupted and sends the child a signal. The main thread notices on its next
//! poll and unwinds normally, so the usual cleanup runs.
//!
//! Windows has neither signals nor `SIGPIPE`, so the same shape is built from
//! `SetConsoleCtrlHandler` plus a **Job Object**. Every QEMU is assigned to a
//! job whose kill-on-close limit is set, which closes a hole the Unix side
//! still has: if WinQuick is killed outright rather than interrupted, the
//! kernel tears the VM down anyway when the last handle to the job goes.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// PID of the QEMU process for the run in progress, or 0.
static CHILD: AtomicU32 = AtomicU32::new(0);

/// What both handlers do: remember it happened, and take the VM down with us.
fn on_interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        crate::proc::force_kill(pid);
    }
}

// ---------------------------------------------------------------- Unix

#[cfg(unix)]
mod imp {
    use super::on_interrupt;

    type Handler = extern "C" fn(i32);

    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;

    extern "C" fn handle(_sig: i32) {
        on_interrupt();
    }

    pub fn install() {
        unsafe {
            let h: Handler = handle;
            signal(SIGINT, h as usize);
            signal(SIGTERM, h as usize);
            // Rust ignores SIGPIPE at startup, which turns `winquick run ... | head`
            // into a panic on the first write to the closed pipe. Restore the normal
            // command-line behaviour: exit quietly.
            //
            // This is safe with respect to leaving a VM running, because nothing is
            // written to stdout until after QEMU has already been shut down.
            signal(SIGPIPE, SIG_DFL);
        }
    }

    /// Nothing to do: on Unix the signal handler reaches the child by pid.
    pub fn contain(_pid: u32) {}
}

// ------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use super::on_interrupt;
    use std::sync::atomic::{AtomicIsize, Ordering};

    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;
    const CTRL_CLOSE_EVENT: u32 = 2;

    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time: i64,
        per_job_user_time: i64,
        limit_flags: u32,
        minimum_working_set: usize,
        maximum_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_ops: u64,
        write_ops: u64,
        other_ops: u64,
        read_bytes: u64,
        write_bytes: u64,
        other_bytes: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
        fn CreateJobObjectW(attrs: *const u8, name: *const u16) -> isize;
        fn SetInformationJobObject(job: isize, class: i32, info: *const u8, len: u32) -> i32;
        fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn CloseHandle(handle: isize) -> i32;
    }

    const PROCESS_SET_QUOTA: u32 = 0x0100;
    const PROCESS_TERMINATE: u32 = 0x0001;

    /// The job every QEMU we start is put into.
    ///
    /// Its whole purpose is the kill-on-close limit: when this process exits
    /// for any reason -- a clean exit, a panic, Task Manager, a power of the
    /// plug on the debugger -- Windows closes the last handle to the job and
    /// terminates everything in it. That is a stronger guarantee than the Unix
    /// side has, where a `SIGKILL` to WinQuick would strand QEMU.
    static JOB: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "system" fn handle(kind: u32) -> i32 {
        match kind {
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
                on_interrupt();
                // Handled: Windows must not terminate us, or the normal
                // unwinding that removes the run directory never happens.
                1
            }
            _ => 0,
        }
    }

    pub fn install() {
        unsafe {
            SetConsoleCtrlHandler(Some(handle), 1);

            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job == 0 {
                return;
            }
            let mut info = ExtendedLimits::default();
            info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &info as *const _ as *const u8,
                std::mem::size_of::<ExtendedLimits>() as u32,
            );
            if ok == 0 {
                CloseHandle(job);
                return;
            }
            JOB.store(job, Ordering::SeqCst);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Windows checks the length it is given and refuses a mismatch, and
        /// `SetInformationJobObject` failing here would lose containment
        /// silently -- the VM would still run, and would still be strandable.
        /// These are the sizes from `winnt.h` on x86_64.
        #[test]
        fn the_job_limit_structures_are_the_size_windows_expects() {
            assert_eq!(std::mem::size_of::<BasicLimits>(), 64);
            assert_eq!(std::mem::size_of::<IoCounters>(), 48);
            assert_eq!(std::mem::size_of::<ExtendedLimits>(), 144);
        }
    }

    /// Put a freshly started child into the job, so it cannot outlive us.
    pub fn contain(pid: u32) {
        let job = JOB.load(Ordering::SeqCst);
        if job == 0 {
            return;
        }
        unsafe {
            let h = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if h != 0 {
                AssignProcessToJobObject(job, h);
                CloseHandle(h);
            }
        }
    }
}

/// Install the handlers. Safe to call once, early in `main`.
pub fn install() {
    imp::install();
}

/// Note the QEMU process to kill if we are interrupted.
pub fn watch_child(pid: u32) {
    CHILD.store(pid, Ordering::SeqCst);
    imp::contain(pid);
}

/// The run finished on its own; stop watching.
pub fn clear_child() {
    CHILD.store(0, Ordering::SeqCst);
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}
