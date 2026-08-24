//! A very small QMP client.
//!
//! Only what the runner needs: negotiate, stop, migrate, cont, and wait for a
//! migration to finish. QMP interleaves asynchronous events with command
//! replies, so every read loop has to skip anything without a `return` or
//! `error` key.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct Qmp {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Qmp {
    /// Connect once the socket shows up. QEMU creates it a moment after exec.
    pub fn connect(socket: &Path, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            if socket.exists() {
                if let Ok(s) = UnixStream::connect(socket) {
                    let writer = s.try_clone()?;
                    let mut q = Qmp { reader: BufReader::new(s), writer };
                    q.read_greeting()?;
                    q.command("qmp_capabilities", json!({}))?;
                    return Ok(q);
                }
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for the QMP socket at {}", socket.display());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn read_greeting(&mut self) -> Result<()> {
        let mut line = String::new();
        self.reader.read_line(&mut line).context("reading QMP greeting")?;
        if line.trim().is_empty() {
            bail!("QEMU closed the QMP connection immediately");
        }
        Ok(())
    }

    pub fn command(&mut self, name: &str, args: Value) -> Result<Value> {
        let msg = if args.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            json!({ "execute": name })
        } else {
            json!({ "execute": name, "arguments": args })
        };
        writeln!(self.writer, "{msg}").with_context(|| format!("sending QMP {name}"))?;
        self.writer.flush()?;
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .with_context(|| format!("reading reply to QMP {name}"))?;
            if n == 0 {
                bail!("QEMU closed the QMP connection during {name}");
            }
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(e) = v.get("error") {
                bail!("QMP {name} failed: {e}");
            }
            if let Some(r) = v.get("return") {
                return Ok(r.clone());
            }
            // an asynchronous event; keep reading
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        self.command("stop", json!({})).map(|_| ())
    }

    pub fn cont(&mut self) -> Result<()> {
        self.command("cont", json!({})).map(|_| ())
    }

    /// Save RAM and device state to `path`, blocking until it completes.
    ///
    /// Deliberately migration rather than `savevm`: `savevm` demands that every
    /// writable block device support snapshots (the raw mailbox does not), and
    /// it picks which device stores the state, which it gets wrong here.
    pub fn migrate_to_file(&mut self, path: &Path, timeout: Duration) -> Result<()> {
        self.command("migrate", json!({ "uri": format!("file:{}", path.display()) }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let st = self.command("query-migrate", json!({}))?;
            match st.get("status").and_then(Value::as_str) {
                Some("completed") => return Ok(()),
                Some(s @ ("failed" | "cancelled")) => bail!("migration {s}"),
                _ => {}
            }
            if Instant::now() > deadline {
                bail!("migration did not finish within {:?}", timeout);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait for an `-incoming` load to finish. QEMU stays paused afterwards.
    pub fn wait_incoming(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let st = self.command("query-migrate", json!({}))?;
            match st.get("status").and_then(Value::as_str) {
                Some("completed") => return Ok(()),
                Some(s @ ("failed" | "cancelled")) => bail!("incoming migration {s}"),
                _ => {}
            }
            if Instant::now() > deadline {
                bail!("state restore did not finish within {:?}", timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

}
