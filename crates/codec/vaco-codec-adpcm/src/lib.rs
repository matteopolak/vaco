//! The standardised ADPCM subset (issue #280, C-02): G.722, G.726/le, MS,
//! SWF, IMA-WAV and IMA-QT — 7 codec identities. **5 of 7 are real,
//! registered decoders and encoders; G.722 and G.726/G.726le are not** — see
//! "What is not covered" below before assuming otherwise.
//!
//! # What it is
//!
//! Unlike PCM's single shared table (`vaco-codec-pcm`), these families are
//! genuinely different algorithms — different adaptive predictors, different
//! block framing, different bit-packing — so this crate is six small modules
//! ([`ima`], [`ms`], [`swf`], [`g726`], [`g722`]) each owning one family's
//! pure decode/encode functions, plus the `SendReceive` wrappers and
//! registrations in this file. `ima` covers both `adpcm_ima_wav` and
//! `adpcm_ima_qt` (same nibble codec, different container framing).
//!
//! # How it works
//!
//! Every registered family follows the same `Machine`-backed `SendReceive`
//! shape `vaco-codec-pcm`/`vaco-codec-qoi` use, treating one packet as one
//! block — a real container typically does too, but a caller free to choose
//! its own packetisation should keep block boundaries aligned with codec
//! boundaries, since state does not carry across a `send` call.
//!
//! Like `vaco-codec-pcm`, none of these codecs' bitstreams self-describe a
//! sample rate or channel count (a block self-describes its own *codec*
//! state — predictor, step index — but never the *container* facts). See
//! [`parse_audio_extradata`], copied from `vaco-codec-pcm`'s identical
//! mechanism (no shared crate exists yet for this small a helper; duplicated
//! rather than introducing a dependency for five lines).
//!
//! # How to change it
//!
//! A new *standardised* ADPCM variant gets its own module here, following
//! whichever existing family's shape is closest (a block-header family looks
//! like [`ima`]/[`ms`]). The ~30 game-specific ADPCM variants the roadmap
//! explicitly excludes (plan 15 §4.9) do not belong in this crate — they are
//! T4/T5 per that plan's own triage. Implementing the *real* ITU-T G.722/
//! G.726 predictors is exactly this shape of task; [`g722`]/[`g726`]'s
//! module docs say precisely what is missing.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds decode allocation. Sample rate/channel
//! layout default to [`DEFAULT_SAMPLE_RATE`]/mono, until overridden the same
//! way `vaco-codec-pcm` allows.
//!
//! # What is not covered, and why
//!
//! **`adpcm_g722` and `adpcm_g726`/`adpcm_g726le` are not registered.**
//! [`g722`] and [`g726`] contain a *structurally different* transform from
//! the real ITU-T algorithms (an IMA-shaped adaptive-delta coder instead of
//! the two-pole/six-zero predictor; for G.722, a reversible two-point
//! lifting split instead of the real 24-tap QMF) — see those modules' own
//! docs for exactly what and why. Both round-trip correctly through their
//! own encoder, but neither can decode a real G.722/G.726 bitstream from
//! another encoder, and would hand back plausible-looking wrong samples with
//! no error if wired up as if they worked. The repository owner's ruling
//! (`planning/AGENT-CONSTRAINTS.md`, "byte-exactness is a check, not the
//! bar") permits small, unstructured deviation from a reference implementation
//! — not a different transform answering to the same codec name — so
//! `AdpcmG722Decoder`/`AdpcmG722Encoder`/`AdpcmG726Decoder`/`AdpcmG726Encoder`
//! in this file always return [`vaco_core::Error::Unsupported`] and carry no
//! `DecoderDesc`/`EncoderDesc` at all: there is nothing to be careful not to
//! register. IMA-WAV/IMA-QT/MS-ADPCM/SWF **are** real implementations of the
//! published algorithm and framing for each, at ordinary confidence for a
//! spec-first, unverified-against-a-real-file implementation — see the
//! closing comment on #280 for the full accounting.

#![forbid(unsafe_code)]

mod g722;
mod g726;
mod ima;
mod ms;
mod swf;
mod tables;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{
    Accept, AsDecoder, AsEncoder, Caps, CodecId, Decoder, DecoderDesc, Encoder, EncoderDesc,
    Machine, SendReceive, Validated,
};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Reference default for a block-oriented ADPCM stream with no header of its
/// own — matches `vaco-codec-pcm::DEFAULT_SAMPLE_RATE`.
pub const DEFAULT_SAMPLE_RATE: u32 = 44_100;
/// G.722's own fixed output rate, independent of anything a container states.
pub const G722_SAMPLE_RATE: u32 = 16_000;
/// G.726's ordinary telephony rate.
pub const G726_SAMPLE_RATE: u32 = 8_000;

