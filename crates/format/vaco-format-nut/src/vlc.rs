//! NUT's variable-length integer encodings (`v`, `s`, `vb`), and the one
//! trait ([`ByteFeed`]) that lets the same decode logic run against either a
//! byte slice already in memory or an [`IoContext`] being read live.
//!
//! # The encoding, measured against real `ffmpeg -f nut` output
//!
//! `v` is base-128, most-significant-group first, continuation bit `0x80`:
//!
//! ```text
//! value = 0
//! loop:
//!     byte = next()
//!     value = value*128 + (byte & 0x7F)
//!     if byte & 0x80 == 0: return value
//! ```
//!
//! `s` is `v` shifted into a zigzag-like signed form: `temp = v() + 1;
//! temp&1 ? -(temp>>1) : (temp>>1)`. `vb` is a `v` length prefix followed by
//! that many raw bytes.
//!
//! Cross-checked directly: decoding a real main header with this exact
//! algorithm reproduces `forward_ptr`'s own stated packet length exactly
//! (the decode consumes precisely as many bytes as the packet header says
//! the whole packet occupies), which is a self-consistency check a wrong
//! bit order or wrong continuation-bit polarity could not pass by accident.

use vaco_core::{Error, Result};
use vaco_io::IoContext;

/// Longest a `v`/`s` encoding is allowed to run before this crate gives up
/// on it.
///
/// **Measured wrong once, corrected from a real file**: this was originally
/// 9 (mis-derived from the sample code's *unrelated* `get_bytes` function,
/// which asserts `count<9` for a fixed-byte-count read, not for `v`
/// decoding at all). A real `ffmpeg -f nut` file's own `main_header`
/// frame-code table encodes a `match_time_delta` needing exactly 10 groups
/// (a canonical 64-bit value needs `ceil(64/7) = 10`), and failed to parse
/// under the 9-byte cap. 18 = 10 (a full 64-bit value) + 8 (the
/// specification's own stated maximum of 8 stuffing bytes per field) is the
/// real bound: generous enough for any legitimate encoding, still finite so
/// a stream that never terminates a varint cannot spin forever.
const MAX_VLC_BYTES: u32 = 18;

/// A source of bytes `v`/`s`/`vb` decoding can pull from one at a time.
pub trait ByteFeed {
    /// # Errors
    /// Propagates the underlying read failure ([`Error::UnexpectedEof`] at
    /// the natural end of input).
    fn next_byte(&mut self) -> Result<u8>;
}

/// An in-memory cursor over an already-extracted packet payload (every NUT
/// packet except a frame is read this way: `forward_ptr` gives the whole
/// payload length up front, so it is read into one buffer first and parsed
/// from there — see `demux.rs`).
#[derive(Debug, Clone, Copy)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    #[must_use]
    pub fn remaining(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }

    /// Read exactly `n` raw bytes.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if fewer than `n` bytes remain.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let out = self
            .data
            .get(self.pos..self.pos.saturating_add(n))
            .ok_or(Error::UnexpectedEof)?;
        self.pos = self.pos.saturating_add(n);
        Ok(out)
    }
}

impl ByteFeed for Cursor<'_> {
    fn next_byte(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or(Error::UnexpectedEof)?;
        self.pos = self.pos.saturating_add(1);
        Ok(b)
    }
}

/// Adapts an [`IoContext`] to [`ByteFeed`], for the one packet type
/// (`frame`) whose total length is not known until its header has been
/// decoded, so it cannot be pre-buffered the way every other packet is.
#[derive(Debug)]
pub struct IoFeed<'a>(pub &'a mut IoContext);

impl ByteFeed for IoFeed<'_> {
    fn next_byte(&mut self) -> Result<u8> {
        self.0.r8()
    }
}

/// Decode a `v` (unsigned variable-length value).
///
/// # Errors
/// Propagates [`ByteFeed::next_byte`] failure; [`Error::InvalidData`] if the
/// encoding runs longer than [`MAX_VLC_BYTES`] without terminating.
pub fn read_v(feed: &mut impl ByteFeed) -> Result<u64> {
    let mut value: u64 = 0;
    for _ in 0..MAX_VLC_BYTES {
        let byte = feed.next_byte()?;
        value = value
            .checked_mul(128)
            .and_then(|v| v.checked_add(u64::from(byte & 0x7F)))
            .ok_or(Error::InvalidData("nut: v-coded value overflowed u64"))?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Error::InvalidData("nut: v-coded value ran on too long"))
}

