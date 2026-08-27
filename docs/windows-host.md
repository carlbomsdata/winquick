# Running WinQuick *on* Windows — where this stands

WinQuick's guest has always been Windows. This is about the other half: making
Windows a **host**, so a developer on Windows x86_64 gets the same product a
developer on an Apple Silicon Mac gets.

**Status: the product runs on Windows.** `winquick setup` builds a runtime and
`winquick run` executes a command in it, both through `winquick.exe`, with no
elevation, no mounted images and no exception asked of the endpoint security
software.

It is not yet *fast* there. Every run is a cold boot at **~16.5 s**, because a
restored guest under WHPX resumes and then never executes — the one thing that
did not work, written up [below](#the-prepared-guest-restores-and-then-does-nothing).
macOS keeps its ~300 ms because restoring works there.

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

That is fan-out reusable prepared state, not a one-way transfer — as far as the
file and the loader are concerned. Whether the *guest* resumes executing
afterwards is a separate question, and the answer turned out to be no; see
[below](#the-prepared-guest-restores-and-then-does-nothing).

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

That was lab scripting. The same command now runs through `winquick.exe` itself:

```console
> winquick setup --from C:\media\ValidationOS.vhdx
  [1/4] expanding the image
  [2/4] installing the WinQuick agent
  [3/4] configuring the guest
  [4/4] packing the runtime
Windows runtime installed (652 MiB).

> winquick run -- cmd /c ver
Microsoft Windows [Version 10.0.26100.8972]
```

Setup takes 57 s and a cold run 16.5 s, repeatably, with no orphaned QEMU
processes left behind.

### The prepared guest restores, and then does nothing

This is the one that did not work, and it is worth stating precisely because
the earlier measurements are all still true and all still insufficient.

With the patched QEMU, WinQuick builds a prepared state on Windows exactly as it
does on macOS: the guest boots, announces itself, is stopped, and is migrated to
a file. That takes **25-32 s** and produces a **~341 MiB** state. A later run
starts a fresh QEMU with `-incoming`, the load completes, `cont` succeeds, and
QEMU reports a running guest.

The guest then does nothing at all. It never reads the command, never writes
`WQOUT.TXT`, never writes an exit code; the overlay does not grow by a single
byte. Waiting longer does not help.

What the earlier work proved was that the *state file* survives a round trip and
can be loaded many times over — 20 fresh processes, hash unchanged. That is a
statement about the file and about QEMU accepting it. It is not a statement
about the guest resuming execution, and this is where the two part company.
**This has since been investigated properly**, and the boundary turned out to be
sharp: a **one-processor** guest restores and executes; **two or more** and
every vCPU halts within fifty milliseconds and never runs again, with every
`WHvRunVirtualProcessor` return being `Canceled`. A minimal hand-written guest
restores fine, and `stop`/`cont` in the same process with four processors is
fine, so neither the migration stream nor the run-state transition is at fault.
Registers and the whole LAPIC register page come back byte-identical.

The full write-up, with what was ruled out and how, is in
[whpx-resume.md](whpx-resume.md).

**So on Windows every run is a cold boot: ~16.5 s.** That is the honest number,
and it is what the product does today.

WinQuick handles this rather than repeating it. The first run that hits it
writes `~/.winquick/restore-unsupported`, keyed on the QEMU and the
accelerator, and later runs skip the warm path entirely instead of rebuilding a
prepared guest, restoring it, waiting and giving up — three boots to run one
command. A prepared guest that has never actually run a command also gets a
short budget rather than the user's whole timeout, because there is nothing to
wait for. Install a QEMU that can restore and the note stops matching, on its
own; `winquick doctor` says when it is in effect.

The note is written **only** when the guest resumed and then said nothing.
That distinction is load-bearing, and it was learned the hard way: an earlier
version recorded it on any warm-path failure, and a QEMU killed by hand during
testing was enough to disable the fast path permanently on a Mac where it works
perfectly. QEMU dying, a disk error or a Ctrl-C are accidents of one run; a
silent guest is the only one that says anything about the host.

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

### Preparing the base image needs no mounting at all

WinQuick injects its agent by writing two things into the guest image: the batch
file, and an `AutoRun` value in the SOFTWARE hive. On macOS that was done by
attaching the image with `hdiutil -nomount` and pointing `ntfscp` and `hivexsh`
at the partition node.

The Windows equivalent would be to attach the VHDX and copy the files. That does
not work on the validation host:

- `Mount-DiskImage` fails with a CIM `PermissionDenied`, even from a genuinely
  elevated context (verified: `elevated: True`).
- `diskpart` `attach vdisk` selects the disk and then fails with *Access denied*.
- The Virtual Disk Service was started first, and `vhdmp.sys` is present, so
  neither is the cause. **Bitdefender Endpoint Security Tools** is installed
  alongside Defender, and blocking disk-image attach is standard anti-evasion
  behaviour for managed endpoints.

**So WinQuick stopped attaching images.** Every access the ntfs helpers make is
a seek or a positioned read/write, so an `NTFS_IMAGE_OFFSET` environment
variable shifts them by the byte offset of the partition, and "image plus
offset" behaves exactly like a partition device node did. WinQuick finds that
offset by reading the GPT itself (`src/gpt.rs`) and picking the largest basic
data partition.

This is now the **only** path on either host: macOS no longer attaches anything
either, and `hdiutil attach -nomount`, its detach handling and its
stale-attachment recovery are gone. One code path, no privileges, nothing
touched outside the image file.

The helpers are native Windows builds of the same programs macOS uses, from the
same upstream tarball and the same patch — see
[`patches/ntfsprogs-windows.patch`](../patches/ntfsprogs-windows.patch) and
[`scripts/build-ntfs-helpers.sh`](../scripts/build-ntfs-helpers.sh). MSYS2 is a
build-time dependency only; the results link nothing but `KERNEL32` and
`msvcrt`.

Measured: a 6 MB `SOFTWARE` hive read out of a 32 GB image, edited, written back
and read again **byte-identical over two cycles**, in 153 ms.

`hivexsh` had to be built too, because no Windows package provides it and
upstream excludes it from Windows builds — it composes its interactive prompt
with `open_memstream`, which mingw lacks. WinQuick's build uses a fixed prompt
and drives it from a script file, where no prompt is printed.
See [`patches/hivex-windows.patch`](../patches/hivex-windows.patch).

### Two Windows bugs worth knowing, both the same bug

Neither is subtle in hindsight and both cost real time.

**`__attribute__` is defined away.** `ntfs-3g`'s `compat.h` neuters
`__attribute__` on Windows, for MSVC's benefit. That also discards
`__attribute__((packed))` on every on-disk structure: `NTFS_BOOT_SECTOR` grows
from 512 to 520 bytes, `oem_id` lands at the wrong offset, and a perfectly good
NTFS volume reports *"NTFS signature is missing"*.

**Text mode eats binary data.** The C runtime rewrites `0x0A` on the way out and
treats `0x1A` as end of file. A registry hive read through `ntfscat`'s stdout
grew by exactly its own newline count on every cycle — 6 029 312 bytes became
6 040 930, then 6 052 548 — and `ntfscp` reading its source in text mode did the
same in reverse. This is the third time the same class of bug has appeared in
this port; the QEMU migration stream was the first
(see [`patches/README.md`](../patches/README.md)).

A trap that follows from it: `#ifdef O_BINARY` compiles to *nothing* if
`<fcntl.h>` was not included, so the guard silently does the wrong thing rather
than failing to build.

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

### One trap that is not WinQuick's fault

Driving `winquick.exe` from an **MSYS2 or Cygwin shell** silently corrupts the
command. Those runtimes rewrite arguments that look like POSIX paths before the
program ever sees them, so

```console
$ winquick run -- cmd /c ver
```

arrives as `cmd C:/ ver`. That starts an *interactive* `cmd.exe` in the guest,
which never exits, and the run times out with no useful explanation. The
captured output ends mid-prompt at `C:\workspace>`, which is the tell.

It cost an afternoon to find, because every symptom pointed inward: the guest
booted, the agent announced itself, the command file was written, output files
appeared, and nothing came back. Set `MSYS2_ARG_CONV_EXCL='*'`, or use
`cmd.exe` or PowerShell. Nothing on WinQuick's side can detect this -- by the
time `main` runs, the argument is already gone.

### What is verified there

Measured on Windows 11 Pro 25H2 (build 26200), i5-8265U, QEMU 11.1.0,
Validation OS x64 26100.8972.

| | |
|---|---|
| `winquick setup` from a VHDX | 57 s |
| `winquick setup` from the ISO | + ~0.5 s to read the VHDX out of it |
| `winquick run -- cmd /c ver` | 14.6-18.2 s, repeatable |
| first run after a fresh setup | 112 s — it builds a prepared guest, finds it will not resume, records that, and boots cold |
| orphaned QEMU processes | none |

The **unit suite runs natively there too: 127 tests, all passing**, built with
the `x86_64-pc-windows-gnu` toolchain. Getting it to run turned up a real bug:
every GPT test failed, because giving a copied disk a fresh identity read
`/dev/urandom`, so the servicing path could not have worked on Windows at all.
It now goes through `hostfs::fill_random`, which uses `ProcessPrng` there. Two
of the tests are new and exist because their failure modes are silent — that
the Job Object limit structures are the size `winnt.h` says (a mismatch makes
`SetInformationJobObject` fail and quietly lose containment), and that only a
guest that resumed and said nothing is treated as evidence about the host.

The behaviour suite covers doctor, stdout and stderr separation, exit-code
propagation, the workspace, artifact retrieval, disposability and containment:
**14 checks, all passing**. `winquick mcp` passes the protocol suite in
[`tests/mcp.py`](../tests/mcp.py) — **72 checks, all passing**, including the
Windows-touching workspace and artifact tools.

Two harness fixes were needed for that, and neither was a product bug: the
suite looked for a runtime under `validation-arm64` only, and it built its
temporary workspaces with MSYS2 paths that a native `winquick.exe` cannot
resolve. Both now handle either host.

Capabilities — PowerShell, the .NET runtime and SDK — resolve `win-x64`
payloads and unpack with the `tar` Windows ships, but have not been run there
yet. That is a claim about the code, not a measurement.

### Reading Microsoft's ISO

The media is a **UDF bridge disc**: its ISO 9660 filesystem holds a single
`README.TXT` saying so, and `ValidationOS.vhdx` lives in the UDF filesystem
beside it. Mounting that needs `hdiutil` on macOS and `Mount-DiskImage` on
Windows, and the second is exactly what this port refuses to depend on.

[`src/udf.rs`](../src/udf.rs) is the smallest reader that gets the file out:
anchor, volume descriptors, file set, root directory, one file's extents. It
takes **0.48 s for 1 GB** and is now the path on both hosts, so `hdiutil` is
gone from `setup` entirely. It implements no more UDF than that, and says so
rather than guessing when it meets something else.

### What is not done yet

- **Capabilities are wired but untested here.** PowerShell and the .NET runtime
  and SDK now resolve `win-x64` payloads, with digests verified against the
  publishers', and archives are unpacked with the `tar` Windows has shipped
  since 10 1803 rather than the `unzip` it has not. None of that has been run
  on Windows yet, so it is a claim about the code, not a measurement.
- **A restored guest does not resume**, so every run is a cold boot. This is the
  one real gap, and it is above.
- **The desktop** needs an x64 `wqui.exe` and x64 drivers.
- **Packaging.** A Windows user should not assemble a QEMU directory by hand,
  and the fast path additionally needs a patched QEMU.

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

The gate was: prove the restore mechanism, then port. Both are done. `setup` and
`run` work on Windows x86_64 through the product's own binary, using the same
guest protocol, the same agent and the same image-preparation code as macOS.

The two hosts now differ in exactly the places
[`src/platform.rs`](../src/platform.rs) names -- QEMU binary, accelerator,
machine, CPU model, firmware, guest architecture -- plus the handful of
filesystem and process spellings in [`src/hostfs.rs`](../src/hostfs.rs) and
[`src/proc.rs`](../src/proc.rs). Everything above that line is one product.

What is left is breadth — the list is above — and one piece of depth: making a
restored guest actually resume under WHPX, which is what stands between a
16.5 second cold boot and the sub-second run macOS gets. That one now has its
own document, [whpx-resume.md](whpx-resume.md), narrowed to multiprocessor
partitions and to two remaining candidates.
