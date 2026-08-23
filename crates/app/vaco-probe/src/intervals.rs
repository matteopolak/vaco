//! `-read_intervals`: the user's bound on a packet dump.
//!
//! # What it is
//!
//! A packet dump is unbounded by construction — `-show_packets` on a two-hour
//! file emits one section per packet. `-read_intervals` is the option that
//! bounds it, and D6's "a hostile file must terminate" rests on it as much as
//! any user's convenience does. The grammar, from plan 14 §5.3:
//!
//! ```ebnf
//! INTERVALS ::= INTERVAL ( ',' INTERVAL )*
//! INTERVAL  ::= [ START | '+' START_OFFSET ] [ '%' [ END | '+' END_OFFSET | '#' COUNT ] ]
//! ```
//!
//! # How it works
//!
//! [`parse`] turns the text into [`ReadInterval`]s; [`Cursor`] applies one at a
//! time to a packet stream. The two are deliberately separate — the parser is
//! pure and property-tested, the cursor is a small state machine over
//! timestamps, and neither needs the other to be tested.
//!
//! # Provenance
//!
//! Measured against `ffprobe` 8.1 (Homebrew, arm64 macOS) under `LC_ALL=C`, on
//! a 2 s H.264+AAC MP4 built by
//!
//! ```sh
//! ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25:duration=2 \
//!        -f lavfi -i sine=frequency=440:duration=2 \
//!        -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest av.mp4
//! ```
//!
//! and read back with
//!
//! ```sh
//! ffprobe -v error -of csv=p=0:nk=1 -show_entries packet=pos \
//!         -show_packets -select_streams v -read_intervals '<spec>' av.mp4
//! ```
//!
//! What that run established, none of which is derivable from the grammar:
//!
//! | Spec | Observed | Rule |
//! |---|---|---|
//! | `%+#5` | 5 packets | `#N` counts *selected* packets only |
//! | `%+#1,%+#1` | packets 1 and **3** | each interval eats one extra packet on the way out |
//! | `%#5` | rejected | `#` is legal only directly after `%+` |
//! | `#5` | rejected | `#` is never a *start* |
//! | `%+#-1` | 0 packets, **exit 0** | a bad count is a warning and an empty interval, not an error |
//! | `1%+0.04` on a file whose only keyframe is at 0 | end is 0.04, not 1.04 | the offset end is measured from the position actually **found** |
//! | `-read_intervals a -read_intervals b` | `b` | last wins |
//! | `,%+#2` / `%+#2,` | rejected | an empty interval is an error, so no trailing comma |
//!
//! The one that costs a naive implementation is the second: after an interval
//! ends, the packet that ended it has already been consumed and is **not**
//! shown by the next interval.
//!
//! # How to change it
//!
//! Every rule above is a reference run, and a change to any of them needs a new
//! one in the commit. The parse results are pinned by this module's own tests;
//! the `%+#1,%+#1` rule is pinned twice — once in [`Cursor`]'s tests and once
//! end-to-end in `packets`'s — because it is the rule most likely to be
//! "simplified" away.

use core::fmt;

/// A position in an interval, in microseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bound {
    /// An absolute timestamp: seek here.
    Absolute(i64),
    /// An offset from the current position: no seek.
    Relative(i64),
}

/// Where an interval stops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EndBound {
    /// At a timestamp, absolute or relative to the position actually found.
    Time(Bound),
    /// After this many packets. `#N`.
    Packets(u64),
}

/// One `START%END` interval.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReadInterval {
    pub start: Option<Bound>,
    pub end: Option<EndBound>,
}

impl ReadInterval {
    /// The whole file: no seek, no end. What an absent `-read_intervals` means.
    pub const ALL: Self = Self {
        start: None,
        end: None,
    };
}

/// A `-read_intervals` value the reference rejects.
///
/// The wording follows the reference's, but note that plan 14 §5.6 makes only
/// the **exit code** conformance surface here, not the message: stderr is
/// compared for "did it fail at all" and nothing more. The wording is a
/// convenience for anyone diffing two runs by eye, not a contract.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IntervalError {
    Empty,
    Start(String),
    End(String),
}

impl fmt::Display for IntervalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Invalid empty interval specification"),
            Self::Start(s) => write!(f, "Invalid interval start specification '{s}'"),
            Self::End(s) => write!(f, "Invalid interval end/duration specification '{s}'"),
        }
    }
}

/// The warning a malformed `#COUNT` produces.
///
/// Observed: `-read_intervals '%+#-1'` prints this and **exits 0**, having read
/// one packet and shown none. So it is not a parse failure; it is an interval
/// that ends immediately. Reproduced rather than tidied into an error, because
/// the exit code is conformance surface (plan 13 §1b).
pub const BAD_COUNT: &str = "Invalid or negative value";

