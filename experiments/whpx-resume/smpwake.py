#!/usr/bin/env python3
"""Reproduce the SMP restore freeze, then try to wake it.

    python3 smpwake.py <qemu> <statedir> <base.qcow2> <workdir> <smp>

Cold boot, settle, migrate, restore into a fresh process, prove whether the
guest executes, and if it does not, inject an NMI and look again. An NMI needs
no APIC, no timer and no device: if the guest resumes on one, what is missing
is a wake-up rather than processor state.
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips
from cycle import args_for

def moving(q, label, samples=5, gap=0.4):
    seen = []
    for _ in range(samples):
        seen.append(tuple(rips(q)))
        time.sleep(gap)
    n = len({s for s in seen})
    print(f"  {label}: {n} distinct RIP sets -> {'EXECUTING' if n > 1 else 'FROZEN'}")
    for s in seen[:3]:
        print(f"      {list(s)}")
    return n > 1

def diag(p, tail=4):
    err = p.stderr.read().decode("utf-8", "replace")
    lines = err.splitlines()
    for l in [x for x in lines if x.startswith(("whpx-vp","whpx-lapicraw"))]:
        print("    " + l[:230])
    for l in [x for x in lines if x.startswith(("whpx-diag", "whpx-msi", "whpx-apic", "whpx-cancel"))][-tail:]:
        print("    " + l[:190])
    return err

def main():
    qemu, statedir, base, work, smp = sys.argv[1:6]
    smp = int(smp)
    os.makedirs(work, exist_ok=True)
    overlay = os.path.join(work, "root.qcow2")
    if os.path.exists(overlay):
        os.remove(overlay)
    qi = shutil.which("qemu-img") or "/c/Program Files/qemu/qemu-img.exe"
    subprocess.run([qi, "create", "-q", "-f", "qcow2", "-F", "qcow2",
                    "-b", win(base), win(overlay)], check=True)
    for s0, d0 in (("ready-vars.fd", "uefi-vars.fd"), ("ready-mailbox.img", "mailbox.img"),
                   ("ready-workspace.img", "workspace.img"), ("ready-artifacts.img", "artifacts.img")):
        shutil.copyfile(os.path.join(statedir, s0), os.path.join(work, d0))
    state = os.path.join(work, f"smp{smp}.state")
    if os.path.exists(state):
        os.remove(state)

    print(f"=== smp={smp}: cold boot ===")
    p = subprocess.Popen(args_for(qemu, work, 55890, smp),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55890)
        end = time.time() + 150
        while time.time() < end:
            r = rips(q)
            if r and r[0].startswith("fffff8"):
                break
            time.sleep(0.5)
        print("  kernel up; settling 25s so nothing is mid-flight at the freeze")
        time.sleep(25)
        moving(q, "cold guest", samples=3, gap=0.3)
        q.cmd("stop")
        q.cmd("migrate-set-parameters", **{"downtime-limit": 600000, "max-bandwidth": 0})
        q.cmd("migrate", uri=f"file:{win(state)}")
        end = time.time() + 150
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled") or time.time() > end:
                raise SystemExit(f"migrate {st}")
            time.sleep(0.05)
        print(f"  migrated {os.path.getsize(state):,} bytes")
        q.cmd("quit")
    finally:
        try:
            p.wait(timeout=30)
        except Exception:
            p.kill()
        print("  --- SOURCE per-VP state ---")
        diag(p, tail=3)

    print(f"=== smp={smp}: restore into a fresh process ===")
    p = subprocess.Popen(args_for(qemu, work, 55891, smp, incoming=state),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55891)
        end = time.time() + 120
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled") or time.time() > end:
                raise SystemExit(f"incoming {st}")
            time.sleep(0.02)
        q.cmd("cont")
        time.sleep(2.0)
        ran = moving(q, "after cont")
        if not ran:
            print("  injecting an NMI on every processor...")
            q.cmd("inject-nmi")
            time.sleep(1.0)
            woke = moving(q, "after NMI")
            print()
            print(f"RESULT smp={smp}: frozen after cont, "
                  + ("WOKE on NMI" if woke else "still frozen after NMI"))
        else:
            print()
            print(f"RESULT smp={smp}: restored guest EXECUTES")
        q.cmd("quit")
    finally:
        try:
            p.wait(timeout=30)
        except Exception:
            p.kill()
        print("  --- DESTINATION per-VP state ---")
        diag(p)

if __name__ == "__main__":
    main()
