# Troubleshooting

Start with:

```console
winquick doctor --smoke
```

It checks the host, the tools, the runtime, disk space, and optionally runs a
real Windows command. Most problems below are things it will name for you.

## Setup

**"WinQuick needs Microsoft's Windows validation runtime"**

Expected on a fresh install. Microsoft distributes the image under its own
licence, so WinQuick cannot ship it. Either let WinQuick download it
(`winquick setup --accept-microsoft-terms`) or point it at a file you already
have (`winquick setup --from <path>.iso`).

**"does not look like a Validation OS ARM64 image"**

You probably have the AMD64 edition. WinQuick needs the ARM64 one, from
<https://aka.ms/DownloadValidationOS_arm64>.

**Setup was interrupted**

Just run it again. The runtime is built into a staging file and only moved into
place when complete, so an interrupted setup leaves nothing half-installed.
Downloads resume.

**"ntfscp is missing" / "hivexsh is missing"**

Only `setup` needs these. `brew install hivex` covers hivex. The NTFS helpers
ship with WinQuick; if you are running from a source checkout, build them once
with `./scripts/build-ntfs-helpers.sh`.

## Running

**"No Windows runtime is installed yet"**

Run `winquick setup`.

**The first run after setup or a change takes ~12 seconds**

Expected. WinQuick boots Windows once and freezes it, then restores that frozen
guest for every later run. Anything that changes what the guest depends on — a
new capability, a package-cache update, a WinQuick upgrade — makes it rebuild
once. Subsequent runs are back to ~270 ms.

**`'"dotnet test --nologo"' is not recognized`**

The whole command was passed as one quoted string. Arguments work like
`docker run` — separate words:

```console
winquick run -- dotnet test --nologo
```

WinQuick prints this hint when it sees the mistake.

**"the guest reported a result for a different run"**

WinQuick detected that the guest returned a stale result and refused to report
it. It rebuilds and retries automatically. If it repeats, `winquick reset`.

**A run hangs**

Every run has a timeout (default 300 s, `--timeout`). Ctrl-C is safe: WinQuick
kills the VM and cleans up, exiting 130. It never leaves a VM running.

**Windows can't reach the internet**

By design — no network device is attached. That is what makes runs reproducible
and safe. For .NET packages use `winquick cache sync`.

## PowerShell and .NET

**"'pwsh' is not recognized"**

```console
winquick capability install powershell
```

**"'dotnet' is not recognized"**

```console
winquick capability install dotnet-sdk        # to build and test
winquick capability install dotnet-runtime    # only to run built apps
```

Install one or the other, not both — they provide the same `dotnet` command and
whichever is found first wins.

**A program exits `0xC0000135` (`-1073741515`) and prints nothing**

`STATUS_DLL_NOT_FOUND`. For a .NET Framework program — including `nuget.exe`
and anything else built for net4xx — this means the guest has no .NET Framework
runtime, which the stock image does not:

```console
winquick capability install dotnet-framework
```

That services a second image, which `winquick run` then boots; `winquick
doctor` says which of the two it is. The same runtime brings the classic
`MSBuild.exe` toolchain, which is what restores a `packages.config` project and
markup-compiles a classic WPF one. See [dotnet.md](dotnet.md).

**`error MSB4019: The imported project "…\Microsoft.CSharp.targets" was not
found`, or MC1000 on a classic WPF project**

The SDK's MSBuild cannot drive a classic non-SDK project all the way. Use the
Framework one, which the `dotnet-framework` capability provides:

```console
winquick run -w . -- cmd /c "C:\Windows\Microsoft.NET\Framework64\v4.0.30319\MSBuild.exe App.sln /p:Configuration=Release"
```

**`error NU1301: No such host is known (api.nuget.org)`**

The packages are not cached and Windows has no network. Restore them on your Mac:

```console
cd MyProject
winquick cache sync
```

WinQuick prints this hint when it sees NU1301. The next run rebuilds the prepared
guest once, then it is fast again.

**`dotnet test` takes ~10 seconds**

Most of that is .NET: `dotnet restore` alone costs about 6 seconds inside the
guest even reading from a local cache. WinQuick's own overhead is ~145 ms.

## Workspace and artifacts

**Windows didn't see my changes**

The workspace is copied in at the start of each run. Save your files first.

**My source files didn't change after the run**

Correct and deliberate — the guest gets a copy. Use `--artifact` to bring
specific files back.

**`--artifact` found nothing**

Patterns are relative to the workspace root and are matched inside Windows:

| Pattern | Meaning |
|---|---|
| `bin/Release/**` | that directory, recursively, hierarchy preserved |
| `**/*.dll` | every `.dll` anywhere under the workspace |
| `bin/**/*.exe` | every `.exe` anywhere under `bin` |
| `*.log`, `logs/*.txt` | wildcard within one directory |
| `foo?.txt` | `?` matches exactly one character |
| `logs/build.log` | one named file or directory |

A single `*` matches one directory level, as it does in any other glob; use `**`
to recurse. Slashes may lean either way. Patterns stay inside the workspace: one
containing `..`, or an absolute path, is refused before the run starts.