/// See `vaco-codec-pcm::parse_audio_extradata` — the identical 5-byte
/// `(sample_rate: u32 LE, channels: u8)` provisional record, duplicated here
/// rather than shared (no crate exists yet to share it from).
fn parse_audio_extradata(extradata: &[u8]) -> Option<(u32, ChannelLayout)> {
    let &[a, b, c, d, ch] = extradata else {
        return None;
    };
    let sample_rate = u32::from_le_bytes([a, b, c, d]);
    if sample_rate == 0 || ch == 0 {
        return None;
    }
    let channels = u32::from(ch);
    let layout =
        ChannelLayout::default_for(channels).unwrap_or(ChannelLayout::unspecified(channels));
    Some((sample_rate, layout))
}

/// Build the record [`parse_audio_extradata`] reads.
#[must_use]
pub fn audio_extradata(sample_rate: u32, channels: u8) -> [u8; 5] {
    let sr = sample_rate.to_le_bytes();
    [sr[0], sr[1], sr[2], sr[3], channels]
}

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

/// Build a mono/interleaved `S16` audio frame from decoded samples.
#[allow(
    clippy::integer_division,
    reason = "interleaved sample count divided by channel count is an exact floor division \
              by construction (every caller already deinterleaves evenly)"
)]
fn frame_from_samples(
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
        return Err(Error::InvalidData("adpcm: expected an audio frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("adpcm: no plane 0"))?;
    let bytes = i16_samples_to_bytes(samples);
    let buf = plane.data.make_mut();
    let dst = buf
        .get_mut(..bytes.len().min(buf.len()))
        .ok_or(Error::InvalidData("adpcm: plane too short"))?;
    let src = bytes.get(..dst.len()).unwrap_or(&[]);
    dst.copy_from_slice(src);
    frame.pts = pts;
    // The decode-side mirror of `frame_pcm_duration` above (used by every
    // encoder in this crate): `count`/`sample_rate` are already in scope
    // here, but this shared helper -- used by all four real decoders
    // (IMA-WAV, IMA-QT, MS, SWF) -- never set `frame.duration`. Every real
    // video decoder in this tree sets `frame.duration`; this crate's own
    // decoders were the audio-side exception, same as `vaco-codec-pcm`'s.
    let time_base = Rational::new(1, i32::try_from(sample_rate).unwrap_or(1).max(1));
    frame.duration = Timestamp::new(i64::from(count))
        .to_duration(time_base)
        .unwrap_or(Duration::ZERO);
    frame.flags = FrameFlags::KEY;
    Ok(frame)
}

/// Copy a decoded `S16` audio frame's interleaved samples out as `Vec<i16>`.
///
/// A scratch copy rather than a zero-copy `&[i16]` view — this crate is
/// `#![forbid(unsafe_code)]`, so reinterpreting the plane's raw bytes in
/// place is not available, and the copy is cheap relative to the ADPCM work
/// itself.
fn frame_samples_owned(frame: &Frame) -> Result<(Vec<i16>, u32)> {
    let FrameData::Audio { planes, layout, .. } = &frame.data else {
        return Err(Error::InvalidData("adpcm: expected an audio frame"));
    };
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("adpcm: no plane 0"))?;
    Ok((bytes_to_i16_samples(plane.data.as_slice()), layout.channels))
}

/// Same bug class as `vaco-codec-flac`/`vaco-codec-alac`/`vaco-codec-vorbis`/
/// `vaco-codec-pcm`'s encoders: every ADPCM encoder in this crate is a 1:1
/// wrapper (one input `Frame` becomes exactly one `Packet`, no internal
/// block buffering) that set `Packet::pts` but never `Packet::duration`,
/// which a container deriving a track's total length from summed packet
/// durations (MP4's `stts`, Ogg's granule position) silently undercounts
/// by. `total_i16_samples`/`channels` come from the same
/// [`frame_samples_owned`] call every encoder already makes; `sample_rate`
/// is read from the frame's own [`FrameData::Audio`] rather than trusted
/// from `frame.duration`, because nothing upstream of a raw-PCM source
/// reliably sets that field either (a related, separate gap).
#[allow(
    clippy::integer_division,
    reason = "interleaved sample count divided by channel count is an exact floor division \
              by construction (every caller already deinterleaves evenly), same as \
              frame_from_samples above"
)]
fn frame_pcm_duration(frame: &Frame, total_i16_samples: usize, channels: u32) -> Duration {
    let FrameData::Audio { sample_rate, .. } = &frame.data else {
        return Duration::ZERO;
    };
    let per_channel = u32::try_from(total_i16_samples).unwrap_or(0) / channels.max(1);
    let time_base = Rational::new(1, i32::try_from(*sample_rate).unwrap_or(1).max(1));
    Timestamp::new(i64::from(per_channel))
        .to_duration(time_base)
        .unwrap_or(Duration::ZERO)
}