/// Parse a `-read_intervals` value.
///
/// # Errors
/// [`IntervalError`] for a spec the reference rejects outright. A malformed
/// `#COUNT` is **not** one of those: it yields `Packets(0)` plus a warning in
/// the returned list, matching the reference's exit-0 behaviour.
pub fn parse(spec: &str) -> Result<(Vec<ReadInterval>, Vec<String>), IntervalError> {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for part in spec.split(',') {
        out.push(one(part, &mut warnings)?);
    }
    Ok((out, warnings))
}

fn one(spec: &str, warnings: &mut Vec<String>) -> Result<ReadInterval, IntervalError> {
    if spec.is_empty() {
        return Err(IntervalError::Empty);
    }
    let (start_text, end_text) = match spec.split_once('%') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };

    let start = if start_text.is_empty() {
        None
    } else if let Some(rest) = start_text.strip_prefix('+') {
        Some(Bound::Relative(
            duration(rest).ok_or_else(|| IntervalError::Start(rest.to_owned()))?,
        ))
    } else {
        Some(Bound::Absolute(duration(start_text).ok_or_else(|| {
            IntervalError::Start(start_text.to_owned())
        })?))
    };

    let end = match end_text {
        None | Some("") => None,
        Some(text) => Some(end_bound(text, warnings)?),
    };

    Ok(ReadInterval { start, end })
}

fn end_bound(text: &str, warnings: &mut Vec<String>) -> Result<EndBound, IntervalError> {
    if let Some(rest) = text.strip_prefix('+') {
        if let Some(count) = rest.strip_prefix('#') {
            // A bad count is a warning and an empty interval, not an error.
            return Ok(EndBound::Packets(count.parse::<u64>().unwrap_or_else(
                |_| {
                    warnings.push(format!(
                        "{BAD_COUNT} '{count}' for duration number of frames"
                    ));
                    0
                },
            )));
        }
        return Ok(EndBound::Time(Bound::Relative(
            duration(rest).ok_or_else(|| IntervalError::End(rest.to_owned()))?,
        )));
    }
    Ok(EndBound::Time(Bound::Absolute(
        duration(text).ok_or_else(|| IntervalError::End(text.to_owned()))?,
    )))
}

/// A duration in microseconds, in the reference's duration grammar.
///
/// ```text
/// [ws]* [ '-' | '+' ] D+ [ ':' D+ [ ':' D+ ] ] [ '.' D* ] [ 's' | 'ms' | 'us' ]
/// ```
///
/// Measured, and each row is a rule that is not obvious from the shape:
///
/// | Input | Verdict | |
/// |---|---|---|
/// | `0.1` `100ms` `0.1s` `00:00:00.1` `0:00.1` `0:00.1s` | accepted | |
/// | `.1` | rejected | a digit must come first |
/// | ` 0.1` | accepted | leading whitespace is skipped |
/// | `0.1 ` | rejected | trailing anything is not |
/// | `1e-1` `1.0e1` `0x10` | rejected | no exponent, no hex |
/// | `1m` `1h` | rejected | only `s`, `ms`, `us` |
/// | `1:2:3:4` | rejected | at most three colon-separated parts |
/// | `99999999999999999999` | rejected | overflow is a rejection, not a clamp |
/// | `000…0009` | accepted | leading zeros are not a base prefix |
#[must_use]
pub fn duration(text: &str) -> Option<i64> {
    let s = text.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, s.get(1..)?),
        Some(b'+') => (false, s.get(1..)?),
        _ => (false, s),
    };
    if !s.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }

    // The unit suffix comes off first: it applies to the whole value, including
    // the colon form (`0:00.1s` is accepted).
    let (body, scale) = if let Some(b) = s.strip_suffix("ms") {
        (b, 1_000i64)
    } else if let Some(b) = s.strip_suffix("us") {
        (b, 1)
    } else if let Some(b) = s.strip_suffix('s') {
        (b, 1_000_000)
    } else {
        (s, 1_000_000)
    };

    let (whole, frac) = match body.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (body, None),
    };

    let mut parts = whole.split(':');
    let mut units: [i64; 3] = [0; 3];
    let mut count = 0usize;
    for part in &mut parts {
        if count == 3 {
            return None;
        }
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *units.get_mut(count)? = part.parse::<i64>().ok()?;
        count += 1;
    }
    if count == 0 {
        return None;
    }

    // 1 part = seconds, 2 = MM:SS, 3 = HH:MM:SS. The scale suffix applies to
    // the last component only in the one-part case; with colons the value is
    // already a clock reading, and the suffix measures as a no-op there.
    let seconds = match count {
        1 => return finish(units.first().copied()?, frac, scale, negative),
        2 => units.first().copied()? * 60 + units.get(1).copied()?,
        _ => {
            units.first().copied()?.checked_mul(3600)?
                + units.get(1).copied()? * 60
                + units.get(2).copied()?
        }
    };
    finish(seconds, frac, 1_000_000, negative)
}

