//! A minimal `AVColor`-string parser: `#RRGGBB[AA]`/`0xRRGGBB[AA]` hex, a
//! small set of unambiguous named primaries, and an `@alpha` suffix.
//!
//! # Measured (`ffmpeg 8.1`, `drawbox`, `gbrp`, `-bitexact`)
//!
//! `color=0x11223344` on a black `gbrp` background with `t=fill` (opaque
//! blend, see `blend_channel`'s own doc) produced `R=4, G=9, B=13` —
//! matching `floor(0x11 * (0x44/255))`, `floor(0x22 * (0x44/255))`,
//! `floor(0x33 * (0x44/255))` exactly. That pins the hex layout as
//! `RRGGBBAA`, alpha **last** — not `AARRGGBB`, which `drawgraph`'s own
//! `fg1..fg4` *expression* defaults (`"0xffff0000"`) use instead; those are
//! raw 32-bit ARGB pixel values from a different option, not this same
//! `AVColor` grammar, and this module does not parse them.
//!
//! # Not implemented
//!
//! The reference's full named-colour table (~140 X11-derived names,
//! several of which do not match their CSS/web namesakes — `green` is
//! `0,128,0`, not `0,255,0`, so it is deliberately left out rather than
//! guessed). Only primaries whose values are the same under every common
//! convention are named here. `box_source` (drawbox's own "read a
//! rectangle from side data" option) is unrelated to colour and not
//! parsed by this module at all.

/// An opaque-or-blended source colour, resolved to 8-bit RGB plus a
/// `0.0..=1.0` alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f64,
}

const NAMED: &[(&str, (u8, u8, u8))] = &[
    ("black", (0, 0, 0)),
    ("white", (255, 255, 255)),
    ("red", (255, 0, 0)),
    ("blue", (0, 0, 255)),
    ("yellow", (255, 255, 0)),
    ("cyan", (0, 255, 255)),
    ("magenta", (255, 0, 255)),
];

fn hex_pair(s: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(s.get(i..i + 2)?, 16).ok()
}

/// Parse one `AVColor`-shaped string.
///
/// # Errors
/// A plain message naming the unparseable text — this is a filter option,
/// not a place a caller programmatically branches on the failure kind.
pub(crate) fn parse_color(text: &str) -> Result<Rgba, String> {
    let (base, alpha) = match text.split_once('@') {
        Some((b, a)) => {
            let a: f64 = a
                .parse()
                .map_err(|_| format!("bad alpha `{a}` in colour `{text}`"))?;
            (b, a.clamp(0.0, 1.0))
        }
        None => (text, 1.0),
    };
    let hex = base.strip_prefix('#').or_else(|| base.strip_prefix("0x"));
    if let Some(hex) = hex {
        let r = hex_pair(hex, 0).ok_or_else(|| format!("bad hex colour `{text}`"))?;
        let g = hex_pair(hex, 2).ok_or_else(|| format!("bad hex colour `{text}`"))?;
        let b = hex_pair(hex, 4).ok_or_else(|| format!("bad hex colour `{text}`"))?;
        let a = hex_pair(hex, 6).map_or(alpha, |a| f64::from(a) / 255.0);
        return Ok(Rgba { r, g, b, a });
    }
    let lower = base.to_ascii_lowercase();
    NAMED
        .iter()
        .find(|&&(name, _)| name == lower)
        .map(|&(_, (r, g, b))| Rgba { r, g, b, a: alpha })
        .ok_or_else(|| format!("unrecognised colour `{text}` (only hex and a few primaries are implemented — see this module's doc)"))
}

/// Blend one 8-bit channel: `floor(src*(1-a) + color*a)` — measured, not
/// `round`, against three independent alpha values (`0.5`, `0.3`, `0.33`)
/// each landing exactly on the reference's own floored result rather than
/// the nearest-rounded one.
#[must_use]
pub(crate) fn blend_channel(src: u8, color: u8, alpha: f64) -> u8 {
    if alpha >= 1.0 {
        return color;
    }
    if alpha <= 0.0 {
        return src;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the blend result is mathematically within 0..=255 for src/color \
                  in 0..=255 and alpha in 0.0..=1.0; `as` truncates toward zero, \
                  which is the measured floor behaviour itself, not a lossy cast"
    )]
    let out = (f64::from(src) * (1.0 - alpha) + f64::from(color) * alpha) as u8;
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn hex_rrggbbaa_matches_the_reference_probe() {
        let c = parse_color("0x11223344").unwrap();
        assert_eq!((c.r, c.g, c.b), (0x11, 0x22, 0x33));
        assert!((c.a - f64::from(0x44) / 255.0).abs() < 1e-9);
    }

    #[test]
    fn hex_without_alpha_is_fully_opaque() {
        let c = parse_color("#00ff00").unwrap();
        assert_eq!((c.r, c.g, c.b), (0, 255, 0));
        assert!((c.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn named_at_alpha_suffix_overrides_hex_less_opacity() {
        let c = parse_color("white@0.5").unwrap();
        assert_eq!((c.r, c.g, c.b), (255, 255, 255));
        assert!((c.a - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_colour_is_a_clean_error() {
        assert!(parse_color("chartreuse").is_err());
    }

    /// Pinned against the reference probe: `blend_channel(0,255,0.3)==76`
    /// (`255*0.3=76.5`, floors down) and `blend_channel(100,255,0.5)==177`
    /// (`177.5` floors down) — not `77`/`178`, which `round` would give.
    #[test]
    fn blend_floors_rather_than_rounds() {
        assert_eq!(blend_channel(0, 255, 0.3), 76);
        assert_eq!(blend_channel(100, 255, 0.5), 177);
        assert_eq!(blend_channel(10, 255, 0.33), 90);
    }

    #[test]
    fn full_and_zero_alpha_are_exact_without_floating_point() {
        assert_eq!(blend_channel(10, 200, 1.0), 200);
        assert_eq!(blend_channel(10, 200, 0.0), 10);
    }
}
