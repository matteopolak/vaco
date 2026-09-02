//! The `vaco_codec_core::Decoder` face over [`crate::decode`], and the
//! [`DecoderDesc`]s that make the six codecs it dispatches on reachable
//! from `vaco-registry`.
//!
//! # One packet, at most one frame
//!
//! Every codec here decodes one packet to one dialogue event, except
//! `mov_text`'s gap samples, which decode to none (`crate::decode` already
//! returns `Option<String>` for exactly this reason). Neither
//! [`Caps::SUBFRAMES`] nor [`Caps::DELAY`] applies — producing nothing for
//! an input is ordinary, and there is no cross-packet state to flush at
//! end of stream — matching the same reasoning
//! `vaco-codec-subtitle-bitmap::decoder::PgsSubtitleDecoder`'s own docs
//! give for its identical shape.
//!
//! # `SubtitleContent::Ass`, for every codec including `text`
//!
//! `crate`'s own module docs state the measured fact this rests on:
//! `ffmpeg -bitexact -f ass -` shows `subrip`, `webvtt`, `mov_text` and
//! `text` all emitting an ASS `Dialogue:` line, not plain text — this
//! workspace's decoders reproduce that, so `SubtitleRect::ass` is the
//! correct constructor for all six registrations below, not
//! `SubtitleRect::text`.
//!
//! # `ssa`, decoding `CodecId::Ssa`
//!
//! `vaco-demux-matroska` maps a `S_TEXT/SSA` track to `CodecId::Ssa`
//! (distinct from `CodecId::Ass`, which this workspace's own `.ass`/`.ssa`
//! file demuxer always produces instead — see `vaco_subtitle_text::ass`'s
//! module docs). Measured against the reference (`ffmpeg -decoders`):
//! `ssa` is a real, separate decoder name, documented as "(codec ass)" —
//! the same ASS-chunk decode, registered a second time under the other
//! name. Without this, a Matroska `S_TEXT/SSA` track has no decoder in
//! this workspace at all, since nothing else produces or consumes
//! `CodecId::Ssa`.
//!
//! # Timing
//!
//! `Frame::pts`/`duration` are copied from the packet unchanged.
//! `vaco_format_subtitle`'s demuxers already resolve a cue's display window
//! to `Packet::duration` (`Cue::duration()`, see `vaco-subtitle-text::
//! engine`'s main demux path) before a packet ever reaches this crate, so
//! there is nothing left for the decoder to compute.

use vaco_codec_core::{Accept, Caps, CodecId, DecoderDesc, Machine, SendReceive};
use vaco_core::{MediaType, Result};
use vaco_frame::{Frame, FrameData, SubtitleRect};
use vaco_limits::Limits;
use vaco_packet::Packet;

use crate::TextCodec;

fn frame_of(codec: TextCodec, packet: &Packet) -> Option<Frame> {
    let ass = crate::decode(codec, packet.payload())?;
    let rect = SubtitleRect::ass(0, 0, 0, 0, false, ass);
    let mut frame = Frame::from_data(FrameData::Subtitle {
        rects: std::iter::once(rect).collect(),
    });
    frame.pts = packet.pts;
    frame.duration = packet.duration;
    Some(frame)
}

/// Text subtitle decode as a `SendReceive`, parameterised by which of the
/// six codecs [`crate::decode`] dispatches on. See the module docs for
/// `Caps` and the `SubtitleContent::Ass` choice.
#[derive(Debug)]
pub struct TextSubtitleDecoder {
    machine: Machine<Frame>,
    codec: TextCodec,
}

impl TextSubtitleDecoder {
    #[must_use]
    pub fn new(codec: TextCodec) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            codec,
        }
    }
}

impl SendReceive for TextSubtitleDecoder {
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
                let Some(pkt) = input else {
                    return Ok(());
                };
                if let Some(frame) = frame_of(self.codec, pkt) {
                    self.machine.emit(frame);
                }
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

macro_rules! make_fn {
    ($name:ident, $codec:expr) => {
        fn $name(_limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
            Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
                TextSubtitleDecoder::new($codec),
            )))
        }
    };
}

make_fn!(make_subrip, TextCodec::SubRip);
make_fn!(make_ass, TextCodec::Ass);
make_fn!(make_ssa, TextCodec::Ass); // CodecId::Ssa: same ASS-chunk decode, see module docs.
make_fn!(make_webvtt, TextCodec::WebVtt);
make_fn!(make_movtext, TextCodec::MovText);
make_fn!(make_text, TextCodec::Text);
make_fn!(make_ttml, TextCodec::Ttml);

/// Registered as this crate's `subrip` decoder fragment (`vaco-component.toml`).
pub static SUBRIP_DECODER: DecoderDesc = DecoderDesc {
    name: "subrip",
    long_name: "SubRip subtitle",
    id: CodecId::SubRip,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_subrip,
};

