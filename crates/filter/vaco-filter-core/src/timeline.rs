//! Timeline support: the universal `enable=` expression.
//!
//! Every filter that declares timeline support gets an `enable` option, parsed
//! once into a `vaco-expr` program and evaluated once per frame. Parsing costs
//! about twenty-five evaluations, so parsing once at configure time and
//! evaluating per frame is worth several orders of magnitude over the obvious
//! alternative.
//!
//! # Variables
//!
//! | Name | Meaning |
//! |---|---|
//! | `t` | presentation time in seconds, `NAN` when the frame has no timestamp |
//! | `n` | frame index on this link, from zero |
//! | `w`, `h` | the input link's dimensions; zero for audio |
//! | `pos` | permanently `NAN` — the reference deprecated it, and scripts that reference it should not hard-fail |
//!
//! # A trap worth naming: `NAN` cuts both ways
//!
//! Truthiness in the expression language is `x != 0`, so **`NAN` is true**. But
//! the *comparison* functions return `0` for `NAN` rather than propagating it.
//! Those two facts point in opposite directions and both matter here.
//!
//! | `enable=` | `t` is `NAN` | Filter is |
//! |---|---|---|
//! | `between(t,10,20)` | comparison yields `0` | **off** |
//! | `gte(t,10)` | comparison yields `0` | **off** |
//! | `t` | the value *is* `NAN`, and `NAN != 0` | **on** |
//! | `if(t,1,0)` | `NAN` is truthy | **on** |
//!
//! Measured, not assumed. Against the pinned reference:
//!
//! ```sh
//! ffmpeg -f lavfi -i "aevalsrc=exprs='between(nan,10,20)':s=1:n=1:d=1" -f f64le -
//! #  -> 0000000000000000
//! ```
//!
//! and against `vaco-expr` directly, which agrees. The practical consequence: a
//! time-gated filter is **disabled** on frames with no timestamp, which is
//! usually what you want and is emphatically not what "NAN is truthy" would lead
//! you to guess.

use vaco_core::{Error, Result};
use vaco_expr::{Bindings, Context, Expr, Registers};
use vaco_frame::{Frame, FrameData};

use crate::{FilterContext, LinkFormat};

/// The three timeline modes.
///
/// A three-state enum rather than two flags, because a filter cannot
/// meaningfully be both: either the framework forwards the frame when `enable`
/// is false, or the filter consults the flag itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineSupport {
    /// No `enable=` option.
    #[default]
    None,
    /// The framework evaluates and, when false, forwards the input untouched.
    Generic,
    /// The framework evaluates and the filter consults the result. Needed when
    /// the filter must keep temporal state advancing while disabled.
    Internal,
}

/// The variable names bound for an `enable=` expression, in slice order.
pub const TIMELINE_VARS: &[&str] = &["t", "n", "w", "h", "pos"];

/// A compiled `enable=` expression, plus the state one evaluation needs.
#[derive(Debug)]
pub struct Timeline {
    program: Option<Expr>,
    registers: Registers,
    /// Cached from the input link at configure time.
    width: f64,
    height: f64,
    time_base: vaco_core::Rational,
    /// The result for the frame currently being processed.
    enabled: bool,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::always()
    }
}

impl Timeline {
    /// A timeline that is always on — no expression, no per-frame cost.
    #[must_use]
    pub fn always() -> Self {
        Self {
            program: None,
            registers: Registers::new(),
            width: 0.0,
            height: 0.0,
            time_base: vaco_core::Rational::UNDEFINED,
            enabled: true,
        }
    }

    /// Compile `expression`.
    ///
    /// # Errors
    ///
    /// [`Error::Option`] naming `enable`, with the parser's own message, when
    /// the expression does not parse.
    pub fn parse(expression: &str) -> Result<Self> {
        let mut t = Self::always();
        t.set_expression(expression)?;
        Ok(t)
    }

    /// Replace the expression. This is what the `enable` runtime command does.
    ///
    /// # Errors
    ///
    /// [`Error::Option`] when the expression does not parse. The previous
    /// expression is kept, so a rejected command leaves the filter unmodified.
    pub fn set_expression(&mut self, expression: &str) -> Result<()> {
        if expression.is_empty() {
            self.program = None;
            self.enabled = true;
            return Ok(());
        }
        let parsed =
            Expr::parse(expression, &Bindings::new(TIMELINE_VARS)).map_err(|e| Error::Option {
                name: "enable".to_owned(),
                detail: e.to_string(),
            })?;
        self.program = Some(parsed);
        Ok(())
    }

    /// Whether an expression is installed at all.
    #[must_use]
    pub const fn is_gated(&self) -> bool {
        self.program.is_some()
    }

    /// The result for the most recently evaluated frame.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Cache the input link's geometry and time base.
    ///
    /// Called from `Filter::configure`; without it `w`, `h` and `t` are zero and
    /// `NAN` respectively, which is a silently wrong expression rather than a
    /// loud one.
    pub fn configure(&mut self, ctx: &FilterContext<'_>) {
        let Some(format) = ctx.input_link(0) else {
            return;
        };
        self.time_base = format.time_base();
        if let LinkFormat::Video { width, height, .. } = format {
            self.width = f64::from(*width);
            self.height = f64::from(*height);
        }
    }

