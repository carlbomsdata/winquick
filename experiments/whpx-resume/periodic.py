#!/usr/bin/env python3
"""Does a periodic external wake revive a restored SMP guest?

If the guest only lacks a recurring wake-up, feeding it one should make it do
real work -- visible as growth in the copy-on-write overlay, which only happens
when the guest actually writes to disk.
"""
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
    state = os.path.join(work, "p.state")
    if os.path.exists(state):
        os.remove(state)

    p = subprocess.Popen(args_for(qemu, work, 55920, smp),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55920)
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

    for label, periodic in (("no wake", False), ("periodic NMI", True)):
        shutil.copyfile(os.path.join(statedir, "ready-mailbox.img"),
                        os.path.join(work, "mailbox.img"))
        if os.path.exists(ov):
            os.remove(ov)
        subprocess.run([qi, "create", "-q", "-f", "qcow2", "-F", "qcow2",
                        "-b", win(base), win(ov)], check=True)
        port = 55921 if not periodic else 55922
        p = subprocess.Popen(args_for(qemu, work, port, smp, incoming=state),
                             stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            q = Qmp(port)
            while True:
                st = q.cmd("query-migrate").get("status")
                if st == "completed":
                    break
                if st in ("failed", "cancelled"):
                    raise SystemExit(st)
                time.sleep(0.02)
            d0 = os.path.getsize(ov)
            q.cmd("cont")
            t0 = time.time()
            while time.time() - t0 < 20:
                if periodic:
                    try:
                        q.cmd("inject-nmi")
                    except Exception:
                        pass
                time.sleep(0.2)
            d1 = os.path.getsize(ov)
            print(f"  {label:14}: overlay {d0:,} -> {d1:,}  (+{d1 - d0:,})")
            q.cmd("quit")
        finally:
            try: p.wait(timeout=30)
            except Exception: p.kill()

if __name__ == "__main__":
    main()
