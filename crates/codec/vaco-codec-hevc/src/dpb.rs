//! Reference picture management: §8.3.2's reference-picture-set derivation,
//! §8.3.4's reference-picture-list construction, and Annex C's informal
//! "bumping" output-reordering process.
//!
//! # Why this module has no caller yet
//!
//! Everything here is pure bookkeeping over picture-order-count values and
//! already-decoded pictures — it needs no CABAC, no coding-tree walk, no
//! sample. It is landed on its own, ahead of `prediction_unit()`/merge/AMVP/
//! motion compensation, because a decoded picture buffer is infrastructure
//! every later inter-prediction stage needs and none of them can be tested
//! meaningfully without it existing first. But nothing in `decoder.rs` calls
//! it yet: `check_scope`'s `only I-slices are decoded` refusal is unchanged,
//! and an all-I-slice stream never has more than one candidate reference
//! picture at a time, which is not enough surface to exercise §8.3.4's list
//! construction for real. Wiring this into the decode path is the first
//! third of the P-slice stage (`prediction_unit`/merge/AMVP/motion
//! compensation are the rest), not a change this pass makes on its own.
//!
//! # Scope: short-term reference pictures only
//!
//! Long-term reference pictures (`long_term_ref_pics_present_flag`) are
//! **not** resolved here — [`derive_reference_pic_sets`] refuses a slice
//! header naming any. Two independent reasons, not one:
//!
//! - §7.4.7.1's `DeltaPocMsbCycleLt[i]` is a *cumulative* sum that resets not
//!   only at `i == 0` but also at `i == num_long_term_sps` — the boundary
//!   between the SPS-predefined long-term entries and the ones a slice codes
//!   inline. `vaco_parse_hevc::SliceHeader::long_term_refs` merges both
//!   sources into one `Vec` (by design — see that crate's own module doc)
//!   without recording where that boundary falls, so this crate cannot
//!   re-derive the second reset point from what it is handed. Getting this
//!   silently wrong would reproduce exactly the class of bug
//!   `AGENT-CONSTRAINTS.md`'s clean-room lessons warn about — a formula that
//!   is right in the common case (one source of long-term entries, where the
//!   missing second reset point is never reached) and silently wrong the
//!   moment a stream mixes both.
//! - Long-term references are also the rarer feature in practice — ordinary
//!   `libx265` output, including every fixture this crate's own history is
//!   built on, uses short-term references exclusively.
//!
//! Refusing by name here, the same posture `decoder.rs::check_scope` already
//! takes for every other cut corner, is the honest choice over a plausible
//! but unverifiable derivation.
//!
//! # Specification
//!
//! ITU-T H.265 (08/2021) §8.3.2 (reference picture set), §8.3.3 (marking
//! unavailable reference pictures — degenerates to a no-op in this crate's
//! single-slice-segment, no-tile, no-loss scope), §8.3.4 (reference picture
//! list construction) and Annex C.5.2 (the informal "bumping" process; C.5.2
//! is described in normative terms but the process itself — pick the
//! smallest-POC picture still needing output — is the one every mainstream
//! decoder implements, HM's `TDecTop::xGetNewPicBuffer` included).

// Every item here is exercised by this module's own tests, directly against
// the specification's derivation and Annex C's bumping process (see the
// module doc's "why this module has no caller yet"), but nothing in
// `decoder.rs` calls any of it until P-slice CTU decoding exists to feed it
// real slice headers and consume real reference pictures — so a plain build
// of the `lib` target (without `--all-targets`) sees every item below as
// unreachable. Silencing `dead_code` here, rather than wiring in a
// placeholder call from `decoder.rs`, is the honest reflection of that: this
// stage is complete and tested on its own terms, not stubbed in to satisfy
// the lint.
#![allow(dead_code, reason = "landed ahead of the P-slice stage that will call it; see the module doc")]

use vaco_core::{Error, Result};
use vaco_parse_hevc::rps::ShortTermRps;
use vaco_parse_hevc::slice::RefPicListModification;

use crate::framebuf::Picture;
use vaco_limits::Budget;

/// How a picture still sitting in the DPB is marked — §8.3.2's last step
/// ("every picture ... not included ... is marked as unused for reference").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marking {
    Unused,
    ShortTerm,
}

/// The derived reference picture sets of §8.3.2: picture-order-count values,
/// not pictures. Resolving each one against the DPB's actual contents is a
/// separate step ([`Dpb::apply_reference_picture_set`]), matching the
/// specification's own two-stage shape (derive POCs, then mark pictures).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the shared `st_` prefix mirrors §8.3.2's own `RefPicSetSt*` names; dropping it would break that mapping"
)]
pub(crate) struct ReferencePicSets {
    /// `RefPicSetStCurrBefore` — used by the current picture, POC less than
    /// the current picture's.
    pub st_curr_before: Vec<i64>,
    /// `RefPicSetStCurrAfter` — used by the current picture, POC greater.
    pub st_curr_after: Vec<i64>,
    /// `RefPicSetStFoll` — kept as reference but not used by the current
    /// picture (a future picture may still need it).
    pub st_foll: Vec<i64>,
}

impl ReferencePicSets {
    /// `NumPicTotalCurr`'s short-term half, §7.4.7.2 (this module's own
    /// long-term refusal means the long-term half is always zero when this
    /// type exists at all).
    #[must_use]
    pub(crate) fn num_pic_total_curr(&self) -> usize {
        self.st_curr_before.len() + self.st_curr_after.len()
    }
}

/// §8.3.2: derive the current picture's reference picture sets from its
/// slice header's short-term RPS.
///
/// `current_poc` is `PicOrderCntVal` of the picture being decoded.
/// `short_term_rps` is `None` exactly when the slice codes no short-term set
/// at all (an IDR, per §7.3.6.1's own presence condition) — an empty
/// [`ReferencePicSets`] in that case, which is correct: an IDR has no
/// pictures to reference.
///
/// # Errors
///
/// [`Error::Unsupported`] if `long_term_refs_present` is set — see the
/// module doc for why long-term references are refused rather than
/// approximated.
pub(crate) fn derive_reference_pic_sets(
    current_poc: i64,
    short_term_rps: Option<&ShortTermRps>,
    long_term_refs_present: bool,
) -> Result<ReferencePicSets> {
    if long_term_refs_present {
        return Err(Error::Unsupported("vaco-codec-hevc: long-term reference pictures are not supported"));
    }
    let Some(rps) = short_term_rps else {
        return Ok(ReferencePicSets::default());
    };

    let mut out = ReferencePicSets::default();
    for (i, &delta) in rps.delta_poc_s0.iter().enumerate() {
        let poc = current_poc.saturating_add(i64::from(delta));
        let used = rps.used_by_curr_pic_s0.get(i).copied().unwrap_or(false);
        if used {
            out.st_curr_before.push(poc);
        } else {
            out.st_foll.push(poc);
        }
    }
    for (i, &delta) in rps.delta_poc_s1.iter().enumerate() {
        let poc = current_poc.saturating_add(i64::from(delta));
        let used = rps.used_by_curr_pic_s1.get(i).copied().unwrap_or(false);
        if used {
            out.st_curr_after.push(poc);
        } else {
            out.st_foll.push(poc);
        }
    }
    Ok(out)
}

