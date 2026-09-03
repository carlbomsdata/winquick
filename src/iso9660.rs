//! Just enough ISO 9660 to copy files out of a disc image.
//!
//! WinQuick reads two kinds of image. Microsoft's Validation OS media is UDF,
//! which `udf.rs` handles. Red Hat's virtio-win disc, which the desktop
//! capability takes two drivers from, is plain ISO 9660 -- and mounting it
//! meant `hdiutil`, so on any host but macOS the desktop build stopped there.
//!
//! Only what that needs: the primary volume descriptor, directory records, and
//! file extents. No Rock Ridge, no Joliet -- the names wanted here are already
//! 8.3-clean, and the primary descriptor spells them in upper case.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const SECTOR: u64 = 2048;

pub struct Image {
    file: File,
    root_extent: u32,
    root_len: u32,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    extent: u32,
    length: u32,
}

impl Image {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        // Volume descriptors start at sector 16 and run until the terminator.
        for sector in 16..32 {
            let mut buf = [0u8; SECTOR as usize];
            file.seek(SeekFrom::Start(sector * SECTOR))?;
            if file.read_exact(&mut buf).is_err() {
                break;
            }
            if &buf[1..6] != b"CD001" {
                continue;
            }
            match buf[0] {
                1 => {
                    // Primary volume descriptor. The root directory record sits
                    // at offset 156 and is itself a directory record.
                    let rec = &buf[156..190];
                    return Ok(Image {
                        file,
                        root_extent: u32::from_le_bytes(rec[2..6].try_into().unwrap()),
                        root_len: u32::from_le_bytes(rec[10..14].try_into().unwrap()),
                    });
                }
                255 => break, // terminator
                _ => continue,
            }
        }
        bail!("{} has no ISO 9660 filesystem", path.display())
    }

    fn read_at(&mut self, extent: u32, length: u32) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; length as usize];
        self.file.seek(SeekFrom::Start(extent as u64 * SECTOR))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn parse_dir(&mut self, extent: u32, length: u32) -> Result<Vec<Entry>> {
        let data = self.read_at(extent, length)?;
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + 33 <= data.len() {
            let len = data[i] as usize;
            if len == 0 {
                // Records never straddle a sector; skip to the next one.
                i = (i / SECTOR as usize + 1) * SECTOR as usize;
                continue;
            }
            if i + len > data.len() {
                break;
            }
            let rec = &data[i..i + len];
            let ext = u32::from_le_bytes(rec[2..6].try_into().unwrap());
            let size = u32::from_le_bytes(rec[10..14].try_into().unwrap());
            let flags = rec[25];
            let name_len = rec[32] as usize;
            if 33 + name_len <= rec.len() {
                let raw = &rec[33..33 + name_len];
                // 0 and 1 are "." and ".."; everything else is a real name,
                // minus the ";1" version suffix ISO 9660 puts on files.
                if !(name_len == 1 && (raw[0] == 0 || raw[0] == 1)) {
                    let mut name = String::from_utf8_lossy(raw).to_string();
                    if let Some(p) = name.find(';') {
                        name.truncate(p);
                    }
                    out.push(Entry { name, is_dir: flags & 0x02 != 0, extent: ext, length: size });
                }
            }
            i += len;
        }
        Ok(out)
    }

    pub fn root(&mut self) -> Result<Vec<Entry>> {
        let (e, l) = (self.root_extent, self.root_len);
        self.parse_dir(e, l)
    }

    pub fn list(&mut self, entry: &Entry) -> Result<Vec<Entry>> {
        if !entry.is_dir {
            bail!("{} is not a directory", entry.name);
        }
        self.parse_dir(entry.extent, entry.length)
    }

    pub fn extract(&mut self, entry: &Entry, dest: &Path) -> Result<u64> {
        if entry.is_dir {
            bail!("{} is a directory", entry.name);
        }
        let data = self.read_at(entry.extent, entry.length)?;
        let mut out = File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
        out.write_all(&data)?;
        out.flush()?;
        Ok(data.len() as u64)
    }

    pub fn extract_tree(&mut self, entry: &Entry, dest: &Path) -> Result<u64> {
        std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
        let mut total = 0;
        for child in self.list(entry)? {
            let target = dest.join(&child.name);
            total += if child.is_dir {
                self.extract_tree(&child, &target)?
            } else {
                self.extract(&child, &target)?
            };
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_iso9660_is_reported_as_such() {
        let p = std::env::temp_dir().join(format!("wq-iso-{}", std::process::id()));
        std::fs::write(&p, vec![0u8; 2048 * 40]).unwrap();
        let e = match Image::open(&p) {
            Ok(_) => panic!("a file of zeroes must not open as ISO 9660"),
            Err(e) => e.to_string(),
        };
        assert!(e.contains("no ISO 9660"), "{e}");
        let _ = std::fs::remove_file(&p);
    }
}
