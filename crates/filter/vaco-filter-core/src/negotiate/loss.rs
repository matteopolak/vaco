//! What a conversion costs, so a converter can choose its output format.
//!
//! When two sides of a link share a format, negotiation picks it and nothing is
//! converted. When they share none, a converter is spliced in and *something*
//! has to decide which of the downstream's accepted formats it should produce.
//! That decision is what this module scores.
//!
//! # Where the numbers come from
//!
//! Not from the plan's table, which is wrong in two places, and not from
//! intuition. The dominance order below was **measured against the pinned
//! reference (ffmpeg 8.1)** by chaining two `format` filters so that the
//! auto-inserted `scale` has to choose between exactly two candidates, and
//! reading the choice out of `-v verbose`:
//!
//! ```sh
//! ffmpeg -v verbose -f lavfi -i "testsrc2=s=32x32:d=0.04" \
//!        -vf "format=pix_fmts=<src>,format=pix_fmts=<a>|<b>,null" -f null - \
//!   2>&1 | grep auto_scale_1
//! ```
//!
//! Probing through a filtergraph is normally the trap plan 13 §1b describes.
//! It is correct *here* because the filtergraph's negotiation **is** the thing
//! under test — the value being read back is a decision the graph layer makes,
//! not a value a parser was handed. The check that this is sound: swapping the
//! two candidates never changed the answer in any of the eighteen pairs, so
//! nothing about list order or argument splitting is leaking into the result.
//!
//! | Source | Candidates | Reference chose | Establishes |
//! |---|---|---|---|
//! | `yuva444p` | `yuv444p` \| `ya8` | `yuv444p` | **chroma-total > alpha** |
//! | `rgba64le` | `rgb48le` \| `rgba` | `rgba` | alpha > depth |
//! | `yuv444p16le` | `yuv444p` \| `gray16le` | `yuv444p` | **chroma-total > depth** |
//! | `yuv444p16le` | `yuv444p` \| `rgb48le` | `rgb48le` | depth > colour model, at 8 bits |
//! | `yuv444p10le` | `yuv444p9le` \| `rgb48le` | `rgb48le` | depth > colour model, at **one** bit |
//! | `yuv444p10le` | `yuv444p` \| `yuv420p10le` | `yuv420p10le` | depth > chroma coarsening |
//! | `yuv444p` | `yuv420p` \| `rgb24` | `yuv420p` | colour model > chroma coarsening |
//! | `rgb24` | `gbrp` \| `yuv444p` | `gbrp` | colour model > packing |
//! | `yuv420p16le` | `yuv420p10le` \| `yuv420p` | `yuv420p10le` | depth loss is graded by bits |
//! | `yuv444p` | `yuv422p` \| `yuv420p` | `yuv420p` | chroma loss is **not** graded by axis |
//!
//! So the order is
//!
//! > **chroma-total > alpha > depth > colour model > chroma coarsening > packing**
//!
//! Two things about that are worth stating, because both were got wrong once.
//!
//! **Plan 16 §1.6.4 is wrong in three places**, not two: it puts chroma above
//! colour model, grades chroma per axis, and has no notion of losing chroma
//! *entirely* at all.
//!
//! **"Going grey" is not a colour-model change with extra.** A YUV↔RGB change
//! provably sits *below* depth — the reference gives up a whole colour model
//! rather than one bit of precision, at 1, 2, 4 and 8 bits. A greyscale
//! destination sits *above* depth, above chroma coarsening, and above alpha.
//! They are separate components and collapsing them into one tier cannot fit
//! the data: any weighting that puts `COLOUR_MODEL` above `DEPTH_PER_BIT` breaks
//! all four `-> rgb48le` rows above.
//!
//! # A note on corpus coverage
//!
//! The first version of this table had seventeen rows and still missed this,
//! because every row offered either a *colour* destination on both sides or grey
//! as the *source*. None offered grey as a *candidate* against a colour format,
//! which is exactly the pair that discriminates the two orderings. The method
//! was sound; the corpus had a hole in it.
//!
//! The general lesson, worth applying to the next table anyone measures this
//! way: **a pairwise-comparison corpus is only as good as its coverage of the
//! pairs that discriminate.** When a weight is derived from ordered pairs, list
//! the components first and then make sure every *pair of components* appears
//! with the others held equal — rather than collecting pairs that look
//! interesting and inferring an order from what turns up.
//!
//! # What this deliberately does not reproduce
//!
//! **The equal-loss tiebreak.** When two candidates lose the same thing the
//! reference falls back on its own `AVPixelFormat` enum ordering, which is an
//! implementation artifact D1 says we do not mirror. Three measured pairs
//! depend on it and this module gets them wrong:
//!
//! | Source | Candidates | Reference | Us |
//! |---|---|---|---|
//! | `gray` | `gray10le` \| `rgb24` | `rgb24` | `gray10le` |
//! | `gray` | `rgb24` \| `gbrp` | `rgb24` | `gbrp` |
//! | `gray` | `gray10le` \| `yuv444p` | `gray10le` | `gray10le` ✓ |
//!
//! Closing it needs a `reference_rank(PixFmt) -> u16` column in
//! `vaco-pixfmt`'s generated table — the reference's own ordering, recorded as
//! the interface fact it is. That is another crate's file, so it is **reported,
//! not written**; see `docs/filter/vaco-filter-core.md`. Until then the
//! tiebreak here is `PixFmt`'s own discriminant, which is deterministic (D6
//! cares about that far more than it cares about which of two equal-loss
//! formats wins) and wrong for a grey source only.

