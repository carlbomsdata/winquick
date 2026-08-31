# Running WinQuick *on* Windows — where this stands

WinQuick's guest has always been Windows. This is about the other half: making
Windows a **host**, so a developer on Windows x86_64 gets the same product a
developer on an Apple Silicon Mac gets.

**Status: the product runs on Windows.** `winquick setup` builds a runtime and
`winquick run` executes a command in it, both through `winquick.exe`, with no
elevation, no mounted images and no exception asked of the endpoint security
software.

**The fast path now works there with more than one processor.** Getting that
far meant a bug in WinQuick itself — it was freezing the guest half a step too
early, mid-dismount ([mailbox-freeze.md](mailbox-freeze.md)) — and then three
separate pieces of per-processor state that WHP owns and QEMU does not carry
across a migration: the activity state that leaves an application processor
parked in `StartupSuspend`, and the Hyper-V hypercall page. Each has its own
patch in [`patches/`](../patches/). One failure mode is still open -- some
restored guests halt and are never woken -- and WinQuick builds another prepared
guest rather than giving up on the warm path. The investigation is
[whpx-resume.md](whpx-resume.md).

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

**It works under WHPX, with seven QEMU patches.** Measured on Windows 11 Pro
25H2 (build 26200), Intel i5-8265U, Windows Hypervisor Platform enabled, QEMU
11.1.0, Validation OS x64 26100.8972:

> Windows booted under WHPX → stopped → migrated to a **153 MB file** → source
> process terminated → **the same immutable file restored into 20 fresh QEMU
> processes**, 20/20, p50 **1.99 s**, state hash unchanged, zero orphans.

