//! The reference-output parsers against arbitrary bytes.
//!
//! These consume the stdout of a separate process. That process is trusted, but
//! its output is still data crossing a boundary — a truncated pipe, a build
//! that prints an unexpected column, an interleaved warning. A parser that
//! panics there turns an oracle hiccup into a harness crash, so all four are
//! held to the same no-panic standard as everything else.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_conformance::compare::structured;
use vaco_conformance::extract::{colors, pixfmt};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else { return };

    let sections = structured::parse_sections(s);
    for section in &sections {
        let _ = section.key();
    }

    for fmt in pixfmt::parse_show_pixel_formats(s) {
        for field in pixfmt::FIELDS {
            let _ = field;
        }
        let _ = fmt.depths.len();
    }

    for fmt in pixfmt::parse_pix_fmts(s) {
        let _ = fmt.nb_components;
        let _ = fmt.flags.len();
    }

    let _ = colors::parse_colors(s);
});
