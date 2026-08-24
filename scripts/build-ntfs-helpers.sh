#!/bin/bash
# Build the two ntfsprogs helpers WinQuick needs, as self-contained arm64 binaries.
#
# WinQuick has to write two files into the Windows system volume when it builds a
# runtime, and macOS has no NTFS write support at all. Homebrew's ntfs-3g formula
# is Linux-only, so we build just the pieces we use.
#
# ntfsprogs is GPL-2.0-or-later. These stay separate executables, invoked as
# child processes; see THIRD_PARTY_NOTICES.md and scripts/fetch-helper-sources.sh.
set -euo pipefail

VERSION="${NTFS3G_VERSION:-2022.10.3}"
TARBALL="ntfs-3g_ntfsprogs-${VERSION}.tgz"
URL="https://tuxera.com/opensource/${TARBALL}"
SHA256="f20e36ee68074b845e3629e6bced4706ad053804cbaf062fbae60738f854170c"

OUT="${1:-$(cd "$(dirname "$0")/.." && pwd)/vendor/ntfsprogs}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "==> fetching ntfs-3g/ntfsprogs $VERSION"
curl -sSL -o "$WORK/$TARBALL" "$URL"
GOT=$(shasum -a 256 "$WORK/$TARBALL" | cut -d' ' -f1)
if [ "$GOT" != "$SHA256" ]; then
  echo "checksum mismatch for $TARBALL" >&2
  echo "  expected $SHA256" >&2
  echo "  got      $GOT" >&2
  exit 1
fi

echo "==> building"
tar xzf "$WORK/$TARBALL" -C "$WORK"
cd "$WORK/ntfs-3g_ntfsprogs-${VERSION}"
./configure \
  --disable-ntfs-3g --enable-ntfsprogs --disable-plugins \
  --enable-static --disable-shared \
  --without-uuid --without-hd \
  CFLAGS="-O2 -arch arm64" >/dev/null

# `make install` trips over an install hook we do not need; the binaries are fine.
make -j"$(sysctl -n hw.ncpu)" >/dev/null 2>&1 || true

mkdir -p "$OUT"
for tool in ntfscp ntfscat; do
  src="ntfsprogs/.libs/$tool"
  [ -f "$src" ] || src="ntfsprogs/$tool"
  if [ ! -f "$src" ]; then echo "did not build $tool" >&2; exit 1; fi
  cp "$src" "$OUT/$tool"
  strip -S "$OUT/$tool" 2>/dev/null || true
  chmod +x "$OUT/$tool"
done

echo "==> built into $OUT"
for tool in ntfscp ntfscat; do
  printf '    %-8s %s\n' "$tool" "$(file -b "$OUT/$tool" | cut -d, -f1-2)"
  if otool -L "$OUT/$tool" | tail -n +2 | grep -qv '^\s*/usr/lib\|^\s*/System'; then
    echo "    WARNING: $tool has non-system dynamic dependencies:" >&2
    otool -L "$OUT/$tool" | tail -n +2 | grep -v '/usr/lib\|/System' >&2
  fi
done
