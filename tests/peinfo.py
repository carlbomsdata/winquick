#!/usr/bin/env python3
"""Read what a .NET assembly actually is, from its own bytes.

A `.csproj` saying `<TargetFrameworkVersion>v4.0</TargetFrameworkVersion>` is a
statement of intent. This reads the produced file instead: the PE machine type,
the CLR header flags that decide 32- vs 64-bit execution, the CLR metadata
version, and the `TargetFrameworkAttribute` the compiler actually stamped.

    python3 tests/peinfo.py <assembly> [...]
    python3 tests/peinfo.py --json <assembly>

Deliberately dependency-free: it parses the PE and CLI headers directly rather
than trusting a toolchain to describe its own output.
"""

import json
import re
import struct
import sys

MACHINE = {
    0x014C: "x86",
    0x8664: "x64",
    0xAA64: "arm64",
    0x01C4: "arm",
    0x0200: "ia64",
}

SUBSYSTEM = {1: "native", 2: "windows-gui", 3: "windows-console"}

# COR20 header flags (ECMA-335 II.25.3.3.1).
COMIMAGE_FLAGS_ILONLY = 0x00000001
COMIMAGE_FLAGS_32BITREQUIRED = 0x00000002
COMIMAGE_FLAGS_STRONGNAMESIGNED = 0x00000008
COMIMAGE_FLAGS_32BITPREFERRED = 0x00020000


def _rva_to_offset(sections, rva):
    for va, vsize, raw_ptr, raw_size in sections:
        if va <= rva < va + max(vsize, raw_size):
            return raw_ptr + (rva - va)
    return None


def inspect(path):
    data = open(path, "rb").read()
    out = {"file": path, "bytes": len(data)}

    if data[:2] != b"MZ":
        out["error"] = "not a PE file"
        return out
    pe_off = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe_off:pe_off + 4] != b"PE\0\0":
        out["error"] = "no PE signature"
        return out

    coff = pe_off + 4
    machine, nsections = struct.unpack_from("<HH", data, coff)
    opt_size = struct.unpack_from("<H", data, coff + 16)[0]
    out["machine"] = MACHINE.get(machine, f"0x{machine:04x}")

    opt = coff + 20
    magic = struct.unpack_from("<H", data, opt)[0]
    pe32_plus = magic == 0x20B
    out["peFormat"] = "PE32+" if pe32_plus else "PE32"

    # PE32 and PE32+ diverge only between offsets 24 and 32 (BaseOfData plus a
    # 4-byte ImageBase, against an 8-byte ImageBase), so every field from
    # SectionAlignment onwards sits at the same place in both.
    major_os, minor_os = struct.unpack_from("<HH", data, opt + 40)
    major_sub, minor_sub = struct.unpack_from("<HH", data, opt + 48)
    subsystem = struct.unpack_from("<H", data, opt + 68)[0]
    out["subsystem"] = SUBSYSTEM.get(subsystem, subsystem)
    out["osVersion"] = f"{major_os}.{minor_os}"
    out["subsystemVersion"] = f"{major_sub}.{minor_sub}"

    # Section table, needed to turn an RVA into a file offset.
    sec_off = opt + opt_size
    sections = []
    for i in range(nsections):
        s = sec_off + i * 40
        vsize, va, raw_size, raw_ptr = struct.unpack_from("<IIII", data, s + 8)
        sections.append((va, vsize, raw_ptr, raw_size))

    # Data directory 14 is the CLI header; a native binary has none.
    dd = opt + (0x70 if pe32_plus else 0x60)
    clr_rva, clr_size = struct.unpack_from("<II", data, dd + 14 * 8)
    if clr_rva == 0:
        out["managed"] = False
        return out
    out["managed"] = True

    clr_off = _rva_to_offset(sections, clr_rva)
    if clr_off is None:
        out["error"] = "CLI header RVA outside every section"
        return out
    (_cb, rt_major, rt_minor, md_rva, md_size, flags) = struct.unpack_from(
        "<IHHIII", data, clr_off
    )
    out["clrHeaderVersion"] = f"{rt_major}.{rt_minor}"
    out["ilOnly"] = bool(flags & COMIMAGE_FLAGS_ILONLY)
    out["requires32Bit"] = bool(flags & COMIMAGE_FLAGS_32BITREQUIRED)
    out["prefers32Bit"] = bool(flags & COMIMAGE_FLAGS_32BITPREFERRED)
    out["strongNameSigned"] = bool(flags & COMIMAGE_FLAGS_STRONGNAMESIGNED)

    # How the assembly presents itself to a loader, in ILDASM's terms.
    if out["machine"] == "x86" and out["requires32Bit"]:
        out["platform"] = "x86 (32-bit required)"
    elif out["machine"] == "x86" and out["prefers32Bit"]:
        out["platform"] = "AnyCPU (32-bit preferred)"
    elif out["machine"] == "x86":
        out["platform"] = "AnyCPU"
    else:
        out["platform"] = out["machine"]

    # CLR metadata: "BSJB" signature, then a version string such as v4.0.30319.
    md_off = _rva_to_offset(sections, md_rva)
    if md_off is not None and data[md_off:md_off + 4] == b"BSJB":
        vlen = struct.unpack_from("<I", data, md_off + 12)[0]
        ver = data[md_off + 16:md_off + 16 + vlen].split(b"\0")[0]
        out["metadataVersion"] = ver.decode("ascii", "replace")

    # TargetFrameworkAttribute's argument is a UTF-8 string in the blob heap.
    m = re.search(
        rb"\.NET(?:Framework|CoreApp|Standard),Version=v[0-9][0-9.]*"
        rb"(?:,Profile=[A-Za-z0-9]+)?",
        data,
    )
    if m:
        out["targetFramework"] = m.group(0).decode("ascii")

    refs = sorted(set(re.findall(rb"(?<![A-Za-z0-9.])(mscorlib|System\.Windows\.Forms|"
                                 rb"PresentationFramework|System\.Runtime)(?![A-Za-z0-9])", data)))
    out["notableReferences"] = [r.decode() for r in refs]
    return out


def main():
    args = [a for a in sys.argv[1:] if a != "--json"]
    as_json = "--json" in sys.argv[1:]
    results = [inspect(p) for p in args]
    if as_json:
        print(json.dumps(results if len(results) > 1 else results[0], indent=2))
        return 0
    for r in results:
        print(r["file"])
        if "error" in r:
            print(f"  error: {r['error']}")
            continue
        if not r.get("managed"):
            print(f"  native {r['machine']} {r['peFormat']}")
            continue
        print(f"  machine            {r['machine']}   ({r['peFormat']}, {r['platform']})")
        print(f"  targetFramework    {r.get('targetFramework', '<none stamped>')}")
        print(f"  metadataVersion    {r.get('metadataVersion', '?')}")
        print(f"  clrHeaderVersion   {r['clrHeaderVersion']}")
        print(f"  subsystem          {r['subsystem']} {r['subsystemVersion']} (min OS {r['osVersion']})")
        print(f"  flags              ILONLY={r['ilOnly']} 32BITREQ={r['requires32Bit']} "
              f"32BITPREF={r['prefers32Bit']}")
        if r["notableReferences"]:
            print(f"  references         {', '.join(r['notableReferences'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
