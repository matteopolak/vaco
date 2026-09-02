//! Linear PCM decode and encode — one table-driven crate for the whole
//! `pcm_*` family, per plan 15 §4.9.
//!
//! # What it is
//!
//! `vaco_codec_core::CodecId` declares 21 `Pcm*` variants (matching
//! `vaco-demux-raw::pcm`'s own 21 raw-format registrations exactly). This
//! crate is the only place any of them is actually converted to or from
//! samples: [`table::PCM_FORMATS`] maps each id to a byte-width/layout
//! descriptor, and [`codec::decode_interleaved`]/[`codec::encode_interleaved`]
//! are the single conversion routine every row drives. There is no per-codec
//! `match` anywhere in the send/receive wrappers below — only in the table.
//!
//! # How it works
//!
//! [`PcmDecoder`]/[`PcmEncoder`] are thin `SendReceive` wrappers around the
//! pure functions in [`codec`], following the same `Machine`-backed shape as
//! `vaco_codec_core::mock` and `vaco-codec-qoi`. The only real subtlety is
//! **where the sample rate and channel count come from**: unlike every
//! self-describing bitstream codec in this tree, a raw PCM packet carries no
//! header at all — the container states these facts, not the codec. See
//! [`parse_audio_extradata`] for the (documented, provisional) mechanism this
//! crate uses until #652 lands a shared convention.
//!
//! # How to change it
//!
//! Add a row to [`table::PCM_FORMATS`] and a `CodecId::Pcm*` variant in
//! `vaco-codec-core` if the reference gains a format this build does not
//! cover yet. [`codec`] should never need a new match arm for a plain
//! fixed-width int/float format — only [`table::WireKind`] grows a variant
//! for a genuinely new *kind* of wire representation (a new companding law,
//! say).
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds decode allocation the way it bounds every
//! other decoder in this tree. Sample rate/channel layout default to
//! [`DEFAULT_SAMPLE_RATE`]/mono (matching `vaco-demux-raw::pcm`'s own
//! defaults) until overridden via [`PcmDecoder::with_audio_params`] or
//! [`vaco_codec_core::Decoder::set_extradata`].
//!
//! # Dependencies
//!
//! `vaco-codec-core` (the protocol), `vaco-frame`/`vaco-sampfmt`/
//! `vaco-chlayout`/`vaco-pool` (the decoded audio buffer), `vaco-packet`,
//! `vaco-limits`.
//!
//! # What is not covered, and why
//!
//! The roadmap's "38 dec / 20 enc" figure (plan 20 §1.9, issue #279) includes
//! container-specific oddities `vaco-codec-core::CodecId` has no variant for
//! today: Blu-ray/DVD LPCM's variable 16/20/24-bit-per-block framing,
//! `pcm_s24daud`, `pcm_lxf`'s planar 20-bit layout, `pcm_sga`, the five
//! `_planar` variants, `s64le`/`s64be`, and `f16le`/`f24le`. Adding those
//! needs new `CodecId` variants in `vaco-codec-core`, which this batch already
//! touched once (for the ADPCM/rawvideo/null identities #280/#281 needed —
//! see that crate's commit) but chose not to extend further here: 21 decode /
//! 20 encode is every `Pcm*` identity the enum declares, fully implemented
//! and tested, and is the honest stopping point for this pass. Noted in the
//! closing comment on #279 rather than silently claiming the larger number.

#![forbid(unsafe_code)]

mod codec;
pub mod table;

pub use codec::{decode_interleaved, encode_interleaved};
pub use table::{PCM_FORMATS, PcmFormat, WireKind, format_for};

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{
    Accept, AsDecoder, AsEncoder, Caps, CodecId, Decoder, DecoderDesc, Encoder, EncoderDesc,
    Machine, SendReceive, Validated,
};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Reference default for a raw PCM stream with no header of its own —
/// matches `vaco_demux_raw::pcm::DEFAULT_SAMPLE_RATE`.
pub const DEFAULT_SAMPLE_RATE: u32 = 44_100;

