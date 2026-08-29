//! Top-level FFV1 framing: the Configuration Record (RFC 9043 §4.3), the
//! per-`Frame` `keyframe` bit (§4.4), the single-slice loop this crate
//! targets (§4.5-§4.9), and the glue between [`vaco_frame::Frame`]'s planes
//! and the RFC's Y/Cb/Cr-or-JPEG-2000-RCT sample domain (§3.7).
//!
//! `Vaco-Spec-Ref: rfc9043 RFC 9043 §4.3 (ConfigurationRecord), §4.4 (Frame),
//! §3.7.2 (RGB via JPEG 2000 RCT: Figures 6-7)`.
//!
//! # Coverage
//!
//! - **Version**: 3 only, matching `ffmpeg -c:v ffv1`'s own default (measured
//!   via `ffmpeg -h encoder=ffv1` and a real encode's Matroska `CodecPrivate`
//!   size — see the crate's top-level docs for the blackbox provenance entry).
//! - **Slicing**: this crate's own encoder writes exactly one slice.
//!   *Decode*, however, had to cover more than one: measured directly, even a
//!   64x64 `ffmpeg` encode — nowhere near RFC 9043 §5's 101376-pixel
//!   multi-slice threshold — defaults to a 2x2 slice grid. [`locate_slices`]
//!   walks `SliceFooter.slice_size` backward from the end of the packet to
//!   find each slice's independent byte range (RFC 9043 §4.9.1's own stated
//!   purpose for that field), and this is cross-checked pixel-exact against
//!   a real 4-slice `ffmpeg` file (range-coder mode — see the `coder_type`
//!   note below).
//! - **Coder**: this crate's own encoder always emits `coder_type = 1`
//!   (range coder, default table), and decoding that back is cross-checked
//!   pixel-exact against a real `ffmpeg -coder range_def` encode (multi-slice,
//!   Y/Cb/Cr all exact). `coder_type = 0` (Golomb-Rice) — `ffmpeg -c:v
//!   ffv1`'s own *default* — parses without erroring but has a known,
//!   unresolved decode bug: cross-checked against a real default-coder
//!   `ffmpeg` file, output diverges from the very first sample in a pattern
//!   consistent with the run-mode prefix in `rice.rs`'s `RunState` never
//!   correctly extending a run (every context-0 sample falls through to the
//!   terminating-value decode instead), even once the byte-level Sentinel
//!   handoff position was confirmed by exhaustive search not to be the
//!   cause. Documented here rather than silently left — `coder_type = 2`
//!   (custom state transition table) is untested for the same reason no
//!   fixture reaches it.
//! - **Bit depth**: 8 only.
//! - **Color**: `Yuv420p`/`Yuv422p`/`Yuv444p` (`colorspace_type` 0) and `Gbrp`
//!   (`colorspace_type` 1, via the JPEG 2000 RCT). No alpha plane. Own-encoder
//!   round trip is cross-checked for all four; the real-`ffmpeg` cross-check
//!   fixture is `Yuv420p` only.

use vaco_core::{Error, Result};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::crc::{crc32_ffv1, crc32_ffv1_parity};
use crate::params::{CoderType, ColorSpace, Parameters};
use crate::rangecoder::{RangeDecoder, RangeEncoder};
use crate::slice::{
    PlaneStates, SliceBuf, SliceFooter, SliceHeader, decode_plane_golomb, decode_plane_range,
    encode_plane_range, quant_index_for_plane,
};

/// This crate's per-format configuration: how a [`PixFmt`] maps onto FFV1's
/// `colorspace_type`/`chroma_planes`/subsampling parameters.
#[derive(Debug, Clone, Copy)]
struct FormatMapping {
    colorspace: ColorSpace,
    log2_h: u32,
    log2_v: u32,
}

fn mapping_for(format: PixFmt) -> Result<FormatMapping> {
    match format {
        PixFmt::Yuv420p => Ok(FormatMapping {
            colorspace: ColorSpace::YCbCr,
            log2_h: 1,
            log2_v: 1,
        }),
        PixFmt::Yuv422p => Ok(FormatMapping {
            colorspace: ColorSpace::YCbCr,
            log2_h: 1,
            log2_v: 0,
        }),
        PixFmt::Yuv444p => Ok(FormatMapping {
            colorspace: ColorSpace::YCbCr,
            log2_h: 0,
            log2_v: 0,
        }),
        PixFmt::Gbrp => Ok(FormatMapping {
            colorspace: ColorSpace::JpegRct,
            log2_h: 0,
            log2_v: 0,
        }),
        _ => Err(Error::Unsupported(
            "ffv1: pixel format not covered by this crate",
        )),
    }
}

