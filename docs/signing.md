# Signing and notarizing the macOS release

The macOS binaries are unsigned today, which is fine for Homebrew — `brew`
fetches the archive itself, so macOS never sets the quarantine flag and every
file runs without a prompt. It is **not** fine for a browser download: macOS
quarantines the whole download, and a quarantined unsigned binary hangs on a
Gatekeeper prompt (see `docs/install.md`). Signing and notarizing fixes the
download path, and is the last step before calling the macOS build production.

Everything below is wired and ready. The one thing WinQuick cannot provide for
you is the **certificate** — it requires an Apple Developer Program membership
and Apple's identity verification, which only the account holder can obtain.

## One-time setup (on the Mac that cuts releases)

1. **Enrol** in the Apple Developer Program (about USD 99/year):
   <https://developer.apple.com/programs/>. For a company, enrol as the
   organization "Carlboms Data AB" so the signature reads with that name.

2. **Create a "Developer ID Application" certificate** (not "Apple
   Distribution" — that one is for the App Store, which this is not) at
   <https://developer.apple.com/account/resources/certificates>, download it,
   and double-click to install it in the login keychain. Confirm it is there:

   ```console
   security find-identity -v -p codesigning
   # 1) ABCD…  "Developer ID Application: Carlboms Data AB (TEAMID)"
   ```

3. **Store notarization credentials** once, in the keychain, so the release
   script never sees your password. Use an app-specific password from
   <https://account.apple.com> (Sign-In and Security → App-Specific Passwords):

   ```console
   xcrun notarytool store-credentials winquick-notary \
     --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
   ```

## Cutting a signed release

Shipped Linux and Windows archives come from CI, which has no certificate, so
the **signed macOS archive is built on your Mac**. With the two variables set,
`release.sh` signs the binaries, packages, and notarizes in one run:

```console
export WINQUICK_SIGN_IDENTITY="Developer ID Application: Carlboms Data AB (TEAMID)"
export WINQUICK_NOTARY_PROFILE="winquick-notary"
./scripts/release.sh 0.4.6
```

It prints `signed and notarized` on success. Then, for that release:

- upload `dist/winquick-<version>-darwin-arm64.tar.gz` as the **darwin-arm64**
  asset, replacing the CI-built one;
- put its `.sha256` into `packaging/winquick.rb` (and the tap), the same as any
  release.

Nothing else changes: Linux and Windows keep coming from CI.

## Verifying it worked

```console
# the signature is a real Developer ID, not ad-hoc:
codesign -dv --verbose=4 dist/winquick-*/bin/winquick 2>&1 | grep Authority
# Gatekeeper accepts it for execution:
spctl --assess --type execute --verbose=4 dist/winquick-*/bin/winquick
# and a fresh download no longer needs the quarantine dance:
#   curl -LO <asset-url> && tar xzf … && ./winquick --version   # runs, no prompt
```

Without the variables set, `release.sh` builds an **unsigned** archive and says
so. That release still works — via Homebrew with no friction, or from a browser
download after clearing quarantine (`xattr -dr com.apple.quarantine …`). It just
is not the production, no-prompt experience that signing provides.
