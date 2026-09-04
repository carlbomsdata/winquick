#!/bin/bash
# Build a WinQuick release archive for Apple Silicon.
#
#   ./scripts/release.sh 0.1.0
#
# Produces, under dist/:
#   winquick-<version>-darwin-arm64.tar.gz          the archive
#   winquick-<version>-darwin-arm64.tar.gz.sha256   its checksum
#   ntfs-3g_ntfsprogs-<v>.tgz                       GPL corresponding source
#   SHA256SUMS                                      everything, one file
#
# Signing and notarization need Apple Developer credentials; see sign() below.
set -euo pipefail

# The licence text travels with the binaries it covers, so a release without it
# is not one that may be distributed. It is kept in the repository rather than
# downloaded: fetching it made the build depend on gnu.org answering at exactly
# the wrong moment, which it repeatedly did not. See licenses/README.md.
copy_licence() {
  local name="$1" dest="$2"
  local src="$ROOT/licenses/$name"
  [ -s "$src" ] || { echo "missing licence text $src" >&2; exit 1; }
  cp "$src" "$dest"
}


VERSION="${1:?usage: release.sh <version>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NAME="winquick-${VERSION}-darwin-arm64"
DIST="$ROOT/dist"
STAGE="$DIST/$NAME"

CARGO_VERSION=$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)
if [ "$CARGO_VERSION" != "$VERSION" ]; then
  echo "Cargo.toml says $CARGO_VERSION but you asked for $VERSION" >&2
  exit 1
fi

echo "==> building winquick $VERSION"
cd "$ROOT"
cargo build --release --locked
BIN="$ROOT/target/release/winquick"
file "$BIN" | grep -q 'arm64' || { echo "not an arm64 binary" >&2; exit 1; }

echo "==> building NTFS helpers"
"$ROOT/scripts/build-ntfs-helpers.sh" "$ROOT/vendor/ntfsprogs" >/dev/null

echo "==> staging"
rm -rf "$STAGE"; mkdir -p "$STAGE/bin" "$STAGE/libexec/winquick" "$STAGE/share/doc/winquick"
cp "$BIN" "$STAGE/bin/winquick"
cp "$ROOT/vendor/ntfsprogs/ntfscp" "$ROOT/vendor/ntfsprogs/ntfscat" "$STAGE/libexec/winquick/"
cp "$ROOT/README.md" "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$STAGE/share/doc/winquick/"
cp -R "$ROOT/docs" "$STAGE/share/doc/winquick/docs"

# The desktop capability builds its guest bridge from source, inside Windows, at
# install time. Without these an installed WinQuick can run `winquick capability
# install desktop` right up to the last step and then fail with "cannot find the
# guest bridge sources".
mkdir -p "$STAGE/share/winquick"
cp -R "$ROOT/guest/wqui" "$STAGE/share/winquick/wqui"
rm -rf "$STAGE/share/winquick/wqui/bin" "$STAGE/share/winquick/wqui/obj"

copy_licence GPL-2.0.txt "$STAGE/libexec/winquick/LICENSE.ntfsprogs"

"$ROOT/scripts/sign.sh" "$STAGE" || true

echo "==> checking the staged tree"
for required in bin/winquick libexec/winquick/ntfscp libexec/winquick/ntfscat \
                share/doc/winquick/LICENSE share/doc/winquick/THIRD_PARTY_NOTICES.md \
                libexec/winquick/LICENSE.ntfsprogs \
                share/winquick/wqui/wqui.csproj share/winquick/wqui/Program.cs; do
  [ -e "$STAGE/$required" ] || { echo "missing from the archive: $required" >&2; exit 1; }
done

echo "==> packaging"
mkdir -p "$DIST"
# Deterministic: the same source has to produce the same bytes, or the checksum
# in the Homebrew formula stops meaning anything the moment anyone rebuilds.
# Sorted entries, fixed timestamps, no owner names, and a gzip header without
# the mtime it would otherwise stamp in.
( cd "$DIST" && find "$NAME" -exec touch -h -t 200001010000.00 {} + ) 2>/dev/null || true
( cd "$DIST" && find "$NAME" | LC_ALL=C sort > "$DIST/.filelist" )
# -n: the file list already names every directory, so letting tar recurse into
# them as well would archive each file once per ancestor directory.
( cd "$DIST" && tar -cf "$DIST/.$NAME.tar" --format ustar -n \
    --uid 0 --gid 0 --uname root --gname root -T "$DIST/.filelist" )
gzip -n -9 -c "$DIST/.$NAME.tar" > "$DIST/$NAME.tar.gz"
rm -f "$DIST/.$NAME.tar" "$DIST/.filelist"
shasum -a 256 "$DIST/$NAME.tar.gz" | sed "s|$DIST/||" > "$DIST/$NAME.tar.gz.sha256"

echo "==> GPL corresponding source"
NTFS_VER="2022.10.3"
curl -sSL -o "$DIST/ntfs-3g_ntfsprogs-${NTFS_VER}.tgz" \
  "https://tuxera.com/opensource/ntfs-3g_ntfsprogs-${NTFS_VER}.tgz"

# Named rather than globbed: dist/ accumulates archives from earlier versions,
# and a SHA256SUMS listing those describes a release that is not this one.
( cd "$DIST" && shasum -a 256 "$NAME.tar.gz" "ntfs-3g_ntfsprogs-${NTFS_VER}.tgz" > SHA256SUMS )

echo
echo "==> dist/"
ls -la "$DIST" | tail -n +2 | awk '{printf "    %-52s %s\n", $9, $5}'
echo
echo "Archive checksum:"
cat "$DIST/$NAME.tar.gz.sha256" | sed 's/^/    /'
