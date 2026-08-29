# Patches

Changes WinQuick makes to third-party sources, kept here with the evidence for
why each exists.

Two of them are applied by the build recipes and end up in binaries WinQuick
ships -- `ntfsprogs-windows.patch` and `hivex-windows.patch`. The QEMU one is
not applied to anything WinQuick ships; it is what a user builds for themselves
if they want the fast path on Windows.

| Patch | Applied by | Shipped |
|---|---|---|
| `ntfsprogs-windows.patch` | `scripts/build-ntfs-helpers.sh` | yes, both hosts |
| `hivex-windows.patch` | `scripts/build-hivex-windows.sh` | yes, Windows only |
| `whpx-stop-and-copy.patch` | nothing | no |
| `whpx-resume-diagnostics.patch` | nothing | no |
| `whpx-nmi-delivery.patch` | nothing | no |
| `whpx-activity-state-migration.patch` | nothing | no |
| `whpx-hyperv-synthetic-migration.patch` | nothing | no |
| `whpx-lapic-timer-migration.patch` | nothing | no |

The five QEMU patches stack in this order, and
applying them to a pristine **QEMU v11.1.0** (`84f0721`) reproduces the tree
these measurements were taken on, byte for byte:

```console
$ patch -p1 -i patches/whpx-nmi-delivery.patch
$ patch -p1 -i patches/whpx-stop-and-copy.patch
$ patch -p1 -i patches/whpx-activity-state-migration.patch
$ patch -p1 -i patches/whpx-hyperv-synthetic-migration.patch
$ patch -p1 -i patches/whpx-lapic-timer-migration.patch
```

## `ntfsprogs-windows.patch`

Against **ntfs-3g/ntfsprogs 2022.10.3**. Seven files, ~165 lines. It does two
separate jobs.

**Addressing a partition without mounting anything.** `ntfscp` and `ntfscat`
normally want a partition device node. macOS can produce one with
`hdiutil attach -nomount`; Windows cannot without elevation and a virtual-disk
driver, and endpoint security software blocks that route in practice -- on the
validation host, `Mount-DiskImage` fails with a CIM `PermissionDenied` and
`diskpart attach vdisk` with *Access denied*, both from a verified elevated
context with the Virtual Disk Service running and `vhdmp.sys` present.

Every access in `unix_io.c` is a seek or a positioned read/write, so an
`NTFS_IMAGE_OFFSET` environment variable shifts them all by a fixed base and
"image plus offset" behaves exactly like the partition node does elsewhere.
Unset, it is zero and nothing changes -- which is why both hosts now use it and
macOS no longer attaches anything either.

**Making the Windows build correct.** Five of the seven files are ordinary
portability fixes, and one of them is a real bug worth stating plainly:

> `include/ntfs-3g/compat.h` defines `__attribute__` away on Windows. That also
> discards `__attribute__((packed))` on every on-disk structure.
> `NTFS_BOOT_SECTOR` grows from 512 to 520 bytes, `oem_id` lands at the wrong
> offset, and every NTFS volume reports *"NTFS signature is missing"*.

The rest: the image, `ntfscat`'s stdout and `ntfscp`'s source file are opened in
binary mode, because the C runtime otherwise rewrites `0x0A` and treats `0x1A`
as end of file -- a 6 MB registry hive grew by exactly its newline count on
every read/write cycle until this was fixed; the file-based device operations
are selected instead of the physical-drive ones; upstream's Windows
format-translation macros get `##` so zero-argument calls compile; and `dir.c`
gets an explicit union cast because GCC ignores `transparent_union` on the
Windows ABI.

Measured with this patch: a 6 MB `SOFTWARE` hive read out of a 32 GB image,
edited, written back and read again **byte-identical over two cycles**, in
153 ms, with no mounting, no elevation and no endpoint-security exception.

## `hivex-windows.patch`

Against **hivex 1.3.24**. One file, one hunk.

Upstream excludes `hivexsh` from Windows builds entirely, and the reason is
narrow: `set_prompt_string()` composes the interactive prompt with
`open_memstream`, which is POSIX and which mingw does not have. On Windows the
prompt becomes a fixed `"> "`. Nothing else changes, and WinQuick drives
`hivexsh` from a script file, where no prompt is ever printed.

