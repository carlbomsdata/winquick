#!/usr/bin/env python3
"""Does a QEMU/WHPX guest resume execution after migrate + restore?

The smallest guest that can answer it: no firmware, no disk, no devices worth
the name. A hand-assembled real-mode loop that increments a counter in low
memory, so "is it executing" is one QMP memory read away.

    python3 tinyguest.py <qemu.exe> <workdir>

Paths are given POSIX-style. Under MSYS2 the driver is a POSIX-flavoured
Python but QEMU is a native Windows binary, so the executable and the files
python touches use one spelling and QEMU's own arguments use the other.
"""
import json, os, shutil, socket, subprocess, sys, time

def win(path):
    """The Windows spelling of a path, for arguments handed to QEMU."""
    cygpath = shutil.which("cygpath")
    if not cygpath:
        return path
    try:
        out = subprocess.run([cygpath, "-w", path], capture_output=True,
                             text=True).stdout.strip()
    except OSError:
        return path
    return out or path

# ---- the guest ------------------------------------------------------------
# 128 KiB of BIOS. QEMU maps the last byte at 0xFFFFFFFF, so file offset
# 0x1FFF0 is the reset vector at 0xFFFFFFF0 and 0x1FF00 is IP 0xFF00.
LOOP_OFF, RESET_OFF, BIOS_SIZE = 0x1FF00, 0x1FFF0, 0x20000
COUNTER = 0x1000

LOOP = bytes([
    0x31, 0xC0,                          # xor ax, ax
    0x8E, 0xD8,                          # mov ds, ax
    0x66, 0xFF, 0x06, 0x00, 0x10,        # inc dword [0x1000]
    0xEB, 0xF9,                          # jmp -7  (back to the inc)
])
RESET = bytes([0xE9, 0x0D, 0xFF])        # jmp 0xFF00

def build_bios(path):
    rom = bytearray(b"\xF4" * BIOS_SIZE)   # hlt everywhere else
    rom[LOOP_OFF:LOOP_OFF + len(LOOP)] = LOOP
    rom[RESET_OFF:RESET_OFF + len(RESET)] = RESET
    open(path, "wb").write(rom)

# ---- QMP ------------------------------------------------------------------
class Qmp:
    def __init__(self, port, timeout=20):
        deadline = time.time() + timeout
        while True:
            try:
                self.s = socket.create_connection(("127.0.0.1", port), 2)
                break
            except OSError:
                if time.time() > deadline:
                    raise
                time.sleep(0.05)
        self.s.settimeout(30)
        self.f = self.s.makefile("rwb")
        self.f.readline()                      # greeting
        self.cmd("qmp_capabilities")

    def cmd(self, name, **args):
        self.f.write((json.dumps({"execute": name, "arguments": args}) + "\n").encode())
        self.f.flush()
        while True:
            line = self.f.readline()
            if not line:
                raise RuntimeError("monitor closed")
            m = json.loads(line)
            if "event" in m:
                continue
            if "error" in m:
                raise RuntimeError(f"{name}: {m['error']['desc']}")
            return m.get("return")

    def hmp(self, line):
        return self.cmd("human-monitor-command", **{"command-line": line})

    def counter(self):
        """The guest's counter, read straight out of guest physical memory."""
        out = self.hmp(f"xp/1wx 0x{COUNTER:x}")
        return int(out.strip().split(":")[1].strip(), 16)

    def runstate(self):
        return self.cmd("query-status")["status"]

def boot(qemu, port, work, incoming=None, extra=()):
    args = [qemu, "-machine", "q35,accel=whpx", "-cpu", "Nehalem",
            "-m", "128", "-smp", "1",
            "-bios", win(os.path.join(work, "tiny.bin")),
            "-nodefaults", "-no-user-config", "-display", "none",
            "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off", *extra]
    if incoming:
        args += ["-incoming", f"file:{win(incoming)}"]
    return subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

def connect(proc, port, what):
    """QMP, or QEMU's own explanation of why there is none."""
    deadline = time.time() + 20
    while time.time() < deadline:
        if proc.poll() is not None:
            err = proc.stderr.read().decode("utf-8", "replace").strip()
            raise SystemExit(f"{what}: qemu exited ({proc.returncode}):\n{err}")
        try:
            return Qmp(port, timeout=1)
        except OSError:
            time.sleep(0.1)
    err = ""
    if proc.poll() is not None:
        err = proc.stderr.read().decode("utf-8", "replace").strip()
    raise SystemExit(f"{what}: no monitor after 20s\n{err}")


def advancing(q, label, samples=4, gap=0.15):
    vals = []
    for _ in range(samples):
        vals.append(q.counter())
        time.sleep(gap)
    moved = vals[-1] != vals[0]
    print(f"  {label}: {[hex(v) for v in vals]}  -> {'ADVANCING' if moved else 'FROZEN'}")
    return moved, vals

def main():
    qemu, work = sys.argv[1], sys.argv[2]
    os.makedirs(work, exist_ok=True)
    build_bios(os.path.join(work, "tiny.bin"))
    state = os.path.join(work, "tiny.state")
    for p in (state,):
        if os.path.exists(p):
            os.remove(p)

    print("== source: boot and check the guest runs at all ==")
    src = boot(qemu, 55801, work)
    try:
        q = connect(src, 55801, "source")
        time.sleep(0.3)
        ran, _ = advancing(q, "cold guest")
        if not ran:
            print("  the guest never ran under WHPX at all; nothing else is meaningful")
            return 2

        print("== stop and migrate to a file ==")
        q.cmd("stop")
        stopped, before = advancing(q, "after stop", samples=2)
        q.cmd("migrate-set-parameters", **{"downtime-limit": 600000, "max-bandwidth": 0})
        q.cmd("migrate", uri=f"file:{win(state)}")
        deadline = time.time() + 30
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled") or time.time() > deadline:
                print(f"  migration did not complete: {st}")
                return 3
            time.sleep(0.05)
        saved = before[-1]
        print(f"  migrated, state {os.path.getsize(state):,} bytes, counter frozen at {hex(saved)}")
        q.cmd("quit")
    finally:
        try:
            src.wait(timeout=15)
        except Exception:
            src.kill()

    print("== destination: fresh process, restore, cont ==")
    dst = boot(qemu, 55802, work, incoming=state)
    try:
        q = connect(dst, 55802, "destination")
        deadline = time.time() + 30
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed":
                break
            if st in ("failed", "cancelled") or time.time() > deadline:
                print(f"  incoming migration did not complete: {st}")
                return 4
            time.sleep(0.02)
        print(f"  migration loaded, runstate={q.runstate()}")
        restored = q.counter()
        print(f"  counter after load: {hex(restored)} (saved {hex(saved)})"
              f" {'MATCHES' if restored == saved else 'DIFFERS'}")
        q.cmd("cont")
        print(f"  runstate after cont: {q.runstate()}")
        moved, _ = advancing(q, "restored guest", samples=6, gap=0.25)
        print()
        print("RESULT: restored guest " + ("EXECUTES" if moved else "DOES NOT EXECUTE"))
        return 0 if moved else 1
    finally:
        try:
            q.cmd("quit")
            dst.wait(timeout=15)
        except Exception:
            dst.kill()

if __name__ == "__main__":
    sys.exit(main())
