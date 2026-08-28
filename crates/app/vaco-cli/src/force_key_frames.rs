//! CL-22 (#223): `-force_key_frames`'s four syntaxes, and the per-frame
//! decision each one makes — plan 14 §6.5.
//!
//! # What this module is, and what it is not wired to
//!
//! [`parse`] and [`Evaluator`] are a complete, independently testable
//! implementation of the option's grammar and its per-frame state machine.
//! Plan 14 §6.5 describes the evaluator as "a small state machine owned by
//! the encoder node; it sets `Frame.flags |= FORCE_KEYFRAME` and never talks
//! to the encoder directly" — but that presupposes two things this build does
//! not have:
//!
//! 1. **A `FORCE_KEYFRAME` bit.** [`vaco_frame::FrameFlags`] has `KEY`, which
//!    a *decoder* sets on output to report "this frame was intra", not a
//!    request bit a caller sets on encoder *input*. Repurposing it would
//!    conflate the two meanings on the very type `-force_key_frames source`
//!    reads to make its own decision.
//! 2. **A seam to set anything per frame at all.** `vaco_sched::PipelineSpec`
//!    exposes exactly four ways to add a node — `add_decoder`, `add_encoder`,
//!    `add_converter`, `add_filter` — and none of them is a generic
//!    frame-mutating callback. Threading a decision made per frame into a
//!    live pipeline needs a new node kind in `vaco-sched`, which is a
//!    cross-crate change outside `crates/app/vaco-cli`.
//!
//! **And even with that seam, this build has nothing for it to change**:
//! every registered video encoder (`crates/codec/vaco-codec-{png,qoi,gif,
//! jpeg,tiff,exr,webp,rawvideo,…}`) is intra-only — there is no GOP structure,
//! no inter-frame prediction, and therefore no decision a request to force a
//! keyframe could actually alter. Forcing is presently unobservable on any
//! encode this build can perform, which is a fact about the encoder roster,
//! not about this module.
//!
//! So `crate::exec::resolve_output` parses `-force_key_frames` per output
//! stream (a malformed value is a real, early error — the same treatment
//! `-s`/`-pix_fmt` already get) and keeps the result on [`OutStream`]; nothing
//! downstream reads it yet. Reported rather than worked around, per this
//! crate's constraints on stopping at a genuine cross-crate boundary.
//!
//! [`OutStream`]: crate::exec::OutStream

use vaco_expr::{Bindings, Expr};

/// One item in the `Times` form's comma-separated list.
#[derive(Debug, Clone, PartialEq)]
pub enum TimeSpecItem {
    /// An absolute time, in microseconds.
    At(i64),
    /// `chapters[delta]`: every chapter start plus `delta` seconds.
    Chapters { delta: f64 },
}

/// What `-force_key_frames` resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum ForceKeyFrames {
    Times(Vec<TimeSpecItem>),
    Expr(Expr),
    SourceKeyframes,
    SceneChangeMetadata,
}

/// The four variable names `expr:` binds, in the order [`Evaluator`] passes
/// their values — `n`, `n_forced`, `prev_forced_n`, `prev_forced_t`, `t`.
pub const EXPR_VARS: &[&str] = &["n", "n_forced", "prev_forced_n", "prev_forced_t", "t"];

/// Parse one `-force_key_frames` value.
///
/// # Errors
/// A message naming what was wrong: an unparsable `expr:` expression, an
/// invalid `chapters[delta]` suffix, or a time token that is neither a
/// duration nor `chapters[...]`.
pub fn parse(s: &str) -> Result<ForceKeyFrames, String> {
    if let Some(e) = s.strip_prefix("expr:") {
        let bindings = Bindings::new(EXPR_VARS);
        return Expr::parse(e, &bindings)
            .map(ForceKeyFrames::Expr)
            .map_err(|err| format!("invalid -force_key_frames expression '{e}': {err}"));
    }
    match s {
        "source" => Ok(ForceKeyFrames::SourceKeyframes),
        "scd_metadata" => Ok(ForceKeyFrames::SceneChangeMetadata),
        _ => parse_time_list(s).map(ForceKeyFrames::Times),
    }
}

fn parse_time_list(s: &str) -> Result<Vec<TimeSpecItem>, String> {
    s.split(',').map(parse_time_item).collect()
}

