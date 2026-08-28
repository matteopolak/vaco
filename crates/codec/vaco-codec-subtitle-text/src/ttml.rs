//! TTML1 inline content to ASS override markup.
//!
//! # The one format here with no reference decoder
//!
//! Measured: `ffmpeg -decoders` lists no `ttml` row at all
//! (`ffmpeg -h decoder=ttml` answers "known to `FFmpeg`, but no decoders for it
//! are available"), and `-demuxers` has no `ttml` row either — only a muxer.
//! So unlike the other five modules in this crate, nothing here was
//! differential-tested against the reference: it is implemented from the W3C
//! TTML1 recommendation and is exactly as good as its own tests.
//! `crates/format/vaco-subtitle-text/src/ttml.rs` records the same finding
//! for the demuxer side.
//!
//! # Scope
//!
//! This is the *decode* half: a `<p>`'s inline content becoming styled text.
//! Timing (`begin`/`end`/`dur`) and cue segmentation are the demuxer's job and
//! already live in `vaco-subtitle-text`. What that crate currently drops, and
//! this one recovers, is the inline styling — `<span tts:fontStyle="italic">`
//! and friends — plus `<br/>`.
//!
//! Recognised: `<br/>` as a line break; `tts:fontStyle="italic"`,
//! `tts:fontWeight="bold"`, `tts:textDecoration="underline"` and
//! `tts:color="…"` on a `<span>`. Not recognised: referenced `<style>`/
//! `<region>` definitions (the styling lives in a separate element this
//! decoder is not handed), `tts:textOutline`, and ruby. An unrecognised
//! attribute contributes nothing rather than guessing a mapping.

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::ass;

/// One `<span>`'s worth of closers, innermost last, so it can be unwound.
#[derive(Debug, Default, Clone)]
struct Span {
    closers: Vec<&'static str>,
}

/// Convert a TTML fragment — a whole `<p>` element, or the bare inline
/// content of one — into ASS dialogue text.
///
/// Anything outside `<p>` is ignored, so passing a complete TTML document
/// yields the concatenated content of its paragraphs; passing a bare fragment
/// like `hello <span tts:fontStyle="italic">there</span>` works too, since a
/// fragment with no `<p>` is treated as inline content directly.
#[must_use]
pub fn to_ass(fragment: &str) -> String {
    let has_p = fragment.contains("<p") || fragment.contains(":p ") || fragment.contains(":p>");
    let mut reader = Reader::from_str(fragment);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = false;

    let mut out = String::new();
    let mut spans: Vec<Span> = Vec::new();
    // When the fragment has <p> elements, only their contents count.
    let mut depth_in_p = u32::from(!has_p);

    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                if name == "p" {
                    depth_in_p = depth_in_p.saturating_add(1);
                    continue;
                }
                if depth_in_p == 0 {
                    continue;
                }
                if name == "span" {
                    let mut span = Span::default();
                    for attr in e.attributes().flatten() {
                        let key = local_name(attr.key.as_ref());
                        let value = attr.unescape_value().unwrap_or_default();
                        push_style(&mut out, &mut span, &key, value.as_ref());
                    }
                    spans.push(span);
                } else if name == "br" {
                    out.push_str("\\N");
                }
            }
            Ok(Event::Empty(e)) => {
                let name = local_name(e.name().as_ref());
                if depth_in_p > 0 && name == "br" {
                    out.push_str("\\N");
                }
            }
            Ok(Event::End(e)) => {
                let name = local_name(e.name().as_ref());
                if name == "p" {
                    depth_in_p = depth_in_p.saturating_sub(1);
                } else if depth_in_p > 0 && name == "span" {
                    if let Some(span) = spans.pop() {
                        for closer in span.closers.iter().rev() {
                            out.push_str(closer);
                        }
                    }
                } else {
                    // Any other end tag contributes nothing.
                }
            }
            Ok(Event::Text(e)) => {
                if depth_in_p > 0 {
                    let text = e.unescape().unwrap_or_default();
                    ass::escape_plain(&mut out, text.as_ref());
                }
            }
            Ok(_) => {}
        }
    }
    out
}

fn push_style(out: &mut String, span: &mut Span, key: &str, value: &str) {
    match key {
        "fontStyle" if value.eq_ignore_ascii_case("italic") => {
            out.push_str("{\\i1}");
            span.closers.push("{\\i0}");
        }
        "fontWeight" if value.eq_ignore_ascii_case("bold") => {
            out.push_str("{\\b1}");
            span.closers.push("{\\b0}");
        }
        "textDecoration" if value.eq_ignore_ascii_case("underline") => {
            out.push_str("{\\u1}");
            span.closers.push("{\\u0}");
        }
        "color" => {
            if let Some((r, g, b)) = ass::parse_color(value) {
                ass::push_color(out, r, g, b);
                span.closers.push("{\\c}");
            }
        }
        _ => {}
    }
}

/// An element or attribute name with any namespace prefix removed, so
/// `tt:span`, `tts:fontStyle` and bare `span` all compare equal to their
/// local part. TTML is namespace-heavy and a document is free to bind any
/// prefix it likes.
fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_owned(),
        None => s.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paragraph_text_comes_through() {
        assert_eq!(to_ass("<p>hello there</p>"), "hello there");
    }

    #[test]
    fn br_becomes_a_line_break() {
        assert_eq!(to_ass("<p>one<br/>two</p>"), "one\\Ntwo");
    }

    #[test]
    fn italic_and_bold_spans_map_to_override_tags() {
        assert_eq!(
            to_ass(r#"<p>a <span tts:fontStyle="italic">it</span> b</p>"#),
            "a {\\i1}it{\\i0} b"
        );
        assert_eq!(
            to_ass(r#"<p><span tts:fontWeight="bold">bo</span></p>"#),
            "{\\b1}bo{\\b0}"
        );
    }

    #[test]
    fn nested_spans_unwind_in_reverse() {
        assert_eq!(
            to_ass(
                r#"<p><span tts:fontWeight="bold"><span tts:fontStyle="italic">x</span></span></p>"#
            ),
            "{\\b1}{\\i1}x{\\i0}{\\b0}"
        );
    }

    #[test]
    fn colour_uses_the_same_bgr_rule_as_every_other_module() {
        assert_eq!(
            to_ass(r##"<p><span tts:color="#00ff00">g</span></p>"##),
            "{\\c&HFF00&}g{\\c}"
        );
    }

    #[test]
    fn namespace_prefixes_are_ignored() {
        assert_eq!(to_ass("<tt:p>x<tt:br/>y</tt:p>"), "x\\Ny");
    }

    #[test]
    fn xml_entities_are_resolved() {
        assert_eq!(to_ass("<p>a &amp; b &lt;c&gt;</p>"), "a & b <c>");
    }

    #[test]
    fn content_outside_p_is_ignored_when_p_elements_exist() {
        assert_eq!(
            to_ass("<div><metadata>junk</metadata><p>kept</p></div>"),
            "kept"
        );
    }

    #[test]
    fn a_bare_inline_fragment_with_no_p_still_decodes() {
        assert_eq!(
            to_ass(r#"hello <span tts:fontStyle="italic">there</span>"#),
            "hello {\\i1}there{\\i0}"
        );
    }

    #[test]
    fn malformed_xml_does_not_panic() {
        let _ = to_ass("<p>unclosed <span tts:fontStyle=\"italic\">x");
        let _ = to_ass("<<<>>>");
        let _ = to_ass("");
    }
}
