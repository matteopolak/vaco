//! [`OpusDecoder`]: the top-level [`Decoder`] this crate registers. Wires
//! together CELT, SILK, the hybrid combination (RFC 6716 §4.5) and
//! multistream/surround (RFC 7845 §5) on top of `vaco-parse-opus`'s
//! packet framing.

use std::collections::VecDeque;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Decoder;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_opus::{Bandwidth, IdentificationHeader, Mode, OpusPacket, OUTPUT_SAMPLE_RATE};
use vaco_sampfmt::SampleFmt;

use crate::celt::CeltDecoder;
use crate::silk::{InternalRate, SilkDecoder};

/// CELT's end band for a given Opus bandwidth. `opus_decoder.c`'s
/// bandwidth-to-`CELT_SET_END_BAND` mapping (measured against the
/// reference's own behaviour, not RFC prose, which states the four
/// bandwidth cutoffs but not this exact table).
fn celt_end_band(bw: Bandwidth) -> usize {
    match bw {
        Bandwidth::Narrowband => 13,
        Bandwidth::Mediumband | Bandwidth::Wideband => 17,
        Bandwidth::SuperWideband => 19,
        Bandwidth::Fullband => 21,
    }
}

/// The band CELT starts at inside a hybrid frame — the SILK/CELT crossover
/// at 8 kHz. RFC 6716 §4.5.
const HYBRID_START_BAND: usize = 17;

fn silk_rate_for_bandwidth(bw: Bandwidth, hybrid: bool) -> InternalRate {
    if hybrid {
        return InternalRate::Wideband;
    }
    match bw {
        Bandwidth::Narrowband => InternalRate::Narrowband,
        Bandwidth::Mediumband => InternalRate::Mediumband,
        _ => InternalRate::Wideband,
    }
}

/// One embedded elementary Opus stream (RFC 7845 §5.1.1): 1 or 2 coded
/// channels, its own persistent CELT/SILK/resampler state.
#[derive(Debug)]
struct StreamDecoder {
    /// The stream's declared/negotiated output channel count (from the
    /// mapping table) -- fixed for the stream's lifetime, and the width of
    /// every PCM buffer this type hands back.
    channels: usize,
    /// The *coded* channel count SILK/CELT were last configured for --
    /// `Toc::coded_channels()`'s per-packet stereo flag (RFC 6716 SS3.1),
    /// not `channels` above. A "coupled" (2-channel) stream can still code
    /// an individual packet mono (e.g. during silence), and SILK/CELT then
    /// only carry that packet's worth of bits for one channel -- decoding
    /// with the stream's fixed channel count instead of the packet's own
    /// desyncs every subsequent field for the rest of the packet.
    coded_channels: usize,
    celt: CeltDecoder,
    silk: SilkDecoder,
    silk_ready: bool,
    resamplers: Vec<crate::silk::resample::Upsampler>,
}

impl StreamDecoder {
    fn new(channels: usize) -> Self {
        let channels = channels.clamp(1, 2);
        Self {
            channels,
            coded_channels: channels,
            celt: CeltDecoder::new(channels),
            silk: SilkDecoder::new(channels, InternalRate::Wideband, 20),
            silk_ready: false,
            resamplers: Vec::new(),
        }
    }

    fn ensure_silk(&mut self, channels: usize, rate: InternalRate, frame_ms: u32) {
        if !self.silk_ready || self.silk.internal_khz() != rate.khz() || self.coded_channels != channels {
            self.silk.reconfigure(channels, rate, frame_ms);
            self.silk_ready = true;
            self.coded_channels = channels;
            let factor = (48 / rate.khz()).max(1) as usize;
            self.resamplers = (0..channels).map(|_| crate::silk::resample::Upsampler::new(factor)).collect();
        }
    }

    /// Decode one Opus packet's worth of frames (already split by TOC/code)
    /// for this stream, returning per-channel PCM at 48 kHz, normalized.
    fn decode_packet(&mut self, packet: &OpusPacket<'_>) -> Vec<Vec<f32>> {
        let toc = packet.toc;
        let mut out: Vec<Vec<f32>> = vec![Vec::new(); self.channels];
        for frame in &packet.frames {
            let pcm = self.decode_one_frame(toc, frame);
            for (dst, src) in out.iter_mut().zip(pcm) {
                dst.extend(src);
            }
        }
        out
    }