fn parse_time_item(tok: &str) -> Result<TimeSpecItem, String> {
    if let Some(rest) = tok.strip_prefix("chapters") {
        let delta = if rest.is_empty() {
            0.0
        } else {
            rest.parse::<f64>()
                .map_err(|_| format!("invalid chapters delta in '{tok}'"))?
        };
        return Ok(TimeSpecItem::Chapters { delta });
    }
    let duration = vaco_core::parse::duration(tok)
        .ok_or_else(|| format!("invalid time '{tok}' for -force_key_frames"))?;
    let micros = i64::try_from(duration.as_micros()).unwrap_or(i64::MAX);
    Ok(TimeSpecItem::At(micros))
}

/// The signals [`Evaluator::wants_force`] needs about one frame, in
/// presentation order.
#[derive(Debug, Clone, Copy)]
pub struct FrameSignal {
    /// Presentation time, in microseconds.
    pub pts_us: i64,
    /// Whether the *source* frame (before any encoder-side decision) carried
    /// a keyframe flag — what `source` mode reads.
    pub source_is_key: bool,
    /// Whether the frame carries the `lavfi.scd.time` metadata key — what
    /// `scd_metadata` mode reads.
    pub has_scd_metadata: bool,
}

/// The per-frame state machine plan 14 §6.5 describes: processed frame count,
/// forced count, and the previous force's frame index/time (`NaN` before the
/// first force, matching the reference's own `expr:` surprise —
/// `gte(t, prev_forced_t+5)` is false on frame 0, and this reproduces that by
/// construction rather than special-casing it).
#[derive(Debug)]
pub struct Evaluator {
    spec: ForceKeyFrames,
    n: u64,
    n_forced: u64,
    prev_forced_n: Option<u64>,
    prev_forced_t: Option<f64>,
    /// `Times`/`Chapters`, expanded and sorted once at construction; a cursor
    /// rather than repeated removal, since frames arrive in non-decreasing
    /// presentation order.
    pending_times: Vec<i64>,
    cursor: usize,
}

impl Evaluator {
    /// `chapters` are chapter start times in seconds, already resolved from
    /// whichever input `-map_chapters` names — needed only for the `Times`
    /// form's `chapters[delta]` token.
    #[must_use]
    pub fn new(spec: ForceKeyFrames, chapters: &[f64]) -> Self {
        let mut pending_times = Vec::new();
        if let ForceKeyFrames::Times(items) = &spec {
            for item in items {
                match item {
                    TimeSpecItem::At(t) => pending_times.push(*t),
                    TimeSpecItem::Chapters { delta } => {
                        for c in chapters {
                            let us = (c + delta) * 1_000_000.0;
                            pending_times.push(us.round() as i64);
                        }
                    }
                }
            }
            pending_times.sort_unstable();
        }
        Self {
            spec,
            n: 0,
            n_forced: 0,
            prev_forced_n: None,
            prev_forced_t: None,
            pending_times,
            cursor: 0,
        }
    }

    /// Decide whether `frame` should be forced, and advance the state
    /// machine. Call exactly once per frame, in presentation order.
    #[must_use]
    pub fn wants_force(&mut self, frame: FrameSignal) -> bool {
        let t = frame.pts_us as f64 / 1_000_000.0;
        let force = match &self.spec {
            ForceKeyFrames::Times(_) => {
                let mut hit = false;
                while self
                    .pending_times
                    .get(self.cursor)
                    .is_some_and(|&next| next <= frame.pts_us)
                {
                    self.cursor += 1;
                    hit = true;
                }
                hit
            }
            ForceKeyFrames::Expr(expr) => {
                let vars = [
                    self.n as f64,
                    self.n_forced as f64,
                    self.prev_forced_n.map_or(f64::NAN, |x| x as f64),
                    self.prev_forced_t.unwrap_or(f64::NAN),
                    t,
                ];
                expr.eval(&vars) != 0.0
            }
            ForceKeyFrames::SourceKeyframes => frame.source_is_key,
            ForceKeyFrames::SceneChangeMetadata => frame.has_scd_metadata,
        };
        if force {
            self.n_forced += 1;
            self.prev_forced_n = Some(self.n);
            self.prev_forced_t = Some(t);
        }
        self.n += 1;
        force
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn signal(pts_us: i64) -> FrameSignal {
        FrameSignal {
            pts_us,
            source_is_key: false,
            has_scd_metadata: false,
        }
    }

    #[test]
    fn source_and_scd_metadata_parse_as_bare_keywords() {
        assert_eq!(parse("source").unwrap(), ForceKeyFrames::SourceKeyframes);
        assert_eq!(
            parse("scd_metadata").unwrap(),
            ForceKeyFrames::SceneChangeMetadata
        );
    }

    #[test]
    fn a_time_list_parses_literal_and_chapters_tokens_mixed() {
        let ForceKeyFrames::Times(items) = parse("0:05:00,chapters-0.1").unwrap() else {
            panic!("expected Times");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], TimeSpecItem::At(300_000_000));
        assert_eq!(items[1], TimeSpecItem::Chapters { delta: -0.1 });
    }

