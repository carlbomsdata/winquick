#!/bin/bash
# WinQuick integration tests. Runs the real CLI, not a harness.
#   ./tests/integration.sh [warm-run-count]
SCRIPTDIR="$(cd "$(dirname "$0")" && pwd)"
WQ="$SCRIPTDIR/../target/release/winquick"
# The image `run` actually boots: the serviced one when the .NET Framework
# capability is installed, the pristine one otherwise. Checking the wrong one
# would let a run write to the image it boots and still report "unchanged".
BASE=~/.winquick/images/netfx-arm64/base.qcow2
[ -f "$BASE" ] || BASE=~/.winquick/images/validation-arm64/base.qcow2
N=${1:-100}
# Optional .NET fixtures, built on the host; tests are skipped when absent.
FDAPP=${WQ_FDAPP:-/tmp/wqnet/out/fd-arm64}
SCAPP=${WQ_SCAPP:-/tmp/wqnet/out/sc-arm64}
TESTPROJ=${WQ_TESTPROJ:-/tmp/wqnet/tproj}
# Published WPF app for the desktop tests; skipped when unset.
WQ_UIAPP=${WQ_UIAPP:-}
pass=0; fail=0
ok()   { printf "  PASS  %s\n" "$1"; pass=$((pass+1)); }
bad()  { printf "  FAIL  %s -- %s\n" "$1" "$2"; fail=$((fail+1)); }
check(){ [ "$2" = "$3" ] && ok "$1" || bad "$1" "got [$2] want [$3]"; }

echo "== streams and exit codes =="
out=$("$WQ" run -- cmd /c ver 2>/dev/null)
case "$out" in *10.0.26100.8972*) ok "stdout carries the Windows build string";; *) bad "stdout" "$out";; esac

for c in 0 1 7 42 99 255; do
  "$WQ" run -- cmd /c exit $c >/dev/null 2>&1
  check "exit code $c propagates" "$?" "$c"
done

"$WQ" run -- cmd /c "echo S-OUT & echo S-ERR 1>&2 & exit 7" >/tmp/wq_o 2>/tmp/wq_e
check "exit code alongside output" "$?" "7"
grep -q S-OUT /tmp/wq_o && ! grep -q S-ERR /tmp/wq_o && ok "stdout holds only stdout" || bad "stdout separation" "$(cat /tmp/wq_o)"
grep -q S-ERR /tmp/wq_e && ! grep -q S-OUT /tmp/wq_e && ok "stderr holds only stderr" || bad "stderr separation" "$(cat /tmp/wq_e)"

"$WQ" run -- cmd /c nosuchcommand_zz >/dev/null 2>/tmp/wq_e
check "unknown command exits 1" "$?" "1"
grep -qi "not recognized" /tmp/wq_e && ok "unknown command explains itself on stderr" || bad "stderr message" "$(cat /tmp/wq_e)"

echo "== disposability =="
"$WQ" run -- cmd /c "echo SENTINEL> C:\wqtest.txt" >/dev/null 2>&1
"$WQ" run -- cmd /c "type C:\wqtest.txt" >/dev/null 2>&1
[ $? -ne 0 ] && ok "filesystem mutation does not survive" || bad "filesystem" "C:\\wqtest.txt persisted"

"$WQ" run -- cmd /c "reg add HKLM\SOFTWARE\WQTEST /v X /t REG_SZ /d LEAK /f" >/dev/null 2>&1
"$WQ" run -- cmd /c "reg query HKLM\SOFTWARE\WQTEST /v X" >/dev/null 2>&1
[ $? -ne 0 ] && ok "registry mutation does not survive" || bad "registry" "HKLM\\SOFTWARE\\WQTEST persisted"

# A leak would echo [1]; an unset variable expands to nothing, so [] is clean.
"$WQ" run -- cmd /c "set WQLEAK=1" >/dev/null 2>&1
env_out=$("$WQ" run -- cmd /c "echo [%WQLEAK%]" 2>/dev/null | tr -d '\n')
check "environment mutation does not survive" "$env_out" "[]"

echo "== base image immutability =="
before=$(shasum -a 256 "$BASE" | cut -d' ' -f1)
for i in 1 2 3; do "$WQ" run -- cmd /c ver >/dev/null 2>&1; done
after=$(shasum -a 256 "$BASE" | cut -d' ' -f1)
check "base.qcow2 unchanged" "$after" "$before"

echo "== ready-state invalidation and fallback =="
"$WQ" run -- cmd /c ver >/dev/null 2>&1   # ensure a state exists
v=$("$WQ" run --verbose --memory 1536 -- cmd /c ver 2>&1 >/dev/null)
case "$v" in *"guest memory changed"*) ok "changed RAM invalidates the ready state";; *) bad "invalidation on RAM" "$v";; esac
# The RAM check above leaves no state for the default topology, so re-establish
# one or this measures "no ready state yet" instead of vcpu invalidation.
"$WQ" run -- cmd /c ver >/dev/null 2>&1
v=$("$WQ" run --verbose --cpus 2 -- cmd /c ver 2>&1 >/dev/null)
case "$v" in *"vcpu count changed"*|*"device configuration changed"*) ok "changed vCPU count invalidates the ready state";; *) bad "invalidation on vCPU" "$v";; esac

