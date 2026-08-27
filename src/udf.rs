//! Just enough UDF to take one file off Microsoft's media.
//!
//! Validation OS ships as a **UDF bridge disc**: there is an ISO 9660
//! filesystem on it, but it contains a single `README.TXT` explaining that the
//! real content is in UDF. Everything WinQuick needs -- `ValidationOS.vhdx` --
//! lives there.
//!
//! macOS could mount it. Windows could too, with `Mount-DiskImage`, which needs
//! elevation and which endpoint security software blocks in practice -- the
//! same wall the image preparation ran into. So WinQuick reads the filesystem
//! itself. It needs no privileges, works identically on both hosts, and cannot
//! leave a mount behind.
//!
//! This is deliberately the smallest reader that does the job: find the volume,
//! read the root directory, copy one file out. It does not implement UDF. In
//! particular there is no support for subdirectories, named streams, metadata
//! partitions or compressed extents -- none of which appear on this media, and
//! each of which is reported as unsupported rather than guessed at.
//!
//! ```text
//!   sector 256      Anchor Volume Descriptor Pointer  -> where the descriptors are
//!   main VDS        Partition Descriptor              -> where the partition starts
//!                   Logical Volume Descriptor         -> where the file set is
//!   file set        root directory ICB                -> the root's File Entry
//!   File Entry      allocation descriptors            -> the directory contents
//!   directory       File Identifier Descriptors       -> names, and each file's ICB
//! ```

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// UDF's logical sector size on optical media. Fixed for this purpose: the
/// descriptors that would say otherwise are themselves found by assuming it.
const SECTOR: u64 = 2048;

// Descriptor tag identifiers, ECMA-167 3/7.2.1 and 4/7.2.1.
const TAG_ANCHOR: u16 = 2;
const TAG_PARTITION: u16 = 5;
const TAG_LOGICAL_VOLUME: u16 = 6;
const TAG_TERMINATING: u16 = 8;
const TAG_FILE_SET: u16 = 256;
const TAG_FILE_IDENTIFIER: u16 = 257;
const TAG_FILE_ENTRY: u16 = 261;
const TAG_EXTENDED_FILE_ENTRY: u16 = 266;

/// One entry in the root directory.
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// Partition-relative block of this entry's File Entry.
    icb: u32,
}

/// An opened UDF volume, positioned to read files out of its root directory.
pub struct Volume {
    file: File,
    /// Absolute block where the partition starts; everything else is relative
    /// to it.
    partition: u32,
    /// Partition-relative block of the file set descriptor.
    file_set: u32,
}

impl Volume {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file =
            File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let blocks = file.metadata()?.len() / SECTOR;
        if blocks < 300 {
            bail!("{} is too small to be Microsoft installation media", path.display());
        }

        // The anchor is required at 256, and mirrored near the end. Any of the
        // three will do, and a disc that has none is not UDF.
        let mut vds = None;
        for lba in [256, blocks.saturating_sub(256), blocks.saturating_sub(1)] {
            let d = read_block(&mut file, lba, 1)?;
            if tag_of(&d) == TAG_ANCHOR {
                let len = u32::from_le_bytes(d[16..20].try_into().unwrap());
                let loc = u32::from_le_bytes(d[20..24].try_into().unwrap());
                vds = Some((loc, len));
                break;
            }
        }
        let Some((vds_loc, vds_len)) = vds else {
            bail!(
                "{} has no UDF filesystem.\n\n\
                 Microsoft's Validation OS media is a UDF disc; this file does not look like one.",
                path.display()
            );
        };

        // The volume descriptor sequence names the partition and the file set.
        let mut partition = None;
        let mut file_set = None;
        for i in 0..(vds_len as u64 / SECTOR) {
            let d = read_block(&mut file, vds_loc as u64 + i, 1)?;
            match tag_of(&d) {
                TAG_PARTITION => {
                    partition = Some(u32::from_le_bytes(d[188..192].try_into().unwrap()));
                }
                TAG_LOGICAL_VOLUME => {
                    // The file set is a long_ad: length, then block.
                    file_set = Some(u32::from_le_bytes(d[252..256].try_into().unwrap()));
                }
                TAG_TERMINATING => break,
                _ => {}
            }
        }
        let (Some(partition), Some(file_set)) = (partition, file_set) else {
            bail!("{} has a UDF anchor but no usable partition", path.display());
        };

