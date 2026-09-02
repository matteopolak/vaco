//! The real ALAC per-packet bitstream: one packet in, one [`Frame`] out
//! (and back).
//!
//! # Provenance — supersedes this crate's own earlier framing
//!
//! An earlier version of this module defined its own packet framing
//! (`channels-1`/`bit_depth_code`/`num_samples`/`escape` fields, sample-
//! interleaved adaptive-Rice residuals) because issue #285's brief read as a
//! blanket prohibition on consulting any ALAC reference — self-consistent,
//! round-trip-clean against itself, and unable to decode a single real ALAC
//! file. Apple's ALAC reference (<https://github.com/macosforge/alac>) is
//! Apache License 2.0 — confirmed directly — which is outside this
//! project's D7/D15 clean-room rule (specifically FFmpeg/libav GPL code).
//! This module is a translation of `ALACDecoder.cpp`'s `Decode()` element
//! loop (`ID_SCE`/`ID_CPE` cases) and `matrix_dec.c`'s `unmix16` (this
//! crate's encoder always chooses `mixres == 0`, i.e. the "conventional,
//! just-interleave" path, so the matrixed-stereo arithmetic in `unmix16`'s
//! `mixres != 0` branch is exercised by decode but never produced by this
//! crate's own encoder; still implemented, since a real file may use it).
//!
//! `Vaco-Spec-Ref: alac-agc-source codec/ALACDecoder.cpp Decode() (ID_SCE/
//! ID_CPE element parsing, chanBits derivation), codec/matrix_dec.c
//! unmix16, Apple Inc., Apache License 2.0`
//!
//! # What decodes, and what this crate's own encoder produces
//!
//! **Decode** handles what a real encoder emits: `ID_SCE` (mono) and
//! `ID_CPE` (stereo) elements, both `modeU`/`modeV` predictor stages (0:
//! straight `numU`/`numV`-tap prediction; 1: an order-31 first-difference
//! pre-pass then the tap predictor), the escape/verbatim path, and both
//! matrixed (`mixres != 0`) and conventional (`mixres == 0`) stereo. **Not**
//! handled: `bytesShifted != 0` ("wasted bits" — [`Error::Unsupported`],
//! a real, documented gap: a real encoder can choose it and this cannot
//! decode that packet), `ID_CCE`/`ID_DSE`/`ID_PCE`/`ID_FIL` elements, and
//! more than one `ID_SCE`/`ID_CPE` per packet (multichannel beyond stereo).
//!
//! **Encode** uses a real, fixed-order (see [`encode`]'s doc) adaptive
//! predictor plus adaptive-Rice coding — not a rate-distortion search
//! across orders/modes the way Apple's own encoder does, but genuine
//! linear prediction rather than the `numU`/`numV = 0` "residual is the
//! sample" identity case an earlier version of this function always chose.
//! `mixres = mixbits = 0` for stereo (plain interleave, no decorrelation),
//! `bytesShifted = 0`, `escapeFlag = 0`. The nominal predictor order this
//! function writes is a fixed constant regardless of frame length --
//! [`pc_block`]/[`unpc_block`] both clamp it down to `num_samples - 1` (or
//! `1`, or run the trivial `num_samples <= 1` case) identically and
//! deterministically from `num_samples` alone, so a short final frame
//! degrades gracefully without this function needing its own fallback
//! logic. `partialFrame` is always set with an explicit sample count.
//! `escapeFlag = 1` (verbatim, no predictor at all) is [`decode`]'s to
//! read, not [`encode`]'s to write anymore -- see `tests/oracle_alac_crate.rs`
//! for the independent-decoder verification both element-level modes are
//! checked against.

use vaco_bitstream::{BitReader, BitWriter};
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::predictor::{MAX_ORDER, pc_block, unpc_block};
use crate::rice::{AgParams, dyn_comp, dyn_decomp};

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_LFE: u32 = 3;
const ID_END: u32 = 7;

/// Only mono and stereo packets are supported — see the module doc.
const MAX_CHANNELS: u8 = 2;

fn bytes_per_sample(fmt: SampleFmt) -> Result<usize> {
    match fmt {
        SampleFmt::S16P => Ok(2),
        SampleFmt::S32P => Ok(4),
        _ => Err(Error::Unsupported(
            "alac: encoder accepts s16p or s32p input only",
        )),
    }
}

fn read_sample(buf: &[u8], index: usize, bytes: usize) -> i32 {
    let off = index.saturating_mul(bytes);
    match bytes {
        2 => i32::from(
            buf.get(off..off.saturating_add(2))
                .and_then(|s| s.try_into().ok())
                .map_or(0i16, i16::from_le_bytes),
        ),
        4 => buf
            .get(off..off.saturating_add(4))
            .and_then(|s| s.try_into().ok())
            .map_or(0, i32::from_le_bytes),
        _ => 0,
    }
}

/// Write `value` into plane byte buffer `buf` at sample `index`, as a
/// 4-byte little-endian `i32`. Used only when the packet's own `bit_depth`
/// is greater than 16 — see [`decode`]'s `out_fmt` selection.
fn write_sample_s32(buf: &mut [u8], index: usize, value: i32) {
    let off = index.saturating_mul(4);
    if let Some(dst) = buf.get_mut(off..off.saturating_add(4)) {
        dst.copy_from_slice(&value.to_le_bytes());
    }
}

/// Write `value` into plane byte buffer `buf` at sample `index`, as a
/// 2-byte little-endian `i16`, clamped to `i16`'s range. `value` is already
/// a genuine `chan_bits`-wide (≤16) sample here, so the clamp is a no-op in
/// practice; it only guards the theoretical case of a malformed packet
/// whose decoded residual falls outside that range.
fn write_sample_s16(buf: &mut [u8], index: usize, value: i32) {
    let off = index.saturating_mul(2);
    if let Some(dst) = buf.get_mut(off..off.saturating_add(2)) {
        let clamped = value.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "just clamped to i16's range"
        )]
        dst.copy_from_slice(&(clamped as i16).to_le_bytes());
    }
}

