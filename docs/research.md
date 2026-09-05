# Research log

Everything below was measured on real hardware. Where something is unproven or
untested it says so. No numbers here are estimates.

**Test host**

| | |
|---|---|
| Machine | Apple M4 Pro, 24 GB RAM |
| OS | macOS 26.5.2 (build 25F84) |
| QEMU | 11.1.0 (Homebrew), accelerators: `hvf`, `tcg` |
| Firmware | `edk2-aarch64-code.fd`, edk2-stable202408, shipped with QEMU |
| Date | 2026-08-24 |

## Headline result

Microsoft Validation OS ARM64 boots headlessly under QEMU + HVF on Apple Silicon
and runs commands with faithful stdout, stderr and exit codes, in **8.4–9.2
seconds end to end**, from a **763 MiB** base image, in as little as **512 MiB**
of guest RAM.

```console
$ winquick run -- cmd /c ver

Microsoft Windows [Version 10.0.26100.8972]

$ echo $?
0
```

Validation OS is **not** a blocker for the first milestone. It is a real NT
kernel, it is small, and it needed no driver injection whatsoever to boot.

## Microsoft Validation OS

Obtained from Microsoft directly, accepting Microsoft's licence:
<https://aka.ms/DownloadValidationOS_arm64>

| | |
|---|---|
| File | `26100.8972.260722-1659.ge_release_svc_prod3_arm64fre_en-us_VALIDATIONOS.iso` |
| Size | 2,569,562,112 bytes (2.39 GiB) |
| SHA-256 | `904a81ccd80fd1a7046b1628b7fb7ecfa7ffc09880212eb339e5290e8b492599` |
| Build | 10.0.26100.8972 |

The ISO is not install media. It contains:

| Item | Size | Notes |
|---|---|---|
| `ValidationOS.vhdx` | 964 MiB | **A ready-to-boot GPT disk.** This is the useful one. |
| `ValidationOS.wim` | 261 MiB | LZX-compressed; index 1 `winvos_install`, index 2 `winvos_boot` |
| `cabs/` | — | Optional feature packages (`Common`, `Extra`, `Test`, …) |
| `GenImage/` | — | Microsoft's image customisation scripts |
| `SDK/`, `ValidationOSImageBuilder/` | — | |

The VHDX partition layout, as read on macOS:

```
 #  Start        Size       Type
 1  34           128.0 MiB  Microsoft reserved
 2  264192        38.0 MiB  EFI system partition   (FAT, bootaa64.efi + BCD)
 3  342016        16.0 MiB  Microsoft reserved
 4  374784        31.8 GiB  Basic data (NTFS, label CORESYSTEM)
```

32 GiB virtual, ~1 GiB actually occupied. It is a normal disk-resident Windows
install, not a RAM-booted WinPE image.

### Licence terms that shape the product

The Validation OS licence (obtained with the download) says, in §2(e), that you
may not "share, publish, distribute, or lease the software, provide the software
as a stand-alone offering for others to use, or transfer the software or this
agreement to any third party". §13 adds a five-year confidentiality obligation.

So WinQuick ships no Microsoft bits, and images generated from them never leave
the user's machine. This is not caution; it is the licence.

### What is actually inside

Measured by reading the NTFS partition directly:

- **538 files in `Windows\System32`.** For comparison, a Windows 11 install has
  tens of thousands.
- **145 registered services.**
- Present and useful: `cmd.exe`, `reg.exe`, `net.exe`, `shutdown.exe`,
  `mountvol.exe`, `sort.exe`.
- **Absent:** PowerShell, `.NET`, `drvload.exe`, `pnputil.exe`, `devcon.exe`,
  `wpeinit.exe`, `find.exe`, `more.exe`, `timeout.exe`, `where.exe`,
  `tasklist.exe`, `fsutil.exe`, `diskpart.exe`.
- **Absent: the user-mode PnP service (`PlugPlay`).** This one matters — see
  the virtio-serial section.
- Present: `Wdf01000` (KMDF), `stornvme` (boot start), `mountmgr`, `volmgr`,
  `partmgr`, `fastfat`, `exfat`, `pci`.
- The shell is launched from `SOFTWARE\Microsoft\Windows NT\CurrentVersion\
  Winlogon\Shell`, which is set to `cmd.exe`.

Most of the missing pieces are available as optional `.cab` packages on the ISO
(`Microsoft-WinVOS-PowerShell-Package`, `Microsoft-WinVOS-NetFx45-Package`,
`Microsoft-WinVOS-Driver-Support-Package`, `Microsoft-WinVOS-PnP-Package`, …),
and Microsoft's own `GenImage.cmd` applies them only on a Windows 11 host with
DISM. That read as "no macOS path" for a long time, and was the single biggest
open problem for anything beyond `cmd.exe` workloads.

**Solved.** WinQuick runs DISM *inside WinQuick*, against an offline copy of
its own image (`src/servicing.rs`), which needs no Windows host at all. That is
how the `desktop` and `dotnet-framework` capabilities are built. `/Online`
servicing of the running Validation OS still returns `0x80070032` and never
works; offline is the only route. See [desktop.md](desktop.md).

## Boot configuration that works

No driver injection, no BCD editing, no firmware tweaks. The stock VHDX converted
to qcow2 boots as-is:

```
qemu-system-aarch64
  -M virt -accel hvf -cpu host -smp 4 -m 2048
  -drive if=pflash,format=raw,readonly=on,file=edk2-aarch64-code.fd
  -drive if=pflash,format=raw,file=<per-run 64 MiB zeroed vars store>
  -drive if=none,id=root,file=<overlay>.qcow2,format=qcow2
  -device nvme,drive=root,serial=wqroot
  -drive if=none,id=mbox,file=mailbox.img,format=raw,cache=writethrough
  -device nvme,drive=mbox,serial=wqmbox
  -device ramfb -display none -vga none
  -rtc base=localtime -no-reboot
```

Findings behind those choices:

- **`-M virt` with default `highmem`** is fine on an M4 Pro with QEMU 11.1. The
  `highmem=off` workaround that older Windows-on-QEMU guides insist on was not
  needed, and neither were any SME-related `-cpu` overrides.
- **NVMe, not virtio-blk.** Validation OS has `stornvme` as a boot-start driver,
  so an NVMe root disk needs no third-party storage driver. virtio-blk would
  need `viostor` present before the guest can read its own boot volume.
- **UEFI only.** Confirmed from the firmware log: EDK2 finds the disk as
  `Boot0001 "UEFI QEMU NVMe Ctrl wqroot 1"` and hands off to the Windows
  bootloader. The ESP contains both `bootaa64.efi` and an x64 loader; the
  firmware logs `Image type X64 can't be loaded on AARCH64 UEFI system` while
  trying the wrong one, then proceeds correctly. **That message is harmless.**
- **`-display none -device ramfb`.** No window ever appears on the host, but the
  guest still has a display device. Windows boots to a `cmd.exe` console on it.
  During bring-up, QMP `screendump` against `ramfb` was the only way to see what
  the guest was doing; it is a debugging tool, not a product feature.
- **A fresh 64 MiB zeroed UEFI variable store per run.** Firmware state is
  disposable too.

Two harmless firmware log lines appear on every boot and can be ignored:
`ArmTrngLib could not be correctly initialized` and the `Tpm2SubmitCommand -
Tcg2 - Not Found` pair.

## Guest control channel

### QEMU Guest Agent: rejected

`qemu-ga` would have given us `guest-ping`, `guest-exec`, `guest-exec-status`,
captured output and `guest-file-*` for free. It is not usable here:

1. **There is no ARM64 build.** virtio-win 0.1.285 — the current release —
   ships exactly `guest-agent/qemu-ga-i386.msi` and
   `guest-agent/qemu-ga-x86_64.msi`. Nothing for ARM64. Upstream QEMU does not
   produce a Windows ARM64 `qemu-ga`.
2. **It is an MSI.** Validation OS has no Windows Installer service, so an MSI
   cannot be installed there even if an ARM64 one existed.

Reason 1 alone settles it for v0.1.

### virtio-serial: viable, but blocked on driver registration

Checked directly in virtio-win 0.1.285:

- `vioserial/w11/ARM64/vioser.sys` **exists** and is a genuine ARM64 PE
  (`IMAGE_FILE_MACHINE_ARM64`, `0xaa64`).
- Its catalogue is signed by **"Microsoft Windows Hardware Compatibility
  Publisher"**, chaining to Microsoft Root Certificate Authority 2010. That is a
  real WHQL/attestation signature — **no test-signing needed** on ARM64.
- KMDF (`Wdf01000`), which `vioser.sys` depends on, is present in Validation OS.

So the driver is fine. The problem is installing it:

- Validation OS has **no user-mode PnP service**, so INF-based installation
  cannot happen in the guest at all.
- It has **no `pnputil.exe` and no `drvload.exe`**, so there is no runtime
  driver-load path either.

The remaining route is to register the driver **offline**, from the host: create
`SYSTEM\ControlSet001\Services\vioser`, copy `vioser.sys` into the image, and add
a `CriticalDeviceDatabase` entry mapping `pci#ven_1af4&dev_1003` to that service,
so kernel-mode PnP binds it at boot without any user-mode involvement. That is
the standard technique for boot-critical drivers and there is no obvious reason
it should not work here. **It has not been attempted yet.**

### What v0.1 actually uses: a FAT mailbox disk

Given that virtio-serial needs offline driver surgery and a compiled guest agent
(Windows ARM64 cross-compilation from macOS is its own project), v0.1 uses a
transport that needs neither:

A second NVMe device backed by a 64 MiB MBR-partitioned FAT32 image.

- Host writes `WQMARK.TXT` (volume marker) and `WQCMD.CMD` (the command) before boot.
- The guest agent — a `cmd.exe` batch script, ~20 lines — finds the volume by
  probing drive letters for the marker, runs the command with stdout and stderr
  redirected to `WQOUT.TXT` and `WQERR.TXT`, writes the exit code to
  `WQCODE.TXT`, and calls `shutdown /s /t 0 /f`.
- Host waits for QEMU to exit, then reads the results.

Both halves need only inbox components: NVMe, FAT, `cmd.exe`, `shutdown.exe`.
Zero third-party drivers, zero compiled guest code.

The cost is real and worth stating plainly: **no streaming.** Output arrives when
the VM shuts down, and one VM boot runs one command. That is exactly the shape of
`winquick run`, so it is not a problem yet — but it is why virtio-serial remains
the target architecture rather than a nice-to-have.

The agent is hooked in via `SOFTWARE\Microsoft\Command Processor\AutoRun`, which
`cmd.exe` executes on start. `Winlogon\Shell` was tried first and is cleaner in
principle, but see the gotchas.

## Gotchas found the hard way

These cost the most time, in order.

**Windows will not mount a partitionless FAT volume on a fixed disk.** The
`fatfs` crate formats a bare "superfloppy" — a valid FAT32 filesystem starting at
sector 0 with an all-zero partition table area. Windows silently exposes no
volume for it, and the guest agent reports "mailbox volume not found". `mformat`
images worked, and bisecting the two boot sectors byte by byte showed the only
functional difference was that mtools writes an MBR partition entry at offset
0x1BE and fatfs writes zeros. Ruled out along the way, all individually
irrelevant: FAT16 vs FAT32, reserved sector count (8 vs 32), CHS geometry
(32/64 vs 63/16), OEM name, and FAT tail entries past the last valid cluster.
WinQuick now writes a real MBR with one type-`0x0C` partition at LBA 2048.

**`Winlogon\Shell` with arguments does not launch.** Setting it to
`cmd.exe /c C:\Windows\System32\wqagent.cmd` produces a black screen and no
shell at all. `Command Processor\AutoRun` works reliably and keeps the console
available, which also makes failures debuggable via `screendump`.

**`echo %RC%>file` is a stdin redirect.** `cmd.exe` parses the digit
immediately before `>` as a file handle, so with `RC=0` this becomes `echo 0>
file` — redirecting handle 0 and echoing nothing. It prints "ECHO is off." and
writes an empty file. Redirect first instead: `>file echo %RC%`.

**`call` lets the workload kill the agent.** Running the user's command with
`call` executes it in the agent's own `cmd.exe` context, so a command ending in
`exit` terminates the agent before it can record the exit code or shut down —
the VM then hangs until the host timeout. Using a child `cmd /c` isolates it and
still propagates `%errorlevel%` correctly.

**Child `cmd /c` echoes the batch line into stdout.** `@echo off` has to be
written into the generated command file itself, not merely inherited.

**Guest FAT writes are not visible to the host until shutdown.** Windows caches
them regardless of QEMU's `cache=` setting. Results must be read after QEMU
exits, not polled during the run.

## Things tried that did not work

**QMP `send-key` bootstrap.** The idea was to avoid host-side NTFS tooling
entirely by booting the stock image, typing `d:\s.cmd` into the console over QMP,
and letting Windows patch its own registry with `reg.exe`. Keystrokes had no
effect: the `-M virt` machine has no keyboard device unless `-device qemu-xhci
-device usb-kbd` is added, and whether Validation OS has USB HID support at all
depends on optional packages. Not pursued further, since the host-side path
already worked.

