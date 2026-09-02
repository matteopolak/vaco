//! Raw and lightly-packed uncompressed video: `rawvideo`, `bitpacked`,
//! `wrapped_avframe`, `v210`/`v210x`, `r10k`/`r210`, `y41p`, `avui`.
//!
//! # What it is
//!
//! Nine `CodecId` identities, all video, all
//! `CodecProperties::LOSSLESS | INTRA_ONLY`, sharing one property: none of
//! them is compressed. What differs between them is only how pixels are
//! packed into bytes — the pure conversion routines are the real content of
//! this crate ([`raw`], [`v210`], [`rgb10`], [`y41p`]), and [`RawVideoDecoder`]/
//! [`RawVideoEncoder`] are one thin `SendReceive` wrapper shared by all nine,
//! parameterised at construction by [`Packing`], following the same
//! table/macro shape `vaco-codec-pcm` uses for its 21-member `pcm_*` family.
//!
//! # How it works
//!
//! * **`rawvideo`, `bitpacked`, `wrapped_avframe`** ([`Packing::Configurable`]):
//!   byte-identical pixel-plane pass-through. [`raw::decode_raw`]/
//!   [`raw::encode_raw`] do the copy; see that module's docs for exactly why
//!   it cannot be a single `memcpy`. Read `vaco-demux-raw::rawvideo`'s crate
//!   docs first if you have not — this crate's `Packing::Configurable` is the
//!   codec-side mirror of that demuxer's `Packing::PixFmtPlanes`, built from
//!   the same measurement. `wrapped_avframe` is the reference's own
//!   passthrough pseudo-codec (packet payload *is* the frame's raw pixels,
//!   whatever format the caller configured) and needs nothing beyond that
//!   same routine.
//! * **`v210`, `v210x`** ([`Packing::V210`]): SMPTE 292M/424M 10-bit 4:2:2
//!   packing into [`vaco_pixfmt::PixFmt::Yuv422p10le`]. See [`v210`]'s docs
//!   for the exact bit layout and, importantly, what is and is not verified.
//! * **`r210`** ([`Packing::R210`]): its wire format is bit-for-bit identical
//!   to `x2rgb10be`'s in-memory layout, so it reuses [`raw::decode_raw`]/
//!   [`raw::encode_raw`] directly rather than needing dedicated code — see
//!   [`rgb10`]'s docs for why.
//! * **`r10k`** ([`Packing::R10k`]): the AJA Kona convention, same three
//!   10-bit components as `r210` but with the padding bits in a different
//!   position, needing a real per-pixel bit shift. See [`rgb10`].
//! * **`y41p`** ([`Packing::Y41p`]): 4:1:1 packed YUV, 8 pixels to 12 bytes.
//!   See [`y41p`].
//! * **`avui`** ([`Packing::Avui`]): Avid Meridien Uncompressed. No
//!   sufficiently confident public description of its exact byte layout was
//!   found in this pass, so rather than guess at a packing this crate cannot
//!   verify even structurally, the identity is registered and both directions
//!   return [`vaco_core::Error::Unsupported`] with a message that says so.
//!   This is the "document the gap explicitly" option the issue brief calls
//!   out as preferable to a wild guess.
//!
//! # Where width/height/pixel-format come from
//!
//! A raw-video packet carries no header — the container states the geometry,
//! not the codec — the identical problem `vaco-codec-pcm` has for sample
//! rate/channel count. This crate follows that crate's precedent exactly:
//! [`RawVideoDecoder::with_video_params`] for a caller that already knows the
//! geometry, and (documented as provisional, same status as
//! `vaco_codec_pcm::parse_audio_extradata`, pending the shared registry-to-CLI
//! codec-parameter convention #652 is expected to bring) [`parse_video_extradata`]
//! for a decoder reached only through [`vaco_codec_core::DecoderDesc::make`]'s
//! fixed `fn(Limits) -> Box<dyn Decoder>` signature, via
//! [`vaco_codec_core::Decoder::set_extradata`]. `v210`/`v210x`/`r10k`/`r210`/
//! `y41p` need only width/height (their pixel format is fixed by the codec
//! identity); `rawvideo`/`bitpacked`/`wrapped_avframe` need a pixel format
//! too, defaulting to [`DEFAULT_PIXEL_FORMAT`] (matching
//! `vaco_demux_raw::rawvideo::DEFAULT_PIXEL_FORMAT`'s own `yuv420p` default,
//! by convention rather than by dependency — this crate does not depend on
//! `vaco-demux-raw`).
//!
//! A `0`/`0` width or height is not defaulted to anything: it is treated the
//! same way `vaco-demux-raw::rawvideo` treats it, as
//! [`vaco_core::Error::InvalidData`] ("picture size 0x0 is invalid"), since
//! there is no meaningful default frame size. A registry-built decoder that
//! is never configured will therefore refuse every packet until
//! [`RawVideoDecoder::with_video_params`] or `set_extradata` gives it real
//! dimensions.
//!
//! An encoder never needs any of this configuration: every
//! [`vaco_frame::Frame`] it is sent already carries its own width, height and
//! pixel format, exactly as `vaco-codec-qoi`'s encoder reads geometry
//! straight off the frame it is given.
//!
//! # How to change it
//!
//! Add a [`Packing`] variant, a pure decode/encode function pair in its own
//! module, an arm in [`decode_for`]/[`encode_for`]/[`accepted_pix_fmts_for`],
//! and a `raw_desc!` invocation. [`RawVideoDecoder`]/[`RawVideoEncoder`]
//! should never need a match on `CodecId` themselves — only on `Packing`.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds decode allocation like every other decoder
//! in this tree.
//!
//! # Dependencies
//!
//! `vaco-codec-core` (the protocol), `vaco-frame`/`vaco-pixfmt`/`vaco-pool`
//! (the decoded picture), `vaco-packet` (the encoded bytes), `vaco-limits`
//! (allocation bounds).

