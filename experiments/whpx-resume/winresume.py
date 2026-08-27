#!/usr/bin/env python3
"""Restore WinQuick's prepared Windows guest and watch whether it executes.

QMP saying `running` proves only QEMU's runstate. This samples RIP on every
vCPU, so the answer is about the processor, not the bookkeeping.

    python3 winresume.py <qemu> <statedir> <workdir> [seconds]
"""
import json, os, re, shutil, socket, subprocess, sys, time

def win(p):
    cyg = shutil.which("cygpath")
    if not cyg:
        return p
    return subprocess.run([cyg, "-w", p], capture_output=True, text=True).stdout.strip() or p

class Qmp:
    def __init__(self, port, timeout=30):
        end = time.time() + timeout
        while True:
            try:
                self.s = socket.create_connection(("127.0.0.1", port), 2); break
            except OSError:
                if time.time() > end: raise
                time.sleep(0.05)
        self.s.settimeout(60)
        self.f = self.s.makefile("rwb")
        self.f.readline()
        self.cmd("qmp_capabilities")
    def cmd(self, name, **args):
        self.f.write((json.dumps({"execute": name, "arguments": args}) + "\n").encode()); self.f.flush()
        while True:
            line = self.f.readline()
            if not line: raise RuntimeError("monitor closed")
            m = json.loads(line)
            if "event" in m: continue
            if "error" in m: raise RuntimeError(f"{name}: {m['error']['desc']}")
            return m.get("return")
    def hmp(self, line):
        return self.cmd("human-monitor-command", **{"command-line": line})

RIP_RE = re.compile(r"RIP=([0-9a-f]+)")

def rips(q):
    """RIP for every vCPU, in order."""
    return RIP_RE.findall(q.hmp("info registers -a"))

def main():
    qemu, statedir, work = sys.argv[1], sys.argv[2], sys.argv[3]
    watch = float(sys.argv[4]) if len(sys.argv) > 4 else 6.0
    os.makedirs(work, exist_ok=True)

    # Working copies, exactly as a warm run makes them.
    for src, dst in (("ready-disk.qcow2", "root.qcow2"),
                     ("ready-vars.fd", "uefi-vars.fd"),
                     ("ready-mailbox.img", "mailbox.img"),
                     ("ready-workspace.img", "workspace.img"),
                     ("ready-artifacts.img", "artifacts.img")):
        shutil.copyfile(os.path.join(statedir, src), os.path.join(work, dst))
    state = os.path.join(statedir, "ready.state")
    W = lambda n: win(os.path.join(work, n))

    args = [qemu, "-M", "q35", "-accel", "whpx", "-cpu", "Nehalem",
            "-smp", "4", "-m", "1024",
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
            "-qmp", "tcp:127.0.0.1:55810,server=on,wait=off",
            "-incoming", f"file:{win(state)}"]

    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55810)
        t0 = time.time()
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed": break
            if st in ("failed", "cancelled"): raise SystemExit(f"incoming migration {st}")
            if time.time() - t0 > 60: raise SystemExit("incoming migration never completed")
            time.sleep(0.02)
        print(f"migration loaded in {time.time()-t0:.2f}s, runstate={q.cmd('query-status')['status']}")

        before = rips(q)
        print(f"RIP before cont : {before}")
        q.cmd("cont")
        print(f"runstate after cont: {q.cmd('query-status')['status']}")

        disk0 = os.path.getsize(os.path.join(work, "root.qcow2"))
        marks = [0.0, 0.01, 0.1, 0.5, 1.0, 2.0, 4.0]
        marks = [m for m in marks if m <= watch] + [watch]
        t1 = time.time()
        seen = []
        marks = [0.0, 0.05, 0.25, 1.0, 2.0, 4.0, 8.0]
        for m in marks:
            d = m - (time.time() - t1)
            if d > 0: time.sleep(d)
            r = rips(q)
            seen.append(r)
            # Interrupt counts answer the question RIP cannot: is anything
            # still being delivered to a guest that has gone quiet?
            irq = " ".join(q.hmp("info irq").split())
            print(f"  t+{m:6.2f}s  RIP {r}")
            print(f"            irq {irq}")
        # The state that decides whether an interrupt can ever land: halted or
        # not, and whether the guest has interrupts enabled at all.
        full = q.hmp("info registers -a")
        open(os.path.join(work, "restored-registers.txt"), "w").write(full)
        print()
        print("per-vCPU interrupt-relevant state after cont:")
        for line in full.splitlines():
            t = line.strip()
            if t.startswith("RIP=") or t.startswith("CR0=") or t.startswith("EFER="):
                print("   ", t[:150])
        disk1 = os.path.getsize(os.path.join(work, "root.qcow2"))

        moved = any(seen[i] != seen[0] for i in range(len(seen)))
        allsame = all(len(set(r)) <= 1 for r in seen)
        print()
        print(f"overlay: {disk0:,} -> {disk1:,} bytes ({'grew' if disk1>disk0 else 'unchanged'})")
        print("VERDICT: RIP " + ("ADVANCES" if moved else "NEVER CHANGES"))
        if not moved and allsame:
            print("  (and every vCPU sits at the same address)")
    finally:
        try:
            q.cmd("quit"); p.wait(timeout=20)
        except Exception:
            p.kill()
        err = p.stderr.read().decode("utf-8", "replace")
        open(os.path.join(work, "qemu-stderr.txt"), "w").write(err)
        lines = [l for l in err.splitlines() if l.strip()]
        diag = [l for l in lines if l.startswith("whpx-diag")]
        other = [l for l in lines if not l.startswith("whpx-diag")]
        print(f"qemu stderr: {len(lines)} lines ({len(diag)} diagnostic)")
        for l in diag[-9:]:
            print("  " + l[:190])
        for l in other[:3]:
            print("  " + l[:160])

if __name__ == "__main__":
    main()
