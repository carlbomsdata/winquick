# Third-party notices

WinQuick is Apache-2.0, © Carlboms Data AB. It uses the following third-party
software. See [docs/licensing.md](docs/licensing.md) for how the boundaries work.

---

## ntfsprogs (`ntfscp`, `ntfscat`) — distributed with WinQuick

**Included in WinQuick release archives on every host: macOS, Linux and
Windows.** A distribution's own ntfsprogs is not a substitute -- WinQuick
addresses a partition inside a whole-disk image through `NTFS_IMAGE_OFFSET`,
which is this project's patch, and an unpatched `ntfscp` reads offset zero and
reports `NTFS signature is missing`.

- Project: ntfs-3g / ntfsprogs
- Version: 2022.10.3, **modified** -- see below
- Licence: GNU General Public License, version 2 or later
- Homepage: <https://www.tuxera.com/community/open-source-ntfs-3g/>
- Source: <https://tuxera.com/opensource/ntfs-3g_ntfsprogs-2022.10.3.tgz>
  (SHA-256 `f20e36ee68074b845e3629e6bced4706ad053804cbaf062fbae60738f854170c`)

Copyright © 2000-2022 Anton Altaparmakov, Richard Russon, Szabolcs Szakacsits,
Jean-Pierre André and contributors.

WinQuick uses these two programs during `winquick setup` only, to write files
into the Windows system volume: macOS cannot write NTFS at all, and on Windows
the alternative would be attaching the disk image, which needs elevation.
They are invoked as separate child processes and are not linked into WinQuick.

**Changes made (GPL-2.0 section 2a).** WinQuick modifies the upstream sources.
The complete diff is [`patches/ntfsprogs-windows.patch`](patches/ntfsprogs-windows.patch),
applied by the build recipe. In summary:

- `libntfs-3g/unix_io.c` -- an `NTFS_IMAGE_OFFSET` environment variable shifts
  every seek and positioned read/write by a fixed number of bytes, so a
  partition inside a whole-disk image can be addressed without a partition
  device node. Unset, it changes nothing.
- `libntfs-3g/unix_io.c` -- the image is opened in binary mode on Windows, where
  the C runtime would otherwise translate line endings and stop at `0x1A`.
- `include/ntfs-3g/compat.h` -- upstream defines `__attribute__` away on
  Windows, which also discards `__attribute__((packed))` and silently unpacks
  every on-disk structure. GCC supports the attribute, so it is left alone.
- `include/ntfs-3g/device_io.h` -- the file-based device operations are used on
  Windows rather than the physical-drive ones.
- `ntfsprogs/ntfscat.c`, `ntfsprogs/ntfscp.c` -- stdout and the source file are
  put in binary mode on Windows, for the same reason as above.
- `ntfsprogs/utils.h` -- upstream's Windows format-translation macros are
  variadic but expand to a trailing comma; `##` makes zero-argument calls
  compile.
- `libntfs-3g/dir.c` -- an explicit union cast, because GCC ignores
  `transparent_union` on the Windows ABI.

**Corresponding source.** The build recipe is
[`scripts/build-ntfs-helpers.sh`](scripts/build-ntfs-helpers.sh); it downloads
the exact upstream tarball above, verifies its digest, applies the patch and
builds. Running it reproduces the shipped binaries. The upstream tarball and the
patch are attached to each WinQuick release so the source remains available
alongside the binaries for as long as they are distributed.

> This program is free software; you can redistribute it and/or modify it under
> the terms of the GNU General Public License as published by the Free Software
> Foundation; either version 2 of the License, or (at your option) any later
> version.
>
> This program is distributed in the hope that it will be useful, but WITHOUT ANY
> WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
> PARTICULAR PURPOSE. See the GNU General Public License for more details.
>
> You should have received a copy of the GNU General Public License along with
> this program; if not, write to the Free Software Foundation, Inc., 51 Franklin
> Street, Fifth Floor, Boston, MA 02110-1301 USA.

