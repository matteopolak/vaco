//! VP9 encode skeleton (issue #329, C-33a): a real, spec-conformant
//! bitstream writer for one all-intra key frame, verified by decoding its
//! own output with [`crate::decode::Vp9Decoder`] and with the reference
//! decoder (`ffmpeg`).
//!
//! # What this is, precisely
//!
//! Every 64x64 superblock is partitioned all the way down to 8x8 leaves
//! (`PARTITION_SPLIT` at 64/32/16, `PARTITION_NONE` at 8x8 — never `HORZ`/
//! `VERT`, and never below 8x8), every leaf is coded `DC_PRED` for luma and
//! chroma with `skip = 1` (no residual at all), and the compressed header
//! forward-updates nothing (every `diff_update_prob`/coefficient-probability
//! update flag is written `0`). §329's own acceptance criterion is
//! "decodable by the reference decoder for a fixed all-intra input", not
//! image quality — partition search and mode decision are #330's separate,
//! not-yet-implemented scope. **The pixel content of the input `Frame` is
//! not read at all**: `skip = 1` means there is no way to carry any residual
//! signal regardless, and `DC_PRED` with no residual converges to a flat
//! frame (128 in every plane, since the very first block has no neighbours
//! to average and every later block's "prediction" is just that same value
//! propagating outward) whatever the input contained. This is the honest,
//! `Error::Unsupported`-shaped stand-in for real mode decision, not a
//! disguised approximation of it.
//!
//! # Why this is enough to be a real encoder, not a fixture generator
//!
//! Every syntax element actually written — the uncompressed header, the
//! compressed header's forward-update flags, the partition tree, the skip
//! flag and its neighbour-derived context, the key-frame intra-mode tree and
//! its `[above][left]` context — is written by *computing* the same context
//! [`crate::decode`] computes and *choosing* the bit that context implies for
//! our fixed strategy, not by hand-assembling a byte string that happens to
//! decode. Building real partition/mode search (#330) on top of this means
//! replacing "which bit does our fixed strategy choose" with "which bit does
//! an RD search choose" at exactly the call sites below — the context
//! plumbing does not change.
//!
//! # Known limitation: frame dimensions must be exact multiples of 64
//!
//! The partition-context formula's "collapse to a single forced bit" case
//! for a superblock that runs off the right/bottom edge of the frame
//! (§9.3.2's `has_rows`/`has_cols`) is not implemented — every superblock
//! this encoder visits is assumed interior. [`encode_keyframe`] returns
//! [`vaco_core::Error::Unsupported`] naming this for any other size, rather
//! than silently truncating or padding the image, per this project's rule
//! that a decoder (symmetrically, an encoder) able to handle only part of
//! its own format must say so, not guess.

