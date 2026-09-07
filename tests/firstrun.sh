#!/bin/bash
# WinQuick first-run test: install from nothing, then do what the readme says.
#
#   ./tests/firstrun.sh <path to the ValidationOS ISO or its VHDX>
#
# Every other test in this directory starts from a working ~/.winquick. This is
# the one that does not. It builds a runtime in a throwaway HOME and asks the
# only question a new user asks: I cloned this and ran the two commands in the
# readme -- did I get Windows?
#
# The image is a parameter because WinQuick ships no Microsoft software and
# never will. Point it at the ISO you downloaded, or at the VHDX inside it.
#
# The real ~/.winquick is not touched, read, or relied on: HOME is overridden
# for every command, so a machine with no WinQuick installed runs this exactly
# the same way as one with a 20 GiB runtime already built.
set -u
SCRIPTDIR="$(cd "$(dirname "$0")" && pwd)"
WQ="$SCRIPTDIR/../target/release/winquick"
IMAGE="${1:-}"

pass=0; fail=0
ok()  { echo "  ok   $1"; pass=$((pass+1)); }
bad() { echo "  FAIL $1"; [ -n "${2:-}" ] && echo "       $2"; fail=$((fail+1)); }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "expected [$3], got [$2]"; fi }

[ -x "$WQ" ] || { echo "build it first: cargo build --release"; exit 2; }
[ -n "$IMAGE" ] && [ -f "$IMAGE" ] || {
  echo "usage: $0 <ValidationOS ISO or VHDX>"
  echo
  echo "WinQuick ships no Microsoft software. Obtain Validation OS from Microsoft"
  echo "under its licence and pass the file here."
  exit 2
}

case "$(uname -m)" in
  arm64|aarch64) GUEST=arm64 ;;
  *)             GUEST=x64   ;;
esac
if command -v shasum >/dev/null 2>&1; then sha256() { shasum -a 256 "$@"; }
else sha256() { sha256sum "$@"; }; fi

# A throwaway HOME, and deliberately a short one. QEMU's QMP socket lives under
# $HOME/.winquick/run/<run>/qmp.sock and a UNIX socket path cannot exceed 104
# bytes, so putting this under a deep scratch directory fails the run with an
# error about sockets that has nothing to do with what is being tested. A real
# home directory is nowhere near the limit; a test harness's can be.
# /var/tmp before /tmp: /tmp is a 3.9 GiB tmpfs on a stock Ubuntu, and a runtime
# needs about 8 GiB, so the install dies part-way through with ENOSPC and every
# check after it fails for a reason that has nothing to do with WinQuick.
# WQ_FIRSTRUN_DIR overrides both.
TMPROOT=${WQ_FIRSTRUN_DIR:-}
if [ -z "$TMPROOT" ]; then
  for cand in /var/tmp /tmp; do
    [ -d "$cand" ] && [ -w "$cand" ] && { TMPROOT=$cand; break; }
  done
fi
HOMEDIR=$(mktemp -d "$TMPROOT/wq-firstrun.XXXXXX") || exit 2
if [ ${#HOMEDIR} -gt 40 ]; then
  echo "refusing to run: the temporary HOME is $HOMEDIR"
  echo "that leaves too little room for a QMP socket path (limit is 104 bytes)"
  echo "set WQ_FIRSTRUN_DIR to somewhere shorter"
  rmdir "$HOMEDIR" 2>/dev/null
  exit 2
fi
# Setup writes a multi-gigabyte image. Finding that out half way through turns
# every later check into a mystery, so ask first.
avail_kb=$(df -Pk "$HOMEDIR" | awk 'NR==2 {print $4}')
if [ "${avail_kb:-0}" -lt 12000000 ]; then
  echo "refusing to run: only $(( avail_kb / 1024 )) MiB free on $TMPROOT"
  echo "a runtime needs about 8 GiB; set WQ_FIRSTRUN_DIR to a filesystem with room"
  rmdir "$HOMEDIR" 2>/dev/null
  exit 2
fi
wq() { env HOME="$HOMEDIR" "$WQ" "$@"; }
cleanup() { rm -rf "$HOMEDIR"; }
trap cleanup EXIT

echo "WinQuick first-run test"
echo "  binary $WQ"
echo "  image  $IMAGE"
echo "  home   $HOMEDIR  (removed afterwards; ~/.winquick is not touched)"
echo

# -- nothing installed yet ---------------------------------------------------
echo "== before setup =="
out=$(wq run -- cmd /c ver 2>&1); rc=$?
[ "$rc" -ne 0 ] && ok "a run before setup fails rather than hanging" \
  || bad "run before setup" "exit $rc"
case "$out" in
  *"winquick setup"*) ok "and it says to run setup" ;;
  *) bad "run before setup names no remedy" "$out" ;;
esac

