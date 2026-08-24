# Third-party components

WinQuick's own code is Apache-2.0 (see LICENSE). It depends on, but does not
include or link against, the following.

## QEMU — GPLv2

WinQuick invokes `qemu-system-aarch64` and `qemu-img` as **separate executables**
over documented interfaces. It does not link against QEMU, statically or
dynamically, and contains no QEMU code.

If a WinQuick distribution ever bundles a QEMU build, that build ships as a
clearly separate component, with its licence text and copyright notices intact
and its corresponding-source obligations honoured. WinQuick's own source remains
Apache-2.0; the separation is deliberate and must be preserved.

QEMU: <https://www.qemu.org/> — GPL-2.0-only.

## Microsoft Validation OS and other Microsoft software — not redistributed

**WinQuick contains no Microsoft software.** No ISO, no WIM, no VHDX, no derived
disk image, no fragment of one. Nothing from Microsoft is in this repository or
in any WinQuick release artifact.

Users download Microsoft Validation OS from Microsoft and accept Microsoft's
licence terms themselves. That licence forbids sharing, publishing or
distributing the software (§2(e)) and imposes confidentiality obligations (§13).

Images that `winquick setup` generates are derived from Microsoft software. They
stay on the user's machine under `~/.winquick` and must not be redistributed.

Download and licence: <https://learn.microsoft.com/en-us/legal/windows/hardware/validation-os-license>

## Host tools invoked by `winquick setup`

Executed as separate processes; not linked, not bundled.

- **hivex** (`hivexsh`) — LGPL-2.1 — <https://github.com/libguestfs/hivex>
- **ntfsprogs** (`ntfscp`, `ntfscat`), part of ntfs-3g — GPL-2.0-or-later —
  <https://www.tuxera.com/community/open-source-ntfs-3g/>

## Rust dependencies

Ordinary crates.io dependencies, linked into the binary under their own permissive
licences (MIT/Apache-2.0). See `Cargo.toml` and `cargo tree`.
