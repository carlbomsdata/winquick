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

This runs natively on macOS via `qemu-img`, `hdiutil`, `hivexsh` and `ntfscp`.
The NTFS tooling currently has to be built from source, which is a known rough
edge; see research.md.

## Disposable execution

```
  base.qcow2                 read-only backing file, never written
      |
      +-- root.qcow2         copy-on-write overlay, one per run, then deleted
```

`winquick run`:

1. ensure the runtime exists (otherwise say to run `winquick setup`)
2. `qemu-img create -f qcow2 -b base.qcow2 -F qcow2 <overlay>`
3. build the mailbox image and write the command into it
4. boot headlessly from the overlay
5. the guest agent runs the command and shuts the VM down
6. read stdout, stderr and the exit code out of the mailbox
7. write them to the host's stdout and stderr, translating CRLF to LF
8. delete the entire run directory
9. exit with the guest process's exit code

Three properties are worth being pedantic about, because they are the product:

**The base image is never written.** qcow2 backing files are opened read-only.
A run that destroys Windows destroys one overlay. Verified by SHA-256 across
many runs — that is what makes it safe to hand to an agent.

**Nothing survives a run.** The run directory holds the overlay, the mailbox and
the UEFI variable store, and is removed by a `Drop` guard that fires on success,
error and panic alike. Set `WINQUICK_KEEP=1` to keep it for debugging.

**Streams and exit codes pass through.** stdout and stderr stay separate and are
never interleaved or prefixed; the Windows exit code becomes the CLI's exit code.
The one deliberate transformation is CRLF → LF, so that piping into `grep` on the
host behaves.

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

Note that `dotnet test` specifically needs far more than a workspace — Validation
OS as shipped has no .NET and no PowerShell. See the open questions in
research.md.

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
