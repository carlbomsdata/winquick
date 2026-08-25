# Licensing

WinQuick sits between two licensing worlds and keeps them apart deliberately.

## WinQuick itself

Apache License 2.0, © Carlboms Data AB. See [LICENSE](../LICENSE).

That covers the `winquick` binary and everything in this repository that we
wrote. It does not cover QEMU, ntfsprogs, hivex, or anything from Microsoft.

## Microsoft software: never redistributed

**WinQuick ships no Microsoft software of any kind.** Not the ISO, not a WIM, not
a VHDX, not a derived disk image, not a fragment of one. Nothing from Microsoft
is in this repository or in any WinQuick release artifact.

You obtain Microsoft Validation OS from Microsoft and accept Microsoft's licence
terms yourself. `winquick setup` will point you at Microsoft's download, or
download it on your behalf once you pass `--accept-microsoft-terms`, which is
your acceptance of those terms — not ours.

The Validation OS licence forbids sharing, publishing or distributing the
software (§2(e)) and imposes confidentiality obligations (§13). WinQuick
therefore treats every image it generates from it as a derived work that stays on
your machine: it lives under `~/.winquick`, is never uploaded, and is never a
release artifact. The repository's `.gitignore` refuses `*.iso`, `*.vhdx`,
`*.qcow2` and friends as a backstop, not as the primary control.

The same applies to PowerShell and .NET: WinQuick downloads them from Microsoft
to your machine and builds local volumes from them. It does not redistribute
them.

## VirtIO drivers: obtained by you, staged locally

The desktop capability needs a display driver, because Validation OS has none —
it registers the `BasicDisplay` service but ships no `BasicDisplay.sys`. WinQuick
uses Red Hat's `viogpudo` from the `virtio-win` ISO.

WinQuick does not redistribute it. You download the ISO from Red Hat and point
`winquick capability install desktop --virtio` at it. The driver is staged into
the desktop image built on your machine, under `~/.winquick/`, and stays there.

The drivers are three-clause BSD, which permits redistribution with the notice
attached; WinQuick simply has no reason to redistribute them, and does not. See
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

The images WinQuick generates are subject to the same rule as the base image:
they contain Microsoft software and must not be redistributed.

## GPL components: separate programs, not linked

WinQuick uses three external programs. None of them is linked into the WinQuick
binary, statically or dynamically. Each is invoked as a child process over a
documented command-line interface, and each keeps its own licence.

| Program | Licence | Used for | How it is obtained |
|---|---|---|---|
| QEMU | GPL-2.0-only | running Windows | Homebrew |
| hivex (`hivexsh`) | LGPL-2.1-or-later | one registry edit during setup | Homebrew |
| ntfsprogs (`ntfscp`, `ntfscat`) | GPL-2.0-or-later | two file writes during setup | built by us, shipped beside the CLI |

The separation is a design constraint, not an accident. It is why `run` spawns
`qemu-system-aarch64` rather than embedding it, and why setup shells out rather
than linking a filesystem library.

### ntfsprogs, which we do distribute

WinQuick release archives include `ntfscp` and `ntfscat` built from unmodified
ntfs-3g/ntfsprogs 2022.10.3, because Homebrew's `ntfs-3g` formula is Linux-only
and macOS cannot write NTFS on its own.

These are GPL-2.0-or-later, so distributing them carries obligations, which we
meet as follows:

- The exact upstream source is identified by version and SHA-256 in
  [`scripts/build-ntfs-helpers.sh`](../scripts/build-ntfs-helpers.sh), which is
  also the complete, unmodified build recipe. Running it reproduces the shipped
  binaries.
- The upstream tarball is available from
  <https://tuxera.com/opensource/ntfs-3g_ntfsprogs-2022.10.3.tgz>, and a copy is
  attached to WinQuick releases as `ntfs-3g_ntfsprogs-2022.10.3.tgz` so the
  corresponding source stays available alongside the binaries.
- No modifications are made to the source.
- Full licence text and copyright notices ship in
  [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) and beside the binaries.

If any of that is ever unavailable, that is a bug — please report it.

### QEMU and hivex, which we do not distribute

Both come from Homebrew, so Homebrew distributes them and its own compliance
applies. WinQuick records which versions it was tested against and identifies
them in `winquick doctor`. If a future WinQuick release bundles a QEMU build, it
will ship as a clearly separate component with its licence text, copyright
notices and corresponding-source availability intact, under the same posture as
ntfsprogs above.

## What this means for you

- You may use WinQuick commercially, under Apache-2.0.
- You may not redistribute the Windows runtime WinQuick builds on your machine.
- If you redistribute WinQuick's release archive, you are also redistributing
  ntfsprogs, and the GPL obligations above come with it — keep
  `THIRD_PARTY_NOTICES.md` and the source pointer intact.
