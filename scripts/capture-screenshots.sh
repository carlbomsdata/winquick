#!/bin/bash
# Reproduce the screenshots used on the website and in the README.
#
# Repository tooling, not product surface: there is no `winquick screenshot`
# command and there should not be one.
#
# Every image comes from a real run. Terminal images are typeset from output
# this script actually captured - nothing is typed by hand into a mockup - and
# the guest screenshots come from `winquick ui-test`, which drives the real
# application and asks Windows for its own framebuffer.
#
#     ./scripts/capture-screenshots.sh [output-dir]
#
# Reproduced here: the terminal transcripts, plus ui-automation.png and
# desktop.png from examples/WpfDemo.
#
# NOT reproduced here: notepad.png, taskmgr.png, real-apps.png,
# thirdparty-gui.png and wpf-app.png. Those show software this repository does
# not ship and may not redistribute - Sysinternals tools, Windows' own Notepad
# and Task Manager, and a third-party release binary - so they were captured
# by hand, the same way, from the same guest framebuffer. Anyone can reproduce
# them by putting those programs in a published directory and running
# `winquick start --app` against it.
set -euo pipefail

# Every proof below asserts its own result. A capture that silently produced a
# misleading picture is worse than no picture, so anything unexpected stops the
# script rather than being typeset into an asset.
die() { echo "capture failed: $*" >&2; exit 1; }

# Runs a command that is EXPECTED to fail, and checks that it did. `set -e`
# would otherwise abort on a deliberate negative proof.
expect_fail() {
  local what="$1"; shift
  if "$@" >/dev/null 2>&1; then die "$what unexpectedly succeeded"; fi
}
HERE="$(cd "$(dirname "$0")/.." && pwd)"
WQ="$HERE/target/release/winquick"
OUT="${1:-$HERE/assets/screenshots}"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"

command -v rsvg-convert >/dev/null || { echo "needs rsvg-convert (brew install librsvg)" >&2; exit 1; }
[ -x "$WQ" ] || { echo "build first: cargo build --release" >&2; exit 1; }