/// One channel's decoded predictor output, per `modeU`/`modeV`.
fn predict_channel(
    residuals: &[i32],
    coefs: &mut [i32],
    order: usize,
    mode: u32,
    chanbits: u32,
    denshift: u32,
    budget: &mut Budget,
) -> Result<Vec<i32>> {
    if mode == 0 {
        Ok(unpc_block(
            residuals, coefs, order, chanbits, denshift, budget,
        )?)
    } else {
        let empty: &mut [i32] = &mut [];
        let stage1 = unpc_block(residuals, empty, 31, chanbits, 0, budget)?;
        Ok(unpc_block(
            &stage1, coefs, order, chanbits, denshift, budget,
        )?)
    }
}

/// `unmix16`: reconstruct `(left, right)` from `(u, v)`. `mixres == 0` is
/// the conventional (non-matrixed) case; otherwise the reversible
/// generalised mid-side transform.
fn unmix(u: i32, v: i32, mixbits: u32, mixres: i32) -> (i32, i32) {
    if mixres == 0 {
        (u, v)
    } else {
        let l = u
            .wrapping_add(v)
            .wrapping_sub((mixres.wrapping_mul(v)) >> mixbits);
        let r = l.wrapping_sub(v);
        (l, r)
    }
}

/// `mix16`: the write-side mirror of [`unmix`], deriving `(u, v)` from real
/// `(left, right)` samples. Algebraically inverted from `unmix`'s own two
/// equations (`l = u + v - ((mixres * v) >> mixbits)`, then `r = l - v`):
/// solving for `v` gives `v = l - r` directly (exact, no rounding -- this
/// *is* the difference channel), and substituting back gives
/// `u = r + ((mixres * v) >> mixbits)`. Plugging these back into `unmix`
/// cancels the shifted-product term exactly regardless of which way it
/// rounds, since both directions compute it from the identical `v` --
/// verified by `tests::mix_round_trips_through_unmix_for_a_grid_of_l_r_mixres_pairs`,
/// which round-trips a grid of `(l, r, mixbits, mixres)` combinations
/// through both functions.
///
/// `mixres == 0` is `unmix`'s own pass-through special case, not this
/// general formula evaluated at `mixres == 0` (which would compute
/// something else entirely, `u = r, v = l - r`) -- see [`choose_stereo_mix`]
/// for why a real encoder chooses between exactly this pass-through and one
/// non-zero `(mixbits, mixres)` pair per frame, not a wider search.
fn mix16(l: i32, r: i32, mixbits: u32, mixres: i32) -> (i32, i32) {
    if mixres == 0 {
        (l, r)
    } else {
        let v = l.wrapping_sub(r);
        let u = r.wrapping_add((mixres.wrapping_mul(v)) >> mixbits);
        (u, v)
    }
}

/// Read one element's compressed-frame header fields shared by `ID_SCE`
/// (`extra_chanbits = 0`) and `ID_CPE` (`extra_chanbits = 1`, per
/// `ALACDecoder::Decode`'s own `+ 1` on the stereo `chanBits` derivation).
struct ElementHeader {
    num_samples: u32,
    escape: bool,
    chan_bits: u32,
}

fn read_element_header(
    r: &mut BitReader<'_>,
    bit_depth: u8,
    frame_length: u32,
    extra_chanbits: u32,
) -> Result<ElementHeader> {
    let _instance_tag = r.get(4);
    let unused = r.get(12);
    if unused != 0 {
        return Err(Error::InvalidData(
            "alac: element header's 12 unused bits are nonzero",
        ));
    }
    let header_nibble = r.get(4);
    let partial_frame = (header_nibble >> 3) & 1;
    let bytes_shifted = (header_nibble >> 1) & 0x3;
    if bytes_shifted == 3 {
        return Err(Error::InvalidData("alac: reserved bytesShifted value"));
    }
    if bytes_shifted != 0 {
        return Err(Error::Unsupported(
            "alac: bytesShifted (wasted-bits) frames are not implemented",
        ));
    }
    let escape = (header_nibble & 1) != 0;
    let num_samples = if partial_frame != 0 {
        (r.get(16) << 16) | r.get(16)
    } else {
        frame_length
    };
    // A real element always carries at least one sample -- `send_packet(None)`
    // is how draining is signalled (see `AlacDecoder`'s own `draining`
    // field), never a packet with an empty element, so `num_samples == 0`
    // means something upstream is wrong, not that this frame is legitimately
    // silent-and-empty. Measured concretely: `vaco-demux-mp4` handing over
    // an `alac` full box's un-stripped 4-byte version+flags prefix in place
    // of the real `ALACSpecificConfig` made `frame_length` read as `0`, and
    // every packet whose `partialFrame` bit relied on that cookie value
    // (i.e. every full-length packet in the file) decoded as a valid-
    // looking, silently empty frame -- 21 of 22 frames in one real fixture,
    // with the CLI exiting 0 having produced about 2.5% of the audio. This
    // turns that whole class of "technically a valid frame, semantically
    // nothing" outcome into a loud, immediate decode error at the one place
    // every such packet passes through, independent of which upstream bug
    // produces it.
    if num_samples == 0 {
        return Err(Error::InvalidData(
            "alac: element declares zero samples -- likely a corrupt cookie (frame_length) or packet header, not a legitimately empty frame",
        ));
    }
    let chan_bits = u32::from(bit_depth)
        .wrapping_sub(bytes_shifted * 8)
        .wrapping_add(extra_chanbits);
    Ok(ElementHeader {
        num_samples,
        escape,
        chan_bits,
    })
}

struct ChannelParams {
    mode: u32,
    den_shift: u32,
    pb_factor: u32,
    order: usize,
    coefs: Vec<i32>,
}