"$WQ" run -- cmd /c ver >/dev/null 2>&1
printf 'not a real migration stream' > ~/.winquick/states/validation-arm64/ready.state
v=$("$WQ" run --verbose -- cmd /c ver 2>&1 >/tmp/wq_o); rc=$?
check "corrupt ready.state still returns the right answer" "$rc" "0"
grep -q 10.0.26100.8972 /tmp/wq_o && ok "corrupt ready.state falls back and produces output" || bad "corrupt fallback" "$(cat /tmp/wq_o)"
case "$v" in *"size does not match"*|*"warm path failed"*) ok "corrupt ready.state is detected";; *) bad "corruption detection" "$v";; esac

rm -rf ~/.winquick/states
"$WQ" run -- cmd /c ver >/dev/null 2>&1
check "missing ready state rebuilds automatically" "$?" "0"

if [ -f ~/.winquick/capabilities/powershell.img ]; then
echo "== powershell =="
v=$("$WQ" run -- pwsh -NoProfile -NonInteractive -Command '$PSVersionTable.PSVersion.ToString()' 2>/dev/null | tr -d '\r\n')
case "$v" in 7.*) ok "pwsh runs and reports version $v";; *) bad "pwsh version" "$v";; esac

o=$("$WQ" run -- pwsh -NoProfile -NonInteractive -Command "'WQ-' + (6*7)" 2>/dev/null | tr -d '\r\n')
check "pwsh evaluates expressions" "$o" "WQ-42"

"$WQ" run -- pwsh -NoProfile -NonInteractive -Command "exit 42" >/dev/null 2>&1
check "pwsh exit code propagates" "$?" "42"

"$WQ" run -- pwsh -NoProfile -NonInteractive -Command 'Write-Output OUT; [Console]::Error.WriteLine("ERR"); exit 3' >/tmp/wq_o 2>/tmp/wq_e
check "pwsh mixed streams exit code" "$?" "3"
check "pwsh stdout" "$(tr -d '\r\n' </tmp/wq_o)" "OUT"
check "pwsh stderr" "$(tr -d '\r\n' </tmp/wq_e)" "ERR"

"$WQ" run -- pwsh -NoProfile -NonInteractive -Command 'Write-Error "boom"' >/tmp/wq_o 2>/tmp/wq_e
check "pwsh Write-Error exits nonzero" "$?" "1"
[ -s /tmp/wq_e ] && [ ! -s /tmp/wq_o ] && ok "pwsh error goes to stderr only" || bad "pwsh error routing" "out=$(wc -c </tmp/wq_o) err=$(wc -c </tmp/wq_e)"

o=$("$WQ" run -- pwsh -NoProfile -NonInteractive -Command 'Write-Output "spaced and `"quoted`""' 2>/dev/null | tr -d '\r\n')
check "pwsh argument quoting survives" "$o" 'spaced and "quoted"' 
o=$("$WQ" run -- pwsh -NoProfile -NonInteractive -Command 'Write-Output "C:\Program Files"' 2>/dev/null | tr -d '\r\n')
check "pwsh path with space and backslash" "$o" 'C:\Program Files' 
else
  echo "== powershell (skipped: capability not installed) =="
fi

if [ -f ~/.winquick/capabilities/dotnet-runtime.img ] || [ -f ~/.winquick/capabilities/dotnet-sdk.img ]; then
echo "== dotnet =="
v=$("$WQ" run -- dotnet --list-runtimes 2>/dev/null | tr -d '\r')
case "$v" in *Microsoft.NETCore.App*) ok "dotnet runtime is visible in the guest";; *) bad "dotnet runtime" "$v";; esac

if [ -d "$FDAPP" ]; then
  o=$("$WQ" run -w "$FDAPP" -- dotnet hello.dll 2>/dev/null | tr -d '\r')
  case "$o" in *"is windows   : True"*) ok "framework-dependent app runs on the guest .NET runtime";; *) bad "fd app" "$o";; esac

  "$WQ" run -w "$FDAPP" -- dotnet hello.dll 42 >/dev/null 2>&1
  check "framework-dependent app exit code propagates" "$?" "42"

  "$WQ" run -w "$FDAPP" -- dotnet hello.dll >/tmp/wq_o 2>/tmp/wq_e
  grep -q "WQNET hello" /tmp/wq_o && grep -q "WQNET stderr line" /tmp/wq_e \
    && ok "dotnet stdout and stderr stay separate" || bad "dotnet streams" "out/err mixed"
fi

if [ -d "$SCAPP" ]; then
  o=$("$WQ" run -w "$SCAPP" -- cmd /c hello.exe 2>/dev/null | tr -d '\r')
  case "$o" in *".NET 10"*) ok "self-contained app runs (no guest .NET needed for it)";; *) bad "self-contained app" "$o";; esac
fi
else
  echo "== dotnet (skipped: capability not installed) =="
fi

echo "== workspace =="
WSTMP=$(mktemp -d)
echo "hello-from-host" > "$WSTMP/probe.txt"
o=$("$WQ" run -w "$WSTMP" -- cmd /c "type C:\workspace\probe.txt" 2>/dev/null | tr -d '\r\n')
check "host directory appears at C:\\workspace" "$o" "hello-from-host"
"$WQ" run -w "$WSTMP" -- cmd /c "echo guest-wrote-this > C:\workspace\fromguest.txt" >/dev/null 2>&1
[ ! -f "$WSTMP/fromguest.txt" ] && ok "guest writes do not mutate the host project" || bad "workspace writeback" "host file was created"
o=$("$WQ" run -w "$WSTMP" -- cmd /c "if exist C:\workspace\fromguest.txt (echo LEAKED) else (echo CLEAN)" 2>/dev/null | tr -d '\r\n')
check "workspace is disposable between runs" "$o" "CLEAN"
rm -rf "$WSTMP"

