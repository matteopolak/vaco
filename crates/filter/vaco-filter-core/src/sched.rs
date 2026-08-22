//! The cooperative scheduler: node and link arenas, readiness, and quiescence
//! diagnosis.
//!
//! Filters run under an `activate` model rather than async (plan 16 §1.2): one
//! bounded step, then the framework decides who runs next. The two properties
//! that buys, and which this module exists to deliver:
//!
//! 1. **Readiness is computed, not asserted.** The reference requires a filter
//!    to declare "I still have work"; forgetting is a hang, and it is a
//!    recurring bug class there. Here the score comes from observable link
//!    state, so a filter that forgets still runs again if any of its links
//!    changed. [`Activity::Progressed`] is a hint, never the sole mechanism.
//!
//! 2. **Quiescence is diagnosed, not tolerated.** When nothing is runnable and
//!    the sinks are not finished, [`Graph::run`] says which node is blocked on
//!    which link at what depth, instead of hanging.
//!
//! # The borrow that makes this work
//!
//! `activate` needs `&mut` on the filter *and* `&mut` on links its neighbours
//! also touch. Nodes and links therefore live in two separate arenas owned by
//! the [`Graph`]; the driver borrows one field of each. Ordinary disjoint-field
//! borrowing — no `RefCell`, no `Rc`, no `unsafe`.

use std::collections::VecDeque;

use vaco_core::{Error, MediaType, Result, Timestamp};
use vaco_frame::{Frame, FramePool};

use crate::context::NodeLinks;
use crate::link::{Direction, Link, LinkArena, LinkId, NodeId, PadRef, Status, rescale_pts};
use crate::negotiate::{
    AutoConvert, Conflict, ConverterFactory, ConverterSpec, NegotiationPlan, NoConversion,
    NodeFormats, negotiate,
};
use crate::{Activity, Filter, FilterContext, FilterDesc, LinkFormat};

/// How many steps [`Graph::run`] takes before giving up.
///
/// Not a tuning knob: it is what turns a mis-written filter from a hang into a
/// [`GraphStatus::Deadlock`]. Fuzz targets lower it.
pub const DEFAULT_STEP_BUDGET: u64 = 1 << 20;

/// What a node is, from the scheduler's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Filter,
    /// A buffer source: frames arrive from outside through [`Graph::send`].
    Source,
    /// A buffer sink: frames leave through [`Graph::recv`].
    Sink,
}

struct Node {
    kind: Kind,
    /// `None` for sources and sinks, which the scheduler drives itself. Taken
    /// out for the duration of an `activate` call so that the node arena is not
    /// borrowed while the filter runs.
    filter: Option<Box<dyn Filter>>,
    desc: FilterDesc,
    label: String,
    formats: NodeFormats,
    /// For a source, the format the caller declared its frames arrive in.
    declared: Option<LinkFormat>,
    links: NodeLinks,
    retired: bool,
    /// The link-epoch sum this node saw when it last said it could not proceed.
    /// While the sum is unchanged nothing it is waiting on has moved, so
    /// re-running it would be pure spin. This is what makes readiness
    /// *computed*: the wake-up signal is the state change, not the filter's
    /// memory of having asked.
    parked_at: Option<u64>,
    /// The filter said it had more to do right now.
    self_driven: bool,
    /// Step number at which this node last ran, for FIFO tie-breaking.
    last_run: u64,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("filter", &self.filter.as_ref().map(|_| "<dyn Filter>"))
            .field("links", &self.links)
            .field("retired", &self.retired)
            .finish_non_exhaustive()
    }
}

/// How urgently a node wants to run. Higher runs first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// An output is wanted but no input has arrived.
    Wanted = 1,
    /// An input carries an unconsumed end of stream.
    HasStatus = 2,
    /// An input has a queued frame.
    HasFrame = 3,
    /// The filter said it had more to do right now.
    SelfDriven = 4,
}

/// One `activate` call's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// A node ran.
    Stepped,
    /// Nothing was runnable.
    Quiescent,
}

/// Why the graph stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphStatus {
    /// Every sink has seen end of stream. The normal finish.
    Eof,
    /// A source is waiting to be fed. Normal: the caller sends more.
    NeedInput(Vec<NodeId>),
    /// A sink is holding frames. Normal: the caller drains them.
    HasOutput(Vec<NodeId>),
    /// Nothing is runnable, nothing is finished, and nobody is waiting on the
    /// outside world. A bug, reported rather than hung on.
    Deadlock(Vec<Stall>),
    /// The step budget ran out. Also a bug, and also reported.
    BudgetExhausted,
}

/// One node that could not make progress, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    pub node: NodeId,
    pub label: String,
    /// The link it is waiting on, if one can be identified.
    pub link: Option<LinkId>,
    /// That link's queue depth.
    pub queue_depth: usize,
    /// Whether that link has been closed by its producer.
    pub closed: bool,
}

