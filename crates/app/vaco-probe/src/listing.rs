//! The options that print a table and exit: `-formats`, `-codecs`,
//! `-sections`, and the rest.
//!
//! These are pure renderings of the registry and of the model crates' own
//! tables, which is exactly what plan 14 §2.8 predicted: no component is
//! instantiated, because a descriptor is inspectable without constructing
//! anything.
//!
//! # Provenance
//!
//! Column layouts were measured from the reference, since a listing is compared
//! byte for byte like anything else. Under `LC_ALL=C`:
//!
//! ```sh
//! ffprobe -v quiet -hide_banner -formats | sed -n '1,8p'
//! ffprobe -v quiet -hide_banner -codecs  | sed -n '1,14p'
//! ffprobe -v quiet -hide_banner -sections | head -8
//! ```
//!
//! * `-formats`: `" %-3s %-15s %s\n"` — leading space, three flag columns, a
//!   space, the name in fifteen, a space, the long name.
//! * `-codecs`: the same with six flag columns and a twenty-wide name.
//! * `-sections`: four flag characters, then `3` spaces at the root and
//!   `4·depth + 2` below it. Measured across all thirteen distinct depths, and
//!   the step is genuinely 3 then 4, not 4 throughout.
//!
//! Plan 13 §1.3.2's `component-intersection` normaliser is what makes these
//! comparable at all: the reference lists ~90 demuxers and we list what we
//! have, so the harness intersects and additionally asserts our set is a
//! subset. A listing with no rows is a correct listing of an empty registry.

use std::io::Write;

use vaco_codec_core::CodecId;
use vaco_core::{MediaType, Result};
use vaco_registry::{Component, Kind};
use vaco_textformat::sections::{SectionDesc, SectionFlags, SectionId, desc};

use crate::cli::Listing;

/// Render a listing.
///
/// # Errors
/// Propagates the sink's I/O error.
pub fn render<W: Write>(w: &mut W, which: Listing) -> Result<()> {
    match which {
        Listing::Sections => sections(w),
        Listing::Formats => formats(w, true, true),
        Listing::Demuxers => formats(w, true, false),
        Listing::Muxers => formats(w, false, true),
        // A device is a component kind we do not have and do not plan to; the
        // reference prints the same header with no rows when built without
        // avdevice.
        Listing::Devices => formats(w, false, false),
        Listing::Codecs => codecs(w),
        Listing::Decoders => coders(w, "Decoders"),
        Listing::Encoders => coders(w, "Encoders"),
        Listing::Bsfs => named(w, "Bitstream filters:", Kind::BitstreamFilter),
        Listing::Filters => named(w, "Filters:", Kind::Filter),
        Listing::Protocols => protocols(w),
        Listing::PixFmts => pix_fmts(w),
        Listing::SampleFmts => sample_fmts(w),
        Listing::Layouts => layouts(w),
        Listing::Dispositions => dispositions(w),
        Listing::Colors => colors(w),
        // Version, licence, build configuration and help are the binary's
        // identity rather than the registry's; `crate::banner` owns them.
        Listing::Version | Listing::License | Listing::BuildConf | Listing::Help => Ok(()),
    }
}

/// `-sections`: the schema tree, from `vaco-textformat`'s own table.
///
/// Rendered by walking [`SECTIONS`] from the root rather than by iterating the
/// array, because the output is a *tree* and the array is flat; a section
/// reachable twice would be printed twice, which is what the reference does
/// too — nothing is, in this schema.
fn sections<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"Sections:\n")?;
    w.write_all(b"W... = Section is a wrapper (contains other sections, no local entries)\n")?;
    w.write_all(b".A.. = Section contains an array of elements of the same type\n")?;
    w.write_all(b"..V. = Section may contain a variable number of fields with variable keys\n")?;
    w.write_all(b"...T = Section contain a unique type\n")?;
    w.write_all(b"FLAGS NAME/UNIQUE_NAME\n")?;
    w.write_all(b"----\n")?;
    walk(w, SectionId::ROOT, 0)
}

fn walk<W: Write>(w: &mut W, id: SectionId, depth: usize) -> Result<()> {
    let s: &SectionDesc = desc(id);
    let flag = |f: SectionFlags, c: char| if s.flags.contains(f) { c } else { '.' };
    let name = if s.name == s.unique_name {
        s.name.to_owned()
    } else {
        format!("{}/{}", s.name, s.unique_name)
    };
    // 3 at the root, then 4 per level plus 2. Measured, not guessed.
    let indent = if depth == 0 { 3 } else { 4 * depth + 2 };
    writeln!(
        w,
        "{}{}{}{}{:indent$}{name}",
        flag(SectionFlags::WRAPPER, 'W'),
        flag(SectionFlags::ARRAY, 'A'),
        flag(SectionFlags::VAR_FIELDS, 'V'),
        flag(SectionFlags::UNIQUE_TYPE, 'T'),
        "",
    )?;
    for child in s.children {
        walk(w, *child, depth + 1)?;
    }
    Ok(())
}

