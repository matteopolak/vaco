//! VP8 uncompressed frame tag and key-frame header, RFC 6386 §9.1-9.2.
//!
//! # No byte-stream framing question here — measured, not assumed
//!
//! Unlike H.264 Annex B, there is no VP8 elementary-stream demuxer anywhere
//! in this workspace (no `vaco-demux-raw` `BitstreamSpec` names it, and
//! there is no IVF demuxer either), so [`Vp8Parser`] never has to find a
//! frame boundary inside a longer buffer: every container that carries VP8
//! (`WebM`'s `V_VP8`, `AVI`'s `VP80` fourcc, MP4's `vp08`) already delimits one
//! sample as one frame before this crate ever sees it. `vaco-parse-opus`
//! documents the identical contract for the identical reason — Opus has no
//! byte-stream demuxer either — and [`Vp8Parser::parse`] follows it: **one
//! `parse` call, one already-framed input, the whole slice consumed.**
//!
//! # What is read, and what is not
//!
//! The 3-byte frame tag (`key_frame`, `version`, `show_frame`,
//! `first_part_size`) and, on a key frame, the 3-byte start code and the two
//! 16-bit (14 bits + 2-bit scale) dimension fields. Nothing past that: the
//! colour-space and clamping-type bits that follow live inside the
//! boolean-coded first partition, and RFC 6386 §9.2 fixes VP8 to one
//! colour format (4:2:0, studio or full range signalled by a bit this crate
//! does not need) — measured, `ffprobe -show_entries
//! stream=pix_fmt,color_range` on `libvpx` output is always `yuv420p`
//! whatever `-color_range` is asked for, so there is no second pixel format
//! this crate would ever need to name from those bits.

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters, Parser};
use vaco_core::{MediaType, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// RFC 6386 §9.1's `start_code`: `0x9d 0x01 0x2a`, present at the start of
/// every key frame's payload (right after the 3-byte frame tag).
const KEY_FRAME_START_CODE: [u8; 3] = [0x9d, 0x01, 0x2a];

/// What the 3-byte frame tag and, for a key frame, the dimension fields say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTag {
    pub key_frame: bool,
    /// 0..=3; selects the reconstruction and loop filters. Printed by the
    /// reference as the bare `profile` number — measured, `ffprobe
    /// -show_entries stream=profile` on `-bsf vp8` output with no filter
    /// applied prints `profile=0` for ordinary `libvpx` output and there is
    /// no descriptive name to look up, unlike VP9.
    pub version: u8,
    pub show_frame: bool,
    /// Size of the first (boolean-coded) partition, in bytes.
    pub first_part_size: u32,
    /// `Some` only on a key frame, where the dimensions are coded.
    pub size: Option<(u16, u16)>,
}

/// Parse the frame tag, and the key-frame header that follows it.
///
/// Returns `None` for a slice too short to hold the 3-byte tag, or a key
/// frame whose start code does not match RFC 6386 §9.1 — both are "this is
/// not a VP8 frame", not a partial read to retry, because the caller already
/// promises a whole frame per [`Vp8Parser::parse`]'s contract.
#[must_use]
pub fn parse_frame_tag(data: &[u8]) -> Option<FrameTag> {
    let mut r = ByteReader::new(data);
    let raw = r.le24();
    if r.overrun() {
        return None;
    }
    // RFC 6386 §9.1: LSB first. bit 0 is inverted (0 = key frame).
    let key_frame = raw & 1 == 0;
    let version = ((raw >> 1) & 0x7) as u8;
    let show_frame = (raw >> 4) & 1 != 0;
    let first_part_size = (raw >> 5) & 0x7_ffff;

    let size = if key_frame {
        let mut kr = ByteReader::new(r.rest());
        let start = kr.bytes(3);
        if kr.overrun() || start != KEY_FRAME_START_CODE {
            return None;
        }
        let w = kr.le16();
        let h = kr.le16();
        if kr.overrun() {
            return None;
        }
        // Low 14 bits are the dimension; the top 2 bits are a display scale
        // this crate has no field for (ffprobe does not report it either).
        Some((w & 0x3fff, h & 0x3fff))
    } else {
        None
    };

    Some(FrameTag {
        key_frame,
        version,
        show_frame,
        first_part_size,
        size,
    })
}

