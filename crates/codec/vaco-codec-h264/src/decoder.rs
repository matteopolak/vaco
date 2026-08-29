//! [`H264Decoder`] — the [`Decoder`] this crate registers, and the thing
//! `vaco -i input.mp4` actually calls to turn an H.264 access unit into
//! pixels.
//!
//! # What this decoder covers
//!
//! **CABAC I/P slices, one slice per picture, `ChromaArrayType == 1`
//! (4:2:0), frame (non-MBAFF, non-field) pictures.** That is exactly
//! [`crate::mb::decode_slice_cabac`]'s own scope, and exactly what
//! [`crate::reconstruct::reconstruct_picture`] turns into real luma/Cb/Cr
//! samples — see both modules' own docs for the full account of what is
//! and is not implemented one level down (`I_PCM`, B slices, MBAFF, the
//! 8x8 transform, `constrained_intra_pred_flag`'s substitution rule, and
//! more than one slice per picture are all refused explicitly, not
//! silently mishandled).
//!
//! This used to stop at resolving `entropy_coding_mode_flag` and return
//! [`Error::Unsupported`] unconditionally — the macroblock layer,
//! reconstruction, motion compensation and chroma all existed by then but
//! had no caller outside their own tests. That gap is what this module
//! now closes for the CABAC path.
//!
//! **CAVLC is still refused, honestly, not silently mishandled.**
//! [`crate::mb::decode_slice_cavlc`] verifies bit-exact *consumption* of a
//! real CAVLC slice (`tests/macroblock_layer.rs`), but
//! [`crate::mb::decode_residual`] discards every decoded coefficient
//! (only `TotalCoeff` survives, for the next block's own `nC`) and
//! `decode_mb_pred_inter`/`decode_parts`/`decode_sub_mb_pred` discard
//! every `ref_idx`/`mvd` the same way — there is no motion-vector
//! prediction grid for CAVLC at all, the CABAC side's `CabacGrids`
//! equivalent. Wiring CAVLC to real pixels needs that whole apparatus
//! rebuilt for CAVLC's own neighbour derivation, which is a
//! multiple-real-bug-finding undertaking of its own scale (see this
//! crate's `vaco-component.toml`/module docs for how many real bugs the
//! CABAC side alone took to reach byte-exactness) — attempting it under
//! this dispatch's own time-box risked exactly the "measured but
//! confidently wrong" failure this project's own constraints warn
//! against, so it stays an explicit [`Error::Unsupported`] naming the gap
//! precisely, the same choice this crate already makes for `I_PCM` and
//! CABAC B slices.
//!
//! # AVCC vs Annex B
//!
//! MP4 stores length-prefixed samples (`avcC`); this decoder's own
//! [`H264Parser`] already tells the two framings apart from the
//! `avcC`/Annex-B extradata shape ([`H264Parser::set_extradata`]) and
//! remembers which one applies ([`H264Parser::framing`]) — the same
//! detection ffmpeg's own H.264 decoder performs, driven off the same
//! signal. [`vaco_format_nalu::units`] (the low-level NAL-unit iterator
//! both this module and [`H264Parser`] itself are built on — reused, not
//! reimplemented) then walks either framing identically, so **no
//! `h264_mp4toannexb` bitstream filter is applied or needed in this
//! path**: measured against `push_access_unit`'s own two entry points, a
//! separate conversion step would only duplicate what `Framing`-aware
//! iteration already does for free.
//!
//! # Access-unit assembly
//!
//! [`H264Parser::push_access_unit`] is called on every packet ([`send_packet`](Decoder::send_packet)),
//! reusing its own parameter-set bookkeeping, picture-order-count and
//! new-picture detection rather than re-deriving any of it — this
//! decoder only re-scans the same access unit's NAL units afterward
//! (again via [`vaco_format_nalu::units`], not a second parser) to reach
//! the one primary-coded-picture slice's raw bits `push_access_unit`
//! itself does not hand back.
//!
//! # Output ordering
//!
//! Frames are emitted in decode order with no reorder buffer. That is
//! exactly right for what this decoder supports: CABAC B slices are
//! refused (see above), and for an I/P-only stream decode order and
//! display order are the same order (clause 8.2.1's own picture order
//! count is monotonic across I/P pictures with no MMCO/long-term
//! marking, which this crate's DPB does not implement — #422). A real
//! B-frame stream cannot reach this decoder at all today: its B slices
//! are refused before any reordering question would even arise.
//!
//! # Reference picture buffering
//!
//! A simple sliding window (clause 8.2.5.3's own removal process, minus
//! MMCO/long-term marking — #422 tracks that gap): every reference
//! picture decoded, most recent first, capped at the SPS's own
//! `max_num_ref_frames`. An IDR access unit clears it first, so a P slice
//! can never accidentally reach across a GOP boundary.

