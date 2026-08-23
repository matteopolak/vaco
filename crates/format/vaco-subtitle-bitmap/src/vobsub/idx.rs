//! The `VobSub` `.idx` grammar: plain text, and entirely container work.
//!
//! A `.idx` file states, per line:
//!
//! * `size: WxH` — the subtitle canvas size.
//! * `palette: rrggbb, rrggbb, …` — up to 16 (in practice; this parser
//!   accepts up to [`vaco_format_subtitle_bitmap::Palette::MAX_ENTRIES`])
//!   hex RGB triples, no alpha.
//! * `id: <lang>, index: <n>` — starts a new track.
//! * `timestamp: HH:MM:SS:mmm, filepos: <hex>` — one cue in the current
//!   track: a presentation time and a byte offset into the sibling `.sub`.
//!
//! None of this touches the `.sub` file's MPEG-PS/RLE payload — it is plain
//! ASCII, the exact shape `planning/AGENT-CONSTRAINTS.md`'s "vobsub" example
//! calls "container work".

#![allow(
    clippy::integer_division,
    reason = "every division here is exact by construction against fixed bases (60, 1000, 3_600_000) this module's own callers chose, mirroring vaco-format-subtitle::time's identical allowance"
)]

use vaco_core::Duration;
use vaco_format_subtitle_bitmap::{Palette, Rgba};
use vaco_limits::Limits;

/// One cue: a presentation time and the `.sub` byte offset it starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdxEntry {
    pub time: Duration,
    pub filepos: u64,
}

/// One language track.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdxTrack {
    pub lang: Option<String>,
    /// The sub-stream index (`0`-based); the `.sub` file's `private_stream_1`
    /// sub-id for this track is `0x20 + index`.
    pub index: u8,
    pub entries: Vec<IdxEntry>,
}

/// A fully parsed `.idx` file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdxFile {
    pub size: Option<(u32, u32)>,
    pub palette: Option<Palette>,
    pub tracks: Vec<IdxTrack>,
}

