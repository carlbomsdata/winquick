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