/// A way a filter broke the frame-flow contract.
///
/// Each names a rule from `docs/filter/vaco-filter-core.md`. None is reachable
/// by the scheduler doing something wrong, so every one is a bug in the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Violation {
    /// Returned [`Activity::Progressed`] without taking, pushing or closing
    /// anything. Rule F6. Left unchecked this is a spin, not a hang, which is
    /// worse: it looks like the program is working.
    ProgressWithoutChange,
    /// Returned [`Activity::NeedInput`] when every input is already at end of
    /// stream. Rule F7 — the classic hang: nothing will ever arrive.
    NeedInputAtEof,
    /// Returned [`Activity::Eof`] with an output pad still open. Rule F5.
    EofWithOpenOutput,
    /// Pushed a frame to a pad that had already been closed. Rule F3.
    PushAfterClose,
    /// Pushed a frame whose format does not match the link it went to. Rule F9
    /// — negotiation agreed something the filter then did not honour.
    FrameFormatMismatch,
    /// Ran after returning [`Activity::Eof`]. Rule F5.
    ActivateAfterEof,
}

impl Violation {
    /// A one-line explanation, for a test failure or a log line.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::ProgressWithoutChange => {
                "activate returned Progressed without changing any link; the scheduler would spin"
            }
            Self::NeedInputAtEof => {
                "activate returned NeedInput with every input at end of stream; nothing can arrive"
            }
            Self::EofWithOpenOutput => {
                "activate returned Eof with an output pad still open; downstream would hang"
            }
            Self::PushAfterClose => "a frame was pushed to an output pad that was already closed",
            Self::FrameFormatMismatch => {
                "a frame was pushed whose format does not match the negotiated link format"
            }
            Self::ActivateAfterEof => "activate ran again after it had reported Eof",
        }
    }
}