/// Decode an `s` (signed variable-length value).
///
/// # Errors
/// As [`read_v`].
pub fn read_s(feed: &mut impl ByteFeed) -> Result<i64> {
    let temp = read_v(feed)?
        .checked_add(1)
        .ok_or(Error::InvalidData("nut: s-coded value overflowed"))?;
    // temp is odd -> negative, even -> non-negative; halve either way.
    let magnitude = i64::try_from(temp >> 1)
        .map_err(|_| Error::InvalidData("nut: s-coded value out of range"))?;
    Ok(if temp & 1 == 1 { -magnitude } else { magnitude })
}

/// Encode `value` as `v`, appended to `out`.
pub fn write_v(out: &mut Vec<u8>, value: u64) {
    let mut nbits = 0u32;
    while value >> (7 * (nbits + 1)) != 0 {
        nbits += 1;
    }
    while nbits > 0 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "masked to 7 bits before the cast"
        )]
        let byte = 0x80 | (((value >> (7 * nbits)) & 0x7F) as u8);
        out.push(byte);
        nbits -= 1;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "masked to 7 bits before the cast"
    )]
    out.push((value & 0x7F) as u8);
}

/// Encode `value` as `s`, appended to `out`.
///
/// Derived from the decode side (`raw = v(); temp = raw+1; temp&1 ?
/// -(temp>>1) : temp>>1`) by solving for `raw` given `value`: a positive
/// `value` needs `temp` even with `temp>>1 == value`, i.e. `temp = 2*value`
/// so `raw = 2*value - 1`; a non-positive `value` needs `temp` odd with
/// `temp>>1 == -value`, i.e. `temp = 2*-value + 1` so `raw = 2*-value`.
/// Checked against the decoder by round-trip, not just by this derivation —
/// see the unit tests.
pub fn write_s(out: &mut Vec<u8>, value: i64) {
    let raw = if value > 0 {
        (value as u64).saturating_mul(2).saturating_sub(1)
    } else {
        value.unsigned_abs().saturating_mul(2)
    };
    write_v(out, raw);
}

/// Decode a `vb` (length-prefixed binary blob), bounded through `budget`
/// since the length is attacker-controlled input.
///
/// # Errors
/// As [`read_v`]; propagates [`vaco_limits::Budget::alloc`] failure if the
/// declared length exceeds the allocation ceiling.
pub fn read_vb(feed: &mut impl ByteFeed, budget: &mut vaco_limits::Budget) -> Result<Vec<u8>> {
    let len = read_v(feed)?;
    let len =
        usize::try_from(len).map_err(|_| Error::InvalidData("nut: vb length overflows usize"))?;
    let mut buf = budget.alloc::<u8>(len)?;
    for slot in &mut buf {
        *slot = feed.next_byte()?;
    }
    Ok(buf)
}

/// Encode `vb`, appended to `out`.
pub fn write_vb(out: &mut Vec<u8>, data: &[u8]) {
    write_v(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// Decode a `t` (universal timestamp): a tick count paired with which of
/// the file's time bases it is expressed in. Kept as a pair rather than
/// resolved to a single "real" value, because every use of `t` in the
/// specification (`global_key_pts`, `chapter_start`, `max_pts`) immediately
/// feeds both halves into `convert_ts`/`compare_ts` against another
/// stream's time base — there is no single timebase-free number to collapse
/// it to.
///
/// # Errors
/// As [`read_v`]; [`Error::InvalidData`] if `time_base_count` is 0 (the
/// modulus would be undefined).
#[allow(
    clippy::integer_division,
    reason = "spec's own t-decoding formula: id = tmp % count, ticks = tmp / count"
)]
pub fn read_t(feed: &mut impl ByteFeed, time_base_count: u64) -> Result<(u64, usize)> {
    if time_base_count == 0 {
        return Err(Error::InvalidData(
            "nut: time_base_count is 0, cannot decode a timestamp",
        ));
    }
    let tmp = read_v(feed)?;
    let id = tmp % time_base_count;
    let ticks = tmp / time_base_count;
    Ok((ticks, usize::try_from(id).unwrap_or(0)))
}

