//! ASS/SSA: the dialogue-chunk wire shape, and the override-tag markup every
//! other decoder in this crate emits into.
//!
//! # Why every decoder in this crate produces ASS
//!
//! Measured against the reference (`ffmpeg -bitexact -i in.srt -f ass -`, and
//! the same for `.vtt` and an `mov_text` MP4): the `subrip`, `webvtt`,
//! `mov_text` and `text` decoders do **not** produce plain strings. Each one
//! emits an ASS `Dialogue:` line whose text field carries ASS override tags —
//! `<i>x</i>` comes back as `{\i1}x{\i0}`, a line break as `\N`. That is one
//! decoder family with one output language, and reproducing it is what makes
//! this crate's output comparable to the reference's at all.
//!
//! So [`escape_plain`] and the `to_ass` function in each sibling module are
//! the whole contract: markup in, ASS override markup out.
//!
//! # Two different things are called "an ASS packet"
//!
//! The reference's ASS *demuxer* hands its decoder a nine-field chunk with the
//! timestamps stripped out (they live on the packet instead) — measured with
//! `ffmpeg -i in.ass -c:s copy -f data -`:
//!
//! ```text
//! ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text
//! 0,0,Default,Speaker,5,6,7,fx,{\i1}hi{\i0} there, with, commas
//! ```
//!
//! This workspace's own `vaco-subtitle-text` ASS demuxer instead puts **only
//! the `Text` field** in the packet (`ass.rs`'s `parts.last()`). Both are
//! reachable here, so this module offers [`parse_chunk`] for the first shape
//! and leaves the second to [`crate::text`], rather than sniffing between them
//! and guessing wrong on a `Text` field that happens to contain eight commas.

/// The nine fields the reference's ASS demuxer packs into one packet, in
/// order. `Text` is last and may itself contain commas, so a chunk is split
/// at most eight times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialogue<'a> {
    pub read_order: u32,
    pub layer: i32,
    pub style: &'a str,
    pub name: &'a str,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    pub effect: &'a str,
    /// The dialogue text, override tags and all, exactly as transmitted.
    pub text: &'a str,
}

/// Parse one nine-field ASS dialogue chunk as the reference's ASS demuxer
/// emits it.
///
/// Returns `None` when `chunk` does not have nine comma-separated fields, or
/// when a numeric field does not parse — a caller holding a bare `Text` field
/// (this workspace's own demuxer's shape) gets `None` here rather than a
/// `Dialogue` built out of the first eight commas of its prose.
#[must_use]
pub fn parse_chunk(chunk: &str) -> Option<Dialogue<'_>> {
    let mut it = chunk.splitn(9, ',');
    let read_order = it.next()?.trim().parse().ok()?;
    let layer = it.next()?.trim().parse().ok()?;
    let style = it.next()?;
    let name = it.next()?;
    let margin_l = it.next()?.trim().parse().ok()?;
    let margin_r = it.next()?.trim().parse().ok()?;
    let margin_v = it.next()?.trim().parse().ok()?;
    let effect = it.next()?;
    let text = it.next()?;
    Some(Dialogue {
        read_order,
        layer,
        style,
        name,
        margin_l,
        margin_r,
        margin_v,
        effect,
        text,
    })
}

/// Append `text` to `out` as ASS dialogue text, converting line breaks to
/// `\N` and leaving everything else byte-for-byte.
///
/// Deliberately does **not** escape `{`/`}`. Measured: a `.srt` cue reading
/// `{\an8}already ass {braces}` decodes to exactly `{\an8}already ass
/// {braces}` — the reference passes braces straight through, so a cue that
/// already contains override tags keeps working and one that contains a
/// literal brace is not rescued. Escaping here would be more defensive and
/// would not match.
pub fn escape_plain(out: &mut String, text: &str) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CRLF is one break, not two.
                if chars.peek() == Some(&'\n') {
                    let _ = chars.next();
                }
                out.push_str("\\N");
            }
            '\n' => out.push_str("\\N"),
            other => out.push(other),
        }
    }
}

/// An ASS colour override for a 24-bit RGB value: `{\c&HBBGGRR&}`.
///
/// ASS orders the components blue-green-red, the reverse of the `#RRGGBB`
/// an HTML-ish `<font color=…>` states, and the reference prints the result
/// with leading zeroes stripped — measured: `<font color="#00ff00">` decodes
/// to `{\c&HFF00&}`, not `{\c&H00FF00&}`.
pub fn push_color(out: &mut String, r: u8, g: u8, b: u8) {
    let bgr = (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r);
    out.push_str("{\\c&H");
    if bgr == 0 {
        out.push('0');
    } else {
        let mut started = false;
        for shift in (0..6).rev() {
            let nib = (bgr >> (shift * 4)) & 0xF;
            if nib != 0 {
                started = true;
            }
            if started {
                out.push(char::from_digit(nib, 16).unwrap_or('0').to_ascii_uppercase());
            }
        }
    }
    out.push_str("&}");
}

