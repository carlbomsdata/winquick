# Changelog

## Unreleased

### Fixed

- **`winquick cache sync` could report "already up to date" while the guest saw
  none of the new packages.** Freshness was judged by what that particular
  restore added, so packages arriving any other way — an earlier sync whose
  rebuild failed, or a `dotnet restore --packages` run by hand — never reached
  the volume. It now compares the cache against what the volume was built from.
  Packages are also counted per *version* rather than per id, so a second
  version of a package already present is noticed.

### Added

- **Windows host: the shared core now compiles.** `cargo check --target
  x86_64-pc-windows-msvc` passes with no errors, down from 16. The platform
  seam is isolated in `src/hostfs.rs` — allocated size, file identity, advisory
  locking and the QEMU monitor transport — and the MCP stdout guarantee is
  ported to the Windows CRT. Nothing about the guest, workspace, artifact,
  capability, desktop or MCP layers needed to change.
- **Windows x86_64: the backend works and a real command ran.** With three
  small QEMU patches ([`patches/`](patches/)), a stopped guest can be saved and
  restored under WHPX: 20 fresh QEMU processes restored the same immutable
  147 MB state over the native `file:` transport, 20/20, p50 962 ms, hash
  unchanged. A prepared x64 Validation OS then executed `cmd /c ver` through
  WinQuick's existing mailbox protocol and returned
  `Microsoft Windows [Version 10.0.26100.8972]` with exit code 0.
  `src/platform.rs` now carries the host differences — QEMU binary,
  accelerator, machine, CPU model, firmware — and they are part of the
  prepared-state fingerprint. The port is not finished: `setup` and `run` do
  not yet drive the Windows backend, so Apple Silicon macOS remains the only
  supported host. See [docs/windows-host.md](docs/windows-host.md).
- **[docs/dotnet.md](docs/dotnet.md) — an empirical .NET build matrix.** Which
  target frameworks WinQuick can build, which the standard guest can also run,
  and what the produced binaries actually are. .NET Framework 2.0 through
  4.8.1, netstandard 2.0/2.1 and net6.0 through net10.0 all build; the guest
  carries no .NET Framework runtime, so those build but do not run there.
  Includes an **x86 WinForms application targeting .NET Framework 4.0** — a
  Windows XP-era target — built from a classic non-SDK project with no Visual
  Studio anywhere, and verified by reading the output's PE and CLR metadata.
- **`tests/peinfo.py`** — reads an assembly's real machine type, CLR flags,
  metadata version and stamped `TargetFrameworkAttribute`, so a claim about a
  build rests on the binary rather than on the project file.
- Fixtures for the whole matrix under `experiments/dotnet-matrix/`.

- **[docs/windows-host.md](docs/windows-host.md)** — what running WinQuick *on*
  Windows would take, measured rather than guessed: 16 compile errors across 6
  files, all in the host seam, and the prepared-state restore experiment that
  has to be answered on real hardware before the backend can be chosen. Not
  implemented; Apple Silicon macOS remains the supported host.

### Changed

- Public URLs and the Homebrew command use the canonical `carlbomsdata`
  namespace rather than depending on redirects from the old organisation name.
  The install command is now `brew install carlbomsdata/tap/winquick`.

## v0.3.0 — native MCP

WinQuick is now a Model Context Protocol server, so an AI agent can build, run
and verify Windows software through structured tools instead of shell syntax.

```console
claude mcp add winquick -- winquick mcp
```

### Added

- **`winquick mcp`** — a native MCP server over stdio, built into the same
  binary. No Node, no Python, no separate executable. It calls the same internal
  functions the CLI calls rather than shelling out and parsing terminal output.
- **Thirteen tools.** `windows_run` for disposable Windows commands, builds and
  tests, with workspace and artifact support; `desktop_start`, `desktop_stop`,
  `desktop_status`, `desktop_launch` and `desktop_wait_window` for a real
  Windows desktop; `ui_tree`, `ui_get`, `ui_click` and `ui_type` for Microsoft
  UI Automation; `ui_screenshot`, which returns a real PNG **in the response**;
  and `winquick_info` and `winquick_doctor` as structured data.
