#!/usr/bin/env python3
"""Does a stalled restored guest just need waking?

Restore, run, and if the agent has not answered after a while, inject an NMI --
which this QEMU can actually deliver, see patches/whpx-nmi-delivery.patch -- and
see whether the guest completes the command afterwards. A guest that finishes
after an NMI was waiting for an interrupt, not stuck.

Any crash dump produced after this point is a contaminated diagnostic run.

    python3 wake.py <qemu> <statedir> <workdir> <smp> <nmi_after_s> <mailbox.img>
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips

qemu, statedir, work, smp = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
nmi_after = float(sys.argv[5])
mbox_src = sys.argv[6]
os.makedirs(work, exist_ok=True)
for src, dst in (("ready-disk.qcow2", "root.qcow2"), ("ready-vars.fd", "uefi-vars.fd"),
                 ("ready-workspace.img", "workspace.img"), ("ready-artifacts.img", "artifacts.img")):
    shutil.copyfile(os.path.join(statedir, src), os.path.join(work, dst))
shutil.copyfile(mbox_src, os.path.join(work, "mailbox.img"))
state = os.path.join(statedir, "ready.state")
W = lambda n: win(os.path.join(work, n))
port = 55840
args = [qemu, "-M", "q35", "-accel", "whpx", "-cpu", "Nehalem",
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
        "-serial", f"file:{W('serial.log')}",
        "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off",
        "-incoming", f"file:{win(state)}"]
p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
try:
    q = Qmp(port)
    while q.cmd("query-migrate").get("status") != "completed":
        time.sleep(0.02)
    q.cmd("cont")
    t0 = time.time()
    poked = False
    mb = os.path.join(work, "mailbox.img")
    while time.time() - t0 < nmi_after + 40:
        t = time.time() - t0
        answered = b"WQCODE" in open(mb, "rb").read()
        print("  t+%5.1fs  %s  %s%s" % (t, " ".join(rips(q)),
                                        "ANSWERED" if answered else "-",
                                        "   (nmi sent)" if poked else ""))
        if answered:
            print("  the agent answered %s the NMI" % ("after" if poked else "without"))
            break
        if not poked and t >= nmi_after:
            q.cmd("inject-nmi")
            poked = True
            print("  --- NMI injected ---")
        time.sleep(2)
    else:
        print("  never answered")
    q.cmd("quit")
finally:
    try: p.wait(timeout=40)
    except Exception: p.kill()
