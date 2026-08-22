//! Instantiation, link resolution and validation.
//!
//! # Order matters and is not obvious
//!
//! Pad counts depend on options (`amix=inputs=3`, `split=4`), so links cannot be
//! resolved from the syntax tree alone:
//!
//! 1. **Instantiate** each filter, in parse order, giving it its instance name.
//! 2. **Resolve** labels and unlabelled pads, chain by chain, left to right.
//! 3. **Validate** structurally.
//! 4. Negotiate and configure — `vaco-filter-core`'s, reached through
//!    [`BuiltGraph::configure`].
//!
//! # The two link mechanisms, in this order per filter
//!
//! **Explicit labels.** A leading `[L]` connects to an open output named `L`,
//! or records an unmatched input that a later chain may satisfy — forward
//! references work, and `[a]hflip[out];[0:v]null[a]` is a legal graph. A
//! trailing `[L]` connects to an unmatched input named `L`, or opens an output.
//!
//! **Unlabelled auto-connection, within a chain.** After filter *k*, the output
//! pads that received no label are carried forward. Filter *k+1*'s labelled
//! inputs take pads `0..n` and the carried list fills what remains, in pad
//! order. Measured directly, because the ordering is not obvious:
//!
//! ```sh
//! ffmpeg -f lavfi -i "color=c=red:s=64x64:d=0.1[x];color=c=blue:s=8x8:d=0.1,[x]overlay" ...
//! # -> the output is 64x64, so `[x]` took overlay's *main* pad and the carried
//! #    8x8 stream took the overlay pad. Labels first, carried after.
//! ```
//!
//! Whatever is unmatched at the end is exported as an [`OpenPad`], which is how
//! `-vf`'s implicit ends and `-filter_complex`'s `[0:v]` / `-map [out]` labels
//! are reached.

use vaco_core::{MediaType, Result as CoreResult};
use vaco_filter_core::negotiate::{AutoConvert, Conflict, NodeFormats};
use vaco_filter_core::{FilterDesc, Graph, LinkFormat, NodeId};

use crate::ast::{Ast, FilterSpec};
use crate::convert::DefaultConverters;
use crate::error::{ErrorKind, GraphError, suggest};
use crate::registry::{FilterRegistry, Instantiate};
use crate::span::Span;

/// A pad the description left for the caller to attach something to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPad {
    /// The label it was given, or `None` for an unlabelled chain end.
    pub label: Option<String>,
    pub node: NodeId,
    pub pad: u32,
    pub media: MediaType,
    /// Where in the description it was written.
    pub span: Span,
}

/// One instantiated filter, for introspection and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub id: NodeId,
    /// `Parsed_scale_1`, or `scale@big` for an explicit tag.
    pub instance: String,
    /// The registered filter name.
    pub filter: String,
    pub span: Span,
}

/// A graph built from a description, plus everything the description said that
/// the scheduler has no room for.
#[derive(Debug)]
pub struct BuiltGraph {
    /// The scheduler's graph. Not yet configured: open pads must be attached
    /// first.
    pub graph: Graph,
    pub nodes: Vec<NodeInfo>,
    pub open_inputs: Vec<OpenPad>,
    pub open_outputs: Vec<OpenPad>,
    /// The `sws_flags=` prefix, applied to every auto-inserted `scale`.
    pub sws_opts: String,
    /// The application-level `-aresample_swr_opts`. There is no graph-string
    /// prefix for it; only the application option exists, matching the
    /// reference.
    pub swr_opts: String,
}

impl BuiltGraph {
    /// Attach a buffer source to open input `index`, removing it from the list.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] for an unknown index or a link the
    /// scheduler refuses.
    pub fn attach_source(
        &mut self,
        index: usize,
        formats: NodeFormats,
        format: LinkFormat,
    ) -> CoreResult<NodeId> {
        let Some(open) = self.open_inputs.get(index).cloned() else {
            return Err(vaco_core::Error::InvalidData("no such open input pad"));
        };
        let label = open.label.clone().unwrap_or_else(|| "in".to_owned());
        let src = self.graph.add_source(&label, open.media, formats);
        self.graph.set_source_format(src, format)?;
        self.graph.connect(src, 0, open.node, open.pad)?;
        self.open_inputs.remove(index);
        Ok(src)
    }

