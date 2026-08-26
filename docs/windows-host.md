# Running WinQuick *on* Windows — where this stands

WinQuick's guest has always been Windows. This is about the other half: making
Windows a **host**, so a developer on Windows x86_64 gets the same product a
developer on an Apple Silicon Mac gets.

**Status: architecture proven, product port not started.** Prepared-state
restore works under WHPX with two small QEMU patches, measured on real
hardware. What remains is the WinQuick port itself.

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
state** into a fresh QEMU process — that is what turns a ~20 second boot into a
~300 ms command. Every other property is downstream of it.

**It works under WHPX, with two small QEMU patches.** Measured on Windows 11 Pro
25H2 (build 26200), Intel i5-8265U, Windows Hypervisor Platform enabled, QEMU
11.1.0, Validation OS x64 26100.8972:

> Windows booted under WHPX → stopped → migrated to a **153 MB file** → source
> process terminated → **the same immutable file restored into 20 fresh QEMU
> processes**, 20/20, p50 **1.99 s**, state hash unchanged, zero orphans.

That is fan-out reusable prepared state, not a one-way transfer.

### Why stock QEMU refuses

QEMU's WHPX backend registers an unconditional migration blocker at vCPU init.
It is not a capability probe — it is a hardcoded string, and it refuses every
form of state save including `savevm`. Its two claims stand differently today:

| Claim | Status |
|---|---|
| "missing dirty memory tracking support" | **True of QEMU**, which has no MemoryListener for WHPX at all. **Not true of the platform**: `WHvQueryGpaRangeDirtyBitmap` works — measured returning `S_OK` with pages correctly marked. |
| "some system register/state save-restore" | **Stale.** `whpx_get_registers()`/`whpx_set_registers()` already carry XSAVE through `WHvGet/SetVirtualProcessorXsaveState`. |

And dirty tracking is not needed for what WinQuick does. `migration/ram.c`
starts with every page marked dirty —

```c
/*
 * The initial dirty bitmap for migration must be set with all
 * ones to make sure we'll migrate every guest RAM page to
 * destination.
 */
```

— so the first pass copies all of RAM, and a *stopped* guest cannot dirty a
page afterwards. Dirty logging is what makes the *iterative* phase converge
while the guest still runs. Stop-and-copy needs none of it.

One practical consequence: without dirty logging, QEMU's iterative loop never
decides it can finish, and re-sends RAM forever (measured: 11 GB transferred
for a 1 GB guest, still `active`). Because the guest is stopped and downtime is
meaningless, setting `downtime-limit` high makes it converge on the first pass —
`completed` in ~2.5 s.

### The Windows transport is a separate, real bug

`qio_channel_file_set_blocking()` is a `/* not implemented */` stub on Win32
that always fails, so `migrate file:` and `-incoming file:` cannot work there
**regardless of accelerator** — confirmed under TCG too. The same migration over
`tcp:` succeeds. Both are addressed in
[`patches/`](../patches/whpx-stop-and-copy.patch).

### Native partition migration is not the answer

`WHvStartPartitionMigration` / `Accept` / `Complete` all exist and succeed, but
they are **consumptive, not reusable**: a second `WHvAcceptPartitionMigration`
on the same handle returns `0x80070006` (`E_HANDLE`). It transfers a partition
once; it does not clone one many times. WinQuick needs fan-out, so this API is
the wrong primitive — the low-level state APIs are the right one.

That was established with a standalone WHP program before touching QEMU: a
tiny real-mode guest captured to plain bytes (registers, XSAVE, RAM) and
restored into **20 fresh processes** from the same file, 20/20, p50 52 ms,
hash unchanged.

### The guest path works: a real command ran

With the patched QEMU, a prepared x64 Validation OS and WinQuick's own agent and
mailbox, the guest executed a real command:

```
WQOUT.TXT   Microsoft Windows [Version 10.0.26100.8972]
WQCODE.TXT  0 wqtoken-milestone7
WQERR.TXT   (empty)
```

Windows x64, hardware-accelerated under WHPX, driven entirely through the
existing mailbox protocol — the same `guest/agent.cmd` macOS uses, unmodified.
The agent is a batch file, so nothing about it needed an x64 build. Round trip
from cold boot to exit code: 18.4 s (this is a cold boot, not a restore).

The *host* side of that is still lab scripting rather than `winquick.exe`: the
Rust backend now knows the right QEMU, accelerator, machine, CPU and firmware,
but `setup` and `run` are not yet wired to drive it.

### The CPU model is not a free choice

`qemu64` looked like the safe default and is wrong. It is a Pentium 4-era model
without SSE4.2 or POPCNT, both of which Windows 11 requires. The symptom is
quiet and misleading: firmware runs, the kernel starts and reaches a kernel
address, and then nothing — an unchanging RIP, no display (Validation OS has no
graphics driver), and no first `cmd.exe`.

