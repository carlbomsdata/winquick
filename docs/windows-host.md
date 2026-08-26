# Running WinQuick *on* Windows — where this stands

WinQuick's guest has always been Windows. This is about the other half: making
Windows a **host**, so a developer on Windows x86_64 gets the same product a
developer on an Apple Silicon Mac gets.

**Status: not implemented.** This document records what was measured, what the
work actually is, and the one question that has to be answered before any of it
is worth writing.

## The rule that must not be broken

A Windows host must not shortcut the product:

```
    WinQuick on Windows  ─X→  CreateProcess("cmd.exe")
```

Running commands on the host's own Windows installation would be a different
product. WinQuick means a *disposable* Windows: a guest that starts clean,
executes, and is discarded, leaving the host untouched. On Windows that still
means a virtual machine.

```
    Windows host
        ↓
    accelerated hypervisor
        ↓
    disposable Windows x86_64 guest
        ↓
    command / build / test / desktop
        ↓
    guest discarded
```

## The question that gates everything

WinQuick is fast because it does not boot Windows. It restores a **prepared
migration state** into a fresh QEMU process — that is what turns a ~20 second
boot into a ~300 ms command. Every other property is downstream of it.

So the first thing to establish on Windows is not whether the code compiles.
It is whether this works:

1. boot the x64 Validation OS under QEMU with a Windows hardware accelerator
2. reach the normal guest-ready state
3. write the prepared migration state
4. kill QEMU
5. start a new QEMU process and restore that state
6. run a real command
7. repeat enough times to know whether it is stable, not merely possible

If QEMU with WHPX preserves migration state reliably, the existing architecture
carries over and the port is mostly a host seam. If it does not, the honest
answer is a bounded study of a Windows-native backend that can still deliver
disposable guests, fast restore, deterministic clean state, headless operation
and a programmatic lifecycle — and only then a decision.

**This experiment has not been run.** It needs a real Windows x86_64 machine
with hardware virtualization enabled; nested virtualization on a shared CI
runner does not answer the question. Until it is run, writing the port would
mean guessing the backend, and the backend determines the shape of everything
else.

## What the code audit found

Measured against the current tree with
`cargo check --target x86_64-pc-windows-msvc`:

**16 compile errors, in 6 files.** Every one is in the host seam, not in the
product logic:

| Kind | Where | Count |
|---|---|---|
| `std::os::unix` imports | helpers, state, qmp, lock, capability, mcp | 7 |
| `MetadataExt::blocks` (allocated size) | capability, facts, state | 3 |
| `MetadataExt::mtime`/`ino` (image identity) | state | 3 |
| `as_raw_fd` / `from_raw_fd` (MCP stdout capture) | mcp | 2 |
| accelerator, binary name, firmware discovery | qemu, helpers, runner | conditional |

That is a small, well-localised surface — the guest protocol, workspace,
artifact, capability, desktop and MCP layers are already platform-neutral. The
port is not blocked by the language. It is blocked by the backend decision.

Other host assumptions the audit catalogued: `hvf` as the accelerator (5
references), `qemu-system-aarch64` as the binary name (3 files), Homebrew paths
for dependency discovery (12), `hdiutil` for mounting Microsoft media (9), Unix
signals for process cleanup (6), and `win-arm64` as the capability RID (4).

## What the work would be

Sketched so the estimate is honest, not so it can be started blind:

- **A host backend seam.** Executable and firmware discovery, accelerator
  selection, process invocation and containment, dependency detection,
  filesystem locking, native path handling. Guest semantics — workspace,
  artifacts, capabilities, the command protocol, desktop, MCP — stay shared.
- **Process containment.** Unix signal handling does not translate. Windows
  wants **Job Objects**, so that a killed or interrupted WinQuick cannot strand
  `qemu-system-x86_64.exe`.
- **File locking.** Windows share semantics are stricter; every image, overlay,
  prepared state and control disk needs its handle lifetime checked rather than
  papered over with retries.
- **Architecture-aware capabilities.** PowerShell, the .NET runtime and SDK,
  and the desktop bridge all resolve architecture-specific payloads today
  (`win-arm64`). One logical capability must resolve `win-x64` too, and a
  prepared state must be fingerprinted by host OS, host architecture, guest
  architecture, backend and version so an ARM64 capability can never attach to
  an x64 guest.
- **Guest components.** The desktop bridge is currently a **native ARM64**
  binary. An x64 guest needs an x64 build; shipping the ARM64 one would simply
  not run.
- **Media.** `setup` must fetch the x64 Validation OS rather than the ARM64
  image, and `hdiutil` has no Windows equivalent — mounting Microsoft media
  needs a native path.
- **Distribution.** A Windows user should not assemble a QEMU directory by
  hand. Whether that means a pinned dependency, a bundled runtime with its GPL
  obligations satisfied, or WinGet is a decision that follows the backend
  choice.

## Why this is written down instead of built

The instruction that shaped this was: prove the restore mechanism *before*
porting. That was the right instruction, and it cuts both ways — without a
Windows machine to prove it on, the disciplined thing is to stop at the audit
rather than write a large amount of virtualization code that cannot be run,
against a backend that has not been chosen.

Apple Silicon macOS remains the supported host.
