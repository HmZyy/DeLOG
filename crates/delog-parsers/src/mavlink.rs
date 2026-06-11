//! Shared MAVLink layer (PLAN.md §6.4/§7.2/§7.3, PAR-16).
//!
//! One code path for the `.tlog` parser (PAR-11) and the live link readers
//! (LIV-02/05): a push-based v1/v2 frame decoder with honest counters (CRC
//! failures, sequence gaps, resync bytes) plus a flat serde-based field
//! extractor that turns a decoded `MavMessage` into `(field, Scalar)` pairs
//! without going through a self-describing format.

use std::collections::HashMap;

use ::mavlink::dialects::all::MavMessage;
use ::mavlink::{MavlinkVersion, Message, calculate_crc};

/// v1 magic / header bytes after magic / trailing CRC bytes.
const V1_MAGIC: u8 = 0xFE;
const V1_HEADER: usize = 5;
/// v2 magic / header bytes after magic / signature length when flagged.
const V2_MAGIC: u8 = 0xFD;
const V2_HEADER: usize = 9;
const V2_SIGNATURE: usize = 13;
const V2_IFLAG_SIGNED: u8 = 0x01;
const CRC_LEN: usize = 2;

/// Honest per-link counters (§7.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameCounters {
    /// Frames that passed CRC (decoded or unknown-type).
    pub frames: u64,
    /// Candidate frames rejected by CRC.
    pub crc_failures: u64,
    /// Messages lost to sequence-number gaps, per (sysid, compid) stream.
    pub seq_gaps: u64,
    /// Bytes skipped hunting for a frame magic.
    pub resync_bytes: u64,
    /// CRC-valid frames whose message id the dialect cannot decode.
    pub unknown_messages: u64,
}

/// One CRC-valid frame.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub version: MavlinkVersion,
    pub system_id: u8,
    pub component_id: u8,
    pub sequence: u8,
    pub message_id: u32,
    /// `None` when the dialect cannot decode `message_id` (counted in
    /// [`FrameCounters::unknown_messages`]; emit the once-per-type diag
    /// upstream).
    pub message: Option<MavMessage>,
    /// Exact frame bytes after CRC validation. Live recording tees these into
    /// the `.tlog` envelope so record/replay stays bit-faithful (§7.5).
    pub raw: Vec<u8>,
}