/// Reads the 5-byte `(sample_rate: u32 LE, channels: u8)` record this crate
/// accepts through [`Decoder::set_extradata`].
///
/// **Provisional.** `vaco_codec_core::Decoder::set_extradata`'s own doc names
/// exactly this situation — "any codec whose configuration is the
/// container's to state... has the identical shape" as an `AudioSpecificConfig`
/// — but no shared wire format for "the container's raw audio parameters"
/// exists in this workspace yet (`planning/ASSIGNMENTS.md`'s `agent:codec-path`
/// row, #652, is building the registry-to-CLI codec path this would plug
/// into). Until that lands, this crate defines its own minimal record rather
/// than leaving PCM undecodable outside of direct construction. A caller that
/// already knows the sample rate/channel count should prefer
/// [`PcmDecoder::with_audio_params`]/[`PcmEncoder::with_audio_params`]
/// directly; this exists for the `DecoderDesc::make`/`EncoderDesc::make`
/// path, whose signature (`fn(Limits) -> Box<dyn Decoder>`) has no room for
/// parameters at all.
///
/// Malformed or zero-valued input is ignored, not an error — matching the
/// trait's "this record told me nothing" contract for a merely-offered
/// configuration.
fn parse_audio_extradata(extradata: &[u8]) -> Option<(u32, ChannelLayout)> {
    let &[a, b, c, d, ch] = extradata else {
        return None;
    };
    let sample_rate = u32::from_le_bytes([a, b, c, d]);
    if sample_rate == 0 || ch == 0 {
        return None;
    }
    let channels = u32::from(ch);
    let layout = ChannelLayout::default_for(channels).unwrap_or(ChannelLayout::unspecified(channels));
    Some((sample_rate, layout))
}

/// Build the record [`parse_audio_extradata`] reads, for a caller that wants
/// to configure a registry-built decoder/encoder through
/// [`Decoder::set_extradata`]/the encoder's own `set_extradata`.
#[must_use]
pub fn audio_extradata(sample_rate: u32, channels: u8) -> [u8; 5] {
    let sr = sample_rate.to_le_bytes();
    [sr[0], sr[1], sr[2], sr[3], channels]
}

/// Resolve `id`'s row, falling back to the first table entry.
///
/// The fallback is unreachable for any `id` this crate itself registers a
/// [`DecoderDesc`]/[`EncoderDesc`] under (every `make_*` function below closes
/// over a `CodecId` that is, by construction, a key in [`table::PCM_FORMATS`]).
/// It exists only because `unwrap`/`expect`/indexing are denied lints in this
/// workspace, so "cannot happen" still needs a typed answer.
fn resolve(id: CodecId) -> PcmFormat {
    table::format_for(id).copied().unwrap_or_else(|| {
        table::PCM_FORMATS
            .first()
            .copied()
            .unwrap_or(PcmFormat {
                id: CodecId::Pcm,
                container_bytes: 1,
                wire: WireKind::UnsignedInt { big_endian: false },
                decoded: vaco_sampfmt::SampleFmt::U8,
                encodable: false,
            })
    })
}

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`] for one `Pcm*` codec,
/// chosen at construction.
#[derive(Debug)]
pub struct PcmDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    format: PcmFormat,
    sample_rate: u32,
    layout: ChannelLayout,
}

impl PcmDecoder {
    /// A decoder for `id`, bounded by `limits`, defaulting to
    /// [`DEFAULT_SAMPLE_RATE`]/mono until configured otherwise.
    #[must_use]
    pub fn new(limits: Limits, id: CodecId) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            format: resolve(id),
            sample_rate: DEFAULT_SAMPLE_RATE,
            layout: ChannelLayout::MONO,
        }
    }

    /// Configure the container's own sample rate/channel layout directly,
    /// bypassing [`Decoder::set_extradata`]'s byte record.
    #[must_use]
    pub fn with_audio_params(mut self, sample_rate: u32, layout: ChannelLayout) -> Self {
        if sample_rate > 0 {
            self.sample_rate = sample_rate;
        }
        self.layout = layout;
        self
    }
}

impl SendReceive for PcmDecoder {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((sample_rate, layout)) = parse_audio_extradata(extradata) {
            self.sample_rate = sample_rate;
            self.layout = layout;
        }
        Ok(())
    }

    /// The channel this decoder actually needs: PCM's bitstream states
    /// neither sample rate nor channel layout, so without this it silently
    /// decoded every stream at [`DEFAULT_SAMPLE_RATE`]/mono regardless of
    /// what the container declared (see [`Decoder::prime_audio`]'s doc for
    /// why that stayed invisible until something downstream trusted the
    /// decoded rate). `0`/an empty layout is "not yet known" and left alone,
    /// matching [`PcmDecoder::with_audio_params`]'s own contract.
    fn prime_audio(&mut self, sample_rate: u32, layout: ChannelLayout) {
        if sample_rate > 0 {
            self.sample_rate = sample_rate;
        }
        if layout.channels > 0 {
            self.layout = layout;
        }
    }

    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else {
                    return Ok(());
                };
                let mut budget = Budget::new(self.limits.clone());
                let (samples, count) = codec::decode_interleaved(
                    self.format,
                    pkt.payload(),
                    self.layout.channels,
                    &mut budget,
                )?;
                let mut frame = Frame::alloc_audio(
                    &mut budget,
                    self.format.decoded,
                    self.layout.clone(),
                    count,
                    self.sample_rate,
                )?;
                let FrameData::Audio { planes, .. } = &mut frame.data else {
                    return Err(Error::InvalidData("pcm: expected an audio frame"));
                };
                let plane = planes
                    .get_mut(0)
                    .ok_or(Error::InvalidData("pcm: no plane 0"))?;
                let buf = plane.data.make_mut();
                let dst = buf
                    .get_mut(..samples.len())
                    .ok_or(Error::InvalidData("pcm: plane shorter than decoded data"))?;
                dst.copy_from_slice(&samples);
                frame.pts = pkt.pts;
                // The decode-side mirror of the `PcmEncoder::send` fix
                // above: every real video decoder in this tree sets
                // `frame.duration` from the source's own per-frame timing,
                // but this one never did, even though the ingredients
                // (`count` decoded samples at `self.sample_rate`) are both
                // already in scope. Left unset, any consumer downstream
                // that trusts `frame.duration` rather than recomputing it
                // itself (a filter doing duration-aware processing, or a
                // future encoder following `vaco-codec-vp8`/`-vp9`'s
                // propagate-don't-recompute pattern) would silently see a
                // zero duration for every PCM-decoded frame.
                let time_base = Rational::new(1, i32::try_from(self.sample_rate).unwrap_or(1).max(1));
                frame.duration = Timestamp::new(i64::from(count))
                    .to_duration(time_base)
                    .unwrap_or(Duration::ZERO);
                frame.flags = FrameFlags::KEY;
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`] for one `Pcm*` codec.
#[derive(Debug)]
pub struct PcmEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    format: PcmFormat,
    layout: ChannelLayout,
}