fn format_for(colorspace: ColorSpace, log2_h: u32, log2_v: u32) -> Result<PixFmt> {
    match (colorspace, log2_h, log2_v) {
        (ColorSpace::YCbCr, 1, 1) => Ok(PixFmt::Yuv420p),
        (ColorSpace::YCbCr, 1, 0) => Ok(PixFmt::Yuv422p),
        (ColorSpace::YCbCr, 0, 0) => Ok(PixFmt::Yuv444p),
        (ColorSpace::JpegRct, 0, 0) => Ok(PixFmt::Gbrp),
        _ => Err(Error::Unsupported(
            "ffv1: colorspace/subsampling combination not covered by this crate",
        )),
    }
}

/// Pixel formats [`Ffv1Encoder`](crate::Ffv1Encoder) accepts, most-preferred
/// first — this crate's whole declared coverage (see the module docs).
pub(crate) const SUPPORTED_PIX_FMTS: &[PixFmt] = &[
    PixFmt::Yuv420p,
    PixFmt::Yuv444p,
    PixFmt::Yuv422p,
    PixFmt::Gbrp,
];

/// Build the Configuration Record (RFC 9043 §4.3): `Parameters()`, no
/// `reserved_for_future_use` (this crate's encoder never writes any), and a
/// trailing CRC.
///
/// # Errors
/// Whatever [`Parameters::write`] returns (never, for a `Parameters` this
/// crate builds itself).
pub(crate) fn build_extradata(params: &Parameters) -> Result<Vec<u8>> {
    let mut enc = RangeEncoder::new();
    params.write(&mut enc)?;
    let mut bytes = enc.finish();
    let parity = crc32_ffv1_parity(&bytes);
    bytes.extend_from_slice(&parity);
    Ok(bytes)
}

/// Parse a Configuration Record, verifying the trailing CRC first (RFC 9043
/// §4.3.2) so a corrupt or truncated record is rejected before any field
/// parsing runs.
///
/// # Errors
/// [`Error::UnexpectedEof`] if `data` is shorter than the trailing CRC;
/// [`Error::InvalidData`] if the CRC does not check out; otherwise whatever
/// [`Parameters::parse`] returns.
pub(crate) fn parse_extradata(data: &[u8]) -> Result<Parameters> {
    if data.len() < 4 {
        return Err(Error::UnexpectedEof);
    }
    if !extradata_crc_ok(data) {
        return Err(Error::InvalidData(
            "ffv1: configuration record CRC mismatch",
        ));
    }
    let mut dec = RangeDecoder::new(data);
    Parameters::parse(&mut dec)
}

/// Whether `data`'s trailing 4 bytes are a valid CRC parity for the record
/// (RFC 9043 §4.3.2: the whole record, including the parity, CRCs to zero
/// under the non-reflected variant confirmed in `crc.rs`).
#[must_use]
pub(crate) fn extradata_crc_ok(data: &[u8]) -> bool {
    crc32_ffv1(data) == 0
}

/// Everything needed to decode/encode frames once the Configuration Record
/// is known: the parsed [`Parameters`] plus the [`PixFmt`] it maps to.
#[derive(Debug, Clone)]
pub(crate) struct Ffv1Config {
    pub params: Parameters,
    pub format: PixFmt,
}

impl Ffv1Config {
    /// Build the encoder-side config for `format` (must be one of
    /// [`SUPPORTED_PIX_FMTS`]).
    ///
    /// # Errors
    /// [`Error::Unsupported`] for any other format.
    pub(crate) fn for_encode(format: PixFmt) -> Result<Self> {
        let mapping = mapping_for(format)?;
        let params =
            Parameters::own_encoder(mapping.colorspace, 8, true, mapping.log2_h, mapping.log2_v);
        Ok(Self { params, format })
    }

    /// Parse from a Configuration Record (decode-side, via `set_extradata`).
    ///
    /// # Errors
    /// Whatever [`parse_extradata`] returns, plus [`Error::Unsupported`] if
    /// the resulting colorspace/subsampling/bit-depth is not one this crate
    /// covers.
    pub(crate) fn from_extradata(data: &[u8]) -> Result<Self> {
        let params = parse_extradata(data)?;
        if params.bits_per_raw_sample != 8 {
            return Err(Error::Unsupported(
                "ffv1: only 8-bit content is implemented",
            ));
        }
        if params.extra_plane {
            return Err(Error::Unsupported(
                "ffv1: alpha/extra plane is not implemented",
            ));
        }
        let format = format_for(
            params.colorspace,
            params.log2_h_chroma_subsample,
            params.log2_v_chroma_subsample,
        )?;
        Ok(Self { params, format })
    }
}

