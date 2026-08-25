# WinQuick

**Instant disposable Windows environments.**

Run a real Windows command from your Mac in about a quarter of a second, in a
clean Windows that is thrown away afterwards.

```console
$ winquick run -- cmd /c ver

Microsoft Windows [Version 10.0.26100.8972]
```

A real Windows ARM64 kernel under QEMU with Apple's Hypervisor Framework —
not Wine, not an emulator, not a container. Every run starts from a pristine
image and leaves nothing behind.

| | |
|---|---|
| Windows command | **~288 ms** |
| PowerShell command | ~840 ms |
| Desktop session start | **~380 ms** |
| UI automation step in a session | ~20 ms |
| Host | Apple Silicon macOS (Linux and Windows hosts planned) |

## Why

Building or testing Windows software from a Mac usually means keeping a Windows
VM alive: tens of gigabytes, minutes of boot, snapshots that rot, a desktop to
click through. That is far too heavy for "run the test suite once" — and much too
heavy for a coding agent that wants to do it fifty times an hour.

WinQuick does the narrow thing that matters for builds, tests and automation: run
one command inside a genuine Windows environment, get the exact output and exit
code back, throw the environment away.

The mental model is `docker run --rm`, with a real Windows kernel on the other
end.

## Install

```console
brew install Carlboms-Data-AB/tap/winquick
winquick setup
```

Setup needs Microsoft's Windows validation runtime, which Microsoft distributes
under its own licence — WinQuick cannot ship it for you. It will offer to
download it, or take a file you already have:

```console
winquick setup --accept-microsoft-terms     # download it (about 2.4 GB)
winquick setup --from ~/Downloads/vos.iso   # use a file you already have
```

Setup finishes by booting Windows and running a real command, so it only says
"Ready" when it actually is. It takes about a minute.

Requirements: an Apple Silicon Mac (M1 or newer) and macOS 13 or later.

Not on Homebrew yet? See [docs/install.md](docs/install.md) for the release
archive.

## Use it

**Run anything**

```console
winquick run -- cmd /c ver
winquick run -- cmd /c "echo A & echo B"
```

Arguments work like `docker run`: the program and its arguments are separate
words, and anything containing spaces stays one argument. stdout, stderr and the
exit code come back exactly as Windows produced them.

**PowerShell**

```console
winquick capability install powershell
winquick run -- pwsh -NoProfile -Command '$PSVersionTable'
```

**.NET**

```console
winquick capability install dotnet-sdk
cd MyProject
winquick cache sync                      # restore packages on your Mac, once
winquick run -w . -- dotnet test
```

`-w .` makes the current directory appear inside Windows as `C:\workspace` and
become the working directory. It is copied in and never copied back, so a build
cannot change your source.

**Get files back out**

```console
winquick run -w . -a "bin/Release/**" -- dotnet publish -c Release
```

Files land in `./winquick-artifacts/`. They are collected even when the command
fails — a failed build's logs are usually the point — and the exit code is passed
through untouched.

**Windows desktop applications**

WinQuick can build a WPF or WinForms application, run it in a real Windows
desktop, show you what it looks like, and drive it. Nothing appears on your
screen: no QEMU window, no RDP, no VNC.

```console
winquick capability install desktop      # once, about a minute
```

Build it, launch it, look at it, work it:

```console
# Build for Windows and bring the output back
winquick run -w . -a "publish/**" -- dotnet publish -c Release -o publish

# Start a Windows desktop with that build available to it
winquick desktop start --app ./winquick-artifacts/publish
winquick desktop launch app\MyApp.exe
winquick desktop wait-window --title "Device Configuration"

# See it
winquick desktop screenshot before.png

# Inspect its controls
winquick desktop tree --title "Device Configuration"

# Work it
winquick desktop type   --automation-id DeviceNameBox --text "PLC-01"
winquick desktop select --automation-id ModeCombo --item Diagnostic
winquick desktop toggle --automation-id LoggingCheck --state on
winquick desktop click  --automation-id SaveButton
winquick desktop get    --automation-id StatusText

winquick desktop screenshot after.png
winquick desktop stop
```

A session starts in about 380 ms and stays up; each step after that takes tens
of milliseconds. It is not booting Windows that fast — it restores a Windows
that already booted. Controls are addressed by `AutomationId`, and a selector matching
more than one element is an error listing the candidates rather than a guess.

