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
set -uo pipefail
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
for i in 1 2 3 4 5; do
  s=$(python3 -c "import time;print(time.time())")
  "$WQ" run -- cmd /c ver >/dev/null 2>&1
  e=$(python3 -c "import time;print(time.time())")
  python3 -c "print('  run $i   %d ms' % (($e-$s)*1000))" >> "$T"
done
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
  echo '  WARNING: host source changed' >> "$T"
fi
render workspace "winquick — copy-in workspace, host untouched" "$T"

# --- 6. artifacts out --------------------------------------------------------
ART="$WORK/artifacts"
T="$WORK/t"; : > "$T"
echo '# nothing leaves the guest unless you ask for it' >> "$T"
echo '$ winquick run -w . -a "out/**" -- cmd /c "mkdir out & echo built > out\report.txt"' >> "$T"
"$WQ" run -w "$WS" -a "out/**" --artifacts-dir "$ART" -- \
  cmd /c "mkdir out & echo built > out\\report.txt" 2>&1 | grep -i retrieved | sed 's/^winquick: /  /' >> "$T"
echo '' >> "$T"
echo '$ cat winquick-artifacts/out/report.txt' >> "$T"
[ -f "$ART/out/report.txt" ] && sed 's/^/  /' "$ART/out/report.txt" >> "$T"
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
"$WQ" run -- cmd /c "ipconfig | find /c \"IPv4\"" 2>/dev/null | sed '/^$/d;s/^/  /' >> "$T"
echo '' >> "$T"
echo '$ winquick run -- cmd /c "ping -n 1 -w 2000 1.1.1.1"' >> "$T"
"$WQ" run -- cmd /c "ping -n 1 -w 2000 1.1.1.1" 2>/dev/null | sed '/^$/d' | head -3 >> "$T"
render offline "winquick — offline by default" "$T"

echo
echo "Wrote terminal images to $OUT"
