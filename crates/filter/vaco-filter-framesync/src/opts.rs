//! The user-facing options, and the per-input behaviour they compile down to.
//!
//! `eof_action`, `shortest`, `repeatlast` and `ts_sync_mode` are documented
//! surface that must behave identically on all 68 filters that synchronise
//! inputs, so the mapping from them to [`ExtendMode`] lives here, once, and is
//! pinned by a truth table measured against ffmpeg 8.1.

use vaco_core::TimeBase;

/// What to do when an input reaches end of stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EofAction {
    /// Hold the last frame of the ended input. The default.
    #[default]
    Repeat,
    /// The first end of stream ends the filter's output.
    EndAll,
    /// The main input carries on; a secondary that ends simply disappears.
    Pass,
}

impl EofAction {
    /// Parse the option value.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "repeat" => Some(Self::Repeat),
            "endall" => Some(Self::EndAll),
            "pass" => Some(Self::Pass),
            _ => None,
        }
    }

    /// The option value.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Repeat => "repeat",
            Self::EndAll => "endall",
            Self::Pass => "pass",
        }
    }
}

/// How a non-driving input is sampled at an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TsSyncMode {
    /// The most recent frame at or before the event. The default.
    #[default]
    Default,
    /// The frame nearest the event, which costs one frame of lookahead.
    Nearest,
}

impl TsSyncMode {
    /// Parse the option value.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "default" => Some(Self::Default),
            "nearest" => Some(Self::Nearest),
            _ => None,
        }
    }

    /// The option value.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Nearest => "nearest",
        }
    }
}

/// The four options, as a filter's option schema would carry them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSyncOpts {
    pub eof_action: EofAction,
    /// Force termination when any input ends. Overrides `eof_action`.
    pub shortest: bool,
    /// Hold the last secondary frame after its end of stream.
    pub repeatlast: bool,
    pub ts_sync: TsSyncMode,
}

impl Default for FrameSyncOpts {
    fn default() -> Self {
        Self {
            eof_action: EofAction::Repeat,
            shortest: false,
            repeatlast: true,
            ts_sync: TsSyncMode::Default,
        }
    }
}

/// Per-input behaviour outside its own frame range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendMode {
    /// No event may occur here; the event stream ends (after) or has not begun
    /// (before).
    Stop,
    /// No frame is available; the callback sees `None` for this input.
    Null,
    /// Hold the first (before) or last (after) frame.
    Infinity,
}

/// One input's role in the synchronisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsInput {
    /// Behaviour before this input's first frame.
    pub before: ExtendMode,
    /// Behaviour after this input's end of stream.
    pub after: ExtendMode,
    /// `0` is passive: sampled, never advancing the clock. The highest value
    /// present is the sync level, and only inputs at that level choose event
    /// times. `overlay` uses main = 2, overlay = 1, so only the main drives.
    pub sync: u32,
    /// The time base this input's timestamps are in.
    pub time_base: TimeBase,
}

impl Default for FsInput {
    fn default() -> Self {
        Self {
            before: ExtendMode::Stop,
            after: ExtendMode::Infinity,
            sync: 1,
            time_base: TimeBase::UNDEFINED,
        }
    }
}

impl FsInput {
    /// The two-input roles `overlay`, `blend`, `lut2` and the rest of the
    /// dual-input family declare: input 0 drives, input 1 is sampled and may be
    /// absent early.
    ///
    /// Measured: `blend` with a secondary starting at 0.5 s still emits from
    /// 0 s, while `hstack` — which gives every input the same sync level and
    /// `before = Stop` — emits nothing until both have started.
    #[must_use]
    pub fn dual(n: usize) -> Vec<Self> {
        (0..n)
            .map(|i| {
                if i == 0 {
                    Self {
                        before: ExtendMode::Stop,
                        after: ExtendMode::Infinity,
                        sync: 2,
                        time_base: TimeBase::UNDEFINED,
                    }
                } else {
                    Self {
                        before: ExtendMode::Null,
                        after: ExtendMode::Infinity,
                        sync: 1,
                        time_base: TimeBase::UNDEFINED,
                    }
                }
            })
            .collect()
    }

