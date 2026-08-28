//! In-process filter execution for [`crate::case::Tool::Filter`] — "ours"
//! without a subprocess.
//!
//! # What it is
//!
//! Every other [`crate::case::Tool`] compares two subprocesses given the
//! same argv. That shape does not exist for filters yet: there is no `vaco
//! -vf` CLI (a separate, larger piece of work than this harness), so "what
//! `vaco` does with this filter" can only be observed by calling the filter
//! crate's own `FilterRegistry` directly, through a real
//! `vaco_filter_core::Graph` — the same thing every filter crate's own
//! `tests/*.rs` integration test already does by hand. This module
//! generalises that by-hand pattern into something the runner can drive
//! from a manifest instead of from bespoke Rust per filter.
//!
//! # The argv convention
//!
//! A `filter`-tool case's `argv`, after `{media}` substitution, is exactly
//! nine positional tokens (not CLI flags — there is no CLI to hand them
//! to):
//!
//! ```text
//! [0] path to the generated raw input file (from a `[[media]]` `generate`
//!     that ends in `-f rawvideo`, so the bytes need no container parsing)
//! [1] filter name, e.g. "histogram"
//! [2] filter args string, e.g. "level_height=50:scale_height=0:components=1"
//!     (empty string for no args)
//! [3] input pixel format: "gray8" | "yuv444p" | "gbrp"
//! [4] input width
//! [5] input height
//! [6] output pixel format
//! [7] output width
//! [8] output height
//! ```
//!
//! Output geometry is declared rather than derived because every filter in
//! the first corpus already has a fixed, filter-specific output shape (a
//! `histogram` case knows its own `level_height`; `vectorscope` is always
//! `256x256`) — deriving it generically would mean re-implementing each
//! filter's own `configure` logic a second time in the harness, which is
//! exactly the kind of "looks measured, is not" risk this project has
//! already hit. A case that gets the declared geometry wrong fails loudly
//! (a size mismatch is caught before any byte comparison runs), not
//! silently.
//!
//! # Which filters are reachable
//!
//! [`REGISTRIES`] is the explicit, short list of `FilterRegistry`s this
//! module tries, in order, for a case's filter name. Adding a new filter
//! crate to the corpus means adding its registry here — a genuine,
//! reviewable code change, the same shape `vaco-registry`'s own generated
//! table requires a `vaco-component.toml` entry for. There is no aggregate
//! registry combining every filter crate in the tree yet (a real, separate
//! gap — see `planning/INTERFACE-GAPS.md`), so this list is deliberately
//! short rather than papering over that with something that looks complete
//! and is not.
//!
//! # How to change it
//!
//! A filter crate whose filters take more than one input frame per case
//! (`Dual`-shaped filters), or whose input needs a non-planar pixel format,
//! is out of scope for [`build_frame`]/[`extract_output`] as written — both
//! assume full-resolution planar 8-bit, 1-to-3-plane formats, which is
//! every format this project's own T3 scope/draw filters use. Extending
//! either function to a new format means adding an arm to [`parse_pixfmt`]
//! and to the plane-copy loops; nothing here special-cases a particular
//! filter.

use vaco_color::ColorInfo;
use vaco_core::{Duration, MediaType, Rational, Timestamp};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{Graph, LinkFormat};
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_frame::FramePool;
use vaco_pixfmt::PixFmt;

use crate::run::Observation;

/// The filter crates this corpus can reach. See the module doc's "Which
/// filters are reachable".
const REGISTRIES: &[&dyn FilterRegistry] = &[
    &vaco_filter_scope::ScopeRegistry,
    &vaco_filter_draw_vf::DrawVfRegistry,
    &vaco_filter_blur::BlurRegistry,
    &vaco_filter_geometry::T2GeometryRegistry,
    &vaco_filter_convolve::ConvolveRegistry,
];

fn find_registry(name: &str) -> Option<&'static dyn FilterRegistry> {
    REGISTRIES
        .iter()
        .copied()
        .find(|r| r.names().contains(&name))
}

/// Parse this module's three supported pixel-format tokens.
///
/// # Errors
/// A message naming the unsupported token, never a panic — an unknown
/// pixfmt in a manifest is a case-authoring mistake, not a harness bug.
fn parse_pixfmt(token: &str) -> Result<PixFmt, String> {
    match token {
        "gray8" => Ok(PixFmt::Gray8),
        "yuv444p" => Ok(PixFmt::Yuv444p),
        "gbrp" => Ok(PixFmt::Gbrp),
        other => Err(format!(
            "filterexec: pixel format `{other}` is not one of gray8/yuv444p/gbrp \
             (see filterexec's own doc for why this list is short)"
        )),
    }
}

