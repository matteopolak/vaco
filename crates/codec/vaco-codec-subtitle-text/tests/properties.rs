//! Properties every decoder in this crate must hold over arbitrary input.
//!
//! These mirror the `subtitle_text_decode` fuzz target's invariants so they
//! run under plain `cargo test` too — fuzzing needs a nightly toolchain and
//! is not part of the standard gate.

#![allow(clippy::expect_used, reason = "test code")]

use proptest::prelude::*;
use vaco_codec_subtitle_text::{TextCodec, ass, decode, movtext, srt, ttml, webvtt};

const CODECS: [TextCodec; 6] = [
    TextCodec::SubRip,
    TextCodec::Ass,
    TextCodec::WebVtt,
    TextCodec::MovText,
    TextCodec::Text,
    TextCodec::Ttml,
];

proptest! {
    /// No decoder panics on any byte string, for any codec.
    #[test]
    fn decode_never_panics(bytes: Vec<u8>) {
        for codec in CODECS {
            let _ = decode(codec, &bytes);
        }
    }

    /// The same, for arbitrary *text* — the interesting shapes (unbalanced
    /// angle brackets, stray ampersands) are far likelier to be generated
    /// from a string than from random bytes.
    #[test]
    fn text_decoders_never_panic(s: String) {
        let _ = srt::to_ass(&s);
        let _ = webvtt::to_ass(&s);
        let _ = ttml::to_ass(&s);
    }

    /// Markup-free input passes through every text decoder unchanged, except
    /// that line breaks become `\N`. This is the invariant that catches a
    /// tag scanner accidentally eating ordinary characters.
    #[test]
    fn plain_text_is_preserved(s in "[a-zA-Z0-9 .,!?]{0,64}") {
        prop_assert_eq!(srt::to_ass(&s), s.clone());
        prop_assert_eq!(webvtt::to_ass(&s), s.clone());
    }

    /// Output is bounded by input: every decoder here expands by a small
    /// constant factor at worst, so a cue cannot be an amplification vector.
    /// `<i>` -> `{\i1}` is the widest expansion at under 2x, and the tag
    /// scanner never re-emits a tag it consumed.
    #[test]
    fn output_is_bounded_by_input(s: String) {
        let bound = s.len().saturating_mul(8).saturating_add(64);
        prop_assert!(srt::to_ass(&s).len() <= bound);
        prop_assert!(webvtt::to_ass(&s).len() <= bound);
        prop_assert!(ttml::to_ass(&s).len() <= bound);
    }

    /// A mov_text sample never yields more output than a generous multiple of
    /// its own size, however its style table is shaped.
    #[test]
    fn movtext_output_is_bounded(bytes: Vec<u8>) {
        if let Some(out) = movtext::to_ass(&bytes) {
            prop_assert!(out.len() <= bytes.len().saturating_mul(16).saturating_add(64));
        }
    }

    /// Round trip: a dialogue chunk built from parts parses back to those
    /// parts, including a text field containing commas.
    #[test]
    fn ass_chunk_round_trips(
        read_order in 0u32..10_000,
        layer in -100i32..100,
        style in "[A-Za-z0-9]{0,12}",
        name in "[A-Za-z0-9]{0,12}",
        text in "[a-zA-Z0-9 ,]{0,64}",
    ) {
        let chunk = format!("{read_order},{layer},{style},{name},0,0,0,,{text}");
        let parsed = ass::parse_chunk(&chunk).expect("built as a valid chunk");
        prop_assert_eq!(parsed.read_order, read_order);
        prop_assert_eq!(parsed.layer, layer);
        prop_assert_eq!(parsed.style, style.as_str());
        prop_assert_eq!(parsed.name, name.as_str());
        prop_assert_eq!(parsed.text, text.as_str());
    }

    /// Whatever a decoder emits is valid UTF-8 by construction (it is a
    /// `String`) and never contains a bare CR or LF — line breaks must have
    /// become `\N`, or downstream ASS serialisation would produce a second
    /// dialogue line out of one event.
    #[test]
    fn no_raw_line_breaks_survive(s: String) {
        for out in [srt::to_ass(&s), webvtt::to_ass(&s), ttml::to_ass(&s)] {
            prop_assert!(!out.contains('\n'), "raw LF survived: {out:?}");
            prop_assert!(!out.contains('\r'), "raw CR survived: {out:?}");
        }
    }
}
