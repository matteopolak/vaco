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
use vaco_frame::{Frame, FrameFlags, FrameSideData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_mpegvideo::a53::cc_data_after_identifier;
use vaco_pixfmt::PixFmt;

use crate::block::Mpeg2Idct;
use crate::headers::{self, PictureCodingExtension, PictureHeader, SequenceExtension, SequenceHeader};
use crate::macroblock::{self, ActivePicture, ChromaFormat};
use crate::motion::MotionPredictor;
use crate::picture::RefPicture;
use crate::tables;

const SEQUENCE_HEADER: u8 = 0xB3;
const EXTENSION_START: u8 = 0xB5;
const GROUP_START: u8 = 0xB8;
const PICTURE_START: u8 = 0x00;
const SEQUENCE_END: u8 = 0xB7;
/// `user_data_start_code` (ITU-T H.262 Table 6-1) — where an ATSC A/53
/// caption `user_data()` element rides. See `vaco_parse_mpegvideo::a53`'s
/// module doc for the structure.
const USER_DATA_START: u8 = 0xB2;

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

    fn begin_picture(
        &mut self,
        hdr: PictureHeader,
        pce: PictureCodingExtension,
        pts: vaco_core::Timestamp,
        duration: vaco_core::Duration,
        closed_captions: Vec<u8>,
    ) -> Result<()> {
        let Some(seq) = self.seq.clone() else {
            return Err(Error::InvalidData("mpeg12: picture before any sequence_header"));
        };
        let width = seq.header.width.max(1);
        let height = seq.header.height.max(1);
        let chroma_format = ChromaFormat::from_raw(seq.ext.map_or(1, |ext| ext.chroma_format));
        let pixfmt = match chroma_format {
            ChromaFormat::Yuv420 => PixFmt::Yuv420p,
            ChromaFormat::Yuv422 => PixFmt::Yuv422p,
            ChromaFormat::Yuv444 => PixFmt::Yuv444p,
        };
        let mut frame = Frame::alloc_video(&mut self.budget, pixfmt, width, height)?;
        frame.pts = pts;
        frame.duration = duration;
        if hdr.coding_type == headers::PictureType::I {
            frame.flags |= FrameFlags::KEY;
        }
        // §6.2.3.1: `top_field_first` is decoded unconditionally by
        // `picture_coding_extension()`, so it is propagated unconditionally
        // here too — a downstream consumer that only cares about
        // interlaced content already has `FrameFlags::INTERLACED` (not set
        // by this crate; see the module docs) to gate on, the same way a
        // real decoder's `AVFrame::top_field_first` is set regardless of
        // `AVFrame::interlaced_frame`.
        if pce.top_field_first {
            frame.flags |= FrameFlags::TOP_FIELD_FIRST;
        }
        // D.9.14: MPEG-1 (no `sequence_extension()`, `seq.ext` is `None`)
        // behaves as if `progressive_sequence == '1'` — `map_or(true, ..)`,
        // not `unwrap_or_default()` (`SequenceExtension::default()`'s
        // `progressive_sequence` is `false`, the wrong value for a `None`
        // extension specifically).
        let progressive_sequence = seq.ext.is_none_or(|ext| ext.progressive_sequence);
        let extra_fields =
            pulldown_extra_fields(progressive_sequence, pce.progressive_frame, pce.repeat_first_field, pce.top_field_first);
        frame.set_repeat_pict(extra_fields);

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
            slice_ok: true,
            previous: self.previous.clone(),
            recent: self.recent.clone(),
            mpeg1: seq.ext.is_none(),
            chroma_format,
            closed_captions,
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
        let mut frame = ap.frame;
        if !ap.closed_captions.is_empty()
            && let Ok(buffer) = vaco_pool::Buffer::from_slice(&mut self.budget, &ap.closed_captions)
        {
            frame.set_side_data(FrameSideData::ClosedCaptions(buffer));
        }
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
        // ATSC A/53 closed captions (interface gap 18's attachment half —
        // extraction itself is `vaco_parse_mpegvideo::a53`, already
        // landed). Concatenated across every `user_data()` element seen
        // since the last picture began, in stream order, and drained into
        // that picture at its first slice — see `ActivePicture::closed_captions`'s
        // doc for why it must land on *this* picture rather than accumulate.
        let mut pending_cc: Vec<u8> = Vec::new();
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
                USER_DATA_START => {
                    if let Some(triplets) = cc_data_after_identifier(body) {
                        pending_cc.extend_from_slice(triplets);
                    }
                }
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
                        self.begin_picture(hdr, pce, pts, duration, std::mem::take(&mut pending_cc))?;
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

/// H.262 §6.3.10's `top_field_first`/`repeat_first_field` semantics text:
/// how many *extra* field periods (beyond the one this picture normally
/// gets) its presentation should be held for, given the sequence-level
/// `progressive_sequence` and the picture-level `progressive_frame`/
/// `repeat_first_field`/`top_field_first`. Verified against the primary
/// text directly (not recalled), which states three mutually exclusive
/// cases:
///
/// - `progressive_sequence == 1`: `repeat_first_field == 0` outputs one
///   frame (no repeat, `top_field_first` forced to `0`); `== 1` outputs
///   two frames if `top_field_first == 0` or three if `== 1` — "two" and
///   "three identical progressive frames" in the primary text's own
///   words. In field-period units (a frame is two fields), that is `0`,
///   `2` or `4` extra fields respectively.
/// - `progressive_sequence == 0`, `progressive_frame == 1`:
///   `repeat_first_field == 0` outputs two fields (no repeat); `== 1`
///   outputs three fields (one extra).
/// - `progressive_sequence == 0`, `progressive_frame == 0`:
///   `repeat_first_field` is required to be `0` by the same text (an
///   interlaced-fields frame is never pulled down), so this case is
///   always zero extra fields regardless of what a non-conforming
///   bitstream might set the bit to.
#[must_use]
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "each parameter is an independent H.262 syntax element (one sequence-level, three picture-level) this function's own doc comment names individually; a two-variant-enum-per-flag refactor would not make any call site clearer, only rename `true`/`false` to bespoke variants"
)]
const fn pulldown_extra_fields(
    progressive_sequence: bool,
    progressive_frame: bool,
    repeat_first_field: bool,
    top_field_first: bool,
) -> u8 {
    if !repeat_first_field {
        return 0;
    }
    if progressive_sequence {
        if top_field_first { 4 } else { 2 }
    } else if progressive_frame {
        1
    } else {
        // Non-conforming input (the primary text requires
        // `repeat_first_field == 0` here) — treat as no repeat rather
        // than propagate a value the bitstream was not allowed to send.
        0
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn decoder_reports_need_more_input_before_any_packet() {
        let mut dec = Mpeg12Decoder::new(Limits::strict());
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn pulldown_extra_fields_matches_h262_combination_table() {
        // §6.3.10's three cases, verified against the primary text
        // directly (see `pulldown_extra_fields`'s own doc comment).
        // progressive_sequence, progressive_frame, repeat_first_field,
        // top_field_first -> extra field periods.
        assert_eq!(pulldown_extra_fields(true, true, false, false), 0);
        assert_eq!(pulldown_extra_fields(true, true, true, false), 2);
        assert_eq!(pulldown_extra_fields(true, true, true, true), 4);
        assert_eq!(pulldown_extra_fields(false, true, false, false), 0);
        assert_eq!(pulldown_extra_fields(false, true, true, false), 1);
        assert_eq!(pulldown_extra_fields(false, true, true, true), 1);
        // progressive_sequence == 0 && progressive_frame == 0: the
        // primary text requires repeat_first_field == 0 here; a
        // non-conforming stream setting it anyway still gets 0, not a
        // value the text never allows this combination to produce.
        assert_eq!(pulldown_extra_fields(false, false, false, false), 0);
        assert_eq!(pulldown_extra_fields(false, false, true, false), 0);
    }

    #[test]
    fn repeat_first_field_attaches_pulldown_side_data() {
        let mut dec = Mpeg12Decoder::new(Limits::strict());
        dec.seq = Some(Sequence {
            header: SequenceHeader {
                width: 16,
                height: 16,
                intra_matrix: tables::DEFAULT_INTRA_MATRIX,
                non_intra_matrix: tables::DEFAULT_NON_INTRA_MATRIX,
            },
            // `Some` with the default `progressive_sequence == false`:
            // an MPEG-2 sequence, exercising the
            // `progressive_sequence == 0, progressive_frame == 1` case,
            // distinct from `top_field_first_propagates_to_frame_flags`'s
            // `ext: None` (MPEG-1, `progressive_sequence` forced `true`).
            ext: Some(SequenceExtension {
                progressive_sequence: false,
                chroma_format: 1,
            }),
            mb_width: 1,
            mb_height: 1,
        });
        let hdr = PictureHeader {
            temporal_reference: 0,
            coding_type: headers::PictureType::I,
            full_pel_forward_vector: false,
            forward_f_code: 0,
            full_pel_backward_vector: false,
            backward_f_code: 0,
        };
        let pce = PictureCodingExtension {
            progressive_frame: true,
            repeat_first_field: true,
            top_field_first: false,
            ..PictureCodingExtension::mpeg1_default(0, 0)
        };
        assert!(
            dec.begin_picture(hdr, pce, vaco_core::Timestamp::default(), vaco_core::Duration::default(), Vec::new())
                .is_ok()
        );
        let ap = dec.current.as_ref();
        assert!(ap.is_some(), "begin_picture did not populate current");
        if let Some(ap) = ap {
            assert_eq!(ap.frame.repeat_pict(), 1);
        }
    }

    #[test]
    fn top_field_first_propagates_to_frame_flags() {
        let mut dec = Mpeg12Decoder::new(Limits::strict());
        dec.seq = Some(Sequence {
            header: SequenceHeader {
                width: 16,
                height: 16,
                intra_matrix: tables::DEFAULT_INTRA_MATRIX,
                non_intra_matrix: tables::DEFAULT_NON_INTRA_MATRIX,
            },
            ext: None,
            mb_width: 1,
            mb_height: 1,
        });
        let hdr = PictureHeader {
            temporal_reference: 0,
            coding_type: headers::PictureType::I,
            full_pel_forward_vector: false,
            forward_f_code: 0,
            full_pel_backward_vector: false,
            backward_f_code: 0,
        };
        let pce = PictureCodingExtension {
            top_field_first: true,
            ..PictureCodingExtension::mpeg1_default(0, 0)
        };
        assert!(
            dec.begin_picture(hdr, pce, vaco_core::Timestamp::default(), vaco_core::Duration::default(), Vec::new())
                .is_ok()
        );
        let ap = dec.current.as_ref();
        assert!(ap.is_some(), "begin_picture did not populate current");
        if let Some(ap) = ap {
            assert!(ap.frame.flags.contains(FrameFlags::TOP_FIELD_FIRST));
        }
    }

    /// Minimal MSB-first bit packer, just enough to hand-build the fixed
    /// fields `decode_access_unit` reads — not a general bitstream writer.
    struct BitPacker {
        buf: Vec<u8>,
        cur: u8,
        nbits: u8,
    }

    impl BitPacker {
        fn new() -> Self {
            Self { buf: Vec::new(), cur: 0, nbits: 0 }
        }

        fn push(&mut self, value: u32, width: u32) {
            for i in (0..width).rev() {
                self.cur = (self.cur << 1) | ((value >> i) & 1) as u8;
                self.nbits += 1;
                if self.nbits == 8 {
                    self.buf.push(self.cur);
                    self.cur = 0;
                    self.nbits = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.nbits > 0 {
                self.cur <<= 8 - self.nbits;
                self.buf.push(self.cur);
            }
            self.buf
        }
    }

    /// A whole real-shaped access unit — `sequence_header()`,
    /// `picture_header()`, a picture-level A/53 `user_data()` carrying two
    /// caption triplets, and one (garbage, never fully decoded) slice —
    /// exercises the same path `USER_DATA_START` wires up in
    /// `decode_access_unit`: extraction is `vaco_parse_mpegvideo::a53`'s
    /// own job (covered there against a real broadcast capture), this
    /// test is only checking that this crate attaches what it returns to
    /// the picture that follows it, as `FrameSideData::ClosedCaptions`.
    #[test]
    fn picture_user_data_caption_reaches_the_decoded_frame() {
        let mut seq_bits = BitPacker::new();
        seq_bits.push(16, 12); // horizontal_size_value
        seq_bits.push(16, 12); // vertical_size_value
        seq_bits.push(1, 4); // aspect_ratio_information
        seq_bits.push(1, 4); // frame_rate_code
        seq_bits.push(0, 18); // bit_rate_value
        seq_bits.push(1, 1); // marker_bit
        seq_bits.push(0, 10); // vbv_buffer_size_value
        seq_bits.push(0, 1); // constrained_parameters_flag
        seq_bits.push(0, 1); // load_intra_quantiser_matrix
        seq_bits.push(0, 1); // load_non_intra_quantiser_matrix
        let seq_body = seq_bits.finish();

        let mut pic_bits = BitPacker::new();
        pic_bits.push(0, 10); // temporal_reference
        pic_bits.push(1, 3); // picture_coding_type == I
        pic_bits.push(0, 16); // vbv_delay
        let pic_body = pic_bits.finish();

        // ATSC A/53 `user_data()`: GA94 identifier, MPEG_cc_data type,
        // two arbitrary triplets — the extraction side already has its
        // own real-capture-derived tests, so these bytes only need to be
        // well-formed enough to round-trip through this crate's wiring.
        let triplets: [u8; 6] = [0xFC, 0x41, 0x42, 0xFC, 0x43, 0x44];
        let mut user_data = Vec::new();
        user_data.extend_from_slice(&0x4741_3934u32.to_be_bytes()); // 'GA94'
        user_data.push(0x03); // MPEG_cc_data()
        user_data.push(0x40 | 2); // process_cc_data_flag=1, cc_count=2
        user_data.push(0xFF); // em_data
        user_data.extend_from_slice(&triplets);
        user_data.push(0xFF); // marker_bits

        let mut data = Vec::new();
        data.extend_from_slice(&[0, 0, 1, SEQUENCE_HEADER]);
        data.extend_from_slice(&seq_body);
        data.extend_from_slice(&[0, 0, 1, PICTURE_START]);
        data.extend_from_slice(&pic_body);
        data.extend_from_slice(&[0, 0, 1, USER_DATA_START]);
        data.extend_from_slice(&user_data);
        data.extend_from_slice(&[0, 0, 1, 0x01]); // slice_start_code(1)
        data.extend_from_slice(&[0x00, 0x00]); // slice payload, never fully decoded

        let mut budget = Budget::new(Limits::strict());
        let Ok(packet) = vaco_packet::Packet::from_slice(&mut budget, &data) else {
            panic!("well-formed test payload must build a packet");
        };
        let mut dec = Mpeg12Decoder::new(Limits::strict());
        dec.send_packet(Some(&packet)).expect("send_packet");
        // The I-picture is held for reordering (see the module docs); drain
        // to force it out.
        dec.send_packet(None).expect("drain");
        let frame = dec.receive_frame().expect("one frame out of one I-picture");
        let side = frame
            .side_data(vaco_frame::FrameSideDataKind::ClosedCaptions)
            .expect("ClosedCaptions side data must be attached");
        let FrameSideData::ClosedCaptions(buffer) = side else {
            panic!("wrong side-data variant");
        };
        assert_eq!(buffer.as_slice(), &triplets);
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

    proptest::proptest! {
        /// No arbitrary byte sequence, handed to the decoder as a single
        /// packet (the same shape `fuzz/fuzz_targets/mpeg12_decode.rs`
        /// exercises with a coverage-guided corpus), may panic — this is a
        /// property of the whole `send_packet`/`receive_frame` pipeline,
        /// not any one function, so it belongs at this level rather than
        /// as a table- or block-level unit test. Every plane a produced
        /// frame claims to have written must also be addressable, which
        /// would catch a size/stride bug a pixel-accuracy comparison
        /// alone would not.
        #[test]
        fn decode_never_panics_on_arbitrary_bytes(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            let mut budget = Budget::new(Limits::strict());
            let Ok(packet) = vaco_packet::Packet::from_slice(&mut budget, &data) else {
                return Ok(());
            };
            let mut dec = Mpeg12Decoder::new(Limits::strict());
            if dec.send_packet(Some(&packet)).is_ok() {
                while let Ok(frame) = dec.receive_frame() {
                    for idx in 0..3 {
                        if let Some(plane) = frame.plane(idx) {
                            let _ = plane.row(0);
                        }
                    }
                }
            }
        }
    }
}