#![forbid(unsafe_code)]

mod raw;
mod rgb10;
mod v210;
mod y41p;

pub use raw::{decode_raw, encode_raw};
pub use rgb10::{decode_r10k, encode_r10k};

use vaco_codec_core::{
    Accept, AsDecoder, AsEncoder, Caps, CodecId, Decoder, DecoderDesc, Encoder, EncoderDesc,
    Machine, SendReceive, Validated,
};
use vaco_core::{Error, MediaType, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

/// Reference default for `-pixel_format` on `rawvideo`/`bitpacked` with no
/// configuration at all — matches `vaco_demux_raw::rawvideo::DEFAULT_PIXEL_FORMAT`.
pub const DEFAULT_PIXEL_FORMAT: PixFmt = PixFmt::Yuv420p;

/// How one member of this family turns bytes into pixels. See the crate docs
/// for what each variant covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    /// `rawvideo`, `bitpacked`, `wrapped_avframe`: byte-identical
    /// [`vaco_pixfmt::PixFmt`]-plane copy at a caller-configured pixel format.
    Configurable,
    /// `v210`, `v210x`: SMPTE 10-bit 4:2:2 packing, fixed
    /// [`PixFmt::Yuv422p10le`] target.
    V210,
    /// `r210`: fixed [`PixFmt::X2rgb10be`] target, wire-identical to it.
    R210,
    /// `r10k`: fixed [`PixFmt::X2rgb10be`] target, AJA bit-shifted wire.
    R10k,
    /// `y41p`: fixed [`PixFmt::Yuv411p`] target, 4:1:1 packed wire.
    Y41p,
    /// `avui`: not implemented; both directions return
    /// [`Error::Unsupported`]. See the crate docs for why.
    Avui,
}

fn decode_for(
    packing: Packing,
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_format: PixFmt,
    budget: &mut Budget,
) -> Result<Frame> {
    match packing {
        Packing::Configurable => raw::decode_raw(payload, width, height, pixel_format, budget),
        Packing::V210 => v210::decode(payload, width, height, budget),
        Packing::R210 => raw::decode_raw(payload, width, height, PixFmt::X2rgb10be, budget),
        Packing::R10k => rgb10::decode_r10k(payload, width, height, budget),
        Packing::Y41p => y41p::decode(payload, width, height, budget),
        Packing::Avui => Err(Error::Unsupported(
            "avui: decoder not implemented (no sufficiently confident public description of \
             the Avid Meridien packing was available; see crate docs)",
        )),
    }
}