That is fan-out reusable prepared state, not a one-way transfer — as far as the
file and the loader are concerned. Whether the *guest* resumes executing
afterwards is a separate question, and the answer turned out to be no at first;
see [below](#the-prepared-guest-restores-and-then-does-nothing). It is yes now,
at one and two processors -- and no at four, for a reason that is a property of
the platform rather than a bug left to fix; see
[Why four processors are not supported](#why-four-processors-are-not-supported).

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

### The prepared guest restores — what it took

This is the one that did not work for a long time, and it is worth stating
precisely, because the earlier measurements were all true and all insufficient.

With the patched QEMU, WinQuick builds a prepared state on Windows exactly as it
does on macOS: the guest boots, announces itself, is stopped, and is migrated to
a file. That takes **25-45 s** and produces a **~350 MiB** state. A later run
starts a fresh QEMU with `-incoming`, the load completes, `cont` succeeds, and
QEMU reports a running guest.

What the early work proved was that the *state file* survives a round trip and
can be loaded many times over — 20 fresh processes, hash unchanged. That is a
statement about the file, and about QEMU accepting it. It is not a statement
about the guest resuming execution, and this is exactly where the two part
company: the guest then did nothing at all.

Two pieces of per-processor state turned out to be missing, one hidden behind
the other. WHP keeps state for a virtual processor that is not a register, and
none of it is in QEMU's `whpx_register_names`:

| Missing state | What it looked like |
|---|---|
| `InternalActivityState` | every application processor parked in `StartupSuspend` for ever |
| the Hyper-V **hypercall page** | the guest bugchecks `0xD1` about three seconds in |

Each has its own patch and its own evidence; the full account, including the
crash dump that named the second one, is in
[whpx-resume.md](whpx-resume.md). A third failure remains open there: some
restored guests halt and are never woken. Migrating the synthetic interrupt
controller was the obvious candidate, and the measurement refuted it.

There is a fourth thing, and it is WinQuick's rather than QEMU's: *where* a
prepared guest gets frozen is partly luck, because the agent's poll loop never
goes quiet. A guest caught in the wrong part of it comes back unusable. That is
a property of the state, not of the machine, so WinQuick builds another one
rather than concluding the host cannot restore.

WinQuick still handles a host that genuinely cannot restore, rather than
repeating the discovery. After three silent prepared guests in a row it writes
`~/.winquick/restore-unsupported`, keyed on the accelerator and on the QEMU
binary's own identity, and later runs skip the warm path instead of rebuilding a
prepared guest, restoring it, waiting and giving up — three boots to run one
command. Install a QEMU that can restore and the note stops matching, on its
own; `winquick clean` forgets it outright, and `winquick doctor` says when it is
in effect.

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
| `winquick run -- cmd /c ver`, cold | 14.6-18.2 s, repeatable |
| orphaned QEMU processes | none |

And with the warm path working, `winquick run --cpus N -- cmd /c ver` twenty
times from one prepared guest:

| processors | result | min | p50 | mean | p95 | max |
|---|---|---|---|---|---|---|
| 1 | **20 warm of 20** | 13.3 s | 18.4 s | 18.7 s | 25.3 s | 26.0 s |
| 2 | **20 warm of 20** | 13.9 s | 24.8 s | 22.7 s | 28.2 s | 29.2 s |
| 4 | **0 warm of 20** — every prepared guest came back halted | | | | | |

Right output and right exit code at every processor count; prepared state and
canonical image byte-identical afterwards; no QEMU left behind. The restore
itself is 92-180 ms and the guest answers in about 520 ms — what dominates the
roundtrip is copying the two-gigabyte workspace and artifact volumes per run,
which Windows has no APFS-style clone for.

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

- **Capabilities install but break the fast path.** PowerShell 7.6.5 now
  installs on Windows and runs correctly on a cold-booted guest. It does not
  work on a restored one, and neither does anything else once a capability
  volume is attached. See
  [Capabilities do not survive a prepared state](#capabilities-do-not-survive-a-prepared-state-and-that-is-the-real-limit).
  Getting the install itself to work needed one fix: digest verification shelled
  out to `/usr/bin/shasum`, which does not exist on Windows.
- **A restored guest resumes at one and two processors, and not at four.**
  That is now a supported limit rather than an open gap: `platform::MAX_PREPARED_CPUS`
  is `Some(2)` on Windows, the Windows default is two processors, and asking for
  more on the fast path is refused with an error that names both ways out. See
  [Why four processors are not supported](#why-four-processors-are-not-supported)
  below.
- **The desktop** needs x64 drivers. It no longer needs a separate `wqui.exe`
  source: the bridge's runtime identifier follows `platform::GUEST_ARCH`, so an
  x64 host publishes a `win-x64` bridge from the same project. That is a claim
  about the code; it has not been built on Windows yet.
- **Packaging.** A Windows user should not assemble a QEMU directory by hand,
  and the fast path additionally needs a patched QEMU.

## Why four processors are not supported

Windows parks an idle processor by way of the Hyper-V enlightened idle path:
`nt!PoIdle` reaches `nt!PpmIdleGuestExecute`, which writes `HV_X64_MSR_GUEST_IDLE`
(`0x400000F0`) and leaves the processor waiting on a **synthetic timer** whose
expiry is an absolute point in the *partition's* reference-time domain.

Restoring a prepared state does not resume a partition; it builds a **new** one
and pours the old one's state into it. The new partition's reference time starts
where it likes. To keep those absolute expiries meaningful, the restore would
have to read the source partition's reference count and synthetic timer state
and rebase both -- and the public Windows Hypervisor Platform API exposes
neither:

- there is no reference-count register in `WHvRegister*`;
- there is no `Stimer*` register either; the synthetic timers come back only
  inside an opaque `WHvGetVirtualProcessorState` blob with no time base in it;
- the reference TSC page is a hypervisor-owned overlay, and reading it back from
  the source partition returns `seq=0 scale=0 offset=0`.

With one or two processors this does not matter, because the guest always has a
processor still executing that reaches its own clock code and reprograms the
timers itself. Beyond that there is nobody left to do it. Measured at four
processors on the build below: five prepared states in a row unusable, then
roughly one usable guest in five thereafter.

This is a limit of *reconstructing a partition from saved state*, not a limit of
WHP. A cold-booted WHPX guest runs four processors perfectly well, which is why
`--cold` is the documented escape rather than a silent fallback:

    winquick run --cpus 2 -- <command>          # fast, supported
    winquick run --cold --cpus 4 -- <command>   # any count, boots from scratch

The restriction is a Windows fact and only a Windows fact.
`platform::MAX_PREPARED_CPUS` is `None` on every other host, macOS/HVF still
defaults to four processors, and a unit test asserts a non-Windows host inherits
no limit, so a Linux/KVM port cannot pick this up by accident.

## Measured gates

Windows 11 Pro 25H2 (build 26200), Intel i5-8265U, Windows Hypervisor Platform
enabled, QEMU 11.1.0 with the seven patches in [`../patches/`](../patches/).

| | 1 vCPU | 2 vCPU | 4 vCPU |
|---|---|---|---|
| warm runs | 20/20 | 20/20, then 99/100 | 0/20 |
| min | 1,660 ms | 1,249 ms | -- |
| p50 | 1,855 ms | 1,443 ms | -- |
| mean | 1,840 ms | 2,678 ms | -- |
| p95 | 1,947 ms | 1,645 ms | -- |
| max | 2,303 ms | 68,751 ms | -- |

Exit codes propagate exactly (0, 42 and 255 each arrive unchanged). The
canonical image hashes identically before and after every soak. No QEMU process
and no run directory is left behind.

The 2-vCPU maximum is a restore that took the slow path once in a hundred; the
run still produced the right answer, which is why it is counted as a success and
reported rather than hidden.

## The first timer wait after a restore costs about four minutes

This is the blocker, and it is separate from the capability bug below.

A restored guest executes at full speed but cannot wait. Measured at 1 vCPU,
otherwise identical runs:

| command | what it waits for | warm | cold |
|---|---|---|---|
| `cmd /c echo` | nothing | 2-5 s | ~20 s |
| `ping -n 1` (one packet) | nothing | 3 s | 22 s |
| `ipconfig` | nothing | 2 s | -- |
| `ping -n 2` | 1 second | **252 s** | 25 s |
| `ping -n 3` | 2 seconds | **230 s** | -- |
| `ping -n 6` | 5 seconds | **240 s** | 26 s |

The cost is not proportional to the wait, and it is not paid twice: one
1-second sleep took 259 s and *four* consecutive 1-second sleeps in the same
run took 237 s. So it is a single stall of roughly four minutes, paid the first
time the restored guest waits on a timer, after which timers behave normally.

It is not the network stack -- `ping -n 1` and `ipconfig` exercise it fully and
return in about two seconds. It is not the CPU, which runs at full speed. It is
waiting, and only waiting.

That makes the prepared state worthless for real work on Windows. `pwsh`,
MSBuild, `dotnet test` and every ordinary build wait constantly, so they pay
the stall and a warm run ends up no faster than a cold one -- when it finishes
inside the timeout at all.

**What it is not.** Windows here does not use the local APIC in TSC-deadline
mode: instrumenting `IA32_TSC_DEADLINE` at the freeze showed `deadline=0` on
every save, so migrating that register -- which QEMU's WHPX backend does not do
-- changes nothing, and an experiment adding it confirmed that. The TSC itself
migrates accurately: 44,996,906,340 at save against 44,994,889,321 at restore,
about a millisecond apart.

That leaves the Hyper-V synthetic timers, which is where the four-processor
limitation already lives. The two are the same defect seen from different
angles: a restored partition cannot reconstruct timer state whose expiry is
absolute in the source partition's reference-time domain. At four processors
every processor parks on one immediately and nothing recovers. At one or two a
processor is still running, so a command that never waits finishes in two
seconds and nothing looks wrong -- which is exactly why 300 soak runs of
`cmd /c echo` passed while the product remained unusable for real workloads.

**A measurement trap worth recording.** WinQuick writes a `restore-unsupported`
note when a warm attempt fails, and thereafter boots cold. The note is keyed on
the QEMU binary's identity, and `restore-works` is too -- so rebuilding QEMU
produces a fresh identity that the existing "this works" note does not cover,
and one failed attempt silently disables the fast path for that build. A whole
round of measurements was taken through that fallback and read as fast warm
runs; they were cold runs. Check the note, or run with `--verbose` and read the
`warm run` / `cold run` line, before believing any timing on this host.

## Capabilities do not survive a prepared state, and that is the real limit

This is the first time PowerShell has actually been run on a Windows host, and
it does not work on the fast path. Measured, 2026-08-30, on the build below:

| | warm (restored) | cold |
|---|---|---|
| `pwsh -c "Write-Output HELLO"`, 1 vCPU | 3/3 hang, >300 s each | 2/2 pass, ~25 s |
| `pwsh -c "Write-Output HELLO"`, 2 vCPU | hangs, >1200 s | passes, ~20 s |

The cause is not PowerShell, and not the number of processors. Once the
PowerShell capability was installed, **trivial** warm commands began failing
too, at 1 vCPU, in exactly the configuration that had just run 100/100:

    cmd /c "echo NOSLEEP-OK"   1 vCPU warm, capability installed:  2/2 timed out
    cmd /c "echo NOCAP-OK"     1 vCPU warm, capability removed:    5/5 pass, ~2 s

So the finding is about the prepared state, not the command: **a prepared state
built with a capability volume attached does not restore into a working guest
on Windows.** Removing the capability and letting the state rebuild restores the
2-second warm run immediately.

What it is not: the sparse clone. `cargo test` passes 156/156 on the Windows
host, including `a_cloned_volume_is_byte_for_byte_its_source` -- and that test
matters here because the Windows clone path is a different implementation from
the macOS one and had only ever been exercised on macOS.

The likeliest mechanism, unproven, is the FAT-sharing rule this project already
knows about. A capability volume is FAT and the guest holds it mounted across
the freeze, so the prepared state carries the guest's cached FAT metadata for a
volume whose on-disk image is then replaced, per run, by a freshly cloned copy
that does not contain whatever the guest still had in flight. The mailbox avoids
exactly this by never being touched by both sides at once. Testing that would
mean dismounting capability volumes before the freeze, the way the mailbox does.

**This one is fixed.** The mechanism was the mount, not the bytes and not the
device. Snapshotting the capability volume at the freeze so the run's clone is
byte-identical to what the guest cached did *not* help -- still 5/5 hangs --
which ruled out content. What fixed it was making the guest let go: the agent
now dismounts capability volumes before signalling ready and remounts them
before the command runs, exactly as the mailbox, workspace and artifact volumes
have always done. The package cache had the identical bug and got the same
treatment.

After the fix, with PowerShell installed, `cmd /c echo` runs 5/5 in about two
seconds at 1 vCPU, and the guest can read the capability volume (`PWSH-PRESENT`,
`dir G:\` lists `pwsh`). What it still cannot do is wait -- see the section
above, which is the remaining blocker.

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

## The same test on Linux/KVM

The timer defect above is specific to WHPX. Measured 2026-08-31 on
Intel x86_64 -> VMware Workstation 17.6.4 (nested VT-x) -> Ubuntu 24.04.4
x86_64 -> KVM -> QEMU 11.1.0 -> Validation OS x64, every run verified warm
from what WinQuick reported rather than from its latency:

| command | waits | WHPX | KVM |
|---|---|---|---|
| `cmd /c echo` | nothing | 2-5 s | 8.2 s |
| `ping -n 1` | nothing | 3 s | 8.0 s |
| `ping -n 2` | 1 second | **252 / 430 / 644 s** | **9.1 s** |
| `ping -n 6` | 5 seconds | 240 s | 11.8 s |
| four 1-second waits | 4 seconds | 237 s (same as one) | 12.6 s |

On KVM the cost tracks the wait: one second costs about one second, five cost
about four and a half, and four waits cost more than one instead of the same.
That is the behaviour WHPX cannot produce, and it is the difference between a
prepared state being useful for real work and not.

The absolute figures are slow because the lab is nested -- KVM inside VMware
inside Windows on a 2018 i5-8265U -- not because of anything WinQuick does.
