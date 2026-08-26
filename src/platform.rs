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
//!   Windows x64   →  qemu-system-x86_64   -M q35   -accel whpx  -cpu qemu64
//!                    → Windows x64
//! ```

/// The guest architecture this host runs, in the spelling Microsoft uses for
/// download RIDs (`win-arm64`, `win-x64`).
pub const GUEST_RID: &str = if cfg!(target_arch = "aarch64") { "win-arm64" } else { "win-x64" };

/// Short name for the guest architecture, used in image directory names.
pub const GUEST_ARCH: &str = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };

/// The QEMU system emulator for this host's guest architecture.
pub const QEMU_SYSTEM: &str =
    if cfg!(target_arch = "aarch64") { "qemu-system-aarch64" } else { "qemu-system-x86_64" };

/// QEMU machine type.
pub const MACHINE: &str = if cfg!(target_arch = "aarch64") { "virt" } else { "q35" };

/// The hardware accelerator. Never TCG: software emulation would not be the
/// same product.
pub const ACCEL: &str = if cfg!(target_os = "macos") { "hvf" } else { "whpx" };

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The host decides the guest: running an ARM64 guest on x86_64 by
    /// emulation would throw away the entire performance model.
    #[test]
    fn guest_architecture_follows_the_host() {
        if cfg!(target_arch = "aarch64") {
            assert_eq!(GUEST_ARCH, "arm64");
            assert_eq!(GUEST_RID, "win-arm64");
            assert!(QEMU_SYSTEM.ends_with("aarch64"));
        } else {
            assert_eq!(GUEST_ARCH, "x64");
            assert_eq!(GUEST_RID, "win-x64");
            assert!(QEMU_SYSTEM.ends_with("x86_64"));
        }
    }

    /// Software emulation is not the product.
    #[test]
    fn the_accelerator_is_never_tcg() {
        assert!(ACCEL == "hvf" || ACCEL == "whpx", "unexpected accelerator {ACCEL}");
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