    fn decode_one_frame(&mut self, toc: vaco_parse_opus::Toc, payload: &[u8]) -> Vec<Vec<f32>> {
        if payload.is_empty() {
            let n = toc.frame_samples() as usize;
            return (0..self.channels).map(|_| vec![0.0f32; n]).collect();
        }
        let mut dec = crate::range::RangeDecoder::new(payload);
        // The *packet's own* coded channel count (RFC 6716 SS3.1's TOC
        // stereo flag), not the stream's fixed `self.channels` -- a
        // "coupled" stream can still code an individual packet mono, and
        // SILK/CELT must decode exactly as many channels' worth of fields
        // as this packet actually carries.
        let channels = toc.coded_channels().clamp(1, 2) as usize;
        let bandwidth = toc.bandwidth();
        let frame_samples = toc.frame_samples() as usize;

        let pcm = match toc.mode() {
            Mode::CeltOnly => self.celt.decode(&mut dec, payload.len(), frame_samples, channels, 0, celt_end_band(bandwidth)),
            Mode::SilkOnly => {
                let rate = silk_rate_for_bandwidth(bandwidth, false);
                let frame_ms = (frame_samples / 48).max(10) as u32;
                self.ensure_silk(channels, rate, frame_ms);
                let silk_pcm = self.silk.decode(&mut dec, frame_ms);
                self.resample_and_normalize(&silk_pcm)
            }
            Mode::Hybrid => {
                let rate = silk_rate_for_bandwidth(bandwidth, true);
                let frame_ms = (frame_samples / 48).max(10) as u32;
                self.ensure_silk(channels, rate, frame_ms);
                let silk_pcm = self.silk.decode(&mut dec, frame_ms);
                let silk_48k = self.resample_and_normalize(&silk_pcm);
                let celt_48k = self.celt.decode(&mut dec, payload.len(), frame_samples, channels, HYBRID_START_BAND, celt_end_band(bandwidth));
                let mut mixed = Vec::new();
                for c in 0..channels {
                    let s = silk_48k.get(c).map_or(&[][..], Vec::as_slice);
                    let ce = celt_48k.get(c).map_or(&[][..], Vec::as_slice);
                    let n = frame_samples;
                    let mut v = vec![0.0f32; n];
                    for (i, slot) in v.iter_mut().enumerate().take(n) {
                        *slot = s.get(i).copied().unwrap_or(0.0) + ce.get(i).copied().unwrap_or(0.0);
                    }
                    mixed.push(v);
                }
                mixed
            }
        };

        // `dec_API.c`'s "Create two channel output from mono stream": a
        // mono-coded packet inside a coupled stream duplicates its one
        // decoded channel into both outputs, rather than leaving the
        // second output silent.
        if self.channels == 2 && pcm.len() == 1 {
            let mono = pcm.into_iter().next().unwrap_or_default();
            vec![mono.clone(), mono]
        } else {
            pcm
        }
    }

    fn resample_and_normalize(&mut self, silk_pcm: &[Vec<f32>]) -> Vec<Vec<f32>> {
        silk_pcm
            .iter()
            .enumerate()
            .map(|(c, ch)| {
                let normalized: Vec<f32> = ch.iter().map(|&v| v / 32768.0).collect();
                if let Some(r) = self.resamplers.get_mut(c) { r.process(&normalized) } else { normalized }
            })
            .collect()
    }

    fn flush(&mut self) {
        self.celt.reset();
        self.silk_ready = false;
    }
}

/// The Opus decoder. Handles mapping family 0 (mono/stereo, one embedded
/// stream) and families 1/2/255 (multistream/surround, RFC 7845 §5.1.1)
/// alike, since [`vaco_parse_opus::OpusParser::split_streams`] already
/// does the hard part of locating each embedded stream's bytes.
#[derive(Debug)]
pub struct OpusDecoder {
    budget: Budget,
    head: Option<IdentificationHeader>,
    streams: Vec<StreamDecoder>,
    pending: VecDeque<Frame>,
    /// `Error::Eof` once draining starts and `pending` is empty, rather than
    /// `NeedMoreInput` forever — the same fix `vaco-codec-mpegaudio`'s and
    /// (this session) `vaco-codec-flac`'s decoders carry. Before this field
    /// existed, `send_packet(None)` returned `Err(Error::Eof)` directly,
    /// which is the wrong half of the contract (`Decoder::send_packet`'s own
    /// doc: only `receive_frame` answers `Eof`) and read to the scheduler as
    /// a hard failure rather than "start draining" — measured end to end via
    /// `vaco -i <opus-in-ogg> -f null -`, which reported "Error while
    /// filtering: end of stream" instead of decoding, before this fix.
    draining: bool,
}

