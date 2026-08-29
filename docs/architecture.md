# WinQuick architecture

This describes what v0.1 actually does, and where it is heading. Measurements and
the reasoning behind each choice are in [research.md](research.md).

## Shape

```
   Claude / human / CI / script
              |
              v
        winquick CLI                    (Rust, single native binary, macOS arm64)
              |
              +---- spawn / wait -----> qemu-system-aarch64   (separate process)
              |                                 |
              |                              -accel hvf
              |                                 |
              +---- FAT mailbox disk ---> Validation OS guest
                                                |
                                                +-- cmd.exe agent (AutoRun hook)
                                                +-- stdout / stderr / exit code
```

Two disks and no network. The root disk is a disposable copy-on-write overlay;
the mailbox disk carries the command in and the results out. Nothing else
crosses the boundary — no SSH, no WinRM, no RDP, no listening ports, no shared
folders, no IP addresses to manage.

QEMU is spawned as a child process and never linked into the WinQuick binary.
That is a licensing boundary (QEMU is GPLv2) as much as a stability one.

## Host requirements

Apple Silicon macOS only. `-accel hvf` runs guest ARM64 code directly on the
host's ARM64 cores through Apple's Hypervisor Framework, so there is no
instruction emulation. This is the entire reason the product is viable; the same
design under TCG would be far too slow to be interesting.

That also fixes the guest architecture: it must be ARM64 Windows.

## Machine configuration

```
qemu-system-aarch64
  -M virt -accel hvf -cpu host -smp 4 -m 2048
  -drive if=pflash,format=raw,readonly=on,file=edk2-aarch64-code.fd
  -drive if=pflash,format=raw,file=<per-run vars>
  -drive if=none,id=root,file=<overlay>.qcow2,format=qcow2
  -device nvme,drive=root,serial=wqroot
  -drive if=none,id=mbox,file=mailbox.img,format=raw,cache=writethrough
  -device nvme,drive=mbox,serial=wqmbox
  -device ramfb -display none -vga none
  -rtc base=localtime -no-reboot
```

**UEFI.** ARM64 Windows boots via UEFI only. EDK2's `edk2-aarch64-code.fd` ships
with QEMU. The variable store is regenerated per run, so even firmware state is
disposable.

**NVMe, not virtio-blk.** Validation OS has `stornvme` as a boot-start driver, so
an NVMe root disk needs no third-party driver at all. virtio-blk would require
`viostor` to be present before the guest can read its own boot volume — a
chicken-and-egg problem at the worst possible moment. NVMe costs a little
throughput and buys a bring-up with zero driver injection.

**Headless with a framebuffer.** `-display none` means no window ever appears.
`ramfb` still gives the guest a display device, which Windows expects to find.

## Guest control channel

### Where this landed, and why

QEMU Guest Agent was the obvious candidate and does not work: virtio-win ships
`qemu-ga` for i386 and x86_64 only — there is no Windows ARM64 build — and it is
delivered as an MSI, which Validation OS cannot install because it has no Windows
Installer service.

virtio-serial is the right destination and is *nearly* available: the ARM64
`vioser.sys` in virtio-win 0.1.285 is genuinely ARM64 and carries a real
Microsoft WHQL signature, and Validation OS has the KMDF it depends on. What
Validation OS does not have is a user-mode PnP service, `pnputil` or `drvload`,
so there is no way to install an INF-based driver — in the guest, at any point.
Getting there means registering the driver offline from the host via
`CriticalDeviceDatabase`, plus writing a compiled guest agent. Both are real
work; neither is done.

### What v0.1 uses: a FAT mailbox disk

A second NVMe device backed by a 64 MiB MBR-partitioned FAT32 image.

| Host writes, before boot | Guest writes, before shutdown |
|---|---|
| `WQMARK.TXT` — volume marker | `WQOUT.TXT` — stdout |
| `WQCMD.CMD` — the command | `WQERR.TXT` — stderr |
| | `WQCODE.TXT` — exit code |

The guest agent is about twenty lines of `cmd.exe` batch, hooked in through
`SOFTWARE\Microsoft\Command Processor\AutoRun` so it runs the moment the shell
starts. It probes drive letters for the marker file, runs the command in a
*child* `cmd.exe` with stdout and stderr redirected, records `%errorlevel%`, and
calls `shutdown /s /t 0 /f`.

Both halves use only inbox components — NVMe, FAT, `cmd.exe`, `shutdown.exe`.
No third-party drivers, no compiled guest code, nothing to install.

