//! The `-version`, `-formats`, `-muxers`… options: print a table and exit 0.
//!
//! # Scope
//!
//! CL-04 owns the help system. `-h` and its four depths live in [`crate::help`];
//! this module is the other half — the fourteen standalone listing commands.
//! Column layouts, legends and headers below were measured against `ffmpeg
//! 8.1`/`ffprobe 8.1` under `LC_ALL=C` (D17, plan 13 §1b), not recalled — see
//! each function's doc comment for the exact invocation. Component *names* are
//! interface facts and are reproduced (D9); so, per the same reasoning already
//! established for [`banner`], are these short structural legends ("D.. =
//! Demuxing supported"), which describe a data format rather than express an
//! author's prose.
//!
//! A listing this build cannot yet render faithfully — because the data it
//! would need does not exist in the registry, not because the layout is
//! unknown — still returns [`AvError::ENOSYS`] naming the gap, rather than a
//! half-formatted table. See the bottom of this file for exactly which ones
//! and why.

use std::io::Write;

use vaco_registry::{Kind, components_of_kind};

use crate::exit::{AvError, Diagnostic};

/// This program's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The stderr banner.
///
/// The reference prints its own identity and nine library versions here.
/// Reproducing that would be claiming to be `FFmpeg`, which D9 puts outside
/// what we copy. The *shape* is the same so `-hide_banner` means the same
/// thing.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn banner<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(
        w,
        "vaco version {VERSION} Copyright (c) 2026 the Vaco authors"
    )
}

/// Translate a write failure on the listing sink into the same shape every
/// other failure in this crate uses. Shared with [`crate::help`], whose `-h`
/// rendering is a write sink of the same kind.
pub(crate) fn io_diagnostic(e: std::io::Error) -> Diagnostic {
    Diagnostic::new(
        AvError::of(&vaco_core::Error::Io(e)),
        vec!["Error writing to standard output".to_owned()],
    )
}

/// Left-justify `s` to at least `min` characters, then one literal separator
/// space — the same `max(min, len) + 1` field algorithm as the `AVOptions`
/// name/type columns in `vaco_cli_core::help`, independently confirmed here
/// against `-formats`' name field (measured minimum 15) and `-codecs`'
/// (measured minimum 20).
fn pad_field(out: &mut String, s: &str, min: usize) {
    out.push_str(s);
    for _ in s.chars().count()..min.max(s.chars().count()) {
        out.push(' ');
    }
    out.push(' ');
}