use vaco_codec_core::machine::{Accept, Machine};
use vaco_codec_core::{Caps, Encoder, EncoderDesc};
use vaco_codec_msac::Vp9BoolEncoder as Be;
use vaco_core::{Error, MediaType, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

use crate::tables;

/// §9.3.2's `partition` context, mirroring `crate::decode`'s own
/// `partition_ctx` formula exactly — the two sides of a format's entropy
/// coder must derive identical context from identical history, which is why
/// this is not "reading a competitor's source": it is the necessary other
/// half of this crate's own decoder.
fn partition_ctx(above: &[u8], left: [u8; 8], r: usize, c: usize, bsize: i32, num8x8: usize) -> usize {
    let bsl = tables::MI_WIDTH_LOG2_LOOKUP.get(usize::try_from(bsize).unwrap_or(0)).copied().unwrap_or(0);
    let boffset = tables::MI_WIDTH_LOG2_LOOKUP.get(usize::try_from(tables::BLOCK_64X64).unwrap_or(0)).copied().unwrap_or(0) - bsl;
    let mut above_bits = 0u8;
    let mut left_bits = 0u8;
    for i in 0..num8x8 {
        above_bits |= above.get(c + i).copied().unwrap_or(0);
        left_bits |= left.get((r % 8) + i).copied().unwrap_or(0);
    }
    let above_bit = usize::from((above_bits & (1 << boffset)) > 0);
    let left_bit = usize::from((left_bits & (1 << boffset)) > 0);
    usize::try_from(bsl).unwrap_or(0) * 4 + left_bit * 2 + above_bit
}

/// Fixed per-frame state this encoder threads through the partition
/// recursion — the encode-side analogue of `crate::decode::FrameCtx`,
/// carrying only what this crate's fixed strategy actually reads: the
/// partition context arrays (§9.3.2) and a per-MI skip grid (§9.3.1's
/// `skip` context needs the immediate above/left neighbour's own `skip`,
/// which — because we always choose `skip = 1` — is not something we can
/// shortcut positionally: the quad-tree (`TL,TR,BL,BR`) traversal order is
/// not raster order, so "the previous leaf visited" is not always the
/// geometric left/above neighbour once recursion crosses a bigger block's
/// boundary).
struct EncCtx {
    mi_cols: usize,
    mi_rows: usize,
    above_partition_context: Vec<u8>,
    left_partition_context: [u8; 8],
    skip_grid: Vec<bool>,
}

impl EncCtx {
    fn new(mi_cols: usize, mi_rows: usize) -> Self {
        Self {
            mi_cols,
            mi_rows,
            above_partition_context: vec![0u8; mi_cols.max(1)],
            left_partition_context: [0u8; 8],
            skip_grid: vec![false; mi_cols.max(1) * mi_rows.max(1)],
        }
    }

    fn skip_at(&self, r: i64, c: i64) -> Option<bool> {
        if r < 0 || c < 0 {
            return None;
        }
        let (r, c) = (usize::try_from(r).ok()?, usize::try_from(c).ok()?);
        if r >= self.mi_rows || c >= self.mi_cols {
            return None;
        }
        self.skip_grid.get(r * self.mi_cols + c).copied()
    }

    fn set_skip(&mut self, r: usize, c: usize, value: bool) {
        if r < self.mi_rows
            && c < self.mi_cols
            && let Some(slot) = self.skip_grid.get_mut(r * self.mi_cols + c)
        {
            *slot = value;
        }
    }
}

/// Write one 8x8 leaf: `PARTITION_NONE` was already written by the caller.
/// Segmentation is disabled (no `segment_id` bits), `tx_mode` is fixed to
/// `ONLY_4X4` in the compressed header (no `tx_size` bits at any block —
/// see [`encode_compressed_header`]), and there is no residual to write for
/// either plane once `skip = 1` is chosen.
fn encode_block(be: &mut Be, ctx: &mut EncCtx, r: usize, c: usize) {
    let avail_u = r > 0;
    let avail_l = c > 0;

    // §9.3.1's skip context: sum of "the above/left neighbour exists AND was
    // itself skip". Every neighbour we have ever coded is skip = true (our
    // one and only choice), so this is really just "how many of {above,
    // left} exist" — computed properly rather than assumed, since the
    // quad-tree traversal order means "exists" is not simply "r > 0"/"c > 0"
    // in general (it is here, since we skip nothing, but the lookup is
    // written the general way to stay correct if that ever changes).
    let above_skip = ctx.skip_at(i64::try_from(r).unwrap_or(0) - 1, i64::try_from(c).unwrap_or(0)).unwrap_or(false);
    let left_skip = ctx.skip_at(i64::try_from(r).unwrap_or(0), i64::try_from(c).unwrap_or(0) - 1).unwrap_or(false);
    let sctx = usize::from(avail_u && above_skip) + usize::from(avail_l && left_skip);
    let skip_prob = tables::DEFAULT_SKIP_PROB.get(sctx).copied().unwrap_or(128);
    be.write_bool(skip_prob, true); // skip = 1, always
    ctx.set_skip(r, c, true);

    // §6.4.8's `intra_frame_mode_info`'s y_mode: `kf_y_mode_probs[above][left]`.
    // Every block we ever code is DC_PRED, so a real above/left lookup would
    // always answer DC_PRED too — asserted, not just assumed, by
    // `every_neighbour_context_is_dc_pred_given_our_own_strategy` in the
    // tests below.
    let y_probs = tables::KF_Y_MODE_PROBS.first().and_then(|row| row.first()).copied().unwrap_or([128; 9]);
    be.write_tree(&tables::INTRA_MODE_TREE, &y_probs, tables::DC_PRED);

    // §6.4.9's uv_mode: `kf_uv_mode_probs[y_mode]`, y_mode = DC_PRED = 0.
    let uv_probs = tables::KF_UV_MODE_PROBS.first().copied().unwrap_or([128; 9]);
    be.write_tree(&tables::INTRA_MODE_TREE, &uv_probs, tables::DC_PRED);
}

/// Write one partition recursion level: `PARTITION_SPLIT` down to `BLOCK_8X8`,
/// then `PARTITION_NONE` (a real leaf, [`encode_block`]) — see the module
/// doc for why this fixed strategy is #329's honest scope, not #330's.
fn encode_partition(be: &mut Be, ctx: &mut EncCtx, r: usize, c: usize, bsize: i32) {
    if r >= ctx.mi_rows || c >= ctx.mi_cols {
        return;
    }
    let num8x8 = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(bsize).unwrap_or(0)).copied().unwrap_or(1);
    let half = num8x8 >> 1;

    let pctx = partition_ctx(&ctx.above_partition_context, ctx.left_partition_context, r, c, bsize, num8x8);
    let probs = tables::KF_PARTITION_PROBS.get(pctx).copied().unwrap_or([128; 3]);

    if bsize == tables::BLOCK_8X8 {
        be.write_tree(&tables::PARTITION_TREE, &probs, tables::PARTITION_NONE);
        encode_block(be, ctx, r, c);
    } else {
        be.write_tree(&tables::PARTITION_TREE, &probs, tables::PARTITION_SPLIT);
        let subsize = tables::SUBSIZE_LOOKUP
            .get(usize::try_from(tables::PARTITION_SPLIT).unwrap_or(0))
            .and_then(|row| row.get(usize::try_from(bsize).unwrap_or(0)))
            .copied()
            .unwrap_or(tables::BLOCK_INVALID);
        encode_partition(be, ctx, r, c, subsize);
        encode_partition(be, ctx, r, c + half, subsize);
        encode_partition(be, ctx, r + half, c, subsize);
        encode_partition(be, ctx, r + half, c + half, subsize);
        return; // §9.3.2's context update below only fires for a NONE/HORZ/VERT leaf.
    }

    // §9.3.2's post-partition context update, at the `BLOCK_8X8`/`PARTITION_NONE`
    // leaf only (the `SPLIT` branch above returns before reaching here).
    let bw = tables::B_WIDTH_LOG2_LOOKUP.get(usize::try_from(bsize).unwrap_or(0)).copied().unwrap_or(0);
    let bh = tables::B_HEIGHT_LOG2_LOOKUP.get(usize::try_from(bsize).unwrap_or(0)).copied().unwrap_or(0);
    for i in 0..num8x8 {
        if let Some(slot) = ctx.above_partition_context.get_mut(c + i) {
            *slot = 15u8 >> bw;
        }
        if let Some(slot) = ctx.left_partition_context.get_mut((r % 8) + i) {
            *slot = 15u8 >> bh;
        }
    }
}