**The honest cost: no streaming.** Output arrives when the VM shuts down, and one
boot runs one command. For `winquick run -- <command>` that is exactly the right
shape, which is why this is acceptable for v0.1 and not merely expedient. It
stops being acceptable the moment someone runs a four-minute test suite and
watches nothing happen — which is why virtio-serial stays the target.

The child-`cmd.exe` detail matters more than it looks: with `call`, a workload
ending in `exit` would terminate the agent before it could record anything, and
the VM would hang until the host timeout.

## Image pipeline

The user obtains a Validation OS ARM64 ISO from Microsoft. `winquick setup` then
transforms it, entirely locally:

```
  <user's VALIDATIONOS.iso>              never modified, never redistributed
        |
        |  ValidationOS.vhdx  (964 MiB, a complete bootable GPT disk)
        v
     raw image
        |
        |  + Windows\System32\wqagent.cmd
        |  + SOFTWARE hive: Command Processor\AutoRun -> the agent
        v
  ~/.winquick/images/validation-arm64/base.qcow2   (763 MiB, pristine hereafter)
```

Two writes. Nothing else is changed — no drivers injected, no BCD edited, no
packages added. The stock VHDX already boots under QEMU/HVF unmodified.

This runs through `qemu-img`, `ntfscp` and `hivexsh`, and it **mounts nothing**.
The helpers are pointed at the image file plus the byte offset of the Windows
partition, which WinQuick reads out of the GPT itself. Microsoft's ISO is read
the same way, by a small UDF reader (`src/udf.rs`), rather than being mounted.

That matters on both hosts: macOS could attach an image with
`hdiutil -nomount`, but Windows cannot without elevation and a virtual-disk
driver that endpoint security software routinely blocks. One code path, no
privileges, and no mount left behind for the next run to trip over.

The NTFS tooling and `hivexsh` are built from upstream sources; see
[`scripts/`](../scripts/) and [`patches/`](../patches/).

## Two execution paths

```
                      winquick run -- <command>
                                |
                      valid prepared guest?
                        /                \
                     yes                  no
                      |                    |
              clone + resume        boot from base image
              (~225 ms)             wait for the agent
                      |             freeze it  -> prepared guest
                      |                    |
                      +---------> clone + resume
                                           |
                                  (if that fails: boot and run directly)
```

The cold path is not just a fallback — it is how the prepared guest gets built,
so the slow route happens once and pays for every run after it.

### Warm path

1. clone the prepared guest's four files (APFS clone, ~4 ms regardless of size)
2. write the command into the cloned mailbox
3. start QEMU with `-incoming file:<state>`; it loads RAM and device state paused
4. `cont`
5. the agent's wait loop remounts the mailbox, sees the command, runs it
6. the agent writes stdout, stderr and the exit code, then dismounts to flush
7. kill QEMU, delete the clones

Measured phase breakdown of a 229 ms run: prep 4 ms, QEMU spawn 31 ms, state
restore 103 ms, guest execution and mailbox synchronisation 80 ms, teardown 7 ms.

### Cold path

Boot the base image, wait for the agent's `WQREADY.TXT`, `stop`, migrate RAM and
device state to a file, and keep the root overlay, UEFI varstore and mailbox as
they were at that instant. About 11 seconds, once.

Note the cold path no longer waits for Windows to shut down — the host kills QEMU
once the exit code has been written, which removes ~1.4 s.

**Migration, not `savevm`.** `savevm` requires every writable block device to
support snapshots, which the raw mailbox does not, and it chooses which device
stores the state — putting it in the UEFI varstore, where `loadvm` crashed QEMU.
Migration has no such requirement. See docs/research.md.

**The UEFI variable store must stay writable.** Making it read-only satisfies
`savevm`'s constraint and silently prevents Windows from booting at all.

## The prepared guest

```
~/.winquick/states/validation-arm64/
    ready.state         RAM + device state (~415 MiB)
    ready-disk.qcow2    root overlay at the freeze instant (~40 MiB)
    ready-vars.fd       UEFI variable store at that instant
    ready-mailbox.img   mailbox at that instant
    ready.json          fingerprint
```

All four restore together. RAM restored against a different disk is not a
slightly-wrong VM, it is an undefined one.

`ready-mailbox.img` is there because the guest re-reads the mailbox by dismounting
it and re-creating the mount point from its **volume GUID**, which is derived from
the filesystem. Reformatting the mailbox between runs would change that GUID and
the guest could never mount it again — so it is formatted once, here, and cloned
from then on.