/// Parse `text` as a `.idx` file. Never fails: an unrecognised or malformed
/// line is skipped, matching this crate's demuxers-are-lenient convention —
/// a `.idx` with one bad `palette:` line still yields every track's
/// timestamps.
#[must_use]
pub fn parse(text: &str) -> IdxFile {
    let mut file = IdxFile::default();
    let mut current: Option<IdxTrack> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("size:") {
            file.size = parse_size(rest.trim());
        } else if let Some(rest) = line.strip_prefix("palette:") {
            file.palette = parse_palette(rest.trim(), &Limits::permissive());
        } else if let Some(rest) = line.strip_prefix("id:") {
            if let Some(t) = current.take() {
                file.tracks.push(t);
            }
            let (lang, index) = parse_id_line(rest);
            current = Some(IdxTrack {
                lang,
                index: index.unwrap_or(0),
                entries: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("timestamp:")
            && let (Some(entry), Some(track)) = (parse_timestamp_line(rest), current.as_mut())
        {
            track.entries.push(entry);
        }
    }
    if let Some(t) = current.take() {
        file.tracks.push(t);
    }
    file
}

/// `en, index: 3` -> `(Some("en"), Some(3))`.
fn parse_id_line(rest: &str) -> (Option<String>, Option<u8>) {
    let mut lang = None;
    let mut index = None;
    for (i, part) in rest.split(',').enumerate() {
        let part = part.trim();
        if i == 0 && !part.is_empty() {
            lang = Some(part.to_string());
        } else if let Some(n) = part.strip_prefix("index:") {
            index = n.trim().parse::<u8>().ok();
        }
    }
    (lang, index)
}

/// `00:00:01:234, filepos: 000000000`.
fn parse_timestamp_line(rest: &str) -> Option<IdxEntry> {
    let (ts_part, pos_part) = rest.split_once(',')?;
    let time = parse_timestamp(ts_part.trim())?;
    let hex = pos_part.trim().strip_prefix("filepos:")?.trim();
    let filepos = parse_filepos(hex)?;
    Some(IdxEntry { time, filepos })
}

/// `HH:MM:SS:mmm` — hours unbounded, minutes/seconds `0..=59`, milliseconds
/// `0..=999`. Colon-separated throughout, unlike every other timestamp
/// grammar in this workspace's subtitle formats, which is exactly the kind
/// of format-specific punctuation `vaco-format-subtitle`'s docs warn a parser
/// must not guess at.
#[must_use]
pub fn parse_timestamp(s: &str) -> Option<Duration> {
    let mut parts = s.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let sec: i64 = parts.next()?.parse().ok()?;
    let ms: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some()
        || !(0..60).contains(&m)
        || !(0..60).contains(&sec)
        || !(0..1000).contains(&ms)
        || h < 0
    {
        return None;
    }
    let total_ms = h
        .checked_mul(3_600_000)?
        .checked_add(m.checked_mul(60_000)?)?
        .checked_add(sec.checked_mul(1000)?)?
        .checked_add(ms)?;
    Some(Duration::from_micros(total_ms.checked_mul(1000)?))
}

/// The inverse of [`parse_timestamp`]: `HH:MM:SS:mmm`, zero-padded, hours
/// unbounded in width (a `.idx` can legitimately run past 99 hours).
#[must_use]
pub fn format_timestamp(d: Duration) -> String {
    let total_ms = d.as_micros().max(0) / 1000;
    let ms = total_ms % 1000;
    let total_sec = total_ms / 1000;
    let sec = total_sec % 60;
    let total_min = total_sec / 60;
    let min = total_min % 60;
    let hours = total_min / 60;
    format!("{hours:02}:{min:02}:{sec:02}:{ms:03}")
}

/// A bare hex byte offset, e.g. `000123AB`. No `0x` prefix, per every real
/// `.idx` this format's tooling produces.
#[must_use]
pub fn parse_filepos(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

/// `720x480` -> `(720, 480)`.
#[must_use]
pub fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// `000000, 828282, …` -> a [`Palette`] of opaque RGB colours (a `.idx`
/// palette states no alpha).
#[must_use]
pub fn parse_palette(s: &str, limits: &Limits) -> Option<Palette> {
    let _ = limits;
    let mut entries = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.len() != 6 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let r = u8::from_str_radix(part.get(0..2)?, 16).ok()?;
        let g = u8::from_str_radix(part.get(2..4)?, 16).ok()?;
        let b = u8::from_str_radix(part.get(4..6)?, 16).ok()?;
        entries.push(Rgba::new(r, g, b, 0xFF));
        if entries.len() > Palette::MAX_ENTRIES {
            return None;
        }
    }
    Palette::new(entries).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# VobSub index file, v7\n\
size: 720x480\n\
palette: 000000, ffffff, ff0000\n\
\n\
id: en, index: 0\n\
timestamp: 00:00:01:234, filepos: 000000000\n\
timestamp: 00:00:03:456, filepos: 0000004ab\n\
\n\
id: fr, index: 1\n\
timestamp: 00:00:02:000, filepos: 000000200\n\
";

    #[test]
    fn parses_size_palette_and_both_tracks() {
        let f = parse(SAMPLE);
        assert_eq!(f.size, Some((720, 480)));
        assert_eq!(f.palette.as_ref().unwrap().len(), 3);
        assert_eq!(f.tracks.len(), 2);
        assert_eq!(f.tracks[0].lang.as_deref(), Some("en"));
        assert_eq!(f.tracks[0].index, 0);
        assert_eq!(f.tracks[0].entries.len(), 2);
        assert_eq!(f.tracks[1].lang.as_deref(), Some("fr"));
        assert_eq!(f.tracks[1].index, 1);
        assert_eq!(f.tracks[1].entries.len(), 1);
    }

    #[test]
    fn timestamp_parses_hours_minutes_seconds_milliseconds() {
        let d = parse_timestamp("01:02:03:456").unwrap();
        let expected = ((3_600 + 2 * 60 + 3) * 1000 + 456) * 1000;
        assert_eq!(d, Duration::from_micros(expected));
    }

    #[test]
    fn timestamp_rejects_out_of_range_fields() {
        assert!(parse_timestamp("00:60:00:000").is_none());
        assert!(parse_timestamp("00:00:60:000").is_none());
        assert!(parse_timestamp("00:00:00:1000").is_none());
        assert!(parse_timestamp("not:a:time:stamp").is_none());
    }

    #[test]
    fn format_timestamp_is_the_inverse_for_known_values() {
        assert_eq!(
            format_timestamp(Duration::from_micros(1_234_000)),
            "00:00:01:234"
        );
    }

    #[test]
    fn filepos_rejects_non_hex() {
        assert_eq!(parse_filepos("00zz"), None);
        assert_eq!(parse_filepos(""), None);
    }

    #[test]
    fn size_accepts_uppercase_x() {
        assert_eq!(parse_size("640X480"), Some((640, 480)));
    }

    #[test]
    fn palette_rejects_a_too_short_entry() {
        assert_eq!(parse_palette("abc", &Limits::permissive()), None);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// [`format_timestamp`] / [`parse_timestamp`] round-trips every
        /// representable `HH:MM:SS:mmm` value — the `.idx` timestamp grammar
        /// and its inverse the brief calls out explicitly.
        #[test]
        fn timestamp_round_trips(
            h in 0u32..1000,
            m in 0u32..60,
            s in 0u32..60,
            ms in 0u32..1000,
        ) {
            let text = format!("{h:02}:{m:02}:{s:02}:{ms:03}");
            let parsed = parse_timestamp(&text);
            prop_assert!(parsed.is_some());
            let formatted = format_timestamp(parsed.unwrap());
            prop_assert_eq!(formatted, text);
        }

        /// [`parse_filepos`] round-trips any `u32` printed as 8 lowercase hex
        /// digits — the shape every real `.idx` uses.
        #[test]
        fn filepos_round_trips(v in any::<u32>()) {
            let text = format!("{v:08x}");
            prop_assert_eq!(parse_filepos(&text), Some(u64::from(v)));
        }
    }
}