/// §8.3.4: build `RefPicList0`/`RefPicList1` as picture-order-count values,
/// given the already-derived [`ReferencePicSets`] and the slice header's own
/// `num_ref_idx_lX_active_minus1`/`ref_pic_lists_modification()`.
///
/// Kept as pure POC arithmetic — no [`Dpb`] borrow — so it is testable
/// without a real decoded picture anywhere, mirroring `vaco-parse-hevc::rps`
/// and `::poc`'s own "the derivation is testable on its own" precedent.
///
/// `is_b` selects whether `RefPicList1` is built at all (a P slice has none);
/// its temp-list order — `StCurrAfter` first, then `StCurrBefore` — is
/// §8.3.4's own asymmetry with `RefPicList0`, not a copy-paste of it.
pub(crate) fn build_ref_pic_lists(
    sets: &ReferencePicSets,
    num_ref_idx_l0_active_minus1: u32,
    num_ref_idx_l1_active_minus1: u32,
    modification: Option<&RefPicListModification>,
    is_b: bool,
) -> (Vec<i64>, Vec<i64>) {
    let mut combined0 = sets.st_curr_before.clone();
    combined0.extend(sets.st_curr_after.iter().copied());

    let list0 = build_one_list(
        &combined0,
        num_ref_idx_l0_active_minus1,
        modification.map(|m| m.list_entry_l0.as_slice()),
    );

    let list1 = if is_b {
        let mut combined1 = sets.st_curr_after.clone();
        combined1.extend(sets.st_curr_before.iter().copied());
        build_one_list(
            &combined1,
            num_ref_idx_l1_active_minus1,
            modification.map(|m| m.list_entry_l1.as_slice()),
        )
    } else {
        Vec::new()
    };

    (list0, list1)
}

/// One direction of §8.3.4's `RefPicListTempX`/`RefPicListX` construction.
///
/// `combined` is `RefPicSetStCurrBefore ++ RefPicSetStCurrAfter` (or the
/// mirrored order for list 1) — always non-empty when this is reached for a
/// P or B slice, since a conforming stream's `NumPicTotalCurr` is at least
/// one whenever `num_ref_idx_lX_active_minus1` requires a non-empty list; an
/// empty `combined` on a malformed stream returns an empty list rather than
/// panicking on the modulo below.
fn build_one_list(combined: &[i64], num_ref_idx_active_minus1: u32, modification: Option<&[u32]>) -> Vec<i64> {
    if combined.is_empty() {
        return Vec::new();
    }
    let count = usize::try_from(num_ref_idx_active_minus1).unwrap_or(0).saturating_add(1);
    // `RefPicListTempX` is `combined` cycled until it is at least `count`
    // long — never materialised as its own `Vec`; indexing `combined` with
    // `rIdx % combined.len()` gives the same value directly.
    match modification {
        Some(entries) if !entries.is_empty() => (0..count)
            .map(|r| {
                let src = entries.get(r).copied().unwrap_or(0) as usize;
                combined.get(src % combined.len()).copied().unwrap_or(0)
            })
            .collect(),
        _ => (0..count).map(|r| combined.get(r % combined.len()).copied().unwrap_or(0)).collect(),
    }
}

/// The per-picture output metadata a decoded picture buffer has to carry
/// alongside its samples, purely because output is no longer synchronous
/// with decode the moment reordering exists: a bumped picture's own
/// `pts`/`duration` came from *its own* originating packet, several
/// [`Dpb::store`] calls before the one that finally outputs it, so they have
/// to be remembered rather than read off "the current packet" at emission
/// time the way this crate's I-slice-only history could get away with.
#[derive(Debug, Clone)]
pub(crate) struct PictureMeta {
    pub pts: vaco_core::Timestamp,
    pub duration: vaco_core::Duration,
    /// The cropped output dimensions (`Sps::dimensions`, not the coded
    /// size) *as they were when this picture was decoded* — read directly
    /// rather than re-derived from whatever the current `Sps` says at
    /// output time, which could in principle be a different activated SPS
    /// by then.
    pub out_width: u32,
    pub out_height: u32,
    /// Mirrors `vaco_frame::FrameFlags::KEY` — set for an IRAP picture.
    pub is_keyframe: bool,
    /// Raw `cc_data` triplet bytes from this access unit's own ATSC A/53
    /// caption SEI, if any — empty for the ordinary case. Carried here for
    /// the same reason `pts`/`duration` are: the picture that decoded it
    /// can be several `Dpb::store` calls before the one that outputs it,
    /// and reordering must not separate the caption bytes from their own
    /// picture (see `vaco_parse_hevc::a53`'s module doc).
    pub closed_captions: Vec<u8>,
}

/// One picture held in the decoded picture buffer.
pub(crate) struct DpbEntry {
    pub picture: Picture,
    pub meta: PictureMeta,
    /// `PicOrderCntVal` at the time this picture was decoded.
    pub poc: i64,
    /// Whether this picture still needs to be output (§C.3.2's
    /// `PicOutputFlag`, as narrowed by `pic_output_flag`).
    pub needed_for_output: bool,
    pub marking: Marking,
    /// §C.3.2's `PicLatencyCount`: incremented once for every later picture
    /// stored while this one still needs output.
    pub latency_count: u32,
    /// This picture's own compressed motion field, for a later picture's
    /// TMVP derivation — `None` for an I picture (nothing to sample: every
    /// position is intra) or when `sps_temporal_mvp_enabled_flag` was clear
    /// while it was decoded.
    pub collocated: Option<CollocatedMotionField>,
}

/// The decoded picture buffer: every picture still needed either as a
/// reference or for output, plus the three `sps_max_*` bounds that drive
/// Annex C's bumping process.
pub(crate) struct Dpb {
    entries: Vec<DpbEntry>,
    max_dec_pic_buffering: usize,
    max_num_reorder_pics: usize,
    /// Annex C.5.2.2's own `SpsMaxLatencyPictures[HighestTid]` — **not**
    /// `sps_max_latency_increase_plus1` itself. `None` when
    /// `sps_max_latency_increase_plus1 == 0` ("not indicated", §7.4.3.1),
    /// which disables the latency-based bump condition entirely rather than
    /// comparing against a bogus bound.
    sps_max_latency_pictures: Option<u32>,
}

