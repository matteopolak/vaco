//! Layer III decode: side information, bit reservoir, Huffman decoding,
//! requantisation, MS stereo, alias reduction, the IMDCT and the shared
//! synthesis filterbank.
//!
//! # Known gap: short blocks (`block_type == 2`)
//!
//! Side information for a `block_type == 2` granule (pure short or mixed)
//! parses correctly, but its scalefactor and Huffman payload is skipped
//! rather than decoded: this decoder does not implement the short-block
//! scalefactor layout (band-major, window-minor over 12 bands × 3 windows)
//! or the per-window 12-point IMDCT, so a short-block granule is emitted as
//! silence rather than its actual audio. This is a real, measured gap on
//! transient material (drums, attacks) — see this crate's
//! `docs/codec/vaco-codec-mpegaudio.md` for how often it triggers on the
//! verification fixtures. `block_type` 0 (normal), 1 (start) and 3 (stop)
//! all use the same 36-point long transform and are decoded fully, only
//! differing in which window shape is applied.
//!
//! Every granule resynchronises to its side-info-declared `part2_3_length`
//! after decoding (`Vaco-Spec-Ref: iso-11172-3` §2.4.1.7's own description of
//! `part2_3_length` as exactly this: "this value can be used to calculate
//! the beginning of the main information for each granule"), so a mistake or
//! omission decoding one granule's payload cannot desynchronise any other
//! granule, channel or frame.

use vaco_bitstream::{BitReader, Mark};
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_format_mpegaudio::{ChannelMode, MpegAudioHeader};
use vaco_frame::Frame;
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::huffman::{decode_big_value, decode_count1};
use crate::synthesis::Synthesis;
use crate::tables::{ALIAS_CI, PRETAB, SCALEFAC_COMPRESS};

const LINES: usize = 576;
const SUBBANDS: usize = 32;
const LINES_PER_SUBBAND: usize = 18;
const GRANULES: usize = 2;

/// Bytes of bit-reservoir history retained across packets: comfortably more
/// than the largest backward reference `main_data_begin` (9 bits, so at most
/// 511 bytes) plus one maximum-size Layer III frame.
const RESERVOIR_CAP: usize = 4096;

#[derive(Debug)]
pub(crate) struct Layer3State {
    /// The second half of each subband's last IMDCT output, carried into the
    /// next block's overlap-add. One per channel.
    overlap: Vec<[[f32; LINES_PER_SUBBAND]; SUBBANDS]>,
    /// Raw main-data bytes, oldest first; `send_packet` appends this frame's
    /// slot and decode reads backward from the end via `main_data_begin`.
    reservoir: Vec<u8>,
}