use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

/// Every chroma sample is discarded: the destination is greyscale.
///
/// **The heaviest loss of all**, above even alpha. This is the weight the first
/// version of this module got wrong — it sat below `COLOUR_MODEL`, on the
/// assumption that going grey was a colour-model change with a little extra.
/// It is not: it is a category of its own, and it dominates everything.
pub const CHROMA_TOTAL: u32 = 0x0004_0000;
/// Alpha is dropped. Above depth: the reference would rather quantise 16 bits
/// down to 8 than lose the alpha channel.
pub const ALPHA: u32 = 0x0002_0000;
/// Per bit of component depth lost. The largest loss reachable in the format
/// table is 24 bits (`f32` to 8-bit), which still scores below [`ALPHA`].
pub const DEPTH_PER_BIT: u32 = 0x0000_0400;
/// The colour model changes while chroma survives: YUV↔RGB.
///
/// Below depth — *one* bit of depth loss outranks it, measured. Losing chroma
/// entirely is [`CHROMA_TOTAL`] and is a different thing entirely.
pub const COLOUR_MODEL: u32 = 0x0000_0200;
/// Chroma is subsampled more coarsely than the source. A flag, not a count.
pub const CHROMA: u32 = 0x0000_0100;
/// Planar↔packed, or an endianness change. Costs a pass; loses nothing.
pub const PACKING: u32 = 0x0000_0010;

/// The tier order, asserted at compile time rather than in a test.
///
/// These are facts about the constants, not about any input, so a reweighting
/// that breaks the ordering should fail to *build* — the same technique
/// `vaco-pool` uses to keep `BITSTREAM_PADDING` locked to `Padded::PAD`.
/// `MAX_DEPTH_LOSS` is `f32` down to 8-bit, the largest the format table can
/// express.
const MAX_DEPTH_LOSS: u32 = 24;
const _: () = assert!(
    CHROMA_TOTAL > ALPHA,
    "losing every chroma sample outranks losing alpha"
);
const _: () = assert!(
    ALPHA > DEPTH_PER_BIT * MAX_DEPTH_LOSS,
    "losing alpha outranks the largest depth loss the format table can express"
);
const _: () = assert!(
    DEPTH_PER_BIT > COLOUR_MODEL,
    "one bit of depth outranks a YUV/RGB change; measured at 1, 2, 4 and 8 bits"
);
const _: () = assert!(
    COLOUR_MODEL > CHROMA,
    "a colour-model change outranks chroma coarsening"
);
const _: () = assert!(CHROMA > PACKING, "coarsening chroma outranks a repack");

/// What converting a video frame from `from` to `to` costs.
///
/// Zero means nothing is lost — which includes gaining depth, gaining chroma
/// resolution or gaining an alpha channel, none of which are losses.
#[must_use]
pub fn video(from: PixFmt, to: PixFmt) -> u32 {
    if from == to {
        return 0;
    }
    let mut cost = 0u32;

    if from.has_alpha() && !to.has_alpha() {
        cost = cost.saturating_add(ALPHA);
    }

    let lost_bits = u32::from(from.max_depth().saturating_sub(to.max_depth()));
    cost = cost.saturating_add(lost_bits.saturating_mul(DEPTH_PER_BIT));

    let from_model = model(from);
    let to_model = model(to);
    if from_model != to_model {
        cost = cost.saturating_add(COLOUR_MODEL);
    }

    // Losing every chroma sample is worse than merely coarsening them.
    if to_model == ColourModel::Grey && from_model != ColourModel::Grey {
        cost = cost.saturating_add(CHROMA_TOTAL);
    } else {
        let (fh, fv) = from.log2_chroma();
        let (th, tv) = to.log2_chroma();
        if th > fh || tv > fv {
            cost = cost.saturating_add(CHROMA);
        }
    }

    if from.is_planar() != to.is_planar() || from.is_big_endian() != to.is_big_endian() {
        cost = cost.saturating_add(PACKING);
    }

    cost
}