**WinPE-style RAM boot of `ValidationOS.wim`.** Attractive — a 261 MiB base
image, no NTFS anywhere, everything buildable with `wimlib` and `mtools`. Not
possible: `boot.sdi` is required for a ramdisk BCD entry and is present neither
on the ISO nor in the WIM. It ships with the Windows ADK, which is Windows-only
and not redistributable.

**Homebrew `ntfs-3g`.** The formula is marked Linux-only and refuses to install
on macOS, bottle or source. Building `ntfsprogs` from the upstream tarball works
(see below).

## Host-side image build

`winquick setup` transforms the user's VHDX into a base image with exactly two
writes: the agent script into `Windows\System32\wqagent.cmd`, and the `AutoRun`
value in the `SOFTWARE` registry hive.

Tools it shells out to, all running natively on macOS ARM64:

| Tool | Purpose | Availability |
|---|---|---|
| `qemu-img` | VHDX → raw → qcow2 | `brew install qemu` |
| `hivexsh` | edit the offline `SOFTWARE` hive | `brew install hivex` |
| `ntfscp` / `ntfscat` | read and write files in the NTFS partition | **must be built from source** |

> **Later.** `hdiutil` used to be in this table, for mounting the ISO and
> attaching the raw image. Neither is done any more: the helpers take a byte
> offset into the image (`NTFS_IMAGE_OFFSET`) and the ISO is read by
> `src/udf.rs`. Nothing is mounted on either host. See
> [windows-host.md](windows-host.md), which is where the reason came from.


The NTFS tooling is the rough edge. macOS 26 has no NTFS driver at all (not even
read-only), and Homebrew's `ntfs-3g` is Linux-only. `ntfsprogs` does build
cleanly from the upstream tarball, and needs no FUSE:

```console
curl -LO https://tuxera.com/opensource/ntfs-3g_ntfsprogs-2022.10.3.tgz
tar xzf ntfs-3g_ntfsprogs-2022.10.3.tgz && cd ntfs-3g_ntfsprogs-2022.10.3
./configure --disable-ntfs-3g --enable-ntfsprogs --disable-plugins \
            --without-uuid --without-hd
make
export WINQUICK_NTFSCP=$PWD/ntfsprogs/ntfscp
```

(`make install` fails in its install hook; the binaries in `ntfsprogs/` are fine.)

This is not acceptable long term. Options, roughly in order of preference:
vendor a prebuilt `ntfsprogs` alongside the QEMU runtime (GPLv2, same compliance
posture as QEMU); or implement the two writes directly — both are same-size
overwrites of existing files, which is a far smaller problem than a general NTFS
writer.

## Measurements

**End-to-end `winquick run -- cmd /c ver`, ten consecutive runs**

```
8.48  8.46  8.42  9.17  8.51  8.75  8.60  8.89  8.48  8.44   (seconds)
```

Min 8.42 s, max 9.17 s, mean 8.62 s. That covers overlay creation, mailbox
build, UEFI, full Windows boot, command execution, clean ACPI shutdown, result
readback and teardown. Host-side overhead outside the VM is ~0.1 s.

**Guest RAM** — boot time is flat, and it works far below the 1–2 GiB target:

| RAM | Result |
|---|---|
| 512 MiB | works, 8.9 s |
| 768 MiB | works, 8.8 s |
| 1024 MiB | works, 8.7 s |
| 1536 MiB | works, 8.7 s |
| 2048 MiB | works, 8.8 s (current default) |

Below 512 MiB is untested. Note that these all ran a trivial command; a real
build will need more.

**vCPUs** — 1 vCPU works, at 9.8 s vs 8.5 s for 4.

**Sizes**

| | |
|---|---|
| Stock `ValidationOS.vhdx` | 964 MiB |
| `base.qcow2` | **763 MiB** |
| Per-run overlay after a full boot | **44 MiB** |
| Per-run mailbox (allocated) | 2 MiB of a 64 MiB sparse file |
| Per-run UEFI variable store | 768 KiB of a 64 MiB sparse file |
| `winquick` binary (release) | 676 KiB |

**`winquick setup`** — about 2 s on this machine, dominated by `qemu-img`.

## Correctness checks

All verified through the real CLI against the base image built by
`winquick setup`:

| Check | Result |
|---|---|
| stdout captured | ✅ |
| stderr captured, kept separate from stdout | ✅ |
| exit code 0 | ✅ |
| exit code 42 preserved | ✅ |
| exit code 3 preserved alongside output on both streams | ✅ |
| unknown command → message on stderr, exit 1 | ✅ |
| CRLF translated to LF for Unix consumers | ✅ |
| 28 KB / 547 lines of stdout | ✅ |
| base image SHA-256 unchanged after 15+ runs | ✅ |
| `~/.winquick/run` empty after a clean run | ✅ |
| guest is genuinely ARM64 Windows (`%PROCESSOR_ARCHITECTURE%` = `ARM64`) | ✅ |

## Open questions

1. **Optional packages.** PowerShell, .NET and the build tooling that would make
   `winquick run -- dotnet test` meaningful all live in `cabs/` on the ISO and
   are added by Microsoft's `GenImage.cmd`, which requires Windows + DISM. Either
   a macOS-native cab/WIM servicing path is needed, or WinQuick has to drive a
   one-time Validation OS "builder VM" that runs the ISO's own bundled DISM
   (`GenImage/Tools/DISM/arm64`) against a target disk. The second looks more
   promising and reuses machinery WinQuick already has.
2. **virtio-serial via offline `CriticalDeviceDatabase` registration** — the path
   to streaming output and multi-command sessions.
3. **A compiled guest agent.** Rust supports `aarch64-pc-windows-gnullvm`, which
   avoids the MSVC toolchain and its licensing, but needs an `llvm-mingw` build
   for aarch64 that Homebrew does not carry.
4. **Where the 8.5 s goes.** Not yet instrumented into boot / execute / shutdown
   phases. Worth knowing before trying to make it faster.
5. **Shrinking the base image.** The NTFS partition is 31.8 GiB for ~1 GiB of
   content. `ntfsresize` is available and could cut the virtual size
   dramatically, which would speed up `qemu-img` and reduce overlay overhead.
6. **Workspace mounting** (`C:\workspace`) — untouched so far.

---

# Warm start: eliminating the 8.5 s boot

Everything in this section was measured on the same M4 Pro host described at the top.
The warm mechanism below is a **validated prototype outside the CLI**; it is not yet wired
into `winquick run`. The cold path is untouched and still works.

## Where the 8.5 seconds goes

Profiled by polling host-side file state (`serial.log` growth, overlay `st_blocks`, mailbox
mtime) during a real `winquick run -- cmd /c ver`. QMP polling was tried first and rejected:
polling `query-blockstats` every 20 ms added ~1 s of overhead and distorted the measurement.

| Phase | Time | Notes |
|---|---|---|
| `winquick` startup, overlay create, mailbox build, QEMU spawn | **0.07 s** | negligible |
| UEFI (EDK2) | **~0.4 s** | serial log stops growing here |
| Windows boot → agent runs command → results written | **~6.5 s** | mailbox first written at t=6.92 s |
| Windows shutdown → QEMU exit | **~1.4 s** | overlay jumps 0.6 → 43 MiB (dirty page writeback) |
| **Total** | **8.33 s** | |

The host-side code is not the problem. Essentially all of the time is Windows booting and
then shutting down again. The overlay barely grows until the very end, which confirms the
boot is CPU/decompression-bound rather than write-bound.

## Mechanisms evaluated

### `savevm` / `loadvm` (qcow2 internal snapshots) — rejected

Raw timings were excellent: **savevm 0.07 s, loadvm 0.03–0.05 s** (mean 0.038 s). But it is
unusable here for two independent reasons.

**Every writable block device must support snapshots.** The mailbox is a raw image, which
does not, so `savevm` fails outright:

```
Error: Device 'mbox' is writable but does not support snapshots
```

Converting the mailbox to qcow2 does not help, because `loadvm` would then *revert* the
mailbox — discarding the very command we just wrote into it.

**Where the VM state lands is not controllable.** QEMU stores it in the first snapshot-capable
writable device. With a qcow2 UEFI varstore, the RAM state went into the *varstore* (405 MiB)
rather than the root overlay, and `loadvm` then crashed QEMU outright:

```
Assertion failed: (!auto_alloc || *pptr == NULL), function vmstate_load_next, file vmstate.c, line 265
```

### `migrate` to file + `-incoming` — selected

Migration has no snapshot-capability requirement on block devices; disk consistency is the
caller's responsibility, which suits us exactly since we already control the overlay.

| Operation | Measured |
|---|---|
| `migrate file:<path>` (1024 MiB guest, stopped) | **3.4 s**, one-off at build time |
| Ready-state file | **407 MiB** |
| Ready disk (overlay at migration instant) | **41 MiB** |
| `-incoming file:<path>` restore to `paused` | **52–86 ms** |

Restore spawns a fresh QEMU process each run, so there is no long-lived daemon, no resident
RAM, and no idle CPU — and crash recovery is trivial because nothing persists between runs.

### Keeping QEMU paused between runs — not needed

Given a 52–86 ms cold restore from file, a resident daemon would save at most a few tens of
milliseconds while costing ~400 MiB RSS permanently and requiring lifecycle management. Not
pursued; revisit only if the per-run figure has to drop below ~100 ms.

## The blocker that mattered: Windows filesystem caching

A booted, resumed Windows does **not** see host-side changes to the mailbox, and its own
writes do **not** reach the host image. The mailbox only synchronises at volume mount and
dismount — which in the cold path happen to coincide with boot and shutdown.

Measured directly: with the guest running and the host rewriting the mailbox image
underneath (including with `cache=none`), the guest never observed the change within 30 s.

### The primitive that fixes it

`mountvol.exe` is present in Validation OS and provides a complete invalidation cycle:

```bat
for /f "tokens=*" %%v in ('mountvol D: /L') do set VOL=%%v   :: stable volume GUID
mountvol D: /P            :: dismount -> flushes guest writes out to the host image
mountvol D: %VOL%         :: recreate mount point -> next read comes from disk
```

Verified in a diagnostic boot (results written to `C:` and read back with `ntfscat`):

```
VOL=[\\?\Volume{47b92dd1-a01d-11f1-97e6-806e6f6e6963}\]
GO:YES                 <- file visible before dismount
after-P errorlevel=0
afterP-GO:NO           <- volume gone, cache dropped
after-remount errorlevel=0
afterRemount-GO:YES    <- re-read from disk, fresh content
```

A separate test confirmed the flush direction: a file written by the guest then followed by
`mountvol D: /P` appeared in the host image **without any shutdown**.

`mountvol /R` does *not* bring a dismounted volume back — it only prunes stale entries. The
mount point must be recreated explicitly from the volume GUID.

**Consequence for the per-run mailbox:** the volume GUID is derived from the filesystem, so
the mailbox image must keep its identity across runs. Reformatting it per run (`mformat`)
changes the serial and the agent's saved GUID stops resolving. The prototype instead copies
a pristine ready-mailbox with `cp -c` (APFS clone, effectively free) and rewrites only the
files inside it.

## The warm-mode guest agent

Same shape as the cold agent, but it waits instead of shutting down:

```bat
for /f "tokens=*" %%v in ('mountvol %WQ% /L') do set VOL=%%v
mountvol %WQ% /P
:loop
mountvol %WQ% %VOL%                  :: remount -> fresh view
if exist %WQ%\WQGO.TXT goto run
mountvol %WQ% /P
goto loop
:run
cmd /c %WQ%\WQCMD.CMD > %WQ%\WQOUT.TXT 2> %WQ%\WQERR.TXT
set RC=%errorlevel%
>%WQ%\WQCODE.TXT echo %RC%
mountvol %WQ% /P                     :: dismount -> flush results to the host
```

The poll loop spins, but only while the VM is actually running; between runs no VM exists at
all.

## A trap worth recording: read-only UEFI varstore

Several hours were lost to this. To satisfy `savevm`'s "writable devices must support
snapshots" rule, the UEFI variable store was made `readonly=on`. **Windows then never boots** —
EDK2 cannot persist boot variables, the guest sits in firmware, and the symptom is a
completely black framebuffer with no error anywhere.

This produced a cascade of false conclusions: a "stale cache" verdict that was really "the
guest never booted", and a 63 MiB ready-state file that looked plausible but contained a VM
stuck in UEFI. The giveaway is state-file size: a genuinely booted 1024 MiB guest produces
**~407 MiB**, not 63 MiB.

The varstore must stay writable. It is copied per run alongside the disk, so it stays
disposable.

## Measured results

Prototype: fresh QEMU per run, `-incoming file:` restore, APFS-cloned ready disk + varstore +
mailbox, warm agent, teardown by killing QEMU.

