//! The 188-byte transport packet writer: header, adaptation field, PCR,
//! stuffing, per-PID continuity counters, and the M2TS four-byte wrapper.
//!
//! This is the layer [`crate::mux::MpegTsMuxer`] hands finished PES packets
//! and PSI sections to; it knows nothing about PES or PSI syntax, only about
//! slicing a byte string into 188-byte cells the way ISO/IEC 13818-1 §2.4.3.2
//! to §2.4.3.5 requires.
//!
//! # The continuity-counter rule this module exists to get right
//!
//! "Continuity counters are per-PID, four bits, and increment only on
//! packets carrying payload" (the brief's own words, and ISO/IEC 13818-1
//! §2.4.3.3's, independently). Every packet this writer emits *does* carry
//! payload — there is no code path here that builds an adaptation-field-only
//! packet — so in practice the counter simply advances once per packet. The
//! distinction still matters and is enforced in [`TsWriter::next_cc`] rather
//! than assumed, because the day this module grows CBR null-packet stuffing
//! (see [`crate::options::MpegTsMuxOptions::muxrate_bps`]'s doc: not implemented
//! yet) a null packet must **not** advance its PID's counter, and getting
//! that wrong the day it is added is exactly how "plays until it doesn't"
//! bugs happen.

use vaco_core::{Error, Result};
use vaco_format_mpegts_tables::{TS_PACKET_SIZE, packet::Pcr};
use vaco_io::MediaSink;

/// `188 - 4`: payload bytes available in a packet with no adaptation field.
const BODY_LEN: usize = TS_PACKET_SIZE - 4;

/// What the adaptation field of the *first* packet of one `write_payload`
/// call should carry. Every later packet of the same call gets a trivial
/// (absent, or stuffing-only) adaptation field.
#[derive(Debug, Clone, Copy, Default)]
pub struct AfRequest {
    pub discontinuity: bool,
    pub random_access: bool,
    pub pcr: Option<Pcr>,
}

impl AfRequest {
    /// Bytes of adaptation field *content* (after the length byte itself)
    /// this request needs, before any stuffing pad.
    const fn min_content_len(self) -> usize {
        if self.is_trivial() {
            0
        } else {
            1 + if self.pcr.is_some() { 6 } else { 0 }
        }
    }

    const fn is_trivial(self) -> bool {
        !self.discontinuity && !self.random_access && self.pcr.is_none()
    }
}

/// Encode a PCR field the way [`vaco_format_mpegts_tables::packet::Pcr::parse`]
/// decodes it, byte for byte — this is the function `crate::tsw`'s own tests
/// round-trip against that parser directly, since a wrong bit here is exactly
/// the kind of error the brief calls out as invisible until playback breaks.
fn encode_pcr(pcr: Pcr) -> [u8; 6] {
    let base = (pcr.base as u64) & ((1u64 << 33) - 1);
    let hi32 = (base >> 1) as u32;
    let lsb = (base & 1) as u8;
    let ext = pcr.extension & 0x01FF;
    let b4 = (lsb << 7) | 0x7E | ((ext >> 8) as u8 & 1);
    let b5 = (ext & 0xFF) as u8;
    let hi = hi32.to_be_bytes();
    [hi[0], hi[1], hi[2], hi[3], b4, b5]
}

/// Build one adaptation field's bytes, `total_wire_len` long (length byte
/// included). `total_wire_len == 0` means "no adaptation field at all" and
/// this returns an empty vector; the caller decides `adaptation_field_control`
/// from that, not from inspecting the bytes.
fn encode_af(req: AfRequest, total_wire_len: usize) -> Vec<u8> {
    if total_wire_len == 0 {
        return Vec::new();
    }
    let length_value = total_wire_len - 1;
    let mut af = Vec::new();
    af.push(length_value as u8);
    if length_value == 0 {
        // "a length byte of zero is legal and means one stuffing byte, no
        // flags" — packet.rs's own doc for `AdaptationField::parse`.
        return af;
    }
    let mut flags = 0u8;
    if req.discontinuity {
        flags |= 0x80;
    }
    if req.random_access {
        flags |= 0x40;
    }
    if req.pcr.is_some() {
        flags |= 0x10;
    }
    af.push(flags);
    if let Some(pcr) = req.pcr {
        af.extend_from_slice(&encode_pcr(pcr));
    }
    while af.len() < total_wire_len {
        af.push(0xFF);
    }
    af
}

