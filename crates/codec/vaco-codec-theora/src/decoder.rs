//! `Decoder` implementation: header setup via `set_extradata`, then one
//! reconstructed picture per keyframe packet.

use vaco_codec_core::Decoder;
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::blocks::FrameGeom;
use crate::frame::{self, crop_plane};
use crate::ident::{Ident, PixelFormat};
use crate::setup::{self, COMMENT_MAGIC, IDENT_MAGIC, SETUP_MAGIC, Setup};

/// Unpack the Xiph-laced `extradata` blob a container hands a Theora
/// stream: a packet count minus one, each packet's length lace-encoded
/// except the last (whose length is simply what remains), then every
/// packet's raw bytes concatenated. Identical in shape to `vaco-codec-vorbis`'s
/// copy of the same routine (D14.1 keeps a codec crate from depending on the
/// container crate that produces this layout to reuse its copy).
fn split_xiph_headers(data: &[u8]) -> Option<Vec<&[u8]>> {
    let (&count_minus_one, mut cursor) = data.split_first()?;
    let count = usize::from(count_minus_one).saturating_add(1);
    let mut lens = Vec::new();
    for _ in 0..count.saturating_sub(1) {
        let mut len = 0usize;
        loop {
            let (&b, rest) = cursor.split_first()?;
            cursor = rest;
            len = len.saturating_add(usize::from(b));
            if b != 255 {
                break;
            }
        }
        lens.push(len);
    }
    let mut headers = Vec::new();
    for len in lens {
        if cursor.len() < len {
            return None;
        }
        let (head, rest) = cursor.split_at(len);
        headers.push(head);
        cursor = rest;
    }
    headers.push(cursor);
    Some(headers)
}

fn pix_fmt_for(pf: PixelFormat) -> PixFmt {
    match pf {
        PixelFormat::Yuv420 => PixFmt::Yuv420p,
        PixelFormat::Yuv422 => PixFmt::Yuv422p,
        PixelFormat::Yuv444 => PixFmt::Yuv444p,
    }
}

#[derive(Debug)]
pub struct TheoraDecoder {
    limits: Limits,
    ident: Option<Ident>,
    setup: Option<Setup>,
    geom: Option<FrameGeom>,
    pending: Option<Frame>,
    /// Set by `send_packet(None)`; makes `receive_frame` answer `Eof`
    /// once `pending` is empty instead of `NeedMoreInput` forever -- see
    /// `vaco-codec-ac3`'s decoder's own `draining` field doc for the full
    /// reasoning (measured against `vaco-sched`'s `ProgressGuard`
    /// watchdog, same contract violation as `vaco-codec-alac`'s and
    /// `vaco-codec-vorbis`'s decoders).
    draining: bool,
}