/// `-formats`, `-demuxers`, `-muxers` — one table, three filters.
fn formats<W: Write>(w: &mut W, demux: bool, mux: bool) -> Result<()> {
    w.write_all(b"Formats:\n")?;
    w.write_all(b" D.. = Demuxing supported\n")?;
    w.write_all(b" .E. = Muxing supported\n")?;
    w.write_all(b" ..d = Is a device\n")?;
    w.write_all(b" ---\n")?;

    // One row per *name*, with D and E merged: the reference lists `3g2` once
    // with both flags when it can both read and write it.
    let mut rows: Vec<(&str, &str, bool, bool)> = Vec::new();
    for c in vaco_registry::components() {
        let (is_d, is_m) = (c.kind == Kind::Demuxer, c.kind == Kind::Muxer);
        if !is_d && !is_m {
            continue;
        }
        match rows.iter_mut().find(|(n, _, _, _)| *n == c.name) {
            Some(row) => {
                row.2 |= is_d;
                row.3 |= is_m;
            }
            None => rows.push((c.name, long_name(c), is_d, is_m)),
        }
    }
    rows.sort_by(|a, b| a.0.cmp(b.0));

    for (name, long, d, m) in rows {
        if !(d && demux || m && mux) {
            continue;
        }
        // Three flag columns; the third is `d` for a device, which we have no
        // kind for, so it is always blank.
        writeln!(
            w,
            " {}{}  {name:<15} {long}",
            if d { 'D' } else { ' ' },
            if m { 'E' } else { ' ' },
        )?;
    }
    Ok(())
}

/// `-codecs`: the codec *identity* table, annotated with what this build has.
///
/// Rows come from `CodecId::all()`, not from the registry: the reference lists
/// every codec it knows of, marking which have a decoder or an encoder. A build
/// with no decoders still lists the codecs.
fn codecs<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"Codecs:\n")?;
    w.write_all(b" D..... = Decoding supported\n")?;
    w.write_all(b" .E.... = Encoding supported\n")?;
    w.write_all(b" ..V... = Video codec\n")?;
    w.write_all(b" ..A... = Audio codec\n")?;
    w.write_all(b" ..S... = Subtitle codec\n")?;
    w.write_all(b" ..D... = Data codec\n")?;
    w.write_all(b" ..T... = Attachment codec\n")?;
    w.write_all(b" ...I.. = Intra frame-only codec\n")?;
    w.write_all(b" ....L. = Lossy compression\n")?;
    w.write_all(b" .....S = Lossless compression\n")?;
    w.write_all(b" -------\n")?;

    let mut rows: Vec<CodecId> = CodecId::all().collect();
    rows.sort_by_key(|c| c.name());
    for codec in rows {
        let p = codec.properties();
        let flags = [
            if vaco_registry::can_decode(codec) {
                'D'
            } else {
                '.'
            },
            // No encoders in this build; the column exists so the shape is right.
            '.',
            media_letter(codec.media_type()),
            if p.contains(vaco_codec_core::CodecProperties::INTRA_ONLY) {
                'I'
            } else {
                '.'
            },
            if p.contains(vaco_codec_core::CodecProperties::LOSSY) {
                'L'
            } else {
                '.'
            },
            if p.contains(vaco_codec_core::CodecProperties::LOSSLESS) {
                'S'
            } else {
                '.'
            },
        ];
        let flags: String = flags.iter().collect();
        writeln!(w, " {flags} {:<20} {}", codec.name(), codec.long_name())?;
    }
    Ok(())
}

const fn media_letter(m: MediaType) -> char {
    match m {
        MediaType::Video => 'V',
        MediaType::Audio => 'A',
        MediaType::Subtitle => 'S',
        MediaType::Data => 'D',
        MediaType::Attachment => 'T',
    }
}

