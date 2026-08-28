#!/usr/bin/env python3
"""Restore a prepared SMP guest cleanly and say whether Windows crashed.

No NMIs, no pokes -- the guest is left completely alone so the bugcheck is the
guest's own. Liveness is judged by whether a crash dump appears, not by RIP
movement or overlay growth, both of which a dump-writing guest fakes.

    python3 crashrun.py <qemu> <statedir> <base> <workdir> <smp> [seconds]
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips
from cycle import args_for

def build_state(qemu, statedir, base, work, smp, port):
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
    p = subprocess.Popen(args_for(qemu, work, port, smp),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(port)
        end = time.time() + 200
        while time.time() < end:
            r = rips(q)
            if r and r[0].startswith("fffff8"):
                break
            time.sleep(0.5)
        time.sleep(25)
        tsc_src = {}
        for i in range(smp):
            tsc_src[i] = q.hmp(f"info registers -a")  # placeholder, real TSC below
        q.cmd("stop")
        q.cmd("migrate-set-parameters", **{"downtime-limit": 600000, "max-bandwidth": 0})
        q.cmd("migrate", uri=f"file:{win(state)}")
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled"):
                raise SystemExit(f"migrate {st}")
            time.sleep(0.05)
        q.cmd("quit")
    finally:
        try: p.wait(timeout=30)
        except Exception: p.kill()
    return state

def main():
    qemu, statedir, base, work, smp = sys.argv[1:6]
    smp = int(smp)
    watch = float(sys.argv[6]) if len(sys.argv) > 6 else 60.0
    os.makedirs(work, exist_ok=True)
    state = build_state(qemu, statedir, base, work, smp, 55940)
    print(f"  prepared state {os.path.getsize(state):,} bytes")

    ov = os.path.join(work, "root.qcow2")
    qi = shutil.which("qemu-img") or "/c/Program Files/qemu/qemu-img.exe"
    os.remove(ov)
    subprocess.run([qi, "create", "-q", "-f", "qcow2", "-F", "qcow2",
                    "-b", win(base), win(ov)], check=True)
    shutil.copyfile(os.path.join(statedir, "ready-mailbox.img"),
                    os.path.join(work, "mailbox.img"))

    p = subprocess.Popen(args_for(qemu, work, 55941, smp, incoming=state),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55941)
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled"):
                raise SystemExit(f"incoming {st}")
            time.sleep(0.02)
        q.cmd("cont")
        t0 = time.time()
        first_dump = None
        while time.time() - t0 < watch:
            time.sleep(2)
            with open(ov, "rb") as f:
                if b"PAGEDU64" in f.read():
                    first_dump = time.time() - t0
                    break
        if first_dump is not None:
            print(f"  CRASH: dump signature appeared {first_dump:.1f}s after cont")
            time.sleep(20)          # let it finish writing
        else:
            print(f"  no dump within {watch:.0f}s")
        q.cmd("quit")
    finally:
        try: p.wait(timeout=40)
        except Exception: p.kill()

if __name__ == "__main__":
    main()
