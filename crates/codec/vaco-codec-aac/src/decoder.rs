//! [`AacDecoder`] — the [`Decoder`] implementation this crate registers.
//!
//! # What this decoder can and cannot do today
//!
//! It resolves configuration, fully parses `raw_data_block()` syntax, and
//! reconstructs PCM through inverse quantisation, perceptual noise substitution,
//! joint stereo (M/S and intensity), TNS application, and the
//! IMDCT/windowing/overlap-add filterbank — see `crate::reconstruct` for
//! the pipeline and `docs/codec/vaco-codec-aac.md` for the measured
//! `correlation/max_abs/rms` table (AAC, like every lossy codec this
//! workspace has decoded, defines a compliance tolerance rather than one
//! correct output — this crate does not claim or chase bit-exactness).
//!
//! Known gaps, disclosed rather than silently approximated: `CCE`
//! (coupling) is refused; `channelConfiguration` 14 is
//! gated at the configuration layer; intensity stereo always assumes
//! in-phase (`INTENSITY_HCB`), since `IcsStream` does not retain which of
//! the two intensity codebooks a band used; the `LongStart`/`LongStop`
//! window-transition boundary arithmetic follows the standard,
//! widely-implemented convention rather than a clean primary-text
//! citation (see `crate::reconstruct::build_window`'s doc). Real
//! ffmpeg-encoded fixtures use KBD windows (`window_shape == 1`), provided by
//! `vaco-codec-dsp-sinewin`.

use std::collections::VecDeque;

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Decoder;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_aac::{AdtsHeader, AudioSpecificConfig};
use vaco_sampfmt::SampleFmt;

use crate::config::DecoderConfig;
use crate::raw_data_block::{self, Element};
use crate::reconstruct::{self, ImdctPlans, OverlapState};
use crate::swb_tables::{swb_offset_long, swb_offset_short};
use crate::tns_apply::tns_max_bands;

/// The AAC-LC decoder. See the module doc for exactly what is and is not
/// implemented yet.
#[derive(Debug)]
pub struct AacDecoder {
    budget: Budget,
    /// Set by [`Decoder::set_extradata`], when the container offered one.
    extradata_config: Option<DecoderConfig>,
    /// The configuration currently in force — from `extradata_config` if
    /// present, otherwise (re-)derived per packet from a leading ADTS
    /// header.
    config: Option<DecoderConfig>,
    /// One [`OverlapState`] per output channel, in decode order (matching
    /// `raw_data_block()`'s own element order — `SCE`/`LFE` contribute one
    /// channel each, `CPE` two). Reset by [`Decoder::flush`] and
    /// re-sized lazily on the first packet, since the channel count is not
    /// known until then for every path except an already-resolved
    /// `AudioSpecificConfig`.
    overlap: Vec<OverlapState>,
    /// The long and short `vaco-tx` IMDCT plans. Built lazily on first use —
    /// `AacDecoder::new` is infallible (its `make` signature in
    /// `DecoderDesc` cannot report an error), while `Plan::new` returns a
    /// `Result` in general, even though AAC's two fixed lengths (2048, 256)
    /// never actually fail it.
    imdct: Option<ImdctPlans>,
    /// A running counter feeding perceptual-noise-substitution's
    /// pseudo-random generator a different (but fully deterministic) seed
    /// per channel per frame — PNS is explicitly not required to be
    /// bit-exact across decoders (§4.6.13.3), so nothing depends on this
    /// beyond "not identical across channels".
    prng_counter: u32,
    pending: VecDeque<Frame>,
    /// `send_packet(None)` has been seen. `receive_frame` reports `Eof`
    /// once `pending` is empty and this is set, rather than `NeedMoreInput`
    /// forever -- see `send_packet`'s own doc for why this exists.
    draining: bool,
}

