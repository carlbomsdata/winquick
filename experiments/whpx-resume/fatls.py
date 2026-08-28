#!/usr/bin/env python3
"""List a FAT32 mailbox's root directory straight from the image.

The point is to see what a prepared guest left behind: which entries exist,
which clusters they claim, and whether the FAT agrees that those clusters are
allocated. A guest frozen in the middle of its own filesystem work leaves the
two disagreeing.
"""
import struct, sys

PART = 2048 * 512

def dump(path):
    f = open(path, "rb")
    f.seek(PART); bpb = f.read(512)
    bps  = struct.unpack_from("<H", bpb, 0x0B)[0]
    spc  = bpb[0x0D]
    rsvd = struct.unpack_from("<H", bpb, 0x0E)[0]
    nfat = bpb[0x10]
    fsz  = struct.unpack_from("<I", bpb, 0x24)[0]
    root = struct.unpack_from("<I", bpb, 0x2C)[0]
    fat_off  = PART + rsvd * bps
    data_off = fat_off + nfat * fsz * bps
    def fat(n):
        f.seek(fat_off + 4 * n); return struct.unpack("<I", f.read(4))[0] & 0x0FFFFFFF
    print(f"--- {path}")
    print(f"    bps={bps} spc={spc} rsvd={rsvd} nfat={nfat} fatsz={fsz} rootclus={root}")
    clus, chain = root, []
    while 2 <= clus < 0x0FFFFFF8 and len(chain) < 64:
        chain.append(clus); clus = fat(clus)
    for c in chain:
        f.seek(data_off + (c - 2) * spc * bps)
        blk = f.read(spc * bps)
        for i in range(0, len(blk), 32):
            e = blk[i:i+32]
            if e[0] == 0: break
            if e[0] == 0xE5 or e[11] & 0x0F == 0x0F: continue
            name = e[0:11].decode("latin-1").rstrip()
            first = (struct.unpack_from("<H", e, 20)[0] << 16) | struct.unpack_from("<H", e, 26)[0]
            size = struct.unpack_from("<I", e, 28)[0]
            state = "-"
            if first >= 2:
                v = fat(first)
                state = "free!" if v == 0 else f"fat={v:#x}"
            print(f"      {name:<12} attr={e[11]:#04x} clus={first:<6} size={size:<8} {state}")

for p in sys.argv[1:]:
    dump(p)
