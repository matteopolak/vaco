//! The `mermaid`/`mermaidhtml` writers (CL-27).
//!
//! Neither is part of `ffprobe -sections` (this crate's own transcription
//! source for every other writer): `-print_graphs_format mermaid`/
//! `mermaidhtml` is an `ffmpeg`-only option, and `ffmpeg` has no `-sections`
//! dump. Observed instead, directly: `ffmpeg 9.0.1 -print_graphs
//! -print_graphs_format mermaid` on a real `-filter_complex` run emits a
//! Mermaid `flowchart` with one node per filter/graph-input/graph-output,
//! wrapped in a large `%%{init: ...}%%` directive that carries the
//! reference's own CSS (HTML-styled node labels, per-media-type colours, a
//! gradient `<defs>` block) and additional nodes for the *whole pipeline*
//! (input files, decoders, encoders, output files) that this crate has no
//! access to at the `vaco-textformat` layer -- it only ever sees the section
//! tree a caller drives it with, and `vaco-cli` builds that tree from
//! `vaco_filter_graph::BuiltGraph` alone.
//!
//! This writer reproduces the part that data supports: a real, valid,
//! renderable Mermaid flowchart of the filtergraph itself -- one node per
//! filter and per graph input/output, one edge per link, coloured by media
//! type -- using the same [`SectionId::GRAPH`]/[`SectionId::FILTER`] family
//! every other writer reads. It does not reproduce the reference's CSS
//! styling or its input-file/decoder/encoder/output-file nodes, which are
//! outside what a filtergraph-only section tree can express. `mermaidhtml`
//! is the same diagram wrapped in a minimal HTML document that renders it via
//! `mermaid.js`, matching the reference's own "a browser can open this file
//! directly" shape without its exact markup.

use std::collections::BTreeMap;

use vaco_core::Result;

use crate::opts::{CommonOpts, unknown_option};
use crate::sections::SectionId;
use crate::{Ctx, Out, TextWriter, WriterFlags};

/// One node of the diagram, keyed by this crate's own `filter_id`/`link_label`
/// string (whatever the caller used to identify it — [`super::super`]'s
/// section field, not a Mermaid-internal id).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Node {
    label: String,
    media: String,
}

/// One directed edge, `from -> to`, coloured by `media` when known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Edge {
    from: String,
    to: String,
    media: String,
}

/// Scratch for the section currently being read — filled field by field as
/// `print_str`/`print_int` arrive, interpreted once its `section_footer`
/// fires. One frame per open section, since `FILTER` nests `FILTER_INPUT`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scratch {
    id: SectionId,
    fields: BTreeMap<&'static str, String>,
}

/// `-of mermaid` / `-of mermaidhtml`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MermaidWriter {
    html: bool,
    common: CommonOpts,
    diagram: Diagram,
}

impl MermaidWriter {
    /// # Errors
    /// [`Error::Option`] for an option this writer does not accept.
    pub fn from_options(opts: &[(String, String)], html: bool) -> Result<Self> {
        let mut w = Self {
            html,
            common: CommonOpts::default(),
            diagram: Diagram::default(),
        };
        for (k, v) in opts {
            if !w.common.set(k, v)? {
                return Err(unknown_option("mermaid", k));
            }
        }
        Ok(w)
    }
}

/// Accumulated across the whole run, in a `RefCell` because [`TextWriter`]'s
/// methods take `&mut self` for the writer but this state needs to survive
/// from the first `FILTER`/`GRAPH_INPUT` to `fini`, which is exactly `&mut
/// self` already — no interior mutability actually needed, so this is a
/// plain field. (Kept as a doc note because the obvious first design reaches
/// for `RefCell` here out of habit; a mutable method does not need it.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Diagram {
    nodes: Vec<(String, Node)>,
    edges: Vec<Edge>,
    stack: Vec<Scratch>,
}

