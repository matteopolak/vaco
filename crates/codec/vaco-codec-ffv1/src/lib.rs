//! FFV1 lossless video decode and encode ([RFC 9043]).
//!
//! [RFC 9043]: https://www.rfc-editor.org/rfc/rfc9043
//!
//! FFV1 is an intra-only lossless video codec: a range coder or Golomb-Rice
//! bit coder (`coder_type`), a median predictor plus a quantized-gradient
//! context model, and (from version 3 on) an out-of-band Configuration
//! Record stating color space, bit depth, and quantization tables once for
//! the whole stream.
//!
//! [`rangecoder`] is the binary range coder (§3.8.1); [`rice`] is
//! Golomb-Rice decode (§3.8.2, decode-only — the encoder always uses the
//! range coder); [`quant`] holds the Quantization Table Sets, median
//! predictor and context computation (§3.4-§3.6); [`params`]/[`crc`] cover
//! the stream-wide `Parameters` (§4.2) and Configuration Record CRC
//! (§4.3.2/§4.9.3); [`slice`] is `SliceHeader`/`SliceContent`/`SliceFooter`
//! (§4.5-§4.9) and the per-plane loop; [`codec`] frames it all into whole
//! frames, wrapped as [`Ffv1Decoder`]/[`Ffv1Encoder`] in this tree's
//! `SendReceive` protocol.
//!
//! # Coverage
//!
//! Version 3 only; 8-bit; `Yuv420p`/`Yuv422p`/`Yuv444p`, `Gray8`, and `Gbrp`
//! (via the JPEG 2000 RCT). The encoder always uses the range coder with the
//! default state transition table and writes one slice; decode also covers
//! Golomb-Rice-coded, custom-table and multi-slice files. See
//! [`codec`]'s module docs for the measurements behind those claims. Add a
//! pixel format by
//! extending `codec::mapping_for`/`format_for`; a higher bit depth needs
//! `slice.rs`'s `wrap_diff`/`wrap_sample` plus 16-bit-capable reads/writes
//! in `codec.rs`.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds every allocation from attacker-controlled
//! bitstream fields. RFC 9043 §4 says width/height "MUST be provided by
//! external means", so [`Ffv1Decoder`] takes them via
//! [`vaco_codec_core::Decoder::prime_video`] rather than `set_extradata`;
//! [`Ffv1Encoder`] mirrors this by attaching the Configuration Record as
//! `PacketSideDataKind::NewExtradata` on its first packet.

#![forbid(unsafe_code)]
// D22: nightly is pinned (`rust-toolchain.toml`) specifically so D21's
// branch-hint idiom has `std::hint::{likely, unlikely, cold_path}` available,
// not only `#[cold]`/`#[inline(always)]`. Used in `slice.rs`'s per-sample
// border lookup, the hottest branchy function this crate has (RFC 9043 §3.1):
// for all but the first two rows/columns of a plane, every one of its
// bounds checks is false.
#![feature(likely_unlikely)]

mod codec;
mod crc;
mod params;
mod quant;
mod rangecoder;
mod rice;
mod slice;

