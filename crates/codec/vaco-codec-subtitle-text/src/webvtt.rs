//! `WebVTT` cue text to ASS override markup.
//!
//! # Where this differs from `SubRip`, measured
//!
//! `WebVTT` looks like `SubRip` with angle brackets and is not
//! (`ffmpeg -bitexact -i fixture.vtt -f ass -`):
//!
//! | Input | Output | vs [`crate::srt`] |
//! |---|---|---|
//! | `<i>x</i>` | `{\i1}x{\i0}` | same |
//! | `<b>x</b>`, `<u>x</u>` | `{\b1}…`, `{\u1}…` | same |
//! | `<v Roger>Hi there` | `Hi there` | srt has no voice span |
//! | `<c.yellow>classy</c>` | `classy` | srt would also drop it |
//! | `<ruby>base<rt>anno</rt></ruby>` | `baseanno` | — |
//! | `&amp; &lt;esc&gt;` | `& <esc>` | **srt leaves entities alone** |
//! | `A&nbsp;B` | `A\hB` | ASS hard space, not U+00A0 |
//!
//! The entity row is the one that matters: `WebVTT` is an HTML-derived format
//! and its decoder resolves character references, while `SubRip`'s does not.
//! Implementing one from the other silently corrupts every `&amp;` in a
//! subtitle file, so the two live in separate modules with separate tests
//! rather than sharing a "generic angle-bracket" helper.
//!
//! `WebVTT` has no `<s>`, `<font>` or colour markup: styling is carried by
//! classes (`<c.yellow>`) that resolve against a stylesheet this decoder does
//! not have, so a class span contributes its text and nothing else — which is
//! what the reference does too.

use crate::ass;

/// Resolve one `WebVTT` character reference body (the text between `&` and
/// `;`) to its replacement.
///
/// # Exactly six names, measured
///
/// `WebVTT`'s *current* specification (§4.2.2, §6.4) delegates to HTML's full
/// named-character-reference table — some 2,200 names — plus numeric
/// references; it has done since the format dropped its own six-entity list
/// in November 2015. The reference binary did not follow. Measured on a
/// fixture carrying ten different references:
///
/// | Input | Reference output |
/// |---|---|
/// | `&amp;` `&lt;` `&gt;` | `&` `<` `>` |
/// | `&lrm;` `&rlm;` | U+200E, U+200F |
/// | `&nbsp;` | `\h` — ASS's hard space, **not** U+00A0 |
/// | `&quot;` `&apos;` | left verbatim |
/// | `&hellip;` `&eacute;` | left verbatim |
/// | `&#65;` `&#x42;` | left verbatim |
///
/// So this implements the pre-2015 six and nothing else, because reproducing
/// the reference is the goal and the reference stopped at six. The
/// specification's wider table is recorded in the crate docs as a known gap
/// rather than implemented against a decoder that would disagree with it.
///
/// `&nbsp;` returning a two-character ASS escape rather than a character is
/// why this returns `&str`: the replacement is markup, not text.
fn entity(body: &str) -> Option<&'static str> {
    match body {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "lrm" => Some("\u{200E}"),
        "rlm" => Some("\u{200F}"),
        // ASS hard space. A *literal* U+00A0 in the payload is not converted
        // — measured: a `.srt` cue containing one decodes with the byte pair
        // intact — so this mapping belongs to the entity, not to escaping.
        "nbsp" => Some("\\h"),
        _ => None,
    }
}

/// Expand character references in `src`, leaving unrecognised ones alone.
#[must_use]
pub fn decode_entities(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(amp) = rest.find('&') {
        let (before, from_amp) = rest.split_at(amp);
        out.push_str(before);
        let body_start = from_amp.get(1..).unwrap_or("");
        // A reference is short; a bare '&' in prose must not swallow the line
        // looking for a semicolon that belongs to something else.
        let limit = body_start.len().min(32);
        let resolved = body_start
            .get(..limit)
            .and_then(|w| w.find(';'))
            .and_then(|semi| {
                body_start
                    .get(..semi)
                    .and_then(entity)
                    .map(|c| (c, semi.saturating_add(1)))
            });
        if let Some((c, consumed)) = resolved {
            out.push_str(c);
            rest = body_start.get(consumed..).unwrap_or("");
        } else {
            out.push('&');
            rest = body_start;
        }
    }
    out.push_str(rest);
    out
}

