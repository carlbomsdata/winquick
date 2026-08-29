# Changelog

## Unreleased

### Fixed

- **Windows x86_64: a prepared guest now restores with more than one
  processor.** It used to resume and then stop, and the reason was two separate
  pieces of per-processor state that the Windows Hypervisor Platform owns and
  QEMU does not carry across a migration, one hidden behind the other, and
  neither a register in `whpx_register_names`:

  - `InternalActivityState`, which left every application processor parked in
    `StartupSuspend` waiting for a startup message the guest had already sent
    in another process;
  - the **Hyper-V hypercall page**, an overlay the hypervisor projects over
    guest memory rather than guest RAM. The migration stream carries the filler
    underneath it, so the first enlightened remote TLB flush jumped into filler
    and bugchecked `0xD1`. Only multiprocessor guests reached it, because
    `nt!HvlFlushRangeListTb` is the *remote* flush and one processor has nobody
    to flush.

  Two QEMU patches, in [`patches/`](patches/), with the evidence in
  [docs/whpx-resume.md](docs/whpx-resume.md) -- including the guest's own crash
  dump, read with WinDbg against Microsoft's public symbols.

  Measured on ROAD-WARRIOR01, `winquick run --cpus 2 -- cmd /c ver` twenty
  times from one prepared guest: **20 warm runs of 20**, p50 24.8 s, restore
  92-180 ms, the guest answering in about 520 ms. The prepared state and the
  canonical image were byte-identical afterwards and no QEMU was left behind.
  The roundtrip is dominated by copying the workspace and artifact volumes per
  run, which Windows has no APFS-style clone for.

  One failure mode is still open: some restored guests resume, run for about
  two seconds and then halt for good. Migrating the synthetic interrupt
  controller looked like the answer, was implemented, and made warm runs
  eighty-six times slower without fixing the halting, so it is not shipped. The
  measurement and the likely reason are in the same document.
- **One bad freeze no longer disables the fast path for good.** Where a
  prepared guest gets frozen is partly luck: the agent's poll loop mounts the
  mailbox, looks and dismounts again without ever going quiet, and a guest
  caught in the wrong part of it comes back unable to poll. That is evidence
  about the state, not about the machine, so WinQuick builds another one -- up
  to three -- before writing `restore-unsupported`. The note is also keyed on
  the QEMU binary's identity now, not just its version string, so a QEMU
  rebuilt with a restore fix stops the note applying instead of leaving the
  fast path switched off on a host where it works.
- **A QEMU that has restored a prepared guest is no longer told it cannot.**
  A hundred-run soak found one prepared guest serving twenty-five warm runs and
  then failing; WinQuick rebuilt, three in a row were unlucky, and it wrote
  `restore-unsupported` — after which every remaining run cold-booted on a
  machine that had just proved twenty-five times that it restores. The note is a
  claim about the QEMU, and one that has restored a guest refutes it, so the
  demonstration is recorded and outranks any later run of silent guests.

  The integration suite had been doing the same thing to itself on macOS: its
  own deliberate corrupt-prepared-state tests produced enough silent guests to
  write the note, and every later run in that suite — and the next one — booted
  cold on a Mac where the warm path works perfectly.
- **`winquick clean` forgets the `restore-unsupported` note.** It is the
  "forget what you worked out about this machine" command, and installing a
  QEMU that can restore is exactly the kind of change a user runs it after.
- **A prepared guest was frozen half a step too early.** WinQuick stopped the
  guest the instant `WQREADY.TXT` appeared, which is the moment its directory
  entry reaches the image — the middle of the agent's work, not the end of it:
  the agent writes the flag and then dismounts the mailbox volume. The prepared
  state therefore captured a guest with mailbox I/O in flight, and restoring it
  into a fresh process left that operation permanently incomplete. The agent
  never reached its poll loop and never saw the next command.

  On Windows this made warm runs fail **8 times out of 8**; letting the guest
  settle before freezing makes them succeed **20 out of 20**, restoring in about
  100 ms. macOS was masking the same race by timing. See
  [docs/mailbox-freeze.md](docs/mailbox-freeze.md).
- **A QEMU that fails to start now says why.** Its stderr was captured and
  discarded, so `winquick run` reported an exit code and nothing else. The
  message it wanted to print — a missing accelerator, an unreadable file, an
  option this build does not support — is now included.
- **`--verbose` prints the QEMU command line**, quoted so it can be pasted back
  into a shell. A VM that will not boot is usually diagnosed by reading the
  arguments it was given.
