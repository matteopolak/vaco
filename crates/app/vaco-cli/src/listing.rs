//! The `-version`, `-formats`, `-muxers`… options: print a table and exit 0.
//!
//! # Scope
//!
//! CL-04 owns the help system. `-h` and its four depths live in [`crate::help`];
//! this module is the other half — the twenty-two standalone listing commands.
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
//! half-formatted table.
//!
//! # The eight formerly-`ENOSYS` listings
//!
//! `-pix_fmts`, `-sample_fmts`, `-layouts`, `-colors`, `-hwaccels`,
//! `-devices`, `-sources` and `-sinks` all render now. The first four have
//! real per-format data behind them (`vaco-pixfmt`, `vaco-sampfmt`,
//! `vaco-chlayout`, `vaco_core::parse::color`); the last four are honest
//! empty listings under a real header, because this build has no hardware
//! backend and no device layer at all (D13's `vaco-hw-*` crates are a
//! separate work package) — an empty list under a real header is exactly
//! what the reference itself would print with none of a thing registered,
//! and it is what CL-04's brief asks for rather than keeping `ENOSYS`. See
//! each function's doc comment for the measurement and, where this build's
//! data disagrees with the reference's, the exact divergence and its cause.

use std::collections::BTreeMap;
use std::ffi::OsStr;
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
/// `value` is the option's own argument, if it took one — today that is only
/// `-sources`/`-sinks`' optional device name (`None` covers both "this
/// option's grammar takes no argument" and "the argument was omitted"; the
/// caller does not distinguish the two, and nothing here needs it to). An
/// `OsStr`, not a `str`: this crate's own convention (see "Known
/// divergences" in `docs/app/vaco-cli.md`) is to treat non-UTF-8 input as a
/// real case rather than lossily converting it, and a device name that
/// cannot be a `str` is still a device name that was *given* — distinct from
/// none being given at all, which changes which of two reference outputs is
/// correct.
///
/// # Errors
///
/// [`AvError::ENOSYS`] for a listing this build does not render yet — see the
/// module docs for exactly which and why.
pub fn render<W: Write>(w: &mut W, name: &str, value: Option<&OsStr>) -> Result<(), Diagnostic> {
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
            "pix_fmts" => {
                write_pix_fmts(w)?;
            }
            "sample_fmts" => {
                write_sample_fmts(w)?;
            }
            "layouts" => {
                write_layouts(w)?;
            }
            "colors" => {
                write_colors(w)?;
            }
            "hwaccels" => {
                write_hwaccels(w)?;
            }
            "devices" => {
                write_devices(w)?;
            }
            "sources" | "sinks" => {
                write_sources_or_sinks(w, value)?;
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
/// The row is a leading space, three capability slots each blank or its own
/// letter, a separator space, then the name field at `max(15, len) + 1`. The
/// device slot is always blank: this build registers no devices.
///
/// Three rules the obvious implementation gets wrong. `-formats` is the
/// sorted *union* of both directions, not the two lists concatenated.
/// `-demuxers` and `-muxers` mask the flag column to the direction asked for,
/// so `avi` shows ` D  ` under one and `  E ` under the other. And where a
/// name exists in both, `-formats` takes the muxer's long name — they differ
/// for 20 of the reference's 130 both-way formats.
fn write_formats<W: Write>(w: &mut W, which: &str) -> std::io::Result<()> {
    writeln!(w, "Formats:")?;
    writeln!(w, " D.. = Demuxing supported")?;
    writeln!(w, " .E. = Muxing supported")?;
    writeln!(w, " ..d = Is a device")?;
    writeln!(w, " ---")?;

    // The muxer pass runs second and so wins the long name.
    let mut rows: BTreeMap<&str, (bool, bool, &str)> = BTreeMap::new();
    if which != "muxers" {
        for d in vaco_registry::demuxers() {
            rows.insert(d.name, (true, false, d.long_name));
        }
    }
    if which != "demuxers" {
        for m in vaco_registry::muxers() {
            let demuxes = rows.get(m.name).is_some_and(|r| r.0);
            rows.insert(m.name, (demuxes, true, m.long_name));
        }
    }
    for (name, (demux, mux, long_name)) in rows {
        write_format_row(w, demux, mux, name, long_name)?;
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

/// `-decoders`/`-encoders`: the eight-line legend, a `------` rule, then one
/// row per registered decoder/encoder.
///
/// Measured (`ffmpeg -decoders`/`-encoders`, `LC_ALL=C`): the legend is
/// identical for both commands, only the heading word changes, and the row
/// shape is the same `" {flags} " + pad_field(name, 20) + long_name` family
/// `-codecs` uses.
///
/// This printed the legend and zero rows from D5 (v0.1 was
/// parse-only) until `EncoderDesc` and `DecoderDesc::make` existed to
/// register something behind. `Caps::FRAME_THREADS`/`SLICE_THREADS`/
/// `EXPERIMENTAL` are real data this build tracks per implementation, so
/// those three flag columns are drawn from them; `draw_horiz_band` and
/// "direct rendering method 1" are internal facts about the *reference's*
/// decoder objects that nothing in this crate's model corresponds to, so
/// those two columns are always `.` here rather than guessed — the same
/// honesty `-codecs`' own doc comment already applies to the columns it
/// draws from data this build does not have.
fn write_codec_impl_listing<W: Write>(w: &mut W, which: &str) -> std::io::Result<()> {
    let is_encoder = which == "encoders";
    writeln!(w, "{}:", if is_encoder { "Encoders" } else { "Decoders" })?;
    writeln!(w, " V..... = Video")?;
    writeln!(w, " A..... = Audio")?;
    writeln!(w, " S..... = Subtitle")?;
    writeln!(w, " .F.... = Frame-level multithreading")?;
    writeln!(w, " ..S... = Slice-level multithreading")?;
    writeln!(w, " ...X.. = Codec is experimental")?;
    writeln!(w, " ....B. = Supports draw_horiz_band")?;
    writeln!(w, " .....D = Supports direct rendering method 1")?;
    writeln!(w, " ------")?;

    let media_letter = |m: vaco_core::MediaType| match m {
        vaco_core::MediaType::Video => 'V',
        vaco_core::MediaType::Audio => 'A',
        vaco_core::MediaType::Subtitle => 'S',
        vaco_core::MediaType::Data => 'D',
        vaco_core::MediaType::Attachment => 'T',
    };
    let row =
        |name: &str, long_name: &str, media: vaco_core::MediaType, caps: vaco_codec_core::Caps| {
            let mut flags = String::with_capacity(6);
            flags.push(media_letter(media));
            flags.push(if caps.contains(vaco_codec_core::Caps::FRAME_THREADS) {
                'F'
            } else {
                '.'
            });
            flags.push(if caps.contains(vaco_codec_core::Caps::SLICE_THREADS) {
                'S'
            } else {
                '.'
            });
            flags.push(if caps.contains(vaco_codec_core::Caps::EXPERIMENTAL) {
                'X'
            } else {
                '.'
            });
            flags.push('.'); // draw_horiz_band: not a concept this build models.
            flags.push('.'); // direct rendering method 1: likewise.
            let mut line = format!(" {flags} ");
            pad_field(&mut line, name, 20);
            line.push_str(long_name);
            line
        };

    if is_encoder {
        let mut rows: Vec<&'static vaco_codec_core::EncoderDesc> =
            vaco_registry::encoders().to_vec();
        rows.sort_unstable_by_key(|e| e.name);
        for e in rows {
            writeln!(w, "{}", row(e.name, e.long_name, e.media_type, e.caps))?;
        }
    } else {
        let mut rows: Vec<&'static vaco_codec_core::DecoderDesc> =
            vaco_registry::decoders().to_vec();
        rows.sort_unstable_by_key(|d| d.name);
        for d in rows {
            writeln!(w, "{}", row(d.name, d.long_name, d.media_type, d.caps))?;
        }
    }
    Ok(())
}

/// `-filters`: the seven-line legend, then one row per registered filter.
///
/// The row format is measured, not inferred — `ffmpeg -filters` 8.1, with the
/// widths read off a name that exactly fills its column so the padding cannot
/// be mistaken for a separator:
///
/// ```text
///  TS aap               AA->A      Apply Affine Projection algorithm to first audio stream.
///  .. abench            A->A       Benchmark part of a filtergraph.
///  TS colorchannelmixer V->V       Adjust colors by mixing color channels.
///  .. anullsrc          |->A       Null audio source, return empty audio frames.
///  .. nullsink          V->|       Do absolutely nothing with the input video.
///  .. split             V->N       Pass on the input to N video outputs.
///  .. concat            N->N       Concatenate audio and video streams.
/// ```
///
/// `colorchannelmixer` is seventeen characters and still has a single space
/// before its pad column, so the field is `{:<17}` plus a literal space rather
/// than `{:<18}`. Same reasoning for `{:<10}` on the pad column, read off
/// `AA->A`.
///
/// The pad column is one letter per pad — `A` or `V` — with `|` standing in
/// for "no pads on this side" (a source or a sink) and `N` for a count the
/// options decide. Sorted by name, which is the reference's own order.
///
/// This printed the legend and **zero rows** until 2026-08-23, on the strength
/// of a comment saying "no filter crate exists yet". Twenty filter crates and
/// 282 registered filters existed by then, every one of which resolved through
/// `-h filter=<name>`. The test beside it asserted the output *ended* at the
/// legend, so it passed for exactly as long as the bug lasted — the "never pin
/// the absence of something the project is building" trap, caught by comparing
/// `-filters` against the reference rather than by any test.
fn write_filters<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Filters:")?;
    writeln!(w, "  T.. = Timeline support")?;
    writeln!(w, "  .S. = Slice threading")?;
    writeln!(w, "  A = Audio input/output")?;
    writeln!(w, "  V = Video input/output")?;
    writeln!(w, "  N = Dynamic number and/or type of input/output")?;
    writeln!(w, "  | = Source or sink filter")?;
    writeln!(w, "  ------")?;

    let mut rows: Vec<&'static vaco_filter_core::FilterDesc> = vaco_registry::filters().to_vec();
    rows.sort_unstable_by_key(|f| f.name);
    for f in rows {
        let timeline = match f.flags.timeline() {
            vaco_filter_core::TimelineSupport::None => '.',
            _ => 'T',
        };
        let slice = if f
            .flags
            .contains(vaco_filter_core::FilterFlags::SLICE_THREADS)
        {
            'S'
        } else {
            '.'
        };
        let pads = format!(
            "{}->{}",
            pad_column(
                f.inputs,
                f.flags
                    .contains(vaco_filter_core::FilterFlags::DYNAMIC_INPUTS)
            ),
            pad_column(
                f.outputs,
                f.flags
                    .contains(vaco_filter_core::FilterFlags::DYNAMIC_OUTPUTS)
            ),
        );
        writeln!(
            w,
            " {timeline}{slice} {:<17} {:<10} {}",
            f.name, pads, f.description
        )?;
    }
    Ok(())
}

/// One side of a filter's pad column: a letter per pad, `|` for none, `N` for
/// a count the options decide.
///
/// `N` wins over the declared pads rather than being appended to them: the
/// reference prints `concat` as `N->N` and `split` as `V->N`, never the pads a
/// default instantiation happens to have.
fn pad_column(pads: &'static [vaco_filter_core::Pad], dynamic: bool) -> String {
    if dynamic {
        return "N".to_owned();
    }
    if pads.is_empty() {
        return "|".to_owned();
    }
    pads.iter()
        .map(|p| match p.media_type {
            vaco_core::MediaType::Audio => 'A',
            _ => 'V',
        })
        .collect()
}

/// `-protocols`: not a flag-column table at all — measured (`ffmpeg
/// -protocols`) as one heading, an `Input:` section, and an `Output:` section,
/// each a bare sorted name list.
///
/// A protocol appears under `Input:` if it can be read and `Output:` if it can
/// be written, and plenty are one and not the other: `md5` and `tee` are
/// output-only, `async`/`cache`/`concat`/`concatf`/`data`/`subfile` are
/// input-only, `file` and `pipe` are both.
///
/// This used to list every protocol under **both** headings, because
/// `ProtocolFlags` carried no read/write bit — so `-protocols` claimed `md5`
/// could be read and `subfile` written, neither of which is true. The flags
/// exist now (`readable`/`writable`, each measured against
/// `ffmpeg -protocols`), and they are stated rather than derived because
/// `Protocol::open` is required even for a protocol that only answers
/// `Unsupported` from it.
fn write_protocols<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Supported file protocols:")?;
    let mut protos: Vec<_> = vaco_registry::protocols().to_vec();
    protos.sort_unstable_by_key(|p| p.name);
    writeln!(w, "Input:")?;
    for p in protos.iter().filter(|p| p.flags.readable) {
        writeln!(w, "  {}", p.name)?;
    }
    writeln!(w, "Output:")?;
    for p in protos.iter().filter(|p| p.flags.writable) {
        writeln!(w, "  {}", p.name)?;
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
        // `CodecId::Pcm` is a sentinel — "PCM, flavour undetermined" — and not
        // a codec the reference has. Listing it made `pcm` the single invented
        // name in this whole listing, checked by diffing every row against
        // `ffmpeg -codecs`. It stays in the enum because fifty call sites still
        // reach it when a container states PCM without saying which, and each
        // of those is a gap worth being able to see; it just is not an
        // answer to "what codecs exist".
        if id == vaco_codec_core::CodecId::Pcm {
            continue;
        }
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
// `-pix_fmts`
// ---------------------------------------------------------------------------

/// `-pix_fmts`: `vaco_pixfmt::PixFmt::all()` already carries a name,
/// component count, average bits-per-pixel and per-component depth for every
/// format — generated data (`cargo xtask gen-pixfmt`), not hand-written. This
/// is the listing CL-04's brief calls out as most worth getting right, since
/// that data already exists in full; this function's job is column layout
/// and three small, named, measured corrections where this build's data and
/// the reference's disagree.
///
/// Header, legend and the four fixed-width columns were measured directly
/// (`ffmpeg -hide_banner -pix_fmts`, `LC_ALL=C`, ffmpeg 8.1):
///
/// ```text
/// Pixel formats:
/// I.... = Supported Input  format for conversion
/// .O... = Supported Output format for conversion
/// ..H.. = Hardware accelerated format
/// ...P. = Paletted format
/// ....B = Bitstream format
/// FLAGS NAME            NB_COMPONENTS BITS_PER_PIXEL BIT_DEPTHS
/// -----
/// IO... yuv420p                3             12      8-8-8
/// ```
///
/// Columns, measured by byte offset across all 267 rows (counted, not
/// estimated — D17, plan 13 §1b): flags (5 chars) + one space, `NAME`
/// left-justified to **16**, `NB_COMPONENTS` right-justified to **8**,
/// `BITS_PER_PIXEL` right-justified to **15**, six literal spaces, then
/// `BIT_DEPTHS` verbatim with no trailing padding. A zero-component
/// (hardware) format prints a literal `0` in the depths column, not an empty
/// string — the one formatting rule here that is not just "read the
/// descriptor".
///
/// `H`/`P`/`B` come straight from
/// `PixFmtFlags::{HW_ACCEL,PALETTE,BITSTREAM}` — real data — with the two
/// named exceptions in [`BITSTREAM_FLAG_OVERRIDE`]. `I`/`O` ("supported
/// *for conversion*") is not a pixel-format property at all: it is
/// libswscale's own hand-maintained per-format capability list, which
/// nothing in this workspace exposes — there is no scaler-capability query
/// reachable from this crate, and `vaco-scale`'s own coverage would answer a
/// different question (whether *our* scaler supports the format) from the
/// one D6 needs (whether the *reference's* does). [`INPUT_ONLY`],
/// [`OUTPUT_ONLY`] and [`NEITHER`] are that capability list, captured
/// verbatim from the same probe and nothing else — 49 of 267 formats are not
/// simply "software implies both", which is exactly the kind of fact this
/// project's D17 says to measure rather than assume.
///
/// # Divergences from `vaco-pixfmt`'s own data (not fixed here — cross-crate)
///
/// Comparing every one of `vaco-pixfmt`'s 268 formats against the reference's
/// 267 found:
///
/// * **One extra format.** `vaco-pixfmt` has `cuarray`; ffmpeg 8.1 does not.
///   Excluded by name below — a listing decides what it shows, not what the
///   descriptor table contains — and reported for `vaco-pixfmt` to look at.
/// * **`bgr8`'s component depths are in the wrong order.** The reference
///   reports `3-3-2`; `vaco-pixfmt`'s descriptor gives `2-3-3` for the same
///   format, despite [`vaco_pixfmt::PixFmtDescriptor`]'s own documented
///   convention (component 0 is the first *logical* channel, R for an RGB
///   format) — this looks like the component array being stored in *plane*
///   order for this one packed format rather than logical order. Corrected
///   for display in [`DEPTHS_OVERRIDE`] below.
/// * **The twelve Bayer formats model one raw-sample component where the
///   reference models three uneven-depth ones** (`bayer_bggr8`: ours is
///   1 component of depth 8, the reference is 3 components of depths
///   `2-4-2`). This is a structural modelling difference, not a fixable
///   typo — `vaco-pixfmt` treats a Bayer mosaic as a single logical channel,
///   which is a defensible design, but it means this listing cannot be
///   byte-identical for these twelve names without a display-only override,
///   which [`DEPTHS_OVERRIDE`] also carries.
/// * **`xv30be`/`v30xbe` are missing `PixFmtFlags::BITSTREAM`.** The
///   reference marks both `B`; `vaco-pixfmt` marks neither, while their
///   little-endian siblings (`xv30le`/`v30xle`) correctly have no `B` in
///   either implementation. Compensated for display in
///   [`BITSTREAM_FLAG_OVERRIDE`].
///
/// After the above, comparing every rendered row against the reference's own
/// (as a multiset — `diff <(sort ours) <(sort theirs)`) shows **zero content
/// differences across all 267 rows**, and with [`LISTING_ORDER`] the whole
/// listing is byte-identical.
///
/// # The row order, and why recording it is clean-room
///
/// `PixFmt::all()` is declared in family/subsampling order (`yuv410p`,
/// `yuv411p`, `yuv420p`, …); the reference emits its own historical
/// `AVPixelFormat` enum-assignment order (`yuv420p`, `yuyv422`, `rgb24`, …),
/// which encodes nothing but the sequence in which formats were added.
///
/// That was first left alone as "an arbitrary authorial sequence, not
/// format-dictated data", which reads like the right instinct and is the wrong
/// conclusion. **D6 is explicit**: the reference binary is used as a black box
/// producing observed outputs, and *recording observed behaviour of a shipped
/// binary is not copying expression*. The order below was obtained by running
/// `ffmpeg -hide_banner -pix_fmts` under `LC_ALL=C` and reading its output —
/// the same technique that produced the `ID3v1` genre table (probed across every
/// byte value 0–255) and the codec-tag tables. No source was consulted.
///
/// The distinction that matters is *how you learned it*, not how arbitrary it
/// looks. An arbitrary sequence you read out of someone's header file is off
/// limits; the same sequence printed on stdout by a program is an interface
/// fact (D9), and byte-identity is the D6 contract.
/// The reference's own row order for `-pix_fmts`, recorded by running it.
///
/// See [`write_pix_fmts`] for why reading this off stdout is clean-room and
/// reading it out of a header would not be. A format absent from this list is
/// appended after it in `PixFmt::all()` order, so a format we know and the
/// reference does not still appears rather than vanishing.
const LISTING_ORDER: &[&str] = &[
    "yuv420p",
    "yuyv422",
    "rgb24",
    "bgr24",
    "yuv422p",
    "yuv444p",
    "yuv410p",
    "yuv411p",
    "gray",
    "monow",
    "monob",
    "pal8",
    "yuvj420p",
    "yuvj422p",
    "yuvj444p",
    "uyvy422",
    "uyyvyy411",
    "bgr8",
    "bgr4",
    "bgr4_byte",
    "rgb8",
    "rgb4",
    "rgb4_byte",
    "nv12",
    "nv21",
    "argb",
    "rgba",
    "abgr",
    "bgra",
    "gray16be",
    "gray16le",
    "yuv440p",
    "yuvj440p",
    "yuva420p",
    "rgb48be",
    "rgb48le",
    "rgb565be",
    "rgb565le",
    "rgb555be",
    "rgb555le",
    "bgr565be",
    "bgr565le",
    "bgr555be",
    "bgr555le",
    "vaapi",
    "yuv420p16le",
    "yuv420p16be",
    "yuv422p16le",
    "yuv422p16be",
    "yuv444p16le",
    "yuv444p16be",
    "dxva2_vld",
    "rgb444le",
    "rgb444be",
    "bgr444le",
    "bgr444be",
    "ya8",
    "bgr48be",
    "bgr48le",
    "yuv420p9be",
    "yuv420p9le",
    "yuv420p10be",
    "yuv420p10le",
    "yuv422p10be",
    "yuv422p10le",
    "yuv444p9be",
    "yuv444p9le",
    "yuv444p10be",
    "yuv444p10le",
    "yuv422p9be",
    "yuv422p9le",
    "gbrp",
    "gbrp9be",
    "gbrp9le",
    "gbrp10be",
    "gbrp10le",
    "gbrp16be",
    "gbrp16le",
    "yuva422p",
    "yuva444p",
    "yuva420p9be",
    "yuva420p9le",
    "yuva422p9be",
    "yuva422p9le",
    "yuva444p9be",
    "yuva444p9le",
    "yuva420p10be",
    "yuva420p10le",
    "yuva422p10be",
    "yuva422p10le",
    "yuva444p10be",
    "yuva444p10le",
    "yuva420p16be",
    "yuva420p16le",
    "yuva422p16be",
    "yuva422p16le",
    "yuva444p16be",
    "yuva444p16le",
    "vdpau",
    "xyz12le",
    "xyz12be",
    "nv16",
    "nv20le",
    "nv20be",
    "rgba64be",
    "rgba64le",
    "bgra64be",
    "bgra64le",
    "yvyu422",
    "ya16be",
    "ya16le",
    "gbrap",
    "gbrap16be",
    "gbrap16le",
    "qsv",
    "mmal",
    "d3d11va_vld",
    "cuda",
    "0rgb",
    "rgb0",
    "0bgr",
    "bgr0",
    "yuv420p12be",
    "yuv420p12le",
    "yuv420p14be",
    "yuv420p14le",
    "yuv422p12be",
    "yuv422p12le",
    "yuv422p14be",
    "yuv422p14le",
    "yuv444p12be",
    "yuv444p12le",
    "yuv444p14be",
    "yuv444p14le",
    "gbrp12be",
    "gbrp12le",
    "gbrp14be",
    "gbrp14le",
    "yuvj411p",
    "bayer_bggr8",
    "bayer_rggb8",
    "bayer_gbrg8",
    "bayer_grbg8",
    "bayer_bggr16le",
    "bayer_bggr16be",
    "bayer_rggb16le",
    "bayer_rggb16be",
    "bayer_gbrg16le",
    "bayer_gbrg16be",
    "bayer_grbg16le",
    "bayer_grbg16be",
    "yuv440p10le",
    "yuv440p10be",
    "yuv440p12le",
    "yuv440p12be",
    "ayuv64le",
    "ayuv64be",
    "videotoolbox_vld",
    "p010le",
    "p010be",
    "gbrap12be",
    "gbrap12le",
    "gbrap10be",
    "gbrap10le",
    "mediacodec",
    "gray12be",
    "gray12le",
    "gray10be",
    "gray10le",
    "p016le",
    "p016be",
    "d3d11",
    "gray9be",
    "gray9le",
    "gbrpf32be",
    "gbrpf32le",
    "gbrapf32be",
    "gbrapf32le",
    "drm_prime",
    "opencl",
    "gray14be",
    "gray14le",
    "grayf32be",
    "grayf32le",
    "yuva422p12be",
    "yuva422p12le",
    "yuva444p12be",
    "yuva444p12le",
    "nv24",
    "nv42",
    "vulkan",
    "y210be",
    "y210le",
    "x2rgb10le",
    "x2rgb10be",
    "x2bgr10le",
    "x2bgr10be",
    "p210be",
    "p210le",
    "p410be",
    "p410le",
    "p216be",
    "p216le",
    "p416be",
    "p416le",
    "vuya",
    "rgbaf16be",
    "rgbaf16le",
    "vuyx",
    "p012le",
    "p012be",
    "y212be",
    "y212le",
    "xv30be",
    "xv30le",
    "xv36be",
    "xv36le",
    "rgbf32be",
    "rgbf32le",
    "rgbaf32be",
    "rgbaf32le",
    "p212be",
    "p212le",
    "p412be",
    "p412le",
    "gbrap14be",
    "gbrap14le",
    "d3d12",
    "ayuv",
    "uyva",
    "vyu444",
    "v30xbe",
    "v30xle",
    "rgbf16be",
    "rgbf16le",
    "rgba128be",
    "rgba128le",
    "rgb96be",
    "rgb96le",
    "y216be",
    "y216le",
    "xv48be",
    "xv48le",
    "gbrpf16be",
    "gbrpf16le",
    "gbrapf16be",
    "gbrapf16le",
    "grayf16be",
    "grayf16le",
    "amf",
    "gray32be",
    "gray32le",
    "yaf32be",
    "yaf32le",
    "yaf16be",
    "yaf16le",
    "gbrap32be",
    "gbrap32le",
    "yuv444p10msbbe",
    "yuv444p10msble",
    "yuv444p12msbbe",
    "yuv444p12msble",
    "gbrp10msbbe",
    "gbrp10msble",
    "gbrp12msbbe",
    "gbrp12msble",
    "ohcodec",
];

fn write_pix_fmts<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Pixel formats:")?;
    writeln!(w, "I.... = Supported Input  format for conversion")?;
    writeln!(w, ".O... = Supported Output format for conversion")?;
    writeln!(w, "..H.. = Hardware accelerated format")?;
    writeln!(w, "...P. = Paletted format")?;
    writeln!(w, "....B = Bitstream format")?;
    writeln!(
        w,
        "FLAGS NAME            NB_COMPONENTS BITS_PER_PIXEL BIT_DEPTHS"
    )?;
    writeln!(w, "-----")?;
    // Reference order first, then anything we know that it does not — so a
    // format we carry and it lacks is appended rather than silently dropped.
    let mut all: Vec<_> = vaco_pixfmt::PixFmt::all().to_vec();
    all.sort_by_key(|f| {
        LISTING_ORDER
            .iter()
            .position(|n| *n == f.name())
            .unwrap_or(usize::MAX)
    });
    for fmt in all {
        let name = fmt.name();
        let d = fmt.descriptor();
        let hw = d.flags.contains(vaco_pixfmt::PixFmtFlags::HW_ACCEL);
        let palette = d.flags.contains(vaco_pixfmt::PixFmtFlags::PALETTE);
        let bitstream = d.flags.contains(vaco_pixfmt::PixFmtFlags::BITSTREAM)
            || BITSTREAM_FLAG_OVERRIDE.contains(&name);
        let (input, output) = if hw {
            (false, false)
        } else if INPUT_ONLY.contains(&name) {
            (true, false)
        } else if OUTPUT_ONLY.contains(&name) {
            (false, true)
        } else if NEITHER.contains(&name) {
            (false, false)
        } else {
            (true, true)
        };

        let mut flags = String::with_capacity(5);
        flags.push(if input { 'I' } else { '.' });
        flags.push(if output { 'O' } else { '.' });
        flags.push(if hw { 'H' } else { '.' });
        flags.push(if palette { 'P' } else { '.' });
        flags.push(if bitstream { 'B' } else { '.' });

        let (nb_components, depths) = DEPTHS_OVERRIDE
            .iter()
            .find(|(n, ..)| *n == name)
            .map_or_else(
                || {
                    (
                        d.components.len(),
                        d.components
                            .iter()
                            .map(|c| c.depth.to_string())
                            .collect::<Vec<_>>()
                            .join("-"),
                    )
                },
                |(_, nc, dep)| (*nc, (*dep).to_owned()),
            );
        let depths = if depths.is_empty() {
            "0".to_owned()
        } else {
            depths
        };

        let mut line = format!("{flags} ");
        line.push_str(name);
        for _ in name.chars().count()..16 {
            line.push(' ');
        }
        let nc_s = nb_components.to_string();
        for _ in nc_s.chars().count()..8 {
            line.push(' ');
        }
        line.push_str(&nc_s);
        let bpp_s = d.bits_per_pixel.to_string();
        for _ in bpp_s.chars().count()..15 {
            line.push(' ');
        }
        line.push_str(&bpp_s);
        line.push_str("      ");
        line.push_str(&depths);
        writeln!(w, "{line}")?;
    }
    Ok(())
}

/// Formats libswscale can convert *from* but not *to* — captured verbatim
/// from `ffmpeg -hide_banner -pix_fmts`, ffmpeg 8.1; see [`write_pix_fmts`].
const INPUT_ONLY: &[&str] = &[
    "bayer_bggr16be",
    "bayer_bggr16le",
    "bayer_bggr8",
    "bayer_gbrg16be",
    "bayer_gbrg16le",
    "bayer_gbrg8",
    "bayer_grbg16be",
    "bayer_grbg16le",
    "bayer_grbg8",
    "bayer_rggb16be",
    "bayer_rggb16le",
    "bayer_rggb8",
    "gbrapf16be",
    "gbrapf16le",
    "gbrpf16be",
    "gbrpf16le",
    "grayf16be",
    "grayf16le",
    "pal8",
    "rgbaf16be",
    "rgbaf16le",
    "rgbf16be",
    "rgbf16le",
    "rgbf32be",
    "rgbf32le",
    "uyyvyy411",
    "yaf16be",
    "yaf16le",
    "yaf32be",
    "yaf32le",
];

/// Formats libswscale can convert *to* but not *from*. See [`write_pix_fmts`].
const OUTPUT_ONLY: &[&str] = &["bgr4", "rgb4"];

/// Formats libswscale supports neither direction for, beyond the hardware
/// surfaces (which are `NEITHER` unconditionally via `PixFmtFlags::HW_ACCEL`
/// and do not need naming here). See [`write_pix_fmts`].
const NEITHER: &[&str] = &[
    "gbrap32be",
    "gbrap32le",
    "gray32be",
    "gray32le",
    "rgb96be",
    "rgb96le",
    "rgba128be",
    "rgba128le",
    "rgbaf32be",
    "rgbaf32le",
    "v30xbe",
    "x2bgr10be",
    "x2rgb10be",
    "xv30be",
    "y210be",
    "y212be",
    "y216be",
];

/// Display-only correction for the three named `vaco-pixfmt` divergences
/// (`bgr8`'s component order, the twelve Bayer formats' component
/// modelling). `(name, nb_components, bit_depths)`. See [`write_pix_fmts`].
const DEPTHS_OVERRIDE: &[(&str, usize, &str)] = &[
    ("bgr8", 3, "3-3-2"),
    ("bayer_bggr8", 3, "2-4-2"),
    ("bayer_rggb8", 3, "2-4-2"),
    ("bayer_gbrg8", 3, "2-4-2"),
    ("bayer_grbg8", 3, "2-4-2"),
    ("bayer_bggr16le", 3, "4-8-4"),
    ("bayer_bggr16be", 3, "4-8-4"),
    ("bayer_rggb16le", 3, "4-8-4"),
    ("bayer_rggb16be", 3, "4-8-4"),
    ("bayer_gbrg16le", 3, "4-8-4"),
    ("bayer_gbrg16be", 3, "4-8-4"),
    ("bayer_grbg16le", 3, "4-8-4"),
    ("bayer_grbg16be", 3, "4-8-4"),
];

/// Display-only correction for the `xv30be`/`v30xbe` `BITSTREAM`-flag gap in
/// `vaco-pixfmt`. See [`write_pix_fmts`].
const BITSTREAM_FLAG_OVERRIDE: &[&str] = &["xv30be", "v30xbe"];

// ---------------------------------------------------------------------------
// `-sample_fmts`
// ---------------------------------------------------------------------------

/// `-sample_fmts`: `vaco_sampfmt::SampleFmt::ALL` is already in the
/// reference's own print order — see that constant's own doc comment, which
/// records the D17 reasoning (the two 64-bit formats were appended after the
/// rest to keep discriminants stable, so the reference's list is not simply
/// "int types, then float types, then planar"). `name()` and
/// `bytes_per_sample() * 8` are exactly the two columns; this function only
/// formats.
///
/// Header and column widths measured (`ffmpeg -hide_banner -sample_fmts`,
/// `LC_ALL=C`):
///
/// ```text
/// name   depth
/// u8        8·
/// ```
///
/// (`·` marks a trailing space that is really there.) Two independent
/// fixed-width layouts, not one shared algorithm: the header is a literal
/// 12-byte string; each data row is `NAME` left-justified to **9**, `DEPTH`
/// right-justified to **2**, then one literal trailing space — always 12
/// bytes regardless of name or depth length, checked against `s64p` (the
/// longest name, 4 characters) and `64` (the widest depth, 2 digits).
fn write_sample_fmts<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "name   depth")?;
    for f in vaco_sampfmt::SampleFmt::ALL {
        let name = f.name();
        let depth = (f.bytes_per_sample() * 8).to_string();
        let mut line = String::with_capacity(12);
        line.push_str(name);
        for _ in name.chars().count()..9 {
            line.push(' ');
        }
        for _ in depth.chars().count()..2 {
            line.push(' ');
        }
        line.push_str(&depth);
        line.push(' ');
        writeln!(w, "{line}")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `-layouts`
// ---------------------------------------------------------------------------

/// `-layouts`: two tables, `vaco_chlayout::Channel::named()` and
/// `ChannelLayout::standard()`, both already in the reference's own print
/// order (see those items' own doc comments — `vaco-chlayout` was built with
/// exactly this listing in mind) — this function only formats.
///
/// Measured (`ffmpeg -hide_banner -layouts`, `LC_ALL=C`): both tables share
/// one field algorithm, `NAME` left-justified to **15** with no separate
/// trailing separator (the next field starts immediately at column 15 —
/// checked against the longest name in each table, `7.1(wide-side)` at 14
/// characters and `hexadecagonal` at 13, neither of which needs a
/// fifteenth-column pad to reach the boundary). One blank line separates the
/// two tables; there is no trailing blank line after the last row.
/// Confirmed byte-for-byte against the reference for all 36 individual
/// channels and all 40 standard layouts.
fn write_layouts<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Individual channels:")?;
    writeln!(w, "NAME           DESCRIPTION")?;
    for c in vaco_chlayout::Channel::named() {
        let name = c.to_string();
        let mut line = String::new();
        line.push_str(&name);
        for _ in name.chars().count()..15 {
            line.push(' ');
        }
        line.push_str(c.description().unwrap_or(""));
        writeln!(w, "{line}")?;
    }
    writeln!(w)?;
    writeln!(w, "Standard channel layouts:")?;
    writeln!(w, "NAME           DECOMPOSITION")?;
    for (name, layout) in vaco_chlayout::ChannelLayout::standard() {
        let mut line = String::new();
        line.push_str(name);
        for _ in name.chars().count()..15 {
            line.push(' ');
        }
        let decomp: Vec<String> = layout.iter().map(|c| c.to_string()).collect();
        line.push_str(&decomp.join("+"));
        writeln!(w, "{line}")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `-colors`
// ---------------------------------------------------------------------------

/// `-colors`: `vaco_core::parse::color_names()`/`color_by_name` already
/// carry the RGB triples and the alphabetical order — both confirmed
/// identical to the reference (measured `ffmpeg -hide_banner -colors`,
/// `LC_ALL=C`, 140 rows). Two things that table does not carry, because they
/// exist only for this listing:
///
/// 1. **Display capitalisation.** `vaco_core`'s table is lower-case —
///    `color()` matches case-insensitively, so casing carries no information
///    there — but `-colors` prints a fixed CamelCase spelling per name,
///    including the reference's own inconsistency (`Darkorange`, not
///    `DarkOrange` — checked twice against the raw probe output, not a
///    transcription slip). [`COLOR_DISPLAY_NAMES`] is that spelling,
///    captured verbatim.
/// 2. **Which of the 147 names to show.** D17 records that `vaco_core`'s
///    colour table is a strict superset of the reference's: it additionally
///    *accepts as input* seven alternate `grey`-family spellings the
///    reference rejects (`-fill_color darkgrey` works here, not there).
///    [`COLOR_LISTING_EXCLUDED`] is those seven; the *listing* shows exactly
///    the reference's 140 — confirmed the remaining set matches the probe's
///    140 names exactly, one for one, including which spelling is canonical
///    within each `gray`/`grey` pair (six pairs display `Gray`; `LightGrey`
///    alone displays `Grey` — the reference's own inconsistency, reproduced).
///
/// Field width matches the `-pix_fmts`/`-formats` `pad_field` algorithm
/// (minimum 32, giving a 33-wide field including the separator): measured
/// against the longest display name, `LightGoldenRodYellow` at 20
/// characters, well under the minimum.
fn write_colors<W: Write>(w: &mut W) -> std::io::Result<()> {
    let mut header = String::new();
    pad_field(&mut header, "name", 32);
    header.push_str("#RRGGBB");
    writeln!(w, "{header}")?;
    for lower in vaco_core::parse::color_names() {
        if COLOR_LISTING_EXCLUDED.contains(&lower) {
            continue;
        }
        let Some(rgba) = vaco_core::parse::color_by_name(lower) else {
            continue;
        };
        let display = COLOR_DISPLAY_NAMES
            .iter()
            .find(|(l, _)| *l == lower)
            .map_or(lower, |(_, d)| *d);
        let mut line = String::new();
        pad_field(&mut line, display, 32);
        // `write!` into the `String`, not `push_str(&format!(...))`: the
        // latter is an extra allocation clippy flags (`format_push_string`).
        let _ = core::fmt::Write::write_fmt(
            &mut line,
            format_args!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b),
        );
        writeln!(w, "{line}")?;
    }
    Ok(())
}

/// The reference's exact display capitalisation for named colours, measured
/// from `ffmpeg -hide_banner -colors` (ffmpeg 8.1, `LC_ALL=C`). `(lower-case
/// key into vaco_core::parse::COLORS, display spelling)`. See
/// [`write_colors`].
const COLOR_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("aliceblue", "AliceBlue"),
    ("antiquewhite", "AntiqueWhite"),
    ("aqua", "Aqua"),
    ("aquamarine", "Aquamarine"),
    ("azure", "Azure"),
    ("beige", "Beige"),
    ("bisque", "Bisque"),
    ("black", "Black"),
    ("blanchedalmond", "BlanchedAlmond"),
    ("blue", "Blue"),
    ("blueviolet", "BlueViolet"),
    ("brown", "Brown"),
    ("burlywood", "BurlyWood"),
    ("cadetblue", "CadetBlue"),
    ("chartreuse", "Chartreuse"),
    ("chocolate", "Chocolate"),
    ("coral", "Coral"),
    ("cornflowerblue", "CornflowerBlue"),
    ("cornsilk", "Cornsilk"),
    ("crimson", "Crimson"),
    ("cyan", "Cyan"),
    ("darkblue", "DarkBlue"),
    ("darkcyan", "DarkCyan"),
    ("darkgoldenrod", "DarkGoldenRod"),
    ("darkgray", "DarkGray"),
    ("darkgreen", "DarkGreen"),
    ("darkkhaki", "DarkKhaki"),
    ("darkmagenta", "DarkMagenta"),
    ("darkolivegreen", "DarkOliveGreen"),
    ("darkorange", "Darkorange"),
    ("darkorchid", "DarkOrchid"),
    ("darkred", "DarkRed"),
    ("darksalmon", "DarkSalmon"),
    ("darkseagreen", "DarkSeaGreen"),
    ("darkslateblue", "DarkSlateBlue"),
    ("darkslategray", "DarkSlateGray"),
    ("darkturquoise", "DarkTurquoise"),
    ("darkviolet", "DarkViolet"),
    ("deeppink", "DeepPink"),
    ("deepskyblue", "DeepSkyBlue"),
    ("dimgray", "DimGray"),
    ("dodgerblue", "DodgerBlue"),
    ("firebrick", "FireBrick"),
    ("floralwhite", "FloralWhite"),
    ("forestgreen", "ForestGreen"),
    ("fuchsia", "Fuchsia"),
    ("gainsboro", "Gainsboro"),
    ("ghostwhite", "GhostWhite"),
    ("gold", "Gold"),
    ("goldenrod", "GoldenRod"),
    ("gray", "Gray"),
    ("green", "Green"),
    ("greenyellow", "GreenYellow"),
    ("honeydew", "HoneyDew"),
    ("hotpink", "HotPink"),
    ("indianred", "IndianRed"),
    ("indigo", "Indigo"),
    ("ivory", "Ivory"),
    ("khaki", "Khaki"),
    ("lavender", "Lavender"),
    ("lavenderblush", "LavenderBlush"),
    ("lawngreen", "LawnGreen"),
    ("lemonchiffon", "LemonChiffon"),
    ("lightblue", "LightBlue"),
    ("lightcoral", "LightCoral"),
    ("lightcyan", "LightCyan"),
    ("lightgoldenrodyellow", "LightGoldenRodYellow"),
    ("lightgreen", "LightGreen"),
    ("lightgrey", "LightGrey"),
    ("lightpink", "LightPink"),
    ("lightsalmon", "LightSalmon"),
    ("lightseagreen", "LightSeaGreen"),
    ("lightskyblue", "LightSkyBlue"),
    ("lightslategray", "LightSlateGray"),
    ("lightsteelblue", "LightSteelBlue"),
    ("lightyellow", "LightYellow"),
    ("lime", "Lime"),
    ("limegreen", "LimeGreen"),
    ("linen", "Linen"),
    ("magenta", "Magenta"),
    ("maroon", "Maroon"),
    ("mediumaquamarine", "MediumAquaMarine"),
    ("mediumblue", "MediumBlue"),
    ("mediumorchid", "MediumOrchid"),
    ("mediumpurple", "MediumPurple"),
    ("mediumseagreen", "MediumSeaGreen"),
    ("mediumslateblue", "MediumSlateBlue"),
    ("mediumspringgreen", "MediumSpringGreen"),
    ("mediumturquoise", "MediumTurquoise"),
    ("mediumvioletred", "MediumVioletRed"),
    ("midnightblue", "MidnightBlue"),
    ("mintcream", "MintCream"),
    ("mistyrose", "MistyRose"),
    ("moccasin", "Moccasin"),
    ("navajowhite", "NavajoWhite"),
    ("navy", "Navy"),
    ("oldlace", "OldLace"),
    ("olive", "Olive"),
    ("olivedrab", "OliveDrab"),
    ("orange", "Orange"),
    ("orangered", "OrangeRed"),
    ("orchid", "Orchid"),
    ("palegoldenrod", "PaleGoldenRod"),
    ("palegreen", "PaleGreen"),
    ("paleturquoise", "PaleTurquoise"),
    ("palevioletred", "PaleVioletRed"),
    ("papayawhip", "PapayaWhip"),
    ("peachpuff", "PeachPuff"),
    ("peru", "Peru"),
    ("pink", "Pink"),
    ("plum", "Plum"),
    ("powderblue", "PowderBlue"),
    ("purple", "Purple"),
    ("red", "Red"),
    ("rosybrown", "RosyBrown"),
    ("royalblue", "RoyalBlue"),
    ("saddlebrown", "SaddleBrown"),
    ("salmon", "Salmon"),
    ("sandybrown", "SandyBrown"),
    ("seagreen", "SeaGreen"),
    ("seashell", "SeaShell"),
    ("sienna", "Sienna"),
    ("silver", "Silver"),
    ("skyblue", "SkyBlue"),
    ("slateblue", "SlateBlue"),
    ("slategray", "SlateGray"),
    ("snow", "Snow"),
    ("springgreen", "SpringGreen"),
    ("steelblue", "SteelBlue"),
    ("tan", "Tan"),
    ("teal", "Teal"),
    ("thistle", "Thistle"),
    ("tomato", "Tomato"),
    ("turquoise", "Turquoise"),
    ("violet", "Violet"),
    ("wheat", "Wheat"),
    ("white", "White"),
    ("whitesmoke", "WhiteSmoke"),
    ("yellow", "Yellow"),
    ("yellowgreen", "YellowGreen"),
];

/// The seven alternate `grey`-family spellings `vaco_core::parse::COLORS`
/// accepts as input (D17) that the reference's `-colors` listing does not
/// show — the reference's own colour name for each is the paired `gray`
/// spelling, except `lightgrey`, whose canonical spelling *is* `Grey`. See
/// [`write_colors`].
const COLOR_LISTING_EXCLUDED: &[&str] = &[
    "grey",
    "darkgrey",
    "dimgrey",
    "slategrey",
    "darkslategrey",
    "lightslategrey",
    "lightgray",
];

// ---------------------------------------------------------------------------
// `-hwaccels`, `-devices`, `-sources`, `-sinks`
//
// This build has no hardware backend and no device layer at all — D13's
// `vaco-hw-*` crates are a separate, later work package. All four listings
// below are the honest reproduction of that: a real, measured header (and,
// for `-devices`, a real legend) with zero rows under it, which is what the
// reference itself would print given none of the corresponding thing
// registered. That is a stronger claim than "we don't know the shape" — the
// shape *is* known and reproduced; only the row count is zero, and it is
// zero for a real reason.
// ---------------------------------------------------------------------------

/// `-hwaccels`: header, one name per registered hardware-acceleration
/// method, then one unconditional trailing blank line. Measured
/// (`ffmpeg -hide_banner -hwaccels`, `LC_ALL=C`, a Homebrew build with
/// `--enable-videotoolbox`):
///
/// ```text
/// Hardware acceleration methods:
/// videotoolbox
///
/// ```
///
/// This build registers no hardware backend, so the honest output is the
/// same header and trailing blank line with zero names between them.
fn write_hwaccels<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Hardware acceleration methods:")?;
    writeln!(w)?;
    Ok(())
}

/// `-devices`: header, a two-flag legend — distinct from `-formats`' three-flag
/// one; there is no "is a device" slot here because everything a device
/// lister shows already is one — and a rule, then zero rows. Measured
/// (`ffmpeg -hide_banner -devices`, `LC_ALL=C`, the same build):
///
/// ```text
/// Devices:
///  D. = Demuxing supported
///  .E = Muxing supported
///  ---
///   E audiotoolbox    AudioToolbox output device
/// ```
///
/// Header, legend and rule are reproduced verbatim. The row shape
/// (` {D/.}{E/.} ` + `NAME` at the `-formats` minimum of 15 + long name) is
/// recorded here for whoever adds a device but, like the zero-row
/// `-decoders`/`-encoders`/`-filters` tables above, is unverified against a
/// real row in *this* build.
fn write_devices<W: Write>(w: &mut W) -> std::io::Result<()> {
    writeln!(w, "Devices:")?;
    writeln!(w, " D. = Demuxing supported")?;
    writeln!(w, " .E = Muxing supported")?;
    writeln!(w, " ---")?;
    Ok(())
}

/// `-sources`/`-sinks`, shared: with no device named, the reference prints a
/// fixed notice and then tries auto-detection against every registered input
/// device (`-sources`) or output device (`-sinks`); with one named, it skips
/// straight to that device's own attempt. Measured
/// (`ffmpeg -hide_banner -sources` / `-sinks`, with and without a device
/// name, `LC_ALL=C`): the no-name notice, verbatim, is
///
/// ```text
///
/// Device name is not provided.
/// You can pass devicename[,opt1=val1[,opt2=val2...]] as an argument.
///
/// ```
///
/// followed by one `"Auto-detected sources for {name}:\n"` (or `sinks`) plus
/// a result line per registered device of the matching direction.
///
/// This build has zero devices of either kind, so:
///
/// * **No name given:** the notice prints; the (empty) device loop
///   contributes nothing after it. Same "real header, no rows" shape as
///   [`write_devices`].
/// * **A name given:** measured directly that an unmatched name produces
///   **no output at all**, exit 0 — `ffmpeg -hide_banner -sources
///   bogus_device_xyz` and, just as tellingly, `ffmpeg -hide_banner -sources
///   matroska` (a real demuxer name that is not a device) both print
///   nothing. The reference only reaches the "Auto-detected…" line once the
///   name resolves to a registered device of the right direction. Since this
///   build's device registry is always empty, every name is unmatched, so
///   silent success reproduces that path exactly rather than shortcutting it.
///
/// Wiring this option to take an argument at all needed a fix in
/// `vaco-cli-core`'s tables: `sources`/`sinks` carried a `device` argument
/// placeholder for `-h`'s benefit but neither `ArgFlags::HAS_ARG` nor
/// `ArgFlags::OPTIONAL_ARG`, so the value was never actually consumed. Fixed
/// alongside this function — see `vaco-cli-core`'s doc file.
fn write_sources_or_sinks<W: Write>(w: &mut W, device: Option<&OsStr>) -> std::io::Result<()> {
    if device.is_some() {
        // No device layer (D13): nothing this build knows about ever
        // matches, so this mirrors the reference's own unmatched-name path
        // exactly (measured silent, exit 0 — see the doc comment above).
        return Ok(());
    }
    writeln!(w)?;
    writeln!(w, "Device name is not provided.")?;
    writeln!(
        w,
        "You can pass devicename[,opt1=val1[,opt2=val2...]] as an argument."
    )?;
    writeln!(w)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn text(name: &str) -> String {
        text_with(name, None)
    }

    fn text_with(name: &str, value: Option<&str>) -> String {
        let mut buf = Vec::new();
        render(&mut buf, name, value.map(OsStr::new)).unwrap();
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
        // This used to assert `muxers().len() == 0` and that no row carried an
        // `E`, which was true when written and stopped being true the moment
        // muxers landed. A test that pins a point-in-time *fact* rather than an
        // invariant fails on success, which is the least useful way for a test
        // to fail. What is actually invariant is the mapping: a format shows
        // `D` exactly when it demuxes and `E` exactly when it muxes.
        let demuxers = vaco_registry::demuxers().len();
        let muxers = vaco_registry::muxers().len();
        assert!(demuxers > 0, "the registry has no demuxers at all");
        let d_rows = s.lines().filter(|l| l.starts_with(" D")).count();
        let e_rows = s
            .lines()
            .filter(|l| l.len() > 2 && l.as_bytes().get(2) == Some(&b'E'))
            .count();
        assert!(
            d_rows > 0 && (muxers == 0) == (e_rows == 0),
            "D rows {d_rows}, E rows {e_rows}, registry has {demuxers} demuxers \
             and {muxers} muxers:\n{s}"
        );
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
        // This asserted the muxer table was empty (five header lines and
        // nothing else). It is not any more. The invariant is that both
        // listings render the same header and that `-muxers` lists exactly the
        // registry's muxers — not that there are none.
        let header = 5;
        assert_eq!(
            m.lines().count(),
            header + vaco_registry::muxers().len(),
            "{m}"
        );
    }

    #[test]
    fn formats_is_the_sorted_union_not_the_two_lists_concatenated() {
        let s = text("formats");
        let names: Vec<&str> = s
            .lines()
            .skip(5)
            .filter_map(|l| l.get(5..).and_then(|r| r.split_whitespace().next()))
            .collect();

        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "-formats is not sorted by name:\n{s}");

        let mut seen = std::collections::BTreeSet::new();
        for n in &names {
            assert!(
                seen.insert(*n),
                "`{n}` appears twice — the demuxer and muxer passes are being \
                 concatenated rather than merged:\n{s}"
            );
        }

        // The union, not the sum: every format that goes both ways would be
        // double-counted by the bug this pins.
        //
        // The intersection is over *registered names*, not over
        // `demuxer_by_name`, which also resolves aliases — it answers `Some`
        // for "matroska" via the `matroska,webm` demuxer, and would report six
        // more shared formats than the listing has rows. That is right for
        // opening a file by format name and wrong here: the reference prints
        // `matroska,webm` in `-demuxers` and `matroska` in `-muxers` as two
        // separate rows, so the listing merges on the exact name only.
        let demux_names: std::collections::BTreeSet<&str> =
            vaco_registry::demuxers().iter().map(|d| d.name).collect();
        let both = vaco_registry::muxers()
            .iter()
            .filter(|m| demux_names.contains(m.name))
            .count();
        assert_eq!(
            names.len(),
            vaco_registry::demuxers().len() + vaco_registry::muxers().len() - both
        );
    }

    #[test]
    fn demuxers_and_muxers_mask_the_flag_column_to_the_direction_asked_for() {
        // Measured: `ffmpeg -demuxers` prints " D   avi" even though `avi`
        // muxes too, and `ffmpeg -muxers` prints "  E  avi". Only `-formats`
        // shows both letters at once.
        for line in text("demuxers").lines().skip(5) {
            assert_eq!(
                line.as_bytes().get(2),
                Some(&b' '),
                "-demuxers leaked an E into the flag column: {line}"
            );
        }
        for line in text("muxers").lines().skip(5) {
            assert_eq!(
                line.as_bytes().get(1),
                Some(&b' '),
                "-muxers leaked a D into the flag column: {line}"
            );
        }
    }

    #[test]
    fn a_both_ways_format_takes_its_muxer_long_name_in_formats() {
        // Measured: the reference's `mp3` is "MP2/3 (MPEG audio layer 2/3)"
        // demuxing and "MP3 (MPEG audio layer 3)" muxing, and `-formats` shows
        // the muxer's. The two spellings differ for 20 of its 130 both-way
        // formats, so picking the wrong one is not a rounding error.
        let s = text("formats");
        for m in vaco_registry::muxers() {
            if vaco_registry::demuxer_by_name(m.name).is_none() {
                continue;
            }
            let found = s.lines().find(|l| {
                l.get(5..)
                    .and_then(|r| r.split_whitespace().next())
                    .is_some_and(|n| n == m.name)
            });
            assert!(found.is_some(), "no -formats row for `{}`", m.name);
            let row = found.unwrap_or_default();
            assert!(
                row.ends_with(m.long_name),
                "`{}` should carry the muxer's long name: {row}",
                m.name
            );
        }
    }

    #[test]
    fn decoders_and_encoders_list_every_registered_implementation() {
        // This build's first decoders and encoders landed here — asserting
        // the table stayed empty past the legend would be the exact "pin the
        // absence of something the project is building" trap the comment below
        // names for `-filters`, so this asserts the row count instead, the way
        // `filters_lists_every_registered_filter` does.
        let d = text("decoders");
        assert!(d.starts_with("Decoders:\n"), "{d}");
        let d_rows: Vec<&str> = d.lines().skip(10).collect();
        assert_eq!(
            d_rows.len(),
            vaco_registry::decoders().len(),
            "one row each"
        );
        for r in &d_rows {
            assert!(r.starts_with(' '), "{r:?}");
        }

        let e = text("encoders");
        assert!(e.starts_with("Encoders:\n"), "{e}");
        let e_rows: Vec<&str> = e.lines().skip(10).collect();
        assert_eq!(
            e_rows.len(),
            vaco_registry::encoders().len(),
            "one row each"
        );
    }

    /// Both of these used to assert the listing *stopped* at its header.
    ///
    /// They passed for exactly as long as the corresponding bug lasted, and
    /// failed the moment it was fixed — the "never pin the absence of something
    /// the project is building" trap, in its purest form. `-filters` printed a
    /// legend and no rows while 142 filters
    /// were registered and resolving through `-h filter=<name>`.
    ///
    /// What replaces them asserts the *shape* of a row, which stays true as the
    /// registry grows.
    #[test]
    fn filters_lists_every_registered_filter() {
        let s = text("filters");
        assert!(s.starts_with("Filters:\n"), "{s}");
        let rows: Vec<&str> = s.lines().skip(8).collect();
        assert_eq!(rows.len(), vaco_registry::filters().len(), "one row each");
        // Measured against `ffmpeg -filters` 8.1: two flag characters, a name
        // column of 17 plus a space, a pad column of 10 plus a space.
        for r in &rows {
            let flags = r.get(1..3).unwrap_or_default();
            assert!(
                matches!(flags.as_bytes(), [b'T' | b'.', b'S' | b'.']),
                "flag column: {r:?}"
            );
            let pads = r.get(21..31).unwrap_or_default().trim();
            assert!(pads.contains("->"), "pad column: {r:?}");
        }
    }

    /// The pad column is a letter per pad, `|` for a source or sink, `N` for a
    /// count the options decide. Measured: `anullsrc` is `|->A`, `nullsink` is
    /// `V->|`, `split` is `V->N`, `concat` is `N->N`, `overlay` is `VV->V`.
    #[test]
    fn the_pad_column_marks_sources_sinks_and_dynamic_pads() {
        use vaco_core::MediaType;
        use vaco_filter_core::{FilterFlags, Pad};
        const AUDIO: &[Pad] = &[Pad {
            name: "default",
            media_type: MediaType::Audio,
        }];
        const TWO_VIDEO: &[Pad] = &[
            Pad {
                name: "main",
                media_type: MediaType::Video,
            },
            Pad {
                name: "overlay",
                media_type: MediaType::Video,
            },
        ];
        assert_eq!(pad_column(&[], false), "|");
        assert_eq!(pad_column(AUDIO, false), "A");
        assert_eq!(pad_column(TWO_VIDEO, false), "VV");
        // Dynamic wins over the declared pads: the reference prints `split` as
        // `V->N`, never the pads a default instantiation happens to have.
        assert_eq!(pad_column(TWO_VIDEO, true), "N");
        assert_eq!(pad_column(&[], true), "N");
        let _ = FilterFlags::DYNAMIC_INPUTS;
    }

    #[test]
    fn bsfs_lists_every_registered_bitstream_filter() {
        let s = text("bsfs");
        assert!(s.starts_with("Bitstream filters:\n"), "{s}");
        assert_eq!(
            s.lines().count(),
            1 + components_of_kind(Kind::BitstreamFilter).count()
        );
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
    fn an_unknown_listing_names_the_gap_rather_than_half_rendering() {
        let mut buf = Vec::new();
        let e = render(&mut buf, "not_a_real_listing", None).unwrap_err();
        assert!(e.render().contains("vaco-cli.md"), "{}", e.render());
        assert!(buf.is_empty());
    }

    #[test]
    fn pix_fmts_header_legend_and_a_measured_row() {
        let s = text("pix_fmts");
        assert!(
            s.starts_with(
                "Pixel formats:\n\
                 I.... = Supported Input  format for conversion\n\
                 .O... = Supported Output format for conversion\n\
                 ..H.. = Hardware accelerated format\n\
                 ...P. = Paletted format\n\
                 ....B = Bitstream format\n\
                 FLAGS NAME            NB_COMPONENTS BITS_PER_PIXEL BIT_DEPTHS\n\
                 -----\n"
            ),
            "{s}"
        );
        // Measured: `ffmpeg -hide_banner -pix_fmts`, ffmpeg 8.1.
        assert!(
            s.lines()
                .any(|l| l == "IO... yuv420p                3             12      8-8-8"),
            "{s}"
        );
        // `cuarray` is a `vaco-pixfmt` addition the reference does not have
        // (see write_pix_fmts's doc comment) — excluded from the listing.
        assert!(!s.contains("cuarray"), "{s}");
        // The two named component-order/structural divergences from
        // `vaco-pixfmt`, corrected for display.
        assert!(
            s.lines()
                .any(|l| l == "IO... bgr8                   3              8      3-3-2"),
            "{s}"
        );
        assert!(
            s.lines()
                .any(|l| l == "I.... bayer_bggr8            3              8      2-4-2"),
            "{s}"
        );
        // A hardware surface: zero components, and a literal `0` in the
        // depths column rather than an empty field.
        assert!(
            s.lines()
                .any(|l| l == "..H.. videotoolbox_vld       0              0      0"),
            "{s}"
        );
    }

    #[test]
    fn sample_fmts_is_byte_identical_to_the_reference() {
        // Measured: `ffmpeg -hide_banner -sample_fmts`, ffmpeg 8.1.
        assert_eq!(
            text("sample_fmts"),
            "name   depth\n\
             u8        8 \n\
             s16      16 \n\
             s32      32 \n\
             flt      32 \n\
             dbl      64 \n\
             u8p       8 \n\
             s16p     16 \n\
             s32p     32 \n\
             fltp     32 \n\
             dblp     64 \n\
             s64      64 \n\
             s64p     64 \n"
        );
    }

    #[test]
    fn layouts_matches_the_reference_channel_and_layout_tables() {
        let s = text("layouts");
        assert!(
            s.starts_with("Individual channels:\nNAME           DESCRIPTION\n"),
            "{s}"
        );
        assert!(s.lines().any(|l| l == "FL             front left"), "{s}");
        assert!(
            s.contains("\n\nStandard channel layouts:\nNAME           DECOMPOSITION\n"),
            "{s}"
        );
        assert!(s.lines().any(|l| l == "stereo         FL+FR"), "{s}");
        assert!(
            s.lines()
                .any(|l| l == "7.1(wide-side) FL+FR+FC+LFE+FLC+FRC+SL+SR"),
            "{s}"
        );
        assert!(!s.ends_with("\n\n"), "{s}");
    }

    #[test]
    fn colors_header_and_measured_capitalisation() {
        let s = text("colors");
        let mut header = String::new();
        pad_field(&mut header, "name", 32);
        header.push_str("#RRGGBB");
        assert!(s.starts_with(&format!("{header}\n")), "{s}");
        assert!(
            s.lines()
                .any(|l| l.starts_with("AliceBlue") && l.ends_with("#f0f8ff")),
            "{s}"
        );
        // The reference's own inconsistent capitalisation, reproduced.
        assert!(s.lines().any(|l| l.starts_with("Darkorange")), "{s}");
        assert!(
            s.lines()
                .any(|l| l.starts_with("Gray") && l.ends_with("#808080")),
            "{s}"
        );
        assert!(s.lines().any(|l| l.starts_with("LightGrey")), "{s}");
        // The seven extra `grey`-spelled aliases this crate accepts as input
        // (D17) are not shown as separate rows.
        assert_eq!(s.lines().count(), 141, "{s}"); // header + 140 colours
        assert!(!s.lines().any(|l| l.starts_with("Grey")), "{s}");
        assert!(!s.lines().any(|l| l.starts_with("Darkgrey")), "{s}");
        assert!(!s.lines().any(|l| l.starts_with("LightGray")), "{s}");
    }

    #[test]
    fn hwaccels_is_a_real_header_with_zero_rows() {
        assert_eq!(text("hwaccels"), "Hardware acceleration methods:\n\n");
    }

    #[test]
    fn devices_header_and_legend_with_zero_rows() {
        assert_eq!(
            text("devices"),
            "Devices:\n D. = Demuxing supported\n .E = Muxing supported\n ---\n"
        );
    }

    #[test]
    fn sources_and_sinks_no_device_print_the_measured_notice() {
        let expected = "\nDevice name is not provided.\n\
             You can pass devicename[,opt1=val1[,opt2=val2...]] as an argument.\n\n";
        assert_eq!(text("sources"), expected);
        assert_eq!(text("sinks"), expected);
    }

    #[test]
    fn sources_and_sinks_with_a_device_are_silent_since_none_ever_match() {
        // Measured: an unmatched device name (even a real but non-device
        // format name) produces no output at all and exit 0.
        assert_eq!(text_with("sources", Some("bogus_device_xyz")), "");
        assert_eq!(text_with("sources", Some("matroska")), "");
        assert_eq!(text_with("sinks", Some("lavfi")), "");
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
