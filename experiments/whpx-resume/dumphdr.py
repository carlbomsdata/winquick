#!/usr/bin/env python3
"""Parse every Windows crash-dump header in a file.

DUMP_HEADER64, from wdbgexts.h. The context record matters as much as the
bugcheck code: it carries the register state at the moment of the fault.

    python3 dumphdr.py <file> [...]
"""
import struct, sys

BUGCHECKS = {
    0x0A: "IRQL_NOT_LESS_OR_EQUAL",
    0x1E: "KMODE_EXCEPTION_NOT_HANDLED",
    0x50: "PAGE_FAULT_IN_NONPAGED_AREA",
    0x7E: "SYSTEM_THREAD_EXCEPTION_NOT_HANDLED",
    0x80: "NMI_HARDWARE_FAILURE",
    0x9C: "MACHINE_CHECK_EXCEPTION",
    0xD1: "DRIVER_IRQL_NOT_LESS_OR_EQUAL",
    0x101: "CLOCK_WATCHDOG_TIMEOUT",
    0x109: "CRITICAL_STRUCTURE_CORRUPTION",
    0x124: "WHEA_UNCORRECTABLE_ERROR",
    0x133: "DPC_WATCHDOG_VIOLATION",
    0x139: "KERNEL_SECURITY_CHECK_FAILURE",
}

# CONTEXT (amd64) field offsets, from winnt.h.
CTX = [
    (0x30, "ContextFlags"), (0x34, "MxCsr"),
    (0x38, "SegCs"), (0x3A, "SegDs"), (0x3C, "SegEs"), (0x3E, "SegFs"),
    (0x40, "SegGs"), (0x42, "SegSs"), (0x44, "EFlags"),
    (0x5C, "Dr0"), (0x78, "Rax"), (0x80, "Rcx"), (0x88, "Rdx"),
    (0x90, "Rbx"), (0x98, "Rsp"), (0xA0, "Rbp"), (0xA8, "Rsi"),
    (0xB0, "Rdi"), (0xB8, "R8"), (0xC0, "R9"), (0xC8, "R10"),
    (0xD0, "R11"), (0xD8, "R12"), (0xE0, "R13"), (0xE8, "R14"),
    (0xF0, "R15"), (0xF8, "Rip"),
]

def parse(buf, at, label=""):
    h = buf[at:at + 0x2000]
    if len(h) < 0x1100:
        print(f"  {label}truncated header at {at:#x}")
        return None
    mach = struct.unpack_from("<I", h, 0x30)[0]
    if mach != 0x8664:
        return None                      # not a real x64 dump header
    nproc = struct.unpack_from("<I", h, 0x34)[0]
    code = struct.unpack_from("<I", h, 0x38)[0]
    p = struct.unpack_from("<QQQQ", h, 0x40)
    dtb = struct.unpack_from("<Q", h, 0x10)[0]
    kdbg = struct.unpack_from("<Q", h, 0x80)[0]
    dumptype = struct.unpack_from("<I", h, 0xF98)[0]
    required = struct.unpack_from("<Q", h, 0xFA0)[0]
    print(f"  {label}dump header @ {at:#x}")
    print(f"    NumberProcessors = {nproc}   DirectoryTableBase = {dtb:#x}")
    print(f"    KdDebuggerDataBlock = {kdbg:#x}   DumpType = {dumptype}"
          f"   RequiredDumpSpace = {required:,}")
    print(f"    BugCheck {code:#x}  {BUGCHECKS.get(code, '(unnamed)')}")
    print(f"      P1={p[0]:#x}  P2={p[1]:#x}  P3={p[2]:#x}  P4={p[3]:#x}")
    # context record
    ctx = h[0x348:0x348 + 0x4D0]
    flags = struct.unpack_from("<I", ctx, 0x30)[0]
    rip = struct.unpack_from("<Q", ctx, 0xF8)[0]
    rsp = struct.unpack_from("<Q", ctx, 0x98)[0]
    if rip or rsp:
        print(f"    context: RIP={rip:#x} RSP={rsp:#x} ContextFlags={flags:#x}")
        for off, name in CTX:
            if name in ("Rax", "Rcx", "Rdx", "Rbx", "Rsi", "Rdi", "R8", "R9"):
                v = struct.unpack_from("<Q", ctx, off)[0]
                print(f"      {name:4} = {v:#018x}")
        cs = struct.unpack_from("<H", ctx, 0x38)[0]
        gs = struct.unpack_from("<H", ctx, 0x40)[0]
        efl = struct.unpack_from("<I", ctx, 0x44)[0]
        print(f"      SegCs={cs:#x} SegGs={gs:#x} EFlags={efl:#x}")
    return code

def main():
    for path in sys.argv[1:]:
        data = open(path, "rb").read()
        print(f"=== {path} ({len(data):,} bytes) ===")
        found, at = 0, 0
        while True:
            i = data.find(b"PAGEDU64", at)
            if i < 0:
                break
            at = i + 8
            if parse(data, i) is not None:
                found += 1
        if not found:
            print("  no valid dump header -> the guest did not bugcheck")

if __name__ == "__main__":
    main()
