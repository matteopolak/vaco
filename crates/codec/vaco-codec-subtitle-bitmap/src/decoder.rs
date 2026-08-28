//! The `vaco_codec_core::Decoder` face over this crate's three decode
//! functions, and the [`DecoderDesc`]s that make them reachable from
//! `vaco-registry`.
//!
//! # Why this is a thin wrapper and not where the work happens
//!
//! [`crate::dvb`], [`crate::pgs`] and [`crate::vobsub`] decode; this module
//! only adapts. It exists because `FrameData::Subtitle` landed (interface
//! gap 17) after those three were written, so the decode functions were
//! designed against this crate's own [`crate::SubtitleEvent`] rather than
//! against `vaco_frame`. Keeping the adaptation in one file means the
//! decoders stay testable without a `Frame`, and the mapping from
//! `SubtitleEvent` to `FrameData::Subtitle` is stated once, in
//! [`frame_of_event`], where it can be checked.
//!
//! # The `SubtitleContent::Bitmap` fit
//!
//! Close, and worth stating precisely because that variant's author asked
//! its first real consumer to push back. An
//! [`vaco_format_subtitle_bitmap::IndexedBitmap`] is a `Rect`, a `Palette`
//! of at most 256 `Rgba`, and a row-major `Vec<u8>` of indices with no
//! padding — so `stride` is always exactly `w` (never something this
//! decoder computes separately), and the palette converts with one `map`.
//! Nothing in the three formats needed a rect field the variant lacks.
//!
//! # Timing: what these frames carry, and why no time base was needed
//!
//! `Frame::pts` is copied from the packet unchanged: it is a tick count in
//! the stream's own time base, and neither DVB nor `VobSub` states a display
//! window relative to anything other than the packet's own arrival, so there
//! is nothing to shift it by.
//!
//! `Frame::duration` is a [`vaco_core::Duration`] — always real microseconds
//! by construction (`Duration::from_micros`/`as_micros`), never ticks of a
//! time base — which is also what [`SubtitleEvent::start`]/`end` already are.
//! So DVB's `page_time_out` (whole seconds) and `VobSub`'s SPU start/stop
//! delays (90 kHz / 1024 ticks, converted by [`crate::vobsub`]) both reach
//! [`frame_of_event`] pre-converted to the same unit `Frame::duration` wants,
//! and it uses `event.end - event.start` in place of the packet's own
//! duration whenever the codec stated one. No stream time base is needed
//! for this, and none is threaded through `Decoder` — worth stating plainly
//! since the container's out-of-band configuration (`Decoder::set_extradata`)
//! genuinely does need a channel `Decoder` never had, and it would be easy
//! to assume the two problems share a fix.
//!
//! PGS never states an end (`SubtitleEvent::end` is always `None` there), so
//! its frames keep the packet's own duration exactly as before.
//!
//! One piece is still open: `event.start` can be non-zero for `VobSub` (a
//! `SP_STA_DSP` delayed past the packet's first control sequence), and
//! shifting `Frame::pts` forward by that amount *would* need the stream's
//! time base to convert a microsecond delay into ticks. Left as `Frame::pts
//! = packet.pts` unconditionally; the display *length* this module now
//! reports is correct regardless, only the display *start* can be off by
//! that delay on the rare stream that sets one.

use vaco_codec_core::{Accept, Caps, CodecId, Machine, SendReceive};
use vaco_core::{Duration, MediaType, Result};
use vaco_frame::{Frame, FrameData, SubtitleRect};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::SubtitleEvent;

/// The display duration `event` states, in the same unit as
/// `Frame::duration`, or `fallback` when the codec did not state one.
fn display_duration(event: &SubtitleEvent, fallback: Duration) -> Duration {
    match event.end {
        Some(end) => Duration::from_micros(end.as_micros().saturating_sub(event.start.as_micros())),
        None => fallback,
    }
}