impl AacDecoder {
    /// Build a decoder bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits),
            extradata_config: None,
            config: None,
            overlap: Vec::new(),
            imdct: None,
            prng_counter: 0,
            pending: VecDeque::new(),
            draining: false,
        }
    }

    /// Resolve this packet's configuration and locate its `raw_data_block()`
    /// body: reuse `extradata_config` if one was set (the whole payload is
    /// then the `raw_data_block` — MP4/LATM carry no per-frame ADTS header),
    /// otherwise parse a leading `AdtsHeader` and take the body from just
    /// past it. If the resolved configuration is still waiting on a program
    /// config element (`channelConfiguration == 0`), try to clear that from
    /// the body's own leading bits too.
    fn resolve_packet<'a>(&mut self, payload: &'a [u8]) -> Result<(DecoderConfig, &'a [u8])> {
        let (mut cfg, mut body) = if let Some(cfg) = &self.extradata_config {
            (cfg.clone(), payload)
        } else {
            let header = AdtsHeader::parse(payload)?;
            let cfg = DecoderConfig::from_adts_header(&header)?;
            let body = payload.get(header.header_len()..).unwrap_or(&[]);
            (cfg, body)
        };
        let cached_pce_config = self.config.as_ref().filter(|cached| {
            cached.channel_configuration == 0
                && !cached.is_pending()
                && cached.object_type == cfg.object_type
                && cached.sample_rate == cfg.sample_rate
                && cached.frame_length == cfg.frame_length
        });
        if cfg.is_pending() {
            let mut r = BitReader::new(body);
            if cfg.try_resolve_pending(&mut r)? {
                if !r.is_aligned() {
                    return Err(Error::InvalidData(
                        "vaco-codec-aac: leading program_config_element is not byte-aligned",
                    ));
                }
                let pce_bytes = usize::try_from(r.bit_pos() >> 3).map_err(|_| {
                    Error::InvalidData("vaco-codec-aac: leading program_config_element is too long")
                })?;
                body = body.get(pce_bytes..).ok_or(Error::InvalidData(
                    "vaco-codec-aac: leading program_config_element exceeds packet body",
                ))?;
            } else if let Some(cached) = cached_pce_config {
                cfg = cached.clone();
            }
        }
        Ok((cfg, body))
    }

    fn next_prng_seed(&mut self) -> u32 {
        self.prng_counter = self.prng_counter.wrapping_add(0x9e37_79b9);
        self.prng_counter | 1 // never zero: an all-zero LCG state stays zero
    }
}

/// Permute `channels` from `raw_data_block`'s syntactic element order into
/// the conventional front-left-first output order for the
/// `channelConfiguration` values this crate resolves without a program
/// config element (`known_channel_count` in `crate::config`: 1 through 7, 11, 12, 14).
///
/// The entry for each configuration is `output_index -> source_index`,
/// derived from Table 1.19's syntactic element order (`SCE`, `CPE`, `CPE`,
/// `LFE`, in that order) against the output order
/// `vaco_parse_aac::tables::layout_for_config`'s channel mask implies:
///
/// - 1 (mono, centre only) and 2 (stereo, front L/R): already in output
///   order — the single `SCE` or `CPE` maps straight through.
/// - 3 (3.0): syntactic order is `[C, L, R]` (one `SCE`, one front `CPE`);
///   output order is `[FL, FR, FC]`.
/// - 4 (4.0): syntactic order is `[C, L, R, BC]` (one `SCE`, one front
///   `CPE`, one back-centre `SCE`); output order is `[FL, FR, FC, BC]`.
/// - 5 (5.0): syntactic order is `[C, L, R, Ls, Rs]` (one `SCE`, one front
///   `CPE`, one back `CPE`); output order is `[FL, FR, FC, BL, BR]`.
/// - 6 (5.1): syntactic order is `[C, L, R, Ls, Rs, LFE]` (one `SCE`, one
///   front `CPE`, one back `CPE`, one `LFE`); output order is
///   `[FL, FR, FC, LFE, BL, BR]`. Confirmed empirically against
///   `ffmpeg -bitexact`'s own channel order for a real 5.1 fixture (see
///   `docs/codec/vaco-codec-aac.md`) — before this reorder, per-channel
///   correlation was solid (~0.98) but the *global* interleaved
///   correlation was near zero because channel 0 held centre content
///   while the reference's channel 0 held front-left silence.
/// - 7 (7.1): syntactic order is `[C, L, R, Ls, Rs, Lb, Rb, LFE]`
///   (one `SCE`, three `CPE`s, one `LFE`); output order is
///   `[FL, FR, FC, LFE, BL, BR, SL, SR]`.
/// - 11 (6.1 back): syntactic order is `[C, L, R, Lb, Rb, BC, LFE]`
///   (two `SCE`s, two `CPE`s, one `LFE`); output order is
///   `[FL, FR, FC, LFE, BL, BR, BC]`.
/// - 12 (7.1): uses the same syntax and output order as 7.
/// - 14 (5.1.2 back): syntactic order is `[C, L, R, Lb, Rb, LFE, TBL, TBR]`
///   (one `SCE`, one front `CPE`, one back `CPE`, one `LFE`, one top-back
///   `CPE`); output order is `[FL, FR, FC, LFE, BL, BR, TBL, TBR]`.
///
/// Any other `channel_configuration` (including PCE-explicit layouts whose
/// element structure does not have a verified map, and the 14 value
/// gated at the configuration layer) is left in parsed order. Reordering by
/// count alone would be a guess.
fn reorder_to_output_channel_order(channels: &mut Vec<Vec<f32>>, channel_configuration: u8) {
    let perm: &[usize] = match (channel_configuration, channels.len()) {
        (3, 3) => &[1, 2, 0],
        (4, 4) => &[1, 2, 0, 3],
        (5, 5) => &[1, 2, 0, 3, 4],
        (6, 6) => &[1, 2, 0, 5, 3, 4],
        (7 | 12, 8) => &[1, 2, 0, 7, 5, 6, 3, 4],
        (11, 7) => &[1, 2, 0, 6, 3, 4, 5],
        (14, 8) => &[1, 2, 0, 5, 3, 4, 6, 7],
        _ => return,
    };
    reorder_channels(channels, perm);
}