use codec::{Ffv1Config, build_extradata};
use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData};
use vaco_pixfmt::PixFmt;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`], one FFV1 frame per
/// packet (this crate is intra-only and declares no [`Caps::DELAY`] — see
/// the crate docs on slicing/threading scope).
#[derive(Debug)]
pub struct Ffv1Decoder {
    machine: Machine<Frame>,
    limits: Limits,
    config: Option<Ffv1Config>,
    width: u32,
    height: u32,
    /// Per-slice-position adaptive context state, persisted across frames
    /// whose own `keyframe` bit reads `false` — see
    /// [`codec::PersistedContexts`]'s docs for why this exists.
    contexts: codec::PersistedContexts,
}

impl Ffv1Decoder {
    /// A decoder that bounds every allocation by `limits`. Call
    /// [`SendReceive::set_extradata`] (the plain RFC 9043 Configuration
    /// Record) and [`SendReceive::prime_video`] (the container's reported
    /// frame dimensions) before sending packets — see the crate docs on why
    /// both are needed and neither alone is enough.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            config: None,
            width: 0,
            height: 0,
            contexts: codec::PersistedContexts::default(),
        }
    }
}

impl Default for Ffv1Decoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for Ffv1Decoder {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.config = Some(Ffv1Config::from_extradata(extradata)?);
        // A (re-)configured decoder starts adaptation over, the same as a
        // seek — see PersistedContexts::reset's docs.
        self.contexts.reset();
        Ok(())
    }

    fn prime_video(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
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
                let config = self.config.as_ref().ok_or(Error::InvalidData(
                    "ffv1: decoder has no configuration; call set_extradata first",
                ))?;
                if self.width == 0 || self.height == 0 {
                    return Err(Error::InvalidData(
                        "ffv1: decoder does not know the frame size; call prime_video first",
                    ));
                }
                let mut budget = Budget::new(self.limits.clone());
                let mut frame = codec::decode_frame(
                    config,
                    &mut self.contexts,
                    pkt.payload(),
                    self.width,
                    self.height,
                    &mut budget,
                )?;
                frame.pts = pkt.pts;
                frame.duration = pkt.duration;
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
        // A seek discards buffered state; a decoder that resumed mid-
        // adaptation against a now-discontinuous stream would decode
        // garbage exactly like the bug PersistedContexts::reset's docs
        // describe, just triggered by a seek instead of a missing
        // `keyframe` check.
        self.contexts.reset();
    }
}

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`], one FFV1 frame per
/// packet. The target pixel format is discovered from the first frame sent
/// (see [`Encoder::accepted_pix_fmts`](vaco_codec_core::Encoder::accepted_pix_fmts)),
/// since [`vaco_codec_core::EncoderDesc::make`] takes only [`Limits`].
#[derive(Debug)]
pub struct Ffv1Encoder {
    machine: Machine<Packet>,
    limits: Limits,
    config: Option<Ffv1Config>,
    sent_extradata: bool,
}

impl Ffv1Encoder {
    /// An encoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            config: None,
            sent_extradata: false,
        }
    }
}

impl Default for Ffv1Encoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for Ffv1Encoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [PixFmt] {
        codec::SUPPORTED_PIX_FMTS
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
                let format = frame
                    .pixel_format()
                    .ok_or(Error::InvalidData("ffv1: expected a video frame"))?;
                let config = match &self.config {
                    Some(c) if c.format == format => c,
                    _ => {
                        self.config = Some(Ffv1Config::for_encode(format)?);
                        self.sent_extradata = false;
                        self.config
                            .as_ref()
                            .unwrap_or_else(|| unreachable!("just assigned"))
                    }
                };
                let body = codec::encode_frame(config, frame)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &body)?;
                packet.pts = frame.pts;
                // Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`'s
                // encoders: never set `Packet::duration`. Propagated from
                // the input `Frame` for consistency with every other video
                // encoder in this tree -- a container deriving a track's
                // total length from summed packet durations (Matroska/AVI/
                // NUT, the containers FFV1 actually reaches) was silently
                // undercounting it.
                packet.duration = frame.duration;
                packet.flags |= vaco_packet::PacketFlags::KEY;
                if !self.sent_extradata {
                    // Plain RFC 9043 Configuration Record — deliberately
                    // *not* this crate's decode-side envelope, so a future
                    // muxer sees exactly the bytes RFC 9043 §4.3.3 says go in
                    // a container's Configuration Record slot. See the crate
                    // docs.
                    let record = build_extradata(&config.params)?;
                    let mut side_budget = Budget::new(self.limits.clone());
                    let buf = vaco_pool::Buffer::alloc(&mut side_budget, record.len())?;
                    let mut buf = buf;
                    buf.make_mut().copy_from_slice(&record);
                    packet.set_side_data(PacketSideData::NewExtradata(buf));
                    self.sent_extradata = true;
                }
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

    /// `-coder` and `-slices`, the two `AVOption`s `ffmpeg -h encoder=ffv1`
    /// exposes that this crate's fixed encoding shape can meaningfully
    /// answer. Both are pure validation, not configuration: this encoder
    /// always emits `coder_type = 1` (range coder, default state transition
    /// table -- see the module docs' "Coder" coverage note) in exactly one
    /// slice per frame, so there is nothing to switch. A value equal to what
    /// the encoder already does is accepted; a value it cannot honour
    /// (Golomb-Rice, a custom transition table, or more than one slice) is a
    /// real [`Error::Option`] rather than a silent wrong-config -- the case
    /// [`vaco_codec_core::Encoder::set_option`]'s own doc calls out. Every
    /// other key is silently ignored, matching that same default.
    ///
    /// # Errors
    /// [`Error::Option`] for a `coder`/`slices` value this encoder does not
    /// implement, or one that does not parse.
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "coder" => match value.trim() {
                "-2" | "range_def" => Ok(()),
                "0" | "rice" => Err(Error::Option {
                    name: "coder".to_owned(),
                    detail: "this encoder always emits the range coder (RFC 9043 \
                             coder_type=1); Golomb-Rice output is not implemented"
                        .to_owned(),
                }),
                "1" | "ac" | "2" | "range_tab" => Err(Error::Option {
                    name: "coder".to_owned(),
                    detail: "this encoder always uses the range coder's default state \
                             transition table; a custom transition table is not implemented"
                        .to_owned(),
                }),
                other => Err(Error::Option {
                    name: "coder".to_owned(),
                    detail: format!("unknown coder type: {other:?}"),
                }),
            },
            "slices" => {
                let n: i64 = value.trim().parse().map_err(|_| Error::Option {
                    name: "slices".to_owned(),
                    detail: format!("not an integer: {value:?}"),
                })?;
                if n == 0 || n == 1 {
                    Ok(())
                } else {
                    Err(Error::Option {
                        name: "slices".to_owned(),
                        detail: format!(
                            "this encoder always writes exactly one slice per frame; {n} is not supported"
                        ),
                    })
                }
            }
            _ => Ok(()),
        }
    }
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        Ffv1Decoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        Ffv1Encoder::new(limits),
    )))
}

