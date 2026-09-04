//! What every muxer in this crate shares: the `#`-comment header block, the
//! per-stream time base this crate assigns, and turning a [`CodecParameters`]
//! into the codec-name spelling the header prints.
//!
//! # Field widths, verbatim from the reference (ffmpeg 8.1, `LC_ALL=C`)
//!
//! `ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 -pix_fmt yuv420p
//! -c:v rawvideo -f framecrc -`, byte-inspected with `od -c` (a plain terminal
//! capture hides the padding spaces):
//!
//! ```text
//! #software: Lavf62.12.100
//! #tb 0: 1/5
//! #media_type 0: video
//! #codec_id 0: rawvideo
//! #dimensions 0: 64x64
//! #sar 0: 1/1
//! 0,          0,          0,        1,     6144, 0xb907b704
//! ```
//!
//! and, for an audio stream in the same run, `#dimensions`/`#sar` are replaced
//! by `#sample_rate 0: 44100` / `#channel_layout_name 0: mono`.
//!
//! `framemd5`/`framehash` print three more header lines and a column header
//! that `framecrc` does not have at all (measured on the same command with
//! `-f framemd5`):
//!
//! ```text
//! #format: frame checksums
//! #version: 2
//! #hash: MD5
//! #software: Lavf62.12.100
//! ⋮
//! #stream#, dts,        pts, duration,     size, hash
//! ```
//!
//! `streamhash` has no header at all — its output is bare `stream,type,ALGO=hex`
//! lines, nothing else.

use core::fmt::Write as _;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Rational, Result, TimeBase};
use vaco_io::IoWriter;

/// This crate's own `#software` identity.
///
/// The reference prints `#software: Lavf<version>` — a *version*, not a bare
/// name — and only **without** `-bitexact` (measured, `ffmpeg 8.1`: the line
/// is absent entirely under `-bitexact`). A value that encodes a library
/// build is exactly what `-bitexact` strips from reproducible output. This
/// crate cannot and should not print `ffmpeg`'s own version string — claiming
/// to be a build of the reference would make the line actively misleading —
/// so it prints its own version instead, in the same `name<version>` shape,
/// suppressed the same way. See `docs/format/vaco-mux-hash.md`.
pub const SOFTWARE_LINE: &str = concat!("vaco", env!("CARGO_PKG_VERSION"));

/// This crate's fallback opinion of a stream's time base, from its
/// [`CodecParameters`] alone — used only when nobody hands it a better one.
///
/// # Why this crate must have a fallback opinion at all
///
/// [`vaco_format_core::Muxer::add_stream`] receives only `CodecParameters` —
/// no time base — and [`vaco_format_core::Muxer::stream_time_base`] is a
/// getter the *caller* queries, never a value the caller hands back. A muxer
/// that answers `None` (§ "keep whatever the caller declared") therefore has
/// no channel of its own to learn what base ended up governing its packets,
/// which is fine for a muxer that never prints the base — `vaco-mux-raw`'s
/// `RawMuxer` is exactly that. This crate prints `#tb` and rescales
/// `Packet::duration` (stored in real microseconds, not stream ticks) back
/// into ticks for display, so it needs a definite answer, not a shrug.
///
/// [`vaco_format_core::Muxer::add_stream_with`] supplies that channel:
/// [`StreamHeader::new`] prefers
/// [`vaco_format_core::StreamSpec::time_base`] when provided —
/// for stream copy, `vaco_format_core::mux::MuxBuilder::add_stream` passes
/// the *input* stream's own base, which is what the reference actually
/// prints (measured: `1/12800` for one MP4 and `1/90000` for one MPEG-TS).
/// This function is only the fallback when no usable spec is supplied.
///
/// The reference's own rawvideo/PCM *encoders* set `time_base` to `1/fps` or
/// `1/sample_rate` before any muxer sees the stream, which is what that
/// fallback recomputes: exactly the case these muxers exist for besides
/// stream copy — dumping freshly encoded raw or PCM media — without this
/// crate pretending to be an encoder.
///
/// `None` when the codec parameters don't say (frame rate or sample rate is
/// zero or absent, or the stream is neither audio nor video). Callers that
/// need a value regardless should use [`display_time_base`], whose fallback
/// ([`vaco_core::Rational::MICROSECONDS`] — the same one
/// `vaco_format_core::mux` itself falls back to) this crate's `Muxer` impls
/// also return from `stream_time_base` verbatim: by construction, whatever
/// this crate *prints* as a stream's `#tb` is also what M1 actually rescaled
/// that stream's packets into, so the two can never quietly disagree.
#[must_use]
pub fn resolve_time_base(params: &CodecParameters) -> Option<TimeBase> {
    match params.effective_media_type() {
        Some(MediaType::Video) => params.video.as_ref().and_then(|v| {
            let fr = v.frame_rate;
            (fr.num > 0 && fr.den > 0).then(|| Rational::new(fr.den, fr.num))
        }),
        Some(MediaType::Audio) => params.audio.as_ref().and_then(|a| {
            let sr = i32::try_from(a.sample_rate).ok()?;
            (sr > 0).then_some(Rational::new(1, sr))
        }),
        _ => None,
    }
}