fn plane_count(fmt: PixFmt) -> usize {
    match fmt {
        PixFmt::Gray8 => 1,
        _ => 3,
    }
}

/// The nine positional tokens [`crate::case::Tool::Filter`]'s argv carries,
/// after `{media}` substitution.
#[derive(Debug)]
pub struct FilterArgs<'a> {
    pub media_path: &'a str,
    pub filter_name: &'a str,
    pub filter_args: &'a str,
    pub in_pixfmt: &'a str,
    pub in_width: u32,
    pub in_height: u32,
    pub out_pixfmt: &'a str,
    pub out_width: u32,
    pub out_height: u32,
}

impl<'a> FilterArgs<'a> {
    /// Parse a case's argv into the nine positional tokens.
    ///
    /// # Errors
    /// A message naming which token is missing or malformed.
    pub fn parse(argv: &'a [String]) -> Result<Self, String> {
        let get = |i: usize, name: &str| -> Result<&'a str, String> {
            argv.get(i)
                .map(String::as_str)
                .ok_or_else(|| format!("filter case argv is missing token [{i}] ({name})"))
        };
        let get_u32 = |i: usize, name: &str| -> Result<u32, String> {
            get(i, name)?
                .parse::<u32>()
                .map_err(|e| format!("token [{i}] ({name}) is not a u32: {e}"))
        };
        Ok(Self {
            media_path: get(0, "media_path")?,
            filter_name: get(1, "filter_name")?,
            filter_args: get(2, "filter_args")?,
            in_pixfmt: get(3, "in_pixfmt")?,
            in_width: get_u32(4, "in_width")?,
            in_height: get_u32(5, "in_height")?,
            out_pixfmt: get(6, "out_pixfmt")?,
            out_width: get_u32(7, "out_width")?,
            out_height: get_u32(8, "out_height")?,
        })
    }
}

/// Run one `filter`-tool case entirely in-process and report it the same
/// shape a subprocess's [`Observation`] would have been: `stdout` carries
/// the raw output frame's bytes, plane by plane, row-major, no padding —
/// exactly the layout `ffmpeg -f rawvideo` writes for these three pixel
/// formats, so [`crate::compare::raw`] can diff the two streams with no
/// format-specific knowledge at all.
///
/// # Errors
/// A message describing what went wrong (unknown filter, format mismatch,
/// a real filter error) — the caller turns this into
/// [`crate::case::Verdict::OursFailed`], never a panic.
pub fn run(args: &FilterArgs<'_>) -> Result<Observation, String> {
    let started = std::time::Instant::now();
    let in_fmt = parse_pixfmt(args.in_pixfmt)?;
    let out_fmt = parse_pixfmt(args.out_pixfmt)?;
    let registry = find_registry(args.filter_name).ok_or_else(|| {
        format!(
            "filterexec: no registry in this corpus declares a filter named `{}` \
             (see filterexec's own `REGISTRIES` list)",
            args.filter_name
        )
    })?;

    let raw = std::fs::read(args.media_path)
        .map_err(|e| format!("reading generated media `{}`: {e}", args.media_path))?;
    let expected_len = plane_size_sum(in_fmt, args.in_width, args.in_height);
    if raw.len() != expected_len {
        return Err(format!(
            "generated media `{}` is {} bytes; {in_fmt:?} {}x{} needs {expected_len}",
            args.media_path,
            raw.len(),
            args.in_width,
            args.in_height
        ));
    }

    let filter_args = (!args.filter_args.is_empty()).then_some(args.filter_args);
    let instance = registry
        .create(&Instantiate {
            name: args.filter_name,
            instance: "conformance",
            args: filter_args,
            arguments: &[],
        })
        .map_err(|e| format!("instantiating `{}`: {e}", args.filter_name))?;

    let mut graph = Graph::new();
    let src = graph.add_source(
        "src",
        MediaType::Video,
        NodeFormats {
            outputs: vec![FormatSet::video_exact(in_fmt)],
            label: "src".into(),
            ..NodeFormats::default()
        },
    );
    let node = graph.add(instance.desc, instance.formats, instance.filter);
    let sink = graph.add_sink(
        "sink",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "sink".into(),
            ..NodeFormats::default()
        },
    );
    graph
        .connect(src, 0, node, 0)
        .map_err(|e| format!("connecting source to `{}`: {e}", args.filter_name))?;
    graph
        .connect(node, 0, sink, 0)
        .map_err(|e| format!("connecting `{}` to sink: {e}", args.filter_name))?;

    let time_base = Rational::new(1, 25);
    let source_format = LinkFormat::Video {
        format: in_fmt,
        width: args.in_width,
        height: args.in_height,
        time_base,
        frame_rate: time_base.inverse(),
        sample_aspect_ratio: Rational::ONE,
        color: ColorInfo::default(),
    };
    graph
        .set_source_format(src, source_format)
        .map_err(|e| format!("setting source format: {e}"))?;
    graph.configure().map_err(|e| format!("configuring graph: {e}"))?;

    let pool = FramePool::default();
    let mut frame = pool
        .acquire_video(in_fmt, args.in_width, args.in_height)
        .map_err(|e| format!("acquiring input frame: {e}"))?;
    fill_planes(&mut frame, in_fmt, args.in_width, args.in_height, &raw)?;
    frame.pts = Timestamp::new(0);
    frame.time_base = time_base;
    frame.duration = Duration(1);

    graph
        .send(src, frame)
        .map_err(|e| format!("sending frame: {e}"))?;
    graph
        .close_source(src, Timestamp::new(1))
        .map_err(|e| format!("closing source: {e}"))?;
    graph.run().map_err(|e| format!("running graph: {e}"))?;

    let out = graph
        .recv(sink)
        .map_err(|e| format!("receiving output frame: {e}"))?;
    let stdout = extract_output(&out, out_fmt, args.out_width, args.out_height)?;

    Ok(Observation {
        stdout,
        stderr: Vec::new(),
        exit: Some(0),
        timed_out: false,
        truncated: false,
        wall: started.elapsed(),
    })
}

