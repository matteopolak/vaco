//! Small, open-spec audio codecs that share no algorithm with each other:
//! SBC, `DFPWM1a`, QOA (Quite OK Audio) and RFC 3389 comfort noise. Grouped in one
//! crate the way `vaco-codec-image-simple` groups unrelated trivial image
//! formats — each is too small to earn its own crate, and none is a
//! variant of another. **Three of four are real, registered codecs; DFPWM is
//! not** — see [`dfpwm`]'s module doc before assuming otherwise.
//!
//! # How it works
//!
//! [`sbc`], [`dfpwm`], [`qoa`] and [`comfortnoise`] each own their decode/encode
//! functions; this file only wraps them in the `Machine`-backed
//! `SendReceive` shape every codec in this tree uses (see
//! `vaco-codec-adpcm`/`vaco-codec-pcm` for the same pattern) and registers
//! [`CodecId::Sbc`], [`CodecId::Qoa`] and [`CodecId::ComfortNoise`] — not
//! [`CodecId::Dfpwm`], whose wrappers always return
//! [`vaco_core::Error::Unsupported`].
//!
//! # How to change it
//!
//! A new codec this small belongs here as its own module plus a pair of
//! `SendReceive` wrappers below, following whichever existing codec's
//! packetisation is closest: [`sbc`]/[`dfpwm`] for "state persists across an
//! unbounded byte stream", [`qoa`]/[`comfortnoise`] for "one packet is one
//! self-contained unit".
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds every allocation. DFPWM and comfort noise
//! have no self-describing sample rate/channel layout (comfort noise has no
//! *duration* either — see [`comfortnoise`]'s module doc); QOA's frame
//! header supplies both directly, so it needs no external configuration at
//! all. SBC also carries both in every frame header.

#![forbid(unsafe_code)]

pub mod comfortnoise;
pub mod dfpwm;
pub mod qoa;
pub mod sbc;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{
    Accept, AsDecoder, AsEncoder, Caps, CodecId, Decoder, DecoderDesc, Encoder, EncoderDesc,
    Machine, SendReceive, Validated,
};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn i16_samples_to_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    for s in samples {
        out.extend_from_slice(&s.to_ne_bytes());
    }
    out
}

fn bytes_to_i16_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .filter_map(|c| <[u8; 2]>::try_from(c).ok())
        .map(i16::from_ne_bytes)
        .collect()
}

/// Build an interleaved `S16` audio frame from decoded samples.
#[allow(
    clippy::integer_division,
    reason = "interleaved sample count divided by channel count is an exact floor division \
              by construction (every caller already deinterleaves evenly)"
)]
fn frame_from_interleaved(
    limits: &Limits,
    samples: &[i16],
    channels: u32,
    sample_rate: u32,
    pts: vaco_core::Timestamp,
) -> Result<Frame> {
    let mut budget = Budget::new(limits.clone());
    let layout = ChannelLayout::default_for(channels.max(1))
        .unwrap_or(ChannelLayout::unspecified(channels.max(1)));
    let count = samples.len() as u32 / channels.max(1);
    let mut frame = Frame::alloc_audio(
        &mut budget,
        vaco_sampfmt::SampleFmt::S16,
        layout,
        count,
        sample_rate,
    )?;
    let FrameData::Audio { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("simple-audio: expected an audio frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("simple-audio: no plane 0"))?;
    let bytes = i16_samples_to_bytes(samples);
    let buf = plane.data.make_mut();
    let dst = buf
        .get_mut(..bytes.len().min(buf.len()))
        .ok_or(Error::InvalidData("simple-audio: plane too short"))?;
    let src = bytes.get(..dst.len()).unwrap_or(&[]);
    dst.copy_from_slice(src);
    frame.pts = pts;
    // The decode-side mirror of the duration fixes already applied to
    // this crate's own `QoaEncoder`/`ComfortNoiseEncoder` above, and to
    // `vaco-codec-pcm`/`vaco-codec-adpcm`'s decoders: `count`/`sample_rate`
    // are already in scope here, but this shared helper (used by both
    // real decoders in this crate) never set `frame.duration`.
    let time_base = Rational::new(1, i32::try_from(sample_rate).unwrap_or(1).max(1));
    frame.duration = Timestamp::new(i64::from(count))
        .to_duration(time_base)
        .unwrap_or(Duration::ZERO);
    frame.flags = FrameFlags::KEY;
    Ok(frame)
}

/// Copy a decoded `S16` audio frame's interleaved samples and channel count
/// out as `(Vec<i16>, channels)`.
fn interleaved_owned(frame: &Frame) -> Result<(Vec<i16>, u32)> {
    let FrameData::Audio { planes, layout, .. } = &frame.data else {
        return Err(Error::InvalidData("simple-audio: expected an audio frame"));
    };
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("simple-audio: no plane 0"))?;
    Ok((bytes_to_i16_samples(plane.data.as_slice()), layout.channels))
}

// ---------------------------------------------------------------- DFPWM1a

/// **Not implemented as a real codec.** [`dfpwm`]'s module doc ("Not wired
/// up as the `dfpwm` codec") records the measurement: this predictor,
/// transcribed exactly from the only public `DFPWM1a` write-up available,
/// does not reproduce `ffmpeg 8.1`'s actual decode of a real `.dfpwm`
/// stream. [`DfpwmDecoder`]/[`DfpwmEncoder`] exist only to refuse loudly and
/// are deliberately **not** registered in `vaco-component.toml` — matching
/// `vaco-codec-adpcm`'s `AdpcmG722Decoder`/`AdpcmG726Decoder` shape for the
/// identical reason.
#[derive(Debug)]
pub struct DfpwmDecoder {
    machine: Machine<Frame>,
}

impl DfpwmDecoder {
    #[must_use]
    pub fn new(_limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
        }
    }
}