fn read_channel_params(r: &mut BitReader<'_>) -> ChannelParams {
    let h1 = r.get(8);
    let mode = h1 >> 4;
    let den_shift = h1 & 0xf;
    let h2 = r.get(8);
    let pb_factor = h2 >> 5;
    let order = (h2 & 0x1f) as usize;
    let mut coefs = vec![0i32; order];
    for c in &mut coefs {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "r.get(16) is masked to 16 bits already"
        )]
        let raw = r.get(16) as u16;
        *c = i32::from(raw.cast_signed());
    }
    ChannelParams {
        mode,
        den_shift,
        pb_factor,
        order,
        coefs,
    }
}

/// Decode one packet's bytes into an audio [`Frame`], `S16P` when
/// `bit_depth <= 16` and `S32P` otherwise — the same rule
/// `vaco-codec-flac`'s decoder uses to pick its own output format from a
/// stream's actual bit depth, rather than always widening to `S32P`.
///
/// # Why this matters (not just a style choice)
///
/// A decoded sample here is a genuine `chan_bits`-wide value (16 for a
/// 16-bit source), never left-justified into the full width of its
/// container. `vaco-resample`'s own `S32P` narrowing (`convert.rs`) takes
/// the *opposite* convention — it treats `S32P` as always full-scale and
/// narrows to `S16P` with `(x >> 16) as i16` — so a decoder that always
/// declared `S32P` regardless of actual bit depth handed every downstream
/// consumer of that convention a value 65536x too small, which read back
/// as near-silence (small magnitudes shift to 0) interleaved with bursts
/// of `-1` (small negative magnitudes' sign-extended top bits shift to
/// `0xffff`) — exactly the corruption measured end to end via `vaco -i
/// <16-bit-source>.wav -c:a alac out.mkv` followed by `vaco -i out.mkv -f
/// s16le -`, even though `vaco`'s own `-f s32le` decode of the same file
/// (bypassing that narrowing entirely) was byte-exact. Matching `S16P` to
/// an actual 16-bit stream sidesteps the convention mismatch entirely,
/// the same way `vaco-codec-flac`'s decoder already does.
///
/// `sample_rate` comes from the stream's extradata; `layout_hint` names the
/// container's declared layout when it agrees with the packet's own channel
/// count (self-describing per element, so this falls back to plain mono/
/// stereo purely from that count otherwise); `frame_length` is the cookie's
/// nominal samples-per-packet, used only when a packet's `partialFrame` bit
/// is clear (every full-length packet in practice, per `ALACDecoder.cpp`).
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed packet; [`Error::Unsupported`] for
/// `bytesShifted != 0`, more than one element, more than [`MAX_CHANNELS`]
/// channels, or an element type this crate does not decode; whatever
/// [`Budget`] returns if the decoded frame would exceed it.
#[allow(
    clippy::many_single_char_names,
    reason = "r/u/v/a/b/n match ALACDecoder::Decode's own bitstream reader and per-sample U/V channel names"
)]
pub(crate) fn decode(
    bytes: &[u8],
    sample_rate: u32,
    bit_depth: u8,
    frame_length: u32,
    layout_hint: Option<ChannelLayout>,
    budget: &mut Budget,
) -> Result<Frame> {
    let mut r = BitReader::new(bytes);
    let tag = r.get(3);

    let (channels, samples_u, samples_v, num_samples, mix_bits, mix_res) = match tag {
        ID_SCE | ID_LFE => {
            let hdr = read_element_header(&mut r, bit_depth, frame_length, 0)?;
            let n = hdr.num_samples as usize;
            let u = if hdr.escape {
                let mut out = budget.alloc::<i32>(n)?;
                for slot in &mut out {
                    *slot = r.get_signed(hdr.chan_bits.min(32));
                }
                out
            } else {
                // A mono element still carries mixBits/mixRes (expected
                // zero, per ALACDecoder.cpp's own SCE case) even though
                // there is nothing to mix -- skipping these here would
                // desync every field that follows. A real encoder has
                // nothing to mix for one channel, so unlike `ID_CPE`
                // these are read and discarded rather than threaded
                // anywhere -- there is no second channel for them to mean
                // anything about.
                let _mix_bits = r.get(8);
                let _mix_res = r.get(8);
                let (mb, pb, kb) = ag_defaults();
                let ch = read_channel_params(&mut r);
                #[expect(
                    clippy::integer_division,
                    reason = "the reference's own pbFactor/4 scaling"
                )]
                let params = AgParams::new(mb, (pb * ch.pb_factor) / 4, kb);
                let residuals = dyn_decomp(&params, &mut r, n, hdr.chan_bits);
                let mut coefs = ch.coefs;
                predict_channel(
                    &residuals,
                    &mut coefs,
                    ch.order,
                    ch.mode,
                    hdr.chan_bits,
                    ch.den_shift,
                    budget,
                )?
            };
            (1u8, u, Vec::new(), hdr.num_samples, 0u32, 0i32)
        }
        ID_CPE => {
            let hdr = read_element_header(&mut r, bit_depth, frame_length, 1)?;
            let n = hdr.num_samples as usize;
            if hdr.escape {
                // Verbatim samples are exactly `bit_depth` wide, *not*
                // `hdr.chan_bits` (`bit_depth + 1` for `ID_CPE`, per
                // `read_element_header`'s `extra_chanbits = 1`) -- that
                // extra bit exists only to give the predictor path's mid/
                // side sum arithmetic headroom (see `predict_channel`'s
                // `chanbits` parameter), and escape mode does no mixing at
                // all. Verified against the independent `alac` crate
                // oracle in `tests/oracle_alac_crate.rs`'s
                // `stereo_escape_mode_chan_bits_equals_bit_depth_is_accepted_by_the_oracle_decoder`:
                // using `hdr.chan_bits` here desynced every sample after
                // the first, measured end to end as `ffmpeg`'s "invalid
                // element channel count" on a real stereo encode.
                //
                // No mixBits/mixRes field exists in escape mode at all --
                // `u`/`v` below are already literally left/right, so the
                // final `unmix` call must treat this as pass-through
                // (`mixres = 0`) regardless of anything a non-escape
                // sibling packet in the same stream might have used.
                let verbatim_bits = u32::from(bit_depth).min(32);
                let mut u = budget.alloc::<i32>(n)?;
                let mut v = budget.alloc::<i32>(n)?;
                for i in 0..n {
                    let a = r.get_signed(verbatim_bits);
                    let b = r.get_signed(verbatim_bits);
                    if let Some(s) = u.get_mut(i) {
                        *s = a;
                    }
                    if let Some(s) = v.get_mut(i) {
                        *s = b;
                    }
                }
                (2u8, u, v, hdr.num_samples, 0u32, 0i32)
            } else {
                let mix_bits = r.get(8);
                // Sign-extended from the wire's 8-bit two's-complement
                // field, the same `get(n) as uN -> cast_signed()` pattern
                // `read_channel_params` already uses for the 16-bit
                // coefficients just below -- a real encoder's `mixres` is
                // a genuine signed weight (this crate's own encoder now
                // writes `1`, but the field is not unsigned in general;
                // ffmpeg's own ALAC muxer measured non-zero values here on
                // real stereo content).
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "r.get(8) is masked to 8 bits already"
                )]
                let mix_res_raw = r.get(8) as u8;
                let mix_res = i32::from(mix_res_raw.cast_signed());
                let (mb, pb, kb) = ag_defaults();
                let chu = read_channel_params(&mut r);
                let chv = read_channel_params(&mut r);
                #[expect(
                    clippy::integer_division,
                    reason = "the reference's own pbFactor/4 scaling"
                )]
                let params_u = AgParams::new(mb, (pb * chu.pb_factor) / 4, kb);
                let res_u = dyn_decomp(&params_u, &mut r, n, hdr.chan_bits);
                let mut coefs_u = chu.coefs;
                let u = predict_channel(
                    &res_u,
                    &mut coefs_u,
                    chu.order,
                    chu.mode,
                    hdr.chan_bits,
                    chu.den_shift,
                    budget,
                )?;
                #[expect(
                    clippy::integer_division,
                    reason = "the reference's own pbFactor/4 scaling"
                )]
                let params_v = AgParams::new(mb, (pb * chv.pb_factor) / 4, kb);
                let res_v = dyn_decomp(&params_v, &mut r, n, hdr.chan_bits);
                let mut coefs_v = chv.coefs;
                let v = predict_channel(
                    &res_v,
                    &mut coefs_v,
                    chv.order,
                    chv.mode,
                    hdr.chan_bits,
                    chv.den_shift,
                    budget,
                )?;
                (2u8, u, v, hdr.num_samples, mix_bits, mix_res)
            }
        }
        ID_END => return Err(Error::InvalidData("alac: packet has no audio elements")),
        _ => return Err(Error::Unsupported("alac: element type is not implemented")),
    };
    let _ = r.get(3).eq(&ID_END); // best-effort: consume a trailing END tag if present, ignore otherwise

    let layout = match (channels, layout_hint) {
        (1, Some(h)) if h.channels == 1 => h,
        (2, Some(h)) if h.channels == 2 => h,
        (1, _) => ChannelLayout::MONO,
        (2, _) => ChannelLayout::STEREO,
        _ => {
            return Err(Error::Unsupported(
                "alac: more than 2 channels is not implemented",
            ));
        }
    };

    let out_fmt = if bit_depth <= 16 {
        SampleFmt::S16P
    } else {
        SampleFmt::S32P
    };
    let write_sample: fn(&mut [u8], usize, i32) = if out_fmt == SampleFmt::S16P {
        write_sample_s16
    } else {
        write_sample_s32
    };
    let mut frame = Frame::alloc_audio(budget, out_fmt, layout, num_samples, sample_rate)?;
    let FrameData::Audio { ref mut planes, .. } = frame.data else {
        return Err(Error::Unsupported("alac: allocated frame was not audio"));
    };
    if channels == 1 {
        if let Some(plane) = planes.first_mut() {
            let stride = plane.stride;
            let row = plane.data.make_mut();
            if let Some(row) = row.get_mut(..stride) {
                for (i, &s) in samples_u.iter().enumerate() {
                    write_sample(row, i, s);
                }
            }
        }
    } else {
        // `mix_bits`/`mix_res` are the real header's own values now
        // (previously hardcoded to `0, 0` here regardless of what the
        // packet declared -- a real, if previously invisible, bug: this
        // crate's own encoder never wrote anything else before the
        // stereo-decorrelation work that made this matter, but a real
        // ffmpeg-produced ALAC file using mid/side stereo would have
        // decoded with the wrong channel values, silently, the whole
        // time). `predict_channel` already reconstructed U and V; only
        // the final unmix is left.
        if let Some((left_plane, right_plane)) = planes
            .split_first_mut()
            .map(|(l, rest)| (l, rest.first_mut()))
        {
            let l_stride = left_plane.stride;
            let l_row = left_plane.data.make_mut();
            let mut left_buf = vec![0u8; l_stride];
            let mut right_buf = vec![0u8; l_stride];
            for i in 0..num_samples as usize {
                let u = samples_u.get(i).copied().unwrap_or(0);
                let v = samples_v.get(i).copied().unwrap_or(0);
                let (l, rr) = unmix(u, v, mix_bits, mix_res);
                write_sample(&mut left_buf, i, l);
                write_sample(&mut right_buf, i, rr);
            }
            if let Some(dst) = l_row.get_mut(..l_stride) {
                dst.copy_from_slice(&left_buf);
            }
            if let Some(right_plane) = right_plane {
                let r_stride = right_plane.stride;
                if let Some(dst) = right_plane.data.make_mut().get_mut(..r_stride) {
                    dst.copy_from_slice(right_buf.get(..r_stride).unwrap_or(&right_buf));
                }
            }
        }
    }
    Ok(frame)
}

