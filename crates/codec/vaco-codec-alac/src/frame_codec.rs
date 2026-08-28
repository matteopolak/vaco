//! The per-packet bitstream: one packet in, one [`Frame`] out (and back).
//!
//! # Provenance
//!
//! This crate's own framing (`provenance/vaco-codec-alac.toml`, id
//! `alac-payload-original`) — see `predictor.rs`'s doc comment for why the
//! compressed payload is not a transcription of Apple's ALAC bitstream. What
//! *is* spec-derived is everything outside this module: the magic cookie
//! (`cookie.rs`) that a real `.m4a`/`.caf` file's container carries, which
//! this crate parses correctly and independently of how the packet payload
//! itself is framed.
//!
//! # Shape
//!
//! Every packet is self-describing: channel count, bit depth and sample
//! count are all in the packet header, so decode needs no extradata to make
//! sense of an individual packet (extradata supplies stream-wide facts a
//! packet has no room for: sample rate, and a channel *layout* beyond plain
//! count — see [`decode`]'s `layout_hint`/`sample_rate` parameters).
//!
//! A packet is:
//! - `channels - 1`: 3 bits
//! - `bit_depth_code`: 2 bits (`0` = 16-bit, `1` = 32-bit)
//! - `num_samples`: 32 bits
//! - `escape`: 1 bit — verbatim samples follow, no prediction or stereo
//!   decorrelation. Chosen by the encoder whenever there are too few samples
//!   for adaptive state to help (`escape_worthwhile` below), which is also
//!   what exercises this path in the round-trip tests.
//! - if `escape`: for each of the `channels` planes, `num_samples` raw
//!   `bit_depth`-bit two's-complement samples, channel-sequential.
//! - else: one 5-bit predictor `order` per logical channel (`channels` of
//!   them), then `num_samples` groups of `channels` adaptive-Rice-coded
//!   residuals, **sample-interleaved** across channels — chosen so decode
//!   can reconstruct straight into the output [`Frame`]'s planes with no
//!   extra allocation (mid/side combine only needs both channels' value at
//!   the same sample index, never the whole array).
//!
//! Stereo uses the standard reversible lifting transform (the same one
//! FLAC's mid-side stereo mode uses): `side = left - right`, `mid = right +
//! (side >> 1)`; recovered exactly as `right = mid - (side >> 1)`, `left =
//! right + side`, because the `side >> 1` term is identical on both sides and
//! cancels regardless of its own rounding.

use vaco_bitstream::{BitReader, BitWriter};
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::predictor::Predictor;
use crate::rice::RiceState;

/// The only channel counts this crate's payload framing supports. Layouts
/// beyond stereo are a documented gap — see the crate's top-level doc.
const MAX_CHANNELS: u8 = 2;
/// Fixed predictor order this crate's encoder always writes. Kept as a
/// transmitted field (not a compile-time constant on the decode side) so a
/// hand-built test packet can exercise `order = 0`.
const ENCODE_ORDER: u32 = 8;

fn bit_depth_code(bit_depth: u8) -> Result<u32> {
    match bit_depth {
        16 => Ok(0),
        32 => Ok(1),
        _ => Err(Error::Unsupported(
            "alac: only 16-bit and 32-bit PCM are implemented",
        )),
    }
}

fn bit_depth_from_code(code: u32) -> Result<u8> {
    match code {
        0 => Ok(16),
        1 => Ok(32),
        _ => Err(Error::Unsupported("alac: reserved bit-depth code")),
    }
}

fn bytes_per_sample(fmt: SampleFmt) -> Result<usize> {
    match fmt {
        SampleFmt::S16P => Ok(2),
        SampleFmt::S32P => Ok(4),
        _ => Err(Error::Unsupported(
            "alac: encoder accepts s16p or s32p input only",
        )),
    }
}

