//! [`HevcDecoder`] — the [`Decoder`] this crate registers.
//!
//! # AVCC-equivalent (`hvcC`) vs Annex B
//!
//! MP4/Matroska store length-prefixed samples (`hvcC`); a raw `.hevc`
//! elementary stream or an MPEG-TS PES payload is Annex B. This decoder
//! embeds [`HevcParser`] exactly the way `vaco-codec-h264` embeds
//! `H264Parser` — [`HevcParser::set_extradata`] tells the two framings
//! apart from the `hvcC`/Annex-B extradata shape and remembers which one
//! applies ([`HevcParser::framing`]); [`vaco_format_nalu::units`] (the
//! same low-level, `Framing`-aware NAL-unit iterator both this module and
//! [`HevcParser`] itself are built on) then walks either framing
//! identically. **No `hevc_mp4toannexb` bitstream filter is applied or
//! needed in this path** — that filter's job is muxer-side re-framing on
//! *output*, not decode input; see `vaco-bsf-h2645`'s own module doc.
//!
//! This used to walk every packet as a hardcoded Annex-B byte stream
//! (`vaco_bitstream::annexb::nal_units`) with its own ad-hoc VPS/SPS/PPS
//! `HashMap`s, so `vaco -i real.mp4 -c:v hevc` decoded nothing: MP4 never
//! carries in-band parameter sets, and the decoder never called
//! [`Decoder::set_extradata`]'s actual implementation because it didn't
//! have one — the trait's no-op default swallowed the `hvcC` box
//! silently. That is exactly `vaco-codec-h264`'s own "AVCC vs Annex B"
//! history repeating; the fix is the identical pattern, reusing
//! [`HevcParser`]'s parameter-set bookkeeping rather than duplicating it
//! a second time (D14: this crate must not re-derive what
//! `vaco-parse-hevc` already owns).
//!
//! # Access-unit assembly
//!
//! [`HevcParser::push_access_unit`] is called on every packet
//! ([`send_packet`](Decoder::send_packet)), reusing its own parameter-set
//! bookkeeping and slice-header parse rather than re-deriving either —
//! this decoder only re-scans the same access unit's NAL units afterward
//! (again via [`vaco_format_nalu::units`], not a second parser) to reach
//! the one primary-coded-picture slice's raw bits `push_access_unit`
//! itself does not hand back.
//!
//! # Output reordering (`Caps::DELAY` / `Caps::SUBFRAMES`)
//!
//! An I-slice-only decoder could emit its one frame per packet
//! synchronously (Stage 1's own `decode_packet -> Result<Option<Frame>>`
//! shape). Once P-slices exist at all, decode order and output order can
//! differ (Annex C.5.2's own bumping process), so a decoded picture's
//! frame is not necessarily ready the moment its packet is decoded, and one
//! packet's decode can flush zero, one, or several pending pictures at
//! once. `self.machine` (the shared send/receive protocol every codec in
//! this project uses) is declared with both flags for exactly that reason
//! — `Caps::DELAY` because a `None` (drain) send is required to flush
//! whatever the DPB is still holding, `Caps::SUBFRAMES` because one input
//! packet may legitimately `emit` more than one frame (an IRAP that bumps
//! several pending pictures before clearing) or none at all (a picture
//! decoded but not yet due for output).
//!
//! P-slices are implemented and no longer refused (see `ctu::coding_unit_p`
//! and everything under it, plus [`crate::motion`]/[`crate::mc`]) — the
//! TMVP-for-AMVP defect that used to block this is fixed; see
//! `docs/codec/vaco-codec-hevc.md`'s "Stage 2: P-slices" section for the
//! root cause. Weighted prediction (`weighted_pred_flag`, §8.5.3.3.4.3) is
//! implemented too — see [`crate::weight`]'s own module doc. B-slices
//! remain refused unconditionally by [`decode_packet`]'s own slice-kind
//! check, below.

use vaco_codec_cabac::CabacDecoder;
use vaco_codec_core::Decoder;
use vaco_codec_core::Caps;
use vaco_core::{Error, Result};
use vaco_format_nalu::{RbspBuf, units};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_hevc::{ChromaFormat, HevcNalHeader, HevcParser, PocState, Pps, SliceHeader, SliceKind, Sps};

use crate::cabac_ctx::ContextBank;
use crate::ctu::{self, Ctx, InterSliceParams, RefPic};
use crate::deblock;
use crate::dpb::{CollocatedMotionField, Dpb, PictureMeta};
use crate::framebuf::{CuGrid, Picture};
use crate::sao;