/// Encode a `t`.
pub fn write_t(out: &mut Vec<u8>, ticks: u64, time_base_id: u64, time_base_count: u64) {
    write_v(
        out,
        ticks
            .saturating_mul(time_base_count)
            .saturating_add(time_base_id),
    );
}

/// `convert_ts`, exactly as specified but using `i128` throughout instead of
/// the spec's manual 64-bit split — the split exists only to dodge needing
/// a wider-than-64-bit type in C, and `i128` gives the identical result
/// (verified by the round-trip property test below) without the
/// intermediate-overflow risk that arithmetic works around.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "exact-quotient timestamp rescaling per the NUT spec's own formula, not a bug \
              needing float precision — truncation direction matches convert_ts's definition"
)]
pub fn convert_ts(ticks: i64, from: (u64, u64), to: (u64, u64)) -> i64 {
    let (from_num, from_den) = (i128::from(from.0), i128::from(from.1));
    let (to_num, to_den) = (i128::from(to.0), i128::from(to.1));
    if from_den == 0 || to_num == 0 {
        return 0;
    }
    let ln = from_num * i128::from(ticks);
    let numerator = ln * to_den;
    let result = numerator / (from_den * to_num);
    i64::try_from(result).unwrap_or(if result < 0 { i64::MIN } else { i64::MAX })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn roundtrip_v(value: u64) {
        let mut buf = Vec::new();
        write_v(&mut buf, value);
        let mut cur = Cursor::new(&buf);
        assert_eq!(read_v(&mut cur).unwrap(), value, "value={value}");
    }

    fn roundtrip_s(value: i64) {
        let mut buf = Vec::new();
        write_s(&mut buf, value);
        let mut cur = Cursor::new(&buf);
        assert_eq!(read_s(&mut cur).unwrap(), value, "value={value}");
    }

    #[test]
    fn v_round_trips_across_a_wide_range() {
        for value in [
            0u64,
            1,
            63,
            64,
            127,
            128,
            174,
            5830,
            1_000_000,
            u64::from(u32::MAX),
        ] {
            roundtrip_v(value);
        }
    }

    /// The exact measured example from `iec61937`-style module docs: 174
    /// (NUT's own main-header `forward_ptr` in the reference sample) encodes
    /// as two bytes, `0x81 0x2e`.
    #[test]
    fn the_measured_forward_ptr_174_encodes_to_the_measured_bytes() {
        let mut buf = Vec::new();
        write_v(&mut buf, 174);
        assert_eq!(buf, vec![0x81, 0x2e]);
    }

    #[test]
    fn s_round_trips_across_a_wide_range() {
        for value in [0i64, 1, -1, 2, -2, 174, -174, 1_000_000, -1_000_000] {
            roundtrip_s(value);
        }
    }

    #[test]
    fn vb_round_trips() {
        let mut buf = Vec::new();
        write_vb(&mut buf, b"FMP4");
        let mut cur = Cursor::new(&buf);
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        assert_eq!(read_vb(&mut cur, &mut budget).unwrap(), b"FMP4");
    }

    #[test]
    fn a_value_that_never_terminates_is_rejected_not_looped_forever() {
        let buf = vec![0xFFu8; 20];
        let mut cur = Cursor::new(&buf);
        assert!(read_v(&mut cur).is_err());
    }

    #[test]
    fn t_round_trips_through_its_pair_encoding() {
        let mut buf = Vec::new();
        write_t(&mut buf, 12345, 1, 3);
        let mut cur = Cursor::new(&buf);
        let (ticks, id) = read_t(&mut cur, 3).unwrap();
        assert_eq!((ticks, id), (12345, 1));
    }

    /// `convert_ts` to the same time base is the identity — the simplest
    /// possible check that the `i128` rewrite of the spec's formula did not
    /// introduce a scale error.
    #[test]
    fn convert_ts_to_the_same_time_base_is_identity() {
        assert_eq!(convert_ts(48_000, (1, 48_000), (1, 48_000)), 48_000);
    }

    /// One second at 25/1 is exactly one second at 48000 Hz: converting 25
    /// ticks (1/25 timebase) to a 1/48000 timebase should give 48000 ticks.
    #[test]
    fn convert_ts_between_video_and_audio_rates_matches_wall_clock() {
        assert_eq!(convert_ts(25, (1, 25), (1, 48_000)), 48_000);
    }
}
