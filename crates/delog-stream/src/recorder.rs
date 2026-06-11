//! Raw MAVLink `.tlog` recorder (PLAN.md §7.5, LIV-09).

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Writes `[8-byte big-endian Unix µs][raw MAVLink frame]` records, matching
/// the tlog parser's framing exactly.
pub struct TlogRecorder {
    writer: BufWriter<File>,
    records: u64,
    bytes: u64,
}

impl TlogRecorder {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            writer: BufWriter::new(File::create(path)?),
            records: 0,
            bytes: 0,
        })
    }

    pub fn write_frame(&mut self, unix_us: i64, frame: &[u8]) -> io::Result<()> {
        self.writer.write_all(&unix_us.to_be_bytes())?;
        self.writer.write_all(frame)?;
        self.records += 1;
        self.bytes += 8 + frame.len() as u64;
        Ok(())
    }

    pub fn records(&self) -> u64 {
        self.records
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl Drop for TlogRecorder {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