    /// Attach a buffer sink to open output `index`, removing it from the list.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] for an unknown index or a link the
    /// scheduler refuses.
    pub fn attach_sink(&mut self, index: usize, formats: NodeFormats) -> CoreResult<NodeId> {
        let Some(open) = self.open_outputs.get(index).cloned() else {
            return Err(vaco_core::Error::InvalidData("no such open output pad"));
        };
        let label = open.label.clone().unwrap_or_else(|| "out".to_owned());
        let sink = self.graph.add_sink(&label, open.media, formats);
        self.graph.connect(open.node, open.pad, sink, 0)?;
        self.open_outputs.remove(index);
        Ok(sink)
    }

    /// Negotiate formats and configure, inserting converters where a link has
    /// no common format.
    ///
    /// `registry` is asked for `scale` and `aresample` instances;
    /// [`DefaultConverters`] decides which, and the `sws_flags=` prefix reaches
    /// them here.
    ///
    /// # Errors
    ///
    /// Whatever `vaco-filter-core` reports. On a negotiation failure,
    /// `graph.last_conflict()` carries the renderable diagnostic.
    pub fn configure(
        &mut self,
        registry: &dyn FilterRegistry,
        mode: AutoConvert,
    ) -> CoreResult<Vec<Conflict>> {
        if mode == AutoConvert::None {
            self.graph.configure()?;
            return Ok(Vec::new());
        }
        let sws = self.sws_opts.clone();
        let swr = self.swr_opts.clone();
        let factory = DefaultConverters::new(sws.clone(), swr.clone());
        self.graph.configure_converting(&factory, |spec| {
            // `configure_converting` rebuilds the spec with an empty `args`
            // before calling us — see the signature gap in the crate docs — so
            // the options are recovered from the factory's own policy here
            // rather than read off the spec.
            let args = factory.args_for(spec.filter);
            let arguments = Vec::new();
            let req = Instantiate {
                name: spec.filter,
                instance: spec.filter,
                args: Some(args.as_str()),
                arguments: &arguments,
            };
            registry
                .create(&req)
                .map(|i| i.filter)
                .map_err(|_| vaco_core::Error::Unsupported("auto-conversion filter unavailable"))
        })
    }

    /// A Graphviz rendering, with the media type on every edge.
    #[must_use]
    pub fn to_dot(&self) -> String {
        use core::fmt::Write as _;
        let mut out = String::from("digraph filtergraph {\n  rankdir=LR;\n");
        for node in &self.nodes {
            let _ = writeln!(
                out,
                "  n{} [label=\"{}\\n({})\"];",
                node.id.0, node.instance, node.filter
            );
        }
        for link in self.graph.links().iter() {
            let _ = writeln!(
                out,
                "  n{} -> n{} [label=\"{:?}\"];",
                link.src().node.0,
                link.dst().node.0,
                link.media()
            );
        }
        out.push_str("}\n");
        out
    }

    /// A textual table, close in spirit to the reference's graph dump.
    ///
    /// Deliberately not byte-identical: it is diagnostic prose, not an
    /// interface fact, and the differential harness allowlists it.
    #[must_use]
    pub fn dump(&self) -> String {
        use core::fmt::Write as _;
        let mut out = String::new();
        for node in &self.nodes {
            let _ = writeln!(out, "{} ({})", node.instance, node.filter);
        }
        for pad in &self.open_inputs {
            let _ = writeln!(
                out,
                "  open input  {} -> node {}:{}",
                pad.label.as_deref().unwrap_or("<unlabelled>"),
                pad.node.0,
                pad.pad
            );
        }
        for pad in &self.open_outputs {
            let _ = writeln!(
                out,
                "  open output {} <- node {}:{}",
                pad.label.as_deref().unwrap_or("<unlabelled>"),
                pad.node.0,
                pad.pad
            );
        }
        out
    }
}