Or put the whole thing in a script and run it in one command:

```console
winquick ui-test MyApp.csproj --script my.uitest --out ./shots
```

```
launch app\MyApp.exe
wait-window --title "Device Configuration"
expect --automation-id SaveButton --expect-enabled false
type --automation-id DeviceNameBox --text "PLC-01"
click --automation-id SaveButton
expect --automation-id StatusText --expect-name "Saved: PLC-01"
screenshot after.png
```

`ui-test` builds the project inside Windows first, so no .NET SDK is needed on
your Mac. See [docs/desktop.md](docs/desktop.md).

**Coding agents**

WinQuick is a normal CLI, so Claude Code, Codex, Cursor, shell scripts and CI all
use it the same way. One line in your project's README is enough:

```
Windows commands can be run locally with:  winquick run -- <command>
```

A fresh Claude Code session given that line diagnosed and fixed four Windows-only
bugs in a .NET project, verifying each fix against a real Windows kernel, without
knowing anything about how WinQuick works. See
[experiments/dogfood](experiments/dogfood/).

## What you get

| | |
|---|---|
| Windows | Microsoft Validation OS, build 10.0.26100 ARM64 |
| Runtime size | 763 MiB |
| Trivial command | ~288 ms |
| PowerShell command | ~840 ms |
| `dotnet --version` | ~500 ms |
| `dotnet test` on a small project | ~10 s |
| Desktop session start | ~380 ms, then ~20 ms per UI step |

Optional capabilities, installed only if you ask:

| | Size on disk |
|---|---|
| `powershell` — PowerShell 7.6.5 | 273 MiB |
| `dotnet-runtime` — .NET 10 runtime | 90 MiB |
| `dotnet-sdk` — .NET 10 SDK | 837 MiB |
| `desktop` — WPF/WinForms, UI automation, screenshots | 2.0 GiB |

## Every run is clean

Files, registry keys and environment variables written by one run are gone in the
next. The Windows image itself is never modified. That is what makes it safe to
hand to an automated agent that might do anything.

## Known limits

- **Apple Silicon only.** No Intel Macs, no Linux, no Windows hosts.
- **Windows has no network access.** This is deliberate — it is what makes runs
  reproducible and safe. `winquick cache sync` restores NuGet packages on your
  Mac and shares them with Windows offline.
- **GUI needs the desktop capability.** The base runtime has no graphics at all.
  With `winquick capability install desktop` you get a real composited desktop,
  WPF and WinForms, UI Automation and screenshots — still headless from the Mac's
  point of view. QEMU's own framebuffer stays blank even then; screenshots are
  captured inside the guest, which also lets you frame a single window.
  [docs/desktop.md](docs/desktop.md) explains why.
- **One command per run**, and output arrives when the command finishes rather
  than streaming.
- Artifact patterns are three shapes, not full globbing — see
  [docs/troubleshooting.md](docs/troubleshooting.md).

## Commands

```
winquick setup                          install Windows (once)
winquick run -- <command>               run something
winquick capability list|install|remove optional tools inside Windows
winquick cache sync|info|clear          offline packages for dotnet
winquick desktop start|stop|status|...  drive a real Windows desktop
winquick ui-test <project>              build a GUI app and test its UI
winquick doctor [--smoke]               check the installation
winquick info                           what is installed
winquick reset                          rebuild the prepared guest
winquick clean [--all]                  remove generated data
```

`winquick --help` and `winquick <command> --help` have examples.

## Documentation

- [docs/install.md](docs/install.md) — installing and updating
- [docs/architecture.md](docs/architecture.md) — how it works
- [docs/desktop.md](docs/desktop.md) — the desktop capability and UI automation
- [docs/security.md](docs/security.md) — the isolation model, precisely
- [docs/licensing.md](docs/licensing.md) — what may be redistributed
- [docs/troubleshooting.md](docs/troubleshooting.md) — when something breaks
- [docs/research.md](docs/research.md) — measurements and findings
- [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)

## Licence

WinQuick is Apache-2.0, © Carlboms Data AB. It uses QEMU, ntfsprogs and hivex as
separate programs and ships no Microsoft software. See
[docs/licensing.md](docs/licensing.md).