impl Layer3State {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            overlap: vec![[[0.0; LINES_PER_SUBBAND]; SUBBANDS]; channels.max(1)],
            reservoir: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct GranuleInfo {
    part2_3_length: u16,
    big_values: u16,
    global_gain: u8,
    scalefac_compress: u8,
    block_type: u8,
    table_select: [u8; 3],
    region_count: [u8; 2],
    preflag: bool,
    scalefac_scale: bool,
    count1table_select: bool,
}

struct SideInfo {
    main_data_begin: u16,
    scfsi: [[bool; 4]; 2],
    granules: [[GranuleInfo; 2]; GRANULES],
}

fn parse_side_info(r: &mut BitReader<'_>, channels: usize) -> SideInfo {
    let main_data_begin = r.get(9) as u16;
    let _private_bits = r.get(if channels == 1 { 5 } else { 3 });
    let mut scfsi = [[false; 4]; 2];
    for ch in scfsi.iter_mut().take(channels) {
        for band in ch.iter_mut() {
            *band = r.get(1) == 1;
        }
    }
    let mut granules = [[GranuleInfo::default(); 2]; GRANULES];
    for gr in &mut granules {
        for ch in gr.iter_mut().take(channels) {
            ch.part2_3_length = r.get(12) as u16;
            ch.big_values = r.get(9) as u16;
            ch.global_gain = r.get(8) as u8;
            ch.scalefac_compress = r.get(4) as u8;
            let blocksplit = r.get(1) == 1;
            if blocksplit {
                let block_type = r.get(2) as u8;
                let _switch_point = r.get(1);
                let mut ts = [0u8; 3];
                ts[0] = r.get(5) as u8;
                ts[1] = r.get(5) as u8;
                ts[2] = ts[1];
                ch.table_select = ts;
                for _ in 0..3 {
                    r.get(3); // subblock_gain, unused: short blocks are not transformed
                }
                ch.block_type = block_type;
                ch.region_count = if block_type == 2 { [9, 0] } else { [8, 0] };
            } else {
                for slot in &mut ch.table_select {
                    *slot = r.get(5) as u8;
                }
                ch.region_count = [r.get(4) as u8, r.get(3) as u8];
                ch.block_type = 0;
            }
            ch.preflag = r.get(1) == 1;
            ch.scalefac_scale = r.get(1) == 1;
            ch.count1table_select = r.get(1) == 1;
        }
    }
    SideInfo {
        main_data_begin,
        scfsi,
        granules,
    }
}

const fn scfsi_group(band: usize) -> usize {
    if band < 6 {
        0
    } else if band < 11 {
        1
    } else if band < 16 {
        2
    } else {
        3
    }
}

/// Decode one long-block (`block_type` 0, 1 or 3) granule's scalefactors and
/// Huffman-coded spectral lines into 576 requantised `xr` values. Returns
/// all zeros for `block_type == 2` — see the module doc's short-block gap.
#[allow(clippy::too_many_arguments)]
fn decode_granule(
    r: &mut BitReader<'_>,
    g: &GranuleInfo,
    sfb: &[u16],
    prev_scalefac: Option<&[u8; 21]>,
    scfsi: [bool; 4],
    is_second_granule: bool,
    granule_end_bit: u64,
) -> ([f32; LINES], [u8; 21]) {
    let mut xr = [0.0f32; LINES];
    let mut scalefac = [0u8; 21];
    if g.block_type == 2 {
        return (xr, scalefac);
    }

    let (slen1, slen2) = SCALEFAC_COMPRESS
        .get(usize::from(g.scalefac_compress))
        .copied()
        .unwrap_or((0, 0));
    for (band, slot) in scalefac.iter_mut().enumerate() {
        let bits = if band < 11 { slen1 } else { slen2 };
        *slot = if bits == 0 {
            0
        } else if is_second_granule && scfsi.get(scfsi_group(band)).copied().unwrap_or(false) {
            prev_scalefac.and_then(|p| p.get(band)).copied().unwrap_or(0)
        } else {
            r.get(u32::from(bits)) as u8
        };
    }

    // Huffman decode: three "big values" regions, then the count1 quads.
    let bound = sfb.len().saturating_sub(1).min(21);
    let region0_end = sfb
        .get(usize::from(g.region_count[0]).min(bound))
        .copied()
        .unwrap_or(0) as usize;
    let region1_end = sfb
        .get((usize::from(g.region_count[0]) + usize::from(g.region_count[1])).min(bound))
        .copied()
        .unwrap_or(0) as usize;
    let big_values_end = (usize::from(g.big_values) * 2).min(LINES);

    // `part2_3_length` is the ONLY authoritative bound on how much Huffman
    // data exists: a silent granule can have `big_values == 0` and no
    // count1 data at all, so `r.bit_pos() < granule_end_bit` has to gate
    // both loops alongside the "576 lines" rule — otherwise a
    // short-on-content granule reads real bits belonging to the next
    // channel or granule and manufactures spectral energy that was never
    // transmitted (found by comparing decoded PCM to `ffmpeg`: a silent
    // second channel came out at full scale until this bound was added).
    let mut is = [0i32; LINES];
    let mut i = 0usize;
    while i < big_values_end && r.bit_pos() < granule_end_bit {
        let table = if i < region0_end {
            g.table_select[0]
        } else if i < region1_end {
            g.table_select[1]
        } else {
            g.table_select[2]
        };
        let Some(v) = decode_big_value(r, table) else {
            break;
        };
        if let Some(slot) = is.get_mut(i) {
            *slot = v.x;
        }
        if let Some(slot) = is.get_mut(i + 1) {
            *slot = v.y;
        }
        i += 2;
    }
    while i < LINES && r.bit_pos() < granule_end_bit {
        let Some(quad) = decode_count1(r, u8::from(g.count1table_select)) else {
            break;
        };
        for (offset, &v) in quad.iter().enumerate() {
            if let Some(slot) = is.get_mut(i + offset) {
                *slot = v;
            }
        }
        i += 4;
    }

    // Requantisation: xr[i] = sign(is)*|is|^(4/3) * 2^(0.25*(gain-210)) *
    // 2^(-0.5*(1+scale)*(scalefac[sfb]+preflag*pretab[sfb])). The `210`
    // constant is confirmed empirically, not by citation: ISO/IEC 11172-3's
    // own text names this formula's scaling constant ("The constant 64 in
    // this formula...") but the PDF-to-text extraction this crate's tables
    // were built from lost the formula itself (it was embedded as an
    // image), leaving only that one surrounding sentence. `210` is the
    // value that reproduces `ffmpeg`-decoded PCM on the verification
    // fixtures; see `docs/codec/vaco-codec-mpegaudio.md`.
    let gain_term = 2f64.powf(0.25 * (f64::from(g.global_gain) - 210.0));
    for (band, win) in sfb.windows(2).enumerate().take(21) {
        let &[lo, hi] = win else { continue };
        let scale_exp = -0.5
            * (if g.scalefac_scale { 2.0 } else { 1.0 })
            * (f64::from(scalefac.get(band).copied().unwrap_or(0))
                + f64::from(u8::from(g.preflag)) * f64::from(*PRETAB.get(band).unwrap_or(&0)));
        let band_term = gain_term * 2f64.powf(scale_exp);
        for (idx, &value) in is
            .get(usize::from(lo)..usize::from(hi))
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let Some(dst) = xr.get_mut(usize::from(lo) + idx) else {
                continue;
            };
            if value != 0 {
                let mag = f64::from(value.unsigned_abs()).powf(4.0 / 3.0) * band_term;
                *dst = if value < 0 { -mag as f32 } else { mag as f32 };
            }
        }
    }
    (xr, scalefac)
}

