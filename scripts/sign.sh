#!/bin/bash
# Sign and notarize a staged WinQuick release.
#
#   ./scripts/sign.sh dist/winquick-0.1.0-darwin-arm64 dist/winquick-0.1.0-darwin-arm64.tar.gz
#
# Requires an Apple Developer ID and a notarytool keychain profile:
#
#   WINQUICK_SIGN_IDENTITY="Developer ID Application: Carlboms Data AB (TEAMID)"
#   WINQUICK_NOTARY_PROFILE="winquick-notary"
#
#   xcrun notarytool store-credentials winquick-notary \
#       --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
#
# Without credentials this exits 0 and says what was skipped: the release is
# still usable, users just have to clear the quarantine attribute themselves.
set -euo pipefail

STAGE="${1:?usage: sign.sh <staged-dir> [archive.tar.gz]}"
ARCHIVE="${2:-}"
IDENTITY="${WINQUICK_SIGN_IDENTITY:-}"
PROFILE="${WINQUICK_NOTARY_PROFILE:-}"

if [ -z "$IDENTITY" ]; then
  echo "==> no WINQUICK_SIGN_IDENTITY set; skipping signing and notarization"
  echo "    The release will work, but macOS will quarantine downloaded binaries."
  echo "    Users can clear it with: xattr -d com.apple.quarantine <file>"
  exit 0
fi

echo "==> signing with $IDENTITY"
# Helpers first, then the CLI: signatures are verified leaf-first.
for f in "$STAGE"/libexec/winquick/ntfscp "$STAGE"/libexec/winquick/ntfscat "$STAGE"/bin/winquick; do
  codesign --force --timestamp --options runtime --sign "$IDENTITY" "$f"
  codesign --verify --strict --verbose=2 "$f"
done

if [ -z "$PROFILE" ] || [ -z "$ARCHIVE" ]; then
  echo "==> no notary profile or archive given; signed but not notarized"
  exit 0
fi

echo "==> notarizing $ARCHIVE"
xcrun notarytool submit "$ARCHIVE" --keychain-profile "$PROFILE" --wait
# A .tar.gz cannot be stapled; the notarization ticket is looked up online.
# Verify what Gatekeeper will conclude:
spctl --assess --type execute --verbose=4 "$STAGE/bin/winquick" || true
echo "==> done"
