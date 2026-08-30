//! [`H264Decoder`] — the [`Decoder`] this crate registers, and the thing
//! `vaco -i input.mp4` actually calls to turn an H.264 access unit into
//! pixels.
//!
//! # What this decoder covers
//!
//! **CABAC I/P/B slices, one slice per picture, `ChromaArrayType == 1`
//! (4:2:0), frame (non-MBAFF, non-field) pictures, short-term references
//! only (no `MMCO`/long-term marking).** That is exactly
//! [`crate::mb::decode_slice_cabac`]'s own scope, and exactly what
//! [`crate::reconstruct::reconstruct_picture`] turns into real luma/Cb/Cr
//! samples — see both modules' own docs for the full account of what is
//! and is not implemented one level down (`I_PCM`, MBAFF, the 8x8
//! transform, `constrained_intra_pred_flag`'s substitution rule,
//! temporal direct prediction, long-term references, and more than one
//! slice per picture are all refused explicitly, not silently
//! mishandled).
//!
//! This used to stop at resolving `entropy_coding_mode_flag` and return
//! [`Error::Unsupported`] unconditionally, then grew to cover CABAC I/P
//! slices only (CABAC B slices refused before `decode_slice_cabac` ever
//! ran). B-slice support closes that gap: reference picture list 1
//! construction (clause 8.2.4.2.3, both lists' own default order plus
//! `ref_pic_list_modification()`), spatial direct prediction's own
//! colocated-picture lookup (clause 8.4.1.2.1/2, [`ColocatedField`]) and
//! clause 8.4.2.3's bi-prediction weighting (default average, explicit,
//! and implicit -- `weighted_bipred_idc == 2`, x264's own default for B
//! slices) all live here, since this is the only place that has ever seen
//! more than one decoded picture at once.
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
//! precisely, the same choice this crate already makes for `I_PCM`.
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
//! Frames are held in a small POC-ordered reorder buffer
//! ([`H264Decoder::reorder`]) rather than emitted the instant they
//! decode: a B-frame stream's decode order and display order genuinely
//! differ (a `B` picture between two anchors decodes *after* both but
//! displays *before* the later one), so I/P-only "decode order already is
//! display order" no longer holds once B slices are real. The buffer's
//! own depth is `sps.max_num_reorder_frames()` when the VUI states one,
//! else `sps.max_num_ref_frames` (a conservative bound every real encoder
//! stays within) -- once more than that many pictures are held, the
//! lowest-POC one is emitted. An IDR access unit flushes every pending
//! picture (in POC order) *before* it decodes, the same way it clears the
//! reference-picture DPB first: POC restarts near zero at an IDR, so a
//! stale pre-IDR POC could otherwise sort after a post-IDR one and emit
//! in the wrong order. [`Machine`] itself needs [`Caps::DELAY`] declared
//! for this to be legal at all -- a machine with neither `DELAY` nor
//! `SUBFRAMES` polices "at most one buffered output" in debug builds,
//! which a multi-picture reorder window is not.
//!
//! # Reference picture buffering
//!
//! A simple sliding window (clause 8.2.5.3's own removal process, minus
//! MMCO/long-term marking — #422 tracks that gap): every reference
//! picture decoded, capped at the SPS's own `max_num_ref_frames`. An IDR
//! access unit clears it first, so a P or B slice can never accidentally
//! reach across a GOP boundary. Each entry now also carries its own POC,
//! `frame_num` and per-4x4-luma-block motion field
//! ([`crate::mb::ColocatedField`]'s own raw material) -- clause 8.2.4.2's
//! default reference-list construction needs the first two (descending
//! `FrameNumWrap` for P/SP list 0; POC-relative-to-current ordering for
//! both of a B slice's lists) and clause 8.4.1.2.1's `colZeroFlag` needs
//! the third, whenever a *later* B slice's `RefPicList1[0]` turns out to
//! be this exact picture.

use std::collections::VecDeque;
use std::sync::Arc;

use vaco_bitstream::BitReader;
use vaco_codec_cabac::CabacDecoder;
use vaco_codec_core::picture::{PictureRef, PictureSpec, PlaneSpec, ProgressPicture};
use vaco_codec_core::{Accept, Caps, Decoder, FrameRunner, Machine, Threading};
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_h264::slice::{RefPicListModification, RefPicMarking};
use vaco_parse_h264::{H264NalHeader, H264Parser, NalUnitType, SliceHeader, SliceKind};

use crate::frame_task::{DeblockParams, FrameGeometry, H264FrameTask};
use crate::mb::{ColocatedField, MvInfo};
use crate::reconstruct::{BiPredMode, ImplicitWeight, ImplicitWeights, SliceWeightTables};