/// A pad waiting to be matched by a label from another chain.
#[derive(Debug, Clone)]
struct Pending {
    label: String,
    node: NodeId,
    pad: u32,
    media: MediaType,
    span: Span,
}

/// Build a graph from a parsed description.
///
/// # Errors
///
/// A span-anchored [`GraphError`]. Render it with the same source string.
pub fn build(ast: &Ast, registry: &dyn FilterRegistry) -> Result<BuiltGraph, GraphError> {
    let mut graph = Graph::new();
    let mut nodes: Vec<NodeInfo> = Vec::new();
    let mut descs = Descs::default();
    let mut open_out: Vec<Pending> = Vec::new();
    let mut open_in: Vec<Pending> = Vec::new();
    let mut unlabelled_in: Vec<OpenPad> = Vec::new();
    let mut unlabelled_out: Vec<OpenPad> = Vec::new();
    let mut instance_names: Vec<String> = Vec::new();
    let mut index = 0usize;

    for chain in &ast.chains {
        // Output pads of the previous filter in this chain that got no label.
        let mut carried: Vec<(NodeId, u32, MediaType)> = Vec::new();

        for spec in &chain.filters {
            let instance = spec.instance_name(index);
            if spec.instance.is_some() && instance_names.contains(&instance) {
                return Err(GraphError::new(
                    ErrorKind::DuplicateInstanceName(instance),
                    spec.name_span,
                ));
            }
            instance_names.push(instance.clone());

            let node = instantiate(spec, &instance, registry, &mut graph, &mut descs)?;
            let (n_in, n_out) = descs.counts(node);
            nodes.push(NodeInfo {
                id: node,
                instance: instance.clone(),
                filter: spec.name.clone(),
                span: spec.span,
            });
            index = index.saturating_add(1);

            if spec.inputs.len() > n_in {
                return Err(GraphError::new(
                    ErrorKind::TooManyInputLabels {
                        filter: spec.name.clone(),
                        given: spec.inputs.len(),
                        has: n_in,
                    },
                    spec.span,
                ));
            }
            if spec.outputs.len() > n_out {
                return Err(GraphError::new(
                    ErrorKind::TooManyOutputLabels {
                        filter: spec.name.clone(),
                        given: spec.outputs.len(),
                        has: n_out,
                    },
                    spec.span,
                ));
            }

            // Labelled inputs take pads 0..k.
            for (pad, label) in spec.inputs.iter().enumerate() {
                let pad = pad as u32;
                let media = descs.input_media(node, pad);
                if let Some(pos) = open_out.iter().position(|p| p.label == label.name) {
                    let src = open_out.remove(pos);
                    link(&mut graph, &descs, &src, node, pad, &instance, label.span)?;
                } else {
                    open_in.push(Pending {
                        label: label.name.clone(),
                        node,
                        pad,
                        media,
                        span: label.span,
                    });
                }
            }

            // The carried list fills what remains, in pad order.
            let mut pad = spec.inputs.len() as u32;
            let mut rest = carried.into_iter();
            while (pad as usize) < n_in {
                let Some((src_node, src_pad, media)) = rest.next() else {
                    break;
                };
                let want = descs.input_media(node, pad);
                if want != media {
                    return Err(GraphError::new(
                        ErrorKind::MediaMismatch {
                            src: format!("{} ({media:?})", graph.label(src_node)),
                            dst: format!("{instance} input {pad} ({want:?})"),
                        },
                        spec.span,
                    ));
                }
                graph
                    .connect(src_node, src_pad, node, pad)
                    .map_err(|e| connect_error(spec, &e))?;
                pad = pad.saturating_add(1);
            }
            for (n, p, media) in rest {
                // More carried outputs than this filter has inputs: they stay
                // open, exactly as the reference leaves them.
                unlabelled_out.push(OpenPad {
                    label: None,
                    node: n,
                    pad: p,
                    media,
                    span: spec.span,
                });
            }
            while (pad as usize) < n_in {
                unlabelled_in.push(OpenPad {
                    label: None,
                    node,
                    pad,
                    media: descs.input_media(node, pad),
                    span: spec.span,
                });
                pad = pad.saturating_add(1);
            }

            // Labelled outputs take pads 0..k; the rest are carried.
            for (pad, label) in spec.outputs.iter().enumerate() {
                let pad = pad as u32;
                let media = descs.output_media(node, pad);
                if let Some(pos) = open_in.iter().position(|p| p.label == label.name) {
                    let dst = open_in.remove(pos);
                    let src = Pending {
                        label: label.name.clone(),
                        node,
                        pad,
                        media,
                        span: label.span,
                    };
                    link(
                        &mut graph, &descs, &src, dst.node, dst.pad, &instance, label.span,
                    )?;
                } else {
                    if let Some(first) = open_out.iter().find(|p| p.label == label.name) {
                        return Err(GraphError::new(
                            ErrorKind::DuplicateOutputLabel {
                                label: label.name.clone(),
                                first: first.span,
                            },
                            label.span,
                        ));
                    }
                    open_out.push(Pending {
                        label: label.name.clone(),
                        node,
                        pad,
                        media,
                        span: label.span,
                    });
                }
            }
            carried = (spec.outputs.len() as u32..n_out as u32)
                .map(|p| (node, p, descs.output_media(node, p)))
                .collect();
        }

        for (node, pad, media) in carried {
            unlabelled_out.push(OpenPad {
                label: None,
                node,
                pad,
                media,
                span: chain.span,
            });
        }
    }

    let mut open_inputs: Vec<OpenPad> = open_in
        .into_iter()
        .map(|p| OpenPad {
            label: Some(p.label),
            node: p.node,
            pad: p.pad,
            media: p.media,
            span: p.span,
        })
        .collect();
    open_inputs.extend(unlabelled_in);
    let mut open_outputs: Vec<OpenPad> = open_out
        .into_iter()
        .map(|p| OpenPad {
            label: Some(p.label),
            node: p.node,
            pad: p.pad,
            media: p.media,
            span: p.span,
        })
        .collect();
    open_outputs.extend(unlabelled_out);

    check_acyclic(&graph, &nodes)?;

    Ok(BuiltGraph {
        graph,
        nodes,
        open_inputs,
        open_outputs,
        sws_opts: ast.sws_flags.clone().unwrap_or_default(),
        swr_opts: String::new(),
    })
}

