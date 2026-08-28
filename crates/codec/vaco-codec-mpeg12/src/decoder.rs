//! The `Decoder` trait implementation: one MPEG-1/2 access unit (packet) in,
//! zero or more decoded [`Frame`]s out.
//!
//! # Scope
//!
//! Frame pictures only. Both `frame_pred_frame_dct == 1` (progressive-style:
//! every macroblock frame-DCT, frame-predicted) and `== 0` (interlaced
//! frame pictures with per-macroblock field/frame DCT and field/frame
//! prediction, e.g. `ffmpeg -flags +ilme+ildct`) are implemented, but the
//! former is what this crate's differential harness spent most of its time
//! against — see `docs/codec/vaco-codec-mpeg12.md` for measured accuracy on
//! each. Separate field pictures (`picture_structure != Frame`), dual-prime
//! and 16x8 MC are not implemented; a picture that uses
//! one of them is decoded as a flat mid-grey `CORRUPT` frame rather than
//! silently producing wrong pixels, and counted in
//! [`Mpeg12Decoder::unsupported_pictures`].
//!
//! # Reference management and B-picture reordering
//!
//! Decode order and display order differ whenever B-pictures are present:
//! a B-picture's two references are always already decoded (that is what
//! makes it a B-picture), so the bitstream carries every reference picture
//! *before* the B-pictures that need it, which is *after* those
//! B-pictures' own display position. The fix, used here: hold the
//! most-recently-decoded reference picture (`held`) instead of emitting it
//! immediately; emit it only once the *next* reference picture is decoded,
//! by which point every B-picture between them has already been decoded
//! and emitted. `previous`/`recent` are the forward/backward references a
//! B-picture reads; `recent` alone is what a P-picture reads.

use vaco_bitstream::{BitReader, annexb};
use vaco_codec_core::{Caps, Decoder};
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::block::Mpeg2Idct;
use crate::headers::{self, PictureCodingExtension, PictureHeader, SequenceExtension, SequenceHeader};
use crate::macroblock::{self, ActivePicture};
use crate::motion::MotionPredictor;
use crate::picture::RefPicture;
use crate::tables;

const SEQUENCE_HEADER: u8 = 0xB3;
const EXTENSION_START: u8 = 0xB5;
const GROUP_START: u8 = 0xB8;
const PICTURE_START: u8 = 0x00;
const SEQUENCE_END: u8 = 0xB7;

const EXT_SEQUENCE: u32 = 1;
const EXT_QUANT_MATRIX: u32 = 3;
const EXT_PICTURE_CODING: u32 = 8;

/// Persistent sequence-level state, valid until the next
/// `sequence_header()`.
#[derive(Debug, Clone)]
pub(crate) struct Sequence {
    pub header: SequenceHeader,
    pub ext: Option<SequenceExtension>,
    pub mb_width: u32,
    pub mb_height: u32,
}

/// MPEG-1/2 video decoder. See the module docs.
#[derive(Debug)]
pub struct Mpeg12Decoder {
    machine: vaco_codec_core::Machine<Frame>,
    budget: Budget,
    seq: Option<Sequence>,
    idct: Mpeg2Idct,
    previous: Option<RefPicture>,
    recent: Option<RefPicture>,
    held: Option<Frame>,
    current: Option<ActivePicture>,
    unsupported_pictures: u64,
}