/// Reorder exact channel planes with an `output_index -> source_index` map.
/// A length mismatch is not a map this decoder has established, so leave it
/// untouched rather than dropping or duplicating a plane.
fn reorder_channels(channels: &mut Vec<Vec<f32>>, perm: &[usize]) {
    if channels.len() != perm.len() {
        return;
    }
    let reordered: Vec<Vec<f32>> = perm
        .iter()
        .map(|&i| channels.get_mut(i).map(std::mem::take).unwrap_or_default())
        .collect();
    *channels = reordered;
}

#[cfg(test)]
mod output_order_tests {
    use super::reorder_to_output_channel_order;

    #[test]
    fn five_zero_moves_the_centre_after_the_front_pair() {
        let mut channels = vec![vec![0.0], vec![1.0], vec![2.0], vec![3.0], vec![4.0]];
        reorder_to_output_channel_order(&mut channels, 5);
        assert_eq!(
            channels,
            vec![vec![1.0], vec![2.0], vec![0.0], vec![3.0], vec![4.0]]
        );
    }

    #[test]
    fn four_zero_moves_the_centre_after_the_front_pair() {
        let mut channels = vec![vec![0.0], vec![1.0], vec![2.0], vec![3.0]];
        reorder_to_output_channel_order(&mut channels, 4);
        assert_eq!(channels, vec![vec![1.0], vec![2.0], vec![0.0], vec![3.0]]);
    }

    #[test]
    fn three_zero_moves_the_centre_after_the_front_pair() {
        let mut channels = vec![vec![0.0], vec![1.0], vec![2.0]];
        reorder_to_output_channel_order(&mut channels, 3);
        assert_eq!(channels, vec![vec![1.0], vec![2.0], vec![0.0]]);
    }

    #[test]
    fn seven_one_moves_each_direct_channel_to_native_order() {
        // AAC's syntax order is FC, FL, FR, SL, SR, BL, BR, LFE.
        let mut channels = vec![
            vec![0.0],
            vec![1.0],
            vec![2.0],
            vec![3.0],
            vec![4.0],
            vec![5.0],
            vec![6.0],
            vec![7.0],
        ];
        reorder_to_output_channel_order(&mut channels, 7);
        assert_eq!(
            channels,
            vec![
                vec![1.0],
                vec![2.0],
                vec![0.0],
                vec![7.0],
                vec![5.0],
                vec![6.0],
                vec![3.0],
                vec![4.0],
            ]
        );
    }

    #[test]
    fn configuration_twelve_moves_each_direct_channel_to_native_order() {
        let mut channels = vec![
            vec![0.0],
            vec![1.0],
            vec![2.0],
            vec![3.0],
            vec![4.0],
            vec![5.0],
            vec![6.0],
            vec![7.0],
        ];
        reorder_to_output_channel_order(&mut channels, 12);
        assert_eq!(
            channels,
            vec![
                vec![1.0],
                vec![2.0],
                vec![0.0],
                vec![7.0],
                vec![5.0],
                vec![6.0],
                vec![3.0],
                vec![4.0],
            ]
        );
    }

    #[test]
    fn configuration_eleven_moves_each_direct_channel_to_native_order() {
        let mut channels = vec![
            vec![0.0],
            vec![1.0],
            vec![2.0],
            vec![3.0],
            vec![4.0],
            vec![5.0],
            vec![6.0],
        ];
        reorder_to_output_channel_order(&mut channels, 11);
        assert_eq!(
            channels,
            vec![
                vec![1.0],
                vec![2.0],
                vec![0.0],
                vec![6.0],
                vec![3.0],
                vec![4.0],
                vec![5.0],
            ]
        );
    }

    #[test]
    fn configuration_fourteen_moves_height_channels_after_the_51_bed() {
        let mut channels = vec![
            vec![0.0],
            vec![1.0],
            vec![2.0],
            vec![3.0],
            vec![4.0],
            vec![5.0],
            vec![6.0],
            vec![7.0],
        ];
        reorder_to_output_channel_order(&mut channels, 14);
        assert_eq!(
            channels,
            vec![
                vec![1.0],
                vec![2.0],
                vec![0.0],
                vec![5.0],
                vec![3.0],
                vec![4.0],
                vec![6.0],
                vec![7.0],
            ]
        );
    }
}