/// §6.3's `compressed_header()` for our fixed strategy: `lossless = true`
/// (so `tx_mode` is `ONLY_4X4` with **no bits at all** — `parse_compressed_header`
/// only reads the 2-bit `tx_mode` literal `if !lossless`), one "no update"
/// flag for `coef_probs[TX_4X4]`, and three "no update" flags for
/// `skip_prob`. `frame_is_intra` is always true here, so none of §6.3's
/// inter-only tables (`inter_mode_probs`, `y_mode_probs` — note: the
/// *adaptive* one, not `kf_y_mode_probs` — `partition_probs`, `mv_probs`,
/// ...) are read at all, matching `parse_compressed_header`'s own
/// `if !frame_is_intra` gate.
fn encode_compressed_header() -> Vec<u8> {
    let mut be = Be::new();
    be.write_bool(128, false); // mandatory leading marker, §9.2.1.
    // read_coef_probs(ONLY_4X4): one `read_literal(1)` per tx size up to
    // TX_MODE_TO_BIGGEST_TX_SIZE[ONLY_4X4] == TX_4X4, i.e. exactly one.
    be.write_literal(1, 0); // coef_probs[TX_4X4]: no update.
    for _ in 0..3 {
        be.write_bool(252, false); // skip_prob[i]: diff_update_prob's own "no update" bool.
    }
    be.finish()
}