The full licence text ships as `LICENSE.ntfsprogs` beside the binaries in the
release archive, and is available at
<https://www.gnu.org/licenses/old-licenses/gpl-2.0.html>.

---

## QEMU — required, not distributed

- Licence: GNU General Public License, version 2 (GPL-2.0-only)
- Homepage: <https://www.qemu.org/>
- Obtained by the user: `brew install qemu` on macOS, the distribution's own
  package on Linux, a build on Windows
- Tested against: 11.1.0 on all three hosts
- **Minimum on Linux: 11.0.** Older QEMU cannot migrate the NVMe device the
  guest boots from -- Ubuntu 24.04's 8.2.2 fails with `State blocked by
  non-migratable device '.../nvme'` -- so `winquick doctor` refuses it rather
  than letting every run silently boot cold.

WinQuick runs `qemu-system-aarch64` (macOS) or `qemu-system-x86_64` (Linux and
Windows) and `qemu-img` as separate child processes. It does not link against
QEMU, statically or dynamically, and contains no QEMU code. **WinQuick does not
distribute QEMU on any host**, the Linux archive included -- bundling it would
mean shipping a GPL-2.0 binary and its whole shared-library closure and
carrying the corresponding-source obligation for it, to avoid a package the
user can install.

**The Linux QEMU is pristine upstream.** The seven WHPX patches below are
Windows-only and are not applied to it; nothing in this project patches QEMU
for KVM.

On Windows the prepared-state path additionally needs **seven** changes to
QEMU's WHPX backend and its migration transport, applied in the order given in
[`patches/README.md`](patches/README.md):

| Patch | What it does |
|---|---|
| `whpx-nmi-delivery.patch` | delivers an externally injected NMI through the APIC, which was dropped |
| `whpx-stop-and-copy.patch` | lifts WHPX's unconditional migration blocker for a stopped guest |
| `whpx-activity-state-migration.patch` | carries each processor's `WHvRegisterInternalActivityState` |
| `whpx-hyperv-synthetic-migration.patch` | carries the Hyper-V `GuestOsId`, hypercall page, VP assist page and reference TSC |
| `whpx-lapic-timer-migration.patch` | carries the local APIC timer's current count |
| `whpx-idle-suspend-restore.patch` | clears the halt/idle suspend bits so a restored processor is runnable |
| `whpx-migration-file-binary.patch` | opens the migration file in binary mode, which the Windows CRT otherwise mangles |

`whpx-resume-diagnostics.patch` is a laboratory instrument and is not part of
the set.

**None of these are applied to any binary WinQuick currently distributes** --
there is no Windows release archive yet. Without them a Windows host still
works; it boots cold every time, which costs roughly 16.5 s per run instead of
1.4 s. If a Windows archive ever bundles a patched `qemu-system-x86_64.exe`,
that is distribution of a GPL-2.0 work and this section has to grow the same
corresponding-source offer the ntfsprogs section already carries: the exact
upstream tarball, these patches, the build recipe, and the full licence text
beside the binary.

---

## hivex (`hivexsh`)

- Licence: `hivexsh` is GPL-2.0-or-later; the `hivex` library it uses is
  LGPL-2.1-or-later
- Homepage: <https://github.com/libguestfs/hivex>
- Version: 1.3.24
- Source: <https://download.libguestfs.org/hivex/hivex-1.3.24.tar.gz>
  (SHA-256 `a52fa45cecc9a78adb2d28605d68261e4f1fd4514a778a5473013d2ccc8a193c`)

Used during `winquick setup` only, to set one value in a Windows registry hive.
Invoked as a separate child process; not linked into WinQuick.

**On macOS and Linux this is not distributed** -- the user installs it with
`brew install hivex` or `apt install libhivex-bin`. Neither host's copy is
modified, so no change disclosure arises there.

**On Windows it is distributed with WinQuick, modified**, because no package
provides it there. Upstream excludes `hivexsh` from Windows builds entirely, for
one reason: it composes its interactive prompt with `open_memstream`, which
mingw does not have. WinQuick's change, in
[`patches/hivex-windows.patch`](patches/hivex-windows.patch), uses a fixed
prompt on Windows instead. Nothing else is altered, and WinQuick drives
`hivexsh` from a script file where no prompt is ever printed. The build recipe
is [`scripts/build-hivex-windows.sh`](scripts/build-hivex-windows.sh); it
downloads the tarball above, verifies its digest, applies the patch and builds.
The tarball and patch are attached to each Windows release.

---

## virtio-win drivers — required by the desktop capability, not distributed

`winquick capability install desktop` stages two Windows ARM64 drivers into the
desktop image it builds: `viogpudo`, Red Hat's display-only VirtIO GPU driver,
and `vioinput`. Validation OS has no inbox driver for a plain framebuffer — it
registers the `BasicDisplay` service but does not ship `BasicDisplay.sys` — so
without `viogpudo` there is no display adapter and nothing renders.

WinQuick does not distribute these. They come from Red Hat's `virtio-win` ISO,
which the user downloads:

    https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/

    winquick capability install desktop --virtio ~/Downloads/virtio-win.iso

The drivers are copied into an image generated on the user's own machine, under
`~/.winquick/images/desktop-arm64/`, and never leave it.

They are three-clause BSD:

> Copyright 2009-2022 Red Hat, Inc. and/or its affiliates.
> Copyright 2016 Google, Inc.
> Copyright 2016 Virtuozzo, Inc.
> Copyright 2007 IBM Corporation
>
> Redistribution and use in source and binary forms, with or without
> modification, are permitted provided that the following conditions are met:
> redistributions of source code must retain the above copyright notice, this
> list of conditions and the following disclaimer; redistributions in binary
> form must reproduce it in the documentation and/or other materials provided
> with the distribution; and neither the name of the copyright holder nor the
> names of its contributors may be used to endorse or promote products derived
> from this software without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
> AND ANY EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

The full text is `virtio-win_license.txt` on the ISO.

## Microsoft software — never distributed

WinQuick ships no Microsoft software. Microsoft Validation OS, PowerShell and
.NET are downloaded by you, from Microsoft, under Microsoft's terms. See
[docs/licensing.md](docs/licensing.md).

---

## Rust dependencies

Linked into the `winquick` binary. These are the direct dependencies; run
`cargo tree` for the full graph.

| Crate | Licence |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| clap | MIT OR Apache-2.0 |
| fatfs | MIT |
| fscommon | MIT |
| serde, serde_json | MIT OR Apache-2.0 |

Every crate in the transitive graph is permissive, and between them they use
four licences rather than the two the direct list suggests:

| Licence | Where it appears |
|---|---|
| MIT OR Apache-2.0 | the great majority, including everything above |
| MIT | `fatfs`, `fscommon`, `slab`, `strsim`, `zmij` |
| Unlicense OR MIT | `byteorder`, `memchr` |
| (MIT OR Apache-2.0) AND Unicode-3.0 | `unicode-ident` |

`unicode-ident` is the one that is not simply "MIT or Apache-2.0": the Unicode
licence applies *in addition* to whichever of the two is chosen, because the
crate embeds character tables derived from the Unicode Character Database. It
is a permissive licence and its terms are reproduced with the crate, but it is
a third licence and is named here rather than folded into the other two.

Nothing in the graph is copyleft. That is deliberate and worth keeping: these
crates are linked statically into a binary WinQuick distributes under
Apache-2.0, which is exactly the situation where a GPL dependency would not be
distributable on those terms. The programs that *are* copyleft — QEMU,
ntfsprogs, hivex — stay separate executables, invoked as child processes, and
are covered by the sections above.