impl Decoder for AacDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            // Frames do not buffer across packets, so draining only marks the
            // state. `receive_frame`, not `send_packet`, reports final EOF once
            // the pending queue is empty.
            self.draining = true;
            return Ok(());
        };
        let (cfg, body) = self.resolve_packet(packet.payload())?;
        if cfg.is_pending() {
            return Err(Error::Unsupported(
                "vaco-codec-aac: channelConfiguration == 0 and no leading program \
                 config element was found; cannot determine channel layout",
            ));
        }
        let sfi = vaco_parse_aac::tables::index_for_frequency(cfg.sample_rate);
        let swb_long = swb_offset_long(sfi).ok_or(Error::Unsupported(
            "vaco-codec-aac: no scalefactor band table for this sampling rate (7350 Hz)",
        ))?;
        let swb_short = swb_offset_short(sfi).ok_or(Error::Unsupported(
            "vaco-codec-aac: no scalefactor band table for this sampling rate (7350 Hz)",
        ))?;
        let max_bands_long = tns_max_bands(sfi, false);
        let max_bands_short = tns_max_bands(sfi, true);

        let mut r = BitReader::new(body);
        let elements = raw_data_block::read(&mut r, sfi)?;
        if elements
            .iter()
            .any(|element| matches!(element, Element::ProgramConfig(_)))
        {
            return Err(Error::Unsupported(
                "vaco-codec-aac: mid-stream program_config_element() is not implemented — \
                 refusing to decode with a stale channel configuration",
            ));
        }
        if let Some(expected) = cfg.pce_element_order() {
            let actual: Vec<_> = elements
                .iter()
                .filter_map(Element::channel_element_ref)
                .collect();
            if actual.as_slice() != expected {
                return Err(Error::InvalidData(
                    "vaco-codec-aac: PCE channel-element sequence does not match raw_data_block",
                ));
            }
        }

        // Count output channels first, so `self.overlap` can be sized
        // before any element needs it.
        let total_channels: usize = elements
            .iter()
            .map(|e| match e {
                Element::Single { .. } | Element::Lfe { .. } => 1,
                Element::Pair { .. } => 2,
                Element::ProgramConfig(_) => 0,
            })
            .sum();
        if self.overlap.len() != total_channels {
            self.overlap = (0..total_channels).map(|_| OverlapState::new()).collect();
        }

        // Built once, reused for every channel and every packet: `Tx::execute`
        // takes `&mut self` only for its scratch buffer, and channels within
        // one packet are reconstructed strictly sequentially below, so one
        // pair of plans is enough (see `ImdctPlans`'s own doc).
        let mut imdct = match self.imdct.take() {
            Some(p) => p,
            None => ImdctPlans::new()?,
        };

        let mut channels: Vec<Vec<f32>> = Vec::new();
        let mut overlap_iter = 0usize;
        for element in &elements {
            match element {
                Element::Single { stream, .. } | Element::Lfe { stream, .. } => {
                    let seed = self.next_prng_seed();
                    let spec = reconstruct::deinterleave_channel(stream, swb_long, swb_short, seed);
                    let Some(overlap) = self.overlap.get_mut(overlap_iter) else {
                        continue;
                    };
                    let out = reconstruct::finalize_channel(
                        stream,
                        spec,
                        swb_long,
                        swb_short,
                        max_bands_long,
                        max_bands_short,
                        overlap,
                        &mut imdct,
                    );
                    channels.push(out);
                    overlap_iter += 1;
                }
                Element::Pair {
                    ms_mask, ch0, ch1, ..
                } => {
                    let seed0 = self.next_prng_seed();
                    let seed1 = self.next_prng_seed();
                    let mut spec0 =
                        reconstruct::deinterleave_channel(ch0, swb_long, swb_short, seed0);
                    let mut spec1 =
                        reconstruct::deinterleave_channel(ch1, swb_long, swb_short, seed1);
                    if let Some(mask) = ms_mask {
                        reconstruct::apply_joint_stereo(
                            &mut spec0, &mut spec1, ch1, swb_long, swb_short, mask,
                        );
                    }
                    let (Some(overlap0_idx), Some(overlap1_idx)) =
                        (overlap_iter.checked_add(0), overlap_iter.checked_add(1))
                    else {
                        continue;
                    };
                    let out0 = {
                        let Some(overlap) = self.overlap.get_mut(overlap0_idx) else {
                            continue;
                        };
                        reconstruct::finalize_channel(
                            ch0,
                            spec0,
                            swb_long,
                            swb_short,
                            max_bands_long,
                            max_bands_short,
                            overlap,
                            &mut imdct,
                        )
                    };
                    let out1 = {
                        let Some(overlap) = self.overlap.get_mut(overlap1_idx) else {
                            continue;
                        };
                        reconstruct::finalize_channel(
                            ch1,
                            spec1,
                            swb_long,
                            swb_short,
                            max_bands_long,
                            max_bands_short,
                            overlap,
                            &mut imdct,
                        )
                    };
                    channels.push(out0);
                    channels.push(out1);
                    overlap_iter += 2;
                }
                Element::ProgramConfig(_) => {}
            }
        }
        self.imdct = Some(imdct);

        if channels.is_empty() {
            return Err(Error::Unsupported(
                "vaco-codec-aac: raw_data_block parsed with no audio elements",
            ));
        }

        // `channels` is in raw_data_block's *syntactic* element order
        // (SCE/CPE/LFE parse order), which is not the conventional
        // front-left-first output order this crate's callers (and
        // `ffmpeg -bitexact`, used for this crate's own verification)
        // expect. Reorder it for the configurations this crate resolves.
        reorder_to_output_channel_order(&mut channels, cfg.channel_configuration);
        if let Some(permutation) = cfg.pce_output_permutation() {
            reorder_channels(&mut channels, permutation);
        }

        let samples = channels.first().map_or(0, Vec::len) as u32;
        let layout = cfg
            .output_layout()
            .unwrap_or_else(|| ChannelLayout::unspecified(channels.len() as u32));
        let sample_rate = cfg.sample_rate;
        let mut frame = Frame::alloc_audio(
            &mut self.budget,
            SampleFmt::F32P,
            layout,
            samples,
            sample_rate,
        )?;
        for (ch, data) in channels.iter().enumerate() {
            let Some(mut plane) = frame.plane_mut(ch) else {
                continue;
            };
            let Some(row) = plane.row_mut(0) else {
                continue;
            };
            for (i, &v) in data.iter().enumerate() {
                let bytes = v.to_le_bytes();
                if let Some(dst) = row.get_mut(i * 4..i * 4 + 4) {
                    dst.copy_from_slice(&bytes);
                }
            }
        }
        self.config = Some(cfg);
        frame.pts = packet.pts;
        // The decode-side mirror of this session's audio-decoder duration
        // audit (`vaco-codec-pcm`/`-adpcm`/`-simple-audio`/`-vorbis`/
        // `-ac3`): `samples`/`sample_rate` were already in scope, but
        // `frame.duration` was never set.
        let time_base = Rational::new(1, i32::try_from(sample_rate).unwrap_or(1).max(1));
        frame.duration = Timestamp::new(i64::from(samples))
            .to_duration(time_base)
            .unwrap_or(Duration::ZERO);
        self.pending.push_back(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(if self.draining {
            Error::Eof
        } else {
            Error::NeedMoreInput
        })
    }

    fn flush(&mut self) {
        self.config = None;
        self.overlap.clear();
        self.pending.clear();
        self.draining = false;
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.extradata_config = None;
        self.config = None;
        self.overlap.clear();
        let asc = AudioSpecificConfig::parse(extradata)?;
        self.extradata_config = Some(DecoderConfig::from_audio_specific_config(&asc)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::AacDecoder;
    use vaco_codec_core::Decoder;
    use vaco_core::{Duration, Error};
    use vaco_frame::FrameData;
    use vaco_limits::{Budget, Limits};
    use vaco_packet::Packet;

    /// A real ADTS header (mono, so a single `SCE` is the whole
    /// `raw_data_block`) wrapping a minimal-but-complete `SCE` (`max_sfb=1`,
    /// one `ZERO_HCB` band) followed by `ID_END` and `byte_alignment()`.
    fn adts_frame_with_minimal_raw_data_block() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut body = BitWriter::new();
        body.put(3, 0); // ID_SCE
        body.put(4, 0); // element_instance_tag
        body.put(8, 100); // global_gain
        body.put(1, 0); // ics_reserved_bit
        body.put(2, 0); // ONLY_LONG
        body.put(1, 0); // sine window
        body.put(6, 1); // max_sfb = 1
        body.put(1, 0); // predictor_data_present
        body.put(4, 0); // sect_cb = ZERO_HCB
        body.put(5, 1); // sect_len = 1
        body.put(1, 0); // pulse_data_present
        body.put(1, 0); // tns_data_present
        body.put(1, 0); // gain_control_data_present
        body.put(3, 7); // ID_END
        let body_bytes = body.finish();

        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 1); // mono
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7 + body_bytes.len() as u32); // aac_frame_length
        w.put(11, 0x7ff);
        w.put(2, 0);
        let mut bytes = w.finish();
        bytes.extend_from_slice(&body_bytes);
        bytes
    }

    /// A framed mono AAC-LC packet whose complete `SCE` is followed by the
    /// `ID_CCE` selector. The decoder must reject the CCE before it can make
    /// the preceding audio visible as a partial frame.
    fn adts_frame_with_cce_after_minimal_sce() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut body = BitWriter::new();
        body.put(3, 0); // ID_SCE
        body.put(4, 0); // element_instance_tag
        body.put(8, 100); // global_gain
        body.put(1, 0); // ics_reserved_bit
        body.put(2, 0); // ONLY_LONG
        body.put(1, 0); // sine window
        body.put(6, 1); // max_sfb = 1
        body.put(1, 0); // predictor_data_present
        body.put(4, 0); // sect_cb = ZERO_HCB
        body.put(5, 1); // sect_len = 1
        body.put(1, 0); // pulse_data_present
        body.put(1, 0); // tns_data_present
        body.put(1, 0); // gain_control_data_present
        body.put(3, 2); // ID_CCE
        let body_bytes = body.finish();

        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 1); // mono
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7 + body_bytes.len() as u32); // aac_frame_length
        w.put(11, 0x7ff);
        w.put(2, 0);
        let mut bytes = w.finish();
        bytes.extend_from_slice(&body_bytes);
        bytes
    }

    fn adts_frame_with_sbr_fill_payload() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut body = BitWriter::new();
        body.put(3, 6); // ID_FIL
        body.put(4, 1); // one payload byte
        body.put(4, 13); // EXT_SBR_DATA
        body.put(4, 0); // remaining payload bits
        body.put(3, 7); // ID_END
        let body_bytes = body.finish();

        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 1); // mono
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7 + body_bytes.len() as u32); // aac_frame_length
        w.put(11, 0x7ff);
        w.put(2, 0);
        let mut bytes = w.finish();
        bytes.extend_from_slice(&body_bytes);
        bytes
    }

    fn adts_frame_with_truncated_sbr_fill_payload() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut body = BitWriter::new();
        body.put(3, 6); // ID_FIL
        body.put(4, 2); // declares two payload bytes
        body.put(4, 13); // EXT_SBR_DATA
        body.put(4, 0); // deliberately truncated payload
        let body_bytes = body.finish();

        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 1); // mono
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7 + body_bytes.len() as u32); // aac_frame_length
        w.put(11, 0x7ff);
        w.put(2, 0);
        let mut bytes = w.finish();
        bytes.extend_from_slice(&body_bytes);
        bytes
    }

    fn explicit_ps_audio_specific_config() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut w = BitWriter::new();
        w.put(5, 2); // AAC-LC core object type
        w.put(4, 7); // 22050 Hz core rate
        w.put(4, 1); // mono core
        w.put(1, 0); // frameLengthFlag
        w.put(1, 0); // dependsOnCoreCoder
        w.put(1, 0); // extensionFlag
        w.put(11, 0x2b7); // syncExtensionType: SBR
        w.put(5, 5); // extensionAudioObjectType: SBR
        w.put(1, 1); // sbrPresentFlag
        w.put(4, 4); // extensionSamplingFrequencyIndex: 44100 Hz
        w.put(11, 0x548); // syncExtensionType: Parametric Stereo
        w.put(1, 1); // psPresentFlag
        w.finish()
    }

    /// The first raw data block supplies a mono PCE before its SCE. Later
    /// ADTS blocks carry only the SCE and rely on that in-band configuration.
    fn adts_frame_with_leading_mono_pce_with_sce_tag(sce_tag: u32) -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut body = BitWriter::new();
        body.put(3, 5); // ID_PCE
        body.put(4, 0); // PCE element_instance_tag
        body.put(2, 1); // PCE object_type = AAC-LC
        body.put(4, 3); // PCE sampling_frequency_index = 48000 Hz
        body.put(4, 1); // one front channel element
        body.put(4, 0); // no side channel elements
        body.put(4, 0); // no back channel elements
        body.put(2, 0); // no LFE channel elements
        body.put(3, 0); // no associated-data elements
        body.put(4, 0); // no valid CC elements
        body.put(1, 0); // no mono mixdown
        body.put(1, 0); // no stereo mixdown
        body.put(1, 0); // no matrix mixdown
        body.put(1, 0); // front[0] is an SCE
        body.put(4, 0); // front[0] tag
        body.align_zero();
        body.put(8, 0); // empty PCE comment field
        body.put(3, 0); // ID_SCE
        body.put(4, sce_tag); // element_instance_tag
        body.put(8, 100); // global_gain
        body.put(1, 0); // ics_reserved_bit
        body.put(2, 0); // ONLY_LONG
        body.put(1, 0); // sine window
        body.put(6, 1); // max_sfb = 1
        body.put(1, 0); // predictor_data_present
        body.put(4, 0); // sect_cb = ZERO_HCB
        body.put(5, 1); // sect_len = 1
        body.put(1, 0); // pulse_data_present
        body.put(1, 0); // tns_data_present
        body.put(1, 0); // gain_control_data_present
        body.put(3, 7); // ID_END
        let body_bytes = body.finish();

        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 0); // channelConfiguration: PCE supplies it
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7 + body_bytes.len() as u32); // aac_frame_length
        w.put(11, 0x7ff);
        w.put(2, 0);
        let mut bytes = w.finish();
        bytes.extend_from_slice(&body_bytes);
        bytes
    }

    fn adts_frame_with_leading_mono_pce() -> Vec<u8> {
        adts_frame_with_leading_mono_pce_with_sce_tag(0)
    }

    fn adts_frame_with_mono_sce_and_pce_configuration() -> Vec<u8> {
        let mut bytes = adts_frame_with_minimal_raw_data_block();
        // The three channel-configuration bits straddle byte 2's low bit
        // and byte 3's two high bits. The base helper emitted config 1;
        // clear all three to defer the layout to the preceding PCE.
        bytes[2] &= !1;
        bytes[3] &= 0x3f;
        bytes
    }

    fn adts_frame_with_mono_sce_then_pce() -> Vec<u8> {
        use vaco_bitstream::BitWriter;
        let mut body = BitWriter::new();
        body.put(3, 0); // ID_SCE
        body.put(4, 0); // element_instance_tag
        body.put(8, 100); // global_gain
        body.put(1, 0); // ics_reserved_bit
        body.put(2, 0); // ONLY_LONG
        body.put(1, 0); // sine window
        body.put(6, 1); // max_sfb = 1
        body.put(1, 0); // predictor_data_present
        body.put(4, 0); // sect_cb = ZERO_HCB
        body.put(5, 1); // sect_len = 1
        body.put(1, 0); // pulse_data_present
        body.put(1, 0); // tns_data_present
        body.put(1, 0); // gain_control_data_present
        body.put(3, 5); // ID_PCE
        body.put(4, 0); // PCE element_instance_tag
        body.put(2, 1); // PCE object_type = AAC-LC
        body.put(4, 3); // PCE sampling_frequency_index = 48000 Hz
        body.put(4, 1); // one front channel element
        body.put(4, 0); // no side channel elements
        body.put(4, 0); // no back channel elements
        body.put(2, 0); // no LFE channel elements
        body.put(3, 0); // no associated-data elements
        body.put(4, 0); // no valid CC elements
        body.put(1, 0); // no mono mixdown
        body.put(1, 0); // no stereo mixdown
        body.put(1, 0); // no matrix mixdown
        body.put(1, 0); // front[0] is an SCE
        body.put(4, 0); // front[0] tag
        body.align_zero();
        body.put(8, 0); // empty PCE comment field
        body.put(3, 7); // ID_END
        let body_bytes = body.finish();

        let mut w = BitWriter::new();
        w.put(12, 0xfff);
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 1); // protection_absent
        w.put(2, 1); // profile: LC
        w.put(4, 3); // 48000
        w.put(1, 0);
        w.put(3, 0); // channelConfiguration: PCE supplies it
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        w.put(13, 7 + body_bytes.len() as u32); // aac_frame_length
        w.put(11, 0x7ff);
        w.put(2, 0);
        let mut bytes = w.finish();
        bytes.extend_from_slice(&body_bytes);
        bytes
    }

    #[test]
    fn an_all_zero_frame_produces_1024_silent_samples() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let bytes = adts_frame_with_minimal_raw_data_block();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        dec.send_packet(Some(&packet)).unwrap();
        let frame = dec.receive_frame().unwrap();
        let FrameData::Audio {
            samples, planes, ..
        } = &frame.data
        else {
            panic!("expected an audio frame");
        };
        assert_eq!(*samples, 1024);
        assert_eq!(planes.len(), 1);
        let plane = frame.plane(0).unwrap();
        let row = plane.row(0).unwrap();
        // The very first frame's overlap-add half is all-zero (nothing to
        // add from a previous frame yet), and this ICS is all-zero
        // spectral data, so the output must be exactly silent.
        assert!(
            row.chunks_exact(4)
                .all(|c| f32::from_le_bytes(c.try_into().unwrap()) == 0.0)
        );
    }

    /// One packet produces one frame without reordering, so the decoded
    /// frame preserves that packet's PTS exactly.
    #[test]
    fn the_decoded_frames_pts_is_the_packets_pts() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let bytes = adts_frame_with_minimal_raw_data_block();
        let mut budget = Budget::new(Limits::permissive());
        let mut packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        packet.pts = vaco_core::Timestamp::new(1234);
        dec.send_packet(Some(&packet)).unwrap();
        let frame = dec.receive_frame().unwrap();
        assert_eq!(frame.pts, vaco_core::Timestamp::new(1234));
    }

    /// Decoded duration is derived from the emitted sample count and rate.
    #[test]
    fn the_decoded_frames_duration_is_real_and_nonzero() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let bytes = adts_frame_with_minimal_raw_data_block();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        dec.send_packet(Some(&packet)).unwrap();
        let frame = dec.receive_frame().unwrap();
        assert_ne!(frame.duration, Duration::ZERO);
    }

    #[test]
    fn draining_with_nothing_sent_reports_eof() {
        // `send_packet(None)` itself is `Ok` -- `Decoder::send_packet`'s own
        // doc reserves an error return for `OutputPending`, not end of
        // stream -- and `receive_frame` is where `Eof` actually surfaces.
        let mut dec = AacDecoder::new(Limits::permissive());
        assert!(dec.send_packet(None).is_ok());
        assert!(matches!(dec.receive_frame(), Err(Error::Eof)));
    }

    #[test]
    fn an_implicit_sbr_fill_payload_is_refused_before_frame_output() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &adts_frame_with_sbr_fill_payload()).unwrap();

        let error = dec.send_packet(Some(&packet)).unwrap_err();
        assert!(error.to_string().contains("SBR fill payload"));
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn a_cce_after_audio_is_refused_before_partial_frame_output() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let packet =
            Packet::from_slice(&mut budget, &adts_frame_with_cce_after_minimal_sce()).unwrap();

        let error = dec.send_packet(Some(&packet)).unwrap_err();
        assert!(error.to_string().contains("coupling_channel_element"));
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn a_truncated_sbr_fill_payload_fails_before_frame_output() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let packet =
            Packet::from_slice(&mut budget, &adts_frame_with_truncated_sbr_fill_payload()).unwrap();

        assert!(matches!(
            dec.send_packet(Some(&packet)),
            Err(Error::UnexpectedEof)
        ));
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn rejected_explicit_sbr_extradata_cannot_leave_an_old_lc_config_live() {
        let mut dec = AacDecoder::new(Limits::permissive());
        dec.set_extradata(&[0x11, 0x88]).unwrap(); // AAC-LC, 48000 Hz mono

        let error = dec
            .set_extradata(&[0x13, 0x90, 0x56, 0xe5, 0xa0])
            .unwrap_err(); // HE-AAC's recorded explicit-SBR configuration
        assert!(error.to_string().contains("SBR (HE-AAC)"));

        let adts = adts_frame_with_minimal_raw_data_block();
        let raw_body = adts.get(7..).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, raw_body).unwrap();

        let error = dec.send_packet(Some(&packet)).unwrap_err();
        assert!(matches!(error, Error::UnexpectedEof), "{error}");
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn rejected_explicit_ps_extradata_cannot_leave_an_old_lc_config_live() {
        let extradata = explicit_ps_audio_specific_config();
        let asc = vaco_parse_aac::AudioSpecificConfig::parse(&extradata).unwrap();
        assert!(asc.has_sbr());
        assert!(matches!(asc.ps, vaco_parse_aac::Signal::Present));

        let mut dec = AacDecoder::new(Limits::permissive());
        dec.set_extradata(&[0x11, 0x88]).unwrap(); // AAC-LC, 48000 Hz mono
        let error = dec.set_extradata(&extradata).unwrap_err();
        assert!(error.to_string().contains("Parametric Stereo"));

        let adts = adts_frame_with_minimal_raw_data_block();
        let raw_body = adts.get(7..).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, raw_body).unwrap();

        let error = dec.send_packet(Some(&packet)).unwrap_err();
        assert!(matches!(error, Error::UnexpectedEof), "{error}");
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn flush_resets_overlap_state_so_a_stale_history_is_not_reused() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let bytes = adts_frame_with_minimal_raw_data_block();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        dec.send_packet(Some(&packet)).unwrap();
        let _ = dec.receive_frame().unwrap();
        dec.flush();
        assert!(dec.overlap.is_empty());
    }

    #[test]
    fn leading_pce_configuration_persists_for_following_adts_packets() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let first = Packet::from_slice(&mut budget, &adts_frame_with_leading_mono_pce()).unwrap();
        dec.send_packet(Some(&first)).unwrap();
        let _ = dec.receive_frame().unwrap();

        let mut budget = Budget::new(Limits::permissive());
        let following = Packet::from_slice(
            &mut budget,
            &adts_frame_with_mono_sce_and_pce_configuration(),
        )
        .unwrap();
        dec.send_packet(Some(&following)).unwrap();
        let frame = dec.receive_frame().unwrap();
        let FrameData::Audio {
            samples,
            planes,
            layout,
            ..
        } = &frame.data
        else {
            panic!("expected an audio frame");
        };
        assert_eq!(*samples, 1024);
        assert_eq!(planes.len(), 1);
        assert_eq!(layout.mask(), 0x4);
    }

    #[test]
    fn a_mid_stream_pce_is_refused_not_silently_ignored() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let first = Packet::from_slice(&mut budget, &adts_frame_with_leading_mono_pce()).unwrap();
        dec.send_packet(Some(&first)).unwrap();
        let _ = dec.receive_frame().unwrap();

        let mut budget = Budget::new(Limits::permissive());
        let following =
            Packet::from_slice(&mut budget, &adts_frame_with_mono_sce_then_pce()).unwrap();
        let error = dec.send_packet(Some(&following)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("mid-stream program_config_element")
        );
    }

    #[test]
    fn a_pce_and_its_channel_element_tags_must_match() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(
            &mut budget,
            &adts_frame_with_leading_mono_pce_with_sce_tag(1),
        )
        .unwrap();

        let error = dec.send_packet(Some(&packet)).unwrap_err();
        assert!(error.to_string().contains("PCE channel-element sequence"));
    }
}