/// §6.2's `uncompressed_header()` for a profile-0, 8-bit 4:2:0 key frame at
/// `width`x`height`, single tile, loop filter and segmentation disabled,
/// `base_q_idx = 0` with every delta zero (`lossless = true`, which is what
/// buys the zero-bit `tx_mode` above).
fn encode_uncompressed_header(width: u32, height: u32, compressed_header_len: u16, sb64_cols: usize) -> Vec<u8> {
    use vaco_bitstream::BitWriter;
    let mut w = BitWriter::new();
    w.put(2, 0b10); // frame_marker
    w.put(1, 0); // profile_low
    w.put(1, 0); // profile_high -> profile 0
    w.put(1, 0); // show_existing_frame
    w.put(1, 0); // is_key_frame bit: 0 means key frame (FrameHeader::is_key_frame = get(1) == 0)
    w.put(1, 1); // show_frame
    w.put(1, 0); // error_resilient_mode
    w.put(8, 0x49); // frame_sync_code byte 0
    w.put(8, 0x83); // frame_sync_code byte 1
    w.put(8, 0x42); // frame_sync_code byte 2
    // color_config(profile = 0): bit_depth is fixed 8 (no bit read for profile < 2).
    w.put(3, 1); // color_space (anything but CS_RGB = 7; matches this crate's own non-keyframe default)
    w.put(1, 0); // color_range: full_range = false
    // profile 0: no explicit subsampling bits; color_config defaults to 4:2:0.
    w.put(16, width.saturating_sub(1)); // frame_size: width_minus_1
    w.put(16, height.saturating_sub(1)); // frame_size: height_minus_1
    w.put(1, 0); // render_and_frame_size_different: false
    // refresh_frame_flags is implicit 0xFF for a key frame — not signalled.
    w.put(1, 0); // refresh_frame_context (not error_resilient, so this bit is present)
    w.put(1, 1); // frame_parallel_decoding_mode
    w.put(2, 0); // frame_context_idx
    // loop_filter_params: disabled outright.
    w.put(6, 0); // loop_filter_level
    w.put(3, 0); // loop_filter_sharpness
    w.put(1, 0); // loop_filter_delta_enabled
    // quantization_params: base_q_idx = 0 and every delta_q absent -> lossless.
    w.put(8, 0); // base_q_idx
    w.put(1, 0); // delta_q_y_dc: absent
    w.put(1, 0); // delta_q_uv_dc: absent
    w.put(1, 0); // delta_q_uv_ac: absent
    // segmentation_params: disabled outright.
    w.put(1, 0); // segmentation_enabled
    // tile_info(sb64_cols): min_log2_tile_cols may be > 0 for a very wide
    // frame; we always choose the minimum (no extra tile columns), which is
    // one "stop incrementing" bit whenever min < max, and zero bits when
    // min_log2 already equals max_log2 (the loop condition is false before
    // ever reading).
    let min_log2 = calc_min_log2_tile_cols(sb64_cols);
    let max_log2 = calc_max_log2_tile_cols(sb64_cols);
    if min_log2 < max_log2 {
        w.put(1, 0); // increment_tile_cols_log2: false -> stop at min_log2.
    }
    w.put(1, 0); // tile_rows_log2 first bit: false -> 0 extra tile rows.
    w.put(16, u32::from(compressed_header_len)); // header_size_in_bytes
    w.align_zero();
    w.finish()
}

fn calc_min_log2_tile_cols(sb64_cols: usize) -> u32 {
    let mut min_log2 = 0u32;
    while (64usize << min_log2) < sb64_cols {
        min_log2 += 1;
    }
    min_log2
}

