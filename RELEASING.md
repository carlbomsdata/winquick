# Releasing

Everything for v0.2.0 is built and committed. What remains needs credentials
this machine does not have.

v0.1.0 was tagged but never published, so v0.2.0 is the first release anyone
will see. It is a different product from v0.1.0 in two ways: it can build,
render and drive Windows GUI applications, and a desktop session starts in about
380 ms rather than the 9.3 seconds booting Windows took.

## Build the artifacts

```console
./scripts/release.sh 0.2.0
```

Produces `dist/`:

| File | What |
|---|---|
| `winquick-0.2.0-darwin-arm64.tar.gz` | the release archive |
| `winquick-0.2.0-darwin-arm64.tar.gz.sha256` | its checksum |
| `ntfs-3g_ntfsprogs-2022.10.3.tgz` | GPL corresponding source, must ship with it |
| `SHA256SUMS` | all of the above |

The archive also carries `share/winquick/wqui/` — the guest bridge sources,
which `winquick capability install desktop` builds inside Windows. `release.sh`
refuses to package without them, and `winquick doctor` reports whether an
installed copy can find them.

The archive packaging is deterministic: sorted entries, fixed timestamps, no
owner names, and a gzip header without an mtime. Two builds from the same
checkout produce the same SHA-256, so rebuilding after filling the checksum
into the formula is safe.

It is **not** reproducible across different checkout directories. The macOS
linker stamps an `LC_UUID` that varies with the build path, so the same source
built at a different path yields a binary differing in 48 bytes. Suppressing it
with `-Wl,-no_uuid` produces a binary dyld refuses to load, so this is left
alone. The practical consequence: publish the `dist/` you hashed, and treat the
checksum as identifying that artifact rather than as something a third party can
independently regenerate.

## Sign and notarize (needs an Apple Developer ID)

```console
export WINQUICK_SIGN_IDENTITY="Developer ID Application: Carlboms Data AB (TEAMID)"
export WINQUICK_NOTARY_PROFILE="winquick-notary"

xcrun notarytool store-credentials winquick-notary \
    --apple-id you@example.com --team-id TEAMID --password <app-specific-password>

./scripts/release.sh 0.2.0        # signs and notarizes automatically when set
```

Without these, `release.sh` still produces a working archive and says clearly
that signing was skipped. Users then need `xattr -d com.apple.quarantine` on the
downloaded binary, which is why Homebrew is the recommended path.

**This is the one step that cannot be completed without credentials.**

## Publish (needs GitHub authentication)

```console
gh auth login

git push origin main
git push origin v0.1.0

gh release create v0.1.0 \
    --title "WinQuick v0.1.0" \
    --notes-file CHANGELOG.md \
    dist/winquick-0.2.0-darwin-arm64.tar.gz \
    dist/winquick-0.2.0-darwin-arm64.tar.gz.sha256 \
    dist/ntfs-3g_ntfsprogs-2022.10.3.tgz \
    dist/SHA256SUMS
```

The ntfsprogs tarball is not optional: we distribute GPL binaries, so the
corresponding source has to be available alongside them. See
[docs/licensing.md](docs/licensing.md).

## Homebrew tap

`packaging/winquick.rb` is ready, with the archive's SHA-256 already filled in.
Publish it to a tap repository named `carlbomsdata/homebrew-tap`:

```console
gh repo create carlbomsdata/homebrew-tap --public --clone
mkdir -p homebrew-tap/Formula
cp packaging/winquick.rb homebrew-tap/Formula/
cd homebrew-tap && git add . && git commit -m "winquick 0.1.0" && git push
```

Then verify end to end on a machine that has never seen WinQuick:

```console
brew install carlbomsdata/tap/winquick
winquick doctor
winquick setup
winquick run -- cmd /c ver
```

## After publishing

Update the download URL in `docs/install.md` if the release path changes, and
confirm `brew audit --strict winquick` is clean before proposing the formula to
homebrew-core.
