//! Just enough GPT to service a Windows image safely.
//!
//! Building the desktop image means attaching a *copy* of the Windows disk to a
//! running Windows guest and letting DISM write into it offline. That runs into
//! a rule which is easy to miss and fails silently:
//!
//! > Windows will not mount two disks that carry the same GPT disk GUID and the
//! > same partition GUIDs read-write. The duplicate is mounted read-only, and
//! > writes to it are discarded without an error.
//!
//! So the copy gets fresh GUIDs before servicing ([`randomize`]). But the
//! bootloader records the partition GUID it boots from, so shipping the image
//! with the new identity leaves it unbootable with
//! `0xc000000e \windows\system32\boot\winload.efi`. The original tables are
//! therefore captured first ([`snapshot`]) and put back afterwards
//! ([`restore`]).
//!
//! Only the two table regions are touched: the first 34 sectors, and the last
//! 33. Partition *contents* are never involved.

use anyhow::{bail, Context, Result};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const SECTOR: u64 = 512;
/// Protective MBR plus the primary header plus the standard 32-sector entry array.
const PRIMARY_SECTORS: u64 = 34;
/// The backup entry array plus the backup header, at the very end of the disk.
const BACKUP_SECTORS: u64 = 33;

const SIG: &[u8; 8] = b"EFI PART";

// Field offsets within a GPT header, from UEFI 2.10 table 5.5. These are easy
// to get wrong by one field, and doing so is invisible on a disk where the
// neighbouring value happens to look plausible.
const MY_LBA: usize = 24;
const ALT_LBA: usize = 32;

/// The GPT regions of a disk, enough to put a disk's identity back exactly as
/// it was.
pub struct Snapshot {
    primary: Vec<u8>,
    backup: Vec<u8>,
    disk_len: u64,
}

pub fn snapshot(disk: &Path) -> Result<Snapshot> {
    let mut f = OpenOptions::new()
        .read(true)
        .open(disk)
        .with_context(|| format!("opening {}", disk.display()))?;
    let disk_len = f.metadata()?.len();
    check_len(disk_len)?;

    let mut primary = vec![0u8; (PRIMARY_SECTORS * SECTOR) as usize];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut primary)?;
    if &primary[(SECTOR as usize)..(SECTOR as usize + 8)] != SIG {
        bail!("{} does not start with a GPT header", disk.display());
    }

    let mut backup = vec![0u8; (BACKUP_SECTORS * SECTOR) as usize];
    f.seek(SeekFrom::Start(disk_len - BACKUP_SECTORS * SECTOR))?;
    f.read_exact(&mut backup)?;

    Ok(Snapshot { primary, backup, disk_len })
}

pub fn restore(disk: &Path, snap: &Snapshot) -> Result<()> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(disk)
        .with_context(|| format!("opening {}", disk.display()))?;
    let len = f.metadata()?.len();
    if len != snap.disk_len {
        bail!(
            "cannot restore a partition table taken from a {}-byte disk onto a {len}-byte one",
            snap.disk_len
        );
    }
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&snap.primary)?;
    f.seek(SeekFrom::Start(len - BACKUP_SECTORS * SECTOR))?;
    f.write_all(&snap.backup)?;
    f.flush()?;
    Ok(())
}

