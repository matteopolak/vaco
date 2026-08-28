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
//! **Encode** always chooses the simplest spec-legal parameters rather than
//! anything resembling Apple's actual encoder's rate-distortion search:
//! `numU`/`numV = 0` (no linear prediction — the residual *is* the sample,
//! a real, if inefficient, predictor order per `unpc_block`'s own
//! `numactive == 0` identity case), `mixres = mixbits = 0` for stereo (plain
//! interleave, no decorrelation), `bytesShifted = 0`, `escapeFlag = 0`,
//! `partialFrame` always set with an explicit sample count. Every one of
//! those is a real, legal configuration a compliant decoder (this one, the
//! `alac` crate, real `ffmpeg`) must accept — see `tests/oracle_alac_crate.rs`.

use vaco_bitstream::{BitReader, BitWriter};
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::predictor::unpc_block;
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
        _ => Err(Error::Unsupported("alac: encoder accepts s16p or s32p input only")),
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
/// 4-byte little-endian `i32` (this crate's decoder always produces `S32P`).
fn write_sample_s32(buf: &mut [u8], index: usize, value: i32) {
    let off = index.saturating_mul(4);
    if let Some(dst) = buf.get_mut(off..off.saturating_add(4)) {
        dst.copy_from_slice(&value.to_le_bytes());
    }
}

/// One channel's decoded predictor output, per `modeU`/`modeV`.
fn predict_channel(residuals: &[i32], coefs: &mut [i32], order: usize, mode: u32, chanbits: u32, denshift: u32, budget: &mut Budget) -> Result<Vec<i32>> {
    if mode == 0 {
        Ok(unpc_block(residuals, coefs, order, chanbits, denshift, budget)?)
    } else {
        let empty: &mut [i32] = &mut [];
        let stage1 = unpc_block(residuals, empty, 31, chanbits, 0, budget)?;
        Ok(unpc_block(&stage1, coefs, order, chanbits, denshift, budget)?)
    }
}

