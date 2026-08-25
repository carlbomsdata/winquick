# Changelog

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
