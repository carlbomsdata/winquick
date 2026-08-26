# Installing WinQuick

## Requirements

- An Apple Silicon Mac (M1 or newer)
- macOS 13 (Ventura) or later
- About 4 GB of disk for the Windows runtime, more for optional tools
- Microsoft's Validation OS image, which you obtain from Microsoft

## Homebrew

```console
brew install carlbomsdata/tap/winquick
winquick setup
```

This installs the CLI, its `ntfscat`/`ntfscp` helpers, the guest bridge sources
under `share/winquick/wqui`, and the documentation, and pulls in QEMU and hivex.

Homebrew downloads and unpacks the archive itself, so nothing is marked with
`com.apple.quarantine` and no Gatekeeper step is needed. Verified on macOS 26.

## Release archive

If you would rather not use Homebrew:

```console
curl -LO https://github.com/carlbomsdata/winquick/releases/download/v0.3.0/winquick-0.3.0-darwin-arm64.tar.gz
curl -LO https://github.com/carlbomsdata/winquick/releases/download/v0.3.0/winquick-0.3.0-darwin-arm64.tar.gz.sha256
shasum -a 256 -c winquick-0.3.0-darwin-arm64.tar.gz.sha256
tar xzf winquick-0.3.0-darwin-arm64.tar.gz
sudo cp -R winquick-0.3.0-darwin-arm64/* /usr/local/
brew install qemu hivex
```

The archive's SHA-256 is

```
ab8914eff97e0c58f78b50a1f6f49e5f3b357ea8849e83fa9fe32f69aaf3e963
```

WinQuick looks for its helpers next to the binary, in `../libexec/winquick`, or
in a `winquick-helpers` directory beside the binary — any of those layouts work.

Then `winquick doctor` to check, and `winquick setup`.

## Gatekeeper

The v0.3.0 release is **not signed and not notarized** — no Apple Developer ID
was available when it was built.

This only matters for a **browser download**. Safari and other browsers mark
downloaded files with `com.apple.quarantine`, and macOS then refuses to run an
unsigned binary. Clear the attribute on the file you installed, and nothing
broader:

```console
xattr -d com.apple.quarantine /usr/local/bin/winquick
```

Never disable Gatekeeper system-wide.

Installing with **Homebrew needs none of this**: brew fetches the archive itself,
so the quarantine attribute is never set.

## Setting up Windows

```console
winquick setup
```

WinQuick needs Microsoft's Windows validation runtime. Microsoft distributes it
under its own licence, so WinQuick cannot ship it. Either:

```console
winquick setup --accept-microsoft-terms      # download it (about 2.4 GB)
winquick setup --from ~/Downloads/vos.iso    # use a file you already have
```

Setup builds the runtime, then boots Windows and runs a real command to prove it
works. About a minute.

Add optional tools at the same time, or later:

```console
winquick setup --accept-microsoft-terms --with powershell dotnet-sdk
winquick capability install dotnet-runtime
```

## Updating

```console
brew upgrade winquick
```

If a new version changes anything the prepared guest depends on, WinQuick
notices and rebuilds it on the next run — there is nothing to do by hand. If a
release changes the guest agent, `winquick doctor` will say the runtime was built
by a different version and to run `winquick setup --force`.

## Uninstalling

```console
winquick clean --all          # remove the Windows runtime and all generated data
brew uninstall winquick
```

`winquick clean` (without `--all`) removes only the prepared guest, downloads and
temporary files, keeping the runtime so you do not have to set up again. Neither
form touches your projects or extracted artifacts.

Everything WinQuick generates lives under `~/.winquick`; removing that directory
is equivalent to `winquick clean --all`.