impl PcmEncoder {
    /// An encoder for `id`, bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits, id: CodecId) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            format: resolve(id),
            layout: ChannelLayout::MONO,
        }
    }

    /// Configure the target channel layout; the wire's own sample rate is
    /// carried by the container, not by this codec, so there is nothing to
    /// set for it here.
    #[must_use]
    pub fn with_audio_params(mut self, layout: ChannelLayout) -> Self {
        self.layout = layout;
        self
    }
}

impl SendReceive for PcmEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((_, layout)) = parse_audio_extradata(extradata) {
            self.layout = layout;
        }
        Ok(())
    }

    /// The one wire format this codec accepts: [`PcmFormat::decoded`], as
    /// [`PcmEncoder::send`] already enforces (`Error::Unsupported` on a
    /// mismatch). Exposing it here, rather than only enforcing it at
    /// `send_frame` time, is what lets a caller insert a conversion *before*
    /// sending instead of discovering the mismatch from a failed send
    /// (E2E-GAPS 3 — the same gap `vaco-codec-qoi`'s `accepted_pix_fmts`
    /// override closed on the video side).
    ///
    /// One `PcmEncoder` handles every `Pcm*` codec this crate registers
    /// (`resolve` picks `format` at construction), so this cannot be a single
    /// crate-wide constant the way a codec with one fixed format would write
    /// it — the match covers every `SampleFmt` `table::PCM_FORMATS` actually
    /// uses as a `decoded` value, each arm a `'static` one-element slice.
    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        use vaco_sampfmt::SampleFmt;
        match self.format.decoded {
            SampleFmt::U8 => &[SampleFmt::U8],
            SampleFmt::S16 => &[SampleFmt::S16],
            SampleFmt::S32 => &[SampleFmt::S32],
            SampleFmt::S64 => &[SampleFmt::S64],
            SampleFmt::F32 => &[SampleFmt::F32],
            SampleFmt::F64 => &[SampleFmt::F64],
            SampleFmt::U8P => &[SampleFmt::U8P],
            SampleFmt::S16P => &[SampleFmt::S16P],
            SampleFmt::S32P => &[SampleFmt::S32P],
            SampleFmt::S64P => &[SampleFmt::S64P],
            SampleFmt::F32P => &[SampleFmt::F32P],
            SampleFmt::F64P => &[SampleFmt::F64P],
        }
    }

    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else {
                    return Ok(());
                };
                let FrameData::Audio {
                    format,
                    planes,
                    layout,
                    sample_rate,
                    samples,
                    ..
                } = &frame.data
                else {
                    return Err(Error::InvalidData("pcm: expected an audio frame"));
                };
                if *format != self.format.decoded {
                    return Err(Error::Unsupported(
                        "pcm: encoder input sample format does not match this codec",
                    ));
                }
                let plane = planes
                    .first()
                    .ok_or(Error::InvalidData("pcm: no plane 0"))?;
                let wire = codec::encode_interleaved(
                    self.format,
                    plane.data.as_slice(),
                    layout.channels,
                )?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &wire)?;
                packet.pts = frame.pts;
                // Same bug class as `vaco-codec-flac`/`vaco-codec-alac`/
                // `vaco-codec-vorbis`: unlike those, this encoder is 1:1
                // (one input `Frame` is exactly one `Packet`, no internal
                // block buffering), so there is no separate short-final-
                // packet case, but the container-level symptom is
                // identical -- a container that derives a track's total
                // duration from summed `Packet::duration` (MP4's `stts`,
                // Ogg's granule position) silently undercounts by every
                // packet's real length when this stays at the `Duration`
                // default. Computed from the frame's own `samples`/
                // `sample_rate` rather than trusted from `frame.duration`,
                // because raw PCM's own decode side does not set that
                // field either (a related, separate gap).
                let time_base = Rational::new(1, i32::try_from(*sample_rate).unwrap_or(1).max(1));
                packet.duration = Timestamp::new(i64::from(*samples))
                    .to_duration(time_base)
                    .unwrap_or(Duration::ZERO);
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

