# Third-party notices

WinQuick is Apache-2.0, © Carlboms Data AB. It uses the following third-party
software. See [docs/licensing.md](docs/licensing.md) for how the boundaries work.

---

## ntfsprogs (`ntfscp`, `ntfscat`) — distributed with WinQuick

**Included in WinQuick release archives.**

- Project: ntfs-3g / ntfsprogs
- Version: 2022.10.3, unmodified
- Licence: GNU General Public License, version 2 or later
- Homepage: <https://www.tuxera.com/community/open-source-ntfs-3g/>
- Source: <https://tuxera.com/opensource/ntfs-3g_ntfsprogs-2022.10.3.tgz>
  (SHA-256 `f20e36ee68074b845e3629e6bced4706ad053804cbaf062fbae60738f854170c`)

Copyright © 2000-2022 Anton Altaparmakov, Richard Russon, Szabolcs Szakacsits,
Jean-Pierre André and contributors.

WinQuick uses these two programs during `winquick setup` only, to write files
into the Windows system volume, because macOS cannot write NTFS. They are
invoked as separate child processes and are not linked into WinQuick.

**Corresponding source.** The build recipe is
[`scripts/build-ntfs-helpers.sh`](scripts/build-ntfs-helpers.sh); it downloads
the exact upstream tarball above, verifies its digest, and builds with no
patches. Running it reproduces the shipped binaries. The upstream tarball is also
attached to each WinQuick release so the source remains available alongside the
binaries for as long as they are distributed.

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
- Obtained by the user via Homebrew (`brew install qemu`)
- Tested against: 11.1.0

WinQuick runs `qemu-system-aarch64` and `qemu-img` as separate child processes.
It does not link against QEMU, statically or dynamically, and contains no QEMU
code. WinQuick does not distribute QEMU; Homebrew does.

---

## hivex (`hivexsh`) — required, not distributed

- Licence: GNU Lesser General Public License, version 2.1 or later
- Homepage: <https://github.com/libguestfs/hivex>
- Obtained by the user via Homebrew (`brew install hivex`)
- Tested against: 1.3.24

Used during `winquick setup` only, to set one value in a Windows registry hive.
Invoked as a separate child process; not linked into WinQuick.

---

## Microsoft software — never distributed

WinQuick ships no Microsoft software. Microsoft Validation OS, PowerShell and
.NET are downloaded by you, from Microsoft, under Microsoft's terms. See
[docs/licensing.md](docs/licensing.md).

---

## Rust dependencies

Linked into the `winquick` binary under permissive licences (MIT and/or
Apache-2.0). Run `cargo tree` for the full graph.

| Crate | Licence |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| clap | MIT OR Apache-2.0 |
| fatfs | MIT |
| fscommon | MIT |
| serde, serde_json | MIT OR Apache-2.0 |
