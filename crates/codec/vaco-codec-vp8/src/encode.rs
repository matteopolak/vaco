//! VP8 encode skeleton (issue #302, C-17a): a real, spec-conformant
//! bitstream writer for one all-intra key frame, verified by decoding its
//! own output with [`crate::decode::Vp8Decoder`] and with the reference
//! decoder (`ffmpeg`).
//!
//! # What this is, precisely
//!
//! Every macroblock is coded `DC_PRED` (mode 0) for luma and chroma with
//! `skip = 1` (`mb_no_skip_coeff = 1`, `prob_skip_false` chosen so the bit
//! is always written `1`), so no residual is ever coded — RFC 6386 §9.2's
//! `update_coeff_probs()` accordingly writes "no update" for all 1056
//! coefficient-probability entries, since nothing downstream ever reads a
//! coefficient token. The compressed header disables segmentation and the
//! loop filter, uses a single token partition (`log2_nbr_of_dct_partitions
//! = 0`), and sets `refresh_entropy_probs = 0`. Issue #302's own acceptance
//! criterion is "decodable by the reference decoder for a fixed all-intra
//! input", not image quality — mode decision, RDO and quantisation are
//! #303's separate, not-yet-implemented scope. **The pixel content of the
//! input [`Frame`] is not read at all**: `skip = 1` forecloses any way to
//! carry a residual signal regardless, and `DC_PRED` with no residual and no
//! loop filter converges to a flat frame (128 in every plane — every block's
//! prediction average falls back to 128 when neighbours are themselves 128,
//! starting from the top-left corner where RFC 6386 §12.2 defines no
//! neighbours to average at all). This is the honest,
//! `Error::Unsupported`-shaped stand-in for real mode decision, not a
//! disguised approximation of it — see `vaco-codec-vp9`'s `encode` module,
//! built the same day against the same brief shape, for the sibling
//! decision.
//!
//! # Two independent bitstreams, not one
//!
//! RFC 6386 §9.5 splits every frame into the first partition (the
//! compressed header plus every macroblock's mode/skip record, all read
//! through one `bd`) and one or more *separate* token partitions carrying
//! DCT coefficients, each its own independently-initialised boolean-coded
//! stream. This skeleton never codes a coefficient (`skip = 1` everywhere),
//! so its single token partition (`log2_nbr_of_dct_partitions = 0`) carries
//! no data bits — but it still has to exist as a well-formed stream: an
//! empty [`BoolWriter`]'s `finish()` output, four zero bytes a decoder can
//! safely prime its own bool-decoder state from. An earlier version of this
//! function omitted that second partition outright, reasoning that "no
//! tokens are coded" meant "no bytes to emit" — the first partition still
//! parsed correctly under this crate's own [`crate::decode::Vp8Decoder`]
//! (which never needed to *read* the missing partition to reach a valid
//! `skip = 1` leaf), so the crate's own round trip passed while `ffmpeg`
//! and a from-source-built `vpxdec` both rejected the output as a corrupt
//! key frame. Found by decoding this crate's own output with the reference
//! decoder rather than trusting the self-consistent round trip alone.
//!
//! # [`BoolWriter`]: the arithmetic inverse of a decoder RFC 6386 never describes
//!
//! RFC 6386 is titled "VP8 Data Format and **Decoding** Guide" and gives no
//! encoder pseudocode at all — §7 covers only the boolean *decoder*. This
//! module's [`BoolWriter`] is derived from that decoder's own arithmetic
//! (`crate`'s dependency `vaco_codec_msac::vp8::BoolDecoder`: `split = 1 +
//! ((range-1)*prob)>>8`, `range` doubling while `<128`, one input byte
//! folded in per 8 renormalisation shifts): a decoder specified this
//! precisely has a *unique* mathematical inverse, and constructing it is
//! what D7 calls deriving from the specification, not transcribing an
//! implementation. Correctness is checked the way this project checks any
//! arithmetic-coder inverse with no published encoder pseudocode to check
//! against: encode a real sequence, decode it back with the crate's own
//! verified-against-`ffmpeg` [`crate::decode::Vp8Decoder`], and — separately
//! — decode it with `ffmpeg` itself.
//!
//! # Known limitation: frame dimensions are otherwise unrestricted
//!
//! Unlike `vaco-codec-vp9`'s sibling skeleton, VP8 macroblocks tile the
//! frame with ordinary `div_ceil`-by-16 sizing the same way
//! [`crate::decode`] already does for non-16-multiple frames (its own
//! bit-exact `crop.ivf`/`fullpel.ivf` fixtures are 100x70 and 96x64), so no
//! multiple-of-N restriction is needed here.