/// [`resolve_time_base`], with the display-only fallback described there.
#[must_use]
pub fn display_time_base(params: &CodecParameters) -> TimeBase {
    resolve_time_base(params).unwrap_or(Rational::MICROSECONDS)
}

/// A defined, non-zero base, or `None` — the same acceptance test
/// `vaco_format_core::mux::MuxBuilder::add_stream` applies to a muxer's own
/// [`vaco_format_core::Muxer::stream_time_base`] answer, applied here to a
/// supplied [`vaco_format_core::StreamSpec::time_base`] so the two can never
/// disagree about what counts as "no real answer".
fn usable(tb: Option<TimeBase>) -> Option<TimeBase> {
    tb.filter(|tb| tb.is_defined() && !tb.is_zero())
}

/// Per-stream facts the header block needs, gathered once at `add_stream`.
///
/// `params.extradata` doubles as the source for the `#extradata` header
/// line — [`crate::frame::FrameHashMuxer::extradata_lines`] reads it
/// directly rather than this type summarising it, because that line's hash
/// depends on which [`crate::frame::FrameMode`] is active, a fact this
/// module has no reason to know.
#[derive(Debug, Clone)]
pub struct StreamHeader {
    pub params: CodecParameters,
    pub time_base: TimeBase,
}

impl StreamHeader {
    /// `spec_time_base` is [`vaco_format_core::StreamSpec::time_base`],
    /// already screened by the caller — pass it through unconditionally;
    /// `usable` re-screens it anyway, so a caller that skips the check costs
    /// nothing.
    #[must_use]
    pub fn new(params: &CodecParameters, spec_time_base: Option<TimeBase>) -> Self {
        Self {
            time_base: usable(spec_time_base).unwrap_or_else(|| display_time_base(params)),
            params: params.clone(),
        }
    }
}