/// Read a plane's `w x h` unsigned 8-bit samples out of a decoded
/// [`SliceBuf`] into a `vaco_frame` plane at `(x_off, y_off)`, respecting
/// stride. The offset is what makes multiple slices land in their own
/// distinct region of one shared output frame (RFC 9043 §4.7.3-§4.8.3's
/// slice geometry) instead of all overwriting the top-left corner.
fn write_pixels(buf: &SliceBuf, dst: &mut vaco_frame::PlaneMut<'_>, x_off: usize, y_off: usize) {
    for y in 0..buf.h {
        let Some(row) = dst.row_mut(y + y_off) else {
            continue;
        };
        for x in 0..buf.w {
            if let Some(slot) = row.get_mut(x + x_off) {
                *slot = buf.get(x, y) as u8;
            }
        }
    }
}

/// The inverse: copy an 8-bit plane into a fresh [`SliceBuf`] of `i32`s.
fn read_pixels(
    budget: &mut Budget,
    src: &vaco_frame::PlaneRef<'_>,
    w: usize,
    h: usize,
) -> Result<SliceBuf> {
    let mut buf = SliceBuf::alloc(budget, w, h)?;
    for y in 0..h {
        let Some(row) = src.row(y) else { continue };
        for x in 0..w {
            let v = row.get(x).copied().unwrap_or(0);
            buf.set(x, y, i32::from(v));
        }
    }
    Ok(buf)
}

/// RFC 9043 §3.7.2's JPEG 2000 RCT, forward direction (RGB -> coded Y/Cb/Cr).
/// `bits` is `bits_per_raw_sample`; this crate only reaches 8, where the RGB
/// Exception (§3.7.2.1, for `bits_per_raw_sample` 9-15) does not apply.
fn rct_forward(g: i32, b: i32, r: i32, bits: u32) -> (i32, i32, i32) {
    let cb = b - g;
    let cr = r - g;
    let y = g + ((cb + cr) >> 2);
    let offset = 1i32 << bits;
    (y, cb + offset, cr + offset)
}

/// Inverse of [`rct_forward`].
fn rct_inverse(y: i32, cb_off: i32, cr_off: i32, bits: u32) -> (i32, i32, i32) {
    let offset = 1i32 << bits;
    let cb = cb_off - offset;
    let cr = cr_off - offset;
    let g = y - ((cb + cr) >> 2);
    let r = cr + g;
    let b = cb + g;
    (g, b, r)
}

/// The single-bit `keyframe` context, read/written once per `Frame()` (not
/// per slice — it sits outside the `while` loop over `Slice()` in RFC 9043
/// §4.4's pseudocode), as opposed to per-pixel context state (`PlaneStates`)
/// and `SliceHeader`'s own state array, both of which reset fresh for every
/// *slice* (RFC 9043 §3.8.1.3 for per-pixel contexts on every keyframe;
/// `SliceHeader`'s own reset follows from slices being independently
/// decodable — see `locate_slices`'s docs).
///
/// RFC 9043 says this context "has its own initial state, set to 128" but
/// never says whether a *later* frame's read resets to that or keeps
/// adapting. An earlier version of this crate kept adapting it across every
/// frame of one stream, on the strength of a byte-level comparison of a real
/// 5-frame `ffmpeg` encode's own output ("frame 0's opening bytes differ from
/// frames 1-4's, which are identical to each other despite different pixel
/// content, exactly what a persisting, saturating context produces"). That
/// comparison never actually decoded the bytes back, and it was wrong: a real
/// multi-frame `ffmpeg` file decodes with plausible `SliceHeader` geometry on
/// every frame only when this resets to 128 for every `Frame()`, and produces
/// nonsense geometry (values nowhere near the frame, headers reading a
/// quant-table index out of range) from the *second* frame onward when it is
/// allowed to persist — measured directly by forcing each read to a fresh 128
/// and watching every frame's header become sane again. A single-frame test
/// cannot catch this either way, which is exactly why the earlier, wrong
/// model shipped unnoticed: it only shows up decoding the second frame of a
/// real multi-frame file onward.
///
/// This crate's own encoder and decoder were a self-consistent pair either
/// way — the round trip passed under the old model too, since both sides
/// agreed with each other — which is why fixing this needed a *real*
/// `ffmpeg`-produced multi-frame fixture, not another round trip through this
/// crate's own encoder.
#[must_use]
pub(crate) const fn fresh_keyframe_state() -> u8 {
    128
}

