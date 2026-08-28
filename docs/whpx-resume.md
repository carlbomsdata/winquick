# Why a restored Windows guest does not run under WHPX

WinQuick is fast because it does not boot Windows: it restores a prepared state
into a fresh QEMU process. On Windows that does not work, and this is the
investigation into why.

**Status: root cause identified, not fixed.** The failure is reproducible, the
boundary is sharp, and the missing state has been named: a restored application
processor is left in WHP's `StartupSuspend`, waiting for a SIPI that was
delivered in another process minutes ago, because QEMU never saves or restores
`WHvRegisterInternalActivityState`. An unmerged 2022 upstream patch reached the
same conclusion from the other direction. Restoring that bit is necessary and
is not by itself sufficient, so something beyond it is still missing.

Measured on ROAD-WARRIOR01: Windows 11 Pro 25H2 (build 26200), Intel i5-8265U,
Windows Hypervisor Platform enabled, QEMU 11.1.0 (`84f0721`) carrying
[`patches/whpx-stop-and-copy.patch`](../patches/whpx-stop-and-copy.patch),
guest Microsoft Validation OS x64 26100.8972, `-cpu Nehalem`.

## The one-line result

> A prepared guest with **one** vCPU restores and executes. The same guest with
> **two or more** vCPUs restores, runs for about fifty milliseconds, and then
> every vCPU halts permanently. Nothing wakes them again.

## What was measured

| Experiment | Result |
|---|---|
| Minimal hand-written real-mode guest, no firmware or devices, migrate → restore | **executes** |
| Windows x64, `-smp 1`, migrate → restore | **executes** (6,535 exits: 4,340 MemoryAccess, 2,190 Cpuid) |
| Windows x64, `-smp 2`, migrate → restore | **frozen** |
| Windows x64, `-smp 4`, migrate → restore | **frozen** |
| Windows x64, `-smp 4`, plain `stop` → `cont`, same process | **executes** (96,980 exits on vCPU0) |

The minimal guest ([`experiments/whpx-resume/tinyguest.py`](../experiments/whpx-resume/tinyguest.py))
matters because it divides the problem: QEMU's generic WHPX save/restore path is
**not** broken. It increments a counter in low memory, and the counter keeps
climbing after the restore.

That `stop`/`cont` in the same process works is equally important: the run-state
transition and the migration blocker patch are not at fault either. The fault
needs a *fresh partition* and *more than one processor*.

## What the frozen guest is actually doing

`query-status` says `running`, which proves nothing. What the processors do:

```
RIP before cont : fffff801882f0330  fffff80187ea5d47  fffff80118a10003  fffff80188066f8f
  t+  0.00s      fffff80188074d27  fffff80187ea5d47  fffff80118a10003  fffff80188066f8f
  t+  0.05s      fffff80187d98910  fffff80187ea5d47  fffff80118a10003  fffff80188066f8f
  t+  0.10s      fffff80188066f8f  fffff80187ea5d47  fffff80118a10003  fffff80188066f8f
  ... unchanged through t+8s
```

vCPU0 executes real work for roughly fifty milliseconds and then arrives at
`...066f8f`, which is where vCPU3 already sits. Three of the four report
`HLT=1`. The overlay does not grow by a byte. The address is the same modulo
KASLR across separate states, so it is one specific place in the kernel — an
idle loop.

Instrumenting the accelerator ([`patches/whpx-resume-diagnostics.patch`](../patches/whpx-resume-diagnostics.patch))
gives the decisive number:

```
whpx-diag per-vcpu runs/exits/canceled: cpu0=8/8/8 cpu1=8/7/7 cpu2=8/7/7 cpu3=8/7/7
whpx-diag exits by reason: Canceled=29
```

**Every single `WHvRunVirtualProcessor` return, on every processor, is
`Canceled`.** Not one `MemoryAccess`, `X64IoPortAccess`, `X64Cpuid`,
`X64Halt` or `X64InterruptWindow`. The processors do re-enter the hypervisor —
they are not parked in QEMU's idle loop — and the hypervisor gives them nothing
to do. The 29 cancellations over nine seconds are QEMU's own periodic kicks;
`kicks=0` from the run loop and `exit_request=0` confirm nobody is cancelling
deliberately.

For contrast, the same guest cold-booted and idling:

```
whpx-diag exits by reason: MemoryAccess=79602 IoPortAccess=43555 Cpuid=11793 ApicEoi=7 Canceled=88
whpx-msi calls=11006 ok=11004 by vector: 80=4298 97=300 129=328 161=1991 177=3731 178=186 ...
```

## What has been ruled out

- **The migration stream and the generic resume path.** The minimal guest
  restores and runs.
