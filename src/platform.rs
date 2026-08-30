//! What differs between the hosts WinQuick runs on.
//!
//! Everything above this file — the guest protocol, the workspace, artifacts,
//! capabilities, the desktop, MCP — is the same product on every host. What
//! genuinely differs is which QEMU binary to run, which accelerator and CPU
//! model to ask for, which firmware to load, and which guest architecture the
//! resulting Windows is. Those facts live here, in one place, so the rest of
//! the code can stay free of `cfg` branches.
//!
//! ```text
//!   macOS arm64   →  qemu-system-aarch64  -M virt  -accel hvf   -cpu host
//!                    → Windows ARM64
//!
//!   Windows x64   →  qemu-system-x86_64   -M q35   -accel whpx  -cpu Nehalem
//!                    → Windows x64
//! ```

/// Short name for the guest architecture, used in image directory names.
pub const GUEST_ARCH: &str = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };

/// The QEMU system emulator for this host's guest architecture.
pub const QEMU_SYSTEM: &str =
    if cfg!(target_arch = "aarch64") { "qemu-system-aarch64" } else { "qemu-system-x86_64" };

/// QEMU machine type.
pub const MACHINE: &str = if cfg!(target_arch = "aarch64") { "virt" } else { "q35" };

/// The hardware accelerator. Never TCG: software emulation would not be the
/// same product.
pub const ACCEL: &str = if cfg!(target_os = "macos") {
    "hvf"
} else if cfg!(target_os = "linux") {
    "kvm"
} else {
    "whpx"
};

/// The CPU model to ask QEMU for.
///
/// macOS passes the host CPU straight through, which is both fastest and
/// stable because Apple Silicon is one family.
///
/// Windows cannot: `-cpu host` and `-cpu max` make OVMF crash in `PlatformPei`
/// under WHPX — measured on Windows 11 26200 with QEMU 11.1. A concrete model
/// is required, and it must be the *same* concrete model everywhere, because a
/// prepared state carries the CPUID it was created with.
///
/// `qemu64` looked like the conservative choice and is wrong: it is a
/// Pentium 4-era model without SSE4.2 or POPCNT, which Windows 11 requires. The
/// guest firmware and kernel start, then userland never comes up — measured, by
/// a guest that reached a kernel address and then sat there with an unchanging
/// RIP and never ran its first `cmd.exe`.
///
/// `Nehalem` is the oldest model that carries SSE4.2 and POPCNT, so it is the
/// least demanding thing a Windows 11 guest will actually boot on, and it is
/// available on any x86_64 host from roughly 2008 onwards. `Skylake-Client`
/// also works and exposes more, at the cost of requiring newer hardware.
pub const CPU_MODEL: &str = if cfg!(target_os = "macos") { "host" } else { "Nehalem" };

/// The UEFI firmware code image QEMU ships for this guest architecture.
pub const UEFI_CODE: &str =
    if cfg!(target_arch = "aarch64") { "edk2-aarch64-code.fd" } else { "edk2-x86_64-code.fd" };

/// The variable-store template to seed a writable varstore from.
///
/// aarch64 has no template in QEMU's share directory, so WinQuick creates a
/// blank one of the right size. x86_64 does ship one, and the x86_64 code image
/// expects that exact layout.
pub const UEFI_VARS_TEMPLATE: Option<&str> =
    if cfg!(target_arch = "aarch64") { None } else { Some("edk2-i386-vars.fd") };

/// Whether the guest state can be saved and restored at all on this host.
///
/// On Windows this needs a QEMU carrying the WHPX stop-and-copy patches; stock
/// QEMU refuses every state save behind an unconditional migration blocker.
/// `doctor` uses this to explain the situation rather than letting a run fail
/// mysteriously.
pub const NEEDS_PATCHED_QEMU: bool = cfg!(target_os = "windows");

