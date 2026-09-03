#!/bin/bash
# Build a WinQuick release archive for Windows x86_64.
#
#   ./scripts/release-windows.sh 0.3.0
#
# Produces, under dist/:
#   winquick-<version>-windows-x86_64.zip          the archive
#   winquick-<version>-windows-x86_64.zip.sha256   its checksum
#   ntfs-3g_ntfsprogs-<v>.tgz                      GPL corresponding source
#   hivex-<v>.tar.gz                               LGPL/GPL corresponding source
#
# Runs on Windows x86_64 under MSYS2, because the two helper build scripts do.
# MSYS2 is a build-time dependency only: nothing in the archive links against it.
#
# A zip rather than a tarball: Explorer opens it without a third-party tool, and
# it is what scoop and winget manifests expect.
#
# QEMU is NOT bundled, for the same reason it is not bundled on Linux -- it is a
# separate GPL-2.0 program WinQuick runs as a child process, and shipping it
# would drag in its whole DLL closure plus a corresponding-source obligation.
# `winquick doctor` says where to get it.
set -euo pipefail

VERSION="${1:?usage: release-windows.sh <version>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NAME="winquick-${VERSION}-windows-x86_64"
DIST="$ROOT/dist"
STAGE="$DIST/$NAME"

CARGO_VERSION=$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)
if [ "$CARGO_VERSION" != "$VERSION" ]; then
  echo "Cargo.toml says $CARGO_VERSION but you asked for $VERSION" >&2
  exit 1
fi
case "$(uname -s)" in
  MINGW*|MSYS*) : ;;
  *) echo "this script packages the Windows build and must run under MSYS2" >&2; exit 1 ;;
esac
command -v zip >/dev/null || { echo "zip is missing. Install it with:  pacman -S zip" >&2; exit 1; }

echo "==> building winquick $VERSION"
cd "$ROOT"
cargo build --release --locked
BIN="$ROOT/target/release/winquick.exe"
[ -f "$BIN" ] || { echo "cargo did not produce $BIN" >&2; exit 1; }
file "$BIN" | grep -q 'PE32+' || { echo "not a 64-bit Windows binary" >&2; exit 1; }

echo "==> building the NTFS helpers"
# The stock ntfsprogs will not do: WinQuick addresses a partition inside a
# whole-disk image through NTFS_IMAGE_OFFSET, which is this project's patch.
"$ROOT/scripts/build-ntfs-helpers.sh" "$ROOT/vendor/ntfsprogs" >/dev/null

echo "==> building hivexsh"
# Upstream excludes hivexsh from its Windows builds, and no package ships one.
"$ROOT/scripts/build-hivex-windows.sh" "$ROOT/vendor/hivex" >/dev/null

echo "==> staging"
# Flat, the way a Windows program is unpacked and put on PATH: winquick.exe finds
# helpers sitting beside it, so there is no prefix layout to preserve.
rm -rf "$STAGE"; mkdir -p "$STAGE/doc"
cp "$BIN" "$STAGE/winquick.exe"
cp "$ROOT/vendor/ntfsprogs/ntfscp.exe" "$ROOT/vendor/ntfsprogs/ntfscat.exe" "$STAGE/"
cp "$ROOT/vendor/hivex/hivexsh.exe" "$STAGE/"
cp "$ROOT/README.md" "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$STAGE/doc/"
cp -R "$ROOT/docs" "$STAGE/doc/docs"

# The desktop capability builds its guest bridge from source, inside Windows, at
# install time, so the sources have to travel with the binary.
cp -R "$ROOT/guest/wqui" "$STAGE/wqui"
rm -rf "$STAGE/wqui/bin" "$STAGE/wqui/obj"

# GPL/LGPL: the licence texts travel with the binaries they cover.
curl -sSL -o "$STAGE/doc/LICENSE.ntfsprogs" \
  https://www.gnu.org/licenses/old-licenses/gpl-2.0.txt
curl -sSL -o "$STAGE/doc/LICENSE.hivex" \
  https://www.gnu.org/licenses/old-licenses/lgpl-2.1.txt

# A source tree that has been near macOS carries AppleDouble sidecars, and
# `cp -R` brings them along. They are not ours to ship.
find "$STAGE" -name '._*' -delete
find "$STAGE" -name '.DS_Store' -delete

echo "==> checking the staged tree"
for required in winquick.exe ntfscp.exe ntfscat.exe hivexsh.exe \
                doc/LICENSE doc/LICENSE.ntfsprogs doc/LICENSE.hivex \
                wqui/wqui.csproj; do
  [ -e "$STAGE/$required" ] || { echo "missing from the archive: $required" >&2; exit 1; }
done
# A helper that needs a DLL only MSYS2 has is not shippable.
for tool in ntfscp ntfscat hivexsh; do
  stray="$(objdump -p "$STAGE/$tool.exe" | sed -n 's/^\s*DLL Name: //p' \
           | grep -viE '^(kernel32|msvcrt|advapi32|user32|ucrtbase|api-ms-win-)' || true)"
  [ -z "$stray" ] || { echo "$tool.exe needs non-system DLLs:" >&2; echo "$stray" >&2; exit 1; }
done

echo "==> packaging"
# Deterministic, for the same reason the macOS and Linux archives are: a scoop
# or winget manifest pins a checksum, which stops meaning anything the moment
# the same source rebuilds to different bytes. -X drops the extra-field
# timestamps zip would otherwise record beside the fixed DOS ones.
( cd "$DIST" && find "$NAME" -exec touch -h -t 200001010000.00 {} + ) 2>/dev/null || true
rm -f "$DIST/$NAME.zip"
( cd "$DIST" && find "$NAME" | LC_ALL=C sort | zip -X -9 -q "$NAME.zip" -@ )

echo "==> corresponding source"
NTFS_TARBALL="ntfs-3g_ntfsprogs-2022.10.3.tgz"
[ -f "$DIST/$NTFS_TARBALL" ] || \
  curl -sSL -o "$DIST/$NTFS_TARBALL" "https://tuxera.com/opensource/$NTFS_TARBALL"
HIVEX_TARBALL="hivex-1.3.24.tar.gz"
[ -f "$DIST/$HIVEX_TARBALL" ] || \
  curl -sSL -o "$DIST/$HIVEX_TARBALL" "https://download.libguestfs.org/hivex/$HIVEX_TARBALL"

( cd "$DIST" && sha256sum "$NAME.zip" > "$NAME.zip.sha256" )
echo "==> done"
ls -lh "$DIST/$NAME.zip"
cat "$DIST/$NAME.zip.sha256"
