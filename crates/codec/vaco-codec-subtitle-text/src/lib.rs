//! Text subtitle decode: `SubRip`, ASS/SSA, `WebVTT`, 3GPP timed text
//! (`mov_text`), raw text and TTML.
//!
//! # What it is
//!
//! The markup half of six subtitle codecs. A demuxer's job ends at "here are
//! the bytes shown between these two timestamps"
//! (`vaco_format_subtitle::Cue`, whose own docs say markup "is rendering, and
//! rendering is a decoder's job"). This crate is that job: cue bytes in,
//! styled text out.
//!
//! # The one thing to know first
//!
//! **Every decoder here produces ASS override markup, not plain text**, and
//! that is not a design choice — it is what the reference does. Measured
//! (D17) with `ffmpeg -bitexact -i fixture -f ass -` against fixtures written
//! for the purpose: `subrip`, `webvtt`, `mov_text` and `text` all emit an ASS
//! `Dialogue:` line, so `<i>x</i>` in a `.srt` comes back as `{\i1}x{\i0}`
//! and a line break as `\N`. One decoder family, one output language.
//!
//! | Module | Input | Notes |
//! |---|---|---|
//! | [`srt`] | `SubRip` markup | entities **not** decoded; `</font>` closes in opening order |
//! | [`webvtt`] | `WebVTT` cue text | entities **are** decoded; `<v>`/`<c>`/`<ruby>` contribute text only |
//! | [`movtext`] | tx3g binary sample | `styl` offsets are *character* offsets; spans close with `{\r}` |
//! | [`text`] | raw bytes | line breaks only |
//! | [`ass`] | dialogue chunk | the shared output language, plus [`ass::parse_chunk`] |
//! | [`ttml`] | TTML `<p>` content | **no reference decoder exists** — spec-derived, see below |
//!
//! # Reachable from `vaco-registry`
//!
//! [`registry::TextSubtitleDecoder`] is the `vaco_codec_core::Decoder` face
//! over [`decode`], registered under seven names (`subrip`, `ass`, `ssa`,
//! `webvtt`, `mov_text`, `text`, `ttml` — `ssa` and `ass` share this
//! crate's ASS-chunk decode, since the reference's own `ssa` decoder is
//! documented as "(codec ass)"). Every one emits `SubtitleContent::Ass`,
//! matching the measured reference behaviour described above. See
//! [`registry`]'s own module docs for `Caps` and timing.
//!
//! # Verification status, per format
//!
//! Five of the six were differential-tested against the reference binary and
//! agree on every fixture. TTML was not and could not be: `ffmpeg -decoders`
//! has no `ttml` row and `-demuxers` has none either — the reference ships a
//! TTML *muxer* only. [`ttml`] is therefore implemented from the W3C TTML1
//! recommendation with nothing to diff against, and is exactly as good as its
//! own tests. `docs/codec/vaco-codec-subtitle-text.md` carries the
//! fixture-by-fixture table.
//!
//! # Known gaps, stated rather than implied
//!
//! - **`WebVTT` character references are a subset.** The current spec
//!   (§4.2.2, §6.4) delegates to HTML's full named-reference table — roughly
//!   2,200 names, some expanding to two code points. [`webvtt::decode_entities`]
//!   implements the six names `WebVTT` defined before it adopted HTML's table
//!   in 2015 — and so, measured, does the reference: `&quot;`, `&apos;`,
//!   `&hellip;`, `&#65;` and `&#x42;` all come back verbatim from
//!   `ffmpeg -f ass -`. Matching the reference and matching the current spec
//!   are different targets here, and this crate matches the reference.
//! - **`mov_text` UTF-16 text is not decoded.** 3GPP TS 26.245 §5.1 allows a
//!   sample's text to be UTF-16 behind a BOM. [`movtext`] treats text as
//!   UTF-8 always; the reference's own encoder writes UTF-8, so this was not
//!   reachable in testing.
//! - **TTML referenced styles are not resolved.** Only inline `tts:` attributes
//!   on a `<span>` are read, not `<style>`/`<region>` definitions elsewhere in
//!   the document, which a decoder handed one `<p>` does not have.

#![forbid(unsafe_code)]

pub mod ass;
pub mod movtext;
pub mod registry;
pub mod srt;
pub mod text;
pub mod ttml;
pub mod webvtt;

pub use ass::Dialogue;
pub use movtext::{StyleRecord, TextSample};

