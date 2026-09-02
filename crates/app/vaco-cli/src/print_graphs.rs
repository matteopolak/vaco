//! `-print_graphs`/`-print_graphs_file`/`-print_graphs_format` (CL-27, #230).
//!
//! # Reference basis
//!
//! Observed directly (D6): `ffmpeg 9.0.1 -print_graphs -print_graphs_format
//! default|json -filter_complex "[0:v]scale=160:120[v];[1:a]volume=0.5[a]"
//! -map "[v]" -map "[a]" …` prints, before it writes any packet, one
//! `[GRAPH]` per filtergraph occurrence containing `[GRAPH_INPUT]`/
//! `[GRAPH_OUTPUT]` (the graph's own boundary pads) and `[FILTER]`
//! (one per instantiated filter), each `[FILTER]` in turn containing
//! `[FILTER_INPUT]`/`[FILTER_OUTPUT]`. `vaco-textformat`'s
//! `sections::{GRAPHS, GRAPH, GRAPH_INPUTS, GRAPH_INPUT, GRAPH_OUTPUTS,
//! GRAPH_OUTPUT, FILTERS, FILTER, FILTER_INPUTS, FILTER_INPUT,
//! FILTER_OUTPUTS, FILTER_OUTPUT}` section family transcribes that shape
//! (see that crate's `sections.rs` for the full field list and the ffmpeg
//! invocation it was measured against).
//!
//! # What this does not reproduce
//!
//! - **Per-link negotiated formats.** The reference's `[FILTER_INPUT]`/
//!   `[FILTER_OUTPUT]` also print `format`/`width`/`height`/`sar`/
//!   `sample_rate`/`color_range`/`color_space` (or the audio equivalents),
//!   read back from the graph *after* it configures. This dump never calls
//!   [`vaco_filter_graph::build::BuiltGraph::configure`] — negotiation needs
//!   real attached sources with real formats, which only
//!   [`crate::exec::run_pipeline`]/[`crate::complexgraph::build_and_attach`]
//!   have, and duplicating that attachment here purely to throw the result
//!   away was judged not worth the added failure surface for a diagnostic
//!   dump. `vaco-textformat`'s own section table already omits the fields
//!   this would have fed (see its module doc) rather than naming a field
//!   with nothing behind it.
//! - **`-vf`/`-af`'s implicit per-stream graphs.** The reference dumps those
//!   too (each gets its own `[GRAPH]`); this only dumps `-filter_complex`/
//!   `-lavfi` occurrences, which is what `#230`'s own investigation and this
//!   module's test fixtures exercise. A simple-graph dump is not implemented.
//! - **`name`/`id`.** The reference's `name` is an internal
//!   `"Graph <filtergraph-index>.<subgraph-index>"` label and `id` a
//!   matching internal string; this reports `"Graph <index>"` /
//!   `"graph_<index>"` (this crate's own occurrence index within
//!   `-filter_complex`'s repeats) — a real, stable identifier, just not the
//!   reference's own internal numbering scheme.
//! - **`nb_inputs`/`nb_outputs`.** The reference reports each filter's
//!   *declared* pad arity (`AVFilterPad` count, including a pad nothing is
//!   attached to yet). This reports the number of pads this dump actually
//!   observed connected or left open — identical for every filter with fixed
//!   arity (the overwhelming majority: `scale`, `volume`, `overlay`, …), and
//!   a real but smaller number for a variable-arity filter with an unused
//!   optional pad.
//! - **`filter_id`.** The reference's own opaque per-run id (`"G0_Parsed_scale_0"`
//!   style); this reports [`vaco_filter_graph::build::NodeInfo::instance`]
//!   directly (`"Parsed_scale_0"`, or the user's own `scale@tag` when
//!   tagged) — real and stable within a run, just not byte-identical to the
//!   reference's own prefix.

use vaco_cli_core::CommandLine;
use vaco_core::MediaType;
use vaco_filter_graph::build::BuiltGraph;
use vaco_textformat::{FormatOpts, SectionId, TextFormat, writers};

use crate::cli::value_str;
use crate::exit::{AvError, Diagnostic};
use vaco_registry::Filters;

/// A resolved `-print_graphs*` request.
#[derive(Debug)]
pub struct PrintGraphsSpec {
    /// `-print_graphs_format`, or `"default"` when unstated — matching the
    /// reference's own fallback.
    pub format: String,
    /// `-print_graphs_file`, or `None` to write to stderr.
    pub file: Option<String>,
}

