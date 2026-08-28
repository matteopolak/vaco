//! `SliceHeader`, `SliceContent`, `SliceFooter` (RFC 9043 §4.5-§4.9), and the
//! per-sample decode/encode loop (RFC 9043 §3.1-§3.6, §3.8) built on
//! [`crate::quant`] and [`crate::rangecoder`]/[`crate::rice`].
//!
//! `Vaco-Spec-Ref: rfc9043 RFC 9043 §3.1 (Border), §3.2 (Samples), §3.3
//! (Median Predictor), §3.5 (Context), §4.5-§4.9 (Slice/SliceHeader/
//! SliceContent/Line/SliceFooter pseudocode)`.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::quant::{QuantTableSet, compute_context, median_predictor};
use crate::rangecoder::{RangeDecoder, RangeEncoder, StateTransition, SymbolStates, fresh_states};
use crate::rice::{RiceState, RunState};

/// A rectangular sample buffer for one plane of one slice: `w*h` `i32`
/// samples in raster order, wide enough for up to 16-bit content even though
/// this crate's own encoder only reaches 8-bit today.
#[derive(Debug, Clone)]
pub(crate) struct SliceBuf {
    pub w: usize,
    pub h: usize,
    data: Vec<i32>,
}

impl SliceBuf {
    /// Zero-filled `w*h` buffer, bounded by `budget` since `w`/`h` come from
    /// an attacker-controlled bitstream on the decode path.
    ///
    /// # Errors
    /// Whatever [`Budget::alloc`] returns when `w*h` is over budget.
    pub(crate) fn alloc(budget: &mut Budget, w: usize, h: usize) -> Result<Self> {
        let n = w
            .checked_mul(h)
            .ok_or(Error::InvalidData("ffv1: slice too large"))?;
        Ok(Self {
            w,
            h,
            data: budget.alloc::<i32>(n)?,
        })
    }

    #[inline]
    pub(crate) fn get(&self, x: usize, y: usize) -> i32 {
        self.data.get(y * self.w + x).copied().unwrap_or(0)
    }

    #[inline]
    pub(crate) fn set(&mut self, x: usize, y: usize, v: i32) {
        if let Some(slot) = self.data.get_mut(y * self.w + x) {
            *slot = v;
        }
    }

    /// The six labelled border-aware neighbours (RFC 9043 §3.1-§3.2):
    /// `(l, t, tl, tr, ll, tt)`.
    #[inline]
    fn neighbours(&self, x: usize, y: usize) -> (i32, i32, i32, i32, i32, i32) {
        let xi = x.cast_signed();
        let yi = y.cast_signed();
        (
            self.border(xi - 1, yi),
            self.border(xi, yi - 1),
            self.border(xi - 1, yi - 1),
            self.border(xi + 1, yi - 1),
            self.border(xi - 2, yi),
            self.border(xi, yi - 2),
        )
    }

    /// RFC 9043 §3.1's assumed border, expressed as a lookup that also
    /// covers in-bounds positions (which are always already decoded, since
    /// every caller only asks for positions earlier in raster order).
    #[inline]
    fn border(&self, x: isize, y: isize) -> i32 {
        if y < 0 {
            // "Two rows of samples above the coded slice are assumed to be
            // 0" — covers y == -1 and y == -2 uniformly, for every column
            // including the border columns themselves.
            return 0;
        }
        let y = y as usize;
        if x <= -2 {
            // "An additional column of samples to the left... assumed 0."
            return 0;
        }
        if x == -1 {
            // "One column to the left is identical to the leftmost column
            // shifted down by one row; its topmost sample is 0."
            return if y == 0 { 0 } else { self.get(0, y - 1) };
        }
        let x = x as usize;
        if x < self.w {
            return self.get(x, y);
        }
        // "One column to the right is identical to the rightmost column"
        // (same row, no shift).
        self.get(self.w.saturating_sub(1), y)
    }
}