/// Per-pixel adaptive context state, one [`PlaneStates`] per slice *position*
/// (index into [`locate_slices`]'s output, stable frame to frame since
/// `num_h_slices`/`num_v_slices` are stream-wide), persisted across
/// [`decode_frame`] calls.
///
/// RFC 9043 §3.8.1.3/§3.8.2.5 say these contexts reset "on a keyframe" — not
/// on every frame, which is what an earlier version of this crate did
/// unconditionally, on the theory that this crate treats every frame as a
/// keyframe (its own intra-only encoder always writes `keyframe = 1`). That
/// theory does not hold for a real `ffmpeg`-produced stream: measured
/// directly against a real 5-frame, 4-slice `ffmpeg -coder range_def` encode,
/// only frame 0 reads `keyframe = true`; frames 1-4 read `false`, and decode
/// with plausible `SliceHeader` geometry (this crate already got that right)
/// but *garbage pixels* — every context starting cold at 128 instead of
/// wherever frame 0 left it adapted, which reads as a near-zero-residual bias
/// for the first several samples of every following frame. Keying the reset
/// to the decoded `keyframe` bit instead of to "every frame" fixed it.
///
/// This crate's own encoder never writes `keyframe = 0`, so [`PersistedContexts::reset`]
/// runs on every one of its frames and the persisting path below is never
/// exercised by this crate's own round-trip tests — only by decoding a real
/// multi-frame `ffmpeg` file with more than one keyframe-marked frame in a
/// row absent. That asymmetry is exactly why this was invisible until now.
#[derive(Debug, Clone, Default)]
pub(crate) struct PersistedContexts {
    slices: Vec<PlaneStates>,
}

impl PersistedContexts {
    /// Discard every slice position's adapted state, so the next
    /// [`PersistedContexts::slot`] call for each position starts fresh.
    /// Call this whenever a frame's own `keyframe` bit reads `true`, and once
    /// up front (a fresh decoder already has an empty `slices`, so this only
    /// matters after at least one frame has been decoded — [`Ffv1Decoder`]
    /// also calls it from `flush`, so a seek does not resume mid-adaptation
    /// against a discontinuous stream).
    pub(crate) fn reset(&mut self) {
        self.slices.clear();
    }

    /// The adapted state for slice position `i`, creating a fresh
    /// [`PlaneStates`] the first time this position is asked for (either at
    /// the very start of the stream, or right after [`PersistedContexts::reset`]
    /// cleared it). Falls back to a scratch, never-persisted [`PlaneStates`]
    /// on the unreachable branch where growing `slices` to cover `i` still
    /// left it out of bounds, rather than indexing or panicking.
    pub(crate) fn slot<'a>(
        &'a mut self,
        i: usize,
        quant_table_set_index_count: usize,
        scratch: &'a mut PlaneStates,
    ) -> &'a mut PlaneStates {
        if self.slices.len() <= i {
            self.slices
                .resize_with(i + 1, || PlaneStates::fresh(quant_table_set_index_count));
        }
        self.slices.get_mut(i).unwrap_or(scratch)
    }
}