**20 consecutive warm runs of `cmd /c ver`** — 0 failures, output verified each time:

| | |
|---|---|
| min | **0.199 s** |
| p50 | **0.210 s** |
| mean | **0.209 s** |
| p95 | **0.214 s** |
| max | **0.214 s** |

Against a cold baseline of mean 8.5 s (min 8.31, max 9.65): **~40× faster**, and comfortably
inside the "< 1 second" goal.

Per-run breakdown: ~20–40 ms host prep (three APFS clones + two `mcopy`), ~55–85 ms QEMU
start and RAM restore, remainder is the guest's poll iteration plus command execution.

### Correctness

| Check | Result |
|---|---|
| `cmd /c ver` → correct build string, exit 0 | ✅ 0.21 s |
| `cmd /c exit 42` → exit 42 | ✅ |
| stdout and stderr separated, exit 7 preserved | ✅ `OUT1` / `ERR1` / 7 |
| unknown command → stderr message, exit 1 | ✅ |
| **disk** changes discarded between runs | ✅ file written in run 1 absent in run 2 |
| **registry** changes discarded between runs | ✅ `HKLM\SOFTWARE\WQTEST` absent next run |
| **environment** changes discarded between runs | ✅ |
| base image SHA-256 unchanged | ✅ |
| cold path still works after all of this | ✅ 8.80 s |

### Storage cost

| Artifact | Size |
|---|---|
| Ready state (RAM + device state) | 407 MiB |
| Ready disk (overlay at snapshot) | 41 MiB |
| Ready varstore | 64 MiB (sparse) |
| Ready mailbox | 64 MiB (sparse) |

Per-run copies are APFS clones, so they cost approximately nothing until written.

## Not yet done

- **Not integrated into the CLI.** `winquick run` still always takes the cold path.
- **No staleness/invalidation metadata yet.** A ready state must be invalidated when the base
  image, QEMU version, guest agent, firmware, vCPU count or memory size changes. Nothing
  records those today, and restoring a mismatched state is unsafe.
- **No automatic fallback** from a missing or corrupt ready state to the cold path.
- Guest RAM for the warm prototype was 1024 MiB (the cold default is 2048 MiB). The ready
  state scales with RAM, so this is a real trade-off that has not been explored.
- The warm agent's poll loop spins a vCPU while running; acceptable now, but it sets a floor
  on how low per-run latency can go.

---

# Warm path in the CLI: measured results

The prototype above is now `winquick run`. Everything below was measured on the
real CLI, not a harness, on the same M4 Pro.

Reproduce with:

```console
cargo build --release
winquick setup --force        # rebuilds the base image with the waiting agent
rm -rf ~/.winquick/states     # or: winquick reset
./tests/integration.sh 100
```

## What changed in the product

- The guest agent no longer calls `shutdown`. It writes `WQREADY.TXT`, then waits.
  The host kills QEMU once the exit code lands, which also removes ~1.4 s of
  Windows shutdown from the cold path (cold is now **7.9 s**, was 8.5 s).
- `winquick run` resumes a frozen guest when one is valid, and builds one when it
  is not.
- New `winquick reset` discards the frozen guest; new `--cold` forces a full boot.
- Mailbox protocol is versioned (v1) and documented in `src/mailbox.rs`.

## Timings

**First run after `setup`** (no prepared guest):

```
host startup 2ms
no ready state yet
preparing a reusable Windows image (one-off, takes a few seconds)
guest ready after 7.9s
ready state built in 11.3s (414 MiB)
warm phases: prep 5ms | qemu spawn 41ms | state restore 105ms | guest exec + mailbox sync 80ms
teardown 7ms
warm run, total 11581ms
```

**Steady state**, 100 consecutive `winquick run -- cmd /c ver`, zero failures:

| | |
|---|---|
| min | 216 ms |
| p50 | **225 ms** |
| mean | 225 ms |
| p95 | **234 ms** |
| p99 | 236 ms |
| max | 236 ms |

Acceptance target was p95 < 300 ms. Met with margin.

**Phase breakdown** of a representative warm run (229 ms total):

| Phase | Time |
|---|---|
| WinQuick host startup | 2 ms |
| Prep — clone 4 files, inject command | 4 ms |
| QEMU process startup to QMP | 31 ms |
| State restore (`-incoming file:`) | 103 ms |
| Guest execution + mailbox synchronisation | 80 ms |
| QEMU termination | 7 ms |

State restore and the guest's own mount/execute/dismount cycle dominate. The
80 ms guest phase includes at least one full dismount-remount cycle of the
mailbox volume, which is the price of cache coherency without a driver.

## Resource cost

| | |
|---|---|
| `ready.state` | 415 MiB |
| `ready-disk.qcow2` | 40 MiB |
| `ready-vars.fd` | 64 MiB (sparse) |
| `ready-mailbox.img` | 64 MiB (sparse) |
| **Prepared guest, total on disk** | **460 MiB** |
| Peak QEMU RSS during a warm run | **1286 MiB** |
| Guest RAM | 1024 MiB |

Peak RSS is roughly guest RAM plus QEMU overhead: restoring the state faults the
whole guest RAM in. There is no resident daemon, so this exists only while a
command is actually running.

## Integration tests

`tests/integration.sh` runs the real CLI. 23 checks, all passing:

| Group | Checks |
|---|---|
| Streams and exit codes | build string on stdout; exit codes 0/1/7/42/99/255 round-trip; stdout and stderr never cross-contaminate; unknown command exits 1 with a message on stderr |
| Disposability | filesystem, `HKLM` registry and environment mutations all absent in the next run |
| Base immutability | `base.qcow2` SHA-256 unchanged across runs |
| Invalidation | changing `--memory` or `--cpus` invalidates the prepared guest, with `--verbose` naming the reason |
| Corruption | a truncated `ready.state` is detected, discarded, rebuilt, and the command still returns the right answer and exit code |
| Rebuild | a deleted prepared guest is rebuilt automatically |
| Reliability | 100 consecutive warm runs, zero failures |

Invalidation and fallback are exercised automatically, not just reasoned about.

### A test that was wrong, not a bug

The environment-leak check initially asserted that `cmd /c echo [%WQLEAK%]`
returns the literal `[%WQLEAK%]`. It does not: an undefined variable expands to
nothing, so the correct clean result is `[]`, and a leak would show `[1]`. The
assertion was fixed; no product change was needed.

## Known issues and limits

- **1024 MiB guest RAM** on both paths now, down from the cold default of 2048 MiB,
  because that is the configuration the warm prototype proved. Larger workloads
  may need more, and the prepared-guest size scales with it.
- **Peak RSS is ~1.3 GiB while a command runs.** Fine for one at a time; running
  many concurrently would need thought.
- **Building the prepared guest takes ~11 s** and is not optimised. Migration
  alone is ~3.4 s of that.
- **The agent's wait loop spins a vCPU** while the VM is running. It costs nothing
  between runs, because between runs there is no VM, but it sets a floor on
  per-run latency.
- **Concurrent `winquick run` invocations are untested.** They would share the
  prepared guest read-only, which should be safe, but nothing verifies it.
- **`qemu_version` in the fingerprint is derived from the binary path**, not from
  running `--version`, to keep the warm path fast. An in-place QEMU upgrade
  changes the binary's mtime and inode, which is what actually triggers
  invalidation.
- The two `setup` caveats are unchanged: `ntfsprogs` must be built from source,
  and the ISO is left mounted at `/private/tmp/winquick-vos`.

---

# PowerShell 7 on Validation OS

**It works, unmodified, with no additional Windows components.**

## What was tested

| | |
|---|---|
| Package | `PowerShell-7.6.5-win-arm64.zip`, the official portable ZIP |
| Source | `github.com/PowerShell/PowerShell/releases/download/v7.6.5/` |
| SHA-256 | `20514a755d16428dc4355c85e0883c859531e71cc3e122670aa1fccdbf96ba7e` |
| Verified against | the `digest` field GitHub publishes for the release asset — matches |
| Extracted | 271 MiB, 657 files, 41 directories |

No MSI, no installer, no registry work. Unzip and run `pwsh.exe`.

First successful result, in a disposable guest:

```
PSVersion                      7.6.5
PSEdition                      Core
GitCommitId                    7.6.5
OS                             Microsoft Windows 10.0.26100
Platform                       Win32NT
```

**Additional Windows dependencies required: none.** No missing DLL, no missing
API, nothing on stderr. PowerShell 7 is a self-contained .NET deployment and
Validation OS already carries everything it needs. This was not a foregone
conclusion — WinPE guides routinely claim PowerShell 5.1 and extra optional
components are prerequisites, and none of that turned out to be true here.

## The failure that was not PowerShell's fault

The first three attempts produced no PowerShell at all: the guest simply had no
such drive. `mountvol` showed only `C:` and the mailbox. QEMU's stderr had the
answer:

```
qemu-system-aarch64: aio failed: Operation not permitted
```

The capability disk had been attached `readonly=on`. **Windows writes to a volume
when it mounts it**, those writes failed against a read-only NVMe, and the volume
never appeared. Nothing was wrong with the image, the filesystem, or PowerShell.

Time was also lost to a hand-rolled MBR + `mformat @@offset` image that Windows
would not mount; the fix was to build the image with the same MBR + FAT32 code the
mailbox already uses.

**Capability volumes must be attached writable and cloned per run.**

## Deployment: a capability volume, not a bigger base image

PowerShell lives in its own FAT32 image, attached as a third NVMe device only when
it exists:

```
~/.winquick/images/validation-arm64/
    base.qcow2   763 MiB   unchanged
    pwsh.img     401 MiB apparent / 272 MiB allocated
```

Built by `winquick setup --with powershell`, which downloads the ZIP from
Microsoft, verifies the SHA-256, unpacks it and writes the volume. About 7 seconds.

Baking PowerShell into the base image was rejected for two reasons. It would grow
a 763 MiB runtime by 271 MiB — 36% — for something not every run needs. And
`ntfscp` cannot create directories, so writing 41 nested directories into the NTFS
system volume from macOS is not straightforward, whereas building a FAT32 image is.

The agent probes attached volumes for `\pwsh\pwsh.exe` and prepends it to `PATH`,
so `pwsh` works by bare name without anyone knowing a drive letter. Attaching the
volume changes the device topology, which is part of the ready-state fingerprint,
so the frozen guest is rebuilt automatically.

## Cost

| | Before | After |
|---|---|---|
| Base image apparent | 762.7 MiB | **762.7 MiB — unchanged** |
| Base image allocated | 799,801,344 B | **799,801,344 B — unchanged** |
| Prepared state | 460 MiB | 460 MiB (`ready-disk` 42.2 → 42.7 MiB) |
| PowerShell volume | — | 401 MiB apparent / **272 MiB allocated** |
| Peak QEMU RSS, `cmd` | 1286 MiB | 1318 MiB |
| Peak QEMU RSS, `pwsh` | — | **1433 MiB** |

## Latency

`cmd` numbers must not be quoted as PowerShell numbers — PowerShell costs roughly
half a second of its own process startup.

| Command | p50 | p95 | p99 |
|---|---|---|---|
| `cmd /c echo hello` | 234 ms | 242 ms | 248 ms |
| `pwsh -Command "Write-Output hello"` | 731 ms | 741 ms | 745 ms |
| `pwsh -Command "'WQ-' + (6*7)"` | 599 ms | 611 ms | 715 ms |

Phase breakdown, same machine, same run shape:

| Phase | `cmd` | `pwsh` |
|---|---|---|
| WinQuick host startup | 2 ms | 2 ms |
| Prep (clone volumes, inject command) | 6 ms | 6 ms |
| QEMU spawn | 33 ms | 33 ms |
| State restore | 104 ms | 107 ms |
| **Guest execution + mailbox sync** | **73 ms** | **573 ms** |

WinQuick's own overhead is identical at ~145 ms. The difference is entirely
PowerShell's startup inside the guest — about **500 ms** — which is .NET runtime
initialisation and module loading, and is not something WinQuick can shorten.
`Write-Output` is measurably slower than a bare expression because it pulls in
more of the cmdlet machinery.

## A defect this milestone exposed: argument quoting

`winquick run` joined argv with spaces, so any argument containing a space lost
its grouping. `cmd /c ver` was unaffected, which is why it went unnoticed — but
PowerShell needs quotes constantly, and

```console
winquick run -- pwsh -Command 'Write-Output OUT; [Console]::Error.WriteLine("ERR")'
```

arrived at PowerShell as several arguments and failed with
`Missing ')' in method call`.

`winquick run -- a b c` now means "run program `a` with arguments `b` and `c`",
like `docker run`, and arguments are quoted using the Windows C-runtime rules that
`pwsh.exe` uses to split the command line back up — including the awkward part,
where a run of N backslashes before a quote must become 2N+1, and a run at the end
of a quoted argument must become 2N. Seven unit tests cover the rules; four
end-to-end cases confirm them against the real guest.