/// How the last, partial packet of a run is padded to 188 bytes.
///
/// The two containers this writer serves pad differently, and mixing them up
/// produces a file that happens to parse but is not what any real muxer
/// emits:
///
/// * A **PES** payload is elementary-stream bytes; appending raw `0xFF` after
///   the last of them would corrupt the stream, so the padding has to live
///   in an adaptation field instead ([`TsWriter::write_pes`]).
/// * A **PSI section**'s own framing already treats a run of `0xFF` after a
///   section as stuffing-to-end-of-packet — see
///   [`vaco_format_mpegts_tables::section::SectionAssembler`]'s doc — so the
///   reference pads a short PAT/PMT/SDT packet with trailing `0xFF` payload
///   bytes and never attaches an adaptation field just to make the packet
///   188 bytes long ([`TsWriter::write_section`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PadStyle {
    /// Shrink the payload and grow the adaptation field with `0xFF` stuffing
    /// bytes.
    AdaptationField,
    /// Keep the payload at full capacity; bytes past the real data are left
    /// as the packet buffer's own `0xFF` fill.
    TrailingBytes,
}

/// Writes 188-byte transport packets (optionally M2TS-wrapped) to a sink,
/// tracking one continuity counter per PID.
// `Box<dyn MediaSink>` is not `Debug`, hence the hand-written impl below
// rather than `#[derive(Debug)]`.
pub struct TsWriter {
    sink: Box<dyn MediaSink>,
    m2ts: bool,
    /// `(pid, continuity_counter)`. A `Vec` rather than a map: real streams
    /// carry a handful of PIDs (PAT, PMT, SDT, a few elementary streams), so
    /// a linear scan is both simpler and faster than hashing here.
    cc: Vec<(u16, u8)>,
    bytes_written: u64,
    /// Bits per second used to pace M2TS arrival timestamps when
    /// [`crate::options::MpegTsMuxOptions::muxrate_bps`] is unset. Not a claim
    /// about the reference's own internal rate — see the crate docs' M2TS
    /// section for what this approximates and why.
    nominal_bps: u64,
}

/// Used to pace M2TS's arrival-time-stamp field when no `-muxrate` is given.
/// Arbitrary but documented: a modest HD broadcast rate, chosen only so the
/// field is monotonically increasing at a plausible pace rather than because
/// it matches any specific reference behaviour.
pub const NOMINAL_ATS_RATE_BPS: u64 = 20_000_000;

impl core::fmt::Debug for TsWriter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TsWriter")
            .field("m2ts", &self.m2ts)
            .field("pids", &self.cc.len())
            .field("bytes_written", &self.bytes_written)
            .finish_non_exhaustive()
    }
}