### Invalidation

`ready.json` fingerprints everything the frozen state depends on: WinQuick
version, mailbox protocol version, base image, guest agent, QEMU binary, UEFI
firmware, guest RAM, vCPU count, machine type and device topology.

Large files are identified by length, mtime and inode rather than hashed. Hashing
a 763 MiB base image costs several times the entire warm-run budget, and the case
that matters — `setup` rewriting the image — changes all three.

The guest agent is a special case: it lives *inside* the base image, so changing
it needs a `setup` rebuild rather than just a new prepared guest. `setup` records
the agent's hash beside the image and `run` checks it, so a mismatch produces a
clear instruction instead of a mysterious hang.

### Failure handling

Warm execution must never make WinQuick fragile, so failures are layered:

1. stale fingerprint, missing file, wrong-sized state, failed restore, unresponsive
   guest → discard the prepared guest, rebuild it, run
2. that fails too → boot and run directly, with no prepared guest involved

Every step is reported under `--verbose`; default output stays clean.

## Disposable execution

```
  base.qcow2                 read-only backing file, never written
      |
      +-- ready-disk.qcow2   frozen guest, written once
              |
              +-- root.qcow2 per-run clone, deleted afterwards
```

`winquick run` guarantees three things, and they are the product:

**The base image is never written.** qcow2 backing files are opened read-only.
Verified by SHA-256 across many runs.

**Nothing survives a run.** The run directory holds the overlay, mailbox and UEFI
variable store, and is removed by a `Drop` guard that fires on success, error and
panic alike. Set `WINQUICK_KEEP=1` to keep it for debugging. Because every run
starts from a clone of the same frozen guest, filesystem, registry and environment
changes are all discarded — tested explicitly.

**Streams and exit codes pass through.** stdout and stderr stay separate and are
never interleaved or prefixed; the Windows exit code becomes the CLI's exit code.
The one deliberate transformation is CRLF → LF, so piping into `grep` behaves.

## The command surface

```
winquick setup                          build the runtime from Microsoft's image
winquick run -- <command>               execute, return stdout/stderr/exit code
winquick capability install|remove|list optional tooling inside Windows
winquick cache sync|info|clear          offline packages for dotnet
winquick doctor | info | reset | clean  diagnose, inspect, rebuild, tidy up
```

`run` needs only QEMU. `setup` additionally needs `ntfscp`/`ntfscat` (shipped
with WinQuick) and `hivexsh` (Homebrew), because macOS cannot write NTFS and has
no notion of a Windows registry hive. All three are separate processes, which is
a licensing boundary as much as a design one.

## The desktop path

`winquick desktop` and `winquick ui-test` run against a second image, built by
`winquick capability install desktop`, that has Windows' optional desktop
packages applied and a display driver staged into it. The topology differs from
a `run` guest in three ways:

* a VirtIO GPU instead of `ramfb`, because Validation OS has no inbox driver for
  a plain framebuffer;
* USB keyboard and tablet, so synthetic input has real devices to come from;
* a **partitionless raw disk** carrying the session's control channel.

That last one is the load-bearing difference. A `run` guest gets its command
through the FAT mailbox, which works because only one side touches the volume at
a time. A session has the host writing while Windows still has the volume
mounted, and two FAT implementations sharing allocation tables corrupt them.
Windows will not mount a partitionless fixed disk, so it never caches one
either; both sides read and write whole sectors, payload first and header last.
See [desktop.md](desktop.md).

Capability volumes are cloned per session, exactly as they are per run — Windows
writes to a volume when it mounts it, and the installed images must not carry a
session's fingerprints.

A session does not boot. It restores a prepared desktop state, frozen with the
bridge already answering, in the same way `winquick run` restores its prepared
guest: about 380 ms instead of the 9.3 seconds booting took. Preparing that
state costs ~17 s, once, and it is rebuilt whenever anything about the machine
changes. The bridge and the application sit on separate volumes because the
application's contents change per session and refreshing the guest's view of a
volume means dismounting it — which is not an option for the volume the bridge
is executing from.

## Why output is not streamed, and the guest has no network

Both come down to the same thing: the base runtime deliberately contains no
third-party driver and no compiled WinQuick code. Everything crosses the
boundary through inbox components — NVMe and FAT — driven by a batch agent.

