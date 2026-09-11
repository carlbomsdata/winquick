# Installing WinQuick

## Requirements

Common to every host: about 4 GB of disk for the Windows runtime (more for
optional tools), and Microsoft's Validation OS image, which you obtain from
Microsoft.

### Apple Silicon macOS

- An Apple Silicon Mac (M1 or newer), macOS 13 (Ventura) or later

### Linux x86_64 or aarch64

- Hardware virtualisation enabled in firmware, and KVM available:
  `/dev/kvm` must exist and be readable and writable by you. If it is not,
  add yourself to the `kvm` group and log in again:
  `sudo usermod -aG kvm $USER`
- **QEMU 11 or newer.** Ubuntu 24.04 ships 8.2.2, which cannot migrate the
  NVMe device the guest boots from; WinQuick would then boot cold on every
  run. `winquick doctor` checks this.
- `libhivex-bin` for `winquick setup` (`sudo apt install libhivex-bin`)
- UEFI firmware: `ovmf` on x86_64, `qemu-efi-aarch64` on arm64

WinQuick does not use libvirt and does not run a daemon.

### Windows x86_64

- Hardware virtualisation, and the **Windows Hypervisor Platform** feature,
  which is not the same thing as installing the Hyper-V role
- QEMU 11 or newer on `PATH`

Windows boots the guest from scratch on every run rather than resuming a
prepared one, so a command costs about 17 seconds instead of a fraction of a
second. That is deliberate and measured: see the platform table in the README.

### Not inside a virtual machine

WinQuick needs real hardware virtualisation, and a nested hypervisor does not
reliably provide it. Measured under Apple's Virtualization.framework, the guest
firmware runs and then neither Windows nor a plain Linux kernel reaches its
first line of output. `winquick doctor` reports when the machine is itself a
guest, and a failed run says so rather than looking like a hang. Nesting works
on some stacks; it is not something to count on.

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
curl -LO https://github.com/carlbomsdata/winquick/releases/download/v0.4.8/winquick-0.4.8-darwin-arm64.tar.gz
curl -LO https://github.com/carlbomsdata/winquick/releases/download/v0.4.8/winquick-0.4.8-darwin-arm64.tar.gz.sha256
shasum -a 256 -c winquick-0.4.8-darwin-arm64.tar.gz.sha256
tar xzf winquick-0.4.8-darwin-arm64.tar.gz
sudo cp -R winquick-0.4.8-darwin-arm64/* /usr/local/
brew install qemu hivex
```

On Linux, take the archive matching `uname -m`:

```console
sudo apt install qemu-system qemu-utils ovmf libhivex-bin
curl -LO https://github.com/carlbomsdata/winquick/releases/download/v0.4.8/winquick-0.4.8-linux-x86_64.tar.gz
tar xzf winquick-0.4.8-linux-x86_64.tar.gz
sudo cp -R winquick-0.4.8-linux-x86_64/* /usr/local/
winquick doctor
```

**Check what QEMU that gave you.** The line above installs whatever your
distribution packages, and on Ubuntu 24.04 that is QEMU 8.2.2 — measured, on a
current image. WinQuick runs on it, but 8.2.2 cannot migrate the NVMe device the
guest boots from, so every run boots cold instead of resuming in a fraction of a
second, and `winquick doctor` reports the version as too old. For the fast path
you need QEMU 11 or newer than your distribution is likely to carry, from
Homebrew on Linux, a backport, or a source build.

Both `linux-x86_64` and `linux-aarch64` are published.

On Windows, install QEMU 11 or newer, then unpack the zip and put the folder on
`PATH`. It is one flat directory: `winquick.exe` with `ntfscp.exe`,
`ntfscat.exe` and `hivexsh.exe` beside it, so there is nothing else to install.

```console
curl.exe -LO https://github.com/carlbomsdata/winquick/releases/download/v0.4.8/winquick-0.4.8-windows-x86_64.zip
tar -xf winquick-0.4.8-windows-x86_64.zip
```

Every archive's SHA-256 is published beside it, and `SHA256SUMS` covers the lot.

WinQuick looks for its helpers next to the binary, in `../libexec/winquick`, or
in a `winquick-helpers` directory beside the binary — any of those layouts work.

Then `winquick doctor` to check, and `winquick setup`.

## Gatekeeper

The release is **not signed and not notarized** — no Apple Developer ID was
available when it was built.

**Use Homebrew.** `brew install carlbomsdata/tap/winquick` fetches the archive
itself, so the quarantine flag is never set and every file runs without a prompt.
This is the tested, clean path on macOS and the one to give other people.

A **browser download of the tarball is the awkward path**, and worse than a
one-line fix suggests. Safari and other browsers stamp the download with
`com.apple.quarantine`, `tar` carries that flag onto **every file it extracts** —
the `winquick` binary *and* the `ntfscp`/`ntfscat` helpers it runs during setup —
and on recent macOS a quarantined unsigned binary does not fail cleanly: it
**hangs on a Gatekeeper prompt**. Clearing the flag from the binary alone is not
enough, because setup then blocks on a quarantined helper. Clear it from the
whole tree before copying it into place:

```console
tar xzf winquick-*-darwin-arm64.tar.gz
xattr -dr com.apple.quarantine winquick-*-darwin-arm64
sudo cp -R winquick-*-darwin-arm64/* /usr/local/
```

Never disable Gatekeeper system-wide. If `winquick` ever seems to hang doing
nothing on a fresh install, a leftover quarantine flag is the first thing to
check (`xattr -r com.apple.quarantine "$(dirname "$(command -v winquick)")"/..`).
A signed, notarized release is the proper fix and is not yet available; until
then, Homebrew is the path that avoids all of this.

### On Windows: unsigned, and antivirus may quarantine it

The Windows binaries are unsigned too, and the consequence there is sharper than
a Gatekeeper prompt. Windows SmartScreen warns on first run ("Windows protected
your PC" -> More info -> Run anyway). More aggressively, an endpoint-protection
product can **quarantine `winquick.exe` outright**: measured on a machine running
Bitdefender Endpoint Security, the binary ran once and was then removed, and a
fresh download was blocked with "access denied" before it reached disk. This is
a property of the security product, not of WinQuick, but the effect on a first
run is real.

If that happens, the fix is an exclusion for the install directory in your AV
console -- not disabling protection. On a machine with only Microsoft Defender,
a first run typically gets the SmartScreen prompt and nothing worse. A signed
release is the proper fix and is not yet available.

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

**`setup --force` does not re-download the 2.4 GB image.** It reuses the copy
already in `~/.winquick/cache/` and rebuilds the runtime from it in about a
minute — it prints *"Using the Validation OS image already downloaded to …"* so
you can see it never touched the network. Only a first install, on a machine
with an empty cache, downloads.

If you have installed the `dotnet-framework` or `desktop` capability, those are
serviced images with their own copy of the agent, so they go stale on the same
upgrade. `winquick doctor` names each one and the exact command to rebuild it
(`winquick capability install dotnet-framework --force`, and the same for
`desktop`). Those rebuild from the runtime you just rebuilt, so they do not
download either.

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
