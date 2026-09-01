//! [`AacDecoder`] — the [`Decoder`] implementation this crate registers.
//!
//! # What this decoder can and cannot do today
//!
//! It resolves configuration (T3-03a / #443), fully parses
//! `raw_data_block()`'s syntax (T3-03b / #444), and reconstructs PCM
//! (T3-03c / #445): inverse quantisation, perceptual noise substitution,
//! joint stereo (M/S and intensity), TNS application, and the
//! IMDCT/windowing/overlap-add filterbank — see `crate::reconstruct` for
//! the pipeline and `docs/codec/vaco-codec-aac.md` for the measured
//! `correlation/max_abs/rms` table (AAC, like every lossy codec this
//! workspace has decoded, defines a compliance tolerance rather than one
//! correct output — this crate does not claim or chase bit-exactness).
//!
//! Known gaps, disclosed rather than silently approximated: `CCE`
//! (coupling) is refused; `channelConfiguration` 3/4/5/7/11/12/14 are
//! gated at the configuration layer; intensity stereo always assumes
//! in-phase (`INTENSITY_HCB`), since `IcsStream` does not retain which of
//! the two intensity codebooks a band used; the `LongStart`/`LongStop`
//! window-transition boundary arithmetic follows the standard,
//! widely-implemented convention rather than a clean primary-text
//! citation (see `crate::reconstruct::build_window`'s doc). Real
//! ffmpeg-encoded fixtures use KBD windows (`window_shape == 1`), which
//! `vaco-codec-dsp-sinewin` now implements (extended past its original
//! sine-only scope — see that crate's own doc).

