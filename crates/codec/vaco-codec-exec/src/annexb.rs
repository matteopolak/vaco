//! Split an Annex-B byte stream into one packet per access unit.
//!
//! Both ITU-T H.264 and H.265 write NAL units as `00 00 01 <nal> ...` or
//! `00 00 00 01 <nal> ...` (a leading start code, 3 or 4 bytes). Working out
//! where one *access unit* (one encoded frame's worth of NAL units) ends and
//! the next begins normally needs a real bitstream parser — a slice header's
//! `first_mb_in_slice` field, in the general case.
//!
//! This crate does not need the general case, because it controls the
//! encoder's command line: both `x264` and `x265` are invoked with `--aud`
//! (`crate::encoder`'s `X264`/`X265` tool specs), which makes every access unit begin
//! with an Access Unit Delimiter NAL — H.264 type 9 (ITU-T H.264 §7.4.1.2.3),
//! H.265 type 35 (ITU-T H.265 §7.4.3.5) — and nothing else ever emits one.
//! Splitting on "a new AUD started" is therefore exact, not a heuristic,
//! for any stream this crate itself produced the command line for.
//!
//! # NAL header shape (why H.264 and H.265 need different masks)
//!
//! H.264's NAL header is one byte: `forbidden_zero_bit(1) nal_ref_idc(2)
//! nal_unit_type(5)` — the type is the low 5 bits.
//!
//! H.265's NAL header is two bytes: `forbidden_zero_bit(1) nal_unit_type(6)
//! nuh_layer_id(6) nuh_temporal_id_plus1(3)` — the type is bits 1-6 of the
//! *first* byte, i.e. `(byte0 >> 1) & 0x3f`.

use vaco_core::Result;
use vaco_limits::Budget;
use vaco_pool::Buffer;

/// Which family of NAL header this stream uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalFamily {
    H264,
    H265,
}

impl NalFamily {
    /// The `nal_unit_type` of an Access Unit Delimiter in this family.
    const fn aud_type(self) -> u8 {
        match self {
            Self::H264 => 9,
            Self::H265 => 35,
        }
    }

    /// The `nal_unit_type` of a keyframe-carrying NAL: H.264's IDR slice
    /// (type 5), or any of H.265's thirteen IRAP types (16-28, ITU-T H.265
    /// Table 7-1) — encoders in practice only ever emit 19 (`IDR_W_RADL`),
    /// 20 (`IDR_N_LP`) or 21 (`CRA_NUT`), but the full range is cheap to
    /// recognise and correct either way.
    const fn is_key_type(self, nal_type: u8) -> bool {
        match self {
            Self::H264 => nal_type == 5,
            Self::H265 => nal_type >= 16 && nal_type <= 21,
        }
    }

    /// `nal_unit_type` from the header byte(s) immediately after a start code.
    fn nal_type(self, header: &[u8]) -> Option<u8> {
        match self {
            Self::H264 => header.first().map(|b| b & 0x1f),
            Self::H265 => header.first().map(|b| (b >> 1) & 0x3f),
        }
    }
}

/// One `(start_code_len, nal_header_offset)` match, found by scanning forward
/// from `from`.
fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data.get(i..i + 3) == Some([0u8, 0, 1].as_slice()) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Incremental Annex-B → access-unit splitter.
///
/// Fed arbitrary byte chunks as they arrive from a child process's stdout
/// (which has no reason to align with NAL or frame boundaries); emits one
/// complete access unit at a time via [`Splitter::push`]'s return value, plus
/// whatever is left over at end of stream via [`Splitter::finish`].
#[derive(Debug)]
pub struct Splitter {
    family: NalFamily,
    /// Bytes seen so far that have not yet been attributed to a completed
    /// access unit.
    pending: Vec<u8>,
}

impl Splitter {
    #[must_use]
    pub const fn new(family: NalFamily) -> Self {
        Self { family, pending: Vec::new() }
    }