/// The HEVC decoder. See the crate doc and module doc for exactly what is
/// and is not implemented.
pub struct HevcDecoder {
    limits: Limits,
    parser: HevcParser,
    budget: Budget,
    rbsp: RbspBuf,
    machine: vaco_codec_core::machine::Machine<vaco_frame::Frame>,
    /// `None` until the first slice of the stream is decoded — a P-slice
    /// stream's own reorder/reference bounds (`Sps::max_dec_pic_buffering`
    /// etc.) are not known before then, and [`Dpb::new`] needs them.
    dpb: Option<Dpb>,
    /// ITU-T H.265 §8.3.1's own carried state (`prevTid0Poc`), owned by
    /// `vaco-parse-hevc` (see that crate's `poc` module doc for why POC
    /// derivation is parse-side, not decode-side, despite living in clause
    /// 8) — this decoder only calls it once per slice and resets it on
    /// [`Decoder::flush`].
    poc_state: PocState,
}

impl std::fmt::Debug for HevcDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HevcDecoder").finish_non_exhaustive()
    }
}

impl HevcDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            parser: HevcParser::new(limits.clone()),
            budget: Budget::new(limits.clone()),
            rbsp: RbspBuf::new(),
            machine: vaco_codec_core::machine::Machine::new(Caps::DELAY | Caps::SUBFRAMES),
            dpb: None,
            poc_state: PocState::new(),
            limits,
        }
    }

    /// Decode one packet (one container sample / access unit), storing any
    /// newly-decoded picture in the DPB and pushing whatever that bumps out
    /// (zero, one, or several frames) into `self.machine`. Returning
    /// nothing (rather than Stage 1's `Result<Option<Frame>>`) is exactly
    /// what `Caps::SUBFRAMES` means: a caller reads output back out via
    /// `receive_frame`, not this function's own return value.
    fn decode_packet(&mut self, pkt: &Packet) -> Result<()> {
        let payload = pkt.payload();
        let framing = self.parser.framing();

        // Reuse `HevcParser`'s own access-unit assembly for parameter-set
        // bookkeeping and slice-header parsing — not re-derived here.
        // `info.picture_type` is `None` when this access unit held no VCL
        // slice at all (or its parameter sets are not yet known, which is
        // legal for a stream joined mid-flight).
        let info = self.parser.push_access_unit(payload, framing)?;
        if info.picture_type.is_none() {
            return Ok(());
        }

        // Locate this access unit's one primary-coded-picture slice —
        // `vaco_format_nalu::units` is the same reusable, `Framing`-aware
        // NAL-unit iterator `HevcParser` itself is built on, not a second
        // parser.
        let mut slice_nal: Option<&[u8]> = None;
        let mut slice_count = 0u32;
        for nal in units(payload, framing) {
            let Some(header) = HevcNalHeader::parse(nal.data) else { continue };
            if !header.is_base_layer() {
                continue;
            }
            if header.nal_unit_type.has_slice_header() {
                slice_count += 1;
                if slice_nal.is_none() {
                    slice_nal = Some(nal.data);
                }
            }
        }
        if slice_count > 1 {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: more than one slice segment per picture is not supported",
            ));
        }
        let Some(ebsp) = slice_nal else { return Ok(()) };
        let header = HevcNalHeader::parse(ebsp).ok_or(Error::InvalidData("vaco-codec-hevc: empty NAL unit"))?;

        self.rbsp.fill(ebsp, &mut self.budget)?;
        let rbsp = self.rbsp.as_slice();

        let pps_id = vaco_parse_hevc::slice::peek_pps_id(rbsp)
            .ok_or(Error::InvalidData("vaco-codec-hevc: slice segment header truncated before pps_id"))?;
        // Cloned rather than borrowed: the CTU walk below needs `&mut
        // self.budget` for the rest of this function, which a borrow of
        // `self.parser`'s tables held open across that call would conflict
        // with.
        let (pps, sps) = {
            let (p, s) = self
                .parser
                .parameter_sets()
                .sps_for_pps(pps_id)
                .ok_or(Error::Unsupported("vaco-codec-hevc: referenced PPS/SPS not seen yet"))?;
            (p.clone(), s.clone())
        };

        check_scope(&sps, &pps)?;

        let mut reader = vaco_bitstream::BitReader::new(rbsp);
        reader.skip(16);
        let hdr = SliceHeader::parse_data(&mut reader, header, &sps, &pps, &mut self.budget)?;
        reader.check()?;

        // B-slices: `inter_pred_idc`/`ref_idx_l1`/`mvd_coding(x, y, 1)`/
        // `mvp_l1_flag` parsing (`ctu::decode_inter_cu`), `RefPicList1`
        // construction and `collocated_from_l0_flag` (both above, in this
        // function), the combined bi-predictive merge candidates
        // (`motion::derive_merge_candidates`'s own `is_b` branch) and
        // bi-predictive motion compensation (`ctu::predict_component`'s own
        // `(Some, Some)` arm, `mc::default_biprediction`/`apply_weight_bi`)
        // are all implemented. **This refusal stays in place regardless**
        // until a real B-slice fixture measures byte-exact against plain
        // `ffmpeg`, per this crate's own "registered-but-wrong is worse than
        // absent" discipline (`AGENT-CONSTRAINTS.md`) — see
        // `docs/codec/vaco-codec-hevc.md`'s B-slice section for what has and
        // has not been measured.
        if hdr.kind == SliceKind::B {
            return Err(Error::Unsupported("vaco-codec-hevc: B-slices are not supported"));
        }
        if hdr.dependent {
            return Err(Error::Unsupported("vaco-codec-hevc: dependent slice segments are not supported"));
        }
        if !hdr.first_slice_segment_in_pic {
            return Err(Error::Unsupported("vaco-codec-hevc: multiple slice segments per picture are not supported"));
        }

        reader.align();
        let cabac_data = reader.remaining_bytes();

        let slice_qp = 26 + pps.init_qp_minus26 + hdr.qp_delta;

        let width = usize::try_from(sps.pic_width_in_luma_samples).unwrap_or(0);
        let height = usize::try_from(sps.pic_height_in_luma_samples).unwrap_or(0);
        let mut pic = Picture::new(&mut self.budget, width, height)?;
        let cu_grid = CuGrid::new(&mut self.budget, width, height, hdr.kind == SliceKind::B)?;

        // ITU-T H.265 §8.3.1: this picture's own picture order count, and
        // (§8.1) whether it is the one IRAP in its sequence that clears
        // RASL output rather than merely refreshing prediction.
        let no_rasl_output =
            header.nal_unit_type.is_idr() || header.nal_unit_type.is_bla() || (header.nal_unit_type.is_cra() && !self.poc_state.started());
        let poc = self.poc_state.advance_with(&sps, &hdr, header.temporal_id, no_rasl_output);

        let max_dec_pic_buffering = usize::try_from(sps.max_dec_pic_buffering()).unwrap_or(1).max(1);
        let max_num_reorder_pics = usize::try_from(sps.max_num_reorder_pics()).unwrap_or(0);
        let max_latency_increase = sps.max_latency_increase_plus1.last().copied().unwrap_or(0);
        let dpb = self
            .dpb
            .get_or_insert_with(|| Dpb::new(max_dec_pic_buffering, max_num_reorder_pics, max_latency_increase));

        let sets = crate::dpb::derive_reference_pic_sets(poc.value, hdr.short_term_rps.as_ref(), !hdr.long_term_refs.is_empty())?;

        // §C.5.2.2: an IRAP with `NoRaslOutputFlag` bumps everything still
        // pending (unless `no_output_of_prior_pics_flag` says to drop it
        // silently) and then empties the DPB outright, before this
        // picture's own reference-picture-set marking (which would be
        // meaningless against an empty DPB anyway) runs.
        if header.nal_unit_type.is_irap() && no_rasl_output {
            let pocs = dpb.clear_for_irap(hdr.no_output_of_prior_pics);
            Self::emit_pocs(self.dpb.as_ref(), &mut self.budget, &mut self.machine, &pocs)?;
            if let Some(dpb) = self.dpb.as_mut() {
                dpb.clear_all(&mut self.budget);
            }
        } else if let Some(dpb) = self.dpb.as_mut() {
            // §8.3.2's marking (`remove_unused`'s own trailing call inside
            // `apply_reference_picture_set` is §C.5.2.2's unconditional
            // "empty every not-needed-and-unused buffer" step), then the
            // ordinary (non-IRAP-clear) pre-decode bump — against the DPB
            // state *before* this picture is stored, per §C.5.2.2's own
            // three-condition check. See `Dpb::bump_pre_decode`'s own doc
            // for why this has to run here and not folded into the
            // post-store bump below.
            dpb.apply_reference_picture_set(&sets, &mut self.budget);
            let pre_bumped = dpb.bump_pre_decode();
            Self::emit_pocs(self.dpb.as_ref(), &mut self.budget, &mut self.machine, &pre_bumped)?;
            if let Some(dpb) = self.dpb.as_mut() {
                dpb.reap_unused(&mut self.budget);
            }
        }

        let is_b = hdr.kind == SliceKind::B;
        let (list0, list1) = crate::dpb::build_ref_pic_lists(
            &sets,
            hdr.num_ref_idx_l0_active_minus1,
            hdr.num_ref_idx_l1_active_minus1,
            hdr.ref_pic_list_modification.as_ref(),
            is_b,
        );

        let inter = if hdr.kind == SliceKind::I {
            None
        } else {
            let dpb_ref = self.dpb.as_ref();
            let ref_pics_l0: Vec<RefPic<'_>> = list0
                .iter()
                .filter_map(|&p| dpb_ref.and_then(|d| d.reference_picture(p)).map(|pic| RefPic { poc: p, pic }))
                .collect();
            let ref_pics_l1: Vec<RefPic<'_>> = list1
                .iter()
                .filter_map(|&p| dpb_ref.and_then(|d| d.reference_picture(p)).map(|pic| RefPic { poc: p, pic }))
                .collect();
            // §8.5.3.2.9: `ColPic` is named from `RefPicList0` or
            // `RefPicList1` depending on `collocated_from_l0_flag` — a P
            // slice's own parser default (`collocated_from_l0 == true`)
            // makes this collapse to the pre-existing `list0`-only lookup.
            let collocated = if hdr.temporal_mvp_enabled {
                let col_list = if hdr.collocated_from_l0 { &list0 } else { &list1 };
                col_list
                    .get(usize::try_from(hdr.collocated_ref_idx).unwrap_or(0))
                    .and_then(|&p| dpb_ref.and_then(|d| d.collocated_for(p)))
            } else {
                None
            };
            // §8.5.3.2.9's `NoBackwardPredFlag`: every picture in *both*
            // lists has a POC no greater than the current picture's. Always
            // `true` trivially for a P slice (list1 is empty).
            let is_low_delay = list0.iter().chain(list1.iter()).all(|&p| p <= poc.value);
            let max_num_merge_cand = usize::try_from(5u32.saturating_sub(hdr.five_minus_max_num_merge_cand)).unwrap_or(1).max(1);
            // §8.5.3.3.4.1's `weightedPredFlag`: `weighted_pred_flag` for a P
            // slice, `weighted_bipred_flag` for a B slice — resolved once per
            // slice rather than per PU. `pred_weight_table()`'s own presence
            // condition in `vaco-parse-hevc` already gates on exactly this,
            // so `hdr.pred_weight_table.is_some()` alone is enough; no
            // separate flag check is needed here.
            let weights_l0 = hdr
                .pred_weight_table
                .as_ref()
                .map(|t| crate::weight::resolve_list(t, 0, ref_pics_l0.len(), u32::from(sps.bit_depth_luma), u32::from(sps.bit_depth_chroma)));
            let weights_l1 = if is_b {
                hdr.pred_weight_table
                    .as_ref()
                    .map(|t| crate::weight::resolve_list(t, 1, ref_pics_l1.len(), u32::from(sps.bit_depth_luma), u32::from(sps.bit_depth_chroma)))
            } else {
                None
            };
            Some(InterSliceParams {
                max_num_merge_cand,
                log2_parallel_merge_level: pps.log2_parallel_merge_level,
                amp_enabled: sps.amp_enabled,
                cur_poc: poc.value,
                ref_pics_l0,
                ref_pics_l1,
                is_b,
                collocated_from_l0: hdr.collocated_from_l0,
                is_low_delay,
                mvd_l1_zero: hdr.mvd_l1_zero,
                collocated,
                weights_l0,
                weights_l1,
            })
        };

        let mut walk = Ctx::new(
            &mut self.budget,
            &mut pic,
            cu_grid,
            &sps,
            &pps,
            slice_qp,
            hdr.deblocking_filter_disabled,
            hdr.beta_offset_div2,
            hdr.tc_offset_div2,
            hdr.sao_luma,
            hdr.sao_chroma,
            inter,
        )?;

        let qp_i8 = i8::try_from(slice_qp.clamp(0, 51)).unwrap_or(0);

        let ctb_size = 1u32 << walk.log2_ctb_size;
        let pic_width_u32 = u32::try_from(walk.pic_width).unwrap_or(0);
        let pic_height_u32 = u32::try_from(walk.pic_height).unwrap_or(0);
        let ctbs_x = pic_width_u32.div_ceil(ctb_size).max(1);
        let ctbs_y = pic_height_u32.div_ceil(ctb_size).max(1);
        let ctb_size_i = i32::try_from(ctb_size).unwrap_or(0);

        if pps.entropy_coding_sync_enabled {
            // §7.4.7.1's entry-point offsets are byte counts over the *coded*
            // (still-escaped) slice segment data, emulation-prevention bytes
            // included by the specification's own words — not over the
            // de-escaped RBSP `cabac_data` is sliced from. See
            // `decode_wpp_rows`'s own doc for why using `cabac_data`'s byte
            // positions directly under-counts every row boundary that a
            // `00 00 03` escape happens to precede.
            let header_rbsp_len = rbsp.len().saturating_sub(cabac_data.len());
            decode_wpp_rows(
                &mut self.budget,
                ebsp,
                header_rbsp_len,
                &hdr.entry_point_offsets,
                &mut walk,
                ctbs_x,
                ctbs_y,
                ctb_size_i,
                qp_i8,
                hdr.kind,
                hdr.cabac_init,
            )?;
        } else {
            let mut cabac = CabacDecoder::new(cabac_data);
            let mut ctx = new_context_bank(hdr.kind, hdr.cabac_init, qp_i8);
            let total_ctbs = ctbs_x.saturating_mul(ctbs_y);
            for addr in 0..total_ctbs {
                let col = addr.checked_rem(ctbs_x).unwrap_or(0);
                let row = addr.checked_div(ctbs_x).unwrap_or(0);
                let cx = i32::try_from(col).unwrap_or(0) * ctb_size_i;
                let cy = i32::try_from(row).unwrap_or(0) * ctb_size_i;
                ctu::decode_ctu(&mut cabac, &mut ctx, &mut walk, cx, cy, addr)?;
                let end = cabac.decode_terminate();
                if cabac.malformed() {
                    return Err(Error::InvalidData("vaco-codec-hevc: CABAC decode ran past the slice segment data"));
                }
                if end != 0 {
                    break;
                }
            }
        }

        deblock::filter_picture(&mut walk);
        sao::filter_picture(&mut self.budget, &mut walk)?;

        // Built here, while `walk.cu_grid` is still alive, from the same
        // per-4x4-block motion this slice itself just recorded — an I
        // slice records none (`inter_at` gates on `is_inter`, never set),
        // so its own collocated field is trivially `None` rather than an
        // all-`None` allocation nobody will read.
        let collocated_out = if hdr.kind == SliceKind::I {
            None
        } else {
            Some(CollocatedMotionField::build(poc.value, width, height, |x, y| walk.cu_grid.inter_at(x, y)))
        };

        let out_dims = sps.dimensions().unwrap_or((sps.pic_width_in_luma_samples, sps.pic_height_in_luma_samples));
        let meta = PictureMeta {
            pts: pkt.pts,
            duration: pkt.duration,
            out_width: out_dims.0,
            out_height: out_dims.1,
            is_keyframe: header.nal_unit_type.is_irap(),
        };
        // `used_as_reference`: every NAL unit type except the
        // "sub-layer-non-reference" ones (`*_N`, §7.4.2.2) can be
        // referenced by a later picture.
        let is_reference = !header.nal_unit_type.is_sub_layer_non_reference();

        // Release `walk` (and, with it, `inter`/`ref_pics_l0`/`collocated`'s
        // borrows of `self.dpb`) before taking `&mut self.dpb` below —
        // `pic` and `cu_grid` are moved out of `walk` first since `store`
        // and `CollocatedMotionField::build` above still needed them.
        drop(walk);

        let dpb = self.dpb.as_mut().ok_or(Error::InvalidData("vaco-codec-hevc: DPB missing after its own first use"))?;
        dpb.store(pic, meta, poc.value, hdr.pic_output, is_reference, collocated_out);
        // §C.5.2.3's own "additional bumping" — reorder/latency only, run
        // against the DPB state *after* this picture is stored (see
        // `Dpb::bump_pre_decode`'s own doc for why capacity is deliberately
        // absent here and handled earlier instead).
        let bumped = dpb.bump_post_decode(poc.value);
        Self::emit_pocs(self.dpb.as_ref(), &mut self.budget, &mut self.machine, &bumped)?;
        if let Some(dpb) = self.dpb.as_mut() {
            dpb.reap_unused(&mut self.budget);
        }
        Ok(())
    }

    /// Read `pocs` (already POC-ordered by every [`Dpb`] method that
    /// produces one) back out of `dpb` and push each as a frame into
    /// `machine`. A free function rather than a `&mut self` method: the
    /// caller still holds `self.rbsp.as_slice()`'s borrow live at some call
    /// sites (the IRAP-clear one, before the CTU walk consumes
    /// `cabac_data`), and a `&mut self` method call there would conflict
    /// with it even though the two touch disjoint fields — the borrow
    /// checker cannot see across a method boundary the way it can across
    /// plain field accesses in one function body.
    fn emit_pocs(dpb: Option<&Dpb>, budget: &mut Budget, machine: &mut vaco_codec_core::machine::Machine<vaco_frame::Frame>, pocs: &[i64]) -> Result<()> {
        let Some(dpb) = dpb else { return Ok(()) };
        for &poc in pocs {
            let (Some(pic), Some(meta)) = (dpb.picture_for_output(poc), dpb.output_meta(poc)) else { continue };
            let mut frame = pic_to_frame(budget, meta.out_width, meta.out_height, pic)?;
            frame.pts = meta.pts;
            frame.duration = meta.duration;
            if meta.is_keyframe {
                frame.flags |= vaco_frame::FrameFlags::KEY;
            }
            machine.emit(frame);
        }
        Ok(())
    }
}