One consequence: a whole command line can no longer be passed as a single
argument. Write `cmd /c "echo A & echo B"` rather than `'cmd /c echo A & echo B'`.

## Reliability

50 consecutive `pwsh -NoProfile -NonInteractive -Command "'WQ-' + (6*7)"`:

| | |
|---|---|
| failures | **0** |
| output | exactly `WQ-42` every time |
| stderr | empty every time |
| min / p50 / mean | 589 / 599 / 602 ms |
| p95 / p99 / max | 611 / 715 / 715 ms |

Exit codes through PowerShell: `exit 42` → 42, `exit 0` → 0, `exit 7` → 7.
`Write-Error "boom"` → exit 1 with output on stderr only and stdout empty.
`throw "fatal"` → exit 1, `Exception: fatal` on stderr. Mixed streams with
`exit 3` → stdout `OUT`, stderr `ERR`, exit 3.

The integration suite is now 33 checks, all passing.

## Caveats

- **PowerShell writes ANSI colour escapes to stdout** (`$PSVersionTable` renders
  with `[32;1m` sequences). Fine for a terminal, awkward for a script parsing
  output. `-NoProfile -NonInteractive` does not suppress it; callers wanting clean
  text should use `$env:NO_COLOR` or `Out-String`.
- **~500 ms of PowerShell startup per run** is inherent. A workload making many
  small `pwsh` calls will feel it; one `pwsh` call doing many things will not.
- **272 MiB allocated** for the capability volume, of which about **55 MiB is WPF
  and WinForms** (`PresentationFramework`, `System.Windows.Forms`,
  `PresentationCore`, `wpfgfx_cor3`, …) that a headless guest can never use.
  Trimming them is an obvious future saving, untested so far.
- Only PowerShell 7 was tested. Windows PowerShell 5.1 was not attempted and is
  not present in Validation OS.
- The capability volume must be writable; see above.

## Recommendation

**Keep PowerShell optional, as it is now.**

The base runtime stays at 763 MiB and the `cmd`-only path stays at 234 ms. Users
who want PowerShell run `winquick setup --with powershell` once and then use
`pwsh` by name, with no profile management, no drive letters and no flags at the
call site — the UX goal is already met without a profile system.

Making it default would add 272 MiB to every installation for a capability many
runs will not use, and would raise the floor on a `cmd`-only run's peak RSS. The
argument for changing this would be evidence that most real workloads want
PowerShell; that evidence does not exist yet.

---

# Modern .NET: three models measured

Tested with **.NET SDK 10.0.201 / runtime 10.0.5**, host SDK 10.0.201 on macOS
(`osx-arm64`), guest Validation OS 26100.8972 ARM64.

## Model A — build on macOS, execute on Windows

Cross-publishing from macOS works and the results run in the guest **with no .NET
installed there at all**.

| Deployment | Size | Files | Runs in guest? |
|---|---|---|---|
| self-contained `win-arm64` | 86 MiB | 192 | ✅ |
| self-contained single-file | 79 MiB | 2 | ✅ |
| framework-dependent | 168 KiB | 5 | ❌ without a runtime |
| self-contained `win-x64` | 77 MiB | — | not run (guest is ARM64) |

A self-contained app reports:

```
runtime      : .NET 10.0.5
os           : Microsoft Windows 10.0.26100
architecture : Arm64
is windows   : True
```

The framework-dependent build fails exactly as it should when no runtime is
present: `Error: the default install location cannot be obtained`, exit
`-2147450749`. That is the clean proof that Model A needs nothing in the guest.

### Does WinQuick actually add anything over building on macOS?

Yes, and this is the part worth being concrete about. A `net10.0-windows` app
cross-built on macOS was run in the guest:

```
PASS pinvoke kernel32   : GetTickCount64=2531ms SystemDirectory=C:\windows\system32
PASS registry           : HKCU\Software\WinQuickTest\Probe = hello-from-winquick
PASS registry HKLM      : CurrentBuild = 26100
PASS paths              : sep='\' backslash=True normalised=C:\Windows\System32 caseInsensitive=True
PASS named pipes        : round-tripped byte 42
PASS environment        : OSVersion=10.0.26100.0 SystemDirectory=C:\windows\system32 Is64=True
ALL-WINDOWS-CHECKS-PASSED
```

The same source run natively on macOS:

```
host os  : macOS 26.5.2
registry : NullReferenceException: Object reference not set to an instance of an object.
kernel32 : DllNotFoundException: Unable to load shared library 'kernel32.dll'
```

The compiler even warns (`CA1416`) that these APIs are Windows-only. Compiling
successfully on macOS proves nothing about whether the code works; running it
under a real Windows kernel does.

## Model B — .NET runtime capability

`dotnet-runtime-10.0.5-win-arm64.zip`, unpacked into a capability volume.

| | |
|---|---|
| Download | 33.5 MiB |
| Volume, apparent | 171 MiB |
| Volume, **allocated** | **90 MiB** |
| `dotnet --info` | p50 **246 ms** |
| framework-dependent app | p50 **333 ms**, p95 367 ms |
| Peak QEMU RSS | 1356 MiB |

`dotnet --info` correctly reports no SDKs and `Microsoft.NETCore.App 10.0.5`.

## Model C — .NET SDK capability

`dotnet-sdk-10.0.201-win-arm64.zip`, same mechanism.

| | |
|---|---|
| Download | 281 MiB |
| Volume, apparent | 1087 MiB |
| Volume, **allocated** | **837 MiB** |
| Build time for the volume | ~71 s, one-off |
| `dotnet --info` | p50 **567 ms** |
| `dotnet build` (2-file project) | p50 **2773 ms** |
| `dotnet test` (3 xunit tests) | **11.1 s** |
| Peak QEMU RSS (`dotnet build`) | **1688 MiB** |

Everything works inside the guest:

