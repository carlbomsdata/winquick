# WinQuick

WinQuick runs software inside disposable local Windows environments on macOS
with Apple Silicon. A run gets a real Windows kernel in a hardware-accelerated
QEMU virtual machine, executes what you asked for, and returns its stdout,
stderr and exit code.

Each run starts from the same clean Windows state and the writes it makes are
discarded when it ends. WinQuick keeps the base image, capabilities and caches
between runs; the run's own machine does not survive it.

It is one binary. QEMU runs the guest as a separate process on the host's own
hypervisor. There is no daemon and no libvirt.

> **Under heavy development.** Commands, capabilities and measured numbers
> change between releases. macOS on Apple Silicon is the supported host; Windows
> and Linux are a future plan, not something to depend on. See
> [Host support](#host-support).

## Install

```console
brew install carlbomsdata/tap/winquick
winquick setup --accept-microsoft-terms
winquick run -- cmd /c ver
```

`winquick setup` downloads Microsoft's Validation OS image (about 2.4 GB) under
Microsoft's licence, writes the runtime, and verifies it by running a real
Windows command. WinQuick ships no Microsoft software. If you already have the
image, use `winquick setup --from <path>` instead.

WinQuick runs on macOS with Apple Silicon only; see
[Host support](#host-support). You need QEMU 11 or newer and about 8 GiB of free
disk. `winquick doctor` checks all of it.

## Run Windows software

The command runs as-is and its exit code is returned unchanged, so `&&`, `||`
and CI logic behave as they would on Windows.

```console
$ winquick run -- cmd /c ver
Microsoft Windows [Version 10.0.26100.8972]

$ winquick run -- ipconfig /all
Host Name . . . . : minwinpc
```

Anything the guest can execute works the same way, including a Windows-only
vendor tool you have no source for:

```console
winquick run -w . -- 'tools\SomeVendorTool.exe' --report
```

`-w` copies a directory into the guest, where it becomes `C:\workspace` and the
working directory. WinQuick copies it in before the run and does not copy it
back: changes made inside Windows do not reach your files.

`-a` names files to bring back afterwards, relative to the workspace. They are
collected even when the command fails, and a pattern that would escape the
workspace is refused before the run starts. `**` recurses; a single `*` does
not.

```console
winquick run -w . -a "bin/Release/**" -- <build command>
```

Each run executes one command. There is no interactive shell; a desktop session
is the long-lived alternative. Output is returned when the command finishes
rather than streamed, for the reasons in
[docs/architecture.md](docs/architecture.md).

## What the guest can run

The guest is Microsoft's Validation OS, a deliberately minimal Windows with the
kernel, registry and shell. Ordinary Windows console programs run against it:
system tools, a vendor CLI you only have as a Windows binary, or something you
just built.

Two limits are worth knowing before you start.

**A program that opens a window needs a desktop session.** Under `winquick run`
it does nothing useful either way: on the base runtime, which has no graphics
stack, it fails with a missing-DLL error, and where a capability has added one
it opens a window nobody can see and runs until the timeout. WinQuick names the
commands that fix it when it can tell the program is graphical.

**Validation OS does not carry every Windows service.** A tool that needs one it
lacks will not work, and no package adds it. `winquick run -- sc query <name>`
answers that for any tool in a second.

What was measured on the ARM64 guest, including which Sysinternals tools run
and which do not, is in [docs/research.md](docs/research.md).

## Optional tools

Validation OS ships no PowerShell and no .NET. Each is a capability you install
once:

| Capability | Adds | Size |
|---|---|---|
| `powershell` | PowerShell 7.6.5 | 273 MiB |
| `dotnet-runtime` | .NET 10 runtime | 90 MiB |
| `dotnet-sdk` | .NET 10 SDK | 837 MiB |
| `dotnet-framework` | .NET Framework and classic MSBuild | 2.0 GiB image |
| `desktop` | Windows desktop, screenshots and UI Automation | 3.0 GiB image |

The first three attach as volumes. The last two are serviced into a copy of the
Windows image, so they are whole images. The base runtime is never written
to either way.

Building .NET needs the SDK capability, and the guest has no network, so
packages are restored on the host first and shared with the guest through a
cache:

```console
winquick capability install dotnet-sdk
winquick cache sync                     # once per project, for its packages
winquick run -w . -- dotnet test
```

For a build specifically, `winquick build` does all of that in one step — it
reads the project, installs the capability it needs, syncs the cache, builds in
a disposable Windows, and keeps the output:

```console
winquick build ./MyApp                  # -> ./winquick-artifacts, or -o <dir>
winquick build ./MyApp --dry-run        # show the plan without building
```

It picks the toolchain from the project's shape (`dotnet build` for SDK-style,
`dotnet msbuild` for classic). A project targeting .NET 3.5 or older is built
against the .NET 3.5 reference assemblies — so the output runs on the old
runtime, not the guest's .NET 4 — and WinQuick verifies the produced binary is
CLR v2.0 before reporting success.

WinQuick can build .NET Framework 2.0 through 4.8.1, netstandard, and net6.0
through net10.0, including classic non-SDK projects. Running a .NET Framework
binary additionally needs `dotnet-framework`.
[docs/dotnet.md](docs/dotnet.md) records what was measured.

## Bringing your own tools

The guest is a real Windows, so it will run anything Windows can — not only
.NET. Anything beyond the .NET capability you bring in yourself: a C toolchain,
Go, Node, Python, a portable CLI. Dropping it in the workspace works, but the
workspace is copied into a fresh guest on every run, so a 300 MiB toolchain
turns a one-second command into a two-minute one.

A tool volume fixes that. Register a directory once and every later `run` and
`build` attaches it — cloned like a capability volume, which on APFS is
effectively free — with its directories already on `PATH`:

```console
winquick tool add zig --from ~/zig-windows-aarch64      # once
winquick run -w . -- zig build-exe hello.c              # zig is on PATH, no re-copy
winquick tool list
winquick tool remove zig
```

`--from` is a directory; its contents become the volume. WinQuick puts a `bin`
subdirectory on `PATH` if the toolchain has one, otherwise the volume root —
override with `--path <dir>` (repeatable) to name the directories yourself.
Tool volumes are cached, frozen into the prepared guest and discarded from each
run exactly like the built-in capabilities; nothing about them is .NET-specific.

## Desktop applications

A desktop session boots Windows once and leaves it running, so each step after
that is a round trip rather than a boot. Nothing appears on your screen: no QEMU
window, no RDP, no VNC.

What it gives you comes in layers, and they are not the same claim:

- **Launch.** Any Windows desktop application the guest can run. Notepad, Task
  Manager and several Sysinternals GUI tools were tested; none of them are .NET.
- **Capture.** A screenshot of the screen or one window, read back from the
  guest's own framebuffer.
- **Drive.** Controls that the application exposes through Microsoft UI
  Automation can be inspected and interacted with, addressed by `AutomationId`
  rather than pixel position. An application that exposes nothing useful through
  UI Automation can still be launched and captured, but not driven.
- **Build.** `ui-test` can build a `.csproj` inside Windows before driving it.
  WPF and WinForms are the project types tested end to end that way; it also
  accepts a directory you already published.

It needs two capabilities:

```console
winquick capability install dotnet-sdk
winquick capability install desktop
```

```console
winquick start --app ./publish
winquick desktop launch 'app\MyApp.exe'
winquick desktop wait-window --title "My App"
winquick desktop type   --automation-id NameBox --text "PLC-01"
winquick desktop click  --automation-id SaveButton
winquick desktop get    --automation-id StatusText
winquick desktop screenshot after.png
winquick stop
```

`winquick ui-test` runs the same verbs as a script and exits non-zero if any
`expect` fails, which is the form worth putting in CI:

```console
winquick ui-test MyApp.csproj --script smoke.uitest --out ./shots
```

A selector matching more than one element is an error rather than a guess.
[examples/WpfDemo](examples/WpfDemo/) is a worked example, and
[docs/desktop.md](docs/desktop.md) covers every verb.

**Desktop sessions only run where a prepared state can be saved.** They always
resume one and have no cold path, which on a Windows host means a self-built
QEMU; see [Host support](#host-support).

## MCP

`winquick mcp` is a native MCP server exposing the same operations as structured
tools, so an agent does not have to construct shell syntax.

```console
claude mcp add winquick -- winquick mcp
```

[docs/mcp.md](docs/mcp.md) lists the tools.

## Host support

**macOS on Apple Silicon is the only supported host.** It is where WinQuick is
developed and where every figure here was measured: 100 consecutive runs of
`cmd /c ver` gave p50 310 ms, p95 317 ms, p99 319 ms, no failures.

| Host | Accelerator | Guest | Status |
|---|---|---|---|
| Apple Silicon macOS 13+ | HVF | Windows ARM64 | **Supported**, ~310 ms warm |
| Windows 10/11 x86_64 | WHPX | Windows x64 | Future plan |
| Linux x86_64 / arm64 | KVM | matches the host | Future plan |

### Future plan: Windows and Linux

The groundwork for other hosts exists in the tree but neither is supported or
released. Do not depend on them.

- **Windows (WHPX).** `winquick run` boots and runs, but cold-boots every run: a
  resumed guest under WHPX waits far longer than asked on a timer — 212 s for
  `ping -n 4` against 20 s cold. Saving guest state needs a patched QEMU (stock
  WHPX registers an unconditional migration blocker); the patches in
  [patches/](patches/) are not applied to anything WinQuick ships. Without them
  `--warm`, `start`, `desktop` and `ui-test` do not work.
  [docs/windows-host.md](docs/windows-host.md) has the detail.
- **Linux (KVM).** The binary builds and `winquick doctor` reports the host
  correctly, but no Windows guest has been booted on physical Linux hardware, so
  guest execution is unverified.

Windows on ARM64 and Intel Macs are not planned.

## Isolation

WinQuick is not a hardened malware sandbox and has not been audited as one. It
runs your own builds and tests inside a hardware hypervisor boundary. What that
boundary actually is:

| Direction | What crosses |
|---|---|
| host to guest | the command, a copy of the workspace, capability volumes, the package cache |
| guest to host | stdout, stderr, the exit code, and files named with `-a` |

- The guest is started with `-nic none`, so QEMU creates no network device and
  no network backend. There is nothing for the guest to bind a driver to.
- The workspace is copied in as a fresh filesystem image per run. The guest
  writes to its copy.
- The base image is opened read-only and a copy-on-write overlay is discarded
  when the run ends, along with the rest of the run directory.
- The host reaches the network only for `winquick setup`, `winquick capability
  install` and `winquick cache sync`. There is no telemetry, update check or
  crash reporting.

[docs/security.md](docs/security.md) states what is not protected.

## Commands

```
winquick setup                          install the Windows runtime (once)
winquick run -- <command>               run a command
winquick build <project>                build a project and keep the output
winquick start|stop|status              a session that stays up
winquick desktop <verb>                 drive the session's desktop
winquick ui-test <project>              build a GUI app and test its UI
winquick capability list|install|remove optional tools inside Windows
winquick cache sync|info|clear          offline packages for dotnet
winquick doctor [--smoke]               check the installation
winquick info                           what is installed
winquick reset                          rebuild the prepared guest
winquick clean [--all]                  remove generated data
```

`winquick <command> --help` includes examples.

## Documentation

- [docs/install.md](docs/install.md) — installing and updating
- [docs/architecture.md](docs/architecture.md) — how it works
- [docs/desktop.md](docs/desktop.md) — the desktop capability and UI automation
- [docs/mcp.md](docs/mcp.md) — the MCP server
- [docs/dotnet.md](docs/dotnet.md) — which .NET targets build
- [docs/windows-host.md](docs/windows-host.md) — the Windows host
- [docs/security.md](docs/security.md) — the isolation model
- [docs/licensing.md](docs/licensing.md) — what may be redistributed
- [docs/signing.md](docs/signing.md) — signing and notarizing the macOS release
- [docs/troubleshooting.md](docs/troubleshooting.md) — when something breaks
- [docs/research.md](docs/research.md) — measurements and findings
- [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)

## Licence

Apache-2.0, © Carlboms Data AB. WinQuick uses QEMU, ntfsprogs and hivex as
separate programs and ships no Microsoft software. See
[docs/licensing.md](docs/licensing.md).