use std::collections::VecDeque;

use vaco_bitstream::BitReader;
use vaco_codec_cabac::CabacDecoder;
use vaco_codec_core::{Accept, Caps, Decoder, Machine};
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_h264::{H264NalHeader, H264Parser, NalUnitType, SliceHeader};
use vaco_pixfmt::PixFmt;

use crate::reconstruct::{ReconstructedPicture, RefPicturePlanes, reconstruct_picture};

/// One decoded reference picture's three planes, coded (macroblock-aligned,
/// uncropped) size — the shape [`crate::reconstruct::reconstruct_picture`]'s
/// own `ref_list0` needs, kept from before cropping since a later
/// picture's own motion compensation reads the *coded* picture, not the
/// display-cropped one.
#[derive(Debug)]
struct RefPicture {
    luma: Vec<u8>,
    cb: Vec<u8>,
    cr: Vec<u8>,
}

/// A copy of `src`, charged to `budget` -- the DPB's own reference-picture
/// clone (`self.dpb.push_back`'s own `luma`/`cb`/`cr` fields) used to be a
/// plain [`slice::to_vec`]/[`Clone`], real memory the budget never heard
/// about at all. Charging it here is what makes the matching
/// `budget.release` at eviction (see [`H264Decoder::decode_packet`]'s own
/// DPB-push site) balance a real charge instead of releasing bytes that
/// were never committed in the first place.
fn budgeted_clone(budget: &mut Budget, src: &[u8]) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = budget.alloc(src.len())?;
    out.copy_from_slice(src);
    Ok(out)
}

/// The bytes a [`RefPicture`] holds live, for the `budget.release` call at
/// every eviction site (DPB overflow, an IDR's own clear-before-decode,
/// and `flush`) to release exactly what [`budgeted_clone`] charged for it.
fn ref_picture_bytes(rp: &RefPicture) -> u64 {
    (rp.luma.len() as u64)
        .saturating_add(rp.cb.len() as u64)
        .saturating_add(rp.cr.len() as u64)
}

/// The bytes a [`ReconstructedPicture`] holds live -- what
/// `PictureBuffer::new` (inside [`reconstruct_picture`]) charged
/// `self.budget` for, and what must be released once `decode_packet` is
/// done reading from it (after `build_frame` and, for a reference
/// picture, the `budgeted_clone` above have both taken their own copies).
fn reconstructed_picture_bytes(pic: &ReconstructedPicture) -> u64 {
    (pic.luma.len() as u64)
        .saturating_add(pic.cb.len() as u64)
        .saturating_add(pic.cr.len() as u64)
}

/// The H.264 decoder. See the module doc for exactly what is and is not
/// implemented today.
#[derive(Debug)]
pub struct H264Decoder {
    limits: Limits,
    parser: H264Parser,
    budget: Budget,
    rbsp: vaco_format_nalu::RbspBuf,
    machine: Machine<Frame>,
    /// Sliding-window DPB, most-recently-decoded reference picture last —
    /// see the module doc's "reference picture buffering" section.
    dpb: VecDeque<RefPicture>,
}