/// One DPB entry: this picture's clause 8.2 bookkeeping, plus a handle to its
/// samples at the coded (macroblock-aligned, uncropped) size — kept uncropped
/// because a later picture's motion compensation reads the *coded* picture, not
/// the display-cropped one.
///
/// The split between the two halves is what makes frame threading work at all.
/// Everything except `planes` is final the instant this picture's slice has
/// been entropy-decoded, on the serial thread, so reference-list construction,
/// clause 8.2.5 marking and the co-located motion lookup never wait for a
/// pixel. `planes` is a [`PictureRef`]: the samples may still be in production
/// on a worker, and only [`crate::frame_task::H264FrameTask`] — the half that
/// actually reads them — ever blocks on it.
#[derive(Debug)]
struct RefPicture {
    /// This picture's samples, published once by the task that decodes it.
    planes: PictureRef,
    /// Bytes [`ProgressPicture::allocate`] and the motion field below charged
    /// to the decoder's budget, for the matching `release` at every eviction
    /// site (#421: `Budget::release` is never automatic).
    charged: u64,
    /// `PicOrderCnt`, clause 8.2.1 -- clause 8.2.4.2's own default
    /// reference-list ordering for B slices (and the `colZeroFlag`
    /// short-term test) both need it.
    poc: i32,
    /// `frame_num`, clause 7.4.3 -- clause 8.2.4.1's `PicNum`/`FrameNumWrap`
    /// arithmetic (P/SP list 0's own default order, and every list's
    /// `ref_pic_list_modification()`) needs it; this decoder does not
    /// implement `MaxLongTermFrameIdx`, so every reference here is always
    /// short-term.
    frame_num: u32,
    /// This picture's own per-4x4-luma-block motion field, absolute
    /// frame coordinates, row-major (`y * (mbs_wide*4) + x`) -- built once
    /// from [`crate::mb::SliceStats::macroblocks`] right after this
    /// picture decoded, from data that already existed and was otherwise
    /// thrown away. Only ever read back via [`ColocatedField`], and only
    /// when a *later* B slice's `RefPicList1[0]` turns out to be this
    /// exact picture.
    motion: Arc<Vec<MvInfo>>,
}

/// The three coded sample planes of one 4:2:0 picture, in bytes.
///
/// The unit both halves of the budget accounting are expressed in: one DPB
/// entry holds exactly this much, and so does one in-flight task's own working
/// picture.
fn coded_picture_bytes(mbs_wide: u32, mbs_high: u32) -> u64 {
    let luma = u64::from(mbs_wide)
        .saturating_mul(16)
        .saturating_mul(u64::from(mbs_high).saturating_mul(16));
    // Cb and Cr are each a quarter of luma for `ChromaArrayType == 1`, so the
    // three planes together are 3/2 of luma.
    luma.saturating_add(luma.saturating_div(2))
}

/// The `PictureSpec` a DPB entry is allocated with.
///
/// **One band, no guard rows.** This decoder publishes a reference picture only
/// once clause 8.7's two whole-picture deblocking passes have swept it, because
/// until then no row of it is final and a later picture must not predict from
/// undeblocked samples. Row-granularity publication is what the band machinery
/// exists for and is the obvious next step, but it needs deblocking interleaved
/// into the macroblock-row loop first — see `crate::frame_task`'s module doc.
fn dpb_picture_spec(mbs_wide: u32, mbs_high: u32) -> PictureSpec {
    let (w, h) = (mbs_wide.saturating_mul(16), mbs_high.saturating_mul(16));
    let (cw, ch) = (mbs_wide.saturating_mul(8), mbs_high.saturating_mul(8));
    PictureSpec::new(vec![
        PlaneSpec::new(w, h),
        PlaneSpec::new(cw, ch),
        PlaneSpec::new(cw, ch),
    ])
    .single_band()
}

/// Clause 8.2.4.1's `FrameNumWrap`: `frame_num` reinterpreted as "distance
/// before `curr_frame_num`, allowing one wraparound" -- the quantity both
/// P/SP list 0's default order and every list's `ref_pic_list_modification()`
/// sort/search by. Short-term references only (`PicNum == FrameNumWrap` for
/// a non-MBAFF frame picture), matching this decoder's own scope.
fn frame_num_wrap(stored: u32, curr: u32, max_frame_num: u32) -> i64 {
    if stored > curr { i64::from(stored) - i64::from(max_frame_num) } else { i64::from(stored) }
}