/// Give the disk and every partition on it a fresh identity.
///
/// Both the primary and backup tables are rewritten, and all three CRCs are
/// recomputed — a stale CRC makes Windows treat the table as corrupt and
/// silently fall back to the backup, which would undo the whole point.
pub fn randomize(disk: &Path) -> Result<()> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(disk)
        .with_context(|| format!("opening {}", disk.display()))?;
    let disk_len = f.metadata()?.len();
    check_len(disk_len)?;

    let mut header = read_at(&mut f, SECTOR, SECTOR as usize)?;
    if &header[..8] != SIG {
        bail!("{} does not start with a GPT header", disk.display());
    }

    let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
    let count = u32::from_le_bytes(header[80..84].try_into().unwrap()) as usize;
    let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap()) as usize;
    if entry_size < 128 || count == 0 || count > 512 {
        bail!("unsupported GPT geometry: {count} entries of {entry_size} bytes");
    }

    let array_len = count * entry_size;
    let mut entries = read_at(&mut f, entries_lba * SECTOR, array_len)?;

    // A fresh unique GUID for every populated entry. An empty entry (all-zero
    // type GUID) stays empty; giving it an identity would invent a partition.
    for i in 0..count {
        let off = i * entry_size;
        if entries[off..off + 16].iter().all(|&b| b == 0) {
            continue;
        }
        entries[off + 16..off + 32].copy_from_slice(&random_guid()?);
    }
    let array_crc = crc32(&entries);
    let disk_guid = random_guid()?;

    // Primary header.
    header[56..72].copy_from_slice(&disk_guid);
    header[88..92].copy_from_slice(&array_crc.to_le_bytes());
    reseal(&mut header);

    // Backup header: same identity, but MyLBA and AlternateLBA are swapped and
    // it points at its own copy of the entry array.
    let my_lba = u64::from_le_bytes(header[MY_LBA..MY_LBA + 8].try_into().unwrap());
    let alt_lba = u64::from_le_bytes(header[ALT_LBA..ALT_LBA + 8].try_into().unwrap());
    let mut backup = read_at(&mut f, alt_lba * SECTOR, SECTOR as usize)?;
    if &backup[..8] != SIG {
        bail!("no backup GPT header at LBA {alt_lba}");
    }
    let backup_entries_lba = u64::from_le_bytes(backup[72..80].try_into().unwrap());
    backup[56..72].copy_from_slice(&disk_guid);
    backup[88..92].copy_from_slice(&array_crc.to_le_bytes());
    backup[MY_LBA..MY_LBA + 8].copy_from_slice(&alt_lba.to_le_bytes());
    backup[ALT_LBA..ALT_LBA + 8].copy_from_slice(&my_lba.to_le_bytes());
    reseal(&mut backup);

    write_at(&mut f, entries_lba * SECTOR, &entries)?;
    write_at(&mut f, backup_entries_lba * SECTOR, &entries)?;
    write_at(&mut f, SECTOR, &header)?;
    write_at(&mut f, alt_lba * SECTOR, &backup)?;
    f.flush()?;
    Ok(())
}

/// The GUID that marks a Microsoft basic data partition, in GPT's mixed-endian
/// layout. Validation OS puts the Windows volume in one of these.
const BASIC_DATA: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];

/// Where the Windows volume starts inside a whole-disk image, in bytes.
///
/// This is what lets the ntfsprogs helpers work on the image file itself
/// instead of on a partition device node. macOS could produce such a node with
/// `hdiutil attach -nomount`; Windows has no equivalent that does not require
/// elevation and a virtual-disk driver, and endpoint security software blocks
/// that route in practice. Reading the partition table and passing an offset
/// works the same way on both, needs no privileges, and touches nothing outside
/// the file.
///
/// A Validation OS disk carries several Microsoft partitions; the Windows
/// volume is the large basic data one, so the largest is what gets picked
/// rather than a fixed index.
pub fn windows_volume_offset(disk: &Path) -> Result<u64> {
    let mut f = std::fs::File::open(disk)
        .with_context(|| format!("opening {}", disk.display()))?;
    check_len(f.metadata()?.len())?;

    let header = read_at(&mut f, SECTOR, SECTOR as usize)?;
    if &header[..8] != SIG {
        bail!("{} does not start with a GPT header", disk.display());
    }
    let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
    let count = u32::from_le_bytes(header[80..84].try_into().unwrap()) as usize;
    let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap()) as usize;
    if entry_size < 128 || count == 0 || count > 512 {
        bail!("unsupported GPT geometry: {count} entries of {entry_size} bytes");
    }
    let entries = read_at(&mut f, entries_lba * SECTOR, count * entry_size)?;

    let mut best: Option<(u64, u64)> = None; // (sectors, offset)
    for i in 0..count {
        let e = &entries[i * entry_size..i * entry_size + entry_size];
        if e[..16] != BASIC_DATA {
            continue;
        }
        let first = u64::from_le_bytes(e[32..40].try_into().unwrap());
        let last = u64::from_le_bytes(e[40..48].try_into().unwrap());
        if last < first {
            continue;
        }
        let sectors = last - first + 1;
        if best.map_or(true, |(b, _)| sectors > b) {
            best = Some((sectors, first * SECTOR));
        }
    }
    best.map(|(_, off)| off).ok_or_else(|| {
        anyhow::anyhow!("{} has no basic data partition to write into", disk.display())
    })
}

/// Recompute a header's own CRC, which is defined over the header with the CRC
/// field zeroed.
fn reseal(header: &mut [u8]) {
    let size = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    let size = size.clamp(92, header.len());
    header[16..20].copy_from_slice(&[0; 4]);
    let crc = crc32(&header[..size]);
    header[16..20].copy_from_slice(&crc.to_le_bytes());
}

fn check_len(len: u64) -> Result<()> {
    if len < (PRIMARY_SECTORS + BACKUP_SECTORS) * SECTOR {
        bail!("disk is too small to hold a partition table");
    }
    Ok(())
}