/// Defaults this crate uses for `(mb, pb, kb)` when a caller has none from a
/// real `ALACSpecificConfig` in hand — kept as one place so decode and
/// encode agree, and identical to the reference's own `MB0`/`PB0`/`KB0`.
///
/// Real interop note: decode should really take these three from the
/// stream's cookie (a real encoder is free to choose different values),
/// but every `ffmpeg`-produced file measured while building this module
/// used exactly these defaults, and threading `(mb, pb, kb)` through from
/// `cookie.rs` into every call site is a real, separate plumbing change —
/// noted rather than done under this pass's time budget.
const fn ag_defaults() -> (u32, u32, u32) {
    (crate::rice::MB0, crate::rice::PB0, crate::rice::KB0)
}

/// One candidate `(mixbits, mixres)` a stereo encode may choose, plus the
/// `(u, v)` channel pair [`mix16`] derives from the real `(left, right)`
/// samples under it.
struct StereoCandidate {
    mixbits: u32,
    mixres: i32,
    u: Vec<i32>,
    v: Vec<i32>,
}

/// The exact bit cost [`dyn_comp`] would spend on `samples` alone (a fresh,
/// zero-seeded [`pc_block`] pass plus the adaptive-Rice coder), measured by
/// actually running both into a scratch [`BitWriter`] and discarding the
/// bits -- not a proxy like "sum of absolute residuals", because a proxy
/// can rank two candidates differently than the Rice coder actually would
/// (its cost is not linear in residual magnitude near the adaptive mean's
/// escape threshold). The cost this exists to compare is real bits, so it
/// is measured as real bits.
fn channel_bit_cost(
    samples: &[i32],
    order: usize,
    chan_bits: u32,
    budget: &mut Budget,
) -> Result<u64> {
    let mut coefs = vec![0i32; order];
    let residuals = pc_block(samples, &mut coefs, order, chan_bits, DENSHIFT);
    let (mb, pb, kb) = ag_defaults();
    let params = AgParams::new(mb, pb, kb);
    let capacity_hint = samples.len().saturating_mul(4).saturating_add(64);
    let mut scratch = BitWriter::with_capacity(budget, capacity_hint)?;
    dyn_comp(&params, &mut scratch, &residuals, chan_bits);
    Ok(scratch.bit_len())
}