fn finish(whole: i64, frac: Option<&str>, scale: i64, negative: bool) -> Option<i64> {
    // How many fractional digits survive at this scale. Seconds carry six
    // (microsecond resolution), milliseconds three, microseconds none — the
    // rest is truncated, exactly as a microsecond clock must.
    let places: usize = match scale {
        1_000_000 => 6,
        1_000 => 3,
        _ => 0,
    };
    let mut total = whole.checked_mul(scale)?;
    if let Some(frac) = frac {
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        for (i, b) in frac.bytes().enumerate() {
            let Some(exp) = places.checked_sub(i + 1) else {
                break;
            };
            let unit = 10i64.checked_pow(u32::try_from(exp).ok()?)?;
            total = total.checked_add(i64::from(b - b'0').checked_mul(unit)?)?;
        }
    }
    Some(if negative { -total } else { total })
}

/// What a [`Cursor`] says about one packet.
///
/// Named `Admission` rather than the obvious `Verdict` because
/// `vaco-conformance` already owns that name for a different concept, and
/// D19's `dup-check` is right to refuse two of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
    /// Show it, and count it.
    Show,
    /// The interval is over. This packet is **consumed and not shown** — the
    /// measured `%+#1,%+#1` behaviour.
    Stop,
}

/// One interval's progress through a packet stream.
///
/// Only *selected* packets reach [`Cursor::admit`]; a packet filtered out by
/// `-select_streams` never counts toward `#N` and never establishes the origin
/// of a relative end. Measured: `-select_streams v -read_intervals '%+#1,%+#1'`
/// skips the second **video** packet, not the second packet.
#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    end: Option<EndBound>,
    seen: u64,
    /// The absolute end in microseconds, once a relative one has an origin.
    deadline: Option<i64>,
}

impl Cursor {
    /// Begin `interval`. `found` is the position the seek actually reached, in
    /// microseconds, which is what a relative end is measured from — not the
    /// position that was asked for.
    #[must_use]
    pub const fn new(interval: ReadInterval) -> Self {
        let deadline = match interval.end {
            Some(EndBound::Time(Bound::Absolute(t))) => Some(t),
            _ => None,
        };
        Self {
            end: interval.end,
            seen: 0,
            deadline,
        }
    }

    /// Judge one selected packet, whose timestamp is `ts` microseconds when it
    /// has one.
    pub fn admit(&mut self, ts: Option<i64>) -> Admission {
        match self.end {
            None => {
                self.seen = self.seen.saturating_add(1);
                Admission::Show
            }
            Some(EndBound::Packets(n)) => {
                if self.seen >= n {
                    return Admission::Stop;
                }
                self.seen = self.seen.saturating_add(1);
                Admission::Show
            }
            Some(EndBound::Time(bound)) => {
                // A relative end has no meaning until a packet fixes the
                // origin, so the first packet with a timestamp establishes it.
                if let (None, Bound::Relative(d), Some(ts)) = (self.deadline, bound, ts) {
                    self.deadline = Some(ts.saturating_add(d));
                }
                if let (Some(deadline), Some(ts)) = (self.deadline, ts)
                    && ts >= deadline
                {
                    return Admission::Stop;
                }
                self.seen = self.seen.saturating_add(1);
                Admission::Show
            }
        }
    }