impl H264Decoder {
    /// Build a decoder bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            parser: H264Parser::new(limits.clone()),
            budget: Budget::new(limits.clone()),
            rbsp: vaco_format_nalu::RbspBuf::new(),
            machine: Machine::new(Caps::empty()),
            dpb: VecDeque::new(),
            limits,
        }
    }

    /// Decode one packet (one MP4 sample / access unit) into at most one
    /// frame. `Ok(None)` means the access unit carried no primary-coded
    /// picture (parameter sets/SEI only), which is legal and not an
    /// error.
    fn decode_packet(&mut self, pkt: &Packet) -> Result<Option<Frame>> {
        let payload = pkt.payload();
        let framing = self.parser.framing();

        // Reuse `H264Parser`'s own access-unit assembly for parameter-set
        // bookkeeping, POC and new-picture detection -- not re-derived
        // here. `info.picture_type` is `None` when this access unit held
        // no VCL slice at all.
        let info = self.parser.push_access_unit(payload, framing)?;
        if info.picture_type.is_none() {
            return Ok(None);
        }

        // Locate this access unit's one primary-coded-picture slice --
        // `vaco_format_nalu::units` is the same reusable, `Framing`-aware
        // NAL-unit iterator `H264Parser` itself is built on, not a second
        // parser. Slice partitions (types 2/3/4) and the auxiliary/MVC
        // extension types are out of scope and skipped, same as
        // `push_access_unit`'s own picture-boundary derivation already
        // treats them.
        let mut slice_nal: Option<&[u8]> = None;
        let mut slice_count = 0u32;
        for nal in vaco_format_nalu::units(payload, framing) {
            let Some(header) = H264NalHeader::parse(nal.data) else {
                continue;
            };
            if matches!(header.nal_unit_type, NalUnitType::IdrSlice | NalUnitType::NonIdrSlice) {
                slice_count += 1;
                if slice_nal.is_none() {
                    slice_nal = Some(nal.data);
                }
            }
        }
        if slice_count > 1 {
            return Err(Error::Unsupported(
                "vaco-codec-h264: more than one slice per picture is not supported \
                 (crate::reconstruct's own scope line)",
            ));
        }
        let Some(nal_bytes) = slice_nal else {
            return Ok(None);
        };
        let nal = H264NalHeader::parse(nal_bytes).ok_or(Error::InvalidData("empty NAL unit"))?;

        self.rbsp.fill(nal_bytes, &mut self.budget)?;
        let rbsp = self.rbsp.as_slice();

        // clause 7.3.3's own three leading `ue(v)` fields, read once just
        // far enough to resolve `pic_parameter_set_id` -- the active
        // PPS/SPS pair has to be known before the *rest* of
        // `slice_header()` can be parsed (several later fields' own
        // presence depends on the PPS), so this short peek is
        // unavoidable, not a duplicate of the full parse below.
        let mut peek = BitReader::new(rbsp);
        peek.skip(8);
        let pps_id = {
            let mut g = BoundedGolomb::new(&mut peek, &mut self.budget);
            let _first_mb_in_slice = g.ue_v(u32::MAX)?;
            let _slice_type = g.ue_v(9)?;
            g.ue_v(255)? as u8
        };
        let (pps, sps) = self
            .parser
            .parameter_sets()
            .sps_for_pps(pps_id)
            .ok_or(Error::Unsupported("vaco-codec-h264: referenced PPS/SPS not seen yet"))?;

        let mut reader = BitReader::new(rbsp);
        reader.skip(8);
        let slice_header = SliceHeader::parse_data(&mut reader, nal, sps, pps, &mut self.budget)?;
        if slice_header.first_mb_in_slice != 0 {
            return Err(Error::Unsupported(
                "vaco-codec-h264: a slice that does not start at the first macroblock \
                 implies more than one slice per picture, which is not supported",
            ));
        }

        if info.is_idr {
            // clause 8.2.5.1: an IDR access unit empties the DPB before
            // anything in it decodes, so a P slice can never reach across
            // a GOP boundary. Every evicted picture's planes were charged
            // to `self.budget` when they were pushed (see the reference-
            // picture push below) and must be released here, not just
            // dropped -- #421: `Budget::release` is never automatic, so a
            // `clear()` that does not call it leaves `committed` counting
            // bytes real memory no longer holds.
            for evicted in self.dpb.drain(..) {
                self.budget.release(ref_picture_bytes(&evicted));
            }
        }

        if !pps.entropy_coding_mode {
            return Err(Error::Unsupported(
                "vaco-codec-h264: CAVLC picture reconstruction is not implemented -- \
                 decode_slice_cavlc verifies bit consumption only and discards every \
                 decoded coefficient and motion vector; see this module's own doc",
            ));
        }

        let mbs_wide = sps.pic_width_in_mbs;
        let mbs_high = sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
        let chroma_qp_offset_cb = pps.chroma_qp_index_offset;
        let chroma_qp_offset_cr = pps.second_chroma_qp_index_offset;
        let max_num_ref_frames = sps.max_num_ref_frames;
        // Extracted now, not read from `sps`/`pps` again later: both
        // borrow `self.parser`, which `build_frame` below needs `&mut
        // self` to allocate through.
        let dimensions = sps.dimensions();
        let crop_unit = sps.crop_unit();
        let crop = sps.crop.unwrap_or_default();

        let mut cabac = CabacDecoder::from_reader(reader);
        let stats = crate::mb::decode_slice_cabac(&mut cabac, &mut self.budget, sps, pps, &slice_header)?;
        if cabac.malformed() {
            return Err(Error::InvalidData(
                "vaco-codec-h264: CABAC engine reported malformed input",
            ));
        }
        // `!cabac.malformed()` alone is not proof the slice actually
        // decoded correctly -- `end_of_slice_flag` can fire at a
        // macroblock-count-plausible point purely by coincidence even
        // when some decoded value upstream of it was wrong (this crate's
        // own `tests/macroblock_layer_cabac.rs` documents this failure
        // mode). Measured directly against real `ffmpeg`-encoded
        // multi-reference content: without this check, `H264Decoder`
        // silently emitted a partially-grey, visibly wrong frame --
        // `stats.macroblocks` only covers whatever was actually visited
        // before a premature `end_of_slice_flag`, and `reconstruct_picture`
        // leaves the rest of the picture at its own default fill.
        //
        // This crate's own `assert_slice_ends_at_rbsp_trailing_bits` test
        // helper goes one step further and also checks that what
        // immediately follows `end_of_slice_flag` is a clean
        // `rbsp_slice_trailing_bits()` pattern (clause 7.3.2.10) -- **that
        // check was tried here too and reverted**: measured directly
        // against this crate's own byte-exact-pixel-output corpus
        // (`cabac_i_only.264`, `crate::reconstruct`'s own 100%-match-
        // against-ffmpeg fixture), it produces false positives -- content
        // whose every sample is already proven byte-exact still fails
        // the trailing-bits alignment check on several frames. That is
        // exactly the "right answers, wrong bit cost" phenomenon
        // `tests/macroblock_layer_cabac.rs`'s own ignored tests describe:
        // a CABAC arithmetic-engine bit-accounting discrepancy confined
        // to the terminating decision itself, not a defect that reaches
        // any decoded sample. Enforcing it here would refuse decodes this
        // crate has already proven correct, which is a worse failure than
        // the gap it was meant to close.
        let total_mbs = mbs_wide.saturating_mul(mbs_high);
        if stats.macroblock_count != total_mbs {
            return Err(Error::InvalidData(
                "vaco-codec-h264: CABAC slice's end_of_slice_flag fired before every \
                 macroblock in the picture was decoded -- a real, still-open decode \
                 desync (see tests/macroblock_layer_cabac.rs's own ignored tests), \
                 refused rather than emitting a partially-reconstructed frame",
            ));
        }

        let ref_list0: Vec<RefPicturePlanes<'_>> = self
            .dpb
            .iter()
            .rev()
            .map(|r| RefPicturePlanes {
                luma: &r.luma,
                cb: &r.cb,
                cr: &r.cr,
            })
            .collect();

        let mut pic: ReconstructedPicture = reconstruct_picture(
            &stats.macroblocks,
            mbs_wide,
            mbs_high,
            chroma_qp_offset_cb,
            chroma_qp_offset_cr,
            &ref_list0,
            &mut self.budget,
        )?;
        drop(ref_list0);

        // Clause 8.7's deblocking filter, luma and chroma, both I and P
        // slices -- `crate::deblock`'s own module doc has the full
        // boundary-strength derivation (Table 8-18, collapsed to this
        // decoder's single-reference-list P-slice scope) and the
        // luma-to-chroma `bS` mapping for 4:2:0.
        crate::deblock::deblock_picture_luma(
            &mut pic.luma,
            &stats.macroblocks,
            mbs_wide,
            mbs_high,
            slice_header.disable_deblocking_filter_idc,
            slice_header.slice_alpha_c0_offset_div2,
            slice_header.slice_beta_offset_div2,
        )?;
        for (chroma, offset) in [(&mut pic.cb, chroma_qp_offset_cb), (&mut pic.cr, chroma_qp_offset_cr)] {
            crate::deblock::deblock_picture_chroma(
                chroma,
                &stats.macroblocks,
                mbs_wide,
                mbs_high,
                offset,
                slice_header.disable_deblocking_filter_idc,
                slice_header.slice_alpha_c0_offset_div2,
                slice_header.slice_beta_offset_div2,
            );
        }

        if info.is_reference {
            let cap = max_num_ref_frames.max(1) as usize;
            // Evict down to `cap - 1` *before* pushing this picture's own
            // clone, not after: pushing first and evicting after (the
            // shape #421 was originally filed against) briefly holds
            // `cap + 1` reference pictures' worth of budget at once on
            // every single frame, which is a real, avoidable peak on top
            // of the leak -- at 4K, with a CABAC slice's own working
            // grids and the just-reconstructed picture *also* alive at
            // that same instant, that extra picture's worth of charge is
            // what was still deciding whether the last frame or two of a
            // `max_alloc_total`-bounded run fit.
            while self.dpb.len() > cap.saturating_sub(1) {
                // #421: the picture this evicts was charged to the budget
                // by the `budgeted_clone` calls below (on a previous call
                // to this method) -- `pop_front` alone drops its `Vec`s
                // (real memory freed correctly) without ever telling
                // `Budget` those bytes are free, which is what let
                // `committed` climb forever and cap 1080p at exactly 10
                // frames. Releasing here is the other half of that same
                // fix, matched to the `budgeted_clone` charge below.
                if let Some(evicted) = self.dpb.pop_front() {
                    self.budget.release(ref_picture_bytes(&evicted));
                }
            }
            let stored = RefPicture {
                luma: budgeted_clone(&mut self.budget, &pic.luma)?,
                cb: budgeted_clone(&mut self.budget, &pic.cb)?,
                cr: budgeted_clone(&mut self.budget, &pic.cr)?,
            };
            self.dpb.push_back(stored);
        }

        // `Frame::alloc_video` (inside `build_frame`) charges `self.budget`
        // for the frame's own planes, and nothing about a `Frame`'s `Drop`
        // ever calls `Budget::release` -- `vaco_pool::Buffer`'s own Drop
        // only returns storage to a *pool*, and an unpooled buffer (what
        // `alloc_video` always builds) has none. Once this frame is handed
        // to `self.machine.emit`, it is the caller's memory to account for,
        // not this decoder's own working set -- exactly the same
        // "no longer mine to track" reasoning the DPB eviction and `pic`
        // release below already apply to reference pictures and the
        // just-reconstructed picture. Measuring the real charge via the
        // `committed()` delta (rather than recomputing `PixFmt::plane_layout`
        // by hand a second time) is what stays correct through this
        // format's own row-stride/alignment padding without duplicating it.
        let before_frame = self.budget.committed();
        let frame = self.build_frame(dimensions, crop_unit, crop, mbs_wide, &pic, pkt, info.is_idr)?;
        let frame_bytes = self.budget.committed().saturating_sub(before_frame);
        self.budget.release(frame_bytes);
        // `pic`'s own three planes were charged to `self.budget` inside
        // `reconstruct_picture` (via `PictureBuffer::new`) and have now
        // been fully consumed -- `build_frame` copied whatever it needed
        // into `frame`'s own (separately budgeted, just released above)
        // planes, and any reference copy this picture needed was already
        // charged independently above. Releasing here, right before `pic`
        // drops, is the DPB fix's other half: without it, every decoded
        // picture -- reference or not -- would still add O(picture size)
        // to `committed` and never give it back, which is what made even a
        // `-refs 1` stream fail after a fixed frame count regardless of
        // the DPB's own bound.
        self.budget.release(reconstructed_picture_bytes(&pic));
        Ok(Some(frame))
    }

    /// Crops `pic` from its coded (macroblock-aligned) size down to the
    /// SPS's own displayed size (clause 7.4.2.1.1's `frame_crop_*`,
    /// already resolved by the caller into `dimensions`/`crop_unit`/`crop`
    /// so this method needs no live borrow of `self.parser`'s own
    /// parameter-set store) and packs it into a real [`Frame`], `yuv420p`.
    fn build_frame(
        &mut self,
        dimensions: Option<(u32, u32)>,
        crop_unit: (u32, u32),
        crop: vaco_parse_h264::Crop,
        mbs_wide: u32,
        pic: &ReconstructedPicture,
        pkt: &Packet,
        is_idr: bool,
    ) -> Result<Frame> {
        let (width, height) =
            dimensions.ok_or(Error::InvalidData("vaco-codec-h264: SPS crop leaves no visible picture area"))?;
        let (unit_x, unit_y) = crop_unit;
        // Luma offsets are in luma samples (crop units scaled by
        // `CropUnitX`/`CropUnitY`); chroma offsets are the raw crop
        // values themselves, since `CropUnitX`/`CropUnitY` already fold
        // in `SubWidthC`/`SubHeightC` -- one crop unit horizontally is
        // exactly one chroma sample for `ChromaArrayType == 1`.
        let luma_x0 = (crop.left.saturating_mul(unit_x)) as usize;
        let luma_y0 = (crop.top.saturating_mul(unit_y)) as usize;
        let chroma_x0 = crop.left as usize;
        let chroma_y0 = crop.top as usize;

        let fmt = PixFmt::from_name("yuv420p")
            .map_err(|_| Error::InvalidData("vaco-codec-h264: yuv420p pixel format is not registered"))?;
        let mut frame = Frame::alloc_video(&mut self.budget, fmt, width, height)?;
        if is_idr {
            frame.flags |= FrameFlags::KEY;
        }

        let luma_stride = (mbs_wide * 16) as usize;
        let chroma_stride = (mbs_wide * 8) as usize;
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        blit_plane(&pic.luma, luma_stride, luma_x0, luma_y0, &mut frame, 0, w, h);
        blit_plane(&pic.cb, chroma_stride, chroma_x0, chroma_y0, &mut frame, 1, cw, ch);
        blit_plane(&pic.cr, chroma_stride, chroma_x0, chroma_y0, &mut frame, 2, cw, ch);

        frame.pts = pkt.pts;
        frame.duration = pkt.duration;
        Ok(frame)
    }
}