fn read_sample(buf: &[u8], index: usize, bytes: usize) -> i64 {
    let off = index.saturating_mul(bytes);
    match bytes {
        2 => i64::from(
            buf.get(off..off.saturating_add(2))
                .and_then(|s| s.try_into().ok())
                .map_or(0, i16::from_le_bytes),
        ),
        4 => i64::from(
            buf.get(off..off.saturating_add(4))
                .and_then(|s| s.try_into().ok())
                .map_or(0, i32::from_le_bytes),
        ),
        _ => 0,
    }
}

/// Write `value` into plane byte buffer `buf` at sample `index`, as a
/// 4-byte little-endian `i32` (this crate's decoder always produces `S32P`).
fn write_sample_s32(buf: &mut [u8], index: usize, value: i64) {
    let off = index.saturating_mul(4);
    let clamped = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    if let Some(dst) = buf.get_mut(off..off.saturating_add(4)) {
        dst.copy_from_slice(&clamped.to_le_bytes());
    }
}

/// Whether an escape (verbatim) frame is worth choosing: too few samples for
/// the adaptive predictor/Rice state to earn back its own header overhead.
fn escape_worthwhile(num_samples: u32) -> bool {
    num_samples < 2
}

/// Encode one audio [`Frame`] (`S16P` or `S32P`, mono or stereo) to this
/// crate's packet bytes.
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
    let bit_depth = if bytes == 2 { 16u8 } else { 32u8 };
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
    w.put(3, channels.saturating_sub(1));
    w.put(2, bit_depth_code(bit_depth)?);
    w.put(32, num_samples);
    let escape = escape_worthwhile(num_samples);
    w.put(1, u32::from(escape));

    if escape {
        for i in 0..num_samples as usize {
            let l = read_sample(&plane0, i, bytes);
            w.put_signed(u32::from(bit_depth), l as i32);
            if let Some(p1) = &plane1 {
                let r = read_sample(p1, i, bytes);
                w.put_signed(u32::from(bit_depth), r as i32);
            }
        }
        return Ok(w.finish());
    }

    for _ in 0..channels {
        w.put(5, ENCODE_ORDER);
    }
    let mut predictors: Vec<Predictor> = (0..channels)
        .map(|_| Predictor::new(ENCODE_ORDER as usize))
        .collect();
    let mut rice: Vec<RiceState> = (0..channels).map(|_| RiceState::new()).collect();

    for i in 0..num_samples as usize {
        if channels == 2 {
            let Some(p1) = &plane1 else {
                return Err(Error::Unsupported("alac: encoder needs plane 1 for stereo"));
            };
            let left = read_sample(&plane0, i, bytes);
            let right = read_sample(p1, i, bytes);
            let side = left.wrapping_sub(right);
            let mid = right.wrapping_add(side >> 1);
            let logical = [mid, side];
            for ch in 0..2usize {
                let value = *logical.get(ch).unwrap_or(&0);
                let residual = predictors.get_mut(ch).map_or(value, |p| p.residual(value));
                if let Some(rs) = rice.get_mut(ch) {
                    rs.write(&mut w, residual);
                }
            }
        } else {
            let value = read_sample(&plane0, i, bytes);
            let residual = predictors.get_mut(0).map_or(value, |p| p.residual(value));
            if let Some(rs) = rice.get_mut(0) {
                rs.write(&mut w, residual);
            }
        }
    }
    Ok(w.finish())
}