## `whpx-stop-and-copy.patch`

Against **QEMU v11.1.0** (`84f0721e0e`). Two changes, 46 insertions across two
files, so that a *stopped* guest can be saved and restored under WHPX.

**`target/i386/whpx/whpx-all.c`** — WHPX registered an unconditional migration
blocker at vCPU init, which refused every form of state save. Its stated reason
is missing dirty-page tracking, and that is the right reason to refuse *live*
migration. It is not a reason to refuse a stopped guest: `migration/ram.c`
starts with every page marked dirty, so the first pass already copies all of
RAM, and a guest that is not executing cannot dirty a page afterwards. The
blocker now goes on when the guest runs and comes off when it stops, using the
vm-state-change hook the backend already installs.

The blocker's other claim — "some system register/state save-restore" — is
stale: `whpx_get_registers()` and `whpx_set_registers()` already carry XSAVE
through `WHvGet/SetVirtualProcessorXsaveState`.

**`io/channel-file.c`** — two Win32 bugs, either of which alone stops
`migrate file:` working there regardless of accelerator:

- `qio_channel_file_set_blocking()` was a `/* not implemented */` stub that
  always failed. A regular file is always ready, so the call is now a no-op.
- File channels were opened in the CRT's **text mode**, which translates CRLF
  and treats `0x1A` as end of file. A migration stream is binary: it was written
  mangled and then failed to load with `Failed to load vmstate ... ret: -5`.
  `O_BINARY` is now set. This is a plain bug fix and the most clearly
  upstreamable of the three.

### Measured with this patch

Windows 11 Pro 25H2 (26200), i5-8265U, Validation OS x64 26100.8972:

- `stop` then `migrate` under WHPX: **completed**, ~147 MB state, ~2.2 s
- the same immutable state restored into **20 fresh QEMU processes** over the
  **native `file:` transport**: 20/20, p50 **962 ms**, hash unchanged, zero
  orphans. (An earlier measurement of 1.99 s went through a relay, which the
  `O_BINARY` fix made unnecessary.)
- a real command through the guest: `cmd /c ver` →
  `Microsoft Windows [Version 10.0.26100.8972]`, exit code 0

### Upstreamability