echo "== artifacts =="
ATMP=$(mktemp -d); pushd "$ATMP" >/dev/null
mkdir -p "src/deep dir"
echo "staged-content" > "src/deep dir/file with space.txt"
echo "top" > src/top.txt

"$WQ" run -w "$ATMP/src" -a "deep dir/**" -- cmd /c "echo x" >/dev/null 2>&1
check "artifact: nested dir with spaces" "$(cat "winquick-artifacts/deep dir/file with space.txt" 2>/dev/null)" "staged-content"

rm -rf winquick-artifacts
"$WQ" run -a "one/**" -a "two/**" -- cmd /c "mkdir one & mkdir two & echo 1> one\a.txt & echo 2> two\b.txt" >/dev/null 2>&1
n=$(find winquick-artifacts -type f 2>/dev/null | wc -l | tr -d ' ')
check "artifact: multiple patterns" "$n" "2"

rm -rf winquick-artifacts
"$WQ" run -a "report.txt" -- cmd /c "echo the-report> report.txt" >/dev/null 2>&1
# cmd's `echo x> f` writes the space before the redirect, so trim it.
check "artifact: single named file" "$(cat winquick-artifacts/report.txt 2>/dev/null | tr -d ' \r\n')" "the-report"

rm -rf winquick-artifacts
out=$("$WQ" run -a "nothing/**" -- cmd /c "echo x" 2>&1)
case "$out" in *"no files matched"*) ok "artifact: no matches reported cleanly";; *) bad "artifact no-match" "$out";; esac

rm -rf winquick-artifacts
"$WQ" run -a "logs/**" -- cmd /c "mkdir logs & echo failure-log> logs\err.txt & exit 42" >/dev/null 2>&1
check "artifact: exit code survives extraction" "$?" "42"
check "artifact: retrieved after a failing command" "$(cat winquick-artifacts/logs/err.txt 2>/dev/null | tr -d ' \r\n')" "failure-log"

before=$(find "$ATMP/src" -type f | wc -l | tr -d ' ')
"$WQ" run -w "$ATMP/src" -a "top.txt" -- cmd /c "echo mutated> top.txt & echo new> extra.txt" >/dev/null 2>&1
after=$(find "$ATMP/src" -type f | wc -l | tr -d ' ')
check "artifact: host source tree untouched" "$after" "$before"
check "artifact: host file not rewritten" "$(cat "$ATMP/src/top.txt")" "top"

rm -rf winquick-artifacts
mkdir -p big && dd if=/dev/urandom of=big/blob.bin bs=1m count=32 2>/dev/null
"$WQ" run -w "$ATMP/big" -a "**" -- cmd /c "echo x" >/dev/null 2>&1
sz=$(stat -f%z winquick-artifacts/blob.bin 2>/dev/null || echo 0)
check "artifact: 32 MiB file exact" "$sz" "$(stat -f%z big/blob.bin)"
popd >/dev/null; rm -rf "$ATMP"

if [ -f ~/.winquick/capabilities/dotnet-sdk.img ] && [ -d "$TESTPROJ" ]; then
echo "== nuget cache =="
# Populate the cache for this project first: that is the documented workflow,
# and it exercises `cache sync` as part of the suite.
"$WQ" cache sync "$TESTPROJ" >/dev/null 2>&1
check "cache sync succeeds" "$?" "0"
CACHE=~/.winquick/capabilities/nuget-cache.img
before=$(shasum -a 256 "$CACHE" | cut -d" " -f1)
rm -rf "$TESTPROJ/obj" "$TESTPROJ/bin"
out=$("$WQ" run -w "$TESTPROJ" -- dotnet test --nologo 2>&1)
rc=$?
check "nuget: cached dotnet test succeeds offline" "$rc" "0"
case "$out" in *"Passed!"*) ok "nuget: tests actually ran and passed";; *) bad "nuget test result" "$(echo "$out" | tail -3)";; esac
case "$out" in *NU1301*) bad "nuget: hit the network" "NU1301 in output";; *) ok "nuget: no network was needed";; esac

"$WQ" run -w "$TESTPROJ" -- cmd /c "echo pwned> %NUGET_PACKAGES%\pwned.txt & echo done" >/dev/null 2>&1
after=$(shasum -a 256 "$CACHE" | cut -d" " -f1)
check "nuget: guest cannot mutate the canonical cache" "$after" "$before"
o=$("$WQ" run -- cmd /c "if exist %NUGET_PACKAGES%\pwned.txt (echo LEAKED) else (echo CLEAN)" 2>/dev/null | tr -d "\r\n")
check "nuget: guest writes do not persist into later runs" "$o" "CLEAN"

BASESHA=$(shasum -a 256 "$BASE" | cut -d" " -f1)
check "nuget: base image unchanged by cache use" "$BASESHA" "$(shasum -a 256 "$BASE" | cut -d" " -f1)"
else
  echo "== nuget cache (skipped: cache/SDK/test project not present) =="
fi