/// Convert one decoded event into a `Frame`. `pts` is copied from `packet`;
/// `duration` prefers the codec's own display window over the packet's, see
/// the module docs' "Timing" section.
///
/// Every rect's pixel bytes go through `budget`, matching the rule this
/// workspace applies to every other decoder output: the dimensions came
/// from attacker-controlled bytes, so the copy is metered even though the
/// decode that produced them already was.
///
/// # Errors
/// Whatever [`SubtitleRect::bitmap`] reports — in practice
/// [`vaco_core::Error::LimitExceeded`] if the budget cannot take the copy.
fn frame_of_event(event: &SubtitleEvent, packet: &Packet, budget: &mut Budget) -> Result<Frame> {
    let mut rects = Vec::new();
    for bitmap in &event.rects {
        let rect = bitmap.rect();
        let palette: Vec<[u8; 4]> = bitmap
            .palette()
            .entries()
            .iter()
            .map(|c| [c.r, c.g, c.b, c.a])
            .collect();
        // `IndexedBitmap` is row-major with no inter-row padding by
        // construction, so the stride *is* the width. Stated here rather
        // than assumed at the call site.
        let stride = rect.width as usize;
        rects.push(SubtitleRect::bitmap(
            budget,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            event.forced,
            stride,
            bitmap.indices(),
            palette,
        )?);
    }
    // Collected rather than named: the field is a `SmallVec`, and inference
    // reaches it through `FromIterator` without this crate taking a
    // dependency on `smallvec` for one type name.
    let mut frame = Frame::from_data(FrameData::Subtitle {
        rects: rects.into_iter().collect(),
    });
    frame.pts = packet.pts;
    frame.duration = display_duration(event, packet.duration);
    Ok(frame)
}

// ------------------------------------------------------------------ dvbsub

/// DVB subtitle decode as a `SendReceive`.
///
/// [`Caps::SUBFRAMES`], measured against this crate's own framing rather
/// than assumed: the registered `dvbsub` demuxer emits fixed 1024-byte
/// chunks with no segment awareness (see `vaco_subtitle_bitmap::dvbsub`'s
/// docs), so one packet can legitimately complete two or more display sets.
/// Deliberately **not** [`Caps::DELAY`] — this decoder buffers internally,
/// but an epoch left incomplete at end of stream is discarded rather than
/// emitted, so draining never yields a frame and promising otherwise would
/// be a false claim to the caller.
#[derive(Debug)]
pub struct DvbSubtitleDecoder {
    machine: Machine<Frame>,
    inner: crate::dvb::DvbSubDecoder,
    limits: Limits,
}

impl DvbSubtitleDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES),
            inner: crate::dvb::DvbSubDecoder::new(limits.clone()),
            limits,
        }
    }
}

impl SendReceive for DvbSubtitleDecoder {
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
                let events = self.inner.push(pkt.payload())?;
                let mut budget = Budget::new(self.limits.clone());
                for event in &events {
                    self.machine.emit(frame_of_event(event, pkt, &mut budget)?);
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
        self.inner = crate::dvb::DvbSubDecoder::new(self.limits.clone());
    }
}

// --------------------------------------------------------------------- pgs

/// PGS/HDMV decode as a `SendReceive`.
///
/// One packet is one segment record (what
/// `vaco_subtitle_bitmap::sup::PgsDemuxer` emits), and only the `END`
/// segment of a display set completes an event — so most packets produce no
/// frame and none produces two. Neither [`Caps::SUBFRAMES`] nor
/// [`Caps::DELAY`] applies: producing nothing for an input is ordinary, and
/// an epoch still open at end of stream is discarded rather than emitted.
#[derive(Debug)]
pub struct PgsSubtitleDecoder {
    machine: Machine<Frame>,
    inner: crate::pgs::PgsDecoder,
    limits: Limits,
}

impl PgsSubtitleDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            inner: crate::pgs::PgsDecoder::new(),
            limits,
        }
    }
}

impl SendReceive for PgsSubtitleDecoder {
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
                if let Some(event) = self.inner.push_segment(pkt.payload(), &self.limits)? {
                    let mut budget = Budget::new(self.limits.clone());
                    self.machine.emit(frame_of_event(&event, pkt, &mut budget)?);
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
        self.inner = crate::pgs::PgsDecoder::new();
    }
}

// ------------------------------------------------------------------ vobsub

