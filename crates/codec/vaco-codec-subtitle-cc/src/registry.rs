//! The `vaco_codec_core::Decoder` face over [`crate::CcDecoder`], and the
//! [`DecoderDesc`] that makes it reachable from `vaco-registry` as
//! `CodecId::Eia608`'s decoder.
//!
//! # Named `cc_dec`, measured
//!
//! `ffmpeg -decoders` (`ffmpeg 9.0.1`): `cc_dec  Closed Captions (EIA-608 /
//! CEA-708) (codec eia_608)` — one decoder name for both formats, matching
//! this workspace's own single `CodecId::Eia608` for both (see the crate's
//! top-level docs).
//!
//! # One packet, any number of frames
//!
//! `CcDecoder::feed` takes one access unit's `cc_data` bytes and returns
//! every caption event that access unit produced — zero, one, or several
//! (CEA-608 field 1 and field 2 can both change in the same access unit,
//! and CEA-708 can update more than one window). [`Caps::SUBFRAMES`]
//! reflects that. Not [`Caps::DELAY`]: nothing here buffers a screen that
//! is still open at end of stream — `feed` already emits a screen the
//! moment it *changes*, so whatever is on screen when the packets stop
//! was already emitted by the access unit that put it there.
//!
//! # `SubtitleContent::Text`, not `Bitmap` or `Ass`
//!
//! [`event::Screen::text`] already renders a screen's cells to plain text
//! with no coordinate information, matching `vaco_frame::subtitle`'s own
//! module docs, which name CEA-608/708 as formats that "decode ... to
//! positioned text" — `Text` is the variant for exactly that, once "their
//! own decoders produce characters" (this module). A CEA-708 window's
//! actual on-screen `geometry` is discarded in this first pass: nothing
//! here yet converts it to `SubtitleRect`'s pixel-space `x`/`y`/`w`/`h`.
//!
//! # No upstream source yet — stated, not hidden
//!
//! This decoder is reachable from `vaco-registry`, but nothing in this
//! workspace's H.264/HEVC/MPEG-2 parsers yet extracts `cc_data` from a
//! compressed stream's `user_data_registered_itu_t_t35` SEI (or MPEG-2's
//! picture user data) and attaches it as a packet this decoder could
//! receive — see the crate's top-level module docs, gap 1. Registering the
//! `Decoder` itself does not depend on that gap closing; it is exercised
//! today the same way [`crate::CcDecoder`] itself is, by constructing
//! `cc_data` bytes directly (see this module's own tests).
//!
//! # Timing
//!
//! `Frame::pts`/`duration` are copied from the packet unchanged — `cc_data`
//! carries no independent timing of its own; it rides one video access
//! unit's own timestamp.

use vaco_codec_core::{Accept, Caps, CodecId, DecoderDesc, Machine, SendReceive};
use vaco_core::{MediaType, Result};
use vaco_frame::{Frame, FrameData, SubtitleRect};
use vaco_limits::Limits;
use vaco_packet::Packet;

use crate::{CcDecoder, Event};

fn frame_of(event: &Event, packet: &Packet) -> Frame {
    let text = match event {
        Event::Cea608 { screen, .. } => screen.text(),
        Event::Cea708 { screen, .. } => screen.as_ref().map(event::Screen::text).unwrap_or_default(),
    };
    let rect = SubtitleRect::text(0, 0, 0, 0, false, text);
    let mut frame = Frame::from_data(FrameData::Subtitle {
        rects: std::iter::once(rect).collect(),
    });
    frame.pts = packet.pts;
    frame.duration = packet.duration;
    frame
}

use crate::event;

/// Closed-caption decode as a `SendReceive`. See the module docs for
/// `Caps` and the `SubtitleContent::Text` choice.
#[derive(Debug)]
pub struct CcSubtitleDecoder {
    machine: Machine<Frame>,
    inner: CcDecoder,
}

impl CcSubtitleDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES),
            inner: CcDecoder::new(),
        }
    }
}

