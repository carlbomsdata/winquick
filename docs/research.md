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
but adding them requires Microsoft's `GenImage.cmd`, **which needs a Windows 11
host with DISM**. There is no macOS path to it. This is the single biggest open
problem for anything beyond `cmd.exe` workloads.

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
| `hdiutil` | mount ISO, attach raw image | built in |
| `hivexsh` | edit the offline `SOFTWARE` hive | `brew install hivex` |
| `ntfscp` / `ntfscat` | read and write files in the NTFS partition | **must be built from source** |

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