impl PrintGraphsSpec {
    /// `Ok(None)` when `-print_graphs` was not given at all.
    ///
    /// Deliberately does **not** validate `-print_graphs_format` here: measured
    /// directly against the reference (`ffmpeg 9.0.1 -print_graphs
    /// -print_graphs_format bogus …`), a bad name is not fatal at all — the run
    /// completes exit 0, `-print_graphs_file`'s file (if any) is never created,
    /// and the only sign is a `Unknown filter graph output format with name
    /// '<name>'` line on stderr. [`render`] reports that outcome instead of
    /// erroring, and the caller (`crate::execute`) warns and continues rather
    /// than aborting — see [`RenderOutcome::UnknownFormat`].
    ///
    /// # Errors
    /// A [`Diagnostic`] for a non-UTF-8 `-print_graphs_file`/
    /// `-print_graphs_format` value.
    pub fn resolve(line: &CommandLine) -> Result<Option<Self>, Diagnostic> {
        if line.last_global("print_graphs").is_none() {
            return Ok(None);
        }
        let format = line
            .last_global("print_graphs_format")
            .map(value_str)
            .transpose()?
            .unwrap_or_else(|| "default".to_owned());
        let file = line
            .last_global("print_graphs_file")
            .map(value_str)
            .transpose()?;
        Ok(Some(Self { format, file }))
    }
}

/// What [`render`] produced.
#[derive(Debug)]
pub enum RenderOutcome {
    /// The rendered document, ready to write to stderr or
    /// `-print_graphs_file`'s path.
    Graphs(Vec<u8>),
    /// `format` is not a writer this build has. Matching the reference
    /// (see [`PrintGraphsSpec::resolve`]'s doc), this is not an error: the
    /// caller should warn and otherwise continue the run exactly as if
    /// `-print_graphs` had not been given.
    UnknownFormat(String),
}

/// Render every `-filter_complex`/`-lavfi` occurrence as one `[GRAPH]` each,
/// through the writer named `format`.
///
/// Each graph is parsed and built fresh, purely for this dump —
/// [`crate::exec::run_pipeline`] builds and attaches its own independent copy
/// for the real run. See the module doc for why `configure` (format
/// negotiation) is deliberately never called on this copy.
///
/// # Errors
/// A [`Diagnostic`] if a graph text fails to parse (should not happen here —
/// `crate::complexgraph::catalog` already validated the same texts earlier in
/// `execute`) or the writer itself refuses the run. An unknown `format` name
/// is reported through [`RenderOutcome::UnknownFormat`], not an `Err` — see
/// [`PrintGraphsSpec::resolve`]'s doc for why.
pub fn render(complex_filters: &[String], format: &str) -> Result<RenderOutcome, Diagnostic> {
    let Ok(writer) = writers::make(format) else {
        return Ok(RenderOutcome::UnknownFormat(format.to_owned()));
    };
    let mut tf = TextFormat::new(writer, Vec::new(), FormatOpts::default());
    tf.open(SectionId::ROOT).map_err(|e| text_err(&e))?;
    tf.open(SectionId::GRAPHS).map_err(|e| text_err(&e))?;
    for (index, text) in complex_filters.iter().enumerate() {
        let built = vaco_filter_graph::parse_and_build(text, &Filters).map_err(|e| {
            Diagnostic::new(
                AvError::EINVAL,
                vec![format!("Error configuring filter graph: {}", e.render(text))],
            )
        })?;
        write_graph(&mut tf, index, text, &built).map_err(|e| text_err(&e))?;
    }
    tf.close().map_err(|e| text_err(&e))?; // GRAPHS
    tf.close().map_err(|e| text_err(&e))?; // ROOT
    tf.finish().map_err(|e| text_err(&e)).map(RenderOutcome::Graphs)
}

fn text_err(e: &vaco_core::Error) -> Diagnostic {
    Diagnostic::new(AvError::EINVAL, vec![format!("print_graphs: {e}")])
}

/// One connected or open pad, ordered by pad index, for either side of one
/// node.
struct PadRow {
    pad: u32,
    media: MediaType,
    /// The neighbour's `filter_id`, or `None` for a pad this graph leaves
    /// open at its own boundary (an unattached [`BuiltGraph::open_inputs`]/
    /// `open_outputs` pad, in which case the boundary's own synthetic id is
    /// substituted by the caller).
    neighbour: Option<String>,
}