/// Everything that makes one prepared state incompatible with another.
///
/// A state carries the CPUID, machine and accelerator it was created under, so
/// restoring it into a differently-configured QEMU is not merely slower — it is
/// wrong. This string goes into the state's fingerprint, and a mismatch forces
/// a rebuild rather than a silent reuse.
pub fn backend_signature() -> String {
    format!(
        "host={}-{};accel={ACCEL};machine={MACHINE};cpu={CPU_MODEL};guest={GUEST_ARCH}",
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// The largest number of virtual processors a *reusable prepared state* can be
/// restored onto on this host.
///
/// `None` means the host has no such limit, which is the normal case: macOS/HVF
/// restores any count WinQuick asks for, and this must stay `None` for Linux/KVM
/// unless a Linux host is measured to need otherwise.
///
/// Windows is the exception, and the reason is specific. Windows parks an idle
/// processor on a Hyper-V synthetic timer whose expiry is an absolute point in
/// the partition's reference-time domain. Restoring a prepared state builds a
/// *fresh* WHP partition, and the public Windows Hypervisor Platform API exposes
/// neither the source partition's reference count nor its synthetic timer state,
/// so those absolute expiries cannot be rebased onto the new partition's clock.
/// With one or two processors the guest always has someone left to reprogram its
/// own timers; beyond that, measured on Windows 11 26200 with QEMU 11.1, roughly
/// four prepared guests in five come back with a processor that never wakes.
///
/// This is a limit of reconstructing a partition from a saved state, not of WHP:
/// a cold-booted WHPX guest runs four processors perfectly well, which is why
/// `--cold` is the documented way out rather than a silent fallback.
pub const MAX_PREPARED_CPUS: Option<u32> =
    if cfg!(target_os = "windows") { Some(2) } else { None };

/// Refuses a vCPU count this host cannot restore a prepared state onto.
///
/// Called only on the path that actually reuses a prepared state. A cold run is
/// a different mechanism with a different answer, so it is not checked here.
pub fn check_prepared_cpus(cpus: u32) -> anyhow::Result<()> {
    let Some(max) = MAX_PREPARED_CPUS else { return Ok(()) };
    if cpus <= max {
        return Ok(());
    }
    anyhow::bail!(
        "{cpus} processors are not supported for fast runs on this host.\n\n\
         A fast run resumes a prepared guest, which rebuilds the Windows\n\
         Hypervisor Platform partition from saved state. Windows parks its idle\n\
         processors on Hyper-V synthetic timers that the public WHP API gives no\n\
         way to carry across that rebuild, so above {max} processors a resumed\n\
         guest usually comes back with one that never wakes. A cold boot does not\n\
         go through that path and is unaffected.\n\n\
         Use at most {max} processors:\n    \
         winquick run --cpus {max} -- <command>\n\n\
         or boot Windows from scratch, which supports any count but is much\n\
         slower per run:\n    \
         winquick run --cold --cpus {cpus} -- <command>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host decides the guest: running an ARM64 guest on x86_64 by
    /// emulation would throw away the entire performance model.
    #[test]
    fn guest_architecture_follows_the_host() {
        if cfg!(target_arch = "aarch64") {
            assert_eq!(GUEST_ARCH, "arm64");
            assert!(QEMU_SYSTEM.ends_with("aarch64"));
        } else {
            assert_eq!(GUEST_ARCH, "x64");
            assert!(QEMU_SYSTEM.ends_with("x86_64"));
        }
    }

    /// Software emulation is not the product.
    #[test]
    fn the_accelerator_is_never_tcg() {
        assert!(
            matches!(ACCEL, "hvf" | "kvm" | "whpx"),
            "unexpected accelerator {ACCEL}"
        );
    }

    /// `-cpu host` crashes OVMF under WHPX, so Windows must pin a concrete
    /// model — and it must stay pinned, because prepared states carry it.
    ///
    /// It also has to be new enough for Windows 11: `qemu64` predates SSE4.2
    /// and POPCNT, and a guest on it boots its kernel and never reaches
    /// userland.
    #[test]
    fn windows_pins_a_cpu_model_windows_11_can_boot() {
        if cfg!(target_os = "windows") {
            assert_ne!(CPU_MODEL, "host");
            assert_ne!(CPU_MODEL, "max");
            assert_ne!(CPU_MODEL, "qemu64", "qemu64 has no SSE4.2/POPCNT");
        }
    }

    /// A fingerprint that omitted any of these would let a state be reused with
    /// a configuration it was not created for.
    #[test]
    fn the_backend_signature_pins_what_a_state_depends_on() {
        let s = backend_signature();
        for part in ["host=", "accel=", "machine=", "cpu=", "guest="] {
            assert!(s.contains(part), "{part} missing from {s}");
        }
        assert!(s.contains(ACCEL));
        assert!(s.contains(CPU_MODEL));
        assert!(s.contains(GUEST_ARCH));
    }

    #[test]
    fn the_prepared_cpu_limit_is_a_windows_fact_only() {
        if cfg!(target_os = "windows") {
            assert_eq!(MAX_PREPARED_CPUS, Some(2));
            assert!(check_prepared_cpus(2).is_ok());
            let e = check_prepared_cpus(4).unwrap_err().to_string();
            // The message has to say what to do, not just that it refuses.
            assert!(e.contains("--cold"), "{e}");
            assert!(e.contains("--cpus 2"), "{e}");
        } else {
            // macOS/HVF restores four, and a future Linux/KVM host must not
            // inherit a limit that was measured on WHP.
            assert_eq!(MAX_PREPARED_CPUS, None);
            assert!(check_prepared_cpus(4).is_ok());
            assert!(check_prepared_cpus(64).is_ok());
        }
    }

    #[test]
    fn the_default_cpu_count_is_within_the_limit() {
        if let Some(max) = MAX_PREPARED_CPUS {
            assert!(crate::runner::DEFAULT_CPUS <= max);
            assert!(crate::desktop::DEFAULT_CPUS <= max);
        }
    }

    #[test]
    fn firmware_matches_the_guest_architecture() {
        if cfg!(target_arch = "aarch64") {
            assert!(UEFI_CODE.contains("aarch64"));
            assert!(UEFI_VARS_TEMPLATE.is_none());
        } else {
            assert!(UEFI_CODE.contains("x86_64"));
            assert!(UEFI_VARS_TEMPLATE.is_some());
        }
    }
}