# A classic, non-SDK .NET Framework project: the shape that found most of what
# `dotnet-framework` exists for. Each check below stands for a failure that was
# once discovered by a real build rather than by this suite.
FIXTURE="$SCRIPTDIR/../experiments/dotnet-matrix/ClassicNetFxX64"
REFPKG=~/.winquick/caches/nuget/microsoft.netframework.referenceassemblies.net472/1.0.3
if [ -f ~/.winquick/images/netfx-arm64/base.qcow2 ] && [ -d "$REFPKG" ] && [ -d "$FIXTURE" ]; then
echo "== classic .NET Framework project =="
NTMP=$(mktemp -d)
cp -R "$FIXTURE/." "$NTMP/"
MSB='C:\Windows\Microsoft.NET\Framework64\v4.0.30319\MSBuild.exe'
REFROOT='/p:TargetFrameworkRootPath=%NUGET_PACKAGES%\microsoft.netframework.referenceassemblies.net472\1.0.3\build'
rm -rf winquick-artifacts
# One run, because the workspace is disposable: what is built has to be run
# before the guest is thrown away. `dotnet build` cannot drive this project at
# all; the Framework MSBuild the capability brings can.
out=$("$WQ" run -w "$NTMP" -a "bin/Release/**" --timeout 900 \
  -- cmd /c "$MSB ClassicNetFxX64.csproj /p:Configuration=Release $REFROOT /nologo /v:q && bin\\Release\\ClassicNetFxX64.exe" 2>&1)
check "netfx: classic MSBuild builds a non-SDK project and runs it" "$?" "0"
EXE=winquick-artifacts/bin/Release/ClassicNetFxX64.exe
[ -f "$EXE" ] && ok "netfx: the build produced an executable" || bad "netfx: no executable" "$out"
# PE machine 0x8664. An ARM64 guest produced an x64 binary.
mach=$(python3 "$SCRIPTDIR/peinfo.py" "$EXE" 2>/dev/null | awk '/machine/{print $2}')
check "netfx: an ARM64 guest produced an x64 binary" "$mach" "x64"
tfm=$(python3 "$SCRIPTDIR/peinfo.py" "$EXE" 2>/dev/null | awk '/targetFramework/{print $2}')
check "netfx: the target framework is stamped" "$tfm" ".NETFramework,Version=v4.7.2"
# The other half: a guest with no Framework builds this correctly and then
# dies with 0xC0000135, so only running it proves the runtime is there.
case "$out" in *"ptr=8"*) ok "netfx: the x64 binary ran as a 64-bit process";; *) bad "netfx: pointer size" "$out";; esac
case "$out" in *"bitmap=8x8 red=255"*) ok "netfx: System.Drawing and GDI+ work";; *) bad "netfx: GDI+" "$out";; esac
case "$out" in *"baml=yes"*) ok "netfx: XAML was markup-compiled into the assembly";; *) bad "netfx: markup compile" "$out";; esac
case "$out" in *"ndp-version=4."*) ok "netfx: the guest reports an installed .NET Framework 4.x";; *) bad "netfx: NDP version" "$out";; esac
rm -rf "$NTMP" winquick-artifacts
else
  echo "== classic .NET Framework project (skipped: dotnet-framework capability or net472 reference assemblies not present) =="
fi

echo "== lifecycle =="
"$WQ" --version | grep -q "winquick " && ok "--version reports a version" || bad "--version" "$("$WQ" --version)"
"$WQ" --help | grep -q "winquick run -- cmd /c ver" && ok "--help shows examples" || bad "--help" "no examples"
"$WQ" doctor >/dev/null 2>&1
check "doctor reports a healthy install" "$?" "0"
"$WQ" info | grep -q "runtime" && ok "info reports the runtime" || bad "info" "no runtime line"
out=$("$WQ" clean --dry-run 2>&1)
case "$out" in *total*) ok "clean --dry-run reports without removing";; *) bad "clean --dry-run" "$out";; esac
[ -f ~/.winquick/images/validation-arm64/base.qcow2 ] && ok "clean --dry-run removed nothing" || bad "clean --dry-run" "runtime gone"

echo "== interrupt and timeout =="
before_q=$(pgrep -f qemu-system-aarch64 | wc -l | tr -d " ")
# Warm this run up first, so the check below is about the timeout and not about
# there being no prepared guest yet.
"$WQ" run -- cmd /c "exit 0" >/dev/null 2>&1
t0=$(date +%s)
out=$("$WQ" run --timeout 2 -- cmd /c "ping -n 30 127.0.0.1" 2>&1)
rc=$?
el=$(( $(date +%s)-t0 ))
[ "$rc" -ne 0 ] && ok "timeout fails rather than hanging" || bad "timeout" "exit $rc"
case "$out" in *"--timeout"*) ok "a timeout says which flag to change";; *) bad "timeout message" "$out";; esac
# A command that ran out of time says nothing about the guest that ran it.
# Falling back used to re-run the whole command cold, once per prepare attempt.
[ "$el" -lt 60 ] && ok "a timeout is not retried on a fresh guest" || bad "timeout retried" "took ${el}s for a 2 s timeout"
[ -f ~/.winquick/states/validation-arm64/ready.json ] && ok "a timeout keeps the prepared guest" || bad "prepared guest discarded by a timeout" "no ready.json"
sleep 1
after_q=$(pgrep -f qemu-system-aarch64 | wc -l | tr -d " ")
check "timeout leaves no qemu behind" "$after_q" "$before_q"
check "timeout leaves no run directories" "$(ls -A ~/.winquick/run 2>/dev/null | wc -l | tr -d " ")" "0"