impl Diagram {
    fn node(&mut self, id: &str) -> &mut Node {
        let pos = self
            .nodes
            .iter()
            .position(|(k, _)| k == id)
            .unwrap_or_else(|| {
                self.nodes.push((id.to_owned(), Node::default()));
                self.nodes.len() - 1
            });
        #[allow(
            clippy::indexing_slicing,
            reason = "pos was either found in nodes or is the index just pushed"
        )]
        &mut self.nodes[pos].1
    }

    /// A `GRAPH_INPUT`/`GRAPH_OUTPUT` record: one node (the buffer/buffersink
    /// filter itself), plus its edge to/from the graph boundary.
    fn finish_graph_io(&mut self, s: &Scratch) {
        let filter_id = s.fields.get("filter_id").cloned().unwrap_or_default();
        let filter_name = s.fields.get("filter_name").cloned().unwrap_or_default();
        let media = s.fields.get("media_type").cloned().unwrap_or_default();
        if filter_id.is_empty() {
            return;
        }
        {
            let n = self.node(&filter_id);
            n.label = filter_name;
            n.media.clone_from(&media);
        }
        if s.id == SectionId::GRAPH_INPUT {
            let label = s
                .fields
                .get("link_label")
                .cloned()
                .unwrap_or_else(|| "in".to_owned());
            let src = format!("input_{filter_id}");
            {
                let n = self.node(&src);
                n.label = label;
                n.media.clone_from(&media);
            }
            self.edges.push(Edge {
                from: src,
                to: filter_id,
                media,
            });
        } else {
            let name = s
                .fields
                .get("name")
                .cloned()
                .unwrap_or_else(|| "out".to_owned());
            let dst = format!("output_{filter_id}");
            {
                let n = self.node(&dst);
                n.label = name;
                n.media.clone_from(&media);
            }
            self.edges.push(Edge {
                from: filter_id,
                to: dst,
                media,
            });
        }
    }

    /// A `FILTER_INPUT`/`FILTER_OUTPUT` record: one edge, from whichever side
    /// names the *other* filter. `vaco-filter-graph::BuiltGraph` does not
    /// expose the negotiated per-link format (`format`/`width`/`height`/…
    /// the reference also prints here), so the edge carries only `media`.
    fn finish_filter_io(&mut self, s: &Scratch) {
        let filter_id = s.fields.get("filter_id").cloned().unwrap_or_default();
        let media = s.fields.get("media_type").cloned().unwrap_or_default();
        if s.id == SectionId::FILTER_INPUT {
            let Some(source) = s.fields.get("source_filter_id").cloned() else {
                return;
            };
            if source.is_empty() || filter_id.is_empty() {
                return;
            }
            self.edges.push(Edge {
                from: source,
                to: filter_id,
                media,
            });
        } else {
            let Some(dest) = s.fields.get("dest_filter_id").cloned() else {
                return;
            };
            if dest.is_empty() || filter_id.is_empty() {
                return;
            }
            self.edges.push(Edge {
                from: filter_id,
                to: dest,
                media,
            });
        }
    }

    fn render(&self) -> String {
        use core::fmt::Write as _;
        let mut out = String::from("flowchart LR\n");
        for (id, node) in &self.nodes {
            let shape_open = match node.media.as_str() {
                "video" | "audio" => "([",
                _ => "[",
            };
            let shape_close = match node.media.as_str() {
                "video" | "audio" => "])",
                _ => "]",
            };
            let label = if node.label.is_empty() {
                id.as_str()
            } else {
                node.label.as_str()
            };
            let _ = writeln!(
                out,
                "  {}{}\"{}\"{}",
                sanitise_id(id),
                shape_open,
                escape_label(label),
                shape_close
            );
            if !node.media.is_empty() {
                let _ = writeln!(
                    out,
                    "  class {} ff-{}",
                    sanitise_id(id),
                    sanitise_id(&node.media)
                );
            }
        }
        for edge in &self.edges {
            if edge.media.is_empty() {
                let _ = writeln!(
                    out,
                    "  {} --> {}",
                    sanitise_id(&edge.from),
                    sanitise_id(&edge.to)
                );
            } else {
                let _ = writeln!(
                    out,
                    "  {} -- {} --> {}",
                    sanitise_id(&edge.from),
                    edge.media,
                    sanitise_id(&edge.to)
                );
            }
        }
        out.push_str("  classDef ff-video stroke:#6eaa7b,color:#6eaa7b;\n");
        out.push_str("  classDef ff-audio stroke:#477fb3,color:#477fb3;\n");
        out
    }
}

