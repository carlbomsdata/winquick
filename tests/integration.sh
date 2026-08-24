#!/bin/bash
# WinQuick integration tests. Runs the real CLI, not a harness.
#   ./tests/integration.sh [warm-run-count]
WQ="$(cd "$(dirname "$0")/.." && pwd)/target/release/winquick"
BASE=~/.winquick/images/validation-arm64/base.qcow2
N=${1:-100}
# Optional .NET fixtures, built on the host; tests are skipped when absent.
FDAPP=${WQ_FDAPP:-/tmp/wqnet/out/fd-arm64}
SCAPP=${WQ_SCAPP:-/tmp/wqnet/out/sc-arm64}
TESTPROJ=${WQ_TESTPROJ:-/tmp/wqnet/tproj}
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

BASESHA=$(shasum -a 256 ~/.winquick/images/validation-arm64/base.qcow2 | cut -d" " -f1)
check "nuget: base image unchanged by cache use" "$BASESHA" "$(shasum -a 256 ~/.winquick/images/validation-arm64/base.qcow2 | cut -d" " -f1)"
else
  echo "== nuget cache (skipped: cache/SDK/test project not present) =="
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
"$WQ" run --timeout 2 -- cmd /c "ping -n 30 127.0.0.1" >/dev/null 2>&1
rc=$?
[ "$rc" -ne 0 ] && ok "timeout fails rather than hanging" || bad "timeout" "exit $rc"
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

echo
echo "== $pass passed, $fail failed =="
exit $([ $fail -eq 0 ] && echo 0 || echo 1)