That is enough to hand a command in and results out, and not enough for a live
channel. Windows only synchronises a FAT volume with the disk beneath it at
mount and dismount, so the host cannot watch a file the guest is still writing;
that is exactly the incoherency that forced the desktop session onto a
partitionless raw disk. The two alternatives both break the constraint:

* **A serial port.** QEMU's `virt` machine has a PL011 and Windows enumerates it
  as `ACPI\ARMH0011`, but no driver binds and `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM`
  is empty, so there is no `COM1` to write to.
* **A raw control disk**, as desktop sessions use. That works, and needs a
  compiled program in the guest to poll it.

Networking is the same story with a different device: attaching
`virtio-net-pci` does enumerate `PCI\VEN_1AF4&DEV_1000`, and Windows binds
nothing to it, so `ipconfig` reports no adapters. Red Hat's `netkvm` driver for
ARM64 exists and could be staged with `dism /Add-Driver`, exactly as the display
driver is — but only the *desktop* image is serviced today, so this would mean
adding a servicing pass to `winquick setup` and making the virtio-win media a
dependency of basic installation.

Both are therefore deferred rather than impossible, and the route for each is
known.

## Concurrency and interruption

Several `winquick run` invocations can proceed at once: each gets its own run
directory, its own QEMU, and its own clones of every writable volume. Nothing is
shared that a run can write.

Operations that *change* shared state — setup, capability changes, cache sync,
clean — take an exclusive lock and say so if they have to wait. Building the
prepared guest takes a second lock held across both the check and the build, so
concurrent cold starts cannot read a state another process is still writing.

Ctrl-C kills the VM and removes the run directory, exiting 130. This needs an
explicit signal handler: Rust's default SIGINT terminates the process without
running any `Drop`, which would leave a VM holding a gigabyte of RAM. The handler
does only async-signal-safe work — it records the interruption and signals the
child — and the main thread unwinds normally on its next poll. Interruption is
deliberately *not* treated as a recoverable warm-path failure, which would
otherwise start a second VM.

## What is permanent, what is not

Five different lifetimes, and keeping them straight is most of the design:

| | Lifetime | Written by |
|---|---|---|
| **Base image** | permanent, immutable | `winquick setup`, once |
| **Capabilities** (PowerShell, .NET, package cache) | permanent, replaceable | `winquick capability` / `cache sync` |
| **Prepared guest** | until something it depends on changes | rebuilt automatically |
| **Workspace** | one run | copied in from the host, never back |
| **Artifacts** | explicit | copied out on request |

Capabilities and the package cache are attached as extra disks, **cloned per
run**, so anything the guest writes to them is discarded. That is what stops an
untrusted build script from using the package cache as a persistence channel: only
host-side tooling ever writes the canonical copies.

## Workspace

Not implemented. The target is:

```console
cd MyProject
winquick run -- dotnet test     # project visible at C:\workspace
```

Candidates, in order of how much they are worth trying: a generated read-only
disk image attached as a third device (simple, fast, one-way — and the mailbox
already proves the mechanism); agent-based file transfer once a real agent
exists; virtiofs eventually, though Windows ARM64 `viofs` support is the weakest
link. SMB and shared folders are explicitly not assumed.

Note that `dotnet test` specifically needs far more than a workspace —
Validation OS as shipped has no .NET, no .NET Framework and no PowerShell. Each
of those is a capability now: `dotnet-sdk` and `powershell` are volumes,
`dotnet-framework` is serviced into the image. See
[dotnet.md](dotnet.md).

## Licensing boundaries

Two obligations, deliberately kept apart.

**Microsoft.** WinQuick redistributes no Microsoft software of any kind. The
Validation OS licence forbids sharing, publishing or distributing the software
(§2(e)) and imposes confidentiality obligations (§13). The user downloads the ISO
from Microsoft and accepts Microsoft's terms directly. Images WinQuick generates
are derived from Microsoft software and therefore never leave the user's machine:
they live under `~/.winquick`, are not uploaded, and are never release artifacts.
`.gitignore` refuses `*.iso`, `*.wim`, `*.qcow2` and friends as a backstop, not as
the primary control.

**QEMU.** GPLv2. WinQuick invokes it as an external executable and never links
against it. If a WinQuick distribution bundles a QEMU build, that build ships as a
separate component with its licence text, copyright notices and
corresponding-source availability intact.

**virtio-win, ntfsprogs, hivex.** Not vendored today; fetched or built by the
user. If any of them is ever bundled, it ships under the same posture as QEMU —
separate component, licence intact.