        let mut v = Volume { file, partition, file_set };
        // The file set descriptor must actually be there, or the offsets above
        // are being read out of something that is not UDF after all.
        let fsd = v.read_partition(file_set as u64, 1)?;
        if tag_of(&fsd) != TAG_FILE_SET {
            bail!("{} has no UDF file set where its volume descriptor says", path.display());
        }
        Ok(v)
    }

    /// Everything in the root directory.
    pub fn root(&mut self) -> Result<Vec<Entry>> {
        let fsd = self.read_partition(self.file_set as u64, 1)?;
        // Root directory ICB: a long_ad at offset 400.
        let root_icb = u32::from_le_bytes(fsd[404..408].try_into().unwrap());
        let dir = self.read_file_data(root_icb)?;
        parse_directory(&dir)
    }

    /// Copy one entry's contents to `dest`, returning how many bytes were written.
    ///
    /// Streamed a block at a time: the payload is roughly a gigabyte and there
    /// is no reason for it to pass through memory.
    pub fn extract(&mut self, entry: &Entry, dest: &Path) -> Result<u64> {
        if entry.is_dir {
            bail!("{} is a directory", entry.name);
        }
        let (size, extents) = self.file_extents(entry.icb)?;
        let mut out = File::create(dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        let mut left = size;
        for (len, block) in extents {
            let mut remaining = len.min(left);
            let mut at = block as u64;
            while remaining > 0 {
                let want = remaining.min(SECTOR * 512);
                let blocks = want.div_ceil(SECTOR);
                let buf = self.read_partition(at, blocks)?;
                let take = (want as usize).min(buf.len());
                out.write_all(&buf[..take])?;
                remaining -= take as u64;
                left -= take as u64;
                at += blocks;
            }
            if left == 0 {
                break;
            }
        }
        out.flush()?;
        Ok(size)
    }

    /// A File Entry's size and where its data lives.
    fn file_extents(&mut self, icb: u32) -> Result<(u64, Vec<(u64, u32)>)> {
        let d = self.read_partition(icb as u64, 1)?;
        let tag = tag_of(&d);
        // Both entry kinds carry the same fields; only the header length differs.
        let (l_ea, l_ad, base) = match tag {
            TAG_FILE_ENTRY => (
                u32::from_le_bytes(d[168..172].try_into().unwrap()) as usize,
                u32::from_le_bytes(d[172..176].try_into().unwrap()) as usize,
                176usize,
            ),
            TAG_EXTENDED_FILE_ENTRY => (
                u32::from_le_bytes(d[208..212].try_into().unwrap()) as usize,
                u32::from_le_bytes(d[212..216].try_into().unwrap()) as usize,
                216usize,
            ),
            other => bail!("unexpected UDF descriptor {other} where a file entry was expected"),
        };
        let size = u64::from_le_bytes(d[56..64].try_into().unwrap());
        // ICB flags, low three bits: how the allocation descriptors are written.
        let kind = u16::from_le_bytes(d[34..36].try_into().unwrap()) & 7;
        let start = base + l_ea;
        let end = start + l_ad;
        if end > d.len() {
            bail!("UDF file entry claims more allocation descriptors than it has room for");
        }
        let ads = &d[start..end];

        let mut out = Vec::new();
        match kind {
            // Short allocation descriptors: length, then block.
            0 => {
                for c in ads.chunks_exact(8) {
                    let raw = u32::from_le_bytes(c[0..4].try_into().unwrap());
                    let len = (raw & 0x3FFF_FFFF) as u64;
                    if len == 0 {
                        break;
                    }
                    if raw >> 30 == 3 {
                        bail!("this UDF file is stored in extents WinQuick cannot follow");
                    }
                    out.push((len, u32::from_le_bytes(c[4..8].try_into().unwrap())));
                }
            }
            // Long allocation descriptors: length, block, partition, then use.
            1 => {
                for c in ads.chunks_exact(16) {
                    let raw = u32::from_le_bytes(c[0..4].try_into().unwrap());
                    let len = (raw & 0x3FFF_FFFF) as u64;
                    if len == 0 {
                        break;
                    }
                    if raw >> 30 == 3 {
                        bail!("this UDF file is stored in extents WinQuick cannot follow");
                    }
                    out.push((len, u32::from_le_bytes(c[4..8].try_into().unwrap())));
                }
            }
            3 => bail!("this UDF file is stored inside its own descriptor, which WinQuick cannot read"),
            other => bail!("unsupported UDF allocation descriptor type {other}"),
        }
        Ok((size, out))
    }

    /// A File Entry's contents, in memory. Used for directories, which are small.
    fn read_file_data(&mut self, icb: u32) -> Result<Vec<u8>> {
        let (size, extents) = self.file_extents(icb)?;
        if size > 16 * 1024 * 1024 {
            bail!("the UDF root directory is implausibly large ({size} bytes)");
        }
        let mut out = Vec::with_capacity(size as usize);
        for (len, block) in extents {
            let blocks = len.div_ceil(SECTOR);
            let buf = self.read_partition(block as u64, blocks)?;
            let take = (len as usize).min(buf.len());
            out.extend_from_slice(&buf[..take]);
        }
        out.truncate(size as usize);
        Ok(out)
    }

    fn read_partition(&mut self, block: u64, count: u64) -> Result<Vec<u8>> {
        read_block(&mut self.file, self.partition as u64 + block, count)
    }
}