/// Push-based MAVLink v1/v2 frame decoder. Feed arbitrary byte slices from a
/// file or socket via [`Self::push`], drain frames via [`Self::next_frame`].
/// Garbage between frames is skipped one byte at a time (counted), so torn
/// streams resynchronize on the next valid magic + CRC.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    pos: usize,
    counters: FrameCounters,
    last_seq: HashMap<(u8, u8), u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn counters(&self) -> FrameCounters {
        self.counters
    }

    /// Append raw transport bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        // Compact consumed prefix before growing.
        if self.pos > 0 && (self.pos >= self.buf.len() || self.pos > 4096) {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes not yet consumed (a partial frame awaiting more input).
    pub fn pending(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Decode the next CRC-valid frame, or `None` when the buffer holds no
    /// complete frame (push more bytes).
    pub fn next_frame(&mut self) -> Option<DecodedFrame> {
        loop {
            // Hunt for a magic byte.
            while self.pos < self.buf.len()
                && self.buf[self.pos] != V1_MAGIC
                && self.buf[self.pos] != V2_MAGIC
            {
                self.pos += 1;
                self.counters.resync_bytes += 1;
            }
            let rest = &self.buf[self.pos..];
            if rest.is_empty() {
                return None;
            }

            let Some(frame_len) = frame_len(rest) else {
                // Possibly a partial frame: wait for more bytes.
                return None;
            };
            if rest.len() < frame_len {
                return None;
            }
            let frame = &rest[..frame_len];

            let Some(decoded) = decode_frame(frame) else {
                // Bad CRC: not a real frame start; resync one byte ahead.
                self.counters.crc_failures += 1;
                self.pos += 1;
                self.counters.resync_bytes += 1;
                continue;
            };

            self.counters.frames += 1;
            if decoded.message.is_none() {
                self.counters.unknown_messages += 1;
            }
            let key = (decoded.system_id, decoded.component_id);
            if let Some(last) = self.last_seq.insert(key, decoded.sequence) {
                let gap = decoded.sequence.wrapping_sub(last).wrapping_sub(1);
                self.counters.seq_gaps += u64::from(gap);
            }
            self.pos += frame_len;
            return Some(decoded);
        }
    }
}

/// Total frame length implied by the header at `bytes[0]`, or `None` when the
/// first byte is not a frame magic or not enough bytes have arrived to know the
/// length yet. Shared by [`FrameDecoder`] (which positions on a magic byte) and
/// the `.tlog` parser's explicit µs-envelope framing (PAR-11): one length
/// computation, no duplicate header math.
pub fn frame_len(bytes: &[u8]) -> Option<usize> {
    match *bytes.first()? {
        V1_MAGIC => {
            let len = *bytes.get(1)? as usize;
            Some(1 + V1_HEADER + len + CRC_LEN)
        }
        V2_MAGIC => {
            let len = *bytes.get(1)? as usize;
            let incompat = *bytes.get(2)?;
            let sig = if incompat & V2_IFLAG_SIGNED != 0 {
                V2_SIGNATURE
            } else {
                0
            };
            Some(1 + V2_HEADER + len + CRC_LEN + sig)
        }
        _ => None,
    }
}

/// Validate the CRC of a complete candidate frame (its length given by
/// [`frame_len`]) and decode its message. Returns `None` on a bad CRC or a
/// non-frame first byte. Shared by [`FrameDecoder`] and the `.tlog` parser so
/// CRC validation and dialect decoding live in exactly one place.
pub fn decode_frame(frame: &[u8]) -> Option<DecodedFrame> {
    match frame[0] {
        V1_MAGIC => {
            let len = frame[1] as usize;
            let crc_at = 1 + V1_HEADER + len;
            let message_id = u32::from(frame[5]);
            let crc = u16::from_le_bytes([frame[crc_at], frame[crc_at + 1]]);
            if crc != calculate_crc(&frame[1..crc_at], MavMessage::extra_crc(message_id)) {
                return None;
            }
            let payload = &frame[1 + V1_HEADER..crc_at];
            Some(DecodedFrame {
                version: MavlinkVersion::V1,
                system_id: frame[3],
                component_id: frame[4],
                sequence: frame[2],
                message_id,
                message: MavMessage::parse(MavlinkVersion::V1, message_id, payload).ok(),
                raw: frame.to_vec(),
            })
        }
        V2_MAGIC => {
            let len = frame[1] as usize;
            let crc_at = 1 + V2_HEADER + len;
            let message_id = u32::from_le_bytes([frame[7], frame[8], frame[9], 0]);
            let crc = u16::from_le_bytes([frame[crc_at], frame[crc_at + 1]]);
            if crc != calculate_crc(&frame[1..crc_at], MavMessage::extra_crc(message_id)) {
                return None;
            }
            let payload = &frame[1 + V2_HEADER..crc_at];
            Some(DecodedFrame {
                version: MavlinkVersion::V2,
                system_id: frame[5],
                component_id: frame[6],
                sequence: frame[4],
                message_id,
                message: MavMessage::parse(MavlinkVersion::V2, message_id, payload).ok(),
                raw: frame.to_vec(),
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Field extraction (§7.3): a flat serde Serializer over the dialect's derives.
// ---------------------------------------------------------------------------

/// One extracted value, dtype-faithful (§6.2 raw-value policy). `Str` carries
/// enum variant names (the dialect serializes its C-like enums internally
/// tagged, so the name is what the wire representation exposes).
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    Str(String),
}

/// Flatten `message` into `(field, Scalar)` pairs. Arrays expand to indexed
/// names (`satellite_prn[3]` is element 3); enum fields carry their variant
/// name, bitflags their raw bits.
pub fn extract_fields(message: &MavMessage) -> Vec<(String, Scalar)> {
    let mut sink = FieldSink::default();
    // The serializer is infallible by construction (every method returns Ok).
    let _ = serde::Serialize::serialize(message, &mut sink);
    sink.fields
}

#[derive(Default)]
struct FieldSink {
    fields: Vec<(String, Scalar)>,
    /// Struct-field name stack. MAVLink data structs are flat, so depth > 1
    /// only happens inside the internally-tagged enum representation
    /// (`mavtype: {"type": "MAV_TYPE_..."}`); the outermost name is the field.
    path: Vec<&'static str>,
    /// Current sequence index, if inside an array field.
    index: Option<usize>,
}

impl FieldSink {
    fn emit(&mut self, scalar: Scalar) {
        let Some(&field) = self.path.first() else {
            return; // value outside any named field
        };
        let name = match self.index {
            Some(i) => format!("{field}[{i}]"),
            None => field.to_owned(),
        };
        self.fields.push((name, scalar));
        if let Some(i) = &mut self.index {
            *i += 1;
        }
    }
}

/// The serializer never fails; this error type exists for the trait only.
#[derive(Debug)]
struct Never;

impl std::fmt::Display for Never {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unreachable")
    }
}
impl std::error::Error for Never {}
impl serde::ser::Error for Never {
    fn custom<T: std::fmt::Display>(_msg: T) -> Self {
        Never
    }
}

impl serde::Serializer for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    /// Non-human-readable so bitflags serialize as their raw bits integer
    /// instead of a flag-name string.
    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_bool(self, v: bool) -> Result<(), Never> {
        self.emit(Scalar::U8(u8::from(v)));
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Never> {
        self.emit(Scalar::I8(v));
        Ok(())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Never> {
        self.emit(Scalar::I16(v));
        Ok(())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Never> {
        self.emit(Scalar::I32(v));
        Ok(())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Never> {
        self.emit(Scalar::I64(v));
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Never> {
        self.emit(Scalar::U8(v));
        Ok(())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Never> {
        self.emit(Scalar::U16(v));
        Ok(())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Never> {
        self.emit(Scalar::U32(v));
        Ok(())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Never> {
        self.emit(Scalar::U64(v));
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), Never> {
        self.emit(Scalar::F32(v));
        Ok(())
    }
    fn serialize_f64(self, v: f64) -> Result<(), Never> {
        self.emit(Scalar::F64(v));
        Ok(())
    }

    // Not plottable: skipped without error.
    fn serialize_char(self, _: char) -> Result<(), Never> {
        Ok(())
    }
    /// Strings only occur as the tag of the internally-tagged enum
    /// representation (depth 2) — capture the variant name there.
    fn serialize_str(self, v: &str) -> Result<(), Never> {
        if self.path.len() >= 2 {
            self.emit(Scalar::Str(v.to_owned()));
        }
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), Never> {
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Never> {
        Ok(())
    }
    fn serialize_some<T: serde::Serialize + ?Sized>(self, value: &T) -> Result<(), Never> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Never> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Never> {
        Ok(())
    }

    /// C-like enum field (untagged path) → its variant name.
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), Never> {
        self.emit(Scalar::Str(variant.to_owned()));
        Ok(())
    }

    fn serialize_newtype_struct<T: serde::Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), Never> {
        value.serialize(self)
    }

    /// `MavMessage::Variant(DATA)` — descend into the data struct.
    fn serialize_newtype_variant<T: serde::Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        value: &T,
    ) -> Result<(), Never> {
        value.serialize(self)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self, Never> {
        self.index = Some(0);
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, Never> {
        self.index = Some(0);
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Never> {
        self.index = Some(0);
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Never> {
        self.index = Some(0);
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, Never> {
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Never> {
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Never> {
        Ok(self)
    }
}

impl serde::ser::SerializeStruct for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: serde::Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Never> {
        self.path.push(key);
        if self.path.len() == 1 {
            self.index = None;
        }
        value.serialize(&mut **self)?;
        self.path.pop();
        if self.path.is_empty() {
            self.index = None;
        }
        Ok(())
    }
    fn end(self) -> Result<(), Never> {
        Ok(())
    }
}

impl serde::ser::SerializeSeq for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    fn serialize_element<T: serde::Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Never> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Never> {
        self.index = None;
        Ok(())
    }
}

impl serde::ser::SerializeTuple for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    fn serialize_element<T: serde::Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Never> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Never> {
        self.index = None;
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Never> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Never> {
        self.index = None;
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: serde::Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Never> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Never> {
        self.index = None;
        Ok(())
    }
}

impl serde::ser::SerializeMap for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    fn serialize_key<T: serde::Serialize + ?Sized>(&mut self, _: &T) -> Result<(), Never> {
        Ok(())
    }
    fn serialize_value<T: serde::Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Never> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Never> {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for &mut FieldSink {
    type Ok = ();
    type Error = Never;
    fn serialize_field<T: serde::Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Never> {
        serde::ser::SerializeStruct::serialize_field(self, key, value)
    }
    fn end(self) -> Result<(), Never> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ::mavlink::dialects::all::{ATTITUDE_DATA, GPS_STATUS_DATA, HEARTBEAT_DATA};
    use ::mavlink::{MAVLinkV1MessageRaw, MAVLinkV2MessageRaw, MavHeader};

    use super::*;

    fn header(seq: u8) -> MavHeader {
        MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: seq,
        }
    }

    fn attitude(roll: f32) -> MavMessage {
        MavMessage::ATTITUDE(ATTITUDE_DATA {
            time_boot_ms: 1_000,
            roll,
            pitch: 0.5,
            yaw: -0.25,
            rollspeed: 0.0,
            pitchspeed: 0.0,
            yawspeed: 0.0,
        })
    }

    fn v2_bytes(seq: u8, msg: &MavMessage) -> Vec<u8> {
        let mut raw = MAVLinkV2MessageRaw::new();
        raw.serialize_message(header(seq), msg);
        raw.raw_bytes().to_vec()
    }

    fn v1_bytes(seq: u8, msg: &MavMessage) -> Vec<u8> {
        let mut raw = MAVLinkV1MessageRaw::new();
        raw.serialize_message(header(seq), msg);
        raw.raw_bytes().to_vec()
    }

    #[test]
    fn decodes_mixed_v1_and_v2_frames_with_garbage_between() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0x00, 0x13, 0x37]); // leading garbage
        decoder.push(&v2_bytes(0, &attitude(1.0)));
        decoder.push(&[0xAA, 0xBB]); // inter-frame garbage
        decoder.push(&v1_bytes(1, &attitude(2.0)));

        let first = decoder.next_frame().expect("v2 frame");
        assert_eq!(first.version, MavlinkVersion::V2);
        assert!(matches!(first.message, Some(MavMessage::ATTITUDE(ref d)) if d.roll == 1.0));

        let second = decoder.next_frame().expect("v1 frame");
        assert_eq!(second.version, MavlinkVersion::V1);
        assert!(matches!(second.message, Some(MavMessage::ATTITUDE(ref d)) if d.roll == 2.0));

        assert!(decoder.next_frame().is_none());
        let counters = decoder.counters();
        assert_eq!(counters.frames, 2);
        assert_eq!(counters.crc_failures, 0);
        assert!(counters.resync_bytes >= 5);
    }

    #[test]
    fn partial_frames_wait_for_more_bytes() {
        let bytes = v2_bytes(0, &attitude(1.0));
        let mut decoder = FrameDecoder::new();
        decoder.push(&bytes[..10]);
        assert!(decoder.next_frame().is_none());
        decoder.push(&bytes[10..]);
        assert!(decoder.next_frame().is_some());
    }

    #[test]
    fn corrupted_crc_is_counted_and_resyncs_to_the_next_frame() {
        let mut bad = v2_bytes(0, &attitude(1.0));
        let last = bad.len() - 1;
        bad[last] ^= 0xFF; // break the CRC
        let good = v2_bytes(1, &attitude(2.0));

        let mut decoder = FrameDecoder::new();
        decoder.push(&bad);
        decoder.push(&good);
        let frame = decoder.next_frame().expect("recovers the good frame");
        assert!(matches!(frame.message, Some(MavMessage::ATTITUDE(ref d)) if d.roll == 2.0));
        assert!(decoder.counters().crc_failures >= 1);
    }

    #[test]
    fn sequence_gaps_are_counted_per_stream() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&v2_bytes(0, &attitude(1.0)));
        decoder.push(&v2_bytes(1, &attitude(1.0)));
        decoder.push(&v2_bytes(5, &attitude(1.0))); // 2,3,4 lost
        while decoder.next_frame().is_some() {}
        assert_eq!(decoder.counters().seq_gaps, 3);
    }

    #[test]
    fn extraction_flattens_scalars_with_original_dtypes() {
        let fields = extract_fields(&attitude(1.5));
        let get = |name: &str| {
            fields
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("missing field {name}"))
                .1
                .clone()
        };
        assert_eq!(get("time_boot_ms"), Scalar::U32(1_000));
        assert_eq!(get("roll"), Scalar::F32(1.5));
        assert_eq!(get("yaw"), Scalar::F32(-0.25));
        assert_eq!(fields.len(), 7);
    }

    #[test]
    fn extraction_expands_arrays_and_names_enums() {
        let mut gps = GPS_STATUS_DATA {
            satellites_visible: 3,
            ..Default::default()
        };
        gps.satellite_prn[2] = 17;
        let fields = extract_fields(&MavMessage::GPS_STATUS(gps));
        assert!(
            fields
                .iter()
                .any(|(n, s)| n == "satellite_prn[2]" && *s == Scalar::U8(17))
        );
        // 5 × 20-element arrays + satellites_visible.
        assert_eq!(fields.len(), 101);

        // HEARTBEAT: enums carry their variant name, bitflags their raw bits.
        let hb = MavMessage::HEARTBEAT(HEARTBEAT_DATA::default());
        let fields = extract_fields(&hb);
        let get = |name: &str| {
            fields
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("missing field {name}"))
                .1
                .clone()
        };
        assert_eq!(get("mavtype"), Scalar::Str("MAV_TYPE_GENERIC".to_owned()));
        assert_eq!(get("base_mode"), Scalar::U8(128));
        assert_eq!(get("custom_mode"), Scalar::U32(0));
    }
}
