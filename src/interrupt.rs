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

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// PID of the QEMU process for the run in progress, or 0.
static CHILD: AtomicI32 = AtomicI32::new(0);

type Handler = extern "C" fn(i32);

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn kill(pid: i32, sig: i32) -> i32;
}

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const SIGPIPE: i32 = 13;
const SIG_DFL: usize = 0;

extern "C" fn handle(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe { kill(pid, SIGKILL) };
    }
}

/// Install the handlers. Safe to call once, early in `main`.
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

/// Note the QEMU process to kill if we are interrupted.
pub fn watch_child(pid: u32) {
    CHILD.store(pid as i32, Ordering::SeqCst);
}

/// The run finished on its own; stop watching.
pub fn clear_child() {
    CHILD.store(0, Ordering::SeqCst);
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}