fn apply_alias_reduction(xr: &mut [f32; LINES]) {
    let cs_ca: Vec<(f32, f32)> = ALIAS_CI
        .iter()
        .map(|&ci| {
            let norm = (1.0 + ci * ci).sqrt();
            (1.0 / norm, ci / norm)
        })
        .collect();
    for sb in 0..SUBBANDS - 1 {
        for (k, &(cs, ca)) in cs_ca.iter().enumerate() {
            let ia = sb * LINES_PER_SUBBAND + (LINES_PER_SUBBAND - 1 - k);
            let ib = (sb + 1) * LINES_PER_SUBBAND + k;
            let (Some(&a), Some(&b)) = (xr.get(ia), xr.get(ib)) else {
                continue;
            };
            let new_a = a * cs - b * ca;
            let new_b = b * cs + a * ca;
            if let Some(slot) = xr.get_mut(ia) {
                *slot = new_a;
            }
            if let Some(slot) = xr.get_mut(ib) {
                *slot = new_b;
            }
        }
    }
}

/// The windowed IMDCT for one subband's 18 spectral lines: `Vaco-Spec-Ref:
/// iso-11172-3` §2.4.3.4.9.3, one of four window shapes selected by
/// `block_type` applied to the raw 36-sample IMDCT output.
fn windowed_imdct(coeffs: &[f64; 18], block_type: u8) -> [f32; 36] {
    let raw = vaco_tx::reference::imdct(coeffs);
    let mut out = [0.0f32; 36];
    for (i, slot) in out.iter_mut().enumerate() {
        let w = window_value(block_type, i);
        *slot = (raw.get(i).copied().unwrap_or(0.0) * w) as f32;
    }
    out
}