It was isolated by making the guest agent drop a marker on C: before doing
anything else, then reading the disk back offline:

| `-cpu` | Windows userland reached |
|---|---|
| `qemu64` | **no** |
| `Nehalem` | yes |
| `Skylake-Client` | yes |

`Nehalem` is now pinned: the oldest model carrying SSE4.2 and POPCNT, so the
least demanding thing a Windows 11 guest actually boots on, and available on any
x86_64 host from about 2008. It is part of the prepared-state fingerprint,
because a state carries the CPUID it was made with.

### Preparing the base image on Windows is blocked

WinQuick injects its agent by writing two things into the guest image: the batch
file, and an `AutoRun` value in the SOFTWARE hive. On macOS that is done without
mounting anything — `hdiutil -nomount` exposes the partition and `ntfscp` and
`hivexsh` write into it.

The Windows equivalent would be to attach the VHDX and copy the files. That does
not work on the validation host:

- `Mount-DiskImage` fails with a CIM `PermissionDenied`, even from a genuinely
  elevated context (verified: `elevated: True`).
- `diskpart` `attach vdisk` selects the disk and then fails with *Access denied*.
- The Virtual Disk Service was started first, and `vhdmp.sys` is present, so
  neither is the cause. **Bitdefender Endpoint Security Tools** is installed
  alongside Defender, and blocking disk-image attach is standard anti-evasion
  behaviour for managed endpoints.

For this work the image was prepared on the Mac instead and copied over, which
proved the guest path. For the product the better answer is not to mount at all:
build the same `ntfsprogs` helpers WinQuick already ships for macOS, and write
into the image directly. That needs no elevation, no VHD driver and no argument
with the endpoint protection — and it is the same code path on both hosts.

### Windows holds its disk images exclusively

QEMU on Windows opens a disk image such that an ordinary read from the host
fails with *the process cannot access the file because it is being used by
another process*. Polling the mailbox while the guest runs needs an explicit
`FileShare.ReadWrite` open. Worth knowing before the run loop is written.

### Other findings

- **`-cpu host` and `-cpu max` crash OVMF under WHPX**, in `PlatformPei`.
  Concrete models (`qemu64`, `Skylake-Client`, `Nehalem`) boot. WinQuick uses
  `-cpu host` on macOS, so a Windows backend must pin a concrete model — which
  is fine, and the prepared state's fingerprint must include it.

### What this does not yet prove

Restore is **~2 s**, not the ~300 ms macOS delivers. Much of that is streaming
153 MB through a relay rather than reading the file directly, so there is real
headroom, but it is not yet a WinQuick-class number. And no WinQuick command has
run through a restored x64 guest: that needs the agent, the x64 capability
payloads and an x64 guest bridge, none of which exist yet.

So the architecture is proven and the product port is not started.

## What the code audit found, and what was fixed

The audit found **16 compile errors across 6 files** for
`x86_64-pc-windows-msvc`, every one in the host seam rather than the product
logic. Since that work stands whichever backend a Windows port eventually uses,
it was done: **`cargo check --target x86_64-pc-windows-msvc` now passes with no
errors**, and macOS is unaffected.

| Was | Now |
|---|---|
| `std::os::unix` imports in 6 files | `src/hostfs.rs` |
| `MetadataExt::blocks` for allocated size | `hostfs::allocated` — block count on Unix, length on Windows |
| `MetadataExt::mtime`/`ino` for image identity | `hostfs::identity` — length plus mtime, portable; the inode is gone |
| `flock` via a raw fd | `hostfs::try_lock` / `open_lock_file` — flock on Unix, exclusive share mode on Windows |
| `dup`/`dup2` for the MCP stdout guarantee | the same technique through the Windows CRT's `_dup`/`_dup2` |
| `UnixStream` for QMP | `hostfs::ControlStream` — Unix socket on macOS, TCP on Windows |

What is *not* done, because it depends on a backend that has not been chosen:
the accelerator and machine selection, QEMU executable and firmware discovery,
Windows process containment, and architecture-aware capability payloads. Those
are listed below.

Other host assumptions the audit catalogued and which remain macOS-shaped:
`hvf` as the accelerator, `qemu-system-aarch64` as the binary name, Homebrew
paths for dependency discovery, `hdiutil` for mounting Microsoft media, Unix
signals for process cleanup, and `win-arm64` as the capability RID.

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

## Where this stands

The gate was: prove the restore mechanism before porting. It is proven, so the
port is now worth doing — and it is the next phase, not this one.

What was worth doing regardless of the backend has been done — the shared core
now compiles for `x86_64-pc-windows-msvc` with no errors, and the platform seam
is isolated in `src/hostfs.rs`. Whichever backend a Windows port eventually
uses, that work stands.

Apple Silicon macOS remains the only supported host until that port lands.
