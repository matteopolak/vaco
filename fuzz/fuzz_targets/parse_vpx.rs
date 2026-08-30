//! VP8/VP9 header parsing against arbitrary bytes.
//!
//! Both parsers follow the "whole input is one already-framed sample"
//! contract `vaco-parse-vpx`'s crate doc describes — the same one
//! `vaco-parse-opus`'s fuzz target exercises for Opus — so unlike
//! `parse_h264`/`parse_hevc` there is no incremental reassembly to check for
//! chunking-invariance. What is worth checking:
//!
//! 1. **Total consumption.** `Parser::parse` on non-empty input always
//!    reports consuming the whole slice, whatever it decides the bytes mean.
//! 2. **A reported size is never zero and never absurd.** VP9's `frame_size()`
//!    codes width/height as `x_minus_1`, so zero can only mean "not present",
//!    and the field-plus-one arithmetic tops out at exactly `1 << 16`
//!    (0xFFFF + 1), not one less — see the comment at the assertion itself;
//!    VP8 dimensions are masked to 14 bits, bounding them independently of
//!    what this crate does with them.
//! 3. **The superframe index never walks past the buffer it was found in** —
//!    `last_subframe`'s return, when `Some`, always points inside `data`.
//! 4. **No panic anywhere**, including `set_extradata` on a `vpcC` payload of
//!    arbitrary bytes.
//!
//! fuzz-crate: vaco-parse-vpx
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Parser;
use vaco_limits::Limits;
use vaco_parse_vpx::superframe::last_subframe;
use vaco_parse_vpx::{Vp8Parser, Vp9Parser, parse_display_header, parse_frame_tag, parse_vpcc};

fn within(outer: &[u8], inner: &[u8]) -> bool {
    let base = outer.as_ptr() as usize;
    let start = inner.as_ptr() as usize;
    start >= base && start.saturating_add(inner.len()) <= base.saturating_add(outer.len())
}

fuzz_target!(|data: &[u8]| {
    if let Some(tag) = parse_frame_tag(data) {
        if let Some((w, h)) = tag.size {
            assert!(w <= 0x3fff && h <= 0x3fff, "VP8 dimensions are 14-bit");
        }
    }

    if let Some(h) = parse_display_header(data) {
        if let Some((w, height)) = h.size {
            assert!(w > 0 && height > 0, "frame_size() codes width/height minus 1");
            // Not `< (1 << 16)`: §7.2.3 of the VP9 Bitstream & Decoding
            // Process Specification (v0.6) defines `FrameWidth =
            // frame_width_minus_1 + 1` with `frame_width_minus_1` a plain
            // f(16) field and no additional bitstream-conformance ceiling on
            // a key/intra-only frame's value (the only conformance bounds in
            // that section are the *inter*-frame reference-scaling ratios in
            // §7.2.5, which do not apply here). So the all-ones encoding
            // (0xFFFF) is a legal `frame_width_minus_1` and decodes to
            // 65536 = 1 << 16 — one past a 16-bit field's own range, since
            // the value is the field *plus one*. `vaco_parse_vpx::profile`
            // already encodes this: `MAX_DIMENSION = 1 << 16` is used as an
            // inclusive bound (`q.width <= self.max_h_size` in
            // `LevelConstraints::admits`), independently agreeing that 65536
            // is admissible. This assertion previously used `<` and failed
            // on crash-739636bad2164ffa41e3c104cc036d1d229bf408, which
            // decodes to exactly (65536, 64752) — a false harness invariant,
            // not a parser defect.
            assert!(w <= (1 << 16) && height <= (1 << 16), "16-bit-plus-one fields");
        }
    }

    if let Some(sub) = last_subframe(data) {
        assert!(within(data, sub), "a superframe index pointed outside its buffer");
        // Not asserted: `sub` is non-empty. An index's own size field can
        // legally spell a zero-length final frame — `last_subframe` reports
        // whatever the index describes rather than second-guessing it, and
        // `parse_uncompressed_header(&[])` (the only caller) already answers
        // `None` for an empty slice, so a zero-length "sub-frame" is inert,
        // not a bug. Found by this exact target: an earlier version of this
        // assertion claimed otherwise and failed on `[0xD0, 0, 0, 0, 0xD0]`.
    }

    let _ = parse_vpcc(data);

    {
        let mut parser = Vp8Parser::new(Limits::strict());
        if let Ok((_pkt, used)) = parser.parse(data) {
            assert_eq!(used, data.len(), "the whole input must be consumed");
        }
    }
    {
        let mut parser = Vp9Parser::new(Limits::strict());
        let _ = parser.set_extradata(data);
        if let Ok((_pkt, used)) = parser.parse(data) {
            assert_eq!(used, data.len(), "the whole input must be consumed");
        }
    }
});