    /// Evaluate for one frame. `index` is the frame's position on the link.
    ///
    /// Returns `true` when there is no expression, so an ungated filter pays a
    /// branch and nothing else.
    pub fn evaluate(&mut self, frame: &Frame, index: u64) -> bool {
        let Some(program) = self.program.as_ref() else {
            self.enabled = true;
            return true;
        };
        let base = if self.time_base.is_defined() {
            self.time_base
        } else {
            frame.time_base
        };
        let t = frame.pts.to_seconds(base).unwrap_or(f64::NAN);
        let (w, h) = match &frame.data {
            FrameData::Video { width, height, .. } => (f64::from(*width), f64::from(*height)),
            FrameData::Audio { .. } => (self.width, self.height),
        };
        let vars = [t, index as f64, w, h, f64::NAN];
        let value = program.eval_with(&mut Context::new(&vars, &mut self.registers));
        // Truthiness is `x != 0`, which makes NAN true. Comparisons, however,
        // return 0 for NAN rather than propagating it — see the module docs, and
        // do not "simplify" this to `value > 0.0`.
        self.enabled = value != 0.0;
        self.enabled
    }
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
    use crate::test_support::video_frame;
    use vaco_core::{Rational, Timestamp};

    fn at(pts: i64) -> Frame {
        let mut f = video_frame(16, 16, pts);
        f.time_base = Rational::new(1, 10);
        f
    }

    #[test]
    fn no_expression_is_always_enabled() {
        let mut t = Timeline::always();
        assert!(!t.is_gated());
        assert!(t.evaluate(&at(0), 0));
    }

    #[test]
    fn between_gates_on_time() {
        let mut t = Timeline::parse("between(t,1,2)").expect("parses");
        assert!(!t.evaluate(&at(0), 0), "t=0.0");
        assert!(t.evaluate(&at(10), 1), "t=1.0");
        assert!(t.evaluate(&at(20), 2), "t=2.0");
        assert!(!t.evaluate(&at(30), 3), "t=3.0");
    }

    #[test]
    fn frame_index_is_available_as_n() {
        let mut t = Timeline::parse("gte(n,2)").expect("parses");
        assert!(!t.evaluate(&at(0), 0));
        assert!(!t.evaluate(&at(0), 1));
        assert!(t.evaluate(&at(0), 2));
    }

    #[test]
    fn dimensions_come_from_the_frame() {
        let mut t = Timeline::parse("eq(w,32)").expect("parses");
        assert!(!t.evaluate(&video_frame(16, 16, 0), 0));
        assert!(t.evaluate(&video_frame(32, 16, 0), 0));
    }

    #[test]
    fn an_absent_timestamp_disables_a_time_gate() {
        // `between(NAN,...)` is 0 — comparisons do not propagate NAN. Confirmed
        // against the reference: `aevalsrc=exprs='between(nan,10,20)'` writes
        // eight zero bytes.
        let mut t = Timeline::parse("between(t,10,20)").expect("parses");
        let mut f = at(0);
        f.pts = Timestamp::NONE;
        assert!(!t.evaluate(&f, 0));
    }

    #[test]
    fn an_expression_yielding_nan_enables_the_filter() {
        // The other half of the rule: truthiness is `x != 0`, and NAN != 0.
        let mut t = Timeline::parse("t").expect("parses");
        let mut f = at(0);
        f.pts = Timestamp::NONE;
        assert!(t.evaluate(&f, 0));
    }

    #[test]
    fn a_bad_expression_is_an_option_error_naming_enable() {
        let e = Timeline::parse("between(");
        match e {
            Err(Error::Option { name, .. }) => assert_eq!(name, "enable"),
            other => panic!("expected an option error, got {other:?}"),
        }
    }

    #[test]
    fn a_rejected_command_leaves_the_expression_alone() {
        let mut t = Timeline::parse("gte(n,2)").expect("parses");
        assert!(t.set_expression("gte(").is_err());
        assert!(!t.evaluate(&at(0), 0));
        assert!(
            t.evaluate(&at(0), 5),
            "the original expression still applies"
        );
    }

    #[test]
    fn registers_survive_between_evaluations() {
        // `st`/`ld` state is the caller's, which is what makes a counting
        // expression work across frames at all.
        let mut t = Timeline::parse("st(0,ld(0)+1);gte(ld(0),3)").expect("parses");
        assert!(!t.evaluate(&at(0), 0));
        assert!(!t.evaluate(&at(0), 1));
        assert!(t.evaluate(&at(0), 2));
    }

    #[test]
    fn an_empty_expression_clears_the_gate() {
        let mut t = Timeline::parse("gte(n,99)").expect("parses");
        assert!(t.is_gated());
        t.set_expression("").expect("clears");
        assert!(!t.is_gated());
        assert!(t.evaluate(&at(0), 0));
    }
}