use vaco_codec_core::machine::{Accept, Machine};
use vaco_codec_core::{Caps, Encoder, EncoderDesc};
use vaco_codec_msac::{Tree, write_tree};
use vaco_core::{Error, MediaType, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

use crate::tables;

/// RFC 6386 §7.3's boolean entropy encoder — the arithmetic inverse of
/// [`vaco_codec_msac::vp8::BoolDecoder`]'s `read_bool`. See the module doc
/// for why RFC 6386 gives no encoder pseudocode to transcribe and what
/// "derived" means here instead.
///
/// `bottom` accumulates the interval's lower bound at a 32-bit scale
/// (matching the decoder's 16-bit `value` window plus 16 bits of headroom
/// for carry detection: `bottom`'s top bit going high on a shift means a
/// carry must ripple into already-buffered output bytes before it is lost).
/// `bit_count` counts down from 24 to 0 across three bytes' worth of
/// shifts before the next output byte is extracted, mirroring the
/// decoder's own `bit_count` counting *up* to 8 before it pulls a new
/// input byte in — this is that refill cadence run in reverse.
#[derive(Debug)]
pub struct BoolWriter {
    out: Vec<u8>,
    range: u32,
    bottom: u32,
    bit_count: i32,
}

impl Default for BoolWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BoolWriter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            range: 255,
            bottom: 0,
            bit_count: 24,
        }
    }

    /// Ripple a carry out of `bottom` into the most recently buffered
    /// output bytes: a run of `0xFF` bytes all become `0x00` and the byte
    /// before that run absorbs the `+1`. Bytes before that point are
    /// already final and cannot be reached by any later carry, by
    /// construction of the range-coder invariant this mirrors.
    fn carry(&mut self) {
        for b in self.out.iter_mut().rev() {
            if *b == 255 {
                *b = 0;
            } else {
                *b += 1;
                return;
            }
        }
    }

    /// The inverse of [`vaco_codec_msac::vp8::BoolDecoder::read_bool`] at
    /// the same `prob`: writing `bit` here and reading it back there at an
    /// identical `prob` reproduces `bit`.
    pub fn write_bool(&mut self, prob: u8, bit: bool) {
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        if bit {
            self.bottom += split;
            self.range -= split;
        } else {
            self.range = split;
        }
        while self.range < 128 {
            self.range <<= 1;
            if self.bottom & (1 << 31) != 0 {
                self.carry();
            }
            self.bottom <<= 1;
            self.bit_count -= 1;
            if self.bit_count == 0 {
                self.out.push((self.bottom >> 24) as u8);
                self.bottom &= (1 << 24) - 1;
                self.bit_count = 8;
            }
        }
    }

    /// `Flag`/`F`: a bool at probability 128/256.
    pub fn write_flag(&mut self, bit: bool) {
        self.write_bool(128, bit);
    }

    /// `L(n)`/`Lit(n)`: an unsigned `n`-bit literal, high bit first, each
    /// bit at probability 128/256 — the inverse of
    /// [`vaco_codec_msac::vp8::BoolDecoder::read_literal`].
    pub fn write_literal(&mut self, num_bits: u32, value: u32) {
        for i in (0..num_bits).rev() {
            self.write_flag((value >> i) & 1 != 0);
        }
    }

    /// The inverse of `read_tree`: walk `tree` writing the branch bit at
    /// `probs[node]` for each interior node on the path to `value`.
    pub fn write_tree(&mut self, tree: &Tree, probs: &[u8], value: i32) {
        write_tree(tree, value, |node, bit| {
            let p = probs.get(node).copied().unwrap_or(128);
            self.write_bool(p, bit);
        });
    }

    /// Drain the remaining accumulator, resolving any final carry, and
    /// return the encoded bytes. Pads four bytes past the last real
    /// decision — always enough for a decoder to finish reading the last
    /// symbol regardless of exactly how many trailing bits its own
    /// renormalisation still needed, at the cost of a few harmless trailing
    /// zero bytes no VP8 decoder inspects.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        let mut c = self.bit_count;
        let mut v = self.bottom;
        if v & (1 << (32 - c)) != 0 {
            self.carry();
        }
        v <<= c & 7;
        c >>= 3;
        for _ in 0..c {
            v <<= 8;
        }
        for _ in 0..4 {
            self.out.push((v >> 24) as u8);
            v <<= 8;
        }
        self.out
    }
}