impl SendReceive for DfpwmDecoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, _input: Option<&Packet>) -> Result<()> {
        Err(Error::Unsupported(
            "dfpwm: this crate's predictor does not reproduce ffmpeg's real DFPWM1a decode \
             (measured; see the dfpwm module's doc)",
        ))
    }
    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

/// **Not implemented as a real codec** — see [`DfpwmDecoder`].
#[derive(Debug)]
pub struct DfpwmEncoder {
    machine: Machine<Packet>,
}

impl DfpwmEncoder {
    #[must_use]
    pub fn new(_limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
        }
    }
}

impl SendReceive for DfpwmEncoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, _input: Option<&Frame>) -> Result<()> {
        Err(Error::Unsupported(
            "dfpwm: this crate's predictor does not reproduce ffmpeg's real DFPWM1a decode \
             (measured; see the dfpwm module's doc)",
        ))
    }
    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

// -------------------------------------------------------------------- QOA

#[derive(Debug)]
pub struct QoaDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl QoaDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl SendReceive for QoaDecoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else { return Ok(()) };
                let mut budget = Budget::new(self.limits.clone());
                let decoded = qoa::decode(&mut budget, pkt.payload())?;
                let frame = frame_from_interleaved(
                    &self.limits,
                    &decoded.interleaved,
                    decoded.num_channels,
                    decoded.sample_rate,
                    pkt.pts,
                )?;
                self.machine.emit(frame);
                Ok(())
            }
        }
    }
    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

#[derive(Debug)]
pub struct QoaEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    states: Vec<qoa::LmsState>,
}

impl QoaEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES.union(Caps::VARIABLE_FRAME_SIZE)),
            limits,
            states: Vec::new(),
        }
    }
}