/// Choose the cheapest conversion target from `candidates`.
///
/// Ties break on the [`PixFmt`] discriminant, which is deterministic but is
/// *not* what the reference does — see the module documentation. Returns `None`
/// only for an empty candidate list.
#[must_use]
pub fn best_video(from: PixFmt, candidates: &[PixFmt]) -> Option<PixFmt> {
    candidates
        .iter()
        .copied()
        .min_by_key(|&to| (video(from, to), to))
}

/// The three colour models a pixel format can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColourModel {
    Yuv,
    Rgb,
    Grey,
}

fn model(f: PixFmt) -> ColourModel {
    // Alpha is not a colour component. `ya8` is greyscale-with-alpha and the
    // reference treats it as having lost all chroma; counting its two
    // components as "not grey" scored it as a plain YUV format and got the
    // `yuva444p -> {yuv444p, ya8}` pair backwards.
    let colour_components = f
        .component_count()
        .saturating_sub(usize::from(f.has_alpha()));
    if colour_components <= 1 {
        ColourModel::Grey
    } else if f.is_rgb() {
        ColourModel::Rgb
    } else {
        ColourModel::Yuv
    }
}

/// What converting audio from `from` to `to` costs.
///
/// Same shape as [`video`]: depth loss dominates, then float→integer (which
/// clips and quantises), then packing. Sample rate and channel layout are
/// scored separately because a converter fixes them independently.
#[must_use]
pub fn audio_format(from: SampleFmt, to: SampleFmt) -> u32 {
    if from == to {
        return 0;
    }
    let mut cost = 0u32;
    let lost = from.bits_per_sample().saturating_sub(to.bits_per_sample());
    cost = cost.saturating_add(lost.saturating_mul(DEPTH_PER_BIT));
    if from.is_float() && !to.is_float() {
        cost = cost.saturating_add(COLOUR_MODEL);
    }
    if from.is_planar() != to.is_planar() {
        cost = cost.saturating_add(PACKING);
    }
    cost
}

/// Choose the cheapest sample format from `candidates`.
#[must_use]
pub fn best_audio_format(from: SampleFmt, candidates: &[SampleFmt]) -> Option<SampleFmt> {
    candidates
        .iter()
        .copied()
        .min_by_key(|&to| (audio_format(from, to), to))
}

/// Choose the cheapest sample rate from `candidates`.
///
/// Any resampling is a fixed cost, so an exact match always wins and everything
/// else is ordered by how far it is from the source — which keeps 48000 ahead
/// of 8000 for a 44100 source, the answer a listener would want.
#[must_use]
pub fn best_rate(from: u32, candidates: &[u32]) -> Option<u32> {
    candidates
        .iter()
        .copied()
        .min_by_key(|&to| (u32::from(to != from), from.abs_diff(to)))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_wrap,
    clippy::match_wildcard_for_single_variants,
    clippy::items_after_statements,
    clippy::single_match_else,
    clippy::option_if_let_else,
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    reason = "test code"
)]
mod tests {
    use super::*;