/// A filter graph: nodes, links, and the driver that runs them.
#[derive(Debug)]
pub struct Graph {
    nodes: Vec<Node>,
    links: LinkArena,
    pool: FramePool,
    configured: bool,
    step: u64,
    budget: u64,
    violations: Vec<Violation>,
    last_conflict: Option<Conflict>,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    /// An empty graph with a fresh frame pool.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            links: LinkArena::new(),
            pool: FramePool::default(),
            configured: false,
            step: 0,
            budget: DEFAULT_STEP_BUDGET,
            violations: Vec::new(),
            last_conflict: None,
        }
    }

    /// Bound how many steps [`Graph::run`] takes before reporting
    /// [`GraphStatus::BudgetExhausted`].
    #[must_use]
    pub const fn with_step_budget(mut self, budget: u64) -> Self {
        self.budget = budget;
        self
    }

    /// Share an existing frame pool, so that a whole pipeline recycles through
    /// one free list rather than one per graph.
    #[must_use]
    pub fn with_pool(mut self, pool: FramePool) -> Self {
        self.pool = pool;
        self
    }

    /// The pool filters allocate from.
    #[must_use]
    pub const fn pool(&self) -> &FramePool {
        &self.pool
    }

    /// Contract violations observed so far. Empty is the only acceptable value
    /// for a graph of correct filters.
    #[must_use]
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Add a filter.
    ///
    /// `formats` is what its pads accept — built here rather than read off the
    /// `&'static` descriptor because a filter's accepted formats routinely
    /// depend on its options and on its realised pad count.
    pub fn add(
        &mut self,
        desc: FilterDesc,
        formats: NodeFormats,
        filter: Box<dyn Filter>,
    ) -> NodeId {
        let label = if formats.label.is_empty() {
            format!("{}@{}", desc.name, self.nodes.len())
        } else {
            formats.label.clone()
        };
        self.push_node(Kind::Filter, desc, formats, label, Some(filter))
    }

    /// Add a buffer source: zero inputs, one output, fed from outside by
    /// [`Graph::send`].
    ///
    /// Lives here rather than in a filter crate because it needs privileged
    /// access to a link's queue, and because it is the API boundary every
    /// consumer of the subsystem uses (plan 16 §1.13).
    pub fn add_source(&mut self, label: &str, media: MediaType, formats: NodeFormats) -> NodeId {
        let desc = source_desc(media);
        self.push_node(Kind::Source, desc, formats, label.to_owned(), None)
    }

    /// Add a buffer sink: one input, zero outputs, drained by [`Graph::recv`].
    pub fn add_sink(&mut self, label: &str, media: MediaType, formats: NodeFormats) -> NodeId {
        let desc = sink_desc(media);
        self.push_node(Kind::Sink, desc, formats, label.to_owned(), None)
    }

    fn push_node(
        &mut self,
        kind: Kind,
        desc: FilterDesc,
        formats: NodeFormats,
        label: String,
        filter: Option<Box<dyn Filter>>,
    ) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        let links = NodeLinks::new(formats.inputs.len(), formats.outputs.len());
        self.nodes.push(Node {
            kind,
            filter,
            desc,
            label,
            formats,
            declared: None,
            links,
            retired: false,
            parked_at: None,
            self_driven: false,
            last_run: 0,
        });
        id
    }

    /// Connect an output pad to an input pad.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for an unknown node or pad, for a pad that is
    /// already connected (fan-out is `split`, never implicit), or for a media
    /// type mismatch — which the reference also diagnoses here rather than
    /// during negotiation.
    pub fn connect(
        &mut self,
        src: NodeId,
        src_pad: u32,
        dst: NodeId,
        dst_pad: u32,
    ) -> Result<LinkId> {
        let src_media = self.pad_media(src, Direction::Output, src_pad)?;
        let dst_media = self.pad_media(dst, Direction::Input, dst_pad)?;
        if self.node(src)?.links.output(src_pad).is_some() {
            return Err(Error::InvalidData(
                "output pad is already connected; fan-out needs a split filter",
            ));
        }
        if self.node(dst)?.links.input(dst_pad).is_some() {
            return Err(Error::InvalidData("input pad is already connected"));
        }
        let link = Link::new(
            PadRef::output(src, src_pad),
            PadRef::input(dst, dst_pad),
            src_media,
            dst_media,
        )?;
        let id = self.links.push(link);
        if let Some(n) = self.nodes.get_mut(src.0 as usize) {
            n.links.set_output(src_pad, id);
        }
        if let Some(n) = self.nodes.get_mut(dst.0 as usize) {
            n.links.set_input(dst_pad, id);
        }
        Ok(id)
    }

    fn pad_media(&self, node: NodeId, direction: Direction, pad: u32) -> Result<MediaType> {
        let n = self.node(node)?;
        let pads = match direction {
            Direction::Input => n.desc.inputs,
            Direction::Output => n.desc.outputs,
        };
        pads.get(pad as usize)
            .map(|p| p.media_type)
            .ok_or(Error::InvalidData("filter has no such pad"))
    }

    fn node(&self, id: NodeId) -> Result<&Node> {
        self.nodes
            .get(id.0 as usize)
            .ok_or(Error::InvalidData("unknown filter node"))
    }

    /// The label a node is known by in diagnostics.
    #[must_use]
    pub fn label(&self, id: NodeId) -> &str {
        self.nodes
            .get(id.0 as usize)
            .map_or("<unknown>", |n| n.label.as_str())
    }

    /// The links, for inspection and for `graphmonitor`-style reporting.
    #[must_use]
    pub const fn links(&self) -> &LinkArena {
        &self.links
    }

    /// How many nodes the graph has.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    // ------------------------------------------------------------ configure

    /// Declare the format a buffer source's frames arrive in.
    ///
    /// Negotiation decides the pixel or sample *format*; everything else on the
    /// link — dimensions, time base, frame rate, sample rate — has to come from
    /// the caller, because nothing inside the graph knows it. Call this before
    /// [`Graph::configure`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the node is not a source.
    pub fn set_source_format(&mut self, node: NodeId, format: LinkFormat) -> Result<()> {
        let idx = node.0 as usize;
        let kind = self.node(node)?.kind;
        if kind != Kind::Source {
            return Err(Error::InvalidData("node is not a buffer source"));
        }
        if let Some(n) = self.nodes.get_mut(idx) {
            n.declared = Some(format);
        }
        Ok(())
    }

    /// Negotiate formats and configure every node, without inserting anything.
    ///
    /// A link whose two sides share no format is an error here. That is the
    /// `-noauto_conversion_filters` behaviour; use
    /// [`Graph::configure_converting`] for the default one. On failure,
    /// [`Graph::last_conflict`] carries the renderable diagnostic, because the
    /// frozen `Result` type can only carry a category.
    ///
    /// # Errors
    ///
    /// Whatever [`negotiate`] reports, or [`Error::InvalidData`] for an
    /// unconnected pad, a cycle, or a link left without a usable format.
    pub fn configure(&mut self) -> Result<()> {
        let mut conflicts = Vec::new();
        let outcome =
            self.configure_inner(&NoConversion, AutoConvert::None, &mut conflicts, |_| {
                Err(Error::Unsupported("auto-conversion is disabled"))
            });
        self.last_conflict = conflicts.into_iter().next();
        outcome
    }

    /// Negotiate formats, inserting converters where a link has no common one.
    ///
    /// `factory` decides *what* to insert (policy, supplied by
    /// `vaco-filter-graph`); `build` turns that decision into an instance. Core
    /// owns neither, which is what keeps layer 5a from having to know that a
    /// filter called `scale` exists.
    ///
    /// Returns any conflict that survived — empty on success.
    ///
    /// # Errors
    ///
    /// As [`Graph::configure`], plus anything `build` reports.
    pub fn configure_converting<B>(
        &mut self,
        factory: &dyn ConverterFactory,
        build: B,
    ) -> Result<Vec<Conflict>>
    where
        B: FnMut(&ConverterSpec) -> Result<Box<dyn Filter>>,
    {
        let mut conflicts = Vec::new();
        let outcome = self.configure_inner(factory, AutoConvert::All, &mut conflicts, build);
        self.last_conflict.clone_from(&conflicts.first().cloned());
        outcome?;
        Ok(conflicts)
    }

    fn configure_inner<B>(
        &mut self,
        factory: &dyn ConverterFactory,
        mode: AutoConvert,
        conflicts: &mut Vec<Conflict>,
        mut build: B,
    ) -> Result<()>
    where
        B: FnMut(&ConverterSpec) -> Result<Box<dyn Filter>>,
    {
        self.check_connected()?;
        let mut plan = NegotiationPlan::new();
        for node in &self.nodes {
            let mut formats = node.formats.clone();
            formats.label.clone_from(&node.label);
            plan.add_node(formats);
        }
        for link in self.links.iter() {
            plan.connect(link.src(), link.dst(), link.media())?;
        }
        let assignment = negotiate(&mut plan, factory, mode, conflicts)?;

        // Mirror every splice into the real arenas, in the same order, so that
        // link ids agree between the plan and the graph.
        for insertion in &assignment.inserted {
            let formats = plan
                .nodes()
                .get(insertion.node.0 as usize)
                .cloned()
                .unwrap_or_default();
            let spec = ConverterSpec {
                filter: insertion.filter,
                args: String::new(),
                formats,
            };
            let filter = build(&spec)?;
            let media = self
                .links
                .get(insertion.link)
                .map_or(MediaType::Video, Link::media);
            let desc = converter_desc(media);
            let node = self.push_node(
                Kind::Filter,
                desc,
                spec.formats,
                insertion.name.clone(),
                Some(filter),
            );
            self.resplice(insertion.link, node, media)?;
        }

        // Walk in topological order. Each node's output links inherit their
        // geometry and time base from its first input (plan 16 §1.8.3: a filter
        // that does not alter timing leaves the time base alone, and the
        // framework does that for it), then take the negotiated format, then the
        // filter gets to override in `configure`.
        for id in self.topological_order()? {
            let idx = id.0 as usize;
            let (kind, declared, links) = {
                let Some(node) = self.nodes.get(idx) else {
                    continue;
                };
                (node.kind, node.declared.clone(), node.links.clone())
            };
            let inherited = if kind == Kind::Source {
                declared
            } else {
                links
                    .input(0)
                    .and_then(|l| self.links.get(l))
                    .map(|l| l.format().clone())
            };
            for pad in 0..links.outputs().len() {
                let Some(link_id) = links.output(pad as u32) else {
                    continue;
                };
                let media = self
                    .links
                    .get(link_id)
                    .map_or(MediaType::Video, Link::media);
                let mut format = inherited
                    .clone()
                    .filter(|f| f.media_type() == media)
                    .unwrap_or_else(|| LinkFormat::unconfigured(media));
                if let Some(set) = assignment.link(link_id) {
                    format.apply(set);
                }
                if let Some(link) = self.links.get_mut(link_id) {
                    link.set_format(format);
                }
            }
            let Some(node) = self.nodes.get_mut(idx) else {
                continue;
            };
            let Some(mut filter) = node.filter.take() else {
                continue;
            };
            let mut ctx = FilterContext::new(&mut self.links, &links, &self.pool);
            let result = filter.configure(&mut ctx);
            if let Some(node) = self.nodes.get_mut(idx) {
                node.filter = Some(filter);
            }
            result?;
        }

        self.validate_configured()?;
        self.configured = true;
        Ok(())
    }

    /// Repoint `link` at `node`'s input and add a link from `node`'s output to
    /// the original consumer, mirroring [`NegotiationPlan::splice`].
    fn resplice(&mut self, link: LinkId, node: NodeId, media: MediaType) -> Result<()> {
        let Some(existing) = self.links.get(link) else {
            return Err(Error::InvalidData("splice names an unknown link"));
        };
        let old_dst = existing.dst();
        let src = existing.src();
        let tail = Link::new(PadRef::output(node, 0), old_dst, media, media)?;
        let head = Link::new(src, PadRef::input(node, 0), media, media)?;
        if let Some(slot) = self.links.get_mut(link) {
            *slot = head;
        }
        let tail_id = self.links.push(tail);
        if let Some(n) = self.nodes.get_mut(node.0 as usize) {
            n.links.set_input(0, link);
            n.links.set_output(0, tail_id);
        }
        if let Some(n) = self.nodes.get_mut(old_dst.node.0 as usize) {
            n.links.set_input(old_dst.pad, tail_id);
        }
        Ok(())
    }

    fn check_connected(&self) -> Result<()> {
        for node in &self.nodes {
            if node.links.inputs().iter().any(Option::is_none) {
                return Err(Error::InvalidData("an input pad is not connected"));
            }
            if node.links.outputs().iter().any(Option::is_none) {
                return Err(Error::InvalidData("an output pad is not connected"));
            }
        }
        Ok(())
    }

    fn validate_configured(&self) -> Result<()> {
        for link in self.links.iter() {
            if !link.is_configured() || !link.format().is_usable() {
                return Err(Error::InvalidData(
                    "a link was left without a usable format after configuration",
                ));
            }
        }
        Ok(())
    }

    /// Nodes in dependency order, sources first.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the graph contains a cycle.
    pub fn topological_order(&self) -> Result<Vec<NodeId>> {
        let mut indegree = vec![0usize; self.nodes.len()];
        for link in self.links.iter() {
            if let Some(d) = indegree.get_mut(link.dst().node.0 as usize) {
                *d += 1;
            }
        }
        let mut queue: VecDeque<NodeId> = (0..self.nodes.len())
            .filter(|i| indegree.get(*i).copied().unwrap_or(0) == 0)
            .map(|i| NodeId(i as u32))
            .collect();
        let mut order = Vec::new();
        while let Some(id) = queue.pop_front() {
            order.push(id);
            let Some(node) = self.nodes.get(id.0 as usize) else {
                continue;
            };
            for out in node.links.outputs().iter().flatten() {
                let Some(link) = self.links.get(*out) else {
                    continue;
                };
                let dst = link.dst().node.0 as usize;
                if let Some(d) = indegree.get_mut(dst) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(NodeId(dst as u32));
                    }
                }
            }
        }
        if order.len() == self.nodes.len() {
            Ok(order)
        } else {
            Err(Error::InvalidData("filtergraph contains a cycle"))
        }
    }

    // --------------------------------------------------------- buffer i/o

    /// Push a frame into a buffer source.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the node is not a source, [`Error::Eof`] if it
    /// has been closed, [`Error::OutputPending`] for backpressure — the frame is
    /// returned to the caller by being left un-consumed, so retry with the same
    /// one.
    pub fn send(&mut self, node: NodeId, frame: Frame) -> Result<()> {
        let id = self.source_link(node)?;
        let Some(link) = self.links.get_mut(id) else {
            return Err(Error::InvalidData("source has no output link"));
        };
        if cfg!(debug_assertions) && !link.format().accepts(&frame) {
            return Err(Error::InvalidData(
                "frame sent to a source does not match the link's negotiated format",
            ));
        }
        link.push(frame)
    }

    /// Whether a buffer source has been asked for a frame.
    ///
    /// The backpressure signal `vaco-sched` reads before pulling from a decoder.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the node is not a source.
    pub fn source_wants(&self, node: NodeId) -> Result<bool> {
        let id = self.source_link(node)?;
        Ok(self
            .links
            .get(id)
            .is_some_and(|l| l.is_wanted() && !l.is_full()))
    }

    /// Declare that a buffer source will produce nothing further.
    ///
    /// `pts` is the timestamp the stream ended at, in the source link's time
    /// base, or [`Timestamp::NONE`] if unknown.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the node is not a source.
    pub fn close_source(&mut self, node: NodeId, pts: Timestamp) -> Result<()> {
        let id = self.source_link(node)?;
        if let Some(link) = self.links.get_mut(id) {
            link.close(Status::Eof, pts);
        }
        Ok(())
    }

    /// Take the next frame from a buffer sink, requesting another.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the node is not a sink; [`Error::Eof`] once
    /// the sink has drained and its input is closed;
    /// [`Error::NeedMoreInput`] when nothing is queued yet.
    pub fn recv(&mut self, node: NodeId) -> Result<Frame> {
        let id = self.sink_link(node)?;
        let Some(link) = self.links.get_mut(id) else {
            return Err(Error::InvalidData("sink has no input link"));
        };
        if let Some(frame) = link.pop() {
            link.request();
            return Ok(frame);
        }
        match link.pop_status() {
            Some(Status::Eof) => Err(Error::Eof),
            Some(Status::Failed) => Err(Error::InvalidData(
                "an upstream filter failed; the failure itself was returned by run_once",
            )),
            None => {
                link.request();
                Err(Error::NeedMoreInput)
            }
        }
    }

    /// The negotiated format at a buffer sink. Valid after configuration.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the node is not a sink.
    pub fn sink_format(&self, node: NodeId) -> Result<&LinkFormat> {
        let id = self.sink_link(node)?;
        self.links
            .get(id)
            .map(Link::format)
            .ok_or(Error::InvalidData("sink has no input link"))
    }

    fn source_link(&self, node: NodeId) -> Result<LinkId> {
        let n = self.node(node)?;
        if n.kind != Kind::Source {
            return Err(Error::InvalidData("node is not a buffer source"));
        }
        n.links
            .output(0)
            .ok_or(Error::InvalidData("source output is not connected"))
    }

    fn sink_link(&self, node: NodeId) -> Result<LinkId> {
        let n = self.node(node)?;
        if n.kind != Kind::Sink {
            return Err(Error::InvalidData("node is not a buffer sink"));
        }
        n.links
            .input(0)
            .ok_or(Error::InvalidData("sink input is not connected"))
    }

    // ------------------------------------------------------------- driving

    /// Run one node, if any is runnable.
    ///
    /// # Errors
    ///
    /// Whatever the filter reports. The failure is also propagated downstream as
    /// [`Status::Failed`] so a sink sees it rather than the graph merely
    /// stopping.
    pub fn run_once(&mut self) -> Result<Progress> {
        let Some(id) = self.pick() else {
            return Ok(Progress::Quiescent);
        };
        self.step = self.step.saturating_add(1);
        let step = self.step;
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return Ok(Progress::Quiescent);
        };
        node.last_run = step;
        node.self_driven = false;
        if node.retired {
            self.violations.push(Violation::ActivateAfterEof);
            return Ok(Progress::Stepped);
        }
        let Some(mut filter) = node.filter.take() else {
            // A source or sink: the scheduler owns its behaviour.
            let kind = node.kind;
            self.drive_endpoint(id, kind);
            return Ok(Progress::Stepped);
        };
        let links = node.links.clone();
        let before = self.links.epoch_sum();
        let mut ctx = FilterContext::new(&mut self.links, &links, &self.pool);
        let outcome = filter.activate(&mut ctx);
        let pushed_bad_format = ctx.saw_format_mismatch();
        let pushed_after_close = ctx.saw_push_after_close();
        if let Some(node) = self.nodes.get_mut(id.0 as usize) {
            node.filter = Some(filter);
        }
        if pushed_bad_format {
            self.violations.push(Violation::FrameFormatMismatch);
        }
        if pushed_after_close {
            self.violations.push(Violation::PushAfterClose);
        }
        let after = self.links.epoch_sum();
        match outcome {
            Ok(activity) => self.apply(id, activity, before != after),
            Err(e) => {
                self.fail_node(id);
                return Err(e);
            }
        }
        Ok(Progress::Stepped)
    }

    fn apply(&mut self, id: NodeId, activity: Activity, changed: bool) {
        match activity {
            Activity::Progressed => {
                if changed {
                    if let Some(n) = self.nodes.get_mut(id.0 as usize) {
                        n.self_driven = true;
                        n.parked_at = None;
                    }
                } else {
                    // A filter that claims progress but changed nothing would
                    // spin forever. Loud, attributable, and not fatal — park it
                    // like any other stalled node so the graph reports a
                    // deadlock instead of burning the step budget.
                    self.violations.push(Violation::ProgressWithoutChange);
                    self.park(id);
                }
            }
            Activity::NeedInput => {
                let all_eof = self.request_inputs(id);
                if all_eof {
                    self.violations.push(Violation::NeedInputAtEof);
                }
                self.park(id);
            }
            Activity::Blocked => self.park(id),
            Activity::Eof => {
                let open = self.close_all_outputs(id);
                if open {
                    self.violations.push(Violation::EofWithOpenOutput);
                }
                if let Some(n) = self.nodes.get_mut(id.0 as usize) {
                    n.retired = true;
                    n.parked_at = None;
                }
            }
        }
    }

    /// Record the link state a node could not proceed from.
    ///
    /// `request_inputs` runs *before* this, so a request it issued is already
    /// folded into the sum and does not immediately un-park the node that made
    /// it.
    fn park(&mut self, id: NodeId) {
        let epoch = self.node_epoch(id);
        if let Some(n) = self.nodes.get_mut(id.0 as usize) {
            n.parked_at = Some(epoch);
            n.self_driven = false;
        }
    }

    /// The sum of the epochs of every link this node touches.
    fn node_epoch(&self, id: NodeId) -> u64 {
        let Some(node) = self.nodes.get(id.0 as usize) else {
            return 0;
        };
        node.links
            .inputs()
            .iter()
            .chain(node.links.outputs().iter())
            .flatten()
            .filter_map(|l| self.links.get(*l))
            .fold(0u64, |acc, l| acc.wrapping_add(l.epoch()))
    }

    /// Mark every input link wanted; report whether they are all already at EOF.
    fn request_inputs(&mut self, id: NodeId) -> bool {
        let Some(node) = self.nodes.get(id.0 as usize) else {
            return false;
        };
        let inputs: Vec<LinkId> = node.links.inputs().iter().flatten().copied().collect();
        if inputs.is_empty() {
            return false;
        }
        let mut all_eof = true;
        for link in inputs {
            if let Some(l) = self.links.get_mut(link) {
                if !l.at_eof() {
                    all_eof = false;
                }
                l.request();
            }
        }
        all_eof
    }

    /// Close every output; report whether any had been left open.
    fn close_all_outputs(&mut self, id: NodeId) -> bool {
        let Some(node) = self.nodes.get(id.0 as usize) else {
            return false;
        };
        let outputs: Vec<LinkId> = node.links.outputs().iter().flatten().copied().collect();
        let mut any_open = false;
        let end = self.node_end_pts(id);
        for link in outputs {
            if let Some(l) = self.links.get_mut(link).filter(|l| !l.is_closed()) {
                any_open = true;
                let pts = rescale_pts(end.0, end.1, l.time_base());
                l.close(Status::Eof, pts);
            }
        }
        any_open
    }

    /// The end timestamp to forward, taken from the first closed input, with
    /// the time base it is expressed in.
    fn node_end_pts(&self, id: NodeId) -> (Timestamp, vaco_core::TimeBase) {
        let Some(node) = self.nodes.get(id.0 as usize) else {
            return (Timestamp::NONE, vaco_core::Rational::UNDEFINED);
        };
        node.links
            .inputs()
            .iter()
            .flatten()
            .find_map(|l| self.links.get(*l))
            .map_or((Timestamp::NONE, vaco_core::Rational::UNDEFINED), |l| {
                (l.end_pts(), l.time_base())
            })
    }

    fn fail_node(&mut self, id: NodeId) {
        let Some(node) = self.nodes.get(id.0 as usize) else {
            return;
        };
        let outputs: Vec<LinkId> = node.links.outputs().iter().flatten().copied().collect();
        for link in outputs {
            if let Some(l) = self.links.get_mut(link) {
                l.close(Status::Failed, Timestamp::NONE);
            }
        }
        if let Some(n) = self.nodes.get_mut(id.0 as usize) {
            n.retired = true;
        }
    }

    /// A source has nothing to do of its own; a sink forwards nothing. Both
    /// exist so that the ends of the graph have link state like everything else.
    fn drive_endpoint(&mut self, id: NodeId, kind: Kind) {
        if kind == Kind::Sink {
            // Ask upstream for more; the caller drains through `recv`.
            if let Some(node) = self.nodes.get(id.0 as usize) {
                let inputs: Vec<LinkId> = node.links.inputs().iter().flatten().copied().collect();
                for link in inputs {
                    if let Some(l) = self
                        .links
                        .get_mut(link)
                        .filter(|l| !l.is_closed() && l.depth() == 0)
                    {
                        l.request();
                    }
                }
            }
        }
        if let Some(n) = self.nodes.get_mut(id.0 as usize) {
            n.self_driven = false;
        }
    }

    /// The highest-priority runnable node.
    ///
    /// Scores are recomputed from link state on every call rather than
    /// maintained incrementally. That is O(nodes × pads) per step, which is
    /// nothing next to a frame of pixels, and it removes the entire class of bug
    /// where a filter or the framework forgets to mark something ready. If a
    /// profile ever shows this mattering, the incremental version keeps the same
    /// scores; it does not change the schedule.
    fn pick(&self) -> Option<NodeId> {
        let mut best: Option<(Priority, u64, NodeId)> = None;
        for (i, node) in self.nodes.iter().enumerate() {
            let id = NodeId(i as u32);
            if node.parked_at == Some(self.node_epoch(id)) {
                continue;
            }
            let Some(priority) = self.score(node) else {
                continue;
            };
            // Highest priority, then the node that ran longest ago (FIFO), then
            // the lowest id. A total order, so two runs schedule identically.
            let candidate = (priority, node.last_run, id);
            let better = match best {
                None => true,
                Some((bp, bl, bid)) => {
                    priority > bp
                        || (priority == bp && candidate.1 < bl)
                        || (priority == bp && candidate.1 == bl && id < bid)
                }
            };
            if better {
                best = Some(candidate);
            }
        }
        best.map(|(_, _, id)| id)
    }

    fn score(&self, node: &Node) -> Option<Priority> {
        if node.retired {
            return None;
        }
        match node.kind {
            // Sources are driven from outside; sinks are drained from outside.
            // Neither is ever "runnable" on its own account, so neither can keep
            // the driver spinning.
            Kind::Source | Kind::Sink => None,
            Kind::Filter => {
                if node.self_driven {
                    return Some(Priority::SelfDriven);
                }
                let mut has_frame = false;
                let mut has_status = false;
                for link in node.links.inputs().iter().flatten() {
                    let Some(l) = self.links.get(*link) else {
                        continue;
                    };
                    if l.depth() > 0 {
                        has_frame = true;
                    } else if l.is_closed() {
                        has_status = true;
                    }
                }
                let mut wanted = false;
                let mut room = node.links.outputs().is_empty();
                for link in node.links.outputs().iter().flatten() {
                    let Some(l) = self.links.get(*link) else {
                        continue;
                    };
                    if l.is_wanted() {
                        wanted = true;
                    }
                    if !l.is_full() && !l.is_closed() {
                        room = true;
                    }
                }
                if has_frame && room {
                    Some(Priority::HasFrame)
                } else if has_status {
                    Some(Priority::HasStatus)
                } else if wanted && node.links.inputs().is_empty() {
                    // A generator: nothing upstream to wait for.
                    Some(Priority::Wanted)
                } else {
                    None
                }
            }
        }
    }

    /// Run until nothing is runnable, then say why.
    ///
    /// # Errors
    ///
    /// Propagates a filter failure.
    pub fn run(&mut self) -> Result<GraphStatus> {
        let start = self.step;
        loop {
            if self.step.saturating_sub(start) >= self.budget {
                return Ok(GraphStatus::BudgetExhausted);
            }
            match self.run_once()? {
                Progress::Stepped => {}
                Progress::Quiescent => break,
            }
        }
        Ok(self.classify())
    }

    /// Why the graph is not running.
    #[must_use]
    pub fn classify(&self) -> GraphStatus {
        let mut sinks_done = true;
        let mut has_output = Vec::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.kind != Kind::Sink {
                continue;
            }
            let drained_and_closed = node
                .links
                .inputs()
                .iter()
                .flatten()
                .filter_map(|l| self.links.get(*l))
                .all(Link::at_eof);
            if !drained_and_closed {
                sinks_done = false;
            }
            let pending = node
                .links
                .inputs()
                .iter()
                .flatten()
                .filter_map(|l| self.links.get(*l))
                .any(|l| l.depth() > 0);
            if pending {
                has_output.push(NodeId(i as u32));
            }
        }
        if !has_output.is_empty() {
            return GraphStatus::HasOutput(has_output);
        }
        if sinks_done && self.nodes.iter().any(|n| n.kind == Kind::Sink) {
            return GraphStatus::Eof;
        }
        let mut waiting = Vec::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.kind != Kind::Source {
                continue;
            }
            let open_and_wanted = node
                .links
                .outputs()
                .iter()
                .flatten()
                .filter_map(|l| self.links.get(*l))
                .any(|l| !l.is_closed());
            if open_and_wanted {
                waiting.push(NodeId(i as u32));
            }
        }
        if !waiting.is_empty() {
            return GraphStatus::NeedInput(waiting);
        }
        GraphStatus::Deadlock(self.stalls())
    }

    fn stalls(&self) -> Vec<Stall> {
        let mut out = Vec::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.retired || node.kind != Kind::Filter {
                continue;
            }
            // The link it is actually waiting on, or — when every input is
            // already finished, which is the more confusing case — the first
            // input, so that `closed` and `queue_depth` still say something.
            let inputs = node.links.inputs();
            let blocking = inputs
                .iter()
                .flatten()
                .find(|l| self.links.get(**l).is_some_and(|l| !l.at_eof()))
                .or_else(|| inputs.iter().flatten().next())
                .copied();
            let (depth, closed) = blocking
                .and_then(|l| self.links.get(l))
                .map_or((0, false), |l| (l.depth(), l.is_closed()));
            out.push(Stall {
                node: NodeId(i as u32),
                label: node.label.clone(),
                link: blocking,
                queue_depth: depth,
                closed,
            });
        }
        out
    }

    /// Discard every queued frame and terminal status, keeping the negotiated
    /// formats. What a seek does.
    ///
    /// Links are cleared first, then every filter is told, so a filter's
    /// `flush` sees the post-seek link state rather than the old one.
    pub fn flush(&mut self) {
        for link in self.links.iter_mut() {
            link.flush();
        }
        for node in &mut self.nodes {
            if let Some(filter) = node.filter.as_mut() {
                filter.flush();
            }
            node.retired = false;
            node.self_driven = false;
            node.parked_at = None;
        }
        self.violations.clear();
    }
}