fn plane_size_sum(fmt: PixFmt, width: u32, height: u32) -> usize {
    plane_count(fmt) * (width as usize) * (height as usize)
}

fn fill_planes(
    frame: &mut vaco_frame::Frame,
    fmt: PixFmt,
    width: u32,
    height: u32,
    raw: &[u8],
) -> Result<(), String> {
    // `PlaneMut::rows_mut()` yields each row's *stride*, padding included
    // (its own doc says so explicitly: splitting the padding off would
    // need a second borrow of the same allocation) -- only meaningful for
    // performance-sensitive per-pixel kernels that do not care about the
    // trailing bytes. Copying real source bytes into that raw stride was
    // a real bug here: it silently ran the source cursor past the actual
    // pixel data (and out of the buffer entirely) the moment a plane's
    // width did not line up with the allocator's row-padding boundary --
    // invisible at `64` (already aligned), immediate at `20` (the first
    // width this harness tried that was not). `row_mut(y)`, not
    // `rows_mut()`, is what stays trimmed to the meaningful `row_bytes`.
    let plane_bytes = (width as usize) * (height as usize);
    let h = height as usize;
    for p in 0..plane_count(fmt) {
        let Some(chunk) = raw.get(p * plane_bytes..(p + 1) * plane_bytes) else {
            return Err(format!("input buffer too short for plane {p}"));
        };
        let Some(mut plane) = frame.plane_mut(p) else {
            return Err(format!("frame has no plane {p}"));
        };
        for y in 0..h {
            let Some(row) = plane.row_mut(y) else {
                return Err(format!("plane {p} has no row {y}"));
            };
            let start = y * row.len();
            let Some(src) = chunk.get(start..start + row.len()) else {
                return Err(format!("plane {p} ran out of source bytes at row {y}"));
            };
            row.copy_from_slice(src);
        }
    }
    Ok(())
}

fn extract_output(
    frame: &vaco_frame::Frame,
    fmt: PixFmt,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for p in 0..plane_count(fmt) {
        let plane = frame
            .plane(p)
            .ok_or_else(|| format!("output frame has no plane {p}"))?;
        for row in plane.rows_iter() {
            out.extend_from_slice(row);
        }
    }
    let expected = plane_size_sum(fmt, width, height);
    if out.len() != expected {
        return Err(format!(
            "output frame produced {} bytes; declared {fmt:?} {width}x{height} needs {expected} \
             (declared output geometry does not match what the filter actually produced)",
            out.len()
        ));
    }
    Ok(out)
}

