//! In-process execution for [`crate::case::Tool::Filter`].
//!
//! There is no `vaco -vf` CLI, so the harness instantiates a filter registry
//! directly and runs it through a real [`vaco_filter_core::Graph`].
//!
//! After media substitution, a single-input case has nine positional tokens:
//! input path, filter name and arguments, input pixel format/width/height, and
//! output pixel format/width/height. Each additional pad adds a four-token
//! group containing its path, format, width, and height. [`FilterArgs::parse`]
//! accepts those groups, and [`run`] checks their count against the filter's
//! declared inputs so missing or surplus pads fail explicitly.
//!
//! Output geometry is declared by the case rather than duplicating each
//! filter's configuration logic in the harness. A byte-size mismatch fails
//! before comparison. Only one video output pad is supported.
//!
//! The iterated `{media}` is pad zero. Fixed additional inputs are named by an
//! axis value's `extra_media` and referenced as `{media:<id>}`, so adding them
//! does not multiply the case matrix.
//!
//! [`REGISTRIES`] is the explicit list of reachable filter registries. Adding a
//! filter crate requires adding its registry here; there is no aggregate
//! workspace registry to imply broader coverage.
//!
//! [`run`] sends one frame per input and then closes all sources. This covers
//! lockstep multi-input filters but not multi-frame `framesync` timelines.
//! Packed formats use [`PixFmt::plane_layout`] and [`PixFmt::plane_height`] for
//! byte sizes and strides. Subsampled formats use the same primitives but have
//! not yet been exercised by a case.

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
    &vaco_filter_key::KeyRegistry,
    &vaco_filter_stack::StackRegistry,
];

fn find_registry(name: &str) -> Option<&'static dyn FilterRegistry> {
    REGISTRIES
        .iter()
        .copied()
        .find(|r| r.names().contains(&name))
}

/// Parse this module's supported pixel-format tokens.
///
/// # Errors
/// A message naming the unsupported token, never a panic — an unknown
/// pixfmt in a manifest is a case-authoring mistake, not a harness bug.
fn parse_pixfmt(token: &str) -> Result<PixFmt, String> {
    match token {
        "gray8" => Ok(PixFmt::Gray8),
        "yuv444p" => Ok(PixFmt::Yuv444p),
        "gbrp" => Ok(PixFmt::Gbrp),
        // `yuva444p` is planar and 4:4:4, so it needed no new plane math,
        // only an arm here and `fmt.plane_count()` (below) reporting the
        // right count. `rgba`/`argb` are the packed case the module doc
        // describes: one plane, four interleaved components, correctly
        // sized by `plane_size_sum`'s `PixFmt::plane_layout` call below.
        "yuva444p" => Ok(PixFmt::Yuva444p),
        "rgb24" => Ok(PixFmt::Rgb24),
        "rgba" => Ok(PixFmt::Rgba),
        "argb" => Ok(PixFmt::Argb),
        "rgba64le" => Ok(PixFmt::Rgba64le),
        other => Err(format!(
            "filterexec: pixel format `{other}` is not one of gray8/yuv444p/gbrp/yuva444p/\
             rgb24/rgba/argb/rgba64le (see filterexec's own doc for why this list is short)"
        )),
    }
}

/// The real per-format plane count ([`PixFmt::plane_count`]), not a
/// hand-rolled guess.
fn plane_count(fmt: PixFmt) -> usize {
    fmt.plane_count()
}

/// One input pad's declared shape: a group of four tokens, either the
/// mandatory `[3..6)` for pad 0 (folded into [`FilterArgs`]'s own fields
/// for backward compatibility) or one of the trailing `[9..]` groups for
/// pad 1 and beyond. See [`FilterArgs::extra_inputs`].
#[derive(Debug, Clone, Copy)]
pub struct ExtraInput<'a> {
    pub media_path: &'a str,
    pub pixfmt: &'a str,
    pub width: u32,
    pub height: u32,
}

/// The positional tokens [`crate::case::Tool::Filter`]'s argv carries,
/// after `{media}`/`{media:<id>}` substitution: the mandatory nine for
/// input 0 (unchanged since before multi-input support), plus zero or more
/// trailing four-token groups in [`FilterArgs::extra_inputs`], one per
/// additional input pad in pad order. See this module's doc for the full
/// convention.
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
    /// Pads 1, 2, ... in declaration order. Empty for the (still by far
    /// the most common) single-input case.
    pub extra_inputs: Vec<ExtraInput<'a>>,
}

