//! `-h`'s three depths and its `-h <kind>=<name>` form: the wiring between
//! `vaco-cli-core`'s renderers (which know how to lay out a line, not what
//! this build contains) and `vaco-registry` (which knows what this build
//! contains, not how to lay out a line). See `vaco_cli_core::help` for the
//! measured column algorithm and blank-line rules this module assembles.
//!
//! # The kind matrix, measured (`ffmpeg 8.1`, `LC_ALL=C`, no pipe)
//!
//! | kind | no name | unknown name | notes |
//! |---|---|---|---|
//! | `decoder`/`encoder` | `No codec name specified.` | `Codec 'x' is not recognized by FFmpeg.` | we say `Vaco` (D9: not claiming to be the reference) |
//! | `demuxer`/`muxer` | `Unknown format '(null)'.` | `Unknown format 'x'.` | share one message: both are `AVFormatContext` lookups in the reference |
//! | `filter` | `No filter name specified.` | `Unknown filter 'x'.` | |
//! | `bsf` | `No bitstream filter name specified.` | `Unknown bit stream filter 'x'.` | |
//! | `protocol` | `No protocol name specified.` | `Unknown protocol 'x'.` | the *found* case prints no header at all, straight into the `AVOptions` block (or nothing, if the protocol has none) |
//!
//! `-h demuxer` (no `=`) and `-h demuxer=` (`=` then nothing) are *different*
//! no-name cases distinguished by [`vaco_cli_core::help::KindTopic::name`]:
//! the first is the reference's C `NULL` format argument, printed literally
//! as `(null)`; the second is a real, empty string.
//!
//! # Found cases this build cannot reach today, implemented anyway
//!
//! `vaco-codec-core::DecoderDesc` and `vaco-filter-core::FilterDesc` carry no
//! options-schema hook at all — unlike
//! [`vaco_protocol_core::ProtocolDesc::options`], there is nothing to call
//! even if a name matched. Combined with `DECODERS`/`FILTERS` being
//! unconditionally empty in this build (no decoder or filter crate exists
//! yet), the "found" branches for `decoder`/`encoder`/`filter`/`bsf` below are
//! unreachable in practice. They are still written as real lookups rather
//! than hard-coded failures, so the moment a component lands the path lights
//! up instead of silently staying wrong — but their header-only rendering is
//! a best guess, not a measurement, because nothing exists to measure against.
//! Reported in `docs/app/vaco-cli.md` as a gap in the two other crates.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{self, Write};

use vaco_cli_core::help::{HelpLevel, KindTopic, Topic, ends_in_options_block, parse_topic};
use vaco_cli_core::{render_options_help, render_schema_block, table::ffmpeg};
use vaco_format_core::options::FormatOptions;
use vaco_opts::schema_of;
use vaco_registry::Kind;

/// Render the complete `-h` output for the topic that followed `-h` (or one
/// of its `-?`/`-help`/`--help` spellings) on the command line, to `w`.
///
/// Always succeeds — every outcome here, a known depth, an unknown topic, or
/// an unrecognised component name, is success in the reference (measured:
/// `ffmpeg -h zzzz=x` and `ffmpeg -h full` both exit 0). The one error this
/// function can return is a write failure on `w` itself.
///
/// # Errors
/// The sink's I/O error.
pub fn render<W: Write>(w: &mut W, topic_raw: Option<&OsStr>) -> io::Result<()> {
    // Lossy on non-UTF-8: the reference reads `argv` as bytes and would
    // presumably echo them back verbatim in `Unknown help option '…'.`, but
    // every layer below this one takes `&str` (the same limitation `cli.rs`
    // already documents for URLs), so a non-UTF-8 topic degrades to the
    // replacement character rather than panicking or being rejected outright.
    let topic_str = topic_raw.map(|s| s.to_string_lossy());
    let body = match parse_topic(topic_str.as_deref()) {
        Topic::Level(level) => render_level(level),
        Topic::Kind(kt) => render_kind(&kt),
        Topic::Unrecognized(t) => {
            format!(
                "Unknown help option '{t}'.\n{}",
                render_level(HelpLevel::Basic)
            )
        }
    };
    finish(w, &body)
}