- **The QEMU monitor no longer waits forever.** A QEMU that stopped answering
  hung WinQuick with it, and because building a prepared guest holds a lock,
  every other run on the machine then failed too.
- **`winquick cache sync` could report "already up to date" while the guest saw
  none of the new packages.** Freshness was judged by what that particular
  restore added, so packages arriving any other way — an earlier sync whose
  rebuild failed, or a `dotnet restore --packages` run by hand — never reached
  the volume. It now compares the cache against what the volume was built from.
  Packages are also counted per *version* rather than per id, so a second
  version of a package already present is noticed.
- **A build big enough to be worth caching never got a warm run.** Measured on
  a three-project solution, `dotnet build App.sln` took **122 s** and
  discarded five prepared guests, every single time, while `dotnet build` of
  one project in the same workspace took 8 s warm. The go flag disappearing is
  a FAT directory write, and the agent starts the workload the instant it has
  the token — so the acknowledgement and the workload race on the same volume,
  and a solution build wins. The host read "busy" as "halted". It now asks
  QEMU's own byte counters when the deadline passes: a halted guest has moved
  nothing, a building one had moved 210 MiB. **122 s to 11 s**, and the
  prepared guest survives. A monitor that will not answer still falls back in
  ten seconds. Evidence in [docs/research.md](docs/research.md).
- **A guest that was alive but idle was still mistaken for a halted one.** The
  byte-counter check above asks "is this guest working hard?", and plenty of
  healthy commands are not: `winquick run --timeout 2 -- cmd /c "ping -n 30
  127.0.0.1"` moves almost nothing while holding the go flag in the guest's
  cache for thirty seconds, and was read as halted — a discarded prepared
  guest, five rebuilds and **117 s** for a two-second timeout. When the total
  is not enough to decide, WinQuick now asks the smaller question the total
  cannot: not how much the guest has moved, but whether it is still moving, by
  comparing two readings a second and a half apart. A halted guest's counters
  stop dead and cannot pass. **117 s to 14 s**, and the prepared guest is kept;
  the heavy case still decides on the total and pays nothing extra (an
  unchanged 11 s on the three-project solution).
- **A command that hit `--timeout` was run again, up to six times.** The warm
  path asks two questions with the same mailbox wait — "did this guest take the
  command?" and "has the command finished?" — and both reported failure as a
  silent guest. So a command that ran past its timeout was read as a broken
  prepared guest: WinQuick threw the guest away, cold-booted, ran the whole
  command again, and repeated that for every prepare attempt. Measured on a
  `--timeout 90` command that hangs in the guest: **745 s** to give up. Only
  the first question is evidence about the guest — by the time the command is
  running, the guest has demonstrably picked it up — so the second now reports
  what it is, names the limit and says which flag changes it, and the prepared
  guest is kept. **745 s to 101 s** on the same command.
- **`winquick cache sync` could not restore a `net*-windows` project at all.**
  The host is macOS, every project WinQuick exists to build targets Windows,
  and the SDK refused each one with `NETSDK1100: To build a project targeting
  Windows on this operating system, set the EnableWindowsTargeting property to
  true` before resolving a single package. The host restore sets it. Found
  against a real WPF/Worker solution targeting `net9.0-windows`, which could
  not be cached and therefore could not be built offline.
- **`winquick cache sync` wrote into your project.** `dotnet restore` is not
  read-only: it drops `obj/project.assets.json` and two generated MSBuild files
  beside every project file it touches. WinQuick promises your source is never
  written to, and that now holds on the Mac as well as in the guest — the
  restore runs on a throwaway copy, which is deleted whether it succeeds or
  fails. Errors still name your own paths.
- **`--artifact "**/App.dll"` retrieved nothing, silently.** `xcopy` recurses
  with `/S` only for a *wildcard*: given a literal name it answers "File not
  found" with the file one directory down. Naming a file under `**` now walks
  the tree, preserving the directories it was found in.
- **A pattern that matched nothing is now reported.** Only the all-or-nothing
  case was, so four `--artifact` patterns with one mistake among them produced
  a plausible "retrieved 1 file" and a missing artifact nobody noticed.
- **A wildcard in a directory name is refused rather than ignored.**
  `*/bin/Release/*.dll` reads as if it should work; neither `xcopy` nor `for`
  expands it, so it matched nothing and said nothing. It is now an error that
  names `**/*.dll` as the pattern that does what was meant.

### Added

