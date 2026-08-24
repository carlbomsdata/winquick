# DevicePrep

A small provisioning helper for Windows devices: it stores per-operator settings
in the registry, resolves deployment-manifest paths, talks to the worker service
over a named pipe, and writes a diagnostic report for support.

The code targets `net10.0-windows`. It builds on macOS, but most of it only
*means* anything under a real Windows kernel — the registry, the Win32 calls and
the named pipe all need one.

## Building

```console
dotnet build
```

## Testing

The test suite has to run on Windows. Windows commands can be executed locally
with:

```console
winquick run -- <command>
```

## Layout

```
src/DevicePrep          the library
tests/DevicePrep.Tests  xunit tests
```
