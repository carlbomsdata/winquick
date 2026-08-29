# .NET build matrix fixtures

The projects behind [docs/dotnet.md](../../docs/dotnet.md). Each is the smallest
thing that exercises one target, so a failure points at the target rather than at
the code.

- `console-net*` — one console app per target framework
- `lib-netstandard*` — netstandard libraries
- `arch-*` — the same net48 console at each `PlatformTarget`
- `winforms-net48`, `wpf-net48` — legacy desktop frameworks
- `XpPanel` — a **classic non-SDK** WinForms project, .NET Framework 4.0, x86:
  the Windows XP-era build proof
- `ClassicNetFxX64` — a **classic non-SDK** project carrying everything that
  needed the `dotnet-framework` capability at once: `packages.config`, XAML to
  markup-compile, `PlatformTarget=x64` on an ARM64 guest, and `System.Drawing`
  at runtime. `tests/integration.sh` builds and runs it when the capability and
  the net472 reference assemblies are present.

Each carries a `NuGet.config` that clears package sources, so an offline restore
reports the package it actually wants instead of complaining about the network.

Build one with:

```console
winquick cache sync ./console-net48
winquick run -w ./console-net48 -a "bin/**/*" -- dotnet build app.csproj -c Release
```

`XpPanel` needs its reference assemblies pointed at explicitly — see
[docs/dotnet.md](../../docs/dotnet.md).

Sources only. Nothing built is committed.