impl Default for CcSubtitleDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SendReceive for CcSubtitleDecoder {
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
                for event in self.inner.feed(pkt.payload()) {
                    self.machine.emit(frame_of(&event, pkt));
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
        self.inner = CcDecoder::new();
    }
}

fn make(_limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        CcSubtitleDecoder::new(),
    )))
}

/// Registered as this crate's `cc_dec` decoder fragment
/// (`vaco-component.toml`). Named for the reference's own decoder name —
/// see the module docs for the measurement.
pub static CC_DECODER: DecoderDesc = DecoderDesc {
    name: "cc_dec",
    long_name: "Closed Captions (EIA-608 / CEA-708)",
    id: CodecId::Eia608,
    media_type: MediaType::Subtitle,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Budget;

    fn cc_byte_pair(ch: u8, parity_fn: fn(u8) -> u8) -> [u8; 2] {
        [parity_fn(ch), parity_fn(0)]
    }

    fn odd_parity(byte: u8) -> u8 {
        let d = byte & 0x7F;
        if d.count_ones() % 2 == 1 {
            d
        } else {
            d | 0x80
        }
    }

    /// One field-1 line-21 triplet carrying byte pair `(b1, b2)`.
    fn triplet608(b1: u8, b2: u8) -> [u8; 3] {
        // cc_valid=1, cc_type=00 (NTSC field 1): 0b1111_1100 | cc_type.
        [0b1111_1100, b1, b2]
    }

    fn packet_from(bytes: &[u8]) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        Packet::from_slice(&mut budget, bytes).unwrap()
    }

    #[test]
    fn resume_and_text_produces_a_subtitle_frame_carrying_packet_timing() {
        let mut decoder = CcSubtitleDecoder::new();
        let mut cc_data = Vec::new();
        // Resume Caption Loading (0x1420, odd parity), moves to pop-on mode.
        cc_data.extend_from_slice(&triplet608(odd_parity(0x14), odd_parity(0x20)));
        // "HI" as two standard characters.
        let hi = cc_byte_pair(b'H', odd_parity);
        cc_data.extend_from_slice(&triplet608(hi[0], odd_parity(b'I')));
        // End of Caption (0x142F) swaps the built screen onto display.
        cc_data.extend_from_slice(&triplet608(odd_parity(0x14), odd_parity(0x2F)));

        let mut pkt = packet_from(&cc_data);
        pkt.pts = vaco_core::Timestamp::new(9);
        pkt.duration = vaco_core::Duration::from_micros(500_000);
        decoder.send(Some(&pkt)).unwrap();

        let frame = decoder.receive().unwrap();
        assert_eq!(frame.pts, vaco_core::Timestamp::new(9));
        assert_eq!(frame.duration, vaco_core::Duration::from_micros(500_000));
        let FrameData::Subtitle { rects } = &frame.data else {
            unreachable!("cc decode must produce FrameData::Subtitle");
        };
        let vaco_frame::SubtitleContent::Text(text) = &rects[0].content else {
            unreachable!("cc decode must produce SubtitleContent::Text");
        };
        assert!(text.contains("HI"), "got {text:?}");
    }

    #[test]
    fn a_packet_with_no_command_produces_no_frame() {
        let mut decoder = CcSubtitleDecoder::new();
        let pkt = packet_from(&[]);
        decoder.send(Some(&pkt)).unwrap();
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::NeedMoreInput)));
    }

    #[test]
    fn drain_with_nothing_pending_reaches_eof_immediately() {
        let mut decoder = CcSubtitleDecoder::new();
        decoder.send(None).unwrap();
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::Eof)));
    }

    #[test]
    fn registered_decoder_matches_the_codec_id() {
        assert_eq!(CC_DECODER.id, CodecId::Eia608);
        assert_eq!(CC_DECODER.media_type, MediaType::Subtitle);
        let mut decoder = (CC_DECODER.make)(Limits::permissive());
        decoder.flush(); // must not panic on a freshly built decoder
    }
}
