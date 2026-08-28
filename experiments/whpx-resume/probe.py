#!/usr/bin/env python3
"""Restore a WinQuick prepared state and watch what the guest actually does.

Takes the state directory WinQuick writes (ready.state, ready-disk.qcow2, ...),
puts a command in the mailbox by copying one a real warm run prepared, and then
samples RIP per processor while polling the mailbox for the agent's answer.

    python3 probe.py <qemu> <statedir> <workdir> <smp> [seconds] [mailbox.img]
"""
import os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips

def main():
    qemu, statedir, work, smp = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
    watch = float(sys.argv[5]) if len(sys.argv) > 5 else 40.0
    mbox_src = sys.argv[6] if len(sys.argv) > 6 else os.path.join(statedir, "ready-mailbox.img")
    os.makedirs(work, exist_ok=True)
    for src, dst in (("ready-disk.qcow2", "root.qcow2"), ("ready-vars.fd", "uefi-vars.fd"),
                     ("ready-workspace.img", "workspace.img"), ("ready-artifacts.img", "artifacts.img")):
        shutil.copyfile(os.path.join(statedir, src), os.path.join(work, dst))
    shutil.copyfile(mbox_src, os.path.join(work, "mailbox.img"))
    state = os.path.join(statedir, "ready.state")
    W = lambda n: win(os.path.join(work, n))
    port = 55830
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
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed": break
            if st in ("failed", "cancelled"): raise SystemExit(f"incoming {st}")
            time.sleep(0.02)
        print("  RIP before cont:", " ".join(rips(q)))
        q.cmd("cont")
        t0 = time.time()
        seen = set()
        mb = os.path.join(work, "mailbox.img")
        while time.time() - t0 < watch:
            r = rips(q)
            for x in r: seen.add(x)
            data = open(mb, "rb").read()
            marks = [m.decode() for m in (b"WQGO", b"WQOUT", b"WQERR", b"WQCODE") if m in data]
            print("  t+%5.1fs  %s   mailbox:%s" % (time.time() - t0, " ".join(r), ",".join(marks) or "-"))
            if "WQCODE" in marks:
                print("  the agent answered"); break
            time.sleep(2)
        print("  distinct RIPs seen: %d" % len(seen))
        q.cmd("quit")
    finally:
        try: p.wait(timeout=40)
        except Exception: p.kill()

main()