/// `new`/`new_p_slice`/`new_b_slice`'s own dispatch, picked by slice type —
/// the CABAC context tables an I slice initialises are a strict subset
/// (`initType == 2`, always) of what a P or B slice needs (`initType` 0 or
/// 1, per `cabac_init_flag`, in opposite default directions for P versus B
/// — see [`ContextBank::new_b_slice`]'s own doc), so this is the one place
/// `decode_packet` has to know which of the three exists at all.
fn new_context_bank(kind: SliceKind, cabac_init: bool, qp: i8) -> ContextBank {
    match kind {
        SliceKind::I => ContextBank::new(qp),
        SliceKind::P => ContextBank::new_p_slice(qp, cabac_init),
        SliceKind::B => ContextBank::new_b_slice(qp, cabac_init),
    }
}

/// Split `cabac_data` into one byte range per CTU row, from
/// `entry_point_offsets` (§7.4.7.1's `entry_point_offset_minus1[i] + 1`,
/// already resolved to lengths by `vaco_parse_hevc::SliceHeader`).
///
/// This crate's scope (checked by `check_scope`) never has tiles and always
/// has exactly one slice segment per picture, so — unlike the general
/// `numEntryPointOffsets` derivation, which also has to account for tile
/// columns — one CTU row is exactly one substream here: `ctbs_y` rows need
/// `ctbs_y - 1` entry points (the last row's length is whatever remains).
/// A stream whose count disagrees is rejected rather than guessed at.
fn wpp_row_ranges(budget: &mut Budget, data_len: usize, offsets: &[u32], ctbs_y: u32) -> Result<Vec<(usize, usize)>> {
    let ctbs_y = usize::try_from(ctbs_y).unwrap_or(1).max(1);
    if offsets.len().saturating_add(1) != ctbs_y {
        return Err(Error::InvalidData(
            "vaco-codec-hevc: entry_point_offsets count does not match the CTU row count",
        ));
    }
    let mut ranges: Vec<(usize, usize)> = budget.alloc(ctbs_y)?;
    let mut start = 0usize;
    for (i, off) in offsets.iter().enumerate() {
        let len = usize::try_from(*off).map_err(|_| Error::InvalidData("vaco-codec-hevc: entry point offset too large"))?;
        let end = start
            .checked_add(len)
            .ok_or(Error::InvalidData("vaco-codec-hevc: entry point offset overflow"))?;
        if end > data_len {
            return Err(Error::InvalidData("vaco-codec-hevc: entry point offset exceeds the slice segment data"));
        }
        if let Some(slot) = ranges.get_mut(i) {
            *slot = (start, end);
        }
        start = end;
    }
    if let Some(slot) = ranges.get_mut(offsets.len()) {
        *slot = (start, data_len);
    }
    Ok(ranges)
}

