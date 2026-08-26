# QEMU patches

Not applied to anything WinQuick ships. These are the changes a Windows host
port would need in QEMU, kept here with the evidence for why each exists.

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