- **`docs/mcp.md`**, and a companion
  [agent skill](https://github.com/carlbomsdata/winquick-agent-skill).

### Changed

- `winquick info` and `winquick doctor` now render from one structured source
  of truth, which is also what MCP serialises. `doctor` distinguishes `ok`,
  `note` and `fail`, so "no prepared guest yet" no longer reads as a fault.
- The experiment write-ups use neutral paths rather than one machine's.

### Semantics

- A non-zero Windows exit code is a **successful** tool result carrying that
  code, not a transport failure. Tool-level problems — no desktop session, a
  selector that matched nothing, a timeout — are results with `isError` and a
  readable reason. Only malformed requests are JSON-RPC errors.
- **A persistent MCP process is not a persistent Windows VM.** A VM exists only
  during a `windows_run`, or between `desktop_start` and `desktop_stop`. A
  desktop session started over MCP is stopped when the client disconnects.
- Nothing but protocol traffic reaches stdout: the server takes the real stdout
  at startup and redirects everything else to stderr, so no log line from
  anywhere in WinQuick can corrupt the connection.

### Verified

106 unit tests, 125 integration checks, 74 MCP protocol checks and 45 MCP
desktop checks, all passing. MCP adds about 4 ms over the CLI for a warm command
(288 ms against 284 ms at the median); the server answers `initialize` in about
3 ms. A fresh Claude Code session, given no WinQuick syntax, used the MCP tools
for 45 of its 54 tool calls, found and fixed five planted defects in a WPF
application, and verified all twelve of its requirements against the running
Windows UI.

Guest networking, output streaming, HTTP/remote MCP, multiple desktop sessions
and non-BMP filenames remain unavailable.

## v0.2.1 — hardening

A hardening release. Everything here came from using v0.2.0 as an outside user
would: an isolated environment, a fresh HOME, and the published archive rather
than a development build.

Validated with 67 unit tests and 122 integration checks (0 failures), a
100-command soak, a 50-cycle desktop soak, and a 50-repetition stress of the
live desktop sequence.

No new features. Guest networking, streaming output, Linux and Windows hosts,
and filenames above U+FFFF all remain unavailable; see **Investigated and
deferred** below and the README's *Current scope*.

### Fixed

- **Quoting reaching `cmd` was corrupted.** `winquick run -- cmd /c 'echo say
  "hi"'` printed `say \"hi\"`, and a quoted path failed outright. The command
  line is now built for whoever parses it: `cmd` gets what you wrote, a program
  gets arguments its own runtime will split correctly. Removing the escaping
  outright was tried first and broke PowerShell, so both rules are kept and the
  first argument chooses.
- **A cmd metacharacter after a quote split the command line.** `winquick run
  -- pwsh -Command 'Write-Output "a&b"'` failed with `'b\""' is not
  recognized`. The C runtime's `\"` is invisible to `cmd`, which counts plain
  quotes, so after an escaped quote cmd believed it was outside quotes and read
  `&` as an operator. Metacharacters cmd would see as unquoted are now
  `^`-escaped; the program's own argv is unchanged.
- **A filename outside the basic multilingual plane** — an emoji, usually —
  aborted the whole workspace transfer with a message that named no file. The
  tree is now checked before anything is copied, and every offending path is
  listed. Accents, CJK, Cyrillic and Greek were never affected.
- **`winquick desktop screenshot --hwnd`** exists, so one of two windows sharing
  a title can be captured. `get` and `tree` already accepted it.
- **Ctrl-C during `winquick desktop start`** exits 130 and says whether a session
  was left running, instead of exiting 0 in silence.
- **Clicking a disabled control** says `cannot click: SaveButton is disabled`
  rather than `Unrecognized error`.
- **A mistyped option is named.** `--id` reported "no selector given"; it now
  reports the unknown option and lists the ones that exist.
- **An unknown desktop verb is a syntax error**, reported without needing a
  session. It used to surface as "no desktop session is running".
- **`winquick desktop <verb> --help` answers**, without a session and without
  the desktop capability installed. It used to be unobtainable either way: with
  no session it said "no desktop session is running", and with one the bridge
  rejected `--help` as an unknown option and listed every option of every verb.
- **`winquick doctor` notices a missing guest bridge**, which previously only
  failed later, at `desktop start`.

### Improved

- **Artifact patterns are a real glob subset**: `**/*.dll`, `bin/**/*.exe`,
  `logs/*.txt`, `foo?.txt`, `bin/Release/**` and named files. A single `*` is one
  directory deep and `**` recurses, as everywhere else — in v0.2.0 `dir/*` meant
  the whole tree. Patterns that try to leave the workspace are refused before the
  run starts.
- **`winquick info` reports the desktop capability**, its prepared state and any
  running session.
- **`winquick desktop start` lists everything still missing at once**, in the
  order it has to be done, instead of one failed command at a time.
- Getting-started help introduces the desktop.
- Documentation: WinForms needs `Control.Name` for an AutomationId, `%` follows
  batch rules, `--hwnd` disambiguates windows, and the measured numbers agree
  with each other.
- README's *Known limits* is now *Current scope*, rewritten so each entry says
  what WinQuick does and why, rather than restating a list of things it does
  not. The artifact-pattern entry is gone because the limitation is gone.

### Investigated and deferred

- **Opt-in guest networking.** Attaching a NIC enumerates
  `PCI\VEN_1AF4&DEV_1000` and Windows binds nothing to it. `netkvm` for ARM64
  exists and could be staged with `dism /Add-Driver`, but only the desktop image
  is serviced today, so this means adding a servicing pass to `winquick setup`.
  Deferred; offline stays the default regardless.
- **Live output streaming.** The guest has no channel that reaches the host
  while a command runs: FAT only synchronises at dismount, and the PL011 serial
  port enumerates as `ACPI\ARMH0011` with no driver bound and no `COM1`. A raw
  control disk works — that is what desktop sessions use — and needs a compiled
  program in the guest. Deferred.
- **The desktop capability's `dotnet-sdk` dependency.** A standalone Windows
  Desktop runtime for `win-arm64` exists at 34 MB, but it is an add-on without a
  base runtime, so a smaller capability means merging two archives into one
  volume. Practical; deferred to keep this release small.

## v0.2.0 — the desktop capability

### Windows GUI applications, built and driven from the Mac

Windows GUI applications, built and driven from the Mac.

```console
$ winquick capability install desktop
$ winquick ui-test MyApp.csproj --script my.uitest --out ./shots
 1. launch app\MyApp.exe  OK (pid 760)
 2. wait-window --title "My App"  OK (waitedMs 637)
 3. screenshot before.png (620x460, 100% non-black)
 4. click --automation-id SaveButton  OK (via Invoke)
 5. expect --automation-id StatusText name = "Saved"  OK
```

- `winquick capability install desktop` builds a desktop-capable Windows image
  by running DISM *inside WinQuick* against a copy of the existing runtime. No
  Windows machine, no downloads, and nothing Microsoft-licensed is
  redistributed — the packages come from the ISO you already supplied to
  `winquick setup`.
- Real rendering: a VirtIO GPU plus Red Hat's `viogpudo` display driver, staged
  with `dism /Add-Driver`. Windows reports a primary display adapter at
  1280x800x32 and DWM composites normally.
- `winquick desktop start|stop|status` keeps a session up, so each verb after
  the ~10 s boot is a few milliseconds rather than another boot.
- UI Automation as the interface: `tree`, `find`, `get`, `click`, `type`,
  `select`, `toggle`, `key`, `mouse`, `focus`, `windows`, `display`. Elements
  are addressed by `AutomationId`; a selector matching more than one element is
  an error listing the candidates, never a guess.
- `winquick desktop screenshot` returns a real PNG of the composited desktop, or
  of a single window.
- `winquick ui-test <project|dir>` builds the project inside Windows, runs a
  script of UI steps against it, and writes the screenshots to your Mac.
- `examples/WpfDemo` is a real WPF application covering `TextBlock`, `TextBox`,
  `ComboBox`, `CheckBox`, `Button` and `ListBox`, with `demo.uitest` driving all
  of them.

### Fixed

- The mailbox now writes the command and arms the go flag in two separate
  flushed passes, and the flag carries the run's token. A guest polling the
  volume could previously see the flag before the command behind it and run the
  previous command under the new token.
- The guest agent reads that token with `for /f` rather than `set /p`. On an
  empty file `set /p` falls back to reading the console, which wedged the agent
  permanently.

### Fast

A desktop session starts in **~380 ms**, down from 9.3 s.

It stopped booting Windows. `winquick run` has always frozen a booted guest with
QEMU's migration and restored it per run; a desktop session now does the same,
frozen at the point where the bridge is already answering rather than at the
login prompt. Preparing that state costs about 17 s, once, after the capability
is installed or anything about the machine changes.

Measured over 30 consecutive sessions, each verified by launching a WPF
application, reading its UI Automation tree and taking a screenshot before being
stopped: 30/30, min 373 ms, p50 380 ms, mean 382 ms, p95 399 ms, max 402 ms.

`winquick run` is unchanged: 30 runs, p50 308 ms, max 323 ms, no failures.

Two other things came out of that work:

- **Default session sizing is now 2 processors and 2048 MiB**, measured rather
  than assumed. Four processors is no faster at anything a session does, and a
  `winquick run` issued alongside a four-processor session used to take minutes;
  alongside a two-processor one it takes 290 ms. Halving the memory took a start
  from 507 ms to 349 ms and resident size from 4.6 GiB to 2.3 GiB.
- **Installing the desktop capability no longer invalidates the command guest.**
  The internal build ran with different memory than `winquick run` defaults to,
  so the next ordinary command silently paid for a full rebuild.

### Found by dogfooding it

A fresh Claude Code session was given a WPF utility with five planted defects
and told only to make it satisfy its requirements file. It fixed all five. What
it stumbled over became these fixes:

- Options a verb does not understand are now refused, listing what it does
  understand. `--class-name` instead of `--class` used to drop silently out of
  the selector and return a confident answer about a different element.
- `winquick desktop tree --automation-id X` scopes to that element instead of
  ignoring the selector and dumping the whole window.
- `expect --expect-enabled true|false` in scripts. "Save is disabled until a
  name is entered" was the single most important requirement in that app and
  the one assertion the script language could not express.
- `get` on a combo box reports its selection as the value. A non-editable combo
  box exposes no value pattern, so the selection was previously only reachable
  by walking children looking for `selected: true`.
- Asserting a property an element does not have says so, and suggests the
  assertion that fits, instead of comparing against an empty string.
- `--help` lists all six assertions instead of trailing off in `...`.
- **A desktop session no longer writes to the installed capability volumes.**
  It gets clones, as `winquick run` always has — Windows writes to a volume when
  it mounts it, and `dotnet-sdk.img` was changing underneath.
- **Session start is no longer flaky.** The bridge scanned for its control disk
  once, before the guest had finished enumerating devices, and about one start
  in ten never came up. It retries now: 0 failures in 12, mean 9.3 s. A failure
  also reports what the bridge printed rather than only that it did not answer.

### Known limits

- QEMU's own framebuffer stays blank even with the display driver bound, so
  screenshots are captured inside the guest. `--host` still exposes the QMP
  path. [docs/desktop.md](docs/desktop.md) has the measurements.
- One desktop session at a time. A running session is a four-processor virtual
  machine, so a `winquick run` issued while one is up is slow — stop it first.
- The desktop capability requires the `dotnet-sdk` capability, which supplies
  the Windows Desktop runtime the bridge and WPF applications run on.

## v0.1.0 — first release

Run real Windows commands on an Apple Silicon Mac.

```console
$ winquick run -- cmd /c ver
Microsoft Windows [Version 10.0.26100.8972]
```

### What works

- A real Windows ARM64 kernel under QEMU with Apple's Hypervisor Framework,
  started and discarded per command. About 270 ms for a trivial command.
- Exact stdout, stderr and exit-code passthrough.
- Every run is clean: filesystem, registry and environment changes never survive,
  and the Windows image itself is never modified.
- `winquick setup` builds the runtime from Microsoft's Validation OS image, then
  proves it works by booting Windows and running a command.
- Optional capabilities: PowerShell 7.6.5, .NET 10 runtime, .NET 10 SDK.
- Projects: `-w <dir>` appears inside Windows as `C:\workspace`, copied in and
  never copied back.
- Artifacts: `--artifact` brings specific files out, including after a failed
  command.
- Offline package cache for `dotnet`, populated on the Mac and shared read-only
  in effect with Windows.
- `winquick doctor`, `info`, `reset`, `clean`.
- Concurrent runs, Ctrl-C, and timeouts all behave: no orphaned VMs, no leftover
  state.

### Known limits

- Apple Silicon macOS only.
- Windows has no network access, by design.
- Headless: no GUI, and GDI+ is absent, so WinForms/WPF compile and their
  non-visual code runs, but windows and dialogs do not.
- One command per run; output arrives when the command finishes.
- Artifact patterns support three shapes, not full globbing.
