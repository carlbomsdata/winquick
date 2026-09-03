#!/bin/bash
# Build a WinQuick release archive for Linux, on the architecture you run it on.
#
#   ./scripts/release-linux.sh 0.3.0
#
# Produces, under dist/:
#   winquick-<version>-linux-<arch>.tar.gz          the archive
#   winquick-<version>-linux-<arch>.tar.gz.sha256   its checksum
#   ntfs-3g_ntfsprogs-<v>.tgz                       GPL corresponding source
#
# QEMU is NOT bundled. WinQuick runs it as a separate child process, the same
# way it does on macOS, and a distribution's own package is the right thing to
# use -- provided it is new enough. Ubuntu 24.04's QEMU 8.2.2 cannot migrate
# the NVMe device the guest boots from, which makes every run cold; `winquick
# doctor` checks the version and says so rather than letting that happen
# quietly. Bundling a QEMU would mean shipping a GPL-2.0 binary plus its whole
# shared-library closure, and carrying the corresponding-source obligation for
# it, to work around a dependency the user can simply install.
set -euo pipefail

VERSION="${1:?usage: release-linux.sh <version>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Both architectures are supported hosts -- the guest follows the host, so an
# arm64 Linux machine runs an ARM64 Windows exactly as an Apple Silicon Mac
# does. The archive is named for the one it was built on.
case "$(uname -m)" in
  x86_64)          ARCH=x86_64;  ELF='ELF 64-bit.*x86-64' ;;
  aarch64|arm64)   ARCH=aarch64; ELF='ELF 64-bit.*ARM aarch64' ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
NAME="winquick-${VERSION}-linux-${ARCH}"
DIST="$ROOT/dist"
STAGE="$DIST/$NAME"

CARGO_VERSION=$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)
if [ "$CARGO_VERSION" != "$VERSION" ]; then
  echo "Cargo.toml says $CARGO_VERSION but you asked for $VERSION" >&2
  exit 1
fi
[ "$(uname -s)" = Linux ]   || { echo "run this on Linux" >&2; exit 1; }


echo "==> building winquick $VERSION"
cd "$ROOT"
cargo build --release --locked
BIN="$ROOT/target/release/winquick"
file "$BIN" | grep -qE "$ELF" || { echo "not a $ARCH ELF" >&2; exit 1; }

echo "==> building the NTFS helpers"
# The distribution's ntfscp will not do: WinQuick addresses a partition inside
# a whole-disk image through NTFS_IMAGE_OFFSET, which is this project's patch.
"$ROOT/scripts/build-ntfs-helpers.sh" "$ROOT/vendor/ntfsprogs" >/dev/null

echo "==> staging"
rm -rf "$STAGE"; mkdir -p "$STAGE/bin" "$STAGE/libexec/winquick" "$STAGE/share/doc/winquick"
cp "$BIN" "$STAGE/bin/winquick"
cp "$ROOT/vendor/ntfsprogs/ntfscp" "$ROOT/vendor/ntfsprogs/ntfscat" "$STAGE/libexec/winquick/"
cp "$ROOT/README.md" "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$STAGE/share/doc/winquick/"
cp -R "$ROOT/docs" "$STAGE/share/doc/winquick/docs"

# The desktop capability builds its guest bridge from source, inside Windows,
# at install time, so the sources have to travel with the binary.
mkdir -p "$STAGE/share/winquick"
cp -R "$ROOT/guest/wqui" "$STAGE/share/winquick/wqui"
rm -rf "$STAGE/share/winquick/wqui/bin" "$STAGE/share/winquick/wqui/obj"

# GPL: the licence text travels with the binaries it covers.
curl -sSL -o "$STAGE/libexec/winquick/LICENSE.ntfsprogs" \
  https://www.gnu.org/licenses/old-licenses/gpl-2.0.txt

# A source tree that has been near macOS carries AppleDouble sidecars, and
# `cp -R` brings them along. They are not ours to ship.
find "$STAGE" -name '._*' -delete
find "$STAGE" -name '.DS_Store' -delete

echo "==> checking the staged tree"
for required in bin/winquick libexec/winquick/ntfscp libexec/winquick/ntfscat \
                libexec/winquick/LICENSE.ntfsprogs \
                share/winquick/wqui/wqui.csproj; do
  [ -e "$STAGE/$required" ] || { echo "missing from the archive: $required" >&2; exit 1; }
done
# A helper that needs something from the build tree is not shippable.
for tool in ntfscp ntfscat; do
  stray="$(ldd "$STAGE/libexec/winquick/$tool" 2>/dev/null | awk '{print $1}' \
           | grep -viE '^(linux-vdso|libc|libm|libdl|libpthread|/lib|ld-linux)' || true)"
  [ -z "$stray" ] || { echo "$tool links outside the system libraries:" >&2; echo "$stray" >&2; exit 1; }
done

echo "==> packaging"
mkdir -p "$DIST"
# Deterministic, for the same reason the macOS archive is.
( cd "$DIST" && find "$NAME" -exec touch -h -d "2000-01-01 00:00:00" {} + ) 2>/dev/null || true
( cd "$DIST" && find "$NAME" | LC_ALL=C sort > "$DIST/.filelist" )
( cd "$DIST" && tar -cf "$DIST/.$NAME.tar" --format ustar --no-recursion \
    --owner=0 --group=0 -T "$DIST/.filelist" )
gzip -n -9 -c "$DIST/.$NAME.tar" > "$DIST/$NAME.tar.gz"
rm -f "$DIST/.$NAME.tar" "$DIST/.filelist"

echo "==> GPL corresponding source"
NTFS_TARBALL="ntfs-3g_ntfsprogs-2022.10.3.tgz"
[ -f "$DIST/$NTFS_TARBALL" ] || \
  curl -sSL -o "$DIST/$NTFS_TARBALL" "https://tuxera.com/opensource/$NTFS_TARBALL"

( cd "$DIST" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256" )
echo "==> done"
ls -lh "$DIST/$NAME.tar.gz"
cat "$DIST/$NAME.tar.gz.sha256"
