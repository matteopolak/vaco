//! [`HevcDecoder`] — the [`Decoder`] this crate registers.
//!
//! # Input framing
//!
//! MP4/Matroska store length-prefixed samples (`hvcC`); a raw `.hevc`
//! elementary stream or MPEG-TS payload uses Annex B. [`HevcParser`] detects
//! the framing from extradata and owns VPS/SPS/PPS state;
//! [`vaco_format_nalu::units`] walks either representation. Decode input does
//! not use `hevc_mp4toannexb`, whose job is output-side muxer reframing.
//!
//! # Access-unit assembly
//!
//! [`HevcParser::push_access_unit`] handles parameter sets and slice headers.
//! The decoder then re-scans the same NAL units only to reach the primary
//! coded-picture slice bits that the parser does not return.
//!
//! # Output reordering (`Caps::DELAY` / `Caps::SUBFRAMES`)
//!
//! P-slices can decode and display in different orders under Annex C.5.2's
//! bumping process. `Caps::DELAY` permits a drain send to flush the DPB;
//! `Caps::SUBFRAMES` permits one packet to emit zero or several pictures.
//! Weighted prediction is implemented (§8.5.3.3.4.3), while B-slices are
//! refused by [`decode_packet`]'s slice-kind check.

use vaco_codec_cabac::CabacDecoder;
use vaco_codec_core::Caps;
use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_format_nalu::{RbspBuf, units};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_hevc::{
    ChromaFormat, HevcNalHeader, HevcParser, PocState, Pps, SliceHeader, SliceKind, Sps,
    cc_data_from_sei, sei,
};

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
    /// POC of the active IRAP whose `NoRaslOutputFlag` suppresses leading
    /// RASL pictures from output while retaining them for decoding.
    rasl_output_suppression_poc: Option<i64>,
    /// An EOS/EOB NAL unit makes the next CRA an independent random-access
    /// point, whose old DPB pictures are discarded without output.
    sequence_ended: bool,
    /// Test-only: when set, `decode_packet` runs [`run_deblock_lag_probe`]
    /// against the just-reconstructed (pre-deblock) picture right before
    /// its own real `deblock::filter_picture` call, then clears this back
    /// to `None` and leaves [`HevcDecoder::deblock_lag_probe_result`]
    /// filled in for the test to read after `send_packet` returns. Never
    /// affects the real decode: the production `deblock::filter_picture`
    /// call immediately below runs on the untouched `walk` exactly as
    /// before, the probe works only on throwaway clones.
    #[cfg(test)]
    pub(crate) deblock_lag_probe: Option<DeblockLagProbe>,
    #[cfg(test)]
    pub(crate) deblock_lag_probe_result: Option<Vec<DeblockLagResult>>,
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
            rasl_output_suppression_poc: None,
            sequence_ended: false,
            limits,
            #[cfg(test)]
            deblock_lag_probe: None,
            #[cfg(test)]
            deblock_lag_probe_result: None,
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
        let ends_sequence = units(payload, framing).any(|nal| {
            HevcNalHeader::parse(nal.data).is_some_and(|header| {
                header.is_base_layer()
                    && (header.nal_unit_type == vaco_parse_hevc::NalUnitType::EOS_NUT
                        || header.nal_unit_type == vaco_parse_hevc::NalUnitType::EOB_NUT)
            })
        });
        if info.picture_type.is_none() {
            if ends_sequence {
                self.note_sequence_end();
            }
            return Ok(());
        }

        // Locate this access unit's one primary-coded-picture slice —
        // `vaco_format_nalu::units` is the same reusable, `Framing`-aware
        // NAL-unit iterator `HevcParser` itself is built on, not a second
        // parser.
        let mut slice_nal: Option<&[u8]> = None;
        let mut slice_count = 0u32;
        // ATSC A/53 closed captions (interface gap 18's attachment half —
        // extraction is `vaco_parse_hevc::a53`, already landed). See
        // `vaco-codec-h264::decoder`'s identical comment for why this
        // concatenates across every SEI NAL rather than assuming one.
        let mut cc_data = Vec::new();
        // Mastering-display and content-light-level SEI messages attach to
        // the decoded picture; the last one of each type in the access unit
        // wins.
        let mut mastering_display = None;
        let mut content_light = None;
        for nal in units(payload, framing) {
            let Some(header) = HevcNalHeader::parse(nal.data) else {
                continue;
            };
            if !header.is_base_layer() {
                continue;
            }
            if header.nal_unit_type.has_slice_header() {
                slice_count += 1;
                if slice_nal.is_none() {
                    slice_nal = Some(nal.data);
                }
            } else if header.nal_unit_type.is_sei() {
                self.rbsp.fill(nal.data, &mut self.budget)?;
                if let Ok(messages) = sei::parse(self.rbsp.as_slice(), None, &mut self.budget) {
                    for msg in &messages {
                        if let Some(triplets) = cc_data_from_sei(&msg.payload) {
                            cc_data.extend_from_slice(triplets);
                        }
                        match msg.payload {
                            vaco_parse_hevc::SeiPayload::MasteringDisplay {
                                primaries,
                                white_point,
                                max_luminance,
                                min_luminance,
                            } => {
                                mastering_display = Some(mastering_display_from_sei(
                                    primaries,
                                    white_point,
                                    max_luminance,
                                    min_luminance,
                                ));
                            }
                            vaco_parse_hevc::SeiPayload::ContentLightLevel {
                                max_content_light_level,
                                max_pic_average_light_level,
                            } => {
                                content_light = Some((
                                    u32::from(max_content_light_level),
                                    u32::from(max_pic_average_light_level),
                                ));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        if slice_count > 1 {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: more than one slice segment per picture is not supported",
            ));
        }
        let Some(ebsp) = slice_nal else { return Ok(()) };
        let header = HevcNalHeader::parse(ebsp)
            .ok_or(Error::InvalidData("vaco-codec-hevc: empty NAL unit"))?;
        if rasl_can_be_ignored(header.nal_unit_type, self.rasl_output_suppression_poc) {
            return Ok(());
        }

        self.rbsp.fill(ebsp, &mut self.budget)?;
        let rbsp = self.rbsp.as_slice();

        let pps_id = vaco_parse_hevc::slice::peek_pps_id(rbsp).ok_or(Error::InvalidData(
            "vaco-codec-hevc: slice segment header truncated before pps_id",
        ))?;
        // Cloned rather than borrowed: the CTU walk below needs `&mut
        // self.budget` for the rest of this function, which a borrow of
        // `self.parser`'s tables held open across that call would conflict
        // with.
        let (pps, sps) = {
            let (p, s) =
                self.parser
                    .parameter_sets()
                    .sps_for_pps(pps_id)
                    .ok_or(Error::Unsupported(
                        "vaco-codec-hevc: referenced PPS/SPS not seen yet",
                    ))?;
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
        // are implemented and measured byte-exact against plain `ffmpeg` on
        // a fully-stock `libx265` encode (no `-x265-params` restrictions at
        // all) at multiple resolutions, plus dedicated fixtures for deep
        // hierarchical-B GOPs and explicit weighted bi-prediction
        // (`weightb=1`) — see `docs/codec/vaco-codec-hevc.md`'s B-slice
        // section for the full measured sweep. The refusal is lifted.
        if hdr.dependent {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: dependent slice segments are not supported",
            ));
        }
        if !hdr.first_slice_segment_in_pic {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: multiple slice segments per picture are not supported",
            ));
        }

        reader.align();
        let cabac_data = reader.remaining_bytes();

        let slice_qp = 26 + pps.init_qp_minus26 + hdr.qp_delta;

        let width = usize::try_from(sps.pic_width_in_luma_samples).unwrap_or(0);
        let height = usize::try_from(sps.pic_height_in_luma_samples).unwrap_or(0);
        let mut pic = Picture::new(&mut self.budget, width, height)?;
        // PERF-PROGRAMME.md item B4, Stage 1: the CTU walk itself
        // reconstructs into `recon` (a per-CTU-row-banded buffer -- see
        // `framebuf::ReconPlane`'s own module doc), not `pic` directly.
        // `pic` is materialized from `recon` once the whole walk is done,
        // immediately before deblocking, and is what deblocking/SAO/
        // emission (and every future picture's own reference reads) keep
        // using exactly as before.
        let ctb_size = usize::try_from(
            1u32 << (u32::from(sps.log2_min_cb_size) + u32::from(sps.log2_diff_max_min_cb_size)),
        )
        .unwrap_or(1)
        .max(1);
        let ctb_size_u32 = u32::try_from(ctb_size).unwrap_or(1).max(1);
        let ctbs_x = u32::try_from(width)
            .unwrap_or(0)
            .div_ceil(ctb_size_u32)
            .max(1);
        let ctbs_y = u32::try_from(height)
            .unwrap_or(0)
            .div_ceil(ctb_size_u32)
            .max(1);
        // Stage 2b's "try `std::thread::scope`" resolution
        // (`docs/codec/hevc-wavefront-threading.md`): every one of these
        // five `*Shared` boards is now a plain local, owned by this same
        // stack frame for exactly as long as `walk`/`recon`/`cu_grid`/
        // `edges`/`sao_params` (which all borrow from one) are alive below
        // — no `Arc`, no reference counting, matching `std::thread::scope`'s
        // own borrowing shape ahead of step 4's real dispatch.
        let recon_shared =
            crate::framebuf::ReconPictureShared::new(&mut self.budget, width, height, ctb_size)?;
        let mut recon = crate::framebuf::ReconPicture::new(&recon_shared);
        let cu_grid_shared =
            crate::framebuf::CuGridShared::new(width, height, hdr.kind == SliceKind::B, ctb_size);
        let cu_grid = CuGrid::new(&mut self.budget, &cu_grid_shared)?;
        let edges_shared = crate::framebuf::EdgeMarksShared::new(width, height, ctb_size);
        let edges = crate::framebuf::EdgeMarks::new(&edges_shared);
        let sao_params_shared = sao::SaoParamsGridShared::new(ctbs_x, ctbs_y);
        let sao_params = sao::SaoParamsGrid::new(&mut self.budget, &sao_params_shared)?;

        // ITU-T H.265 §8.3.1: this picture's own picture order count, and
        // (§8.1) whether it is the one IRAP in its sequence that clears
        // RASL output rather than merely refreshing prediction.
        let no_rasl_output = header.nal_unit_type.is_idr()
            || header.nal_unit_type.is_bla()
            || (header.nal_unit_type.is_cra() && !self.poc_state.started());
        let poc = self
            .poc_state
            .advance_with(&sps, &hdr, header.temporal_id, no_rasl_output);
        if header.nal_unit_type.is_irap() {
            self.rasl_output_suppression_poc = no_rasl_output.then_some(poc.value);
        }

        let max_dec_pic_buffering = usize::try_from(sps.max_dec_pic_buffering())
            .unwrap_or(1)
            .max(1);
        let max_num_reorder_pics = usize::try_from(sps.max_num_reorder_pics()).unwrap_or(0);
        let max_latency_increase = sps.max_latency_increase_plus1.last().copied().unwrap_or(0);
        let dpb = self.dpb.get_or_insert_with(|| {
            Dpb::new(
                max_dec_pic_buffering,
                max_num_reorder_pics,
                max_latency_increase,
            )
        });

        let sets = crate::dpb::derive_reference_pic_sets(
            poc.value,
            hdr.short_term_rps.as_ref(),
            !hdr.long_term_refs.is_empty(),
        )?;

        // §C.5.2.2: an IRAP with `NoRaslOutputFlag` bumps everything still
        // pending (unless `no_output_of_prior_pics_flag` says to drop it
        // silently) and then empties the DPB outright, before this
        // picture's own reference-picture-set marking (which would be
        // meaningless against an empty DPB anyway) runs.
        if header.nal_unit_type.is_irap() && no_rasl_output {
            let discard_prior_output = hdr.no_output_of_prior_pics || self.sequence_ended;
            let pocs = dpb.clear_for_irap(discard_prior_output);
            Self::emit_pocs(
                self.dpb.as_ref(),
                &mut self.budget,
                &mut self.machine,
                &pocs,
            )?;
            if let Some(dpb) = self.dpb.as_mut() {
                dpb.clear_all(&mut self.budget);
            }
            self.sequence_ended = false;
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
            Self::emit_pocs(
                self.dpb.as_ref(),
                &mut self.budget,
                &mut self.machine,
                &pre_bumped,
            )?;
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
                .filter_map(|&p| {
                    dpb_ref
                        .and_then(|d| d.reference_picture(p))
                        .map(|pic| RefPic { poc: p, pic })
                })
                .collect();
            let ref_pics_l1: Vec<RefPic<'_>> = list1
                .iter()
                .filter_map(|&p| {
                    dpb_ref
                        .and_then(|d| d.reference_picture(p))
                        .map(|pic| RefPic { poc: p, pic })
                })
                .collect();
            // §8.5.3.2.9: `ColPic` is named from `RefPicList0` or
            // `RefPicList1` depending on `collocated_from_l0_flag` — a P
            // slice's own parser default (`collocated_from_l0 == true`)
            // makes this collapse to the pre-existing `list0`-only lookup.
            let collocated = if hdr.temporal_mvp_enabled {
                let col_list = if hdr.collocated_from_l0 {
                    &list0
                } else {
                    &list1
                };
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
            let max_num_merge_cand =
                usize::try_from(5u32.saturating_sub(hdr.five_minus_max_num_merge_cand))
                    .unwrap_or(1)
                    .max(1);
            // §8.5.3.3.4.1's `weightedPredFlag`: `weighted_pred_flag` for a P
            // slice, `weighted_bipred_flag` for a B slice — resolved once per
            // slice rather than per PU. `pred_weight_table()`'s own presence
            // condition in `vaco-parse-hevc` already gates on exactly this,
            // so `hdr.pred_weight_table.is_some()` alone is enough; no
            // separate flag check is needed here.
            let weights_l0 = hdr.pred_weight_table.as_ref().map(|t| {
                crate::weight::resolve_list(
                    t,
                    0,
                    ref_pics_l0.len(),
                    u32::from(sps.bit_depth_luma),
                    u32::from(sps.bit_depth_chroma),
                )
            });
            let weights_l1 = if is_b {
                hdr.pred_weight_table.as_ref().map(|t| {
                    crate::weight::resolve_list(
                        t,
                        1,
                        ref_pics_l1.len(),
                        u32::from(sps.bit_depth_luma),
                        u32::from(sps.bit_depth_chroma),
                    )
                })
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

        let ctx_shared = crate::ctu::CtxShared::new(
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
        let mut walk = Ctx::new(
            &ctx_shared,
            &mut pic,
            &mut recon,
            cu_grid,
            edges,
            sao_params,
        );

        let qp_i8 = i8::try_from(slice_qp.clamp(0, 51)).unwrap_or(0);
        let ctb_size_i = i32::try_from(ctb_size_u32).unwrap_or(0);

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
                let row_us = usize::try_from(row).unwrap_or(0);
                let col_us = usize::try_from(col).unwrap_or(0);
                if col == 0 {
                    walk.edges.begin_row(row_us)?;
                    walk.cu_grid.begin_row(&mut self.budget, row_us)?;
                    walk.sao_params.begin_row(&mut self.budget, row_us)?;
                }
                walk.recon.begin_ctu(row_us, col_us)?;
                let cx = i32::try_from(col).unwrap_or(0) * ctb_size_i;
                let cy = i32::try_from(row).unwrap_or(0) * ctb_size_i;
                ctu::decode_ctu(&mut cabac, &mut ctx, &mut walk, cx, cy, addr)?;
                walk.recon.publish_ctu(row_us, col_us)?;
                let end = cabac.decode_terminate();
                if cabac.malformed() {
                    return Err(Error::InvalidData(
                        "vaco-codec-hevc: CABAC decode ran past the slice segment data",
                    ));
                }
                if end != 0 {
                    break;
                }
            }
        }

        // The CTU walk is done; hand the finished, tiled reconstruction
        // over to the plain `Picture` deblocking/SAO/emission (and every
        // later picture's own reference reads) already know how to use —
        // see `framebuf::ReconPlane`'s own module doc for why this is a
        // one-time copy rather than `recon` itself growing into `pic`.
        walk.recon.finish()?;
        walk.recon.materialize_into(walk.pic);
        walk.edges.finish()?;
        walk.cu_grid.finish()?;
        walk.sao_params.finish()?;

        #[cfg(test)]
        if let Some(probe) = self.deblock_lag_probe.take() {
            let results = run_deblock_lag_probe(&walk, &probe, &mut self.budget);
            self.deblock_lag_probe_result = Some(results);
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
            Some(CollocatedMotionField::build(
                poc.value,
                width,
                height,
                |x, y| walk.cu_grid.inter_at(x, y),
            ))
        };

        let crop = sps.crop_origin();
        let out_dims = sps.dimensions().unwrap_or((
            sps.pic_width_in_luma_samples,
            sps.pic_height_in_luma_samples,
        ));
        let meta = PictureMeta {
            pts: pkt.pts,
            duration: pkt.duration,
            out_width: out_dims.0,
            out_height: out_dims.1,
            crop_x: crop.0.0,
            crop_y: crop.0.1,
            crop_cx: crop.1.0,
            crop_cy: crop.1.1,
            is_keyframe: header.nal_unit_type.is_irap(),
            closed_captions: cc_data,
            color: sps.color_info(),
            mastering_display,
            content_light,
        };
        // §C.3.4 stores the current decoded picture marked "used for
        // short-term reference" whatever its NAL unit type; §8.3.2's
        // reference-picture-set marking, one picture later, is the only
        // thing that makes a picture unused. §7.4.2.2's `*_N` types are not
        // an instruction to the DPB: a sub-layer non-reference picture is
        // barred only from the RPS of pictures with the **same**
        // `TemporalId`, and a higher-`TemporalId` picture may reference it.
        // This used to store them `Unused`, which dropped exactly those --
        // and because `RefPicList0` is assembled below with a `filter_map`,
        // the missing entry did not fail, it *shifted every later index*,
        // so the prediction read a different picture. Invisible to any
        // `libx265` fixture: measured across `--temporal-layers 1` to `4`,
        // it only ever emits `*_N` pictures in the top sub-layer, where by
        // construction nothing references them.
        let is_reference = true;

        // `walk` owns two `Budget`-tracked working buffers — `cu_grid` and
        // `sao_params` — that nothing outside this one slice's decode ever
        // reads again (the collocated motion field built above already
        // copied out of `cu_grid` everything a later picture's TMVP could
        // need, at its own, un-tracked, always-smaller footprint — see
        // `CollocatedMotionField::build`'s own doc). Releasing their charge
        // back to `self.budget` here, before dropping `walk`, is required:
        // `Budget::release` is never automatic (`vaco-limits`' own "gotcha:
        // releasing" — nothing but a dropped `Reservation` releases on its
        // own, and neither of these was allocated as one), so without this
        // every decoded picture — not just ones the `Dpb` still holds —
        // would add `O(picture size)` to `committed` and never give it back,
        // which is exactly what made a stock `libx265` encode fail
        // `max_alloc_total` past roughly 640x480 (see `CuGrid::budget_bytes`'s
        // own doc for the measured shape of that failure).
        self.budget.release(walk.working_budget_bytes());
        // Release `walk` itself (and, with it, `inter`/`ref_pics_l0`/
        // `collocated`'s borrows of `self.dpb`) before taking `&mut
        // self.dpb` below — `pic` was never inside `walk` (`Ctx::pic` is a
        // `&mut Picture` borrow of this function's own local, not an owned
        // field), so only `cu_grid`/`sao_params`/`edges` actually drop here;
        // `pic` moves into `dpb.store` a few lines down instead.
        drop(walk);

        let dpb = self.dpb.as_mut().ok_or(Error::InvalidData(
            "vaco-codec-hevc: DPB missing after its own first use",
        ))?;
        let needed_for_output = output_is_needed(
            hdr.pic_output,
            header.nal_unit_type,
            poc.value,
            self.rasl_output_suppression_poc,
        );
        dpb.store(
            pic,
            meta,
            poc.value,
            needed_for_output,
            is_reference,
            collocated_out,
        );
        // §C.5.2.3's own "additional bumping" — reorder/latency only, run
        // against the DPB state *after* this picture is stored (see
        // `Dpb::bump_pre_decode`'s own doc for why capacity is deliberately
        // absent here and handled earlier instead).
        let bumped = dpb.bump_post_decode(poc.value);
        Self::emit_pocs(
            self.dpb.as_ref(),
            &mut self.budget,
            &mut self.machine,
            &bumped,
        )?;
        if let Some(dpb) = self.dpb.as_mut() {
            dpb.reap_unused(&mut self.budget);
        }
        if ends_sequence {
            self.note_sequence_end();
        }
        Ok(())
    }

    fn note_sequence_end(&mut self) {
        self.poc_state.reset();
        self.rasl_output_suppression_poc = None;
        self.sequence_ended = true;
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
    fn emit_pocs(
        dpb: Option<&Dpb>,
        budget: &mut Budget,
        machine: &mut vaco_codec_core::machine::Machine<vaco_frame::Frame>,
        pocs: &[i64],
    ) -> Result<()> {
        let Some(dpb) = dpb else { return Ok(()) };
        for &poc in pocs {
            let (Some(pic), Some(meta)) = (dpb.picture_for_output(poc), dpb.output_meta(poc))
            else {
                continue;
            };
            let mut frame = pic_to_frame(
                budget,
                meta.out_width,
                meta.out_height,
                (meta.crop_x as usize, meta.crop_y as usize),
                (meta.crop_cx as usize, meta.crop_cy as usize),
                pic,
            )?;
            frame.pts = meta.pts;
            frame.duration = meta.duration;
            frame.color = meta.color;
            if meta.is_keyframe {
                frame.flags |= vaco_frame::FrameFlags::KEY;
            }
            if !meta.closed_captions.is_empty() {
                let buffer = vaco_pool::Buffer::from_slice(budget, &meta.closed_captions)?;
                frame.set_side_data(vaco_frame::FrameSideData::ClosedCaptions(buffer));
            }
            if let Some(mastering_display) = meta.mastering_display {
                frame.set_side_data(vaco_frame::FrameSideData::MasteringDisplay(Box::new(
                    mastering_display,
                )));
            }
            if let Some((max_cll, max_fall)) = meta.content_light {
                frame.set_side_data(vaco_frame::FrameSideData::ContentLightLevel {
                    max_cll,
                    max_fall,
                });
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
fn wpp_row_ranges(
    budget: &mut Budget,
    data_len: usize,
    offsets: &[u32],
    ctbs_y: u32,
) -> Result<Vec<(usize, usize)>> {
    let ctbs_y = usize::try_from(ctbs_y).unwrap_or(1).max(1);
    if offsets.len().saturating_add(1) != ctbs_y {
        return Err(Error::InvalidData(
            "vaco-codec-hevc: entry_point_offsets count does not match the CTU row count",
        ));
    }
    let mut ranges: Vec<(usize, usize)> = budget.alloc(ctbs_y)?;
    let mut start = 0usize;
    for (i, off) in offsets.iter().enumerate() {
        let len = usize::try_from(*off)
            .map_err(|_| Error::InvalidData("vaco-codec-hevc: entry point offset too large"))?;
        let end = start.checked_add(len).ok_or(Error::InvalidData(
            "vaco-codec-hevc: entry point offset overflow",
        ))?;
        if end > data_len {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: entry point offset exceeds the slice segment data",
            ));
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
#[allow(
    clippy::too_many_arguments,
    reason = "one call site (decode_packet); a sub-struct would not aid clarity"
)]
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
    // `row_ranges` is pure per-slice working state — nothing outside the row
    // loop below ever reads it again. Its charge is released unconditionally
    // once that loop (moved into `decode_wpp_row_ranges` for exactly this
    // reason) returns, success *or* error — a single release site here
    // rather than duplicating `budget.release` at every early-return buried
    // in that loop, which would only need one to be missed (today or in a
    // future edit) to leak again. Same "working buffer this crate's own
    // `Budget` accounting must not let ride past its real lifetime" shape as
    // `CuGrid`/`sao_params`/`Snapshot`, just far smaller (`O(ctbs_y)` rather
    // than `O(picture size)`, since it is only ever one `(usize, usize)` per
    // CTU row).
    let row_ranges_bytes = u64::try_from(row_ranges.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(std::mem::size_of::<(usize, usize)>()).unwrap_or(u64::MAX));
    let result = decode_wpp_row_ranges(
        budget,
        &row_ranges,
        ebsp_slice_data,
        walk,
        ctbs_x,
        ctb_size_i,
        qp,
        kind,
        cabac_init,
    );
    budget.release(row_ranges_bytes);
    result
}

/// The row-by-row CABAC decode `decode_wpp_rows` splits out purely so that
/// function can release `row_ranges`'s own `Budget` charge on every exit
/// path (this one's `Result` return, not an early `return` buried in the
/// loop below) — see that caller's own comment.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors decode_wpp_rows's own signature, one call site"
)]
fn decode_wpp_row_ranges(
    budget: &mut Budget,
    row_ranges: &[(usize, usize)],
    ebsp_slice_data: &[u8],
    walk: &mut Ctx<'_>,
    ctbs_x: u32,
    ctb_size_i: i32,
    qp: i8,
    kind: SliceKind,
    cabac_init: bool,
) -> Result<()> {
    // Row r's own context state, as it stood right after CTU column 1
    // finished (§9.3.2.3) -- published once per row into a fixed-size board
    // (Stage 2b step 2, `docs/codec/hevc-wavefront-threading.md`: the same
    // `RowPublish` primitive `EdgeMarks`/`SaoParamsGrid`/`CuGrid` already
    // publish through, `ContextBank` being `Copy` making this the smallest
    // of the four uses) so row r + 1 can read it back. Still driven by one
    // worker, one row at a time, exactly as `saved_ctx` (the plain local
    // variable this replaces) always was -- the only change is that the
    // handoff is now expressed through the primitive Stage 2's real
    // dispatch will need regardless, rather than through a local that only
    // works because nothing else could observe it mid-flight.
    let ctx_handoff: crate::wavefront::RowPublish<ContextBank> =
        crate::wavefront::RowPublish::new(row_ranges.len());
    // Reused across rows: `to_rbsp` clears it on every call, and each row's
    // `CabacDecoder` borrow ends (the row's CTU loop finishes) before the
    // next row refills it.
    let mut row_rbsp: Vec<u8> = Vec::new();

    for (row_idx, &(start, end)) in row_ranges.iter().enumerate() {
        let row_ebsp = ebsp_slice_data.get(start..end).ok_or(Error::InvalidData(
            "vaco-codec-hevc: entry point range out of bounds",
        ))?;
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
        walk.qp_y_prev = walk.shared.slice_qp;
        walk.edges.begin_row(row_idx)?;
        walk.cu_grid.begin_row(budget, row_idx)?;
        walk.sao_params.begin_row(budget, row_idx)?;
        let mut ctx = if row_idx > 0 && ctbs_x >= 2 {
            // Row `row_idx - 1`'s own publish, from this same loop's
            // previous iteration -- always `Some` by construction (every
            // earlier row already ran its own `col == 1` publish below
            // before this row starts), `new_context_bank` only ever a
            // fallback for the cases the outer `if` already excludes.
            ctx_handoff
                .get(row_idx.saturating_sub(1))
                .copied()
                .unwrap_or_else(|| new_context_bank(kind, cabac_init, qp))
        } else {
            new_context_bank(kind, cabac_init, qp)
        };

        let row_u32 = u32::try_from(row_idx).unwrap_or(0);
        let mut stop = false;
        for col in 0..ctbs_x {
            let col_us = usize::try_from(col).unwrap_or(0);
            walk.recon.begin_ctu(row_idx, col_us)?;
            let addr = row_u32.saturating_mul(ctbs_x).saturating_add(col);
            let cx = i32::try_from(col).unwrap_or(0) * ctb_size_i;
            let cy = i32::try_from(row_idx).unwrap_or(0) * ctb_size_i;
            ctu::decode_ctu(&mut cabac, &mut ctx, walk, cx, cy, addr)?;
            walk.recon.publish_ctu(row_idx, col_us)?;

            // §9.3.2.3: the context state is stored once a row's own second
            // CTU (column index 1) has finished, for the row below to load —
            // not the row's own last CTU, and independent of the
            // end_of_slice_segment_flag/end_of_subset_one_bit terminate
            // calls below, neither of which mutates any context.
            if col == 1 {
                ctx_handoff.publish(row_idx, ctx)?;
            }

            let terminate = cabac.decode_terminate();
            if cabac.malformed() {
                return Err(Error::InvalidData(
                    "vaco-codec-hevc: CABAC decode ran past the slice segment data",
                ));
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
    if sps.range_extension.is_some() {
        return unsupported("vaco-codec-hevc: SPS range-extension flags are not supported");
    }
    if sps.scc_extension.is_some() {
        return unsupported("vaco-codec-hevc: screen-content-coding extensions are not supported");
    }
    if pps.tiles.is_some() {
        return unsupported("vaco-codec-hevc: tiles are not supported");
    }
    if pps.range_extension.is_some() {
        return unsupported("vaco-codec-hevc: PPS range-extension flags are not supported");
    }
    if pps.scc_extension.is_some() {
        return unsupported("vaco-codec-hevc: screen-content-coding extensions are not supported");
    }
    Ok(())
}

/// Convert §D.2.29 mastering-display fields into the shared frame shape.
/// Real `ffprobe` measurements confirm the 50000/10000 denominators and the
/// green/blue/red to red/green/blue permutation. This remains local because
/// the H.264 and HEVC SEI payloads are distinct types and neither codec crate
/// may depend on the other.
fn mastering_display_from_sei(
    primaries_gbr: [(u16, u16); 3],
    white_point: (u16, u16),
    max_luminance: u32,
    min_luminance: u32,
) -> vaco_frame::MasteringDisplay {
    let chromaticity = |(x, y): (u16, u16)| {
        [
            vaco_core::Rational::new(i32::from(x), 50_000),
            vaco_core::Rational::new(i32::from(y), 50_000),
        ]
    };
    let [green, blue, red] = primaries_gbr;
    vaco_frame::MasteringDisplay {
        primaries: [chromaticity(red), chromaticity(green), chromaticity(blue)],
        white_point: chromaticity(white_point),
        max_luminance: vaco_core::Rational::new(
            i32::try_from(max_luminance).unwrap_or(i32::MAX),
            10_000,
        ),
        min_luminance: vaco_core::Rational::new(
            i32::try_from(min_luminance).unwrap_or(i32::MAX),
            10_000,
        ),
    }
}

/// Copy a decoded picture into the frame layout emitted to the caller.
///
/// `Frame::alloc_video` charges the decoder budget, but emitted frames belong
/// to the caller rather than the decoder's working set. Releasing the measured
/// `committed()` delta here accounts for row-stride and alignment padding
/// without duplicating `PixFmt::plane_layout`; otherwise every emitted frame
/// permanently adds `O(picture size)` to the decoder budget.
fn pic_to_frame(
    budget: &mut Budget,
    width: u32,
    height: u32,
    origin: (usize, usize),
    chroma_origin: (usize, usize),
    pic: &Picture,
) -> Result<vaco_frame::Frame> {
    let pix_fmt = vaco_pixfmt::PixFmt::from_name("yuv420p")
        .map_err(|_| Error::InvalidData("vaco-codec-hevc: yuv420p pixel format missing"))?;
    let before = budget.committed();
    let mut frame = vaco_frame::Frame::alloc_video(budget, pix_fmt, width, height)?;
    blit(
        &pic.y,
        &mut frame,
        0,
        width as usize,
        height as usize,
        origin,
    );
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    blit(&pic.cb, &mut frame, 1, cw, ch, chroma_origin);
    blit(&pic.cr, &mut frame, 2, cw, ch, chroma_origin);
    let frame_bytes = budget.committed().saturating_sub(before);
    budget.release(frame_bytes);
    Ok(frame)
}

/// `PERF-PROGRAMME.md` item B1: this used to read every sample through
/// [`crate::framebuf::Plane::get`] (`emit_pocs`'s own 5.11% share of
/// decode, one bounds-checked 2-D index per sample). [`crate::framebuf::Plane::row`]
/// gives the whole row as one bounds-checked slice instead. Item B2 then
/// made the plane's own storage `u8` (the same type `vaco_frame::Frame`'s
/// own plane already used), so what used to be a per-sample narrowing
/// conversion is now a real `copy_from_slice`.
fn blit(
    src: &crate::framebuf::Plane,
    frame: &mut vaco_frame::Frame,
    plane_index: usize,
    width: usize,
    height: usize,
    origin: (usize, usize),
) {
    let Some(mut dst) = frame.plane_mut(plane_index) else {
        return;
    };
    let (ox, oy) = origin;
    for y in 0..height.min(dst.rows()) {
        let Some(row) = dst.row_mut(y) else { continue };
        let len = width.min(row.len());
        let (Some(dst_row), Some(src_row)) = (
            row.get_mut(..len),
            src.row(y + oy).and_then(|r| r.get(ox..ox + len)),
        ) else {
            continue;
        };
        dst_row.copy_from_slice(src_row);
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
        self.rasl_output_suppression_poc = None;
        self.sequence_ended = false;
        // Release every byte charged to the budget along with the state
        // that held them, mirroring `H264Decoder::flush`'s own precedent.
        self.budget = Budget::new(self.limits.clone());
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.parser.set_extradata(extradata)?;
        Ok(())
    }
}

/// Annex C output eligibility after the slice header's own `pic_output_flag`.
/// A RASL associated with an IRAP whose `NoRaslOutputFlag` is set has a lower
/// POC than that IRAP and remains decodable/referenceable but is not displayed.
#[must_use]
fn output_is_needed(
    pic_output: bool,
    nal_unit_type: vaco_parse_hevc::NalUnitType,
    poc: i64,
    rasl_output_suppression_poc: Option<i64>,
) -> bool {
    pic_output
        && !(nal_unit_type.is_rasl()
            && rasl_output_suppression_poc.is_some_and(|irap_poc| poc < irap_poc))
}

/// §8.3.3 permits dropping RASL pictures associated with an IRAP whose
/// `NoRaslOutputFlag` is set: they are neither output nor referenced by any
/// picture that is output.
#[must_use]
fn rasl_can_be_ignored(
    nal_unit_type: vaco_parse_hevc::NalUnitType,
    rasl_output_suppression_poc: Option<i64>,
) -> bool {
    nal_unit_type.is_rasl() && rasl_output_suppression_poc.is_some()
}

// ------------------------------------------------------------- deblock lag

/// Test-only: what CTU row to examine, and which candidate lags (in whole
/// CTU rows) to test on each side of it. See [`run_deblock_lag_probe`]'s own
/// doc for the experiment this drives.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct DeblockLagProbe {
    pub target_ctu_row: usize,
    pub lags: Vec<usize>,
}

/// Test-only: one candidate lag's two-sided result.
///
/// `below_matches`/`above_matches` answer "does the target CTU row's own
/// deblocked output stay identical once everything `lag` CTU rows further
/// away (below / above, respectively) is corrupted, while everything within
/// `lag` rows is left pristine". `false` at `lag == 0` (the immediately
/// adjacent row corrupted) is the "boundary row moves" half of the two-sided
/// bound — if that were `true`, the whole experiment would be measuring
/// nothing, because corrupting the very next row would have to change
/// *something* for a real edge to exist there at all.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeblockLagResult {
    pub lag: usize,
    pub below_matches: bool,
    pub above_matches: bool,
}

/// Runs the deblocking-lag experiment `PERF-PROGRAMME.md` item B4 needs
/// before a wavefront schedule can trust any particular row lag for
/// `deblock::filter_picture`: measured, not argued, per that item's own
/// stop condition.
///
/// `walk` is the just-reconstructed (pre-deblock) picture and its full
/// per-4x4 metadata (`CuGrid`/`EdgeMarks`), captured by `decode_packet`
/// immediately before its own real deblocking call. For each candidate
/// `lag` in `probe.lags`, this clones the raw reconstruction twice more —
/// once with every sample `lag + 1` or more CTU rows *below*
/// `probe.target_ctu_row` inverted (`^= 0xFF`, a content-dependent
/// corruption no coincidental match can survive), once with every sample
/// `lag + 1` or more CTU rows *above* it inverted — runs
/// `deblock::filter_picture` on each corrupted clone via
/// [`Ctx::retarget_pic_for_test`] (identical `CuGrid`/`EdgeMarks`, so
/// boundary-strength and threshold decisions are exactly what the real
/// content produces), and compares the target row's own deblocked samples
/// (all three planes) against a pristine (uncorrupted) reference run.
///
/// Corrupting *only* rows strictly further than `lag` away and asking
/// whether the target's output still matches is the direct measurement of
/// "how far does this row's own deblocked output reach for its inputs" —
/// not an argument about the filter's own tap span, an empirical answer
/// for this exact content's own boundary-strength/threshold decisions.
#[cfg(test)]
fn run_deblock_lag_probe(
    walk: &Ctx<'_>,
    probe: &DeblockLagProbe,
    budget: &mut Budget,
) -> Vec<DeblockLagResult> {
    let ctb = 1usize << walk.shared.log2_ctb_size;
    let target_row_start = probe.target_ctu_row.saturating_mul(ctb);
    let target_row_end = target_row_start.saturating_add(ctb);
    let (width, height) = (
        usize::try_from(walk.shared.pic_width).unwrap_or(0),
        usize::try_from(walk.shared.pic_height).unwrap_or(0),
    );

    // `deblock::filter_picture` never reads `Ctx::recon` -- every retarget
    // below builds its own throwaway `ReconPictureShared`/`ReconPicture`
    // pair, each scoped to its own nested block, purely to satisfy the
    // field. A single one reused across every retarget (the pre-borrowed-
    // reference shape this replaces) no longer works: `Ctx`'s own `recon`
    // field is `&'p mut ReconPicture<'p>`, so `retarget_pic_for_test`'s
    // returned `Ctx<'q>` needs `'q` to equal both the mutable borrow's own
    // duration *and* the `RowPublish` board's lifetime at once (`&mut T`
    // is invariant in `T`) -- reusing one throwaway across three retargets
    // would force that single `'q` out to the whole probe's own scope,
    // exactly the long-lived-overlapping-borrow shape the borrow checker
    // correctly refuses. A fresh throwaway per retarget, scoped tightly to
    // its own block, costs a few small never-read allocations in a
    // `#[cfg(test)]`-only path -- immaterial next to the correctness this
    // buys back.
    let mut pristine_pic = walk.pic.clone();
    {
        let Ok(recon_shared) = crate::framebuf::ReconPictureShared::new(budget, width, height, ctb)
        else {
            // Allocation failure here means the probe cannot run at all; an
            // empty result reads as "missing lag N" in the test's own
            // assertions, which is a loud, specific failure rather than a
            // panic in probe machinery that is not itself under test.
            return Vec::new();
        };
        let mut throwaway_recon = crate::framebuf::ReconPicture::new(&recon_shared);
        let mut pristine_ctx = walk.retarget_pic_for_test(&mut pristine_pic, &mut throwaway_recon);
        deblock::filter_picture(&mut pristine_ctx);
    }
    let reference = capture_rows(&pristine_pic, target_row_start, target_row_end);

    probe
        .lags
        .iter()
        .map(|&lag| {
            let below_first_corrupt_ctu_row =
                probe.target_ctu_row.saturating_add(1).saturating_add(lag);
            let mut below_pic = walk.pic.clone();
            invert_rows_from(
                &mut below_pic,
                below_first_corrupt_ctu_row.saturating_mul(ctb),
            );
            let below_matches = crate::framebuf::ReconPictureShared::new(
                budget, width, height, ctb,
            )
            .is_ok_and(|recon_shared| {
                let mut throwaway_recon = crate::framebuf::ReconPicture::new(&recon_shared);
                let mut below_ctx =
                    walk.retarget_pic_for_test(&mut below_pic, &mut throwaway_recon);
                deblock::filter_picture(&mut below_ctx);
                capture_rows(&below_pic, target_row_start, target_row_end) == reference
            });

            // `above_last_pristine_ctu_row` is the last CTU row (inclusive,
            // counting down from the target) left uncorrupted; corruption
            // covers every row strictly above it. Saturates to "corrupt
            // nothing" once `lag` reaches the picture's own top, which
            // reads as a vacuous match rather than a panic — the caller
            // picks a target row with enough rows to spare on both sides
            // precisely so every lag it asks for is meaningful.
            let above_first_pristine_ctu_row = probe.target_ctu_row.saturating_sub(lag);
            let mut above_pic = walk.pic.clone();
            invert_rows_before(
                &mut above_pic,
                above_first_pristine_ctu_row.saturating_mul(ctb),
            );
            let above_matches = crate::framebuf::ReconPictureShared::new(
                budget, width, height, ctb,
            )
            .is_ok_and(|recon_shared| {
                let mut throwaway_recon = crate::framebuf::ReconPicture::new(&recon_shared);
                let mut above_ctx =
                    walk.retarget_pic_for_test(&mut above_pic, &mut throwaway_recon);
                deblock::filter_picture(&mut above_ctx);
                capture_rows(&above_pic, target_row_start, target_row_end) == reference
            });

            DeblockLagResult {
                lag,
                below_matches,
                above_matches,
            }
        })
        .collect()
}

/// One plane's samples over `[luma_row_start, luma_row_end)`, widened to
/// `u16` and captured as owned rows — a snapshot cheap enough to hold two or
/// three of at once, and independent of whichever `Plane` produced it
/// (`==`-comparable across two entirely separate corrupted clones).
#[cfg(test)]
fn capture_rows(
    pic: &Picture,
    luma_row_start: usize,
    luma_row_end: usize,
) -> (Vec<Vec<u16>>, Vec<Vec<u16>>, Vec<Vec<u16>>) {
    let capture_plane = |plane: &crate::framebuf::Plane, from: usize, to: usize| -> Vec<Vec<u16>> {
        let (w, h) = plane.dims();
        (from..to.min(h))
            .map(|y| (0..w).map(|x| plane.get(x, y)).collect())
            .collect()
    };
    let (_, ch) = pic.cb.dims();
    #[allow(
        clippy::integer_division,
        reason = "chroma row index = luma row index / the fixed 4:2:0 subsampling factor"
    )]
    let c_from = (luma_row_start / 2).min(ch);
    let c_to = luma_row_end.div_ceil(2).min(ch);
    (
        capture_plane(&pic.y, luma_row_start, luma_row_end),
        capture_plane(&pic.cb, c_from, c_to),
        capture_plane(&pic.cr, c_from, c_to),
    )
}

/// Invert every sample (`^= 0xFF`) in every plane of `pic` from luma row
/// `luma_from` to the bottom of the picture.
#[cfg(test)]
fn invert_rows_from(pic: &mut Picture, luma_from: usize) {
    invert_plane_rows(&mut pic.y, luma_from, usize::MAX);
    #[allow(
        clippy::integer_division,
        reason = "chroma row index = luma row index / the fixed 4:2:0 subsampling factor"
    )]
    let c_from = luma_from / 2;
    invert_plane_rows(&mut pic.cb, c_from, usize::MAX);
    invert_plane_rows(&mut pic.cr, c_from, usize::MAX);
}