/// Clause 8.2.4.3.1's short-term reordering (`modification_of_pic_nums_idc`
/// 0/1 only -- `idc == 2`, long-term, is refused by the caller before this
/// ever runs). `default_list` is the already-built clause 8.2.4.2 default
/// order (DPB indices); `num_active` is `num_ref_idx_lX_active_minus1 + 1`.
///
/// Transcribed from the specification's own pseudocode (clause 8.2.4.3.1),
/// not from any reference decoder: `PicNum`/`FrameNumWrap` arithmetic is
/// simple enough, and different enough from JM's own field/MBAFF-generalised
/// implementation, that reproducing the frame-only case directly from the
/// normative text was more direct than isolating JM's own short-term path
/// from a function that also carries fields/`mb_aff` handling this crate
/// does not need.
fn apply_ref_list_modification(
    dpb: &VecDeque<RefPicture>,
    default_list: &[usize],
    num_active: usize,
    mods: &[RefPicListModification],
    curr_frame_num: u32,
    max_frame_num: u32,
) -> Result<Vec<usize>> {
    if mods.is_empty() {
        return Ok(default_list.to_vec());
    }
    let max_pic_num = i64::from(max_frame_num);
    let mut list: Vec<usize> = default_list.to_vec();
    // Clause 8.2.4.3.1's own algorithm works on a list already sized
    // `num_ref_idx_lX_active_minus1 + 1` -- pad with the last default
    // entry rather than index out of range if the DPB itself holds fewer
    // pictures than the slice header claims are active (defensive; a
    // conformant stream never needs this).
    while list.len() < num_active + 1 {
        let Some(&last) = list.last() else { break };
        list.push(last);
    }
    let mut curr_pic_num_pred = i64::from(curr_frame_num);
    let mut ref_idx = 0usize;
    for m in mods {
        if m.idc == 2 {
            return Err(Error::Unsupported(
                "vaco-codec-h264: long-term reference picture reordering (modification_of_pic_nums_idc == 2) is out of scope -- this decoder's DPB has no long-term slot at all",
            ));
        }
        let abs_diff = i64::from(m.value) + 1;
        let pic_num_no_wrap = if m.idc == 0 {
            let mut v = curr_pic_num_pred - abs_diff;
            if v < 0 {
                v += max_pic_num;
            }
            v
        } else {
            let mut v = curr_pic_num_pred + abs_diff;
            if v >= max_pic_num {
                v -= max_pic_num;
            }
            v
        };
        curr_pic_num_pred = pic_num_no_wrap;
        let pic_num =
            if pic_num_no_wrap > i64::from(curr_frame_num) { pic_num_no_wrap - max_pic_num } else { pic_num_no_wrap };
        let Some(found) = (0..dpb.len()).find(|&i| {
            dpb.get(i).is_some_and(|p| frame_num_wrap(p.frame_num, curr_frame_num, max_frame_num) == pic_num)
        }) else {
            return Err(Error::InvalidData(
                "vaco-codec-h264: ref_pic_list_modification named a picture not present in the DPB",
            ));
        };
        if ref_idx >= list.len() {
            list.push(found);
        } else {
            list.insert(ref_idx, found);
        }
        if list.len() > num_active + 1 {
            list.truncate(num_active + 1);
        }
        // Clause 8.2.4.3.1's own compaction: remove the *other* occurrence
        // of `found` beyond the position it was just inserted at (there is
        // at most one, since every earlier step already maintained
        // uniqueness within the active range).
        let mut w = ref_idx + 1;
        for r in (ref_idx + 1)..list.len() {
            let Some(&v) = list.get(r) else { continue };
            if v == found {
                continue;
            }
            if let Some(slot) = list.get_mut(w) {
                *slot = v;
            }
            w += 1;
        }
        list.truncate(w);
        ref_idx += 1;
    }
    list.truncate(num_active.min(list.len()));
    Ok(list)
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
    /// Output reorder buffer -- see the module doc's "output ordering"
    /// section. `(poc, frame)`, unordered internally (the minimum is found
    /// on demand); small by construction (bounded by the reorder window
    /// below), so a linear scan costs nothing a `BinaryHeap` would
    /// meaningfully improve on.
    reorder: Vec<(i32, Frame)>,
    /// The pool the reconstruct/deblock half runs on. One thread by default,
    /// which spawns nothing and runs each task inline at dispatch — exactly
    /// the call sequence this decoder had before frame threading existed.
    runner: FrameRunner<H264FrameTask>,
    /// Every dispatched-but-not-collected picture's ordering decisions, in
    /// decode order. Parallel to the runner's own slots.
    in_flight: VecDeque<InFlight>,
    /// `-threads N`, after clamping.
    threads: usize,
}

/// What the serial half decided about one picture's *output*, held until that
/// picture's samples come back.
///
/// Every one of these is an ordering decision, and every one is made in decode
/// order at [`H264Decoder::collect_one`] rather than at dispatch. That is the
/// reason threaded output is bit-identical: the reorder buffer never sees a
/// picture out of decode order, whatever order the workers finished in.
#[derive(Debug)]
struct InFlight {
    /// This access unit was an IDR, so everything still held for reordering
    /// must be emitted (in POC order) *before* this picture joins the buffer.
    /// Deferred to collection rather than done at split time for exactly the
    /// reason above: at split time the earlier pictures' frames do not exist
    /// yet, so flushing there would emit a shorter, differently-ordered
    /// sequence.
    flush_reorder_first: bool,
    poc: i32,
    reorder_window: usize,
    /// Budget bytes charged on this picture's behalf at dispatch, released
    /// here when it is collected.
    charge: u64,
}