/// `SliceHeader` (RFC 9043 §4.6). "Has its own initial states, all set to
/// 128" (§4.6) — this crate resets that array fresh for every slice, since
/// slices are independently decodable/encodable by design (RFC 9043 §4.5:
/// "provides opportunities for... multithreaded encoding and decoding"), and
/// a state shared across slices would defeat that. The one thing that *does*
/// persist across frames is the single-bit `keyframe` context, which sits
/// outside the per-slice loop entirely — see [`crate::codec::StreamState`]'s
/// docs for the real-`ffmpeg` measurement that settled it.
#[derive(Debug, Clone)]
pub(crate) struct SliceHeader {
    pub slice_x: u32,
    pub slice_y: u32,
    pub slice_width: u32,
    pub slice_height: u32,
    pub quant_table_set_index: Vec<u32>,
    pub picture_structure: u32,
    pub sar_num: u32,
    pub sar_den: u32,
}

impl SliceHeader {
    /// A single slice covering the whole raster (this crate's own encoder:
    /// `num_h_slices = num_v_slices = 1`).
    #[must_use]
    pub(crate) fn whole_frame(quant_table_set_index_count: usize) -> Self {
        Self {
            slice_x: 0,
            slice_y: 0,
            slice_width: 1,
            slice_height: 1,
            quant_table_set_index: vec![0; quant_table_set_index_count],
            picture_structure: 3, // progressive
            sar_num: 0,
            sar_den: 0,
        }
    }

    /// Parse `SliceHeader()` (RFC 9043 §4.6).
    ///
    /// `states` is caller-owned, but every caller in this crate passes a
    /// freshly-reset array for each call — see the struct's own docs on why
    /// per-slice reset (not per-frame persistence) is the right model here.
    ///
    /// # Errors
    /// Never fails on its own; kept `Result` for symmetry with the rest of
    /// this crate's bitstream parsers.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "symmetry with the rest of this crate's fallible bitstream parsers"
    )]
    pub(crate) fn parse(
        dec: &mut RangeDecoder<'_>,
        table: &StateTransition,
        states: &mut SymbolStates,
        quant_table_set_index_count: usize,
    ) -> Result<Self> {
        let slice_x = dec.get_symbol(states, table, false).cast_unsigned();
        let slice_y = dec.get_symbol(states, table, false).cast_unsigned();
        let slice_width = dec.get_symbol(states, table, false).cast_unsigned() + 1;
        let slice_height = dec.get_symbol(states, table, false).cast_unsigned() + 1;
        // `quant_table_set_index_count` is at most 3 (RFC 9043 §4.6.5: `1 +
        // (chroma_planes||version<=3 ? 1 : 0) + (extra_plane ? 1 : 0)`), so a
        // plain `Vec::new()` growing in place is simpler than sizing an
        // allocation through `Budget` for what is always a handful of `u32`s.
        let mut quant_table_set_index = Vec::new();
        for _ in 0..quant_table_set_index_count {
            quant_table_set_index.push(dec.get_symbol(states, table, false).cast_unsigned());
        }
        let picture_structure = dec.get_symbol(states, table, false).cast_unsigned();
        let sar_num = dec.get_symbol(states, table, false).cast_unsigned();
        let sar_den = dec.get_symbol(states, table, false).cast_unsigned();
        Ok(Self {
            slice_x,
            slice_y,
            slice_width,
            slice_height,
            quant_table_set_index,
            picture_structure,
            sar_num,
            sar_den,
        })
    }

    /// Write `SliceHeader()`. See [`SliceHeader::parse`] on `states` being
    /// caller-owned but freshly reset per slice.
    ///
    /// # Errors
    /// Never fails; `Result` kept for symmetry with [`SliceHeader::parse`].
    #[allow(
        clippy::unnecessary_wraps,
        reason = "symmetry with SliceHeader::parse and the rest of this crate's fallible bitstream writers"
    )]
    pub(crate) fn write(
        &self,
        enc: &mut RangeEncoder,
        table: &StateTransition,
        states: &mut SymbolStates,
    ) -> Result<()> {
        enc.put_symbol(states, table, self.slice_x.cast_signed(), false);
        enc.put_symbol(states, table, self.slice_y.cast_signed(), false);
        enc.put_symbol(states, table, self.slice_width.cast_signed() - 1, false);
        enc.put_symbol(states, table, self.slice_height.cast_signed() - 1, false);
        for &idx in &self.quant_table_set_index {
            enc.put_symbol(states, table, idx.cast_signed(), false);
        }
        enc.put_symbol(states, table, self.picture_structure.cast_signed(), false);
        enc.put_symbol(states, table, self.sar_num.cast_signed(), false);
        enc.put_symbol(states, table, self.sar_den.cast_signed(), false);
        Ok(())
    }

    /// `(slice_pixel_x, slice_pixel_y, slice_pixel_width, slice_pixel_height)`
    /// (RFC 9043 §4.7.3-§4.7.4, §4.8.2-§4.8.3), this slice's placement within
    /// the frame.
    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "exact floor division is what RFC 9043's own floor()-based formulas specify; floating point would be the wrong tool here"
    )]
    pub(crate) fn geometry(
        &self,
        frame_w: u32,
        frame_h: u32,
        num_h_slices: u32,
        num_v_slices: u32,
    ) -> (u32, u32, u32, u32) {
        let px = u32::try_from(
            u64::from(self.slice_x) * u64::from(frame_w) / u64::from(num_h_slices.max(1)),
        )
        .unwrap_or(u32::MAX);
        let py = u32::try_from(
            u64::from(self.slice_y) * u64::from(frame_h) / u64::from(num_v_slices.max(1)),
        )
        .unwrap_or(u32::MAX);
        let px_end = u32::try_from(
            u64::from(self.slice_x + self.slice_width) * u64::from(frame_w)
                / u64::from(num_h_slices.max(1)),
        )
        .unwrap_or(u32::MAX);
        let py_end = u32::try_from(
            u64::from(self.slice_y + self.slice_height) * u64::from(frame_h)
                / u64::from(num_v_slices.max(1)),
        )
        .unwrap_or(u32::MAX);
        (px, py, px_end.saturating_sub(px), py_end.saturating_sub(py))
    }
}