# -- setup -------------------------------------------------------------------
echo "== setup =="
t0=$(date +%s)
out=$(wq setup --from "$IMAGE" 2>&1); rc=$?
el=$(( $(date +%s)-t0 ))
check "setup succeeds" "$rc" "0"
case "$out" in
  *"Windows runtime installed"*) ok "setup says the runtime is installed" ;;
  *) bad "setup output" "$out" ;;
esac
# Setup ends by booting Windows and running a command. That is the check that
# matters: an image that is written but does not boot is worse than no image.
case "$out" in
  *"Testing the runtime"*) ok "setup verifies the runtime by booting it" ;;
  *) bad "setup ran no smoke test" "$out" ;;
esac
case "$out" in
  *"failed to start"*|*"FAIL"*) bad "the smoke test did not pass" "$out" ;;
  *) ok "the smoke test passed" ;;
esac
echo "       (setup took ${el}s)"

BASE="$HOMEDIR/.winquick/images/validation-$GUEST/base.qcow2"
[ -f "$BASE" ] && ok "the base image exists" || bad "no base image" "$BASE"
[ -f "${BASE%.qcow2}.json" ] && ok "the base image records which agent built it" \
  || bad "no base metadata" "an image with no version stamp is never checked"
# Recorded now, before a single run has booted it, so the disposability check at
# the end compares against the file setup actually wrote.
BEFORE_FILE="$HOMEDIR/base.sha256"
[ -f "$BASE" ] && sha256 "$BASE" | cut -d' ' -f1 > "$BEFORE_FILE"

# -- the two commands the readme gives a new user ----------------------------
echo "== the documented first commands =="
out=$(wq run -- cmd /c ver 2>&1); rc=$?
check "winquick run exits 0" "$rc" "0"
case "$out" in
  *"Microsoft Windows"*)
    ok "it prints a real Windows version: $(echo "$out" | tr -d '\r' | grep Microsoft | head -1)" ;;
  *) bad "no Windows version" "$out" ;;
esac

# -- the promises the readme makes about that command ------------------------
echo "== stdout, stderr and exit codes =="
out=$(wq run -- cmd /c "exit 3" 2>/dev/null); rc=$?
check "the guest exit code is returned unchanged" "$rc" "3"
# `echo ERR 1>&2` makes cmd emit the space that sits before the redirect, so
# these compare on trimmed text. The stream separation is what is under test.
trim() { tr -d '\r\n' | sed 's/[[:space:]]*$//'; }
o=$(wq run -- cmd /c "echo OUT& echo ERR 1>&2" 2>/dev/null | trim)
check "stdout carries only stdout" "$o" "OUT"
e=$(wq run -- cmd /c "echo OUT& echo ERR 1>&2" 2>&1 >/dev/null | trim)
check "stderr carries only stderr" "$e" "ERR"

# -- the second run is the fast one -----------------------------------------
echo "== the prepared guest =="
[ -f "$HOMEDIR/.winquick/states/validation-$GUEST/ready.json" ] \
  && ok "a prepared guest was built during setup" \
  || bad "no prepared guest" "every run would boot cold"
t0=$(date +%s%N 2>/dev/null || date +%s)
wq run -- cmd /c ver >/dev/null 2>&1
t1=$(date +%s%N 2>/dev/null || date +%s)
ms=$(( (t1-t0)/1000000 ))
[ "$ms" -lt 5000 ] && ok "a warm run takes ${ms}ms" \
  || bad "a warm run took ${ms}ms" "that is a cold boot, not a resume"

# -- a command that outlives its timeout ------------------------------------
#
# On a brand-new install, which is where this went most wrong: the acknowledgement
# the host waits for is written by the agent baked into the image setup just
# built. When that agent deleted the go flag without dismounting the mailbox the
# host never saw it, and a 60 s timeout discarded the prepared guest, rebuilt it
# five times and cold booted -- 182 s, on the first thing a new user tried that
# took longer than ten seconds.
echo "== a command that runs out of time =="
t0=$(date +%s)
wq run --timeout 60 -- cmd /c "ping -n 200 127.0.0.1 >nul" >/dev/null 2>&1
rc=$?
el=$(( $(date +%s)-t0 ))
[ "$rc" -ne 0 ] && ok "a timeout fails rather than hanging" || bad "timeout" "exit $rc"
[ "$el" -lt 120 ] && ok "and it costs the timeout (${el}s), not a rebuild storm" \
  || bad "timeout cascade" "took ${el}s for a 60 s timeout"
[ -f "$HOMEDIR/.winquick/states/validation-$GUEST/ready.json" ] \
  && ok "the prepared guest survives a timeout" \
  || bad "prepared guest discarded by a timeout" "the next run pays to rebuild it"

# -- workspace and artifacts -------------------------------------------------
echo "== workspace and artifacts =="
WS="$HOMEDIR/ws"; mkdir -p "$WS"; echo hello > "$WS/input.txt"
out=$(cd "$WS" && env HOME="$HOMEDIR" "$WQ" run -w . -a "out/**" -- \
  cmd /c "type input.txt && mkdir out && echo made-it > out\\result.txt" 2>&1); rc=$?