impl TheoraDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            ident: None,
            setup: None,
            geom: None,
            pending: None,
            draining: false,
        }
    }

    #[allow(
        clippy::integer_division,
        reason = "chroma crop offset is a floor division by the (1 or 2) subsampling factor, not a rounding shortcut"
    )]
    fn decode_video_packet(&mut self, payload: &[u8], pts: vaco_core::Timestamp) -> Result<()> {
        let Some(ident) = self.ident else {
            return Err(Error::InvalidData("theora: video packet before headers"));
        };
        let Some(setup) = self.setup.as_ref() else {
            return Err(Error::InvalidData("theora: video packet before headers"));
        };
        let Some(geom) = self.geom.as_ref() else {
            return Err(Error::InvalidData("theora: video packet before headers"));
        };

        let mut budget = Budget::new(self.limits.clone());
        let decoded = frame::decode_frame_payload(payload, &ident, setup, geom, &mut budget)?;

        let pf = pix_fmt_for(ident.pf);
        let mut out = Frame::alloc_video(&mut budget, pf, ident.picw, ident.pich)?;
        let vaco_frame::FrameData::Video { planes, .. } = &mut out.data else {
            return Err(Error::InvalidData("theora: allocated frame has no planes"));
        };

        let (cbw, cbh) = ident.pf.chroma_blocks(ident.fmbw, ident.fmbh);
        let full_dims = [
            (ident.fmbw.saturating_mul(16), ident.fmbh.saturating_mul(16)),
            (cbw.saturating_mul(8), cbh.saturating_mul(8)),
            (cbw.saturating_mul(8), cbh.saturating_mul(8)),
        ];
        let (picx, picy) = (ident.picx, ident.picy);
        for (pli, plane) in decoded.planes.iter().enumerate() {
            let Some((full_w, full_h)) = full_dims.get(pli).copied() else {
                continue;
            };
            let is_luma = pli == 0;
            let (crop_x, crop_y, crop_w, crop_h) = if is_luma {
                (picx, picy, ident.picw, ident.pich)
            } else {
                // Proportional chroma crop; see `frame::crop_plane`'s doc for
                // the odd-offset/odd-size cases this does not model exactly.
                let (sx, sy) = ident.pf.chroma_subsample();
                (
                    picx / sx,
                    picy / sy,
                    ident.picw.div_ceil(sx),
                    ident.pich.div_ceil(sy),
                )
            };
            let cropped = crop_plane(plane, full_w, full_h, crop_x, crop_y, crop_w, crop_h);
            if let Some(dst) = planes.get_mut(pli) {
                let stride = dst.stride;
                let rows = dst.rows();
                let buf = dst.data.make_mut();
                for row in 0..rows {
                    let Some(dst_row) = buf.get_mut(row.saturating_mul(stride)..) else {
                        continue;
                    };
                    let w = crop_w as usize;
                    let src_start = row.saturating_mul(w);
                    let Some(src_row) = cropped.get(src_start..src_start.saturating_add(w)) else {
                        continue;
                    };
                    let n = dst_row.len().min(src_row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        out.pts = pts;
        // This decoder discarded the identification header's own declared
        // frame rate (`frn`/`frd`, section 6.2 steps 11-12) on parse
        // (`Ident::parse` bound them to `_frn`/`_frd` and never stored
        // them), so no decoded frame ever carried a `pts` *or* a
        // `duration` -- the exact bug class this session's audit found
        // and fixed across every audio decoder in the tree
        // (`vaco-codec-pcm`/`-adpcm`/`-simple-audio`/`-vorbis`/`-ac3`/
        // `-aac`/`-mpegaudio`/`-opus`), here on the one remaining video
        // decoder that had it too. `frd == 0` is invalid per the spec
        // (section 6.2 step 12) but guarded rather than trusted.
        let time_base = Rational::new(
            i32::try_from(ident.frd.max(1)).unwrap_or(1),
            i32::try_from(ident.frn.max(1)).unwrap_or(1),
        );
        out.duration = Timestamp::new(1)
            .to_duration(time_base)
            .unwrap_or(Duration::ZERO);
        self.pending = Some(out);
        Ok(())
    }
}

impl Decoder for TheoraDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            self.draining = true;
            return Ok(());
        };
        self.decode_video_packet(packet.payload(), packet.pts)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.take().ok_or(if self.draining {
            Error::Eof
        } else {
            Error::NeedMoreInput
        })
    }

    fn flush(&mut self) {
        self.pending = None;
        self.draining = false;
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        let headers = split_xiph_headers(extradata).ok_or(Error::InvalidData(
            "theora: extradata is not Xiph-laced Theora headers",
        ))?;
        let ident_packet = *headers.first().ok_or(Error::InvalidData(
            "theora: extradata missing identification header",
        ))?;
        let setup_packet = *headers
            .get(2)
            .ok_or(Error::InvalidData("theora: extradata missing setup header"))?;

        if setup::common_header_type(ident_packet) != Some(IDENT_MAGIC) {
            return Err(Error::InvalidData(
                "theora: identification header magic mismatch",
            ));
        }
        let ident_body = ident_packet.get(7..).unwrap_or(&[]);
        let ident = Ident::parse(ident_body)?;

        // The comment header (index 1) carries nothing frame decode needs;
        // only its own magic is worth checking, and even that is skippable —
        // a decoder does not need comments to be well-formed to decode
        // pictures.
        if let Some(&comment_packet) = headers.get(1)
            && setup::common_header_type(comment_packet) != Some(COMMENT_MAGIC)
        {
            return Err(Error::InvalidData("theora: comment header magic mismatch"));
        }

        if setup::common_header_type(setup_packet) != Some(SETUP_MAGIC) {
            return Err(Error::InvalidData("theora: setup header magic mismatch"));
        }
        let setup_body = setup_packet.get(7..).unwrap_or(&[]);
        let setup = Setup::parse(setup_body)?;

        let mut budget = Budget::new(self.limits.clone());
        // Bound the coded frame's implied memory before any per-block table
        // is built from it — `FrameGeom::build` below allocates several
        // vectors sized from `fmbw`/`fmbh`, both attacker-controlled 16-bit
        // fields. Theora is always 8-bit 4:2:0/4:2:2/4:4:4, never packed
        // RGBA, so charge the real format's average bytes per pixel (12/16/24
        // bits respectively) rather than a flat 4 — which over-charges every
        // one of those cases and can false-reject a legitimately large frame.
        let bpp = u32::from(pix_fmt_for(ident.pf).bits_per_pixel())
            .div_ceil(8)
            .max(1);
        budget.check_frame(
            ident.fmbw.saturating_mul(16),
            ident.fmbh.saturating_mul(16),
            bpp,
        )?;
        let geom = FrameGeom::build(ident.fmbw, ident.fmbh, ident.pf, &mut budget)?;

        self.ident = Some(ident);
        self.setup = Some(setup);
        self.geom = Some(geom);
        self.pending = None;
        Ok(())
    }
}