/// Registered as this crate's `ass` decoder fragment.
pub static ASS_DECODER: DecoderDesc = DecoderDesc {
    name: "ass",
    long_name: "ASS (Advanced SubStation Alpha) subtitle",
    id: CodecId::Ass,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ass,
};

/// Registered as this crate's `ssa` decoder fragment. See the module docs
/// for why this decodes `CodecId::Ssa` with the same logic as [`ASS_DECODER`].
pub static SSA_DECODER: DecoderDesc = DecoderDesc {
    name: "ssa",
    long_name: "ASS (Advanced SubStation Alpha) subtitle",
    id: CodecId::Ssa,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ssa,
};

/// Registered as this crate's `webvtt` decoder fragment.
pub static WEBVTT_DECODER: DecoderDesc = DecoderDesc {
    name: "webvtt",
    long_name: "WebVTT subtitle",
    id: CodecId::Webvtt,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_webvtt,
};

/// Registered as this crate's `mov_text` decoder fragment.
pub static MOVTEXT_DECODER: DecoderDesc = DecoderDesc {
    name: "mov_text",
    long_name: "3GPP Timed Text subtitle",
    id: CodecId::MovText,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_movtext,
};

/// Registered as this crate's `text` decoder fragment.
pub static TEXT_DECODER: DecoderDesc = DecoderDesc {
    name: "text",
    long_name: "Raw text subtitle",
    id: CodecId::Text,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_text,
};

/// Registered as this crate's `ttml` decoder fragment.
pub static TTML_DECODER: DecoderDesc = DecoderDesc {
    name: "ttml",
    long_name: "TTML subtitle",
    id: CodecId::Ttml,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_ttml,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Budget;

    fn packet_from(bytes: &[u8]) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        Packet::from_slice(&mut budget, bytes).unwrap()
    }

    #[test]
    fn subrip_decoder_emits_an_ass_frame_carrying_packet_timing() {
        let mut decoder = TextSubtitleDecoder::new(TextCodec::SubRip);
        let mut pkt = packet_from(b"a <i>b</i>");
        pkt.pts = vaco_core::Timestamp::new(7);
        pkt.duration = vaco_core::Duration::from_micros(2_000_000);
        decoder.send(Some(&pkt)).unwrap();
        let frame = decoder.receive().unwrap();
        assert_eq!(frame.pts, vaco_core::Timestamp::new(7));
        assert_eq!(frame.duration, vaco_core::Duration::from_micros(2_000_000));
        let FrameData::Subtitle { rects } = &frame.data else {
            unreachable!("text subtitle decode must produce FrameData::Subtitle");
        };
        let vaco_frame::SubtitleContent::Ass(text) = &rects[0].content else {
            unreachable!("text subtitle decode must produce SubtitleContent::Ass");
        };
        assert_eq!(text, "a {\\i1}b{\\i0}");
    }

    #[test]
    fn movtext_gap_sample_produces_no_frame() {
        let mut decoder = TextSubtitleDecoder::new(TextCodec::MovText);
        let pkt = packet_from(&[0, 0]);
        decoder.send(Some(&pkt)).unwrap();
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::NeedMoreInput)));
    }

    #[test]
    fn drain_with_nothing_pending_reaches_eof_immediately() {
        let mut decoder = TextSubtitleDecoder::new(TextCodec::Text);
        decoder.send(None).unwrap();
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::Eof)));
    }

    #[test]
    fn every_registered_decoder_matches_its_codec_id() {
        for (desc, id) in [
            (&SUBRIP_DECODER, CodecId::SubRip),
            (&ASS_DECODER, CodecId::Ass),
            (&SSA_DECODER, CodecId::Ssa),
            (&WEBVTT_DECODER, CodecId::Webvtt),
            (&MOVTEXT_DECODER, CodecId::MovText),
            (&TEXT_DECODER, CodecId::Text),
            (&TTML_DECODER, CodecId::Ttml),
        ] {
            assert_eq!(desc.id, id);
            assert_eq!(desc.media_type, MediaType::Subtitle);
            let mut decoder = (desc.make)(Limits::permissive());
            decoder.flush(); // must not panic on a freshly built decoder
        }
    }

    #[test]
    fn ssa_and_ass_decode_identically() {
        let mut ass = TextSubtitleDecoder::new(TextCodec::Ass);
        let pkt = packet_from(b"0,0,Default,,0,0,0,,hi there");
        ass.send(Some(&pkt)).unwrap();
        let frame = ass.receive().unwrap();
        let FrameData::Subtitle { rects } = &frame.data else {
            unreachable!()
        };
        let vaco_frame::SubtitleContent::Ass(text) = &rects[0].content else {
            unreachable!()
        };
        assert_eq!(text, "hi there");
    }
}