/// The [`CodecParameters`] a [`FrameTag`] implies.
#[must_use]
pub fn codec_parameters(tag: &FrameTag) -> CodecParameters {
    let mut params = CodecParameters::video().with_codec(CodecId::Vp8);
    // No descriptive name exists (see the module doc), so the empty name
    // falls back to the raw number the same way an unnamed H.264 profile_idc
    // does — see `vaco_codec_core::Profile`'s own convention.
    params.profile = Some(vaco_codec_core::Profile::new(i32::from(tag.version), ""));
    if let (Some(v), Some((w, h))) = (params.video.as_mut(), tag.size) {
        v.width = u32::from(w);
        v.height = u32::from(h);
        v.coded_width = u32::from(w);
        v.coded_height = u32::from(h);
        // RFC 6386 fixes the sample format: 4:2:0, no other chroma layout
        // exists. Measured, see the module doc.
        v.format = vaco_pixfmt::PixFmt::from_name("yuv420p").ok();
    }
    params
}

/// A VP8 parser: reads the frame tag and, on a key frame, the dimensions.
/// **It decodes nothing.**
#[derive(Debug)]
pub struct Vp8Parser {
    budget: Budget,
    params: Option<CodecParameters>,
}

impl Vp8Parser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits),
            params: None,
        }
    }
}

impl Parser for Vp8Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            // See the module doc: a frame is whole or it is not a frame, so
            // there is never a partial one left to flush at end of stream.
            return Ok((None, 0));
        }
        let mut packet = Packet::from_slice(&mut self.budget, input)?;
        if let Some(tag) = parse_frame_tag(input) {
            let mut found = codec_parameters(&tag);
            if let Some(existing) = &mut self.params {
                existing.fill_from(&found);
            } else {
                found.media_type = Some(MediaType::Video);
                self.params = Some(found);
            }
            packet.flags = if tag.key_frame {
                PacketFlags::KEY
            } else {
                PacketFlags::empty()
            };
        }
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// `true`: this crate's own module doc already states the contract —
    /// no container carrying VP8 in this workspace ever splits one frame
    /// across more than one packet, so [`Vp8Parser::parse`] never needs
    /// `ParserDriver`'s reassembly buffer at all, whatever the frame's own
    /// size. See `vaco_codec_core::Parser::whole_sample_only`'s own doc.
    fn whole_sample_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// A real `libvpx` key frame tag + start code + 176x144, captured from a
    /// `testsrc` encode (`ffmpeg -c:v libvpx -f ivf`).
    const KEY_FRAME: [u8; 10] = [
        0x70, 0x43, 0x00, // tag: key_frame=0(=key), version=0, show=1
        0x9d, 0x01, 0x2a, // start code
        0xb0, 0x00, // width  176 (0x00b0) | scale 00
        0x90, 0x00, // height 144 (0x0090) | scale 00
    ];

    #[test]
    fn a_real_key_frame_tag_decodes() {
        let tag = parse_frame_tag(&KEY_FRAME).unwrap();
        assert!(tag.key_frame);
        assert_eq!(tag.version, 0);
        assert!(tag.show_frame);
        assert_eq!(tag.size, Some((176, 144)));
    }

    #[test]
    fn an_inter_frame_tag_has_no_size() {
        // key_frame bit set (=1, inter), version 0, show 1, arbitrary size.
        let tag = parse_frame_tag(&[0x71, 0x00, 0x00]).unwrap();
        assert!(!tag.key_frame);
        assert_eq!(tag.size, None);
    }

    #[test]
    fn a_bad_start_code_is_rejected() {
        let mut bad = KEY_FRAME;
        bad[3] = 0x00;
        assert!(parse_frame_tag(&bad).is_none());
    }

    #[test]
    fn a_truncated_tag_is_rejected_not_panicked() {
        assert!(parse_frame_tag(&[]).is_none());
        assert!(parse_frame_tag(&[0x70, 0x43]).is_none());
        assert!(parse_frame_tag(&KEY_FRAME[..5]).is_none());
    }

    #[test]
    fn the_parser_reports_a_key_frame_and_its_dimensions() {
        let mut p = Vp8Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&KEY_FRAME).unwrap();
        let pkt = pkt.unwrap();
        assert_eq!(used, KEY_FRAME.len());
        assert!(pkt.flags.contains(PacketFlags::KEY));
        let v = p.parameters().unwrap().video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (176, 144));
        assert_eq!(v.format, vaco_pixfmt::PixFmt::from_name("yuv420p").ok());
    }

    #[test]
    fn end_of_stream_flushes_nothing() {
        let mut p = Vp8Parser::new(Limits::strict());
        let (pkt, used) = p.parse(&[]).unwrap();
        assert!(pkt.is_none());
        assert_eq!(used, 0);
    }
}