"$WQ" run --timeout 120 -- pwsh -NoProfile -Command "Start-Sleep -Seconds 60" >/dev/null 2>&1 &
IPID=$!
sleep 14
kill -INT $IPID 2>/dev/null
wait $IPID 2>/dev/null; irc=$?
check "Ctrl-C exits 130" "$irc" "130"
sleep 2
check "Ctrl-C leaves no qemu behind" "$(pgrep -f qemu-system-aarch64 | wc -l | tr -d " ")" "0"
check "Ctrl-C leaves no run directories" "$(ls -A ~/.winquick/run 2>/dev/null | wc -l | tr -d " ")" "0"

echo "== concurrency =="
for i in 1 2 3 4; do ( "$WQ" run -- cmd /c "echo conc-$i" > /tmp/wq_c$i.out 2>&1 ) & done
wait
cok=0
for i in 1 2 3 4; do grep -q "conc-$i" /tmp/wq_c$i.out && cok=$((cok+1)); done
check "four concurrent runs all correct" "$cok" "4"
check "concurrency leaves no qemu behind" "$(pgrep -f qemu-system-aarch64 | wc -l | tr -d " ")" "0"

echo "== artifact safety =="
ATMP2=$(mktemp -d); pushd "$ATMP2" >/dev/null
mkdir -p src && echo hi > src/a.txt
"$WQ" run -w "$ATMP2/src" -a "../../../../../../tmp/wq-escape.txt" -- cmd /c "echo x" >/dev/null 2>&1
[ ! -f /tmp/wq-escape.txt ] && ok "artifact pattern cannot escape the workspace" || bad "artifact escape" "/tmp/wq-escape.txt was created"
mkdir -p existing && echo keep > existing/keep.txt
out=$("$WQ" run -w "$ATMP2/src" -a "a.txt" --artifacts-dir "$ATMP2/existing" -- cmd /c "echo x" 2>&1)
case "$out" in *"already exists and is not empty"*) ok "refuses to write into a non-empty artifacts dir";; *) bad "artifact overwrite guard" "$out";; esac
check "existing artifact dir untouched" "$(cat "$ATMP2/existing/keep.txt")" "keep"
popd >/dev/null; rm -rf "$ATMP2"

echo "== $N consecutive warm runs =="
python3 - "$WQ" "$N" <<'PY'
import subprocess, sys, time, statistics
wq, n = sys.argv[1], int(sys.argv[2])
ts=[]; failures=0
for i in range(n):
    t=time.time()
    p=subprocess.run([wq,"run","--","cmd","/c","ver"],capture_output=True)
    dt=time.time()-t
    if p.returncode!=0 or b"10.0.26100.8972" not in p.stdout: failures+=1
    ts.append(dt)
ts.sort()
def q(p): return ts[min(len(ts)-1,int(len(ts)*p))]
print(f"  runs={n} failures={failures}")
print(f"  min={ts[0]*1000:.0f}ms p50={statistics.median(ts)*1000:.0f}ms mean={statistics.mean(ts)*1000:.0f}ms "
      f"p95={q(0.95)*1000:.0f}ms p99={q(0.99)*1000:.0f}ms max={ts[-1]*1000:.0f}ms")
sys.exit(1 if failures else 0)
PY
[ $? -eq 0 ] && ok "$N consecutive warm runs, zero failures" || bad "warm run reliability" "see above"

echo "== v0.2.1 command quoting (WQ-EXT-01) =="
q(){ out=$("$WQ" run -- "${@:2}" 2>/dev/null | tr -d '\r' | tail -1); check "$1" "$out" "$2"; }
out=$("$WQ" run -- cmd /c 'echo say "hi"' 2>/dev/null | tr -d '\r' | tail -1)
check "cmd keeps the user's quotes" "$out" 'say "hi"'
"$WQ" run -- cmd /c 'type "C:\Windows\System32\drivers\etc\hosts"' >/dev/null 2>&1
check "cmd accepts a quoted path" "$?" "0"
out=$("$WQ" run -- cmd /c 'echo A & echo B' 2>/dev/null | tr -d '\r' | tail -1)
check "cmd operators still work" "$out" "B"
out=$("$WQ" run -- cmd /c 'echo åäö-日本語' 2>/dev/null | tr -d '\r' | tail -1)
check "unicode through cmd" "$out" "åäö-日本語"
if [ -f ~/.winquick/capabilities/powershell.img ]; then
  out=$("$WQ" run -- pwsh -NoProfile -Command 'Write-Output "quoted string"' 2>/dev/null | tr -d '\r' | tail -1)
  check "pwsh quoting is not broken by the cmd fix" "$out" "quoted string"

  # An argument holding a quote *and* a cmd metacharacter. cmd counts quotes and
  # does not understand the C runtime's \", so after the escaped quote it
  # believed it was outside quotes and treated & as an operator, splitting the
  # line: the program never ran and the user saw `'b\""' is not recognized`.
  out=$("$WQ" run -- pwsh -NoProfile -Command 'Write-Output "a&b"' 2>/dev/null | tr -d '\r' | tail -1)
  check "a metacharacter after a quote survives cmd" "$out" "a&b"
  for m in '|' '<' '>'; do
    out=$("$WQ" run -- pwsh -NoProfile -Command "Write-Output \"x${m}y\"" 2>/dev/null | tr -d '\r' | tail -1)
    check "metacharacter $m after a quote survives cmd" "$out" "x${m}y"
  done
