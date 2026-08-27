#!/usr/bin/env python3
"""Cold boot, migrate, restore, and report whether the guest still executes.

    python3 cycle.py <qemu> <statedir> <base.qcow2> <workdir> <smp>

Parameterised on vCPU count so the role of SMP can be tested directly.
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips

def args_for(qemu, work, port, smp, incoming=None):
    W = lambda n: win(os.path.join(work, n))
    a = [qemu, "-M", "q35", "-accel", "whpx", "-cpu", "Nehalem",
         "-smp", str(smp), "-m", "1024",
         "-drive", "if=pflash,format=raw,readonly=on,file=C:\\Program Files\\qemu\\share\\edk2-x86_64-code.fd",
         "-drive", f"if=pflash,format=raw,file={W('uefi-vars.fd')}",
         "-drive", f"if=none,id=root,file={W('root.qcow2')},format=qcow2",
         "-device", "nvme,drive=root,serial=wqroot",
         "-drive", f"if=none,id=mbox,file={W('mailbox.img')},format=raw,cache=writethrough",
         "-device", "nvme,drive=mbox,serial=wqmbox",
         "-drive", f"if=none,id=work,file={W('workspace.img')},format=raw,cache=writethrough",
         "-device", "nvme,drive=work,serial=wqwork",
         "-drive", f"if=none,id=arts,file={W('artifacts.img')},format=raw,cache=writethrough",
         "-device", "nvme,drive=arts,serial=wqarts",
         "-device", "ramfb", "-display", "none", "-vga", "none",
         "-rtc", "base=localtime", "-no-reboot",
         "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off"]
    if incoming:
        a += ["-incoming", f"file:{win(incoming)}"]
    return a

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
    state = os.path.join(work, f"smp{smp}.state")
    if os.path.exists(state): os.remove(state)

    print(f"--- smp={smp}: cold boot ---")
    p = subprocess.Popen(args_for(qemu, work, 55870, smp), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55870)
        end = time.time() + 120
        while time.time() < end:
            r = rips(q)
            if r and r[0].startswith("fffff8"): break
            time.sleep(0.5)
        time.sleep(25)
        pre = rips(q); time.sleep(0.4); pre2 = rips(q)
        print(f"  cold running: {'yes' if pre != pre2 else 'SUSPECT (rip static)'}")
        q.cmd("stop")
        q.cmd("migrate-set-parameters", **{"downtime-limit": 600000, "max-bandwidth": 0})
        q.cmd("migrate", uri=f"file:{win(state)}")
        end = time.time() + 120
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed": break
            if st in ("failed", "cancelled") or time.time() > end: raise SystemExit(f"migrate {st}")
            time.sleep(0.05)
        print(f"  migrated {os.path.getsize(state):,} bytes")
        q.cmd("quit")
    finally:
        try: p.wait(timeout=25)
        except Exception: p.kill()

    print(f"--- smp={smp}: restore ---")
    p = subprocess.Popen(args_for(qemu, work, 55871, smp, incoming=state),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55871)
        end = time.time() + 90
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed": break
            if st in ("failed", "cancelled") or time.time() > end: raise SystemExit(f"incoming {st}")
            time.sleep(0.02)
        q.cmd("cont")
        seen = []
        for m in (0.5, 1, 2, 4, 6):
            time.sleep(m if not seen else m - (m/2))
            seen.append(rips(q))
        for i, r in enumerate(seen):
            print(f"  sample {i}: {r}")
        moving = len({tuple(r) for r in seen[1:]}) > 1
        print(f"RESULT smp={smp}: restored guest {'EXECUTES' if moving else 'FROZEN'}")
        q.cmd("quit")
    finally:
        try: p.wait(timeout=25)
        except Exception: p.kill()
        err = p.stderr.read().decode("utf-8", "replace")
        for l in [x for x in err.splitlines() if x.startswith(("whpx-diag", "whpx-msi"))][-3:]:
            print("  " + l[:180])

if __name__ == "__main__":
    main()