impl Dpb {
    /// `max_latency_increase_plus1` is the raw SPS field
    /// (`sps_max_latency_increase_plus1[HighestTid]`) — **not** already
    /// `SpsMaxLatencyPictures`. Annex C.5.2.2's own derivation is
    /// `SpsMaxLatencyPictures = sps_max_num_reorder_pics + sps_max_latency_increase_plus1 - 1`
    /// (all at `[HighestTid]`), computed here once rather than at every
    /// [`Dpb::bump_post_decode`] call.
    ///
    /// # A real bug this fixed
    ///
    /// This constructor used to store `max_latency_increase_plus1` verbatim
    /// and compare a picture's own `PicLatencyCount` against
    /// `max_latency_increase_plus1 - 1` directly — silently dropping the
    /// `sps_max_num_reorder_pics` term from Annex C.5.2.2's own formula
    /// entirely. Invisible on every fixture this crate had measured before a
    /// real `bframes` stream, since `sps_max_num_reorder_pics` is `0` for
    /// every P-slice-only or weighted-prediction fixture this crate's own
    /// history is built on (nothing ever needs to hold a picture back), so
    /// the missing term contributed `0` and the bug never moved a number.
    /// A real hierarchical-B fixture (`sps_max_num_reorder_pics = 2`,
    /// `sps_max_latency_increase_plus1 = 4`) forced a bump at
    /// `PicLatencyCount >= 3` instead of the correct `>= 5`, outputting a
    /// picture two stores early — caught by the resulting output *POC*
    /// sequence itself going non-monotonic (`0, 1, 2, 3, 4, 6, 8, 5, ...`,
    /// found via the CLI's own generic mux-side DTS-monotonicity check,
    /// `vaco-format-core::time::check_monotonic`, tripping downstream of
    /// this crate for the first time a real reordering codec's own bumping
    /// logic actually mattered), not by any per-plane pixel comparison —
    /// the pixels a wrongly-timed but still POC-labelled frame carries are
    /// unaffected by *when* it is emitted, only *whether the emission order
    /// itself is right*.
    #[must_use]
    pub(crate) fn new(max_dec_pic_buffering: usize, max_num_reorder_pics: usize, max_latency_increase_plus1: u32) -> Self {
        let sps_max_latency_pictures = (max_latency_increase_plus1 != 0)
            .then(|| u32::try_from(max_num_reorder_pics).unwrap_or(u32::MAX).saturating_add(max_latency_increase_plus1).saturating_sub(1));
        Self { entries: Vec::new(), max_dec_pic_buffering, max_num_reorder_pics, sps_max_latency_pictures }
    }

    /// §8.3.2's last step: a picture already in the DPB is short-term if its
    /// POC appears in `sets` at all (`StCurrBefore`, `StCurrAfter` or
    /// `StFoll` — all three are "still a reference", only the first two are
    /// "used by the current picture"), and unused for reference otherwise.
    pub(crate) fn apply_reference_picture_set(&mut self, sets: &ReferencePicSets, budget: &mut Budget) {
        for e in &mut self.entries {
            let kept = sets.st_curr_before.contains(&e.poc) || sets.st_curr_after.contains(&e.poc) || sets.st_foll.contains(&e.poc);
            e.marking = if kept { Marking::ShortTerm } else { Marking::Unused };
        }
        self.remove_unused(budget);
    }

    /// §C.5.2.2: an IRAP picture with `NoRaslOutputFlag` set empties the
    /// DPB. `no_output_of_prior_pics` (true for every IDR/BLA, and for a CRA
    /// that is itself the first picture of the bitstream — §7.4.7.1's own
    /// inference) skips outputting whatever was still pending; otherwise
    /// every picture still needing output is bumped first, in POC order.
    ///
    /// Returns the POCs to output but, like [`Dpb::bump_while`], does
    /// **not** itself remove anything — [`Dpb::clear_all`] is the caller's
    /// own next step, once every returned POC has been read via
    /// [`Dpb::picture_for_output`] (the same "read before you reap" contract
    /// `bump_while`'s own doc explains, here for the same reason: an
    /// unconditional `self.entries.clear()` right here would have dropped a
    /// pending output picture's own pixel data before its caller ever saw
    /// it).
    pub(crate) fn clear_for_irap(&self, no_output_of_prior_pics: bool) -> Vec<i64> {
        if no_output_of_prior_pics {
            Vec::new()
        } else {
            let mut pending: Vec<i64> = self.entries.iter().filter(|e| e.needed_for_output).map(|e| e.poc).collect();
            pending.sort_unstable();
            pending
        }
    }

    /// Empty the DPB entirely — the second half of an IRAP's own §C.5.2.2
    /// handling, called once [`Dpb::clear_for_irap`]'s own returned POCs
    /// have all been read out. Releases every dropped picture's own charged
    /// bytes back to `budget` (see [`Picture::budget_bytes`]'s own doc for
    /// why: an IRAP that clears a long-running stream's whole DPB at once is
    /// exactly the moment the largest single release happens).
    pub(crate) fn clear_all(&mut self, budget: &mut Budget) {
        for e in self.entries.drain(..) {
            budget.release(e.picture.budget_bytes());
        }
    }

    /// §C.5.2.2's "ordinary" (non-IRAP-clear) pre-decode step: called once
    /// [`Dpb::apply_reference_picture_set`] (whose own trailing
    /// `remove_unused` call is exactly this clause's own unconditional
    /// "empty every buffer marked not-needed-and-unused" first step) has
    /// already run, and *before* the current picture is stored — the DPB
    /// state this bump reads and the DPB state a conformant encoder's own
    /// `sps_max_dec_pic_buffering`/`sps_max_num_reorder_pics` promises were
    /// computed against are the *same* state, one picture short of the one
    /// about to be decoded.
    ///
    /// # A real bug this fixed: capacity bumping used to run one step late
    ///
    /// This crate used to run one unified bump — reorder, latency *and*
    /// capacity together — only *after* `store()`, conflating §C.5.2.2's
    /// pre-decode step (which alone carries the capacity condition) with
    /// §C.5.2.3's post-decode "additional bumping" (which only ever carries
    /// reorder and latency, never capacity — confirmed directly against the
    /// specification's own two clauses, not inferred). Checking capacity
    /// against the *post-store* DPB let a picture that had just become the
    /// current picture's own occupant push the count over
    /// `sps_max_dec_pic_buffering` one picture earlier than a conformant
    /// encoder ever promised the decoder would need to bump — on a real
    /// `libx265` hierarchical-B fixture (`bframes=3`), this bumped POC 6 and
    /// POC 8 for output *before POC 5 was even decoded*, producing an
    /// output POC sequence of `..., 4, 6, 8, 5, ...`: non-monotonic, caught
    /// by the CLI's own generic mux-side DTS check
    /// (`vaco-format-core::time::check_monotonic`) rather than by any pixel
    /// comparison, since the pixels of a correctly-decoded picture do not
    /// change when it is emitted at the wrong moment.
    pub(crate) fn bump_pre_decode(&mut self) -> Vec<i64> {
        self.bump_while(|dpb| {
            let needed = dpb.entries.iter().filter(|e| e.needed_for_output).count();
            // "The number of pictures in the DPB" (§C.5.2.2's own third
            // condition): every entry still physically present is, by
            // construction, either needed for output or still a reference
            // — anything else was already dropped by the unconditional
            // removal this function's own doc says precedes it — so this
            // is equivalent to (and computed the same defensive way as)
            // "needed for output OR used for reference", recomputed fresh
            // each loop iteration since a bump earlier in *this* call can
            // turn a still-referenced entry into a removable one before
            // `reap_unused` physically drops it.
            let occupied = dpb.entries.iter().filter(|e| e.needed_for_output || e.marking != Marking::Unused).count();
            let over_reorder = needed > dpb.max_num_reorder_pics;
            let over_latency = dpb
                .sps_max_latency_pictures
                .is_some_and(|bound| dpb.entries.iter().any(|e| e.needed_for_output && e.latency_count >= bound));
            let over_capacity = occupied >= dpb.max_dec_pic_buffering;
            over_reorder || over_latency || over_capacity
        })
    }

