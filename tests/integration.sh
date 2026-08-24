#!/bin/bash
# WinQuick integration tests. Runs the real CLI, not a harness.
#   ./tests/integration.sh [warm-run-count]
WQ="$(cd "$(dirname "$0")/.." && pwd)/target/release/winquick"
BASE=~/.winquick/images/validation-arm64/base.qcow2
N=${1:-100}
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

"$WQ" run -- 'cmd /c echo S-OUT & echo S-ERR 1>&2 & exit 7' >/tmp/wq_o 2>/tmp/wq_e
check "exit code alongside output" "$?" "7"
grep -q S-OUT /tmp/wq_o && ! grep -q S-ERR /tmp/wq_o && ok "stdout holds only stdout" || bad "stdout separation" "$(cat /tmp/wq_o)"
grep -q S-ERR /tmp/wq_e && ! grep -q S-OUT /tmp/wq_e && ok "stderr holds only stderr" || bad "stderr separation" "$(cat /tmp/wq_e)"

"$WQ" run -- cmd /c nosuchcommand_zz >/dev/null 2>/tmp/wq_e
check "unknown command exits 1" "$?" "1"
grep -qi "not recognized" /tmp/wq_e && ok "unknown command explains itself on stderr" || bad "stderr message" "$(cat /tmp/wq_e)"

echo "== disposability =="
"$WQ" run -- 'cmd /c echo SENTINEL> C:\wqtest.txt' >/dev/null 2>&1
"$WQ" run -- 'cmd /c type C:\wqtest.txt' >/dev/null 2>&1
[ $? -ne 0 ] && ok "filesystem mutation does not survive" || bad "filesystem" "C:\\wqtest.txt persisted"

"$WQ" run -- 'cmd /c reg add HKLM\SOFTWARE\WQTEST /v X /t REG_SZ /d LEAK /f' >/dev/null 2>&1
"$WQ" run -- 'cmd /c reg query HKLM\SOFTWARE\WQTEST /v X' >/dev/null 2>&1
[ $? -ne 0 ] && ok "registry mutation does not survive" || bad "registry" "HKLM\\SOFTWARE\\WQTEST persisted"

# A leak would echo [1]; an unset variable expands to nothing, so [] is clean.
"$WQ" run -- 'cmd /c set WQLEAK=1' >/dev/null 2>&1
env_out=$("$WQ" run -- 'cmd /c echo [%WQLEAK%]' 2>/dev/null | tr -d '\n')
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