/// Parse and build in one step.
///
/// # Errors
///
/// As [`crate::ast::parse`] and [`build`].
pub fn parse_and_build(src: &str, registry: &dyn FilterRegistry) -> Result<BuiltGraph, GraphError> {
    let ast = crate::ast::parse(src)?;
    build(&ast, registry)
}

fn instantiate(
    spec: &FilterSpec,
    instance: &str,
    registry: &dyn FilterRegistry,
    graph: &mut Graph,
    descs: &mut Descs,
) -> Result<NodeId, GraphError> {
    if !registry.contains(&spec.name) {
        return Err(GraphError::new(
            ErrorKind::UnknownFilter {
                name: spec.name.clone(),
                suggestion: suggest(&spec.name, registry.names()),
            },
            spec.name_span,
        ));
    }
    let arguments = spec.arguments()?;
    let req = Instantiate {
        name: &spec.name,
        instance,
        args: spec.args.as_deref(),
        arguments: &arguments,
    };
    let built = registry.create(&req).map_err(|detail| {
        GraphError::new(
            ErrorKind::Filter {
                filter: instance.to_owned(),
                detail,
            },
            spec.span,
        )
    })?;
    if built.desc.inputs.len() != built.formats.inputs.len()
        || built.desc.outputs.len() != built.formats.outputs.len()
    {
        return Err(GraphError::new(
            ErrorKind::PadCountMismatch {
                filter: instance.to_owned(),
                detail: format!(
                    "descriptor says {}in/{}out, formats say {}in/{}out",
                    built.desc.inputs.len(),
                    built.desc.outputs.len(),
                    built.formats.inputs.len(),
                    built.formats.outputs.len()
                ),
            },
            spec.span,
        ));
    }
    let mut formats = built.formats;
    instance.clone_into(&mut formats.label);
    descs.push(built.desc);
    Ok(graph.add(built.desc, formats, built.filter))
}