/// The palette a registered `vobsub` decoder paints with until
/// [`VobSubSubtitleDecoder::set_extradata`] supplies the real one.
///
/// **This is a fallback, not the format's palette**, and it is visible here
/// rather than buried because the difference is user-visible colour. A DVD
/// subpicture's four pseudo-colours are indices into a 16-entry table that
/// lives *outside* the SPU bytes — in the `.idx` sidecar, or in a Matroska
/// `S_VOBSUB` track's `CodecPrivate`. Before `Decoder::set_extradata`
/// existed there was no way to hand a registry-built decoder that table at
/// all, and geometry and pixel indices came out right while colours came out
/// of this ramp instead of the disc's; a caller wiring the container's own
/// record through now gets the disc's colours, and one that does not (or
/// whose container states none) still gets this.
///
/// A caller that already has the real palette in hand, without a `Decoder`
/// in between, should bypass this wrapper entirely and call
/// [`crate::vobsub::decode_spu`] directly, which takes it as a parameter.
///
/// The ramp itself is this project's own choice, not a measured default:
/// index 0 transparent-black and the rest an even grey ramp, so a rendered
/// subtitle is legible rather than invisible while plainly not claiming to
/// be the disc's colours.
fn fallback_palette() -> vaco_format_subtitle_bitmap::Palette {
    let entries = (0..16u16)
        .map(|i| {
            let level = u8::try_from(i.saturating_mul(17)).unwrap_or(u8::MAX);
            vaco_format_subtitle_bitmap::Rgba::new(level, level, level, 0xFF)
        })
        .collect();
    vaco_format_subtitle_bitmap::Palette::new(entries).unwrap_or_default()
}

/// `VobSub`/DVD subpicture decode as a `SendReceive`. One SPU per packet, so
/// neither [`Caps::SUBFRAMES`] nor [`Caps::DELAY`] applies.
///
/// See [`fallback_palette`] for what this decoder paints with before
/// [`Self::set_extradata`] is called, or when it is never called at all.
#[derive(Debug)]
pub struct VobSubSubtitleDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    palette: vaco_format_subtitle_bitmap::Palette,
}

impl VobSubSubtitleDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            palette: fallback_palette(),
        }
    }
}

impl SendReceive for VobSubSubtitleDecoder {
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
                let event = crate::vobsub::decode_spu(pkt.payload(), &self.palette, &self.limits)?;
                let mut budget = Budget::new(self.limits.clone());
                self.machine.emit(frame_of_event(&event, pkt, &mut budget)?);
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    /// Take the disc's real 16-entry palette from a Matroska `S_VOBSUB`
    /// track's `CodecPrivate`, which the format's own subtitle-mapping page
    /// states is exactly the `.idx` file's `size:`/`palette:` lines
    /// (`id:`/`timestamp:`/comment lines removed): `vaco_subtitle_bitmap`'s
    /// `.idx` grammar already parses that text for the demuxer side, so this
    /// reuses it rather than writing a second parser for the same syntax.
    ///
    /// Bytes that are not valid UTF-8, or that parse but state no
    /// `palette:` line, leave [`fallback_palette`] in place — offering
    /// extradata is not a promise it will be used, matching
    /// [`vaco_codec_core::Parser::set_extradata`]'s own convention.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Ok(text) = std::str::from_utf8(extradata)
            && let Some(palette) = vaco_subtitle_bitmap::vobsub::idx::parse(text).palette
        {
            self.palette = palette;
        }
        Ok(())
    }

    fn flush(&mut self) {
        self.machine.flush();
    }
}

// ------------------------------------------------------------- descriptors

fn make_dvb(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        DvbSubtitleDecoder::new(limits),
    )))
}

fn make_pgs(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        PgsSubtitleDecoder::new(limits),
    )))
}

fn make_vobsub(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        VobSubSubtitleDecoder::new(limits),
    )))
}

/// Registered as this crate's `dvbsub` decoder fragment.
pub static DVBSUB_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "dvbsub",
    long_name: "DVB subtitles",
    id: CodecId::DvbSubtitle,
    media_type: MediaType::Subtitle,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make: make_dvb,
};

/// Registered as this crate's `pgssub` decoder fragment. Named for the
/// reference's own decoder name (`ffmpeg -decoders` lists `pgssub` decoding
/// `hdmv_pgs_subtitle`), not for the container's `sup`.
pub static PGSSUB_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "pgssub",
    long_name: "HDMV Presentation Graphic Stream subtitles",
    id: CodecId::HdmvPgsSubtitle,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_pgs,
};