impl SendReceive for QoaEncoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    #[allow(
        clippy::integer_division,
        reason = "interleaved sample count divided by channel count is an exact floor division \
                  by construction (every caller already deinterleaves evenly)"
    )]
    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else { return Ok(()) };
                let (samples, channels) = interleaved_owned(frame)?;
                let FrameData::Audio { sample_rate, .. } = &frame.data else {
                    return Err(Error::InvalidData("qoa: expected an audio frame"));
                };
                let ch = channels.max(1) as usize;
                if self.states.len() != ch {
                    self.states = vec![qoa::LmsState::default(); ch];
                }
                let mut budget = Budget::new(self.limits.clone());
                let samples_per_channel = samples.len() / ch.max(1);
                let mut start = 0usize;
                // One incoming `Frame` can carry more samples than a single
                // QOA frame can hold (`SUBFRAMES`): split it into as many
                // QOA frames as needed, one packet each. An empty frame
                // (end-of-stream flush artefact) simply emits nothing.
                while start < samples_per_channel {
                    let take = samples_per_channel
                        .saturating_sub(start)
                        .min(qoa::MAX_SLICES_PER_FRAME * qoa::SLICE_SAMPLES);
                    let chunk = samples.get(start * ch..(start + take) * ch).unwrap_or(&[]);
                    let wire = qoa::encode(
                        &mut budget,
                        &mut self.states,
                        channels.max(1),
                        *sample_rate,
                        chunk,
                    )?;
                    let mut packet = Packet::from_slice(&mut budget, &wire)?;
                    packet.pts = frame.pts;
                    // Same bug class as `vaco-codec-flac`/`vaco-codec-alac`/
                    // `vaco-codec-vorbis`/`vaco-codec-pcm`/`vaco-codec-adpcm`'s
                    // encoders. This one is the closest shape to FLAC/ALAC's
                    // own -- one `Frame` can split into several QOA sub-
                    // frame packets (`SUBFRAMES`), and the last chunk is
                    // often shorter than `take`'s cap -- so, like those two,
                    // duration must come from the chunk this iteration
                    // actually encoded (`take`, already the real remaining-
                    // sample count via `saturating_sub`), never a fixed
                    // per-packet constant.
                    let time_base =
                        Rational::new(1, i32::try_from(*sample_rate).unwrap_or(1).max(1));
                    packet.duration = Timestamp::new(i64::try_from(take).unwrap_or(0))
                        .to_duration(time_base)
                        .unwrap_or(Duration::ZERO);
                    packet.set_duration_ts(i64::try_from(take).unwrap_or(0));
                    self.machine.emit(packet);
                    start += take.max(1);
                }
                Ok(())
            }
        }
    }
    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
        self.states.clear();
    }
}

// ------------------------------------------------------------ comfort noise

#[derive(Debug, Clone, Copy)]
struct ComfortNoiseConfig {
    sample_rate: u32,
    frame_samples: u32,
    seed: u64,
    order: usize,
}

impl Default for ComfortNoiseConfig {
    fn default() -> Self {
        let d = comfortnoise::Config::default();
        Self {
            sample_rate: d.sample_rate,
            frame_samples: d.frame_samples,
            seed: d.seed,
            order: 8,
        }
    }
}

#[derive(Debug)]
pub struct ComfortNoiseDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    cfg: ComfortNoiseConfig,
    generator: comfortnoise::Generator,
}

impl ComfortNoiseDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        let cfg = ComfortNoiseConfig::default();
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            generator: comfortnoise::Generator::new(cfg.seed),
            cfg,
        }
    }

    #[must_use]
    pub fn with_sample_rate_and_frame_samples(
        mut self,
        sample_rate: u32,
        frame_samples: u32,
    ) -> Self {
        if sample_rate > 0 {
            self.cfg.sample_rate = sample_rate;
        }
        if frame_samples > 0 {
            self.cfg.frame_samples = frame_samples;
        }
        self
    }
}

impl SendReceive for ComfortNoiseDecoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else { return Ok(()) };
                let mut budget = Budget::new(self.limits.clone());
                let sid = comfortnoise::parse(&mut budget, pkt.payload())?;
                let samples = self
                    .generator
                    .generate(&mut budget, &sid, self.cfg.frame_samples)?;
                let frame = frame_from_interleaved(
                    &self.limits,
                    &samples,
                    1,
                    self.cfg.sample_rate,
                    pkt.pts,
                )?;
                self.machine.emit(frame);
                Ok(())
            }
        }
    }
    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

#[derive(Debug)]
pub struct ComfortNoiseEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    order: usize,
}

impl ComfortNoiseEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            order: ComfortNoiseConfig::default().order,
        }
    }
}