If a pattern matched nothing the run says so on stdout (`winquick: no match for
...`) without failing the command. Check where your build actually writes —
`dotnet build` in a solution puts output under `<project>/bin`, not a top-level
`bin`.

**"already exists and is not empty"**

WinQuick will not write into a non-empty artifacts directory without being told.
Pass `--artifact-overwrite`, or `--artifacts-dir <somewhere-else>`.

## Desktop

**What options does a verb take?**

```console
winquick desktop click --help
winquick desktop toggle --help
```

Both answer without a session running. Most desktop verbs are forwarded to the
guest bridge, so the bridge's own error message for a bad option lists the
options of every verb at once; `--help` is handled on the Mac and describes the
one verb you asked about.

**`no element matches ...`**

Read the tree and use what is actually there:

```console
winquick desktop tree --title "My App"
```

WPF derives an AutomationId from `x:Name`; WinForms needs `Control.Name` set.
If a selector matches several elements WinQuick says so and lists the
candidates rather than picking one, so add `--control-type` or `--title` to
narrow it.

**Clicking at coordinates**

`click` addresses an element. Raw coordinates are `winquick desktop mouse --x
<n> --y <n>`, which clamps to the screen and reports where it actually went.

## Disk and cleanup

**Running out of space**

```console
winquick clean --dry-run     # see what is there
winquick clean               # prepared guest, downloads, temporary files
winquick clean --all         # also the runtime, capabilities and package cache
```

Neither form touches your projects or extracted artifacts.

**Is anything left running?**

```console
pgrep -fl qemu-system-aarch64
```

Should be empty when no run is in progress. If not, it is a bug — please report
it with what you were doing.

## Concurrency

Multiple `winquick run` invocations at once are supported; each gets its own
isolated environment. Operations that change shared state — setup, capability
changes, cache sync, clean — take a lock and will say if they are waiting.

## Environment variables

WinQuick is configured by its flags; there is no configuration file and nothing
to set up before first use. These few variables exist for cases the flags do not
cover, and none of them is needed in normal use.

| Variable | Effect |
|---|---|
| `WINQUICK_KEEP` | Keep the run directory instead of deleting it, and print where it is. For looking at `serial.log` after a guest misbehaves. |
| `WINQUICK_NTFSCP`, `WINQUICK_NTFSCAT` | Use these `ntfscp`/`ntfscat` binaries instead of the ones shipped with WinQuick. |
| `WINQUICK_HIVEXSH` | Use this `hivexsh` instead of the one found on `PATH`. |
| `HOME`, `USERPROFILE` | Where `~/.winquick` goes. `HOME` wins where both are set. It must be an absolute path; WinQuick refuses an empty or relative one rather than resolving its data directory against wherever it happens to be run from. |

`WINQUICK_NTFSCP` and `WINQUICK_NTFSCAT` are worth knowing about if you build
the helpers yourself, because WinQuick will not take them from `PATH`. It writes
into a partition inside a whole-disk image by setting `NTFS_IMAGE_OFFSET`, which
only this project's patch honours — a distribution's `ntfscp` ignores it and
writes at offset zero instead, over the partition table, reporting success. The
two cannot be told apart by asking, so only the bundled copy is trusted and
these variables are the way to override that. `hivexsh` is different: upstream's
is exactly what WinQuick wants, so `PATH` is fine there.

## A filename the workspace cannot carry

Workspace filenames may use any character in the basic multilingual plane.
Characters above U+FFFF -- emoji, and the rarer CJK extensions -- cannot be
represented on the FAT volume that carries the workspace into the guest.

WinQuick checks the whole tree before the run starts and names every offending
path, rather than copying half of it and failing partway through. Rename or
exclude those files.

## Still stuck

`winquick --verbose run -- ...` shows what WinQuick is doing: which path it took,
phase timings, and why it rebuilt anything. Include that when reporting a bug.

## `%` in a command does not mean what it does at a Windows prompt

A command is delivered to the guest as a batch file, so `%` follows batch rules
rather than interactive-prompt rules. `%PATH%` expands as expected, but a `for`
loop variable needs doubling:

```console
$ winquick run -- cmd /c 'for /L %%i in (1,1,3) do @echo %%i'
```

Written with a single `%`, cmd reports `i was unexpected at this time.` — the
same thing it would say inside any `.cmd` file. Nothing else about your quoting
needs adjusting: quotes, spaces, `&`, `|` and Unicode all reach cmd exactly as
you typed them.

## Quoting

WinQuick chooses how to quote based on what you are running.

`cmd` is a shell, so what follows it is passed through as you wrote it:

```console
$ winquick run -- cmd /c 'echo say "hi"'
say "hi"
$ winquick run -- cmd /c 'type "C:\Program Files\app\readme.txt"'
```

Anything else is a program, so each argument is quoted so its own runtime splits
the command line back into exactly the arguments you gave:

```console
$ winquick run -- pwsh -NoProfile -Command 'Write-Output "quoted string"'
quoted string
```

In v0.2.0 both cases used the second rule, which corrupted quotes bound for
`cmd`. If you worked around it by avoiding quotes, you no longer need to.
