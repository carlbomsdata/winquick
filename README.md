# WinQuick

**Status: experimental. Nothing here is stable. It may not work on your machine yet.**

WinQuick gives you a tiny, disposable, real Windows command execution environment on an
Apple Silicon Mac.

```console
$ winquick run -- cmd /c ver

Microsoft Windows [Version 10.0.26100.8972]

$ echo $?
0
```

Command in, clean Windows environment, stdout/stderr/exit code out, environment discarded.

The mental model is `docker run --rm`, except the thing on the other end is a real Windows
NT kernel, not a container and not an emulator shim.

## Why

Building and testing Windows software from a Mac currently means keeping a full Windows 11
VM alive: tens of gigabytes, minutes of boot, snapshots that rot, a desktop you have to
click through. That is far too heavy for "run the test suite once" — and much too heavy for
an automated agent that wants to do it fifty times an hour.

WinQuick targets the narrow case that actually matters for builds, tests, automation and
agents: run one command inside a genuine Windows environment, get the exact output and exit
code back, throw the environment away.

## What it is not

Not a VM manager. Not an emulator. Not a QEMU wrapper you have to configure. Not a desktop.
There is no GUI, and there will not be one. You never see or manage a VM.

## Scope of v0.1

| | |
|---|---|
| Host | macOS on Apple Silicon (M1–M5). Nothing else. |
| Virtualization | QEMU + Apple Hypervisor Framework (HVF), as a separate subprocess |
| Guest | Microsoft Validation OS ARM64, obtained by you from Microsoft |
| Control channel | A private disk. No SSH, no WinRM, no RDP, no open ports |
| Commands | `winquick setup`, `winquick run -- <command>`, `winquick info`, `winquick reset` |
| Optional | PowerShell 7, .NET runtime, .NET SDK (`winquick capability list`) |

Deliberately out of scope for now: Linux/Windows/Intel hosts, cloud execution, GUI
virtualization, full Windows 11 guests, MCP integration, a large command tree.

### Speed

The first run after `winquick setup` takes about 11 seconds: Windows boots, and
WinQuick keeps a copy of the booted machine so it never has to boot it again.
Every run after that starts from that copy.

Measured on an M4 Pro, 100 consecutive runs of `winquick run -- cmd /c ver`,
zero failures:

| | |
|---|---|
| median | **225 ms** |
| p95 | **234 ms** |
| p99 | **236 ms** |
| first run (or after `winquick reset`) | ~11 s |
| base image | 763 MiB |
| prepared guest | ~460 MiB |

Full method and numbers in [docs/research.md](docs/research.md).

### Known limits

- **One command per run, and no streaming.** Output arrives when the command
  finishes, not as it is produced.
- **Validation OS is minimal.** It has `cmd.exe` and 538 files in `System32`. No
  PowerShell, no .NET. So `winquick run -- powershell ...` and
  `winquick run -- dotnet test` do **not** work yet — the packages exist on
  Microsoft's ISO but adding them currently requires a Windows host running
  Microsoft's DISM.
- **`winquick setup` needs `ntfsprogs` built from source**, because macOS 26 has
  no NTFS support and Homebrew's `ntfs-3g` is Linux-only.
- No workspace mounting yet.

## Install

Not packaged yet. Build from source:

```console
cargo build --release
```

Requires a Rust toolchain and `qemu-system-aarch64` / `qemu-img` on PATH
(`brew install qemu`).

`winquick setup` additionally needs `hivex` (`brew install hivex`) and
`ntfsprogs`, which has to be built from source on macOS — see
[docs/research.md](docs/research.md#host-side-image-build). `winquick run` needs
neither.

## Setup

WinQuick needs a Windows guest image, and **you** have to get it from Microsoft.

Download **Validation OS ARM64** from Microsoft, accepting Microsoft's licence
terms yourself: <https://aka.ms/DownloadValidationOS_arm64>

Then point WinQuick at it:

```console
winquick setup --from ~/Downloads/…_arm64fre_en-us_VALIDATIONOS.iso
```

That converts the VHDX inside the ISO into a base image under `~/.winquick` and
installs the guest agent into it. Two files change; nothing is added to the ISO
and nothing leaves your machine. See [Licensing](#licensing).

## Usage

```console
winquick run -- cmd /c ver
winquick run -- cmd /c dir C:\Windows\System32
winquick run --memory 1024 --cpus 2 -- cmd /c exit 42
winquick info
```

stdout and stderr stay separate and are passed through unchanged, apart from CRLF
being translated to LF so that piping into `grep` behaves. `winquick` exits with
the Windows process's exit code.

## Licensing

Two separate boundaries, both taken seriously.

**Microsoft.** WinQuick ships no Microsoft software. Not the ISO, not a WIM, not a derived
disk image. You download Validation OS from Microsoft and accept Microsoft's license
yourself; the Validation OS license terms forbid redistribution. Every image WinQuick
generates stays on your machine under `~/.winquick/`.

**QEMU.** QEMU is GPLv2. WinQuick invokes it as a separate executable and never links
against it. If a WinQuick distribution bundles a QEMU build, it does so as a clearly
separate component with its license and corresponding-source obligations intact.

## Documentation

- [docs/architecture.md](docs/architecture.md) — how it works
- [docs/research.md](docs/research.md) — measured results, what worked, what didn't