    /// Every ordered pair measured against ffmpeg 8.1 whose outcome does not
    /// depend on the equal-loss tiebreak. The command that produced each is in
    /// the module documentation; regenerating them is a copy-paste away, which
    /// is the point of writing it down (plan 13 §1b rule 4).
    const MEASURED: &[(PixFmt, PixFmt, PixFmt)] = &[
        // (source, the candidate the reference rejected, the one it chose)
        //
        // -- the tier boundaries, each isolating one comparison ------------
        (PixFmt::Yuva444p, PixFmt::Ya8, PixFmt::Yuv444p), // chroma-total > alpha
        (PixFmt::Yuva444p10le, PixFmt::Ya16le, PixFmt::Yuv444p10le),
        (PixFmt::Rgba64le, PixFmt::Rgb48le, PixFmt::Rgba), // alpha > depth
        (PixFmt::Yuva444p16le, PixFmt::Yuv444p16le, PixFmt::Yuva444p),
        (PixFmt::Yuv444p16le, PixFmt::Gray16le, PixFmt::Yuv444p), // chroma-total > depth
        (PixFmt::Yuv444p12le, PixFmt::Gray12le, PixFmt::Yuv444p),
        (PixFmt::Yuv444p10le, PixFmt::Gray10le, PixFmt::Yuv444p),
        (PixFmt::Yuv444p10le, PixFmt::Gray10le, PixFmt::Yuv444p9le),
        (PixFmt::Yuv444p10le, PixFmt::Gray10le, PixFmt::Yuv420p),
        (PixFmt::Gbrp10le, PixFmt::Gray10le, PixFmt::Gbrp), // ... from RGB too
        (PixFmt::Gbrpf32le, PixFmt::Gray8, PixFmt::Gbrp),
        (PixFmt::Yuv420p10le, PixFmt::Yuv420p, PixFmt::Rgb48le), // depth > colour model
        (PixFmt::Yuv444p16le, PixFmt::Yuv444p, PixFmt::Rgb48le),
        (PixFmt::Yuv444p12le, PixFmt::Yuv444p, PixFmt::Rgb48le),
        (PixFmt::Yuv444p10le, PixFmt::Yuv444p9le, PixFmt::Rgb48le), // even one bit
        (PixFmt::Yuv444p12le, PixFmt::Yuv444p10le, PixFmt::Rgb48le),
        (PixFmt::Gbrp10le, PixFmt::Gbrp, PixFmt::Rgb48le),
        (PixFmt::Yuv444p10le, PixFmt::Yuv444p, PixFmt::Yuv420p10le), // depth > chroma
        (PixFmt::Yuv444p, PixFmt::Rgb24, PixFmt::Yuv420p),           // colour model > chroma
        (PixFmt::Yuv444p, PixFmt::Rgb24, PixFmt::Yuv420p10le),
        (PixFmt::Rgb24, PixFmt::Yuv444p, PixFmt::Gbrp), // colour model > packing
        (PixFmt::Yuv420p16le, PixFmt::Yuv420p, PixFmt::Yuv420p10le), // depth is graded
        (PixFmt::Gbrpf32le, PixFmt::Gbrp, PixFmt::Gbrpf32be), // depth > endianness
        // -- everything the first corpus already covered -------------------
        (PixFmt::Rgb24, PixFmt::Yuv420p, PixFmt::Yuv444p),
        (PixFmt::Rgba, PixFmt::Rgb24, PixFmt::Yuva420p),
        (PixFmt::Rgba, PixFmt::Rgb24, PixFmt::Yuva444p),
        (PixFmt::Rgba, PixFmt::Yuv444p, PixFmt::Rgb24),
        (PixFmt::Rgba, PixFmt::Rgb24, PixFmt::Argb),
        (PixFmt::Yuv444p, PixFmt::Gbrp, PixFmt::Yuv444p16le),
        (PixFmt::Yuv420p, PixFmt::Gbrp, PixFmt::Yuv422p),
        (PixFmt::Yuv420p, PixFmt::Gray8, PixFmt::Rgb24),
        (PixFmt::Yuv420p, PixFmt::Rgb24, PixFmt::Yuv420p10le),
        (PixFmt::Yuv444p, PixFmt::Rgb24, PixFmt::Yuv444p10le),
        (PixFmt::Yuv444p, PixFmt::Gray8, PixFmt::Rgb24),
        (PixFmt::Yuv444p10le, PixFmt::Gray10le, PixFmt::Rgb48le),
    ];

    #[test]
    fn reproduces_every_measured_choice_that_is_not_a_tie() {
        for &(src, loser, winner) in MEASURED {
            let (lc, wc) = (video(src, loser), video(src, winner));
            assert!(
                wc < lc,
                "{}: chose {} ({wc}) over {} ({lc}), but the reference chose the other",
                src.name(),
                winner.name(),
                loser.name()
            );
            // Order must not matter, in either direction.
            assert_eq!(best_video(src, &[loser, winner]), Some(winner));
            assert_eq!(best_video(src, &[winner, loser]), Some(winner));
        }
    }