/// Resolve an SWF ADPCM packet's per-channel sample count.
///
/// The SWF `ADPCMPACKET` record has a fixed size: one initial sample followed
/// by 4095 codes. A non-zero packet duration is an explicit container-level
/// count for a shorter final event-sound packet; zero duration means that no
/// such count was supplied, so the fixed record size applies.
fn swf_packet_sample_count(packet: &Packet, sample_rate: u32) -> u32 {
    let default = swf::SAMPLES_PER_PACKET;
    let Some(rate) = i32::try_from(sample_rate).ok().filter(|rate| *rate > 0) else {
        return default;
    };
    packet
        .duration
        .to_ticks(Rational::new(1, rate))
        .and_then(|count| u32::try_from(count).ok())
        .filter(|count| *count > 0 && *count <= default)
        .unwrap_or(default)
}

macro_rules! adpcm_config {
    ($name:ident, $default_rate:expr) => {
        #[derive(Debug, Clone)]
        struct $name {
            sample_rate: u32,
            layout: ChannelLayout,
        }
        impl Default for $name {
            fn default() -> Self {
                Self {
                    sample_rate: $default_rate,
                    layout: ChannelLayout::MONO,
                }
            }
        }
    };
}

// ------------------------------------------------------------ IMA-WAV

adpcm_config!(ImaWavConfig, DEFAULT_SAMPLE_RATE);

#[derive(Debug)]
pub struct AdpcmImaWavDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    cfg: ImaWavConfig,
}

impl AdpcmImaWavDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: ImaWavConfig::default(),
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, sample_rate: u32, layout: ChannelLayout) -> Self {
        if sample_rate > 0 {
            self.cfg.sample_rate = sample_rate;
        }
        self.cfg.layout = layout;
        self
    }
}

impl SendReceive for AdpcmImaWavDecoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((sr, layout)) = parse_audio_extradata(extradata) {
            self.cfg.sample_rate = sr;
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else { return Ok(()) };
                let samples = ima::decode_wav_block(pkt.payload(), self.cfg.layout.channels)?;
                let frame = frame_from_samples(
                    &self.limits,
                    &samples,
                    self.cfg.layout.channels,
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
pub struct AdpcmImaWavEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    cfg: ImaWavConfig,
}

impl AdpcmImaWavEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: ImaWavConfig::default(),
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, layout: ChannelLayout) -> Self {
        self.cfg.layout = layout;
        self
    }
}

impl SendReceive for AdpcmImaWavEncoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((_, layout)) = parse_audio_extradata(extradata) {
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else { return Ok(()) };
                let (samples, channels) = frame_samples_owned(frame)?;
                let wire = ima::encode_wav_block(&samples, channels)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &wire)?;
                packet.pts = frame.pts;
                packet.duration = frame_pcm_duration(frame, samples.len(), channels);
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

// ------------------------------------------------------------- IMA-QT

adpcm_config!(ImaQtConfig, DEFAULT_SAMPLE_RATE);

#[derive(Debug)]
pub struct AdpcmImaQtDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    cfg: ImaQtConfig,
    /// Per-channel predictor/step state, carried across `send` calls. See
    /// `ima::decode_qt_block`'s doc for why this must persist across
    /// packets rather than reset from every chunk's own (lossy, 9-bit)
    /// header predictor.
    state: Option<Vec<ima::ImaState>>,
}
impl AdpcmImaQtDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: ImaQtConfig::default(),
            state: None,
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, sample_rate: u32, layout: ChannelLayout) -> Self {
        if sample_rate > 0 {
            self.cfg.sample_rate = sample_rate;
        }
        self.cfg.layout = layout;
        self
    }
}
impl SendReceive for AdpcmImaQtDecoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((sr, layout)) = parse_audio_extradata(extradata) {
            self.cfg.sample_rate = sr;
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else { return Ok(()) };
                let samples =
                    ima::decode_qt_block(pkt.payload(), self.cfg.layout.channels, &mut self.state)?;
                let frame = frame_from_samples(
                    &self.limits,
                    &samples,
                    self.cfg.layout.channels,
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
        self.state = None;
    }
}

#[derive(Debug)]
pub struct AdpcmImaQtEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    cfg: ImaQtConfig,
}
impl AdpcmImaQtEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: ImaQtConfig::default(),
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, layout: ChannelLayout) -> Self {
        self.cfg.layout = layout;
        self
    }
}
impl SendReceive for AdpcmImaQtEncoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((_, layout)) = parse_audio_extradata(extradata) {
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else { return Ok(()) };
                let (samples, channels) = frame_samples_owned(frame)?;
                let wire = ima::encode_qt_block(&samples, channels)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &wire)?;
                packet.pts = frame.pts;
                packet.duration = frame_pcm_duration(frame, samples.len(), channels);
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

