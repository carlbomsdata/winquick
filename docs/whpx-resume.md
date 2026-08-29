# Why a restored Windows guest does not run under WHPX

WinQuick is fast because it does not boot Windows: it restores a prepared state
into a fresh QEMU process. On Windows that did not work, and this is the
investigation into why.

**Status: two pieces of per-processor state found and fixed, one failure mode
still open.**

1. `InternalActivityState` parked every restored application processor in
   `StartupSuspend` for ever, so it never executed an instruction.
   ([patch](../patches/whpx-activity-state-migration.patch))
2. With the processor running, the guest bugchecked --
   `DRIVER_IRQL_NOT_LESS_OR_EQUAL`, reading address `0xa` at `DISPATCH_LEVEL` --
   because the **Hyper-V hypercall page** is an overlay owned by the partition
   and a fresh partition has none.
   ([patch](../patches/whpx-hyperv-synthetic-migration.patch))
3. **Still open.** With the crash gone, some restored guests resume, run for
   about two seconds and then halt for ever. The synthetic interrupt controller
   looked like the answer and is not: migrating it was tried, measured, and made
   things much worse. See [below](#the-synic-was-not-the-answer).

The one-word summary of the two that are fixed: **WHP keeps state for a virtual
processor that is not a register, and none of it is in `whpx_register_names`.**

Measured on ROAD-WARRIOR01: Windows 11 Pro 25H2 (build 26200), Intel i5-8265U,
Windows Hypervisor Platform enabled, QEMU 11.1.0 (`84f0721`) carrying
[`patches/whpx-stop-and-copy.patch`](../patches/whpx-stop-and-copy.patch),
guest Microsoft Validation OS x64 26100.8972, `-cpu Nehalem`.

## The one-line result

> A restored multiprocessor Windows guest crashed because WHP's Hyper-V
> hypercall page is a partition-owned overlay, not guest RAM, and the migration
> stream cannot carry it. One vCPU never noticed, because the only thing the
> guest used the hypercall page for was flushing *other* processors' TLBs.

## The crash, read properly

The decisive evidence is the guest's own crash dump. Getting at it needed no
debugger inside the guest and no change to the image:

1. Restore the prepared guest at `-smp 2` and leave it completely alone. Poll
   the overlay for the `PAGEDU64` signature -- a dump appears 2.7 s after
   `cont`, every time. The same restore at `-smp 1` never produces one, which
   is the control that makes the result mean something.
2. `qemu-img convert -O raw` the overlay, scan it for `PAGEDU64`, and read the
   `DUMP_HEADER64` at the hit. `DumpType` is 5 and `RequiredDumpSpace` is
   247,356,649 -- which is exactly the "mysterious 250 MB of overlay growth"
   that a RIP sample had been mistaking for a live guest.
3. A kernel crash dump is contiguous on disk by construction, so those
   247,356,649 bytes can be lifted straight out of the raw image. No NTFS
   parsing, no `MEMORY.DMP` to find -- and there is no `MEMORY.DMP` to find,
   because Validation OS never gets far enough to convert it.
4. `kd.exe -z` with `srv*C:\symbols*https://msdl.microsoft.com/download/symbols`
   and `!analyze -v`. The debugger ships inside the `Microsoft.WinDbg` winget
   package, under `amd64\`.

```
nt!MiFlushTbList -> nt!HvlFlushRangeListTb -> nt!HvcallFastExtended
  -> nt!HvcallpExtendedFastHypercall+0x51
  -> 0xfffff804`36390000            <- the hypercall page
  -> nt!KiPageFault+0x468 -> nt!KiBugCheckDispatch -> nt!KeBugCheckEx

PROCESS_NAME: mountvol.exe        (via ntdll!LdrpSnapModule -> NtProtectVirtualMemory)
BUGCHECK 0xd1  P1=0xa  P2=0x2  P3=0x0  P4=0xfffff804`36390000
```

And the page itself:

```
0: kd> db fffff804`36390000 L10
fffff804`36390000  af af af af af af af af-af af af af af af af af

0: kd> u fffff804`36390000
fffff804`36390000 af    scas dword ptr [rdi]

0: kd> dq nt!HvlpHypercallCodeVa L1
fffff804`a65b1850  fffff804`36390000
```

`rdi` held `0xa`, a leftover hypercall argument, so `scas` read address `0xa` at
`DISPATCH_LEVEL`. Every bugcheck parameter is accounted for: the near-null read
was never a corrupt per-CPU structure, it was filler bytes being executed as
code.

**Do not stop at "near-null pointer".** That description fits a dozen different
bugs and points at none of them. The structure had to be named, and naming it
took the dump.

## The first bug: the processor that never started

Before any of that could be seen, the application processor had to run at all.

`WHvRegisterInternalActivityState` holds a processor's `StartupSuspend`,
`HaltSuspend` and `IdleSuspend` bits. It is absent from `whpx_register_names`,
the only code that touches it is `whpx_vcpu_kick_out_of_hlt()`, and QEMU
registers no vmstate for WHPX at all. A fresh partition parks every application
processor in `StartupSuspend` waiting for the INIT/SIPI that brings it up --
correct for a cold boot, wrong for a restore, where the guest sent that sequence
long ago in another process and will never send it again.

The value has to be applied at the right moment as well as carried. Writing it
from the vmstate `post_load` releases the processor while the stream is still
being read, before `cpu_synchronize_post_init()` has pushed its architectural
state, so it starts executing from whatever a fresh VP happens to hold; the
result is intermittent. The patch remembers the value at load and applies it at
the end of the full-state push.

## Why only SMP

`nt!HvlFlushRangeListTb` is the *remote* TLB flush. A one-processor guest has
nobody to flush, never takes the enlightened path, and never touches the
hypercall page -- so a missing overlay is invisible to it. That is why the
crash tracked processor count so exactly, and why it looked for so long like a
wake-up bug.

It does not follow that the rest of the enlightenments are multiprocessor-only.
The SynIC and the reference TSC page below are switched on whatever the
processor count; a one-processor guest simply happened not to be caught by
them.


## The second failure: a restored guest that halts and is never woken

With the hypercall page restored, a two-processor guest stops crashing. It does
not yet work every time, and what remains is a different failure with a
different signature:

| Experiment | Result |
|---|---|
| One prepared state, restored and run 8 times at `-smp 2` | **8 warm runs of 8** |
| A different prepared state, built the same way, run twice | **fails both times** |
| The two states' mailbox images, compared entry by entry | **identical** |
| Fresh prepared guests at `-smp 2` | about **one in three** unusable |
| Fresh prepared guests at `-smp 4` | **three of three** unusable |

Most of the risk is in the freeze, not the restore: a state that works tends to
go on working. It is not a clean split, though. A hundred-run soak found one
prepared guest serving twenty-five warm runs and then failing, so a restore from
a good state carries a small independent risk of its own -- a few percent, on
this evidence. What differs is where the guest happened to be frozen, and how
many processors were idle when it was.

Watching both kinds resume, with a real command already in the mailbox, says
what "bad" means:

```
--- a good state
  RIP before cont: 00007ffb69f3a703  fffff801a45a7ba2
  t+  0.2s         00007ffb69f3a6f0  fffff801a45a8a64   mailbox:WQGO
  t+  2.4s         fffff801a4a26f8f  fffff801a4a34820   mailbox:WQOUT,WQERR,WQCODE

--- a bad state
  RIP before cont: 00007ffc8e3b057d  00007ffc8e34ed2b
  t+  0.2s         00007ffc8e319dd8  fffff806ea90f282   mailbox:WQGO
  t+  2.5s         fffff806ea235d47  fffff806ea3f6f8f   mailbox:WQGO
  t+ 38.9s         fffff806ea235d47  fffff806ea3f6f8f   mailbox:WQGO
```

The bad guest resumes, runs for about two seconds, and then both processors
stop at fixed addresses and stay there — four distinct RIPs in thirty-nine
seconds. It is not spinning and it is not crashed. It is halted, waiting for an
interrupt that never arrives.

Those two addresses are the same ones the very first `-smp 4` measurement
recorded, when this was written up as "every vCPU halts permanently". That
failure was never a separate bug. It was always this one, hidden behind the
crash.

### What the guest is enlightened with

Reading the hypervisor's own per-processor state out of a live prepared guest,
just before it is frozen:

```
vp0 GuestOsId     0x1040a0000271b     vp1 GuestOsId     0x1040a0000271b
vp0 Hypercall     0x1fc003            vp1 Hypercall     0x1fc003
vp0 VpAssistPage  0xe001              vp1 VpAssistPage  0x46ff001
vp0 ReferenceTsc  0xd001              vp1 ReferenceTsc  0xd001
vp0 Scontrol      0x1                 vp1 Scontrol      0x1
vp0 Simp          0x19001             vp1 Simp          0x1a001
vp0 Siefp         0                   vp1 Siefp         0
vp0 SynicMessagePage    4096 bytes    vp0 SynicTimerState   200 bytes
```

`Hypercall = 0x1fc003` is page frame `0x1fc` with the enable bit set -- the same
frame the crash dump faulted in, which is a pleasing independent confirmation of
the fix above.

`Scontrol = 1` says the guest has the **synthetic interrupt controller**
switched on, with a message page of its own per processor and two hundred bytes
of synthetic timer state. Windows uses SynIC timers as a clock source when it is
enlightened. None of it is in `whpx_register_names` and none of it crossed the
migration, so it looked exactly like the missing wake-up.

### The SynIC was not the answer

It was implemented and measured: `Scontrol`, `Sversion`, `Simp`, `Siefp` and
`Sint0`-`Sint15`, plus the three opaque per-processor blobs WHP exposes through
`WHvGet/SetVirtualProcessorState` -- message page, event flag page and timer
state -- written in overlay-then-contents order.

It made the product **eighty-six times slower**, on the same host, the same
guest and the same command:

| `winquick run --cpus 2 -- cmd /c ver` | guest exec + mailbox sync | total |
|---|---|---|
| without the SynIC patch | 520, 540, 656, 656, 521 ms | 15.8 - 22.9 s |
| with it | 44,945 and 47,394 ms | 77.9 s |

and it did not fix the halting: prepared states went on failing at about the
same rate either way. So it is not in `patches/`, and it should not be tried
again in that form.

**The most likely reason it hurts is worth writing down.** A synthetic timer's
expiry is a *partition reference-time* value, and a fresh partition starts its
reference clock over. A restored expiry therefore lands an arbitrary distance
into the new partition's future, so the timer the guest is waiting on is now
further away than it was before the migration -- which is the same
discontinuity as the TSC jump measured earlier, arriving by a different route.
Carrying this state correctly would mean translating every expiry into the
destination partition's timeline, which needs a reference-time reading on both
sides that WHP does not obviously expose.

### So what is the halting?

Not established, but narrowed. A halted four-processor restore, poked with an
NMI after fifteen seconds -- a **contaminated diagnostic run** by construction,
since Windows treats an unexpected NMI as a hardware fault, and no dump from it
should ever be read as a root cause:

```
  t+  0.0s  fffff80283605d47 fffff802837c6f8f fffff80283605d47 00007ffa772b73da
  t+  2.2s  fffff80283605d47 fffff802837c6f8f fffff80283605d47 fffff80283605d47
  ...      unchanged through t+15s
  --- NMI injected ---
  t+ 17.3s  fffff802836d14e9 fffff802834c597e fffff802836d14e9 fffff802836d14e9
  ...      through t+54s, never answering
```

**Every processor moved.** So the interrupt path is intact end to end: an
interrupt raised on a restored partition is delivered, taken, and executed. What
is missing is not the ability to receive an interrupt but a *source* of one --
whatever would have woken these processors on its own never fires again.

So, of the candidates:

- it is **not** the ability to deliver an interrupt (the NMI proves that);
- it is **not** the LAPIC's saved state: `whpx_apic_post_load()` pushes the whole
  993-byte interrupt-controller block back through
  `WHvSetVirtualProcessorInterruptControllerState2`, and the state compares
  byte-identical either side. Whether WHP *re-arms* the LAPIC timer from that
  block, as opposed to merely storing its registers, is the obvious next thing
  to measure and was not measured here;
- it is **not** the SynIC, at least not in the form above;
- the guest is genuinely halted, not spinning and not crashed;
- it gets worse with processor count -- one in three prepared guests at two
  processors, all three tries at four -- which is what one would expect if each
  idle processor is independently at risk of being frozen waiting for a tick
  that never comes.

WinQuick's answer for now is to build another prepared guest rather than
conclude the host cannot restore.

## What was ruled out, with evidence

Recorded because each of these cost real time and none of them needs repeating:

- **The APIC.** WHP's own LAPIC state, all 993 bytes of
  `WHvGetVirtualProcessorInterruptControllerState2`, is byte-identical on both
  sides for both processors. (A first attempt compared only the first 320 bytes
  and "found them identical", which was not evidence of anything.)
- **The interrupt/event registers** the code comments call skipped:
  `PendingInterruption`, `InterruptState`, `PendingEvent`,
  `DeliverabilityNotifications` -- all zero on both sides.
- **The IOAPIC.** Its whole redirection table was dumped: the PIT and RTC pins
  are masked by Windows, the only live pins are the keyboard, serial, ACPI and
  mouse. There is no periodic timer there at all.
- **`Canceled` exits.** Instrumenting the single place that issues one showed
  them coming from ordinary `qemu_cpu_kick()`, about twice a second.
- **A short settle before the freeze.** Raising it from 1.5 s to 6 s changes
  nothing.
- **The generic WHPX save/restore path.** A hand-written real-mode guest with no
  firmware and no devices migrates and restores correctly, and keeps
  incrementing its counter afterwards
  ([`experiments/whpx-resume/tinyguest.py`](../experiments/whpx-resume/tinyguest.py)).
- **The migration blocker patch and the run-state transition.** `stop` followed
  by `cont` in the same process works at `-smp 4`.

## Two traps in the diagnostics themselves

**Do not NMI a crash investigation.** Injecting NMIs periodically to see whether
a wake-up revived the guest produced its own crash dump: `BugCheckCode = 0x80`,
`NMI_HARDWARE_FAILURE`. Windows treats an unexpected NMI as a hardware fault.
The first dump recovered was that one, and it very nearly became the reported
root cause. Any dump taken from a guest that has been poked has to be read with
that in mind, and labelled.

**Do not read overlay growth as liveness.** The 250 MB the overlay gained was
the crash dump being written. RIP moved throughout, because dump generation is
code. For most of this investigation the guest was described as hung when it had
in fact crashed, and that one wrong word sent the search in the wrong direction
for a long time.

## Two more bugs found along the way

`inject-nmi` does nothing at all on a WHPX guest, for two independent reasons.
Both are fixed in [`patches/whpx-nmi-delivery.patch`](../patches/whpx-nmi-delivery.patch),
69 lines across two files, and both look upstreamable on their own.

**The external NMI hook is an empty function.** `whpx_apic_external_nmi()` has
no body. With an APIC enabled -- which is always -- `x86_nmi()` delivers through
the APIC rather than by raising `CPU_INTERRUPT_NMI` directly, so every
externally injected NMI reached that stub and stopped there. The fix honours how
the guest programmed LINT1, as KVM's equivalent does, and then raises the
interrupt.

**A prepared interruption is only committed for one APIC mode.** In
`whpx_vcpu_pre_run`, an NMI is computed into `new_int` near the top --
consuming `CPU_INTERRUPT_NMI` and clearing `vcpu->interruptable` as it goes --
but the block that writes `new_int` into `WHvRegisterPendingInterruption` sits
inside `if (!whpx_irqchip_in_kernel())`. With the in-hypervisor APIC the work was
done and thrown away.

## And one that was not about the processor at all

Even with `-smp 1`, where the guest demonstrably resumes and executes, a real
`winquick run` used to time out waiting for `WQCODE.TXT`. The guest was alive;
it just never acted on the command. That looked like mailbox cache coherency. It
was not. See [mailbox-freeze.md](mailbox-freeze.md): WinQuick was freezing the
guest half a step too early, and the fix is one `sleep`.

## What this means for the product

On ROAD-WARRIOR01, with the two patches applied, `winquick run --cpus N -- cmd /c ver`
twenty times from one prepared guest:

| processors | runs | result | min | p50 | mean | p95 | max |
|---|---|---|---|---|---|---|---|
| 1 | 20 | **20 warm of 20** | 13.3 s | 18.4 s | 18.7 s | 25.3 s | 26.0 s |
| 2 | 20 | **20 warm of 20** | 13.9 s | 24.8 s | 22.7 s | 28.2 s | 29.2 s |
| 2 | 100 | **98 warm of 100** | 14.6 s | 23.0 s | 34.8 s | 133.5 s | 237.8 s |
| 4 | 20 | **0 warm of 20** | | | | | |

The hundred-run figures carry the two rebuilds in their tail: a run that has to
build a new prepared guest before it can answer takes a couple of minutes, which
is what the p95 and the max are. The p50 is what a run costs.

Every run produced the right output and the right exit code, at every processor
count; the prepared state and the canonical image were byte-identical
afterwards, and no QEMU process was left behind.

**Four processors is the honest failure.** All three prepared guests WinQuick
built came back halted, so it wrote `restore-unsupported` and every run booted
cold. The warm path is usable at one and two processors and is not usable at
four.

### Where a warm run's time went

The restore is a couple of hundred milliseconds and the guest answers in about
half a second, and yet a warm run took seventeen to twenty-five seconds. All of
the difference was in one line: cloning the workspace and artifact volumes.
They are two gigabytes each, macOS gets them free from APFS cloning, and
everywhere else this was `std::fs::copy` twice.

Those volumes are **0.244% non-zero** -- 5.2 MB of FAT boot sector, allocation
tables and a nearly empty root directory in 2147.5 MB. WinQuick was moving
4.3 GB per run to deliver ten. Writing only what is there, into a sparse
destination, halved a warm run; asking the filesystem where the data is, rather
than reading four gigabytes to find out, did the rest.

Twenty steady-state runs at `-smp 2`, reusing one prepared guest:

| phase | before | min | p50 | mean | p95 | max |
|---|---|---|---|---|---|---|
| prep | 15,839 ms | 183 | **204** | 213 | 224 | 417 ms |
| qemu spawn | | 758 | 830 | 836 | 910 | 945 ms |
| state restore | | 116 | 273 | 267 | 377 | 444 ms |
| guest exec + mailbox | | 491 | 593 | 590 | 695 | 703 ms |
| **full `winquick run`** | 24,826 ms | 1,874 | **2,115** | 2,115 | 2,366 | 2,367 ms |

What is left is dominated by starting a QEMU process, which is what it costs.

The freeze lottery is what is left, and most of it is paid at prepare time: a
prepared guest is built once and reused. WinQuick builds up to three before
concluding the machine cannot restore, so a bad freeze costs a rebuild rather
than the fast path.

**The hundred-run soak found the sting in that.** One prepared guest served
twenty-five warm runs, then a restore came back silent; WinQuick rebuilt, three
in a row were unlucky, and it wrote `restore-unsupported`. Every remaining run
cold-booted -- on a machine that had just demonstrated twenty-five times that it
restores perfectly well. The note claims "this QEMU cannot restore a prepared
guest", and a QEMU that has restored one refutes that claim, so WinQuick now
records the demonstration and lets it outrank any later run of silent guests. A
silent guest is also recognised in seconds now rather than in a minute, by
waiting for the agent to take the go flag rather than for the command to finish,
which is what makes retrying five times affordable.

| hundred runs at `-smp 2` | warm | cold |
|---|---|---|
| before | 25 | 75 |
| after | **98** | 2 |

## Reproducing

[`experiments/whpx-resume/`](../experiments/whpx-resume/) holds the drivers.
They take POSIX paths, run under MSYS2's Python, and drive the native QEMU:

```console
$ python3 tinyguest.py  <qemu> <workdir>                       # the divider
$ python3 cycle.py      <qemu> <statedir> <base> <work> <smp>  # cold -> migrate -> restore
$ python3 stopcont.py   <qemu> <statedir> <base> <work> <smp>  # the control
$ python3 winresume.py  <qemu> <statedir> <work> [seconds]     # sample a restore
$ python3 lapicdiff.py  <qemu> <statedir> <base> <work> <out>  # cold vs restored APIC
$ python3 crashrun.py   <qemu> <statedir> <base> <work> <smp>  # crash, not hang
$ python3 dumphdr.py    <overlay.qcow2>                        # read every dump header
$ python3 probe.py      <qemu> <statedir> <work> <smp> [s] [mailbox]  # watch a restore work or not
$ python3 wake.py       <qemu> <statedir> <work> <smp> <after> <mailbox>  # is it just asleep?
$ python3 fatls.py      <mailbox.img>                          # the mailbox, entry by entry
```

`crashrun.py` is the one that made the difference: it leaves the guest entirely
alone and reports a crash by finding the dump, rather than guessing at liveness
from RIP.

Build QEMU with [`patches/whpx-resume-diagnostics.patch`](../patches/whpx-resume-diagnostics.patch)
applied and set `WHPX_DIAG=1` to get the per-vCPU exit accounting. The patch is
lab instrumentation and is not applied to anything WinQuick ships.