    /// How many packets this interval showed.
    #[must_use]
    pub const fn shown(self) -> u64 {
        self.seen
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    fn ok(spec: &str) -> Vec<ReadInterval> {
        parse(spec).expect("accepted").0
    }

    #[test]
    fn the_five_examples_plan_14_owes() {
        // Plan 14 §5.3: "Examples that must round-trip".
        assert_eq!(ok("%+20").len(), 1);
        assert_eq!(ok("10%+20,01:30%01:45").len(), 2);
        assert_eq!(
            ok("01:23%+#42"),
            [ReadInterval {
                start: Some(Bound::Absolute(83_000_000)),
                end: Some(EndBound::Packets(42)),
            }]
        );
        assert_eq!(
            ok("%02:30"),
            [ReadInterval {
                start: None,
                end: Some(EndBound::Time(Bound::Absolute(150_000_000))),
            }]
        );
    }

    #[test]
    fn hash_is_only_legal_after_percent_plus() {
        // Observed: `#5` and `+#5` are start errors, `%#5` is an end error.
        assert_eq!(parse("#5"), Err(IntervalError::Start("#5".to_owned())));
        assert_eq!(parse("+#5"), Err(IntervalError::Start("#5".to_owned())));
        assert_eq!(parse("%#5"), Err(IntervalError::End("#5".to_owned())));
        assert_eq!(
            ok("%+#5"),
            [ReadInterval {
                start: None,
                end: Some(EndBound::Packets(5)),
            }]
        );
    }

    #[test]
    fn an_empty_interval_is_an_error_anywhere_in_the_list() {
        assert_eq!(parse(""), Err(IntervalError::Empty));
        assert_eq!(parse(",%+#2"), Err(IntervalError::Empty));
        assert_eq!(parse("%+#2,"), Err(IntervalError::Empty));
    }

    #[test]
    fn a_bad_count_warns_and_empties_the_interval_rather_than_failing() {
        // Observed: exit 0, no packets, one line on stderr.
        let (intervals, warnings) = parse("%+#-1").expect("not an error");
        assert_eq!(
            intervals,
            [ReadInterval {
                start: None,
                end: Some(EndBound::Packets(0)),
            }]
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings.first().expect("one").starts_with(BAD_COUNT));

        let (intervals, warnings) = parse("%+#5x").expect("not an error");
        assert_eq!(
            intervals.first().expect("one").end,
            Some(EndBound::Packets(0))
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn the_duration_grammar_is_the_measured_one() {
        assert_eq!(duration("0.1"), Some(100_000));
        assert_eq!(duration("100ms"), Some(100_000));
        assert_eq!(duration("0.1s"), Some(100_000));
        assert_eq!(duration("100000us"), Some(100_000));
        assert_eq!(duration("00:00:00.1"), Some(100_000));
        assert_eq!(duration("0:00.1"), Some(100_000));
        assert_eq!(duration("0:00.1s"), Some(100_000));
        assert_eq!(duration(" 0.1"), Some(100_000));
        assert_eq!(duration("1:0:0"), Some(3_600_000_000));
        assert_eq!(duration("-0.1"), Some(-100_000));
        assert_eq!(duration("000000000000000000000000009"), Some(9_000_000));

        for bad in [
            ".1", "1e-1", "1.0e1", "0x10", "1m", "1h", "1:2:3:4", "0.1 ", "",
        ] {
            assert_eq!(duration(bad), None, "{bad}");
        }
        // Overflow is a rejection, not a clamp.
        assert_eq!(duration("99999999999999999999"), None);
    }

    #[test]
    fn an_interval_eats_the_packet_that_ends_it() {
        // The measured `%+#1,%+#1` rule: the second interval starts *after*
        // the packet that stopped the first. `Admission::Stop` is what carries
        // that — the caller must not re-offer the packet.
        let mut c = Cursor::new(ReadInterval {
            start: None,
            end: Some(EndBound::Packets(1)),
        });
        assert_eq!(c.admit(Some(0)), Admission::Show);
        assert_eq!(c.admit(Some(1)), Admission::Stop);
        assert_eq!(c.shown(), 1);
    }

    #[test]
    fn a_zero_count_shows_nothing_but_still_consumes_one() {
        let mut c = Cursor::new(ReadInterval {
            start: None,
            end: Some(EndBound::Packets(0)),
        });
        assert_eq!(c.admit(Some(0)), Admission::Stop);
        assert_eq!(c.shown(), 0);
    }

    #[test]
    fn a_relative_end_is_measured_from_the_first_packet_actually_seen() {
        // `1%+0.04` on a file whose only keyframe is at 0 ends at 0.04, not
        // 1.04. The origin is the packet, never the requested seek.
        let mut c = Cursor::new(ReadInterval {
            start: Some(Bound::Absolute(1_000_000)),
            end: Some(EndBound::Time(Bound::Relative(40_000))),
        });
        assert_eq!(c.admit(Some(0)), Admission::Show);
        assert_eq!(c.admit(Some(33_367)), Admission::Show);
        assert_eq!(c.admit(Some(66_733)), Admission::Stop);
    }

    #[test]
    fn a_packet_with_no_timestamp_never_ends_a_timed_interval() {
        let mut c = Cursor::new(ReadInterval {
            start: None,
            end: Some(EndBound::Time(Bound::Relative(1))),
        });
        assert_eq!(c.admit(None), Admission::Show);
        assert_eq!(c.admit(None), Admission::Show);
    }

    #[test]
    fn parsing_never_panics() {
        for spec in [
            "",
            "%",
            "%%",
            "+",
            "-",
            "#",
            "%+",
            "%+#",
            "::::",
            "1:",
            ":1",
            "%+#99999999999999999999",
            "\u{0}",
            "\u{1f600}%+#1",
            "%+#1,%+#1,%+#1",
        ] {
            let _ = parse(spec);
        }
    }
}