use std::collections::VecDeque;

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
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
    /// The long/short IMDCT plans (C1: `vaco-tx`'s `Plan`, not the O(n²)
    /// `reference::imdct` production used to call). Built lazily on first
    /// use — `AacDecoder::new` is infallible (its `make` signature in
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
        let (mut cfg, body) = if let Some(cfg) = &self.extradata_config {
            (cfg.clone(), payload)
        } else {
            let header = AdtsHeader::parse(payload)?;
            let cfg = DecoderConfig::from_adts_header(&header)?;
            let body = payload.get(header.header_len()..).unwrap_or(&[]);
            (cfg, body)
        };
        if cfg.is_pending() {
            let mut r = BitReader::new(body);
            let _ = cfg.try_resolve_pending(&mut r)?;
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
/// config element (`known_channel_count` in `crate::config`: 1, 2, 6).
///
/// The entry for each configuration is `output_index -> source_index`,
/// derived from Table 1.19's syntactic element order (`SCE`, `CPE`, `CPE`,
/// `LFE`, in that order) against the output order
/// `vaco_parse_aac::tables::layout_for_config`'s channel mask implies:
///
/// - 1 (mono, centre only) and 2 (stereo, front L/R): already in output
///   order — the single `SCE` or `CPE` maps straight through.
/// - 6 (5.1): syntactic order is `[C, L, R, Ls, Rs, LFE]` (one `SCE`, one
///   front `CPE`, one back `CPE`, one `LFE`); output order is
///   `[FL, FR, FC, LFE, BL, BR]`. Confirmed empirically against
///   `ffmpeg -bitexact`'s own channel order for a real 5.1 fixture (see
///   `docs/codec/vaco-codec-aac.md`) — before this reorder, per-channel
///   correlation was solid (~0.98) but the *global* interleaved
///   correlation was near zero because channel 0 held centre content
///   while the reference's channel 0 held front-left silence.
///
/// Any other `channel_configuration` (including 0/PCE-explicit, and the
/// 3/4/5/7/11/12/14 values gated at the configuration layer) is left in
/// its parsed order — this crate does not yet know their intended output
/// order, and reordering by count alone would be a guess.
fn reorder_to_output_channel_order(channels: &mut Vec<Vec<f32>>, channel_configuration: u8) {
    let perm: &[usize] = match (channel_configuration, channels.len()) {
        (6, 6) => &[1, 2, 0, 5, 3, 4],
        _ => return,
    };
    let reordered: Vec<Vec<f32>> =
        perm.iter().map(|&i| channels.get_mut(i).map(std::mem::take).unwrap_or_default()).collect();
    *channels = reordered;
}

impl Decoder for AacDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            // Draining at EOF: nothing is buffered across packets (each
            // frame is independently decodable once its overlap-add state
            // exists), so there is nothing further to flush out here --
            // but `Error::Eof` is `receive_frame`'s signal to give
            // (`Decoder::send_packet`'s own doc: the only documented error
            // from *this* method is `OutputPending`), not `send_packet`'s.
            // Returning it directly here used to propagate straight through
            // `CodecWork::advance`'s `self.side.send(None)?` as a fatal
            // pipeline error instead of a graceful finish -- measured
            // against a real AAC file transcoded end to end through the
            // CLI: `vaco -i av.mp4 -vn -c:a pcm_s16le out.wav` decoded every
            // frame correctly and then failed the whole run with "Error
            // while filtering: end of stream" instead of completing.
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

        // Count output channels first, so `self.overlap` can be sized
        // before any element needs it.
        let total_channels: usize = elements
            .iter()
            .map(|e| match e {
                Element::Single(_) | Element::Lfe(_) => 1,
                Element::Pair(..) => 2,
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
                Element::Single(stream) | Element::Lfe(stream) => {
                    let seed = self.next_prng_seed();
                    let spec = reconstruct::deinterleave_channel(stream, swb_long, swb_short, seed);
                    let Some(overlap) = self.overlap.get_mut(overlap_iter) else {
                        continue;
                    };
                    let out = reconstruct::finalize_channel(
                        stream, spec, swb_long, swb_short, max_bands_long, max_bands_short, overlap, &mut imdct,
                    );
                    channels.push(out);
                    overlap_iter += 1;
                }
                Element::Pair(ms_mask, ch0, ch1) => {
                    let seed0 = self.next_prng_seed();
                    let seed1 = self.next_prng_seed();
                    let mut spec0 = reconstruct::deinterleave_channel(ch0, swb_long, swb_short, seed0);
                    let mut spec1 = reconstruct::deinterleave_channel(ch1, swb_long, swb_short, seed1);
                    if let Some(mask) = ms_mask {
                        reconstruct::apply_joint_stereo(&mut spec0, &mut spec1, ch1, swb_long, swb_short, mask);
                    }
                    let (Some(overlap0_idx), Some(overlap1_idx)) = (overlap_iter.checked_add(0), overlap_iter.checked_add(1))
                    else {
                        continue;
                    };
                    let out0 = {
                        let Some(overlap) = self.overlap.get_mut(overlap0_idx) else { continue };
                        reconstruct::finalize_channel(
                            ch0, spec0, swb_long, swb_short, max_bands_long, max_bands_short, overlap, &mut imdct,
                        )
                    };
                    let out1 = {
                        let Some(overlap) = self.overlap.get_mut(overlap1_idx) else { continue };
                        reconstruct::finalize_channel(
                            ch1, spec1, swb_long, swb_short, max_bands_long, max_bands_short, overlap, &mut imdct,
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

        let samples = channels.first().map_or(0, Vec::len) as u32;
        let layout = vaco_parse_aac::tables::layout_for_config(cfg.channel_configuration)
            .unwrap_or_else(|| ChannelLayout::unspecified(channels.len() as u32));
        let mut frame = Frame::alloc_audio(&mut self.budget, SampleFmt::F32P, layout, samples, cfg.sample_rate)?;
        for (ch, data) in channels.iter().enumerate() {
            let Some(mut plane) = frame.plane_mut(ch) else { continue };
            let Some(row) = plane.row_mut(0) else { continue };
            for (i, &v) in data.iter().enumerate() {
                let bytes = v.to_le_bytes();
                if let Some(dst) = row.get_mut(i * 4..i * 4 + 4) {
                    dst.copy_from_slice(&bytes);
                }
            }
        }
        self.config = Some(cfg);
        frame.pts = packet.pts;
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
        let asc = AudioSpecificConfig::parse(extradata)?;
        self.extradata_config = Some(DecoderConfig::from_audio_specific_config(&asc)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
    use super::AacDecoder;
    use vaco_codec_core::Decoder;
    use vaco_core::Error;
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

    #[test]
    fn an_all_zero_frame_produces_1024_silent_samples() {
        let mut dec = AacDecoder::new(Limits::permissive());
        let bytes = adts_frame_with_minimal_raw_data_block();
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();
        dec.send_packet(Some(&packet)).unwrap();
        let frame = dec.receive_frame().unwrap();
        let FrameData::Audio { samples, planes, .. } = &frame.data else {
            panic!("expected an audio frame");
        };
        assert_eq!(*samples, 1024);
        assert_eq!(planes.len(), 1);
        let plane = frame.plane(0).unwrap();
        let row = plane.row(0).unwrap();
        // The very first frame's overlap-add half is all-zero (nothing to
        // add from a previous frame yet), and this ICS is all-zero
        // spectral data, so the output must be exactly silent.
        assert!(row.chunks_exact(4).all(|c| f32::from_le_bytes(c.try_into().unwrap()) == 0.0));
    }

    /// E2E-GAPS #5-adjacent: found while verifying `-c:a pcm_s16le` on a real
    /// AAC input end to end through the CLI, which failed downstream with
    /// "this container needs timestamps and the packet has none" even
    /// though decode itself was correct -- this decoder never copied the
    /// triggering packet's `pts` onto its output frame. One packet always
    /// decodes to exactly one frame here (no cross-packet reorder delay,
    /// per this impl's own drain comment), so the mapping is exact.
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
}
