//! Parses one filter's own named-constant options out of real
//! `ffmpeg -h filter=<name>` output — the instrument
//! `crates/filter/vaco-filter-deinterlace/vaco-filter-geometry/vaco-filter-lut`'s
//! own 2026-08-28 option-parsing survey used by hand (in a throwaway
//! script, not this crate) to find the class of bug where a real command
//! line using the reference's own documented spelling
//! (`mode=smear`/`parity=tff`) fails to parse against `vaco` outright.
//!
//! # The pitfall this parser is built around, not patched after
//!
//! `ffmpeg -h filter=<name>` is not always one flat option table. Some
//! filters (`aresample`, most muxers/demuxers `-h`) print their own
//! `<name> AVOptions:` section, then one or more *nested* component
//! sections (`SWResampler AVOptions:`, ...) whose own named-constant rows
//! use the identical indentation as the top-level filter's constants. A
//! parser that tracks "the last option name seen" without a hard
//! section boundary will attach a nested section's constants to
//! whichever top-level option happened to be last, silently. This is
//! not hypothetical: the throwaway survey script hit exactly this on
//! `aresample`, attributing `SWResampler`'s `dither_method`/`resampler`
//! constants to `aresample`'s own `sample_rate` (which has none), and
//! it was only caught by re-checking the real `-h` output by hand rather
//! than trusting the tool. [`tests::aresample_nested_avoptions_section_is_excluded`]
//! pins exactly this case using real captured output, checked in before
//! [`parse`] is ever trusted by [`crate`]'s own conformance gate.
//!
//! # What `parse` returns
//!
//! One entry per option that has at least one named constant, each a
//! list of `(name, value)` pairs in the order the reference prints them
//! (an option may have more than one name per value, e.g. `il`'s
//! `interleave`/`i`).

use std::collections::BTreeMap;

/// One option's named constants, in the order `-h` prints them.
pub type OptionConsts = BTreeMap<String, Vec<(String, i64)>>;