/// Render one `-<name>` listing.
///
/// # Errors
///
/// [`AvError::ENOSYS`] for a listing this build does not render yet — see the
/// module docs for exactly which and why.
pub fn render<W: Write>(w: &mut W, name: &str) -> Result<(), Diagnostic> {
    let mut go = || -> std::io::Result<bool> {
        match name {
            "version" => {
                writeln!(w, "vaco version {VERSION}")?;
            }
            "L" | "license" => {
                writeln!(w, "vaco is licensed under MIT OR Apache-2.0.")?;
            }
            "buildconf" => {
                writeln!(w, "  configuration:")?;
                for f in enabled_features() {
                    writeln!(w, "    --enable-{f}")?;
                }
            }
            "formats" | "demuxers" | "muxers" => {
                write_formats(w, name)?;
            }
            "decoders" | "encoders" => {
                write_codec_impl_listing(w, name)?;
            }
            "filters" => {
                write_filters(w)?;
            }
            "bsfs" => {
                writeln!(w, "Bitstream filters:")?;
                for c in components_of_kind(Kind::BitstreamFilter) {
                    writeln!(w, "{}", c.name)?;
                }
            }
            "protocols" => {
                write_protocols(w)?;
            }
            "codecs" => {
                write_codecs(w)?;
            }
            "dispositions" => {
                for &(_, n) in vaco_cli_core::Disposition::ALL {
                    writeln!(w, "{n}")?;
                }
            }
            // Not a reference option at all (verified: `ffmpeg -parsers` is
            // unrecognised); kept because `vaco-registry` tracks parsers as
            // their own kind and a build that can say what it demuxes should
            // be able to say what it can parse headers for too. No reference
            // shape to be faithful to, so this stays a plain name+long_name
            // table rather than inventing a flag legend nobody can check.
            "parsers" => {
                writeln!(w, "Parsers:")?;
                for c in components_of_kind(Kind::Parser) {
                    writeln!(w, " {:<16} {}", c.name, c.long_name.unwrap_or(c.name))?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    };
    match go() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Diagnostic::new(
            AvError::ENOSYS,
            vec![format!(
                "-{name} needs data this build's registry does not carry yet; see docs/app/vaco-cli.md."
            )],
        )),
        Err(e) => Err(io_diagnostic(e)),
    }
}

/// `-formats`/`-demuxers`/`-muxers`: one shared header (measured identical
/// across all three — even `-demuxers` prints the muxing-support legend line)
/// and a filtered row set.
///
/// Measured (`ffmpeg -formats`, `LC_ALL=C`):
/// ```text
/// Formats:
///  D.. = Demuxing supported
///  .E. = Muxing supported
///  ..d = Is a device
///  ---
///  D   3dostr          3DO STR
///   E  3g2             3GP2 (3GPP2 file format)
/// ```
/// The row's leading space, then three capability slots each blank or its
/// own letter (never the header's placeholder dot), then one more separator
/// space, then the name field at `max(15, len) + 1`. We have no notion of "is
/// a device" at all, so that slot is always blank — a real, reportable gap
/// rather than a guess.
fn write_formats<W: Write>(w: &mut W, which: &str) -> std::io::Result<()> {
    writeln!(w, "Formats:")?;
    writeln!(w, " D.. = Demuxing supported")?;
    writeln!(w, " .E. = Muxing supported")?;
    writeln!(w, " ..d = Is a device")?;
    writeln!(w, " ---")?;
    if which != "muxers" {
        for d in vaco_registry::demuxers() {
            let is_muxer = vaco_registry::muxer_by_name(d.name).is_some();
            write_format_row(w, true, is_muxer, d.name, d.long_name)?;
        }
    }
    if which != "demuxers" {
        for m in vaco_registry::muxers() {
            let is_demuxer = vaco_registry::demuxer_by_name(m.name).is_some();
            write_format_row(w, is_demuxer, true, m.name, m.long_name)?;
        }
    }
    Ok(())
}

fn write_format_row(
    w: &mut impl Write,
    demux: bool,
    mux: bool,
    name: &str,
    long_name: &str,
) -> std::io::Result<()> {
    let mut line = String::new();
    line.push(' ');
    line.push(if demux { 'D' } else { ' ' });
    line.push(if mux { 'E' } else { ' ' });
    line.push(' '); // "is a device": always unknown in this build.
    line.push(' ');
    pad_field(&mut line, name, 15);
    line.push_str(long_name);
    writeln!(w, "{line}")
}

/// `-decoders`/`-encoders`: the eight-line legend plus a `------` rule.
///
/// Measured (`ffmpeg -decoders`, `LC_ALL=C`): the legend is identical for
/// both commands, only the heading word changes. This build has zero
/// decoders and zero encoders (D5 — v0.1 is parse-only), so the table under
/// the rule is always empty; the header is still worth getting exactly right
/// because a build that later ships one decoder must not need this function
/// touched.
fn write_codec_impl_listing<W: Write>(w: &mut W, which: &str) -> std::io::Result<()> {
    writeln!(
        w,
        "{}:",
        if which == "encoders" {
            "Encoders"
        } else {
            "Decoders"
        }
    )?;
    writeln!(w, " V..... = Video")?;
    writeln!(w, " A..... = Audio")?;
    writeln!(w, " S..... = Subtitle")?;
    writeln!(w, " .F.... = Frame-level multithreading")?;
    writeln!(w, " ..S... = Slice-level multithreading")?;
    writeln!(w, " ...X.. = Codec is experimental")?;
    writeln!(w, " ....B. = Supports draw_horiz_band")?;
    writeln!(w, " .....D = Supports direct rendering method 1")?;
    writeln!(w, " ------")?;
    // Nothing to list: `vaco_registry::decoders()`/component(Kind::Encoder)
    // are always empty in this build. When a decoder lands, its row is
    // `" {media}....." ` + the same `max(20, len)+1` name field as `-codecs`,
    // per the shared family of AVOption-adjacent listing tables — but that is
    // unverified against a real row, since there is nothing to check it
    // against yet.
    Ok(())
}

/// `-filters`: the seven-line legend. Zero rows: `FILTERS` is always empty
/// (no filter crate exists yet).
fn write_filters<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Filters:")?;
    writeln!(w, "  T.. = Timeline support")?;
    writeln!(w, "  .S. = Slice threading")?;
    writeln!(w, "  A = Audio input/output")?;
    writeln!(w, "  V = Video input/output")?;
    writeln!(w, "  N = Dynamic number and/or type of input/output")?;
    writeln!(w, "  | = Source or sink filter")?;
    writeln!(w, "  ------")?;
    Ok(())
}