macro_rules! pcm_decoder_desc {
    ($ident:ident, $make:ident, $id:expr, $name:literal, $long_name:literal) => {
        fn $make(limits: Limits) -> Box<dyn Decoder> {
            Box::new(AsDecoder(Validated::new(PcmDecoder::new(limits, $id))))
        }

        #[doc = concat!("`", $name, "` decoder registration.")]
        pub static $ident: DecoderDesc = DecoderDesc {
            name: $name,
            long_name: $long_name,
            id: $id,
            media_type: MediaType::Audio,
            caps: Caps::empty(),
            supported_rates: &[],
            make: $make,
        };
    };
}

macro_rules! pcm_encoder_desc {
    ($ident:ident, $make:ident, $id:expr, $name:literal, $long_name:literal) => {
        fn $make(limits: Limits) -> Box<dyn Encoder> {
            Box::new(AsEncoder(Validated::new(PcmEncoder::new(limits, $id))))
        }

        #[doc = concat!("`", $name, "` encoder registration.")]
        pub static $ident: EncoderDesc = EncoderDesc {
            name: $name,
            long_name: $long_name,
            id: $id,
            media_type: MediaType::Audio,
            caps: Caps::empty(),
            supported_rates: &[],
            make: $make,
        };
    };
}

pcm_decoder_desc!(
    PCM_ALAW_DECODER,
    make_dec_alaw,
    CodecId::PcmAlaw,
    "pcm_alaw",
    "PCM A-law / G.711 A-law"
);
pcm_decoder_desc!(
    PCM_MULAW_DECODER,
    make_dec_mulaw,
    CodecId::PcmMulaw,
    "pcm_mulaw",
    "PCM mu-law / G.711 mu-law"
);
pcm_decoder_desc!(
    PCM_S8_DECODER,
    make_dec_s8,
    CodecId::PcmS8,
    "pcm_s8",
    "PCM signed 8-bit"
);
pcm_decoder_desc!(
    PCM_U8_DECODER,
    make_dec_u8,
    CodecId::PcmU8,
    "pcm_u8",
    "PCM unsigned 8-bit"
);
pcm_decoder_desc!(
    PCM_S16LE_DECODER,
    make_dec_s16le,
    CodecId::PcmS16le,
    "pcm_s16le",
    "PCM signed 16-bit little-endian"
);
pcm_decoder_desc!(
    PCM_S16BE_DECODER,
    make_dec_s16be,
    CodecId::PcmS16be,
    "pcm_s16be",
    "PCM signed 16-bit big-endian"
);
pcm_decoder_desc!(
    PCM_U16LE_DECODER,
    make_dec_u16le,
    CodecId::PcmU16le,
    "pcm_u16le",
    "PCM unsigned 16-bit little-endian"
);
pcm_decoder_desc!(
    PCM_U16BE_DECODER,
    make_dec_u16be,
    CodecId::PcmU16be,
    "pcm_u16be",
    "PCM unsigned 16-bit big-endian"
);
pcm_decoder_desc!(
    PCM_S24LE_DECODER,
    make_dec_s24le,
    CodecId::PcmS24le,
    "pcm_s24le",
    "PCM signed 24-bit little-endian"
);
pcm_decoder_desc!(
    PCM_S24BE_DECODER,
    make_dec_s24be,
    CodecId::PcmS24be,
    "pcm_s24be",
    "PCM signed 24-bit big-endian"
);
pcm_decoder_desc!(
    PCM_U24LE_DECODER,
    make_dec_u24le,
    CodecId::PcmU24le,
    "pcm_u24le",
    "PCM unsigned 24-bit little-endian"
);
pcm_decoder_desc!(
    PCM_U24BE_DECODER,
    make_dec_u24be,
    CodecId::PcmU24be,
    "pcm_u24be",
    "PCM unsigned 24-bit big-endian"
);
pcm_decoder_desc!(
    PCM_S32LE_DECODER,
    make_dec_s32le,
    CodecId::PcmS32le,
    "pcm_s32le",
    "PCM signed 32-bit little-endian"
);
pcm_decoder_desc!(
    PCM_S32BE_DECODER,
    make_dec_s32be,
    CodecId::PcmS32be,
    "pcm_s32be",
    "PCM signed 32-bit big-endian"
);
pcm_decoder_desc!(
    PCM_U32LE_DECODER,
    make_dec_u32le,
    CodecId::PcmU32le,
    "pcm_u32le",
    "PCM unsigned 32-bit little-endian"
);
pcm_decoder_desc!(
    PCM_U32BE_DECODER,
    make_dec_u32be,
    CodecId::PcmU32be,
    "pcm_u32be",
    "PCM unsigned 32-bit big-endian"
);
pcm_decoder_desc!(
    PCM_F32LE_DECODER,
    make_dec_f32le,
    CodecId::PcmF32le,
    "pcm_f32le",
    "PCM 32-bit floating point little-endian"
);
pcm_decoder_desc!(
    PCM_F32BE_DECODER,
    make_dec_f32be,
    CodecId::PcmF32be,
    "pcm_f32be",
    "PCM 32-bit floating point big-endian"
);
pcm_decoder_desc!(
    PCM_F64LE_DECODER,
    make_dec_f64le,
    CodecId::PcmF64le,
    "pcm_f64le",
    "PCM 64-bit floating point little-endian"
);
pcm_decoder_desc!(
    PCM_F64BE_DECODER,
    make_dec_f64be,
    CodecId::PcmF64be,
    "pcm_f64be",
    "PCM 64-bit floating point big-endian"
);
pcm_decoder_desc!(
    PCM_VIDC_DECODER,
    make_dec_vidc,
    CodecId::PcmVidc,
    "pcm_vidc",
    "PCM Archimedes VIDC"
);

