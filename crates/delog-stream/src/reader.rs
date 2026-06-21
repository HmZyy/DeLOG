//! Per-link reader thread with owned framing.
//!
//! Each configured [`Endpoint`] gets one reader thread. We own the byte-level
//! loop — buffered transport → the shared [`FrameDecoder`] (v1/v2 sync, CRC,
//! sequence-gap and resync counting) — and hand decoded *frames* downstream,
//! rather than using `rust-mavlink`'s blocking `connect()` helpers. That access
//! to the raw stream is what makes the honest per-link counters possible.
//! Decoding the frame's *fields* and batching into the ingest pipeline happen
//! on the consumer side of the frame channel; the link state machine and
//! auto-reconnect wrap this.

use std::io::{self, Read};
use std::net::{TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use delog_parsers::mavlink::{DecodedFrame, FrameCounters, FrameDecoder};

use crate::Endpoint;

/// Transport read timeout. Bounds how long a reader blocks before it can notice
/// a stop request, so [`LinkReader::stop`] takes effect within this window.
const READ_TIMEOUT: Duration = Duration::from_millis(200);
/// Transport read buffer; comfortably larger than any single MAVLink frame.
const READ_BUF: usize = 8192;

/// First reconnect backoff; doubles per consecutive failure.
pub const RECONNECT_INITIAL: Duration = Duration::from_millis(500);
/// Reconnect backoff ceiling.
pub const RECONNECT_CAP: Duration = Duration::from_secs(8);

/// Next backoff after `prev` (`None` = first wait): exponential, capped.
fn next_backoff(prev: Option<Duration>) -> Duration {
    match prev {
        None => RECONNECT_INITIAL,
        Some(d) => (d * 2).min(RECONNECT_CAP),
    }
}

/// A connected link goes `Stale` after this long without a valid frame.
pub const STALE_AFTER: Duration = Duration::from_secs(2);
/// …and `Lost` after this long.
pub const LOST_AFTER: Duration = Duration::from_secs(10);

/// Link liveness for the UI indicator. `Connecting` is the state before the
/// first valid frame; afterwards liveness is a function of the time since the
/// last frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Connected,
    Stale,
    Lost,
}

impl LinkState {
    /// Classify from the time since the last valid frame (`None` = none yet).
    pub fn classify(since_last_rx: Option<Duration>) -> Self {
        match since_last_rx {
            None => Self::Connecting,
            Some(d) if d >= LOST_AFTER => Self::Lost,
            Some(d) if d >= STALE_AFTER => Self::Stale,
            Some(_) => Self::Connected,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Stale => "Stale",
            Self::Lost => "Lost",
        }
    }
}

/// Live, lock-free per-link counters. Cloned into the reader thread and read by
/// the UI; values are monotonic, so `Relaxed` ordering is enough.
#[derive(Debug, Default)]
pub struct LinkCounters {
    rx_frames: AtomicU64,
    rx_bytes: AtomicU64,
    crc_failures: AtomicU64,
    seq_gaps: AtomicU64,
    resync_bytes: AtomicU64,
    unknown_messages: AtomicU64,
    /// Transport reconnections after the initial connect.
    reconnects: AtomicU64,
    /// Millis (since the reader's start instant) of the last valid frame; 0
    /// until the first frame arrives. Drives [`LinkReader::state`].
    last_rx_millis: AtomicU64,
}

impl LinkCounters {
    fn add_rx_bytes(&self, n: u64) {
        self.rx_bytes.fetch_add(n, Ordering::Relaxed);
    }

    fn mark_rx(&self, millis: u64) {
        self.last_rx_millis.store(millis, Ordering::Relaxed);
    }

    fn mark_reconnect(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
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
            reconnects: self.reconnects.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time copy of a link's counters.
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
    /// Transport reconnections after the initial connect.
    pub reconnects: u64,
}

/// A running reader thread. Dropping it requests a stop and joins.
pub struct LinkReader {
    stop: Arc<AtomicBool>,
    counters: Arc<LinkCounters>,
    started: Instant,
    join: Option<JoinHandle<io::Result<()>>>,
}

impl LinkReader {
    /// Open `endpoint` and spawn its reader thread. Decoded frames are sent on
    /// `frames`; when the receiver is dropped the thread exits cleanly. Opening
    /// the transport (binding/connecting) happens synchronously, so a refused
    /// connection or bind error surfaces here rather than on the thread.
    pub fn spawn(endpoint: &Endpoint, frames: Sender<DecodedFrame>) -> io::Result<Self> {
        let first = open(endpoint)?;
        let reconnectable = matches!(
            endpoint.kind(),
            crate::EndpointKind::TcpClient | crate::EndpointKind::Serial
        );
        let endpoint = endpoint.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(LinkCounters::default());
        let started = Instant::now();
        let join = {
            let stop = Arc::clone(&stop);
            let counters = Arc::clone(&counters);
            thread::Builder::new()
                .name(format!("link-reader {endpoint}"))
                .spawn(move || {
                    supervise(
                        Some(first),
                        reconnectable,
                        || open(&endpoint),
                        &stop,
                        &counters,
                        started,
                        &frames,
                        |wait| interruptible_sleep(wait, &stop),
                    )
                })?
        };
        Ok(Self {
            stop,
            counters,
            started,
            join: Some(join),
        })
    }