impl OpusDecoder {
    /// A decoder bounded by `limits`, with no identification header yet —
    /// [`Decoder::set_extradata`] must supply one before the first packet,
    /// exactly as `vaco-parse-opus`'s own parser requires (Opus carries no
    /// in-band configuration at all).
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits),
            head: None,
            streams: Vec::new(),
            pending: VecDeque::new(),
            draining: false,
        }
    }

    fn ensure_streams(&mut self, head: &IdentificationHeader) {
        let stream_count = usize::from(head.stream_count.max(1));
        let coupled = usize::from(head.coupled_count);
        if self.streams.len() == stream_count {
            return;
        }
        self.streams = (0..stream_count).map(|i| StreamDecoder::new(if i < coupled { 2 } else { 1 })).collect();
    }

    fn mix_to_output(head: &IdentificationHeader, per_stream: &[Vec<Vec<f32>>]) -> (Vec<Vec<f32>>, usize) {
        let out_channels = usize::from(head.channel_count).max(1);
        let samples = per_stream.iter().flat_map(|s| s.iter().map(Vec::len)).max().unwrap_or(0);
        let mut out = vec![vec![0.0f32; samples]; out_channels];

        if head.mapping_family.has_mapping_table() && !head.channel_mapping.is_empty() {
            for (out_ch, &src) in head.channel_mapping.iter().enumerate() {
                if src == 255 || out_ch >= out_channels {
                    continue; // silent output channel
                }
                // `src` indexes the flattened per-stream decoded-channel
                // sequence: stream 0's channels first, then stream 1's, ...
                let mut remaining = usize::from(src);
                for stream in per_stream {
                    if remaining < stream.len() {
                        if let (Some(dst), Some(s)) = (out.get_mut(out_ch), stream.get(remaining)) {
                            let n = dst.len().min(s.len());
                            dst[..n].copy_from_slice(&s[..n]);
                        }
                        break;
                    }
                    remaining -= stream.len();
                }
            }
        } else {
            // Family 0: one stream, its channels map straight through.
            if let Some(stream) = per_stream.first() {
                for (ch, data) in stream.iter().enumerate() {
                    if let Some(dst) = out.get_mut(ch) {
                        let n = dst.len().min(data.len());
                        dst[..n].copy_from_slice(&data[..n]);
                    }
                }
            }
        }
        (out, samples)
    }
}

impl Decoder for OpusDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            self.draining = true;
            return Ok(());
        };
        let pts = packet.pts;
        let Some(head) = self.head.clone() else {
            return Err(Error::Unsupported(
                "vaco-codec-opus: no OpusHead identification header supplied via set_extradata",
            ));
        };
        self.ensure_streams(&head);

        let payload = packet.payload();
        let stream_count = self.streams.len();
        let per_stream: Vec<Vec<Vec<f32>>> = if stream_count <= 1 {
            let parsed = OpusPacket::parse(payload).map_err(|_| Error::InvalidData("vaco-codec-opus: malformed Opus packet"))?;
            let pcm = self.streams.first_mut().map(|s| s.decode_packet(&parsed)).unwrap_or_default();
            vec![pcm]
        } else {
            let split = vaco_parse_opus::split_streams(payload, stream_count)
                .map_err(|_| Error::InvalidData("vaco-codec-opus: malformed multistream Opus packet"))?;
            split
                .iter()
                .zip(self.streams.iter_mut())
                .map(|(pkt, stream)| stream.decode_packet(pkt))
                .collect()
        };

        let (mixed, samples) = Self::mix_to_output(&head, &per_stream);
        let layout = head
            .channel_layout()
            .unwrap_or_else(|| ChannelLayout::unspecified(u32::from(head.channel_count)));
        let mut frame = Frame::alloc_audio(&mut self.budget, SampleFmt::F32P, layout, samples as u32, OUTPUT_SAMPLE_RATE)?;
        frame.pts = pts;
        // The decode-side mirror of this session's audio-decoder duration
        // audit (`vaco-codec-pcm`/`-adpcm`/`-simple-audio`/`-vorbis`/
        // `-ac3`/`-aac`/`-mpegaudio`): `samples`/`OUTPUT_SAMPLE_RATE` were
        // already in scope, but `frame.duration` was never set. Unrelated
        // to, and does not touch, the known stereo-amplitude bug that
        // keeps this crate deliberately unregistered
        // (`xtask::reachability_check::ALLOW_ORPHAN_CRATE`) -- this only
        // affects the frame's own duration metadata, not decoded sample
        // values.
        let time_base = Rational::new(1, i32::try_from(OUTPUT_SAMPLE_RATE).unwrap_or(1).max(1));
        frame.duration = Timestamp::new(i64::try_from(samples).unwrap_or(0))
            .to_duration(time_base)
            .unwrap_or(Duration::ZERO);
        for (ch, data) in mixed.iter().enumerate() {
            let Some(mut plane) = frame.plane_mut(ch) else { continue };
            let Some(row) = plane.row_mut(0) else { continue };
            for (i, &v) in data.iter().enumerate() {
                let bytes = v.to_le_bytes();
                if let Some(dst) = row.get_mut(i * 4..i * 4 + 4) {
                    dst.copy_from_slice(&bytes);
                }
            }
        }
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
        for s in &mut self.streams {
            s.flush();
        }
        self.pending.clear();
        self.draining = false;
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        let head = IdentificationHeader::parse(extradata)?;
        self.ensure_streams(&head);
        self.head = Some(head);
        Ok(())
    }
}