impl H264Decoder {
    /// Build a decoder bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            parser: H264Parser::new(limits.clone()),
            budget: Budget::new(limits.clone()),
            rbsp: vaco_format_nalu::RbspBuf::new(),
            // `Caps::DELAY`: this decoder can hold more than one decoded
            // picture before emitting the next one in display order (see
            // the module doc's "output ordering" section) -- without this,
            // `Machine` polices "at most one buffered output" in debug
            // builds and caps its own queue at one, which a multi-picture
            // reorder window is not.
            machine: Machine::new(Caps::DELAY),
            dpb: VecDeque::new(),
            reorder: Vec::new(),
            runner: FrameRunner::new(1),
            in_flight: VecDeque::new(),
            threads: 1,
            limits,
        }
    }

    /// Pictures the serial half may run ahead by.
    ///
    /// One more than the worker count, so a worker that finishes early has a
    /// task waiting rather than idling while the coordinator is still parsing.
    /// At one thread this is 1, which makes [`Self::send_packet`] dispatch and
    /// collect the same picture back to back — the pre-threading call sequence,
    /// unchanged.
    const fn max_in_flight(&self) -> usize {
        if self.threads <= 1 {
            1
        } else {
            self.threads.saturating_add(1)
        }
    }

    /// Emit the lowest-POC pending picture, if any -- the reorder buffer's
    /// own single "let one out" step, called either because the buffer is
    /// full ([`Self::decode_packet`]) or because everything must go
    /// (an IDR about to reset POC, or end of stream).
    ///
    /// Takes `reorder`/`machine` as explicit fields, not `&mut self`, so
    /// it can be called from inside [`Self::decode_packet`] while `sps`/
    /// `pps` (borrowed from `self.parser`) or `rbsp` (borrowed from
    /// `self.rbsp`) are still alive -- a method taking `&mut self`
    /// conservatively borrows the *whole* struct even though this one
    /// only ever touches these two fields, which is exactly what made an
    /// earlier version of this call site a borrow-checker error rather
    /// than a real aliasing problem.
    fn emit_lowest_poc(reorder: &mut Vec<(i32, Frame)>, machine: &mut Machine<Frame>) {
        let Some((idx, _)) = reorder.iter().enumerate().min_by_key(|(_, (poc, _))| *poc) else {
            return;
        };
        let (_, frame) = reorder.swap_remove(idx);
        machine.emit(frame);
    }

    /// Flush every pending reordered picture, in ascending POC order --
    /// used both at an IDR (POC is about to restart near zero, so nothing
    /// from before it may be held back past it) and at end of stream. See
    /// [`Self::emit_lowest_poc`]'s own doc for why this takes explicit
    /// fields rather than `&mut self`.
    fn flush_reorder(reorder: &mut Vec<(i32, Frame)>, machine: &mut Machine<Frame>) {
        while !reorder.is_empty() {
            Self::emit_lowest_poc(reorder, machine);
        }
    }

    /// Decode one packet (one MP4 sample / access unit), pushing whatever
    /// pictures become ready for output into `self.machine`. `Ok(())`
    /// covers both "nothing to do" (no primary-coded picture in this
    /// access unit) and "decoded, but still held for reordering".
    #[allow(clippy::too_many_lines)]
    fn split_packet(&mut self, pkt: &Packet) -> Result<()> {
        let payload = pkt.payload();
        let framing = self.parser.framing();

        // Reuse `H264Parser`'s own access-unit assembly for parameter-set
        // bookkeeping, POC and new-picture detection -- not re-derived
        // here. `info.picture_type` is `None` when this access unit held
        // no VCL slice at all.
        let info = self.parser.push_access_unit(payload, framing)?;
        if info.picture_type.is_none() {
            return Ok(());
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
            return Ok(());
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
            // anything in it decodes, so a P/B slice can never reach
            // across a GOP boundary. Every evicted picture's planes were
            // charged to `self.budget` when they were pushed (see the
            // reference-picture push below) and must be released here,
            // not just dropped -- #421: `Budget::release` is never
            // automatic, so a `clear()` that does not call it leaves
            // `committed` counting bytes real memory no longer holds.
            for evicted in self.dpb.drain(..) {
                self.budget.release(evicted.charged);
            }
        }
        // POC restarts near zero at an IDR (clause 8.2.1) -- every picture
        // still held for reordering must go out first, in POC order, or it
        // could sort *after* a post-IDR picture with a numerically smaller POC
        // and be emitted out of order. The *decision* is made here, in decode
        // order; the flush itself happens in `collect_one`, once every earlier
        // picture's frame actually exists. Doing it here would, under
        // threading, flush a buffer that is still missing frames the serial
        // decoder would already have put in it.
        let flush_reorder_first = info.is_idr;

        if !pps.entropy_coding_mode {
            return Err(Error::Unsupported(
                "vaco-codec-h264: CAVLC picture reconstruction is not implemented -- \
                 decode_slice_cavlc verifies bit consumption only and discards every \
                 decoded coefficient and motion vector; see this module's own doc",
            ));
        }

        let mbs_wide = sps.pic_width_in_mbs;
        let mbs_high = sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
        let luma4_width = mbs_wide * 4;
        let chroma_qp_offset_cb = pps.chroma_qp_index_offset;
        let chroma_qp_offset_cr = pps.second_chroma_qp_index_offset;
        let max_num_ref_frames = sps.max_num_ref_frames;
        let max_frame_num = sps.max_frame_num();
        // Extracted now, alongside `dimensions`/`crop_unit`/`crop` below,
        // and for the identical reason: `sps` borrows `self.parser`, and
        // `self.build_frame` (called near the end of this function) needs
        // `&mut self` to allocate through -- a method call's `&mut self`
        // conservatively borrows the whole struct, so `sps` cannot still
        // be alive by then.
        let reorder_window = sps.max_num_reorder_frames().unwrap_or(max_num_ref_frames).max(1) as usize;
        let curr_poc = info.poc.value();
        let is_b_slice = matches!(slice_header.kind, SliceKind::B);
        // Extracted now, not read from `sps`/`pps` again later: both
        // borrow `self.parser`, which `build_frame` below needs `&mut
        // self` to allocate through.
        let dimensions = sps.dimensions();
        let crop_unit = sps.crop_unit();
        let crop = sps.crop.unwrap_or_default();

        // Clause 8.2.4.2's default reference-picture list construction,
        // then clause 8.2.4.3's `ref_pic_list_modification()` -- both as
        // DPB indices, materialised into real plane/motion references only
        // once the final order is known.
        let curr_frame_num = slice_header.frame_num;
        let list0_default: Vec<usize> = if is_b_slice {
            let mut idx: Vec<usize> = (0..self.dpb.len()).collect();
            idx.sort_by_key(|&i| {
                let poc = i64::from(self.dpb.get(i).map_or(0, |p| p.poc));
                if poc < i64::from(curr_poc) { (0i8, -poc) } else { (1i8, poc) }
            });
            idx
        } else {
            let mut idx: Vec<usize> = (0..self.dpb.len()).collect();
            idx.sort_by_key(|&i| {
                core::cmp::Reverse(frame_num_wrap(self.dpb.get(i).map_or(0, |p| p.frame_num), curr_frame_num, max_frame_num))
            });
            idx
        };
        let mut list1_default: Vec<usize> = Vec::new();
        if is_b_slice {
            let mut idx: Vec<usize> = (0..self.dpb.len()).collect();
            idx.sort_by_key(|&i| {
                let poc = i64::from(self.dpb.get(i).map_or(0, |p| p.poc));
                if poc > i64::from(curr_poc) { (0i8, poc) } else { (1i8, -poc) }
            });
            // Clause 8.2.4.2.3's own swap: if list 1 has more than one
            // entry and is identical to list 0, its first two entries
            // swap.
            if idx.len() > 1 && idx == list0_default {
                idx.swap(0, 1);
            }
            list1_default = idx;
        }
        let n0 = usize::try_from(slice_header.num_ref_idx_l0_active_minus1).unwrap_or(0) + 1;
        let n1 = usize::try_from(slice_header.num_ref_idx_l1_active_minus1).unwrap_or(0) + 1;
        let list0_idx = apply_ref_list_modification(
            &self.dpb,
            &list0_default,
            n0,
            &slice_header.ref_pic_list_modification_l0,
            curr_frame_num,
            max_frame_num,
        )?;
        let list1_idx = if is_b_slice {
            apply_ref_list_modification(
                &self.dpb,
                &list1_default,
                n1,
                &slice_header.ref_pic_list_modification_l1,
                curr_frame_num,
                max_frame_num,
            )?
        } else {
            Vec::new()
        };

        // Clause 8.4.1.2.1's own colocated picture, always `RefPicList1[0]`
        // -- built once here (borrowing `self.dpb`) so it can be handed
        // into `decode_slice_cabac` before the borrow of `self.dpb` this
        // function needs later (pushing this picture's own copy) begins.
        let colocated: Option<ColocatedField> = if is_b_slice {
            list1_idx
                .first()
                .and_then(|&i| self.dpb.get(i))
                .map(|p| ColocatedField::new(luma4_width, mbs_high * 4, Arc::clone(&p.motion)))
        } else {
            None
        };

        let mut cabac = CabacDecoder::from_reader(reader);
        let stats =
            crate::mb::decode_slice_cabac(&mut cabac, &mut self.budget, sps, pps, &slice_header, colocated.as_ref())?;
        drop(colocated);
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
        let total_mbs = mbs_wide.saturating_mul(mbs_high);
        if stats.macroblock_count != total_mbs {
            return Err(Error::InvalidData(
                "vaco-codec-h264: CABAC slice's end_of_slice_flag fired before every \
                 macroblock in the picture was decoded -- a real, still-open decode \
                 desync (see tests/macroblock_layer_cabac.rs's own ignored tests), \
                 refused rather than emitting a partially-reconstructed frame",
            ));
        }

        // Clause 8.2.4's final lists, as handles rather than as sample
        // slices: the pictures they name may still be reconstructing on a
        // worker, and only the frame task -- the half that actually reads a
        // sample -- ever waits for them.
        let ref_list0: Vec<PictureRef> =
            list0_idx.iter().filter_map(|&i| self.dpb.get(i)).map(|r| r.planes.clone()).collect();
        let ref_list1: Vec<PictureRef> =
            list1_idx.iter().filter_map(|&i| self.dpb.get(i)).map(|r| r.planes.clone()).collect();
        // `crate::deblock::boundary_strength`'s own reference-picture-identity
        // lookup (its own doc): the same DPB positions as `ref_list0`/
        // `ref_list1` above, as POCs rather than sample planes -- deblocking
        // never touches pixels, only needs to tell two references apart.
        let ref_list0_poc: Vec<i32> = list0_idx.iter().filter_map(|&i| self.dpb.get(i)).map(|r| r.poc).collect();
        let ref_list1_poc: Vec<i32> = list1_idx.iter().filter_map(|&i| self.dpb.get(i)).map(|r| r.poc).collect();

        // Clause 8.4.2.3's `pred_weight_table()`, already parsed by
        // `vaco-parse-h264` (it has to be, the bits are in the slice
        // header) -- `weighted_pred_flag` is x264's own default for P
        // slices, and `weighted_bipred_idc == 1` (explicit) makes it real
        // for B slices too.
        let weights = SliceWeightTables::from_table(slice_header.pred_weight_table.as_ref());
        let bipred_mode = if is_b_slice {
            match pps.weighted_bipred_idc {
                1 => BiPredMode::Explicit,
                2 => BiPredMode::Implicit,
                _ => BiPredMode::Default,
            }
        } else {
            BiPredMode::Default
        };
        // Clause 8.4.2.3.2's implicit weights (`weighted_bipred_idc == 2`,
        // **x264's own default for B slices**): one `(w0, w1)` pair per
        // `(ref_idx_l0, ref_idx_l1)` combination, derived from every
        // candidate's own POC -- `crate::reconstruct` never sees a POC at
        // all, so this table is built here and handed in as a plain
        // lookup, transcribed from JM 19.1's `image.c::fill_wp_params`
        // (Tier A per `provenance/sources.toml`) rather than re-derived
        // from the specification prose a second time.
        let implicit_weights = (is_b_slice && pps.weighted_bipred_idc == 2).then(|| {
            let table: Vec<Vec<ImplicitWeight>> = list0_idx
                .iter()
                .filter_map(|&i0| self.dpb.get(i0))
                .map(|r0| {
                    list1_idx
                        .iter()
                        .filter_map(|&i1| self.dpb.get(i1))
                        .map(|r1| implicit_weight(curr_poc, r0.poc, r1.poc))
                        .collect()
                })
                .collect();
            ImplicitWeights::new(table)
        });

        // What this picture will cost the budget: the task's own working set
        // (see the note at the reservation below) plus, if it is a reference,
        // one more coded picture for its DPB entry.
        let mb_bytes =
            (stats.macroblocks.len().saturating_mul(core::mem::size_of::<crate::mb::MbSummary>())) as u64;
        let task_charge = coded_picture_bytes(mbs_wide, mbs_high)
            .saturating_mul(2)
            .saturating_add(mb_bytes);
        let headroom = task_charge.saturating_add(coded_picture_bytes(mbs_wide, mbs_high));
        // **Memory pressure is backpressure, not failure.** `-threads N` lets
        // `N + 1` pictures be in flight, and at 4K each one is ~84 MiB once the
        // macroblock array is counted honestly -- more than `Limits::permissive`'s
        // whole 1 GiB ceiling at eight threads. Rather than refuse the decode
        // (the shape this would otherwise take: `-threads 8` working at 1080p
        // and erroring at 4K), finish pictures until the next one fits. The
        // thread count then bounds concurrency only as far as the budget
        // allows, which is what a *limit* is supposed to do. If nothing is in
        // flight and it still does not fit, the reservation below reports it
        // honestly -- one picture genuinely does not fit, and no amount of
        // waiting changes that.
        while self.budget.available() < headroom && !self.in_flight.is_empty() {
            if !self.collect_one(true)? {
                break;
            }
        }

        // Clause 8.2.5's reference-picture marking, all of it metadata: no
        // sample of this picture exists yet and none is needed. This is why
        // the serial half can run ahead of the pixels at all.
        let mut store = None;
        if info.is_reference {
            // Clause 8.2.5.4's adaptive marking, which runs *instead of*
            // clause 8.2.5.3's sliding window whenever
            // `adaptive_ref_pic_marking_mode_flag` is 1. x264 emits it for
            // every `b-pyramid` stream (its default): the pyramid's own
            // reference B picture is explicitly unmarked with
            // `memory_management_control_operation == 1` as soon as the
            // next anchor is coded, which is *not* the picture the sliding
            // window would have evicted (the window always takes the
            // smallest `FrameNumWrap`; the B reference is newer than the
            // anchor it sits between). Ignoring the commands and running
            // the window anyway therefore evicted the wrong picture and
            // every later list-0 entry pointed at the wrong reference --
            // large, structured, accumulating errors from the first
            // picture that read past the mistake, with no CABAC desync at
            // all, since nothing about the *bitstream* parse depends on
            // it.
            let adaptive = match slice_header.ref_pic_marking.as_ref() {
                Some(RefPicMarking::Adaptive(cmds)) => Some(cmds.as_slice()),
                _ => None,
            };
            if let Some(cmds) = adaptive {
                for cmd in cmds {
                    match cmd.op {
                        1 => {
                            // eq. (8-40): `picNumX = CurrPicNum -
                            // (difference_of_pic_nums_minus1 + 1)`, and
                            // for a frame `CurrPicNum == frame_num` and a
                            // stored frame's own `PicNum` is its
                            // `FrameNumWrap`.
                            let pic_num_x =
                                i64::from(curr_frame_num) - (i64::from(cmd.arg0) + 1);
                            if let Some(pos) = self.dpb.iter().position(|p| {
                                frame_num_wrap(p.frame_num, curr_frame_num, max_frame_num) == pic_num_x
                            }) && let Some(evicted) = self.dpb.remove(pos)
                            {
                                self.budget.release(evicted.charged);
                            }
                        }
                        5 => {
                            // "Mark all reference pictures as unused" --
                            // the DPB is emptied, and the current picture
                            // becomes the only entry.
                            for evicted in self.dpb.drain(..) {
                                self.budget.release(evicted.charged);
                            }
                        }
                        // Ops 2/3/4/6 are long-term reference management,
                        // which this decoder does not implement at all
                        // (`RefPicList` construction has no long-term
                        // section, and clause 8.2.4.3's `idc == 2`
                        // reordering is refused for the same reason).
                        // Refused rather than silently ignored: dropping a
                        // long-term command leaves the DPB in a state the
                        // encoder never intended, which is exactly the
                        // "registered but wrong" failure this crate treats
                        // as worse than an honest refusal.
                        _ => {
                            return Err(Error::Unsupported(
                                "vaco-codec-h264: long-term reference pictures                                  (memory_management_control_operation 2/3/4/6) are out of scope",
                            ));
                        }
                    }
                }
            }
            let cap = max_num_ref_frames.max(1) as usize;
            // Evict down to `cap - 1` *before* allocating this picture's own
            // DPB entry, not after: allocating first and evicting after (the
            // shape #421 was originally filed against) briefly holds
            // `cap + 1` reference pictures' worth of budget at once on
            // every single frame, which is a real, avoidable peak.
            while self.dpb.len() > cap.saturating_sub(1) {
                // #421: `pop_front` alone drops the entry's storage (real
                // memory freed correctly) without ever telling `Budget`
                // those bytes are free, which is what let `committed` climb
                // forever and cap 1080p at exactly 10 frames.
                if let Some(evicted) = self.dpb.pop_front() {
                    self.budget.release(evicted.charged);
                }
            }
            // This picture's own per-4x4-luma-block motion field, built
            // from `stats.macroblocks` (already computed by
            // `decode_slice_cabac`, never re-derived) for a *future* B
            // slice's own spatial direct prediction -- see
            // `ColocatedField`'s own doc. `mv_blocks` is raster order
            // within one macroblock (`[y*4+x]`, its own doc says so),
            // matching this flat grid's own `y*width+x` addressing with
            // no z-scan conversion needed.
            let n_luma4 = usize::try_from(luma4_width.saturating_mul(mbs_high * 4)).unwrap_or(0);
            let mut motion: Vec<MvInfo> = self.budget.alloc(n_luma4)?;
            for mb in &stats.macroblocks {
                for (i, &block) in mb.mv_blocks.iter().enumerate() {
                    // `i % 4` / `i / 4`, spelled as bit ops so this isn't an
                    // `integer_division` lint hit: 4 is a compile-time
                    // power of two, `mv_blocks`' own fixed raster stride
                    // (see this loop's own doc above).
                    let bx = u32::try_from(i & 3).unwrap_or(0);
                    let by = u32::try_from(i >> 2).unwrap_or(0);
                    let x = mb.mb_x * 4 + bx;
                    let y = mb.mb_y * 4 + by;
                    if let Some(idx) = usize::try_from(y.saturating_mul(luma4_width).saturating_add(x)).ok()
                        && let Some(slot) = motion.get_mut(idx)
                    {
                        *slot = block;
                    }
                }
            }
            let motion_bytes =
                (motion.len().saturating_mul(core::mem::size_of::<MvInfo>())) as u64;
            // The DPB entry's own samples, charged here and released when this
            // entry is evicted. `allocate` hands back the sole writer (which
            // goes to the task) and a shareable reader (which goes in the DPB
            // and into every later picture's reference list).
            let before = self.budget.committed();
            let (writer, planes) = ProgressPicture::allocate(
                &dpb_picture_spec(mbs_wide, mbs_high),
                self.runner.next_decode_index(),
                &mut self.budget,
            )?;
            let charged = self
                .budget
                .committed()
                .saturating_sub(before)
                .saturating_add(motion_bytes);
            store = Some(writer);
            self.dpb.push_back(RefPicture {
                planes,
                charged,
                poc: curr_poc,
                frame_num: curr_frame_num,
                motion: Arc::new(motion),
            });
        }

        // The task's whole working set, charged to *this* budget for as long
        // as the task is in flight and released in `collect_one`. The task
        // carries its own `Budget` for the per-allocation caps
        // (`max_alloc_single`, `max_frame_bytes`), but the aggregate across
        // every in-flight picture is accounted here, in one place, because
        // that is the number `-threads N` actually multiplies.
        //
        // Three components, and the *third* is the big one, which is not what
        // it looks like from the type names. At 4K a coded picture is 12.4 MB
        // and the cropped output frame is about the same, but
        // `stats.macroblocks` is `MbSummary` x 32,400 macroblocks at 1,888
        // bytes each -- **59 MiB**, five times the two sample buffers put
        // together, because every macroblock carries its full residual and its
        // sixteen 4x4 motion blocks. It has never been charged (a plain
        // `Vec::push` inside `decode_slice_cabac`), which cost nothing while
        // it lived for one `send_packet` call and costs `threads + 1` copies
        // of it now. Measured directly: peak RSS on the 4K fixture goes 2854
        // MiB at one thread to 3321 MiB at eight, and the ~470 MiB difference
        // is almost entirely this.
        //
        // The frame is smaller than the coded picture (it is the same picture,
        // cropped) but carries row-stride padding, so charging a second whole
        // coded picture for it is a deliberate over-estimate rather than an
        // exact figure.
        self.budget.reserve(task_charge)?.commit();

        self.runner.dispatch(H264FrameTask {
            macroblocks: stats.macroblocks,
            mbs_wide,
            mbs_high,
            chroma_qp_offset_cb,
            chroma_qp_offset_cr,
            ref_list0,
            ref_list1,
            ref_list0_poc,
            ref_list1_poc,
            weights,
            bipred_mode,
            implicit_weights,
            deblock: DeblockParams {
                disable_idc: slice_header.disable_deblocking_filter_idc,
                alpha_c0_offset_div2: slice_header.slice_alpha_c0_offset_div2,
                beta_offset_div2: slice_header.slice_beta_offset_div2,
            },
            store,
            geometry: FrameGeometry {
                dimensions,
                crop_unit,
                crop,
                pts: pkt.pts,
                duration: pkt.duration,
                is_idr: info.is_idr,
            },
            limits: self.limits.clone(),
        });
        self.in_flight.push_back(InFlight {
            flush_reorder_first,
            poc: curr_poc,
            reorder_window,
            charge: task_charge,
        });
        Ok(())
    }

    /// Take the oldest in-flight picture's frame and apply its output-ordering
    /// decisions. `Ok(false)` when there was nothing to take.
    ///
    /// **Everything here runs in decode order**, which is the whole reason a
    /// threaded run emits the same bytes as a serial one: `FrameRunner::collect`
    /// refuses to hand back picture `k + 1` before picture `k`, so the reorder
    /// buffer sees exactly the sequence it saw before frame threading existed.
    fn collect_one(&mut self, block: bool) -> Result<bool> {
        if self.in_flight.is_empty() {
            return Ok(false);
        }
        let Some(result) = (if block {
            self.runner.collect()
        } else {
            self.runner.try_collect()
        }) else {
            return Ok(false);
        };
        // Pop and release before the `?`: a failed picture's budget charge is
        // still this decoder's to give back.
        let meta = self.in_flight.pop_front();
        if let Some(meta) = meta.as_ref() {
            self.budget.release(meta.charge);
        }
        let frame = result?;
        let Some(meta) = meta else {
            return Err(Error::InvalidData(
                "vaco-codec-h264: a frame arrived with no in-flight record",
            ));
        };
        if meta.flush_reorder_first {
            Self::flush_reorder(&mut self.reorder, &mut self.machine);
        }
        // Hold for reordering rather than emit immediately -- see the
        // module doc's "output ordering" section. A window of `w` pictures
        // means at most `w` may be held at once; once a new one would make
        // `w + 1`, the lowest-POC one is definitely safe to emit (no
        // future picture in a conformant stream can have a lower POC than
        // every one already seen, past `w` pictures of slack).
        self.reorder.push((meta.poc, frame));
        if self.reorder.len() > meta.reorder_window {
            Self::emit_lowest_poc(&mut self.reorder, &mut self.machine);
        }
        Ok(true)
    }

    /// Collect until at most [`Self::max_in_flight`] pictures are outstanding.
    fn drain_to_capacity(&mut self) -> Result<()> {
        while self.in_flight.len() >= self.max_in_flight() {
            if !self.collect_one(true)? {
                break;
            }
        }
        Ok(())
    }

    /// Collect every outstanding picture. End of stream, and `flush`.
    fn drain_all(&mut self) -> Result<()> {
        while self.collect_one(true)? {}
        Ok(())
    }

}