impl<'a> FilterArgs<'a> {
    /// Parse a case's argv into the mandatory nine tokens plus zero or more
    /// trailing four-token extra-input groups.
    ///
    /// # Errors
    /// A message naming which token is missing or malformed, or that the
    /// tokens past the mandatory nine are not a whole number of
    /// four-token groups.
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
        let trailing = argv.len().saturating_sub(9);
        if !trailing.is_multiple_of(4) {
            return Err(format!(
                "filter case argv has {trailing} tokens past the mandatory nine, which is not \
                 a whole number of four-token extra-input groups (`media_path`, pixfmt, width, \
                 height per extra input)"
            ));
        }
        #[allow(
            clippy::integer_division,
            reason = "trailing is already checked to be an exact multiple of 4 above; \
                      this recovers the group count, not an approximation"
        )]
        let extra_count = trailing / 4;
        let mut extra_inputs = Vec::new();
        for pad in 0..extra_count {
            let base = 9 + pad * 4;
            extra_inputs.push(ExtraInput {
                media_path: get(base, "extra input media_path")?,
                pixfmt: get(base + 1, "extra input pixfmt")?,
                width: get_u32(base + 2, "extra input width")?,
                height: get_u32(base + 3, "extra input height")?,
            });
        }
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
            extra_inputs,
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
    let out_fmt = parse_pixfmt(args.out_pixfmt)?;
    let registry = find_registry(args.filter_name).ok_or_else(|| {
        format!(
            "filterexec: no registry in this corpus declares a filter named `{}` \
             (see filterexec's own `REGISTRIES` list)",
            args.filter_name
        )
    })?;

    // Pad 0 first, then pads 1, 2, ... in declaration order — see this
    // module's doc for why pad 0 keeps its own long-standing fields
    // instead of folding into one array from the start.
    let inputs: Vec<(&str, &str, u32, u32)> = std::iter::once((
        args.media_path,
        args.in_pixfmt,
        args.in_width,
        args.in_height,
    ))
    .chain(
        args.extra_inputs
            .iter()
            .map(|e| (e.media_path, e.pixfmt, e.width, e.height)),
    )
    .collect();

    let filter_args = (!args.filter_args.is_empty()).then_some(args.filter_args);
    let instance = registry
        .create(&Instantiate {
            name: args.filter_name,
            instance: "conformance",
            args: filter_args,
            arguments: &[],
        })
        .map_err(|e| format!("instantiating `{}`: {e}", args.filter_name))?;

    // Caught here, not in `FilterArgs::parse`, because only the
    // instantiated filter's own `FilterDesc` knows how many pads it
    // actually has — a case naming too few or too many inputs for this
    // specific filter is a suite-authoring mistake, and this is the
    // earliest point that can be told apart from "a filter that
    // legitimately takes N inputs got exactly N of them."
    let declared = instance.desc.inputs.len();
    if declared != inputs.len() {
        return Err(format!(
            "filter `{}` declares {declared} input pad(s) ({:?}), but this case's argv \
             names {} input(s) (1 mandatory + {} extra group(s)) — see filterexec's \
             `FilterArgs` doc for the pad-order convention",
            args.filter_name,
            instance
                .desc
                .inputs
                .iter()
                .map(|p| p.name)
                .collect::<Vec<_>>(),
            inputs.len(),
            args.extra_inputs.len(),
        ));
    }

    let mut graph = Graph::new();
    let time_base = Rational::new(1, 25);
    let pool = FramePool::default();

    // One source node per input pad, each fed its own file/format/size and
    // connected to that pad specifically (pad order == `inputs` order ==
    // argv order), then a real frame built and queued for it. Nothing
    // here requires the inputs to share a format or geometry; a filter
    // that ties its pads to a common format (`maskedmerge`'s
    // `Tie::all_pads`) enforces that itself during `graph.configure()`,
    // the same as it would for any other caller of this `Graph`.
    let mut sources = Vec::new();
    let mut frames = Vec::new();
    for (pad, &(media_path, pixfmt_token, width, height)) in inputs.iter().enumerate() {
        let fmt = parse_pixfmt(pixfmt_token)
            .map_err(|e| format!("input pad {pad} ({media_path}): {e}"))?;
        let raw = std::fs::read(media_path)
            .map_err(|e| format!("reading generated media `{media_path}` (pad {pad}): {e}"))?;
        let expected_len = plane_size_sum(fmt, width, height)?;
        if raw.len() != expected_len {
            return Err(format!(
                "generated media `{media_path}` (pad {pad}) is {} bytes; {fmt:?} {width}x{height} \
                 needs {expected_len}",
                raw.len(),
            ));
        }

        let label = format!("src{pad}");
        let src = graph.add_source(
            &label,
            MediaType::Video,
            NodeFormats {
                outputs: vec![FormatSet::video_exact(fmt)],
                label: label.clone(),
                ..NodeFormats::default()
            },
        );
        sources.push(src);

        let mut frame = pool
            .acquire_video(fmt, width, height)
            .map_err(|e| format!("acquiring input frame for pad {pad}: {e}"))?;
        fill_planes(&mut frame, fmt, width, height, &raw).map_err(|e| format!("pad {pad}: {e}"))?;
        frame.pts = Timestamp::new(0);
        frame.time_base = time_base;
        frame.duration = Duration(1);
        frames.push(frame);

        let source_format = LinkFormat::Video {
            format: fmt,
            width,
            height,
            time_base,
            frame_rate: time_base.inverse(),
            sample_aspect_ratio: Rational::ONE,
            color: ColorInfo::default(),
        };
        graph
            .set_source_format(src, source_format)
            .map_err(|e| format!("setting source format for pad {pad}: {e}"))?;
    }

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
    for (pad, &src) in sources.iter().enumerate() {
        let pad_u32 = u32::try_from(pad)
            .map_err(|_| format!("pad index {pad} does not fit in the graph's pad type"))?;
        graph
            .connect(src, 0, node, pad_u32)
            .map_err(|e| format!("connecting pad {pad} to `{}`: {e}", args.filter_name))?;
    }
    graph
        .connect(node, 0, sink, 0)
        .map_err(|e| format!("connecting `{}` to sink: {e}", args.filter_name))?;

    graph
        .configure()
        .map_err(|e| format!("configuring graph: {e}"))?;

    // Every input is exactly one frame, lockstep, no per-input timeline —
    // see this module's doc for why that covers every multi-input filter
    // reached so far and what would not fit it.
    for (src, frame) in sources.into_iter().zip(frames) {
        graph
            .send(src, frame)
            .map_err(|e| format!("sending frame: {e}"))?;
        graph
            .close_source(src, Timestamp::new(1))
            .map_err(|e| format!("closing source: {e}"))?;
    }
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