/// Chooses this frame's stereo transform: conventional pass-through
/// (`mixbits = mixres = 0`, `u = left, v = right`) versus true mid/side
/// (`mixbits = 1, mixres = 1`, per [`mix16`]'s own doc for the derivation),
/// by actually encoding both and keeping whichever is smaller.
///
/// This compares exactly those two candidates, not a continuous search
/// across every `(mixbits, mixres)` pair the format allows (real ALAC
/// encoders typically estimate an optimal `mixres` directly from the
/// channels' cross-correlation rather than search at all). Two candidates
/// still capture the dominant, measured effect: highly-correlated stereo
/// content (two similar or identical channels) compresses far better as
/// mid/side, since the side channel `l - r` collapses toward zero, while
/// already-decorrelated content (independent channels, hard-panned
/// mixes) has nothing to gain and a finer per-frame weight search would
/// only chase a smaller second-order effect this crate's own encoder does
/// not otherwise rate-distortion-optimize for (see [`PREDICTOR_ORDER`]'s
/// doc on the same scope decision for the predictor order/denshift).
fn choose_stereo_mix(
    samp_l: &[i32],
    samp_r: &[i32],
    order: usize,
    chan_bits: u32,
    budget: &mut Budget,
) -> Result<StereoCandidate> {
    let pass_through = StereoCandidate {
        mixbits: 0,
        mixres: 0,
        u: samp_l.to_vec(),
        v: samp_r.to_vec(),
    };
    let n = samp_l.len();
    let mut mid_u = vec![0i32; n];
    let mut mid_v = vec![0i32; n];
    for i in 0..n {
        let l = samp_l.get(i).copied().unwrap_or(0);
        let r = samp_r.get(i).copied().unwrap_or(0);
        let (u, v) = mix16(l, r, 1, 1);
        if let Some(slot) = mid_u.get_mut(i) {
            *slot = u;
        }
        if let Some(slot) = mid_v.get_mut(i) {
            *slot = v;
        }
    }
    let mid_side = StereoCandidate {
        mixbits: 1,
        mixres: 1,
        u: mid_u,
        v: mid_v,
    };

    let pass_through_bits = channel_bit_cost(&pass_through.u, order, chan_bits, budget)?
        .saturating_add(channel_bit_cost(&pass_through.v, order, chan_bits, budget)?);
    let mid_side_bits = channel_bit_cost(&mid_side.u, order, chan_bits, budget)?
        .saturating_add(channel_bit_cost(&mid_side.v, order, chan_bits, budget)?);

    Ok(if mid_side_bits < pass_through_bits {
        mid_side
    } else {
        pass_through
    })
}

/// The nominal predictor order this encoder always writes, before either
/// [`pc_block`] or a real decoder's `unpc_block` clamp it down for a short
/// frame (see [`encode`]'s doc). A fixed, unsearched choice -- not the
/// result of a rate-distortion search across candidate orders the way
/// Apple's own encoder makes one -- but not an arbitrary guess either:
/// measured on a real 1s/48kHz mono WAV, encoded size against every
/// `(order, denshift)` pair in `{4, 8, 10, 12, 14, 16, 20, 24, 30} x {6, 7,
/// 8, 9, 12, 14}` bottomed out around `order = 12`, `denshift = 8` for both
/// mono and stereo fixtures -- see [`DENSHIFT`]'s doc for why denshift
/// matters this much for a purely-adaptive (zero-seeded, no offline LPC
/// estimate) predictor.
const PREDICTOR_ORDER: usize = 12;