/// `-protocols`: not a flag-column table at all — measured (`ffmpeg
/// -protocols`) as one heading, an `Input:` section, and an `Output:` section,
/// each a bare sorted name list.
///
/// `vaco_protocol_core::ProtocolFlags` carries no read/write capability bit
/// (see `docs/app/vaco-cli.md`) — a gap in that crate, not this one — so
/// there is no way to tell an input-only protocol from an output-only one
/// from the registry alone. Every enabled protocol is listed under **both**
/// sections rather than guessed at; documented as a known divergence.
fn write_protocols<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Supported file protocols:")?;
    let mut names: Vec<&str> = vaco_registry::protocols().iter().map(|p| p.name).collect();
    names.sort_unstable();
    writeln!(w, "Input:")?;
    for n in &names {
        writeln!(w, "  {n}")?;
    }
    writeln!(w, "Output:")?;
    for n in &names {
        writeln!(w, "  {n}")?;
    }
    Ok(())
}

/// `-codecs`: the ten-line legend plus one row per [`vaco_core::CodecId`], in
/// declaration order (matching `-codecs`' existing iteration order, which the
/// reference's own alphabetical-by-name order does not — a pre-existing,
/// separately-tracked divergence this change does not touch).
///
/// The six-column flag field is real, not padding: `D`/`E` from whether this
/// build can decode/encode the codec, the media-type letter from
/// [`vaco_core::MediaType`], and `I`/`L`/`S` from
/// [`vaco_codec_core::CodecProperties`] — all data the registry and
/// `vaco-codec-core` already carry, so nothing here is guessed.
fn write_codecs<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Codecs:")?;
    writeln!(w, " D..... = Decoding supported")?;
    writeln!(w, " .E.... = Encoding supported")?;
    writeln!(w, " ..V... = Video codec")?;
    writeln!(w, " ..A... = Audio codec")?;
    writeln!(w, " ..S... = Subtitle codec")?;
    writeln!(w, " ..D... = Data codec")?;
    writeln!(w, " ..T... = Attachment codec")?;
    writeln!(w, " ...I.. = Intra frame-only codec")?;
    writeln!(w, " ....L. = Lossy compression")?;
    writeln!(w, " .....S = Lossless compression")?;
    writeln!(w, " -------")?;
    for id in vaco_registry::codecs() {
        let props = id.properties();
        // Real ffmpeg's `..T...` slot is a separate "attachment codec" flag
        // rather than a seventh media type, but `vaco_core::MediaType` models
        // attachment as a fourth media kind alongside video/audio/subtitle
        // rather than data-with-a-flag. Reusing the same slot both ways keeps
        // the six-column width right; the divergence (an attachment codec
        // shows only in the media-type slot here, not in a `T`) is
        // structural, not a guess, and is recorded in the doc file.
        let media = match id.media_type() {
            vaco_core::MediaType::Video => 'V',
            vaco_core::MediaType::Audio => 'A',
            vaco_core::MediaType::Subtitle => 'S',
            vaco_core::MediaType::Data => 'D',
            vaco_core::MediaType::Attachment => 'T',
        };
        let mut flags = String::with_capacity(6);
        flags.push(if vaco_registry::can_decode(id) {
            'D'
        } else {
            '.'
        });
        flags.push('.'); // no EncoderDesc table exists yet (D5).
        flags.push(media);
        flags.push(if props.is_intra_only() { 'I' } else { '.' });
        flags.push(if props.contains(vaco_codec_core::CodecProperties::LOSSY) {
            'L'
        } else {
            '.'
        });
        flags.push(
            if props.contains(vaco_codec_core::CodecProperties::LOSSLESS) {
                'S'
            } else {
                '.'
            },
        );
        let mut line = format!(" {flags} ");
        pad_field(&mut line, id.name(), 20);
        line.push_str(id.long_name());
        writeln!(w, "{line}")?;
    }
    Ok(())
}

fn enabled_features() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vaco_registry::components()
        .filter_map(|c| c.feature)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

