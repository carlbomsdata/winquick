//! The desktop session's control channel: a raw disk with no filesystem on it.
//!
//! `winquick run` hands a command to the guest through a FAT volume, and for one
//! command per boot that works perfectly — the host writes before QEMU starts
//! and reads after the guest has dismounted.
//!
//! A live session breaks that assumption. The host writes while Windows still
//! has the volume mounted, so two independent FAT implementations hold
//! conflicting views of the same allocation tables: Windows flushes its cached
//! copy on dismount, the host writes over it, and the volume ends up genuinely
//! corrupt. Under load that showed up as commands the guest never saw and, once
//! the tables were damaged, as a session that stopped answering at all.
//!
//! So a session gets its own disk with no partition table and no filesystem.
//! Windows refuses to mount a partitionless fixed disk — normally an obstacle,
//! here exactly what is wanted — so it never caches anything about it, and the
//! guest reaches it with unbuffered reads and writes.
//!
//! # Layout
//!
//! | Offset | Contents |
//! |---|---|
//! | 0 | `WQCTLDSK` — how the guest recognises the disk |
//! | 1 MiB | request header, then the request payload |
//! | 16 MiB | response header, then the response payload |
//!
//! Both sides write the payload first and the header last, and each header is a
//! single 512-byte sector. A sector write is atomic at the device, so a header
//! the other side can read always refers to a payload that is already there.
//! That is the whole synchronisation story: no locking, and nothing that can be
//! observed half-written.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// Bumped whenever the on-disk request/response layout changes. A prepared
/// desktop state freezes a guest that speaks one particular version of this.
pub const PROTOCOL_VERSION: u32 = 1;

pub const SECTOR: u64 = 512;
pub const ID_OFFSET: u64 = 0;
pub const REQUEST_OFFSET: u64 = 1 << 20;
pub const RESPONSE_OFFSET: u64 = 16 << 20;
pub const DISK_BYTES: u64 = 48 << 20;
pub const MAX_PAYLOAD: usize = 8 << 20;

const DISK_MAGIC: &[u8; 8] = b"WQCTLDSK";
const REQ_MAGIC: &[u8; 8] = b"WQCTLREQ";
const RSP_MAGIC: &[u8; 8] = b"WQCTLRSP";

/// Create the disk and stamp it so the guest can find it among its devices.
pub fn create(path: &Path) -> Result<()> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("creating the control disk at {}", path.display()))?;
    f.set_len(DISK_BYTES)?;
    let mut f = f;

    let mut id = [0u8; SECTOR as usize];
    id[..8].copy_from_slice(DISK_MAGIC);
    write_at(&mut f, ID_OFFSET, &id)?;

    // Zero both headers, so a fresh session cannot read a previous one's state.
    write_at(&mut f, REQUEST_OFFSET, &[0u8; SECTOR as usize])?;
    write_at(&mut f, RESPONSE_OFFSET, &[0u8; SECTOR as usize])?;
    f.flush()?;
    Ok(())
}

pub struct Channel {
    disk: File,
    seq: u64,
}

impl Channel {
    /// Open the channel, continuing the sequence already on the disk.
    ///
    /// Each `winquick desktop` verb is a separate process, so the counter cannot
    /// live in memory: starting from zero every time makes every request reuse
    /// sequence 1. The guest, which only acts when the sequence differs from the
    /// one it last served, then ignores the request — and the host reads the
    /// previous response, which still carries a matching sequence. That is a
    /// wrong answer reported as a success, so the counter is read back from the
    /// disk instead.
    pub fn open(path: &Path) -> Result<Self> {
        let mut disk = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening the control disk {}", path.display()))?;
        let head = read_at(&mut disk, REQUEST_OFFSET, SECTOR as usize)?;
        let seq = if &head[..8] == REQ_MAGIC {
            u64::from_le_bytes(head[8..16].try_into().unwrap())
        } else {
            0
        };
        Ok(Self { disk, seq })
    }