/// Parse an HTML-ish colour attribute value into RGB.
///
/// Accepts `#RRGGBB`, bare `RRGGBB`, and the sixteen HTML 4 named colours the
/// reference's own colour table covers. Returns `None` for anything else, so
/// an unrecognised colour drops the tag rather than colouring the text wrong.
#[must_use]
pub fn parse_color(value: &str) -> Option<(u8, u8, u8)> {
    let v = value.trim();
    let hex = v.strip_prefix('#').unwrap_or(v);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
        let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
        let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()?;
        return Some((r, g, b));
    }
    let named: &[(&str, (u8, u8, u8))] = &[
        ("black", (0x00, 0x00, 0x00)),
        ("silver", (0xC0, 0xC0, 0xC0)),
        ("gray", (0x80, 0x80, 0x80)),
        ("grey", (0x80, 0x80, 0x80)),
        ("white", (0xFF, 0xFF, 0xFF)),
        ("maroon", (0x80, 0x00, 0x00)),
        ("red", (0xFF, 0x00, 0x00)),
        ("purple", (0x80, 0x00, 0x80)),
        ("fuchsia", (0xFF, 0x00, 0xFF)),
        ("magenta", (0xFF, 0x00, 0xFF)),
        ("green", (0x00, 0x80, 0x00)),
        ("lime", (0x00, 0xFF, 0x00)),
        ("olive", (0x80, 0x80, 0x00)),
        ("yellow", (0xFF, 0xFF, 0x00)),
        ("navy", (0x00, 0x00, 0x80)),
        ("blue", (0x00, 0x00, 0xFF)),
        ("teal", (0x00, 0x80, 0x80)),
        ("aqua", (0x00, 0xFF, 0xFF)),
        ("cyan", (0x00, 0xFF, 0xFF)),
    ];
    let lower = v.to_ascii_lowercase();
    named
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, rgb)| *rgb)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_a_reference_shaped_dialogue_chunk() {
        let d = parse_chunk("0,0,Default,Speaker,5,6,7,fx,{\\i1}hi{\\i0} there, with, commas")
            .unwrap();
        assert_eq!(d.read_order, 0);
        assert_eq!(d.style, "Default");
        assert_eq!(d.name, "Speaker");
        assert_eq!((d.margin_l, d.margin_r, d.margin_v), (5, 6, 7));
        assert_eq!(d.effect, "fx");
        // The text keeps every comma past the eighth split.
        assert_eq!(d.text, "{\\i1}hi{\\i0} there, with, commas");
    }

    #[test]
    fn a_bare_text_field_is_not_mistaken_for_a_chunk() {
        assert!(parse_chunk("just some prose").is_none());
        // Eight commas but non-numeric leading fields: still not a chunk.
        assert!(parse_chunk("a,b,c,d,e,f,g,h,i").is_none());
    }

    #[test]
    fn newlines_become_backslash_n() {
        let mut s = String::new();
        escape_plain(&mut s, "a\nb\r\nc\rd");
        assert_eq!(s, "a\\Nb\\Nc\\Nd");
    }

    #[test]
    fn braces_are_passed_through_unescaped() {
        let mut s = String::new();
        escape_plain(&mut s, "{\\an8}already ass {braces}");
        assert_eq!(s, "{\\an8}already ass {braces}");
    }

    #[test]
    fn colour_is_bgr_with_leading_zeroes_stripped() {
        let mut s = String::new();
        push_color(&mut s, 0x00, 0xFF, 0x00);
        assert_eq!(s, "{\\c&HFF00&}");

        let mut s = String::new();
        push_color(&mut s, 0xFF, 0xFF, 0xFF);
        assert_eq!(s, "{\\c&HFFFFFF&}");

        let mut s = String::new();
        push_color(&mut s, 0x00, 0x00, 0x00);
        assert_eq!(s, "{\\c&H0&}");

        // Red 0xFF0000 -> BGR 0x0000FF -> "FF".
        let mut s = String::new();
        push_color(&mut s, 0xFF, 0x00, 0x00);
        assert_eq!(s, "{\\c&HFF&}");
    }

    #[test]
    fn colour_attribute_parses_hex_and_names() {
        assert_eq!(parse_color("#00ff00"), Some((0x00, 0xFF, 0x00)));
        assert_eq!(parse_color("00FF00"), Some((0x00, 0xFF, 0x00)));
        assert_eq!(parse_color("Red"), Some((0xFF, 0x00, 0x00)));
        assert_eq!(parse_color("not-a-colour"), None);
        assert_eq!(parse_color("#12345"), None);
    }
}