/// `SliceFooter` (RFC 9043 §4.9): fixed-size, byte-aligned, read directly —
/// see the module docs on why this crate never needs range-coder-based
/// termination bookkeeping to find it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SliceFooter {
    pub slice_size: u32,
    #[allow(
        dead_code,
        reason = "parsed for RFC 9043 §4.9.1 completeness (correct byte_len/offset math needs the ec-gated shape); only meaningful when ec != 0, which no ffmpeg-produced fixture measured for this crate used, so per-slice error-status/CRC validation is out of scope for now"
    )]
    pub error_status: Option<u8>,
    #[allow(
        dead_code,
        reason = "parsed for RFC 9043 §4.9.1 completeness (correct byte_len/offset math needs the ec-gated shape); only meaningful when ec != 0, which no ffmpeg-produced fixture measured for this crate used, so per-slice error-status/CRC validation is out of scope for now"
    )]
    pub slice_crc: Option<u32>,
}

impl SliceFooter {
    /// Footer size in bytes for a given `ec`.
    #[must_use]
    pub(crate) const fn byte_len(ec: u32) -> usize {
        if ec != 0 { 3 + 1 + 4 } else { 3 }
    }

    /// Read a footer from its own fixed-size byte range (the caller has
    /// already located `footer_start = packet_len - byte_len(ec)`).
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if fewer than [`SliceFooter::byte_len`] bytes
    /// are available.
    pub(crate) fn read(bytes: &[u8], ec: u32) -> Result<Self> {
        let &[b0, b1, b2, ref rest @ ..] = bytes else {
            return Err(Error::UnexpectedEof);
        };
        let slice_size = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        if ec == 0 {
            return Ok(Self {
                slice_size,
                error_status: None,
                slice_crc: None,
            });
        }
        let &[status, c0, c1, c2, c3] = rest else {
            return Err(Error::UnexpectedEof);
        };
        let crc = u32::from_be_bytes([c0, c1, c2, c3]);
        Ok(Self {
            slice_size,
            error_status: Some(status),
            slice_crc: Some(crc),
        })
    }

    /// Serialize, `slice_size` from the caller (this crate never writes
    /// `error_status`/`slice_crc` — its own encoder always uses `ec = 0`).
    #[must_use]
    pub(crate) fn write(slice_size: u32) -> [u8; 3] {
        let b = slice_size.to_be_bytes();
        [b[1], b[2], b[3]]
    }
}