- **The run-state transition and the stop-and-copy patch.** `stop`/`cont` in the
  same process works with four processors.
- **Register and control state.** `whpx_set_registers` runs ten times per vCPU
  on the destination; CR0/CR3/CR4/EFER are identical cold versus restored, and
  the guest demonstrably executes correct kernel code for the first fifty
  milliseconds.
- **The local APIC register state.** `whpx_apic_post_load` fires for all four
  processors and `whpx_apic_put` runs twice per processor. `info lapic` is
  **byte-identical** cold versus restored on every processor: LVT0 vector 216,
  LVT1 NMI, LVTPC 254, LVTERR 226, SPIV enabled with spurious vector 223,
  LDR 0x01, timer masked with initial count 10000000.
- **QEMU's userspace interrupt injection.** `hard=0`, `inj_event=0`,
  `inj_intr=0` in *both* the healthy and the frozen case — the in-hypervisor
  APIC is in use and that path is simply not involved.
- **The IOAPIC timer.** IRQ 0 asserts about eighteen times a second in the
  frozen guest, exactly as in the healthy one, and produces no delivery in
  either: its redirection entry is masked. A red herring, and it cost time.
- **`warning: Ignoring request for interrupt vector 0`.** It appears once, from
  an unprogrammed MSI-X entry. Not the mechanism.

## What is left

The processors are halted inside the hypervisor and nothing ever wakes them,
only when there is more than one of them. A later session took that apart
further, and the picture is now much sharper.

### The restored AP is parked waiting for a SIPI

`WHV_INTERNAL_ACTIVITY_REGISTER` bit 0 is `StartupSuspend` -- "this processor
is waiting for a SIPI". Reading it on both sides of a migration, per vCPU:

| | cpu0 | cpu1 |
|---|---|---|
| source, at boot | `InternalActivity=0` | `InternalActivity=1` |
| source, at the freeze | `InternalActivity=0` | **`InternalActivity=0`** |
| destination, at resume | `InternalActivity=0` | **`InternalActivity=1`** |

That is the state difference, in one line: the source's application processor
had long since been started by Windows, and the destination's is parked waiting
for a startup message that will never be sent again.

It is missing because **QEMU never saves or restores
`WHvRegisterInternalActivityState`**. `whpx_register_names` does not list it;
the only code that touches it is `whpx_vcpu_kick_out_of_hlt()`, which clears
`HaltSuspend` by hand. A fresh WHP partition parks every AP in
`StartupSuspend`, which is correct for a cold boot -- the guest's own INIT/SIPI
clears it -- and wrong for a restore, where that sequence happened in another
process, minutes ago.

This confirms candidate 1 below, which was a hypothesis before and is now a
measurement.

### Everything else WHP exposes is byte-identical

Worth stating precisely, because it narrows the remaining search a lot. Source
at the freeze versus destination at resume:

- all architectural registers: identical
- `PendingInterruption`, `InterruptState`, `PendingEvent`,
  `DeliverabilityNotifications`: all zero on both sides
- **WHP's own LAPIC state, all 993 bytes** of
  `WHvGetVirtualProcessorInterruptControllerState2`: **identical**, on both
  processors

So this is not the APIC register page, and it is not the interrupt/event
registers the code comments call skipped.

### Clearing StartupSuspend is necessary but not sufficient

Clearing the bit works -- cpu1 goes from `1` to `0`, and after that every piece
of VP state matches the source exactly. The guest still does not run. Tried
both at full-state restore and at the resume transition, with the same result.

So the parked AP is real and is certainly one thing that must be fixed, but
something else is also missing. The most likely remaining place is
hypervisor-internal scheduling that has no save/restore API at all, which is
candidate 2.

### The 1-versus-2 boundary, measured

| | interrupts delivered after restore | outcome |
|---|---|---|
| `-smp 1` | **8,634** (vectors 80, 97, 129, 145, 161, 177, ...) | executes |
| `-smp 2` | **2** | frozen |

Interrupt delivery after a restore is not broken in general -- a single-vCPU
guest gets its ticks and its device completions and runs indefinitely. It is
specifically the multiprocessor case that receives nothing.

### A wake proves the processors are fine

Injecting an NMI into the frozen two-processor guest makes it run: 5,065
`MemoryAccess` exits and MSI deliveries resuming, from a standing start. It
then idles again, because whatever should have woken it still does not. So the
restored processor state is sound and what is missing is purely a wake-up.

That test needed two bug fixes before it meant anything; see below.

### Somebody tried this upstream in 2022