/// Invert every sample in every plane of `pic` from the top of the picture
/// up to (excluding) luma row `luma_before`.
#[cfg(test)]
fn invert_rows_before(pic: &mut Picture, luma_before: usize) {
    invert_plane_rows(&mut pic.y, 0, luma_before);
    #[allow(
        clippy::integer_division,
        reason = "chroma row index = luma row index / the fixed 4:2:0 subsampling factor"
    )]
    let c_before = luma_before / 2;
    invert_plane_rows(&mut pic.cb, 0, c_before);
    invert_plane_rows(&mut pic.cr, 0, c_before);
}

#[cfg(test)]
fn invert_plane_rows(plane: &mut crate::framebuf::Plane, from: usize, to: usize) {
    let (_, height) = plane.dims();
    for y in from..to.min(height) {
        if let Some(row) = plane.row_mut(y) {
            for b in row.iter_mut() {
                *b ^= 0xFF;
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed scenarios"
)]
mod deblock_lag_tests {
    use super::{DeblockLagProbe, HevcDecoder};
    use vaco_codec_core::Decoder;
    use vaco_limits::{Budget, Limits};
    use vaco_packet::Packet;

    /// `tests/fixtures/deblock_lag_256x320.hevc`: one real `libx265` I-frame
    /// (`qp=24`, deblocking and SAO both on — `libx265`'s own defaults),
    /// 256x320 (4x5 CTUs at the default 64-sample CTB size), busy
    /// `mandelbrot` content chosen so the strong filter (the widest reach,
    /// `p2`/`q2`) actually triggers rather than being reachable-but-unused.
    /// Row 2 (of 0..=4) is the target: two CTU rows of real neighbour on
    /// both sides, so lag 0 and lag 1 are both meaningful in both
    /// directions.
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/deblock_lag_256x320.hevc");

    /// `PERF-PROGRAMME.md` item B4's own gating question, answered
    /// empirically rather than argued: [`super::run_deblock_lag_probe`]
    /// corrupts everything more than `lag` CTU rows away from row 2 (in
    /// each direction, one experiment at a time) and checks whether row
    /// 2's own deblocked output still matches a pristine reference.
    ///
    /// The two-sided bound: `lag == 0` must **not** match (corrupting the
    /// immediately adjacent CTU row must move row 2's own output — the
    /// "boundary row moves" half; a probe that matched here would be
    /// measuring nothing, because no edge would exist between adjacent
    /// rows at all), and `lag == 1` **must** match (row 2's own output must
    /// be fully determined once both immediate neighbours are pristine —
    /// the "nothing outside the watermark moves" half). Both hold, in both
    /// directions, on this fixture: deblocking's true dependency extent is
    /// one CTU row, the same shape as H.264's own one-macroblock-row lag,
    /// not the whole picture the current two-full-passes implementation
    /// conservatively assumes.
    #[test]
    fn deblocking_depends_on_exactly_one_ctu_row_each_side() {
        let mut decoder = HevcDecoder::new(Limits::permissive());
        decoder.deblock_lag_probe = Some(DeblockLagProbe {
            target_ctu_row: 2,
            lags: vec![0, 1, 2],
        });
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, FIXTURE).expect("packet from fixture bytes");
        decoder.send_packet(Some(&packet)).expect("fixture decodes");
        decoder.send_packet(None).expect("drain");
        // Drain every pending frame so nothing about the real decode is
        // left half-finished, even though the probe itself is what this
        // test actually checks.
        while decoder.receive_frame().is_ok() {}

        let results = decoder
            .deblock_lag_probe_result
            .take()
            .expect("probe ran during decode_packet");
        assert_eq!(
            results.len(),
            3,
            "one result per requested lag: {results:?}"
        );

        let at = |lag: usize| {
            results
                .iter()
                .find(|r| r.lag == lag)
                .unwrap_or_else(|| panic!("missing lag {lag} in {results:?}"))
        };

        // The "boundary row moves" half: the immediately adjacent CTU row
        // must matter, in both directions.
        assert!(
            !at(0).below_matches,
            "corrupting the CTU row directly below row 2 must move row 2's own deblocked output: {results:?}"
        );
        assert!(
            !at(0).above_matches,
            "corrupting the CTU row directly above row 2 must move row 2's own deblocked output: {results:?}"
        );

        // The "nothing outside the watermark moves" half: once the
        // immediate neighbour is pristine, everything further away must be
        // irrelevant, in both directions.
        assert!(
            at(1).below_matches,
            "row 2's own output must not depend on anything two or more CTU rows below it: {results:?}"
        );
        assert!(
            at(1).above_matches,
            "row 2's own output must not depend on anything two or more CTU rows above it: {results:?}"
        );
        assert!(
            at(2).below_matches,
            "a wider lag than the true extent must still match: {results:?}"
        );
        assert!(
            at(2).above_matches,
            "a wider lag than the true extent must still match: {results:?}"
        );
    }

    /// The same two-sided bound as
    /// [`deblocking_depends_on_exactly_one_ctu_row_each_side`], repeated at
    /// every other interior CTU row this fixture has (rows 1 and 3 of
    /// 0..=4; rows 0 and 4 are picture edges with no neighbour on one side
    /// and are not part of the wavefront-lag question). One row proves the
    /// bound holds *somewhere*; every interior row proves it is not an
    /// accident of row 2's particular content.
    #[test]
    fn deblocking_bound_holds_at_every_interior_row() {
        for target_ctu_row in [1usize, 3usize] {
            let mut decoder = HevcDecoder::new(Limits::permissive());
            decoder.deblock_lag_probe = Some(DeblockLagProbe {
                target_ctu_row,
                lags: vec![0, 1],
            });
            let mut budget = Budget::new(Limits::permissive());
            let packet =
                Packet::from_slice(&mut budget, FIXTURE).expect("packet from fixture bytes");
            decoder.send_packet(Some(&packet)).expect("fixture decodes");
            decoder.send_packet(None).expect("drain");
            while decoder.receive_frame().is_ok() {}

            let results = decoder
                .deblock_lag_probe_result
                .take()
                .expect("probe ran during decode_packet");
            let at = |lag: usize| {
                results.iter().find(|r| r.lag == lag).unwrap_or_else(|| {
                    panic!("missing lag {lag} in {results:?} for row {target_ctu_row}")
                })
            };

            assert!(
                !at(0).below_matches,
                "row {target_ctu_row}: immediate below neighbour must matter: {results:?}"
            );
            assert!(
                !at(0).above_matches,
                "row {target_ctu_row}: immediate above neighbour must matter: {results:?}"
            );
            assert!(
                at(1).below_matches,
                "row {target_ctu_row}: nothing two rows below should matter: {results:?}"
            );
            assert!(
                at(1).above_matches,
                "row {target_ctu_row}: nothing two rows above should matter: {results:?}"
            );
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "the test exercises a pure output-eligibility rule over fixed NAL types"
)]
mod irap_output_tests {
    use super::{HevcDecoder, output_is_needed, rasl_can_be_ignored};
    use vaco_limits::Limits;
    use vaco_parse_hevc::NalUnitType;

    /// §8.1 makes a RASL picture that precedes a BLA's decoding-order
    /// point unavailable for output. NUT_A_ericsson_5 carries this exact
    /// BLA_W_LP → RASL_R/RASL_N sequence; the RASLs are decoded for syntax
    /// coverage but must not add display frames.
    #[test]
    fn bla_suppresses_only_its_preceding_rasl_pictures() {
        let bla_poc = 220;
        assert!(!output_is_needed(
            true,
            NalUnitType::RASL_R,
            210,
            Some(bla_poc)
        ));
        assert!(!output_is_needed(
            true,
            NalUnitType::RASL_N,
            200,
            Some(bla_poc)
        ));
        assert!(output_is_needed(
            true,
            NalUnitType::RADL_R,
            210,
            Some(bla_poc)
        ));
        assert!(output_is_needed(
            true,
            NalUnitType::TRAIL_R,
            230,
            Some(bla_poc)
        ));
        assert!(output_is_needed(
            true,
            NalUnitType::RASL_R,
            220,
            Some(bla_poc)
        ));
    }

    #[test]
    fn rasl_after_a_no_rasl_irap_can_be_ignored_but_radl_cannot() {
        assert!(rasl_can_be_ignored(NalUnitType::RASL_R, Some(0)));
        assert!(rasl_can_be_ignored(NalUnitType::RASL_N, Some(0)));
        assert!(!rasl_can_be_ignored(NalUnitType::RADL_R, Some(0)));
        assert!(!rasl_can_be_ignored(NalUnitType::RASL_R, None));
    }

    #[test]
    fn end_of_sequence_marks_the_next_irap_to_discard_prior_output() {
        let mut decoder = HevcDecoder::new(Limits::default());
        decoder.rasl_output_suppression_poc = Some(8);

        decoder.note_sequence_end();

        assert!(decoder.sequence_ended);
        assert_eq!(decoder.rasl_output_suppression_poc, None);
    }
}