fn make(limits: Limits) -> Box<dyn Decoder> {
    Box::new(TheoraDecoder::new(limits))
}

/// The registry descriptor for Theora decode.
pub const DECODER_THEORA: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "theora",
    long_name: "Theora",
    id: vaco_codec_core::CodecId::Theora,
    media_type: MediaType::Video,
    caps: vaco_codec_core::Caps::empty(),
    supported_rates: &[],
    make,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn split_xiph_headers_matches_the_measured_shape() {
        let headers = vec![vec![1u8, 2, 3], vec![4u8, 5], vec![6u8; 300]];
        let mut packed = vec![2u8];
        packed.push(3);
        packed.push(2);
        for h in &headers {
            packed.extend_from_slice(h);
        }
        let split = split_xiph_headers(&packed).unwrap();
        assert_eq!(split.len(), 3);
        assert_eq!(split[0], &headers[0][..]);
        assert_eq!(split[1], &headers[1][..]);
        assert_eq!(split[2], &headers[2][..]);
    }

    #[test]
    fn garbage_extradata_is_a_decode_error_not_a_panic() {
        let mut dec = TheoraDecoder::new(Limits::permissive());
        assert!(dec.set_extradata(&[0xFF; 4]).is_err());
        assert!(dec.set_extradata(&[]).is_err());
    }

    #[test]
    fn video_packet_before_headers_is_a_clean_error() {
        let mut dec = TheoraDecoder::new(Limits::permissive());
        assert!(dec.decode_video_packet(&[0], Timestamp::NONE).is_err());
    }

    /// A legitimately large 4:4:4 frame must fit `Limits::strict`'s frame
    /// budget, not just `Limits::permissive`'s.
    ///
    /// Regression: `set_extradata`'s coded-size budget check used to charge
    /// a flat 4 bytes per pixel. Theora is always 8-bit, so even its widest
    /// format (4:4:4, `yuv444p`, 24 bits/pixel) needs only 3 bytes per pixel
    /// — the old flat 4 over-charged every Theora frame, worst for 4:2:0
    /// (12 bits, a 2.67x overshoot) but still wrong at 4:4:4. At 2732x1536
    /// the flat-4 overshoot (16.79 MB) crosses `Limits::strict`'s 16 MiB
    /// `max_frame_bytes` cap even though the real 4:4:4 frame is only
    /// 12.6 MB.
    #[test]
    fn a_legitimately_large_4_4_4_frame_is_accepted_by_the_frame_budget() {
        let pix_fmt = pix_fmt_for(PixelFormat::Yuv444);
        assert_eq!(pix_fmt, PixFmt::Yuv444p);
        let bpp = u32::from(pix_fmt.bits_per_pixel()).div_ceil(8).max(1);
        assert_eq!(bpp, 3, "yuv444p averages 24 bits/pixel, not 32");

        let budget = Budget::new(Limits::strict());
        assert!(
            budget.check_frame(2732, 1536, bpp).is_ok(),
            "a real 4:4:4 8-bit frame this size must fit `strict`'s frame budget"
        );
    }

    /// `send_packet(None)` must make `receive_frame` answer `Eof` once
    /// `pending` is drained, not `NeedMoreInput` forever -- see
    /// `vaco-codec-ac3`'s decoder's own `draining` field doc for the full
    /// reasoning (measured against `vaco-sched`'s `ProgressGuard` livelock
    /// watchdog).
    #[test]
    fn draining_answers_eof_once_empty_not_need_more_input_forever() {
        let mut dec = TheoraDecoder::new(Limits::permissive());
        assert!(
            matches!(dec.receive_frame(), Err(Error::NeedMoreInput)),
            "empty and not draining yet"
        );
        dec.send_packet(None).unwrap();
        assert!(
            matches!(dec.receive_frame(), Err(Error::Eof)),
            "must answer Eof once drained and empty, not NeedMoreInput forever"
        );
    }
}
