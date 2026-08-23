//! `ReplayGain`, read from any of its four conventions.
//!
//! `ReplayGain` (<https://wiki.hydrogenaud.io/index.php?title=ReplayGain_specification>)
//! is a loudness-normalisation value pair (gain, peak) at track and album
//! scope, and different container/tag formats each grew their own way of
//! carrying it:
//!
//! | Convention | Carrier | Key/field spelling |
//! |---|---|---|
//! | Vorbis comment | Ogg Vorbis/FLAC/Opus comment header | `REPLAYGAIN_TRACK_GAIN` etc., case-insensitive |
//! | APE tag | this crate's own [`crate::tag::ApeTag`] | same key spelling, case-insensitive (`APEv2` keys always are) |
//! | ID3 `TXXX` | `vaco-format-id3`'s frame table | description `replaygain_track_gain` etc. (measured: `ffmpeg` 8.1 writes it lower-case — see [`from_text_entries`] docs) |
//! | LAME header | the `LAME`/Xing "Info Tag" appended to the first MP3 frame | binary fields, see [`from_lame_header`] |
//!
//! The first three are all "a list of `(key, value)` string pairs with a
//! `"<number> dB"` or bare-float value", so [`from_text_entries`] is the one
//! function all three call into — a container hands it whatever metadata
//! list it already extracted (Vorbis comments, APE items converted to
//! `(key, value)`, or ID3 `TXXX` entries) and gets the same [`ReplayGain`]
//! back regardless of which convention produced it.

/// One scope's gain/peak pair, when present. `gain` is in dB; `peak` is a
/// linear sample-value fraction (nominally `0.0..=1.0`, but not clamped —
/// some encoders report values above `1.0` for genuinely clipping peaks).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReplayGain {
    pub track_gain: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain: Option<f64>,
    pub album_peak: Option<f64>,
}

impl ReplayGain {
    /// Whether any field at all was found.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.track_gain.is_none()
            && self.track_peak.is_none()
            && self.album_gain.is_none()
            && self.album_peak.is_none()
    }

    fn merge_from(&mut self, key: &str, value: &str) {
        let key = key.trim();
        if key.eq_ignore_ascii_case("replaygain_track_gain") {
            self.track_gain = parse_gain(value);
        } else if key.eq_ignore_ascii_case("replaygain_track_peak") {
            self.track_peak = parse_float(value);
        } else if key.eq_ignore_ascii_case("replaygain_album_gain") {
            self.album_gain = parse_gain(value);
        } else if key.eq_ignore_ascii_case("replaygain_album_peak") {
            self.album_peak = parse_float(value);
        }
    }
}

/// Extract [`ReplayGain`] from a generic `(key, value)` metadata list —
/// Vorbis comments, this crate's [`crate::tag::ApeTag`] items converted to
/// text, or `vaco-format-id3`'s `TXXX` entries all use this, because all
/// three spell the four keys identically and differ only in case.
///
/// Measured spelling, `ffmpeg` 8.1: `ffmpeg -i sine.wav -metadata
/// replaygain_track_gain="-3.50 dB" -c:a libmp3lame out.mp3` writes an ID3
/// `TXXX` frame with description `replaygain_track_gain` verbatim (the exact
/// case supplied), and reading it back with `ffprobe -show_entries
/// format_tags` prints the same lower-case key — so this function matches
/// case-insensitively rather than assuming one spelling, which is the safer
/// choice given the four-convention split and costs nothing when a writer
/// is consistent.
#[must_use]
pub fn from_text_entries<'a, I>(entries: I) -> ReplayGain
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut rg = ReplayGain::default();
    for (k, v) in entries {
        rg.merge_from(k, v);
    }
    rg
}

