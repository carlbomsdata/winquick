# What .NET can WinQuick build?

"WinQuick ships the .NET 10 SDK" answers a different question from "can I build
my project". An SDK version and a project's target framework are separate
things: one modern SDK builds a wide range of targets, given the right
reference assemblies.

Everything below was measured by building inside WinQuick and then reading the
produced assembly's own bytes — PE machine type, CLR header flags, metadata
version and the stamped `TargetFrameworkAttribute` — with
[`tests/peinfo.py`](../tests/peinfo.py). A `.csproj` saying `v4.0` is intent;
the output is evidence.

Measured on Apple Silicon macOS with the `dotnet-sdk` capability, **SDK
10.0.201**, guest runtime **.NET 10.0.5** (ARM64 Validation OS).

## Build matrix

| Target | Build | Run in the standard guest | Stamped target framework | Machine |
|---|---|---|---|---|
| .NET Framework 2.0 | yes | no | *(none — predates the attribute)* | x86 |
| .NET Framework 3.5 | yes | no | *(none — predates the attribute)* | x86 |
| .NET Framework 4.0 | yes | no | `.NETFramework,Version=v4.0` | x86 |
| .NET Framework 4.5 | yes | no | `.NETFramework,Version=v4.5` | x86 |
| .NET Framework 4.8 | yes | no | `.NETFramework,Version=v4.8` | x86 |
| .NET Framework 4.8.1 | yes | no | `.NETFramework,Version=v4.8.1` | x86 |
| netstandard2.0 | yes | library | `.NETStandard,Version=v2.0` | x86 |
| netstandard2.1 | yes | library | `.NETStandard,Version=v2.1` | x86 |
| net6.0 | yes | with roll-forward | `.NETCoreApp,Version=v6.0` | x86 IL |
| net8.0 | yes | with roll-forward | `.NETCoreApp,Version=v8.0` | x86 IL |
| net9.0 | yes | with roll-forward | `.NETCoreApp,Version=v9.0` | x86 IL |
| net10.0 | yes | **yes** | `.NETCoreApp,Version=v10.0` | x86 IL |

**Build and run are different questions.** The guest is Microsoft's Validation
OS, which carries **no .NET Framework runtime at all** — a .NET Framework
executable builds correctly and then exits with code 53 if you try to run it
there. That is a property of the guest, not of the build.

For modern targets the guest has only the .NET 10 runtime, so a net8.0
executable fails with exit code 150. It runs if you ask the host to roll
forward, which is verified:

```console
winquick run -w . -- dotnet --roll-forward LatestMajor app.dll
```

Note also that a modern `app.exe` is a **native apphost** matching the guest
architecture (ARM64 here); the IL lives in `app.dll`. Inspect the `.dll` when
you want the managed metadata.

## Output architectures

`<PlatformTarget>` is honoured, and the result is visible in the binary:

| PlatformTarget | PE machine | CLR flags | Reads as |
|---|---|---|---|
| AnyCPU | x86 | — | AnyCPU |
| x86 | x86 | 32BITREQUIRED | x86, 32-bit required |
| x64 | x64 | — | x64 |
| ARM64 | arm64 | — | arm64 |

Building an architecture and *running* it are again separate: the ARM64
Validation OS guest runs ARM64 and (through emulation) x86, but a build never
needs the guest to be able to execute the result.

## Desktop frameworks

| | Build | Run + UI Automation in `winquick desktop` |
|---|---|---|
| WinForms, .NET Framework 4.0 (x86) | yes | no — no Framework runtime in the guest |
| WinForms, .NET Framework 4.8 | yes | no — same reason |
| WinForms, .NET 10 Windows | yes | **yes**, verified through UI Automation and screenshots |
| WPF, .NET Framework 4.8 | yes | no — same reason |
| WPF, .NET 10 Windows | yes | **yes**, verified through UI Automation and screenshots |

Legacy WPF builds without Visual Studio: the XAML build tasks in the modern SDK
handle `net48` given the reference-assemblies package.

## Building for Windows XP-era targets

WinQuick can build an **x86 WinForms application targeting .NET Framework
4.0** — a Windows XP-era target — entirely inside the disposable guest, with no
Visual Studio anywhere on the host.

A classic non-SDK `.csproj` needs the reference assemblies pointed at
explicitly, because it has no `PackageReference` to carry them:

```console
winquick cache sync ./XpPanel
winquick run -w ./XpPanel -a "bin/**/*.exe" -- dotnet msbuild XpPanel.csproj \
  /p:Configuration=Release /p:Platform=x86 \
  "/p:FrameworkPathOverride=H:\packages\microsoft.netframework.referenceassemblies.net40\1.0.3\build\.NETFramework\v4.0"
```

The produced binary, read back from its own bytes:

```
machine            x86   (PE32, x86 (32-bit required))
targetFramework    .NETFramework,Version=v4.0
metadataVersion    v4.0.30319
subsystem          windows-gui 4.0 (min OS 4.0)
flags              ILONLY=True 32BITREQ=True 32BITPREF=False
references         System.Windows.Forms, mscorlib
```

**What this does and does not prove.** It proves WinQuick can produce a real
x86 managed executable whose metadata targets .NET Framework 4.0, with an
XP-era subsystem version. It does **not** prove that any given application runs
on Windows XP. WinQuick's standard guest is a modern Windows validation
environment; it has never executed this binary, and neither has any copy of
Windows XP. Targeting .NET Framework 4.0 is necessary for XP compatibility, not
sufficient — the application must also confine itself to APIs that existed
there. WinQuick has not been tested on Windows XP.

## Classic non-SDK projects

`dotnet build` and `dotnet msbuild` both drive a classic
`<Project ToolsVersion="4.0" xmlns="...">` file, with two caveats:

- Without reference assemblies the build stops at **MSB3644** ("the reference
  assemblies for .NETFramework,Version=v4.0 were not found"). Supply them with
  `FrameworkPathOverride`, as above.
- `dotnet build` adds a restore step a classic project does not need;
  `dotnet msbuild` is the more direct route.

No Visual Studio, no Build Tools and no developer pack are installed in the
guest. The reference assemblies come from Microsoft's
`Microsoft.NETFramework.ReferenceAssemblies.*` NuGet packages, restored on your
Mac and carried in offline — WinQuick redistributes none of it.

## Offline reference and targeting packs

The guest has no network, so everything a build needs must already be in the
package cache:

```console
winquick cache sync ./MyProject
```

That restores on the Mac, where there *is* a network, and rebuilds the volume
Windows sees. Two things are worth knowing:

- **Add the reference assemblies package** to an SDK-style project targeting
  .NET Framework:
  `<PackageReference Include="Microsoft.NETFramework.ReferenceAssemblies" Version="1.0.3" PrivateAssets="all" />`
- **Modern targets below the guest's runtime need their packs too** —
  `Microsoft.NETCore.App.Ref`, `Microsoft.WindowsDesktop.App.Ref` and
  `Microsoft.NETCore.App.Host.win-<arch>` for that version. `cache sync` on the
  project pulls them.

A project with a `NuGet.config` that clears its package sources gives much
better errors offline: restore reports `NU1100 Unable to resolve <package>`
naming exactly what is missing, instead of `NU1301` complaining that
api.nuget.org is unreachable.

```xml
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources><clear /></packageSources>
</configuration>
```

## Fixtures

The projects behind this table live in
[`experiments/dotnet-matrix`](../experiments/dotnet-matrix/), including the
classic non-SDK `XpPanel` used for the .NET Framework 4.0 proof. They are
sources only; nothing built is committed.