pcm_encoder_desc!(
    PCM_ALAW_ENCODER,
    make_enc_alaw,
    CodecId::PcmAlaw,
    "pcm_alaw",
    "PCM A-law / G.711 A-law"
);
pcm_encoder_desc!(
    PCM_MULAW_ENCODER,
    make_enc_mulaw,
    CodecId::PcmMulaw,
    "pcm_mulaw",
    "PCM mu-law / G.711 mu-law"
);
pcm_encoder_desc!(
    PCM_S8_ENCODER,
    make_enc_s8,
    CodecId::PcmS8,
    "pcm_s8",
    "PCM signed 8-bit"
);
pcm_encoder_desc!(
    PCM_U8_ENCODER,
    make_enc_u8,
    CodecId::PcmU8,
    "pcm_u8",
    "PCM unsigned 8-bit"
);
pcm_encoder_desc!(
    PCM_S16LE_ENCODER,
    make_enc_s16le,
    CodecId::PcmS16le,
    "pcm_s16le",
    "PCM signed 16-bit little-endian"
);
pcm_encoder_desc!(
    PCM_S16BE_ENCODER,
    make_enc_s16be,
    CodecId::PcmS16be,
    "pcm_s16be",
    "PCM signed 16-bit big-endian"
);
pcm_encoder_desc!(
    PCM_U16LE_ENCODER,
    make_enc_u16le,
    CodecId::PcmU16le,
    "pcm_u16le",
    "PCM unsigned 16-bit little-endian"
);
pcm_encoder_desc!(
    PCM_U16BE_ENCODER,
    make_enc_u16be,
    CodecId::PcmU16be,
    "pcm_u16be",
    "PCM unsigned 16-bit big-endian"
);
pcm_encoder_desc!(
    PCM_S24LE_ENCODER,
    make_enc_s24le,
    CodecId::PcmS24le,
    "pcm_s24le",
    "PCM signed 24-bit little-endian"
);
pcm_encoder_desc!(
    PCM_S24BE_ENCODER,
    make_enc_s24be,
    CodecId::PcmS24be,
    "pcm_s24be",
    "PCM signed 24-bit big-endian"
);
pcm_encoder_desc!(
    PCM_U24LE_ENCODER,
    make_enc_u24le,
    CodecId::PcmU24le,
    "pcm_u24le",
    "PCM unsigned 24-bit little-endian"
);
pcm_encoder_desc!(
    PCM_U24BE_ENCODER,
    make_enc_u24be,
    CodecId::PcmU24be,
    "pcm_u24be",
    "PCM unsigned 24-bit big-endian"
);
pcm_encoder_desc!(
    PCM_S32LE_ENCODER,
    make_enc_s32le,
    CodecId::PcmS32le,
    "pcm_s32le",
    "PCM signed 32-bit little-endian"
);
pcm_encoder_desc!(
    PCM_S32BE_ENCODER,
    make_enc_s32be,
    CodecId::PcmS32be,
    "pcm_s32be",
    "PCM signed 32-bit big-endian"
);
pcm_encoder_desc!(
    PCM_U32LE_ENCODER,
    make_enc_u32le,
    CodecId::PcmU32le,
    "pcm_u32le",
    "PCM unsigned 32-bit little-endian"
);
pcm_encoder_desc!(
    PCM_U32BE_ENCODER,
    make_enc_u32be,
    CodecId::PcmU32be,
    "pcm_u32be",
    "PCM unsigned 32-bit big-endian"
);
pcm_encoder_desc!(
    PCM_F32LE_ENCODER,
    make_enc_f32le,
    CodecId::PcmF32le,
    "pcm_f32le",
    "PCM 32-bit floating point little-endian"
);
pcm_encoder_desc!(
    PCM_F32BE_ENCODER,
    make_enc_f32be,
    CodecId::PcmF32be,
    "pcm_f32be",
    "PCM 32-bit floating point big-endian"
);
pcm_encoder_desc!(
    PCM_F64LE_ENCODER,
    make_enc_f64le,
    CodecId::PcmF64le,
    "pcm_f64le",
    "PCM 64-bit floating point little-endian"
);
pcm_encoder_desc!(
    PCM_F64BE_ENCODER,
    make_enc_f64be,
    CodecId::PcmF64be,
    "pcm_f64be",
    "PCM 64-bit floating point big-endian"
);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the decoder, not the untrusted-input surface"
)]
mod tests {
    use super::*;
    use vaco_core::Error;