/// Locate every `Slice()`'s `[start, end)` byte range within a `Frame()`'s
/// body (after the `keyframe` bit — `start` for the first slice still
/// includes it, see below), by walking `SliceFooter`s **backward** from the
/// end of `data`.
///
/// RFC 9043 describes slices as independently decodable ("provides
/// opportunities for... multithreaded encoding and decoding", §4.5) and
/// gives each one its own fixed-size, byte-aligned `SliceFooter` whose
/// `slice_size` is "the size of the Slice in bytes" with the explicit note
/// "this allows finding the start of Slices before previous Slices have been
/// fully decoded" (§4.9.1) — i.e. exactly this backward walk. It sidesteps
/// entirely the harder problem this crate does *not* solve in general: where
/// a range-coded region ends when you have not decoded it (needed only once,
/// for the Sentinel-mode handoff into Golomb content within a single slice —
/// see `rangecoder.rs`).
///
/// Measured against a real `ffmpeg` encode: even a 64x64 frame — nowhere
/// near RFC 9043 §5's 101376-pixel multi-slice threshold — comes out as a
/// 2x2 slice grid by default, so this is not an edge case a real decoder can
/// skip.
///
/// # Errors
/// [`Error::InvalidData`] if a `slice_size` is `0` or larger than the bytes
/// remaining (would loop forever / read out of bounds otherwise).
fn locate_slices(data: &[u8], ec: u32) -> Result<Vec<std::ops::Range<usize>>> {
    let footer_len = SliceFooter::byte_len(ec);
    let mut ranges = Vec::new();
    let mut pos = data.len();
    let mut guard = 0u32;
    while pos > 0 {
        guard += 1;
        if guard > 4096 {
            return Err(Error::InvalidData("ffv1: too many slices"));
        }
        let footer_start = pos
            .checked_sub(footer_len)
            .ok_or(Error::InvalidData("ffv1: truncated slice footer"))?;
        let footer =
            SliceFooter::read(data.get(footer_start..pos).ok_or(Error::UnexpectedEof)?, ec)?;
        // `slice_size` measures the SliceHeader+SliceContent span only, not
        // this slice's own trailing SliceFooter — measured directly against
        // a real 4-slice ffmpeg frame: treating it as covering the footer
        // too (the more obvious reading of "the size of the Slice in
        // bytes") walked straight into the middle of the previous slice's
        // content on the second step, while excluding the footer chains
        // all 4 slices back to byte 0 exactly.
        let slice_size = footer.slice_size as usize;
        if slice_size == 0 || slice_size > footer_start {
            return Err(Error::InvalidData("ffv1: implausible slice_size"));
        }
        let content_start = footer_start - slice_size;
        ranges.push(content_start..pos);
        pos = content_start;
    }
    ranges.reverse();
    Ok(ranges)
}

