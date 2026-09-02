//! The `vaco_codec_core::Decoder` face over [`crate::decoder::TeletextDecoder`],
//! and the [`DecoderDesc`] that makes it reachable from `vaco-registry` as
//! `CodecId::DvbTeletext`'s decoder.
//!
//! # `SubtitleContent::Text`, not `Bitmap`
//!
//! `vaco_frame::subtitle`'s own module docs name Teletext as one of the
//! formats that "decode ... to positioned text" and list `Text` as the
//! variant for "already-decoded plain text — CEA-608/708 and Teletext once
//! their own decoders produce characters". [`page_to_text`] is that
//! translation: [`crate::page::Page`]'s 40x25 character grid, rendered as
//! plain lines with no coordinate information — matching `SubtitleRect::
//! text`'s own `(0, 0, 0, 0)` convention for content with no fixed
//! on-screen box, since a Teletext page occupies the whole screen by
//! definition rather than a positioned region within it.
//!
//! Rendering to plain text is lossy, and stated rather than hidden: every
//! spacing attribute [`crate::page::Cell`] carries (colour, flash, box,
//! double height/width, hold-mosaics) is discarded, and a [`crate::page::
//! Glyph::Mosaic`] or [`crate::page::Glyph::Corrupt`] cell — which has no
//! meaningful character — renders as a plain space. A future caller that
//! wants the full presentation (mosaics rendered as blocks, colours
//! preserved) needs a `SubtitleContent` shape this workspace does not have
//! today; `Text` is what it does have, and it is not a lie about what a
//! Teletext page's *readable content* says.
//!
//! # Timing
//!
//! `Frame::pts`/`duration` are copied from the packet unchanged, the same
//! choice `vaco-codec-subtitle-bitmap`'s decoders make for a format with no
//! independent timing of its own — the raw `dvbtxt` demuxer sets
//! `FormatFlags::NOTIMESTAMPS` (see `vaco-subtitle-bitmap::dvbtxt`'s
//! module docs), so a packet's `pts` is not meaningful teletext-side either;
//! this crate does not invent a display duration Teletext itself does not
//! state.
//!
//! # `Caps::DELAY`, measured against `finish()`'s own behaviour
//!
//! Unlike this workspace's other subtitle decoders (DVB/PGS/`VobSub`, which
//! discard an epoch still open at end of stream), [`TeletextDecoder::
//! finish`] deliberately flushes every magazine still assembling a page —
//! see that method's own docs. That is genuine drain-time output, which
//! [`vaco_codec_core::Machine::emit`] refuses without [`Caps::DELAY`]
//! declared (checked, not just documented: a debug assertion fires
//! otherwise). [`Caps::SUBFRAMES`] is also set: one `push` can complete
//! pages on more than one of the eight magazines at once.

use vaco_codec_core::{Accept, Caps, CodecId, DecoderDesc, Machine, SendReceive};
use vaco_core::{MediaType, Result};
use vaco_frame::{Frame, FrameData, SubtitleRect};
use vaco_limits::Limits;
use vaco_packet::Packet;

use crate::decoder::{PageEvent, TeletextDecoder};
use crate::page::{Glyph, Page};

/// Render a page's 40x25 grid as plain text: one line per row, trailing
/// spaces trimmed, rows joined by `\n`. See the module docs for exactly
/// what this discards.
#[must_use]
pub fn page_to_text(page: &Page) -> String {
    let mut out = String::new();
    for (i, row) in page.rows.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line: String = row
            .iter()
            .map(|cell| match cell.glyph {
                Glyph::Text(c) => c,
                Glyph::Mosaic { .. } | Glyph::Corrupt => ' ',
            })
            .collect();
        out.push_str(line.trim_end());
    }
    out
}

fn frame_of_event(event: &PageEvent, packet: &Packet) -> Frame {
    let text = page_to_text(&event.page);
    let rect = SubtitleRect::text(0, 0, 0, 0, false, text);
    let mut frame = Frame::from_data(FrameData::Subtitle {
        rects: std::iter::once(rect).collect(),
    });
    frame.pts = packet.pts;
    frame.duration = packet.duration;
    frame
}

