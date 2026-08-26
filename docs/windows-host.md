# Running WinQuick *on* Windows — where this stands

WinQuick's guest has always been Windows. This is about the other half: making
Windows a **host**, so a developer on Windows x86_64 gets the same product a
developer on an Apple Silicon Mac gets.

**Status: not implemented.** This document records what was measured, what the
work actually is, and the one question that has to be answered before any of it
is worth writing.

## The rule that must not be broken

A Windows host must not shortcut the product:

```
    WinQuick on Windows  ─X→  CreateProcess("cmd.exe")
```

Running commands on the host's own Windows installation would be a different
product. WinQuick means a *disposable* Windows: a guest that starts clean,
executes, and is discarded, leaving the host untouched. On Windows that still
means a virtual machine.

```
    Windows host
        ↓
    accelerated hypervisor
        ↓
    disposable Windows x86_64 guest
        ↓
    command / build / test / desktop
        ↓
    guest discarded
```

## The question that gated everything — answered

WinQuick is fast because it does not boot Windows. It restores a **prepared
migration state** into a fresh QEMU process — that is what turns a ~20 second
boot into a ~300 ms command. Every other property is downstream of it.

So the first thing to establish on Windows was whether that survives. It was
tested on real hardware: Windows 11 Pro 25H2 (build 26200), Intel i5-8265U,
Windows Hypervisor Platform enabled, QEMU 11.1.0 from the Software Freedom
Conservancy's winget package.

**It does not. QEMU's WHPX accelerator cannot save or restore VM state.**

Asking a running guest to migrate returns:

```
{"error": {"class": "GenericError", "desc":
 "State blocked due to missing dirty memory tracking support,
  And some system register/state save-restore "}}
```

This is a migration blocker the WHPX accelerator registers at initialisation.
It was isolated as follows:

| Test | Result |
|---|---|
| Full WinQuick-shaped VM (UEFI, NVMe, ramfb), `migrate file:` | blocked |
| Bare VM, `-nodefaults`, no disks, no devices at all | **blocked — same message** |
| `savevm` internal snapshot instead of migration | blocked, same message |
| CPU models `qemu64`, `Skylake-Client`, `Nehalem` | blocked |
| Same bare VM under TCG instead of WHPX | migrate **accepted** |

Because a VM with no devices whatsoever is still refused, and the identical
configuration under TCG is not, the cause is neither a device, a disk, the
firmware, the CPU model nor the machine type. It is the accelerator.

Two smaller findings came out of the same work:

- **`-cpu host` and `-cpu max` crash OVMF under WHPX**, in `PlatformPei`.
  Concrete models boot fine. WinQuick uses `-cpu host` on macOS, so a Windows
  backend could not have reused that.
- **QEMU's migration transports are weak on Windows** independently of WHPX.
  Under TCG, `file:` failed with `Failed to set FD nonblocking`, and `exec:`
  wants a helper program that does not exist there. Moot given the blocker
  above, but it means even a future WHPX with state support would need a
  working transport.

Falling back to TCG is not an option: it is software emulation, and WinQuick's
whole proposition is hardware-accelerated disposable Windows.

**So QEMU + WHPX cannot host WinQuick's architecture**, and that is a property
of the backend rather than something configuration can fix. The guest booted
fine — the Windows kernel was executing at a kernel-range RIP under WHPX — so
this is specifically about freezing and restoring it, not about running it.

### What that leaves

A Windows backend would have to come from somewhere other than WHPX. Hyper-V
can checkpoint and restore, but adopting it means a genuinely separate backend:
no QEMU, VHDX instead of qcow2, a WMI lifecycle instead of QMP, and a different
route for the guest control channel — and checkpoint *apply* is typically
measured in seconds, against the ~300 ms WinQuick exists to deliver. It is also
not currently enabled on the validation machine, and turning it on requires
elevation and a reboot.

That is a product decision with real cost, not an implementation detail, so it
is written down here rather than started.

## What the code audit found

Measured against the current tree with
`cargo check --target x86_64-pc-windows-msvc`:

**16 compile errors, in 6 files.** Every one is in the host seam, not in the
product logic:

| Kind | Where | Count |
|---|---|---|
| `std::os::unix` imports | helpers, state, qmp, lock, capability, mcp | 7 |
| `MetadataExt::blocks` (allocated size) | capability, facts, state | 3 |
| `MetadataExt::mtime`/`ino` (image identity) | state | 3 |
| `as_raw_fd` / `from_raw_fd` (MCP stdout capture) | mcp | 2 |
| accelerator, binary name, firmware discovery | qemu, helpers, runner | conditional |

That is a small, well-localised surface — the guest protocol, workspace,
artifact, capability, desktop and MCP layers are already platform-neutral. The
port is not blocked by the language. It is blocked by the backend decision.

Other host assumptions the audit catalogued: `hvf` as the accelerator (5
references), `qemu-system-aarch64` as the binary name (3 files), Homebrew paths
for dependency discovery (12), `hdiutil` for mounting Microsoft media (9), Unix
signals for process cleanup (6), and `win-arm64` as the capability RID (4).

## What the work would be

Sketched so the estimate is honest, not so it can be started blind:

- **A host backend seam.** Executable and firmware discovery, accelerator
  selection, process invocation and containment, dependency detection,
  filesystem locking, native path handling. Guest semantics — workspace,
  artifacts, capabilities, the command protocol, desktop, MCP — stay shared.
- **Process containment.** Unix signal handling does not translate. Windows
  wants **Job Objects**, so that a killed or interrupted WinQuick cannot strand
  `qemu-system-x86_64.exe`.
- **File locking.** Windows share semantics are stricter; every image, overlay,
  prepared state and control disk needs its handle lifetime checked rather than
  papered over with retries.
- **Architecture-aware capabilities.** PowerShell, the .NET runtime and SDK,
  and the desktop bridge all resolve architecture-specific payloads today
  (`win-arm64`). One logical capability must resolve `win-x64` too, and a
  prepared state must be fingerprinted by host OS, host architecture, guest
  architecture, backend and version so an ARM64 capability can never attach to
  an x64 guest.
- **Guest components.** The desktop bridge is currently a **native ARM64**
  binary. An x64 guest needs an x64 build; shipping the ARM64 one would simply
  not run.
- **Media.** `setup` must fetch the x64 Validation OS rather than the ARM64
  image, and `hdiutil` has no Windows equivalent — mounting Microsoft media
  needs a native path.
- **Distribution.** A Windows user should not assemble a QEMU directory by
  hand. Whether that means a pinned dependency, a bundled runtime with its GPL
  obligations satisfied, or WinGet is a decision that follows the backend
  choice.

## Why the port stopped here

Prove the restore mechanism before porting. The experiment was run, on real
hardware, and it came back negative for reasons that no amount of the remaining
work would change: the accelerator cannot save VM state, so there is nothing for
the rest of the port to build on.

What *was* worth doing regardless of the backend has been done — the shared core
now compiles for `x86_64-pc-windows-msvc` with no errors, and the platform seam
is isolated in `src/hostfs.rs`. Whichever backend a Windows port eventually
uses, that work stands.

Apple Silicon macOS remains the supported host.