/// Copies one `width x height` region of `src` (row-major, `src_stride`
/// wide) starting at `(x0, y0)` into `frame`'s plane `plane_index` --
/// [`crate::interp`]'s own edge-clamping is not needed here, since the
/// crop region is always within the coded picture by construction (clause
/// 7.4.2.1.1's own range constraint on the crop offsets).
fn blit_plane(src: &[u8], src_stride: usize, x0: usize, y0: usize, frame: &mut Frame, plane_index: usize, width: usize, height: usize) {
    let Some(mut dst) = frame.plane_mut(plane_index) else {
        return;
    };
    for y in 0..height {
        let Some(row) = dst.row_mut(y) else { continue };
        let src_row_start = (y0 + y).saturating_mul(src_stride).saturating_add(x0);
        let src_row = src.get(src_row_start..src_row_start.saturating_add(width)).unwrap_or(&[]);
        for (x, out) in row.iter_mut().enumerate().take(width) {
            *out = src_row.get(x).copied().unwrap_or(0);
        }
    }
}

impl Decoder for H264Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match self.machine.accept(packet.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                match self.decode_packet(pkt) {
                    Ok(Some(frame)) => {
                        self.machine.emit(frame);
                        Ok(())
                    }
                    Ok(None) => Ok(()),
                    Err(e) => Err(e),
                }
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.parser.flush();
        self.dpb.clear();
        // Release every reference-frame byte charged to the budget along
        // with the state that held them, mirroring
        // `vaco_codec_vp8::Vp8Decoder::flush`'s own precedent.
        self.budget = Budget::new(self.limits.clone());
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.parser.set_extradata(extradata)?;
        Ok(())
    }
}