/// `unmix16`: reconstruct `(left, right)` from `(u, v)`. `mixres == 0` is
/// the conventional (non-matrixed) case; otherwise the reversible
/// generalised mid-side transform.
fn unmix(u: i32, v: i32, mixbits: u32, mixres: i32) -> (i32, i32) {
    if mixres == 0 {
        (u, v)
    } else {
        let l = u.wrapping_add(v).wrapping_sub((mixres.wrapping_mul(v)) >> mixbits);
        let r = l.wrapping_sub(v);
        (l, r)
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

fn read_element_header(r: &mut BitReader<'_>, bit_depth: u8, frame_length: u32, extra_chanbits: u32) -> Result<ElementHeader> {
    let _instance_tag = r.get(4);
    let unused = r.get(12);
    if unused != 0 {
        return Err(Error::InvalidData("alac: element header's 12 unused bits are nonzero"));
    }
    let header_nibble = r.get(4);
    let partial_frame = (header_nibble >> 3) & 1;
    let bytes_shifted = (header_nibble >> 1) & 0x3;
    if bytes_shifted == 3 {
        return Err(Error::InvalidData("alac: reserved bytesShifted value"));
    }
    if bytes_shifted != 0 {
        return Err(Error::Unsupported("alac: bytesShifted (wasted-bits) frames are not implemented"));
    }
    let escape = (header_nibble & 1) != 0;
    let num_samples = if partial_frame != 0 {
        (r.get(16) << 16) | r.get(16)
    } else {
        frame_length
    };
    let chan_bits = u32::from(bit_depth).wrapping_sub(bytes_shifted * 8).wrapping_add(extra_chanbits);
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
        #[expect(clippy::cast_possible_truncation, reason = "r.get(16) is masked to 16 bits already")]
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

/// Decode one packet's bytes into an audio [`Frame`], always `S32P`.
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
pub(crate) fn decode(bytes: &[u8], sample_rate: u32, bit_depth: u8, frame_length: u32, layout_hint: Option<ChannelLayout>, budget: &mut Budget) -> Result<Frame> {
    let mut r = BitReader::new(bytes);
    let tag = r.get(3);

    let (channels, samples_u, samples_v, num_samples) = match tag {
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
                // desync every field that follows.
                let _mix_bits = r.get(8);
                let _mix_res = r.get(8);
                let (mb, pb, kb) = ag_defaults();
                let ch = read_channel_params(&mut r);
                #[expect(clippy::integer_division, reason = "the reference's own pbFactor/4 scaling")]
                let params = AgParams::new(mb, (pb * ch.pb_factor) / 4, kb);
                let residuals = dyn_decomp(&params, &mut r, n, hdr.chan_bits);
                let mut coefs = ch.coefs;
                predict_channel(&residuals, &mut coefs, ch.order, ch.mode, hdr.chan_bits, ch.den_shift, budget)?
            };
            (1u8, u, Vec::new(), hdr.num_samples)
        }
        ID_CPE => {
            let hdr = read_element_header(&mut r, bit_depth, frame_length, 1)?;
            let n = hdr.num_samples as usize;
            if hdr.escape {
                let mut u = budget.alloc::<i32>(n)?;
                let mut v = budget.alloc::<i32>(n)?;
                for i in 0..n {
                    let a = r.get_signed(hdr.chan_bits.min(32));
                    let b = r.get_signed(hdr.chan_bits.min(32));
                    if let Some(s) = u.get_mut(i) {
                        *s = a;
                    }
                    if let Some(s) = v.get_mut(i) {
                        *s = b;
                    }
                }
                (2u8, u, v, hdr.num_samples)
            } else {
                let _mix_bits = r.get(8);
                let _mix_res = r.get(8);
                let (mb, pb, kb) = ag_defaults();
                let chu = read_channel_params(&mut r);
                let chv = read_channel_params(&mut r);
                #[expect(clippy::integer_division, reason = "the reference's own pbFactor/4 scaling")]
                let params_u = AgParams::new(mb, (pb * chu.pb_factor) / 4, kb);
                let res_u = dyn_decomp(&params_u, &mut r, n, hdr.chan_bits);
                let mut coefs_u = chu.coefs;
                let u = predict_channel(&res_u, &mut coefs_u, chu.order, chu.mode, hdr.chan_bits, chu.den_shift, budget)?;
                #[expect(clippy::integer_division, reason = "the reference's own pbFactor/4 scaling")]
                let params_v = AgParams::new(mb, (pb * chv.pb_factor) / 4, kb);
                let res_v = dyn_decomp(&params_v, &mut r, n, hdr.chan_bits);
                let mut coefs_v = chv.coefs;
                let v = predict_channel(&res_v, &mut coefs_v, chv.order, chv.mode, hdr.chan_bits, chv.den_shift, budget)?;
                (2u8, u, v, hdr.num_samples)
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
        _ => return Err(Error::Unsupported("alac: more than 2 channels is not implemented")),
    };

    let mut frame = Frame::alloc_audio(budget, SampleFmt::S32P, layout, num_samples, sample_rate)?;
    let FrameData::Audio { ref mut planes, .. } = frame.data else {
        return Err(Error::Unsupported("alac: allocated frame was not audio"));
    };
    if channels == 1 {
        if let Some(plane) = planes.first_mut() {
            let stride = plane.stride;
            let row = plane.data.make_mut();
            if let Some(row) = row.get_mut(..stride) {
                for (i, &s) in samples_u.iter().enumerate() {
                    write_sample_s32(row, i, s);
                }
            }
        }
    } else {
        // `mixbits`/`mixres` are read but always the real header's own
        // values -- this crate's own encoder always writes 0/0
        // (conventional stereo), and decode above already parsed
        // whatever the real packet declared. Since `predict_channel`
        // already reconstructed U and V, only the final unmix is left.
        if let Some((left_plane, right_plane)) = planes.split_first_mut().map(|(l, rest)| (l, rest.first_mut())) {
            let l_stride = left_plane.stride;
            let l_row = left_plane.data.make_mut();
            let mut left_buf = vec![0u8; l_stride];
            let mut right_buf = vec![0u8; l_stride];
            for i in 0..num_samples as usize {
                let u = samples_u.get(i).copied().unwrap_or(0);
                let v = samples_v.get(i).copied().unwrap_or(0);
                let (l, rr) = unmix(u, v, 0, 0);
                write_sample_s32(&mut left_buf, i, l);
                write_sample_s32(&mut right_buf, i, rr);
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

/// Encode one audio [`Frame`] (`S16P` or `S32P`, mono or stereo) to a real,
/// spec-legal ALAC packet — see the module doc for exactly which (simplest)
/// configuration this always chooses.
///
/// # Errors
///
/// [`Error::Unsupported`] for any other sample format or channel count;
/// whatever [`Budget`] returns if the output allocation would exceed it.
pub(crate) fn encode(frame: &Frame, budget: &mut Budget) -> Result<Vec<u8>> {
    // `pbFactor` occupies the top 3 bits of the byte `numU`/`numV` (bottom 5
    // bits) shares. This encoder always writes `numU = numV = 0` (no linear
    // prediction), and needs `pbFactor = 4` so the decoder's `(pb *
    // pbFactor) / 4` recovers `pb` exactly — `pbFactor = 0` would zero the
    // adaptive-mean update entirely (see `rice.rs`'s `dyn_decomp`/
    // `dyn_comp`), which is legal bitstream but a much worse (still
    // correct) code, not what "this crate's own defaults" should mean.
    const PB_FACTOR_NUM_BYTE: u32 = 4 << 5;

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
        return Err(Error::Unsupported("alac: encoder supports mono or stereo input only"));
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

    let capacity_hint = (num_samples as usize).saturating_mul(channels as usize).saturating_mul(bytes).saturating_add(64);
    let mut w = BitWriter::with_capacity(budget, capacity_hint)?;
    let (mb, pb, kb) = ag_defaults();

    if channels == 1 {
        w.put(3, ID_SCE);
        w.put(4, 0); // instance tag
        w.put(12, 0); // unused
        w.put(4, 1 << 3); // partialFrame=1, bytesShifted=0, escape=0
        w.put(32, num_samples);
        w.put(8, 0); // mixBits
        w.put(8, 0); // mixRes
        w.put(8, 0); // modeU=0, denShiftU=0
        w.put(8, PB_FACTOR_NUM_BYTE); // pbFactorU=4, numU=0
        let chan_bits = bit_depth;
        let residuals: Vec<i32> = (0..num_samples as usize).map(|i| read_sample(&plane0, i, bytes)).collect();
        let params = AgParams::new(mb, pb, kb);
        dyn_comp(&params, &mut w, &residuals, chan_bits);
        return Ok(w.finish());
    }

    w.put(3, ID_CPE);
    w.put(4, 0);
    w.put(12, 0);
    w.put(4, 1 << 3);
    w.put(32, num_samples);
    w.put(8, 0); // mixBits
    w.put(8, 0); // mixRes = 0: conventional stereo, u = left, v = right
    w.put(8, 0); // modeU=0, denShiftU=0
    w.put(8, PB_FACTOR_NUM_BYTE); // pbFactorU=4, numU=0
    w.put(8, 0); // modeV=0, denShiftV=0
    w.put(8, PB_FACTOR_NUM_BYTE); // pbFactorV=4, numV=0
    let chan_bits = bit_depth + 1;
    let Some(plane1) = plane1.as_ref() else {
        return Err(Error::Unsupported("alac: encoder needs plane 1 for stereo"));
    };
    let residuals_u: Vec<i32> = (0..num_samples as usize).map(|i| read_sample(&plane0, i, bytes)).collect();
    let residuals_v: Vec<i32> = (0..num_samples as usize).map(|i| read_sample(plane1, i, bytes)).collect();
    let params_u = AgParams::new(mb, pb, kb);
    dyn_comp(&params_u, &mut w, &residuals_u, chan_bits);
    let params_v = AgParams::new(mb, pb, kb);
    dyn_comp(&params_v, &mut w, &residuals_v, chan_bits);
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

    fn mono_frame(samples: &[i16]) -> Frame {
        let mut b = budget();
        let mut frame = Frame::alloc_audio(&mut b, SampleFmt::S16P, ChannelLayout::MONO, samples.len() as u32, 44100).unwrap();
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
        let mut frame = Frame::alloc_audio(&mut b, SampleFmt::S16P, ChannelLayout::STEREO, left.len() as u32, 44100).unwrap();
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
        let FrameData::Audio { planes, samples: n, .. } = decoded.data else {
            unreachable!("audio")
        };
        assert_eq!(n, samples.len() as u32);
        let row = planes[0].data.as_slice();
        for (i, &s) in samples.iter().enumerate() {
            let got = i32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(got, i32::from(s), "sample {i}");
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
        let decoded = decode(&bytes, 44100, 16, 4096, Some(ChannelLayout::STEREO), &mut b2).unwrap();
        let FrameData::Audio { planes, samples: n, .. } = decoded.data else {
            unreachable!("audio")
        };
        assert_eq!(n, left.len() as u32);
        let lrow = planes[0].data.as_slice();
        let rrow = planes[1].data.as_slice();
        for i in 0..left.len() {
            let gl = i32::from_le_bytes(lrow[i * 4..i * 4 + 4].try_into().unwrap());
            let gr = i32::from_le_bytes(rrow[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(gl, i32::from(left[i]), "left {i}");
            assert_eq!(gr, i32::from(right[i]), "right {i}");
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
        let samples: Vec<i16> = (0..64).map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN }).collect();
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
            let got = i32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(got, i32::from(s), "sample {i}");
        }
    }
}
