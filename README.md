# WinQuick

**Disposable Windows environments, on demand.**

Run a real Windows command from macOS, Linux or Windows in a clean environment
that is discarded afterwards.

```console
$ winquick run -- cmd /c ver

Microsoft Windows [Version 10.0.26100.8972]
```

A real Windows kernel under QEMU, on Apple's Hypervisor Framework, KVM or WHPX.
Not Wine, not an emulator, not a container. Every run starts from a pristine
image and leaves nothing behind.

| | |
|---|---|
| Windows command | **~280 ms** |
| PowerShell command | ~650 ms |
| `dotnet --version` | ~450 ms |
| Desktop session start | ~350 ms, then ~20 ms per UI step |

Medians on the reference host (Apple Silicon M4 Pro, macOS 26, QEMU 11.1), not
guaranteed latencies. Per-run cost differs by host; see [Hosts](#hosts).

```console
brew install carlbomsdata/tap/winquick
winquick setup
```

## Why

Testing Windows software without a Windows machine usually means keeping a VM
alive: tens of gigabytes, minutes of boot, snapshots that rot. That is too
heavy for a single test run, and far too heavy for an agent doing it fifty
times an hour.

WinQuick does one thing: run a command inside a genuine Windows environment,
return its stdout, stderr and exit code exactly, and discard the environment.
The mental model is `docker run --rm` with a real Windows kernel.

## Requirements

Hardware virtualisation, 8 GB of free disk (what `winquick doctor` checks; the
base runtime is 1.4 GB and capabilities add more), and Microsoft's Validation
OS image, which you obtain from Microsoft under their licence.

| | macOS | Linux | Windows |
|---|---|---|---|
| CPU | Apple Silicon (M1 or newer) | x86_64 or arm64 | x86_64 |
| OS | macOS 13 or later | any with KVM | Windows 10/11 |
| Accelerator | Hypervisor Framework | KVM | Windows Hypervisor Platform |
| QEMU | 11 or newer | 11 or newer | 11 or newer |
| Also needs | hivex | `libhivex-bin`, `ovmf` | nothing further |

Three things commonly cause trouble, and `winquick doctor` reports all of
them. The first is an old QEMU: anything before version 11 cannot migrate the
NVMe device the guest boots from, so every run falls back to a cold boot, and
Ubuntu 24.04 still ships 8.2.2. The second is permissions on `/dev/kvm`; if it
is not writable by you, add yourself to the `kvm` group with
`sudo usermod -aG kvm $USER` and log in again.

The third is running WinQuick inside a virtual machine. Windows needs genuine
hardware virtualisation, and a nested hypervisor does not reliably provide it —
under Apple's Virtualization.framework the guest firmware faults before Windows
has started at all. Install WinQuick on the machine itself.

It does not use libvirt, and runs no daemon on any host.

## Install

Archives for every host are on the
[latest release](https://github.com/carlbomsdata/winquick/releases/latest).

**macOS**

```console
brew install carlbomsdata/tap/winquick
```

Homebrew installs the binary, the `ntfscp`/`ntfscat` helpers and the guest
bridge sources, and pulls in QEMU and hivex. Nothing is quarantined, so no
`xattr` step is needed. To install the archive by hand instead, see
[docs/install.md](docs/install.md), which covers the Gatekeeper step.

**Linux** — take the archive matching `uname -m`.

```console
sudo apt install qemu-system-x86 qemu-utils ovmf libhivex-bin
tar -xzf winquick-0.4.0-linux-x86_64.tar.gz
sudo cp -R winquick-0.4.0-linux-x86_64/* /usr/local/
```

On arm64, `qemu-system-arm` replaces `qemu-system-x86` and `qemu-efi-aarch64`
replaces `ovmf`.

**Windows** — install QEMU 11 or newer on `PATH`, then unpack the archive and
put that folder on `PATH` as well. It is one flat directory: `winquick.exe`
with its helpers beside it, so nothing further is required.

```console
tar -xf winquick-0.4.0-windows-x86_64.zip
```

**Then, on any host**, install the Windows runtime. Microsoft distributes the
Validation OS image under its own licence, so WinQuick cannot ship it:

```console
winquick setup --accept-microsoft-terms     # download it (about 2.4 GB)
winquick setup --from ~/Downloads/vos.iso   # or use a file you have
```

Setup finishes by booting Windows and running a real command, so it reports
success only when the runtime works. It takes about a minute.

## Hosts

| Host | Accelerator | Guest | Per-run cost | Verified |
|---|---|---|---|---|
| Apple Silicon macOS 13+ | HVF | Windows ARM64 | **~280 ms** | fully; the reference host |
| Windows x86_64 | WHPX | Windows x64 | ~17 s | fully, on Windows 11 26200 |
| Linux x86_64 / arm64 | KVM | matches the host | not measured | build, tests and diagnostics |
| Windows ARM64, Intel Mac | — | — | — | not planned |

macOS is the reference host and the source of the figures above: 100
consecutive runs of `cmd /c ver` measured p50 287 ms, p99 304 ms, no failures.

**Windows boots the guest from scratch on every run, by design.** A resumed
WHPX guest runs correctly until something waits on a timer, and then waits far
longer than asked — measured at 212 s for `ping -n 4` against 20 s cold.
Builds, tests and PowerShell wait constantly, so Windows takes the predictable
option. `winquick run --warm` requests the prepared guest for commands known
not to wait, and needs a QEMU carrying `patches/whpx-stop-and-copy.patch`. The
cause is a Hyper-V synthetic-timer property of the platform, not a defect;
[docs/whpx-resume.md](docs/whpx-resume.md) has the evidence and
[docs/windows-host.md](docs/windows-host.md) the Windows detail.

**On Linux the host side is verified and the guest is not.** WinQuick builds,
the test suite passes and `winquick doctor` reports the host correctly. A guest
has not been booted on real Linux hardware, because the only Linux machine
available was itself a virtual machine. Nothing measured argues against it; see
[docs/research.md](docs/research.md).

## Usage

**Commands**

```console
winquick run -- cmd /c ver
winquick run -- cmd /c "echo A & echo B"
```

Arguments work as in `docker run`: the program and its arguments are separate
words. stdout, stderr and the exit code are returned exactly as Windows
produced them.

**PowerShell and .NET**

```console
winquick capability install powershell
winquick run -- pwsh -NoProfile -Command '$PSVersionTable'

winquick capability install dotnet-sdk
winquick cache sync                      # restore packages on the host, once
winquick run -w . -- dotnet test
```

`-w .` presents the current directory inside Windows as `C:\workspace` and
makes it the working directory. It is copied in and never copied back, so a
build cannot modify your source.

WinQuick builds .NET Framework 2.0 through 4.8.1, netstandard, and net6.0
through net10.0, including classic non-SDK projects, with no Visual Studio
required. Running a .NET Framework binary additionally needs
`winquick capability install dotnet-framework`.
[docs/dotnet.md](docs/dotnet.md) records what has and has not been measured.

**Retrieving files**

```console
winquick run -w . -a "bin/Release/**" -- dotnet publish -c Release
```

Files are written to `./winquick-artifacts/`, and are collected even when the
command fails. Patterns are relative to the workspace and matched inside
Windows; `**` recurses, a single `*` does not, and `?` matches one character. A
pattern that would leave the workspace is refused before the run starts.

**Desktop applications**

WinQuick can build a WPF or WinForms application, run it in a real Windows
desktop and drive it through UI Automation. Nothing appears on your screen: no
QEMU window, no RDP, no VNC.

```console
winquick capability install desktop      # once, about a minute

winquick start --app ./winquick-artifacts/publish
winquick desktop launch 'app\MyApp.exe'
winquick desktop wait-window --title "Device Configuration"
winquick desktop screenshot before.png
winquick desktop type  --automation-id DeviceNameBox --text "PLC-01"
winquick desktop click --automation-id SaveButton
winquick desktop get   --automation-id StatusText
winquick stop
```

A session starts in about 350 ms and stays up; each step then takes tens of
milliseconds. Controls are addressed by `AutomationId`, and a selector matching
more than one element is an error listing the candidates rather than a guess.

The same sequence can be run as a script:

```console
winquick ui-test MyApp.csproj --script my.uitest --out ./shots
```

```
launch app\MyApp.exe
wait-window --title "Device Configuration"
type --automation-id DeviceNameBox --text "PLC-01"
click --automation-id SaveButton
expect --automation-id StatusText --expect-name "Saved: PLC-01"
screenshot after.png
```

`ui-test` builds the project inside Windows first, so no .NET SDK is needed on
the host. See [docs/desktop.md](docs/desktop.md).

**Coding agents**

WinQuick is an ordinary CLI, so agents, shell scripts and CI use it
identically. One line in a project's README is enough:

```
Windows commands can be run locally with:  winquick run -- <command>
```

It is also a native [MCP](https://modelcontextprotocol.io) server:

```console
claude mcp add winquick -- winquick mcp
```

That provides thirteen tools: `windows_run` for commands, builds and tests;
`desktop_*` to start a session and launch an application; `ui_tree`, `ui_get`,
`ui_click` and `ui_type` to drive it; and `ui_screenshot`, which returns a PNG
in the response. `mcp` is a mode of the same binary — no Node, no Python, no
separate server. See [docs/mcp.md](docs/mcp.md).

## Examples

Every image is from a real run, reproduced by
[`scripts/capture-screenshots.sh`](scripts/capture-screenshots.sh).

![A WPF application running in WinQuick with its text box, combo box and checkbox filled in by UI automation](assets/screenshots/ui-automation.png)

*A WPF application driven through Windows UI Automation: typed into, selected,
toggled, clicked and verified.*

| Your project goes in, and stays untouched | The guest has no network adapter |
|---|---|
| ![A project copied into Windows, with the host file SHA-256 unchanged afterwards](assets/screenshots/workspace.png) | ![The Windows guest reporting zero IPv4 adapters and a failed ping](assets/screenshots/offline.png) |

## What you get

| | |
|---|---|
| Windows | Microsoft Validation OS 10.0.26100 — ARM64 on Apple Silicon, x64 elsewhere |
| Runtime size | 763 MiB |
| `dotnet test` on a small project | ~10 s |

Optional capabilities, installed only on request:

| | Size on disk |
|---|---|
| `powershell` — PowerShell 7.6.5 | 273 MiB |
| `dotnet-runtime` — .NET 10 runtime | 90 MiB |
| `dotnet-sdk` — .NET 10 SDK | 837 MiB |
| `dotnet-framework` — .NET Framework and classic MSBuild | 2.0 GiB image |
| `desktop` — WPF/WinForms, UI automation, screenshots | 3.0 GiB image |

The first three are volumes attached to the guest. The last two are serviced
into a copy of the Windows image, so they are whole images; the pristine
runtime is never written to either way.

## Behaviour worth knowing

Every run is clean. Files, registry keys and environment variables written by
one run are gone in the next, and the Windows image itself is never modified.
That is what makes it safe to hand to an automated agent.

The guest has no network adapter, which removes a large source of run-to-run
variability. Being offline is not by itself a security boundary, and
[docs/security.md](docs/security.md) is precise about what is; `winquick cache
sync` restores NuGet packages on the host so that builds still work.

Each run executes one command, though that command may do as much as you like:
`cmd /c` with operators, a script, or `dotnet test` across a solution. What
does not exist is an interactive shell, and a desktop session is the
long-lived alternative. Output comes back byte-exact when the command
finishes rather than streaming as it is produced, for the reasons set out in
[docs/architecture.md](docs/architecture.md).

Workspace filenames may use any character in the basic multilingual plane.
Characters above U+FFFF cannot be represented on the FAT volume that carries
the workspace, so WinQuick checks the whole tree first and names every
offending path rather than failing partway through.

## Commands

```
winquick setup                          install Windows (once)
winquick run -- <command>               run something
winquick start|stop|status              a Windows session that stays up
winquick desktop <verb>                 drive the session's desktop
winquick ui-test <project>              build a GUI app and test its UI
winquick capability list|install|remove optional tools inside Windows
winquick cache sync|info|clear          offline packages for dotnet
winquick doctor [--smoke]               check the installation
winquick info                           what is installed
winquick reset                          rebuild the prepared guest
winquick clean [--all]                  remove generated data
```

`winquick --help` and `winquick <command> --help` include examples.

## Documentation

- [docs/install.md](docs/install.md) — installing and updating
- [docs/architecture.md](docs/architecture.md) — how it works
- [docs/desktop.md](docs/desktop.md) — the desktop capability and UI automation
- [docs/mcp.md](docs/mcp.md) — the MCP server
- [docs/dotnet.md](docs/dotnet.md) — which .NET targets WinQuick can build
- [docs/security.md](docs/security.md) — the isolation model
- [docs/licensing.md](docs/licensing.md) — what may be redistributed
- [docs/troubleshooting.md](docs/troubleshooting.md) — when something breaks
- [docs/research.md](docs/research.md) — measurements and findings
- [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)

## Licence

WinQuick is Apache-2.0, © Carlboms Data AB. It uses QEMU, ntfsprogs and hivex
as separate programs and ships no Microsoft software. See
[docs/licensing.md](docs/licensing.md).