/// The fixed-point shift the adaptive predictor's coefficients and sum use
/// (`denShift` in the element header). This crate's predictor always
/// starts every block's coefficients at zero (see [`encode`]'s doc) and
/// lets the same sign-sign LMS adaptation `unpc_block` implements converge
/// them from there -- which is exactly why this value, unlike a from-
/// scratch LPC quantizer's, is not free to pick generously: a coefficient
/// only ever moves by one step per sample (see `unpc_block`'s `del0`
/// adaptation loop), so a larger `denshift` doesn't just reduce rounding
/// noise, it makes each `±1` step represent a *smaller* real change in the
/// prediction, and convergence within one packet's few thousand samples
/// measurably suffers -- `denshift = 12` roughly doubled encoded size over
/// `denshift = 8` on real content ([`PREDICTOR_ORDER`]'s doc has the full
/// sweep). `8` was the smallest value tried that still won consistently
/// across both the mono and stereo fixture.
const DENSHIFT: u32 = 8;

/// `pbFactor` occupies the top 3 bits of the byte `numU`/`numV` (bottom 5
/// bits) shares. This encoder needs `pbFactor = 4` so the decoder's `(pb *
/// pbFactor) / 4` recovers `pb` exactly — `pbFactor = 0` would zero the
/// adaptive-mean update entirely (see `rice.rs`'s `dyn_decomp`/`dyn_comp`),
/// which is legal bitstream but a much worse (still correct) code, not
/// what "this crate's own defaults" should mean.
const PB_FACTOR_NUM_BYTE: u32 = 4 << 5;