/// `-decoders` / `-encoders`.
fn coders<W: Write>(w: &mut W, title: &str) -> Result<()> {
    writeln!(w, "{title}:")?;
    w.write_all(b" V..... = Video\n")?;
    w.write_all(b" A..... = Audio\n")?;
    w.write_all(b" S..... = Subtitle\n")?;
    w.write_all(b" .F.... = Frame-level multithreading\n")?;
    w.write_all(b" ..S... = Slice-level multithreading\n")?;
    w.write_all(b" ...X.. = Codec is experimental\n")?;
    w.write_all(b" ....B. = Supports draw_horiz_band\n")?;
    w.write_all(b" .....D = Supports direct rendering method 1\n")?;
    w.write_all(b" ------\n")?;
    if title != "Decoders" {
        return Ok(());
    }
    let mut rows: Vec<&vaco_codec_core::DecoderDesc> = vaco_registry::decoders().to_vec();
    rows.sort_by_key(|d| d.name);
    for d in rows {
        let caps = d.caps;
        let flags: String = [
            media_letter(d.media_type),
            if caps.contains(vaco_codec_core::Caps::FRAME_THREADS) {
                'F'
            } else {
                '.'
            },
            if caps.contains(vaco_codec_core::Caps::SLICE_THREADS) {
                'S'
            } else {
                '.'
            },
            if caps.contains(vaco_codec_core::Caps::EXPERIMENTAL) {
                'X'
            } else {
                '.'
            },
            '.',
            '.',
        ]
        .iter()
        .collect();
        writeln!(w, " {flags} {:<20} {}", d.name, d.long_name)?;
    }
    Ok(())
}

/// One-name-per-line listings for kinds with no flag columns.
fn named<W: Write>(w: &mut W, title: &str, kind: Kind) -> Result<()> {
    writeln!(w, "{title}")?;
    let mut names: Vec<&str> = vaco_registry::components_of_kind(kind)
        .map(|c| c.name)
        .collect();
    names.sort_unstable();
    for n in names {
        writeln!(w, "  {n}")?;
    }
    Ok(())
}

/// `-protocols`: two lists, input then output.
///
/// Every protocol appears under both headings. `ProtocolFlags` records
/// `network`, `nested_scheme` and `server_capable` but not read/write
/// capability, so there is nothing to split the two lists on — the reference
/// does split them (`sdp` is input-only, `md5` output-only). A reported gap in
/// `vaco-protocol-core`; see the doc file.
fn protocols<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"Supported file protocols:\n")?;
    let mut names: Vec<&str> = vaco_registry::protocols().iter().map(|p| p.name).collect();
    names.sort_unstable();
    for title in ["Input:", "Output:"] {
        writeln!(w, "{title}")?;
        for n in &names {
            writeln!(w, "  {n}")?;
        }
    }
    Ok(())
}

/// `-pix_fmts`, `-sample_fmts`, `-layouts` and `-colors` print their headers
/// and no rows.
///
/// Not an oversight and not a stub in the usual sense: the *rows* need a
/// public "every variant" iterator on `vaco-pixfmt`, `vaco-sampfmt` and
/// `vaco-chlayout` respectively, and none of the three exposes one. Inventing
/// a local list here would duplicate a generated table and start drifting from
/// it the day it changes, which is precisely the failure mode plan 19 §3.4
/// exists to prevent. Recorded as a reported gap in the doc file; the headers
/// are byte-identical so the shape is already pinned.
fn pix_fmts<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"Pixel formats:\n")?;
    w.write_all(b"I.... = Supported Input  format for conversion\n")?;
    w.write_all(b".O... = Supported Output format for conversion\n")?;
    w.write_all(b"..H.. = Hardware accelerated format\n")?;
    w.write_all(b"...P. = Paletted format\n")?;
    w.write_all(b"....B = Bitstream format\n")?;
    w.write_all(b"FLAGS NAME            NB_COMPONENTS BITS_PER_PIXEL BIT_DEPTHS\n")?;
    w.write_all(b"-----\n").map_err(vaco_core::Error::Io)
}

fn sample_fmts<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"name   depth\n").map_err(vaco_core::Error::Io)
}

fn layouts<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"Individual channels:\n")?;
    w.write_all(b"NAME           DESCRIPTION\n")?;
    w.write_all(b"\nStandard channel layouts:\n")?;
    w.write_all(b"NAME           DECOMPOSITION\n")
        .map_err(vaco_core::Error::Io)
}

fn colors<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"name             #RRGGBB\n")
        .map_err(vaco_core::Error::Io)
}

/// `-dispositions`: one flag name per line, in bit order.
///
/// The list is `vaco_cli_core::Disposition::ALL`, which carries all nineteen
/// flags in the reference's bit order — the same table the
/// `stream_disposition` section prints from, for the same reason.
fn dispositions<W: Write>(w: &mut W) -> Result<()> {
    for &(_, name) in vaco_cli_core::Disposition::ALL {
        writeln!(w, "{name}")?;
    }
    Ok(())
}

