//! Top-level per-frame decode: header, six audio blocks, IMDCT/overlap-add,
//! optional dynamic-range compression.

use vaco_bitstream::BitReader;
use vaco_format_ac3::bsi::Bsi;
use vaco_format_ac3::syncinfo::{self, FrameKind, SyncInfo};

use crate::audblk::{self, BlockState};
use crate::imdct::{self, OverlapState};
use crate::tables::acmod_channel_count;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

/// One decoded AC-3 frame: per-channel PCM (full-bandwidth channels first,
/// in `acmod`'s own order, LFE last if present), plus the header facts a
/// caller needs to know what it is looking at.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub sample_rate: u32,
    pub acmod: u8,
    pub lfeon: bool,
    pub dialnorm: u8,
    /// Per-channel samples, `1536` long for a full classic-AC-3 frame.
    pub channels: Vec<Vec<f32>>,
    pub lfe: Option<Vec<f32>>,
}

/// Decode options a caller controls explicitly: dialnorm/DRC are metadata
/// and gain, not bitstream structure, so applying them is a post-processing
/// step over the same decoded coefficients.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodeOptions {
    /// Apply the transmitted `dynrng` gain word per block. Off by default,
    /// matching a plain PCM comparison against `ffmpeg -i x.ac3 -f s16le -`
    /// with no `-drc_scale` given.
    pub apply_drc: bool,
}

/// State that persists across frames within one stream: block-to-block
/// exponent/bit-allocation carryover and the overlap-add tail per channel.
#[derive(Debug)]
pub struct StreamState {
    block: Option<BlockState>,
    overlap: Vec<OverlapState>,
    lfe_overlap: Option<OverlapState>,
    window: Vec<f32>,
}

impl StreamState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            block: None,
            overlap: Vec::new(),
            lfe_overlap: None,
            window: imdct::kbd_window(512, imdct::AC3_KBD_ALPHA),
        }
    }
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode one classic-AC-3 syncframe (`payload` is exactly one frame, as a
/// demuxer packet delivers).
///
/// # Errors
/// [`DecodeError`] if the header does not parse at all. A structurally valid
/// header with corrupt block data does not error — it produces implausible
/// or silent samples for the affected block, the same degradation a
/// truncated real stream would show.
pub fn decode_frame(
    payload: &[u8],
    state: &mut StreamState,
    opts: &DecodeOptions,
) -> Result<DecodedFrame, DecodeError> {
    let info: SyncInfo = syncinfo::parse(payload).ok_or(DecodeError)?;
    if info.kind != FrameKind::Ac3 {
        return Err(DecodeError);
    }
    let bsi = Bsi::parse(payload, &info).map_err(|_| DecodeError)?;
    let nfchans = acmod_channel_count(bsi.acmod);

    if state.overlap.len() != nfchans {
        state.overlap = (0..nfchans).map(|_| OverlapState::new(256)).collect();
    }
    if bsi.lfeon && state.lfe_overlap.is_none() {
        state.lfe_overlap = Some(OverlapState::new(256));
    }
    let fscod = fscod_for_rate(info.sample_rate);
    let block_state = state
        .block
        .get_or_insert_with(|| BlockState::new(nfchans, bsi.lfeon, bsi.acmod, fscod));
    if block_state.nfchans != nfchans || block_state.lfeon != bsi.lfeon {
        *block_state = BlockState::new(nfchans, bsi.lfeon, bsi.acmod, fscod);
    }

    let mut r = BitReader::new(payload);
    r.skip(bsi.bit_len);

    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); nfchans];
    let mut lfe_out = bsi.lfeon.then(Vec::new);
    let mut last_dynrng = None;

    for _ in 0..6 {
        let block = audblk::decode(&mut r, block_state);
        if block.dynrng.is_some() {
            last_dynrng = block.dynrng;
        }
        let gain = if opts.apply_drc {
            last_dynrng.map_or(1.0, |db| 10f32.powf(db / 20.0))
        } else {
            1.0
        };
        for (ch, coeffs) in block.channels.iter().enumerate() {
            let Some(overlap) = state.overlap.get_mut(ch) else {
                continue;
            };
            let mut padded = coeffs.clone();
            padded.resize(256, 0.0);
            let samples = overlap.push_long(&padded, &state.window);
            if let Some(out_ch) = channels.get_mut(ch) {
                out_ch.extend(samples.into_iter().map(|s| s * gain));
            }
        }
        if let (Some(lfe_coeffs), Some(overlap)) = (&block.lfe, &mut state.lfe_overlap) {
            let mut padded = lfe_coeffs.clone();
            padded.resize(256, 0.0);
            let samples = overlap.push_long(&padded, &state.window);
            if let Some(out) = lfe_out.as_mut() {
                out.extend(samples.into_iter().map(|s| s * gain));
            }
        }
    }

    // §5.3: `auxdata()` and `errorcheck()` are once-per-frame, after all 6
    // blocks — not read here for their content (this crate does not verify
    // the CRC or expose auxiliary data), but consuming them keeps the
    // reader's final position meaningful for anyone checking frame-length
    // alignment as an oracle.
    let auxdatae = r.get_bit() != 0;
    if auxdatae {
        let auxdatal = r.get(14);
        r.skip(auxdatal);
    }
    r.skip(1); // crcrsv
    r.skip(16); // crc2

    Ok(DecodedFrame {
        sample_rate: info.sample_rate,
        acmod: bsi.acmod,
        lfeon: bsi.lfeon,
        dialnorm: bsi.dialnorm,
        channels,
        lfe: lfe_out,
    })
}

const fn fscod_for_rate(rate: u32) -> u8 {
    match rate {
        44100 => 1,
        32000 => 2,
        _ => 0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_silent_frame_never_panics_and_reports_the_right_shape() {
        let mut f = vec![0u8; 768];
        f[0] = 0x0B;
        f[1] = 0x77;
        f[4] = 20; // fscod=0, frmsizecod=20 -> 768 bytes
        f[5] = 8 << 3; // bsid=8
        f[6] = 2 << 5; // acmod=2, stereo
        let mut state = StreamState::new();
        let out = decode_frame(&f, &mut state, &DecodeOptions::default()).unwrap();
        assert_eq!(out.channels.len(), 2);
        assert_eq!(out.sample_rate, 48000);
    }

    #[test]
    fn garbage_input_reports_an_error_rather_than_panicking() {
        let data = vec![0xAAu8; 32];
        let mut state = StreamState::new();
        assert!(decode_frame(&data, &mut state, &DecodeOptions::default()).is_err());
    }
}