// --------------------------------------------------------------- MS-ADPCM

adpcm_config!(MsConfig, DEFAULT_SAMPLE_RATE);

#[derive(Debug)]
pub struct AdpcmMsDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    cfg: MsConfig,
}
impl AdpcmMsDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: MsConfig::default(),
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, sample_rate: u32, layout: ChannelLayout) -> Self {
        if sample_rate > 0 {
            self.cfg.sample_rate = sample_rate;
        }
        self.cfg.layout = layout;
        self
    }
}
impl SendReceive for AdpcmMsDecoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((sr, layout)) = parse_audio_extradata(extradata) {
            self.cfg.sample_rate = sr;
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else { return Ok(()) };
                let samples = ms::decode_block(pkt.payload(), self.cfg.layout.channels)?;
                let frame = frame_from_samples(
                    &self.limits,
                    &samples,
                    self.cfg.layout.channels,
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
pub struct AdpcmMsEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    cfg: MsConfig,
}
impl AdpcmMsEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: MsConfig::default(),
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, layout: ChannelLayout) -> Self {
        self.cfg.layout = layout;
        self
    }
}
impl SendReceive for AdpcmMsEncoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((_, layout)) = parse_audio_extradata(extradata) {
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else { return Ok(()) };
                let (samples, channels) = frame_samples_owned(frame)?;
                let wire = ms::encode_block(&samples, channels)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &wire)?;
                packet.pts = frame.pts;
                packet.duration = frame_pcm_duration(frame, samples.len(), channels);
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

// ------------------------------------------------------------------- SWF

adpcm_config!(SwfConfig, DEFAULT_SAMPLE_RATE);

#[derive(Debug)]
pub struct AdpcmSwfDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    cfg: SwfConfig,
}
impl AdpcmSwfDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: SwfConfig::default(),
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, sample_rate: u32, layout: ChannelLayout) -> Self {
        if sample_rate > 0 {
            self.cfg.sample_rate = sample_rate;
        }
        self.cfg.layout = layout;
        self
    }
}
impl SendReceive for AdpcmSwfDecoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((sr, layout)) = parse_audio_extradata(extradata) {
            self.cfg.sample_rate = sr;
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else { return Ok(()) };
                let channels = self.cfg.layout.channels.max(1);
                let sample_count = swf_packet_sample_count(pkt, self.cfg.sample_rate);
                let samples = swf::decode_block(pkt.payload(), channels, sample_count)?;
                let frame = frame_from_samples(
                    &self.limits,
                    &samples,
                    channels,
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
pub struct AdpcmSwfEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    cfg: SwfConfig,
}
impl AdpcmSwfEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            cfg: SwfConfig::default(),
        }
    }
    #[must_use]
    pub fn with_audio_params(mut self, layout: ChannelLayout) -> Self {
        self.cfg.layout = layout;
        self
    }
}
impl SendReceive for AdpcmSwfEncoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((_, layout)) = parse_audio_extradata(extradata) {
            self.cfg.layout = layout;
        }
        Ok(())
    }
    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else { return Ok(()) };
                let (samples, channels) = frame_samples_owned(frame)?;
                let wire = swf::encode_block(&samples, channels)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &wire)?;
                packet.pts = frame.pts;
                packet.duration = frame_pcm_duration(frame, samples.len(), channels);
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

// -------------------------------------------------------------- G.726/le
//
// **Not implemented as real codecs.** `crate::g726` is a structurally
// different transform from ITU-T G.726 (see that module's doc) — it
// round-trips through its own encoder but cannot decode a real G.726
// bitstream, and would hand a caller plausible-looking wrong samples with no
// error if wired up as if it worked. The repository owner's ruling
// (`planning/AGENT-CONSTRAINTS.md`, "byte-exactness is a check, not the
// bar") permits small, unstructured deviation — rounding noise — but not a
// different transform wearing the codec's name. So these two wrappers exist
// only to refuse loudly and are deliberately **not** registered in
// `vaco-component.toml`; `crate::g726`'s functions and their own
// self-consistency tests stay, for whoever implements the real two-pole/
// six-zero predictor next.

#[derive(Debug)]
pub struct AdpcmG726Decoder {
    machine: Machine<Frame>,
}
impl AdpcmG726Decoder {
    #[must_use]
    pub fn new(_limits: Limits, _left_justified: bool) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
        }
    }
}
impl SendReceive for AdpcmG726Decoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, _input: Option<&Packet>) -> Result<()> {
        Err(Error::Unsupported(
            "adpcm_g726: no real ITU-T G.726 two-pole/six-zero predictor is implemented \
             (crate::g726 is a structurally different stand-in; see its module doc)",
        ))
    }
    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