fn write_graph(
    tf: &mut TextFormat<Vec<u8>>,
    index: usize,
    text: &str,
    built: &BuiltGraph,
) -> vaco_core::Result<()> {
    tf.open(SectionId::GRAPH)?;
    tf.int("graph_index", i64::try_from(index).unwrap_or(i64::MAX))?;
    tf.str("name", &format!("Graph {index}"))?;
    tf.str("id", &format!("graph_{index}"))?;
    tf.str("description", text)?;

    tf.open(SectionId::GRAPH_INPUTS)?;
    for (i, pad) in built.open_inputs.iter().enumerate() {
        tf.open(SectionId::GRAPH_INPUT)?;
        tf.int("input_index", i64::try_from(i).unwrap_or(i64::MAX))?;
        if let Some(label) = &pad.label {
            tf.str("link_label", label)?;
        }
        tf.str("filter_id", &boundary_id(index, "in", i))?;
        tf.str("filter_name", boundary_filter_name(pad.media, true))?;
        tf.str("media_type", media_name(pad.media))?;
        tf.close()?;
    }
    tf.close()?; // GRAPH_INPUTS

    tf.open(SectionId::FILTERS)?;
    for node in &built.nodes {
        let inputs = connected_inputs(built, node.id, index);
        let outputs = connected_outputs(built, node.id, index);

        tf.open(SectionId::FILTER)?;
        tf.str("filter_name", &node.filter)?;
        tf.int("nb_inputs", i64::try_from(inputs.len()).unwrap_or(i64::MAX))?;
        tf.int("nb_outputs", i64::try_from(outputs.len()).unwrap_or(i64::MAX))?;

        tf.open(SectionId::FILTER_INPUTS)?;
        for row in &inputs {
            tf.open(SectionId::FILTER_INPUT)?;
            tf.int("input_index", i64::from(row.pad))?;
            if let Some(src) = &row.neighbour {
                tf.str("source_filter_id", src)?;
            }
            tf.str("filter_id", &node.instance)?;
            tf.str("media_type", media_name(row.media))?;
            tf.close()?;
        }
        tf.close()?; // FILTER_INPUTS

        tf.open(SectionId::FILTER_OUTPUTS)?;
        for row in &outputs {
            tf.open(SectionId::FILTER_OUTPUT)?;
            if let Some(dst) = &row.neighbour {
                tf.str("dest_filter_id", dst)?;
            }
            tf.int("output_index", i64::from(row.pad))?;
            tf.str("filter_id", &node.instance)?;
            tf.str("media_type", media_name(row.media))?;
            tf.close()?;
        }
        tf.close()?; // FILTER_OUTPUTS

        tf.close()?; // FILTER
    }
    tf.close()?; // FILTERS

    tf.open(SectionId::GRAPH_OUTPUTS)?;
    for (i, pad) in built.open_outputs.iter().enumerate() {
        tf.open(SectionId::GRAPH_OUTPUT)?;
        tf.int("output_index", i64::try_from(i).unwrap_or(i64::MAX))?;
        tf.str(
            "name",
            &pad.label.clone().unwrap_or_else(|| format!("out{i}")),
        )?;
        tf.str("filter_id", &boundary_id(index, "out", i))?;
        tf.str("filter_name", boundary_filter_name(pad.media, false))?;
        tf.str("media_type", media_name(pad.media))?;
        tf.close()?;
    }
    tf.close()?; // GRAPH_OUTPUTS

    tf.close() // GRAPH
}

/// Every pad of `node` that is either connected to another node or left open
/// at this graph's own input boundary, ordered by pad index.
fn connected_inputs(built: &BuiltGraph, node: vaco_filter_core::NodeId, graph_index: usize) -> Vec<PadRow> {
    let mut rows: Vec<PadRow> = built
        .open_inputs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.node == node)
        .map(|(i, p)| PadRow {
            pad: p.pad,
            media: p.media,
            neighbour: Some(boundary_id(graph_index, "in", i)),
        })
        .collect();
    for link in built.graph.links().iter() {
        let dst = link.dst();
        if dst.node != node {
            continue;
        }
        let source = built
            .nodes
            .iter()
            .find(|n| n.id == link.src().node)
            .map_or_else(|| format!("n{}", link.src().node.0), |n| n.instance.clone());
        rows.push(PadRow {
            pad: dst.pad,
            media: link.media(),
            neighbour: Some(source),
        });
    }
    rows.sort_by_key(|r| r.pad);
    rows
}