/// `"-3.50 dB"`, `"-3.50dB"`, or a bare `"-3.50"` — all three are seen in the
/// wild — to a gain in dB.
fn parse_gain(s: &str) -> Option<f64> {
    let s = s.trim();
    let numeric = s
        .strip_suffix("dB")
        .or_else(|| s.strip_suffix("DB"))
        .or_else(|| s.strip_suffix("db"))
        .unwrap_or(s)
        .trim();
    parse_float(numeric)
}

fn parse_float(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok()
}

/// The two-byte packed field the LAME/Xing "Info Tag" uses for each of
/// "Radio Replay Gain" and "Audiophile Replay Gain".
///
/// Layout (big-endian 16 bits), per the LAME Tag revision 1 specification
/// (<https://wiki.hydrogenaud.io/index.php?title=LAME_Tag>, the community
/// reference for LAME's own historical documentation of its info tag — a
/// fixed binary layout, D9):
///
/// ```text
/// bits 15-13   name        0 = not set, 1 = radio gain, 2 = audiophile gain
/// bits 12-10   originator  0 = not set, 1 = artist, 2 = user, 3 = automatic, 4 = simple RMS average
/// bit  9       sign        0 = positive/zero, 1 = negative
/// bits 8-0     gain        magnitude in units of 0.1 dB
/// ```
///
/// # Unverified against a live encoder
///
/// This layout is transcribed from the published specification, not
/// recovered by probing an actual LAME/`libmp3lame`-written file: the
/// `ffmpeg` 8.1 build available while writing this crate encodes MP3 via
/// `libmp3lame` but does not populate these fields for any invocation found
/// (LAME only computes and writes them when it runs its own `ReplayGain`
/// analysis pass, which `ffmpeg`'s wrapper does not enable). Per plan 13
/// §1b's rule to say how a table was obtained: this one was not measured,
/// and that is recorded here rather than left implicit.
fn decode_gain_field(raw: u16) -> Option<(GainName, f64)> {
    let name = match raw >> 13 {
        1 => GainName::Radio,
        2 => GainName::Audiophile,
        _ => return None,
    };
    let sign = if (raw >> 9) & 1 == 1 { -1.0 } else { 1.0 };
    let magnitude = f64::from(raw & 0x1ff) / 10.0;
    Some((name, sign * magnitude))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GainName {
    Radio,
    Audiophile,
}

/// Byte offset of the "Peak signal amplitude" field, relative to the start
/// of the 9-byte encoder-version ASCII field that opens the LAME info tag
/// extension (i.e. the caller passes the slice starting at that field).
const PEAK_OFFSET: usize = 11;
/// Byte offset of the Radio Replay Gain field, same base — `PEAK_OFFSET + 4`
/// (the peak field is 32 bits). Not read directly ([`from_lame_header`]
/// reads sequentially past it with [`vaco_bitstream::ByteReader`]); named
/// only for the tests that build a synthetic tag at this layout.
#[cfg(test)]
const RADIO_GAIN_OFFSET: usize = PEAK_OFFSET + 4;
/// Byte offset of the Audiophile Replay Gain field, same base.
#[cfg(test)]
const AUDIOPHILE_GAIN_OFFSET: usize = RADIO_GAIN_OFFSET + 2;

/// Parse `ReplayGain` out of a LAME/Xing "Info Tag" extension.
///
/// `info_tag` must start at the 9-byte encoder short version string (e.g.
/// `b"LAME3.100"`) that opens the extension — the caller is expected to have
/// already located the Xing/Info header inside the first MPEG audio frame
/// and skipped past its own fixed+TOC fields to reach it, which is
/// `vaco-demux`-family territory this crate does not implement (no MP3
/// demuxer exists in this workspace yet to drive it). `radio` gain becomes
/// `track_gain` and `audiophile` becomes `album_gain`, matching the mapping
/// every reader of this tag uses (there is no separate "album" field in the
/// binary layout — LAME predates per-album gain becoming common).
///
/// Returns [`ReplayGain::is_empty`] rather than `None` for a tag with
/// neither field set, since encoder-version bytes alone do not indicate a
/// malformed tag.
///
/// # Unverified
/// See [`decode_gain_field`]'s docs.
#[must_use]
pub fn from_lame_header(info_tag: &[u8]) -> ReplayGain {
    let mut rg = ReplayGain::default();
    // Bytes 0..9 are the encoder version string; not otherwise used here.
    // `ByteReader` zero-fills past the end rather than panicking, so a short
    // `info_tag` decodes to "no gain fields present" instead of an error.
    let mut r = vaco_bitstream::ByteReader::new(info_tag);
    r.skip(PEAK_OFFSET);
    let _peak = r.f32_be();
    let radio = r.be16();
    let audiophile = r.be16();

    if let Some((name, gain)) = decode_gain_field(radio)
        && name == GainName::Radio
    {
        rg.track_gain = Some(gain);
    }
    if let Some((name, gain)) = decode_gain_field(audiophile)
        && name == GainName::Audiophile
    {
        rg.album_gain = Some(gain);
    }
    rg
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::unreadable_literal,
    clippy::decimal_bitwise_operands,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn text_entries_parse_the_four_keys_with_units() {
        let rg = from_text_entries([
            ("replaygain_track_gain", "-3.50 dB"),
            ("replaygain_track_peak", "0.987654"),
            ("REPLAYGAIN_ALBUM_GAIN", "-2.10 dB"),
            ("ReplayGain_Album_Peak", "0.998877"),
        ]);
        assert_eq!(rg.track_gain, Some(-3.50));
        assert_eq!(rg.track_peak, Some(0.987654));
        assert_eq!(rg.album_gain, Some(-2.10));
        assert_eq!(rg.album_peak, Some(0.998877));
    }

    #[test]
    fn a_bare_float_gain_with_no_unit_still_parses() {
        let rg = from_text_entries([("replaygain_track_gain", "1.23")]);
        assert_eq!(rg.track_gain, Some(1.23));
    }

    #[test]
    fn unrelated_keys_are_ignored() {
        let rg = from_text_entries([("title", "Some Song"), ("artist", "Someone")]);
        assert!(rg.is_empty());
    }

    #[test]
    fn garbage_values_do_not_panic_and_leave_the_field_unset() {
        let rg = from_text_entries([("replaygain_track_gain", "not a number")]);
        assert_eq!(rg.track_gain, None);
    }

    #[test]
    fn lame_header_decodes_a_synthetic_positive_and_negative_pair() {
        let mut tag = vec![0u8; 32];
        tag[0..9].copy_from_slice(b"LAME3.100");
        // Radio gain: name=1, originator=3, sign=1 (negative), magnitude=75 (7.5 dB).
        let radio: u16 = (1 << 13) | (3 << 10) | (1 << 9) | 75;
        tag[RADIO_GAIN_OFFSET..RADIO_GAIN_OFFSET + 2].copy_from_slice(&radio.to_be_bytes());
        // Audiophile gain: name=2, sign=0 (positive), magnitude=20 (2.0 dB).
        let audiophile: u16 = (2 << 13) | 20;
        tag[AUDIOPHILE_GAIN_OFFSET..AUDIOPHILE_GAIN_OFFSET + 2]
            .copy_from_slice(&audiophile.to_be_bytes());

        let rg = from_lame_header(&tag);
        assert_eq!(rg.track_gain, Some(-7.5));
        assert_eq!(rg.album_gain, Some(2.0));
    }

    #[test]
    fn a_short_buffer_does_not_panic() {
        let rg = from_lame_header(&[]);
        assert!(rg.is_empty());
        let rg2 = from_lame_header(&[0u8; 10]);
        assert!(rg2.is_empty());
    }

    #[test]
    fn an_unset_gain_field_name_of_zero_is_left_unset() {
        let mut tag = vec![0u8; 32];
        tag[0..9].copy_from_slice(b"LAME3.100");
        // name = 0 ("not set") at both offsets.
        let rg = from_lame_header(&tag);
        assert!(rg.is_empty());
    }
}