/// Every coefficient-probability "no update" bit RFC 6386 §13.4's
/// `update_coeff_probs()` reads, written in exactly the nested order
/// [`crate::header::parse`] reads them, at the same
/// [`tables::COEFF_UPDATE_PROBS`] probability the decoder uses for each —
/// required for a valid bitstream even though every value is "no update",
/// since the *probability itself*, not just the bit, has to match for the
/// arithmetic coder to stay in sync.
fn write_no_coeff_prob_updates(bw: &mut BoolWriter) {
    for plane in &tables::COEFF_UPDATE_PROBS {
        for band in plane {
            for ctx in band {
                for &p in ctx {
                    bw.write_bool(p, false);
                }
            }
        }
    }
}

/// The probability used for every macroblock's `mb_skip_coeff` bit.
/// Arbitrary: every macroblock writes skip=true regardless, and arithmetic
/// coding is correct at any probability value — this only affects
/// (irrelevant, since it is never used for anything but a single constant
/// bit) compression efficiency, not correctness.
const PROB_SKIP_FALSE: u8 = 1;

/// The compressed first partition: everything [`crate::header::parse`]
/// reads before the first macroblock record, then one `DC_PRED`/skip
/// record per macroblock, in raster order.
fn encode_compressed_header(mb_cols: usize, mb_rows: usize) -> Vec<u8> {
    let mut bw = BoolWriter::new();

    bw.write_literal(1, 0); // color_space
    bw.write_literal(1, 0); // clamping_type
    bw.write_flag(false); // segmentation_enabled
    bw.write_flag(false); // filter_type (normal)
    bw.write_literal(6, 0); // filter_level = 0 (loop filter is a no-op)
    bw.write_literal(3, 0); // sharpness_level = 0
    bw.write_flag(false); // loop_filter_adj_enable
    bw.write_literal(2, 0); // log2_nbr_of_dct_partitions = 0 -> 1 partition
    bw.write_literal(7, 0); // y_ac_qi (irrelevant: no coefficient is ever coded)
    for _ in 0..5 {
        bw.write_flag(false); // the five quantiser-delta "present" flags
    }
    bw.write_flag(false); // refresh_entropy_probs
    write_no_coeff_prob_updates(&mut bw);
    bw.write_flag(true); // mb_no_skip_coeff = 1: the per-macroblock skip bit is present
    bw.write_literal(8, u32::from(PROB_SKIP_FALSE));

    for _ in 0..mb_rows {
        for _ in 0..mb_cols {
            bw.write_bool(PROB_SKIP_FALSE, true); // mb_skip_coeff = 1
            bw.write_tree(&tables::KF_YMODE_TREE, &tables::KF_YMODE_PROB, tables::DC_PRED);
            bw.write_tree(&tables::UV_MODE_TREE, &tables::KF_UV_MODE_PROB, tables::DC_PRED);
        }
    }

    bw.finish()
}