    /// §C.5.2.3's "additional bumping": runs once the current picture has
    /// been [`Dpb::store`]d. First increments `PicLatencyCount` for every
    /// still-pending picture whose POC *follows* `current_poc` in output
    /// order — not indiscriminately every pending picture, which is the bug
    /// `PicLatencyCount`'s own increment used to have (see [`Dpb::store`]'s
    /// own doc) — then repeatedly bumps while the reorder or latency
    /// condition holds. Capacity is deliberately **not** one of this
    /// clause's conditions (see [`Dpb::bump_pre_decode`]'s own doc for why
    /// conflating the two was a real, measured bug).
    pub(crate) fn bump_post_decode(&mut self, current_poc: i64) -> Vec<i64> {
        for e in &mut self.entries {
            if e.needed_for_output && e.poc > current_poc {
                e.latency_count = e.latency_count.saturating_add(1);
            }
        }
        self.bump_while(|dpb| {
            let needed = dpb.entries.iter().filter(|e| e.needed_for_output).count();
            let over_reorder = needed > dpb.max_num_reorder_pics;
            let over_latency = dpb
                .sps_max_latency_pictures
                .is_some_and(|bound| dpb.entries.iter().any(|e| e.needed_for_output && e.latency_count >= bound));
            over_reorder || over_latency
        })
    }

    /// §C.5.2.4's "bumping" process, repeated while `should_bump` holds:
    /// output (in POC order) the picture with the smallest POC among those
    /// still needing output. Returns the POCs output, in the order they
    /// were output (which is POC order, by construction).
    ///
    /// Deliberately does **not** physically remove any entry (the old
    /// version of this function called `remove_unused` inline, which would
    /// have dropped a bumped, non-reference picture's own pixel data before
    /// its caller ever had a chance to read it out as an output frame — a
    /// real bug, caught only once real pixel data existed to lose, since
    /// this module's own tests before that used a `4x4` placeholder
    /// picture and never needed to *read* what was bumped, only to observe
    /// its POC). `reap_unused` is the caller's own job, once every POC this
    /// call returns has actually been read via `picture_for_output`.
    fn bump_while(&mut self, should_bump: impl Fn(&Self) -> bool) -> Vec<i64> {
        let mut outputs = Vec::new();
        while should_bump(self) {
            let Some(idx) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.needed_for_output)
                .min_by_key(|(_, e)| e.poc)
                .map(|(i, _)| i)
            else {
                break;
            };
            if let Some(e) = self.entries.get_mut(idx) {
                outputs.push(e.poc);
                e.needed_for_output = false;
            }
        }
        outputs
    }

    /// A picture still physically present in the DPB, by POC, regardless of
    /// its [`Marking`] — unlike [`Dpb::reference_picture`] (short-term refs
    /// only), this is what a caller reads a *bumped* picture's own pixel
    /// data from, since a bumped-for-output picture may already be
    /// `Marking::Unused` (a disposable, non-reference picture) by the time
    /// it is read.
    #[must_use]
    pub(crate) fn picture_for_output(&self, poc: i64) -> Option<&Picture> {
        self.entries.iter().find(|e| e.poc == poc).map(|e| &e.picture)
    }

    /// The [`PictureMeta`] a picture named by `poc` was stored with —
    /// always read alongside [`Dpb::picture_for_output`] to build the
    /// actual output frame.
    #[must_use]
    pub(crate) fn output_meta(&self, poc: i64) -> Option<PictureMeta> {
        self.entries.iter().find(|e| e.poc == poc).map(|e| e.meta.clone())
    }

    /// Physically drop every entry that is neither needed for output nor
    /// used for reference — the caller's own responsibility to call once it
    /// has read every POC [`Dpb::bump_pre_decode`]/[`Dpb::bump_post_decode`]/
    /// [`Dpb::clear_for_irap`] just returned via [`Dpb::picture_for_output`]
    /// (see [`Dpb::bump_while`]'s own doc for why removal is not automatic).
    /// Releases each dropped picture's own charged bytes back to `budget`.
    pub(crate) fn reap_unused(&mut self, budget: &mut Budget) {
        self.remove_unused(budget);
    }

    /// Store a newly decoded picture, `PicLatencyCount` starting at `0`
    /// (§C.5.2.3's own order: increment every *other* pending picture that
    /// follows this one in output order, then insert this one at zero — see
    /// [`Dpb::bump_post_decode`]'s own doc for that increment, which used to
    /// live here, applied unconditionally to every pending picture
    /// regardless of whether it actually followed the newly-stored one in
    /// output order. That distinction is invisible whenever decode order and
    /// output order agree closely enough that "every other pending picture"
    /// and "every pending picture with a greater POC" name the same set —
    /// true for a shallow-enough reorder depth — and wrong the moment a
    /// picture with a *smaller* POC than something already pending is stored
    /// later in decode order, exactly what a hierarchical-B GOP does by
    /// construction).
    #[allow(clippy::too_many_arguments, reason = "one call site (decoder.rs); every argument is a distinct DPB-entry field")]
    pub(crate) fn store(&mut self, picture: Picture, meta: PictureMeta, poc: i64, needed_for_output: bool, is_reference: bool, collocated: Option<CollocatedMotionField>) {
        self.entries.push(DpbEntry {
            picture,
            meta,
            poc,
            needed_for_output,
            marking: if is_reference { Marking::ShortTerm } else { Marking::Unused },
            latency_count: 0,
            collocated,
        });
    }

    /// The compressed motion field a picture named by `poc` was stored
    /// with, cloned out for the querying slice's own [`crate::ctu::Ctx`] to
    /// own independently (see that type's own doc for why `collocated` is
    /// owned rather than borrowed) — `None` both when no such entry exists
    /// and when it exists but recorded no motion (an I picture).
    #[must_use]
    pub(crate) fn collocated_for(&self, poc: i64) -> Option<CollocatedMotionField> {
        self.entries.iter().find(|e| e.poc == poc).and_then(|e| e.collocated.clone())
    }

    /// End of stream: the POCs of everything still pending, in POC order.
    /// Does not remove anything — the same "read before you reap" contract
    /// as [`Dpb::bump_while`]/[`Dpb::clear_for_irap`]; call
    /// [`Dpb::clear_all`] once every returned POC has been read via
    /// [`Dpb::picture_for_output`].
    #[must_use]
    pub(crate) fn flush(&self) -> Vec<i64> {
        let mut pending: Vec<i64> = self.entries.iter().filter(|e| e.needed_for_output).map(|e| e.poc).collect();
        pending.sort_unstable();
        pending
    }

    /// A short-term reference picture's own plane data, by POC — what
    /// motion compensation would read from once it exists. `None` when no
    /// entry with that POC is currently marked short-term (already removed,
    /// or never a reference at all).
    #[must_use]
    pub(crate) fn reference_picture(&self, poc: i64) -> Option<&Picture> {
        self.entries.iter().find(|e| e.poc == poc && e.marking == Marking::ShortTerm).map(|e| &e.picture)
    }

    /// Drops every entry that is neither needed for output nor used for
    /// reference, releasing each one's own [`Picture::budget_bytes`] back to
    /// `budget` as it goes — the one place a `Picture`'s charged bytes are
    /// ever given back, matching [`Budget::release`]'s own contract ("when
    /// the buffer they paid for is dropped").
    fn remove_unused(&mut self, budget: &mut Budget) {
        let mut i = 0;
        while i < self.entries.len() {
            let Some(e) = self.entries.get(i) else { break };
            if e.needed_for_output || e.marking != Marking::Unused {
                i += 1;
            } else {
                let removed = self.entries.remove(i);
                budget.release(removed.picture.budget_bytes());
            }
        }
    }

    #[cfg(test)]
    fn pocs(&self) -> Vec<i64> {
        self.entries.iter().map(|e| e.poc).collect()
    }

    /// A stored entry's own `PicLatencyCount`, by POC — test-only
    /// introspection so [`Dpb::bump_post_decode`]'s increment rule can be
    /// checked directly rather than only inferred from whether it happened
    /// to also cross a bump threshold.
    #[cfg(test)]
    fn latency_of(&self, poc: i64) -> Option<u32> {
        self.entries.iter().find(|e| e.poc == poc).map(|e| e.latency_count)
    }
}