/// Registered as this crate's `dvdsub` decoder fragment.
pub static DVDSUB_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "dvdsub",
    long_name: "DVD subtitles",
    id: CodecId::DvdSubtitle,
    media_type: MediaType::Subtitle,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_vobsub,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    use vaco_core::Timestamp;

    fn packet(payload: &[u8], pts: i64) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut p = Packet::from_slice(&mut budget, payload).unwrap();
        p.pts = Timestamp::new(pts);
        p.duration = vaco_core::Duration::from_micros(2_000_000);
        p
    }

    /// The same display set `crate::dvb`'s own tests decode, and the same one
    /// `tests/fixtures/compare.py` checks against ffmpeg's decoder.
    fn dvb_display_set() -> Vec<u8> {
        fn seg(kind: u8, payload: &[u8]) -> Vec<u8> {
            let mut v = vec![0x0F, kind, 0, 1];
            v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
            v.extend_from_slice(payload);
            v
        }
        let mut page = vec![5u8, 0x08, 0, 0];
        page.extend_from_slice(&10u16.to_be_bytes());
        page.extend_from_slice(&10u16.to_be_bytes());
        let mut region = vec![0u8, 0x08];
        region.extend_from_slice(&4u16.to_be_bytes());
        region.extend_from_slice(&4u16.to_be_bytes());
        region.extend_from_slice(&[0x24, 0x00, 0x00, 0x04]);
        region.extend_from_slice(&1u16.to_be_bytes());
        region.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        let clut = vec![0u8, 0x00, 1, 0x81, 255, 128, 128, 255];
        let line = [0x10u8, 0x55, 0x00, 0xF0];
        let mut field = Vec::new();
        field.extend_from_slice(&line);
        field.extend_from_slice(&line);
        let mut obj = vec![0u8, 1, 0x00];
        obj.extend_from_slice(&(field.len() as u16).to_be_bytes());
        obj.extend_from_slice(&(field.len() as u16).to_be_bytes());
        obj.extend_from_slice(&field);
        obj.extend_from_slice(&field);

        let mut all = Vec::new();
        all.extend(seg(0x10, &page));
        all.extend(seg(0x11, &region));
        all.extend(seg(0x12, &clut));
        all.extend(seg(0x13, &obj));
        all.extend(seg(0x80, &[]));
        all
    }

    #[test]
    fn dvb_packet_becomes_a_subtitle_frame_carrying_the_librarys_own_rects() {
        // The claim this test exists for: what the registered Decoder emits
        // is the same rect the library already produces, not a re-derivation.
        let bytes = dvb_display_set();
        let expected = crate::dvb::decode_display_set(&bytes, &Limits::permissive()).unwrap();

        let mut dec = make_dvb(Limits::permissive());
        dec.send_packet(Some(&packet(&bytes, 4242))).unwrap();
        let frame = dec.receive_frame().unwrap();

        assert_eq!(frame.pts, Timestamp::new(4242));
        // `page_time_out` in `dvb_display_set`'s page composition is 5, and
        // the codec's own stated display window now wins over the packet's
        // fixed 2-second `duration` from the `packet` test helper.
        assert_eq!(frame.duration, vaco_core::Duration::from_micros(5_000_000));
        let FrameData::Subtitle { rects } = &frame.data else {
            unreachable!("a subtitle decoder must produce FrameData::Subtitle");
        };
        assert_eq!(rects.len(), expected.rects.len());
        assert_eq!(rects.len(), 1);
        let got = &rects[0];
        let want = &expected.rects[0];
        assert_eq!(
            (got.x, got.y, got.w, got.h),
            (
                want.rect().x,
                want.rect().y,
                want.rect().width,
                want.rect().height
            )
        );
        let vaco_frame::SubtitleContent::Bitmap {
            stride,
            data,
            palette,
        } = &got.content
        else {
            unreachable!("a bitmap subtitle decoder must produce Bitmap content");
        };
        // The fit claim, asserted rather than asserted-in-prose: stride is
        // exactly the width, and the pixels are the library's own indices.
        assert_eq!(*stride, want.rect().width as usize);
        assert_eq!(data.as_slice(), want.indices());
        assert_eq!(palette.len(), want.palette().len());
        assert_eq!(palette[1], [255, 255, 255, 255]);
    }

    #[test]
    fn pgs_emits_one_frame_only_on_the_end_segment() {
        fn segment(kind: u8, payload: &[u8]) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(b"PG");
            v.extend_from_slice(&90_000u32.to_be_bytes());
            v.extend_from_slice(&0u32.to_be_bytes());
            v.push(kind);
            v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
            v.extend_from_slice(payload);
            v
        }
        let mut pcs = Vec::new();
        pcs.extend_from_slice(&1920u16.to_be_bytes());
        pcs.extend_from_slice(&1080u16.to_be_bytes());
        pcs.push(0x10);
        pcs.extend_from_slice(&0u16.to_be_bytes());
        pcs.extend_from_slice(&[0x80, 0x00, 0, 1]);
        pcs.extend_from_slice(&1u16.to_be_bytes());
        pcs.extend_from_slice(&[0, 0x00]);
        pcs.extend_from_slice(&5u16.to_be_bytes());
        pcs.extend_from_slice(&5u16.to_be_bytes());
        let pds = vec![0u8, 0, 1, 255, 128, 128, 255];
        let rle = [1u8, 1, 0, 0, 1, 1];
        let mut ods = Vec::new();
        ods.extend_from_slice(&1u16.to_be_bytes());
        ods.extend_from_slice(&[0, 0xC0]);
        let data_len = 4 + rle.len();
        ods.extend_from_slice(&[0, (data_len >> 8) as u8, data_len as u8]);
        ods.extend_from_slice(&2u16.to_be_bytes());
        ods.extend_from_slice(&2u16.to_be_bytes());
        ods.extend_from_slice(&rle);

        let mut dec = make_pgs(Limits::permissive());
        let mut frames = 0;
        for (kind, payload) in [
            (0x16u8, pcs.as_slice()),
            (0x14, pds.as_slice()),
            (0x15, ods.as_slice()),
            (0x80, [].as_slice()),
        ] {
            let record = segment(kind, payload);
            dec.send_packet(Some(&packet(&record, 90_000))).unwrap();
            while dec.receive_frame().is_ok() {
                frames += 1;
            }
        }
        assert_eq!(frames, 1, "exactly one display set completed");
    }

    #[test]
    fn every_registered_descriptor_builds_and_declares_subtitle_media() {
        for desc in [&DVBSUB_DECODER, &PGSSUB_DECODER, &DVDSUB_DECODER] {
            assert_eq!(desc.media_type, MediaType::Subtitle);
            assert!(desc.is_default_build_safe());
            let _ = desc.build(Limits::strict());
        }
    }

    #[test]
    fn each_descriptor_claims_the_codec_id_the_registry_looks_it_up_by() {
        // `vaco_registry::decoder_for(codec)` scans `DECODERS` for a matching
        // `id`, and `vaco-cli`'s transcode leg calls exactly that with the
        // demuxer's `CodecParameters::codec_id`. Asserting the mapping rather
        // than the emptiness, per this project's rule about never pinning the
        // absence of something being built: these three ids are the ones the
        // matching demuxers already set on their streams.
        assert_eq!(DVBSUB_DECODER.id, CodecId::DvbSubtitle);
        assert_eq!(PGSSUB_DECODER.id, CodecId::HdmvPgsSubtitle);
        assert_eq!(DVDSUB_DECODER.id, CodecId::DvdSubtitle);
    }

    #[test]
    fn a_dvb_decoder_survives_a_packet_it_cannot_parse() {
        // Detection and decoding ask different questions: a decoder handed
        // rubbish should report, not panic, and should still be usable after.
        let mut dec = make_dvb(Limits::permissive());
        let _ = dec.send_packet(Some(&packet(b"not a display set at all", 0)));
        let bytes = dvb_display_set();
        // The rubbish contained no sync byte, so nothing was buffered and a
        // real display set still decodes afterwards.
        dec.send_packet(Some(&packet(&bytes, 7))).unwrap();
        assert!(dec.receive_frame().is_ok());
    }

    /// A 4x2 SPU whose pattern colour (index 1) reads palette slot 3 —
    /// `vobsub.rs`'s own `sample_spu`/`sample_palette` fixture, reproduced
    /// here because that module's `#[cfg(test)]` items are private to it.
    fn vobsub_spu() -> Vec<u8> {
        let mut body = vec![0, 0, 0, 0];
        let top_offset = body.len();
        body.push(0x55);
        body.push(0x55);
        let bottom_offset = body.len();
        body.push(0x55);
        body.push(0x55);
        let dcsqta = body.len();
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&(dcsqta as u16).to_be_bytes());
        body.push(0x01); // STA_DSP
        body.push(0x03); // SET_COLOR: pattern (colours[1]) -> palette slot 3
        body.push(0x21);
        body.push(0x30);
        body.push(0x04); // SET_CONTR: every nibble at full alpha
        body.push(0xFF);
        body.push(0xFF);
        body.push(0x05); // SET_DAREA: (0,0)-(3,1)
        body.push(0x00);
        body.push(0x00);
        body.push(0x03);
        body.push(0x00);
        body.push(0x00);
        body.push(0x01);
        body.push(0x06); // SET_DSPXA
        body.extend_from_slice(&(top_offset as u16).to_be_bytes());
        body.extend_from_slice(&(bottom_offset as u16).to_be_bytes());
        body.push(0xFF);
        let size = body.len() as u16;
        body[0] = (size >> 8) as u8;
        body[1] = (size & 0xFF) as u8;
        body[2] = (dcsqta >> 8) as u8;
        body[3] = (dcsqta & 0xFF) as u8;
        body
    }

    fn bitmap_palette(frame: &Frame) -> Vec<[u8; 4]> {
        let FrameData::Subtitle { rects } = &frame.data else {
            unreachable!("a subtitle decoder must produce FrameData::Subtitle");
        };
        let vaco_frame::SubtitleContent::Bitmap { palette, .. } = &rects[0].content else {
            unreachable!("a bitmap subtitle decoder must produce Bitmap content");
        };
        palette.clone()
    }

    /// Gap 19's measured claim: before `set_extradata`, the registered
    /// `dvdsub` decoder paints with [`fallback_palette`]'s grey ramp; after
    /// it is offered the container's own record, it paints with the disc's
    /// colours. Both runs decode the identical SPU bytes, so the only
    /// variable is whether the palette record reached the decoder.
    #[test]
    fn dvd_subtitle_colours_come_from_extradata_not_the_grey_ramp() {
        let spu = vobsub_spu();

        let mut before = make_vobsub(Limits::permissive());
        before.send_packet(Some(&packet(&spu, 0))).unwrap();
        let before_frame = before.receive_frame().unwrap();
        // Fallback ramp slot 3 is 3 * 17 = 51, per `fallback_palette`'s doc.
        assert_eq!(bitmap_palette(&before_frame)[1], [51, 51, 51, 255]);

        // The exact shape Matroska's own subtitle-mapping page says a
        // `S_VOBSUB` track's `CodecPrivate` carries: the `.idx` file's own
        // `size:`/`palette:` lines.
        let idx_text = b"size: 720x480\npalette: 000000, 0a141e, ffffff, 010203\n";
        let mut after = make_vobsub(Limits::permissive());
        after.set_extradata(idx_text).unwrap();
        after.send_packet(Some(&packet(&spu, 0))).unwrap();
        let after_frame = after.receive_frame().unwrap();
        // Palette slot 3 is `010203` in the record above.
        assert_eq!(bitmap_palette(&after_frame)[1], [1, 2, 3, 255]);
    }

    /// The same measurement, driven through exactly the path a real caller
    /// uses: `DecoderDesc::build` (`Box<dyn Decoder>`), not the private
    /// `make_vobsub` constructor. If `AsDecoder`/`Validated`'s forwarding of
    /// `set_extradata` ever regressed, this would still see the grey ramp
    /// after offering a real palette — the same trap
    /// `vaco-codec-core`'s own protocol tests catch one layer down.
    #[test]
    fn the_registered_dvdsub_decoder_forwards_set_extradata_through_the_box() {
        let spu = vobsub_spu();
        let mut dec = DVDSUB_DECODER.build(Limits::permissive());
        dec.set_extradata(b"size: 720x480\npalette: 000000, 0a141e, ffffff, 010203\n")
            .unwrap();
        dec.send_packet(Some(&packet(&spu, 0))).unwrap();
        let frame = dec.receive_frame().unwrap();
        assert_eq!(bitmap_palette(&frame)[1], [1, 2, 3, 255]);
    }
}