/// Clause 8.4.2.3.2's implicit bi-prediction weight for one `(ref_idx_l0,
/// ref_idx_l1)` pair, given the three pictures' own POCs -- transcribed
/// from JM 19.1's `image.c::fill_wp_params` (Tier A). `logWD` is fixed at
/// 5 for this mode (not carried in the return value: `crate::reconstruct`'s
/// own `BiPredMode::Implicit` branch hard-codes the same `5`, one
/// definition of the constant rather than two).
#[allow(
    clippy::integer_division,
    reason = "clause 8.4.2.3.2's own fixed-point formula (`tx = (16384 + Abs(td/2)) / td`, td != \
              0 checked immediately above) is exact integer arithmetic by specification, not an \
              approximation a float would improve"
)]
fn implicit_weight(curr_poc: i32, ref0_poc: i32, ref1_poc: i32) -> ImplicitWeight {
    let td = i64::from(ref1_poc.saturating_sub(ref0_poc)).clamp(-128, 127);
    if td == 0 {
        return ImplicitWeight { w0: 32, w1: 32 };
    }
    let tb = i64::from(curr_poc.saturating_sub(ref0_poc)).clamp(-128, 127);
    let tx = (16_384 + (td / 2).abs()) / td;
    let dist_scale_factor = ((tx * tb + 32) >> 6).clamp(-1024, 1023);
    let w1 = dist_scale_factor >> 2;
    let w0 = 64 - w1;
    if !(-64..=128).contains(&w1) {
        return ImplicitWeight { w0: 32, w1: 32 };
    }
    #[allow(clippy::cast_possible_truncation, reason = "w0/w1 are checked into -64..=128 immediately above")]
    ImplicitWeight { w0: w0 as i32, w1: w1 as i32 }
}