/// Total raw byte size of a whole frame, planes concatenated with no
/// padding between or within them (`align = 1`) -- the layout a `-f
/// rawvideo` reference file and this harness's own generated media both
/// use.
///
/// Delegates entirely to [`PixFmt::plane_layout`], which already derives
/// each plane's real row stride from the format's own component table
/// (`step`/`offset` per logical channel) rather than assuming one byte per
/// component per plane. That is what makes `rgba` (one plane, four
/// interleaved `step = 4` components) and `yuv444p` (three separate
/// `step = 1` planes) fall out of the same call: this function does not
/// know or care which shape `fmt` is.
///
/// # Errors
/// [`PixFmt::plane_layout`]'s own errors, which for `align = 1` (always a
/// valid power of two) means only a `width`/`height` combination whose
/// byte size does not fit a `usize` -- not reachable by any real
/// conformance case, but not a panic either.
fn plane_size_sum(fmt: PixFmt, width: u32, height: u32) -> Result<usize, String> {
    fmt.plane_layout(width, height, 1)
        .map(|layout| layout.total)
        .map_err(|e| format!("computing plane layout for {fmt:?} {width}x{height}: {e}"))
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
    //
    // Each plane's byte size in the *source* buffer comes from
    // `PixFmt::plane_layout` (`align = 1`, matching the no-padding raw
    // layout `fmt`'s generated media uses), not a uniform `width *
    // height` -- the fix that makes a packed plane's real size (`width *
    // 4` per row for `rgba`, not `width`) line up with where the next
    // plane actually starts.
    let layout = fmt
        .plane_layout(width, height, 1)
        .map_err(|e| format!("computing plane layout for {fmt:?} {width}x{height}: {e}"))?;
    let mut offset = 0usize;
    for p in 0..plane_count(fmt) {
        let Some(&plane_len) = layout.sizes.get(p) else {
            return Err(format!("plane layout has no size for plane {p}"));
        };
        let Some(chunk) = raw.get(offset..offset + plane_len) else {
            return Err(format!("input buffer too short for plane {p}"));
        };
        offset += plane_len;
        let Some(mut plane) = frame.plane_mut(p) else {
            return Err(format!("frame has no plane {p}"));
        };
        let p_u8 = u8::try_from(p).map_err(|_| format!("plane index {p} does not fit in u8"))?;
        let h = fmt.plane_height(height, p_u8) as usize;
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
    let expected = plane_size_sum(fmt, width, height)?;
    if out.len() != expected {
        return Err(format!(
            "output frame produced {} bytes; declared {fmt:?} {width}x{height} needs {expected} \
             (declared output geometry does not match what the filter actually produced)",
            out.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn toks(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The mandatory nine tokens, no extra-input groups — the shape every
    /// case written before multi-input support existed still uses. Must
    /// keep parsing exactly as before: an empty `extra_inputs`.
    #[test]
    fn nine_tokens_parse_with_no_extra_inputs() {
        let argv = toks(&[
            "in.raw",
            "histogram",
            "",
            "gray8",
            "64",
            "64",
            "gray8",
            "256",
            "50",
        ]);
        let args = FilterArgs::parse(&argv).unwrap();
        assert_eq!(args.media_path, "in.raw");
        assert_eq!(args.filter_name, "histogram");
        assert!(args.extra_inputs.is_empty());
    }

    /// Trailing tokens past the mandatory nine must be a whole number of
    /// four-token groups (`media_path`, pixfmt, width, height) — a suite
    /// author who miscounts a group gets a clear parse error naming the
    /// count, not a silently misread field.
    #[test]
    fn a_partial_trailing_group_is_a_parse_error() {
        let argv = toks(&[
            "base.raw",
            "maskedmerge",
            "",
            "gray8",
            "10",
            "10",
            "gray8",
            "10",
            "10",
            "overlay.raw",
            "gray8",
            "10", // one token short of the group of four
        ]);
        let err = FilterArgs::parse(&argv).unwrap_err();
        assert!(
            err.contains("four-token"),
            "expected a message about the group shape, got: {err}"
        );
    }

    /// Two extra-input groups parse into `extra_inputs` in order, each
    /// field landing on the right token — this is the exact shape
    /// `maskedmerge`'s two extra pads (`overlay`, `mask`) use.
    #[test]
    fn two_extra_input_groups_parse_in_pad_order() {
        let argv = toks(&[
            "base.raw",
            "maskedmerge",
            "",
            "gray8",
            "10",
            "10",
            "gray8",
            "10",
            "10",
            "overlay.raw",
            "gray8",
            "10",
            "10",
            "mask.raw",
            "gray8",
            "10",
            "10",
        ]);
        let args = FilterArgs::parse(&argv).unwrap();
        assert_eq!(args.extra_inputs.len(), 2);
        assert_eq!(args.extra_inputs[0].media_path, "overlay.raw");
        assert_eq!(args.extra_inputs[1].media_path, "mask.raw");
    }

    /// A case that names fewer inputs than the filter actually declares
    /// fails loudly, before any frame is built or compared, naming both
    /// the filter's own pad count and what the case provided — the
    /// scenario this test guards against is a suite typo (forgetting a
    /// `{media:<id>}` group) silently connecting the wrong pad, or
    /// leaving one permanently starved, instead of erroring.
    #[test]
    fn an_arity_mismatch_against_the_real_registry_is_a_clear_error() {
        // `maskedmerge` (vaco-filter-key) declares three input pads
        // (base/overlay/mask); this argv gives it only one. `run` reads a
        // real file for every declared input before reaching the arity
        // check, so pad 0 needs a real (if trivial) 1x1 gray8 file on disk.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), [0u8]).unwrap();
        let argv = toks(&[
            &tmp.path().to_string_lossy(),
            "maskedmerge",
            "",
            "gray8",
            "1",
            "1",
            "gray8",
            "1",
            "1",
        ]);
        let args = FilterArgs::parse(&argv).unwrap();
        let err = run(&args).unwrap_err();
        assert!(
            err.contains("declares 3 input pad"),
            "expected the pad count in the error, got: {err}"
        );
        assert!(
            err.contains("names 1 input"),
            "expected what the case provided in the error, got: {err}"
        );
    }
}