/// Pad media types, kept beside the graph.
///
/// `Graph` has no accessor for a node's descriptor and the builder needs pad
/// counts and media types before anything is connected, so the descriptors are
/// recorded here as nodes are added. They are `Copy`, so this costs nothing.
#[derive(Debug, Default)]
struct Descs(Vec<FilterDesc>);

impl Descs {
    fn push(&mut self, desc: FilterDesc) {
        self.0.push(desc);
    }

    fn counts(&self, node: NodeId) -> (usize, usize) {
        self.0
            .get(node.0 as usize)
            .map_or((0, 0), |d| (d.inputs.len(), d.outputs.len()))
    }

    fn input_media(&self, node: NodeId, pad: u32) -> MediaType {
        self.0
            .get(node.0 as usize)
            .and_then(|d| d.inputs.get(pad as usize))
            .map_or(MediaType::Video, |p| p.media_type)
    }

    fn output_media(&self, node: NodeId, pad: u32) -> MediaType {
        self.0
            .get(node.0 as usize)
            .and_then(|d| d.outputs.get(pad as usize))
            .map_or(MediaType::Video, |p| p.media_type)
    }
}

fn link(
    graph: &mut Graph,
    descs: &Descs,
    src: &Pending,
    dst_node: NodeId,
    dst_pad: u32,
    dst_label: &str,
    span: Span,
) -> Result<(), GraphError> {
    let want = descs.input_media(dst_node, dst_pad);
    if want != src.media {
        return Err(GraphError::new(
            ErrorKind::MediaMismatch {
                src: format!(
                    "{} output {} ({:?})",
                    graph.label(src.node),
                    src.pad,
                    src.media
                ),
                dst: format!("{dst_label} input {dst_pad} ({want:?})"),
            },
            span,
        ));
    }
    graph
        .connect(src.node, src.pad, dst_node, dst_pad)
        .map_err(|e| {
            GraphError::new(
                ErrorKind::Filter {
                    filter: dst_label.to_owned(),
                    detail: e.to_string(),
                },
                span,
            )
        })?;
    Ok(())
}

fn connect_error(spec: &FilterSpec, e: &vaco_core::Error) -> GraphError {
    GraphError::new(
        ErrorKind::Filter {
            filter: spec.name.clone(),
            detail: e.to_string(),
        },
        spec.span,
    )
}

/// Kahn's algorithm over the links that exist so far.
///
/// A filtergraph can only cycle through labels (`[a]anull[a]`), which is rare
/// but reachable from a command line, so it is checked rather than assumed.
/// `Graph::topological_order` also detects it, but reports no participants, and
/// naming them is most of the diagnostic's value.
fn check_acyclic(graph: &Graph, nodes: &[NodeInfo]) -> Result<(), GraphError> {
    let n = graph.node_count();
    let mut indegree = vec![0usize; n];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for l in graph.links().iter() {
        let (s, d) = (l.src().node.0 as usize, l.dst().node.0 as usize);
        if let Some(slot) = indegree.get_mut(d) {
            *slot = slot.saturating_add(1);
        }
        if let Some(list) = edges.get_mut(s) {
            list.push(d);
        }
    }
    let mut queue: Vec<usize> = (0..n)
        .filter(|i| indegree.get(*i).copied() == Some(0))
        .collect();
    let mut seen = 0usize;
    while let Some(i) = queue.pop() {
        seen = seen.saturating_add(1);
        for &d in edges.get(i).map_or(&[][..], Vec::as_slice) {
            if let Some(slot) = indegree.get_mut(d) {
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    queue.push(d);
                }
            }
        }
    }
    if seen == n {
        return Ok(());
    }
    let involved: Vec<String> = (0..n)
        .filter(|i| indegree.get(*i).copied().unwrap_or(0) > 0)
        .filter_map(|i| nodes.get(i).map(|x| x.instance.clone()))
        .collect();
    let span = involved
        .first()
        .and_then(|_| nodes.first().map(|x| x.span))
        .unwrap_or_default();
    Err(GraphError::new(ErrorKind::Cycle(involved), span))
}
