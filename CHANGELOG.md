# Changelog

## v0.2.1 — hardening

A hardening release. Everything here came from using v0.2.0 as an outside user
would: an isolated environment, a fresh HOME, and the published archive rather
than a development build.

Validated with 66 unit tests and 118 integration checks (0 failures), a
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