fi

echo "== v0.2.1 non-BMP filenames (WQ-EXT-02) =="
NB=/tmp/wq_nonbmp; rm -rf $NB; mkdir -p "$NB/deep"; echo x > "$NB/ok-åäö.txt"; echo x > "$NB/deep/bad-🙂.txt"
"$WQ" run -w "$NB" -- cmd /c "echo ." >/tmp/wq_e 2>&1 || true
grep -q "deep/bad-" /tmp/wq_e \
  && ok "an unrepresentable filename is named in the error" \
  || bad "non-BMP diagnostic" "$(head -2 /tmp/wq_e)"
rm -f "$NB/deep/bad-🙂.txt"
"$WQ" run -w "$NB" -- cmd /c "dir /b C:\\workspace" >/dev/null 2>&1
check "a tree of representable names still works" "$?" "0"

echo "== v0.2.1 artifact globs (WQ-EXT / limit 7) =="
GW=/tmp/wq_glob; rm -rf $GW; mkdir -p "$GW/bin/Release/net10.0" "$GW/logs"
echo a > "$GW/root.dll"; echo a > "$GW/bin/one.dll"; echo a > "$GW/bin/Release/net10.0/deep.dll"
echo a > "$GW/bin/Release/app.exe"; echo a > "$GW/logs/a.txt"; echo a > "$GW/foo1.txt"
gl(){ rm -rf /tmp/wq_glob_out
  "$WQ" run -w "$GW" -a "$1" --artifacts-dir /tmp/wq_glob_out -- cmd /c "echo ." >/dev/null 2>&1
  n=$(find /tmp/wq_glob_out -type f 2>/dev/null | wc -l | tr -d ' '); check "glob $1" "$n" "$2"; }
gl "**/*.dll" 3
gl "*.dll" 1
gl "bin/**/*.exe" 1
gl "logs/*.txt" 1
gl "foo?.txt" 1
gl "bin/Release/**" 2
for bad in "../escape" "bin/../../etc"; do
  "$WQ" run -w "$GW" -a "$bad" --artifacts-dir /tmp/wq_glob_out2 -- cmd /c "echo ." >/tmp/wq_e 2>&1
  rc=$?
  grep -q "must not contain" /tmp/wq_e && [ "$rc" != "0" ] \
    && ok "artifact traversal refused: $bad" \
    || bad "traversal" "$bad was not refused"
done

echo "== desktop capability =="
DESKBASE=~/.winquick/images/desktop-arm64/base.qcow2
# The CLI surface must be right whether or not the capability is installed.
"$WQ" desktop --help 2>&1 | grep -q "automation-id" && ok "desktop help documents element selectors" || bad "desktop help" "no selector guidance"
"$WQ" ui-test --help 2>&1 | grep -q "expect" && ok "ui-test help documents the script format" || bad "ui-test help" "no script guidance"
"$WQ" capability list 2>&1 | grep -q "^desktop" && ok "desktop appears in the capability list" || bad "capability list" "desktop missing"

# A verb's help is a question about the CLI, so it must answer without a
# session and without the capability installed.
for v in click toggle key mouse tree; do
  "$WQ" desktop "$v" --help >/tmp/wq_o 2>&1
  rc=$?
  grep -q "Usage: winquick desktop $v" /tmp/wq_o && [ "$rc" = "0" ] \
    && ok "desktop $v --help works without a session" \
    || bad "desktop $v --help" "rc=$rc $(head -1 /tmp/wq_o)"
done
grep -q -- "--automation-id" /tmp/wq_o && ok "verb help explains the selector" \
  || bad "verb help" "no selector guidance"

"$WQ" desktop click --automation-id X >/tmp/wq_e 2>&1
rc=$?
if [ ! -f "$DESKBASE" ]; then
  check "desktop verb without the capability fails" "$rc" "1"
else
  "$WQ" desktop frobnicate >/tmp/wq_e 2>&1
  rc=$?
  grep -q "unknown desktop command" /tmp/wq_e && [ "$rc" != "0" ] \
    && ok "an unknown verb is a syntax error, not a session error" \
    || bad "unknown verb" "$(head -1 /tmp/wq_e)"
  "$WQ" desktop status >/dev/null 2>&1
  [ $? -ne 0 ] && ok "desktop status reports no session" || ok "desktop status reports a running session"
fi

echo "== mcp =="
# The protocol suite drives the real binary over stdin/stdout. It is its own
# script because an MCP client is the only honest way to test an MCP server.
if command -v python3 >/dev/null 2>&1; then
  if python3 "$(dirname "$0")/mcp.py" "$WQ" > /tmp/wq_mcp.log 2>&1; then
    ok "mcp protocol suite ($(grep -c '^  PASS' /tmp/wq_mcp.log) checks)"
  else
    bad "mcp protocol suite" "$(grep '^  FAIL' /tmp/wq_mcp.log | head -3 | tr '\n' ' ')"
  fi
else
  echo "  (skipping the mcp suite: python3 not available)"
fi

"$WQ" mcp --help >/tmp/wq_o 2>&1
grep -q "JSON-RPC" /tmp/wq_o && ok "mcp help explains the transport" || bad "mcp help" "no transport description"
grep -q "claude mcp add winquick" /tmp/wq_o && ok "mcp help shows the Claude Code command" || bad "mcp help" "no registration command"