    /// Send an argument vector and wait for the guest's answer.
    ///
    /// `alive` is polled so a guest that dies is reported as such rather than
    /// waited on until the timeout.
    pub fn call(
        &mut self,
        argv: &[String],
        timeout: Duration,
        alive: impl Fn() -> bool,
    ) -> Result<Response> {
        let payload = serde_json::to_vec(argv)?;
        if payload.len() > MAX_PAYLOAD {
            bail!("the command is too large for the control channel");
        }

        // Sequence numbers only ever go up, so an answer to an earlier command
        // can never be mistaken for this one's.
        self.seq += 1;
        let seq = self.seq;

        write_at(&mut self.disk, REQUEST_OFFSET + SECTOR, &payload)?;
        let mut head = [0u8; SECTOR as usize];
        head[..8].copy_from_slice(REQ_MAGIC);
        head[8..16].copy_from_slice(&seq.to_le_bytes());
        head[16..20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        write_at(&mut self.disk, REQUEST_OFFSET, &head)?;
        self.disk.flush()?;

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = self.poll_response(seq)? {
                return Ok(r);
            }
            if !alive() {
                bail!("the desktop session died while running `{}`", argv.join(" "));
            }
            if Instant::now() >= deadline {
                bail!(
                    "the desktop session did not answer `{}` within {}s",
                    argv.join(" "),
                    timeout.as_secs()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn poll_response(&mut self, want: u64) -> Result<Option<Response>> {
        let head = read_at(&mut self.disk, RESPONSE_OFFSET, SECTOR as usize)?;
        if &head[..8] != RSP_MAGIC {
            return Ok(None);
        }
        let seq = u64::from_le_bytes(head[8..16].try_into().unwrap());
        if seq != want {
            return Ok(None);
        }
        let len = u32::from_le_bytes(head[16..20].try_into().unwrap()) as usize;
        let code = i32::from_le_bytes(head[20..24].try_into().unwrap());
        if len > MAX_PAYLOAD {
            bail!("the guest reported a {len}-byte response, which cannot be right");
        }
        let body = if len == 0 {
            Vec::new()
        } else {
            let mut b = read_at(&mut self.disk, RESPONSE_OFFSET + SECTOR, len)?;
            b.truncate(len);
            b
        };
        Ok(Some(Response { exit_code: code, body }))
    }
}

#[derive(Debug)]
pub struct Response {
    pub exit_code: i32,
    pub body: Vec<u8>,
}

/// Unbuffered access on the guest side means whole sectors, so both sides round
/// up. The header records the true length.
fn write_at(f: &mut File, offset: u64, data: &[u8]) -> Result<()> {
    let rounded = data.len().div_ceil(SECTOR as usize) * SECTOR as usize;
    let mut buf = vec![0u8; rounded];
    buf[..data.len()].copy_from_slice(data);
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(&buf)?;
    Ok(())
}

fn read_at(f: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    let rounded = len.div_ceil(SECTOR as usize) * SECTOR as usize;
    let mut buf = vec![0u8; rounded];
    f.seek(SeekFrom::Start(offset))?;
    f.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wq-ctl-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("control.img")
    }

    /// Stand in for the guest: read the request, write the matching response.
    fn guest_reply(path: &Path, body: &str, code: i32) -> u64 {
        let mut f = OpenOptions::new().read(true).write(true).open(path).unwrap();
        let head = read_at(&mut f, REQUEST_OFFSET, SECTOR as usize).unwrap();
        assert_eq!(&head[..8], REQ_MAGIC);
        let seq = u64::from_le_bytes(head[8..16].try_into().unwrap());

        write_at(&mut f, RESPONSE_OFFSET + SECTOR, body.as_bytes()).unwrap();
        let mut rh = [0u8; SECTOR as usize];
        rh[..8].copy_from_slice(RSP_MAGIC);
        rh[8..16].copy_from_slice(&seq.to_le_bytes());
        rh[16..20].copy_from_slice(&(body.len() as u32).to_le_bytes());
        rh[20..24].copy_from_slice(&code.to_le_bytes());
        write_at(&mut f, RESPONSE_OFFSET, &rh).unwrap();
        seq
    }

    fn read_request(path: &Path) -> Vec<String> {
        let mut f = OpenOptions::new().read(true).open(path).unwrap();
        let head = read_at(&mut f, REQUEST_OFFSET, SECTOR as usize).unwrap();
        let len = u32::from_le_bytes(head[16..20].try_into().unwrap()) as usize;
        let mut body = read_at(&mut f, REQUEST_OFFSET + SECTOR, len).unwrap();
        body.truncate(len);
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn the_disk_is_stamped_so_the_guest_can_find_it() {
        let p = tmp("stamp");
        create(&p).unwrap();
        let mut f = File::open(&p).unwrap();
        let id = read_at(&mut f, ID_OFFSET, SECTOR as usize).unwrap();
        assert_eq!(&id[..8], DISK_MAGIC);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), DISK_BYTES);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// A partition table would make Windows mount the disk, cache it, and
    /// reintroduce exactly the corruption this channel exists to avoid.
    #[test]
    fn the_disk_has_no_partition_table() {
        let p = tmp("nopart");
        create(&p).unwrap();
        let mut f = File::open(&p).unwrap();
        let first = read_at(&mut f, 0, SECTOR as usize).unwrap();
        assert_ne!(
            (first[510], first[511]),
            (0x55, 0xAA),
            "an MBR signature would invite Windows to mount this"
        );
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn a_call_round_trips_argv_and_the_reply() {
        let p = tmp("roundtrip");
        create(&p).unwrap();
        let mut ch = Channel::open(&p).unwrap();

        let argv: Vec<String> = vec!["click".into(), "--automation-id".into(), "Save Button".into()];
        let path = p.clone();
        let t = std::thread::spawn(move || {
            // Wait for the request to land, then answer it.
            for _ in 0..200 {
                let mut f = File::open(&path).unwrap();
                let head = read_at(&mut f, REQUEST_OFFSET, SECTOR as usize).unwrap();
                if &head[..8] == REQ_MAGIC {
                    assert_eq!(read_request(&path), vec!["click", "--automation-id", "Save Button"]);
                    return guest_reply(&path, r#"{"ok":true}"#, 0);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("no request appeared");
        });

        let r = ch.call(&argv, Duration::from_secs(5), || true).unwrap();
        t.join().unwrap();
        assert_eq!(r.exit_code, 0);
        assert_eq!(String::from_utf8(r.body).unwrap(), r#"{"ok":true}"#);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// The point of the sequence number: an answer left over from an earlier
    /// command must never be read as this one's.
    #[test]
    fn a_stale_response_is_ignored() {
        let p = tmp("stale");
        create(&p).unwrap();
        let mut ch = Channel::open(&p).unwrap();

        // First call completes normally, leaving its response on the disk.
        let path = p.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            guest_reply(&path, r#"{"first":true}"#, 0)
        });
        let first = ch.call(&["a".to_string()], Duration::from_secs(5), || true).unwrap();
        t.join().unwrap();
        assert_eq!(String::from_utf8(first.body).unwrap(), r#"{"first":true}"#);

        // Second call, with the guest never answering: the old response is still
        // sitting there and must not satisfy it.
        let err = ch
            .call(&["b".to_string()], Duration::from_millis(300), || true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not answer"), "{err}");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// A payload that is not a whole number of sectors must come back with its
    /// exact length, not padded with the zeroes used to round the write up.
    #[test]
    fn payload_length_survives_sector_rounding() {
        let p = tmp("rounding");
        create(&p).unwrap();
        let mut ch = Channel::open(&p).unwrap();

        let body = "x".repeat(1000); // spans two sectors, ends mid-sector
        let path = p.clone();
        let expected = body.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            guest_reply(&path, &expected, 3)
        });
        let r = ch.call(&["a".to_string()], Duration::from_secs(5), || true).unwrap();
        t.join().unwrap();
        assert_eq!(r.exit_code, 3);
        assert_eq!(r.body.len(), 1000);
        assert_eq!(String::from_utf8(r.body).unwrap(), body);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Every verb is its own process, so the sequence has to survive one ending.
    /// If it restarts, the guest ignores the request as already served and the
    /// host reads the previous answer — a wrong result reported as a success.
    #[test]
    fn the_sequence_continues_across_processes() {
        let p = tmp("seq");
        create(&p).unwrap();

        let seqs: Vec<u64> = (0..3)
            .map(|i| {
                // A fresh Channel each time, as a new CLI invocation would have.
                let mut ch = Channel::open(&p).unwrap();
                let path = p.clone();
                let t = std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(20));
                    guest_reply(&path, &format!("{{\"n\":{i}}}"), 0)
                });
                let r = ch.call(&["v".to_string()], Duration::from_secs(5), || true).unwrap();
                let served = t.join().unwrap();
                assert_eq!(
                    String::from_utf8(r.body).unwrap(),
                    format!("{{\"n\":{i}}}"),
                    "call {i} got another call's answer"
                );
                served
            })
            .collect();

        assert!(
            seqs.windows(2).all(|w| w[1] > w[0]),
            "sequence must strictly increase across processes, got {seqs:?}"
        );
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// A dead guest should be reported immediately, not waited out.
    #[test]
    fn a_dead_guest_is_reported_rather_than_waited_for() {
        let p = tmp("dead");
        create(&p).unwrap();
        let mut ch = Channel::open(&p).unwrap();
        let start = Instant::now();
        let err = ch
            .call(&["a".to_string()], Duration::from_secs(30), || false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("died"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(5), "waited too long");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Requests and responses live far enough apart that neither can tread on
    /// the other, even at the maximum payload size.
    #[test]
    fn the_two_slots_cannot_overlap() {
        assert!(REQUEST_OFFSET + SECTOR + MAX_PAYLOAD as u64 <= RESPONSE_OFFSET);
        assert!(RESPONSE_OFFSET + SECTOR + MAX_PAYLOAD as u64 <= DISK_BYTES);
    }
}