Searching after the fact turned up
[*whpx: Added support for saving/restoring VM state*](https://patchew.org/QEMU/004101d86732$0d33bd70$279b3850$@sysprogs.com/)
(Ivan Shcherbakov, May 2022), which saves exactly **one** register --
`WHvRegisterInternalActivityState` -- and says why in its own comment:

> Initially, all WHPX CPUs except #0 start suspended (with
> `WHV_INTERNAL_ACTIVITY_REGISTER::StartupSuspend` set).

That is the same conclusion reached here independently, from the other end, by
reading the register on both sides of a migration. It registers the state
per-CPU with `register_savevm_live("whpx/cpustate", cpu->cpu_index, ...)`,
which is the missing piece: **QEMU 11.1 has no vmstate registration for WHPX at
all**, so there is nowhere for this register to live in the stream today.

The patch was **not merged**. Review foundered on a different part of it:
Hyper-V uses the *compacted* XSAVE layout and QEMU expects the standard one,
and the thread ended without that being resolved. So the activity-state half
was never the objection -- it was carried along by the half that was.

Worth noting for anyone picking this up: that patch also saves XSAVE state,
which QEMU 11.1 does now handle (`whpx_set_xsave_state`/`whpx_get_xsave_state`).
Only the activity state is still missing.

### The two candidates, restated

1. **Per-vCPU activity state** -- now confirmed, above. The fix needs
   `WHvRegisterInternalActivityState` to be carried across migration rather
   than left at whatever a fresh partition defaults to. That is more than a
   one-line change: there is nowhere in the migration stream to put it today.
2. **Hypervisor-internal timer state.** Still open, and now the leading
   suspect for the residue: everything WHP *exposes* is restored correctly and
   the guest still will not wake itself.

## Two separate bugs found along the way

`inject-nmi` does nothing at all on a WHPX guest, for two independent reasons.
Both are fixed in [`patches/whpx-nmi-delivery.patch`](../patches/whpx-nmi-delivery.patch),
69 lines across two files, and both look upstreamable on their own.

**The external NMI hook is an empty function.** `whpx_apic_external_nmi()` has
no body. With an APIC enabled -- which is always -- `x86_nmi()` delivers
through the APIC rather than by raising `CPU_INTERRUPT_NMI` directly, so every
externally injected NMI reached that stub and stopped there. The fix honours
how the guest programmed LINT1, as KVM's equivalent does, and then raises the
interrupt.

**A prepared interruption is only committed for one APIC mode.** In
`whpx_vcpu_pre_run`, an NMI is computed into `new_int` near the top --
consuming `CPU_INTERRUPT_NMI` and clearing `vcpu->interruptable` as it goes --
but the block that writes `new_int` into `WHvRegisterPendingInterruption` sits
inside `if (!whpx_irqchip_in_kernel())`. With the in-hypervisor APIC the work
was done and thrown away. Moving the commit out of that arm fixes it for both
modes; a halted processor also needs telling separately, the same way the
external-interrupt path already does.

Together these are what made "does an NMI wake the frozen guest?" answerable.
Before them the question had been asked twice and had returned a meaningless
no both times.

## And one more, which was not about the processor at all — now fixed

Even with `-smp 1`, where the guest demonstrably resumes and executes, a real
`winquick run` used to time out waiting for `WQCODE.TXT`. The guest was alive;
it just never acted on the command. That looked like mailbox cache coherency.
It was not. See [mailbox-freeze.md](mailbox-freeze.md): WinQuick was freezing
the guest half a step too early, and the fix is one `sleep`.

## Reproducing

[`experiments/whpx-resume/`](../experiments/whpx-resume/) holds the drivers.
They take POSIX paths, run under MSYS2's Python, and drive the native QEMU:

```console
$ python3 tinyguest.py  <qemu> <workdir>                       # the divider
$ python3 cycle.py      <qemu> <statedir> <base> <work> <smp>  # cold -> migrate -> restore
$ python3 stopcont.py   <qemu> <statedir> <base> <work> <smp>  # the control
$ python3 winresume.py  <qemu> <statedir> <work> [seconds]     # sample a restore
$ python3 lapicdiff.py  <qemu> <statedir> <base> <work> <out>  # cold vs restored APIC
```

Build QEMU with [`patches/whpx-resume-diagnostics.patch`](../patches/whpx-resume-diagnostics.patch)
applied and set `WHPX_DIAG=1` to get the per-vCPU exit accounting. The patch is
lab instrumentation and is not applied to anything WinQuick ships.

## What this means for the product

Windows stays on cold boot, at about 16.5 s per run, and WinQuick already
detects the failure once and stops paying for it. Nothing here changes that.
Setting `--cpus 1` does not rescue the fast path either, because of the mailbox
problem above.
