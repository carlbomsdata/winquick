#!/usr/bin/env python3
"""Restore an SMP guest and watch how much real work it does over a minute."""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips
from cycle import args_for

def main():
    qemu, statedir, base, work, smp = sys.argv[1:6]
    smp = int(smp)
    os.makedirs(work, exist_ok=True)
    ov = os.path.join(work, "root.qcow2")
    if os.path.exists(ov):
        os.remove(ov)
    qi = shutil.which("qemu-img") or "/c/Program Files/qemu/qemu-img.exe"
    subprocess.run([qi, "create", "-q", "-f", "qcow2", "-F", "qcow2",
                    "-b", win(base), win(ov)], check=True)
    for a, b in (("ready-vars.fd", "uefi-vars.fd"), ("ready-mailbox.img", "mailbox.img"),
                 ("ready-workspace.img", "workspace.img"), ("ready-artifacts.img", "artifacts.img")):
        shutil.copyfile(os.path.join(statedir, a), os.path.join(work, b))
    state = os.path.join(work, "s.state")
    if os.path.exists(state):
        os.remove(state)

    p = subprocess.Popen(args_for(qemu, work, 55910, smp),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55910)
        end = time.time() + 180
        while time.time() < end:
            r = rips(q)
            if r and r[0].startswith("fffff8"):
                break
            time.sleep(0.5)
        time.sleep(25)
        q.cmd("stop")
        q.cmd("migrate-set-parameters", **{"downtime-limit": 600000, "max-bandwidth": 0})
        q.cmd("migrate", uri=f"file:{win(state)}")
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled"):
                raise SystemExit(st)
            time.sleep(0.05)
        q.cmd("quit")
    finally:
        try: p.wait(timeout=30)
        except Exception: p.kill()

    print(f"=== smp={smp} restored: one minute of observation ===")
    p = subprocess.Popen(args_for(qemu, work, 55911, smp, incoming=state),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55911)
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled"):
                raise SystemExit(st)
            time.sleep(0.02)
        d0 = os.path.getsize(ov)
        q.cmd("cont")
        seen = set()
        for t in (5, 15, 30, 45, 60):
            time.sleep(t - (0 if t == 5 else t - 15 if t == 15 else 15))
            r = tuple(rips(q))
            seen.add(r)
            print(f"  t+{t:3d}s overlay={os.path.getsize(ov):,}  RIP={list(r)}")
        d1 = os.path.getsize(ov)
        print(f"  overlay {d0:,} -> {d1:,} ({'grew' if d1 > d0 else 'unchanged'})")
        print(f"  distinct RIP sets: {len(seen)}")
        q.cmd("quit")
    finally:
        try: p.wait(timeout=30)
        except Exception: p.kill()
        err = p.stderr.read().decode("utf-8", "replace")
        for l in [x for x in err.splitlines() if x.startswith(("whpx-diag exits", "whpx-msi"))][-2:]:
            print("   " + l[:170])

if __name__ == "__main__":
    main()
