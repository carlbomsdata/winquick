#!/usr/bin/env python3
"""Restore one migration state and watch whether the guest keeps executing.

    python3 samplestate.py <qemu> <state-file> <workdir> [seconds]

The work directory must already hold the disks the state was taken with.
"""
import os, sys, subprocess, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from winresume import Qmp, win, rips
from lapicdiff import args_for

def main():
    qemu, state, work = sys.argv[1:4]
    watch = float(sys.argv[4]) if len(sys.argv) > 4 else 8.0
    p = subprocess.Popen(args_for(qemu, work, 55850, incoming=state),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        q = Qmp(55850)
        end = time.time() + 90
        while True:
            st = q.cmd("query-migrate").get("status")
            if st == "completed": break
            if st in ("failed", "cancelled") or time.time() > end: raise SystemExit(f"incoming {st}")
            time.sleep(0.02)
        print(f"loaded; RIP before cont: {rips(q)}")
        q.cmd("cont")
        seen = []
        for m in (0.05, 0.5, 1, 2, 4, 8):
            if m > watch: break
            time.sleep(m - (0 if not seen else 0))
            r = rips(q)
            seen.append(r)
            irq0 = ""
            try:
                line = q.hmp("info irq")
                irq0 = " ".join(line.split())[:110]
            except Exception:
                pass
            print(f"  t~{m:5.2f}s RIP {r}")
            print(f"          irq {irq0}")
        distinct = len({tuple(r) for r in seen})
        print()
        print(f"distinct RIP tuples over the window: {distinct}")
        print("RESULT: " + ("guest is EXECUTING" if distinct > 1 else "guest is FROZEN"))
        q.cmd("quit")
    finally:
        try: p.wait(timeout=25)
        except Exception: p.kill()

if __name__ == "__main__":
    main()
