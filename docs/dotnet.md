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

**Build and run are different questions.** The `winquick run` guest is
Microsoft's stock Validation OS, which carries **no .NET Framework runtime** —
a .NET Framework executable builds correctly and then dies on launch with
`0xC0000135` (`STATUS_DLL_NOT_FOUND`, the CLR shim) if you try to run it there.
That is a property of that image, not of the build.

**The desktop image does have one.** `.NET Framework` is on Microsoft's own
Validation OS media as `Microsoft-WinVOS-NetFx45-Package.cab`, in `cabs/Common`
beside the graphics and WPF packages, and `winquick capability install desktop`
applies it with everything else. A .NET Framework 4.7.2 WPF application built
by WinQuick launches in a desktop session, renders, and answers UI Automation —
measured, on a real 3,200-line application, with a screenshot to match. The
same package would give `winquick run` a Framework runtime too; nothing does
that yet.

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

Building an architecture and *running* it are again separate, but the guest is
more capable here than it looks. Validation OS ARM64 ships the full emulator
set in `C:\Windows\System32` — `xtajit.dll` for x86 and `xtajit64.dll` plus
`xtajit64se.dll` for x64 — and it works: a **self-contained win-x64 WPF
application** published by WinQuick launches in a desktop session, paints, and
answers UI Automation, with `OpcLogger.UI.exe` reading as `native x64 PE32+`
from its own bytes. A build never needs the guest to execute the result, but if
you want to, x64 is not a barrier.

## Desktop frameworks

| | Build | Run + UI Automation in `winquick desktop` |
|---|---|---|
| WinForms, .NET Framework 4.0 (x86) | yes | untested since the Framework runtime arrived |
| WinForms, .NET Framework 4.8 | yes | untested since the Framework runtime arrived |
| WinForms, .NET 10 Windows | yes | **yes**, verified through UI Automation and screenshots |
| WPF, .NET Framework 4.7.2 | yes | **yes**, verified through UI Automation and screenshots |
| WPF, .NET 10 Windows | yes | **yes**, verified through UI Automation and screenshots |
| WPF, .NET 9 Windows, self-contained **win-x64** | yes | **yes**, under the guest's x64 emulation |

Legacy WPF builds without Visual Studio: the XAML build tasks in the modern SDK
handle `net48` given the reference-assemblies package.

## Building for Windows XP-era targets

WinQuick can build an **x86 WinForms application targeting .NET Framework
4.0** — a Windows XP-era target — entirely inside the disposable guest, with no
Visual Studio anywhere on the host.

A classic non-SDK `.csproj` needs the reference assemblies pointed at
explicitly, because it has no `PackageReference` to carry them. It has none to
*restore* either, so the package has to be asked for by name:

```console
winquick cache add Microsoft.NETFramework.ReferenceAssemblies.net40@1.0.3
winquick run -w ./XpPanel -a "bin/**/*.exe" -- dotnet msbuild XpPanel.csproj \
  /p:Configuration=Release /p:Platform=x86 \
  "/p:FrameworkPathOverride=%NUGET_PACKAGES%\microsoft.netframework.referenceassemblies.net40\1.0.3\build\.NETFramework\v4.0"
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

### Two things a classic project cannot do here

Measured against a real 2015-era WPF application (`packages.config`,
`ToolsVersion="15.0"`, net472, `PlatformTarget=x64`, PdfiumViewer with a native
x64 payload), historically built by Visual Studio's MSBuild:

- **`packages.config` cannot be restored inside the guest.** The two programs
  that can do it are `nuget.exe` and .NET Framework `MSBuild.exe`, and both need
  a Framework runtime the `run` image does not have — the repository's own
  bundled `nuget.exe` exits `0xC0000135`. `dotnet msbuild -t:Restore
  -p:RestorePackagesConfig=true` is not an alternative: NuGet detects the file
  (`ProjectStyle=PackagesConfig`, the right `PackagesConfigPath`) and then
  answers "Nothing to do. None of the projects specified contain packages to
  restore", because that restore path is .NET Framework-only. The packages
  themselves come in fine with `winquick cache add`; what is missing is the
  program that lays them out as `packages\<Id>.<Version>\`.

- **XAML in a classic project does not markup-compile.** A classic `.csproj`
  never imports the WPF targets, and injecting them works —
  `/p:CustomAfterMicrosoftCommonTargets=%DOTNET_ROOT%\sdk\<v>\Sdks\Microsoft.NET.Sdk.WindowsDesktop\targets\Microsoft.WinFX.targets`
  gets `MarkupCompilePass1` to run. It then fails with **MC1000, "Could not find
  assembly 'System.Web'"**. `PresentationBuildTasks` running on .NET resolves
  XAML type references only from `@(ReferencePath)`, and MSBuild's
  `ResolveAssemblyReference` puts no *transitive* framework assemblies there for
  a classic project — `System.Web` arrives only as a dependency of the project's
  `System.Web.Extensions` reference. On the historical build machine
  `PresentationBuildTasks` ran on .NET Framework, where the loader falls back to
  the GAC. Everything else in that build works: framework references resolve,
  the `HintPath` package reference resolves, the native x64 payload is staged,
  and the C# compiler runs.

  Both are properties of the toolchain, not of WinQuick. A classic project whose
  XAML pulls in nothing outside its own explicit references is not affected;
  an SDK-style `net472` WPF project is not affected at all.

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
  — or, when the project is not yours to edit, ask for it by name:
  `winquick cache add Microsoft.NETFramework.ReferenceAssemblies.net472@1.0.3`.
  `cache sync` can only fetch what a project declares, and a `.csproj` that
  targets .NET Framework declares no reference assemblies: on Windows they come
  from a developer pack, not from NuGet.
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
