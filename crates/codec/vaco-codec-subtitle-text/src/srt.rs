//! `SubRip` (`subrip`/`srt`) markup to ASS override markup.
//!
//! # Measured, not assumed
//!
//! Every mapping below came from `ffmpeg -bitexact -i fixture.srt -f ass -`
//! on fixtures written for the purpose (D17), because `SubRip` has no formal
//! specification to read — it is a de-facto format, and the reference binary
//! is the only authority on what its decoder does with a given tag:
//!
//! | Input | Output |
//! |---|---|
//! | `<i>x</i>` | `{\i1}x{\i0}` |
//! | `<b>x</b>` | `{\b1}x{\b0}` |
//! | `<u>x</u>` | `{\u1}x{\u0}` |
//! | `<s>x</s>` | `{\s1}x{\s0}` |
//! | `<font color="#00ff00">x</font>` | `{\c&HFF00&}x{\c}` |
//! | `<font size="24" face="Times">x</font>` | `{\fs24}{\fnTimes}x{\fs}{\fn}` |
//! | `<unknown>x</unknown>` | `x` |
//! | `{\an8}x {braces}` | `{\an8}x {braces}` |
//! | line break | `\N` |
//! | `&amp;` | `&amp;` |
//!
//! Three of those are worth stating as rules because they are the ones a
//! from-first-principles implementation gets wrong:
//!
//! - **Entities are not decoded.** `&` and `&amp;` both survive verbatim.
//!   `WebVTT` *does* decode them ([`crate::webvtt`]), so this is a real
//!   per-format difference and not an oversight here.
//! - **`</font>` closes in opening order**, not reversed: `<font size face>`
//!   closes `{\fs}{\fn}`. Nested `<b><i>` *does* close reversed
//!   (`{\i0}{\b0}`) — the difference is that b/i/u/s nest as a stack while a
//!   single `<font>`'s attributes are one tag's worth of state.
//! - **Unmatched tags are not repaired.** `unclosed <i>italic` decodes to
//!   `unclosed {\i1}italic` with no closer appended.

use crate::ass;

/// Which closing override a `<font>` attribute needs, in the order the
/// attributes were opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontAttr {
    Color,
    Size,
    Face,
}

impl FontAttr {
    const fn closer(self) -> &'static str {
        match self {
            Self::Color => "{\\c}",
            Self::Size => "{\\fs}",
            Self::Face => "{\\fn}",
        }
    }
}

/// Convert one `SubRip` cue's text into ASS dialogue text.
#[must_use]
pub fn to_ass(text: &str) -> String {
    let mut out = String::new();
    // One entry per currently-open <font>, holding the attributes it opened.
    let mut fonts: Vec<Vec<FontAttr>> = Vec::new();
    let mut rest = text;

    while let Some(lt) = rest.find('<') {
        let (before, from_lt) = rest.split_at(lt);
        ass::escape_plain(&mut out, before);
        let Some(gt_rel) = from_lt.find('>') else {
            // A '<' with no '>' after it is literal text, not a tag.
            ass::escape_plain(&mut out, from_lt);
            return out;
        };
        let inner = from_lt.get(1..gt_rel).unwrap_or("");
        rest = from_lt.get(gt_rel.saturating_add(1)..).unwrap_or("");
        emit_tag(&mut out, inner, &mut fonts);
    }
    ass::escape_plain(&mut out, rest);
    out
}

