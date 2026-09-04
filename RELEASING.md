# Releasing

WinQuick ships one archive per host, built by the script for that host. There is
no cross-building: each archive contains a native `winquick` plus the patched
`ntfsprogs` helpers (and on Windows, `hivexsh`) built on the same machine.

| Host | Script | Produces |
|---|---|---|
| macOS arm64 | `./scripts/release.sh <version>` | `winquick-<v>-darwin-arm64.tar.gz` |
| Linux x86_64 / aarch64 | `./scripts/release-linux.sh <version>` | `winquick-<v>-linux-<arch>.tar.gz` |
| Windows x86_64 | `./scripts/release-windows.sh <version>` | `winquick-<v>-windows-x86_64.zip` |

Each refuses to package an incomplete tree: the guest bridge sources must be
present, the helpers must not link anything outside the system libraries, and
the GPL text must have downloaded. `winquick doctor` reports whether an
installed copy can find its own parts.

## Before building

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
   — on every host, or let CI do it (`.github/workflows/ci.yml`).
2. `./tests/integration.sh` on macOS. It boots real guests, so nothing else may
   touch `~/.winquick` while it runs and the binary must not be rebuilt under it.
3. Bump `version` in `Cargo.toml`, refresh `Cargo.lock`, and move the
   `Unreleased` section of `CHANGELOG.md` under the new version.

## Build requirements per host

The scripts build ntfsprogs (and hivex on Windows) from source, so each host
needs a toolchain:

```console
# Linux
sudo apt install build-essential autoconf automake libtool file \
                 qemu-system qemu-utils ovmf libhivex-bin

# Windows, inside MSYS2
pacman -S --needed base-devel zip mingw-w64-x86_64-toolchain \
                   mingw-w64-x86_64-libxml2 perl autoconf automake libtool
```

macOS needs only the Xcode command line tools, plus `brew install qemu hivex`.

## Determinism

The archives are deterministic: sorted entries, fixed timestamps, no owner
names, and a gzip header without an mtime. Two builds from the same checkout
produce the same SHA-256, so rebuilding after filling a checksum into the
formula is safe.

They are **not** reproducible across different checkout directories. The macOS
linker stamps an `LC_UUID` that varies with the build path, so the same source
built elsewhere yields a binary differing in 48 bytes. Suppressing it with
`-Wl,-no_uuid` produces a binary dyld refuses to load, so this is left alone.
Publish the `dist/` you hashed, and treat the checksum as identifying that
artifact rather than as something a third party can independently regenerate.

## Sign and notarize (needs an Apple Developer ID)

```console
export WINQUICK_SIGN_IDENTITY="Developer ID Application: Carlboms Data AB (TEAMID)"
export WINQUICK_NOTARY_PROFILE="winquick-notary"

xcrun notarytool store-credentials winquick-notary \
    --apple-id you@example.com --team-id TEAMID --password <app-specific-password>

./scripts/release.sh <version>        # signs and notarizes automatically when set
```

Without these, `release.sh` still produces a working archive and says clearly
that signing was skipped. Users then need `xattr -d com.apple.quarantine` on a
downloaded archive, which is why Homebrew is the recommended path on macOS.

**This is the one step that cannot be completed without credentials.** The
Windows archive is unsigned for the same reason: code signing there needs an
Authenticode certificate.

## Publish

```console
git push origin main
git tag -a v<version> -m "WinQuick v<version>"
git push origin v<version>

gh release create v<version> \
    --title "WinQuick v<version>" \
    --notes-file <release notes> \
    dist/winquick-<v>-darwin-arm64.tar.gz \
    dist/winquick-<v>-linux-x86_64.tar.gz \
    dist/winquick-<v>-linux-aarch64.tar.gz \
    dist/winquick-<v>-windows-x86_64.zip \
    dist/*.sha256 \
    dist/ntfs-3g_ntfsprogs-2022.10.3.tgz \
    dist/hivex-1.3.24.tar.gz
```

The ntfsprogs and hivex tarballs are not optional: we distribute GPL and LGPL
binaries, so the corresponding source has to be available alongside them. See
[docs/licensing.md](docs/licensing.md).

## Homebrew tap

Update `packaging/winquick.rb` with the new URL and the macOS archive's SHA-256,
then copy it into the tap at `carlbomsdata/homebrew-tap`:

```console
gh repo clone carlbomsdata/homebrew-tap
cp packaging/winquick.rb homebrew-tap/Formula/
cd homebrew-tap && git commit -am "winquick <version>" && git push
```

Verify on a machine that has never seen WinQuick:

```console
brew install carlbomsdata/tap/winquick
winquick doctor
winquick setup
winquick run -- cmd /c ver
```

## After publishing

Update the archive names in `README.md` and `docs/install.md` if the release
path changes, and confirm `brew audit --strict winquick` is clean before
proposing the formula to homebrew-core.