/// Write the `#`-comment block shared by `framecrc`/`framemd5`/`framehash`.
///
/// Line order, measured (`ffmpeg 8.1`, both with and without `-bitexact`):
/// `extra` first (the three `#format`/`#version`/`#hash` lines for
/// `framemd5`/`framehash`, nothing for `framecrc`), then `extradata_lines`
/// (one `#extradata <n>` per stream that has any, already formatted by the
/// caller — [`crate::frame`] knows the active hash scheme and this function
/// does not), then `#software` (present only when `bitexact` is `false`:
/// the reference suppresses it under `-bitexact` because the value carries a
/// library version — see [`SOFTWARE_LINE`]), then the per-stream `#tb` block.
///
/// # Errors
///
/// Whatever [`IoWriter::write`] returns.
pub fn write_common_header(
    out: &mut IoWriter,
    streams: &[StreamHeader],
    extra: &[String],
    extradata_lines: &[String],
    bitexact: bool,
) -> Result<()> {
    let mut buf = String::new();
    for line in extra {
        let _ = writeln!(buf, "{line}");
    }
    for line in extradata_lines {
        let _ = writeln!(buf, "{line}");
    }
    if !bitexact {
        let _ = writeln!(buf, "#software: {SOFTWARE_LINE}");
    }
    for (i, st) in streams.iter().enumerate() {
        let _ = writeln!(buf, "#tb {i}: {}", st.time_base);
        let media = st.params.effective_media_type();
        if let Some(m) = media {
            let _ = writeln!(buf, "#media_type {i}: {}", m.name());
        }
        if let Some(id) = st.params.codec_id {
            let _ = writeln!(buf, "#codec_id {i}: {}", codec_name(id));
        }
        match media {
            Some(MediaType::Video) => {
                if let Some(v) = &st.params.video {
                    let _ = writeln!(buf, "#dimensions {i}: {}x{}", v.width, v.height);
                    let _ = writeln!(buf, "#sar {i}: {}", v.sample_aspect_ratio);
                }
            }
            Some(MediaType::Audio) => {
                if let Some(a) = &st.params.audio {
                    let _ = writeln!(buf, "#sample_rate {i}: {}", a.sample_rate);
                    if let Some(layout) = &a.layout {
                        let _ = writeln!(buf, "#channel_layout_name {i}: {layout}");
                    }
                }
            }
            _ => {}
        }
    }
    out.write(buf.as_bytes())
}

/// `CodecId`'s reference-style name (`h264`, `pcm_s16le`, `rawvideo`, …).
///
/// A generic PascalCase-to-`snake_case` conversion, since `CodecId`'s variant
/// names were themselves chosen to mirror the reference's own spelling (see
/// `vaco_codec_core`'s crate docs). Measured to hold for every variant checked
/// against a real probe except one: `SubRip` prints as `subrip`, with no
/// underscore, so it is special-cased. A variant this crate has not checked
/// against the reference falls through the generic rule, which is right far
/// more often than it is wrong (`Mpeg1video` → `mpeg1video`, `AacLatm` →
/// `aac_latm`, `AmrNb` → `amr_nb`, `AdpcmImaWav` → `adpcm_ima_wav` all match)
/// but is not a substitute for probing a specific codec this crate has not
/// exercised yet.
#[must_use]
pub fn codec_name(id: CodecId) -> String {
    if matches!(id, CodecId::SubRip) {
        return "subrip".to_owned();
    }
    let debug = format!("{id:?}");
    let mut out = String::with_capacity(debug.len() + 4);
    for (i, ch) in debug.chars().enumerate() {
        if i > 0 && ch.is_ascii_uppercase() {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn codec_name_matches_measured_spellings() {
        assert_eq!(codec_name(CodecId::H264), "h264");
        assert_eq!(codec_name(CodecId::Rawvideo), "rawvideo");
        assert_eq!(codec_name(CodecId::PcmS16le), "pcm_s16le");
        assert_eq!(codec_name(CodecId::AacLatm), "aac_latm");
        assert_eq!(codec_name(CodecId::SubRip), "subrip");
        assert_eq!(codec_name(CodecId::MovText), "mov_text");
        assert_eq!(codec_name(CodecId::Mpeg1video), "mpeg1video");
        assert_eq!(codec_name(CodecId::AmrNb), "amr_nb");
    }

    #[test]
    fn time_base_follows_frame_rate_and_sample_rate() {
        let mut p = CodecParameters::video();
        p.video.as_mut().unwrap().frame_rate = Rational::new(5, 1);
        assert_eq!(resolve_time_base(&p), Some(Rational::new(1, 5)));

        let mut p = CodecParameters::audio();
        p.audio.as_mut().unwrap().sample_rate = 44_100;
        assert_eq!(resolve_time_base(&p), Some(Rational::new(1, 44_100)));
    }

    #[test]
    fn an_unknown_rate_falls_back_to_microseconds_for_display_only() {
        let p = CodecParameters::video();
        assert_eq!(resolve_time_base(&p), None);
        assert_eq!(display_time_base(&p), Rational::MICROSECONDS);
    }
}