/// Decode one packet's worth of frame body (the `keyframe` bit + one
/// `Slice()`) into pixel planes, and write them into a freshly allocated
/// [`vaco_frame::Frame`].
///
/// # Errors
/// [`Error::InvalidData`]/[`Error::Unsupported`] for anything this crate's
/// scope does not cover (see the module docs); [`Error::LimitExceeded`] if
/// the frame's dimensions exceed `budget`.
pub(crate) fn decode_frame(
    config: &Ffv1Config,
    contexts: &mut PersistedContexts,
    data: &[u8],
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> Result<vaco_frame::Frame> {
    let params = &config.params;
    let quant_index_count = params.quant_table_set_index_count();
    let mut frame = vaco_frame::Frame::alloc_video(budget, config.format, width, height)?;

    // Frame(): keyframe bit (read once, from the first slice's own byte
    // range — fresh coding state every frame, see fresh_keyframe_state's
    // docs), then one or more independent Slice()s, each with its own byte
    // range found via SliceFooter.slice_size chained backward from the end
    // (see locate_slices's docs).
    let slice_ranges = locate_slices(data, params.ec)?;
    let footer_len = SliceFooter::byte_len(params.ec);

    for (i, range) in slice_ranges.iter().enumerate() {
        let footer_start = range
            .end
            .checked_sub(footer_len)
            .ok_or(Error::InvalidData("ffv1: slice shorter than its footer"))?;
        let region = data
            .get(range.start..footer_start)
            .ok_or(Error::UnexpectedEof)?;
        let mut dec = RangeDecoder::new(region);
        if i == 0 {
            let mut keyframe_state = fresh_keyframe_state();
            let keyframe = dec.get_rac(&mut keyframe_state, &params.state_transition);
            // RFC 9043 §3.8.1.3/§3.8.2.5: per-pixel contexts reset "on a
            // keyframe" — not on every frame. This crate's own encoder always
            // writes `keyframe = 1` (its own intra-only scope), but a real
            // multi-frame `ffmpeg` encode writes it only on the first frame
            // and expects every following frame to keep adapting every slice
            // position's context from where the previous frame left it. See
            // PersistedContexts's docs for how that was measured.
            if keyframe {
                contexts.reset();
            }
        }

        // SliceHeader's own state array resets fresh for every slice: slices
        // are independently decodable (see locate_slices's docs), which a
        // shared/adapting array across them would contradict.
        let mut header_states = crate::rangecoder::fresh_states();
        let header = SliceHeader::parse(
            &mut dec,
            &params.state_transition,
            &mut header_states,
            quant_index_count,
        )?;
        let (slice_x, slice_y, slice_w, slice_h) =
            header.geometry(width, height, params.num_h_slices, params.num_v_slices);

        let plane_count = if params.chroma_planes { 3 } else { 1 };
        // Indexed by quant-table-set-index *slot* (0=luma, 1=chroma, ...),
        // not by plane — see PlaneStates's docs: Cb and Cr share slot 1 and,
        // measured against a real ffmpeg encode, also share one adapting
        // context array. This slice position's own array persists across
        // frames unless the frame just read `keyframe = true` (see
        // fresh_keyframe_state's docs) — `contexts.slot` returns whatever was
        // left adapted from this same slice position last time, or a fresh
        // one the first time this position is used.
        let mut scratch = PlaneStates::fresh(quant_index_count);
        let plane_states = contexts.slot(i, quant_index_count, &mut scratch);

        let planes: Vec<SliceBuf> = match params.coder_type {
            CoderType::RangeDefault | CoderType::RangeCustom => (0..plane_count)
                .map(|p| {
                    let (pw, ph) = plane_dims(p, slice_w, slice_h, params);
                    let qts = quant_table_for_plane(params, &header, p, quant_index_count)?;
                    let slot = quant_index_for_plane(p, params.chroma_planes, params.version)
                        .min(quant_index_count.saturating_sub(1));
                    let states = plane_states
                        .range
                        .get_mut(slot)
                        .ok_or(Error::InvalidData("ffv1: plane index"))?;
                    let coded_bits = coded_bits(params);
                    decode_plane_range(
                        &mut dec,
                        &params.state_transition,
                        qts,
                        states,
                        coded_bits,
                        pw,
                        ph,
                        budget,
                    )
                })
                .collect::<Result<_>>()?,
            CoderType::GolombRice => {
                // RFC 9043 §3.8.1.1.1: the switch from the range-coded
                // SliceHeader to Golomb-coded content is Sentinel mode —
                // read the terminator, then hand off to a plain bit reader
                // at the resulting byte position *within this slice's own
                // region* (each slice is independently byte-ranged; see
                // locate_slices). See rangecoder.rs's module docs.
                dec.read_terminator(&params.state_transition);
                let start = dec.byte_pos();
                let mut r = vaco_bitstream::BitReader::new(region.get(start..).unwrap_or(&[]));
                (0..plane_count)
                    .map(|p| {
                        let (pw, ph) = plane_dims(p, slice_w, slice_h, params);
                        let qts = quant_table_for_plane(params, &header, p, quant_index_count)?;
                        let slot = quant_index_for_plane(p, params.chroma_planes, params.version)
                            .min(quant_index_count.saturating_sub(1));
                        let states = plane_states
                            .rice
                            .get_mut(slot)
                            .ok_or(Error::InvalidData("ffv1: plane index"))?;
                        let coded_bits = coded_bits(params);
                        decode_plane_golomb(&mut r, qts, states, coded_bits, pw, ph, budget)
                    })
                    .collect::<Result<_>>()?
            }
        };

        store_planes(&mut frame, &planes, params, slice_x, slice_y)?;
    }

    Ok(frame)
}

fn coded_bits(params: &Parameters) -> u32 {
    if params.colorspace == ColorSpace::JpegRct {
        params.bits_per_raw_sample + 1
    } else {
        params.bits_per_raw_sample
    }
}

fn quant_table_for_plane<'p>(
    params: &'p Parameters,
    header: &SliceHeader,
    p: usize,
    quant_index_count: usize,
) -> Result<&'p crate::quant::QuantTableSet> {
    let qidx = header
        .quant_table_set_index
        .get(
            quant_index_for_plane(p, params.chroma_planes, params.version)
                .min(quant_index_count.saturating_sub(1)),
        )
        .copied()
        .unwrap_or(0);
    params
        .quant_tables
        .get(qidx as usize)
        .ok_or(Error::InvalidData(
            "ffv1: quant_table_set_index out of range",
        ))
}

/// A luma-plane pixel offset's chroma-plane equivalent: floor-divided by the
/// subsampling factor, matching how a subsampled pixel at chroma index `c`
/// covers luma indices `[c * 2^log2, (c+1) * 2^log2)` — the standard
/// chroma-siting convention, and consistent with how [`plane_dims`] sizes a
/// chroma plane with a ceiling division of the *extent*.
const fn chroma_origin(luma_off: u32, log2: u32) -> u32 {
    luma_off >> log2
}

