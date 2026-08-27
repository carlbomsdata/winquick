#!/usr/bin/env python3
"""Compare the guest's LAPIC state cold versus restored.

`info lapic` reads QEMU's shadow of the APIC, which under WHPX is only synced
from the hypervisor when CPU state is synchronised. Stopping the guest forces
that, so every dump here is taken with the guest stopped and therefore current.
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips

def lapic(q, n=4):
    out = []
    for i in range(n):
        out.append(f"--- lapic {i} ---\n" + (q.hmp(f"info lapic {i}") or ""))
    return "\n".join(out)

def args_for(qemu, work, port, incoming=None):
    W = lambda n: win(os.path.join(work, n))
    a = [qemu, "-M", "q35", "-accel", "whpx", "-cpu", "Nehalem", "-smp", "4", "-m", "1024",
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
    qemu, statedir, base, work, outdir = sys.argv[1:6]
    os.makedirs(work, exist_ok=True); os.makedirs(outdir, exist_ok=True)
    overlay = os.path.join(work, "root.qcow2")
    if os.path.exists(overlay): os.remove(overlay)
    qemu_img = shutil.which("qemu-img") or "/c/Program Files/qemu/qemu-img.exe"
    subprocess.run([qemu_img, "create", "-q", "-f", "qcow2", "-F", "qcow2",
                    "-b", win(base), win(overlay)], check=True)
    for src, dst in (("ready-vars.fd", "uefi-vars.fd"), ("ready-mailbox.img", "mailbox.img"),
                     ("ready-workspace.img", "workspace.img"), ("ready-artifacts.img", "artifacts.img")):
        shutil.copyfile(os.path.join(statedir, src), os.path.join(work, dst))
    state = os.path.join(work, "fresh.state")
    if os.path.exists(state): os.remove(state)

    print("cold boot...")
    p = subprocess.Popen(args_for(qemu, work, 55840), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55840)
        end = time.time() + 120
        while time.time() < end:
            r = rips(q)
            if r and r[0].startswith("fffff8"):
                break
            time.sleep(0.5)
        print("  kernel up; settling 25s")
        time.sleep(25)
        q.cmd("stop"); time.sleep(0.3)
        open(os.path.join(outdir, "cold.lapic.txt"), "w").write(lapic(q))
        open(os.path.join(outdir, "cold.regs.txt"), "w").write(q.hmp("info registers -a"))
        print("  captured cold (stopped)")
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

    print("restore...")
    p = subprocess.Popen(args_for(qemu, work, 55841, incoming=state),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55841)
        end = time.time() + 90
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed": break
            if st in ("failed", "cancelled") or time.time() > end: raise SystemExit(f"incoming {st}")
            time.sleep(0.02)
        open(os.path.join(outdir, "restored-paused.lapic.txt"), "w").write(lapic(q))
        print("  captured restored (paused, after post_load)")
        q.cmd("cont"); time.sleep(4)
        print(f"  RIP after 4s: {rips(q)}")
        q.cmd("stop"); time.sleep(0.3)
        open(os.path.join(outdir, "restored-stopped.lapic.txt"), "w").write(lapic(q))
        open(os.path.join(outdir, "restored-stopped.regs.txt"), "w").write(q.hmp("info registers -a"))
        print("  captured restored (after running, stopped)")
        q.cmd("quit")
    finally:
        try: p.wait(timeout=25)
        except Exception: p.kill()
    print("done ->", outdir)

if __name__ == "__main__":
    main()