/// Convert one `WebVTT` cue's payload text into ASS dialogue text.
#[must_use]
pub fn to_ass(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        let (before, from_lt) = rest.split_at(lt);
        ass::escape_plain(&mut out, &decode_entities(before));
        let Some(gt_rel) = from_lt.find('>') else {
            ass::escape_plain(&mut out, &decode_entities(from_lt));
            return out;
        };
        let inner = from_lt.get(1..gt_rel).unwrap_or("");
        rest = from_lt.get(gt_rel.saturating_add(1)..).unwrap_or("");
        emit_tag(&mut out, inner);
    }
    ass::escape_plain(&mut out, &decode_entities(rest));
    out
}

fn emit_tag(out: &mut String, inner: &str) {
    let trimmed = inner.trim();
    if let Some(name) = trimmed.strip_prefix('/') {
        match tag_name(name).as_str() {
            "i" => out.push_str("{\\i0}"),
            "b" => out.push_str("{\\b0}"),
            "u" => out.push_str("{\\u0}"),
            _ => {} // c, v, lang, ruby, rt and anything unknown: text only
        }
        return;
    }
    match tag_name(trimmed).as_str() {
        "i" => out.push_str("{\\i1}"),
        "b" => out.push_str("{\\b1}"),
        "u" => out.push_str("{\\u1}"),
        _ => {}
    }
}

/// The tag name from a span's start-tag body: everything before the first
/// whitespace (which begins an annotation, as in `<v Roger>`) or the first
/// `.` (which begins a class list, as in `<c.yellow.loud>`).
fn tag_name(src: &str) -> String {
    let s = src.trim();
    let end = s
        .find(|c: char| c.is_whitespace() || c == '.')
        .unwrap_or(s.len());
    s.get(..end).unwrap_or("").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_tags_match_the_reference() {
        assert_eq!(
            to_ass("<i>it</i> <b>bo</b> <u>un</u>"),
            "{\\i1}it{\\i0} {\\b1}bo{\\b0} {\\u1}un{\\u0}"
        );
    }

    #[test]
    fn voice_and_class_spans_contribute_only_their_text() {
        assert_eq!(to_ass("<v Roger>Hi there"), "Hi there");
        assert_eq!(to_ass("<c.yellow>classy</c>"), "classy");
        assert_eq!(
            to_ass("<ruby>base<rt>anno</rt></ruby> plain"),
            "baseanno plain"
        );
    }

    #[test]
    fn entities_are_decoded_unlike_srt() {
        assert_eq!(to_ass("&amp; &lt;esc&gt;"), "& <esc>");
    }

    #[test]
    fn numeric_references_are_left_alone_as_the_reference_leaves_them() {
        assert_eq!(decode_entities("&#65;&#x42;"), "&#65;&#x42;");
    }

    #[test]
    fn nbsp_becomes_an_ass_hard_space_not_u00a0() {
        assert_eq!(to_ass("A&nbsp;B"), "A\\hB");
    }

    #[test]
    fn quot_and_apos_are_not_in_the_reference_six() {
        assert_eq!(decode_entities("&quot;&apos;"), "&quot;&apos;");
    }

    #[test]
    fn bidi_marks_decode() {
        assert_eq!(decode_entities("&lrm;&rlm;"), "\u{200E}\u{200F}");
    }

    #[test]
    fn an_unrecognised_reference_is_left_alone() {
        assert_eq!(decode_entities("a & b"), "a & b");
        assert_eq!(decode_entities("&notareference;"), "&notareference;");
    }

    #[test]
    fn a_bare_ampersand_does_not_swallow_a_distant_semicolon() {
        let long = format!("&{}; tail", "x".repeat(60));
        assert_eq!(decode_entities(&long), long);
    }

    #[test]
    fn newlines_become_backslash_n() {
        assert_eq!(to_ass("a\nb"), "a\\Nb");
    }
}