impl SendReceive for ComfortNoiseEncoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else { return Ok(()) };
                let (samples, channels) = interleaved_owned(frame)?;
                if channels > 1 {
                    return Err(Error::Unsupported(
                        "comfortnoise: mono only (RFC 3389 has no multi-channel convention)",
                    ));
                }
                let sid = comfortnoise::analyze(&samples, self.order)?;
                let wire = comfortnoise::build(sid.level, &sid.reflection);
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &wire)?;
                packet.pts = frame.pts;
                // Same bug class as this crate's own `QoaEncoder` above.
                // One `Frame` is exactly one SID packet here (mono-only,
                // enforced above), so `samples.len()` is already the whole
                // frame's sample count.
                let FrameData::Audio { sample_rate, .. } = &frame.data else {
                    return Err(Error::InvalidData("comfortnoise: expected an audio frame"));
                };
                let time_base = Rational::new(1, i32::try_from(*sample_rate).unwrap_or(1).max(1));
                packet.duration = Timestamp::new(i64::try_from(samples.len()).unwrap_or(0))
                    .to_duration(time_base)
                    .unwrap_or(Duration::ZERO);
                packet.set_duration_ts(i64::try_from(samples.len()).unwrap_or(0));
                self.machine.emit(packet);
                Ok(())
            }
        }
    }
    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

// -------------------------------------------------------------------- SBC

#[derive(Debug)]
pub struct SbcDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    state: sbc::DecoderState,
}

impl SbcDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            state: sbc::DecoderState::default(),
        }
    }
}

impl SendReceive for SbcDecoder {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(packet) = input else { return Ok(()) };
                let decoded = sbc::decode(
                    &mut Budget::new(self.limits.clone()),
                    &mut self.state,
                    packet.payload(),
                )?;
                let frame = frame_from_interleaved(
                    &self.limits,
                    &decoded.interleaved,
                    decoded.channels,
                    decoded.sample_rate,
                    packet.pts,
                )?;
                self.machine.emit(frame);
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.state = sbc::DecoderState::default();
    }
}

// ------------------------------------------------------------- registration

fn make_dfpwm_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(DfpwmDecoder::new(limits))))
}
fn make_dfpwm_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(DfpwmEncoder::new(limits))))
}
fn make_qoa_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(QoaDecoder::new(limits))))
}
fn make_qoa_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(QoaEncoder::new(limits))))
}
fn make_comfortnoise_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(ComfortNoiseDecoder::new(limits))))
}
fn make_comfortnoise_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(ComfortNoiseEncoder::new(limits))))
}
fn make_sbc_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(SbcDecoder::new(limits))))
}

/// **Not listed in `vaco-component.toml`** — always returns
/// [`vaco_core::Error::Unsupported`]; see [`DfpwmDecoder`]'s doc. Kept as a
/// compilable identity for whoever finds the real recursion next.
pub static DFPWM_DECODER: DecoderDesc = DecoderDesc {
    name: "dfpwm",
    long_name: "DFPWM (Dynamic Filter Pulse Width Modulation)",
    id: CodecId::Dfpwm,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_dfpwm_decoder,
};
/// **Not listed in `vaco-component.toml`** — see [`DFPWM_DECODER`].
pub static DFPWM_ENCODER: EncoderDesc = EncoderDesc {
    name: "dfpwm",
    long_name: "DFPWM (Dynamic Filter Pulse Width Modulation)",
    id: CodecId::Dfpwm,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_dfpwm_encoder,
};
pub static QOA_DECODER: DecoderDesc = DecoderDesc {
    name: "qoa",
    long_name: "QOA (Quite OK Audio)",
    id: CodecId::Qoa,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_qoa_decoder,
};
pub static QOA_ENCODER: EncoderDesc = EncoderDesc {
    name: "qoa",
    long_name: "QOA (Quite OK Audio)",
    id: CodecId::Qoa,
    media_type: MediaType::Audio,
    caps: Caps::SUBFRAMES.union(Caps::VARIABLE_FRAME_SIZE),
    supported_rates: &[],
    make: make_qoa_encoder,
};
pub static COMFORTNOISE_DECODER: DecoderDesc = DecoderDesc {
    name: "comfortnoise",
    long_name: "RFC 3389 Comfort Noise",
    id: CodecId::ComfortNoise,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_comfortnoise_decoder,
};
pub static COMFORTNOISE_ENCODER: EncoderDesc = EncoderDesc {
    name: "comfortnoise",
    long_name: "RFC 3389 Comfort Noise",
    id: CodecId::ComfortNoise,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_comfortnoise_encoder,
};
pub static SBC_DECODER: DecoderDesc = DecoderDesc {
    name: "sbc",
    long_name: "Bluetooth Low Complexity Subband Codec",
    id: CodecId::Sbc,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_sbc_decoder,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    reason = "test code exercising the decoder, not the untrusted-input surface"
)]
mod tests {
    use super::*;
    use vaco_core::Error;

