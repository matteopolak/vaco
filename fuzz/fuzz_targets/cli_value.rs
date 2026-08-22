//! The two numeric value grammars, against arbitrary text.
//!
//! An option value comes straight off `argv`, so it is untrusted (D6). Two
//! grammars share this target because the point is that they are *different*:
//! `-ac 1*2` is a hard error while `-crf 1*2` is 2, and a change that quietly
//! merged them would break real command lines in both directions.
//!
//! Beyond "does not panic":
//!
//! * **`strtod` is exact about how much it consumed.** The tail must be a
//!   suffix of the input, and a successful whole-string parse must leave
//!   nothing — that is the property the `Expected number for …` check rests on.
//! * **Failure means nothing consumed.** C's contract is `endptr = nptr` when
//!   no conversion happens, and `-ac ""` succeeding while `-ac " "` fails is a
//!   direct consequence.
//! * **Evaluation is deterministic and total.** A compiled expression
//!   evaluated twice gives the same bits, NaN included.
//! * **Both dialects are exercised.** On the option path `default`, `max` and
//!   `min` are constants naming the option's own bounds, so they shadow the
//!   builtin functions; on the plain path they are the functions. Neither
//!   dialect's acceptance set contains the other's, so the target drives both
//!   rather than relating them.
//!
//! fuzz-crate: vaco-cli-core

#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_cli_core::{Expression, NumberLimits, OptionConstants, parse_number, strtod};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    if s.len() > 4096 {
        return;
    }

    // --- the plain-number grammar -----------------------------------------
    let (value, tail) = strtod(s);
    assert!(
        tail.len() <= s.len() && s.ends_with(tail),
        "strtod tail {tail:?} is not a suffix of {s:?}"
    );
    let consumed = s.len() - tail.len();
    if consumed == 0 {
        // No conversion: the value is zero and the tail is the whole input,
        // leading whitespace included.
        assert_eq!(tail, s, "endptr must equal nptr when nothing parses");
        assert_eq!(value.to_bits(), 0.0f64.to_bits(), "no conversion, no value");
    }

    for limits in [
        NumberLimits::int32(),
        NumberLimits::int64(),
        NumberLimits::float(),
    ] {
        match parse_number("opt", s, limits) {
            Ok(v) => {
                assert!(
                    tail.is_empty(),
                    "{s:?} was accepted with {tail:?} left over"
                );
                assert!(
                    v >= limits.min && v <= limits.max || v.is_nan(),
                    "{v} escaped its bounds"
                );
                if limits.integral {
                    assert!(
                        v.fract() == 0.0 || v.is_nan(),
                        "{v} is not integral but an integer option took it"
                    );
                }
            }
            Err(_) => {
                // Rejection is fine; it must simply not have panicked.
            }
        }
    }

    // --- the expression grammar -------------------------------------------
    if let Ok(expr) = Expression::compile("opt", s) {
        let a = expr.value();
        let b = expr.value();
        assert_eq!(a.to_bits(), b.to_bits(), "evaluation is not deterministic");
        assert_eq!(expr.source(), s, "the compiled source was not preserved");
    }

    // --- the option dialect -----------------------------------------------
    if let Ok(expr) = Expression::compile_for_option("opt", s) {
        let vars = OptionConstants::new(1.0, -2.0, 3.0).values();
        let a = expr.eval(&vars);
        let b = expr.eval(&vars);
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "option-dialect evaluation is not deterministic"
        );
        // NOTE: the two dialects are genuinely incomparable, so there is no
        // subset assertion to make here. `min` parses in the option dialect
        // (a bound variable) and not in the plain one (an unknown constant);
        // `max(1,2)` parses in the plain one (a builtin call) and not in the
        // option dialect (a constant, so `(1,2)` is trailing garbage). Both
        // directions were verified against the reference.
    }
});