/// Encode one audio [`Frame`] (`S16P` or `S32P`, mono or stereo) to a real,
/// spec-legal ALAC packet using the rice+predictor path: a fixed-order
/// (see [`PREDICTOR_ORDER`]), zero-seeded adaptive linear predictor
/// ([`pc_block`], the write-side mirror of `unpc_block`'s own backward-
/// adaptive sign-sign LMS coefficient update) followed by adaptive-Rice
/// entropy coding ([`dyn_comp`]).
///
/// # Why not `order == 0` (verbatim-as-residual) or `escapeFlag = 1`
/// (element-level verbatim) anymore
///
/// An earlier version of this function always chose `numU = numV = 0` --
/// self-consistent against this crate's own decoder (which explicitly
/// special-cases `order == 0` as a pass-through, per `predictor.rs`'s doc)
/// but never checked against any other decoder. Verified end to end via
/// `vaco -i in.wav -c:a alac out.mkv` followed by `ffmpeg -i out.mkv -f
/// null -`: `ffmpeg`'s own ALAC decoder rejected every single packet
/// ("Decoding error: Invalid data found when processing input", 100% error
/// rate), and a second, independent decoder (the `alac` crate, this
/// crate's own oracle in `tests/oracle_alac_crate.rs`) panicked outright
/// ("attempt to subtract with overflow") on the exact same bytes. The next
/// version switched to `escapeFlag = 1` (verbatim samples, no predictor or
/// Rice coding at all) to restore interoperability first -- verified
/// bit-exact against both oracles, but the same size as uncompressed PCM,
/// since a "lossless compressor" that never compresses is only a waypoint.
///
/// This version restores real compression by implementing the predictor
/// properly instead of dropping it: every candidate order/mode Apple's own
/// encoder can choose is spec-legal, `order == 0` included, but shipping
/// only the one choice this crate's own decoder happened to special-case
/// as a trivial identity, without checking it against anything else, is
/// what actually broke interop last time -- the fix is a correct
/// implementation of the general case, not a permanent retreat to the one
/// mode with no compression to get wrong. `tests/oracle_alac_crate.rs`'s
/// `real_encoder_output_is_accepted_by_the_oracle_and_compresses_*` tests
/// verify the predictor path bit-exact against the independent `alac`
/// crate oracle and confirm the output is meaningfully smaller than PCM;
/// `escapeFlag = 1` remains real, spec-legal bitstream [`decode`] must
/// still read (a real encoder may choose it, and did in this crate's own
/// recent past), it is simply no longer what this function writes.
///
/// # Errors
///
/// [`Error::Unsupported`] for any other sample format or channel count;
/// whatever [`Budget`] returns if the output allocation would exceed it.
pub(crate) fn encode(frame: &Frame, budget: &mut Budget) -> Result<Vec<u8>> {
    let FrameData::Audio {
        format,
        samples,
        layout,
        ..
    } = &frame.data
    else {
        return Err(Error::Unsupported("alac: encoder needs an audio frame"));
    };
    let channels = layout.channels;
    if channels == 0 || channels > u32::from(MAX_CHANNELS) {
        return Err(Error::Unsupported(
            "alac: encoder supports mono or stereo input only",
        ));
    }
    let bytes = bytes_per_sample(*format)?;
    let bit_depth = if bytes == 2 { 16u32 } else { 32u32 };
    let num_samples = *samples;

    let plane0 = frame
        .plane(0)
        .and_then(|p| p.row(0).map(<[u8]>::to_vec))
        .ok_or(Error::Unsupported("alac: encoder needs plane 0"))?;
    let plane1 = if channels == 2 {
        Some(
            frame
                .plane(1)
                .and_then(|p| p.row(0).map(<[u8]>::to_vec))
                .ok_or(Error::Unsupported("alac: encoder needs plane 1 for stereo"))?,
        )
    } else {
        None
    };

    let capacity_hint = (num_samples as usize)
        .saturating_mul(channels as usize)
        .saturating_mul(bytes)
        .saturating_add(64);
    let mut w = BitWriter::with_capacity(budget, capacity_hint)?;
    let order = PREDICTOR_ORDER.min(MAX_ORDER);
    let (mb, pb, kb) = ag_defaults();

    // `numU`/`numV` share their byte with `pbFactor` (top 3 bits) -- see
    // `PB_FACTOR_NUM_BYTE`'s own doc below for why `pbFactor` must be 4,
    // not 0, for this crate's chosen `(mb, pb, kb)` to mean what they say.
    let mode_denshift_byte = DENSHIFT & 0xf; // modeU/modeV = 0
    #[expect(
        clippy::cast_possible_truncation,
        reason = "PREDICTOR_ORDER is 8, well within the 5-bit order field"
    )]
    let pbfactor_order_byte = PB_FACTOR_NUM_BYTE | (order as u32 & 0x1f);

    if channels == 1 {
        w.put(3, ID_SCE);
        w.put(4, 0); // instance tag
        w.put(12, 0); // unused
        w.put(4, 1 << 3); // partialFrame=1, bytesShifted=0, escape=0
        w.put(32, num_samples);
        w.put(8, 0); // mixBits
        w.put(8, 0); // mixRes
        w.put(8, mode_denshift_byte);
        w.put(8, pbfactor_order_byte);
        for _ in 0..order {
            w.put(16, 0); // initial coefficients: this predictor always starts a block at zero, see `encode`'s doc
        }
        let chan_bits = bit_depth;
        let samp: Vec<i32> = (0..num_samples as usize)
            .map(|i| read_sample(&plane0, i, bytes))
            .collect();
        let mut coefs = vec![0i32; order];
        let residuals = pc_block(&samp, &mut coefs, order, chan_bits, DENSHIFT);
        let params = AgParams::new(mb, pb, kb);
        dyn_comp(&params, &mut w, &residuals, chan_bits);
        w.put(3, ID_END);
        w.align_zero();
        return Ok(w.finish());
    }

    // Deliberately `bit_depth + 1`, *not* `bit_depth`: `ID_CPE`'s
    // `extra_chanbits = 1` headroom is for this predictor path's mid/side
    // sum arithmetic (`decode`'s own `read_element_header` always adds it
    // for `ID_CPE`, escape or not) -- the escape-mode's own doc on
    // `decode`'s escape branch covers why *that* mode is the one exception.
    let chan_bits = bit_depth + 1;
    let Some(plane1) = plane1.as_ref() else {
        return Err(Error::Unsupported("alac: encoder needs plane 1 for stereo"));
    };
    let samp_l: Vec<i32> = (0..num_samples as usize)
        .map(|i| read_sample(&plane0, i, bytes))
        .collect();
    let samp_r: Vec<i32> = (0..num_samples as usize)
        .map(|i| read_sample(plane1, i, bytes))
        .collect();
    // Chosen once, up front, because `mixBits`/`mixRes` are part of the
    // element header written *before* either channel's coefficients or
    // residuals -- see `choose_stereo_mix`'s own doc for what the two
    // candidates are and why only two.
    let chosen = choose_stereo_mix(&samp_l, &samp_r, order, chan_bits, budget)?;

    w.put(3, ID_CPE);
    w.put(4, 0);
    w.put(12, 0);
    w.put(4, 1 << 3); // partialFrame=1, bytesShifted=0, escape=0
    w.put(32, num_samples);
    w.put(8, chosen.mixbits);
    #[expect(
        clippy::cast_sign_loss,
        reason = "mixres is a small signed value (0 or ±1 here); the 8-bit field is a byte-wise two's-complement slot per the reference, matching decode's own i32 round-trip through the same width"
    )]
    w.put(8, chosen.mixres as u32);
    w.put(8, mode_denshift_byte);
    w.put(8, pbfactor_order_byte);
    for _ in 0..order {
        w.put(16, 0);
    }
    w.put(8, mode_denshift_byte);
    w.put(8, pbfactor_order_byte);
    for _ in 0..order {
        w.put(16, 0);
    }
    let mut coefs_u = vec![0i32; order];
    let residuals_u = pc_block(&chosen.u, &mut coefs_u, order, chan_bits, DENSHIFT);
    let params_u = AgParams::new(mb, pb, kb);
    dyn_comp(&params_u, &mut w, &residuals_u, chan_bits);
    let mut coefs_v = vec![0i32; order];
    let residuals_v = pc_block(&chosen.v, &mut coefs_v, order, chan_bits, DENSHIFT);
    let params_v = AgParams::new(mb, pb, kb);
    dyn_comp(&params_v, &mut w, &residuals_v, chan_bits);
    w.put(3, ID_END);
    w.align_zero();
    Ok(w.finish())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::permissive())
    }

    #[test]
    fn mix_round_trips_through_unmix_for_a_grid_of_l_r_mixres_pairs() {
        let samples: [i32; 7] = [0, 1, -1, 32767, -32768, 12345, -6789];
        for &l in &samples {
            for &r in &samples {
                for mixbits in [0u32, 1, 2, 4] {
                    for mixres in [0i32, 1, -1, 2, -2, 4, -4] {
                        let (u, v) = mix16(l, r, mixbits, mixres);
                        let (l2, r2) = unmix(u, v, mixbits, mixres);
                        assert_eq!(
                            (l2, r2),
                            (l, r),
                            "l={l} r={r} mixbits={mixbits} mixres={mixres}"
                        );
                    }
                }
            }
        }
    }

    fn mono_frame(samples: &[i16]) -> Frame {
        let mut b = budget();
        let mut frame = Frame::alloc_audio(
            &mut b,
            SampleFmt::S16P,
            ChannelLayout::MONO,
            samples.len() as u32,
            44100,
        )
        .unwrap();
        {
            let mut plane = frame.plane_mut(0).unwrap();
            let row = plane.row_mut(0).unwrap();
            for (i, &s) in samples.iter().enumerate() {
                row[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
            }
        }
        frame
    }

    fn stereo_frame(left: &[i16], right: &[i16]) -> Frame {
        let mut b = budget();
        let mut frame = Frame::alloc_audio(
            &mut b,
            SampleFmt::S16P,
            ChannelLayout::STEREO,
            left.len() as u32,
            44100,
        )
        .unwrap();
        {
            let mut plane = frame.plane_mut(0).unwrap();
            let row = plane.row_mut(0).unwrap();
            for (i, &s) in left.iter().enumerate() {
                row[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
            }
        }
        {
            let mut plane = frame.plane_mut(1).unwrap();
            let row = plane.row_mut(0).unwrap();
            for (i, &s) in right.iter().enumerate() {
                row[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
            }
        }
        frame
    }

    #[test]
    fn mono_round_trips() {
        let samples: Vec<i16> = (0..300).map(|i| (((i * 37) % 251) - 125) as i16).collect();
        let frame = mono_frame(&samples);
        let mut b = budget();
        let bytes = encode(&frame, &mut b).unwrap();
        let mut b2 = budget();
        let decoded = decode(&bytes, 44100, 16, 4096, Some(ChannelLayout::MONO), &mut b2).unwrap();
        let FrameData::Audio {
            planes, samples: n, ..
        } = decoded.data
        else {
            unreachable!("audio")
        };
        assert_eq!(n, samples.len() as u32);
        // 16-bit source: `decode` now matches `S16P` to the actual bit
        // depth (see `decode`'s doc), so the plane is 2 bytes per sample,
        // not the old always-`S32P` 4.
        let row = planes[0].data.as_slice();
        for (i, &s) in samples.iter().enumerate() {
            let got = i16::from_le_bytes(row[i * 2..i * 2 + 2].try_into().unwrap());
            assert_eq!(got, s, "sample {i}");
        }
    }

    #[test]
    fn stereo_round_trips() {
        let left: Vec<i16> = (0..300).map(|i| (((i * 37) % 251) - 125) as i16).collect();
        let right: Vec<i16> = (0..300).map(|i| (((i * 59) % 199) - 99) as i16).collect();
        let frame = stereo_frame(&left, &right);
        let mut b = budget();
        let bytes = encode(&frame, &mut b).unwrap();
        let mut b2 = budget();
        let decoded = decode(
            &bytes,
            44100,
            16,
            4096,
            Some(ChannelLayout::STEREO),
            &mut b2,
        )
        .unwrap();
        let FrameData::Audio {
            planes, samples: n, ..
        } = decoded.data
        else {
            unreachable!("audio")
        };
        assert_eq!(n, left.len() as u32);
        let lrow = planes[0].data.as_slice();
        let rrow = planes[1].data.as_slice();
        for i in 0..left.len() {
            let gl = i16::from_le_bytes(lrow[i * 2..i * 2 + 2].try_into().unwrap());
            let gr = i16::from_le_bytes(rrow[i * 2..i * 2 + 2].try_into().unwrap());
            assert_eq!(gl, left[i], "left {i}");
            assert_eq!(gr, right[i], "right {i}");
        }
    }

    #[test]
    fn silence_round_trips() {
        let samples = vec![0i16; 4096];
        let frame = mono_frame(&samples);
        let mut b = budget();
        let bytes = encode(&frame, &mut b).unwrap();
        let mut b2 = budget();
        let decoded = decode(&bytes, 44100, 16, 4096, Some(ChannelLayout::MONO), &mut b2).unwrap();
        let FrameData::Audio { samples: n, .. } = decoded.data else {
            unreachable!("audio")
        };
        assert_eq!(n, 4096);
    }

    #[test]
    fn full_scale_extremes_round_trip() {
        let samples: Vec<i16> = (0..64)
            .map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN })
            .collect();
        let frame = mono_frame(&samples);
        let mut b = budget();
        let bytes = encode(&frame, &mut b).unwrap();
        let mut b2 = budget();
        let decoded = decode(&bytes, 44100, 16, 4096, Some(ChannelLayout::MONO), &mut b2).unwrap();
        let FrameData::Audio { planes, .. } = decoded.data else {
            unreachable!("audio")
        };
        let row = planes[0].data.as_slice();
        for (i, &s) in samples.iter().enumerate() {
            let got = i16::from_le_bytes(row[i * 2..i * 2 + 2].try_into().unwrap());
            assert_eq!(got, s, "sample {i}");
        }
    }

    /// A non-partial element (`partialFrame = 0`) with a zero `frame_length`
    /// (the cookie-derived sample count for exactly this case) must be a
    /// loud decode error, not a silent zero-sample frame that lets the CLI
    /// exit 0 having decoded nothing. Regression for the real bug: `vaco-
    /// demux-mp4` handed over an `alac` full box's un-stripped 4-byte
    /// version+flags prefix as if it were the `ALACSpecificConfig` itself,
    /// so `frame_length` read as `0` -- every full-length packet in a real
    /// `ffmpeg`-produced `.m4a` (21 of 22 frames) decoded as a "valid",
    /// silently empty frame, and only the file's one genuinely
    /// `partialFrame`-tagged (explicit-count) packet, its short final
    /// frame, carried any audio at all: about 2.5% of the file, `vaco`
    /// exiting 0. Fixed at the extraction site (`vaco-demux-mp4`'s
    /// `codec_parameters`) and, independently, here: whatever upstream
    /// mistake produces a zero-sample non-partial element, `decode` itself
    /// must refuse it rather than silently accept it as legitimate.
    #[test]
    fn zero_frame_length_on_a_non_partial_element_is_a_loud_error_not_zero_samples() {
        let mut b = Budget::new(Limits::permissive());
        let mut w = BitWriter::with_capacity(&mut b, 64).unwrap();
        w.put(3, ID_SCE);
        w.put(4, 0); // instance tag
        w.put(12, 0); // unused
        w.put(4, 0); // partialFrame=0, bytesShifted=0, escape=0 -- relies on frame_length
        w.put(8, 0); // mixBits
        w.put(8, 0); // mixRes
        w.put(8, 0); // modeU=0, denShiftU=0
        w.put(8, 0); // pbFactorU=0, numU=0
        let bytes = w.finish();

        let mut b2 = Budget::new(Limits::permissive());
        // frame_length = 0: exactly what a mis-extracted cookie handed to
        // `set_extradata` would produce.
        let result = decode(&bytes, 44100, 16, 0, Some(ChannelLayout::MONO), &mut b2);
        assert!(
            matches!(result, Err(Error::InvalidData(_))),
            "expected InvalidData, got {result:?}"
        );
    }
}