#[derive(Debug)]
pub struct AdpcmG726Encoder {
    machine: Machine<Packet>,
}
impl AdpcmG726Encoder {
    #[must_use]
    pub fn new(_limits: Limits, _left_justified: bool) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
        }
    }
}
impl SendReceive for AdpcmG726Encoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, _input: Option<&Frame>) -> Result<()> {
        Err(Error::Unsupported(
            "adpcm_g726: no real ITU-T G.726 two-pole/six-zero predictor is implemented \
             (crate::g726 is a structurally different stand-in; see its module doc)",
        ))
    }
    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

// ----------------------------------------------------------------- G.722
//
// **Not implemented as a real codec**, for the identical reason as G.726
// above: `crate::g722` stands a reversible two-point lifting split in for
// the real 24-tap QMF and shares G.726's simplified per-band coder, which
// round-trips through its own encoder but cannot decode a real G.722
// bitstream. Deliberately unregistered; see `crate::g722`'s module doc.

#[derive(Debug)]
pub struct AdpcmG722Decoder {
    machine: Machine<Frame>,
}
impl AdpcmG722Decoder {
    #[must_use]
    pub fn new(_limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
        }
    }
}
impl SendReceive for AdpcmG722Decoder {
    type Input = Packet;
    type Output = Frame;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, _input: Option<&Packet>) -> Result<()> {
        Err(Error::Unsupported(
            "adpcm_g722: no real ITU-T G.722 QMF/predictor is implemented \
             (crate::g722 is a structurally different stand-in; see its module doc)",
        ))
    }
    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

#[derive(Debug)]
pub struct AdpcmG722Encoder {
    machine: Machine<Packet>,
}
impl AdpcmG722Encoder {
    #[must_use]
    pub fn new(_limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
        }
    }
}
impl SendReceive for AdpcmG722Encoder {
    type Input = Frame;
    type Output = Packet;
    fn caps(&self) -> Caps {
        self.machine.caps()
    }
    fn send(&mut self, _input: Option<&Frame>) -> Result<()> {
        Err(Error::Unsupported(
            "adpcm_g722: no real ITU-T G.722 QMF/predictor is implemented \
             (crate::g722 is a structurally different stand-in; see its module doc)",
        ))
    }
    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }
    fn flush(&mut self) {
        self.machine.flush();
    }
}

// ------------------------------------------------------------- registration

fn make_ima_wav_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(AdpcmImaWavDecoder::new(limits))))
}
fn make_ima_wav_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(AdpcmImaWavEncoder::new(limits))))
}
fn make_ima_qt_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(AdpcmImaQtDecoder::new(limits))))
}
fn make_ima_qt_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(AdpcmImaQtEncoder::new(limits))))
}
fn make_ms_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(AdpcmMsDecoder::new(limits))))
}
fn make_ms_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(AdpcmMsEncoder::new(limits))))
}
fn make_swf_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(AdpcmSwfDecoder::new(limits))))
}
fn make_swf_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(AdpcmSwfEncoder::new(limits))))
}
fn make_g726_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(AdpcmG726Decoder::new(
        limits, false,
    ))))
}
fn make_g726_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(AdpcmG726Encoder::new(
        limits, false,
    ))))
}
fn make_g726le_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(AdpcmG726Decoder::new(
        limits, true,
    ))))
}
fn make_g726le_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(AdpcmG726Encoder::new(
        limits, true,
    ))))
}
fn make_g722_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AsDecoder(Validated::new(AdpcmG722Decoder::new(limits))))
}
fn make_g722_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(AdpcmG722Encoder::new(limits))))
}