/// Decode one packet's bytes into an audio [`Frame`], always `S32P`.
///
/// `sample_rate` and `layout_hint` come from the stream's extradata (or a
/// caller's default, when none was ever supplied); the packet's own header
/// states the authoritative channel count, and `layout_hint` is used only
/// when it agrees with that count — otherwise this falls back to
/// mono/stereo/unspecified purely from the count, so a packet still decodes
/// correctly even with no extradata at all.
///
/// # Errors
///
/// [`Error::InvalidData`] if the packet is truncated or malformed;
/// [`Error::Unsupported`] for a channel count above [`MAX_CHANNELS`] or a
/// reserved bit-depth code; whatever [`Budget`] returns if the decoded frame
/// would exceed it.
pub(crate) fn decode(
    bytes: &[u8],
    sample_rate: u32,
    layout_hint: Option<ChannelLayout>,
    budget: &mut Budget,
) -> Result<Frame> {
    let mut r = BitReader::new(bytes);
    let channels = r.get(3).saturating_add(1);
    if channels > u32::from(MAX_CHANNELS) {
        return Err(Error::Unsupported(
            "alac: more than 2 channels is not implemented",
        ));
    }
    let bit_depth = bit_depth_from_code(r.get(2))?;
    let num_samples = r.get(32);
    let escape = r.get(1) != 0;

    let layout = match layout_hint {
        Some(l) if l.channels == channels => l,
        _ => match channels {
            1 => ChannelLayout::MONO,
            2 => ChannelLayout::STEREO,
            n => ChannelLayout::unspecified(n),
        },
    };

    let mut frame = Frame::alloc_audio(budget, SampleFmt::S32P, layout, num_samples, sample_rate)?;
    budget.consume_fuel(
        u64::from(num_samples)
            .saturating_mul(u64::from(channels))
            .saturating_add(1),
    )?;

    if escape {
        for i in 0..num_samples as usize {
            for ch in 0..channels as usize {
                let v = i64::from(r.get_signed(u32::from(bit_depth)));
                if let Some(mut plane) = frame.plane_mut(ch)
                    && let Some(row) = plane.row_mut(0)
                {
                    write_sample_s32(row, i, v);
                }
            }
        }
        r.finish()
            .map_err(|_| Error::InvalidData("alac: truncated escape frame"))?;
        return Ok(frame);
    }

    let mut orders = [0usize; MAX_CHANNELS as usize];
    for slot in orders.iter_mut().take(channels as usize) {
        *slot = r.get(5) as usize;
    }
    let mut predictors: Vec<Predictor> = orders
        .iter()
        .copied()
        .take(channels as usize)
        .map(Predictor::new)
        .collect();
    let mut rice: Vec<RiceState> = (0..channels).map(|_| RiceState::new()).collect();

    for i in 0..num_samples as usize {
        if channels == 2 {
            let mid_residual = rice.get_mut(0).map_or(0, |rs| rs.read(&mut r));
            let mid = predictors
                .get_mut(0)
                .map_or(mid_residual, |p| p.reconstruct(mid_residual));
            let side_residual = rice.get_mut(1).map_or(0, |rs| rs.read(&mut r));
            let side = predictors
                .get_mut(1)
                .map_or(side_residual, |p| p.reconstruct(side_residual));
            let right = mid.wrapping_sub(side >> 1);
            let left = right.wrapping_add(side);
            if let Some(mut plane) = frame.plane_mut(0)
                && let Some(row) = plane.row_mut(0)
            {
                write_sample_s32(row, i, left);
            }
            if let Some(mut plane) = frame.plane_mut(1)
                && let Some(row) = plane.row_mut(0)
            {
                write_sample_s32(row, i, right);
            }
        } else {
            let residual = rice.get_mut(0).map_or(0, |rs| rs.read(&mut r));
            let value = predictors
                .get_mut(0)
                .map_or(residual, |p| p.reconstruct(residual));
            if let Some(mut plane) = frame.plane_mut(0)
                && let Some(row) = plane.row_mut(0)
            {
                write_sample_s32(row, i, value);
            }
        }
    }
    r.finish()
        .map_err(|_| Error::InvalidData("alac: truncated or corrupt frame"))?;
    Ok(frame)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn mono_frame(samples: &[i32], fmt: SampleFmt, sample_rate: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            fmt,
            ChannelLayout::MONO,
            samples.len() as u32,
            sample_rate,
        )
        .unwrap();
        let bytes = bytes_per_sample(fmt).unwrap();
        let mut plane = frame.plane_mut(0).unwrap();
        let row = plane.row_mut(0).unwrap();
        for (i, &s) in samples.iter().enumerate() {
            let off = i * bytes;
            if bytes == 2 {
                row[off..off + 2].copy_from_slice(&(s as i16).to_le_bytes());
            } else {
                row[off..off + 4].copy_from_slice(&s.to_le_bytes());
            }
        }
        frame
    }

    fn stereo_frame(left: &[i32], right: &[i32], fmt: SampleFmt, sample_rate: u32) -> Frame {
        assert_eq!(left.len(), right.len());
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            fmt,
            ChannelLayout::STEREO,
            left.len() as u32,
            sample_rate,
        )
        .unwrap();
        let bytes = bytes_per_sample(fmt).unwrap();
        for (ch, data) in [left, right].into_iter().enumerate() {
            let mut plane = frame.plane_mut(ch).unwrap();
            let row = plane.row_mut(0).unwrap();
            for (i, &s) in data.iter().enumerate() {
                let off = i * bytes;
                if bytes == 2 {
                    row[off..off + 2].copy_from_slice(&(s as i16).to_le_bytes());
                } else {
                    row[off..off + 4].copy_from_slice(&s.to_le_bytes());
                }
            }
        }
        frame
    }

    fn plane_samples(frame: &Frame, ch: usize) -> Vec<i32> {
        let plane = frame.plane(ch).unwrap();
        let row = plane.row(0).unwrap();
        row.chunks_exact(4)
            .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn mono_round_trip() {
        let samples: Vec<i32> = (0..1000).map(|i| ((i * 37) % 2001) - 1000).collect();
        let frame = mono_frame(&samples, SampleFmt::S16P, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        let decoded = decode(&bytes, 44100, Some(ChannelLayout::MONO), &mut budget).unwrap();
        assert_eq!(plane_samples(&decoded, 0), samples);
    }

    #[test]
    fn stereo_round_trip() {
        let left: Vec<i32> = (0..800).map(|i| ((i * 13) % 60001) - 30000).collect();
        let right: Vec<i32> = (0..800).map(|i| ((i * 29) % 60001) - 30000).collect();
        let frame = stereo_frame(&left, &right, SampleFmt::S32P, 48000);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        let decoded = decode(&bytes, 48000, Some(ChannelLayout::STEREO), &mut budget).unwrap();
        assert_eq!(plane_samples(&decoded, 0), left);
        assert_eq!(plane_samples(&decoded, 1), right);
    }

    #[test]
    fn silence_round_trips() {
        let samples = vec![0i32; 4096];
        let frame = mono_frame(&samples, SampleFmt::S16P, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        let decoded = decode(&bytes, 44100, Some(ChannelLayout::MONO), &mut budget).unwrap();
        assert_eq!(plane_samples(&decoded, 0), samples);
    }

    #[test]
    fn full_scale_extremes_round_trip() {
        let samples: Vec<i32> = (0..64)
            .map(|i| if i % 2 == 0 { i32::MAX } else { i32::MIN })
            .collect();
        let frame = mono_frame(&samples, SampleFmt::S32P, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        let decoded = decode(&bytes, 44100, Some(ChannelLayout::MONO), &mut budget).unwrap();
        assert_eq!(plane_samples(&decoded, 0), samples);
    }

    #[test]
    fn tiny_frame_uses_the_escape_path() {
        let samples = vec![42i32];
        let frame = mono_frame(&samples, SampleFmt::S16P, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        // channels-1 (3) + bit_depth_code (2) + num_samples (32) => escape
        // bit is the 38th bit written, i.e. bit index 37.
        let escape_bit = (bytes[4] >> (7 - (37 % 8))) & 1;
        assert_eq!(
            escape_bit, 1,
            "a 1-sample frame should choose the escape path"
        );
        let decoded = decode(&bytes, 44100, Some(ChannelLayout::MONO), &mut budget).unwrap();
        assert_eq!(plane_samples(&decoded, 0), samples);
    }

    #[test]
    fn odd_sample_count_round_trips() {
        let samples: Vec<i32> = (0..4097).map(|i| (i % 17) - 8).collect();
        let frame = mono_frame(&samples, SampleFmt::S16P, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        let decoded = decode(&bytes, 44100, Some(ChannelLayout::MONO), &mut budget).unwrap();
        assert_eq!(plane_samples(&decoded, 0), samples);
    }

    #[test]
    fn truncated_packet_is_rejected_not_garbage() {
        let samples: Vec<i32> = (0..2000).map(|i| ((i * 7) % 4001) - 2000).collect();
        let frame = mono_frame(&samples, SampleFmt::S16P, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        let half_len = bytes.len().checked_div(2).unwrap_or(0);
        let half = bytes.get(..half_len).unwrap();
        let mut budget2 = Budget::new(Limits::permissive());
        let result = decode(half, 44100, Some(ChannelLayout::MONO), &mut budget2);
        assert!(result.is_err());
    }

    #[test]
    fn no_layout_hint_falls_back_to_packet_channel_count() {
        let samples: Vec<i32> = (0..50).map(|i| i - 25).collect();
        let frame = mono_frame(&samples, SampleFmt::S16P, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&frame, &mut budget).unwrap();
        let decoded = decode(&bytes, 44100, None, &mut budget).unwrap();
        assert_eq!(plane_samples(&decoded, 0), samples);
    }

    proptest::proptest! {
        /// Random mono `s16p` PCM, any length in `0..=600`, at any 16-bit
        /// value including the full-scale extremes: encode -> decode must be
        /// bit-exact regardless of what proptest's shrinker finds.
        #[test]
        fn mono_s16_round_trips_arbitrary_pcm(
            samples in proptest::collection::vec(i32::from(i16::MIN)..=i32::from(i16::MAX), 0..600),
        ) {
            let frame = mono_frame(&samples, SampleFmt::S16P, 44100);
            let mut budget = Budget::new(Limits::permissive());
            let bytes = encode(&frame, &mut budget).unwrap();
            let decoded = decode(&bytes, 44100, Some(ChannelLayout::MONO), &mut budget).unwrap();
            proptest::prop_assert_eq!(plane_samples(&decoded, 0), samples);
        }

        /// Same, at the full `s32p` range, including `i32::MIN`/`i32::MAX`.
        #[test]
        fn mono_s32_round_trips_arbitrary_pcm(
            samples in proptest::collection::vec(proptest::num::i32::ANY, 0..300),
        ) {
            let frame = mono_frame(&samples, SampleFmt::S32P, 96000);
            let mut budget = Budget::new(Limits::permissive());
            let bytes = encode(&frame, &mut budget).unwrap();
            let decoded = decode(&bytes, 96000, Some(ChannelLayout::MONO), &mut budget).unwrap();
            proptest::prop_assert_eq!(plane_samples(&decoded, 0), samples);
        }

        /// Independent random left/right `s16p` channels: exercises the
        /// mid/side transform and both `RiceState`/`Predictor` instances at
        /// once, not just a correlated pair.
        #[test]
        fn stereo_round_trips_arbitrary_pcm(
            left in proptest::collection::vec(i32::from(i16::MIN)..=i32::from(i16::MAX), 0..400),
            right in proptest::collection::vec(i32::from(i16::MIN)..=i32::from(i16::MAX), 0..400),
        ) {
            let n = left.len().min(right.len());
            let left = left.get(..n).unwrap_or(&[]).to_vec();
            let right = right.get(..n).unwrap_or(&[]).to_vec();
            let frame = stereo_frame(&left, &right, SampleFmt::S16P, 44100);
            let mut budget = Budget::new(Limits::permissive());
            let bytes = encode(&frame, &mut budget).unwrap();
            let decoded = decode(&bytes, 44100, Some(ChannelLayout::STEREO), &mut budget).unwrap();
            proptest::prop_assert_eq!(plane_samples(&decoded, 0), left);
            proptest::prop_assert_eq!(plane_samples(&decoded, 1), right);
        }
    }
}
