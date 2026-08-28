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
//! # Timing: what these frames can and cannot carry
//!
//! `Frame::pts`/`Frame::duration` are copied from the **packet**, i.e. the
//! container's own timing in the stream's time base, which is what the
//! graph edge `PipelineSpec::add_decoder` creates is counted in.
//!
//! The codec-internal display window is deliberately **not** merged into
//! them. DVB's `page_time_out` (whole seconds) and `VobSub`'s SPU
//! start/stop delays (90 kHz / 1024 ticks) are absolute durations, so
//! expressing either as a `pts`/`duration` in the stream's time base needs
//! that time base — and the `Decoder` trait has no channel that carries it
//! (`send_packet`/`receive_frame`/`flush` is the whole surface). Writing
//! them into the frame in some *other* unit would give one frame two
//! disagreeing ideas of when it displays, which is exactly what
//! `vaco_frame::subtitle`'s own docs say the variant was shaped to avoid.
//! Recorded in `planning/INTERFACE-GAPS.md` rather than worked around.

use vaco_codec_core::{Accept, Caps, CodecId, Machine, SendReceive};
use vaco_core::{MediaType, Result};
use vaco_frame::{Frame, FrameData, SubtitleRect};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::SubtitleEvent;

/// Convert one decoded event into a `Frame`, copying `packet`'s timing.
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
    frame.duration = packet.duration;
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

/// The palette a registered `vobsub` decoder paints with when nothing has
/// supplied the real one.
///
/// **This is a fallback, not the format's palette**, and it is visible here
/// rather than buried because the difference is user-visible colour. A DVD
/// subpicture's four pseudo-colours are indices into a 16-entry table that
/// lives *outside* the SPU bytes — in the `.idx` sidecar, or in a Matroska
/// `S_VOBSUB` track's `CodecPrivate`. The `Decoder` trait has no channel for
/// it: `set_extradata` is on `Parser`, not `Decoder`, and `DecoderDesc::make`
/// takes only `Limits`. So a decoder reached through the registry has no way
/// to be told, and geometry and pixel indices come out right while colours
/// come out of this table instead of the disc's.
///
/// A caller that *does* have the real palette should bypass this wrapper and
/// call [`crate::vobsub::decode_spu`] directly, which takes it as a
/// parameter. Recorded in `planning/INTERFACE-GAPS.md`.
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
/// See [`fallback_palette`] for the one thing this wrapper cannot do that
/// [`crate::vobsub::decode_spu`] can.
#[derive(Debug)]
pub struct VobSubSubtitleDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl VobSubSubtitleDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
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
                let event =
                    crate::vobsub::decode_spu(pkt.payload(), &fallback_palette(), &self.limits)?;
                let mut budget = Budget::new(self.limits.clone());
                self.machine.emit(frame_of_event(&event, pkt, &mut budget)?);
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
        let expected =
            crate::dvb::decode_display_set(&bytes, &Limits::permissive()).unwrap();

        let mut dec = make_dvb(Limits::permissive());
        dec.send_packet(Some(&packet(&bytes, 4242))).unwrap();
        let frame = dec.receive_frame().unwrap();

        assert_eq!(frame.pts, Timestamp::new(4242));
        assert_eq!(frame.duration, vaco_core::Duration::from_micros(2_000_000));
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
}
