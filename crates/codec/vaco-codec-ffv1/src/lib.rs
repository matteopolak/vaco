//! FFV1 lossless video decode and encode ([RFC 9043]).
//!
//! [RFC 9043]: https://www.rfc-editor.org/rfc/rfc9043
//!
//! # What it is
//!
//! FFV1 is an intra-only lossless video codec: a range coder or Golomb-Rice
//! bit coder (`coder_type`), a median predictor plus a quantized-gradient
//! context model shared by both, and (from version 3 on) an out-of-band
//! Configuration Record that states color space, bit depth, and quantization
//! tables once for the whole stream rather than per frame.
//!
//! # How it works
//!
//! - [`rangecoder`]: the byte-oriented binary range coder (RFC 9043 §3.8.1)
//!   and its `get_symbol`/`put_symbol` nonbinary layer. Correctness here is
//!   load-bearing for everything else — see that module's own round-trip
//!   tests, which the crate's implementation history relied on catching two
//!   real encoder bugs before anything was built on top.
//! - [`rice`]: Golomb-Rice decode (§3.8.2) — **decode-only**, because this
//!   crate's own encoder never emits it (see that module's docs for why:
//!   `ffmpeg -c:v ffv1`'s own default *is* Golomb-Rice, so decode needs it to
//!   read real files, but this crate's encoder is simpler sticking to one
//!   coder throughout).
//! - [`quant`]: Quantization Table Sets, the median predictor, and the
//!   per-sample context computation (§3.4-§3.6).
//! - [`params`]: `Parameters` (§4.2) — the stream-wide configuration.
//! - [`crc`]: the Configuration Record's CRC (§4.3.2/§4.9.3).
//! - [`slice`]: `SliceHeader`/`SliceContent`/`SliceFooter` (§4.5-§4.9) and the
//!   per-plane decode/encode loop built on the border rules of §3.1-§3.2.
//! - [`codec`]: framing everything above into whole frames — the
//!   Configuration Record, the per-frame `keyframe` bit, and the glue between
//!   [`vaco_frame::Frame`]'s planes and FFV1's Y/Cb/Cr-or-JPEG-2000-RCT
//!   sample domain (§3.7). [`codec::Ffv1Config`]/[`codec::decode_frame`]/
//!   [`codec::encode_frame`] are the pure functions; [`Ffv1Decoder`]/
//!   [`Ffv1Encoder`] wrap them in the `SendReceive` protocol every codec in
//!   this tree shares.
//!
//! # Coverage — what this pass reaches and what it does not
//!
//! - **Version**: 3 only (matches `ffmpeg -c:v ffv1`'s own default, verified
//!   by encoding a real test clip and inspecting its Matroska `CodecPrivate`
//!   — see `provenance/vaco-codec-ffv1.toml`'s `blackbox` entries). Versions
//!   0/1/2 are not implemented.
//! - **Slicing**: this crate's own encoder writes one slice; decode covers
//!   multiple, via `codec::locate_slices`'s backward walk over
//!   `SliceFooter.slice_size` (RFC 9043 §4.9.1's own stated purpose for that
//!   field) — measured necessary directly, since even a 64x64 `ffmpeg`
//!   encode defaults to a 2x2 slice grid. Cross-checked pixel-exact against
//!   a real 4-slice file. `Caps::SLICE_THREADS`/frame-internal parallelism
//!   are not attempted (a possible follow-up).
//! - **Coder**: this crate's encoder always emits `coder_type = 1` (range
//!   coder, default state transition table), cross-checked pixel-exact
//!   against a real `ffmpeg -coder range_def` encode. `coder_type = 0`
//!   (Golomb-Rice — `ffmpeg -c:v ffv1`'s own *default*) parses without error
//!   but has a known, unresolved decode bug (see `codec`'s module docs for
//!   what was ruled out); `2` (custom transition table) is untested.
//! - **Bit depth**: 8-bit only. FFV1 supports up to 16-bit; that is out of
//!   scope here.
//! - **Pixel formats**: `Yuv420p`, `Yuv422p`, `Yuv444p` (`colorspace_type` 0,
//!   YCbCr) and `Gbrp` (`colorspace_type` 1, via the JPEG 2000 Reversible
//!   Color Transform). No alpha/extra plane.
//!
//! # How to change it
//!
//! Add a pixel format by extending `codec::mapping_for`/`format_for` — the
//! slice-content loop itself does not know about pixel formats at all, only
//! plane counts and subsampling factors, so a new 8-bit YCbCr subsampling is
//! usually a one-line addition. A higher bit depth needs `slice.rs`'s
//! `wrap_diff`/`wrap_sample` (already parameterized by `bits`) plus wiring a
//! 16-bit-capable `vaco_frame` plane read/write in `codec.rs`'s
//! `read_pixels`/`write_pixels`, which currently hard-code `u8`.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds every allocation this crate makes from
//! attacker-controlled bitstream fields (frame dimensions, quantization
//! table sizes, slice counts) the same way every other decoder in this tree
//! is bounded.
//!
//! `Ffv1Decoder::set_extradata` takes the plain "container's Configuration
//! Record bytes, nothing else" contract every other codec in this tree uses —
//! [`codec::Ffv1Config::from_extradata`] never needed width/height, only the
//! quantization tables and colour signalling the Configuration Record itself
//! carries. Width and height are a separate problem: RFC 9043 §4 says
//! `frame_pixel_width`/`frame_pixel_height` "MUST be provided by external
//! means" — FFV1's own bitstream (Configuration Record included) never states
//! them at all. [`Ffv1Decoder`] gets them from
//! [`vaco_codec_core::Decoder::prime_video`], the generic channel the CLI's
//! decode wiring calls with the container's reported dimensions before the
//! first packet, the same way `Encoder::prime_audio` tells an audio encoder
//! its stream shape ahead of time.
//!
//! This used to be a private extradata envelope instead —
//! `[width: u32 BE][height: u32 BE][the RFC Configuration Record]` — built by
//! nothing but this crate's own tests, because `set_extradata` was the only
//! channel a generically-registered `Box<dyn Decoder>` had at all before
//! `prime_video` existed. That meant the generic CLI decode path, which hands
//! a decoder the container's *plain* extradata, could never configure this
//! one: `-c:v copy` was the only path that had ever exercised the crate,
//! because it never calls `set_extradata` at all (`planning/E2E-GAPS.md` #2's
//! video-side gap). [`Ffv1Encoder`] already attached the plain RFC
//! Configuration Record (not that envelope) as
//! `PacketSideDataKind::NewExtradata` on its first packet — that is what a
//! real container's own track metadata (Matroska's `PixelWidth`/
//! `PixelHeight`, an MP4 visual sample entry) carries width/height alongside,
//! so this crate now matches that shape on the decode side too.
//!
//! # Dependencies
//!
//! `vaco-bitstream` (the raw bit reader Golomb-Rice decode uses),
//! `vaco-codec-core` (the `SendReceive` protocol), `vaco-frame`/`vaco-pixfmt`/
//! `vaco-pool` (the decoded picture), `vaco-packet` (encoded bytes and
//! extradata side data), `vaco-limits` (allocation bounds). No external
//! crate — every byte format here is implemented directly from RFC 9043.

#![forbid(unsafe_code)]

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

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static FFV1_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "ffv1",
    long_name: "FFmpeg video codec #1",
    id: vaco_codec_core::CodecId::Ffv1,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
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
        enc.set_option("coder", "range_def").expect("already the default");
        enc.set_option("coder", "-2").expect("numeric spelling of the same value");
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
        enc.set_option("slices", "0").expect("auto-detect, same result");
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