impl Mpeg12Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            // `Caps::SUBFRAMES`, not just `Caps::DELAY`: `decode_access_unit`
            // walks every start code in whatever byte range `send_packet`
            // hands it, and a packet is not guaranteed to hold exactly one
            // picture (a generic elementary-stream demuxer, or simply
            // adversarial/fuzzed input, may bundle several) — when it does,
            // `finish_picture` runs more than once and can genuinely emit
            // more than one frame for that one `send_packet` call. Found by
            // `fuzz/fuzz_targets/mpeg12_decode.rs` tripping `Machine::emit`'s
            // "more than one output for one input without Caps::SUBFRAMES"
            // debug_assert on a multi-picture single packet; this crate's
            // own differential-test harness never exercises more than one
            // picture per packet, so this only widens what is *tolerated*.
            machine: vaco_codec_core::Machine::new(Caps::DELAY.union(Caps::SUBFRAMES)),
            budget: Budget::new(limits),
            seq: None,
            idct: new_idct(),
            previous: None,
            recent: None,
            held: None,
            current: None,
            unsupported_pictures: 0,
        }
    }

    /// Pictures whose coding mode this crate does not implement (field
    /// pictures, dual-prime) and therefore emitted as a flat placeholder
    /// rather than decoding wrongly. See the module docs.
    #[must_use]
    pub const fn unsupported_pictures(&self) -> u64 {
        self.unsupported_pictures
    }

    fn begin_picture(&mut self, hdr: PictureHeader, pce: PictureCodingExtension, pts: vaco_core::Timestamp, duration: vaco_core::Duration) -> Result<()> {
        let Some(seq) = self.seq.clone() else {
            return Err(Error::InvalidData("mpeg12: picture before any sequence_header"));
        };
        let width = seq.header.width.max(1);
        let height = seq.header.height.max(1);
        let mut frame = Frame::alloc_video(&mut self.budget, PixFmt::Yuv420p, width, height)?;
        frame.pts = pts;
        frame.duration = duration;
        if hdr.coding_type == headers::PictureType::I {
            frame.flags |= FrameFlags::KEY;
        }

        let supported = pce.is_frame_picture() && hdr.coding_type != headers::PictureType::D;
        if !supported {
            self.unsupported_pictures = self.unsupported_pictures.saturating_add(1);
            fill_neutral(&mut frame);
            frame.flags |= FrameFlags::CORRUPT;
        }

        self.current = Some(ActivePicture {
            frame,
            header: hdr,
            pce,
            intra_matrix: seq.header.intra_matrix,
            non_intra_matrix: seq.header.non_intra_matrix,
            quantiser_scale: 1,
            dc_pred: [tables::intra_dc_reset(pce.intra_dc_precision); 3],
            fwd_pred: MotionPredictor::default(),
            bwd_pred: MotionPredictor::default(),
            prev_mb_forward: false,
            prev_mb_backward: false,
            supported,
            previous: self.previous.clone(),
            recent: self.recent.clone(),
            mpeg1: seq.ext.is_none(),
        });
        Ok(())
    }

    fn finish_picture(&mut self) {
        let Some(ap) = self.current.take() else {
            return;
        };
        let is_reference = matches!(
            ap.header.coding_type,
            headers::PictureType::I | headers::PictureType::P
        );
        let frame = ap.frame;
        if is_reference {
            if let Some(held) = self.held.take() {
                self.machine.emit(held);
            }
            self.previous = self.recent.take();
            self.recent = Some(RefPicture::new(frame.clone()));
            self.held = Some(frame);
        } else {
            self.machine.emit(frame);
        }
    }

    fn decode_access_unit(&mut self, data: &[u8], pts: vaco_core::Timestamp, duration: vaco_core::Duration) -> Result<()> {
        let mut pos = 0usize;
        let mut pending_picture: Option<(PictureHeader, Option<PictureCodingExtension>)> = None;
        while let Some(sc) = annexb::find_start_code(data, pos) {
            let Some(&code) = data.get(sc + 3) else {
                break;
            };
            let body_start = sc + 4;
            let body = data.get(body_start..).unwrap_or(&[]);
            match code {
                SEQUENCE_HEADER => {
                    let header = headers::sequence_header(body);
                    let mb_width = header.width.div_ceil(16).max(1);
                    let mb_height = header.height.div_ceil(16).max(1);
                    self.seq = Some(Sequence {
                        header,
                        ext: None,
                        mb_width,
                        mb_height,
                    });
                }
                EXTENSION_START => {
                    let mut r = BitReader::new(body);
                    let id = r.get(4);
                    if id == EXT_SEQUENCE
                        && let Some(seq) = self.seq.as_mut()
                    {
                        let (ext, h_ext, v_ext) = headers::sequence_extension(&mut r);
                        seq.header.width |= h_ext << 12;
                        seq.header.height |= v_ext << 12;
                        seq.mb_width = seq.header.width.div_ceil(16).max(1);
                        seq.mb_height = seq.header.height.div_ceil(16).max(1);
                        seq.ext = Some(ext);
                    } else if id == EXT_PICTURE_CODING {
                        let pce = headers::picture_coding_extension(&mut r);
                        if let Some((_, existing)) = pending_picture.as_mut() {
                            *existing = Some(pce);
                        }
                    } else if id == EXT_QUANT_MATRIX
                        && let Some(seq) = self.seq.as_mut()
                    {
                        headers::quant_matrix_extension(
                            &mut r,
                            &mut seq.header.intra_matrix,
                            &mut seq.header.non_intra_matrix,
                        );
                    }
                }
                GROUP_START | SEQUENCE_END => {}
                PICTURE_START => {
                    // A new picture_header means the previous picture's
                    // slice data has ended. A well-formed access unit
                    // (exactly one picture) never reaches this with
                    // `self.current` still `Some`, but a caller that hands
                    // over more than one picture per `send_packet` call —
                    // this crate's own differential-test harness does,
                    // deliberately, to avoid depending on another crate's
                    // packetiser for a test fixture — needs the boundary
                    // enforced here rather than silently overwriting
                    // `self.current` and losing the picture in progress.
                    self.finish_picture();
                    pending_picture = headers::picture_header(body).map(|h| (h, None));
                }
                0x01..=0xAF => {
                    let next = annexb::find_start_code(data, body_start).unwrap_or(data.len());
                    let slice_data = data.get(body_start..next).unwrap_or(&[]);
                    if let Some((hdr, pce_opt)) = pending_picture.take() {
                        let pce = pce_opt.unwrap_or_else(|| {
                            PictureCodingExtension::mpeg1_default(hdr.forward_f_code, hdr.backward_f_code)
                        });
                        self.begin_picture(hdr, pce, pts, duration)?;
                    }
                    let seq = self.seq.clone();
                    if let (Some(ap), Some(seq)) = (self.current.as_mut(), seq) {
                        macroblock::decode_slice(code, slice_data, &mut self.idct, ap, &seq);
                    }
                    pos = next;
                    continue;
                }
                _ => {}
            }
            pos = body_start;
        }
        self.finish_picture();
        Ok(())
    }
}