The whpx change is small and self-contained and looks upstreamable as-is. Note
that an active upstream series (v3, April 2026, "WHPX x86 updates for QEMU
11.1") already landed XSAVE handling and reworded this blocker; a real
submission should be rebased on that rather than duplicating it. A 2022 attempt
at WHPX save/restore was never merged — it foundered on converting WHPX's
*compacted* XSAVE layout to QEMU's standard one, which matters for cross-host
migration and not for restoring on the same machine.

The `channel-file.c` change is a behaviour fix rather than a feature and would
need review from someone who knows whether any caller depends on real
non-blocking semantics there.

## `whpx-resume-diagnostics.patch`

Against **QEMU v11.1.0**, on top of `whpx-stop-and-copy.patch`. Lab
instrumentation, not a fix and not shipped.

Everything it adds is behind `WHPX_DIAG=1` and silent otherwise. It tallies, per
virtual processor, how many times `WHvRunVirtualProcessor` is entered and with
which exit reason it returns; how often registers and the local APIC are written
back; whether QEMU's userspace interrupt injection is used; and every interrupt
handed to the hypervisor, by vector.

That accounting is what turned "the guest does not resume" into "every exit on
every processor is `Canceled`, and only when the partition has more than one
processor" — see [../docs/whpx-resume.md](../docs/whpx-resume.md).

## `whpx-nmi-delivery.patch`

Against **QEMU v11.1.0**, on top of `whpx-stop-and-copy.patch`. 69 lines across
two files. Not applied to anything WinQuick ships -- it is here because it is a
real bug worth reporting, and because the WHPX resume investigation could not
proceed without it.

`inject-nmi` does nothing at all on a WHPX guest, for two independent reasons:

- **`whpx_apic_external_nmi()` is an empty function.** With an APIC enabled --
  which is always -- `x86_nmi()` delivers through the APIC rather than raising
  `CPU_INTERRUPT_NMI` directly, so every externally injected NMI reached that
  stub and stopped. The fix honours how the guest programmed LINT1, as KVM's
  equivalent does, then raises the interrupt.
- **A prepared interruption is only committed for one APIC mode.** In
  `whpx_vcpu_pre_run()` an NMI is built into `new_int` near the top -- taking
  `CPU_INTERRUPT_NMI` off the CPU as it goes -- but the block that writes it
  into `WHvRegisterPendingInterruption` sits inside the
  `if (!whpx_irqchip_in_kernel())` arm. With the in-hypervisor APIC the work
  was done and discarded.

With both fixed, an NMI injected into a restored-and-frozen guest makes it
execute again, which is how [../docs/whpx-resume.md](../docs/whpx-resume.md)
establishes that the processors themselves are fine and only the wake-up is
missing.

## `whpx-activity-state-migration.patch`

Against **QEMU v11.1.0**, on top of `whpx-stop-and-copy.patch`. 129 lines, one
file. Not applied to anything WinQuick ships.

`WHvRegisterInternalActivityState` holds a processor's `StartupSuspend`,
`HaltSuspend` and `IdleSuspend` bits. It is absent from `whpx_register_names`,
the only code that touches it is `whpx_vcpu_kick_out_of_hlt()`, and QEMU
registers no vmstate for WHPX at all -- so it is not carried across a
migration. A fresh partition parks every application processor in
`StartupSuspend`, waiting for the INIT/SIPI that brings it up. Correct for a
cold boot; wrong for a restore, where the guest sent that sequence long ago in
another process and will never send it again. The processor waits for ever, and
a multiprocessor guest deadlocks as soon as it needs the second one.

This registers one vmstate section per processor that reads the register on
save and writes it back on load.

**The application matters as much as the value.** Doing it in the vmstate
`post_load` is wrong: that runs while the stream is still being read, before
`cpu_synchronize_post_init()` has pushed the processor's architectural state,
so releasing it from `StartupSuspend` there starts it executing from whatever a
fresh VP holds. The result is intermittent. The value is instead remembered at
load and applied at the end of the full-state push.

With this applied, the restored application processor starts every time. It is
necessary and not sufficient: what the started processor then runs into is the
subject of `whpx-hyperv-synthetic-migration.patch` below.

An unmerged 2022 upstream patch,
[*whpx: Added support for saving/restoring VM state*](https://patchew.org/QEMU/004101d86732$0d33bd70$279b3850$@sysprogs.com/),
saves the same single register for the same reason. It was never merged;
review foundered on the XSAVE half of it.

## `whpx-hyperv-synthetic-migration.patch`

Against **QEMU v11.1.0**, on top of `whpx-activity-state-migration.patch`. 230
lines, one file. Not applied to anything WinQuick ships.

A WHP partition always presents the Hyper-V hypervisor interface to its guest,
because the thing underneath really is Hyper-V's hypervisor. Windows takes the
offer up. It writes `HV_X64_MSR_GUEST_OS_ID` and `HV_X64_MSR_HYPERCALL`, the
hypervisor overlays the guest page named by the second one with hypercall code,
and from then on the guest calls into that page instead of sending IPIs --
`nt!HvlFlushRangeListTb`, the enlightened remote TLB flush, is the path that
matters here.

**That overlay belongs to the partition, not to guest RAM.** The migration
stream faithfully carries the *underlying* bytes of the page, which are filler.
A restored partition has `GuestOsId` zero and no overlay, so the first
enlightened remote TLB flush jumps into the filler and executes it. In the dump
recovered from ROAD-WARRIOR01 the filler is `0xAF` repeated, which decodes as
`scas dword ptr [rdi]`, and `rdi` holds a leftover hypercall argument of `0xa`:

```
nt!MiFlushTbList -> nt!HvlFlushRangeListTb -> nt!HvcallFastExtended
  -> nt!HvcallpExtendedFastHypercall+0x51
  -> 0xfffff804`36390000            the hypercall page, now filler
  -> nt!KiPageFault -> nt!KeBugCheckEx

DRIVER_IRQL_NOT_LESS_OR_EQUAL (0xD1)
  P1 = 0xa                 the address read
  P2 = 0x2                 IRQL, DISPATCH_LEVEL
  P3 = 0x0                 a read
  P4 = 0xfffff804`36390000 the instruction that read it
```

**Only multiprocessor guests reach it**, which is why one vCPU restored happily
for weeks while two never did: `HvlFlushRangeListTb` is the *remote* flush, and
a single processor has nobody to flush.

The patch extends the per-processor vmstate section added by
`whpx-activity-state-migration.patch` to carry `GuestOsId`, `Hypercall`,
`VpAssistPage` and `ReferenceTsc` -- the last two are overlays with the same
lifetime problem. Order is load-bearing twice over: `GuestOsId` must be written
before `Hypercall`, because the hypervisor will not establish the overlay while
it is zero; and the activity state stays last, because clearing `StartupSuspend`
is what makes a processor start executing and it must not do that before the
overlay is back. Windows writes both MSRs from the boot processor, which is
restored before any application processor is released.

Reading and writing these registers is best-effort. A platform that does not
have them fails the access, the fields come across zero, and the restore behaves
exactly as it did before the patch.

## The SynIC, which was tried and is not here

Carrying the synthetic interrupt controller across a restore looked like the
answer to the second failure -- a restored guest that halts and is never woken.
The guest does have it switched on (`Scontrol = 1`, a message page per
processor at `Simp = 0x19001` / `0x1a001`, two hundred bytes of synthetic timer
state), and none of it crosses the migration.

It was implemented -- `Scontrol`, `Sversion`, `Simp`, `Siefp`, `Sint0`-`Sint15`
and the three blobs `WHvGet/SetVirtualProcessorState` exposes -- and measured:

| `winquick run --cpus 2 -- cmd /c ver` | guest exec + mailbox sync | total |
|---|---|---|
| without it | 520, 540, 656, 656, 521 ms | 15.8 - 22.9 s |
| with it | 44,945 and 47,394 ms | 77.9 s |

Eighty-six times slower, on the same host and the same guest, and the halting
carried on at about the same rate. So there is no patch here.

The likely reason is worth keeping: a synthetic timer's expiry is a *partition
reference-time* value, and a fresh partition starts its reference clock over, so
a restored expiry lands an arbitrary distance into the new partition's future.
Carrying it correctly would mean translating every expiry into the destination's
timeline, which needs a reference-time reading on both sides that WHP does not
obviously expose. See [../docs/whpx-resume.md](../docs/whpx-resume.md).

## `whpx-lapic-timer-migration.patch`

Against **QEMU v11.1.0**, on top of `whpx-hyperv-synthetic-migration.patch`.
87 lines, one file. Not applied to anything WinQuick ships.

`whpx_put_apic_state()` moves the local APIC through WHP as a flat array of
registers. It `memset`s the block and then fills in field `0x38`, the timer's
initial count, and `0x3e`, its divide configuration. It never fills in field
`0x39`, **the current count** -- so every restored processor was told its timer
had already run out. `whpx_get_apic_state()` is the same story in reverse: it
reads the initial count back and then sets `initial_count_load_time` to *now*,
throwing away however much of the countdown was left.

Measured on a restored guest, against a cold-booted one:

```
restored  vp0  initial=10000000  current=0        <- and it never moves
cold boot vp0  initial=10000000  current=7093960 -> 6290540 -> 4981920 ...
```

With the current count written, `WHvSetVirtualProcessorInterruptControllerState2`
does arm the countdown -- a restored `vp0` was watched decrementing for thirty
nine seconds. So this is real per-processor state that WHP owns and nothing was
carrying, of exactly the same kind as the hypercall page.

The remaining time is preserved without adding any migration state: QEMU already
carries the timer as an initial count plus the time that count was loaded, so
back-dating the load time by however much has elapsed is enough, and the
destination puts a real current count back into field `0x39`.

**It is not, on its own, the fix for a halted multiprocessor restore.** By the
time Windows has taken up the Hyper-V enlightenments it has moved its clock to
vector `0xd8` and set the LVT's mask bit -- `lvt_timer=0x000300d8` -- so the
local APIC timer delivers nothing however well it counts. The patch is here
because dropping the state was wrong, and because measuring it is what ruled the
timer out.