    #[test]
    fn a_bare_chapters_token_defaults_to_zero_delta() {
        let ForceKeyFrames::Times(items) = parse("chapters").unwrap() else {
            panic!("expected Times");
        };
        assert_eq!(items, vec![TimeSpecItem::Chapters { delta: 0.0 }]);
    }

    #[test]
    fn an_unparsable_time_is_a_named_error() {
        assert!(parse("not-a-time").is_err());
    }

    #[test]
    fn expr_prefix_parses_the_canonical_case() {
        let spec = parse("expr:gte(t,n_forced*5)").unwrap();
        assert!(matches!(spec, ForceKeyFrames::Expr(_)));
    }

    #[test]
    fn an_invalid_expr_is_a_named_error() {
        let e = parse("expr:(((").unwrap_err();
        assert!(e.contains("expr"), "{e}");
    }

    #[test]
    fn times_force_exactly_at_and_after_each_pending_time() {
        let spec = parse("1.0,2.0").unwrap();
        let mut ev = Evaluator::new(spec, &[]);
        assert!(!ev.wants_force(signal(500_000)));
        assert!(ev.wants_force(signal(1_000_000)));
        assert!(!ev.wants_force(signal(1_500_000)));
        assert!(ev.wants_force(signal(2_000_000)));
        assert!(!ev.wants_force(signal(3_000_000)));
    }

    #[test]
    fn chapters_expand_against_the_supplied_chapter_list() {
        let spec = parse("chapters-0.1").unwrap();
        let mut ev = Evaluator::new(spec, &[10.0, 20.0]);
        // 10.0 - 0.1 = 9.9s = 9_900_000us.
        assert!(!ev.wants_force(signal(9_800_000)));
        assert!(ev.wants_force(signal(9_900_000)));
        assert!(ev.wants_force(signal(19_900_000)));
    }

    #[test]
    fn source_mode_reads_the_frame_signal_directly() {
        let mut ev = Evaluator::new(ForceKeyFrames::SourceKeyframes, &[]);
        assert!(!ev.wants_force(FrameSignal {
            pts_us: 0,
            source_is_key: false,
            has_scd_metadata: false
        }));
        assert!(ev.wants_force(FrameSignal {
            pts_us: 1,
            source_is_key: true,
            has_scd_metadata: false
        }));
    }

    #[test]
    fn scd_metadata_mode_reads_its_own_signal() {
        let mut ev = Evaluator::new(ForceKeyFrames::SceneChangeMetadata, &[]);
        assert!(ev.wants_force(FrameSignal {
            pts_us: 0,
            source_is_key: false,
            has_scd_metadata: true
        }));
    }

    #[test]
    fn expr_prev_forced_t_is_nan_before_the_first_force() {
        // The canonical case from plan 14 §6.5: `gte(t, prev_forced_t+5)` is
        // false on frame 0 because `prev_forced_t` starts NaN, not 0 — a
        // property of the state machine's initial values, not of `vaco_expr`
        // itself, so this is asserted through a real `Evaluator` rather than
        // by calling `Expr::eval` directly.
        let spec = parse("expr:gte(t,prev_forced_t+5)").unwrap();
        let mut ev = Evaluator::new(spec, &[]);
        assert!(!ev.wants_force(signal(0)));
    }

    #[test]
    fn expr_n_forced_times_five_forces_every_fifth_second() {
        let spec = parse("expr:gte(t,n_forced*5)").unwrap();
        let mut ev = Evaluator::new(spec, &[]);
        assert!(ev.wants_force(signal(0)));
        assert!(!ev.wants_force(signal(1_000_000)));
        assert!(!ev.wants_force(signal(4_000_000)));
        assert!(ev.wants_force(signal(5_000_000)));
    }
}