    /// Feed more bytes; returns every access unit that is now fully known
    /// (everything up to, but not including, the next AUD's start code).
    ///
    /// An AUD found at offset 0 of `pending` is never a cut point — it is
    /// either the very first byte of the whole stream (nothing precedes it
    /// to close off) or the AUD that starts the unit currently being
    /// accumulated (left there by the previous cut). Only an AUD found
    /// strictly after the start of `pending` closes a unit. This is also
    /// what makes a leading access unit with **no** AUD at all — measured
    /// against real `x265 --aud` output, whose very first frame's VPS/SPS/
    /// PPS/SEI/IDR-slice run has no AUD in front of it even though every
    /// later frame gets one — split correctly: the first cut this finds is
    /// simply the first AUD in the whole stream, wherever it falls, and
    /// everything before it becomes access unit zero.
    ///
    /// # Errors
    /// [`vaco_core::Error::LimitExceeded`] if buffering `chunk` would exceed
    /// `budget` — an unbounded Annex-B accumulator fed by an external process
    /// is exactly the attacker/misbehaving-process-controlled allocation
    /// `vaco-limits` exists to bound, even though the "attacker" here is a
    /// local tool the user chose to run.
    pub fn push(&mut self, chunk: &[u8], budget: &mut Budget) -> Result<Vec<Vec<u8>>> {
        budget.charge(chunk.len().try_into().unwrap_or(u64::MAX))?;
        self.pending.extend_from_slice(chunk);
        let mut out = Vec::new();

        // Re-scan from the start each call rather than tracking a cursor:
        // `pending` shrinks every time a unit is emitted, so offsets from a
        // previous call would be stale.
        loop {
            let mut search_from = 0usize;
            let mut cut = None;
            while let Some(sc) = find_start_code(&self.pending, search_from) {
                let header_start = sc + 3;
                let is_four_byte = sc > 0 && self.pending.get(sc - 1) == Some(&0);
                let header = self.pending.get(header_start..).unwrap_or(&[]);
                let unit_start = if is_four_byte { sc - 1 } else { sc };
                if unit_start > 0 && self.family.nal_type(header) == Some(self.family.aud_type()) {
                    cut = Some(unit_start);
                    break;
                }
                search_from = sc + 3;
            }
            let Some(cut) = cut else { break };
            let unit: Vec<u8> = self.pending.drain(..cut).collect();
            out.push(unit);
        }
        Ok(out)
    }

    /// Whatever remains once the source is known to have ended (the final
    /// access unit has no following AUD to mark its end).
    #[must_use]
    pub fn finish(mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        }
    }
}

/// Whether any NAL unit in `access_unit` is a keyframe NAL for `family` —
/// used to set [`vaco_packet::PacketFlags::KEY`] on the emitted [`Packet`](vaco_packet::Packet).
#[must_use]
pub fn is_keyframe(access_unit: &[u8], family: NalFamily) -> bool {
    let mut from = 0;
    while let Some(sc) = find_start_code(access_unit, from) {
        let header = access_unit.get(sc + 3..).unwrap_or(&[]);
        if let Some(t) = family.nal_type(header)
            && family.is_key_type(t)
        {
            return true;
        }
        from = sc + 3;
    }
    false
}

/// Wrap `bytes` as a pooled [`vaco_pool::Buffer`], the shape every packet
/// payload in this workspace uses.
///
/// # Errors
/// [`vaco_core::Error::LimitExceeded`] if `bytes` would exceed `budget`.
pub fn to_buffer(bytes: &[u8], budget: &mut Budget) -> Result<Buffer> {
    Buffer::from_slice_padded(budget, bytes)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the crate, not the untrusted-input surface"
)]
mod tests {
    use super::*;

    fn aud(nal_type: u8) -> Vec<u8> {
        vec![0, 0, 0, 1, nal_type]
    }

