//! Layer II decode: per-subband bit allocation from one of the four (or, at
//! a low sample rate, one) tables, 3-granule scalefactor transmission
//! patterns, grouped-triple degrouping, and the shared synthesis filterbank.
//!
//! `Vaco-Spec-Ref: iso-11172-3` §2.4.2.4/§2.4.3.3: 3 granules per frame (36
//! subband samples per subband) share one bit-allocation table, and the
//! scalefactor "transmission pattern" (0-3, read from 2 bits per subband)
//! selects which of up to three 6-bit scalefactors apply to which of the
//! three granules within that subband.

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_format_mpegaudio::MpegAudioHeader;
use vaco_frame::Frame;
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::bitalloc::{layer2_dequant_grouped, layer2_dequant_ungrouped, layer2_table};
use crate::synthesis::Synthesis;
use crate::tables::{LAYER12_SCALEFACTORS, QUANT_CLASSES};

const SUBBANDS: usize = 32;
const GRANULES: usize = 12;
/// Output time slots per subband per frame: 12 granules × 3 samples each.
const TIME_SLOTS: usize = GRANULES * 3;
const SCALEFACTOR_GROUPS: usize = 3;

/// Which of up to three scalefactors (one per group of 4 granules) applies
/// to each of the 3 groups, keyed by the 2-bit transmission pattern.
/// `Vaco-Spec-Ref: iso-11172-3` Table 3-B.5: pattern 0 sends 3 independent
/// scalefactors, patterns 1-3 share one or two across groups.
const SCFSI_PATTERN: [[u8; SCALEFACTOR_GROUPS]; 4] = [
    [0, 1, 2], // all three transmitted
    [0, 0, 1], // groups 0-1 share, group 2 its own
    [0, 0, 0], // all three share one
    [0, 1, 1], // group 0 its own, groups 1-2 share
];