    /// The roles the `hstack`/`vstack`/`maskedmerge` family declares: every
    /// input drives, and every input must have started.
    #[must_use]
    pub fn uniform(n: usize) -> Vec<Self> {
        vec![Self::default(); n]
    }
}

/// Apply the option set to a list of per-input roles.
///
/// # The truth table, measured
///
/// | Setting | Effect |
/// |---|---|
/// | `eof_action=repeat` (default) | every input keeps `after = Infinity` |
/// | `eof_action=endall` | every input: `after = Stop` |
/// | `eof_action=pass` | input 0: `after = Stop`; every other: `after = Null` |
/// | `repeatlast=0` | **the same as `pass`** |
/// | `shortest=1` | every input: `after = Stop`, overriding the above |
///
/// The `repeatlast=0` row is the one plan 16 §3.3 gets wrong. It says
/// `repeatlast=0` changes only the non-driving inputs, and that the two options
/// are "nearly but not exactly" the same. Measured, they are identical, and
/// both stop the whole filter when input 0 ends:
///
/// ```sh
/// # main 0.5 s, secondary 1.0 s, both 10 fps
/// ffmpeg -filter_complex "…[m];…[s];[m][s]overlay=repeatlast=0,showinfo" -f null -
/// #   -> 5 frames, not 10: input 0's end of stream ended the output
/// ffmpeg -filter_complex "…[m];…[s];[m][s]overlay=eof_action=pass,showinfo" -f null -
/// #   -> 5 frames, identical
/// ```
pub fn apply_opts(inputs: &mut [FsInput], opts: FrameSyncOpts) {
    if !opts.repeatlast || opts.eof_action == EofAction::Pass {
        for (i, input) in inputs.iter_mut().enumerate() {
            input.after = if i == 0 {
                ExtendMode::Stop
            } else {
                ExtendMode::Null
            };
        }
    }
    if opts.shortest || opts.eof_action == EofAction::EndAll {
        for input in inputs.iter_mut() {
            input.after = ExtendMode::Stop;
        }
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

    fn afters(opts: FrameSyncOpts) -> Vec<ExtendMode> {
        let mut inputs = FsInput::dual(3);
        apply_opts(&mut inputs, opts);
        inputs.iter().map(|i| i.after).collect()
    }

    #[test]
    fn repeat_is_the_default_and_holds_every_last_frame() {
        assert_eq!(afters(FrameSyncOpts::default()), [ExtendMode::Infinity; 3]);
    }

    #[test]
    fn endall_stops_on_the_first_end_of_stream() {
        let opts = FrameSyncOpts {
            eof_action: EofAction::EndAll,
            ..FrameSyncOpts::default()
        };
        assert_eq!(afters(opts), [ExtendMode::Stop; 3]);
    }

    #[test]
    fn pass_and_repeatlast_zero_are_the_same_thing() {
        let pass = FrameSyncOpts {
            eof_action: EofAction::Pass,
            ..FrameSyncOpts::default()
        };
        let repeatlast = FrameSyncOpts {
            repeatlast: false,
            ..FrameSyncOpts::default()
        };
        assert_eq!(afters(pass), afters(repeatlast));
        assert_eq!(
            afters(pass),
            [ExtendMode::Stop, ExtendMode::Null, ExtendMode::Null]
        );
    }

    #[test]
    fn shortest_wins_over_everything() {
        for eof_action in [EofAction::Repeat, EofAction::EndAll, EofAction::Pass] {
            for repeatlast in [true, false] {
                let opts = FrameSyncOpts {
                    eof_action,
                    repeatlast,
                    shortest: true,
                    ts_sync: TsSyncMode::Default,
                };
                assert_eq!(afters(opts), [ExtendMode::Stop; 3], "{eof_action:?}");
            }
        }
    }

    #[test]
    fn option_names_round_trip() {
        for a in [EofAction::Repeat, EofAction::EndAll, EofAction::Pass] {
            assert_eq!(EofAction::from_name(a.name()), Some(a));
        }
        for m in [TsSyncMode::Default, TsSyncMode::Nearest] {
            assert_eq!(TsSyncMode::from_name(m.name()), Some(m));
        }
        assert_eq!(EofAction::from_name("nonsense"), None);
    }
}