fn read_at(f: &mut std::fs::File, off: u64, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    f.seek(SeekFrom::Start(off))?;
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_at(f: &mut std::fs::File, off: u64, data: &[u8]) -> Result<()> {
    f.seek(SeekFrom::Start(off))?;
    f.write_all(data)?;
    Ok(())
}

/// A random RFC 4122 version 4 GUID in the mixed-endian layout GPT uses.
///
/// The byte order does not matter for uniqueness, but the version and variant
/// bits do: tools that parse the GUID will reject one that claims a version it
/// does not have.
fn random_guid() -> Result<[u8; 16]> {
    let mut b = [0u8; 16];
    crate::hostfs::fill_random(&mut b)?;
    b[7] = (b[7] & 0x0F) | 0x40; // version 4, in the little-endian time_hi field
    b[8] = (b[8] & 0x3F) | 0x80; // RFC 4122 variant
    Ok(b)
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY_SIZE: usize = 128;
    const ENTRY_COUNT: usize = 4;
    const DISK_SECTORS: u64 = 128;

    /// A minimal but structurally valid GPT: protective MBR, primary header and
    /// entry array at the front, backup array and header at the back.
    fn make_disk(path: &Path) {
        let len = DISK_SECTORS * SECTOR;
        let mut disk = vec![0u8; len as usize];

        let mut entries = vec![0u8; ENTRY_COUNT * ENTRY_SIZE];
        // Two populated partitions, two empty ones.
        for i in 0..2 {
            let off = i * ENTRY_SIZE;
            entries[off..off + 16].copy_from_slice(&[0xAA; 16]); // type GUID
            entries[off + 16..off + 32].copy_from_slice(&[0xBB + i as u8; 16]); // unique GUID
        }
        let array_crc = crc32(&entries);

        let backup_lba = DISK_SECTORS - 1;
        let backup_entries_lba = backup_lba - 32;

        let build_header = |my: u64, alt: u64, entries_lba: u64| -> Vec<u8> {
            let mut h = vec![0u8; SECTOR as usize];
            h[..8].copy_from_slice(SIG);
            h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // revision
            h[12..16].copy_from_slice(&92u32.to_le_bytes()); // header size
            h[MY_LBA..MY_LBA + 8].copy_from_slice(&my.to_le_bytes());
            h[ALT_LBA..ALT_LBA + 8].copy_from_slice(&alt.to_le_bytes());
            h[40..48].copy_from_slice(&34u64.to_le_bytes()); // FirstUsableLBA
            h[48..56].copy_from_slice(&(DISK_SECTORS - 34).to_le_bytes()); // LastUsableLBA
            h[56..72].copy_from_slice(&[0xCC; 16]); // disk GUID
            h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
            h[80..84].copy_from_slice(&(ENTRY_COUNT as u32).to_le_bytes());
            h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
            h[88..92].copy_from_slice(&array_crc.to_le_bytes());
            let mut h2 = h.clone();
            reseal(&mut h2);
            h2
        };

        let primary = build_header(1, backup_lba, 2);
        let backup = build_header(backup_lba, 1, backup_entries_lba);

        disk[510] = 0x55;
        disk[511] = 0xAA;
        disk[SECTOR as usize..(SECTOR as usize + SECTOR as usize)].copy_from_slice(&primary);
        let eoff = (2 * SECTOR) as usize;
        disk[eoff..eoff + entries.len()].copy_from_slice(&entries);
        let boff = (backup_entries_lba * SECTOR) as usize;
        disk[boff..boff + entries.len()].copy_from_slice(&entries);
        let hoff = (backup_lba * SECTOR) as usize;
        disk[hoff..hoff + SECTOR as usize].copy_from_slice(&backup);

        std::fs::write(path, &disk).unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wq-gpt-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("disk.img")
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    fn header_crc_valid(disk: &[u8], lba: u64) -> bool {
        let off = (lba * SECTOR) as usize;
        let h = &disk[off..off + SECTOR as usize];
        let size = u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize;
        let stored = u32::from_le_bytes(h[16..20].try_into().unwrap());
        let mut copy = h[..size].to_vec();
        copy[16..20].copy_from_slice(&[0; 4]);
        crc32(&copy) == stored
    }

    #[test]
    fn snapshot_then_restore_is_byte_identical() {
        let p = tmp("roundtrip");
        make_disk(&p);
        let before = read(&p);
        let snap = snapshot(&p).unwrap();

        randomize(&p).unwrap();
        assert_ne!(read(&p), before, "randomize must actually change the disk");

        restore(&p, &snap).unwrap();
        assert_eq!(read(&p), before, "restore must put the original identity back");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// The whole point of randomising: Windows compares these, and a collision
    /// means writes to the serviced copy are silently discarded.
    #[test]
    fn randomize_changes_disk_and_partition_guids() {
        let p = tmp("guids");
        make_disk(&p);
        let before = read(&p);
        randomize(&p).unwrap();
        let after = read(&p);

        let hoff = SECTOR as usize;
        assert_ne!(
            &before[hoff + 56..hoff + 72],
            &after[hoff + 56..hoff + 72],
            "disk GUID unchanged"
        );
        for i in 0..2 {
            let off = (2 * SECTOR) as usize + i * ENTRY_SIZE;
            assert_ne!(
                &before[off + 16..off + 32],
                &after[off + 16..off + 32],
                "partition {i} GUID unchanged"
            );
            assert_eq!(
                &before[off..off + 16],
                &after[off..off + 16],
                "partition {i} type GUID must not change"
            );
        }
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// An empty slot must stay empty; inventing a GUID there would invent a
    /// partition.
    #[test]
    fn randomize_leaves_empty_entries_alone() {
        let p = tmp("empty");
        make_disk(&p);
        randomize(&p).unwrap();
        let after = read(&p);
        for i in 2..ENTRY_COUNT {
            let off = (2 * SECTOR) as usize + i * ENTRY_SIZE;
            assert!(
                after[off..off + ENTRY_SIZE].iter().all(|&b| b == 0),
                "empty entry {i} was written to"
            );
        }
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// A stale CRC makes Windows fall back to the backup table, undoing the
    /// randomisation without any visible error.
    #[test]
    fn randomize_reseals_every_checksum() {
        let p = tmp("crc");
        make_disk(&p);
        randomize(&p).unwrap();
        let disk = read(&p);

        assert!(header_crc_valid(&disk, 1), "primary header CRC is stale");
        assert!(
            header_crc_valid(&disk, DISK_SECTORS - 1),
            "backup header CRC is stale"
        );

        let entries = &disk[(2 * SECTOR) as usize..(2 * SECTOR) as usize + ENTRY_COUNT * ENTRY_SIZE];
        let expect = crc32(entries);
        for lba in [1, DISK_SECTORS - 1] {
            let h = (lba * SECTOR) as usize;
            assert_eq!(
                u32::from_le_bytes(disk[h + 88..h + 92].try_into().unwrap()),
                expect,
                "entry-array CRC in header at LBA {lba} is stale"
            );
        }
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Both copies of the array have to agree, or Windows repairs one from the
    /// other and the identities diverge.
    #[test]
    fn randomize_keeps_both_entry_arrays_in_step() {
        let p = tmp("mirror");
        make_disk(&p);
        randomize(&p).unwrap();
        let disk = read(&p);
        let len = ENTRY_COUNT * ENTRY_SIZE;
        let primary = &disk[(2 * SECTOR) as usize..(2 * SECTOR) as usize + len];
        let boff = ((DISK_SECTORS - 33) * SECTOR) as usize;
        assert_eq!(primary, &disk[boff..boff + len], "backup entry array diverged");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Restoring onto a differently sized disk would write the backup table into
    /// the wrong place; refuse rather than corrupt.
    #[test]
    fn restore_refuses_a_resized_disk() {
        let p = tmp("resize");
        make_disk(&p);
        let snap = snapshot(&p).unwrap();
        let f = OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(DISK_SECTORS * SECTOR + SECTOR).unwrap();
        assert!(restore(&p, &snap).is_err());
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// The backup header must keep pointing at the primary and vice versa.
    ///
    /// Reading these two fields one slot too late still yields plausible-looking
    /// LBAs, so assert against the spec offsets rather than against whatever the
    /// fixture happens to write.
    #[test]
    fn randomize_keeps_the_header_cross_references() {
        let p = tmp("xref");
        make_disk(&p);
        randomize(&p).unwrap();
        let disk = read(&p);

        let prim = (SECTOR) as usize;
        let back = ((DISK_SECTORS - 1) * SECTOR) as usize;
        let lba = |base: usize, off: usize| {
            u64::from_le_bytes(disk[base + off..base + off + 8].try_into().unwrap())
        };
        assert_eq!(lba(prim, MY_LBA), 1);
        assert_eq!(lba(prim, ALT_LBA), DISK_SECTORS - 1);
        assert_eq!(lba(back, MY_LBA), DISK_SECTORS - 1);
        assert_eq!(lba(back, ALT_LBA), 1);
        // FirstUsableLBA must survive untouched; overwriting it is the classic
        // off-by-one-field mistake.
        assert_eq!(lba(prim, 40), 34);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Known-answer check so a broken CRC cannot pass by agreeing with itself.
    #[test]
    fn crc32_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }
}