/// Find the byte offset in `ebsp` (the *coded*, still-escaped NAL bytes) at
/// which exactly `rbsp_target_len` bytes of de-escaped RBSP have been
/// produced — i.e. the position right after de-escaping `ebsp[..result]`
/// yields an RBSP of that length.
///
/// This is the map §7.4.7.1's entry-point offsets need and `RbspBuf` does not
/// expose: `entry_point_offset_minus1[i] + 1` counts bytes of the *coded*
/// slice segment data, "emulation prevention bytes ... counted as part of
/// the slice segment data for purposes of subset identification" in the
/// specification's own words, whereas `cabac_data` (and every byte offset
/// derived from it) lives in the de-escaped RBSP `RbspBuf::fill` already
/// stripped emulation-prevention bytes out of. The two only coincide when no
/// `00 00 03` escape occurs before the position in question.
fn ebsp_offset_for_rbsp_len(ebsp: &[u8], rbsp_target_len: usize) -> usize {
    let mut zeros = 0u32;
    let mut produced = 0usize;
    for (i, &b) in ebsp.iter().enumerate() {
        if produced == rbsp_target_len {
            return i;
        }
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        produced += 1;
    }
    ebsp.len()
}

/// Decode a WPP-enabled slice segment's CTU rows, ITU-T H.265 §9.3.2.3.
///
/// Each CTU row is its own CABAC substream, split from the *coded* slice
/// segment data (`ebsp`, from `ebsp_offset_for_rbsp_len(ebsp,
/// header_rbsp_len)` onward — see that function's own doc for why entry-point
/// offsets must be applied there and not to the de-escaped `cabac_data`) via
/// `wpp_row_ranges`, then de-escaped independently per row
/// (`vaco_bitstream::annexb::to_rbsp`) before a fresh [`CabacDecoder`] reads
/// it: the arithmetic-decoding engine always reinitialises at a substream's
/// first byte (clause 9.3.1.2's own init, same as slice start), but the
/// *context* state does not — row 0 starts from the same
/// `new_context_bank(kind, cabac_init, qp)` a non-WPP slice would, while
/// every later row either inherits a saved snapshot or, if no snapshot is
/// available, also starts fresh.
///
/// The snapshot a row inherits is the context state as it stood **right
/// after the row above finished decoding its own second CTU**, column index
/// one — never carried forward from that row's own last CTU, and never
/// taken at all when `ctbs_x < 2` (no second column exists, matching HM's
/// `TDecSlice::decompressSlice`: `pCtuUp && (ctuRsAddr % frameWidthInCtus + 1)
/// < frameWidthInCtus` is exactly "a column-1 CTU exists in the row above").
/// `ContextBank` is `Copy`, so "snapshot" is a plain assignment, not a
/// serialisation format of its own.
#[allow(clippy::too_many_arguments, reason = "one call site (decode_packet); a sub-struct would not aid clarity")]
fn decode_wpp_rows(
    budget: &mut Budget,
    ebsp: &[u8],
    header_rbsp_len: usize,
    entry_point_offsets: &[u32],
    walk: &mut Ctx<'_>,
    ctbs_x: u32,
    ctbs_y: u32,
    ctb_size_i: i32,
    qp: i8,
    kind: SliceKind,
    cabac_init: bool,
) -> Result<()> {
    let data_start = ebsp_offset_for_rbsp_len(ebsp, header_rbsp_len);
    let ebsp_slice_data = ebsp.get(data_start..).unwrap_or(&[]);
    let row_ranges = wpp_row_ranges(budget, ebsp_slice_data.len(), entry_point_offsets, ctbs_y)?;
    let mut saved_ctx: Option<ContextBank> = None;
    // Reused across rows: `to_rbsp` clears it on every call, and each row's
    // `CabacDecoder` borrow ends (the row's CTU loop finishes) before the
    // next row refills it.
    let mut row_rbsp: Vec<u8> = Vec::new();

    for (row_idx, &(start, end)) in row_ranges.iter().enumerate() {
        let row_ebsp = ebsp_slice_data
            .get(start..end)
            .ok_or(Error::InvalidData("vaco-codec-hevc: entry point range out of bounds"))?;
        let row_bytes = vaco_bitstream::annexb::to_rbsp(row_ebsp, &mut row_rbsp);
        let mut cabac = CabacDecoder::new(row_bytes);
        // §8.6.1's `qPY_PREV` resets to `SliceQpY` at the start of every CTB
        // row when WPP is active (the same rule §9.3.2.3's own context reset
        // does not apply to — this is a *different* per-row reset, of
        // `cu_qp_delta`'s running QP prediction rather than CABAC context
        // state). The very first CTU of every row's own `coding_quadtree`
        // call always re-derives `qg_qp_pred`/`cu_qp_delta_val` fresh (see
        // that function's own QG-reset comment), so nothing else needs
        // resetting here.
        walk.qp_y_prev = walk.slice_qp;
        let mut ctx = if row_idx > 0 && ctbs_x >= 2 {
            saved_ctx.unwrap_or_else(|| new_context_bank(kind, cabac_init, qp))
        } else {
            new_context_bank(kind, cabac_init, qp)
        };

        let row_u32 = u32::try_from(row_idx).unwrap_or(0);
        let mut stop = false;
        for col in 0..ctbs_x {
            let addr = row_u32.saturating_mul(ctbs_x).saturating_add(col);
            let cx = i32::try_from(col).unwrap_or(0) * ctb_size_i;
            let cy = i32::try_from(row_idx).unwrap_or(0) * ctb_size_i;
            ctu::decode_ctu(&mut cabac, &mut ctx, walk, cx, cy, addr)?;

            // §9.3.2.3: the context state is stored once a row's own second
            // CTU (column index 1) has finished, for the row below to load —
            // not the row's own last CTU, and independent of the
            // end_of_slice_segment_flag/end_of_subset_one_bit terminate
            // calls below, neither of which mutates any context.
            if col == 1 {
                saved_ctx = Some(ctx);
            }

            let terminate = cabac.decode_terminate();
            if cabac.malformed() {
                return Err(Error::InvalidData("vaco-codec-hevc: CABAC decode ran past the slice segment data"));
            }
            if terminate != 0 {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }
    }
    Ok(())
}

/// Refuse, up front, every combination this crate does not implement — see
/// the crate doc for the complete, stated list.
fn check_scope(sps: &Sps, pps: &Pps) -> Result<()> {
    let unsupported = |why: &'static str| Err(Error::Unsupported(why));
    if sps.chroma_format != ChromaFormat::Yuv420 {
        return unsupported("vaco-codec-hevc: only 4:2:0 chroma is decoded");
    }
    // `sample_adaptive_offset_enabled_flag` is no longer refused here: the
    // per-CTU `sao()` syntax (§7.3.8.3) is now parsed by `ctu::decode_ctu`
    // (via `crate::sao::parse_ctu_sao`) and applied by
    // `crate::sao::filter_picture` after deblocking — see `sao`'s own
    // module doc.
    if sps.bit_depth_luma != 8 || sps.bit_depth_chroma != 8 {
        return unsupported("vaco-codec-hevc: only 8-bit samples are decoded");
    }
    if sps.separate_colour_plane {
        return unsupported("vaco-codec-hevc: separate_colour_plane_flag is not supported");
    }
    if sps.scaling_list_enabled {
        return unsupported("vaco-codec-hevc: custom scaling lists are not supported (flat scaling only)");
    }
    if sps.pcm.is_some() {
        return unsupported("vaco-codec-hevc: I_PCM is not supported");
    }
    if sps.range_extension.is_some() {
        return unsupported("vaco-codec-hevc: SPS range-extension flags are not supported");
    }
    if sps.scc_extension.is_some() {
        return unsupported("vaco-codec-hevc: screen-content-coding extensions are not supported");
    }
    if pps.tiles.is_some() {
        return unsupported("vaco-codec-hevc: tiles are not supported");
    }
    if pps.transquant_bypass_enabled {
        return unsupported("vaco-codec-hevc: transquant_bypass is not supported");
    }
    if pps.range_extension.is_some() {
        return unsupported("vaco-codec-hevc: PPS range-extension flags are not supported");
    }
    if pps.scc_extension.is_some() {
        return unsupported("vaco-codec-hevc: screen-content-coding extensions are not supported");
    }
    Ok(())
}

fn pic_to_frame(budget: &mut Budget, width: u32, height: u32, pic: &Picture) -> Result<vaco_frame::Frame> {
    let pix_fmt = vaco_pixfmt::PixFmt::from_name("yuv420p")
        .map_err(|_| Error::InvalidData("vaco-codec-hevc: yuv420p pixel format missing"))?;
    let mut frame = vaco_frame::Frame::alloc_video(budget, pix_fmt, width, height)?;
    blit(&pic.y, &mut frame, 0, width as usize, height as usize);
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    blit(&pic.cb, &mut frame, 1, cw, ch);
    blit(&pic.cr, &mut frame, 2, cw, ch);
    Ok(frame)
}

fn blit(src: &crate::framebuf::Plane, frame: &mut vaco_frame::Frame, plane_index: usize, width: usize, height: usize) {
    let Some(mut dst) = frame.plane_mut(plane_index) else { return };
    for y in 0..height.min(dst.rows()) {
        let Some(row) = dst.row_mut(y) else { continue };
        for x in 0..width.min(row.len()) {
            let v = src.get(x, y);
            if let Some(b) = row.get_mut(x) {
                *b = u8::try_from(v).unwrap_or(0);
            }
        }
    }
}

impl Decoder for HevcDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match self.machine.accept(packet.is_none())? {
            vaco_codec_core::machine::Accept::Drain => {
                // Flush whatever the DPB is still holding, in POC order,
                // before telling `self.machine` there is nothing left —
                // otherwise a stream's last few pictures (still pending
                // purely because reordering held them back) would be
                // silently dropped rather than delayed.
                if let Some(dpb) = self.dpb.as_ref() {
                    let pending = dpb.flush();
                    Self::emit_pocs(Some(dpb), &mut self.budget, &mut self.machine, &pending)?;
                }
                self.machine.finish();
                Ok(())
            }
            vaco_codec_core::machine::Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                self.decode_packet(pkt)
            }
        }
    }

    fn receive_frame(&mut self) -> Result<vaco_frame::Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.parser.flush();
        self.dpb = None;
        self.poc_state.reset();
        // Release every byte charged to the budget along with the state
        // that held them, mirroring `H264Decoder::flush`'s own precedent.
        self.budget = Budget::new(self.limits.clone());
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.parser.set_extradata(extradata)?;
        Ok(())
    }
}