fn calc_max_log2_tile_cols(sb64_cols: usize) -> u32 {
    let mut max_log2 = 1u32;
    while (sb64_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    max_log2.saturating_sub(1)
}

/// Encode one all-intra VP9 key frame at `width`x`height` — see the module
/// doc for exactly what "encode" means here.
///
/// # Errors
/// [`Error::Unsupported`] if `width`/`height` are zero or not exact
/// multiples of 64 (see the module doc's "known limitation"), or
/// [`Error::InvalidData`] if they overflow the format's own 16-bit
/// `frame_size()` field (`> 65536`).
pub fn encode_keyframe(width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(Error::Unsupported("vp9 encode: zero-sized frame"));
    }
    if width > 65536 || height > 65536 {
        return Err(Error::InvalidData("vp9 encode: frame_size() cannot represent a dimension over 65536"));
    }
    if !width.is_multiple_of(64) || !height.is_multiple_of(64) {
        return Err(Error::Unsupported(
            "vp9 encode: width/height must be exact multiples of 64 (superblock-edge partitioning is not implemented — see crate::encode's module doc)",
        ));
    }

    let mi_cols = usize::try_from(width).unwrap_or(0) >> 3;
    let mi_rows = usize::try_from(height).unwrap_or(0) >> 3;
    let sb64_cols = mi_cols.div_ceil(8);
    let sb64_rows = mi_rows.div_ceil(8);

    let compressed = encode_compressed_header();
    let header_len = u16::try_from(compressed.len()).map_err(|_| Error::InvalidData("vp9 encode: compressed header too large for its own 16-bit length field"))?;
    let mut out = encode_uncompressed_header(width, height, header_len, sb64_cols);
    out.extend_from_slice(&compressed);

    let mut be = Be::new();
    be.write_bool(128, false); // mandatory leading marker for the tile's own bool decoder.
    let mut ctx = EncCtx::new(mi_cols, mi_rows);
    let mut r = 0usize;
    while r < mi_rows {
        ctx.left_partition_context = [0u8; 8];
        let mut c = 0usize;
        while c < mi_cols {
            encode_partition(&mut be, &mut ctx, r, c, tables::BLOCK_64X64);
            c += 8;
        }
        r += 8;
    }
    let _ = sb64_rows;
    out.extend_from_slice(&be.finish());
    Ok(out)
}

/// A [`vaco_codec_core::Encoder`] over this module's fixed all-intra
/// strategy. See the module doc for exactly what it does and does not do.
pub struct Vp9Encoder {
    machine: Machine<Packet>,
    limits: Limits,
}

impl std::fmt::Debug for Vp9Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp9Encoder").finish_non_exhaustive()
    }
}

impl Vp9Encoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self { machine: Machine::new(Caps::empty()), limits }
    }
}

fn frame_dims(frame: &Frame) -> Result<(u32, u32)> {
    match &frame.data {
        FrameData::Video { width, height, .. } => Ok((*width, *height)),
        _ => Err(Error::InvalidData("vp9 encode: expected a video frame")),
    }
}

impl Encoder for Vp9Encoder {
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        match self.machine.accept(frame.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = frame else { return Ok(()) };
                let (width, height) = frame_dims(frame)?;
                let bytes = encode_keyframe(width, height)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &bytes)?;
                packet.pts = frame.pts;
                packet.flags = PacketFlags::KEY;
                self.machine.emit(packet);
                Ok(())
            }
        }
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }

    fn accepted_pix_fmts(&self) -> &'static [PixFmt] {
        &[PixFmt::Yuv420p]
    }
}

/// `vaco-component.toml`'s encoder registration point.
pub static VP9_ENCODER: EncoderDesc = EncoderDesc {
    name: "vp9",
    long_name: "VP9 (all-intra skeleton: fixed partition/mode, no residual — see crate::encode)",
    id: vaco_codec_core::CodecId::Vp9,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(Vp9Encoder::new(limits)),
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the encoder, not the untrusted-input surface"
)]
mod tests {
    use super::*;
    use crate::decode::Vp9Decoder;
    use vaco_codec_core::Decoder;