/// Teletext decode as a `SendReceive`. See the module docs for `Caps` and
/// the text-rendering trade-off.
#[derive(Debug)]
pub struct TeletextSubtitleDecoder {
    machine: Machine<Frame>,
    inner: TeletextDecoder,
    /// The most recent packet's `pts`/`duration`, reused for any page
    /// [`TeletextDecoder::finish`] flushes at drain time — there is no
    /// packet to copy timing from once draining has started.
    last: Packet,
}

impl TeletextSubtitleDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES.union(Caps::DELAY)),
            inner: TeletextDecoder::new(),
            last: Packet::default(),
        }
    }
}

impl Default for TeletextSubtitleDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SendReceive for TeletextSubtitleDecoder {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                for event in self.inner.finish() {
                    self.machine.emit(frame_of_event(&event, &self.last));
                }
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else {
                    return Ok(());
                };
                self.last = pkt.clone();
                for event in self.inner.push(pkt.payload()) {
                    self.machine.emit(frame_of_event(&event, pkt));
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
        self.inner = TeletextDecoder::new();
        self.last = Packet::default();
    }
}

fn make(_limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        TeletextSubtitleDecoder::new(),
    )))
}

/// Registered as this crate's `dvb_teletext` decoder fragment
/// (`vaco-component.toml`), matching `CodecId::DvbTeletext::name()`.
///
/// A prior pass named this `"teletext"`, believing `ffmpeg -decoders`'
/// actual decoder-implementation name (behind `--enable-libzvbi`, absent
/// from this project's build environment) was unmeasurable and unsafe to
/// guess — reasonable, given D17, but incomplete: `ffmpeg -h decoder=<name>`
/// distinguishes a name `FFmpeg`'s codec table recognises but cannot build
/// (`"Codec 'X' is known to FFmpeg, but no decoders for it are available"`)
/// from one it does not know at all (`"Codec 'X' is not recognized by
/// FFmpeg"`), and does not require the feature to actually be built to give
/// that answer. Measured: `dvb_teletext` is known; `teletext` and the
/// guessed `libzvbi_teletextdec` are both unrecognised. `dvb_teletext` is
/// therefore the real, measured codec-level name real `ffmpeg` resolves a
/// decoder through (the same mechanism `-c:v vp8` uses to reach `libvpx`
/// with no encoder literally named `vp8`) — cheaper to reach than the
/// literal decoder-implementation name and just as real a target. Found by
/// `cargo xtask reachability-check`'s rule H (registered name vs. the
/// reference's own measured name).
pub static TELETEXT_DECODER: DecoderDesc = DecoderDesc {
    name: "dvb_teletext",
    long_name: "Teletext (EN 300 706)",
    id: CodecId::DvbTeletext,
    media_type: MediaType::Subtitle,
    caps: Caps::SUBFRAMES.union(Caps::DELAY),
    supported_rates: &[],
    make,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Budget;

    fn parity_byte(data: u8) -> u8 {
        let d = data & 0x7F;
        if d.count_ones() % 2 == 1 {
            d
        } else {
            d | 0x80
        }
    }

    fn hamming_byte(nibble: u8) -> u8 {
        let d1 = nibble & 1;
        let d2 = (nibble >> 1) & 1;
        let d3 = (nibble >> 2) & 1;
        let d4 = (nibble >> 3) & 1;
        let p1 = 1 ^ d1 ^ d3 ^ d4;
        let p2 = 1 ^ d1 ^ d2 ^ d4;
        let p3 = 1 ^ d1 ^ d2 ^ d3;
        let p4 = 1 ^ p1 ^ d1 ^ p2 ^ d2 ^ p3 ^ d3 ^ d4;
        (p1 & 1)
            | ((d1 & 1) << 1)
            | ((p2 & 1) << 2)
            | ((d2 & 1) << 3)
            | ((p3 & 1) << 4)
            | ((d3 & 1) << 5)
            | ((p4 & 1) << 6)
            | ((d4 & 1) << 7)
    }

    fn data_unit(magazine: u8, packet_no: u8, body: &[u8]) -> Vec<u8> {
        let address = u16::from(magazine & 0x7) | (u16::from(packet_no) << 3);
        let byte4 = hamming_byte((address & 0xF) as u8);
        let byte5 = hamming_byte(((address >> 4) & 0xF) as u8);
        let mut record = vec![0x02u8, 0x2C, 0xC0, 0xE4, byte4, byte5];
        record.extend_from_slice(body);
        while record.len() < 46 {
            record.push(parity_byte(b' '));
        }
        record
    }

    #[test]
    fn page_to_text_renders_rows_and_trims_trailing_spaces() {
        let mut page = Page::blank_for_test(1);
        page.fill_body_row(1, &(*b"HI").map(parity_byte));
        let text = page_to_text(&page);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 25);
        assert_eq!(lines[1], "HI");
        assert_eq!(lines[2], "");
    }

    #[test]
    fn corrupt_cells_render_as_space() {
        let mut page = Page::blank_for_test(1);
        // 0x41 has even parity: decodes as Glyph::Corrupt.
        page.fill_body_row(1, &[0x41]);
        let text = page_to_text(&page);
        let row1 = text.split('\n').nth(1).unwrap();
        assert_eq!(row1, "");
    }

    #[test]
    fn decoder_emits_a_frame_carrying_the_packet_timing() {
        let mut decoder = TeletextSubtitleDecoder::new();
        let header_ctrl = [0u8; 8].map(hamming_byte);
        let mut body = header_ctrl.to_vec();
        body.extend("HELLO".bytes().map(parity_byte));
        let header = data_unit(1, 0, &body);

        let row_text: Vec<u8> = "WORLD".bytes().map(parity_byte).collect();
        let row = data_unit(1, 1, &row_text);

        let mut next_body = header_ctrl.to_vec();
        next_body.extend("NEXT".bytes().map(parity_byte));
        let next_header = data_unit(1, 0, &next_body);

        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, &header).unwrap();
        decoder.send(Some(&pkt)).unwrap();
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::NeedMoreInput)));

        pkt = Packet::from_slice(&mut budget, &row).unwrap();
        decoder.send(Some(&pkt)).unwrap();
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::NeedMoreInput)));

        // The frame's timing comes from whichever packet's `push` call
        // caused the page to finish assembling (the *next* header, per
        // `TeletextDecoder::apply_record`'s own docs) — not the header that
        // started it.
        pkt = Packet::from_slice(&mut budget, &next_header).unwrap();
        pkt.pts = vaco_core::Timestamp::new(42);
        decoder.send(Some(&pkt)).unwrap();

        let frame = decoder.receive().unwrap();
        assert_eq!(frame.pts, vaco_core::Timestamp::new(42));
        let FrameData::Subtitle { rects } = &frame.data else {
            unreachable!("teletext must produce FrameData::Subtitle");
        };
        assert_eq!(rects.len(), 1);
        let vaco_frame::SubtitleContent::Text(text) = &rects[0].content else {
            unreachable!("teletext must produce SubtitleContent::Text");
        };
        assert!(text.lines().nth(1).unwrap().contains("WORLD"));
    }

    #[test]
    fn drain_flushes_a_page_still_assembling() {
        let mut decoder = TeletextSubtitleDecoder::new();
        let header_ctrl = [0u8; 8].map(hamming_byte);
        let header = data_unit(2, 0, &header_ctrl);
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &header).unwrap();
        decoder.send(Some(&pkt)).unwrap();
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::NeedMoreInput)));

        decoder.send(None).unwrap();
        let frame = decoder.receive();
        assert!(frame.is_ok(), "finish() must flush the in-progress page");
        assert!(matches!(decoder.receive(), Err(vaco_core::Error::Eof)));
    }

    #[test]
    fn registered_decoder_matches_the_codec_id() {
        assert_eq!(TELETEXT_DECODER.id, CodecId::DvbTeletext);
        assert_eq!(TELETEXT_DECODER.media_type, MediaType::Subtitle);
        let mut decoder = (TELETEXT_DECODER.make)(Limits::permissive());
        decoder.flush(); // must not panic on a freshly built decoder
    }
}