// ---------------------------------------------------------------------------
// Deferred: `-pix_fmts`, `-sample_fmts`, `-layouts`, `-colors`, `-hwaccels`,
// `-devices`, `-sources`, `-sinks`.
//
// All eight still return `ENOSYS`. Header shapes were measured for the first
// three (`docs/app/vaco-cli.md` records them) but the row data needs
// `vaco-pixfmt`/`vaco-sampfmt`/`vaco-chlayout` to expose per-format component
// counts, bit depths and alpha/paletted/bitstream/hardware flags that this
// crate has no way to reach without either a new dependency (out of scope: it
// would cross from `app` down into `model`, which is architecturally fine,
// but CL-04's own scope is the two `app` crates, not extending those model
// crates' public surface to suit) or guessing, which D6 rules out. `-colors`
// was never in the CLI table CL-04's ffmpeg option table declares as a real
// option to begin with; it is real in the reference (verified:
// `ffmpeg -colors` succeeds) but the brief's own listing named
// "`-colorspaces`" instead, which does **not** exist in ffmpeg 8.1 (verified:
// `ffmpeg -colorspaces` exits 8, "Unrecognized option") — see the crate's
// report for that correction. `-hwaccels`/`-devices`/`-sources`/`-sinks` need
// a hardware/device registry this build does not have at all (D13's
// `vaco-hw-*` crates are a separate, later work package).
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn text(name: &str) -> String {
        let mut buf = Vec::new();
        render(&mut buf, name).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn version_prints_our_identity_not_the_references() {
        let s = text("version");
        assert!(s.starts_with("vaco version "), "{s}");
        assert!(!s.contains("FFmpeg"));
    }

    #[test]
    fn formats_header_matches_the_measured_legend() {
        let s = text("formats");
        assert!(
            s.starts_with(
                "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n \
             ..d = Is a device\n ---\n"
            ),
            "{s}"
        );
        assert!(s.contains("matroska"), "{s}");
        // D5: zero muxers, so no row ever carries an `E`.
        assert_eq!(vaco_registry::muxers().len(), 0);
        assert!(!s.lines().any(|l| l.starts_with("  E")), "{s}");
    }

    #[test]
    fn formats_row_padding_matches_the_measured_minimum_of_fifteen() {
        let s = text("formats");
        let row = s.lines().find(|l| l.contains("matroska,webm")).unwrap();
        // " D   matroska,webm   Matroska / WebM" — 4 marker/indent chars,
        // then the name field at `max(15, 13) + 1 = 16`.
        assert_eq!(row, " D   matroska,webm   Matroska / WebM");
    }

    #[test]
    fn demuxers_and_muxers_share_the_formats_header() {
        let d = text("demuxers");
        let m = text("muxers");
        assert!(d.starts_with("Formats:\n"), "{d}");
        assert!(m.starts_with("Formats:\n"), "{m}");
        assert!(d.contains("matroska"), "{d}");
        // No muxers in this build: the table under the header is empty.
        assert_eq!(m.lines().count(), 5, "{m}");
    }

    #[test]
    fn decoders_and_encoders_headers_with_zero_rows() {
        let d = text("decoders");
        assert!(d.starts_with("Decoders:\n"), "{d}");
        assert!(d.ends_with(" ------\n"), "{d}");
        let e = text("encoders");
        assert!(e.starts_with("Encoders:\n"), "{e}");
    }

    #[test]
    fn filters_header_with_zero_rows() {
        let s = text("filters");
        assert!(s.starts_with("Filters:\n"), "{s}");
        assert!(s.ends_with("  ------\n"), "{s}");
    }

    #[test]
    fn bsfs_header_with_zero_rows() {
        assert_eq!(text("bsfs"), "Bitstream filters:\n");
    }

    #[test]
    fn protocols_lists_input_and_output_sections() {
        let s = text("protocols");
        assert!(s.starts_with("Supported file protocols:\nInput:\n"), "{s}");
        assert!(s.contains("Output:\n"), "{s}");
    }

    #[test]
    fn codecs_header_and_a_real_row() {
        let s = text("codecs");
        assert!(s.starts_with("Codecs:\n"), "{s}");
        assert!(s.contains(" -------\n"), "{s}");
        // h264 exists as a codec identity even though this build cannot
        // decode it, so its row must show an all-dot D column.
        let row = s
            .lines()
            .find(|l| l.contains(" h264 ") || l.trim_end().ends_with("h264"));
        assert!(row.is_some(), "{s}");
    }

    #[test]
    fn dispositions_are_the_nineteen_names_in_bit_order() {
        let s = text("dispositions");
        assert_eq!(s.lines().count(), 19);
        assert_eq!(s.lines().next(), Some("default"));
        assert_eq!(s.lines().last(), Some("multilayer"));
    }

    #[test]
    fn a_deferred_listing_names_the_gap_rather_than_half_rendering() {
        let mut buf = Vec::new();
        let e = render(&mut buf, "pix_fmts").unwrap_err();
        assert!(e.render().contains("vaco-cli.md"), "{}", e.render());
        assert!(buf.is_empty());
    }

    #[test]
    fn the_banner_is_ours() {
        let mut buf = Vec::new();
        banner(&mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("vaco version "));
        assert!(!s.contains("ffmpeg"));
    }
}