# Regressions for the UX defects the desktop dogfood turned up.
if [ -f "$DESKBASE" ] && [ -d "$WQ_UIAPP" ]; then
  "$WQ" desktop stop >/dev/null 2>&1
  # A desktop session must not write to the installed capability volumes; it
  # gets clones, exactly as `winquick run` does.
  capsum_before=$(shasum -a 256 ~/.winquick/capabilities/*.img | shasum -a 256)
  "$WQ" desktop start --app "$WQ_UIAPP" >/tmp/wq_sess.log 2>&1
  check "desktop session starts" "$?" "0"

  "$WQ" desktop launch 'app\DeviceConfig.exe' >/dev/null 2>&1 || "$WQ" desktop launch 'app\DemoApp.exe' >/dev/null 2>&1
  "$WQ" desktop wait-window --title "Device Configuration" --timeout 60000 >/dev/null 2>&1 \
    || "$WQ" desktop wait-window --title "WinQuick Demo" --timeout 60000 >/dev/null 2>&1

  # An option the verb does not understand must be refused. Silently ignoring
  # it turned a mistyped selector into a confident answer about the wrong
  # element.
  "$WQ" desktop get --automation-id StatusText --class-name Nope >/tmp/wq_e 2>&1
  grep -q "unknown option --class-name" /tmp/wq_e \
    && ok "unknown options are rejected, not ignored" \
    || bad "unknown option" "$(head -2 /tmp/wq_e)"

  # `tree` used to ignore an element selector and dump the whole window.
  "$WQ" desktop tree --automation-id StatusText --depth 1 >/tmp/wq_o 2>&1
  python3 - /tmp/wq_o <<'PYTREE'
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if d["tree"].get("automationId") == "StatusText" else 1)
PYTREE
  check "tree scopes to an element selector" "$?" "0"

  # A combo box exposes no value pattern; its selection has to come from
  # somewhere or "which item is chosen" is unanswerable. Whichever demo app is
  # under test, one of these two combo boxes exists.
  combo_ok=no
  for combo in DeptCombo ModeCombo; do
    # stdout only: the JSON goes there, the human-readable error to stderr, and
    # merging the two gives something that is not JSON at all.
    "$WQ" desktop get --automation-id "$combo" >/tmp/wq_o 2>/dev/null || continue
    if python3 -c "import json,sys;v=json.load(open('/tmp/wq_o'))['element'].get('value');sys.exit(0 if v else 1)"; then
      combo_ok=yes; break
    fi
  done
  check "combo box reports its selection as a value" "$combo_ok" "yes"

  # WQ-EXT-05/06/07: syntax errors before state, and a usable disabled-element message.
  "$WQ" desktop get --id SampleBox >/tmp/wq_o 2>&1
  grep -q "unknown option --id" /tmp/wq_o \
    && ok "an unknown option names itself" || bad "unknown option" "$(head -2 /tmp/wq_o)"
  "$WQ" desktop screenshot /tmp/wq_hw.png --hwnd 999999 >/tmp/wq_o 2>&1
  grep -qv "unexpected argument" /tmp/wq_o \
    && ok "screenshot accepts --hwnd" || bad "screenshot --hwnd" "not accepted"

  "$WQ" desktop stop >/dev/null 2>&1
  capsum_after=$(shasum -a 256 ~/.winquick/capabilities/*.img | shasum -a 256)
  check "a desktop session leaves capability volumes untouched" "$capsum_before" "$capsum_after"
fi

# The prepared desktop state is what makes a session start in under half a
# second instead of ten. It has to be created, reused, and thrown away the
# moment it stops describing the machine.
if [ -f "$DESKBASE" ] && [ -d "$WQ_UIAPP" ]; then
  DSTATE=~/.winquick/states/desktop-arm64
  "$WQ" desktop stop >/dev/null 2>&1

  "$WQ" desktop start --app "$WQ_UIAPP" >/dev/null 2>&1
  check "a desktop session prepares its state" "$([ -f $DSTATE/ready.json ] && echo yes)" "yes"
  "$WQ" desktop stop >/dev/null 2>&1

  # Reuse: the second start must not rebuild, which is visible as the absence
  # of the one-off preparation notice.
  "$WQ" desktop start --app "$WQ_UIAPP" >/tmp/wq_o 2>&1
  grep -q "Preparing the desktop" /tmp/wq_o \
    && bad "prepared state reuse" "rebuilt when it should have been reused" \
    || ok "a prepared state is reused rather than rebuilt"
  "$WQ" desktop stop >/dev/null 2>&1

  # Corruption must be survivable: a truncated fingerprint is discarded and
  # rebuilt, not run.
  echo "not json" > $DSTATE/ready.json
  "$WQ" desktop start --app "$WQ_UIAPP" >/tmp/wq_o 2>&1
  check "a corrupt prepared state is rebuilt, not run" "$?" "0"
  "$WQ" desktop stop >/dev/null 2>&1

  # A missing piece is the same story: all of it restores together or none.
  rm -f $DSTATE/ready-app.img
  "$WQ" desktop start --app "$WQ_UIAPP" >/tmp/wq_o 2>&1
  check "an incomplete prepared state is rebuilt" "$?" "0"
  "$WQ" desktop stop >/dev/null 2>&1

  # Changing the machine has to invalidate it. A different vCPU count is a
  # different machine, and migration state is only valid against its own.
  "$WQ" desktop start --app "$WQ_UIAPP" --cpus 1 >/tmp/wq_o 2>&1
  grep -q "Preparing the desktop" /tmp/wq_o \
    && ok "a changed vcpu count invalidates the prepared state" \
    || bad "topology invalidation" "reused a state built for another machine"
  "$WQ" desktop stop >/dev/null 2>&1
  # Put the default-shaped state back so later checks are not slowed down.
  "$WQ" desktop start --app "$WQ_UIAPP" >/dev/null 2>&1
  "$WQ" desktop stop >/dev/null 2>&1

  # Two sessions in a row must not share anything the guest wrote.
  "$WQ" desktop start --app "$WQ_UIAPP" >/dev/null 2>&1
  "$WQ" desktop launch 'app\DeviceConfig.exe' >/dev/null 2>&1 || "$WQ" desktop launch 'app\DemoApp.exe' >/dev/null 2>&1
  "$WQ" desktop wait-window --title "Device Configuration" --timeout 60000 >/dev/null 2>&1 \
    || "$WQ" desktop wait-window --title "WinQuick Demo" --timeout 60000 >/dev/null 2>&1
  "$WQ" desktop type --automation-id DeviceNameBox --text "LEAKED" >/dev/null 2>&1 \
    || "$WQ" desktop type --automation-id NameBox --text "LEAKED" >/dev/null 2>&1
  statesum_before=$(shasum -a 256 $DSTATE/ready-disk.qcow2 $DSTATE/ready.state | shasum -a 256)
  "$WQ" desktop stop >/dev/null 2>&1

  "$WQ" desktop start --app "$WQ_UIAPP" >/dev/null 2>&1
  "$WQ" desktop launch 'app\DeviceConfig.exe' >/dev/null 2>&1 || "$WQ" desktop launch 'app\DemoApp.exe' >/dev/null 2>&1
  "$WQ" desktop wait-window --title "Device Configuration" --timeout 60000 >/dev/null 2>&1 \
    || "$WQ" desktop wait-window --title "WinQuick Demo" --timeout 60000 >/dev/null 2>&1
  fresh=$("$WQ" desktop get --automation-id DeviceNameBox 2>/dev/null || "$WQ" desktop get --automation-id NameBox 2>/dev/null)
  echo "$fresh" | grep -q LEAKED \
    && bad "session disposability" "the previous session's typing survived" \
    || ok "a restored session starts clean"
  "$WQ" desktop stop >/dev/null 2>&1
  statesum_after=$(shasum -a 256 $DSTATE/ready-disk.qcow2 $DSTATE/ready.state | shasum -a 256)
  check "sessions never write to the prepared state" "$statesum_before" "$statesum_after"
fi

# demo.uitest addresses the WpfDemo window by title, so it only means anything
# against a published WpfDemo.
if [ -f "$DESKBASE" ] && [ -f "$WQ_UIAPP/DemoApp.exe" ]; then
  echo "  (running the full UI test against $WQ_UIAPP)"
  "$WQ" desktop stop >/dev/null 2>&1
  rm -rf /tmp/wq_uitest
  "$WQ" ui-test "$WQ_UIAPP" --script "$SCRIPTDIR/../examples/WpfDemo/demo.uitest" --out /tmp/wq_uitest >/tmp/wq_ui.log 2>&1
  check "ui-test drives the demo application" "$?" "0"
  grep -q "steps passed" /tmp/wq_ui.log && ok "ui-test reports every step" || bad "ui-test output" "$(tail -3 /tmp/wq_ui.log)"

  # A screenshot has to be a real PNG of a rendered desktop, not a blank buffer.
  shot=/tmp/wq_uitest/02-after.png
  if [ -f "$shot" ]; then
    # The signature starts with a high byte, so compare bytes rather than grep.
    if [ "$(head -c 8 "$shot" | od -An -tx1 | tr -d ' \n')" = "89504e470d0a1a0a" ]; then
      ok "screenshot is a real PNG"
    else
      bad "screenshot format" "not a PNG signature"
    fi
    python3 - "$shot" <<'PYSHOT'
import sys, zlib, struct
d = open(sys.argv[1], 'rb').read()
w = h = None; idat = b''
i = 8
while i < len(d):
    ln = struct.unpack('>I', d[i:i+4])[0]; typ = d[i+4:i+8]
    if typ == b'IHDR': w, h = struct.unpack('>II', d[i+8:i+16])
    elif typ == b'IDAT': idat += d[i+8:i+8+ln]
    i += 12 + ln
raw = zlib.decompress(idat)
stride = w * 3 + 1
nonblack = sum(1 for y in range(h) for x in range(w)
               if any(raw[y*stride + 1 + x*3 + c] > 16 for c in range(3)))
frac = nonblack / (w * h)
print(f"  {w}x{h}, {frac*100:.1f}% non-black")
sys.exit(0 if frac > 0.5 else 1)
PYSHOT
    check "screenshot shows a rendered window, not a blank framebuffer" "$?" "0"
  else
    bad "screenshot" "no 02-after.png produced"
  fi
  "$WQ" desktop stop >/dev/null 2>&1
else
  echo "  (skipping the live UI test: set WQ_UIAPP to a published examples/WpfDemo)"
fi

echo
echo "== $pass passed, $fail failed =="
exit $([ $fail -eq 0 ] && echo 0 || echo 1)
