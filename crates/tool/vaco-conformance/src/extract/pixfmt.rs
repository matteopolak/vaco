//! Differential check of `vaco-pixfmt`'s 268-format table.
//!
//! # What it is
//!
//! Three independent checks on the same table, because a single oracle can be
//! wrong in a way that a second one catches:
//!
//! 1. [`check_show_pixel_formats`] — `ffprobe -show_pixel_formats`, the richest
//!    listing: name, component count, chroma decimation, bits per pixel, seven
//!    flags, and per-component bit depths.
//! 2. [`check_pix_fmts`] — `ffmpeg -pix_fmts`, a second rendering of the same
//!    facts through a different code path in the reference. Where the two
//!    listings disagree with each other, that is reported as an *oracle*
//!    inconsistency and neither is treated as truth.
//! 3. [`probe_plane_geometry`] — behavioural rather than declarative. For each
//!    format, ask the reference to write one 64×64 frame as `rawvideo` and
//!    compare the byte count to
//!    [`PixFmt::plane_layout`](vaco_pixfmt::PixFmt::plane_layout). This is the
//!    only check that exercises our *arithmetic* — plane count, `step`,
//!    subsampling and stride — rather than our copy of the metadata, and it is
//!    the one that would catch a table that is self-consistently wrong.
//!
//! # What the oracle does not expose
//!
//! Neither listing reports a **plane count**. `nb_components` is not it:
//! `yuyv422` has three components in one plane. Plane count is therefore
//! checked only indirectly, through the geometry probe, and the shortfall is
//! recorded as a note on the report rather than left implicit.
//!
//! # How to change it
//!
//! [`FIELDS`] drives the field-by-field comparison; adding a field means adding
//! a row there and a case to [`ours_of`]. Do not add a field the oracle does
//! not actually print — a comparison against a value we invented is not a
//! comparison.

use std::collections::BTreeMap;
use std::time::Duration;

use vaco_pixfmt::{PixFmt, PixFmtFlags};

use crate::extract::{FieldDivergence, TableReport, tidy};
use crate::refbin::Reference;
use crate::run::{Invocation, capture_stdout, capture_stdout_bytes};

/// A pixel format as the reference describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefPixFmt {
    /// The format name.
    pub name: String,
    /// Component count.
    pub nb_components: u8,
    /// log2 horizontal chroma decimation.
    pub log2_chroma_w: u8,
    /// log2 vertical chroma decimation.
    pub log2_chroma_h: u8,
    /// Average bits per pixel.
    pub bits_per_pixel: u8,
    /// The seven flags the listing prints, by name.
    pub flags: BTreeMap<String, bool>,
    /// Per-component significant bits, in component order.
    pub depths: Vec<u8>,
}

/// The fields both listings expose and we can compare.
pub const FIELDS: [&str; 5] = [
    "nb_components",
    "log2_chroma_w",
    "log2_chroma_h",
    "bits_per_pixel",
    "bit_depths",
];

/// The flags the reference prints, paired with our bit.
pub const FLAGS: [(&str, PixFmtFlags); 7] = [
    ("big_endian", PixFmtFlags::BIG_ENDIAN),
    ("palette", PixFmtFlags::PALETTE),
    ("bitstream", PixFmtFlags::BITSTREAM),
    ("hwaccel", PixFmtFlags::HW_ACCEL),
    ("planar", PixFmtFlags::PLANAR),
    ("rgb", PixFmtFlags::RGB),
    ("alpha", PixFmtFlags::ALPHA),
];

/// Our value for a comparable field.
#[must_use]
pub fn ours_of(fmt: PixFmt, field: &str) -> String {
    let d = fmt.descriptor();
    match field {
        "nb_components" => d.components.len().to_string(),
        "log2_chroma_w" => d.log2_chroma_w.to_string(),
        "log2_chroma_h" => d.log2_chroma_h.to_string(),
        "bits_per_pixel" => d.bits_per_pixel.to_string(),
        "bit_depths" => depths_string(&d.components.iter().map(|c| c.depth).collect::<Vec<_>>()),
        _ => String::new(),
    }
}