#[allow(
    clippy::integer_division,
    reason = "bitrate-per-channel and the 4-granule scalefactor group index are both spec-defined floor divisions, not rounding bugs"
)]
pub(crate) fn decode(
    header: MpegAudioHeader,
    body: &[u8],
    synth: &mut [Synthesis],
    budget: &mut Budget,
) -> Result<Frame> {
    let channels = usize::from(header.channels());
    if synth.len() < channels {
        return Err(Error::Unsupported("mpegaudio: missing per-channel synthesis state"));
    }
    let bitrate_per_channel = header
        .bitrate_kbps()
        .map(|kbps| u32::from(kbps) / channels.max(1) as u32);
    let table = layer2_table(header.version, header.sample_rate_hz(), bitrate_per_channel);

    let mut r = BitReader::new(body);
    let sb_count = table.len().min(SUBBANDS);

    // 1. Bit allocation: one `nbal`-bit index per subband per channel,
    // resolving to a `nlevels` entry (or "not allocated") in that subband's
    // table row.
    let mut alloc_idx = [[0u8; SUBBANDS]; 2];
    for sb in 0..sb_count {
        let nbal = table.get(sb).map_or(0, |row| row.nbal);
        for ch in alloc_idx.iter_mut().take(channels) {
            if let Some(slot) = ch.get_mut(sb) {
                *slot = if nbal == 0 { 0 } else { r.get(u32::from(nbal)) as u8 };
            }
        }
    }

    // 2. `scfsi`: 2-bit transmission pattern per subband per channel that
    // actually got a nonzero allocation.
    let mut pattern = [[0u8; SUBBANDS]; 2];
    for sb in 0..sb_count {
        for ch in 0..channels {
            if get2(&alloc_idx, ch, sb) != 0 {
                let v = r.get(2) as u8;
                set2(&mut pattern, ch, sb, v);
            }
        }
    }

    // 3. Scalefactors: up to 3 per subband per channel, per the pattern.
    let mut scalefactor = [[[1.0f32; SCALEFACTOR_GROUPS]; SUBBANDS]; 2];
    for sb in 0..sb_count {
        for ch in 0..channels {
            if get2(&alloc_idx, ch, sb) == 0 {
                continue;
            }
            let transmitted = distinct_count(get2(&pattern, ch, sb));
            let mut values = [0.0f32; 3];
            for v in values.iter_mut().take(transmitted) {
                let idx = usize::from(r.get(6) as u8);
                *v = LAYER12_SCALEFACTORS.get(idx).copied().unwrap_or(0.0);
            }
            let map = SCFSI_PATTERN
                .get(usize::from(get2(&pattern, ch, sb)).min(3))
                .copied()
                .unwrap_or([0, 1, 2]);
            let Some(groups) = scalefactor.get_mut(ch).and_then(|c| c.get_mut(sb)) else {
                continue;
            };
            for (group, slot) in groups.iter_mut().enumerate() {
                let which = map.get(group).copied().unwrap_or(0);
                *slot = values
                    .get(usize::from(which).min(transmitted.saturating_sub(1)))
                    .copied()
                    .unwrap_or(0.0);
            }
        }
    }

    // 4. `Vaco-Spec-Ref: iso-11172-3` §2.4.1.6's own pseudocode is
    // granule-major, not subband-major: `for (gr=0; gr<12; gr++) for (sb...)
    // for (ch...) { ... }` — the bitstream interleaves one sample (or one
    // grouped codeword) per allocated subband for granule 0, then the same
    // for granule 1, and so on, NOT all 12 granules of subband 0 followed
    // by subband 1. An earlier version of this loop nested subband outside
    // granule, which reads the right total number of bits (so the frame
    // still ends in the right place) but from the wrong positions from the
    // second allocated subband onward — found by comparing decoded PCM to
    // `ffmpeg`, which stayed near-uncorrelated with the reference even
    // after the granule *count* (`TIME_SLOTS`, above) was fixed.
    let mut sample = vec![[[0.0f32; SUBBANDS]; 2]; TIME_SLOTS];
    for gr in 0..GRANULES {
        let base = 3 * gr;
        for sb in 0..sb_count {
            let row_nlevels = table.get(sb).map(|row| row.nlevels);
            for ch in 0..channels {
                let idx = get2(&alloc_idx, ch, sb);
                if idx == 0 {
                    continue;
                }
                let Some(&nlevels) = row_nlevels.and_then(|n| n.get(usize::from(idx) - 1)) else {
                    continue;
                };
                let class = QUANT_CLASSES.iter().find(|c| c.nlevels == nlevels);
                let bits = class.map_or(0, |c| u32::from(c.bits_per_codeword));
                let grouped = class.is_some_and(|c| c.grouped);
                let scf_group = (gr / 4).min(SCALEFACTOR_GROUPS - 1);
                let factor = scalefactor
                    .get(ch)
                    .and_then(|c| c.get(sb))
                    .and_then(|g| g.get(scf_group))
                    .copied()
                    .unwrap_or(0.0);
                if grouped {
                    let combined = r.get(bits);
                    let triple = layer2_dequant_grouped(combined, nlevels);
                    for (offset, &v) in triple.iter().enumerate() {
                        if let Some(slot) =
                            sample.get_mut(base + offset).and_then(|g| g.get_mut(ch)).and_then(|c| c.get_mut(sb))
                        {
                            *slot = v * factor;
                        }
                    }
                } else {
                    for offset in 0..3 {
                        let code = r.get(bits);
                        let v = layer2_dequant_ungrouped(code, bits, nlevels);
                        if let Some(slot) =
                            sample.get_mut(base + offset).and_then(|g| g.get_mut(ch)).and_then(|c| c.get_mut(sb))
                        {
                            *slot = v * factor;
                        }
                    }
                }
            }
        }
    }

    let samples_per_channel = SUBBANDS * TIME_SLOTS;
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); channels];
    for granule_sample in &sample {
        for (ch, synth_ch) in synth.iter_mut().enumerate().take(channels) {
            let Some(sb_values) = granule_sample.get(ch) else {
                continue;
            };
            let block = synth_ch.synth_block(sb_values);
            if let Some(dst) = out.get_mut(ch) {
                dst.extend_from_slice(&block);
            }
        }
    }

    let layout = ChannelLayout::default_for(channels as u32)
        .ok_or(Error::Unsupported("mpegaudio: unsupported channel count"))?;
    let mut frame = Frame::alloc_audio(
        budget,
        SampleFmt::F32P,
        layout,
        samples_per_channel as u32,
        header.sample_rate_hz(),
    )?;
    for (ch, samples) in out.iter().enumerate() {
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
    Ok(frame)
}

/// How many distinct scalefactors `pattern` transmits: 3 for pattern 0, 2
/// for patterns 1 and 3, 1 for pattern 2.
const fn distinct_count(pattern: u8) -> usize {
    match pattern {
        0 => 3,
        2 => 1,
        _ => 2,
    }
}

fn get2<T: Copy + Default>(a: &[[T; SUBBANDS]; 2], ch: usize, sb: usize) -> T {
    a.get(ch).and_then(|c| c.get(sb)).copied().unwrap_or_default()
}

fn set2<T>(a: &mut [[T; SUBBANDS]; 2], ch: usize, sb: usize, value: T) {
    if let Some(slot) = a.get_mut(ch).and_then(|c| c.get_mut(sb)) {
        *slot = value;
    }
}