fn new_idct() -> Mpeg2Idct {
    // A fixed, non-zero transform length (N=8); `Idct8x8::new` only fails
    // for a zero-length transform, so this is unreachable in practice —
    // but "unreachable" is not "infallible", so this retries rather than
    // unwrapping, and only panics (via the one documented `expect`) if the
    // underlying transform library itself is broken, which no input to
    // this decoder can trigger.
    match vaco_codec_dsp_idct::mpeg2::idct8x8_f32() {
        Ok(idct) => idct,
        Err(_) => {
            #[allow(
                clippy::expect_used,
                reason = "genuinely unreachable: a length-8 DCT-III plan cannot fail to build"
            )]
            vaco_codec_dsp_idct::mpeg2::idct8x8_f32().expect("length-8 IDCT construction cannot fail")
        }
    }
}

/// Fill every plane with mid-grey (Y=128, Cb=Cr=128): the flat, colour-
/// neutral placeholder for a picture this crate cannot decode (field
/// pictures, dual-prime — see the module docs), so a caller sees an
/// obviously-wrong-looking but harmless frame rather than uninitialised
/// zeros (which would render as solid green in 4:2:0).
fn fill_neutral(frame: &mut Frame) {
    for plane_idx in 0..3 {
        if let Some(mut plane) = frame.plane_mut(plane_idx) {
            for row in plane.rows_mut() {
                row.fill(128);
            }
        }
    }
}

impl Decoder for Mpeg12Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let accept = self.machine.accept(packet.is_none())?;
        if matches!(accept, vaco_codec_core::Accept::Drain) {
            if let Some(held) = self.held.take() {
                self.machine.emit(held);
            }
            self.machine.finish();
            return Ok(());
        }
        let Some(packet) = packet else {
            return Ok(());
        };
        self.decode_access_unit(packet.payload(), packet.pts, packet.duration)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.previous = None;
        self.recent = None;
        self.held = None;
        self.current = None;
        self.seq = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_reports_need_more_input_before_any_packet() {
        let mut dec = Mpeg12Decoder::new(Limits::strict());
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn flush_resets_reference_state() {
        let mut dec = Mpeg12Decoder::new(Limits::strict());
        let Ok(frame) = Frame::alloc_video(&mut Budget::new(Limits::strict()), PixFmt::Yuv420p, 16, 16)
        else {
            return;
        };
        dec.recent = Some(RefPicture::new(frame));
        dec.flush();
        assert!(dec.recent.is_none());
    }
}