fn encode_for(packing: Packing, frame: &Frame) -> Result<Vec<u8>> {
    match packing {
        Packing::Configurable | Packing::R210 => raw::encode_raw(frame),
        Packing::V210 => v210::encode(frame),
        Packing::R10k => rgb10::encode_r10k(frame),
        Packing::Y41p => y41p::encode(frame),
        Packing::Avui => Err(Error::Unsupported(
            "avui: encoder not implemented (no sufficiently confident public description of \
             the Avid Meridien packing was available; see crate docs)",
        )),
    }
}

/// Pixel formats [`RawVideoEncoder::send`] accepts for a fixed-packing
/// member; empty for [`Packing::Configurable`], matching
/// [`vaco_codec_core::Encoder::accepted_pix_fmts`]'s "whatever arrives"
/// default — a `rawvideo`/`bitpacked`/`wrapped_avframe` packet's pixel format
/// is whatever the container says it is, not something this codec fixes.
fn accepted_pix_fmts_for(packing: Packing) -> &'static [PixFmt] {
    match packing {
        Packing::Configurable | Packing::Avui => &[],
        Packing::V210 => &[PixFmt::Yuv422p10le],
        Packing::R210 | Packing::R10k => &[PixFmt::X2rgb10be],
        Packing::Y41p => &[PixFmt::Yuv411p],
    }
}

/// Reads the `(width: u32 LE, height: u32 LE, pixel_format_name: UTF-8)`
/// record this crate accepts through [`Decoder::set_extradata`].
///
/// **Provisional**, for the identical reason
/// `vaco_codec_pcm::parse_audio_extradata` is: `Decoder::set_extradata`'s own
/// doc names exactly this situation — "any codec whose configuration is the
/// container's to state... has the identical shape" as an
/// `AudioSpecificConfig` — but no shared wire format for "the container's raw
/// video parameters" exists in this workspace yet (`planning/ASSIGNMENTS.md`'s
/// `agent:codec-path` row, #652, is building the registry-to-CLI codec path
/// this would plug into). Until that lands, this crate defines its own
/// minimal record. A caller that already knows the geometry should prefer
/// [`RawVideoDecoder::with_video_params`] directly; this exists for the
/// `DecoderDesc::make` path, whose signature has no room for parameters at
/// all.
///
/// The pixel-format name is only meaningful for [`Packing::Configurable`]
/// decoders; a fixed-packing decoder (`v210`, `r10k`, ...) ignores it. It may
/// be empty (width/height only), in which case the pixel format returned is
/// `None`.
///
/// Malformed or zero-valued input is ignored, not an error — matching the
/// trait's "this record told me nothing" contract for a merely-offered
/// configuration.
#[must_use]
pub fn parse_video_extradata(extradata: &[u8]) -> Option<(u32, u32, Option<PixFmt>)> {
    let width_bytes = extradata.get(0..4)?;
    let height_bytes = extradata.get(4..8)?;
    let &[wa, wb, wc, wd] = width_bytes else {
        return None;
    };
    let &[ha, hb, hc, hd] = height_bytes else {
        return None;
    };
    let width = u32::from_le_bytes([wa, wb, wc, wd]);
    let height = u32::from_le_bytes([ha, hb, hc, hd]);
    if width == 0 || height == 0 {
        return None;
    }
    let rest = extradata.get(8..).unwrap_or(&[]);
    let format = if rest.is_empty() {
        None
    } else {
        std::str::from_utf8(rest)
            .ok()
            .and_then(|name| PixFmt::from_name(name).ok())
    };
    Some((width, height, format))
}

