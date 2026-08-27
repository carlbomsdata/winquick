#!/usr/bin/env python3
"""What a healthy cold-booted guest looks like while it idles.

Same sampling as the restored case, so "RIP frozen, IRQ0 ticking" can be
judged against how this guest normally behaves.
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips

def main():
    qemu, statedir, base, work = sys.argv[1:5]
    os.makedirs(work, exist_ok=True)
    overlay = os.path.join(work, "root.qcow2")
    if os.path.exists(overlay):
        os.remove(overlay)
    qemu_img = shutil.which("qemu-img") or "/c/Program Files/qemu/qemu-img.exe"
    subprocess.run([qemu_img, "create", "-q", "-f", "qcow2", "-F", "qcow2",
                    "-b", win(base), win(overlay)], check=True)
    for src, dst in (("ready-vars.fd", "uefi-vars.fd"), ("ready-mailbox.img", "mailbox.img"),
                     ("ready-workspace.img", "workspace.img"), ("ready-artifacts.img", "artifacts.img")):
        shutil.copyfile(os.path.join(statedir, src), os.path.join(work, dst))
    W = lambda n: win(os.path.join(work, n))
    args = [qemu, "-M", "q35", "-accel", "whpx", "-cpu", "Nehalem", "-smp", "4", "-m", "1024",
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
            "-qmp", "tcp:127.0.0.1:55830,server=on,wait=off"]
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55830)
        print("waiting for the kernel to take over...")
        end = time.time() + 90
        while time.time() < end:
            r = rips(q)
            if r and r[0].startswith("fffff8"):
                break
            time.sleep(0.5)
        print(f"  kernel reached at t={time.time()-(end-90):.0f}s, letting it settle")
        time.sleep(20)
        print("sampling a healthy idle guest:")
        for m in (0.0, 0.05, 0.25, 1.0, 2.0, 4.0, 8.0):
            time.sleep(m if m == 0.0 else 0)
            r = rips(q)
            irq = " ".join(q.hmp("info irq").split())
            print(f"  t+{m:5.2f}s  RIP {r}")
            print(f"           irq {irq}")
            time.sleep(m if m else 0.05)
        full = q.hmp("info registers -a")
        print()
        print("per-vCPU state while idling:")
        for line in full.splitlines():
            t = line.strip()
            if t.startswith("RIP="):
                print("   ", t[:130])
        q.cmd("quit")
    finally:
        try: p.wait(timeout=25)
        except Exception: p.kill()
        err = p.stderr.read().decode("utf-8", "replace")
        diag = [l for l in err.splitlines() if l.startswith(("whpx-diag", "whpx-msi"))]
        print()
        print("what a healthy guest's WHPX loop is doing:")
        for l in diag[-8:]:
            print("  " + l[:190])

if __name__ == "__main__":
    main()