/// The 3-byte uncompressed frame tag plus, for a key frame, the 3-byte
/// start code and the two 4-byte-aligned `(dimension, scale)` pairs —
/// exactly what `vaco_parse_vpx::vp8::parse_frame_tag` reads.
fn encode_uncompressed_header(width: u32, height: u32, first_part_size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    // key_frame bit is 0 (bit 0 of `raw`, meaning "is a key frame" per
    // RFC 6386 §9.1); version is 0; show_frame is 1 (bit 4).
    let raw: u32 = (1 << 4) | (first_part_size << 5);
    out.push((raw & 0xff) as u8);
    out.push(((raw >> 8) & 0xff) as u8);
    out.push(((raw >> 16) & 0xff) as u8);
    out.extend_from_slice(&[0x9d, 0x01, 0x2a]);
    let w14 = width.min(0x3fff) as u16;
    let h14 = height.min(0x3fff) as u16;
    out.extend_from_slice(&w14.to_le_bytes());
    out.extend_from_slice(&h14.to_le_bytes());
    out
}

/// Encode one all-intra VP8 key frame at `width`x`height` — see the module
/// doc for exactly what "encode" means here.
///
/// # Errors
///
/// [`Error::Unsupported`] if `width`/`height` is zero or exceeds the
/// format's 14-bit dimension field (16383). [`Error::InvalidData`] if the
/// compressed header overflows the frame tag's 19-bit `first_part_size`
/// field (over 524287 bytes — thousands of macroblocks' worth of `skip`
/// records, not reachable at any size this skeleton is meant for, but
/// checked rather than silently truncated).
pub fn encode_keyframe(width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(Error::Unsupported("vp8 encode: zero-sized frame"));
    }
    if width > 0x3fff || height > 0x3fff {
        return Err(Error::Unsupported(
            "vp8 encode: dimension exceeds the 14-bit field the key-frame tag can carry",
        ));
    }
    let mb_cols = (width as usize).div_ceil(16);
    let mb_rows = (height as usize).div_ceil(16);

    let compressed = encode_compressed_header(mb_cols, mb_rows);
    let first_part_size =
        u32::try_from(compressed.len()).map_err(|_| Error::InvalidData("vp8 encode: compressed header too large"))?;
    if first_part_size > 0x7_ffff {
        return Err(Error::InvalidData(
            "vp8 encode: compressed header overflows the frame tag's 19-bit first_part_size field",
        ));
    }

    // RFC 6386 §9.5: the first partition (frame header plus every
    // macroblock's mode/skip record, all read via `bd` above) is a
    // *separate* boolean-coded stream from the token partition(s) that
    // carry DCT coefficients — `first_part_size` marks exactly where the
    // first ends and the next begins. Every macroblock here is coded
    // `skip = 1`, so the single token partition (`log2_nbr_of_dct_partitions
    // = 0`) carries no coefficient bits at all, but it still has to exist as
    // a well-formed (if empty) arithmetic-coded stream: a bare
    // `BoolWriter`'s `finish()` with nothing written, which flushes to four
    // zero bytes a decoder can safely prime its own bool-decoder state from.
    // Omitting this partition entirely (found by decoding this crate's own
    // output with `ffmpeg`/`libvpx`, which rejected it as a corrupt key
    // frame despite the first partition parsing correctly under this
    // crate's own decoder) truncates the frame one partition short.
    let token_partition = BoolWriter::new().finish();

    let mut out = encode_uncompressed_header(width, height, first_part_size);
    out.extend_from_slice(&compressed);
    out.extend_from_slice(&token_partition);
    Ok(out)
}

fn frame_dims(frame: &Frame) -> Result<(u32, u32)> {
    match &frame.data {
        FrameData::Video { width, height, .. } => Ok((*width, *height)),
        FrameData::Audio { .. } | FrameData::Subtitle { .. } => Err(Error::InvalidData("vp8 encode: expected a video frame")),
    }
}

/// A [`vaco_codec_core::Encoder`] over this module's fixed all-intra
/// strategy. See the module doc for exactly what it does and does not do.
pub struct Vp8Encoder {
    machine: Machine<Packet>,
    limits: Limits,
}

impl std::fmt::Debug for Vp8Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp8Encoder").finish_non_exhaustive()
    }
}

