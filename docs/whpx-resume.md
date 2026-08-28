# Why a restored Windows guest does not run under WHPX

WinQuick is fast because it does not boot Windows: it restores a prepared state
into a fresh QEMU process. On Windows that does not work, and this is the
investigation into why.

**Status: one root cause found and fixed, a second one named.** A restored
application processor used to sit in WHP's `StartupSuspend` for ever, never
executing an instruction, because QEMU carries no per-processor activity state
across a migration. That is fixed, and the processor now starts. With it
running, the guest gets far enough to **bugcheck** -- `DRIVER_IRQL_NOT_LESS_OR_EQUAL`,
a near-null read at `DISPATCH_LEVEL` -- and writes a crash dump instead of
running WinQuick's agent. Multiprocessor prepared-state restore is therefore
still not usable, but it fails somewhere much later and much more specifically
than it did.

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

## What was fixed: the application processor never started

`WHV_INTERNAL_ACTIVITY_REGISTER` bit 0 is `StartupSuspend` -- "this processor is
waiting for a SIPI". Reading it on both sides of a migration:

| | cpu0 | cpu1 |
|---|---|---|
| source, at boot | `0` | `1` |
| source, at the freeze | `0` | **`0`** |
| destination, at resume | `0` | **`1`** |

The source's application processor had long since been started by Windows; the
destination's was parked waiting for a startup message nobody would ever send
again. Watching it live made this unambiguous -- for an entire run, cpu1 held
`activity=1` and its RIP never moved by a single instruction.

**QEMU never saves or restores `WHvRegisterInternalActivityState`.** It is
absent from `whpx_register_names`, the only code that touches it is
`whpx_vcpu_kick_out_of_hlt()`, and QEMU registers no vmstate for WHPX at all,
so there was nowhere to put it.

[`patches/whpx-activity-state-migration.patch`](../patches/whpx-activity-state-migration.patch)
adds one small vmstate section per processor that reads the register on save and
writes it back on load.

**Ordering matters and cost a debugging cycle.** Applying the value in the
vmstate `post_load` looks natural and is wrong: `post_load` runs while the
stream is still being read, before `cpu_synchronize_post_init()` has pushed the
processor's architectural state. Releasing a processor from `StartupSuspend`
there lets it start executing from whatever a fresh VP happens to hold, and the
result was intermittent -- sometimes the guest survived to the first real
instruction, sometimes not. The value is now remembered at load and applied at
the end of the full-state push, when RIP and the rest are already in place.
After that change the application processor starts every time.

## What is left: the guest bugchecks

With the processor starting, the two-processor guest still does not run
WinQuick's agent. It is **not** frozen, which is what the earlier write-up
assumed and what the RIP sampling suggested. It crashes.

Letting a restored guest run untouched for a minute and then searching its
copy-on-write overlay finds a Windows crash dump -- `PAGEDU64`, a valid header
reporting `NumberProcessors=2`:

```
BugCheckCode = 0xd1   DRIVER_IRQL_NOT_LESS_OR_EQUAL
P1 = 0xa              the address referenced
P2 = 0x2              IRQL at the time (DISPATCH_LEVEL)
P3 = 0x0              a read
P4 = 0xfffff800722b0000
```

Something dereferences a near-null pointer at `DISPATCH_LEVEL` shortly after
resume. The overlay grows by about 250 MB while Windows writes the dump, which
is the "heavy activity" that a RIP sample mistakes for a live guest.

That reframes the remaining work: this is no longer "find the missing wake-up",
it is "find the per-processor structure that comes back wrong". Address `0xa` is
what a per-CPU lookup produces when its base is wrong, so the natural suspects
are the structures reached that way -- though `Gs`, `KernelGsBase`, `Gdtr`,
`Idtr` and `Ldtr` are all in `whpx_register_names` and are restored.

### Ruled out along the way

- **The APIC.** WHP's own LAPIC state, all 993 bytes of
  `WHvGetVirtualProcessorInterruptControllerState2`, is byte-identical on both
  sides for both processors.
- **The interrupt/event registers** the code comments call skipped:
  `PendingInterruption`, `InterruptState`, `PendingEvent`,
  `DeliverabilityNotifications` -- all zero on both sides.
- **The IOAPIC.** Its whole redirection table was dumped: the PIT and RTC pins
  are masked by Windows, the only live pins are the keyboard, serial, ACPI and
  mouse, and there is no periodic timer there at all. The PIT firing into a
  masked pin was a red herring that cost time.
- **`Canceled` exits.** Instrumenting the single place that issues one showed
  them coming from ordinary `qemu_cpu_kick()`, ~2/s. Not a cause.
- **A short settle before the freeze.** Raising it from 1.5 s to 6 s changes
  nothing.

### One measurement worth keeping

The guest's TSC jumps forward about 7.2 billion cycles -- roughly four seconds
at this host's clock -- across the restore, despite `whpx_set_tsc()` being
called on the full-state path. Whether that is a cause of the bugcheck or just
another consequence is not established, and it is the first thing to measure
next.

### A trap in the diagnostics themselves

Injecting NMIs periodically to see whether a wake-up revived the guest produced
its own crash dump: `BugCheckCode = 0x80`, `NMI_HARDWARE_FAILURE`. Windows
treats an unexpected NMI as a hardware fault. The first dump found was that one,
and it very nearly became the reported root cause. Any dump recovered from a
guest that has been poked has to be read with that in mind.

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