- **The desktop can run .NET Framework applications.** WinQuick's notes said
  Validation OS "carries no .NET Framework runtime at all", which is true of the
  stock image and had been read as meaning it could not have one. It is on
  Microsoft's own media as `Microsoft-WinVOS-NetFx45-Package.cab`, in
  `cabs/Common` beside the graphics and WPF packages WinQuick already applies,
  and DISM takes it without complaint. A real .NET Framework 4.7.2 WPF
  application — 3,200 lines, `System.Management`, an embedded resource — now
  builds in WinQuick, launches in a desktop session, renders, answers UI
  Automation, and runs all thirteen of its own diagnostics to a result. Before
  this it built correctly and died on launch with `0xC0000135`.
  `winquick capability install desktop --force` picks it up; the image grows
  from 1.8 to 2.5 GiB.
- **`winquick capability install dotnet-framework`** services a .NET Framework
  into the image `winquick run` boots, written to a second image so the
  pristine one stays byte-identical. It brings the classic build toolchain with
  it — an `MSBuild.exe`, `Microsoft.Common.targets`, `Microsoft.CSharp.targets`,
  `Microsoft.WinFX.targets` and `PresentationBuildTasks.dll` — which is the only
  thing that can restore a `packages.config` project or markup-compile a classic
  WPF one. A 2015-era WPF application with `packages.config`,
  `ToolsVersion="15.0"`, net472, `PlatformTarget=x64` and a native x64 Pdfium,
  historically built by Visual Studio, now restores with its own bundled
  `nuget.exe` and builds to an x64 `.exe` inside WinQuick. `capability remove
  dotnet-framework` puts `run` back on the plain image.
- **`winquick desktop pull <guest-path> <file>`** brings a file the application
  produced back to this Mac — the converted page, the exported report, the log
  it wrote. A session could already show you a picture of what happened but not
  give you the thing that happened. The bytes come back the way a screenshot
  does, and the guest's hash is checked against the file that arrives.
- **`winquick cache add <Name>[@<Version>]`** puts named packages in the offline
  cache without touching any project. `cache sync` can only fetch what a project
  declares, and a `.csproj` targeting .NET Framework declares no reference
  assemblies — on Windows they come from a developer pack, not from NuGet — so
  a project you are not allowed to edit could not be built offline at all.
  A pinned version is fetched with `PackageDownload`, which skips the
  target-framework compatibility check that rejects build-only and native
  packages.
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
  prepared-state fingerprint. See [docs/windows-host.md](docs/windows-host.md).
- **Windows x86_64: `winquick setup` and `winquick run` work.** Both through
  `winquick.exe`, on a real x64 Validation OS guest accelerated by the Windows
  Hypervisor Platform, using the same agent and mailbox protocol macOS uses.
  Setup takes 57 s and a run 16.5 s, repeatably, with no orphaned processes.
  Nothing needs elevation, no disk image is ever mounted, and no exception is
  asked of endpoint security software.

  Every Windows run is a **cold boot**, because a prepared guest restored under
  WHPX resumes and then never executes — it reads no command and writes no
  output. WinQuick records that once, keyed on the QEMU and accelerator, and
  later runs skip the warm path instead of rebuilding a prepared guest, waiting
  and giving up on each one.
- **Image preparation no longer mounts anything, on either host.** The
  `ntfsprogs` helpers now take a byte offset into a whole-disk image
  (`NTFS_IMAGE_OFFSET`) instead of a partition device node, so `hdiutil attach`
  and its detach and stale-attachment handling are gone from macOS, and Windows
  never needed `Mount-DiskImage` or the VHD driver at all. One code path, no
  privileges, nothing touched outside the image file. Native Windows builds of
  the same two helpers, plus `hivexsh`, are built from the same upstream
  tarballs by [`scripts/`](scripts/) — see
  [`patches/`](patches/) and THIRD_PARTY_NOTICES.md.
- **Microsoft's ISO is read rather than mounted.** The media is a UDF bridge
  disc, and mounting one needs `hdiutil` or `Mount-DiskImage`. `src/udf.rs` is
  the smallest reader that takes `ValidationOS.vhdx` off it: 1 GB in 0.48 s,
  identical on both hosts, no privileges, and no mount left behind.
- **Two WHPX NMI bugs found and fixed** in
  [`patches/whpx-nmi-delivery.patch`](patches/whpx-nmi-delivery.patch), not
  applied to anything WinQuick ships: `whpx_apic_external_nmi()` is an empty
  function, and a prepared interruption is only committed for one of the two
  APIC modes. Between them, `inject-nmi` did nothing at all on a WHPX guest.