fn long_name(c: &Component) -> &'static str {
    c.long_name.unwrap_or("")
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    fn out(which: Listing) -> String {
        let mut buf = Vec::new();
        render(&mut buf, which).expect("render");
        String::from_utf8(buf).expect("utf8")
    }

    #[test]
    fn sections_matches_the_reference_header_and_indentation() {
        let text = out(Listing::Sections);
        // Captured from `ffprobe -v quiet -sections`, first eight lines.
        assert!(
            text.starts_with(
                "Sections:\n\
             W... = Section is a wrapper (contains other sections, no local entries)\n\
             .A.. = Section contains an array of elements of the same type\n\
             ..V. = Section may contain a variable number of fields with variable keys\n\
             ...T = Section contain a unique type\n\
             FLAGS NAME/UNIQUE_NAME\n\
             ----\n\
             W...   root\n\
             .A..      chapters\n\
             ....          chapter\n\
             ..V.              tags/chapter_tags\n\
             ....      format\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn sections_lists_every_row_exactly_once() {
        let text = out(Listing::Sections);
        let body = text.lines().skip(7).count();
        assert_eq!(body, vaco_textformat::sections::SECTIONS.len());
    }

    /// The one place `-sections` is not byte-identical, pinned so it cannot
    /// drift further and cannot be "fixed" by accident.
    ///
    /// `ffprobe 8.1` nests `pieces/stream_group_pieces` under
    /// **`subcomponent`**; `vaco_textformat::sections` has it as a sibling of
    /// `subcomponents` under `component`. Six lines
    /// (`pieces … block`) therefore come out eight columns to the left of the
    /// reference's. Line *order* is unaffected, and so is every other section.
    ///
    /// Reference, `ffprobe -v quiet -sections`:
    ///
    /// ```text
    /// .A..                      subcomponents
    /// ..VT                          subcomponent
    /// .A..                              pieces/stream_group_pieces
    /// ```
    ///
    /// The section tree belongs to `vaco-textformat`, so this is reported
    /// rather than corrected here. See `docs/app/vaco-probe.md`.
    #[test]
    fn the_stream_group_pieces_divergence_still_exists() {
        let text = out(Listing::Sections);
        let line = text
            .lines()
            .find(|l| l.trim_end().ends_with(" block"))
            .expect("block row");
        let body = line.get(4..).expect("flags are four chars");
        let indent = body.len() - body.trim_start().len();
        assert_eq!(indent, 42, "ours: {line:?}");
        // The reference puts it at 4*12 + 2.
        assert_eq!(50 - indent, 8, "the gap is two levels, or the tree moved");
    }

    #[test]
    fn dispositions_are_the_nineteen_names_in_bit_order() {
        let text = out(Listing::Dispositions);
        let names: Vec<&str> = text.lines().collect();
        assert_eq!(names.len(), 19);
        assert_eq!(names.first(), Some(&"default"));
        assert_eq!(names.get(9), Some(&"clean_effects"));
        assert_eq!(names.last(), Some(&"multilayer"));
    }

    #[test]
    fn every_listing_renders_without_panicking_on_an_empty_registry() {
        for which in [
            Listing::Formats,
            Listing::Muxers,
            Listing::Demuxers,
            Listing::Devices,
            Listing::Codecs,
            Listing::Decoders,
            Listing::Encoders,
            Listing::Bsfs,
            Listing::Protocols,
            Listing::Filters,
            Listing::PixFmts,
            Listing::Layouts,
            Listing::SampleFmts,
            Listing::Dispositions,
            Listing::Colors,
            Listing::Sections,
            Listing::Version,
            Listing::License,
            Listing::BuildConf,
            Listing::Help,
        ] {
            let _ = out(which);
        }
    }

    #[test]
    fn the_formats_header_is_byte_identical() {
        assert!(out(Listing::Formats).starts_with(
            "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n \
             ..d = Is a device\n ---\n"
        ));
    }

    #[test]
    fn codec_rows_use_the_measured_column_widths() {
        let text = out(Listing::Codecs);
        let row = text.lines().nth(12).expect("a codec row");
        // " D.VI.S 012v                 Uncompressed 4:2:2 10-bit"
        //  ^ 1 space, 6 flags, 1 space, 20-wide name, 1 space
        assert_eq!(row.get(..1), Some(" "));
        assert_eq!(row.get(7..8), Some(" "));
        assert_eq!(row.get(28..29), Some(" "));
    }
}