impl TsWriter {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>, m2ts: bool, muxrate_bps: Option<u64>) -> Self {
        Self {
            sink,
            m2ts,
            cc: Vec::new(),
            bytes_written: 0,
            nominal_bps: muxrate_bps.unwrap_or(NOMINAL_ATS_RATE_BPS).max(1),
        }
    }

    /// The continuity counter for `pid`, then advance it — only when this
    /// packet carries a payload, per §2.4.3.3.
    fn next_cc(&mut self, pid: u16, has_payload: bool) -> u8 {
        if let Some(entry) = self.cc.iter_mut().find(|(p, _)| *p == pid) {
            let val = entry.1;
            if has_payload {
                entry.1 = (entry.1 + 1) & 0x0F;
            }
            val
        } else {
            self.cc.push((pid, u8::from(has_payload)));
            0
        }
    }

    /// Mark `pid` as needing a discontinuous continuity counter on its next
    /// packet — `-mpegts_flags initial_discontinuity`'s mechanism, and
    /// unrelated to the adaptation field's own `discontinuity_indicator`
    /// (that one is a *stream* discontinuity signal to a decoder; this is
    /// "the counter itself just jumped", which a demuxer's continuity check
    /// must not flag as loss). This crate does not track *counter*
    /// discontinuity separately from stream discontinuity — see the crate
    /// docs — so `initial_discontinuity` is implemented via the adaptation
    /// field flag alone, which is what every decoder actually keys off.
    pub fn reset_cc(&mut self, pid: u16) {
        if let Some(entry) = self.cc.iter_mut().find(|(p, _)| *p == pid) {
            entry.1 = 0;
        } else {
            self.cc.push((pid, 0));
        }
    }

    /// Bytes written to the sink so far, M2TS prefixes included. Used for
    /// nothing this crate ships yet beyond ATS pacing, and exposed because a
    /// caller computing `-muxrate` stuffing needs exactly this number.
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// A monotonically non-decreasing 30-bit M2TS arrival time stamp, paced
    /// by bytes written so far at [`Self::nominal_bps`].
    ///
    /// This is a documented approximation, not a transcription of the
    /// reference's own scheduling: probing real `ffmpeg -mpegts_m2ts_mode 1`
    /// output found its first few ATS values are **not** monotonic in file
    /// order (the SDT's ATS was larger than the PAT's, even though the SDT
    /// is the earlier packet in the file) — evidence its ATS is computed
    /// from an internal multiplexing order this crate does not reconstruct.
    /// A byte-rate-derived, monotonic ATS is Blu-ray-legal and self-
    /// consistent; it is not proven byte-identical to the reference.
    #[allow(
        clippy::integer_division,
        reason = "a 27 MHz tick count from a bit rate is inherently approximate; \
                  truncation here is no less accurate than the float alternative"
    )]
    fn next_ats(&self) -> u32 {
        let ticks = (self.bytes_written)
            .saturating_mul(8)
            .saturating_mul(27_000_000)
            / self.nominal_bps;
        (ticks & 0x3FFF_FFFF) as u32
    }

    /// Write one 188-byte packet, M2TS-prefixed if configured.
    fn emit(&mut self, packet: &[u8; TS_PACKET_SIZE]) -> Result<()> {
        if self.m2ts {
            let ats = self.next_ats();
            self.sink.write(&ats.to_be_bytes())?;
            self.bytes_written = self.bytes_written.saturating_add(4);
        }
        self.sink.write(packet)?;
        self.bytes_written = self.bytes_written.saturating_add(TS_PACKET_SIZE as u64);
        Ok(())
    }

    /// Split `payload` across as many 188-byte packets on `pid` as it takes.
    ///
    /// `pointer_field`, when `Some`, is written as the first byte of the
    /// first packet's payload with `payload_unit_start_indicator` set — the
    /// PSI-section convention. `af` decorates only the first packet.
    fn write_payload(
        &mut self,
        pid: u16,
        pointer_field: Option<u8>,
        payload: &[u8],
        af: AfRequest,
        pad: PadStyle,
    ) -> Result<()> {
        if pid > 0x1FFF {
            return Err(Error::InvalidData("mpegts: pid does not fit 13 bits"));
        }
        let mut prefixed = Vec::new();
        if let Some(p) = pointer_field {
            prefixed.push(p);
        }
        prefixed.extend_from_slice(payload);

        let mut pos = 0usize;
        let mut first = true;
        loop {
            let remaining = prefixed.len().saturating_sub(pos);
            if remaining == 0 && !first {
                break;
            }
            let this_af = if first { af } else { AfRequest::default() };
            let min_content = this_af.min_content_len();
            let min_af_wire = if min_content > 0 { 1 + min_content } else { 0 };
            // How much payload fits after any mandatory AF content, if we
            // write no stuffing at all.
            let capacity_no_stuff = BODY_LEN.saturating_sub(min_af_wire);
            let (real_len, payload_len, af_wire_len) =
                if remaining >= capacity_no_stuff && min_content > 0 {
                    (capacity_no_stuff, capacity_no_stuff, min_af_wire)
                } else if remaining >= BODY_LEN && min_content == 0 {
                    (BODY_LEN, BODY_LEN, 0)
                } else {
                    // Last packet of this payload.
                    match pad {
                        PadStyle::AdaptationField => {
                            let payload_len = remaining;
                            let af_wire_len = BODY_LEN - payload_len;
                            (remaining, payload_len, af_wire_len)
                        }
                        PadStyle::TrailingBytes => {
                            let payload_len = BODY_LEN - min_af_wire;
                            (remaining, payload_len, min_af_wire)
                        }
                    }
                };

            let has_payload = payload_len > 0;
            let cc = self.next_cc(pid, has_payload);
            // `payload_unit_start_indicator` marks "a new PES packet or PSI
            // section begins here" and is set on the first packet of *every*
            // `write_payload` call, PES or PSI alike — `pointer_field` only
            // controls whether a leading pointer byte precedes the section,
            // which is a PSI-only concept the PES path does not use at all.
            let pusi = first;

            let mut pkt = [0xFFu8; TS_PACKET_SIZE];
            pkt[0] = 0x47;
            pkt[1] = (u8::from(pusi) << 6) | ((pid >> 8) as u8 & 0x1F);
            pkt[2] = (pid & 0xFF) as u8;
            let afc = match (af_wire_len > 0, has_payload) {
                (true, true) => 0x3,
                (true, false) => 0x2,
                // `(false, false)` is unreachable in practice — this writer
                // never builds an empty packet — and falls back to the same
                // "payload only" encoding as `(false, true)` rather than
                // being treated as an error.
                (false, _) => 0x1,
            };
            pkt[3] = (afc << 4) | (cc & 0x0F);

            let mut at = 4usize;
            if af_wire_len > 0 {
                let af_bytes = encode_af(this_af, af_wire_len);
                if let Some(dst) = pkt.get_mut(at..at.saturating_add(af_bytes.len())) {
                    dst.copy_from_slice(&af_bytes);
                }
                at = at.saturating_add(af_bytes.len());
            }
            // Only `real_len` bytes actually exist in `prefixed`; any excess
            // up to `payload_len` (the `TrailingBytes` pad case) is left as
            // the packet buffer's own `0xFF` initial fill.
            if let (Some(src), Some(dst)) = (
                prefixed.get(pos..pos.saturating_add(real_len)),
                pkt.get_mut(at..at.saturating_add(real_len)),
            ) {
                dst.copy_from_slice(src);
            }

            self.emit(&pkt)?;
            pos = pos.saturating_add(real_len);
            first = false;
        }
        Ok(())
    }

    /// Write a complete PSI section (PAT, PMT, SDT, ...): `payload_unit_start`
    /// set, `pointer_field` `0`, and any short final packet padded with
    /// trailing `0xFF` bytes rather than an adaptation field.
    ///
    /// # Errors
    /// Propagates I/O failure, and [`vaco_core::Error::InvalidData`] if `pid`
    /// does not fit the 13-bit field.
    pub fn write_section(&mut self, pid: u16, section: &[u8], af: AfRequest) -> Result<()> {
        self.write_payload(pid, Some(0), section, af, PadStyle::TrailingBytes)
    }

    /// Write a complete PES packet's bytes: no `pointer_field`, and any short
    /// final packet padded with a stuffed adaptation field.
    ///
    /// # Errors
    /// Propagates I/O failure, and [`vaco_core::Error::InvalidData`] if `pid`
    /// does not fit the 13-bit field.
    pub fn write_pes(&mut self, pid: u16, pes: &[u8], af: AfRequest) -> Result<()> {
        self.write_payload(pid, None, pes, af, PadStyle::AdaptationField)
    }

    /// # Errors
    /// Propagates I/O failure.
    pub fn flush(&mut self) -> Result<()> {
        self.sink.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_mpegts_tables::packet::{AdaptationField, TsHeader, TsPacket};
    use vaco_io::DynBuf;

    fn writer(m2ts: bool) -> (TsWriter, vaco_io::SharedDynBuf) {
        let sink = vaco_io::SharedDynBuf::new();
        let mirror = sink.clone();
        (TsWriter::new(Box::new(sink), m2ts, None), mirror)
    }

    #[test]
    fn a_small_payload_fills_one_packet_with_stuffing() {
        let (mut w, mirror) = writer(false);
        w.write_pes(0x100, b"hello", AfRequest::default()).unwrap();
        let bytes = mirror.take();
        assert_eq!(bytes.len(), TS_PACKET_SIZE);
        let pkt = TsPacket::parse(&bytes).unwrap();
        assert_eq!(pkt.header.pid, 0x100);
        assert!(pkt.header.has_payload());
        assert_eq!(pkt.payload, b"hello");
    }

    #[test]
    fn a_pcr_is_written_and_reads_back_exactly() {
        let (mut w, mirror) = writer(false);
        let pcr = Pcr {
            base: 123_456_789,
            extension: 42,
        };
        w.write_pes(
            0x100,
            b"x",
            AfRequest {
                pcr: Some(pcr),
                ..AfRequest::default()
            },
        )
        .unwrap();
        let bytes = mirror.take();
        let pkt = TsPacket::parse(&bytes).unwrap();
        assert_eq!(pkt.pcr(), Some(pcr));
    }

    #[test]
    fn a_payload_spanning_two_packets_carries_no_af_on_the_first() {
        let (mut w, mirror) = writer(false);
        let payload = vec![0xABu8; BODY_LEN + 10];
        w.write_pes(0x101, &payload, AfRequest::default()).unwrap();
        let bytes = mirror.take();
        assert_eq!(bytes.len(), TS_PACKET_SIZE * 2);
        let p0 = TsPacket::parse(&bytes[..TS_PACKET_SIZE]).unwrap();
        assert!(p0.adaptation.is_none());
        assert_eq!(p0.payload.len(), BODY_LEN);
        let p1 = TsPacket::parse(&bytes[TS_PACKET_SIZE..]).unwrap();
        assert_eq!(p1.payload.len(), 10);
    }

    #[test]
    fn a_pointer_field_is_written_with_payload_unit_start() {
        let (mut w, mirror) = writer(false);
        w.write_section(0, &[1, 2, 3], AfRequest::default())
            .unwrap();
        let bytes = mirror.take();
        assert_eq!(bytes.len(), TS_PACKET_SIZE);
        let hdr = TsHeader::parse(&bytes).unwrap();
        assert!(hdr.payload_unit_start);
        assert!(
            !hdr.has_adaptation(),
            "a short section pads with trailing bytes, not an AF"
        );
        let pkt = TsPacket::parse(&bytes).unwrap();
        // Pointer field 0, then the three section bytes, then trailing 0xFF
        // stuffing to fill the rest of the payload — the PSI convention a
        // demuxer's `SectionAssembler` relies on.
        assert_eq!(pkt.payload.len(), BODY_LEN);
        assert_eq!(&pkt.payload[..4], &[0, 1, 2, 3]);
        assert!(pkt.payload[4..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn continuity_counters_advance_per_pid_and_wrap_at_sixteen() {
        let (mut w, mirror) = writer(false);
        for _ in 0..20 {
            w.write_pes(0x200, b"a", AfRequest::default()).unwrap();
        }
        let bytes = mirror.take();
        let ccs: Vec<u8> = (0..20)
            .map(|i| {
                TsHeader::parse(&bytes[i * TS_PACKET_SIZE..])
                    .unwrap()
                    .continuity
            })
            .collect();
        let want: Vec<u8> = (0..20).map(|i| (i % 16) as u8).collect();
        assert_eq!(ccs, want);
    }

    #[test]
    fn two_pids_keep_independent_counters() {
        let (mut w, mirror) = writer(false);
        w.write_pes(0x1, b"a", AfRequest::default()).unwrap();
        w.write_pes(0x2, b"a", AfRequest::default()).unwrap();
        w.write_pes(0x1, b"a", AfRequest::default()).unwrap();
        let bytes = mirror.take();
        let cc = |i: usize| {
            TsHeader::parse(&bytes[i * TS_PACKET_SIZE..])
                .unwrap()
                .continuity
        };
        assert_eq!(cc(0), 0);
        assert_eq!(cc(1), 0);
        assert_eq!(cc(2), 1);
    }

    #[test]
    fn m2ts_mode_prepends_four_bytes_and_stays_188_aligned_after_that() {
        let (mut w, mirror) = writer(true);
        w.write_pes(0x100, b"hello", AfRequest::default()).unwrap();
        let bytes = mirror.take();
        assert_eq!(bytes.len(), 4 + TS_PACKET_SIZE);
        assert_eq!(bytes[4], 0x47);
        // The 30-bit ATS's two reserved top bits (copy_permission_indicator)
        // must be zero for the very first packet of a fresh writer.
        assert_eq!(bytes[0] & 0xC0, 0);
    }

    #[test]
    fn an_empty_pid_returns_zero_before_ever_being_written() {
        let mut w = TsWriter::new(Box::new(DynBuf::new()), false, None);
        assert_eq!(w.next_cc(0x123, false), 0);
        assert_eq!(w.next_cc(0x123, false), 0);
    }

    #[test]
    fn discontinuity_and_random_access_flags_round_trip() {
        let (mut w, mirror) = writer(false);
        w.write_pes(
            0x100,
            b"x",
            AfRequest {
                discontinuity: true,
                random_access: true,
                pcr: None,
            },
        )
        .unwrap();
        let bytes = mirror.take();
        let pkt = TsPacket::parse(&bytes).unwrap();
        let af: AdaptationField = pkt.adaptation.unwrap();
        assert!(af.discontinuity);
        assert!(af.random_access);
    }

    #[test]
    fn a_pid_over_thirteen_bits_is_refused() {
        let (mut w, _mirror) = writer(false);
        assert!(w.write_pes(0x2000, b"x", AfRequest::default()).is_err());
    }

    proptest::proptest! {
        #[test]
        fn any_payload_length_reassembles_to_the_original_bytes(
            len in 0usize..2000,
        ) {
            let (mut w, mirror) = writer(false);
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            w.write_pes(0x111, &payload, AfRequest::default()).unwrap();
            let bytes = mirror.take();
            proptest::prop_assert_eq!(bytes.len() % TS_PACKET_SIZE, 0);
            let mut recovered = Vec::new();
            for chunk in bytes.chunks(TS_PACKET_SIZE) {
                let pkt = TsPacket::parse(chunk).unwrap();
                recovered.extend_from_slice(pkt.payload);
            }
            proptest::prop_assert_eq!(recovered, payload);
        }

        #[test]
        fn any_pcr_round_trips_through_the_parser(
            base in 0i64..(1i64 << 33),
            extension in 0u16..300,
        ) {
            let (mut w, mirror) = writer(false);
            let pcr = Pcr { base, extension };
            w.write_pes(0x100, b"x", AfRequest { pcr: Some(pcr), ..AfRequest::default() }).unwrap();
            let bytes = mirror.take();
            let pkt = TsPacket::parse(&bytes).unwrap();
            proptest::prop_assert_eq!(pkt.pcr(), Some(pcr));
        }
    }
}