check "a workspace run succeeds" "$rc" "0"
case "$out" in *hello*) ok "the workspace is readable inside Windows" ;;
  *) bad "workspace not visible" "$out" ;; esac
got=$(trim < "$WS/winquick-artifacts/out/result.txt" 2>/dev/null)
check "artifacts come back" "$got" "made-it"

# -- doctor and info ---------------------------------------------------------
echo "== doctor and info =="
out=$(wq doctor 2>&1); rc=$?
check "doctor exits 0 on a fresh install" "$rc" "0"
case "$out" in *FAIL*) bad "doctor reports a problem on a fresh install" "$out" ;;
  *) ok "doctor reports no problems" ;; esac
wq info >/dev/null 2>&1 && ok "info works" || bad "info" "non-zero exit"
wq capability list >/dev/null 2>&1 && ok "capability list works" \
  || bad "capability list" "non-zero exit"

# -- the base image is still pristine ---------------------------------------
#
# The whole disposability promise in one check: after every run above, the image
# they all booted from has to be the file setup wrote.
echo "== disposability =="
after=$(sha256 "$BASE" | cut -d' ' -f1)
if [ -f "$BEFORE_FILE" ]; then
  check "the base image is byte-identical after every run" "$after" "$(cat "$BEFORE_FILE")"
else
  bad "base image immutability" "no baseline hash was recorded"
fi
left=$(ls -A "$HOMEDIR/.winquick/run" 2>/dev/null | wc -l | tr -d ' ')
check "no run directories are left behind" "$left" "0"
q=$(ps -eo comm= 2>/dev/null | grep -c '^qemu-system' || true)
check "no qemu is left running" "$q" "0"

# -- stale images name the right rebuild ------------------------------------
#
# A serviced image is a copy of the runtime and carries the runtime's agent, so
# a stale runtime can only produce another stale serviced image. Getting the
# order wrong sent the user to rebuild the serviced one, which came out stale,
# printed the same message and sent them round again -- roughly ten minutes of
# DISM servicing per lap. Faked by editing metadata rather than by installing a
# real capability, which the rest of this test cannot afford. Runs last because
# it rebuilds the runtime.
echo "== stale images name the right rebuild =="
NETFX="$HOMEDIR/.winquick/images/netfx-$GUEST"
mkdir -p "$NETFX"
: > "$NETFX/base.qcow2"          # run() only has to see that the file is there
cp "${BASE%.qcow2}.json" "$NETFX/base.json"
stale() { sed 's/"agent_hash": *"[^"]*"/"agent_hash": "deadbeefdeadbeef"/' "$1" > "$1.new" && mv "$1.new" "$1"; }

stale "${BASE%.qcow2}.json"
stale "$NETFX/base.json"
case "$(wq run -- cmd /c ver 2>&1)" in
  *"winquick setup --force"*) ok "with both stale, run names the runtime first" ;;
  *) bad "run names the wrong rebuild" "$(wq run -- cmd /c ver 2>&1)" ;;
esac
t0=$(date +%s)
out=$(wq capability install dotnet-framework --force 2>&1); rc=$?
el=$(( $(date +%s)-t0 ))
[ "$rc" -ne 0 ] && ok "servicing from a stale runtime is refused" \
  || bad "servicing from a stale runtime succeeded" "it can only produce another stale image"
[ "$el" -lt 15 ] && ok "and refuses before doing the work (${el}s)" \
  || bad "refused only after servicing" "took ${el}s"
case "$out" in
  *"winquick setup --force"*) ok "and says to rebuild the runtime first" ;;
  *) bad "the refusal names no way out" "$out" ;;
esac

# Runtime current again, serviced image still behind. setup rewrites the
# runtime's metadata and leaves the serviced image alone, which is the state a
# half-finished upgrade is in.
out=$(wq setup --force --from "$IMAGE" 2>&1)
case "$out" in
  *"failed to start"*) bad "setup calls a healthy runtime broken" "$out" ;;
  *"capability install dotnet-framework --force"*)
    ok "setup reports the stale serviced image as a next step, not a failure" ;;
  *) bad "setup says nothing about the stale serviced image" "$out" ;;
esac
case "$(wq run -- cmd /c ver 2>&1)" in
  *"capability install dotnet-framework --force"*)
    ok "and run now names that capability, not setup" ;;
  *) bad "run names the wrong rebuild for a stale serviced image" "$(wq run -- cmd /c ver 2>&1)" ;;
esac

rm -rf "$NETFX"
wq run -- cmd /c ver >/dev/null 2>&1 && ok "runs work again once it is gone" \
  || bad "run still fails" "after the stale serviced image was removed"

# -- clean -------------------------------------------------------------------
echo "== clean =="
wq clean --all >/dev/null 2>&1 && ok "clean --all succeeds" || bad "clean --all" "non-zero exit"
[ ! -f "$BASE" ] && ok "clean --all removes the runtime" || bad "runtime survived clean --all" ""

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
