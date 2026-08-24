# Changelog

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