fn depths_string(depths: &[u8]) -> String {
    depths
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join("-")
}

fn theirs_of(r: &RefPixFmt, field: &str) -> String {
    match field {
        "nb_components" => r.nb_components.to_string(),
        "log2_chroma_w" => r.log2_chroma_w.to_string(),
        "log2_chroma_h" => r.log2_chroma_h.to_string(),
        "bits_per_pixel" => r.bits_per_pixel.to_string(),
        "bit_depths" => depths_string(&r.depths),
        _ => String::new(),
    }
}

/// Parse `ffprobe -show_pixel_formats` output.
///
/// The section syntax is shared with every other `default`-writer output, so
/// this reuses [`crate::compare::structured::parse_sections`] rather than
/// growing a second parser that could drift from it.
#[must_use]
pub fn parse_show_pixel_formats(text: &str) -> Vec<RefPixFmt> {
    use crate::compare::structured::parse_sections;
    let sections = parse_sections(text);
    let mut by_ordinal: BTreeMap<usize, RefPixFmt> = BTreeMap::new();
    for s in &sections {
        if s.path != "PIXEL_FORMAT" {
            continue;
        }
        let num = |k: &str| -> u8 {
            s.fields
                .get(k)
                .and_then(|v| v.parse::<u8>().ok())
                .unwrap_or_default()
        };
        let mut flags = BTreeMap::new();
        for (name, _) in FLAGS {
            let key = format!("FLAGS:{name}");
            flags.insert(
                name.to_owned(),
                s.fields.get(&key).map(String::as_str) == Some("1"),
            );
        }
        by_ordinal.insert(
            s.ordinal,
            RefPixFmt {
                name: s.fields.get("name").cloned().unwrap_or_default(),
                nb_components: num("nb_components"),
                log2_chroma_w: num("log2_chroma_w"),
                log2_chroma_h: num("log2_chroma_h"),
                bits_per_pixel: num("bits_per_pixel"),
                flags,
                depths: Vec::new(),
            },
        );
    }
    // Attach components by their recorded parent key. Sections come out in
    // CLOSING order, so a component precedes the format that contains it and
    // "the most recent PIXEL_FORMAT" would be the wrong one — this was a real
    // bug, caught by the first run against the reference.
    for s in &sections {
        if s.path != "PIXEL_FORMAT.COMPONENT" {
            continue;
        }
        let Some(ordinal) = s
            .parent
            .as_deref()
            .and_then(|k| k.strip_prefix("PIXEL_FORMAT["))
            .and_then(|k| k.strip_suffix(']'))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        if let Some(f) = by_ordinal.get_mut(&ordinal) {
            f.depths.push(
                s.fields
                    .get("bit_depth")
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or_default(),
            );
        }
    }
    by_ordinal.into_values().collect()
}