/// The output-side mirror of [`connected_inputs`].
fn connected_outputs(built: &BuiltGraph, node: vaco_filter_core::NodeId, graph_index: usize) -> Vec<PadRow> {
    let mut rows: Vec<PadRow> = built
        .open_outputs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.node == node)
        .map(|(i, p)| PadRow {
            pad: p.pad,
            media: p.media,
            neighbour: Some(boundary_id(graph_index, "out", i)),
        })
        .collect();
    for link in built.graph.links().iter() {
        let src = link.src();
        if src.node != node {
            continue;
        }
        let dest = built
            .nodes
            .iter()
            .find(|n| n.id == link.dst().node)
            .map_or_else(|| format!("n{}", link.dst().node.0), |n| n.instance.clone());
        rows.push(PadRow {
            pad: src.pad,
            media: link.media(),
            neighbour: Some(dest),
        });
    }
    rows.sort_by_key(|r| r.pad);
    rows
}

/// The synthetic id this dump gives a graph's own boundary pad (the
/// reference's real `buffer`/`abuffer`/`buffersink`/`abuffersink` node
/// filter-graph never creates here — see the module doc).
fn boundary_id(graph_index: usize, side: &str, i: usize) -> String {
    format!("g{graph_index}_{side}_{i}")
}

/// `buffer`/`abuffer` (source) or `buffersink`/`abuffersink` (sink) — the
/// reference's own names for a graph's boundary filter, chosen by media type.
/// A subtitle/data/attachment pad (never seen from a real `-filter_complex`
/// graph today, since no registered filter has such a pad) falls back to the
/// video spelling rather than inventing a name the reference has none for.
fn boundary_filter_name(media: MediaType, is_source: bool) -> &'static str {
    match (media, is_source) {
        (MediaType::Audio, true) => "abuffer",
        (MediaType::Audio, false) => "abuffersink",
        (_, true) => "buffer",
        (_, false) => "buffersink",
    }
}

fn media_name(media: MediaType) -> &'static str {
    match media {
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Subtitle => "subtitle",
        MediaType::Data => "data",
        MediaType::Attachment => "attachment",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    impl RenderOutcome {
        fn into_graphs(self) -> Option<Vec<u8>> {
            match self {
                Self::Graphs(b) => Some(b),
                Self::UnknownFormat(_) => None,
            }
        }
    }

    #[test]
    fn a_two_stream_filter_complex_graph_renders_boundary_and_filter_sections() {
        let bytes = render(
            &["[0:v]scale=160:120[v];[1:a]volume=0.5[a]".to_owned()],
            "default",
        )
        .unwrap()
        .into_graphs()
        .expect("known format");
        let text = String::from_utf8(bytes).unwrap();

        assert!(text.contains("[GRAPH]"), "{text}");
        assert!(text.contains("description=[0:v]scale=160:120[v];[1:a]volume=0.5[a]"), "{text}");
        assert!(text.contains("[GRAPH_INPUT]"), "{text}");
        assert!(text.contains("link_label=0:v"), "{text}");
        assert!(text.contains("link_label=1:a"), "{text}");
        assert!(text.contains("[FILTER]"), "{text}");
        assert!(text.contains("filter_name=scale"), "{text}");
        assert!(text.contains("filter_name=volume"), "{text}");
        assert!(text.contains("[GRAPH_OUTPUT]"), "{text}");
        assert!(text.contains("link_label=v") || text.contains("name=v"), "{text}");
    }

    #[test]
    fn json_format_is_accepted_and_nests_the_same_data() {
        let bytes = render(&["[0:v]scale=160:120[v]".to_owned()], "json")
            .unwrap()
            .into_graphs()
            .expect("known format");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"graphs\""), "{text}");
        assert!(text.contains("\"filter_name\": \"scale\""), "{text}");
    }

    #[test]
    fn mermaid_format_renders_a_flowchart() {
        let bytes = render(&["[0:v]scale=160:120[v]".to_owned()], "mermaid")
            .unwrap()
            .into_graphs()
            .expect("known format");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("flowchart LR"), "{text}");
    }

    /// Measured against the reference (module doc): an unknown
    /// `-print_graphs_format` name is not an error at all, just an
    /// [`RenderOutcome::UnknownFormat`] the caller turns into a warning.
    #[test]
    fn an_unknown_format_name_is_reported_not_refused() {
        let outcome = render(&["[0:v]scale=160:120[v]".to_owned()], "nosuchformat").unwrap();
        assert!(matches!(outcome, RenderOutcome::UnknownFormat(n) if n == "nosuchformat"));
    }
}