fn emit_tag(out: &mut String, inner: &str, fonts: &mut Vec<Vec<FontAttr>>) {
    let trimmed = inner.trim();
    if let Some(name) = trimmed.strip_prefix('/') {
        let name = name.trim().to_ascii_lowercase();
        match name.as_str() {
            "i" => out.push_str("{\\i0}"),
            "b" => out.push_str("{\\b0}"),
            "u" => out.push_str("{\\u0}"),
            "s" => out.push_str("{\\s0}"),
            "font" => {
                if let Some(attrs) = fonts.pop() {
                    for a in attrs {
                        out.push_str(a.closer());
                    }
                }
            }
            _ => {} // unknown closing tag: dropped
        }
        return;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_ascii_lowercase();
    let attrs_src = parts.next().unwrap_or("");
    match name.as_str() {
        "i" => out.push_str("{\\i1}"),
        "b" => out.push_str("{\\b1}"),
        "u" => out.push_str("{\\u1}"),
        "s" => out.push_str("{\\s1}"),
        "font" => {
            let mut opened = Vec::new();
            for (key, value) in attributes(attrs_src) {
                match key.as_str() {
                    "color" => {
                        if let Some((r, g, b)) = ass::parse_color(&value) {
                            ass::push_color(out, r, g, b);
                            opened.push(FontAttr::Color);
                        }
                    }
                    "size" => {
                        if value.chars().all(|c| c.is_ascii_digit()) && !value.is_empty() {
                            out.push_str("{\\fs");
                            out.push_str(&value);
                            out.push('}');
                            opened.push(FontAttr::Size);
                        }
                    }
                    "face" if !value.is_empty() => {
                        out.push_str("{\\fn");
                        out.push_str(&value);
                        out.push('}');
                        opened.push(FontAttr::Face);
                    }
                    _ => {}
                }
            }
            fonts.push(opened);
        }
        _ => {} // unknown opening tag: dropped
    }
}

/// Split an attribute list into lowercased keys and unquoted values, in
/// source order — the order matters, since `</font>` closes in it.
fn attributes(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = src.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest.get(..eq).unwrap_or("").trim().to_ascii_lowercase();
        let after = rest.get(eq.saturating_add(1)..).unwrap_or("").trim_start();
        let (value, tail) = if let Some(q @ ('"' | '\'')) = after.chars().next() {
            let body = after.get(1..).unwrap_or("");
            body.find(q).map_or((body, ""), |end| {
                (
                    body.get(..end).unwrap_or(""),
                    body.get(end.saturating_add(1)..).unwrap_or(""),
                )
            })
        } else {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (
                after.get(..end).unwrap_or(""),
                after.get(end..).unwrap_or(""),
            )
        };
        if !key.is_empty() {
            out.push((key, value.to_owned()));
        }
        rest = tail.trim_start();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_style_tags_match_the_reference() {
        assert_eq!(to_ass("Hello <i>world</i>"), "Hello {\\i1}world{\\i0}");
        assert_eq!(to_ass("<u>u</u> <s>s</s>"), "{\\u1}u{\\u0} {\\s1}s{\\s0}");
    }

    #[test]
    fn nested_tags_close_in_reverse() {
        assert_eq!(to_ass("<b><i>bi</i></b>"), "{\\b1}{\\i1}bi{\\i0}{\\b0}");
    }

    #[test]
    fn font_colour_is_bgr() {
        assert_eq!(
            to_ass("<font color=\"#00ff00\">green</font>"),
            "{\\c&HFF00&}green{\\c}"
        );
    }

    #[test]
    fn font_attributes_close_in_opening_order() {
        assert_eq!(
            to_ass("<font size=\"24\" face=\"Times\">sz</font>"),
            "{\\fs24}{\\fnTimes}sz{\\fs}{\\fn}"
        );
    }

    #[test]
    fn unknown_tags_are_stripped_and_entities_are_not_decoded() {
        assert_eq!(to_ass("<unknown>tag</unknown> & amp"), "tag & amp");
        assert_eq!(to_ass("a &amp; b"), "a &amp; b");
    }

    #[test]
    fn existing_override_tags_and_braces_pass_through() {
        assert_eq!(
            to_ass("{\\an8}already ass {braces}"),
            "{\\an8}already ass {braces}"
        );
    }

    #[test]
    fn an_unclosed_tag_is_not_repaired() {
        assert_eq!(to_ass("unclosed <i>italic"), "unclosed {\\i1}italic");
    }

    #[test]
    fn newlines_become_backslash_n() {
        assert_eq!(to_ass("a\nb"), "a\\Nb");
    }

    #[test]
    fn a_lone_less_than_is_literal() {
        assert_eq!(to_ass("2 < 3"), "2 < 3");
    }
}