/// RFC 9043 Figure 10's `coder_input`: reduce a sample difference to the
/// `bits`-wide signed residual actually transmitted (`((d + 2^(bits-1)) &
/// (2^bits-1)) - 2^(bits-1)`), so it stays small regardless of how far apart
/// `sample` and its prediction are in *unwrapped* terms — the case that
/// matters most is a predictor landing just past a `0`/`max` pixel-value
/// wraparound.
#[inline]
#[must_use]
fn wrap_diff(d: i32, bits: u32) -> i32 {
    if bits == 0 || bits >= 32 {
        return d;
    }
    let half = 1i32 << (bits - 1);
    let mask = (1i32 << bits) - 1;
    ((d.wrapping_add(half)) & mask) - half
}

/// Reduce a reconstructed sample back into the valid unsigned `bits`-wide
/// pixel range. `decode`'s `pred + diff` is only correct up to this
/// reduction — see [`wrap_diff`] and the module's border/predictor docs.
#[inline]
#[must_use]
fn wrap_sample(v: i32, bits: u32) -> i32 {
    if bits == 0 || bits >= 32 {
        return v;
    }
    v.rem_euclid(1i32 << bits)
}

/// Decode one plane's `w x h` samples using the range coder (`coder_type`
/// 1 or 2), RFC 9043 §3.8.1.2/§4.7-§4.8.
#[allow(
    clippy::many_single_char_names,
    reason = "l/t name the RFC's own neighbour labels (Figure 3); w/h are plane dimensions"
)]
pub(crate) fn decode_plane_range(
    dec: &mut RangeDecoder<'_>,
    table: &StateTransition,
    qts: &QuantTableSet,
    states: &mut Vec<SymbolStates>,
    bits_per_raw_sample: u32,
    w: usize,
    h: usize,
    budget: &mut Budget,
) -> Result<SliceBuf> {
    let mut buf = SliceBuf::alloc(budget, w, h)?;
    if states.len() < qts.context_count {
        states.resize(qts.context_count, fresh_states());
    }
    for y in 0..h {
        for x in 0..w {
            let (l, t, tl, tr, ll, tt) = buf.neighbours(x, y);
            let (ctx, flip) = compute_context(qts, l, t, tl, tr, ll, tt);
            let st = states
                .get_mut(ctx)
                .ok_or(Error::InvalidData("ffv1: context out of range"))?;
            let mut diff = dec.get_symbol(st, table, true);
            if flip {
                diff = -diff;
            }
            let pred = median_predictor(l, t, tl);
            let sample = wrap_sample(pred + diff, bits_per_raw_sample);
            buf.set(x, y, sample);
        }
    }
    Ok(buf)
}

/// Encode one plane's `w x h` samples using the range coder.
#[allow(
    clippy::many_single_char_names,
    reason = "l/t name the RFC's own neighbour labels (Figure 3)"
)]
pub(crate) fn encode_plane_range(
    enc: &mut RangeEncoder,
    table: &StateTransition,
    qts: &QuantTableSet,
    states: &mut Vec<SymbolStates>,
    bits_per_raw_sample: u32,
    src: &SliceBuf,
) -> Result<()> {
    if states.len() < qts.context_count {
        states.resize(qts.context_count, fresh_states());
    }
    for y in 0..src.h {
        for x in 0..src.w {
            let (l, t, tl, tr, ll, tt) = src.neighbours(x, y);
            let (ctx, flip) = compute_context(qts, l, t, tl, tr, ll, tt);
            let pred = median_predictor(l, t, tl);
            let sample = src.get(x, y);
            let mut diff = wrap_diff(sample - pred, bits_per_raw_sample);
            if flip {
                diff = -diff;
            }
            let st = states
                .get_mut(ctx)
                .ok_or(Error::InvalidData("ffv1: context out of range"))?;
            enc.put_symbol(st, table, diff, true);
        }
    }
    Ok(())
}