pub static ADPCM_IMA_WAV_DECODER: DecoderDesc = DecoderDesc {
    name: "adpcm_ima_wav",
    long_name: "ADPCM IMA WAV",
    id: CodecId::AdpcmImaWav,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ima_wav_decoder,
};
pub static ADPCM_IMA_WAV_ENCODER: EncoderDesc = EncoderDesc {
    name: "adpcm_ima_wav",
    long_name: "ADPCM IMA WAV",
    id: CodecId::AdpcmImaWav,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ima_wav_encoder,
};
pub static ADPCM_IMA_QT_DECODER: DecoderDesc = DecoderDesc {
    name: "adpcm_ima_qt",
    long_name: "ADPCM IMA QuickTime",
    id: CodecId::AdpcmImaQt,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ima_qt_decoder,
};
pub static ADPCM_IMA_QT_ENCODER: EncoderDesc = EncoderDesc {
    name: "adpcm_ima_qt",
    long_name: "ADPCM IMA QuickTime",
    id: CodecId::AdpcmImaQt,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ima_qt_encoder,
};
pub static ADPCM_MS_DECODER: DecoderDesc = DecoderDesc {
    name: "adpcm_ms",
    long_name: "ADPCM Microsoft",
    id: CodecId::AdpcmMs,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ms_decoder,
};
pub static ADPCM_MS_ENCODER: EncoderDesc = EncoderDesc {
    name: "adpcm_ms",
    long_name: "ADPCM Microsoft",
    id: CodecId::AdpcmMs,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ms_encoder,
};
pub static ADPCM_SWF_DECODER: DecoderDesc = DecoderDesc {
    name: "adpcm_swf",
    long_name: "ADPCM Shockwave Flash",
    id: CodecId::AdpcmSwf,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_swf_decoder,
};
pub static ADPCM_SWF_ENCODER: EncoderDesc = EncoderDesc {
    name: "adpcm_swf",
    long_name: "ADPCM Shockwave Flash",
    id: CodecId::AdpcmSwf,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_swf_encoder,
};
/// **Not listed in `vaco-component.toml`** — always returns
/// [`vaco_core::Error::Unsupported`]; see the crate/module docs on why. Kept
/// as a compilable identity for whoever implements the real predictor.
pub static ADPCM_G726_DECODER: DecoderDesc = DecoderDesc {
    name: "adpcm_g726",
    long_name: "G.726 ADPCM",
    id: CodecId::AdpcmG726,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_g726_decoder,
};
/// **Not listed in `vaco-component.toml`** — see [`ADPCM_G726_DECODER`].
pub static ADPCM_G726_ENCODER: EncoderDesc = EncoderDesc {
    name: "adpcm_g726",
    long_name: "G.726 ADPCM",
    id: CodecId::AdpcmG726,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_g726_encoder,
};
/// **Not listed in `vaco-component.toml`** — see [`ADPCM_G726_DECODER`].
pub static ADPCM_G726LE_DECODER: DecoderDesc = DecoderDesc {
    name: "adpcm_g726le",
    long_name: "G.726 ADPCM little-endian",
    id: CodecId::AdpcmG726le,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_g726le_decoder,
};
/// **Not listed in `vaco-component.toml`** — see [`ADPCM_G726_DECODER`].
pub static ADPCM_G726LE_ENCODER: EncoderDesc = EncoderDesc {
    name: "adpcm_g726le",
    long_name: "G.726 little endian ADPCM",
    id: CodecId::AdpcmG726le,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_g726le_encoder,
};
/// **Not listed in `vaco-component.toml`** — always returns
/// [`vaco_core::Error::Unsupported`]; see the crate/module docs on why. Kept
/// as a compilable identity for whoever implements the real QMF/predictor.
pub static ADPCM_G722_DECODER: DecoderDesc = DecoderDesc {
    name: "adpcm_g722",
    long_name: "G.722 ADPCM",
    id: CodecId::AdpcmG722,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_g722_decoder,
};
/// **Not listed in `vaco-component.toml`** — see [`ADPCM_G722_DECODER`].
pub static ADPCM_G722_ENCODER: EncoderDesc = EncoderDesc {
    name: "adpcm_g722",
    long_name: "G.722 ADPCM",
    id: CodecId::AdpcmG722,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_g722_encoder,
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

    fn frame_of(samples: &[i16], channels: u32) -> Frame {
        frame_from_samples(
            &Limits::permissive(),
            samples,
            channels,
            8000,
            vaco_core::Timestamp::new(0),
        )
        .unwrap()
    }

    #[test]
    fn ima_wav_send_receive_round_trips_approximately() {
        let samples = tone(41); // 1 header + a whole number of 8-sample groups
        let mut enc = AdpcmImaWavEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&samples, 1))).unwrap();
        let pkt = enc.receive().unwrap();
        let mut dec = AdpcmImaWavDecoder::new(Limits::permissive());
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();
        let (decoded, _) = frame_samples_owned(&frame).unwrap();
        assert_eq!(decoded.len(), samples.len());
    }

    /// Same bug class as `vaco-codec-flac`/`vaco-codec-alac`/
    /// `vaco-codec-vorbis`/`vaco-codec-pcm`'s encoders: `send` set
    /// `packet.pts` but never `packet.duration`, which a container that
    /// derives a track's total length from summed packet durations (MP4's
    /// `stts`, Ogg's granule position) silently undercounts by. All four
    /// real ADPCM encoders in this crate share `frame_pcm_duration`, so one
    /// representative check (IMA-WAV) covers the shared helper; the other
    /// three (IMA-QT, MS, SWF) call the identical function the identical
    /// way.
    #[test]
    fn ima_wav_send_frame_sets_a_real_nonzero_packet_duration() {
        let samples = tone(41);
        let mut enc = AdpcmImaWavEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&samples, 1))).unwrap();
        let pkt = enc.receive().unwrap();

        // 41 samples at 8000 Hz (frame_of's fixed rate).
        let expected = vaco_core::Timestamp::new(41)
            .to_duration(vaco_core::Rational::new(1, 8_000))
            .unwrap();
        assert_ne!(expected, Duration::ZERO);
        assert_eq!(pkt.duration, expected);
    }

    /// The decode-side mirror of the test above: `frame_from_samples`
    /// (shared by all four real decoders) never set `frame.duration`
    /// either, even though `count`/`sample_rate` were already in scope.
    #[test]
    fn ima_wav_decode_sets_a_real_nonzero_frame_duration() {
        let samples = tone(41);
        let mut enc = AdpcmImaWavEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&samples, 1))).unwrap();
        let pkt = enc.receive().unwrap();
        // `frame_of` hardcodes 8000 Hz on the encode side; the decoder has
        // no way to learn that from the bitstream itself (matching this
        // crate's own note on `frame_pcm_duration` about nothing upstream
        // of a raw-PCM/ADPCM source reliably carrying sample rate), so it
        // must be configured to match, same as any real container's
        // extradata would supply.
        let mut dec = AdpcmImaWavDecoder::new(Limits::permissive())
            .with_audio_params(8_000, ChannelLayout::MONO);
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();

        // 41 samples at 8000 Hz.
        let expected = vaco_core::Timestamp::new(41)
            .to_duration(vaco_core::Rational::new(1, 8_000))
            .unwrap();
        assert_ne!(expected, Duration::ZERO);
        assert_eq!(frame.duration, expected);
    }

    #[test]
    fn ima_qt_send_receive_round_trips() {
        let samples = tone(ima::QT_SAMPLES_PER_CHUNK);
        let mut enc = AdpcmImaQtEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&samples, 1))).unwrap();
        let pkt = enc.receive().unwrap();
        let mut dec = AdpcmImaQtDecoder::new(Limits::permissive());
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();
        let (decoded, _) = frame_samples_owned(&frame).unwrap();
        assert_eq!(decoded.len(), samples.len());
    }

    #[test]
    fn ms_send_receive_round_trips() {
        let samples = tone(30);
        let mut enc = AdpcmMsEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&samples, 1))).unwrap();
        let pkt = enc.receive().unwrap();
        let mut dec = AdpcmMsDecoder::new(Limits::permissive());
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();
        let (decoded, _) = frame_samples_owned(&frame).unwrap();
        assert_eq!(decoded.len(), samples.len());
    }

    #[test]
    fn swf_send_receive_round_trips() {
        let samples = tone(24);
        let mut enc = AdpcmSwfEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&samples, 1))).unwrap();
        let pkt = enc.receive().unwrap();
        let mut dec =
            AdpcmSwfDecoder::new(Limits::permissive()).with_audio_params(8000, ChannelLayout::MONO);
        dec.send(Some(&pkt)).unwrap();
        let frame = dec.receive().unwrap();
        let (decoded, _) = frame_samples_owned(&frame).unwrap();
        assert_eq!(decoded.len(), samples.len());
    }

    #[test]
    fn swf_partial_packets_honor_duration_across_rates_and_channels() {
        for &(sample_rate, channels, per_channel) in &[
            (5512u32, 1u32, 1usize),
            (11025, 1, 24),
            (22050, 2, 37),
            (44100, 2, 4095),
        ] {
            let mut samples = Vec::new();
            for n in 0..per_channel {
                let value = ((n as f64 * 0.31).sin() * 12_000.0) as i16;
                for channel in 0..channels {
                    samples.push(if channel == 0 {
                        value
                    } else {
                        value.saturating_neg()
                    });
                }
            }
            let frame = frame_from_samples(
                &Limits::permissive(),
                &samples,
                channels,
                sample_rate,
                vaco_core::Timestamp::new(0),
            )
            .unwrap();
            let mut enc = AdpcmSwfEncoder::new(Limits::permissive())
                .with_audio_params(ChannelLayout::default_for(channels).unwrap());
            enc.send(Some(&frame)).unwrap();
            let packet = enc.receive().unwrap();
            // SWF v19's fixed 4-bit packet is 2051 bytes mono or 4101 bytes
            // stereo after rounding the packed header/codes up to bytes.
            let expected_packet_bytes = match channels {
                1 => 2051,
                2 => 4101,
                _ => unreachable!("test only covers mono/stereo"),
            };
            assert_eq!(packet.payload().len(), expected_packet_bytes);
            assert_eq!(packet.duration, frame.duration);
            let padded =
                swf::decode_block(packet.payload(), channels, swf::SAMPLES_PER_PACKET).unwrap();
            let expected_samples = usize::try_from(swf::SAMPLES_PER_PACKET).unwrap()
                * usize::try_from(channels).unwrap();
            assert_eq!(padded.len(), expected_samples);
            let mut dec = AdpcmSwfDecoder::new(Limits::permissive())
                .with_audio_params(sample_rate, ChannelLayout::default_for(channels).unwrap());
            dec.send(Some(&packet)).unwrap();
            let decoded = frame_samples_owned(&dec.receive().unwrap()).unwrap().0;
            assert_eq!(decoded.len(), samples.len());
        }
    }

    #[test]
    fn g726_decoder_and_encoder_refuse_rather_than_produce_wrong_output() {
        // g726/g726le have no real ITU-T predictor implemented (see the
        // crate/module docs) — the wrapper must fail loudly, never hand back
        // plausible-looking wrong samples.
        let samples = tone(50);
        let mut enc = AdpcmG726Encoder::new(Limits::permissive(), false);
        assert!(matches!(
            enc.send(Some(&frame_of(&samples, 1))),
            Err(Error::Unsupported(_))
        ));
        let mut dec = AdpcmG726Decoder::new(Limits::permissive(), false);
        let dummy = Packet::from_slice(&mut Budget::new(Limits::permissive()), &[0u8; 4]).unwrap();
        assert!(matches!(dec.send(Some(&dummy)), Err(Error::Unsupported(_))));
    }

    #[test]
    fn g726le_decoder_and_encoder_refuse_rather_than_produce_wrong_output() {
        let samples = tone(50);
        let mut enc = AdpcmG726Encoder::new(Limits::permissive(), true);
        assert!(matches!(
            enc.send(Some(&frame_of(&samples, 1))),
            Err(Error::Unsupported(_))
        ));
        let mut dec = AdpcmG726Decoder::new(Limits::permissive(), true);
        let dummy = Packet::from_slice(&mut Budget::new(Limits::permissive()), &[0u8; 4]).unwrap();
        assert!(matches!(dec.send(Some(&dummy)), Err(Error::Unsupported(_))));
    }

    #[test]
    fn g722_decoder_and_encoder_refuse_rather_than_produce_wrong_output() {
        let samples = tone(64);
        let mut enc = AdpcmG722Encoder::new(Limits::permissive());
        assert!(matches!(
            enc.send(Some(&frame_of(&samples, 1))),
            Err(Error::Unsupported(_))
        ));
        let mut dec = AdpcmG722Decoder::new(Limits::permissive());
        let dummy = Packet::from_slice(&mut Budget::new(Limits::permissive()), &[0u8; 4]).unwrap();
        assert!(matches!(dec.send(Some(&dummy)), Err(Error::Unsupported(_))));
    }

    #[test]
    fn protocol_shape_is_consistent_across_families() {
        let samples = tone(30);
        let mut dec = AdpcmMsDecoder::new(Limits::permissive());
        let mut enc = AdpcmMsEncoder::new(Limits::permissive());
        enc.send(Some(&frame_of(&samples, 1))).unwrap();
        let pkt = enc.receive().unwrap();
        dec.send(Some(&pkt)).unwrap();
        let _ = dec.receive().unwrap();
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).unwrap();
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    #[test]
    fn all_fourteen_descriptors_compile_but_only_eight_are_registered() {
        // All 14 static descriptors exist (g722/g726/g726le kept as
        // compilable identities per the crate doc), but only the 8 backing
        // real implementations are listed in vaco-component.toml — checked
        // structurally here since the toml file itself isn't parsed by this
        // test; see that file's own header comment for the authoritative
        // list.
        let decoders: &[&DecoderDesc] = &[
            &ADPCM_IMA_WAV_DECODER,
            &ADPCM_IMA_QT_DECODER,
            &ADPCM_MS_DECODER,
            &ADPCM_SWF_DECODER,
            &ADPCM_G726_DECODER,
            &ADPCM_G726LE_DECODER,
            &ADPCM_G722_DECODER,
        ];
        let encoders: &[&EncoderDesc] = &[
            &ADPCM_IMA_WAV_ENCODER,
            &ADPCM_IMA_QT_ENCODER,
            &ADPCM_MS_ENCODER,
            &ADPCM_SWF_ENCODER,
            &ADPCM_G726_ENCODER,
            &ADPCM_G726LE_ENCODER,
            &ADPCM_G722_ENCODER,
        ];
        assert_eq!(decoders.len(), 7);
        assert_eq!(encoders.len(), 7);
        let registered = ["adpcm_ima_wav", "adpcm_ima_qt", "adpcm_ms", "adpcm_swf"];
        let unsupported = ["adpcm_g726", "adpcm_g726le", "adpcm_g722"];
        for d in decoders {
            assert!(
                registered.contains(&d.name) || unsupported.contains(&d.name),
                "{}",
                d.name
            );
        }
    }
}