/// Parse `filter_name`'s own named-constant options out of a full
/// `ffmpeg -h filter=<filter_name>` capture (stdout+stderr concatenated,
/// as the reference actually writes it).
///
/// Scopes strictly to the text between `<filter_name> AVOptions:` and
/// the next line that itself ends in `AVOptions:` (a nested component
/// section) or end of input — never to indentation alone, which is the
/// same shape for a nested section's own constants. A filter with no
/// `AVOptions:` header at all (no options) returns an empty map, not an
/// error: `il`/`overlay`/etc. always have one, but a source or sink
/// filter might not.
#[must_use]
pub fn parse(help_text: &str, filter_name: &str) -> OptionConsts {
    let own_header = format!("{filter_name} AVOptions:");
    let mut in_section = false;
    let mut current_option: Option<String> = None;
    let mut out = OptionConsts::new();

    for line in help_text.lines() {
        if !in_section {
            if line.trim_end() == own_header {
                in_section = true;
            }
            continue;
        }
        // Any line ending in "AVOptions:" -- including a repeat of our
        // own header, which does not happen in practice but would be
        // ambiguous to keep parsing past -- closes the section. This is
        // the hard boundary the module doc's pitfall needs: indentation
        // alone cannot tell a nested section's constant rows apart from
        // ours.
        if line.trim_end().ends_with("AVOptions:") {
            break;
        }
        // An option row: exactly three leading spaces, then a name, then
        // a `<type>` tag. A nested section's own option rows are prefixed
        // with `  -` (two spaces and a dash) instead, in the reference's
        // own `-h` formatting -- deliberately not matched here, since a
        // line shaped that way is never one of `filter_name`'s own
        // options even before its section boundary is reached (nested
        // sections always come after, but this keeps the two shapes
        // distinct on their own terms rather than relying only on the
        // boundary check above).
        let mut chars = line.chars();
        let is_option_row = matches!(
            (chars.next(), chars.next(), chars.next(), chars.next()),
            (Some(' '), Some(' '), Some(' '), Some(c)) if c != ' ' && c != '-'
        );
        if is_option_row {
            let name = line.split_whitespace().next();
            current_option = name.map(str::to_owned);
            continue;
        }
        // A named-constant row: exactly five leading spaces, then a
        // name, then an integer value.
        let five_space = line.starts_with("     ") && !line.starts_with("      ");
        if five_space {
            let mut words = line.split_whitespace();
            let (Some(const_name), Some(value_tok)) = (words.next(), words.next()) else {
                continue;
            };
            let Ok(value) = value_tok.parse::<i64>() else {
                continue;
            };
            let Some(opt) = &current_option else {
                continue;
            };
            out.entry(opt.clone())
                .or_default()
                .push((const_name.to_owned(), value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact case that broke the throwaway survey script: `aresample`'s
    /// own `sample_rate` has zero named constants, but the reference's
    /// full `-h` output for it also prints a nested `SWResampler
    /// AVOptions:` section (`dither_method`, `resampler`, ...) whose own
    /// constant rows use the identical five-space indentation. Captured
    /// verbatim from real `ffmpeg 8.1 -h filter=aresample`.
    #[test]
    fn aresample_nested_avoptions_section_is_excluded() {
        let help = "\
Filter aresample
  Resample audio data.
    Inputs:
       #0: default (audio)
    Outputs:
       #0: default (audio)
aresample AVOptions:
   sample_rate       <int>        ..F.A...... (from 0 to INT_MAX) (default 0)

SWResampler AVOptions:
  -isr               <int>        ....A...... set input sample rate (from 0 to INT_MAX) (default 0)
  -dither_method     <int>        ....A...... set dither method (from 0 to 71) (default 0)
     rectangular     1            ....A...... select rectangular dither
     triangular      2            ....A...... select triangular dither
  -resampler         <int>        ....A...... set resampling Engine (from 0 to 1) (default swr)
     swr             0            ....A...... select SW Resampler
     soxr            1            ....A...... select SoX Resampler
";
        let consts = parse(help, "aresample");
        assert!(
            consts.is_empty(),
            "aresample has no named-constant options of its own; got {consts:?}"
        );
    }

    /// The ordinary, single-section case: every option's constants
    /// attach to the right name, in printed order, and a second option
    /// in the same filter does not leak into the first's list.
    #[test]
    fn ordinary_single_section_parses_every_option() {
        let help = "\
Filter maskedthreshold
maskedthreshold AVOptions:
   threshold         <int>        ..FV.....T. set threshold (from 0 to 65535) (default 1)
   planes            <int>        ..FV.....T. set planes (from 0 to 15) (default 15)
   mode              <int>        ..FV....... set mode (from 0 to 1) (default abs)
     abs             0            ..FV....... 
     diff            1            ..FV....... 
";
        let consts = parse(help, "maskedthreshold");
        assert_eq!(consts.len(), 1, "only `mode` has named constants: {consts:?}");
        assert_eq!(
            consts.get("mode"),
            Some(&vec![("abs".to_owned(), 0), ("diff".to_owned(), 1)])
        );
    }

    /// A filter with no `AVOptions:` header (no options) parses to an
    /// empty map, not an error.
    #[test]
    fn no_options_header_is_empty_not_an_error() {
        let help = "Filter nullsink\n  Do absolutely nothing with the input video.\n";
        assert!(parse(help, "nullsink").is_empty());
    }

    /// Two names for the same value (`il`'s `interleave`/`i`) both attach
    /// to the same option, in printed order.
    #[test]
    fn two_names_for_one_value_both_attach() {
        let help = "\
Filter il
il AVOptions:
   luma_mode         <int>        ..FV....... select luma mode (from 0 to 2) (default none)
     none            0            ..FV....... 
     interleave      1            ..FV....... 
     i               1            ..FV....... 
     deinterleave    2            ..FV....... 
     d               2            ..FV....... 
";
        let consts = parse(help, "il");
        assert_eq!(
            consts.get("luma_mode"),
            Some(&vec![
                ("none".to_owned(), 0),
                ("interleave".to_owned(), 1),
                ("i".to_owned(), 1),
                ("deinterleave".to_owned(), 2),
                ("d".to_owned(), 2),
            ])
        );
    }
}