    fn tone(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| ((i as f64 * 0.2).sin() * 6000.0) as i16)
            .collect()
    }

    fn frame_of(samples: &[i16], channels: u32, sample_rate: u32) -> Frame {
        frame_from_interleaved(
            &Limits::permissive(),
            samples,
            channels,
            sample_rate,
            vaco_core::Timestamp::new(0),
        )
        .unwrap()
    }

    #[test]
    fn dfpwm_decoder_and_encoder_refuse_rather_than_produce_wrong_output() {
        // No real ITU/community DFPWM1a recursion is implemented (see
        // dfpwm's module doc) — the wrapper must fail loudly, never hand
        // back plausible-looking wrong samples.
        let mut enc = DfpwmEncoder::new(Limits::permissive());
        let frame = frame_of(&tone(16), 1, 48_000);
        assert!(matches!(enc.send(Some(&frame)), Err(Error::Unsupported(_))));
        let mut dec = DfpwmDecoder::new(Limits::permissive());
        let dummy = Packet::from_slice(&mut Budget::new(Limits::permissive()), &[0u8; 4]).unwrap();
        assert!(matches!(dec.send(Some(&dummy)), Err(Error::Unsupported(_))));
    }

    #[test]
    fn qoa_send_receive_round_trips_stereo() {
        let mut enc = QoaEncoder::new(Limits::permissive());
        let mut dec = QoaDecoder::new(Limits::permissive());
        let samples = tone(600 * 2);
        enc.send(Some(&frame_of(&samples, 2, 44_100))).unwrap();
        let pkt = enc.receive().unwrap();
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();
        let (decoded, channels) = interleaved_owned(&frame).unwrap();
        assert_eq!(channels, 2);
        assert_eq!(decoded.len(), samples.len());
    }

    /// The decode-side mirror of `every_qoa_packet_duration_sums_to_the_
    /// input_including_a_short_last_one` above: `frame_from_interleaved`
    /// (shared by `QoaDecoder` and `ComfortNoiseDecoder`) never set
    /// `frame.duration`, even though `count`/`sample_rate` were already in
    /// scope. QOA's own header carries sample rate, so this needs no
    /// manual configuration the way ADPCM's decode-side test did.
    #[test]
    fn qoa_decode_sets_a_real_nonzero_frame_duration() {
        let mut enc = QoaEncoder::new(Limits::permissive());
        let mut dec = QoaDecoder::new(Limits::permissive());
        let samples = tone(600 * 2);
        enc.send(Some(&frame_of(&samples, 2, 44_100))).unwrap();
        let pkt = enc.receive().unwrap();
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();

        // 600 samples per channel at 44100 Hz.
        let expected = Timestamp::new(600)
            .to_duration(Rational::new(1, 44_100))
            .unwrap();
        assert_ne!(expected, Duration::ZERO);
        assert_eq!(frame.duration, expected);
    }

    #[test]
    fn qoa_splits_a_frame_larger_than_one_qoa_frame_into_several_packets() {
        let mut enc = QoaEncoder::new(Limits::permissive());
        let big = tone(qoa::MAX_SLICES_PER_FRAME * qoa::SLICE_SAMPLES * 2 + 10);
        enc.send(Some(&frame_of(&big, 1, 8_000))).unwrap();
        let mut packets = 0;
        loop {
            match enc.receive() {
                Ok(_) => packets += 1,
                Err(Error::NeedMoreInput | Error::OutputPending) => break,
                Err(e) => panic!("unexpected: {e:?}"),
            }
        }
        assert!(
            packets >= 3,
            "expected at least 3 QOA frames, got {packets}"
        );
    }

    /// Same bug class as `vaco-codec-flac`/`vaco-codec-alac`'s encoders,
    /// same shape: `QoaEncoder` splits one `Frame` into several packets
    /// (the test above), and the *last* one is short -- exactly where a
    /// fixed per-packet duration constant would have been wrong, and
    /// where FLAC/ALAC's own undercount hid. Every packet's duration must
    /// sum to the whole frame's real sample count, with the last packet
    /// short by precisely the total's remainder, not padded or truncated
    /// to a round number.
    #[test]
    fn every_qoa_packet_duration_sums_to_the_input_including_a_short_last_one() {
        let mut enc = QoaEncoder::new(Limits::permissive());
        let one_frame = qoa::MAX_SLICES_PER_FRAME * qoa::SLICE_SAMPLES;
        let total_samples = one_frame * 2 + 10; // two full QOA frames + a short third
        let big = tone(total_samples);
        enc.send(Some(&frame_of(&big, 1, 8_000))).unwrap();

        let mut durations = Vec::new();
        loop {
            match enc.receive() {
                Ok(p) => durations.push(p.duration),
                Err(Error::NeedMoreInput | Error::OutputPending) => break,
                Err(e) => panic!("unexpected: {e:?}"),
            }
        }
        assert_eq!(durations.len(), 3, "two full QOA frames plus one short one");
        for d in &durations {
            assert_ne!(
                *d,
                Duration::ZERO,
                "every packet must carry a real duration"
            );
        }
        let time_base = Rational::new(1, 8_000);
        let full = Timestamp::new(i64::try_from(one_frame).unwrap())
            .to_duration(time_base)
            .unwrap();
        let last = Timestamp::new(10).to_duration(time_base).unwrap();
        assert_eq!(durations, vec![full, full, last]);
    }

    #[test]
    fn comfortnoise_send_receive_produces_the_configured_frame_length() {
        let mut enc = ComfortNoiseEncoder::new(Limits::permissive());
        let mut dec = ComfortNoiseDecoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&tone(400), 1, 8_000))).unwrap();
        let pkt = enc.receive().unwrap();
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();
        let (decoded, channels) = interleaved_owned(&frame).unwrap();
        assert_eq!(channels, 1);
        assert_eq!(
            decoded.len(),
            ComfortNoiseConfig::default().frame_samples as usize
        );
    }

    /// Same bug class as `vaco-codec-flac`/`vaco-codec-alac`/
    /// `vaco-codec-vorbis`/`vaco-codec-pcm`/`vaco-codec-adpcm`'s encoders.
    /// `ComfortNoiseEncoder` is 1:1 (one frame, one SID packet), so this
    /// only needs to check the duration is real and matches the frame's
    /// own sample count -- no separate short-final-packet shape applies.
    #[test]
    fn comfortnoise_send_frame_sets_a_real_nonzero_packet_duration() {
        let mut enc = ComfortNoiseEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&tone(400), 1, 8_000))).unwrap();
        let pkt = enc.receive().unwrap();
        let expected = Timestamp::new(400)
            .to_duration(Rational::new(1, 8_000))
            .unwrap();
        assert_ne!(expected, Duration::ZERO);
        assert_eq!(pkt.duration, expected);
    }

    #[test]
    fn protocol_shape_is_consistent_across_all_three_codecs() {
        let mut dec = QoaDecoder::new(Limits::permissive());
        let mut enc = QoaEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&tone(40), 1, 8_000))).unwrap();
        let pkt = enc.receive().unwrap();
        dec.send(Some(&pkt)).unwrap();
        let _ = dec.receive().unwrap();
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).unwrap();
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    #[test]
    fn all_seven_descriptors_compile_but_only_five_are_registered() {
        // All seven static descriptors exist (dfpwm kept as a compilable
        // identity per its own doc), but only the five backing real
        // implementations (qoa, comfortnoise, sbc) are listed in
        // vaco-component.toml — checked structurally here since the toml
        // file itself isn't parsed by this test.
        let decoders: &[&DecoderDesc] = &[
            &DFPWM_DECODER,
            &QOA_DECODER,
            &COMFORTNOISE_DECODER,
            &SBC_DECODER,
        ];
        let encoders: &[&EncoderDesc] = &[&DFPWM_ENCODER, &QOA_ENCODER, &COMFORTNOISE_ENCODER];
        assert_eq!(decoders.len(), 4);
        assert_eq!(encoders.len(), 3);
        for d in decoders {
            assert_eq!(d.media_type, MediaType::Audio);
        }
    }
}