fn read_block(f: &mut File, lba: u64, count: u64) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; (SECTOR * count) as usize];
    f.seek(SeekFrom::Start(lba * SECTOR))?;
    // Short reads are normal at the end of the image; the caller only ever
    // looks at the part it asked for.
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

fn tag_of(b: &[u8]) -> u16 {
    if b.len() < 2 {
        return 0;
    }
    u16::from_le_bytes(b[0..2].try_into().unwrap())
}

/// Walk a directory's File Identifier Descriptors.
///
/// ```text
///   0  descriptor tag (16 bytes)
///  16  file version number
///  18  file characteristics   bit 1: this entry is a directory
///  19  length of file identifier
///  20  ICB (long_ad, 16 bytes) -- block is at +4
///  36  length of implementation use
///  38  implementation use, then the identifier, then padding to 4 bytes
/// ```
fn parse_directory(data: &[u8]) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 38 <= data.len() {
        if tag_of(&data[off..]) != TAG_FILE_IDENTIFIER {
            break;
        }
        let chars = data[off + 18];
        let l_fi = data[off + 19] as usize;
        let icb = u32::from_le_bytes(data[off + 24..off + 28].try_into().unwrap());
        let l_iu = u16::from_le_bytes(data[off + 36..off + 38].try_into().unwrap()) as usize;
        let name_at = off + 38 + l_iu;
        if name_at + l_fi > data.len() {
            break;
        }
        let raw = &data[name_at..name_at + l_fi];
        // A zero-length identifier is the entry for the parent directory.
        if !raw.is_empty() && chars & 0x08 == 0 {
            out.push(Entry {
                name: decode_name(raw),
                is_dir: chars & 0x02 != 0,
                icb,
            });
        }
        let mut total = 38 + l_iu + l_fi;
        total += (4 - total % 4) % 4;
        if total == 0 {
            break;
        }
        off += total;
    }
    Ok(out)
}

/// UDF names carry their encoding in the first byte: 8 means Latin-1, 16 means
/// big-endian UTF-16.
fn decode_name(raw: &[u8]) -> String {
    match raw[0] {
        16 => {
            let units: Vec<u16> = raw[1..]
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => raw[1..].iter().map(|&b| b as char).collect(),
    }
}

/// Take one file out of a UDF image by name, case-insensitively.
pub fn extract_file(iso: &Path, name: &str, dest: &Path) -> Result<u64> {
    let mut v = Volume::open(iso)?;
    let entries = v.root()?;
    let entry = entries
        .iter()
        .find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            anyhow::anyhow!(
                "{} does not contain {name}.\n\nIt holds: {}",
                iso.display(),
                names.join(", ")
            )
        })?;
    v.extract(entry, dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_udf_is_reported_as_such() {
        let p = std::env::temp_dir().join(format!("wq-udf-{}", std::process::id()));
        std::fs::write(&p, vec![0u8; 2048 * 400]).unwrap();
        let e = match Volume::open(&p) {
            Ok(_) => panic!("a file of zeroes must not open as UDF"),
            Err(e) => e.to_string(),
        };
        assert!(e.contains("no UDF filesystem"), "unexpected error: {e}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_tiny_file_is_not_mistaken_for_media() {
        let p = std::env::temp_dir().join(format!("wq-udf-small-{}", std::process::id()));
        std::fs::write(&p, b"not an iso").unwrap();
        assert!(Volume::open(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    /// The parent-directory entry has no name and must not appear as a file,
    /// or callers would try to extract it.
    #[test]
    fn the_parent_entry_is_skipped() {
        let mut d = vec![0u8; 40];
        d[0..2].copy_from_slice(&TAG_FILE_IDENTIFIER.to_le_bytes());
        d[18] = 0x0A; // parent + directory
        d[19] = 0; // no identifier
        assert!(parse_directory(&d).unwrap().is_empty());
    }

    #[test]
    fn utf16_names_decode() {
        let mut raw = vec![16u8];
        for c in "Ä.vhdx".encode_utf16() {
            raw.extend_from_slice(&c.to_be_bytes());
        }
        assert_eq!(decode_name(&raw), "Ä.vhdx");
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// Not a correctness test: a timing check against real media, run by hand.
    ///
    ///     WQ_UDF_ISO=~/.winquick/cache/validationos-arm64.iso \
    ///       cargo test --release udf_extract_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn udf_extract_speed() {
        let Some(iso) = std::env::var_os("WQ_UDF_ISO") else {
            eprintln!("set WQ_UDF_ISO to a Validation OS image");
            return;
        };
        let iso = std::path::PathBuf::from(iso);
        let dest = std::env::temp_dir().join("wq-udf-bench.vhdx");
        let t = std::time::Instant::now();
        let n = extract_file(&iso, "ValidationOS.vhdx", &dest).expect("extract");
        let secs = t.elapsed().as_secs_f64();
        eprintln!(
            "extracted {n} bytes in {secs:.2}s ({:.1} MB/s)",
            n as f64 / secs / 1e6
        );
        let _ = std::fs::remove_file(&dest);
    }
}