impl Vp8Encoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Encoder for Vp8Encoder {
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
pub static VP8_ENCODER: EncoderDesc = EncoderDesc {
    name: "vp8",
    long_name: "On2 VP8 (all-intra skeleton: fixed DC_PRED/skip, no residual — see crate::encode)",
    id: vaco_codec_core::CodecId::Vp8,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(Vp8Encoder::new(limits)),
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
    use crate::decode::Vp8Decoder;
    use vaco_codec_core::Decoder;

    fn decode_first_frame(bytes: &[u8]) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, bytes).expect("packet");
        let mut dec = Vp8Decoder::new(Limits::permissive());
        dec.send_packet(Some(&packet)).expect("send");
        dec.receive_frame().expect("receive")
    }

    #[test]
    fn a_16x16_key_frame_round_trips_through_our_own_decoder() {
        let bytes = encode_keyframe(16, 16).expect("encode");
        let frame = decode_first_frame(&bytes);
        let FrameData::Video { width, height, format, .. } = frame.data else {
            panic!("video frame");
        };
        assert_eq!((width, height), (16, 16));
        assert_eq!(format, PixFmt::Yuv420p);
    }

    #[test]
    fn a_non_16_multiple_key_frame_round_trips() {
        let bytes = encode_keyframe(100, 70).expect("encode");
        let frame = decode_first_frame(&bytes);
        let FrameData::Video { width, height, .. } = frame.data else {
            panic!("video frame");
        };
        assert_eq!((width, height), (100, 70));
    }

    #[test]
    fn every_luma_sample_is_flat_128() {
        // DC_PRED with no residual and no neighbours converges to 128
        // everywhere, per the module doc.
        let bytes = encode_keyframe(32, 32).expect("encode");
        let frame = decode_first_frame(&bytes);
        let plane = frame.plane(0).expect("luma plane");
        for row in plane.rows_iter() {
            assert!(row.iter().all(|&b| b == 128), "expected flat 128, got {row:?}");
        }
    }

    #[test]
    fn the_token_partition_is_present_past_first_part_size() {
        // Regression test for the bug the module doc's "Two independent
        // bitstreams, not one" section describes: the encoder must emit
        // bytes for the (here, empty) token partition *after*
        // `first_part_size`'s worth of first-partition bytes, not stop
        // there — a decoder needs that partition to exist as a real
        // (if data-free) boolean-coded stream, even though this skeleton
        // never codes a coefficient into it.
        let bytes = encode_keyframe(16, 16).expect("encode");
        // Layout: 3-byte tag + 3-byte start code + 2+2 dimension bytes.
        let uncompressed_header_len = 10;
        let raw = u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16);
        let first_part_size = (raw >> 5) & 0x7_ffff;
        let first_partition_end = uncompressed_header_len + first_part_size as usize;
        assert!(
            bytes.len() > first_partition_end,
            "expected trailing token-partition bytes after the first partition, got {} total bytes ending exactly at the first partition ({first_partition_end})",
            bytes.len()
        );
        // An empty token partition is exactly a bare `BoolWriter::finish()`
        // with nothing written: four zero bytes.
        assert_eq!(&bytes[first_partition_end..], &[0, 0, 0, 0]);
    }

    #[test]
    fn rejects_zero_dimensions() {
        assert!(matches!(encode_keyframe(0, 16), Err(Error::Unsupported(_))));
        assert!(matches!(encode_keyframe(16, 0), Err(Error::Unsupported(_))));
    }

    #[test]
    fn send_receive_protocol_shape() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 16, 16).expect("alloc");
        let mut enc = Vp8Encoder::new(Limits::permissive());
        enc.send_frame(Some(&frame)).expect("send frame");
        let packet = enc.receive_packet().expect("receive packet");
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));
        enc.send_frame(None).expect("begin drain");
        assert!(matches!(enc.receive_packet(), Err(Error::Eof)));

        let frame = decode_first_frame(packet.payload());
        let FrameData::Video { width, height, .. } = frame.data else {
            panic!("video frame");
        };
        assert_eq!((width, height), (16, 16));
    }
}
