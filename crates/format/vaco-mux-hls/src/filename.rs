//! `-hls_segment_filename`'s `%d`/`%0Nd` sequence-number template, and the
//! default naming scheme when no template is given.
//!
//! Measured against `ffmpeg -h muxer=hls`: the option is a plain string with
//! a single `printf`-style integer conversion, not a full-fledged
//! `strftime`/format-string engine — `-strftime` is a **separate** boolean
//! option this crate does not implement (see the docs file).

/// Expand `template`'s first `%d`/`%0Nd` conversion with `index`, left-padded
/// to `N` digits when a width was given. A template with no conversion is
/// returned unchanged — every segment would then share one name, which is a
/// user configuration error this crate does not second-guess.
#[must_use]
pub fn expand(template: &str, index: u64) -> String {
    let Some(pct) = template.find('%') else {
        return template.to_owned();
    };
    let Some(after) = template.get(pct + 1..) else {
        return template.to_owned();
    };
    let width_len = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    let Some(width_str) = after.get(..width_len) else {
        return template.to_owned();
    };
    let Some(rest) = after.get(width_len..) else {
        return template.to_owned();
    };
    let Some(conv) = rest.chars().next() else {
        return template.to_owned();
    };
    if conv != 'd' {
        return template.to_owned();
    }
    let Some(tail) = rest.get(conv.len_utf8()..) else {
        return template.to_owned();
    };
    let number = if width_str.is_empty() {
        index.to_string()
    } else {
        let width: usize = width_str.parse().unwrap_or(0);
        format!("{index:0width$}")
    };
    let Some(head) = template.get(..pct) else {
        return template.to_owned();
    };
    format!("{head}{number}{tail}")
}

/// The default name for segment `index` when `-hls_segment_filename` was not
/// given: `<playlist-stem><index>.<ext>`, with **no** directory component —
/// this is the literal text written into the playlist, always resolved
/// against the playlist's own directory by the reader, the same way the
/// reference emits a bare `stream0.ts` rather than repeating the output
/// path on every line.
#[must_use]
pub fn default_name(playlist_path: &str, index: u64, extension: &str) -> String {
    let stem = match playlist_path.rfind('/') {
        Some(i) => playlist_path.get(i + 1..).unwrap_or(playlist_path),
        None => playlist_path,
    };
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    format!("{stem}{index}.{extension}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn expands_a_plain_percent_d() {
        assert_eq!(expand("seg_%d.ts", 7), "seg_7.ts");
    }

    #[test]
    fn expands_a_zero_padded_width() {
        assert_eq!(expand("seg_%05d.ts", 7), "seg_00007.ts");
        assert_eq!(expand("seg_%03d.ts", 1234), "seg_1234.ts");
    }

    #[test]
    fn a_template_with_no_conversion_is_unchanged() {
        assert_eq!(expand("seg.ts", 3), "seg.ts");
    }

    #[test]
    fn default_naming_derives_from_the_playlist_stem_with_no_directory() {
        assert_eq!(default_name("/tmp/out/stream.m3u8", 3, "ts"), "stream3.ts");
        assert_eq!(default_name("stream.m3u8", 0, "m4s"), "stream0.m4s");
    }
}