pub static FFV1_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "ffv1",
    long_name: "FFmpeg video codec #1",
    id: vaco_codec_core::CodecId::Ffv1,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_decoder,
};

pub static FFV1_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "ffv1",
    long_name: "FFmpeg video codec #1",
    id: vaco_codec_core::CodecId::Ffv1,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_encoder,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the codec, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;
    use vaco_core::Timestamp;
    use vaco_frame::FrameFlags;
    use vaco_packet::PacketSideDataKind;

    fn make_test_frame(format: PixFmt, w: u32, h: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, format, w, h).expect("alloc");
        let plane_count = frame.plane_count();
        for pi in 0..plane_count {
            let mut plane = frame.plane_mut(pi).expect("plane");
            let rows = plane.rows();
            let row_bytes = plane.row_bytes();
            for y in 0..rows {
                if let Some(row) = plane.row_mut(y) {
                    for x in 0..row_bytes {
                        if let Some(slot) = row.get_mut(x) {
                            *slot = ((x * 37 + y * 91 + pi * 53) % 256) as u8;
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(0);
        frame.flags = FrameFlags::KEY;
        frame
    }

    fn frame_bytes(frame: &Frame) -> Vec<u8> {
        let mut out = Vec::new();
        for pi in 0..frame.plane_count() {
            let plane = frame.plane(pi).expect("plane");
            for row in plane.rows_iter() {
                out.extend_from_slice(row);
            }
        }
        out
    }

    #[test]
    fn send_receive_protocol_shape() {
        let frame = make_test_frame(PixFmt::Yuv420p, 8, 8);
        let mut enc = Ffv1Encoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("receive packet");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("begin drain");
        assert!(matches!(enc.receive(), Err(Error::Eof)));

        let record = match packet.side_data(PacketSideDataKind::NewExtradata) {
            Some(PacketSideData::NewExtradata(buf)) => buf.as_slice().to_vec(),
            _ => panic!("expected NewExtradata"),
        };
        let mut dec = Ffv1Decoder::new(Limits::permissive());
        dec.set_extradata(&record).expect("set_extradata");
        dec.prime_video(8, 8);
        dec.send(Some(&packet)).expect("send packet");
        let decoded = dec.receive().expect("receive frame");
        assert_eq!(frame_bytes(&frame), frame_bytes(&decoded));
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("begin drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    /// The decoder must carry `Packet::duration` onto the decoded
    /// `Frame::duration`, the same way `vaco-codec-jpeg`'s decoder already
    /// does right next to its own `pts` assignment.
    ///
    /// This is the video-decode side of the class of bug fixed across many
    /// codecs elsewhere in this workspace: MP4's `stts` derives every
    /// sample's duration except the *last* from the delta to the next
    /// sample's DTS, so the last sample's duration comes only from
    /// `Packet::duration` -- which in turn is only reachable if some
    /// decoder along the way actually preserved it onto `Frame::duration`.
    /// A decoder that drops it is invisible in Matroska/WebM (duration is
    /// inferred from the next block's timecode there) and silently loses
    /// the last frame's duration in MP4. FFV1's own bitstream carries no
    /// frame-rate field to derive a duration from some other way (unlike
    /// Theora's `frn`/`frd`), so `Packet::duration` is the only source of
    /// truth available to the decoder.
    #[test]
    fn a_decoded_frame_carries_the_packets_duration() {
        let frame = make_test_frame(PixFmt::Yuv420p, 8, 8);
        let mut enc = Ffv1Encoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let mut packet = enc.receive().expect("receive packet");
        packet.duration = vaco_core::Duration::from_micros(1234);

        let record = match packet.side_data(PacketSideDataKind::NewExtradata) {
            Some(PacketSideData::NewExtradata(buf)) => buf.as_slice().to_vec(),
            _ => panic!("expected NewExtradata"),
        };
        let mut dec = Ffv1Decoder::new(Limits::permissive());
        dec.set_extradata(&record).expect("set_extradata");
        dec.prime_video(8, 8);
        dec.send(Some(&packet)).expect("send packet");
        let decoded = dec.receive().expect("receive frame");
        assert_eq!(decoded.duration, vaco_core::Duration::from_micros(1234));
    }

    #[test]
    fn decode_without_extradata_is_an_error() {
        let mut dec = Ffv1Decoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[0, 0, 0]).expect("packet");
        assert!(dec.send(Some(&pkt)).is_err());
    }

    /// `-coder range_def` (and its numeric spelling `-2`) names exactly what
    /// this encoder already does, so it must be accepted.
    #[test]
    fn set_option_coder_accepts_the_value_this_encoder_already_produces() {
        let mut enc = Ffv1Encoder::new(Limits::permissive());
        enc.set_option("coder", "range_def")
            .expect("already the default");
        enc.set_option("coder", "-2")
            .expect("numeric spelling of the same value");
    }

    /// `-coder rice`/`-coder ac`/`-coder range_tab` all name a coder this
    /// encoder cannot produce (Golomb-Rice, or a custom transition table);
    /// silently ignoring them would be a wrong-config trap, so each must be
    /// a real error instead.
    #[test]
    fn set_option_coder_rejects_values_this_encoder_cannot_produce() {
        let mut enc = Ffv1Encoder::new(Limits::permissive());
        for value in ["rice", "0", "ac", "1", "range_tab", "2"] {
            assert!(
                matches!(enc.set_option("coder", value), Err(Error::Option { .. })),
                "expected coder={value:?} to be rejected"
            );
        }
    }

    /// `-slices 1` (and the auto-detect spelling `0`) is what this encoder
    /// already writes; any other count cannot be honoured since this
    /// encoder never splits a frame into more than one slice.
    #[test]
    fn set_option_slices_accepts_one_and_rejects_others() {
        let mut enc = Ffv1Encoder::new(Limits::permissive());
        enc.set_option("slices", "1").expect("already the default");
        enc.set_option("slices", "0")
            .expect("auto-detect, same result");
        assert!(matches!(
            enc.set_option("slices", "4"),
            Err(Error::Option { .. })
        ));
        assert!(matches!(
            enc.set_option("slices", "not-a-number"),
            Err(Error::Option { .. })
        ));
    }

    /// A key this encoder has no use for is a silent no-op, matching
    /// `Encoder::set_option`'s own documented default.
    #[test]
    fn set_option_ignores_a_key_this_encoder_has_no_use_for() {
        let mut enc = Ffv1Encoder::new(Limits::permissive());
        enc.set_option("context", "1").expect("silently ignored");
    }
}