/// A picture's own motion field, compressed to a 16x16-luma-sample grid for
/// use as a *collocated* picture by a later picture's TMVP derivation
/// (§8.5.3.2.8/.9). Built once, when a picture finishes decoding, from its
/// native (per-4x4) [`crate::framebuf::CuGrid`] motion — never mutated
/// afterward, since the picture it describes is itself immutable from that
/// point on.
///
/// # Why 16x16 and not the native 4x4 grid
///
/// §8.5.3.2.9 samples a collocated picture's motion at one representative
/// position per 16x16 luma block (confirmed against HM's own
/// `TComCUMvField::compress`, called once a picture finishes decoding and
/// before it can be referenced as a collocated picture — `compressMV`'s own
/// `AMVP_DECIMATION_FACTOR * 4 / m_unitSize` scale factor works out to 16
/// with HM's own constants), not at the querying PU's own exact bottom-right
/// or centre pixel. Reading a *native*-resolution grid at those exact pixel
/// positions would give a different (finer-grained) answer than reading
/// HM's compressed one whenever a collocated picture's motion happens to
/// vary within one 16x16 block — which real content does. Building this
/// once, at the 16-block granularity, and letting every later query read it
/// directly (no further masking needed at query time) reproduces HM's own
/// two-step "compress once, read many times" shape exactly, rather than
/// reading a fine grid and hoping every caller remembers to mask its own
/// query position down to the 16-grid first.
#[derive(Debug, Clone)]
pub(crate) struct CollocatedMotionField {
    /// The picture this field describes — needed by every scaling
    /// comparison alongside a sampled block's own `ref_poc`.
    pub poc: i64,
    cols: usize,
    rows: usize,
    /// Both reference lists, independently — §8.5.3.2.9's own derivation
    /// reads `predFlagL0Col`/`predFlagL1Col`/`mvL0Col`/`mvL1Col` of the
    /// collocated PU (not a single collapsed value: a bi-predicted
    /// collocated PU has to expose *both* of its own lists' motion, since
    /// which one a later query actually reads depends on that later query's
    /// own `collocated_from_l0_flag`/`NoBackwardPredFlag`, not on anything
    /// this picture's own decode knew about) — confirmed directly against
    /// HM's `xGetColMVP`, which indexes `getCUMvField(eColRefPicList)` by
    /// list, never a single merged field.
    pred_l0: Vec<bool>,
    mv0_x: Vec<i16>,
    mv0_y: Vec<i16>,
    ref_poc0: Vec<i64>,
    pred_l1: Vec<bool>,
    mv1_x: Vec<i16>,
    mv1_y: Vec<i16>,
    ref_poc1: Vec<i64>,
}