fn window_value(block_type: u8, i: usize) -> f64 {
    use std::f64::consts::PI;
    let n = i as f64;
    match block_type {
        1 => {
            if i < 18 {
                (PI / 36.0 * (n + 0.5)).sin()
            } else if i < 24 {
                1.0
            } else if i < 30 {
                (PI / 12.0 * (n - 18.0 + 0.5)).sin()
            } else {
                0.0
            }
        }
        3 => {
            if i < 6 {
                0.0
            } else if i < 12 {
                (PI / 12.0 * (n - 6.0 + 0.5)).sin()
            } else if i < 18 {
                1.0
            } else {
                (PI / 36.0 * (n + 0.5)).sin()
            }
        }
        _ => (PI / 36.0 * (n + 0.5)).sin(),
    }
}

pub(crate) fn decode(
    header: MpegAudioHeader,
    body: &[u8],
    state: &mut Layer3State,
    synth: &mut [Synthesis],
    budget: &mut Budget,
) -> Result<Frame> {
    if header.version.is_low_sample_rate() {
        return Err(Error::Unsupported(
            "mpegaudio: MPEG-2/2.5 (low sample rate) Layer III scalefactor layout is not implemented",
        ));
    }
    let channels = usize::from(header.channels());
    if synth.len() < channels || state.overlap.len() < channels {
        return Err(Error::Unsupported("mpegaudio: missing per-channel decode state"));
    }
    let side_info_len = header
        .side_info_len()
        .ok_or(Error::InvalidData("mpegaudio: not a Layer III header"))?;
    let side_bytes = body
        .get(..side_info_len)
        .ok_or(Error::InvalidData("mpegaudio: packet shorter than its side info"))?;
    let this_frame_main_data = body.get(side_info_len..).unwrap_or(&[]);

    let mut side_reader = BitReader::new(side_bytes);
    let side = parse_side_info(&mut side_reader, channels);

    state.reservoir.extend_from_slice(this_frame_main_data);
    let begin = usize::from(side.main_data_begin);
    let start = state
        .reservoir
        .len()
        .saturating_sub(this_frame_main_data.len())
        .saturating_sub(begin);
    let window = state.reservoir.get(start..).unwrap_or(&[]);

    let sfb: &[u16] = sfb_long_for(header.sample_rate_hz());

    let mut r = BitReader::new(window);
    let frame_start: Mark = r.mark();
    let mut cumulative_bits: u64 = 0;
    let mut prev_scalefac: Vec<Option<[u8; 21]>> = vec![None; channels];

    let mut pcm: Vec<Vec<f32>> = vec![Vec::new(); channels];
    let ms_active = matches!(header.channel_mode, ChannelMode::JointStereo)
        && (header.mode_extension & 0b10) != 0
        && channels == 2;

    for (gr_idx, gr) in side.granules.iter().enumerate() {
        let mut xr_ch: Vec<[f32; LINES]> = Vec::new();
        for (ch, info) in gr.iter().enumerate().take(channels) {
            let scfsi = side.scfsi.get(ch).copied().unwrap_or([false; 4]);
            let granule_end_bit = cumulative_bits + u64::from(info.part2_3_length);
            let (xr, scalefac) = decode_granule(
                &mut r,
                info,
                sfb,
                prev_scalefac.get(ch).and_then(|s| s.as_ref()),
                scfsi,
                gr_idx == 1,
                granule_end_bit,
            );
            if gr_idx == 0 && let Some(slot) = prev_scalefac.get_mut(ch) {
                *slot = Some(scalefac);
            }
            xr_ch.push(xr);
            cumulative_bits += u64::from(info.part2_3_length);
            r.restore(frame_start);
            r.skip_long(cumulative_bits);
        }

        if ms_active && let [m, s] = &mut xr_ch[..] {
            for i in 0..LINES {
                let (Some(&mi), Some(&si)) = (m.get(i), s.get(i)) else {
                    continue;
                };
                let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
                if let Some(dst) = m.get_mut(i) {
                    *dst = (mi + si) * inv_sqrt2;
                }
                if let Some(dst) = s.get_mut(i) {
                    *dst = (mi - si) * inv_sqrt2;
                }
            }
        }

        for (ch, xr) in xr_ch.iter_mut().enumerate() {
            let block_type = gr.get(ch).map_or(0, |g| g.block_type);
            if block_type != 2 {
                apply_alias_reduction(xr);
            }
            let mut time_slot = [[0.0f32; SUBBANDS]; LINES_PER_SUBBAND];
            for sb in 0..SUBBANDS {
                let mut coeffs = [0.0f64; 18];
                for (k, c) in coeffs.iter_mut().enumerate() {
                    *c = f64::from(xr.get(sb * LINES_PER_SUBBAND + k).copied().unwrap_or(0.0));
                }
                let windowed = windowed_imdct(&coeffs, block_type);
                let prev_overlap: [f32; LINES_PER_SUBBAND] = state
                    .overlap
                    .get(ch)
                    .and_then(|o| o.get(sb))
                    .copied()
                    .unwrap_or([0.0; LINES_PER_SUBBAND]);
                for (k, &prev) in prev_overlap.iter().enumerate() {
                    let mut value = windowed.get(k).copied().unwrap_or(0.0) + prev;
                    if sb % 2 == 1 && k % 2 == 1 {
                        value = -value;
                    }
                    if let Some(slot) = time_slot.get_mut(k).and_then(|row| row.get_mut(sb)) {
                        *slot = value;
                    }
                }
                if let Some(o) = state.overlap.get_mut(ch).and_then(|o| o.get_mut(sb)) {
                    for (k, slot) in o.iter_mut().enumerate() {
                        *slot = windowed.get(LINES_PER_SUBBAND + k).copied().unwrap_or(0.0);
                    }
                }
            }
            if let Some(synth_ch) = synth.get_mut(ch) {
                for slot in &time_slot {
                    let block = synth_ch.synth_block(slot);
                    if let Some(out) = pcm.get_mut(ch) {
                        out.extend_from_slice(&block);
                    }
                }
            }
        }
    }

    let layout = ChannelLayout::default_for(channels as u32)
        .ok_or(Error::Unsupported("mpegaudio: unsupported channel count"))?;
    let total_samples = pcm.first().map_or(0, Vec::len);
    let mut frame = Frame::alloc_audio(
        budget,
        SampleFmt::F32P,
        layout,
        total_samples as u32,
        header.sample_rate_hz(),
    )?;
    for (ch, samples) in pcm.iter().enumerate() {
        let mut plane = frame
            .plane_mut(ch)
            .ok_or(Error::Unsupported("mpegaudio: missing output plane"))?;
        let row = plane
            .row_mut(0)
            .ok_or(Error::Unsupported("mpegaudio: output plane too short"))?;
        for (dst, &sample) in row.chunks_exact_mut(4).zip(samples.iter()) {
            dst.copy_from_slice(&sample.to_le_bytes());
        }
    }

    if state.reservoir.len() > RESERVOIR_CAP {
        let excess = state.reservoir.len() - RESERVOIR_CAP;
        state.reservoir.drain(..excess);
    }

    Ok(frame)
}