- `dotnet --info` → SDK 10.0.201, RID `win-arm64`, base path `F:\dotnet\sdk\10.0.201\`
- `dotnet new console` → scaffolds **and restores, with no network**
- `dotnet build` → `Build succeeded. 0 Warning(s) 0 Error(s)`
- `dotnet test` → `Passed! - Failed: 0, Passed: 3, Skipped: 0`

### NuGet

The guest has no network, and that is visible immediately:

```
error NU1301: Unable to load the service index for source https://api.nuget.org/v3/index.json.
error NU1301:   No such host is known. (api.nuget.org:443)
```

A plain console project needs nothing external — the SDK carries enough to
restore and build offline. Anything with package references does not.

The prototype that worked: restore on macOS into a local folder
(`dotnet restore -r win-arm64 --packages ./packages`), stage it in the workspace
alongside the project, and point `NUGET_PACKAGES` at it. `dotnet test` then
succeeds fully offline. The cost is real — 18 packages came to 283 MiB, which is
2 s of staging per run — so a durable answer is a NuGet **capability volume**
built once rather than a workspace payload staged every time. Not built yet.

The `NU1900` warnings about vulnerability data are harmless offline noise.

## Workspace transfer

The mechanism is a FAT32 volume of fixed size, always attached so the device
topology (and therefore the prepared-guest fingerprint) does not depend on
whether a given run supplied a project. Per run it is APFS-cloned from a template
kept with the prepared guest, then refilled — never reformatted, because the guest
re-reads it by dismounting and re-creating the mount point from the volume GUID,
exactly like the mailbox.

The agent surfaces it at `C:\workspace` with `mklink /J` and makes it the working
directory.

### Staging cost, measured

| Payload | Files | Staging | Total run |
|---|---|---|---|
| 8 KiB source project | 2 | **0 ms** | 270 ms |
| 168 KiB framework-dependent app | 5 | **2 ms** | 276 ms |
| 79 MiB single-file self-contained | 2 | **220 ms** | 479 ms |
| 86 MiB self-contained | 192 | **817 ms** | 1064 ms |
| 284 MiB project + NuGet cache | 812 | **2011 ms** | 2269 ms |

Roughly 140 MiB/s with a per-file component. **This is the number that decides
Model A versus Model B.** Capability volumes are cloned (free, copy-on-write);
workspace payloads are copied (linear). Shipping an 86 MiB self-contained app
through the workspace costs 817 ms *every run*; shipping a 168 KiB
framework-dependent app against a 90 MiB runtime capability costs 2 ms.

Disposability holds: the guest wrote `C:\workspace\newfile.txt`, the host project
was byte-for-byte unchanged, and the next run did not see the file.

## WPF and WinForms

| Question | Answer |
|---|---|
| Cross-build on macOS? | **Yes**, with `-p:EnableWindowsTargeting=true`. Without it: `NETSDK1100`. |
| Build inside WinQuick with the portable SDK? | **Yes**, no extra flag — it is real Windows. |
| Do the assemblies load headless? | **Yes** — both published apps ran and returned 0. |
| Can UI objects be instantiated? | **No.** |

Constructing a `Form` fails:

```
WINFORMS-FAILED TypeInitializationException: The type initializer for
'Windows.Win32.PInvokeGdiPlus' threw an exception.
```

`C:\windows\system32\gdiplus.dll` is **absent** from Validation OS. The missing
piece is the optional `Microsoft-WinVOS-GDIPlus-Package` cab on Microsoft's ISO,
which cannot currently be added from macOS (it needs `GenImage.cmd` and DISM on a
Windows host — the same blocker recorded earlier).

So: WPF/WinForms projects **compile** and their non-GUI code **runs**; anything
touching GDI+ does not. Self-contained WinForms is 131 MiB, WPF 151 MiB.

## Reliability

30 consecutive framework-dependent invocations: **0 failures**, min 337 / p50 348
/ mean 352 / p95 374 / max 398 ms. stdout and stderr correct and separate every
time.

Exit codes: framework-dependent `42` → 42, self-contained `42` → 42, `0` → 0.

No state leakage: a registry key written under `HKCU` by one .NET run is absent in
the next.

The integration suite is now **41 checks, all passing**, covering PowerShell,
.NET, workspace and disposability.

## Latency summary

| Command | p50 | WinQuick overhead | Guest component |
|---|---|---|---|
| `cmd /c echo hello` | 259 ms | ~145 ms | ~73 ms |
| `pwsh -Command "'WQ-'+(6*7)"` | 629 ms | ~145 ms | ~500 ms |
| `dotnet --info` (runtime only) | 246 ms | ~145 ms | ~100 ms |
| framework-dependent app | 333 ms | ~145 ms | ~190 ms |
| self-contained app (86 MiB staged) | 1144 ms | ~145 ms + 817 ms staging | ~190 ms |
| `dotnet --info` (SDK) | 567 ms | ~145 ms | ~400 ms |
| `dotnet build` | 2773 ms | ~145 ms | ~2600 ms |
| `dotnet test` (with staged cache) | 11.1 s | ~145 ms + 2000 ms staging | ~9 s |

WinQuick's own overhead is a constant ~145 ms in every case.

## Capability sizes

| Capability | Download | Volume apparent | Volume allocated |
|---|---|---|---|
| `powershell` 7.6.5 | 95 MiB | 401 MiB | 272 MiB |
| `dotnet-runtime` 10.0.5 | 34 MiB | 171 MiB | **90 MiB** |
| `dotnet-sdk` 10.0.201 | 281 MiB | 1087 MiB | 837 MiB |

Base image is unchanged at 763 MiB throughout.

## Recommendation: D, the hybrid — with B as the default worth installing

Ranked by what the measurements actually say:

**Execution is the core value, and it needs nothing.** Model A works today with
zero guest .NET. Anyone can cross-publish self-contained on macOS and run it. This
must keep working and stays the zero-configuration path.

**But Model B should be the recommended default for iterative work**, because
self-contained deployment is the *slow* option here, not the fast one. 86 MiB
through the workspace costs 817 ms per run; 168 KiB against a 90 MiB runtime
capability costs 2 ms — a 3.4× difference in total run time (1144 ms vs 333 ms)
for 90 MiB of disk. For an agent running a test loop, that is the difference that
matters.

**Model C is real and worth offering, but it is not the default.** 837 MiB and a
2.8 s build is a genuine Windows build environment, and `dotnet test` passing
in-guest is a meaningful capability. It earns its place when the build itself must
happen on Windows — but most projects can be built on the Mac and only *executed*
under Windows, which is faster and smaller.

So the product shape is what the capability system already implements:

```
base            763 MiB   always
dotnet-runtime   90 MiB   recommended for .NET work
powershell      272 MiB   optional
dotnet-sdk      837 MiB   optional, when building must happen on Windows
```

Nothing here argues for putting .NET in the base image.

## Blockers and gaps

- **NuGet needs a plan.** Offline restore works only for dependency-free projects.
  Staging a package cache through the workspace works but costs ~2 s per run at
  283 MiB. A NuGet capability volume is the obvious fix and is not built.
- **No guest networking at all.** Deliberate so far, but it is what forces the
  NuGet workaround.
- **GDI+ is missing**, so WinForms/WPF UI objects cannot be constructed. Fixing it
  needs the Validation OS optional-package path, which needs Windows + DISM.
- **Workspace is read-only in effect** — the guest can write, but nothing comes
  back. Artifact extraction is not implemented.
- `dotnet test` at 11 s is dominated by staging and SDK startup, not by the tests.

---

# Artifact extraction and the persistent package cache

## Artifacts

### CLI

```console
winquick run --artifact "bin/Release/**" -- dotnet publish -c Release
winquick run -a "TestResults/**" -a "logs/**" -- dotnet test
```

Files land in `./winquick-artifacts/` unless `--artifacts-dir` says otherwise.
Writing into a directory that already has files in it requires
`--artifact-overwrite`, so a run cannot quietly clobber a source tree.

### Pattern semantics

Patterns are **relative to the workspace root** (`C:\workspace`) and resolved
**in the guest**, by Windows, because that is where the files are. Forward and
backward slashes are both accepted and normalised, so the same pattern works
whether it was typed on macOS or lifted from a Windows script.

Three forms — deliberately not a glob engine:

| Pattern | Meaning |
|---|---|
| `bin/Release/**` | that directory, recursively, hierarchy preserved |
| `*.log`, `logs/*.txt` | wildcard match within one directory |
| `logs/build.log` | one named file or directory |

### Implementation

A dedicated FAT32 volume is attached to every run. When `--artifact` is used,
WinQuick writes a small batch script into the mailbox; the agent runs it *after*
the command, `xcopy`s matches onto that volume, and dismounts it to flush. Once
QEMU has exited the host reads the volume and writes the files out — always
before the disposable run directory is deleted.

Chosen over the alternatives because it reuses machinery already proven: MBR +
FAT32 volumes, clone-per-run, and the dismount-to-flush trick the mailbox already
depends on. No guest networking, no server, no writable host mount.

### Behaviour on failure

Artifacts are collected **even when the command failed** — a failed build's logs
are usually the thing you wanted. The exit code is captured before extraction
runs, so it is unaffected:

```console
$ winquick run -a "logs/**" -- cmd /c "mkdir logs & echo failure-log> logs\err.txt & exit 42"
winquick: retrieved 1 file (0.0 MiB) into winquick-artifacts
$ echo $?
42
```

If the guest's copy step itself fails, WinQuick reports the guest's log and exits
non-zero rather than claiming success. Zero matches is not an error: it prints
`no files matched <pattern>` and leaves the command's exit code alone.

### Measured

| Payload | Round trip (staged in + extracted out) | Exact |
|---|---|---|
| — (no `--artifact`) | 295 ms | — |
| `--artifact`, zero matches | **334 ms** (+39 ms fixed) | — |
| 1 KiB | 376 ms | ✅ |
| 1 MiB | 373 ms | ✅ |
| 10 MiB | 437 ms | ✅ |
| 100 MiB | **838 ms** | ✅ |

Roughly 184 MiB/s for the 100 MiB round trip. Fixed overhead is 39 ms.

Verified: nested hierarchies, spaces in both directory and file names, multiple
patterns, single named files, 32 MiB binary exact by size, and that the host
source tree is never modified.

## Persistent package cache

### Architecture

```
canonical cache          ~/.winquick/caches/nuget/      written only by macOS
        |                                                 `winquick cache sync`
        v
cache volume             ~/.winquick/capabilities/nuget-cache.img
        |
        v
per-run clone            attached writable, discarded with the run
```

`winquick cache sync <project>` runs `dotnet restore -r win-arm64 --packages
<cache>` **on the Mac**, then rebuilds the volume. The guest gets
`NUGET_PACKAGES` pointed at its clone.

### Read-only or writable?

**Writable clone, discarded** — and that is not a compromise, it is the stronger
option. A read-only NVMe cannot be used at all: Windows writes when it mounts a
volume, and a read-only device makes those writes fail with `aio failed:
Operation not permitted`, so no volume appears. Cloning per run gives the same
isolation property with none of that: NuGet can write its `.nupkg.metadata`
files happily, and everything it wrote disappears when the run ends.

Tested directly: a run that writes into `%NUGET_PACKAGES%` leaves the canonical
image's SHA-256 unchanged, and the next run does not see the file. Untrusted
build scripts therefore cannot use the cache as a persistence channel — only
host-side `dotnet restore` ever writes the canonical copy.

### Do macOS-restored packages work on Windows?

**Yes, including RID-specific ones.** The cache built on macOS contains
`microsoft.netcore.app.runtime.win-arm64` and `microsoft.netcore.app.host.win-arm64`,
restored with `-r win-arm64`, and the guest consumed them unchanged: `dotnet test`
built and ran, and a `Newtonsoft.Json` app printed `{"ok":true}`. This was worth
checking rather than assuming — package *contents* are archives, but restore
writes RID-specific assets and lock files.

### Measured

Same xunit project that previously needed 284 MiB staged through the workspace.

| | Before (packages in workspace) | After (cache capability) |
|---|---|---|
| Workspace payload | 284 MiB, 812 files | **8 KiB, 2 files** |
| Workspace staging | **2011 ms** | **1 ms** |
| QEMU restore | ~110 ms | ~110 ms |
| Guest execution | ~9082 ms | ~9087 ms |
| **Total `dotnet test`** | **~11.1 s** | **9.15–9.21 s** |

The ~2 s staging penalty is gone. What remains is .NET's own work, not
WinQuick's: `dotnet restore` alone costs ~6 s inside the guest even reading from
a local cache, and `dotnet build` ~8 s including restore. WinQuick's overhead in
both cases is the usual ~145 ms.

Cache for this project: **18 packages, 305 MiB allocated** (8 GiB apparent,
sparse). `winquick cache sync` takes 2.5–8.4 s depending on what has to be
fetched.

### Cache hit and miss

A hit needs no network and no staging. A miss produces NuGet's usual `NU1301`
storm, so WinQuick appends an explanation:

```
winquick: A required NuGet package is not in the cache, and the guest has no
winquick: network by design. Populate the cache from this Mac, then run again:
winquick:     winquick cache sync <project>
```

Recovery is one command: `winquick cache sync` (2.5 s), then the next run
rebuilds the prepared guest once (11.8 s, because the cache is fingerprinted) and
succeeds; subsequent runs are back to ~8.6 s.

### Why the cache is fingerprinted

The cache is a capability volume, so its identity is part of the prepared-guest
fingerprint and changing it forces a rebuild. That costs ~12 s after each sync,
and it is the right trade: the guest never re-reads a volume after the frozen
image was captured, so a changed cache *must* invalidate the frozen guest.

## A silent-success bug this milestone exposed

While adding these volumes, a real and dangerous failure mode appeared: after
certain prepared-guest rebuilds, the guest would hold a **stale view of the
mailbox**, run an empty batch file, and report **exit 0 with no output**. The
command never ran, and WinQuick confidently reported success.

The trigger was over-reach on my part: I had the agent dismount and remount three
volumes before executing. One remount (the workspace) is reliable; three
destabilised the mailbox's own mount. The artifact volume never needs remounting
— it is empty at freeze and at run start — and the cache does not either, because
it is fingerprinted.

But the deeper problem was that a stale read looked like success. Every run now
carries a **nonce**: the host writes a per-run token into the mailbox, the agent
echoes it back beside the exit code, and a mismatch is treated as a failed warm
run — discard the prepared guest, fall back to cold, return the real answer.
Verified over 40 consecutive runs: 0 failures, 0 silent successes.

This is the class of bug that matters most here. A wrong exit code that looks
right is worse than a crash.

## Test suite

**56 checks, all passing**, now covering streams and exit codes, disposability,
invalidation and corruption recovery, PowerShell, .NET, workspace, artifacts and
the package cache — plus 9 unit tests for argument quoting and mailbox
round-tripping.

Warm `cmd` p50 is 295 ms with all capabilities attached (up from 234 ms with
none), which is the cost of six extra volumes at boot.

## Remaining blockers

- **`dotnet restore` costs ~6 s in-guest even on a cache hit.** That is NuGet's
  own work. `--no-restore` workflows would avoid it but need the `obj/` directory
  staged, and macOS-generated `obj/` carries macOS paths.
- **Cache sync forces a ~12 s prepared-guest rebuild.** Acceptable because syncs
  are rare, but it makes adding a package feel slow the first time.
- **Artifacts are copied, not streamed** — a very large output directory is
  bounded by the 2 GiB artifact volume.
- The pattern language was three shapes, not a real glob, and `**` in the middle
  of a path was unsupported. **Fixed in v0.2.1**: `src/artifact_patterns.rs`
  implements a real glob subset, and a single `*` now means one level rather
  than the whole tree.
- No artifact extraction from a run that times out or whose guest never responds.

---

# Dogfooding: can an agent use WinQuick without knowing what it is?

Full write-up, session logs and the test project are in
[`experiments/dogfood/`](../experiments/dogfood/). Summary of what it settled.

Three fresh headless `claude` sessions were given the same task — *"Fix this
project so all tests pass on Windows. You are working on a Mac."* — against a
separate `net10.0-windows` project with four deliberate Windows-only defects
(registry hive mismatch, `CharSet.Ansi` on a `…W` entry point, manifest paths
joined with `/`, case-sensitive path comparison) plus one deliberately
suspicious-but-correct construct as a control.

macOS baseline: 7 of 9 tests fail, every failure a platform artefact. Windows
baseline via WinQuick: 5 fail, each traceable to a defect.

**With WinQuick and one README line** (`winquick run -- <command>`): the agent ran
`winquick --help`, then `winquick run -- dotnet test`, diagnosed all four defects
from the Windows output, fixed them, re-ran, and finished with 9/9 passing. Six
WinQuick invocations, two edit/test iterations, 191 s wall clock, no human
intervention. It never tried Wine, Docker, a remote Windows machine, or QEMU.

**With no documentation at all**: it found WinQuick by searching the filesystem,
read the tool's own README, and used it by absolute path. Discovery did not depend
on the project mentioning it.

**With WinQuick genuinely removed**: it probed for docker, vagrant, VBox,
Parallels, UTM, tart and `az`, found none, and then reasoned statically — fixing
all four defects correctly. That is the honest result: for defects of this kind,
careful reading was enough.

What it could not do was *confirm*. Its report opened with "I fixed four bugs, but
I could not verify them on Windows", and it also rewrote the control construct on
a theory that a single ten-second `winquick run` disproves. So the measured value
of WinQuick here is not that the agent becomes able to fix Windows bugs — it is
that the agent stops guessing, stops editing working code speculatively, and can
say "9/9 passing" instead of "I could not verify".

## UX finding, fixed

The first thing the agent typed was
`winquick run -- "dotnet test --nologo"` — the whole command as one quoted
string. cmd.exe answered `'"dotnet test --nologo"' is not recognized…`, which is
hard to read. It recovered unaided on the next call, but `run` now recognises the
shape and says so:

```
winquick: `run` takes the program and its arguments as separate words,
winquick: like `docker run`. Try:
winquick:     winquick run -- dotnet test --nologo
```

## Answers to the product questions

| | |
|---|---|
| Did it understand WinQuick naturally? | Yes — `--help`, then straight to `run -- dotnet test`. |
| Was one README line enough? | Yes, and it was not even necessary. |
| Did it need `--help`? | It read it first, unprompted, and did not need more. |
| Sensible commands? | Yes: `--help`, `info`, `run -w . -- dotnet test`, `--timeout`. |
| Build on Mac or in Windows? | It chose `winquick run -- dotnet test` — build *and* test inside Windows. |
| Edit/test loops | 2. |
| Confusing error messages? | One (argument shape), now fixed. |
| Cache/capability/state problems? | None hit — the cache was pre-synced, as intended. |
| Quoting | One stumble, self-corrected. |
| Workspace semantics | No surprises; it never expected writes to come back. |
| Artifacts | Not used. The test runner prints results to stdout, so nothing needed extracting — forcing it would have been artificial. |
| Is ~300 ms tool-like? | Yes for trivial commands; irrelevant here because `dotnet test` dominates. |
| Is .NET latency acceptable? | ~10 s per `dotnet test` cycle. Acceptable, not delightful. |
| Did no guest networking matter? | No — the cache was warm. An unsynced project would have hit it. |
| Did no GUI matter? | No. |

## Biggest remaining usability blocker

**`winquick setup` still needs `ntfsprogs` built from source.** Every other rough
edge in this milestone was cosmetic; that one stops a new user from getting to
their first `winquick run` at all. It has been deferred three times now and is the
thing standing between "works on this Mac" and "works on someone else's".

---

# v0.1.0: productisation

What changed between the working prototype and a release, and what it cost.

## The setup blocker, solved

`winquick setup` needs two things macOS cannot do: write files into an NTFS
volume, and set a value in a Windows registry hive. Previously this meant asking
users to build `ntfsprogs` from source, which is not a product.

Options considered, in the order the brief suggested:

1. **Vendor the helpers** — chosen. `scripts/build-ntfs-helpers.sh` builds
   `ntfscp` and `ntfscat` from ntfs-3g/ntfsprogs 2022.10.3, statically
   linked against `libntfs-3g`, producing **312 KiB arm64 binaries whose only
   dynamic dependencies are `/usr/lib/libSystem` and CoreFoundation**. They ship
   in the release archive.

   (They were unmodified at the time. They no longer are: the Windows port
   needed an offset into a whole-disk image and a handful of portability fixes,
   and the same patch is now applied on both hosts so the two builds stay the
   same program. See [`patches/README.md`](../patches/README.md).)
2. **Another library** — hivex is already a Homebrew formula that works on macOS,
   so the registry side needs no vendoring at all: the formula declares it.
3. **Custom NTFS write logic** — rejected. Overwriting an existing same-size file
   would have been tractable, but creating one requires MFT allocation, bitmap
   updates and directory B-tree insertion. Not simpler, and far riskier.

QEMU takes the same route: a Homebrew dependency, so `brew install` handles it
and the user never types a QEMU command. All three stay separate executables,
which is the licensing boundary as much as the design.

Result: `brew install` then `winquick setup`, with nothing to compile.

## Setup as an experience

Setup now explains the Microsoft licensing boundary in plain language and offers
either `--accept-microsoft-terms` or `--from <path>`; looks for an image already
in `~/Downloads` or the cache; builds into a staging file and moves it into place
only when complete, so an interrupted run leaves nothing half-installed; and
finishes by **booting Windows and running a real command**. It never says "Ready"
without proving it.

Measured: **15 s** end to end with the image already downloaded.

## Bugs found by productising

Each of these was found by testing a path the prototype had never exercised.

**Ctrl-C left a virtual machine running.** Rust's default SIGINT terminates the
process without running any `Drop`, so the QEMU child kept its gigabyte of RAM and
the run directory stayed behind. Fixed with a signal handler that does only
async-signal-safe work — record the interruption, signal the child — and lets the
main thread unwind normally. A second bug hid behind it: interruption was being
treated as a recoverable warm-path failure, so WinQuick helpfully started a
*second* VM. Now: exit 130, nothing left behind.

**Concurrent cold starts raced.** The prepared-guest lock was acquired, the state
checked, and the lock dropped — all before the build began. Two runs could then
build simultaneously, and one could read a state the other was still writing. The
lock now spans the re-check and the build. Four concurrent runs, repeatedly, all
correct.

**`cp` wrote into the caller's stdout.** During that race, `cp: ... No such file`
appeared in a run's output while the run still reported success — a corrupted
result presented as a clean one. Child helper processes now have their output
captured.

**`winquick --help | grep -q` panicked.** Rust ignores SIGPIPE at startup, which
turns a closed pipe into `failed printing to stdout: Broken pipe`. Restored the
normal command-line behaviour. Safe here because nothing reaches stdout until
QEMU has already been shut down.

**Artifact names were trusted.** Entries came off a filesystem the guest
controlled and were joined onto the destination path directly. Now anything that
is not a single ordinary path component is skipped with a warning.

## Cache sync no longer costs 12 seconds

The prepared guest is fingerprinted over the capability volumes, so rebuilding
the package-cache volume invalidated it. But a sync that adds nothing does not
need to rebuild the volume at all: `cache sync` now counts packages before and
after and skips the rebuild when nothing changed. **0.6 s instead of ~12 s** for
the common no-op, with no loss of correctness — a sync that *does* add packages
still rebuilds and still invalidates, because the guest cannot re-read a volume
after the frozen image was captured.

## Measured, release build

| Command | p50 | p95 |
|---|---|---|
| `cmd /c ver` | **284 ms** | 295 ms |
| `pwsh -Command "'WQ-'+(6*7)"` | **644 ms** | 657 ms |
| framework-dependent .NET app | **378 ms** | 392 ms |
| `dotnet test`, small project | ~10 s | — |
| `winquick setup` | 15 s | — |
| first run after a change (rebuild) | ~12 s | — |

30 consecutive warm runs: min 266 ms, p50 288 ms, p95 298 ms, zero failures.

| Artifact | Size |
|---|---|
| `winquick` binary | 888 KiB |
| Release archive | 840 KiB |
| Windows runtime | 763 MiB |
| Prepared guest | 433 MiB |
| `powershell` | 273 MiB |
| `dotnet-runtime` | 90 MiB |
| `dotnet-sdk` | 837 MiB |

## Tests

74 integration checks and 9 unit tests. Beyond the earlier coverage, the suite now
exercises `--version`/`--help`/`doctor`/`info`/`clean --dry-run`, command
timeouts, Ctrl-C (exit code, no orphaned QEMU, no leftover directories), four
concurrent runs, and two artifact-safety cases: a pattern that tries to escape
the workspace, and the refusal to write into a non-empty artifacts directory.

## New-user validation

Performed against the release archive on a wiped `~/.winquick`, following only
the public documentation: doctor before setup (names the missing runtime), setup,
doctor --smoke, run, install PowerShell, install the SDK, workspace + cache +
`dotnet test`, artifact extraction, reset and rebuild, exit codes, and both
cleanup paths. Everything worked; the only fixes needed were cosmetic — `doctor`
reporting "prepared guest not built" as a failure when it is normal, and `info`
listing the internal package-cache volume as a capability with version `?`.

## Dogfood, v0.1.0

A fresh `claude` session in the buggy DevicePrep project, with the released
binary installed per `docs/install.md` and one line in the project README:

- **86 seconds, 9 turns, 5 winquick invocations, 2 test runs**
- Ran `winquick doctor`, `winquick info` and `winquick run --help` to orient
- Fixed all four Windows-only defects; 9/9 passing, verified independently
- No attempt at Wine, Docker, or a remote Windows machine
- No human intervention

Faster and more direct than the pre-productisation run (191 s, 6 invocations),
which is what the new diagnostic commands were for.

## Desktop dogfood, and what it cost

A fresh Claude Code session, given a small WPF utility with five planted defects
and told only to make it satisfy its requirements file. It had never seen
WinQuick. The full write-up is in
[experiments/desktop-dogfood](../experiments/desktop-dogfood/); what belongs
here is what it measured.

### It worked

All five defects found and fixed in one session, ~8 minutes, 41 tool calls. Both
deliberately suspicious-but-correct constructs survived — the `Loaded` handler
that looks redundant and is not, and the whitespace-normalising regex that looks
like over-engineering and is required. No speculative edits.

The defect that matters most is the visual one: a Save button with
`Margin="0,-34,0,0"` sitting exactly on top of the *Enable logging* checkbox.
UI Automation reports that checkbox as present, enabled, correctly sized and
`offscreen: false`, because as far as the tree is concerned it is laid out
normally. It is simply painted underneath. Nothing short of looking at the
pixels finds it.

### Timings, idle machine

| Operation | Time |
|---|---|
| `desktop start` — boot plus bridge | 9.3 s (mean of 12) |
| `launch` | 29 ms |
| first window ready (`wait-window`) | 634 ms |
| `get` one element | 22 ms |
| `tree`, full window, depth 12 | 35 ms |
| `type` | 14 ms |
| `click` (via InvokePattern) | 34 ms |
| `select` / `toggle` | 22 / 28 ms |
| `key` | 35 ms |
| `screenshot`, 1280x800 desktop | 140 ms |
| `screenshot`, 600x470 window | 101 ms |
| `stop` | 118 ms |

A whole 33-step scripted requirements run, from a published directory, is about
16 s including the boot.

### Two defects testing could not have found

Both were invisible until someone used the thing for an afternoon.

**A session mutated the installed capability volumes.** `winquick run` has always
cloned them, with a comment saying why: Windows writes to a volume when it mounts
it. The desktop path attached the installed images directly, so `dotnet-sdk.img`
changed hash after a session. Caught by checksumming the images either side of
the dogfood, not by any test.

**One session start in ten never came up.** The bridge scanned for its control
disk once, at the moment the shell started, before the disks had finished
enumerating. Measured at 1 in 5 and then 0 in 8 — the worst kind of failure
rate, frequent enough to matter and rare enough to look like something else. The
guest agent already retries finding the mailbox volume for precisely this
reason; the bridge now retries too. 0 failures in 12 after the fix, mean start
9.3 s.

### The thing that is not a bug

A `winquick run` issued while a desktop session is up takes **5 minutes**
instead of 300 ms. That is not a regression; it is a four-processor, 4 GiB
virtual machine competing for the same host. Measured, documented, and worth
knowing before concluding something has broken.

## Making a desktop session start in 380 ms

The first working desktop capability took 9.3 seconds to start a session. For a
product called WinQuick that is the wrong number, and the profile said so
plainly: 34 ms of host work, then nine seconds of watching Windows boot.

```
[     0ms] session directory      [    43ms] qemu spawned
[    20ms] disk overlay           [  9332ms] guest agent ready (windows booted)
[    36ms] volumes built          [  9517ms] bridge answering
```

The fix was not a new idea. `winquick run` already freezes a booted guest with
QEMU migration and restores it per run, which is where its ~288 ms comes from.
The desktop path simply was not using it.

One design decision mattered more than the rest: **freeze after the bridge is
answering**, not at the login prompt. A state frozen earlier would still have to
start the desktop stack and the bridge on every restore, which is most of the
nine seconds.

| | before | after |
|---|---|---|
| session start, p50 | 9,300 ms | **380 ms** |
| of which: restore RAM and devices | — | 391 ms |
| of which: refresh the app volume | — | 95 ms |
| one-off preparation | — | ~17 s |

30 consecutive sessions, each launching a WPF application, reading its UI
Automation tree and capturing a screenshot before being stopped: 30/30,
min 373 ms, p50 380 ms, mean 382 ms, p95 399 ms, max 402 ms.

### What it cost to keep it disposable

Freezing a guest that has already mounted its volumes means the next session
attaches a *different* application volume behind a cached directory. The bridge
and the application therefore live on separate volumes: the bridge volume is
frozen and never rewritten, because `wqui.exe` is running from it; the
application volume is refilled per session and the host asks the bridge to
dismount and remount it once after restoring. 95 ms.

Disposability is stronger than before, not weaker. Every session restores the
same frozen RAM and a fresh clone of the same frozen disk, so nothing
accumulates. Measured: a session that saved records, wrote `C:\dirty.txt`, added
a registry key and grew its overlay to 40 MB left the prepared state byte for
byte identical, and the next session came up with an empty form.

### Sizing, measured

| | 1 vCPU | 2 vCPU | 4 vCPU |
|---|---|---|---|
| session start | 499 ms | 490 ms | 533 ms |
| launch + first window | 684 ms | 665 ms | 683 ms |
| five UIA reads | 223 ms | 269 ms | 272 ms |
| screenshot | 133 ms | 134 ms | 156 ms |
| concurrent `winquick run` | — | 341 ms | 312 ms |

| memory | prepared state | start | resident |
|---|---|---|---|
| 2048 MiB | 849 MiB | **349 ms** | 2.5 GiB |
| 3072 MiB | 865 MiB | 427 ms | 3.5 GiB |
| 4096 MiB | 886 MiB | 507 ms | 4.6 GiB |

Two processors and 2048 MiB became the defaults. Four bought nothing measurable
and cost the host twice as much.

### The contention problem solved itself

A `winquick run` issued while a four-processor desktop session was up took about
five minutes instead of 300 ms. With the new defaults it takes 290 ms — the same
as with no session running at all. That was never a scheduling bug; it was a
session helping itself to half the machine for no benefit.

### A bug the profiling turned up

Installing the desktop capability invalidated the *command* prepared guest,
because the internal bridge build ran at 2048 MiB while `winquick run` defaults
to 1024. The next ordinary command after every desktop install silently paid for
a 12-second rebuild. Both now use the same shared defaults.

## A busy guest looked exactly like a halted one

Three real projects — a WPF app, a three-project OPC solution and a classic
`packages.config` MSBuild app — were run through WinQuick unmodified. The
solution build never took the warm path. Not sometimes: never.

```
sln-1     122s  cold run  prepared guests discarded=5
sln-2     121s  cold run  prepared guests discarded=5
sln-3     121s  cold run  prepared guests discarded=5
sln-4     122s  cold run  prepared guests discarded=5
csproj-1   19s  warm run  discarded=0
csproj-2    8s  warm run  discarded=0
csproj-3    9s  warm run  discarded=0
csproj-4    8s  warm run  discarded=0
```

Same workspace, same prepared guest, same machine. The only difference was the
command: `dotnet build App.sln` — three projects — against `dotnet build
App.Core/App.Core.csproj`, one of them. Every failure was reported as `timed out
waiting for WQGO.TXT from the guest` — a *restored* guest that never picked the
command up.

Which made no sense, because the go flag is consumed before the command runs.
A command cannot decide whether the guest that is about to receive it is alive.

### It could, though

`FIRST_CONTACT` was raised from 10 s to 120 s and nothing else changed:

```
winquick: using existing ready state
winquick: warm phases: prep 83ms | qemu spawn 42ms | state restore 97ms
        | guest exec + mailbox sync 10867ms
Build succeeded.
winquick: warm run, total 11103ms
```

Eleven seconds, warm, first try. The guest had never been halted.

The go flag disappearing is a **FAT directory write on the mailbox volume**,
and `guest/agent.cmd` deletes the flag and then immediately starts the
workload — on that same guest, hammering that same disk. The acknowledgement
and the workload race each other, and a solution build wins: three projects,
several MSBuild worker processes, and the one directory entry the host is
polling for stays in the guest's cache well past ten seconds. A single-project
build is light enough that it does not.

So the host was discarding a perfectly good prepared guest, five times per run,
because the guest was too busy to say "got it".

### The fix asks QEMU, not the guest

The deadline is still ten seconds, because a genuinely halted restore should
still be recognised in seconds. When it passes, the question becomes one the
guest cannot fail to answer, because it is not asked of the guest at all:
`query-blockstats` over the QMP monitor, summed across every block device.

- a guest that came back halted moves essentially nothing — its poll loop is a
  few sectors of FAT metadata per turn;
- a guest that is building moved **210 MiB** inside that same window, measured.

Sixteen megabytes is the line, and it sits two orders of magnitude clear of the
first case and one clear of the second. A monitor that will not answer counts
as no evidence, never as proof of life, so an unreadable monitor still falls
back in ten seconds.

Result on the same solution, unchanged:

```
winquick: the guest has not acknowledged the command yet but has moved 210 MiB
        — it is working, not halted; waiting for the command instead
Build succeeded.
winquick: warm run, total 11249ms
```

**122 s to 11 s, and the prepared guest survives.**

### The guest-side half, not shipped here

The deeper fix is one line in `guest/agent.cmd`: dismount and remount the
mailbox immediately after deleting the go flag, exactly as the agent already
does after writing `WQREADY.TXT`, so the acknowledgement is on the host's disk
before the workload starts. It is not shipped in this change because the agent
is baked into the base image, and changing its hash makes every existing
runtime report "built by a different version of winquick" until the user runs
`winquick setup --force`. Worth doing at the next runtime rebuild; the
host-side check stands on its own either way.


## Validation OS can have a .NET Framework, and which one

The note that said Validation OS "carries no .NET Framework runtime at all" was
true, and was read for a long time as a property of the product rather than of
the stock image. It is on Microsoft's own media as
`Microsoft-WinVOS-NetFx45-Package.cab`, in `cabs/Common` beside the graphics
and WPF packages WinQuick already applies, and DISM takes it like any other
package -- `rc=0`, offline, no Windows host involved.

The package name is historical. What it delivers is the OS's inbox
`C:\Windows\Microsoft.NET`, and the guest says which version that is. Asked
from inside a serviced guest, by a .NET Framework program reading its own
registry:

```
clr=4.0.30319.42000
ndp-version=4.8.09221
ndp-release=533509
```

One `v4.0.30319` runtime, and 4.x is an in-place family, so it is not a 4.5
runtime that happens to load 4.5 assemblies -- it serves the whole 4.x line.
Measured, by building each fixture in `experiments/dotnet-matrix/` and then
executing the result in the same disposable guest:

| Target | Builds | Runs |
|---|---|---|
| net40 | yes | **yes** |
| net45 | yes | **yes** |
| net472 | yes | **yes** (AnyCPU and x64, console and WPF) |
| net48 | yes | **yes** |
| net481 | yes | **yes** |

There is no CLR 2.0 on the image and no `NetFx35` package on the media, so
net20 and net35 have a build result and nothing more -- and worse than nothing
more: the 4.x shim's "please install .NET Framework v2.0" path wants a user,
and a headless guest has nobody to answer it, so the process never returns.
That is how the timeout defect below was found.

### What it took beyond `NetFx45`

Five more packages, each found by one failure:

| Package | Without it |
|---|---|
| `Apps` | `shell32.dll` is missing, `urlmon.dll` will not load, and `GenerateResource` takes the build down |
| `GDIPlus` | `new Bitmap(...)` throws `TypeInitializationException` |
| `Fonts` | GDI+ wants one the moment anything draws text |
| `COM` | `GenerateResource` asks the shell for a file's security zone and gets `REGDB_E_CLASSNOTREG` |
| `WLAN` | carries `rasapi32.dll`; without it NuGet dies in `ProxyCache`'s type initializer |

Plus the `-WOW64` halves of `Apps`, `COM` and `NetFx45`, for x86 under
emulation.

### Three things people mean by ".NET Framework"

Keeping these apart is what took the longest:

- **Reference assemblies** -- metadata only, what the compiler binds against.
  They come from NuGet (`Microsoft.NETFramework.ReferenceAssemblies.*`),
  restored on the Mac. A `.csproj` targeting .NET Framework does not declare
  them, because on Windows they arrive with a developer pack, which is why
  `winquick cache add <Name>@<Version>` exists.
- **The runtime** -- the CLR and `C:\Windows\Microsoft.NET`. Microsoft's
  media, applied by `capability install dotnet-framework`. Nothing runs without
  it; everything builds without it.
- **The classic toolchain** -- `MSBuild.exe`, `Microsoft.Common.targets`,
  `Microsoft.CSharp.targets`, `Microsoft.WinFX.targets`,
  `PresentationBuildTasks.dll`. Part of the same package. This is what restores
  a `packages.config` project and markup-compiles a classic WPF one, and the
  SDK's CoreCLR copy cannot substitute for it: `PresentationBuildTasks` on
  CoreCLR resolves XAML types from `@(ReferencePath)` alone and RAR puts no
  transitive framework assembly there, so a classic WPF project stops at
  **MC1000**.

`experiments/dotnet-matrix/ClassicNetFxX64` is one fixture that needs all three
at once, and `tests/integration.sh` builds and runs it whenever the capability
and the net472 reference assemblies are present.


## A slow command looked exactly like a broken guest

Found while measuring which .NET Framework targets actually run. A net20
console application builds in two seconds and then *hangs* the guest: there is
no CLR 2.0 on the image, and the 4.x shim's "please install .NET Framework
v2.0" path never returns on a machine with nobody to answer it.

`winquick run --timeout 90` on that binary took **745 seconds** to give up.
Both net20 and net35, four runs, no variance.

The reason is the same shape as the acknowledgement race above, seen from the
other end. The warm path asks two questions with one helper:

1. *Did this guest take the command?* — the go flag disappearing, `FIRST_CONTACT`.
2. *Has the command finished?* — `WQCODE.TXT` appearing, the user's `--timeout`.

Both report failure as `GuestSilent`, and `execute()` reads `GuestSilent` as
"this prepared guest is no good". So the second question timing out threw away
a working prepared guest, cold-booted, **ran the user's command again**, and
kept going for every prepare attempt — five of them now — each paying the whole
timeout. Ninety seconds of waiting became twelve minutes, and the command ran
six times.

Only the first question is evidence about the guest. By the time the command is
running the guest has demonstrably picked it up, so the second wait now reports
what it is:

```
winquick: the command did not finish within 90 s — raise it with `--timeout`
```

and the prepared guest is kept. The same command, unchanged, now costs
**101 seconds** for a 90-second timeout, and `ready.json` is still there
afterwards. The distinction is one error type, and `tests/integration.sh`
checks that a two-second timeout costs seconds rather than minutes and leaves
the prepared guest alone.

This was never a symptom of the six validation commits; it is older than them,
and raising `PREPARE_ATTEMPTS` from three to five made it worse rather than
causing it.


## The other half of the same race: a guest that is alive but idle

The byte-counter check above answers "is this guest working hard?", and a great
many healthy commands are not. Found by a regression test written for something
else:

```
winquick run --timeout 2 -- cmd /c "ping -n 30 127.0.0.1"
```

`ping` moves almost no data, so the guest fails the sixteen-megabyte test. It
also holds the go flag's directory entry in its cache for the whole thirty
seconds, exactly as a solution build does. So the host declared a perfectly
healthy guest halted, threw it away, rebuilt five more, declared each of those
halted too, and cold-booted -- **117 seconds for a two-second timeout**, and no
prepared guest left afterwards. Reproduced in isolation, four times, no
variance.

The byte total was the wrong question for this guest. The right one is smaller:
not *how much* has it moved, but *is it still moving*. Two readings a second
and a half apart settle it, and a halted guest cannot pass: its counters stop
dead once the I/O in flight at resume has drained, and nothing restarts them. A
live Windows guest never stops touching a disk for that long.

The heavy case still short-circuits on the total and pays nothing extra.
Measured after the change:

| | before | after |
|---|---|---|
| `dotnet build App.sln`, three projects | 11.0 s | **11.2 s** |
| `--timeout 2` on `ping -n 30` | 117 s, prepared guest destroyed | **14 s**, prepared guest kept |

The second number is 10 s of `FIRST_CONTACT`, 1.5 s of window and the user's
own two-second timeout, which is about as close to the floor as this shape of
check gets. The remaining cost is the ten seconds, and the only thing that
removes it is the guest-side dismount described above.

## Windows ARM64 will not boot under nested KVM

The Linux host was verified on an aarch64 Ubuntu 26.04 guest (QEMU 10.2.1,
`/dev/kvm` present and writable, 8 vCPU) running under Apple's
Virtualization.framework on an M4 Pro. WinQuick builds there, `cargo test`
passes all 168 host tests, and `winquick doctor` reports the host correctly,
including flagging QEMU 10.2 as too old to migrate the NVMe device. What does
not work is the guest: every run ended in silence and, after the timeout, in a
firmware register dump on the serial line.

    Synchronous Exception at 0x000000007C16DDD4
    ASSERT [ArmCpuDxe] .../DefaultExceptionHandler.c(343): ((BOOLEAN)(0==1))

That is edk2's unhandled-exception path, which means the fault happens before
Windows is running at all. `root.qcow2` for the failed run was 197 KB — the
guest never wrote a sector.

Bisecting the machine, one device at a time:

| configuration | result |
|---|---|
| firmware alone, no disks | boots to BdsDxe, then PXE |
| `-device ramfb` added | boots to BdsDxe |
| blank NVMe disk | boots to BdsDxe, `Boot0001` |
| **Validation OS image on NVMe** | **firmware fault** |
| **Validation OS image on virtio-blk** | **firmware fault, same address** |

So it is neither the firmware, nor `ramfb`, nor the NVMe device, nor the disk
transport: it is the Windows boot manager on that image faulting once the
firmware hands control to it. The same image boots on macOS/HVF.

The discriminator is the accelerator. With everything else held constant —
same QEMU, same firmware, same image, same machine type — swapping `-accel kvm`
for `-accel tcg` gets past the fault entirely and into Windows' own boot:

    ConvertPages: range 7B6AE000 - 7B75DFFF covers multiple entries

TCG is not a supported configuration and never will be; it is used here only as
a control. What it proves is that the fault is a property of *nested* KVM under
Apple's hypervisor, not of WinQuick, the firmware, or the QEMU version.

Two things came out of this. WinQuick now reads the serial log when a guest goes
silent and reports the firmware fault, rather than suggesting a longer timeout
that cannot help; and it names running inside another virtual machine as the
usual cause, because that is what was measured here.

One more measurement settles what the layer can and cannot do. Under the same
nested KVM, with the Windows image out of the picture entirely, the host's own
Linux kernel was booted as a guest:

    qemu-system-aarch64 -M virt -accel kvm -cpu host -smp 2 -m 2048 \
      -kernel /boot/vmlinuz-7.0.0-30-generic -initrd ... \
      -append "console=ttyAMA0 earlycon panic=1"

It produced **not one byte** of console output in sixty seconds. `earlycon`
prints before almost anything else in a kernel's life, so this is a fault
before the first line of output — the same place Windows fails, and nothing to
do with Windows. Meanwhile edk2 boots on the same accelerator every time and
reaches its network stack.

So the layer runs the guest firmware and cannot run a guest operating system,
of either family. The exception confirms the shape of it: `ESR 0x02000000` is
EC 0, "unknown reason", which on AArch64 is what an undefined instruction
raises. `-cpu host` under KVM passes the host's ID registers through, so the
guest is told it has every feature an M4 Pro has, executes one, and the layer
underneath does not implement it. TCG has no such gap because it implements
what it advertises. Masking what QEMU allows to be masked -- `pauth=off`,
`sve=off`, both together, `-smp 1`, `virtualization=off` -- changes nothing;
the fault stays at the same address with the same syndrome every time.

`winquick doctor` now reports when it is running inside a hypervisor, because
this is the first thing worth knowing when a guest will not boot and nothing
else on the host says it. It is a note rather than a refusal: nesting works on
plenty of other stacks, and this result is about one of them.

What is still unmeasured is a Linux host on real hardware. Nothing in this
result says anything against one — the failure is in the layer underneath — but
no bare-metal Linux machine was available, and an x86_64 Linux VM on Apple
Silicon would have no KVM at all. Linux is therefore verified as far as build,
tests, tooling and diagnostics, and unverified for guest bring-up.

## The MSVC target cannot be linked, so the Windows archive is a MinGW build

CI's first run failed on Windows, in the linker rather than the compiler:

    LINK : fatal error LNK1181: cannot open input file 'bcryptprimitives.lib'

Reproduced on `windows-2022` with Visual Studio 2022 (MSVC 14.44) and on
`windows-latest`, which had just moved to a `windows-2025-vs2026` image with
Visual Studio 2026 (MSVC 14.51). Same error, both images, both toolsets — so it
is not one bad runner image.

`bcryptprimitives.dll` has no import library in the Windows SDK. It is meant to
be linked through `raw-dylib`, and the reference comes from Rust's own standard
library by way of `ProcessPrng`. Every Rust program with this dependency set
fails the same way on those images; there is nothing in WinQuick to fix, and
nothing WinQuick can fix.

This went unnoticed here for a while because `cargo check --target
x86_64-pc-windows-msvc` passes: it type-checks and never links. Checking a
target is not building it, and the difference is exactly one linker invocation.

The Windows archive has in fact always been a MinGW build — that is what was
built on the lab machine, published, unpacked and run. It was accidental rather
than chosen, because `release-windows.sh` used whichever toolchain happened to
be the default. It is now named explicitly, both there and in CI, so the
archive is the same binary wherever it is built. The staged-tree check already
enforces what matters about it: the executables link against nothing but system
DLLs.

## Desktop session step costs, measured 2026-09-04

The published desktop figures had never been recorded here, and two of them
were wrong. Re-measured end to end through the CLI, so each number includes
host process start and one round trip to the guest bridge.

Reference host: Apple Silicon M4 Pro, macOS 26, QEMU 11.1, prepared desktop
state present. Application: `examples/WpfDemo`, published inside WinQuick.
n = 7 full cycles (`stop` → `start` → `launch` → `wait-window` → `get` →
`click` → `screenshot` → `stop`).

| Step | p50 | min | max |
|---|---|---|---|
| `start --app` | **368 ms** | 367 ms | 419 ms |
| `desktop launch` | **25 ms** | 21 ms | 40 ms |
| `desktop wait-window` (first window drawn) | **644 ms** | 641 ms | 657 ms |
| `desktop get` | **92 ms** | 85 ms | 111 ms |
| `desktop click` | **309 ms** | 299 ms | 323 ms |
| `desktop screenshot --title` | **120 ms** | 114 ms | 130 ms |
| `stop` | **115 ms** | 106 ms | 116 ms |

`click` looked anomalous, so it was decomposed separately (n = 5, warm
session):

| | p50 |
|---|---|
| `click` with the default settle | 299 ms |
| `click --settle 0` | **49 ms** |
| `get` (warm) | **42 ms** |
| `type` (default settle) | 257 ms |

So a UI Automation round trip is ~40–50 ms; the rest is the deliberate
post-action settle wait that `click` and `type` apply so the next read sees a
finished UI. The first `get` after a window appears costs about twice a warm
one.

This corrected two published claims: "~20 ms per UI step" (a click is ~300 ms
with settle, ~50 ms without) and "a window screenshot ~59 ms" (it is ~120 ms).

## A wedged prepared guest could fail a run outright, 2026-09-04

`tests/integration.sh` failed one check, reproducibly, in the sequence that
corrupts `ready.state` and then removes the state directory entirely:

```
FAIL  missing ready state rebuilds automatically -- got [1] want [0]
```

With `--verbose` the run explains itself:

```
no ready state yet
preparing a reusable Windows image (one-off, takes a few seconds)
guest ready after 9.1s
ready state built in 10.8s (599 MiB)
... -incoming file:.../ready.state
the guest has not acknowledged the command yet but has moved 296 MiB
  -- it is working, not halted; waiting for the command instead
the command did not finish within 90 s
```

So the state was built and immediately used, the guest resumed from it never
took the command out of the mailbox, and the run failed. This is the freeze
lottery already documented above: the agent's poll loop mounts the mailbox,
looks and dismounts, and a guest frozen in the wrong part of that comes back
unable to poll.

What made it fatal rather than merely unlucky is that both `warm_execute` call
sites reduced to `Err(e) if command_timed_out(&e) => return Err(e)`. That is
right for a command the guest picked up and then ran too long on — booting cold
to run it again would spend the same timeout for the same answer. It is wrong
for a command still sitting untaken when the timeout fired, which says the
guest ran nothing at all. The distinction already existed
(`timed_out_without_taking_it`, added when one bad freeze wedged the fast path
for eight hours) but only fed a two-strike counter on the *restore* path, and
nothing at all on the *prepare* path.

Two further details hid it:

- `as_command_timeout_if` replaces the error rather than wrapping it, so the
  `GuestSilent` marker is gone by the time the retry loop tests for it. A guest
  that was silent therefore failed `guest_was_silent` and broke out of the loop
  after one attempt, never using the retries that exist for exactly this.
- Under the two-strike rule the restore path cost the user *two* full timeouts
  before anything was discarded. Both were observed: a suite run whose first
  two commands failed and everything after them passed.

Fixed by naming the decision once, as `a_cold_boot_would_help`, and using it in
all three places: keep the error when the guest took the command, and otherwise
discard the state, use the prepare attempts, and fall through to a cold boot,
which still answers the command.

Verified on the sequence that reproduced it — seed a state, corrupt it, run,
remove the state directory, run — three times in a row:

| | before | after |
|---|---|---|
| `winquick run -- cmd /c ver` | exit 1, timed out | **exit 0**, correct output, 3 of 3 |

The pre-change binary was rebuilt from `git stash` and failed the same way, so
this was a defect in v0.4.0 and not a regression introduced alongside it.

### The correction that fix needed, same day

The first version of the fix read "the command was never taken" as proof on its
own. It is not. The acknowledgement is a FAT directory entry the guest writes
and the host reads back out of the image, and a guest that has taken the
command can still look untaken for a second or two while Windows holds that
write. `tests/integration.sh` caught it immediately:

```
FAIL  timeout retried -- took 136s for a 2 s timeout
FAIL  prepared guest discarded by a timeout -- no ready.json
```

`winquick run --timeout 2 -- cmd /c "ping -n 30 127.0.0.1"` expired before the
acknowledgement was visible, was read as a wedged guest, and re-ran the whole
command cold once per prepare attempt — the exact regression the two tests were
written for, reproduced in full.

The evidence is therefore not "untaken" but "untaken *after long enough to
know*". `ACKNOWLEDGEMENT_IS_CERTAIN` is 60 s: far above the ~100 ms a warm
guest needs, far below the 300 s default timeout, so an ordinary run gets the
fallback and a deliberately short timeout keeps its old, fast answer.

| `--timeout 2` on `ping -n 30` | v0.4.0 | first fix | corrected |
|---|---|---|---|
| elapsed | 14 s | **136 s** | 14 s |
| prepared guest | kept | **discarded** | kept |

Both behaviours were then checked directly rather than only through the suite:
the short timeout fails in 14 s with the guest kept, and the wedged-guest
sequence recovers with exit 0 twice in a row.

## Reference figures, re-measured after the recovery fix, 2026-09-04

Every number published on the readme and the website comes from this run.
Reference host: Apple Silicon M4 Pro, macOS 26, QEMU 11.1, prepared guest
present, `dotnet-framework` and `desktop` installed, no other virtual machine
running. Timed from a single persistent harness, so each figure is wall time
for the whole `winquick` process, not just the guest.

| | n | fail | p50 | p95 | p99 | max |
|---|---|---|---|---|---|---|
| `cmd /c ver` | 100 | **0** | **310 ms** | 317 ms | 319 ms | 319 ms |
| `pwsh -Command $PSVersionTable.PSVersion.Major` | 20 | 0 | **690 ms** | 701 ms | 701 ms | 701 ms |
| `cmd /c dotnet --version` | 20 | 0 | **520 ms** | 531 ms | 545 ms | 545 ms |

Repeated once more on an otherwise idle machine to be sure the numbers were
not a warm-cache artefact: p50 309 / 692 / 522, within 2 ms of the above.

Desktop session, n = 7 full cycles against `examples/WpfDemo`:

| Step | p50 | min | max |
|---|---|---|---|
| `start --app` | **338 ms** | 326 ms | 340 ms |
| `desktop launch` | 20 ms | 18 ms | 27 ms |
| `desktop wait-window` | 659 ms | 639 ms | 666 ms |
| `desktop get` (first, after the window appears) | 93 ms | 86 ms | 101 ms |
| `desktop click` (default settle) | 305 ms | 293 ms | 319 ms |
| `desktop screenshot --title` | 124 ms | 111 ms | 137 ms |

Warm, against a session already up (n = 6): `get` 52 ms, `click --settle 0`
54 ms. So a UI Automation round trip is ~50 ms and the rest of a default
`click` is the settle wait.

These supersede the earlier figures of 292 / 684 / 491 ms and a 370 ms desktop
start. Runs measure about 6% slower than that and the desktop start about 8%
faster. The cause was not chased down: both measurements are on the same
machine with the same capabilities installed, so it is host state rather than
anything in the product, and nothing in the recovery fix touches the path a
successful run takes. What is published is what was measured last.

## Guests had a network device the whole time, 2026-09-05

The product guarantee is that a guest has no network. The documentation said so
and `ipconfig` agreed, reporting zero IPv4 adapters, and a ping from the guest
failed. None of that was evidence about QEMU.

QEMU builds a default NIC and a user-mode backend for the machine type unless
told otherwise. Asked directly, with the same machine arguments WinQuick uses:

```
$ qemu-system-aarch64 -M virt ...          (no -nic flag, as WinQuick was)
(qemu) info network
hub 0
 \ hub0port1: #net100: index=0,type=user,net=10.0.2.0,restrict=off
 \ hub0port0: virtio-net-pci.0: index=0,type=nic,model=virtio-net-pci,...
Ethernet controller: PCI device 1af4:1000

$ qemu-system-x86_64 -M q35 ...            (Windows and Linux x86_64 hosts)
 \ hub0port1: #net171: index=0,type=user,net=10.0.2.0,restrict=off
 \ hub0port0: e1000e.0: index=0,type=nic,model=e1000e,...
Ethernet controller: PCI device 8086:10d3
```

So every guest was constructed with a NIC attached to a SLIRP backend. On the
reference host it was never usable: the ARM64 Validation OS guest has no driver
that binds to `virtio-net-pci`, and nothing WinQuick stages would supply one --
the desktop capability stages `viogpudo` and `vioinput`, not `netkvm`.

The x86_64 side was not tested. `q35` supplies an `e1000e` and Windows does ship
an inbox driver for that part, so whether a guest there would have bound it and
reached the network is an open question rather than a measurement. No guest is
known to have had working connectivity, and none was shown not to.

Either way the guarantee rested on the guest image having no matching driver,
which is not something WinQuick controls or checks. That is what was wrong: the
property was documented but not enforced.

Fixed by passing `-nic none` from all three boot paths (run, desktop,
servicing), which suppresses both the device and the backend. Verified after the
change on a live desktop session over QMP:

```
info network        (empty)
network PCI devices none
```

and in the guest: zero IPv4 adapters, `ping` exits 1, and QMP still works
because it is a host-side socket and unrelated.

`qemu::tests::no_guest_gets_a_network_device` builds all three command lines and
asserts `-nic none` is present and that no `-netdev`, `-net`, `virtio-net` or
`e1000` argument appears. Removing the flag from one path fails the test.

## What the ARM64 guest actually runs

Tested rather than assumed, on the ARM64 Validation OS guest.

| | Result |
|---|---|
| Console programs via `winquick run` | 21 of 26 Sysinternals command-line tools ran clean; the rest returned their own status codes, not errors |
| GUI programs in a desktop session | Notepad, Task Manager, Process Monitor, TCPView, VMMap, RamMap and DiskView all open, and each was driven through UI Automation |
| x64 binaries on the ARM64 guest | Run under Windows' own emulation, including a self-contained x64 WPF application |
| Kernel-driver tools | Process Monitor loaded its driver and captured live kernel events |
| GUI programs via `winquick run` | **Fail.** The base runtime has no graphics stack; a desktop session is required |
| Tools needing a Windows service Validation OS lacks | **Fail**, and no package adds one |

None of the programs in the second row are .NET applications. The desktop
capability starts a Windows desktop and launches Windows software; the WPF and
WinForms work is the part that is built *and* driven end to end, not the limit
of what runs.

### A tool that cannot work, and why

Sysinternals' `disk2vhd` needs the Volume Shadow Copy service to snapshot a live
volume. `vssvc.exe` is not on Microsoft's Validation OS media at all, so there
is no package to add and no configuration that helps. It starts and exits
without a window.

Its console sibling `autorunsc`, which only reads the registry, works. The
distinction is what a tool needs from Windows, not whether Sysinternals wrote
it. `winquick run -- sc query <name>` answers it for any tool in a second, and
running the tool under `winquick run` surfaces the loader error when a DLL is
missing.