/// The command-line section, plus (for [`HelpLevel::Full`]) every
/// `AVOptions` schema this build can reach without opening a file: the
/// generic format-level options every demuxer and muxer shares, and every
/// registered protocol's own.
///
/// What real `ffmpeg -h full` additionally has that this cannot: a private
/// options class per demuxer/muxer/decoder/encoder/filter/bsf. None of our
/// three demuxers (`vaco-demux-matroska`/`-mp4`/`-mpegts`) declare one today
/// — `vaco_format_core::DemuxerDesc` has no schema hook at all, so there is
/// nothing to walk even if they did. Reported in `docs/app/vaco-cli.md`.
fn render_level(level: HelpLevel) -> String {
    let mut body = render_options_help(&ffmpeg(), level);
    if level == HelpLevel::Full {
        let mut blocks = vec![render_schema_block(schema_of::<FormatOptions>())];
        for p in vaco_registry::protocols() {
            if let Some(schema_fn) = p.options {
                blocks.push(render_schema_block(schema_fn()));
            }
        }
        if !body.is_empty() {
            body.push('\n');
        }
        body.push('\n');
        body.push_str(&blocks.join("\n"));
    }
    body
}

fn render_kind(kt: &KindTopic) -> String {
    match kt.kind.as_str() {
        "decoder" | "encoder" => codec_kind(kt.name.as_deref()),
        "demuxer" => format_kind(kt.name.as_deref(), false),
        "muxer" => format_kind(kt.name.as_deref(), true),
        "filter" => named_lookup(
            kt.name.as_deref(),
            "No filter name specified.",
            "Unknown filter",
            |name| {
                vaco_registry::filter_by_name(name)
                    .map(|f| format!("Filter {name} [{}]:\n", f.description))
            },
        ),
        "bsf" => named_lookup(
            kt.name.as_deref(),
            "No bitstream filter name specified.",
            "Unknown bit stream filter",
            |name| {
                vaco_registry::component(Kind::BitstreamFilter, name).map(|c| {
                    format!(
                        "Bit stream filter {name} [{}]:\n",
                        c.long_name.unwrap_or(name)
                    )
                })
            },
        ),
        "protocol" => named_lookup(
            kt.name.as_deref(),
            "No protocol name specified.",
            "Unknown protocol",
            |name| {
                vaco_registry::protocols()
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| {
                        p.options
                            .map_or_else(String::new, |f| render_schema_block(f()))
                    })
            },
        ),
        // An unrecognised `kind` reuses the "unrecognised topic" wording,
        // using only the kind text (D17: `-h zzzz=x` reports 'zzzz', not
        // 'zzzz=x' — measured).
        other => format!(
            "Unknown help option '{other}'.\n{}",
            render_level(HelpLevel::Basic)
        ),
    }
}

/// `decoder`/`encoder`: this build has neither (D5 — v0.1 is parse-only), and
/// `vaco_codec_core::DecoderDesc` has no options-schema hook regardless, so
/// the "found" branch is header-only and untested against a real component.
fn codec_kind(name: Option<&str>) -> String {
    let Some(name) = name else {
        return "No codec name specified.".to_owned();
    };
    if let Some(d) = vaco_registry::decoder_by_name(name) {
        return format!("Decoder {name} [{}]:\n", d.long_name);
    }
    if let Some(c) = vaco_registry::component(Kind::Encoder, name) {
        return format!("Encoder {name} [{}]:\n", c.long_name.unwrap_or(name));
    }
    // Substituting Vaco for the reference's own name: D9 puts claiming to
    // *be* FFmpeg outside what this project reproduces, the same reason
    // `listing::banner` prints our own identity rather than the reference's.
    // It is also the more accurate statement in our own binary's mouth.
    format!("Codec '{name}' is not recognized by Vaco.")
}