impl Graph {
    /// The most recent negotiation conflict, if configuration failed.
    ///
    /// Reachable separately because the frozen `Result` carries only an error
    /// category, and the whole point of the diagnostic is the detail.
    #[must_use]
    pub const fn last_conflict(&self) -> Option<&Conflict> {
        self.last_conflict.as_ref()
    }

    /// Whether [`Graph::configure`] has succeeded.
    #[must_use]
    pub const fn is_configured(&self) -> bool {
        self.configured
    }
}

fn source_desc(media: MediaType) -> FilterDesc {
    FilterDesc {
        name: if media == MediaType::Audio {
            "abuffer"
        } else {
            "buffer"
        },
        description: "buffer the caller feeds frames into",
        inputs: &[],
        outputs: pads_for(media),
        flags: crate::FilterFlags::empty(),
    }
}

fn sink_desc(media: MediaType) -> FilterDesc {
    FilterDesc {
        name: if media == MediaType::Audio {
            "abuffersink"
        } else {
            "buffersink"
        },
        description: "buffer the caller drains frames from",
        inputs: pads_for(media),
        outputs: &[],
        flags: crate::FilterFlags::empty(),
    }
}

fn converter_desc(media: MediaType) -> FilterDesc {
    FilterDesc {
        name: "auto_convert",
        description: "automatically inserted format converter",
        inputs: pads_for(media),
        outputs: pads_for(media),
        flags: crate::FilterFlags::empty(),
    }
}

const VIDEO_PAD: &[crate::Pad] = &[crate::Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_PAD: &[crate::Pad] = &[crate::Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

const fn pads_for(media: MediaType) -> &'static [crate::Pad] {
    match media {
        MediaType::Audio => AUDIO_PAD,
        _ => VIDEO_PAD,
    }
}