    fn make_wire_i16(samples: &[i16]) -> Vec<u8> {
        let mut out = Vec::new();
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn decoder_default_is_44100_mono() {
        let dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le);
        assert_eq!(dec.sample_rate, DEFAULT_SAMPLE_RATE);
        assert_eq!(dec.layout.channels, 1);
    }

    /// The decode-side mirror of `send_frame_sets_a_real_nonzero_packet_duration`
    /// above: `PcmDecoder::send` set `frame.pts` but never `frame.duration`,
    /// even though the ingredients (decoded sample count and the
    /// configured sample rate) were already in scope. Every real video
    /// decoder in this tree sets `frame.duration`; this was the one
    /// exception on the audio side.
    #[test]
    fn send_sets_a_real_nonzero_frame_duration() {
        let wire = make_wire_i16(&[0, 100, -100, 32767, -32768, 12345, -12345]);
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &wire).expect("packet");

        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le)
            .with_audio_params(8_000, ChannelLayout::MONO);
        dec.send(Some(&pkt)).expect("send");
        let frame = dec.receive().expect("frame");

        // 7 samples at 8000 Hz.
        let expected = vaco_core::Timestamp::new(7)
            .to_duration(vaco_core::Rational::new(1, 8_000))
            .expect("duration");
        assert_ne!(expected, vaco_core::Duration::ZERO);
        assert_eq!(frame.duration, expected);
    }

    #[test]
    fn set_extradata_overrides_defaults() {
        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le);
        dec.set_extradata(&audio_extradata(8000, 2)).expect("ok");
        assert_eq!(dec.sample_rate, 8000);
        assert_eq!(dec.layout.channels, 2);
    }

    /// `Decoder::prime_audio` is the channel a registry-built decoder
    /// actually receives the container's declared rate/layout through — a
    /// real container hands the CLI's demuxer output, not a hand-built
    /// `audio_extradata` record `set_extradata` expects, so a decoder that
    /// only honoured `set_extradata` decoded every WAV at
    /// `DEFAULT_SAMPLE_RATE`/mono regardless of what the file's `fmt ` chunk
    /// said (invisible for a mono 44100 Hz asset, wrong for anything else,
    /// and load-bearing once a resampler downstream trusts the decoded rate).
    #[test]
    fn prime_audio_overrides_defaults() {
        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le);
        dec.prime_audio(48_000, ChannelLayout::STEREO);
        assert_eq!(dec.sample_rate, 48_000);
        assert_eq!(dec.layout, ChannelLayout::STEREO);
    }

    /// Mirrors `with_audio_params`'s own contract: a zero rate or an
    /// unspecified (zero-channel) layout means "not yet known" and must not
    /// clobber whatever the decoder already had.
    #[test]
    fn prime_audio_treats_zero_as_not_yet_known() {
        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le);
        dec.prime_audio(48_000, ChannelLayout::STEREO);
        dec.prime_audio(0, ChannelLayout::unspecified(0));
        assert_eq!(dec.sample_rate, 48_000);
        assert_eq!(dec.layout, ChannelLayout::STEREO);
    }

    #[test]
    fn malformed_extradata_is_ignored_not_erred() {
        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le);
        dec.set_extradata(&[1, 2, 3]).expect("ignored, not an error");
        assert_eq!(dec.sample_rate, DEFAULT_SAMPLE_RATE);
    }

    #[test]
    fn full_send_receive_round_trip_mono() {
        let wire = make_wire_i16(&[0, 100, -100, 32767, -32768]);
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &wire).expect("packet");

        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le);
        dec.send(Some(&pkt)).expect("send");
        let frame = dec.receive().expect("frame");
        let FrameData::Audio { samples, .. } = &frame.data else {
            panic!("audio frame")
        };
        assert_eq!(*samples, 5);

        let mut enc = PcmEncoder::new(Limits::permissive(), CodecId::PcmS16le);
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("packet");
        assert_eq!(packet.payload(), wire.as_slice());
    }

    /// Same bug class as `vaco-codec-flac`/`vaco-codec-alac`/
    /// `vaco-codec-vorbis`'s encoders: `PcmEncoder::send` set `packet.pts`
    /// but never `packet.duration`, which a container deriving a track's
    /// total length from summed packet durations (MP4's `stts`, Ogg's
    /// granule position) silently undercounts by. Every `PcmEncoder` is a
    /// 1:1 wrapper -- exactly one packet per input frame, no internal
    /// buffering -- so unlike the block-coder cases there is no separate
    /// short-final-packet shape here: the fix must simply compute a real,
    /// non-zero duration from the frame's own sample count and rate on
    /// every call, not only sometimes.
    #[test]
    fn send_frame_sets_a_real_nonzero_packet_duration() {
        let wire = make_wire_i16(&[0, 100, -100, 32767, -32768, 12345, -12345]);
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &wire).expect("packet");

        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le)
            .with_audio_params(8_000, ChannelLayout::MONO);
        dec.send(Some(&pkt)).expect("send");
        let frame = dec.receive().expect("frame");

        let mut enc = PcmEncoder::new(Limits::permissive(), CodecId::PcmS16le);
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("packet");

        // 7 samples at 8000 Hz.
        let expected = vaco_core::Timestamp::new(7)
            .to_duration(vaco_core::Rational::new(1, 8_000))
            .expect("duration");
        assert_ne!(expected, vaco_core::Duration::ZERO);
        assert_eq!(packet.duration, expected);
    }

    #[test]
    fn protocol_shape_matches_every_other_codec_in_the_tree() {
        let wire = make_wire_i16(&[1, 2, 3]);
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &wire).expect("packet");
        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmS16le);
        dec.send(Some(&pkt)).expect("send");
        let _ = dec.receive().expect("frame");
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    #[test]
    fn stereo_alaw_round_trips_through_send_receive() {
        // A-law is lossy in general, but the codes the encoder itself
        // produces are exactly the ones the decoder expects back out.
        let wire: Vec<u8> = vec![0x2A, 0x3B, 0x55, 0x80];
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &wire).expect("packet");
        let mut dec = PcmDecoder::new(Limits::permissive(), CodecId::PcmAlaw)
            .with_audio_params(8000, ChannelLayout::STEREO);
        dec.send(Some(&pkt)).expect("send");
        let frame = dec.receive().expect("frame");
        let mut enc = PcmEncoder::new(Limits::permissive(), CodecId::PcmAlaw)
            .with_audio_params(ChannelLayout::STEREO);
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("packet");
        assert_eq!(packet.payload(), wire.as_slice());
    }

    #[test]
    fn every_decoder_descriptor_builds_and_decodes_something() {
        let descs: &[&DecoderDesc] = &[
            &PCM_ALAW_DECODER,
            &PCM_MULAW_DECODER,
            &PCM_S8_DECODER,
            &PCM_U8_DECODER,
            &PCM_S16LE_DECODER,
            &PCM_S16BE_DECODER,
            &PCM_U16LE_DECODER,
            &PCM_U16BE_DECODER,
            &PCM_S24LE_DECODER,
            &PCM_S24BE_DECODER,
            &PCM_U24LE_DECODER,
            &PCM_U24BE_DECODER,
            &PCM_S32LE_DECODER,
            &PCM_S32BE_DECODER,
            &PCM_U32LE_DECODER,
            &PCM_U32BE_DECODER,
            &PCM_F32LE_DECODER,
            &PCM_F32BE_DECODER,
            &PCM_F64LE_DECODER,
            &PCM_F64BE_DECODER,
            &PCM_VIDC_DECODER,
        ];
        assert_eq!(descs.len(), 21);
        for desc in descs {
            let format = resolve(desc.id);
            let width = format.container_bytes as usize;
            let zero = vec![0u8; width * 4];
            let mut decoder = desc.build(Limits::permissive());
            let mut budget = Budget::new(Limits::permissive());
            let pkt = Packet::from_slice(&mut budget, &zero).expect("packet");
            decoder.send_packet(Some(&pkt)).expect("send");
            let frame = decoder.receive_frame().expect("frame");
            let FrameData::Audio { samples, .. } = &frame.data else {
                panic!("audio frame")
            };
            assert_eq!(*samples, 4, "{}", desc.name);
        }
    }

    #[test]
    fn every_encoder_descriptor_builds_and_encodes_something() {
        let descs: &[&EncoderDesc] = &[
            &PCM_ALAW_ENCODER,
            &PCM_MULAW_ENCODER,
            &PCM_S8_ENCODER,
            &PCM_U8_ENCODER,
            &PCM_S16LE_ENCODER,
            &PCM_S16BE_ENCODER,
            &PCM_U16LE_ENCODER,
            &PCM_U16BE_ENCODER,
            &PCM_S24LE_ENCODER,
            &PCM_S24BE_ENCODER,
            &PCM_U24LE_ENCODER,
            &PCM_U24BE_ENCODER,
            &PCM_S32LE_ENCODER,
            &PCM_S32BE_ENCODER,
            &PCM_U32LE_ENCODER,
            &PCM_U32BE_ENCODER,
            &PCM_F32LE_ENCODER,
            &PCM_F32BE_ENCODER,
            &PCM_F64LE_ENCODER,
            &PCM_F64BE_ENCODER,
        ];
        assert_eq!(descs.len(), 20);
        for desc in descs {
            let format = resolve(desc.id);
            let mut budget = Budget::new(Limits::permissive());
            let layout = ChannelLayout::MONO;
            let frame = Frame::alloc_audio(&mut budget, format.decoded, layout, 4, 8000)
                .expect("alloc audio");
            let mut encoder = desc.build(Limits::permissive());
            assert_eq!(
                encoder.accepted_sample_fmts(),
                &[format.decoded],
                "{}: accepted_sample_fmts must name exactly this codec's own wire format",
                desc.name
            );
            encoder.send_frame(Some(&frame)).expect("send");
            let packet = encoder.receive_packet().expect("packet");
            assert_eq!(
                packet.payload().len(),
                4 * format.container_bytes as usize,
                "{}",
                desc.name
            );
        }
    }

    /// E2E-GAPS 3: before `accepted_sample_fmts` existed, a mismatch was only
    /// discoverable by calling `send_frame` and parsing the resulting error —
    /// exactly the state `vaco-codec-qoi`'s `accepted_pix_fmts` override
    /// already fixed on the video side. This is the direct check that a
    /// caller can now ask *before* sending: a `pcm_s16le` encoder declares
    /// packed `S16` and nothing else.
    #[test]
    fn accepted_sample_fmts_names_the_one_format_send_frame_will_actually_accept() {
        let enc = PcmEncoder::new(Limits::permissive(), CodecId::PcmS16le);
        assert_eq!(enc.accepted_sample_fmts(), &[vaco_sampfmt::SampleFmt::S16]);

        let enc = PcmEncoder::new(Limits::permissive(), CodecId::PcmF32le);
        assert_eq!(enc.accepted_sample_fmts(), &[vaco_sampfmt::SampleFmt::F32]);
    }
}
