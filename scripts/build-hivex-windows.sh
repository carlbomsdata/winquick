#!/bin/bash
# Build hivexsh as a self-contained native Windows executable.
#
# `winquick setup` sets one value in the guest's SOFTWARE hive -- the AutoRun
# entry that starts the agent -- and hivexsh is what does it. On macOS that
# comes from Homebrew. Windows has no package for it, and upstream excludes
# hivexsh from Windows builds entirely, so WinQuick builds and ships it.
#
# Runs on Windows x86_64 under MSYS2. MSYS2 is a *build-time* dependency only:
# the binary it produces links nothing but KERNEL32 and msvcrt.
#
# hivexsh is GPL-2.0-or-later and the hivex library is LGPL-2.1-or-later.
# WinQuick's one change is in patches/hivex-windows.patch. It stays a separate
# executable, invoked as a child process; see THIRD_PARTY_NOTICES.md.
set -euo pipefail

VERSION="${HIVEX_VERSION:-1.3.24}"
TARBALL="hivex-${VERSION}.tar.gz"
URL="https://download.libguestfs.org/hivex/${TARBALL}"
SHA256="a52fa45cecc9a78adb2d28605d68261e4f1fd4514a778a5473013d2ccc8a193c"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PATCH="$ROOT/patches/hivex-windows.patch"
OUT="${1:-$ROOT/vendor/hivex}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

case "$(uname -s)" in
  MINGW*|MSYS*) : ;;
  *) echo "this script builds the Windows helper and must run under MSYS2" >&2; exit 1 ;;
esac

# pod2man lives in perl's core_perl directory, which is not on the default PATH.
export PATH="$PATH:/usr/bin/core_perl"
command -v pod2man >/dev/null 2>&1 || {
  echo "pod2man is missing. Install it with:  pacman -S perl" >&2
  exit 1
}

echo "==> fetching hivex $VERSION"
curl -sSL -o "$WORK/$TARBALL" "$URL"
GOT="$(sha256sum "$WORK/$TARBALL" | cut -d' ' -f1)"
if [ "$GOT" != "$SHA256" ]; then
  echo "checksum mismatch for $TARBALL" >&2
  echo "  expected $SHA256" >&2
  echo "  got      $GOT" >&2
  exit 1
fi

tar xzf "$WORK/$TARBALL" -C "$WORK"
cd "$WORK/hivex-${VERSION}"

echo "==> applying patches/hivex-windows.patch"
patch -p1 --quiet < "$PATCH"

echo "==> configuring"
# ac_cv_header_libintl_h=no: MSYS2 ships gettext headers, and libintl.h
# redirects snprintf and setlocale to libintl_* symbols that would drag in a
# DLL. WinQuick has no use for translated hivexsh messages.
ac_cv_header_libintl_h=no ./configure --host=x86_64-w64-mingw32 \
  --disable-ocaml --disable-perl --disable-python --disable-ruby --disable-rust \
  --enable-static --disable-shared --disable-nls \
  CFLAGS="-O2" >/dev/null

# readline only serves interactive line editing, which WinQuick never uses, and
# it would pull in libreadline and libtermcap. configure has no switch for it,
# so the detection result is cleared directly.
sed -i 's|^#define HAVE_LIBREADLINE 1|/* #undef HAVE_LIBREADLINE */|' config.h

echo "==> building"
make -j"$(nproc)" >/dev/null
# Upstream leaves sh/ out of SUBDIRS on Windows, so it is built by name.
make -C sh hivexsh.exe LIBREADLINE= LDFLAGS="-all-static" >/dev/null

mkdir -p "$OUT"
cp sh/hivexsh.exe "$OUT/hivexsh.exe"
strip "$OUT/hivexsh.exe" 2>/dev/null || true
chmod +x "$OUT/hivexsh.exe"

echo "==> built into $OUT"
printf '    %-12s %s\n' "hivexsh.exe" "$(file -b "$OUT/hivexsh.exe" | cut -d, -f1-2)"

echo "==> checking for stray dynamic dependencies"
stray="$(objdump -p "$OUT/hivexsh.exe" | sed -n 's/^\s*DLL Name: //p' \
         | grep -viE '^(kernel32|msvcrt|advapi32|user32)\.dll$' || true)"
if [ -n "$stray" ]; then
  echo "    WARNING: hivexsh.exe needs non-system DLLs:" >&2
  echo "$stray" >&2
fi