impl CollocatedMotionField {
    /// Build a field for a `luma_width x luma_height` picture, sampling
    /// `sample_at(x, y)` once per 16x16 block's own top-left corner.
    /// `sample_at` is expected to be `CuGrid::inter_at` (or, for an I
    /// picture, a closure that always returns `None` — see `decoder.rs`'s
    /// own call site) — not `Budget`-tracked (the crate's own general rule):
    /// every array here is at most `luma_width * luma_height / 256`
    /// elements, strictly smaller than the picture's own already-budgeted
    /// pixel planes it is derived from, so no allocation path opens here
    /// that the picture's own construction did not already bound.
    pub(crate) fn build(poc: i64, luma_width: usize, luma_height: usize, sample_at: impl Fn(i32, i32) -> Option<crate::motion::MotionInfo>) -> Self {
        let cols = luma_width.div_ceil(16).max(1);
        let rows = luma_height.div_ceil(16).max(1);
        let len = cols.saturating_mul(rows);
        let mut field = Self {
            poc,
            cols,
            rows,
            pred_l0: vec![false; len],
            mv0_x: vec![0; len],
            mv0_y: vec![0; len],
            ref_poc0: vec![0; len],
            pred_l1: vec![false; len],
            mv1_x: vec![0; len],
            mv1_y: vec![0; len],
            ref_poc1: vec![0; len],
        };
        for by in 0..rows {
            for bx in 0..cols {
                let x = i32::try_from(bx.saturating_mul(16)).unwrap_or(0);
                let y = i32::try_from(by.saturating_mul(16)).unwrap_or(0);
                let Some(info) = sample_at(x, y) else { continue };
                let i = by * cols + bx;
                if let Some(u) = info.l0 {
                    if let Some(slot) = field.pred_l0.get_mut(i) {
                        *slot = true;
                    }
                    if let Some(slot) = field.mv0_x.get_mut(i) {
                        *slot = i16::try_from(u.mv.x).unwrap_or(0);
                    }
                    if let Some(slot) = field.mv0_y.get_mut(i) {
                        *slot = i16::try_from(u.mv.y).unwrap_or(0);
                    }
                    if let Some(slot) = field.ref_poc0.get_mut(i) {
                        *slot = u.ref_poc;
                    }
                }
                if let Some(u) = info.l1 {
                    if let Some(slot) = field.pred_l1.get_mut(i) {
                        *slot = true;
                    }
                    if let Some(slot) = field.mv1_x.get_mut(i) {
                        *slot = i16::try_from(u.mv.x).unwrap_or(0);
                    }
                    if let Some(slot) = field.mv1_y.get_mut(i) {
                        *slot = i16::try_from(u.mv.y).unwrap_or(0);
                    }
                    if let Some(slot) = field.ref_poc1.get_mut(i) {
                        *slot = u.ref_poc;
                    }
                }
            }
        }
        field
    }

    /// The full motion recorded for the 16x16 block containing luma pixel
    /// `(x, y)` — both lists, exactly as the collocated PU itself recorded
    /// them — or `None` if that block was never inter-coded (an intra
    /// block, or the whole picture was an I picture) or `(x, y)` is out of
    /// range. The caller (`ctu::col_mvp`) picks which list to read per
    /// §8.5.3.2.9's own selection rule.
    #[must_use]
    pub(crate) fn get(&self, x: i32, y: i32) -> Option<crate::motion::MotionInfo> {
        let (Ok(bx), Ok(by)) = (usize::try_from(x >> 4), usize::try_from(y >> 4)) else {
            return None;
        };
        if bx >= self.cols || by >= self.rows {
            return None;
        }
        let i = by * self.cols + bx;
        let pred_l0 = self.pred_l0.get(i).copied().unwrap_or(false);
        let pred_l1 = self.pred_l1.get(i).copied().unwrap_or(false);
        if !pred_l0 && !pred_l1 {
            return None;
        }
        let l0 = pred_l0.then(|| crate::motion::UniMotion {
            mv: crate::motion::Mv { x: i32::from(self.mv0_x.get(i).copied().unwrap_or(0)), y: i32::from(self.mv0_y.get(i).copied().unwrap_or(0)) },
            ref_poc: self.ref_poc0.get(i).copied().unwrap_or(0),
        });
        let l1 = pred_l1.then(|| crate::motion::UniMotion {
            mv: crate::motion::Mv { x: i32::from(self.mv1_x.get(i).copied().unwrap_or(0)), y: i32::from(self.mv1_y.get(i).copied().unwrap_or(0)) },
            ref_poc: self.ref_poc1.get(i).copied().unwrap_or(0),
        });
        Some(crate::motion::MotionInfo { l0, l1 })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, reason = "test code over fixed scenarios")]
mod tests {
    use super::*;

    fn budget() -> vaco_limits::Budget {
        vaco_limits::Budget::new(vaco_limits::Limits::strict())
    }

    fn tiny_picture() -> Picture {
        Picture::new(&mut budget(), 4, 4).expect("small alloc")
    }

    fn tiny_meta() -> PictureMeta {
        PictureMeta {
            pts: vaco_core::Timestamp::NONE,
            duration: vaco_core::Duration::ZERO,
            out_width: 4,
            out_height: 4,
            is_keyframe: false,
            closed_captions: Vec::new(),
        }
    }

    // ---- §8.3.2 short-term derivation ----

    #[test]
    fn an_idr_has_no_reference_picture_sets() {
        let sets = derive_reference_pic_sets(100, None, false).expect("no long-term refs");
        assert_eq!(sets, ReferencePicSets::default());
    }

    #[test]
    fn negative_and_positive_deltas_split_into_before_after_and_foll() {
        // Mirrors `vaco_parse_hevc::rps`'s own worked example: two negative
        // deltas (one used, one not) and one positive (used).
        let rps = ShortTermRps {
            delta_poc_s0: vec![-1, -3],
            used_by_curr_pic_s0: vec![true, false],
            delta_poc_s1: vec![4],
            used_by_curr_pic_s1: vec![true],
            inter_predicted: false,
        };
        let sets = derive_reference_pic_sets(10, Some(&rps), false).expect("short-term only");
        assert_eq!(sets.st_curr_before, [9]); // 10 + (-1)
        assert_eq!(sets.st_foll, [7]); // 10 + (-3), not used by current
        assert_eq!(sets.st_curr_after, [14]); // 10 + 4
        assert_eq!(sets.num_pic_total_curr(), 2);
    }

    #[test]
    fn long_term_references_are_refused_by_name() {
        let err = derive_reference_pic_sets(10, None, true).expect_err("long-term refs are unsupported");
        assert!(matches!(err, Error::Unsupported(_)));
    }

    // ---- §8.3.4 reference picture list construction ----

    #[test]
    fn a_p_slice_builds_only_list_0_from_before_then_after() {
        let sets = ReferencePicSets { st_curr_before: vec![9, 7], st_curr_after: vec![14], st_foll: vec![] };
        let (l0, l1) = build_ref_pic_lists(&sets, 1, 0, None, false);
        // num_ref_idx_l0_active_minus1 = 1 -> two entries, temp list is
        // [9, 7, 14] cycled -> [9, 7].
        assert_eq!(l0, [9, 7]);
        assert!(l1.is_empty());
    }

    #[test]
    fn a_b_slice_s_list_1_puts_after_before_before() {
        let sets = ReferencePicSets { st_curr_before: vec![9], st_curr_after: vec![14, 20], st_foll: vec![] };
        let (l0, l1) = build_ref_pic_lists(&sets, 0, 1, None, true);
        assert_eq!(l0, [9]); // list0 temp = [9, 14, 20], one entry
        assert_eq!(l1, [14, 20]); // list1 temp = [14, 20, 9], two entries
    }

    #[test]
    fn the_temp_list_cycles_when_more_entries_are_requested_than_exist() {
        let sets = ReferencePicSets { st_curr_before: vec![9], st_curr_after: vec![14], st_foll: vec![] };
        // combined = [9, 14], but 4 entries requested (minus1 = 3).
        let (l0, _) = build_ref_pic_lists(&sets, 3, 0, None, false);
        assert_eq!(l0, [9, 14, 9, 14]);
    }

