#!/usr/bin/env python3
"""Does the guest survive a plain stop/cont, with no migration at all?

If it does not, the fault is in the WHPX run-state transition rather than in
anything the migration stream carries.
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips
from cycle import args_for

def main():
    qemu, statedir, base, work, smp = sys.argv[1:6]
    smp = int(smp)
    os.makedirs(work, exist_ok=True)
    overlay = os.path.join(work, "root.qcow2")
    if os.path.exists(overlay): os.remove(overlay)
    qi = shutil.which("qemu-img") or "/c/Program Files/qemu/qemu-img.exe"
    subprocess.run([qi, "create", "-q", "-f", "qcow2", "-F", "qcow2",
                    "-b", win(base), win(overlay)], check=True)
    for s0, d0 in (("ready-vars.fd", "uefi-vars.fd"), ("ready-mailbox.img", "mailbox.img"),
                   ("ready-workspace.img", "workspace.img"), ("ready-artifacts.img", "artifacts.img")):
        shutil.copyfile(os.path.join(statedir, s0), os.path.join(work, d0))

    p = subprocess.Popen(args_for(qemu, work, 55880, smp), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55880)
        end = time.time() + 120
        while time.time() < end:
            r = rips(q)
            if r and r[0].startswith("fffff8"): break
            time.sleep(0.5)
        time.sleep(25)
        a = rips(q); time.sleep(0.4); b = rips(q)
        print(f"before stop: running={a != b}")
        print("stop...")
        q.cmd("stop"); time.sleep(1.0)
        print("cont...")
        q.cmd("cont")
        seen = []
        for m in (0.5, 1.0, 2.0, 4.0):
            time.sleep(m/2)
            seen.append(rips(q))
        for i, r in enumerate(seen):
            print(f"  after cont sample {i}: {r}")
        moving = len({tuple(r) for r in seen}) > 1
        print(f"RESULT smp={smp}: after plain stop/cont the guest {'EXECUTES' if moving else 'FROZE'}")
        q.cmd("quit")
    finally:
        try: p.wait(timeout=25)
        except Exception: p.kill()
        err = p.stderr.read().decode("utf-8", "replace")
        for l in [x for x in err.splitlines() if x.startswith("whpx-diag")][-3:]:
            print("  " + l[:180])

if __name__ == "__main__":
    main()
