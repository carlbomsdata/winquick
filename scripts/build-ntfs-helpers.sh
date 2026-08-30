#!/bin/bash
# Build the two ntfsprogs helpers WinQuick needs, as self-contained binaries.
#
# WinQuick has to write two files into the Windows system volume when it builds
# a runtime. macOS has no NTFS write support at all, and on Windows the obvious
# route -- attach the image and copy -- needs elevation and is exactly what
# endpoint security software blocks. So both hosts use the same two programs,
# writing into the image file directly.
#
# Runs on macOS arm64 and on Windows x86_64 under MSYS2. MSYS2 is a *build-time*
# dependency only: the binaries it produces are native Windows executables that
# link nothing but KERNEL32 and msvcrt.
#
# ntfsprogs is GPL-2.0-or-later and WinQuick's changes to it are in
# patches/ntfsprogs-windows.patch. These stay separate executables, invoked as
# child processes; see THIRD_PARTY_NOTICES.md.
set -euo pipefail

VERSION="${NTFS3G_VERSION:-2022.10.3}"
TARBALL="ntfs-3g_ntfsprogs-${VERSION}.tgz"
URL="https://tuxera.com/opensource/${TARBALL}"
SHA256="f20e36ee68074b845e3629e6bced4706ad053804cbaf062fbae60738f854170c"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PATCH="$ROOT/patches/ntfsprogs-windows.patch"
SHIM="$ROOT/scripts/ntfs-windows"
OUT="${1:-$ROOT/vendor/ntfsprogs}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

case "$(uname -s)" in
  Darwin)          HOST=macos ; EXE=""     ;;
  MINGW*|MSYS*)    HOST=windows; EXE=".exe" ;;
  Linux)           HOST=linux ; EXE=""     ;;
  *) echo "unsupported build host: $(uname -s)" >&2; exit 1 ;;
esac

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else sha256sum "$1" | cut -d' ' -f1; fi
}
jobs_count() {
  if command -v nproc >/dev/null 2>&1; then nproc
  else sysctl -n hw.ncpu; fi
}

echo "==> fetching ntfs-3g/ntfsprogs $VERSION"
curl -sSL -o "$WORK/$TARBALL" "$URL"
GOT="$(sha256_of "$WORK/$TARBALL")"
if [ "$GOT" != "$SHA256" ]; then
  echo "checksum mismatch for $TARBALL" >&2
  echo "  expected $SHA256" >&2
  echo "  got      $GOT" >&2
  exit 1
fi

tar xzf "$WORK/$TARBALL" -C "$WORK"
cd "$WORK/ntfs-3g_ntfsprogs-${VERSION}"

echo "==> applying patches/ntfsprogs-windows.patch"
patch -p1 --quiet < "$PATCH"

CONFIGURE=(
  --disable-ntfs-3g --enable-ntfsprogs --disable-plugins
  --enable-static --disable-shared
  --without-uuid --without-hd
)

if [ "$HOST" = macos ]; then
  echo "==> configuring for macOS arm64"
  ./configure "${CONFIGURE[@]}" CFLAGS="-O2 -arch arm64" >/dev/null
  MAKEFLAGS_EXTRA=()
elif [ "$HOST" = linux ]; then
  # A distribution's own ntfsprogs is not a substitute here. WinQuick addresses
  # a partition *inside* a whole-disk image through NTFS_IMAGE_OFFSET, which is
  # this project's patch; an unpatched ntfscp reads offset zero, finds no NTFS
  # boot sector and reports "NTFS signature is missing".
  echo "==> configuring for native Linux $(uname -m)"
  ./configure "${CONFIGURE[@]}" CFLAGS="-O2" >/dev/null
  MAKEFLAGS_EXTRA=()
else
  echo "==> configuring for native Windows x86_64"
  # ac_cv_header_libintl_h=no: MSYS2 ships gettext headers, and libintl.h
  # redirects snprintf and setlocale to libintl_* symbols that would drag in a
  # DLL. WinQuick has no use for translated ntfscp messages.
  ac_cv_header_libintl_h=no ./configure --host=x86_64-w64-mingw32 \
    "${CONFIGURE[@]}" CFLAGS="-O2" >/dev/null

  # Upstream picks win32_io.c on Windows, which drives *physical drives*.
  # WinQuick works on an image file plus an offset and never touches a drive,
  # so the file-based backend is the correct one. The header that names the
  # default operations is switched to match in the patch.
  sed -i 's/win32_io\.lo/unix_io.lo/g; s/win32_io\.c/unix_io.c/g' libntfs-3g/Makefile

  # _FILE_OFFSET_BITS=64 gives mingw a struct stat that can describe a disk
  # image; without it stat() fails outright on anything over 2 GB.
  MAKEFLAGS_EXTRA=(
    CPPFLAGS="-I$SHIM -include $SHIM/wqtypes.h -D_FILE_OFFSET_BITS=64"
    LDFLAGS="-all-static"
  )
fi

echo "==> building"
make -C libntfs-3g -j"$(jobs_count)" "${MAKEFLAGS_EXTRA[@]+"${MAKEFLAGS_EXTRA[@]}"}" >/dev/null
# Only these two are built: the rest of the suite (mkntfs, ntfsclone, ...) is
# not something WinQuick invokes.
make -C ntfsprogs "ntfscp$EXE" "ntfscat$EXE" -j"$(jobs_count)" \
  "${MAKEFLAGS_EXTRA[@]+"${MAKEFLAGS_EXTRA[@]}"}" >/dev/null

mkdir -p "$OUT"
for tool in ntfscp ntfscat; do
  src="ntfsprogs/.libs/$tool$EXE"
  [ -f "$src" ] || src="ntfsprogs/$tool$EXE"
  if [ ! -f "$src" ]; then echo "did not build $tool" >&2; exit 1; fi
  cp "$src" "$OUT/$tool$EXE"
  strip "$OUT/$tool$EXE" 2>/dev/null || true
  chmod +x "$OUT/$tool$EXE"
done

echo "==> built into $OUT"
for tool in ntfscp ntfscat; do
  printf '    %-8s %s\n' "$tool$EXE" "$(file -b "$OUT/$tool$EXE" | cut -d, -f1-2)"
done

# A helper that needs a runtime beside it is not a helper WinQuick can ship.
echo "==> checking for stray dynamic dependencies"
if [ "$HOST" = linux ]; then
  for tool in ntfscp ntfscat; do
    stray="$(ldd "$OUT/$tool" 2>/dev/null | awk '{print $1}' \
             | grep -viE '^(linux-vdso|libc|libm|libdl|libpthread|/lib|ld-linux)' || true)"
    if [ -n "$stray" ]; then
      echo "    WARNING: $tool links outside the system libraries:" >&2
      echo "$stray" >&2
    fi
  done
elif [ "$HOST" = macos ]; then
  for tool in ntfscp ntfscat; do
    if otool -L "$OUT/$tool" | tail -n +2 | grep -qv '^\s*/usr/lib\|^\s*/System'; then
      echo "    WARNING: $tool links outside the system libraries:" >&2
      otool -L "$OUT/$tool" | tail -n +2 | grep -v '/usr/lib\|/System' >&2
    fi
  done
else
  for tool in ntfscp ntfscat; do
    stray="$(objdump -p "$OUT/$tool.exe" | sed -n 's/^\s*DLL Name: //p' \
             | grep -viE '^(kernel32|msvcrt|advapi32|user32)\.dll$' || true)"
    if [ -n "$stray" ]; then
      echo "    WARNING: $tool.exe needs non-system DLLs:" >&2
      echo "$stray" >&2
    fi
  done
fi
