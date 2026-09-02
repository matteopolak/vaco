//! ASS/SSA's own colour grammar: `&H[AA]BBGGRR&` (or the closing `&`
//! omitted, which real files do) — **not** `vaco_core::parse::color`'s
//! `#RRGGBB`/named-colour grammar, and not merely byte-swapped: ASS's alpha
//! byte is *inverted* (`00` opaque, `FF` fully transparent), matching
//! `VSFilter`'s own convention that every ASS renderer, including libass,
//! reproduces. Style-section colours (`PrimaryColour`, ...) and override
//! tags (`\c`, `\1c`-`\4c`, `\alpha`, `\1a`-`\4a`) both use this grammar.

use vaco_core::Rgba;

/// Parse a full colour value: `&H[AA]BBGGRR&` or `&HBBGGRR&`. Returns
/// `None` for anything that is not valid hex of the right length (6 or 8
/// digits after stripping the `&H`/`&` decoration) — including a bare
/// 1-2 digit alpha value, which is [`parse_alpha_only`]'s grammar instead
/// (`\alpha`/`\1a`-`\4a` are a distinct tag from `\c`/`\1c`-`\4c`).
#[must_use]
pub fn parse_color(s: &str) -> Option<Rgba> {
    let inner = s
        .trim()
        .trim_start_matches("&H")
        .trim_start_matches("&h")
        .trim_end_matches('&');
    let hex = inner.trim();
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        1..=6 => {
            let b = (value >> 16) & 0xFF;
            let g = (value >> 8) & 0xFF;
            let r = value & 0xFF;
            Some(Rgba {
                r: r as u8,
                g: g as u8,
                b: b as u8,
                a: 255,
            })
        }
        7..=8 => {
            let aa = (value >> 24) & 0xFF;
            let b = (value >> 16) & 0xFF;
            let g = (value >> 8) & 0xFF;
            let r = value & 0xFF;
            Some(Rgba {
                r: r as u8,
                g: g as u8,
                b: b as u8,
                a: invert_alpha(aa),
            })
        }
        _ => None,
    }
}

/// Overwrite just the alpha channel of `base` from an `\alpha`/`\NNa`-style
/// bare value, leaving RGB untouched.
#[must_use]
pub fn parse_alpha_only(s: &str) -> Option<u8> {
    let inner = s
        .trim()
        .trim_start_matches("&H")
        .trim_start_matches("&h")
        .trim_end_matches('&');
    let value = u32::from_str_radix(inner.trim(), 16).ok()?;
    Some(invert_alpha(value & 0xFF))
}

const fn invert_alpha(ass_alpha: u32) -> u8 {
    (255 - (ass_alpha & 0xFF)) as u8
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn six_digit_is_bgr_opaque() {
        // &H0000FF& is BGR: B=00 G=00 R=FF -> pure red, opaque.
        let c = parse_color("&H0000FF&").unwrap();
        assert_eq!((c.r, c.g, c.b, c.a), (255, 0, 0, 255));
    }

    #[test]
    fn eight_digit_alpha_is_inverted() {
        // &H80FFFFFF&: AA=80 (roughly half-transparent), BGR=FFFFFF white.
        let c = parse_color("&H80FFFFFF&").unwrap();
        assert_eq!((c.r, c.g, c.b), (255, 255, 255));
        assert_eq!(c.a, 255 - 0x80);
    }

    #[test]
    fn fully_transparent_ass_alpha_is_zero() {
        let c = parse_color("&HFF000000&").unwrap();
        assert_eq!(c.a, 0);
    }

    #[test]
    fn fully_opaque_ass_alpha_is_the_default() {
        let c = parse_color("&H00000000&").unwrap();
        assert_eq!(c.a, 255);
    }

    #[test]
    fn alpha_only_tag_inverts_too() {
        assert_eq!(parse_alpha_only("&H80&"), Some(255 - 0x80));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(parse_color("not hex"), None);
    }
}