    #[test]
    fn splits_two_access_units_at_the_second_aud() {
        let mut s = Splitter::new(NalFamily::H264);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut stream = aud(9);
        stream.extend_from_slice(&[0, 0, 0, 1, 0x67, 1, 2, 3]); // fake SPS
        stream.extend_from_slice(&aud(9));
        stream.extend_from_slice(&[0, 0, 0, 1, 0x41, 9, 9, 9]); // fake P slice

        let units = s.push(&stream, &mut budget).unwrap();
        assert_eq!(units.len(), 1, "the first AUD only opens the stream");
        let tail = s.finish().unwrap();
        assert!(tail.windows(4).any(|w| w == [0, 0, 0, 1]), "tail keeps its own AUD");
    }

    #[test]
    fn feeding_byte_at_a_time_still_splits_correctly() {
        let mut s = Splitter::new(NalFamily::H264);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut full = aud(9);
        full.extend_from_slice(&[0, 0, 0, 1, 0x65, 1]);
        full.extend_from_slice(&aud(9));
        full.extend_from_slice(&[0, 0, 0, 1, 0x41, 2]);
        full.extend_from_slice(&aud(9));
        full.extend_from_slice(&[0, 0, 0, 1, 0x41, 3]);

        let mut out = Vec::new();
        for b in &full {
            out.extend(s.push(std::slice::from_ref(b), &mut budget).unwrap());
        }
        if let Some(last) = s.finish() {
            out.push(last);
        }
        assert_eq!(out.len(), 3, "three AUDs => three access units");
    }

    /// Regression for a real bug: real `x265 --aud` output has NO AUD
    /// before the very first frame's VPS/SPS/PPS/SEI/IDR-slice run, only
    /// before every frame after it — unlike `x264`, which puts one in front
    /// of frame zero too. A splitter that assumes "the first AUD merely
    /// opens the stream" (this crate's first implementation) merges frame 0
    /// and frame 1 into one access unit here; caught by the real `x265`
    /// integration test in `encoder.rs`, reduced to this unit test.
    #[test]
    fn first_access_unit_with_no_leading_aud_still_splits_from_the_second() {
        let mut s = Splitter::new(NalFamily::H265);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut stream = vec![0, 0, 0, 1, 0x40, 1, 2]; // fake VPS (type 32), no AUD in front
        stream.extend_from_slice(&[0, 0, 0, 1, 0x26, 1]); // fake IDR (type 19)
        stream.extend_from_slice(&[0, 0, 0, 1, 0x46, 0]); // AUD (type 35) in front of frame 1 only
        stream.extend_from_slice(&[0, 0, 0, 1, 2, 1]); // fake trailing slice

        let units = s.push(&stream, &mut budget).unwrap();
        assert_eq!(units.len(), 1, "frame 0 closes off the moment frame 1's AUD appears");
        assert!(is_keyframe(&units[0], NalFamily::H265), "frame 0's IDR is still recognised without a leading AUD");
        let tail = s.finish().unwrap();
        assert!(!is_keyframe(&tail, NalFamily::H265));
    }

    #[test]
    fn h264_idr_is_detected_as_a_keyframe() {
        let unit = [0, 0, 0, 1, 9, 0, 0, 0, 1, 0x65, 1, 2, 3];
        assert!(is_keyframe(&unit, NalFamily::H264));
        let non_key = [0, 0, 0, 1, 9, 0, 0, 0, 1, 0x41, 1, 2, 3];
        assert!(!is_keyframe(&non_key, NalFamily::H264));
    }

    #[test]
    fn h265_idr_type_is_detected() {
        // nal_unit_type 19 (IDR_W_RADL) in bits 1..=6 of the first byte:
        // (19 << 1) = 0x26.
        let unit = [0, 0, 0, 1, 0x26, 1];
        assert!(is_keyframe(&unit, NalFamily::H265));
    }
}