/// Mermaid node/class ids: `[A-Za-z0-9_]` only, matching the reference's own
/// convention of a plain identifier per node (mermaid itself rejects most
/// punctuation unquoted).
fn sanitise_id(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'n');
    }
    out
}

/// A label inside `"…"`: Mermaid's own quoted-string escape is `#quot;` for a
/// literal quote, and a literal `#` must double itself first or it would be
/// read as the start of that escape.
fn escape_label(s: &str) -> String {
    s.replace('#', "#35;").replace('"', "#quot;")
}

const HTML_PROLOGUE: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n\
<script src=\"https://cdn.jsdelivr.net/npm/mermaid/dist/mermaid.min.js\"></script>\n\
<script>mermaid.initialize({startOnLoad:true});</script>\n\
</head><body>\n<pre class=\"mermaid\">\n";
const HTML_EPILOGUE: &str = "</pre>\n</body></html>\n";

impl TextWriter for MermaidWriter {
    fn name(&self) -> &'static str {
        if self.html { "mermaidhtml" } else { "mermaid" }
    }

    fn flags(&self) -> WriterFlags {
        WriterFlags::DOCUMENT
    }

    fn init(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        if self.html {
            o.s(HTML_PROLOGUE)?;
        }
        Ok(())
    }

    fn fini(&mut self, o: &mut Out<'_>, _ctx: &Ctx<'_>) -> Result<()> {
        o.s(&self.diagram.render())?;
        if self.html {
            o.s(HTML_EPILOGUE)?;
        }
        Ok(())
    }

    fn section_header(&mut self, _o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> {
        self.diagram.stack.push(Scratch {
            id: ctx.cur().id,
            fields: BTreeMap::new(),
        });
        Ok(())
    }

    fn section_footer(&mut self, _o: &mut Out<'_>, _ctx: &Ctx<'_>, _produced: bool) -> Result<()> {
        let Some(s) = self.diagram.stack.pop() else {
            return Ok(());
        };
        match s.id {
            SectionId::GRAPH_INPUT | SectionId::GRAPH_OUTPUT => self.diagram.finish_graph_io(&s),
            SectionId::FILTER_INPUT | SectionId::FILTER_OUTPUT => {
                self.diagram.finish_filter_io(&s);
            }
            _ => {}
        }
        Ok(())
    }

    fn print_int(&mut self, _o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()> {
        self.record(ctx, key, v.to_string());
        Ok(())
    }

    fn print_str(&mut self, _o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()> {
        self.record(ctx, key, v.to_owned());
        Ok(())
    }
}

impl MermaidWriter {
    fn record(&mut self, ctx: &Ctx<'_>, key: &str, value: String) {
        let target_id = ctx.cur().id;
        if let Some(frame) = self
            .diagram
            .stack
            .iter_mut()
            .rev()
            .find(|f| f.id == target_id)
        {
            // Static field-name set (this writer only ever reads the fields
            // `vaco-cli`'s own print_graphs code writes), so leaking the
            // caller's `&str` key past this call is safe.
            let key: &'static str = match key {
                "input_index" => "input_index",
                "output_index" => "output_index",
                "link_label" => "link_label",
                "name" => "name",
                "filter_id" => "filter_id",
                "filter_name" => "filter_name",
                "media_type" => "media_type",
                "source_filter_id" => "source_filter_id",
                "dest_filter_id" => "dest_filter_id",
                _ => return,
            };
            frame.fields.insert(key, value);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use crate::opts::FormatOpts;
    use crate::sections::SectionId;
    use crate::{TextFormat, writers};

    /// One filter fed through the cursor the way `vaco-cli`'s `-print_graphs`
    /// code will drive it: a graph with one input, one filter, one output.
    #[test]
    fn a_one_filter_graph_renders_nodes_and_edges() {
        let mut tf = TextFormat::new(
            writers::make("mermaid").unwrap(),
            Vec::new(),
            FormatOpts::default(),
        );
        tf.open(SectionId::ROOT).unwrap();
        tf.open(SectionId::GRAPHS).unwrap();
        tf.open(SectionId::GRAPH).unwrap();
        tf.int("graph_index", 0).unwrap();
        tf.open(SectionId::GRAPH_INPUTS).unwrap();
        tf.open(SectionId::GRAPH_INPUT).unwrap();
        tf.int("input_index", 0).unwrap();
        tf.str("link_label", "0:v").unwrap();
        tf.str("filter_id", "in_0").unwrap();
        tf.str("filter_name", "buffer").unwrap();
        tf.str("media_type", "video").unwrap();
        tf.close().unwrap(); // graph_input
        tf.close().unwrap(); // graph_inputs
        tf.open(SectionId::FILTERS).unwrap();
        tf.open(SectionId::FILTER).unwrap();
        tf.str("filter_name", "scale").unwrap();
        tf.int("nb_inputs", 1).unwrap();
        tf.int("nb_outputs", 1).unwrap();
        tf.open(SectionId::FILTER_INPUTS).unwrap();
        tf.open(SectionId::FILTER_INPUT).unwrap();
        tf.int("input_index", 0).unwrap();
        tf.str("source_filter_id", "in_0").unwrap();
        tf.str("filter_id", "scale_0").unwrap();
        tf.str("media_type", "video").unwrap();
        tf.close().unwrap(); // filter_input
        tf.close().unwrap(); // filter_inputs
        tf.close().unwrap(); // filter
        tf.close().unwrap(); // filters
        tf.open(SectionId::GRAPH_OUTPUTS).unwrap();
        tf.open(SectionId::GRAPH_OUTPUT).unwrap();
        tf.int("output_index", 0).unwrap();
        tf.str("name", "#0:0").unwrap();
        tf.str("filter_id", "scale_0").unwrap();
        tf.str("filter_name", "buffersink").unwrap();
        tf.str("media_type", "video").unwrap();
        tf.close().unwrap(); // graph_output
        tf.close().unwrap(); // graph_outputs
        tf.close().unwrap(); // graph
        tf.close().unwrap(); // graphs
        tf.close().unwrap(); // root

        let bytes = tf.finish().unwrap();
        let text = String::from_utf8(bytes).unwrap();

        assert!(text.starts_with("flowchart LR\n"), "{text}");
        // One node for the graph input, one for `scale`, one for the output.
        assert!(text.contains("in_0(["), "{text}");
        assert!(text.contains("scale_0(["), "{text}");
        // Edges: input -> scale (via FILTER_INPUT's source_filter_id), and
        // scale -> the graph output boundary (via GRAPH_OUTPUT's filter_id).
        assert!(text.contains("in_0 -- video --> scale_0"), "{text}");
        assert!(
            text.contains("scale_0 -- video --> output_scale_0"),
            "{text}"
        );
    }

    #[test]
    fn mermaidhtml_wraps_the_same_diagram_in_a_pre_block() {
        let mut tf = TextFormat::new(
            writers::make("mermaidhtml").unwrap(),
            Vec::new(),
            FormatOpts::default(),
        );
        tf.open(SectionId::ROOT).unwrap();
        tf.open(SectionId::GRAPHS).unwrap();
        tf.close().unwrap();
        tf.close().unwrap();
        let bytes = tf.finish().unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("<!DOCTYPE html>"), "{text}");
        assert!(text.contains("<pre class=\"mermaid\">"), "{text}");
        assert!(text.contains("flowchart LR"), "{text}");
        assert!(text.trim_end().ends_with("</html>"), "{text}");
    }
}