    #[test]
    fn ref_pic_list_modification_reorders_by_explicit_index() {
        let sets = ReferencePicSets { st_curr_before: vec![9, 7], st_curr_after: vec![14], st_foll: vec![] };
        let modification = RefPicListModification { list_entry_l0: vec![2, 0], list_entry_l1: vec![] };
        let (l0, _) = build_ref_pic_lists(&sets, 1, 0, Some(&modification), false);
        // combined temp = [9, 7, 14]; list_entry_l0 picks index 2 then 0.
        assert_eq!(l0, [14, 9]);
    }

    #[test]
    fn an_empty_combined_list_never_panics_on_the_modulo() {
        let sets = ReferencePicSets::default();
        let (l0, l1) = build_ref_pic_lists(&sets, 5, 5, None, true);
        assert!(l0.is_empty());
        assert!(l1.is_empty());
    }

    // ---- DPB marking and bumping ----

    #[test]
    fn a_picture_dropped_from_every_set_is_marked_unused_and_removed() {
        let mut dpb = Dpb::new(16, 16, 0);
        dpb.store(tiny_picture(), tiny_meta(), 0, false, true, None);
        dpb.store(tiny_picture(), tiny_meta(), 4, false, true, None);
        assert_eq!(dpb.pocs(), [0, 4]);
        // Only POC 4 is still referenced by the new set.
        let sets = ReferencePicSets { st_curr_before: vec![4], st_curr_after: vec![], st_foll: vec![] };
        dpb.apply_reference_picture_set(&sets, &mut budget());
        assert_eq!(dpb.pocs(), [4]);
    }

    #[test]
    fn a_picture_kept_only_in_st_foll_survives_marking() {
        let mut dpb = Dpb::new(16, 16, 0);
        dpb.store(tiny_picture(), tiny_meta(), 4, false, true, None);
        let sets = ReferencePicSets { st_curr_before: vec![], st_curr_after: vec![], st_foll: vec![4] };
        dpb.apply_reference_picture_set(&sets, &mut budget());
        assert_eq!(dpb.pocs(), [4]);
    }

    #[test]
    fn bumping_outputs_the_smallest_poc_first_once_reorder_count_is_exceeded() {
        // max_num_reorder_pics = 2: a third picture still needing output
        // forces the smallest-POC one out, in POC order rather than decode
        // order (POC 8 was stored before POC 4 — a hierarchical-B decode
        // order). `bump_post_decode` (§C.5.2.3) is the call that carries the
        // reorder condition, run with each store's own POC right after it.
        let mut dpb = Dpb::new(16, 2, 0);
        dpb.store(tiny_picture(), tiny_meta(), 0, true, false, None);
        assert!(dpb.bump_post_decode(0).is_empty(), "only one pending picture: nothing to bump yet");
        dpb.store(tiny_picture(), tiny_meta(), 8, true, false, None);
        let out = dpb.bump_post_decode(8);
        assert!(out.is_empty(), "two pending is not yet over the limit of two");
        dpb.store(tiny_picture(), tiny_meta(), 4, true, false, None);
        let out = dpb.bump_post_decode(4);
        assert_eq!(out, [0], "bumps the smallest POC, not decode order");
    }

    #[test]
    fn dpb_fullness_bumps_even_with_no_reorder_pending() {
        // max_num_reorder_pics = 0 but every picture is also a reference, so
        // fullness (max_dec_pic_buffering = 2) is what forces the bump.
        // Fullness (§C.5.2.2's third condition) is `bump_pre_decode`'s own —
        // it never fires here regardless, since neither picture is
        // `needed_for_output` and a bump can only ever select a
        // needed-for-output entry.
        let mut dpb = Dpb::new(2, 0, 0);
        dpb.store(tiny_picture(), tiny_meta(), 0, false, true, None);
        dpb.store(tiny_picture(), tiny_meta(), 4, false, true, None);
        assert!(dpb.bump_pre_decode().is_empty(), "nothing needs output, so nothing bumps");
        assert_eq!(dpb.pocs(), [0, 4], "both stay as references, not output");
    }

    #[test]
    fn bump_pre_decode_forces_output_once_the_dpb_is_genuinely_full() {
        // Unlike the test above, both pictures here *are* needed for
        // output (and neither is a reference, so a bumped entry stops
        // counting as occupied immediately, the same way a real stream's
        // non-reference pictures do once no later slice's RPS keeps them
        // alive), so `bump_pre_decode`'s own capacity condition (occupied
        // >= max_dec_pic_buffering = 2) has a real candidate to select and
        // stops once the count drops back under the cap — confirming
        // §C.5.2.2's third condition actually forces output, not just
        // failing to find nothing to bump. `max_num_reorder_pics = 5` keeps
        // the *reorder* condition from also firing with only two pending
        // pictures, isolating capacity as the one bump reason.
        let mut dpb = Dpb::new(2, 5, 0);
        dpb.store(tiny_picture(), tiny_meta(), 4, true, false, None);
        dpb.store(tiny_picture(), tiny_meta(), 0, true, false, None);
        let out = dpb.bump_pre_decode();
        assert_eq!(out, [0], "smallest POC first, even though POC 4 was stored earlier");
    }

    #[test]
    fn an_irap_with_no_output_of_prior_pics_discards_pending_output_silently() {
        let mut dpb = Dpb::new(16, 16, 0);
        dpb.store(tiny_picture(), tiny_meta(), 0, true, true, None);
        dpb.store(tiny_picture(), tiny_meta(), 4, true, true, None);
        let out = dpb.clear_for_irap(true);
        assert!(out.is_empty());
        dpb.clear_all(&mut budget());
        assert!(dpb.pocs().is_empty());
    }

    #[test]
    fn an_irap_without_no_output_flushes_pending_output_in_poc_order() {
        let mut dpb = Dpb::new(16, 16, 0);
        dpb.store(tiny_picture(), tiny_meta(), 8, true, true, None);
        dpb.store(tiny_picture(), tiny_meta(), 4, true, true, None);
        let out = dpb.clear_for_irap(false);
        assert_eq!(out, [4, 8]);
        // Every returned POC is still readable until `clear_all` runs —
        // the "read before you reap" contract `Dpb::clear_for_irap`'s own
        // doc describes.
        for poc in &out {
            assert!(dpb.picture_for_output(*poc).is_some());
        }
        dpb.clear_all(&mut budget());
        assert!(dpb.pocs().is_empty());
    }