/// Decode one plane's `w x h` samples using Golomb-Rice mode (`coder_type ==
/// 0`), RFC 9043 §3.8.2. Decode-only — see `rice.rs`'s module docs.
#[allow(
    clippy::many_single_char_names,
    reason = "l/t/tl/tr/ll/tt name the RFC's own neighbour labels (Figure 3); r/w/h are the bit reader and plane dimensions"
)]
pub(crate) fn decode_plane_golomb(
    r: &mut BitReader<'_>,
    qts: &QuantTableSet,
    rice_states: &mut Vec<RiceState>,
    bits_per_raw_sample: u32,
    w: usize,
    h: usize,
    budget: &mut Budget,
) -> Result<SliceBuf> {
    let mut buf = SliceBuf::alloc(budget, w, h)?;
    if rice_states.len() < qts.context_count {
        rice_states.resize(qts.context_count, RiceState::fresh());
    }
    // "run_index is reset to zero for each Plane and Slice" — one RunState
    // per plane decode, scoped to this call.
    let mut run = RunState::new();
    for y in 0..h {
        for x in 0..w {
            let (l, t, tl, tr, ll, tt) = buf.neighbours(x, y);
            let (ctx, flip) = compute_context(qts, l, t, tl, tr, ll, tt);
            let pred = median_predictor(l, t, tl);
            let rs = rice_states
                .get_mut(ctx)
                .ok_or(Error::InvalidData("ffv1: context out of range"))?;
            let mut diff = if ctx == 0 {
                run.next_zero_context_diff(r, rs, bits_per_raw_sample, x, w)
            } else {
                crate::rice::decode_level(rs, r, bits_per_raw_sample)
            };
            if flip {
                diff = -diff;
            }
            buf.set(x, y, wrap_sample(pred + diff, bits_per_raw_sample));
        }
    }
    Ok(buf)
}

/// Per-slice adaptive state, reset fresh on every keyframe (RFC 9043
/// §3.8.1.3/§3.8.2.5 — this crate treats every frame as a keyframe, matching
/// its intra-only scope).
///
/// Indexed by **Quantization Table Set index** (RFC 9043 §3.6), not by
/// plane: Cb and Cr share `quant_table_set_index[1]` and, measured against a
/// real `ffmpeg` range-coder encode, they also share the *same adapting
/// context state* — decoding Cb with its own array and Cr with a second,
/// independently-fresh one decoded Cb pixel-exact but corrupted every Cr
/// sample from the first one onward. Indexing by quant-table-set index
/// instead makes that sharing automatic, since the RFC already groups Cb/Cr
/// under one index for exactly this reason (one set of tables, hence one set
/// of contexts).
#[derive(Debug, Clone)]
pub(crate) struct PlaneStates {
    pub range: Vec<Vec<SymbolStates>>,
    pub rice: Vec<Vec<RiceState>>,
}

impl PlaneStates {
    /// Fresh state, one (empty, grown on first use) entry per Quantization
    /// Table Set index up to `quant_table_set_index_count`.
    #[must_use]
    pub(crate) fn fresh(quant_table_set_index_count: usize) -> Self {
        Self {
            range: vec![Vec::new(); quant_table_set_index_count],
            rice: vec![Vec::new(); quant_table_set_index_count],
        }
    }
}

