# Dogfood: can a coding agent use WinQuick to fix Windows-only bugs from a Mac?

Three fresh `claude` sessions were run headlessly against the same project. None
of them had any knowledge of how WinQuick works — no internals, no QEMU, no
mention of Validation OS or capability volumes.

## The test project

`DevicePrep` — a small provisioning helper targeting `net10.0-windows`, kept in a
separate repository. Five source files, nine xunit tests, exercising behaviour
that only means anything under a real Windows kernel:

| Area | What it does |
|---|---|
| Registry | per-operator settings under `HKCU\Software\DevicePrep` |
| Win32 P/Invoke | `GetSystemDirectoryW`, `GetTickCount64` from `kernel32.dll` |
| Path semantics | manifest paths → Windows paths; case-insensitive comparison |
| Named pipes | local control-channel round trip |
| Diagnostics | a report file combining several of the above |

It **builds** on macOS (`EnableWindowsTargeting`), which is the point: compiling
proves nothing.

## Intentional bugs

Four independent defects, none of them syntactic:

1. **Registry hive mismatch** — `Save` writes `HKEY_CURRENT_USER`, `Load` reads
   `HKEY_LOCAL_MACHINE`. Saved values are never found.
2. **P/Invoke charset** — `GetSystemDirectoryW` declared `CharSet = CharSet.Ansi`.
   The wide API writes UTF-16; the ANSI unmarshaller stops at the first NUL, so
   `C:\Windows\System32` comes back as `"C"`.
3. **Manifest path join** — `FromManifest` splits on `/` and rejoins with `/`,
   producing `C:\DevicePrep/tools/agent/agent.exe`.
4. **Case-sensitive comparison** — `SamePath` uses `StringComparison.Ordinal`
   despite its own comment promising Windows case-insensitivity.

A fifth suspicious-looking construct was left in deliberately as a control: the
named pipe constant includes the `\\.\pipe\` prefix. This is **not** a bug — it
works on Windows — and it is there to see whether an agent changes working code
it cannot test.

## Baselines

**macOS** (`dotnet test`): 7 failed, 2 passed — every failure a platform artefact
(`DllNotFoundException: kernel32.dll`, `NullReferenceException` from
`Registry.CurrentUser`). macOS gives **zero signal** about Windows correctness.

**Windows** (`winquick run -w . -- dotnet test`): 5 failed, 4 passed, each failure
traceable to one of the four defects. Unambiguous.

## Session A — WinQuick available, one line in the README

Prompt, verbatim:

```
Fix this project so all tests pass on Windows.
You are working on a Mac. Use the tools available in the repository/environment as needed.
```

The only WinQuick documentation was one line in the project README:
`winquick run -- <command>`.

| | |
|---|---|
| Wall clock | **191 s** |
| Turns | 23 |
| Tool calls | 22 |
| WinQuick invocations | **6** (3 of them `dotnet test` runs) |
| Edit/test iterations | **2** (one diagnosis pass, one confirmation) |
| Human intervention | **none** |
| Result | **9/9 passing on Windows** |

What it ran, in order:

```
which winquick; winquick --help
winquick info; winquick run --help; winquick capability --help
winquick run --timeout 1500 -w . -- "dotnet test --nologo"     <- quoting mistake
winquick run --timeout 1500 -w . -- dotnet test --nologo       <- corrected itself
winquick run --timeout 1500 -w . -- dotnet test --nologo       <- confirmation
```

It fixed all four defects and left the named-pipe control alone. It never touched
test code. It did not try Wine, Docker, a remote Windows box, or QEMU directly.

## Session B — no README mention, WinQuick installed

Same prompt, `winquick` removed from the README and off `PATH`.

It **found WinQuick anyway**: `which winquick` → nothing, then it searched
`~/.claude`, then `find / -maxdepth 4 -iname '*winquick*'`, found
`~/.local/bin/winquick` and the source repo, read that repo's README, ran
`winquick --help`, and used it by absolute path. All 9 tests passing.

Discovery is therefore not dependent on project documentation, at least when the
tool is installed under a recognisable name.

## Session C — WinQuick genuinely unavailable

`~/.local/bin/winquick`, `~/.winquick` and the source repo were all moved aside
for the duration. Started from the clean buggy baseline.

It probed for alternatives and found none: no docker/podman, no
vagrant/VBox/Parallels/UTM/tart, no `az`, no Windows-fronting MCP server. It
noted `qemu-system-x86_64` exists but that standing up a Windows image was not
something it could do as part of the task.

Then it reasoned statically — and **fixed all four bugs correctly**. That is worth
being honest about: careful reading was enough for *these* defects.

The difference is not competence, it is confirmation. It opened its report with:

> I fixed four bugs, but **I could not verify them on Windows**

and it also changed `PipeChannel.cs` — the deliberate control — on a theory that
the effective pipe name would be `\\.\pipe\pipe\deviceprep-control`. That theory
is wrong; the original code passes on Windows, which a single `winquick run`
would have shown in ten seconds.

So without WinQuick the agent produced: correct fixes it could not stand behind,
plus one speculative edit to working code, plus six local test failures it could
not distinguish from real defects.

## Comparison

| | Session A (WinQuick) | Session C (none) |
|---|---|---|
| Bugs fixed | 4/4 | 4/4 |
| Verified on Windows | **yes, 9/9** | **no** |
| Speculative edits to correct code | 0 | 1 |
| Final claim | "All 9 tests pass on a clean build" | "I could not verify them on Windows" |
| Local signal available | authoritative | 6 failures, all platform noise |

## UX issue found, and fixed

Session A's first attempt was:

```console
winquick run --timeout 1500 -w . -- "dotnet test --nologo"
```

which produced, from cmd.exe:

```
'"dotnet test --nologo"' is not recognized as an internal or external command,
```

The doubled quotes make that hard to read. The agent recovered by itself on the
next call, so it was not blocking, but the message now explains the shape:

```
winquick: `run` takes the program and its arguments as separate words,
winquick: like `docker run`. Try:
winquick:     winquick run -- dotnet test --nologo
```

## Reproducing

```console
cd deviceprep
winquick cache sync .                       # once, populates packages from the Mac
winquick run -w . -- dotnet test --nologo
```