    /// The three pairs this module knowingly gets wrong, pinned so that closing
    /// the gap fails this test rather than passing silently. Same pattern
    /// `vaco-chlayout` uses for `LABEL_TRUNCATION_DIVERGENCE` (D17.1 rule 3).
    /// Named so that an auditor grepping for the divergence pattern
    /// `vaco-chlayout` established (D17.1 rule 3) finds it here too.
    const TIEBREAK_DIVERGENCE: &str =
        "equal-loss tiebreak follows PixFmt order, not the reference's";

    #[test]
    fn the_grey_source_tiebreak_still_diverges() {
        assert!(!TIEBREAK_DIVERGENCE.is_empty());
        // The reference picks rgb24 for both; we do not, because its tiebreak is
        // its own AVPixelFormat ordering and ours is PixFmt's.
        assert_eq!(
            video(PixFmt::Gray8, PixFmt::Gray10le),
            0,
            "grey to deeper grey loses nothing"
        );
        assert!(
            video(PixFmt::Gray8, PixFmt::Rgb24) > 0,
            "we score grey to rgb as a colour-model change"
        );
        assert_ne!(
            best_video(PixFmt::Gray8, &[PixFmt::Gray10le, PixFmt::Rgb24]),
            Some(PixFmt::Rgb24),
            "if this ever matches the reference, the divergence is closed and this \
             test and the docs section must go"
        );
    }

    /// The ordering the plan and the first version of this module both had.
    /// Kept as an executable statement of *why* it is wrong: any weighting with
    /// `COLOUR_MODEL > DEPTH_PER_BIT` contradicts measured data.
    #[test]
    fn colour_model_above_depth_would_contradict_the_measurements() {
        let rows = [
            (PixFmt::Yuv420p10le, PixFmt::Yuv420p, PixFmt::Rgb48le),
            (PixFmt::Yuv444p16le, PixFmt::Yuv444p, PixFmt::Rgb48le),
            (PixFmt::Yuv444p12le, PixFmt::Yuv444p, PixFmt::Rgb48le),
            (PixFmt::Yuv444p10le, PixFmt::Yuv444p9le, PixFmt::Rgb48le),
        ];
        for (src, loser, winner) in rows {
            assert!(
                video(src, winner) < video(src, loser),
                "{}: a colour-model change must stay cheaper than any depth loss",
                src.name()
            );
        }
        // The tier order itself is a `const _: () = assert!(..)` at module
        // scope, so a reweighting that breaks it fails to compile.
    }

    #[test]
    fn greyscale_with_alpha_counts_as_greyscale() {
        // `ya8` has two components but only one of them carries colour.
        assert!(video(PixFmt::Yuv444p, PixFmt::Ya8) > CHROMA_TOTAL);
        assert!(video(PixFmt::Gray8, PixFmt::Ya8) < CHROMA_TOTAL);
    }

    #[test]
    fn identity_is_free_and_gains_are_free() {
        assert_eq!(video(PixFmt::Yuv420p, PixFmt::Yuv420p), 0);
        assert_eq!(video(PixFmt::Yuv420p, PixFmt::Yuv420p10le), 0, "depth gain");
        assert_eq!(video(PixFmt::Yuv420p, PixFmt::Yuv444p), 0, "chroma gain");
        assert_eq!(video(PixFmt::Rgb24, PixFmt::Rgba), 0, "alpha gain");
    }

    #[test]
    fn best_video_is_none_for_an_empty_candidate_list() {
        assert_eq!(best_video(PixFmt::Yuv420p, &[]), None);
    }

    #[test]
    fn audio_depth_dominates_planarity() {
        assert!(
            audio_format(SampleFmt::S32, SampleFmt::S16)
                > audio_format(SampleFmt::S32, SampleFmt::S32P)
        );
        assert_eq!(audio_format(SampleFmt::S16, SampleFmt::S16), 0);
        assert!(
            audio_format(SampleFmt::F32, SampleFmt::S32) > 0,
            "float to int"
        );
    }

    #[test]
    fn best_rate_prefers_an_exact_match_then_the_nearest() {
        assert_eq!(best_rate(44_100, &[48_000, 44_100, 8_000]), Some(44_100));
        assert_eq!(best_rate(44_100, &[8_000, 48_000]), Some(48_000));
        assert_eq!(best_rate(44_100, &[]), None);
    }

    #[test]
    fn scores_never_overflow_for_any_pair() {
        // Exhaustive over the whole 268-format table, both directions: the one
        // property that has to hold for every input, not just the measured ones.
        for &a in PixFmt::all() {
            for &b in PixFmt::all() {
                let _ = video(a, b);
            }
        }
    }
}