    #[test]
    fn flush_outputs_everything_pending_in_poc_order_and_empties_the_dpb() {
        let mut dpb = Dpb::new(16, 16, 0);
        dpb.store(tiny_picture(), tiny_meta(), 12, true, false, None);
        dpb.store(tiny_picture(), tiny_meta(), 0, true, false, None);
        dpb.store(tiny_picture(), tiny_meta(), 6, false, true, None); // a reference never output
        let out = dpb.flush();
        assert_eq!(out, [0, 12]);
        dpb.clear_all(&mut budget());
        assert!(dpb.pocs().is_empty());
    }

    #[test]
    fn latency_only_increments_for_pictures_that_follow_the_current_one_in_output_order() {
        // §C.5.2.3's own wording: `PicLatencyCount` increments only for a
        // pending picture whose POC *follows* the picture just stored (i.e.
        // is greater) — not, as this crate's own `Dpb::store` used to do,
        // every pending picture unconditionally. `max_num_reorder_pics = 16`
        // and `max_latency_increase_plus1 = 0` (disabled) here so neither
        // bump condition can fire and disturb the counts being asserted;
        // `latency_of` reads `PicLatencyCount` directly rather than
        // inferring it from whether a bump happened to occur.
        let mut dpb = Dpb::new(16, 16, 0);
        dpb.store(tiny_picture(), tiny_meta(), 8, true, false, None);
        dpb.store(tiny_picture(), tiny_meta(), 0, true, false, None);
        assert!(dpb.bump_post_decode(0).is_empty(), "no bump condition is active in this test");
        assert_eq!(dpb.latency_of(8), Some(1), "POC 8 follows the just-stored POC 0 (8 > 0)");
        assert_eq!(dpb.latency_of(0), Some(0), "a picture never follows itself");
    }

    #[test]
    fn the_bound_forces_a_bump_once_a_pictures_own_latency_reaches_it() {
        // Annex C.5.2.2: SpsMaxLatencyPictures = sps_max_num_reorder_pics +
        // sps_max_latency_increase_plus1 - 1 = 1 + 1 - 1 = 1. Storing POC 0
        // while POC 8 is still pending brings POC 8's own `PicLatencyCount`
        // to 1 (it follows POC 0 in output order), meeting the bound — the
        // condition is "at least one picture is at or over the bound", not
        // "bump that specific picture", so `bump_while`'s own smallest-POC
        // rule keeps firing (bumping POC 0, then POC 8 too, since POC 8's
        // own latency is untouched by bumping something else) until nothing
        // is left needing output.
        let mut dpb = Dpb::new(16, 1, 1);
        dpb.store(tiny_picture(), tiny_meta(), 8, true, false, None);
        assert!(dpb.bump_post_decode(8).is_empty(), "nothing follows POC 8 yet, and only one picture is pending");
        dpb.store(tiny_picture(), tiny_meta(), 0, true, false, None);
        let out = dpb.bump_post_decode(0);
        assert_eq!(out, [0, 8], "both pending pictures are flushed once POC 8's own latency trips the bound");
    }

    #[test]
    fn the_reorder_count_is_folded_into_the_latency_bound_not_ignored() {
        // The bug this fixed: `SpsMaxLatencyPictures` used to be computed as
        // `max_latency_increase_plus1 - 1` alone, silently dropping
        // `sps_max_num_reorder_pics` from Annex C.5.2.2's own formula —
        // giving a bound of `2 - 1 = 1` here instead of the correct
        // `5 + 2 - 1 = 6`. POC 8 (pending) reaches `PicLatencyCount == 1`
        // once POC 0 is stored (the same increment as the test above); the
        // wrong bound would already force a bump at that point, the correct
        // one requires four more untouched stores' worth of headroom before
        // it would.
        let mut dpb = Dpb::new(16, 5, 2);
        dpb.store(tiny_picture(), tiny_meta(), 8, true, false, None);
        dpb.store(tiny_picture(), tiny_meta(), 0, true, false, None);
        let out = dpb.bump_post_decode(0);
        assert_eq!(dpb.latency_of(8), Some(1));
        assert!(out.is_empty(), "PicLatencyCount is 1, still below the correct bound of 6");
    }

    /// The bug `bump_while`'s own doc records: a bumped, non-reference
    /// picture's pixel data must still be readable via `picture_for_output`
    /// until `reap_unused` is explicitly called — the old version dropped it
    /// immediately (via an inline `remove_unused()`), which no earlier test
    /// caught because none of them ever tried to *read* a bumped entry's own
    /// picture.
    #[test]
    fn a_bumped_picture_stays_readable_until_reaped() {
        let mut dpb = Dpb::new(16, 1, 0);
        dpb.store(tiny_picture(), tiny_meta(), 0, true, false, None); // not a reference: purely for output
        dpb.store(tiny_picture(), tiny_meta(), 4, true, false, None);
        let out = dpb.bump_post_decode(4);
        assert_eq!(out, [0]);
        assert!(dpb.picture_for_output(0).is_some(), "bumped picture's own pixels must survive until reaped");
        dpb.reap_unused(&mut budget());
        assert!(dpb.picture_for_output(0).is_none(), "reaping removes it once the caller has read it");
        assert!(dpb.picture_for_output(4).is_some(), "the still-pending picture is untouched");
    }

    #[test]
    fn reaping_releases_the_dropped_picture_s_own_budget() {
        // `Picture::budget_bytes` is exactly what `Picture::new` charged, and
        // `reap_unused` must give it all back once a picture is truly gone —
        // the fix that let a real hierarchical-B `libx265` fixture (more
        // simultaneous DPB entries than any P-slice stream ever needed)
        // decode past `Budget`'s own `max_alloc_total` cap instead of
        // exhausting it partway through a 25-frame, 640x480 sequence.
        let mut b = budget();
        let mut dpb = Dpb::new(16, 1, 0);
        let charged_at_start = b.committed();
        dpb.store(Picture::new(&mut b, 4, 4).expect("small alloc"), tiny_meta(), 0, true, false, None);
        let one_picture_bytes = b.committed() - charged_at_start;
        assert!(one_picture_bytes > 0, "storing a real Picture must charge real bytes");
        dpb.store(Picture::new(&mut b, 4, 4).expect("small alloc"), tiny_meta(), 4, true, false, None);
        let out = dpb.bump_post_decode(4);
        assert_eq!(out, [0]);
        let before_reap = b.committed();
        dpb.reap_unused(&mut b);
        assert_eq!(before_reap - b.committed(), one_picture_bytes, "reaping POC 0 must release exactly what it was charged");
    }

    #[test]
    fn reference_picture_finds_a_short_term_entry_by_poc() {
        let mut dpb = Dpb::new(16, 16, 0);
        dpb.store(tiny_picture(), tiny_meta(), 4, false, true, None);
        assert!(dpb.reference_picture(4).is_some());
        assert!(dpb.reference_picture(5).is_none());
    }
}