fn sfb_long_for(sample_rate_hz: u32) -> &'static [u16] {
    use crate::tables::{SFB_LONG_32000, SFB_LONG_44100, SFB_LONG_48000};
    match sample_rate_hz {
        32000 => &SFB_LONG_32000,
        48000 => &SFB_LONG_48000,
        _ => &SFB_LONG_44100,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod imdct_tests {
    use super::*;

    /// Feed the same constant spectral coefficient through several
    /// consecutive long-block IMDCT + overlap-add steps for one subband, at
    /// zero frequency (a DC-like input). After the first (transient) block,
    /// the overlap-added output for a *constant* input should itself settle
    /// to a constant value, not oscillate — a property that does not depend
    /// on knowing the filterbank's exact numbers, only on overlap-add being
    /// wired correctly.
    #[test]
    fn constant_coefficient_settles_to_a_constant_after_overlap_add() {
        let coeffs = [1.0f64; 18];
        let mut overlap = [0.0f32; LINES_PER_SUBBAND];
        let mut blocks = Vec::new();
        for _ in 0..6 {
            let windowed = windowed_imdct(&coeffs, 0);
            let mut out = [0.0f32; LINES_PER_SUBBAND];
            for k in 0..LINES_PER_SUBBAND {
                out[k] = windowed[k] + overlap[k];
            }
            overlap.copy_from_slice(&windowed[LINES_PER_SUBBAND..2 * LINES_PER_SUBBAND]);
            blocks.push(out);
        }
        let last = blocks[5];
        let prev = blocks[4];
        for k in 0..LINES_PER_SUBBAND {
            assert!(
                (last[k] - prev[k]).abs() < 1e-3,
                "block 5 vs 4 differ at {k}: {} vs {}",
                last[k],
                prev[k]
            );
        }
    }
}


#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod frequency_placement_tests {
    use super::*;
    use crate::synthesis::Synthesis;

    /// Exciting one spectral line, repeated across many granules with no
    /// bitstream involved at all, should produce PCM whose dominant
    /// frequency matches that line's known centre frequency
    /// (`sample_rate/2 / 576` per line). This isolates the
    /// subband-splitting + IMDCT + overlap-add + synthesis half of Layer
    /// III's pipeline from side-info parsing and Huffman decoding — the
    /// two halves an end-to-end real-file comparison cannot tell apart.
    #[test]
    #[allow(clippy::integer_division, reason = "test code: halving a sample count for a DFT window")]
    fn a_single_spectral_line_produces_its_own_frequency() {
        let sample_rate = 44100.0;
        let line = 144usize; // subband 8, k = 0
        let expected_hz = (sample_rate / 2.0 / 576.0) * line as f64;

        let mut synth = Synthesis::new();
        let mut overlap = [[0.0f32; LINES_PER_SUBBAND]; SUBBANDS];
        let mut pcm = Vec::new();
        for _ in 0..40 {
            let mut xr = [0.0f32; LINES];
            xr[line] = 1000.0;
            let mut time_slot = [[0.0f32; SUBBANDS]; LINES_PER_SUBBAND];
            for sb in 0..SUBBANDS {
                let mut coeffs = [0.0f64; 18];
                for (k, c) in coeffs.iter_mut().enumerate() {
                    *c = f64::from(xr[sb * LINES_PER_SUBBAND + k]);
                }
                let windowed = windowed_imdct(&coeffs, 0);
                for k in 0..LINES_PER_SUBBAND {
                    let mut value = windowed[k] + overlap[sb][k];
                    if sb % 2 == 1 && k % 2 == 1 {
                        value = -value;
                    }
                    time_slot[k][sb] = value;
                }
                overlap[sb].copy_from_slice(&windowed[LINES_PER_SUBBAND..2 * LINES_PER_SUBBAND]);
            }
            for slot in &time_slot {
                pcm.extend_from_slice(&synth.synth_block(slot));
            }
        }

        // Naive DFT peak search over the settled (later) portion — small
        // enough sample count that an O(n^2) DFT is fine for a test.
        let settled = &pcm[pcm.len() / 2..];
        let n = settled.len();
        let mut best_bin = 0usize;
        let mut best_mag = 0.0f64;
        for k in 1..n / 2 {
            let mut re = 0.0f64;
            let mut im = 0.0f64;
            for (j, &x) in settled.iter().enumerate() {
                let theta = -2.0 * std::f64::consts::PI * (k as f64) * (j as f64) / (n as f64);
                re += f64::from(x) * theta.cos();
                im += f64::from(x) * theta.sin();
            }
            let mag = (re * re + im * im).sqrt();
            if mag > best_mag {
                best_mag = mag;
                best_bin = k;
            }
        }
        let got_hz = f64::from(best_bin as u32) * sample_rate / n as f64;
        assert!(
            (got_hz - expected_hz).abs() < 200.0,
            "expected ~{expected_hz} Hz, got peak at {got_hz} Hz"
        );
    }
}