/// The Quantization Table Set to use for plane `p` of `primary_color_count`
/// (RFC 9043 §3.6): index 0 for the first (luma/G) plane, index 1 for
/// chroma/B/R planes, and the version-dependent rule for an extra
/// (alpha/transparency) plane.
#[must_use]
pub(crate) fn quant_index_for_plane(p: usize, chroma_planes: bool, version: u32) -> usize {
    if p == 0 {
        0
    } else if chroma_planes && p <= 2 {
        1
    } else {
        // Extra plane: index (version <= 3 || chroma_planes) ? 2 : 1.
        usize::from(version <= 3 || chroma_planes) + 1
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code exercising the module, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;
    use crate::quant::QuantTableSet;
    use vaco_limits::{Budget, Limits};

    fn checker_buf(w: usize, h: usize) -> SliceBuf {
        let mut budget = Budget::new(Limits::permissive());
        let mut buf = SliceBuf::alloc(&mut budget, w, h).expect("alloc");
        for y in 0..h {
            for x in 0..w {
                let v = i32::try_from((x * 7 + y * 13) % 256).unwrap_or(0);
                buf.set(x, y, v);
            }
        }
        buf
    }

    #[test]
    fn plane_round_trips_through_range_coder() {
        let table = StateTransition::default_table();
        let qts = QuantTableSet::small_default();
        let src = checker_buf(9, 7);

        let mut enc = RangeEncoder::new();
        let mut enc_states = Vec::new();
        encode_plane_range(&mut enc, &table, &qts, &mut enc_states, 8, &src).expect("encode");
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let mut dec_states = Vec::new();
        let mut budget = Budget::new(Limits::permissive());
        let decoded = decode_plane_range(
            &mut dec,
            &table,
            &qts,
            &mut dec_states,
            8,
            src.w,
            src.h,
            &mut budget,
        )
        .expect("decode");

        for y in 0..src.h {
            for x in 0..src.w {
                assert_eq!(src.get(x, y), decoded.get(x, y), "({x},{y})");
            }
        }
    }

    #[test]
    fn slice_header_round_trips() {
        let table = StateTransition::default_table();
        let header = SliceHeader::whole_frame(2);
        let mut enc = RangeEncoder::new();
        let mut enc_states = fresh_states();
        header
            .write(&mut enc, &table, &mut enc_states)
            .expect("write");
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes);
        let mut dec_states = fresh_states();
        let parsed = SliceHeader::parse(&mut dec, &table, &mut dec_states, 2).expect("parse");
        assert_eq!(parsed.slice_x, header.slice_x);
        assert_eq!(parsed.slice_width, header.slice_width);
        assert_eq!(parsed.slice_height, header.slice_height);
        assert_eq!(parsed.quant_table_set_index, header.quant_table_set_index);
    }

    #[test]
    fn slice_header_encode_decode_agree_on_shared_states() {
        // get_symbol/put_symbol's contract is symmetric regardless of
        // whether the caller resets `states` between calls or not — this
        // crate always resets per slice (see the struct's docs), but the
        // primitive itself must decode correctly either way, which this
        // checks directly rather than only through the reset path.
        let table = StateTransition::default_table();
        let header = SliceHeader::whole_frame(2);
        let mut enc = RangeEncoder::new();
        let mut enc_states = fresh_states();
        header
            .write(&mut enc, &table, &mut enc_states)
            .expect("write 1");
        header
            .write(&mut enc, &table, &mut enc_states)
            .expect("write 2");
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let mut dec_states = fresh_states();
        let first = SliceHeader::parse(&mut dec, &table, &mut dec_states, 2).expect("parse 1");
        let second = SliceHeader::parse(&mut dec, &table, &mut dec_states, 2).expect("parse 2");
        assert_eq!(first.slice_width, header.slice_width);
        assert_eq!(second.slice_width, header.slice_width);
    }

    #[test]
    fn slice_footer_round_trips_without_ec() {
        let bytes = SliceFooter::write(1234);
        let mut full = bytes.to_vec();
        full.extend_from_slice(b"trailing garbage should not be read");
        let footer = SliceFooter::read(&full, 0).expect("read");
        assert_eq!(footer.slice_size, 1234);
        assert!(footer.error_status.is_none());
    }

    #[test]
    fn border_matches_rfc_figure_2_example() {
        // Figure 2's 3x3 example: a,b,c / d,e,f / g,h,i.
        let mut budget = Budget::new(Limits::permissive());
        let mut buf = SliceBuf::alloc(&mut budget, 3, 3).expect("alloc");
        let vals = [[1, 2, 3], [4, 5, 6], [7, 8, 9]]; // a..i
        for (y, row) in vals.iter().enumerate() {
            for (x, &v) in row.iter().enumerate() {
                buf.set(x, y, v);
            }
        }
        // Row 0 (a,b,c): left border both 0, right border repeats c(=3).
        assert_eq!(buf.border(-1, 0), 0);
        assert_eq!(buf.border(-2, 0), 0);
        assert_eq!(buf.border(3, 0), 3);
        // Row 1 (d,e,f): l-border = a (leftmost of row0, shifted down), L-border = 0.
        assert_eq!(buf.border(-1, 1), 1);
        assert_eq!(buf.border(-2, 1), 0);
        // Row 2 (g,h,i): l-border = d (leftmost of row1).
        assert_eq!(buf.border(-1, 2), 4);
        // Two rows above are always 0.
        assert_eq!(buf.border(0, -1), 0);
        assert_eq!(buf.border(0, -2), 0);
        assert_eq!(buf.border(2, -1), 0);
    }
}
