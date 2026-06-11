//! Per-link reader thread with owned framing (PLAN.md §7.2, LIV-02).
//!
//! Each configured [`Endpoint`] gets one reader thread. We own the byte-level
//! loop — buffered transport → the shared [`FrameDecoder`] (v1/v2 sync, CRC,
//! sequence-gap and resync counting) — and hand decoded *frames* downstream,
//! rather than using `rust-mavlink`'s blocking `connect()` helpers. That access
//! to the raw stream is what makes the honest per-link counters of §7.2
//! possible. Decoding the frame's *fields* (LIV-05) and batching into the
//! ingest pipeline (LIV-07) happen on the consumer side of the frame channel;
//! the link state machine (LIV-03) and auto-reconnect (LIV-04) wrap this.

use std::io::{self, Read};
use std::net::{TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::Sender;
use delog_parsers::mavlink::{DecodedFrame, FrameCounters, FrameDecoder};

use crate::Endpoint;

/// Transport read timeout. Bounds how long a reader blocks before it can notice
/// a stop request, so [`LinkReader::stop`] takes effect within this window.
const READ_TIMEOUT: Duration = Duration::from_millis(200);
/// Transport read buffer; comfortably larger than any single MAVLink frame.
const READ_BUF: usize = 8192;

/// Live, lock-free per-link counters (§7.2). Cloned into the reader thread and
/// read by the UI; values are monotonic, so `Relaxed` ordering is enough.
#[derive(Debug, Default)]
pub struct LinkCounters {
    rx_frames: AtomicU64,
    rx_bytes: AtomicU64,
    crc_failures: AtomicU64,
    seq_gaps: AtomicU64,
    resync_bytes: AtomicU64,
    unknown_messages: AtomicU64,
}

impl LinkCounters {
    fn add_rx_bytes(&self, n: u64) {
        self.rx_bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Mirror the decoder's absolute counters into the shared atomics.
    fn store_frames(&self, c: FrameCounters) {
        self.rx_frames.store(c.frames, Ordering::Relaxed);
        self.crc_failures.store(c.crc_failures, Ordering::Relaxed);
        self.seq_gaps.store(c.seq_gaps, Ordering::Relaxed);
        self.resync_bytes.store(c.resync_bytes, Ordering::Relaxed);
        self.unknown_messages
            .store(c.unknown_messages, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> LinkStats {
        LinkStats {
            rx_frames: self.rx_frames.load(Ordering::Relaxed),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            crc_failures: self.crc_failures.load(Ordering::Relaxed),
            seq_gaps: self.seq_gaps.load(Ordering::Relaxed),
            resync_bytes: self.resync_bytes.load(Ordering::Relaxed),
            unknown_messages: self.unknown_messages.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time copy of a link's counters (§7.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// CRC-valid frames received (rx packets).
    pub rx_frames: u64,
    /// Total transport bytes read.
    pub rx_bytes: u64,
    /// Candidate frames rejected by CRC.
    pub crc_failures: u64,
    /// Messages lost to sequence-number gaps.
    pub seq_gaps: u64,
    /// Bytes skipped hunting for a frame magic.
    pub resync_bytes: u64,
    /// CRC-valid frames whose message id the dialect can't decode.
    pub unknown_messages: u64,
}

/// A running reader thread. Dropping it requests a stop and joins.
pub struct LinkReader {
    stop: Arc<AtomicBool>,
    counters: Arc<LinkCounters>,
    join: Option<JoinHandle<io::Result<()>>>,
}

impl LinkReader {
    /// Open `endpoint` and spawn its reader thread. Decoded frames are sent on
    /// `frames`; when the receiver is dropped the thread exits cleanly. Opening
    /// the transport (binding/connecting) happens synchronously, so a refused
    /// connection or bind error surfaces here rather than on the thread.
    pub fn spawn(endpoint: &Endpoint, frames: Sender<DecodedFrame>) -> io::Result<Self> {
        let transport = open(endpoint)?;
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(LinkCounters::default());
        let join = {
            let stop = Arc::clone(&stop);
            let counters = Arc::clone(&counters);
            thread::Builder::new()
                .name(format!("link-reader {endpoint}"))
                .spawn(move || pump(transport, &stop, &counters, &frames))?
        };
        Ok(Self {
            stop,
            counters,
            join: Some(join),
        })
    }

    /// A live snapshot of this link's counters.
    pub fn stats(&self) -> LinkStats {
        self.counters.snapshot()
    }

    /// Request the reader thread to stop (effective within [`READ_TIMEOUT`]).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Stop and wait for the reader thread, returning its transport result.
    pub fn join(mut self) -> io::Result<()> {
        self.stop();
        match self.join.take() {
            Some(handle) => handle.join().unwrap_or(Ok(())),
            None => Ok(()),
        }
    }
}

impl Drop for LinkReader {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// The owned framing loop: read transport bytes, feed the shared decoder, send
/// each decoded frame on. Returns when the transport ends (`Ok(0)`), the
/// consumer drops the channel, or a stop is requested; a transport IO error
/// (other than a timeout) propagates.
fn pump(
    mut transport: Box<dyn Read + Send>,
    stop: &AtomicBool,
    counters: &LinkCounters,
    frames: &Sender<DecodedFrame>,
) -> io::Result<()> {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; READ_BUF];
    while !stop.load(Ordering::Acquire) {
        let n = match transport.read(&mut buf) {
            Ok(0) => break, // EOF / peer closed
            Ok(n) => n,
            // A read timeout (no data within READ_TIMEOUT) just lets us re-check
            // the stop flag.
            Err(e) if is_timeout(&e) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        counters.add_rx_bytes(n as u64);
        decoder.push(&buf[..n]);
        while let Some(frame) = decoder.next_frame() {
            if frames.send(frame).is_err() {
                return Ok(()); // consumer gone
            }
        }
        counters.store_frames(decoder.counters());
    }
    Ok(())
}

/// A read timeout, spelled differently across platforms (`WouldBlock` on Unix,
/// `TimedOut` on Windows) and transports.
fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Open a transport with a read timeout so the reader stays stoppable.
fn open(endpoint: &Endpoint) -> io::Result<Box<dyn Read + Send>> {
    match endpoint {
        Endpoint::UdpServer { bind } => {
            let socket = UdpSocket::bind(bind)?;
            socket.set_read_timeout(Some(READ_TIMEOUT))?;
            Ok(Box::new(UdpReader(socket)))
        }
        Endpoint::TcpClient { remote } => {
            let stream = TcpStream::connect(remote)?;
            stream.set_read_timeout(Some(READ_TIMEOUT))?;
            Ok(Box::new(stream))
        }
        Endpoint::Serial { path, baud } => {
            let port = serialport::new(path, *baud)
                .timeout(READ_TIMEOUT)
                .open()
                .map_err(io::Error::other)?;
            Ok(Box::new(SerialReader(port)))
        }
    }
}

/// `UdpSocket` as a byte stream. Datagrams carry one or more whole frames; the
/// decoder reassembles regardless. A zero-length datagram is mapped to a
/// timeout so the connectionless link is never mistaken for EOF.
struct UdpReader(UdpSocket);

impl Read for UdpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.recv(buf)? {
            0 => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            n => Ok(n),
        }
    }
}

/// `serialport`'s boxed handle as a plain `Read`.
struct SerialReader(Box<dyn serialport::SerialPort>);

impl Read for SerialReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Write;
    use std::net::TcpListener;

    use crossbeam_channel::unbounded;
    use mavlink::dialects::ardupilotmega::{ATTITUDE_DATA, MavMessage};
    use mavlink::{MAVLinkV1MessageRaw, MAVLinkV2MessageRaw, MavHeader};

    use super::*;

    fn attitude(roll: f32) -> MavMessage {
        MavMessage::ATTITUDE(ATTITUDE_DATA {
            time_boot_ms: 10,
            roll,
            pitch: 0.0,
            yaw: 0.0,
            rollspeed: 0.0,
            pitchspeed: 0.0,
            yawspeed: 0.0,
        })
    }

    fn v2(seq: u8, msg: &MavMessage) -> Vec<u8> {
        let mut raw = MAVLinkV2MessageRaw::new();
        raw.serialize_message(
            MavHeader {
                system_id: 1,
                component_id: 1,
                sequence: seq,
            },
            msg,
        );
        raw.raw_bytes().to_vec()
    }

    fn v1(seq: u8, msg: &MavMessage) -> Vec<u8> {
        let mut raw = MAVLinkV1MessageRaw::new();
        raw.serialize_message(
            MavHeader {
                system_id: 1,
                component_id: 1,
                sequence: seq,
            },
            msg,
        );
        raw.raw_bytes().to_vec()
    }

    fn roll_of(frame: &DecodedFrame) -> f32 {
        match frame.message.as_ref() {
            Some(MavMessage::ATTITUDE(d)) => d.roll,
            other => panic!("expected ATTITUDE, got {other:?}"),
        }
    }

    /// A `Read` that hands back queued chunks (respecting the caller's buffer
    /// size) then `Ok(0)` to end the pump — a deterministic stand-in transport.
    struct ChunkReader {
        chunks: VecDeque<Vec<u8>>,
    }

    impl ChunkReader {
        fn new(chunks: Vec<Vec<u8>>) -> Self {
            Self {
                chunks: chunks.into(),
            }
        }
    }

    impl Read for ChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            if n < chunk.len() {
                self.chunks.push_front(chunk[n..].to_vec());
            }
            Ok(n)
        }
    }

    fn run_pump(reader: ChunkReader) -> (Vec<DecodedFrame>, LinkStats) {
        let (tx, rx) = unbounded();
        let stop = AtomicBool::new(false);
        let counters = LinkCounters::default();
        pump(Box::new(reader), &stop, &counters, &tx).expect("pump");
        drop(tx);
        (rx.try_iter().collect(), counters.snapshot())
    }

    #[test]
    fn decodes_mixed_frames_with_garbage_and_counts_bytes() {
        let mut bytes = vec![0xAA, 0xBB, 0xCC]; // leading garbage
        bytes.extend(v2(0, &attitude(1.0)));
        bytes.push(0x00); // inter-frame garbage
        bytes.extend(v1(1, &attitude(2.0)));
        let total = bytes.len() as u64;

        let (frames, stats) = run_pump(ChunkReader::new(vec![bytes]));

        assert_eq!(frames.len(), 2);
        assert_eq!(roll_of(&frames[0]), 1.0);
        assert_eq!(roll_of(&frames[1]), 2.0);
        assert_eq!(stats.rx_frames, 2);
        assert_eq!(stats.rx_bytes, total);
        assert_eq!(stats.crc_failures, 0);
        assert!(stats.resync_bytes >= 4);
    }

    #[test]
    fn reassembles_frames_split_across_reads() {
        // One byte per read: the decoder must buffer partial frames.
        let chunks = v2(0, &attitude(3.5)).into_iter().map(|b| vec![b]).collect();
        let (frames, stats) = run_pump(ChunkReader::new(chunks));
        assert_eq!(frames.len(), 1);
        assert_eq!(roll_of(&frames[0]), 3.5);
        assert_eq!(stats.rx_frames, 1);
    }

    #[test]
    fn corrupt_crc_is_counted_and_resyncs() {
        let mut bad = v2(0, &attitude(9.0));
        *bad.last_mut().unwrap() ^= 0xFF;
        let mut bytes = bad;
        bytes.extend(v2(1, &attitude(2.0)));

        let (frames, stats) = run_pump(ChunkReader::new(vec![bytes]));
        assert_eq!(frames.len(), 1);
        assert_eq!(roll_of(&frames[0]), 2.0);
        assert!(stats.crc_failures >= 1);
    }

    #[test]
    fn dropped_consumer_ends_the_pump_cleanly() {
        let (tx, rx) = unbounded::<DecodedFrame>();
        drop(rx); // consumer gone before any frame
        let stop = AtomicBool::new(false);
        let counters = LinkCounters::default();
        let reader = ChunkReader::new(vec![v2(0, &attitude(1.0))]);
        // Must return Ok rather than panic on the failed send.
        pump(Box::new(reader), &stop, &counters, &tx).expect("clean exit");
    }

    #[test]
    fn udp_endpoint_opens_a_bound_socket() {
        // The send side isn't exercised here (the bound port is ephemeral); this
        // pins the UDP branch of `open`.
        let endpoint = Endpoint::UdpServer {
            bind: "127.0.0.1:0".parse().unwrap(),
        };
        assert!(open(&endpoint).is_ok());
    }

    /// End-to-end over loopback TCP: spawn the reader against a listener we
    /// control, push a frame, and confirm it arrives, counters update, and the
    /// thread stops within the read-timeout window while the connection is
    /// still open (the stop path, not an EOF).
    #[test]
    fn spawn_reads_a_frame_over_tcp_then_stops() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = unbounded();

        let reader = LinkReader::spawn(&Endpoint::TcpClient { remote: addr }, tx).unwrap();
        let (mut conn, _) = listener.accept().unwrap();
        conn.write_all(&v2(7, &attitude(1.5))).unwrap();
        conn.flush().unwrap();

        let frame = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("frame over tcp");
        assert_eq!(roll_of(&frame), 1.5);
        assert!(reader.stats().rx_frames >= 1);

        reader.join().expect("clean join"); // connection still open: stop-driven
    }
}