/// Copies one `width x height` region of `src` (row-major, `src_stride`
/// wide) starting at `(x0, y0)` into `frame`'s plane `plane_index` --
/// [`crate::interp`]'s own edge-clamping is not needed here, since the
/// crop region is always within the coded picture by construction (clause
/// 7.4.2.1.1's own range constraint on the crop offsets).
pub(crate) fn blit_plane(src: &[u8], src_stride: usize, x0: usize, y0: usize, frame: &mut Frame, plane_index: usize, width: usize, height: usize) {
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
                // End of stream: every picture still reconstructing has to
                // land before the reorder buffer can be flushed, or the
                // sequence would be short by whatever was in flight.
                self.drain_all()?;
                // Everything still held for reordering is now known-safe to
                // emit, in POC order.
                Self::flush_reorder(&mut self.reorder, &mut self.machine);
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                self.split_packet(pkt)?;
                self.drain_to_capacity()
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        // Discard, do not finish: the pictures in flight reference a DPB that
        // is about to be emptied, so their frames are for a position the
        // caller has already left. `FrameRunner::reset` drains the pool before
        // any of the state below is torn down, so no worker is still holding a
        // `PictureWriter` into a picture this method is about to forget.
        self.runner.reset();
        self.in_flight.clear();
        self.machine.flush();
        self.parser.flush();
        self.dpb.clear();
        // Every pending reordered frame is discarded, not emitted --
        // `Machine::flush`'s own contract ("what a fresh machine looks
        // like") applies to this decoder's own output-ordering state too,
        // matching a seek's own "the old position no longer matters"
        // semantics.
        self.reorder.clear();
        // Release every reference-frame byte charged to the budget along
        // with the state that held them, mirroring
        // `vaco_codec_vp8::Vp8Decoder::flush`'s own precedent.
        self.budget = Budget::new(self.limits.clone());
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.parser.set_extradata(extradata)?;
        Ok(())
    }

    /// `-threads N`. **Off by default**: `N <= 1` builds a runner that spawns
    /// nothing and runs every picture inline at dispatch, which is the exact
    /// call sequence this decoder had before frame threading existed.
    ///
    /// Legal to call only before the first packet; the runner is rebuilt here,
    /// which would discard anything in flight.
    fn set_thread_count(&mut self, threads: usize) -> Threading {
        // Rebuilding the runner restarts its decode-index counter, which the
        // DPB's own `PictureRef`s are numbered against. Refusing the change
        // once anything has been decoded is what keeps those two numberings
        // from ever disagreeing; callers set the count once, before the first
        // packet, and that case is every caller there is.
        if self.dpb.is_empty() && self.in_flight.is_empty() {
            self.threads = threads.max(1);
            self.runner = FrameRunner::new(self.threads);
            self.threads = self.runner.threads();
        }
        Threading::Frame {
            max_frames: self.max_in_flight(),
            // Extra *output* latency in frames is zero: `collect_one` still
            // applies the reorder window in decode order, exactly as the
            // serial path did, so no frame is emitted later than it was.
            delay: 0,
        }
        .clamped_to(self.threads)
    }
}