/// `demuxer`/`muxer`: both are `AVFormatContext` lookups in the reference and
/// share one message shape and one "no name" literal (`(null)`, the C
/// argument's own `NULL`, printed by `%s` — a fact about the reference's
/// implementation, not something a Rust `Option` can be asked to reproduce
/// from any option *we* hold, so it is spelled out here as the literal it is).
fn format_kind(name: Option<&str>, is_muxer: bool) -> String {
    let Some(name) = name else {
        return "Unknown format '(null)'.".to_owned();
    };
    if is_muxer {
        if let Some(m) = vaco_registry::muxer_by_name(name) {
            let mut s = format!("Muxer {} [{}]:\n", m.name, m.long_name);
            push_extensions(&mut s, m.extensions);
            push_mime(&mut s, name);
            if let Some(v) = m.default_video {
                let _ = writeln!(s, "    Default video codec: {}.", v.name());
            }
            if let Some(a) = m.default_audio {
                let _ = writeln!(s, "    Default audio codec: {}.", a.name());
            }
            return s;
        }
    } else if let Some(d) = vaco_registry::demuxer_by_name(name) {
        let mut s = format!("Demuxer {} [{}]:\n", d.name, d.long_name);
        push_extensions(&mut s, d.extensions);
        return s;
    }
    format!("Unknown format '{name}'.")
}

/// The muxer's first MIME type, if the registry has one.
///
/// Three things measured rather than assumed:
///
/// * **Muxers print it, demuxers do not.** `-h muxer=aiff` prints
///   `Mime type: audio/aiff.`; `-h demuxer=aiff` prints nothing after its
///   header, even though the same format is involved. So this is called from
///   the muxer arm only.
/// * **Only the first.** `aiff`'s component carries `audio/aiff` and
///   `audio/x-aiff`; the reference prints the first alone.
/// * **It comes from the registry, not the descriptor.** `MuxerDesc` has no
///   MIME field — `vaco_registry::Component` does — which is why this takes a
///   name and looks it up rather than reading it off `m`.
fn push_mime(out: &mut String, name: &str) {
    let Some(mime) = vaco_registry::component(Kind::Muxer, name).and_then(|c| c.mime_types.first())
    else {
        return;
    };
    let _ = writeln!(out, "    Mime type: {mime}.");
}

fn push_extensions(out: &mut String, extensions: &[&str]) {
    if extensions.is_empty() {
        return;
    }
    out.push_str("    Common extensions: ");
    out.push_str(&extensions.join(","));
    out.push_str(".\n");
}

/// The shared shape for `filter`/`bsf`/`protocol`: a "no name" message, an
/// "unknown name" message built as `"{prefix} '{name}'."`, and a lookup that
/// returns the success body when it finds something.
fn named_lookup(
    name: Option<&str>,
    no_name: &str,
    unknown_prefix: &str,
    lookup: impl FnOnce(&str) -> Option<String>,
) -> String {
    let Some(name) = name else {
        return no_name.to_owned();
    };
    lookup(name).unwrap_or_else(|| format!("{unknown_prefix} '{name}'."))
}