fn plane_dims(p: usize, slice_w: u32, slice_h: u32, params: &Parameters) -> (usize, usize) {
    if p == 0 || !params.chroma_planes {
        (slice_w as usize, slice_h as usize)
    } else {
        let w = (u64::from(slice_w)).div_ceil(1u64 << params.log2_h_chroma_subsample) as usize;
        let h = (u64::from(slice_h)).div_ceil(1u64 << params.log2_v_chroma_subsample) as usize;
        (w, h)
    }
}

/// Copy one slice's decoded plane buffers into the output frame at pixel
/// origin `(slice_x, slice_y)` (luma coordinates — chroma planes scale their
/// own offset down, see [`chroma_origin`]), undoing the RCT for `JpegRct`
/// content.
#[allow(
    clippy::many_single_char_names,
    reason = "g/b/r and w/h read naturally for RGB-plane and geometry variables"
)]
fn store_planes(
    frame: &mut vaco_frame::Frame,
    planes: &[SliceBuf],
    params: &Parameters,
    slice_x: u32,
    slice_y: u32,
) -> Result<()> {
    let offset_for = |p: usize| -> (usize, usize) {
        if p == 0 || !params.chroma_planes {
            (slice_x as usize, slice_y as usize)
        } else {
            (
                chroma_origin(slice_x, params.log2_h_chroma_subsample) as usize,
                chroma_origin(slice_y, params.log2_v_chroma_subsample) as usize,
            )
        }
    };
    match params.colorspace {
        ColorSpace::YCbCr => {
            let mut dst_planes = frame.planes_mut();
            for (p, buf) in planes.iter().enumerate() {
                let (x_off, y_off) = offset_for(p);
                if let Some(dst) = dst_planes.get_mut(p) {
                    write_pixels(buf, dst, x_off, y_off);
                }
            }
        }
        ColorSpace::JpegRct => {
            let y_buf = planes
                .first()
                .ok_or(Error::InvalidData("ffv1: missing Y plane"))?;
            let cb_buf = planes
                .get(1)
                .ok_or(Error::InvalidData("ffv1: missing Cb plane"))?;
            let cr_buf = planes
                .get(2)
                .ok_or(Error::InvalidData("ffv1: missing Cr plane"))?;
            let (w, h) = (y_buf.w, y_buf.h);
            let mut g_buf =
                SliceBuf::alloc(&mut Budget::new(vaco_limits::Limits::permissive()), w, h)?;
            let mut b_buf =
                SliceBuf::alloc(&mut Budget::new(vaco_limits::Limits::permissive()), w, h)?;
            let mut r_buf =
                SliceBuf::alloc(&mut Budget::new(vaco_limits::Limits::permissive()), w, h)?;
            for y in 0..h {
                for x in 0..w {
                    let (g, b, r) = rct_inverse(
                        y_buf.get(x, y),
                        cb_buf.get(x, y),
                        cr_buf.get(x, y),
                        params.bits_per_raw_sample,
                    );
                    g_buf.set(x, y, g);
                    b_buf.set(x, y, b);
                    r_buf.set(x, y, r);
                }
            }
            let (x_off, y_off) = offset_for(0);
            let mut dst_planes = frame.planes_mut();
            // Gbrp plane order is G, B, R (vaco-pixfmt's own component table).
            if let Some(dst) = dst_planes.first_mut() {
                write_pixels(&g_buf, dst, x_off, y_off);
            }
            if let Some(dst) = dst_planes.get_mut(1) {
                write_pixels(&b_buf, dst, x_off, y_off);
            }
            if let Some(dst) = dst_planes.get_mut(2) {
                write_pixels(&r_buf, dst, x_off, y_off);
            }
        }
    }
    Ok(())
}