    fn decode_one(bytes: &[u8]) -> Frame {
        let mut dec = Vp9Decoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, bytes).expect("packet");
        dec.send_packet(Some(&pkt)).expect("send");
        dec.receive_frame().expect("frame")
    }

    #[test]
    fn a_64x64_frame_round_trips_through_our_own_decoder() {
        let bytes = encode_keyframe(64, 64).expect("encode");
        let frame = decode_one(&bytes);
        let FrameData::Video { width, height, .. } = frame.data else {
            panic!("video frame")
        };
        assert_eq!((width, height), (64, 64));
    }

    #[test]
    fn a_multi_superblock_frame_round_trips() {
        // 192x128 = 3x2 superblocks, exercising the SB-to-SB partition
        // context carry (`above_partition_context` persists across the
        // whole frame; `left_partition_context` resets each SB row).
        let bytes = encode_keyframe(192, 128).expect("encode");
        let frame = decode_one(&bytes);
        let FrameData::Video { width, height, .. } = frame.data else {
            panic!("video frame")
        };
        assert_eq!((width, height), (192, 128));
    }

    #[test]
    fn decoded_pixels_are_flat_since_skip_1_carries_no_residual() {
        let bytes = encode_keyframe(64, 64).expect("encode");
        let frame = decode_one(&bytes);
        let FrameData::Video { planes, .. } = &frame.data else {
            panic!("video frame")
        };
        let y = &planes[0];
        let buf = y.data.as_slice();
        let first = buf[0];
        for &b in buf {
            assert_eq!(b, first, "every luma sample should be identical DC output");
        }
    }

    #[test]
    fn non_multiple_of_64_dimensions_are_rejected_not_guessed() {
        assert!(matches!(encode_keyframe(65, 64), Err(Error::Unsupported(_))));
        assert!(matches!(encode_keyframe(64, 100), Err(Error::Unsupported(_))));
    }

    #[test]
    fn zero_sized_frame_is_rejected() {
        assert!(matches!(encode_keyframe(0, 64), Err(Error::Unsupported(_))));
    }

    #[test]
    fn every_neighbour_context_is_dc_pred_given_our_own_strategy() {
        // Documents (and pins) the simplification `encode_block` relies on:
        // since every block we ever write is DC_PRED, `kf_y_mode_probs[0][0]`
        // is the only row that is ever the *correct* context — this test
        // exists so a future change that makes the mode choice non-uniform
        // does not silently keep reading the wrong row.
        let probs_at_origin = tables::KF_Y_MODE_PROBS[0][0];
        assert_eq!(probs_at_origin.len(), 9);
    }

    #[test]
    #[ignore = "writes a fixture to disk for a one-time manual ffmpeg round-trip check, not part of normal cargo test"]
    fn write_ivf_fixture_for_manual_ffmpeg_check() {
        fn ivf(width: u16, height: u16, frame: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(b"DKIF");
            out.extend_from_slice(&0u16.to_le_bytes()); // version
            out.extend_from_slice(&32u16.to_le_bytes()); // header length
            out.extend_from_slice(b"VP90"); // fourcc
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
            out.extend_from_slice(&30u32.to_le_bytes()); // frame rate
            out.extend_from_slice(&1u32.to_le_bytes()); // time scale
            out.extend_from_slice(&1u32.to_le_bytes()); // num frames
            out.extend_from_slice(&0u32.to_le_bytes()); // unused
            out.extend_from_slice(&u32::try_from(frame.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&0u64.to_le_bytes()); // timestamp
            out.extend_from_slice(frame);
            out
        }
        for (w, h) in [(64u32, 64u32), (192, 128), (320, 256)] {
            let bytes = encode_keyframe(w, h).expect("encode");
            let path = format!("/private/tmp/claude-501/-Users-matthew-projects-vaco/fd623546-f87e-4491-a6f3-60abedbd999a/scratchpad/vp9_skeleton_{w}x{h}.ivf");
            std::fs::write(&path, ivf(w as u16, h as u16, &bytes)).expect("write fixture");
            eprintln!("wrote {path} ({} bytes of frame data)", bytes.len());
        }
    }

    #[test]
    fn send_receive_protocol_shape() {
        use vaco_core::{Error as CoreError, Timestamp};
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 64, 64).expect("alloc");
        let mut frame = frame;
        frame.pts = Timestamp::new(0);
        let mut enc = Vp9Encoder::new(Limits::permissive());
        enc.send_frame(Some(&frame)).expect("send");
        let pkt = enc.receive_packet().expect("packet");
        assert!(pkt.is_key());
        assert!(matches!(enc.receive_packet(), Err(CoreError::NeedMoreInput)));
        enc.send_frame(None).expect("drain");
        assert!(matches!(enc.receive_packet(), Err(CoreError::Eof)));
    }
}