    /// A live snapshot of this link's counters.
    pub fn stats(&self) -> LinkStats {
        self.counters.snapshot()
    }

    /// Current link liveness. `Connecting` until the first valid frame, then
    /// `Connected`/`Stale`/`Lost` by time since the last frame.
    pub fn state(&self) -> LinkState {
        if self.counters.rx_frames.load(Ordering::Relaxed) == 0 {
            return LinkState::Connecting;
        }
        let now = self.started.elapsed().as_millis() as u64;
        let last = self.counters.last_rx_millis.load(Ordering::Relaxed);
        LinkState::classify(Some(Duration::from_millis(now.saturating_sub(last))))
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

/// Why [`pump`] returned, so the supervisor can decide whether to reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PumpOutcome {
    /// The transport ended (`read` returned `Ok(0)` / peer closed).
    Eof,
    /// A stop was requested.
    Stopped,
    /// The frame consumer dropped the channel.
    ConsumerGone,
}

/// The owned framing loop: read transport bytes, feed the shared decoder, send
/// each decoded frame on. A transport IO error other than a timeout propagates
/// (the supervisor treats it like `Eof` for reconnectable links).
fn pump(
    mut transport: Box<dyn Read + Send>,
    stop: &AtomicBool,
    counters: &LinkCounters,
    started: Instant,
    frames: &Sender<DecodedFrame>,
) -> io::Result<PumpOutcome> {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; READ_BUF];
    while !stop.load(Ordering::Acquire) {
        let n = match transport.read(&mut buf) {
            Ok(0) => return Ok(PumpOutcome::Eof), // peer closed
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
            counters.mark_rx(started.elapsed().as_millis() as u64);
            if frames.send(frame).is_err() {
                return Ok(PumpOutcome::ConsumerGone);
            }
        }
        counters.store_frames(decoder.counters());
    }
    Ok(PumpOutcome::Stopped)
}

/// Supervise a link: pump the connection and, for reconnectable transports,
/// re-open it with exponential backoff. The backoff resets only
/// after a session that actually delivered frames, so a flapping link still
/// backs off. `connect`/`backoff_sleep` are injected so the schedule is
/// testable without real sockets or real sleeps.
#[allow(clippy::too_many_arguments)]
fn supervise(
    mut first: Option<Box<dyn Read + Send>>,
    reconnectable: bool,
    mut connect: impl FnMut() -> io::Result<Box<dyn Read + Send>>,
    stop: &AtomicBool,
    counters: &LinkCounters,
    started: Instant,
    frames: &Sender<DecodedFrame>,
    mut backoff_sleep: impl FnMut(Duration),
) -> io::Result<()> {
    let mut backoff = None;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let transport = match first.take() {
            Some(transport) => transport,
            None => match connect() {
                Ok(transport) => {
                    counters.mark_reconnect();
                    transport
                }
                Err(err) => {
                    if !reconnectable {
                        return Err(err);
                    }
                    backoff = Some(next_backoff(backoff));
                    backoff_sleep(backoff.unwrap());
                    continue;
                }
            },
        };

        let before = counters.snapshot().rx_frames;
        let outcome = pump(transport, stop, counters, started, frames);
        match outcome {
            Ok(PumpOutcome::Stopped | PumpOutcome::ConsumerGone) => return Ok(()),
            Ok(PumpOutcome::Eof) | Err(_) => {
                if !reconnectable {
                    return outcome.map(|_| ());
                }
                if counters.snapshot().rx_frames > before {
                    backoff = None; // a productive session: start over
                }
                backoff = Some(next_backoff(backoff));
                backoff_sleep(backoff.unwrap());
            }
        }
    }
}