- **A restored application processor now starts.** WHP parks every application
  processor in `StartupSuspend` on a fresh partition, waiting for the INIT/SIPI
  that a cold boot supplies. QEMU carries no per-processor activity state
  across a migration, so after a restore that processor waited for ever and
  never executed a single instruction.
  [`patches/whpx-activity-state-migration.patch`](patches/whpx-activity-state-migration.patch)
  adds one vmstate section per processor; applying it after the architectural
  state is pushed, rather than during load, is what makes it reliable.

  Multiprocessor restore is still not usable: with the processor running, the
  guest bugchecks `DRIVER_IRQL_NOT_LESS_OR_EQUAL` and writes a crash dump
  instead of running the agent. It is a crash, not the hang the earlier notes
  described. See [docs/whpx-resume.md](docs/whpx-resume.md).
- **The multiprocessor restore failure is narrowed to a specific missing
  register.** A restored application processor is left in WHP's
  `StartupSuspend` -- waiting for a SIPI that was already sent, in another
  process, minutes ago -- because QEMU never saves or restores
  `WHvRegisterInternalActivityState`. Everything else WHP exposes, including
  all 993 bytes of its own LAPIC state, is byte-identical across the
  migration. Clearing the bit is necessary but not sufficient. See
  [docs/whpx-resume.md](docs/whpx-resume.md).
- **The unit suite runs natively on Windows**: 125 tests, all passing. Fixing
  that turned up a real bug — giving a copied disk a fresh GPT identity read
  `/dev/urandom`, so the servicing path could not have worked there at all.
- **The workspace, artifacts and `winquick mcp` work on Windows.** Verified
  there: 14 behaviour checks (doctor, stream separation, exit codes, workspace,
  artifact retrieval, disposability, containment) and the 72-check MCP protocol
  suite, all passing. `tests/mcp.py` needed two fixes to run on either host —
  it looked for a runtime under `validation-arm64` only, and built temporary
  workspaces with MSYS2 paths a native binary cannot resolve.
- **Windows process containment.** Every QEMU is assigned to a Job Object whose
  kill-on-close limit is set, so a WinQuick that is killed outright — not merely
  interrupted — cannot strand a running VM. Ctrl-C is handled through
  `SetConsoleCtrlHandler`, and `src/proc.rs` carries the three process
  operations both hosts need.
- **[docs/dotnet.md](docs/dotnet.md) — an empirical .NET build matrix.** Which
  target frameworks WinQuick can build, which guest can also run them,
  and what the produced binaries actually are. .NET Framework 2.0 through
  4.8.1, netstandard 2.0/2.1 and net6.0 through net10.0 all build; the *stock*
  image carries no .NET Framework runtime, so those build there and do not run
  — `capability install dotnet-framework` is what changes that, and the matrix
  says for each target whether running it has actually been measured.
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
- **`experiments/dotnet-matrix/ClassicNetFxX64`** — one fixture standing in for
  the shape of project that found most of this: non-SDK `.csproj`,
  `packages.config`, XAML that has to be markup-compiled, `PlatformTarget=x64`
  on an ARM64 guest, and `System.Drawing` at runtime. Each of those was a
  separate failure discovered by a build, and none of them had a regression
  test that did not require somebody's private repository. `tests/integration.sh`
  builds it and runs the result when the `dotnet-framework` capability and the
  net472 reference assemblies are present, and skips otherwise.

### Changed

- Public URLs and the Homebrew command use the canonical `carlbomsdata`
  namespace rather than depending on redirects from the old organisation name.
  The install command is now `brew install carlbomsdata/tap/winquick`.
- **The documentation no longer says a .NET Framework is impossible here.** The
  old note — "Validation OS carries no .NET Framework runtime" — was true of the
  stock image and had been read across the docs as a property of the product.
  Every place that said or implied it now distinguishes the stock image, the
  serviced image, and the three separate things people mean by ".NET
  Framework": *reference assemblies* (NuGet, restored on the Mac, needed to
  compile), the *runtime* (Microsoft's media, applied by
  `capability install dotnet-framework`, needed to launch), and the *classic
  toolchain* (`MSBuild.exe` and `PresentationBuildTasks.dll`, part of the same
  package, needed for `packages.config` and classic WPF). The build matrix in
  [docs/dotnet.md](docs/dotnet.md) gained a "run with `dotnet-framework`"
  column, and it says **not measured** for every target where that is the
  truth: net472 is what has actually been run, in AnyCPU and x64.
- **`winquick info` reports the .NET Framework capability and the image a run
  will boot**, in the JSON an agent reads as well as on the terminal. `doctor`
  already said so; over MCP there was no way to find out except by running a
  program and getting `0xC0000135`.

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