/// Which text subtitle codec a payload is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextCodec {
    /// `SubRip` (`subrip`, `srt`).
    SubRip,
    /// ASS/SSA, as the reference's nine-field dialogue chunk.
    Ass,
    /// `WebVTT`.
    WebVtt,
    /// 3GPP timed text (`mov_text`, `tx3g`) — the one binary member.
    MovText,
    /// Raw text with no markup.
    Text,
    /// TTML inline content.
    Ttml,
}

impl TextCodec {
    /// The reference's own name for this codec, as `ffmpeg -decoders` spells
    /// it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SubRip => "subrip",
            Self::Ass => "ass",
            Self::WebVtt => "webvtt",
            Self::MovText => "mov_text",
            Self::Text => "text",
            Self::Ttml => "ttml",
        }
    }
}

/// Decode one packet payload to ASS dialogue text.
///
/// Returns `None` when the payload carries no event at all — an `mov_text`
/// gap sample is the only case that occurs in practice.
///
/// Byte payloads that are not valid UTF-8 are decoded lossily for the five
/// text formats, matching where `vaco_format_subtitle::Cue`'s own docs place
/// that decision ("Rejecting or replacing invalid UTF-8 inside a cue is the
/// decoder's job, not the demuxer's").
#[must_use]
pub fn decode(codec: TextCodec, payload: &[u8]) -> Option<String> {
    if codec == TextCodec::MovText {
        return movtext::to_ass(payload);
    }
    let s = String::from_utf8_lossy(payload);
    Some(match codec {
        TextCodec::SubRip => srt::to_ass(&s),
        TextCodec::WebVtt => webvtt::to_ass(&s),
        TextCodec::Ttml => ttml::to_ass(&s),
        TextCodec::Text => text::to_ass(&s),
        // A reference-shaped chunk yields its Text field; a bare Text field
        // (this workspace's own ASS demuxer's shape) is the whole payload.
        // Either way the result goes through the same line-break escaping as
        // every other codec here: a well-formed ASS dialogue line cannot
        // contain a raw CR or LF, so this changes nothing the reference
        // produces, and it stops a malformed chunk from turning one event
        // into two dialogue lines downstream. Found by fuzzing
        // (`fuzz/seeds/subtitle_text_decode/regression-ass-raw-cr`), which
        // reached it with a nine-field chunk carrying a raw 0x0D.
        TextCodec::Ass => {
            ass::parse_chunk(&s).map_or_else(|| text::to_ass(&s), |d| text::to_ass(d.text))
        }
        TextCodec::MovText => unreachable!(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn codec_names_match_the_reference_spelling() {
        assert_eq!(TextCodec::SubRip.name(), "subrip");
        assert_eq!(TextCodec::MovText.name(), "mov_text");
    }

    #[test]
    fn decode_dispatches_per_codec() {
        assert_eq!(
            decode(TextCodec::SubRip, b"a <i>b</i>").unwrap(),
            "a {\\i1}b{\\i0}"
        );
        // Same bytes, WebVTT rules: entities decode.
        assert_eq!(decode(TextCodec::WebVtt, b"&amp;").unwrap(), "&");
        // ...and SubRip rules: they do not.
        assert_eq!(decode(TextCodec::SubRip, b"&amp;").unwrap(), "&amp;");
    }

    #[test]
    fn an_ass_chunk_yields_only_its_text_field() {
        assert_eq!(
            decode(TextCodec::Ass, b"0,0,Default,,0,0,0,,hi, there").unwrap(),
            "hi, there"
        );
    }

    #[test]
    fn a_bare_ass_text_field_passes_through() {
        assert_eq!(
            decode(TextCodec::Ass, b"just prose").unwrap(),
            "just prose"
        );
    }

    #[test]
    fn an_ass_chunk_never_leaks_a_raw_line_break() {
        // Fuzz-found: the Text field used to pass through unescaped, so a
        // chunk carrying a raw CR produced output that would serialise as
        // two dialogue lines.
        let got = decode(TextCodec::Ass, b"896,8,89y6,\r96,8,896,8,89y6,\r").unwrap();
        assert!(!got.contains('\r') && !got.contains('\n'), "got {got:?}");
    }

    #[test]
    fn a_movtext_gap_sample_decodes_to_no_event() {
        assert_eq!(decode(TextCodec::MovText, &[0, 0]), None);
    }
}