/// Build the record [`parse_video_extradata`] reads, for a caller that wants
/// to configure a registry-built decoder through
/// [`Decoder::set_extradata`]. `pixel_format` is only meaningful for
/// [`Packing::Configurable`] identities; pass `None` for `v210`/`r10k`/etc.
#[must_use]
pub fn video_extradata(width: u32, height: u32, pixel_format: Option<PixFmt>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    if let Some(fmt) = pixel_format {
        out.extend_from_slice(fmt.descriptor().name.as_bytes());
    }
    out
}

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`] for one member of this
/// family, chosen at construction by [`Packing`].
#[derive(Debug)]
pub struct RawVideoDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    id: CodecId,
    packing: Packing,
    width: u32,
    height: u32,
    pixel_format: PixFmt,
}

impl RawVideoDecoder {
    /// A decoder for `id`/`packing`, bounded by `limits`. Geometry defaults
    /// to `0x0` (invalid — see the crate docs) until configured via
    /// [`RawVideoDecoder::with_video_params`] or `set_extradata`.
    #[must_use]
    pub fn new(limits: Limits, id: CodecId, packing: Packing) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            id,
            packing,
            width: 0,
            height: 0,
            pixel_format: DEFAULT_PIXEL_FORMAT,
        }
    }

    /// The codec identity this instance was built for.
    #[must_use]
    pub const fn id(&self) -> CodecId {
        self.id
    }

    /// Configure the container's own geometry directly, bypassing
    /// [`Decoder::set_extradata`]'s byte record. `pixel_format` is ignored
    /// for a fixed-packing identity.
    #[must_use]
    pub fn with_video_params(mut self, width: u32, height: u32, pixel_format: PixFmt) -> Self {
        self.width = width;
        self.height = height;
        self.pixel_format = pixel_format;
        self
    }
}

impl SendReceive for RawVideoDecoder {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some((width, height, format)) = parse_video_extradata(extradata) {
            self.width = width;
            self.height = height;
            if let Some(format) = format {
                self.pixel_format = format;
            }
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
                let Some(pkt) = input else {
                    return Ok(());
                };
                let mut budget = Budget::new(self.limits.clone());
                let mut frame = decode_for(
                    self.packing,
                    pkt.payload(),
                    self.width,
                    self.height,
                    self.pixel_format,
                    &mut budget,
                )?;
                frame.pts = pkt.pts;
                // Every identity in this family is `CodecProperties::INTRA_ONLY`:
                // there is no inter-frame prediction, so every decoded frame is
                // a keyframe by construction.
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`] for one member of this
/// family. Needs no configuration: geometry and pixel format come straight
/// off each [`Frame`] it is sent, the same way `vaco-codec-qoi`'s encoder
/// works.
#[derive(Debug)]
pub struct RawVideoEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    packing: Packing,
}

impl RawVideoEncoder {
    /// An encoder for `packing`, bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits, packing: Packing) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            packing,
        }
    }
}

impl SendReceive for RawVideoEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [PixFmt] {
        accepted_pix_fmts_for(self.packing)
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
                let bytes = encode_for(self.packing, frame)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &bytes)?;
                packet.pts = frame.pts;
                // Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`'s
                // encoders: never set `Packet::duration`. This is the one
                // real-world video-stream case among this batch (`-c:v
                // rawvideo` into AVI/MOV/MKV/MP4), so propagation from the
                // input `Frame` matters exactly the way it did for VP8/VP9
                // -- a container deriving a track's total length from
                // summed packet durations was silently undercounting it.
                packet.duration = frame.duration;
                // As on the decode side: every identity here is intra-only,
                // so every packet is a keyframe.
                packet.flags = PacketFlags::KEY;
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

macro_rules! raw_video_desc {
    ($dec_ident:ident, $make_dec:ident, $enc_ident:ident, $make_enc:ident, $id:expr, $name:literal, $long_name:literal, $packing:expr) => {
        fn $make_dec(limits: Limits) -> Box<dyn Decoder> {
            Box::new(AsDecoder(Validated::new(RawVideoDecoder::new(
                limits, $id, $packing,
            ))))
        }

        #[doc = concat!("`", $name, "` decoder registration.")]
        pub static $dec_ident: DecoderDesc = DecoderDesc {
            name: $name,
            long_name: $long_name,
            id: $id,
            media_type: MediaType::Video,
            caps: Caps::empty(),
            supported_rates: &[],
            make: $make_dec,
        };

        fn $make_enc(limits: Limits) -> Box<dyn Encoder> {
            Box::new(AsEncoder(Validated::new(RawVideoEncoder::new(
                limits, $packing,
            ))))
        }

        #[doc = concat!("`", $name, "` encoder registration.")]
        pub static $enc_ident: EncoderDesc = EncoderDesc {
            name: $name,
            long_name: $long_name,
            id: $id,
            media_type: MediaType::Video,
            caps: Caps::empty(),
            supported_rates: &[],
            make: $make_enc,
        };
    };
}