/// Sleep up to `total`, in slices, so a stop request during a reconnect wait is
/// honored promptly rather than after the full backoff.
fn interruptible_sleep(total: Duration, stop: &AtomicBool) {
    let slice = Duration::from_millis(50);
    let mut remaining = total;
    while remaining > Duration::ZERO && !stop.load(Ordering::Acquire) {
        let nap = remaining.min(slice);
        thread::sleep(nap);
        remaining = remaining.saturating_sub(nap);
    }
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
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::io::Write;
    use std::net::TcpListener;
    use std::rc::Rc;

    use crossbeam_channel::unbounded;
    use mavlink::dialects::all::{ATTITUDE_DATA, MavMessage};
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
        pump(Box::new(reader), &stop, &counters, Instant::now(), &tx).expect("pump");
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
        pump(Box::new(reader), &stop, &counters, Instant::now(), &tx).expect("clean exit");
    }

    #[test]
    fn link_state_classifies_by_time_since_last_frame() {
        assert_eq!(LinkState::classify(None), LinkState::Connecting);
        assert_eq!(
            LinkState::classify(Some(Duration::from_millis(500))),
            LinkState::Connected
        );
        assert_eq!(
            LinkState::classify(Some(Duration::from_secs(5))),
            LinkState::Stale
        );
        assert_eq!(
            LinkState::classify(Some(Duration::from_secs(30))),
            LinkState::Lost
        );
        // Boundaries are inclusive of the worse state.
        assert_eq!(LinkState::classify(Some(STALE_AFTER)), LinkState::Stale);
        assert_eq!(LinkState::classify(Some(LOST_AFTER)), LinkState::Lost);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut b = next_backoff(None);
        assert_eq!(b, Duration::from_millis(500));
        let expected = [
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(8), // capped
        ];
        for want in expected {
            b = next_backoff(Some(b));
            assert_eq!(b, want);
        }
    }

    #[test]
    fn reconnects_with_backoff_and_resets_after_a_productive_session() {
        let stop = AtomicBool::new(false);
        let counters = LinkCounters::default();
        let (tx, rx) = unbounded();

        // connect: fail, fail, succeed (one frame), then fail forever.
        let attempt = Cell::new(0u32);
        let connect = || -> io::Result<Box<dyn Read + Send>> {
            let a = attempt.get();
            attempt.set(a + 1);
            if a == 2 {
                Ok(Box::new(ChunkReader::new(vec![v2(0, &attitude(1.0))])))
            } else {
                Err(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"))
            }
        };

        // Record each backoff and stop once we've seen the full schedule.
        let sleeps = Rc::new(RefCell::new(Vec::new()));
        let sleeps_rec = Rc::clone(&sleeps);
        let sleep = |wait: Duration| {
            sleeps_rec.borrow_mut().push(wait);
            if sleeps_rec.borrow().len() >= 4 {
                stop.store(true, Ordering::Release);
            }
        };

        supervise(
            None,
            true,
            connect,
            &stop,
            &counters,
            Instant::now(),
            &tx,
            sleep,
        )
        .unwrap();
        drop(tx);

        assert_eq!(rx.try_iter().count(), 1);
        // 0.5 s, 1 s after two failures; then the productive session resets the
        // backoff so the next wait is 0.5 s again, then 1 s.
        assert_eq!(
            *sleeps.borrow(),
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_millis(500),
                Duration::from_secs(1),
            ]
        );
        assert_eq!(counters.snapshot().reconnects, 1);
    }

    #[test]
    fn non_reconnectable_link_stops_on_eof_without_reconnecting() {
        let stop = AtomicBool::new(false);
        let counters = LinkCounters::default();
        let (tx, rx) = unbounded();
        let connect = || -> io::Result<Box<dyn Read + Send>> {
            panic!("a non-reconnectable link must not reconnect")
        };
        let mut sleeps = 0u32;
        let sleep = |_: Duration| sleeps += 1;
        let first: Box<dyn Read + Send> = Box::new(ChunkReader::new(vec![v2(0, &attitude(2.0))]));

        supervise(
            Some(first),
            false,
            connect,
            &stop,
            &counters,
            Instant::now(),
            &tx,
            sleep,
        )
        .unwrap();
        drop(tx);

        assert_eq!(rx.try_iter().count(), 1);
        assert_eq!(sleeps, 0);
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
        assert_eq!(reader.state(), LinkState::Connecting); // no frame yet
        conn.write_all(&v2(7, &attitude(1.5))).unwrap();
        conn.flush().unwrap();

        let frame = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("frame over tcp");
        assert_eq!(roll_of(&frame), 1.5);
        assert!(reader.stats().rx_frames >= 1);
        assert_eq!(reader.state(), LinkState::Connected); // a fresh frame arrived

        reader.join().expect("clean join"); // connection still open: stop-driven
    }
}