/// Assemble the whole `-h` output: the body, the measured blank-line rule
/// (one line before `Exiting with exit code 0` normally, two when the body's
/// last block was an `AVOptions`/consts block), and the trailer itself.
///
/// The trailer is reproduced verbatim and unconditionally for the whole `-h`
/// family (measured: present even at `-loglevel quiet`, absent from every
/// other listing command — see `vaco_cli_core::help`'s module docs). It
/// carries no reference branding, so D9 does not bar it the way the banner's
/// version block is barred.
fn finish<W: Write>(w: &mut W, body: &str) -> io::Result<()> {
    w.write_all(body.as_bytes())?;
    if body.is_empty() {
        return writeln!(w, "Exiting with exit code 0");
    }
    // Terminate the body's own last line if it did not already end in one
    // (the single-line "Codec 'x' is not recognized…" / "Unknown format
    // 'x'." messages have no trailing newline of their own; the multi-line
    // renderers all do).
    if !body.ends_with('\n') {
        writeln!(w)?;
    }
    // The mandatory blank line every non-empty `-h` body gets before the
    // trailer, plus a second one when the body's last block was an
    // `AVOptions`/consts block — both measured, see the module docs.
    writeln!(w)?;
    if ends_in_options_block(body) {
        writeln!(w)?;
    }
    writeln!(w, "Exiting with exit code 0")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn text(topic: Option<&str>) -> String {
        let mut buf = Vec::new();
        render(&mut buf, topic.map(OsStr::new)).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn bare_h_ends_with_one_blank_line_and_the_trailer() {
        let s = text(None);
        assert!(s.starts_with("Print help"), "{s}");
        assert!(s.ends_with("\n\nExiting with exit code 0\n"), "{s}");
        assert!(!s.contains("-buildconf"), "{s}");
    }

    #[test]
    fn long_shows_expert_options() {
        let s = text(Some("long"));
        assert!(s.contains("-buildconf"), "{s}");
    }

    #[test]
    fn full_appends_the_generic_format_schema_with_two_blank_lines() {
        let s = text(Some("full"));
        assert!(s.contains("\n\n\nAVFormatContext AVOptions:\n"), "{s}");
        assert!(s.contains("-fflags"), "{s}");
        assert!(s.ends_with("\n\nExiting with exit code 0\n"), "{s}");
    }

    #[test]
    fn unrecognised_topic_reports_it_and_falls_back_to_basic() {
        let s = text(Some("bogus"));
        assert!(s.starts_with("Unknown help option 'bogus'.\n"), "{s}");
        assert!(s.contains("Print help"), "{s}");
    }

    #[test]
    fn dash_prefixed_topic_is_reported_literally() {
        // Measured: `ffmpeg -h -version` swallows `-version` as h's own
        // topic and reports it, dash and all.
        let s = text(Some("-version"));
        assert!(s.starts_with("Unknown help option '-version'.\n"), "{s}");
    }

    #[test]
    fn unrecognised_kind_reports_only_the_kind_part() {
        let s = text(Some("zzzz=name"));
        assert!(s.starts_with("Unknown help option 'zzzz'.\n"), "{s}");
    }

    #[test]
    fn demuxer_no_equals_is_the_null_literal_but_empty_name_is_not() {
        assert_eq!(
            text(Some("demuxer")),
            "Unknown format '(null)'.\n\nExiting with exit code 0\n"
        );
        assert_eq!(
            text(Some("demuxer=")),
            "Unknown format ''.\n\nExiting with exit code 0\n"
        );
    }

    #[test]
    fn a_known_demuxer_prints_its_header_and_extensions() {
        let s = text(Some("demuxer=matroska"));
        assert!(s.starts_with("Demuxer matroska,webm ["), "{s}");
        assert!(s.contains("Common extensions: "), "{s}");
        assert!(s.contains("mkv"), "{s}");
        // No private schema in this build: structurally the same shape as
        // the reference's own matroska (which also has none), one blank
        // line before the trailer.
        assert!(s.ends_with("\n\nExiting with exit code 0\n"), "{s}");
    }

    #[test]
    fn an_unknown_demuxer_name_is_reported() {
        assert_eq!(
            text(Some("demuxer=nonesuchxyz")),
            "Unknown format 'nonesuchxyz'.\n\nExiting with exit code 0\n"
        );
    }

    #[test]
    fn muxer_kind_describes_a_registered_muxer_and_rejects_anything_else() {
        // This asserted `-h muxer=matroska` reports "Unknown format", which was
        // true when the build had no muxers and became false the moment one
        // landed. The invariant is the mapping, not the emptiness: a name the
        // registry has is described, and one it does not have is unknown.
        if let Some(m) = vaco_registry::muxers().first() {
            let s = text(Some(&format!("muxer={}", m.name)));
            assert!(s.starts_with(&format!("Muxer {} ", m.name)), "{s}");
            assert!(s.ends_with("\n\nExiting with exit code 0\n"), "{s}");
        }
        assert_eq!(
            text(Some("muxer=nonesuchxyz")),
            "Unknown format 'nonesuchxyz'.\n\nExiting with exit code 0\n"
        );
    }

    #[test]
    fn codec_kind_reports_not_recognized_by_vaco_not_ffmpeg() {
        let s = text(Some("decoder=h264"));
        assert_eq!(
            s,
            "Codec 'h264' is not recognized by Vaco.\n\nExiting with exit code 0\n"
        );
        assert!(!s.contains("FFmpeg"));
        // `-h decoder=` (an explicit, empty name) is a *different* case from
        // `-h decoder` (no `=` at all) — measured, the reference does not
        // collapse them: the former still runs the "not recognized" lookup
        // with an empty string, the latter short-circuits before that.
        assert_eq!(
            text(Some("decoder=")),
            "Codec '' is not recognized by Vaco.\n\nExiting with exit code 0\n"
        );
        assert_eq!(
            text(Some("decoder")),
            "No codec name specified.\n\nExiting with exit code 0\n"
        );
    }

    #[test]
    fn filter_and_bsf_describe_what_is_registered_and_reject_the_rest() {
        // This named `scale` as its example of an unknown filter, and `scale`
        // was registered the same day. Ninth test in this project to fail *on
        // success*, which is why the assertions below ask the registry what it
        // holds instead of naming a name.
        assert_eq!(
            text(Some("filter")),
            "No filter name specified.\n\nExiting with exit code 0\n"
        );
        for f in vaco_registry::filters() {
            let s = text(Some(&format!("filter={}", f.name)));
            assert!(
                !s.starts_with("Unknown filter"),
                "registered filter `{}` reported unknown: {s}",
                f.name
            );
        }
        assert_eq!(
            text(Some("filter=nonesuchxyz")),
            "Unknown filter 'nonesuchxyz'.\n\nExiting with exit code 0\n"
        );

        assert_eq!(
            text(Some("bsf")),
            "No bitstream filter name specified.\n\nExiting with exit code 0\n"
        );
        assert_eq!(
            text(Some("bsf=nonesuchxyz")),
            "Unknown bit stream filter 'nonesuchxyz'.\n\nExiting with exit code 0\n"
        );
    }

    #[test]
    fn protocol_describes_a_registered_one_and_rejects_anything_else() {
        // This asserted `-h protocol=file` reports "Unknown protocol", which
        // was true when the build had no protocols and became false the moment
        // one landed. That is the third test in this crate to pin the *absence*
        // of a feature the project was actively building, and each one failed
        // on success — the least useful day for a test to fail.
        //
        // The invariant is the mapping, not the emptiness.
        assert_eq!(
            text(Some("protocol")),
            "No protocol name specified.\n\nExiting with exit code 0\n"
        );
        for p in vaco_registry::protocols() {
            let s = text(Some(&format!("protocol={}", p.name)));
            // Not "starts with `<name> AVOptions:`" — a protocol with no
            // options prints no header at all, which is the reference's own
            // behaviour and took one failed assertion to find. What must hold
            // for every registered protocol is that it is not reported unknown.
            assert!(
                !s.starts_with("Unknown protocol"),
                "registered protocol `{}` reported unknown: {s}",
                p.name
            );
        }
        assert_eq!(
            text(Some("protocol=nonesuchxyz")),
            "Unknown protocol 'nonesuchxyz'.\n\nExiting with exit code 0\n"
        );
    }

    #[test]
    fn non_utf8_topic_degrades_lossily_rather_than_panicking() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bytes = [b'x', 0xffu8, 0xfeu8];
            let os = std::ffi::OsStr::from_bytes(&bytes);
            let mut buf = Vec::new();
            // Must not panic; the exact replacement-character rendering is
            // not asserted, only that this crate's `#![forbid(unsafe_code)]`
            // surface stays total over adversarial argv bytes.
            render(&mut buf, Some(os)).unwrap();
        }
    }
}