/// Encode one frame into a packet body: `keyframe = 1`, one `SliceHeader()`,
/// `SliceContent()` (range coder, `coder_type = 1`), `SliceFooter()`.
///
/// # Errors
/// [`Error::Unsupported`] if `frame`'s pixel format is not
/// [`config.format`](Ffv1Config::format), [`Error::InvalidData`] for a frame
/// missing planes its own format declares.
pub(crate) fn encode_frame(config: &Ffv1Config, frame: &vaco_frame::Frame) -> Result<Vec<u8>> {
    let params = &config.params;
    let vaco_frame::FrameData::Video {
        format,
        width,
        height,
        ..
    } = &frame.data
    else {
        return Err(Error::InvalidData("ffv1: expected a video frame"));
    };
    if *format != config.format {
        return Err(Error::Unsupported(
            "ffv1: frame pixel format does not match this encoder's configuration",
        ));
    }
    let (width, height) = (*width, *height);

    let plane_count = if params.chroma_planes { 3 } else { 1 };
    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let planes = load_planes(frame, params, width, height, &mut budget)?;

    let mut enc = RangeEncoder::new();
    let mut keyframe_state = fresh_keyframe_state();
    enc.put_rac(&mut keyframe_state, &params.state_transition, true); // keyframe = 1

    let header = SliceHeader::whole_frame(params.quant_table_set_index_count());
    let mut header_states = crate::rangecoder::fresh_states();
    header.write(&mut enc, &params.state_transition, &mut header_states)?;

    let quant_index_count = header.quant_table_set_index.len();
    // Indexed by quant-table-set-index slot, not by plane — see
    // PlaneStates's docs (Cb/Cr share slot 1 and its adapting context array).
    let mut plane_states = PlaneStates::fresh(quant_index_count.max(1));
    for p in 0..plane_count {
        let slot = quant_index_for_plane(p, params.chroma_planes, params.version)
            .min(quant_index_count.saturating_sub(1));
        let qidx = header.quant_table_set_index.get(slot).copied().unwrap_or(0);
        let qts = params
            .quant_tables
            .get(qidx as usize)
            .ok_or(Error::InvalidData(
                "ffv1: quant_table_set_index out of range",
            ))?;
        let states = plane_states
            .range
            .get_mut(slot)
            .ok_or(Error::InvalidData("ffv1: plane index"))?;
        let coded_bits = if params.colorspace == ColorSpace::JpegRct {
            params.bits_per_raw_sample + 1
        } else {
            params.bits_per_raw_sample
        };
        let buf = planes
            .get(p)
            .ok_or(Error::InvalidData("ffv1: plane index"))?;
        encode_plane_range(
            &mut enc,
            &params.state_transition,
            qts,
            states,
            coded_bits,
            buf,
        )?;
    }

    let content = enc.finish();
    // slice_size excludes the footer's own bytes (see locate_slices's docs).
    let footer_bytes = SliceFooter::write(content.len() as u32);
    let mut out = content;
    out.extend_from_slice(&footer_bytes);
    Ok(out)
}

/// Build per-plane `i32` buffers ready for [`encode_plane_range`], applying
/// the RCT for `JpegRct` content.
#[allow(
    clippy::many_single_char_names,
    reason = "g/b/r read naturally for RGB-plane variables"
)]
fn load_planes(
    frame: &vaco_frame::Frame,
    params: &Parameters,
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> Result<Vec<SliceBuf>> {
    let plane_count = if params.chroma_planes { 3 } else { 1 };
    match params.colorspace {
        ColorSpace::YCbCr => {
            let mut out = Vec::new();
            for p in 0..plane_count {
                let (pw, ph) = plane_dims(p, width, height, params);
                let plane = frame
                    .plane(p)
                    .ok_or(Error::InvalidData("ffv1: missing plane"))?;
                out.push(read_pixels(budget, &plane, pw, ph)?);
            }
            Ok(out)
        }
        ColorSpace::JpegRct => {
            // Gbrp plane order is G, B, R.
            let g_plane = frame
                .plane(0)
                .ok_or(Error::InvalidData("ffv1: missing G plane"))?;
            let b_plane = frame
                .plane(1)
                .ok_or(Error::InvalidData("ffv1: missing B plane"))?;
            let r_plane = frame
                .plane(2)
                .ok_or(Error::InvalidData("ffv1: missing R plane"))?;
            let (w, h) = (width as usize, height as usize);
            let g_buf = read_pixels(budget, &g_plane, w, h)?;
            let b_buf = read_pixels(budget, &b_plane, w, h)?;
            let r_buf = read_pixels(budget, &r_plane, w, h)?;
            let mut y_buf = SliceBuf::alloc(budget, w, h)?;
            let mut cb_buf = SliceBuf::alloc(budget, w, h)?;
            let mut cr_buf = SliceBuf::alloc(budget, w, h)?;
            for y in 0..h {
                for x in 0..w {
                    let (yv, cb, cr) = rct_forward(
                        g_buf.get(x, y),
                        b_buf.get(x, y),
                        r_buf.get(x, y),
                        params.bits_per_raw_sample,
                    );
                    y_buf.set(x, y, yv);
                    cb_buf.set(x, y, cb);
                    cr_buf.set(x, y, cr);
                }
            }
            Ok(vec![y_buf, cb_buf, cr_buf])
        }
    }
}