# --- typeset a captured transcript ------------------------------------------
# One SVG per image, converted to PNG at 2x so the text stays crisp.
render() {
  local name="$1" title="$2" file="$3"
  local pad=28 lh=25 fs=16.5 top=64
  local lines rows w svg clean
  # PowerShell and friends emit ANSI colour escapes, and a raw ESC is not a
  # legal XML character. Strip them once, for the whole transcript: BSD sed
  # cannot express the character class portably, and doing it per line meant a
  # failing sed silently produced an image with no text in it.
  clean="$WORK/$name.txt"
  python3 -c "
import re,sys
src, dst = sys.argv[1], sys.argv[2]
raw = open(src, 'r', errors='replace').read()
raw = re.sub(r'\x1b\[[0-9;?]*[a-zA-Z]', '', raw)
raw = ''.join(c for c in raw if c == '\n' or ord(c) >= 32)
# A published screenshot should not carry whoever generated it. The path is
# the same path; a shell would print it this way too.
import os
raw = raw.replace(os.path.expanduser('~'), '~')
open(dst, 'w').write(raw)
" "$file" "$clean"
  file="$clean"

  # A published image must not carry this machine. The home directory is
  # rewritten to ~ above; anything else absolute is a path nobody else has, and
  # on macOS the per-user temp directory is a machine identifier in its own
  # right. Refuse rather than typeset it.
  if grep -qE '/var/folders/|/private/(tmp|var)/|/Users/[^/ ]+' "$file"; then
    die "$name transcript still contains a machine-specific path: $(grep -oE '/var/folders/[^ ]*|/private/(tmp|var)/[^ ]*|/Users/[^/ ]+[^ ]*' "$file" | head -1)"
  fi

  rows=$(wc -l < "$file" | tr -d ' ')
  w=$(awk '{ if (length($0) > m) m = length($0) } END { print (m < 58 ? 58 : m) }' "$file")
  local width=$(( pad*2 + w*10 ))
  local height=$(( top + rows*lh + pad ))
  svg="$WORK/$name.svg"
  {
    printf '<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" viewBox="0 0 %d %d">\n' \
      "$width" "$height" "$width" "$height"
    printf '<defs><clipPath id="r"><rect width="%d" height="%d" rx="11"/></clipPath></defs>\n' "$width" "$height"
    printf '<g clip-path="url(#r)">\n'
    printf '<rect width="%d" height="%d" fill="#16181d"/>\n' "$width" "$height"
    printf '<rect width="%d" height="40" fill="#1e2127"/>\n' "$width"
    printf '<circle cx="22" cy="20" r="6" fill="#3a3f4a"/><circle cx="42" cy="20" r="6" fill="#3a3f4a"/><circle cx="62" cy="20" r="6" fill="#3a3f4a"/>\n'
    printf '<text x="%d" y="25" font-family="ui-monospace,SFMono-Regular,Menlo,monospace" font-size="13" fill="#8b91a0">%s</text>\n' \
      $((width/2 - ${#title}*4)) "$title"
    local y=$top
    while IFS= read -r line; do
      local colour="#d5d9e0" text="$line"
      case "$line" in
        '$ '*) colour="#7fd88f" ;;
        '#'*)  colour="#6b7280" ;;
        *' ms'|*' ms '*) colour="#d5d9e0" ;;
      esac
      # PowerShell and friends emit ANSI colour escapes, and a raw ESC is not
      # a legal XML character. Strip control bytes before escaping the rest.
      text=$(printf '%s' "$text" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g')
      printf '<text x="%d" y="%d" xml:space="preserve" font-family="ui-monospace,SFMono-Regular,Menlo,monospace" font-size="%s" fill="%s">%s</text>\n' \
        "$pad" "$y" "$fs" "$colour" "$text"
      y=$((y + lh))
    done < "$file"
    printf '</g></svg>\n'
  } > "$svg"
  rsvg-convert -z 2 -o "$OUT/$name.png" "$svg"
  # A blank or missing image is the failure mode this script exists to
  # avoid; a real typeset transcript is never this small.
  [ -s "$OUT/$name.png" ] || die "$name.png was not written"
  [ "$(wc -c < "$OUT/$name.png")" -gt 2000 ] || die "$name.png looks blank"
  echo "  $name.png"
}

echo "Capturing terminal transcripts from real runs..."

# --- 1. the core promise -----------------------------------------------------
T="$WORK/t"; : > "$T"
echo '$ winquick run -- cmd /c ver' >> "$T"
"$WQ" run -- cmd /c ver 2>/dev/null | sed '/^$/d' >> "$T"
echo '' >> "$T"
echo '$ winquick run -- cmd /c "echo %PROCESSOR_ARCHITECTURE%"' >> "$T"
"$WQ" run -- cmd /c "echo %PROCESSOR_ARCHITECTURE%" 2>/dev/null | sed '/^$/d' >> "$T"
render cli-run "winquick — a real Windows command" "$T"

# --- 2. speed, measured now --------------------------------------------------
T="$WORK/t"; : > "$T"
echo '# every run is a fresh Windows environment' >> "$T"
echo '$ 5 x  winquick run -- cmd /c ver' >> "$T"
# One timing process for the whole sample. Starting a python interpreter
# before and after every run used to add its own startup cost to each number,
# which is how the published figures came to be wrong.
python3 - "$WQ" >> "$T" <<'BENCH'
import subprocess, sys, time
wq = sys.argv[1]
for i in range(1, 6):
    t0 = time.perf_counter_ns()
    r = subprocess.run([wq, "run", "--", "cmd", "/c", "ver"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ms = (time.perf_counter_ns() - t0) / 1e6
    if r.returncode != 0:
        sys.exit(f"warm run {i} exited {r.returncode}")
    print(f"  run {i}   {ms:.0f} ms")
BENCH
render speed "winquick — warm start, measured" "$T"

# --- 3. PowerShell -----------------------------------------------------------
T="$WORK/t"; : > "$T"
echo '$ winquick run -- pwsh -Command $PSVersionTable.PSVersion' >> "$T"
"$WQ" run -- pwsh -Command '$PSVersionTable.PSVersion' 2>/dev/null | sed '/^$/d' | head -4 >> "$T"
render powershell "winquick — PowerShell 7 in disposable Windows" "$T"

# --- 4. .NET -----------------------------------------------------------------
T="$WORK/t"; : > "$T"
echo '$ winquick run -- cmd /c "dotnet --version"' >> "$T"
"$WQ" run -- cmd /c "dotnet --version" 2>/dev/null | sed '/^$/d' >> "$T"
echo '' >> "$T"
echo '$ winquick run -- cmd /c "dotnet --list-sdks"' >> "$T"
"$WQ" run -- cmd /c "dotnet --list-sdks" 2>/dev/null | sed '/^$/d' >> "$T"
render dotnet "winquick — the .NET SDK inside Windows" "$T"

# --- 5. workspace in, source untouched ---------------------------------------
WS="$WORK/project"; mkdir -p "$WS"
printf 'print("hello from the host")\n' > "$WS/app.py"
printf 'a source file the guest must not change\n' > "$WS/NOTES.txt"
BEFORE=$(shasum -a 256 "$WS/NOTES.txt" | cut -d' ' -f1)
T="$WORK/t"; : > "$T"
echo '# your project is copied in, never mounted' >> "$T"
echo '$ winquick run -w . -- cmd /c "dir /b C:\workspace"' >> "$T"
"$WQ" run -w "$WS" -- cmd /c "dir /b C:\\workspace" 2>/dev/null | sed '/^$/d' >> "$T"
echo '' >> "$T"
echo '$ winquick run -w . -- cmd /c "echo CHANGED > C:\workspace\NOTES.txt"' >> "$T"
"$WQ" run -w "$WS" -- cmd /c "echo CHANGED > C:\\workspace\\NOTES.txt" >/dev/null 2>&1
AFTER=$(shasum -a 256 "$WS/NOTES.txt" | cut -d' ' -f1)
if [ "$BEFORE" = "$AFTER" ]; then
  echo '# the guest wrote to its copy. the original is untouched:' >> "$T"
  echo "  NOTES.txt  sha256 unchanged  ${BEFORE:0:24}..." >> "$T"
else
  die "the guest changed the host source file"
fi
render workspace "winquick — copy-in workspace, host untouched" "$T"

# --- 6. artifacts out --------------------------------------------------------
T="$WORK/t"; : > "$T"
echo '# nothing leaves the guest unless you ask for it' >> "$T"
echo '$ winquick run -w . -a "out/**" -- cmd /c "mkdir out & echo built > out\report.txt"' >> "$T"
# Run from inside the project so artifacts land in ./winquick-artifacts and are
# reported by that name. Passing --artifacts-dir put an absolute temp path in
# the picture, which leaked this machine's private temp directory and did not
# match the `cat` line underneath it.
ART="$WS/winquick-artifacts"
rm -rf "$ART"
( cd "$WS" && "$WQ" run -w "$WS" -a "out/**" -- \
    cmd /c "mkdir out & echo built > out\\report.txt" 2>&1 ) \
  | grep -i retrieved | sed 's/^winquick: /  /' >> "$T"
echo '' >> "$T"
echo '$ cat winquick-artifacts/out/report.txt' >> "$T"
[ -f "$ART/out/report.txt" ] || die "the requested artifact was not retrieved"
grep -q '^built' "$ART/out/report.txt" || die "the artifact did not contain what the guest wrote"
sed 's/^/  /' "$ART/out/report.txt" >> "$T"
render artifacts "winquick — explicit artifact extraction" "$T"

# --- 7. what is installed ----------------------------------------------------
T="$WORK/t"; : > "$T"
echo '$ winquick info' >> "$T"
"$WQ" info 2>/dev/null | sed '/^$/d' >> "$T"
render info "winquick — what is installed" "$T"

# --- 8. offline by default ---------------------------------------------------
T="$WORK/t"; : > "$T"
echo '# the guest has no network adapter at all' >> "$T"
echo '$ winquick run -- cmd /c "ipconfig | find /c \"IPv4\""' >> "$T"
# `find /c` prints its count and then exits 1 when that count is zero, which is
# the answer we want rather than an error. Capture it without letting `set -e`
# and pipefail treat the guest's correct reply as a failed capture, then assert
# the number is actually zero -- a picture saying "0 adapters" must not be
# produced from a run that reported one.
ADAPTERS=$("$WQ" run -- cmd /c "ipconfig | find /c \"IPv4\"" 2>/dev/null | tr -d ' \r\n' || true)
[ "$ADAPTERS" = "0" ] || die "the guest reported $ADAPTERS IPv4 adapters, not 0"
echo "  $ADAPTERS" >> "$T"
echo '' >> "$T"
echo '$ winquick run -- cmd /c "ping -n 1 -w 2000 1.1.1.1"' >> "$T"
# No adapter, so this must fail. If it ever succeeds the offline claim is
# wrong and no picture should be produced.
expect_fail "ping from the offline guest" "$WQ" run -- cmd /c "ping -n 1 -w 2000 1.1.1.1"
"$WQ" run -- cmd /c "ping -n 1 -w 2000 1.1.1.1" 2>/dev/null | sed '/^$/d' | head -3 >> "$T" || true
render offline "winquick — offline by default" "$T"

# --- 9. the real application, driven, from the guest's own framebuffer -------
# Not typeset: these two are the pixels Windows drew. `ui-test` builds the demo
# inside the guest, runs every step of demo.uitest against it, and fails if any
# `expect` does not hold - so a picture is only produced once the application
# is genuinely in the state the caption claims.
echo
echo "Driving the demo application for the guest screenshots..."
SHOTS="$WORK/shots"
"$WQ" ui-test "$HERE/examples/WpfDemo/DemoApp.csproj" \
  --script "$HERE/examples/WpfDemo/demo.uitest" --out "$SHOTS" >/dev/null \
  || die "the UI test did not pass, so its screenshots would not mean anything"

# 02-after is the window once every control has been driven; 03-desktop is the
# whole session. Names on the website are what the picture shows.
[ -f "$SHOTS/02-after.png" ]   || die "the driven-window screenshot was not written"
[ -f "$SHOTS/03-desktop.png" ] || die "the desktop screenshot was not written"
cp "$SHOTS/02-after.png"   "$OUT/ui-automation.png"
cp "$SHOTS/03-desktop.png" "$OUT/desktop.png"

# A framebuffer read that arrives too early is a black rectangle, and a black
# rectangle is exactly the kind of picture that looks fine in a thumbnail.
for name in ui-automation desktop; do
  [ "$(wc -c < "$OUT/$name.png")" -gt 2000 ] || die "$name.png looks blank"
  python3 - "$OUT/$name.png" "$name" <<'PIX'
import struct, sys, zlib
path, name = sys.argv[1], sys.argv[2]
data = open(path, 'rb').read()
if data[:8] != b'\x89PNG\r\n\x1a\n':
    sys.exit(f"capture failed: {name}.png is not a PNG")
w, h = struct.unpack('>II', data[16:24])
if w < 200 or h < 200:
    sys.exit(f"capture failed: {name}.png is only {w}x{h}")
# Decode enough to be sure it is not a uniform rectangle.
idat = b''
i = 8
while i < len(data):
    ln = struct.unpack('>I', data[i:i+4])[0]
    kind = data[i+4:i+8]
    if kind == b'IDAT':
        idat += data[i+8:i+8+ln]
    i += 12 + ln
raw = zlib.decompress(idat)
# Sample the whole image, not the first scanlines: the top of a desktop
# capture is uniform background, and judging it on that called a perfectly
# good screenshot blank.
step = max(1, len(raw) // 40000)
if len(set(raw[::step])) < 16:
    sys.exit(f"capture failed: {name}.png looks like a blank framebuffer")
print(f"  {name}.png  {w}x{h}")
PIX
done

echo
echo "Wrote images to $OUT"