raw_video_desc!(
    RAWVIDEO_DECODER,
    make_dec_rawvideo,
    RAWVIDEO_ENCODER,
    make_enc_rawvideo,
    CodecId::Rawvideo,
    "rawvideo",
    "raw video",
    Packing::Configurable
);
raw_video_desc!(
    BITPACKED_DECODER,
    make_dec_bitpacked,
    BITPACKED_ENCODER,
    make_enc_bitpacked,
    CodecId::Bitpacked,
    "bitpacked",
    "Bitpacked",
    Packing::Configurable
);
raw_video_desc!(
    WRAPPED_AVFRAME_DECODER,
    make_dec_wrapped_avframe,
    WRAPPED_AVFRAME_ENCODER,
    make_enc_wrapped_avframe,
    CodecId::WrappedAvframe,
    "wrapped_avframe",
    "AVFrame to AVPacket passthrough",
    Packing::Configurable
);
raw_video_desc!(
    V210_DECODER,
    make_dec_v210,
    V210_ENCODER,
    make_enc_v210,
    CodecId::V210,
    "v210",
    "Uncompressed 4:2:2 10-bit",
    Packing::V210
);
raw_video_desc!(
    V210X_DECODER,
    make_dec_v210x,
    V210X_ENCODER,
    make_enc_v210x,
    CodecId::V210x,
    "v210x",
    "Uncompressed 4:2:2 10-bit",
    Packing::V210
);
raw_video_desc!(
    R210_DECODER,
    make_dec_r210,
    R210_ENCODER,
    make_enc_r210,
    CodecId::R210,
    "r210",
    "Uncompressed RGB 10-bit",
    Packing::R210
);
raw_video_desc!(
    R10K_DECODER,
    make_dec_r10k,
    R10K_ENCODER,
    make_enc_r10k,
    CodecId::R10k,
    "r10k",
    "AJA Kona 10-bit RGB Codec",
    Packing::R10k
);
raw_video_desc!(
    Y41P_DECODER,
    make_dec_y41p,
    Y41P_ENCODER,
    make_enc_y41p,
    CodecId::Y41p,
    "y41p",
    "Uncompressed YUV 4:1:1 12-bit",
    Packing::Y41p
);
raw_video_desc!(
    AVUI_DECODER,
    make_dec_avui,
    AVUI_ENCODER,
    make_enc_avui,
    CodecId::Avui,
    "avui",
    "Avid Meridien Uncompressed",
    Packing::Avui
);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_core::{Error, Timestamp};
    use vaco_frame::FrameData;

    #[test]
    fn decoder_defaults_to_zero_by_zero_which_is_invalid() {
        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::Rawvideo,
            Packing::Configurable,
        );
        let mut budget = Budget::new(Limits::permissive());
        let payload = Packet::from_slice(&mut budget, &[0u8; 16]).expect("packet");
        let err = dec.send(Some(&payload)).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn with_video_params_configures_a_registry_built_decoder() {
        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::Rawvideo,
            Packing::Configurable,
        )
        .with_video_params(4, 4, PixFmt::Gray8);
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[7u8; 16]).expect("packet");
        dec.send(Some(&pkt)).expect("send");
        let frame = dec.receive().expect("frame");
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = &frame.data
        else {
            panic!("video frame")
        };
        assert_eq!(*format, PixFmt::Gray8);
        assert_eq!(*width, 4);
        assert_eq!(*height, 4);
        assert!(frame.flags.contains(FrameFlags::KEY));
    }

    #[test]
    fn set_extradata_configures_width_height_and_format() {
        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::Rawvideo,
            Packing::Configurable,
        );
        dec.set_extradata(&video_extradata(4, 4, Some(PixFmt::Gray8)))
            .expect("ok");
        assert_eq!(dec.width, 4);
        assert_eq!(dec.height, 4);
        assert_eq!(dec.pixel_format, PixFmt::Gray8);
    }

    #[test]
    fn malformed_extradata_is_ignored_not_erred() {
        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::Rawvideo,
            Packing::Configurable,
        );
        dec.set_extradata(&[1, 2, 3])
            .expect("ignored, not an error");
        assert_eq!(dec.width, 0);
    }

    #[test]
    fn extradata_without_a_pixel_format_leaves_the_default() {
        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::Rawvideo,
            Packing::Configurable,
        );
        dec.set_extradata(&video_extradata(4, 4, None)).expect("ok");
        assert_eq!(dec.pixel_format, DEFAULT_PIXEL_FORMAT);
    }

    #[test]
    fn full_send_receive_round_trip_rawvideo() {
        let mut enc = RawVideoEncoder::new(Limits::permissive(), Packing::Configurable);
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 3, 2).expect("alloc");
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("packet");
        assert!(packet.is_key());

        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::Rawvideo,
            Packing::Configurable,
        )
        .with_video_params(3, 2, PixFmt::Rgb24);
        dec.send(Some(&packet)).expect("send");
        let decoded = dec.receive().expect("frame");
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = &decoded.data
        else {
            panic!("video frame")
        };
        assert_eq!(*format, PixFmt::Rgb24);
        assert_eq!((*width, *height), (3, 2));
    }

    #[test]
    fn wrapped_avframe_round_trips_like_rawvideo() {
        let mut enc = RawVideoEncoder::new(Limits::permissive(), Packing::Configurable);
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 4, 4).expect("alloc");
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("packet");

        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::WrappedAvframe,
            Packing::Configurable,
        )
        .with_video_params(4, 4, PixFmt::Yuv420p);
        dec.send(Some(&packet)).expect("send");
        let decoded = dec.receive().expect("frame");
        let FrameData::Video { format, .. } = &decoded.data else {
            panic!("video frame")
        };
        assert_eq!(*format, PixFmt::Yuv420p);
    }

    #[test]
    fn v210_round_trips_through_send_receive() {
        let mut enc = RawVideoEncoder::new(Limits::permissive(), Packing::V210);
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv422p10le, 6, 1).expect("alloc");
        enc.send(Some(&frame)).expect("send");
        let packet = enc.receive().expect("packet");

        let mut dec = RawVideoDecoder::new(Limits::permissive(), CodecId::V210, Packing::V210)
            .with_video_params(6, 1, PixFmt::Yuv422p10le);
        dec.send(Some(&packet)).expect("send");
        let decoded = dec.receive().expect("frame");
        let FrameData::Video { format, .. } = &decoded.data else {
            panic!("video frame")
        };
        assert_eq!(*format, PixFmt::Yuv422p10le);
    }

    #[test]
    fn avui_is_explicitly_unsupported_both_ways() {
        let mut dec = RawVideoDecoder::new(Limits::permissive(), CodecId::Avui, Packing::Avui)
            .with_video_params(4, 4, PixFmt::Yuv422p);
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[0u8; 32]).expect("packet");
        assert!(matches!(
            dec.send(Some(&pkt)).unwrap_err(),
            Error::Unsupported(_)
        ));

        let mut enc = RawVideoEncoder::new(Limits::permissive(), Packing::Avui);
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv422p, 4, 4).expect("alloc");
        assert!(matches!(
            enc.send(Some(&frame)).unwrap_err(),
            Error::Unsupported(_)
        ));
    }

    #[test]
    fn protocol_shape_matches_every_other_codec_in_the_tree() {
        let mut enc = RawVideoEncoder::new(Limits::permissive(), Packing::Configurable);
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 2, 2).expect("alloc");
        enc.send(Some(&frame)).expect("send");
        let _ = enc.receive().expect("packet");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("drain");
        assert!(matches!(enc.receive(), Err(Error::Eof)));
    }

    #[test]
    fn pts_flows_from_packet_to_frame_and_back() {
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, &[0u8; 16]).expect("packet");
        pkt.pts = Timestamp::new(42);
        let mut dec = RawVideoDecoder::new(
            Limits::permissive(),
            CodecId::Rawvideo,
            Packing::Configurable,
        )
        .with_video_params(4, 4, PixFmt::Gray8);
        dec.send(Some(&pkt)).expect("send");
        let frame = dec.receive().expect("frame");
        assert_eq!(frame.pts, Timestamp::new(42));

        let mut enc = RawVideoEncoder::new(Limits::permissive(), Packing::Configurable);
        enc.send(Some(&frame)).expect("send");
        let out = enc.receive().expect("packet");
        assert_eq!(out.pts, Timestamp::new(42));
    }

    /// Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`'s encoders:
    /// `send` never set `Packet::duration`, and this is the one codec in
    /// this batch with a genuine real-world video-stream use (`-c:v
    /// rawvideo` into AVI/MOV/MKV/MP4) -- a container deriving a track's
    /// total length from summed packet durations was silently
    /// undercounting it exactly the way it did for VP8/VP9.
    #[test]
    fn duration_flows_from_frame_to_packet() {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gray8, 4, 4).expect("alloc");
        frame.duration = vaco_core::Duration::from_micros(40_000);

        let mut enc = RawVideoEncoder::new(Limits::permissive(), Packing::Configurable);
        enc.send(Some(&frame)).expect("send");
        let out = enc.receive().expect("packet");
        assert_eq!(out.duration, vaco_core::Duration::from_micros(40_000));
    }

    #[test]
    fn every_descriptor_builds_and_names_the_reference_identity() {
        let decoders: &[(&DecoderDesc, &str, CodecId)] = &[
            (&RAWVIDEO_DECODER, "rawvideo", CodecId::Rawvideo),
            (&BITPACKED_DECODER, "bitpacked", CodecId::Bitpacked),
            (
                &WRAPPED_AVFRAME_DECODER,
                "wrapped_avframe",
                CodecId::WrappedAvframe,
            ),
            (&V210_DECODER, "v210", CodecId::V210),
            (&V210X_DECODER, "v210x", CodecId::V210x),
            (&R210_DECODER, "r210", CodecId::R210),
            (&R10K_DECODER, "r10k", CodecId::R10k),
            (&Y41P_DECODER, "y41p", CodecId::Y41p),
            (&AVUI_DECODER, "avui", CodecId::Avui),
        ];
        assert_eq!(decoders.len(), 9);
        for (desc, name, id) in decoders {
            assert_eq!(desc.name, *name);
            assert_eq!(desc.id, *id);
            assert_eq!(desc.media_type, MediaType::Video);
            let _ = desc.build(Limits::permissive());
        }

        let encoders: &[&EncoderDesc] = &[
            &RAWVIDEO_ENCODER,
            &BITPACKED_ENCODER,
            &WRAPPED_AVFRAME_ENCODER,
            &V210_ENCODER,
            &V210X_ENCODER,
            &R210_ENCODER,
            &R10K_ENCODER,
            &Y41P_ENCODER,
            &AVUI_ENCODER,
        ];
        assert_eq!(encoders.len(), 9);
        for desc in encoders {
            let _ = desc.build(Limits::permissive());
        }
    }

    #[test]
    fn r210_decoder_reuses_the_generic_raw_path() {
        // r210's wire format is byte-identical to x2rgb10be, so decoding
        // through the registered descriptor must match `decode_raw` directly.
        let mut dec = RawVideoDecoder::new(Limits::permissive(), CodecId::R210, Packing::R210)
            .with_video_params(2, 1, PixFmt::X2rgb10be);
        let mut budget = Budget::new(Limits::permissive());
        let payload = vec![0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0];
        let pkt = Packet::from_slice(&mut budget, &payload).expect("packet");
        dec.send(Some(&pkt)).expect("send");
        let via_desc = dec.receive().expect("frame");

        let mut budget2 = Budget::new(Limits::permissive());
        let via_direct =
            decode_raw(&payload, 2, 1, PixFmt::X2rgb10be, &mut budget2).expect("decode");

        let FrameData::Video { planes: p1, .. } = &via_desc.data else {
            panic!()
        };
        let FrameData::Video { planes: p2, .. } = &via_direct.data else {
            panic!()
        };
        assert_eq!(p1[0].data.as_slice()[..8], p2[0].data.as_slice()[..8]);
    }
}