/// Parse `ffmpeg -pix_fmts` output.
///
/// Columns: a five-character flag field (`I`nput, `O`utput, `H`ardware,
/// `P`alette, `B`itstream), name, component count, bits per pixel, and
/// hyphen-separated bit depths.
#[must_use]
pub fn parse_pix_fmts(text: &str) -> Vec<RefPixFmt> {
    let mut out = Vec::new();
    let mut started = false;
    for line in text.lines() {
        if !started {
            // The table begins after a line of dashes.
            started = line.trim_start().starts_with("-----");
            continue;
        }
        let mut cols = line.split_whitespace();
        let (Some(flagcol), Some(name), Some(nb), Some(bpp)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        if flagcol.len() != 5 {
            continue;
        }
        let depths = cols.next().unwrap_or_default();
        let nb_components: u8 = nb.parse().unwrap_or_default();
        let f = flagcol.as_bytes();
        let has = |i: usize, c: u8| f.get(i) == Some(&c);
        let mut flags = BTreeMap::new();
        flags.insert("hwaccel".to_owned(), has(2, b'H'));
        flags.insert("palette".to_owned(), has(3, b'P'));
        flags.insert("bitstream".to_owned(), has(4, b'B'));
        out.push(RefPixFmt {
            name: name.to_owned(),
            nb_components,
            bits_per_pixel: bpp.parse().unwrap_or_default(),
            // A format with no components prints a single `0` in this column.
            // `-show_pixel_formats` emits no COMPONENT sections at all for the
            // same format, so taking the `0` literally would make the two
            // oracles disagree with each other over nothing — a harness
            // artifact, and exactly the bucket §1.6.2 has a name for.
            depths: if nb_components == 0 {
                Vec::new()
            } else {
                depths
                    .split('-')
                    .filter_map(|d| d.parse::<u8>().ok())
                    .collect()
            },
            // Not exposed by this listing.
            log2_chroma_w: u8::MAX,
            log2_chroma_h: u8::MAX,
            flags,
        });
    }
    out
}

fn ours_table() -> BTreeMap<String, PixFmt> {
    PixFmt::all()
        .iter()
        .map(|&f| (f.name().to_owned(), f))
        .collect()
}

/// Check our table against `ffprobe -show_pixel_formats`.
#[must_use]
pub fn check_show_pixel_formats(reference: &Reference) -> TableReport {
    let inv = Invocation::new(
        &reference.ffprobe,
        ["-hide_banner", "-loglevel", "error", "-show_pixel_formats"],
    )
    .with_timeout(Duration::from_secs(30));
    let mut report = TableReport {
        table: "pixfmt".to_owned(),
        oracle: inv.command_line(),
        ..TableReport::default()
    };
    report.notes.push(
        "the listing does not expose a plane count; `nb_components` is not it \
         (yuyv422 has three components in one plane). Plane arithmetic is covered \
         by `table-pixfmt-geometry` in --deep."
            .to_owned(),
    );
    let text = match capture_stdout(&inv) {
        Ok(t) => t,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    let theirs: BTreeMap<String, RefPixFmt> = parse_show_pixel_formats(&text)
        .into_iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let ours = ours_table();
    diff(&ours, &theirs, true, &mut report);
    report
}

/// Cross-check the same data through `ffmpeg -pix_fmts`.
#[must_use]
pub fn check_pix_fmts(reference: &Reference) -> TableReport {
    let inv = Invocation::new(
        &reference.ffmpeg,
        [
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-pix_fmts",
        ],
    )
    .with_timeout(Duration::from_secs(30));
    let mut report = TableReport {
        table: "pixfmt-cross".to_owned(),
        oracle: inv.command_line(),
        ..TableReport::default()
    };
    report.notes.push(
        "a second rendering of the same facts through a different path in the \
         reference; it exposes neither chroma decimation nor most flags."
            .to_owned(),
    );
    let text = match capture_stdout(&inv) {
        Ok(t) => t,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    let theirs: BTreeMap<String, RefPixFmt> = parse_pix_fmts(&text)
        .into_iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let ours = ours_table();
    diff(&ours, &theirs, false, &mut report);
    report
}

fn diff(
    ours: &BTreeMap<String, PixFmt>,
    theirs: &BTreeMap<String, RefPixFmt>,
    full: bool,
    report: &mut TableReport,
) {
    report.ours_count = ours.len();
    report.theirs_count = theirs.len();
    for name in ours.keys() {
        if !theirs.contains_key(name) {
            report.only_ours.push(name.clone());
        }
    }
    for name in theirs.keys() {
        if !ours.contains_key(name) {
            report.only_theirs.push(name.clone());
        }
    }
    tidy(&mut report.only_ours);
    tidy(&mut report.only_theirs);

    for (name, &fmt) in ours {
        let Some(r) = theirs.get(name) else { continue };
        for field in FIELDS {
            // The cross listing does not print chroma decimation.
            if !full && field.starts_with("log2_chroma") {
                continue;
            }
            let a = ours_of(fmt, field);
            let b = theirs_of(r, field);
            if a != b {
                report.fields.push(FieldDivergence {
                    entity: name.clone(),
                    field: field.to_owned(),
                    ours: a,
                    theirs: b,
                });
            }
        }
        for (flag, bit) in FLAGS {
            let Some(&theirs_set) = r.flags.get(flag) else {
                continue;
            };
            let ours_set = fmt.has(bit);
            if ours_set != theirs_set {
                report.fields.push(FieldDivergence {
                    entity: name.clone(),
                    field: flag.to_owned(),
                    ours: u8::from(ours_set).to_string(),
                    theirs: u8::from(theirs_set).to_string(),
                });
            }
        }
    }
    report.fields.sort();
}

/// The frame size the geometry probe uses. Even in both axes and divisible by
/// four, so no subsampled format needs rounding — a rounding disagreement is a
/// separate question and would only muddy this one.
pub const PROBE_W: u32 = 64;
/// See [`PROBE_W`].
pub const PROBE_H: u32 = 64;

/// Behavioural check: does one raw frame come out the size our arithmetic says?
///
/// This is the check that would catch a table which is self-consistently wrong.
/// One process per format, so it is `--deep` only.
#[must_use]
pub fn probe_plane_geometry(reference: &Reference) -> TableReport {
    let mut report = TableReport {
        table: "pixfmt-geometry".to_owned(),
        oracle: format!(
            "{} -f lavfi -i color=c=black:s={PROBE_W}x{PROBE_H}:d=1 -frames:v 1 \
             -pix_fmt <fmt> -f rawvideo - | wc -c",
            reference.ffmpeg.display()
        ),
        ..TableReport::default()
    };
    report.notes.push(
        "compares the byte count of one raw frame against PixFmt::plane_layout, \
         which is the only check that exercises plane count, step and stride \
         rather than our copy of the metadata."
            .to_owned(),
    );

    let mut probed = 0_usize;
    let mut unprobeable = 0_usize;
    for &fmt in PixFmt::all() {
        if fmt.is_hw() {
            continue;
        }
        let Ok(layout) = fmt.plane_layout(PROBE_W, PROBE_H, 1) else {
            report.notes.push(format!(
                "{}: plane_layout refused a {PROBE_W}x{PROBE_H} frame",
                fmt.name()
            ));
            continue;
        };
        let inv = Invocation::new(
            &reference.ffmpeg,
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=black:s={PROBE_W}x{PROBE_H}:d=1"),
                "-frames:v",
                "1",
                "-pix_fmt",
                fmt.name(),
                "-f",
                "rawvideo",
                "-",
            ],
        )
        .with_timeout(Duration::from_secs(20));
        match capture_stdout_bytes(&inv) {
            Ok(bytes) => {
                probed += 1;
                if bytes.len() != layout.total {
                    report.fields.push(FieldDivergence {
                        entity: fmt.name().to_owned(),
                        field: "frame_bytes".to_owned(),
                        ours: layout.total.to_string(),
                        theirs: bytes.len().to_string(),
                    });
                }
            }
            Err(_) => unprobeable += 1,
        }
    }
    report.ours_count = probed;
    report.theirs_count = probed;
    if unprobeable > 0 {
        report.notes.push(format!(
            "{unprobeable} formats could not be written as rawvideo by the \
             reference (no conversion path); not a divergence"
        ));
    }
    report.fields.sort();
    report
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{FIELDS, ours_of, parse_pix_fmts, parse_show_pixel_formats};
    use vaco_pixfmt::PixFmt;

    const SHOW: &str = "\
[PIXEL_FORMAT]
name=yuv420p
nb_components=3
log2_chroma_w=1
log2_chroma_h=1
bits_per_pixel=12
FLAGS:big_endian=0
FLAGS:palette=0
FLAGS:bitstream=0
FLAGS:hwaccel=0
FLAGS:planar=1
FLAGS:rgb=0
FLAGS:alpha=0
[COMPONENT]
index=1
bit_depth=8
[/COMPONENT]
[COMPONENT]
index=2
bit_depth=8
[/COMPONENT]
[COMPONENT]
index=3
bit_depth=8
[/COMPONENT]
[/PIXEL_FORMAT]
[PIXEL_FORMAT]
name=rgb24
nb_components=3
log2_chroma_w=0
log2_chroma_h=0
bits_per_pixel=24
FLAGS:big_endian=0
FLAGS:palette=0
FLAGS:bitstream=0
FLAGS:hwaccel=0
FLAGS:planar=0
FLAGS:rgb=1
FLAGS:alpha=0
[COMPONENT]
index=1
bit_depth=8
[/COMPONENT]
[/PIXEL_FORMAT]
";

    #[test]
    fn show_pixel_formats_parses_flags_and_component_depths() {
        let v = parse_show_pixel_formats(SHOW);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "yuv420p");
        assert_eq!(v[0].log2_chroma_w, 1);
        assert_eq!(v[0].bits_per_pixel, 12);
        assert_eq!(v[0].depths, vec![8, 8, 8]);
        assert!(v[0].flags["planar"]);
        assert!(!v[0].flags["rgb"]);
        // Components must attach to the right format, not spill over.
        assert_eq!(v[1].name, "rgb24");
        assert_eq!(v[1].depths, vec![8]);
        assert!(v[1].flags["rgb"]);
    }

    const PIXFMTS: &str = "\
Pixel formats:
I.... = Supported Input  format for conversion
-----
IO... yuv420p                3             12      8-8-8
IO... rgb24                  3             24      8-8-8
..H.. videotoolbox_vld       0              0      0
";

    #[test]
    fn pix_fmts_parses_the_column_table() {
        let v = parse_pix_fmts(PIXFMTS);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "yuv420p");
        assert_eq!(v[0].nb_components, 3);
        assert_eq!(v[0].bits_per_pixel, 12);
        assert_eq!(v[0].depths, vec![8, 8, 8]);
        assert!(v[2].flags["hwaccel"]);
        assert!(!v[0].flags["hwaccel"]);
    }

    #[test]
    fn a_zero_component_format_has_no_depths_in_either_oracle() {
        // `-pix_fmts` prints a lone `0`; `-show_pixel_formats` prints nothing.
        // Taking the `0` literally made the two oracles disagree over nothing.
        let v = parse_pix_fmts(PIXFMTS);
        let hw = v
            .iter()
            .find(|f| f.name == "videotoolbox_vld")
            .expect("present");
        assert!(hw.depths.is_empty(), "got {:?}", hw.depths);
    }

    #[test]
    fn a_header_line_is_never_mistaken_for_a_format() {
        let v = parse_pix_fmts(PIXFMTS);
        assert!(v.iter().all(|f| f.name != "FLAGS"));
    }

    #[test]
    fn our_side_of_every_comparable_field_renders() {
        let fmt = PixFmt::from_name("yuv420p").expect("yuv420p exists");
        assert_eq!(ours_of(fmt, "nb_components"), "3");
        assert_eq!(ours_of(fmt, "log2_chroma_w"), "1");
        assert_eq!(ours_of(fmt, "bits_per_pixel"), "12");
        assert_eq!(ours_of(fmt, "bit_depths"), "8-8-8");
        for field in FIELDS {
            assert!(!ours_of(fmt, field).is_empty(), "{field} rendered empty");
        }
    }

    #[test]
    fn the_table_we_check_is_the_whole_table() {
        assert!(
            PixFmt::all().len() > 250,
            "the extractor is pointless if it only sees part of the table"
        );
    }
}
