# Why a restored Windows guest does not run under WHPX

WinQuick is fast because it does not boot Windows: it restores a prepared state
into a fresh QEMU process. On Windows that does not work, and this is the
investigation into why.

**Status: root cause narrowed, not fixed.** The failure is reproducible, the
boundary is sharp, and most of the obvious suspects have been eliminated with
measurements rather than reasoning.

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

The processors are halted inside the hypervisor and the in-hypervisor APIC
never delivers a wake-up, on a partition that was reconstructed from scratch,
only when there is more than one of them.

Two candidates remain, in order:

1. **Per-vCPU activity state.** WHP has no obvious equivalent of KVM's
   `mp_state`, and QEMU's WHPX backend restores no notion of "this processor was
   halted" or "this processor is waiting for SIPI". On a single-processor guest
   there is nothing to get wrong. On a multiprocessor one, a processor that
   should be waiting — or one that should not be — would produce exactly this.
   The same class of problem is recognised on other architectures; see the
   ARM64 KVM discussion of exporting vCPU pause state for migration.
2. **Hypervisor-internal timer state.** `whpx_register_names` is a fixed list of
   architectural registers, and `whpx_get_registers` is explicit that
   "Interrupt / Event Registers - Skipped". Anything the hypervisor keeps
   outside the LAPIC register page — an armed timer deadline, synthetic timer
   state — is not carried by the migration stream.

Distinguishing them needs a WHP-level experiment rather than a QEMU-level one:
take the standalone fan-out proof that already resumes a single vCPU
successfully, extend it to two, and see whether it survives. If it does, the
gap is in QEMU; if it does not, it is in what WHP exposes.

## A separate bug found along the way

NMIs are silently discarded when the in-hypervisor APIC is in use. In
`whpx_vcpu_pre_run`, the NMI is computed into `new_int` near the top, but the
block that commits it —

```c
if (new_int.InterruptionPending) {
    reg_values[reg_count].PendingInterruption = new_int;
    reg_names[reg_count] = WHvRegisterPendingInterruption;
    reg_count += 1;
}
```

— sits inside `if (!whpx_irqchip_in_kernel())`. With the in-kernel APIC the
work is done and thrown away. `inject-nmi` on a live WHPX guest does nothing,
which is how this was noticed: it was being used to try to wake the frozen
guest.

This is independent of the resume problem and looks upstreamable on its own.

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
