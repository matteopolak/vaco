//! Format negotiation across filter links.
//!
//! Each pad declares what it accepts; the graph then finds one assignment
//! satisfying every link, inserting conversion filters where no common format
//! exists. Expressed as constraint sets plus a "must be equal" relation over
//! pads, which handles the common case — a filter that does not care what the
//! format is, only that input and output agree — without special-casing it.
//!
//! # The model, in one paragraph
//!
//! Negotiation is a **union-find over `(pad, property)` pairs**. A pad declares
//! a [`Constraint`] per property. A filter declares *ties*: "these of my pads
//! must resolve to the same value for this property". Every link adds a tie
//! between the two pads it joins. Classes are merged in a fixed order — node
//! ties first (in node order), then links (in [`LinkId`] order) — and each merge
//! intersects the two classes' constraints. The merge that first empties a class
//! **is** the conflict, and because merges are ordered, which link gets named is
//! deterministic. Conflicts are repaired by splicing a converter node into the
//! offending link; conversion is the only repair, and a converter declares
//! concrete, untied sets on its two pads, so the link it repairs cannot conflict
//! again for the same property.
//!
//! # Termination
//!
//! [`negotiate`] runs at most `3 × links + 1` rounds and then reports
//! [`Error::Unsupported`] rather than looping. That bound is defensive: the real
//! argument is that each round repairs at least one `(link, property)` pair, a
//! repaired pair cannot recur, and there are finitely many pairs. If the bound
//! is ever hit, a `ConverterFactory` is returning a converter that does not fix
//! what it claimed to fix — a policy bug, and one worth failing loudly on.
//!
//! # When no assignment exists
//!
//! Three distinct outcomes, deliberately not collapsed into one error:
//!
//! | Situation | Result |
//! |---|---|
//! | Two pads of the *same node* are tied and share nothing | [`Error::InvalidData`] — the filter's own declaration is contradictory |
//! | A link's two sides share nothing and auto-conversion is off, or the factory offers no converter | [`Error::Unsupported`], with a [`Conflict`] rendered by [`Conflict::render`] |
//! | A class ends up entirely unconstrained (`Any` everywhere) | [`Error::Unsupported`] — we refuse to invent a format |
//!
//! The last one deserves a note. It is tempting to default an unconstrained
//! pixel format to `yuv420p`, and it is wrong: a graph with no source
//! constraint has not told us what it carries, and picking silently is how a
//! pipeline ends up transcoding through 8-bit 4:2:0 because nobody said
//! otherwise. In practice a buffer source pins its class with
//! [`Constraint::Exact`], so this outcome only fires on a graph that was never
//! going to work.

use std::fmt::Write as _;

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, MediaType, Result};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

use crate::link::{Direction, LinkId, NodeId, PadRef};

pub mod loss;

/// What one pad will accept for a single property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint<T> {
    /// Any value the peer proposes.
    Any,
    /// One of these, in preference order.
    OneOf(Vec<T>),
    /// Exactly this.
    Exact(T),
}

impl<T: Clone + PartialEq> Constraint<T> {
    /// Intersect two constraints.
    ///
    /// `None` means the two share no value. Membership is commutative;
    /// **preference order is not** — `self`'s order is kept, which is what makes
    /// the fold order in [`negotiate`] load-bearing and why that order is
    /// pinned rather than incidental.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Any, o) => Some(o.clone()),
            (s, Self::Any) => Some(s.clone()),
            (Self::Exact(a), Self::Exact(b)) => (a == b).then(|| Self::Exact(a.clone())),
            (Self::Exact(a), Self::OneOf(b)) => b.contains(a).then(|| Self::Exact(a.clone())),
            (Self::OneOf(a), Self::Exact(b)) => a.contains(b).then(|| Self::Exact(b.clone())),
            (Self::OneOf(a), Self::OneOf(b)) => {
                let kept: Vec<T> = a.iter().filter(|x| b.contains(x)).cloned().collect();
                match kept.len() {
                    0 => None,
                    1 => kept.into_iter().next().map(Self::Exact),
                    _ => Some(Self::OneOf(kept)),
                }
            }
        }
    }

    /// Whether `value` satisfies this constraint.
    #[must_use]
    pub fn allows(&self, value: &T) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(t) => t == value,
            Self::OneOf(v) => v.contains(value),
        }
    }

    /// The single value this constraint forces, if it forces one.
    #[must_use]
    pub fn resolved(&self) -> Option<&T> {
        match self {
            Self::Exact(t) => Some(t),
            Self::OneOf(v) if v.len() == 1 => v.first(),
            _ => None,
        }
    }

    /// The candidates, best first. Empty for [`Constraint::Any`], which is
    /// "unconstrained" rather than "nothing".
    #[must_use]
    pub fn candidates(&self) -> &[T] {
        match self {
            Self::Any => &[],
            Self::Exact(t) => std::slice::from_ref(t),
            Self::OneOf(v) => v.as_slice(),
        }
    }

    /// Whether this constraint names no value at all.
    ///
    /// Only reachable by constructing `OneOf(vec![])` by hand; [`Constraint::intersect`]
    /// reports emptiness as `None` rather than producing one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::OneOf(v) if v.is_empty())
    }

    /// Normalise `OneOf` of length one into [`Constraint::Exact`], so that two
    /// constraints naming the same single value compare equal.
    #[must_use]
    pub fn normalised(self) -> Self {
        match self {
            Self::OneOf(mut v) if v.len() == 1 => match v.pop() {
                Some(t) => Self::Exact(t),
                None => Self::OneOf(v),
            },
            other => other,
        }
    }
}

/// The full constraint set for one pad.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatSet {
    pub pixel_formats: Option<Constraint<PixFmt>>,
    pub sample_formats: Option<Constraint<SampleFmt>>,
    pub sample_rates: Option<Constraint<u32>>,
    pub channel_layouts: Option<Constraint<ChannelLayout>>,
}

impl FormatSet {
    /// Intersect two pads' constraints.
    ///
    /// `None` means no common format exists and a conversion filter must be
    /// inserted between them.
    ///
    /// A property that is `None` on either side is "unconstrained here" and
    /// takes the other side's value, so `None` and [`Constraint::Any`] behave
    /// identically. That is deliberate: it lets an audio pad leave the video
    /// properties `None` without every fold having to know the media type.
    ///
    /// Use [`FormatSet::intersect_detailed`] when the *reason* matters —
    /// this signature cannot say which property failed.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        self.intersect_detailed(other).ok()
    }

    /// [`FormatSet::intersect`], reporting which property has no common value.
    ///
    /// Properties are tested in [`Property::ALL`] order, so the property named
    /// on a multi-property conflict is stable across runs (D6).
    ///
    /// # Errors
    ///
    /// The first [`Property`] on which the two sides share nothing.
    pub fn intersect_detailed(&self, other: &Self) -> std::result::Result<Self, Property> {
        Ok(Self {
            pixel_formats: merge(
                self.pixel_formats.as_ref(),
                other.pixel_formats.as_ref(),
                Property::PixelFormat,
            )?,
            sample_formats: merge(
                self.sample_formats.as_ref(),
                other.sample_formats.as_ref(),
                Property::SampleFormat,
            )?,
            sample_rates: merge(
                self.sample_rates.as_ref(),
                other.sample_rates.as_ref(),
                Property::SampleRate,
            )?,
            channel_layouts: merge(
                self.channel_layouts.as_ref(),
                other.channel_layouts.as_ref(),
                Property::ChannelLayout,
            )?,
        })
    }

    /// A video pad accepting exactly one pixel format.
    #[must_use]
    pub fn video_exact(format: PixFmt) -> Self {
        Self {
            pixel_formats: Some(Constraint::Exact(format)),
            ..Self::default()
        }
    }

    /// A video pad accepting a list of pixel formats, best first.
    #[must_use]
    pub fn video_list<I: IntoIterator<Item = PixFmt>>(formats: I) -> Self {
        Self {
            pixel_formats: Some(Constraint::OneOf(formats.into_iter().collect()).normalised()),
            ..Self::default()
        }
    }

    /// An audio pad accepting exactly one sample format, rate and layout.
    #[must_use]
    pub fn audio_exact(format: SampleFmt, rate: u32, layout: ChannelLayout) -> Self {
        Self {
            sample_formats: Some(Constraint::Exact(format)),
            sample_rates: Some(Constraint::Exact(rate)),
            channel_layouts: Some(Constraint::Exact(layout)),
            ..Self::default()
        }
    }

    /// The constraint this set places on one property.
    #[must_use]
    pub fn get(&self, property: Property) -> AnyConstraint<'_> {
        match property {
            Property::PixelFormat => AnyConstraint::PixelFormat(self.pixel_formats.as_ref()),
            Property::SampleFormat => AnyConstraint::SampleFormat(self.sample_formats.as_ref()),
            Property::SampleRate => AnyConstraint::SampleRate(self.sample_rates.as_ref()),
            Property::ChannelLayout => AnyConstraint::ChannelLayout(self.channel_layouts.as_ref()),
        }
    }

    /// Whether every property this set constrains is resolved to one value.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        Property::ALL.iter().all(|&p| match self.get(p) {
            AnyConstraint::PixelFormat(c) => c.is_none_or(|c| c.resolved().is_some()),
            AnyConstraint::SampleFormat(c) => c.is_none_or(|c| c.resolved().is_some()),
            AnyConstraint::SampleRate(c) => c.is_none_or(|c| c.resolved().is_some()),
            AnyConstraint::ChannelLayout(c) => c.is_none_or(|c| c.resolved().is_some()),
        })
    }
}

fn merge<T: Clone + PartialEq>(
    a: Option<&Constraint<T>>,
    b: Option<&Constraint<T>>,
    property: Property,
) -> std::result::Result<Option<Constraint<T>>, Property> {
    match (a, b) {
        (None, None) => Ok(None),
        (Some(x), None) | (None, Some(x)) => Ok(Some(x.clone())),
        (Some(x), Some(y)) => x.intersect(y).map(Some).ok_or(property),
    }
}

/// One negotiable property of a link.
///
/// The set is deliberately small: these four are what
/// [`LinkFormat`](crate::LinkFormat) carries and therefore what a link can
/// actually be wrong about. Colour space, range, primaries and transfer are
/// *carried* on the link rather than negotiated — see the signature gaps
/// section of `docs/filter/vaco-filter-core.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Property {
    PixelFormat,
    SampleFormat,
    SampleRate,
    ChannelLayout,
}

impl Property {
    /// Every property, in the order conflicts are reported in.
    pub const ALL: [Self; 4] = [
        Self::PixelFormat,
        Self::SampleFormat,
        Self::SampleRate,
        Self::ChannelLayout,
    ];

    /// The properties that apply to one media type.
    #[must_use]
    pub const fn for_media(media: MediaType) -> &'static [Self] {
        match media {
            MediaType::Video => &[Self::PixelFormat],
            MediaType::Audio => &[Self::SampleFormat, Self::SampleRate, Self::ChannelLayout],
            _ => &[],
        }
    }

    /// The option-style name, for diagnostics. These are the reference's own
    /// spellings, which are interface facts (D9).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PixelFormat => "pix_fmt",
            Self::SampleFormat => "sample_fmt",
            Self::SampleRate => "sample_rate",
            Self::ChannelLayout => "channel_layout",
        }
    }
}

/// A borrowed constraint of whichever type the property carries.
///
/// Exists so that generic code can walk [`Property::ALL`] without the caller
/// naming four types.
#[derive(Debug, Clone, Copy)]
pub enum AnyConstraint<'a> {
    PixelFormat(Option<&'a Constraint<PixFmt>>),
    SampleFormat(Option<&'a Constraint<SampleFmt>>),
    SampleRate(Option<&'a Constraint<u32>>),
    ChannelLayout(Option<&'a Constraint<ChannelLayout>>),
}

impl AnyConstraint<'_> {
    /// The candidate values, rendered for a diagnostic. Empty means
    /// unconstrained.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        match self {
            Self::PixelFormat(Some(c)) => {
                c.candidates().iter().map(|f| f.name().to_owned()).collect()
            }
            Self::SampleFormat(Some(c)) => {
                c.candidates().iter().map(|f| f.name().to_owned()).collect()
            }
            Self::SampleRate(Some(c)) => c.candidates().iter().map(u32::to_string).collect(),
            Self::ChannelLayout(Some(c)) => {
                c.candidates().iter().map(ChannelLayout::describe).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Whether the constraint is absent or [`Constraint::Any`].
    #[must_use]
    pub fn is_unconstrained(&self) -> bool {
        match self {
            Self::PixelFormat(c) => c.is_none_or(|c| matches!(c, Constraint::Any)),
            Self::SampleFormat(c) => c.is_none_or(|c| matches!(c, Constraint::Any)),
            Self::SampleRate(c) => c.is_none_or(|c| matches!(c, Constraint::Any)),
            Self::ChannelLayout(c) => c.is_none_or(|c| matches!(c, Constraint::Any)),
        }
    }
}

/// One filter's declaration: what each of its pads accepts, and which of them
/// must agree.
///
/// Built at instantiation, not at registration, because a filter's accepted
/// formats routinely depend on its options (`format=pix_fmts=rgb24`) and on its
/// realised pad count (`amix=inputs=3`). A `&'static` descriptor cannot carry
/// either.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeFormats {
    pub inputs: Vec<FormatSet>,
    pub outputs: Vec<FormatSet>,
    /// Pads of *this node* that must resolve to the same value.
    pub ties: Vec<Tie>,
    /// What to call this node in a diagnostic.
    pub label: String,
}

impl NodeFormats {
    /// The most common shape: every pad accepts anything, and all pads of the
    /// same media type must agree. A filter that transforms pixels without
    /// caring which pixels.
    #[must_use]
    pub fn passthrough(inputs: usize, outputs: usize, media: MediaType, label: &str) -> Self {
        Self {
            inputs: vec![FormatSet::default(); inputs],
            outputs: vec![FormatSet::default(); outputs],
            ties: Tie::all_pads(inputs, outputs, media),
            label: label.to_owned(),
        }
    }

    /// Every pad carries `set`, and all pads are tied.
    #[must_use]
    pub fn uniform(
        inputs: usize,
        outputs: usize,
        media: MediaType,
        set: &FormatSet,
        label: &str,
    ) -> Self {
        Self {
            inputs: vec![set.clone(); inputs],
            outputs: vec![set.clone(); outputs],
            ties: Tie::all_pads(inputs, outputs, media),
            label: label.to_owned(),
        }
    }

    /// A converter: concrete sets on both pads, and **no ties**, because
    /// converting is precisely the act of not agreeing.
    #[must_use]
    pub fn converter(input: FormatSet, output: FormatSet, label: &str) -> Self {
        Self {
            inputs: vec![input],
            outputs: vec![output],
            ties: Vec::new(),
            label: label.to_owned(),
        }
    }

    fn set(&self, direction: Direction, pad: u32) -> Option<&FormatSet> {
        match direction {
            Direction::Input => self.inputs.get(pad as usize),
            Direction::Output => self.outputs.get(pad as usize),
        }
    }
}

/// "These pads of one node must resolve to the same value for this property."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tie {
    pub property: Property,
    /// `(direction, pad index)` pairs, local to the node.
    pub pads: Vec<(Direction, u32)>,
}

impl Tie {
    /// Tie every pad of `media` for every property that media has — the
    /// declaration behind `Passthrough` and `Static` in plan 16 §1.6.1.
    #[must_use]
    pub fn all_pads(inputs: usize, outputs: usize, media: MediaType) -> Vec<Self> {
        let pads: Vec<(Direction, u32)> = (0..inputs)
            .map(|i| (Direction::Input, i as u32))
            .chain((0..outputs).map(|i| (Direction::Output, i as u32)))
            .collect();
        if pads.len() < 2 {
            return Vec::new();
        }
        Property::for_media(media)
            .iter()
            .map(|&property| Self {
                property,
                pads: pads.clone(),
            })
            .collect()
    }
}

/// One link, as the negotiator sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkEnds {
    pub src: PadRef,
    pub dst: PadRef,
    pub media: MediaType,
}

/// The node and link sets negotiation runs over.
///
/// Owned and mutable because repair splices converter nodes in. The graph hands
/// one of these to [`negotiate`] and reads the resulting node ordering back.
#[derive(Debug, Clone, Default)]
pub struct NegotiationPlan {
    nodes: Vec<NodeFormats>,
    links: Vec<LinkEnds>,
}

impl NegotiationPlan {
    /// An empty plan.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            nodes: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Add a node, returning its id.
    pub fn add_node(&mut self, node: NodeFormats) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    /// Connect an output pad to an input pad.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when either endpoint names a pad the node does
    /// not have, or when the directions are the wrong way round.
    pub fn connect(&mut self, src: PadRef, dst: PadRef, media: MediaType) -> Result<LinkId> {
        if src.direction != Direction::Output || dst.direction != Direction::Input {
            return Err(Error::InvalidData(
                "a link runs from an output pad to an input pad",
            ));
        }
        self.pad_set(src).ok_or(Error::InvalidData(
            "link names an output pad that does not exist",
        ))?;
        self.pad_set(dst).ok_or(Error::InvalidData(
            "link names an input pad that does not exist",
        ))?;
        let id = LinkId(self.links.len() as u32);
        self.links.push(LinkEnds { src, dst, media });
        Ok(id)
    }

    /// The nodes, in id order.
    #[must_use]
    pub fn nodes(&self) -> &[NodeFormats] {
        &self.nodes
    }

    /// The links, in id order.
    #[must_use]
    pub fn links(&self) -> &[LinkEnds] {
        &self.links
    }

    /// Splice `converter` into `link`, so that `src -> converter -> dst`.
    ///
    /// The original link is rewritten to end at the converter's input, and a new
    /// link is appended from the converter's output to the original consumer.
    /// Appending rather than inserting keeps every existing [`LinkId`] valid,
    /// which is what lets the caller hold ids across a repair round.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for an unknown link id, or a converter that does
    /// not have exactly one input and one output pad.
    pub fn splice(&mut self, link: LinkId, converter: NodeFormats) -> Result<(NodeId, LinkId)> {
        if converter.inputs.len() != 1 || converter.outputs.len() != 1 {
            return Err(Error::InvalidData(
                "a converter must have exactly one input pad and one output pad",
            ));
        }
        let ends = *self
            .links
            .get(link.0 as usize)
            .ok_or(Error::InvalidData("splice names an unknown link"))?;
        let node = self.add_node(converter);
        if let Some(slot) = self.links.get_mut(link.0 as usize) {
            slot.dst = PadRef::input(node, 0);
        }
        let tail = LinkId(self.links.len() as u32);
        self.links.push(LinkEnds {
            src: PadRef::output(node, 0),
            dst: ends.dst,
            media: ends.media,
        });
        Ok((node, tail))
    }

    fn pad_set(&self, pad: PadRef) -> Option<&FormatSet> {
        self.nodes
            .get(pad.node.0 as usize)?
            .set(pad.direction, pad.pad)
    }

    fn label(&self, node: NodeId) -> &str {
        self.nodes
            .get(node.0 as usize)
            .map_or("<unknown>", |n| n.label.as_str())
    }

    /// Every `(node, direction, pad)` flattened to a dense index.
    fn pad_index(&self) -> PadIndex {
        let mut offsets = Vec::new();
        let mut next = 0usize;
        for node in &self.nodes {
            offsets.push((next, next + node.inputs.len()));
            next += node.inputs.len() + node.outputs.len();
        }
        PadIndex {
            offsets,
            total: next,
        }
    }
}

struct PadIndex {
    /// Per node: (first input slot, first output slot).
    offsets: Vec<(usize, usize)>,
    total: usize,
}

impl PadIndex {
    fn of(&self, pad: PadRef) -> Option<usize> {
        let &(inputs, outputs) = self.offsets.get(pad.node.0 as usize)?;
        let base = match pad.direction {
            Direction::Input => inputs,
            Direction::Output => outputs,
        };
        let slot = base.checked_add(pad.pad as usize)?;
        (slot < self.total).then_some(slot)
    }
}

/// Disjoint-set forest over pad slots, with union by size and path halving.
#[derive(Debug)]
struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent.get(x).copied().unwrap_or(x) != x {
            let parent = self.parent.get(x).copied().unwrap_or(x);
            let grand = self.parent.get(parent).copied().unwrap_or(parent);
            if let Some(slot) = self.parent.get_mut(x) {
                *slot = grand;
            }
            x = grand;
        }
        x
    }

    /// Merge, returning the new representative, or `None` if already merged.
    fn union(&mut self, a: usize, b: usize) -> Option<usize> {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return None;
        }
        if self.size.get(ra).copied().unwrap_or(0) < self.size.get(rb).copied().unwrap_or(0) {
            std::mem::swap(&mut ra, &mut rb);
        }
        if let Some(slot) = self.parent.get_mut(rb) {
            *slot = ra;
        }
        let add = self.size.get(rb).copied().unwrap_or(0);
        if let Some(slot) = self.size.get_mut(ra) {
            *slot += add;
        }
        Some(ra)
    }
}

/// Which nodes narrowed a class, and how, for the diagnostic renderer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    /// Node ids that contributed a non-`Any` constraint, in merge order.
    pub narrowed_by: SmallVec<[NodeId; 4]>,
}

impl Provenance {
    fn record(&mut self, node: NodeId) {
        if !self.narrowed_by.contains(&node) {
            self.narrowed_by.push(node);
        }
    }

    fn absorb(&mut self, other: &Self) {
        for &n in &other.narrowed_by {
            self.record(n);
        }
    }
}

/// A link whose two sides share no value for one property.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub link: LinkId,
    pub property: Property,
    pub media: MediaType,
    /// The producing pad, and what its side of the graph accepts.
    pub upstream: ConflictSide,
    /// The consuming pad, and what its side accepts.
    pub downstream: ConflictSide,
    /// Whether auto-conversion was available but no converter was offered.
    pub auto_convert_disabled: bool,
}

/// One side of a [`Conflict`].
#[derive(Debug, Clone)]
pub struct ConflictSide {
    pub pad: PadRef,
    pub label: String,
    /// The candidate values, best first, already rendered.
    pub accepts: Vec<String>,
    /// The nodes that narrowed this side, upstream-most first.
    pub narrowed_by: Vec<String>,
}

impl Conflict {
    /// Render the diagnostic.
    ///
    /// Names the *narrowing* nodes rather than only the two link endpoints,
    /// says what auto-conversion would have done, and prints a concrete fix —
    /// the three things plan 16 §1.6.5 identifies as missing from the
    /// reference's "Impossible to convert between the formats supported by
    /// filter X and filter Y".
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "format negotiation failed for `{}` on link {}:{} -> {}:{}",
            self.property.name(),
            self.upstream.label,
            self.upstream.pad.pad,
            self.downstream.label,
            self.downstream.pad.pad,
        );
        let _ = writeln!(
            s,
            "\n  the link requires one common {}, but the two sides share none:\n",
            self.property.name()
        );
        render_side(&mut s, "upstream side  ", &self.upstream);
        let _ = writeln!(s);
        render_side(&mut s, "downstream side", &self.downstream);
        if self.auto_convert_disabled {
            let _ = writeln!(
                s,
                "\n  auto-conversion is disabled; a converter would normally have been\n  \
                 inserted here."
            );
        } else {
            let _ = writeln!(
                s,
                "\n  no converter is available for `{}` on a {} link.",
                self.property.name(),
                media_name(self.media)
            );
        }
        if let Some(first) = self.downstream.accepts.first() {
            let _ = writeln!(
                s,
                "\n  fix: enable auto-conversion, or convert to `{first}` before {}.",
                self.downstream.label
            );
        }
        s
    }
}

fn render_side(s: &mut String, which: &str, side: &ConflictSide) {
    let shown = 6usize;
    let list = side
        .accepts
        .iter()
        .take(shown)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let extra = side.accepts.len().saturating_sub(shown);
    let _ = write!(s, "    {which} accepts  ");
    if side.accepts.is_empty() {
        let _ = writeln!(s, "(anything)");
    } else if extra > 0 {
        let _ = writeln!(s, "{list}  (+{extra} more)");
    } else {
        let _ = writeln!(s, "{list}");
    }
    for node in &side.narrowed_by {
        let _ = writeln!(s, "      narrowed by   {node}");
    }
}

const fn media_name(media: MediaType) -> &'static str {
    match media {
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Subtitle => "subtitle",
        MediaType::Data => "data",
        MediaType::Attachment => "attachment",
    }
}

/// Whether the graph may insert conversion filters.
///
/// Surfaced by the CLI as `-noauto_conversion_filters`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoConvert {
    /// Insert converters where a link has no common format. The default.
    #[default]
    All,
    /// Never insert. A link with no common format is an error.
    None,
}

/// What to insert to fix a mismatch on a link.
///
/// `vaco-filter-core` must not know that a filter named `scale` exists — layer
/// 5a cannot depend on layer 5b — so core defines the mechanism and the graph
/// layer supplies the policy (plan 16 §1.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConverterSpec {
    /// The filter to instantiate, e.g. `"scale"` or `"aresample"`.
    pub filter: &'static str,
    /// Arguments for it, already rendered.
    pub args: String,
    /// The formats the converter will accept on its input and produce on its
    /// output. Must be concrete enough that the repaired link cannot conflict
    /// again, or [`negotiate`] will hit its round bound.
    pub formats: NodeFormats,
}

/// Supplies converters. Implemented by `vaco-filter-graph`.
pub trait ConverterFactory {
    /// What to insert to fix `properties` on this link, if anything.
    ///
    /// `properties` is every property that conflicts on this link, so that one
    /// `scale` can fix pixel format and colour range together rather than
    /// stacking two nodes — plan 16 §1.7's coalescing rule, expressed by
    /// passing the whole set rather than one property at a time.
    ///
    /// Returning `None` means "there is no automatic fix for this", which is the
    /// correct answer for a hardware-context mismatch and produces the
    /// [`Conflict`] diagnostic rather than a silent device transfer.
    fn converter(
        &self,
        media: MediaType,
        properties: &[Property],
        upstream: &FormatSet,
        downstream: &FormatSet,
    ) -> Option<ConverterSpec>;
}

/// A factory that never converts. The `-noauto_conversion_filters` behaviour,
/// and the right thing to pass when testing that a graph negotiates cleanly.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoConversion;

impl ConverterFactory for NoConversion {
    fn converter(
        &self,
        _media: MediaType,
        _properties: &[Property],
        _upstream: &FormatSet,
        _downstream: &FormatSet,
    ) -> Option<ConverterSpec> {
        None
    }
}

/// The negotiated format of every link, plus what was inserted to get there.
#[derive(Debug, Clone, Default)]
pub struct Assignment {
    /// One resolved [`FormatSet`] per link, indexed by [`LinkId`].
    pub links: Vec<FormatSet>,
    /// Converters spliced in, in insertion order.
    pub inserted: Vec<Insertion>,
    /// How many repair rounds it took. Zero means the graph negotiated as
    /// written.
    pub rounds: u32,
}

impl Assignment {
    /// The resolved set for one link.
    #[must_use]
    pub fn link(&self, id: LinkId) -> Option<&FormatSet> {
        self.links.get(id.0 as usize)
    }
}

/// A converter that was spliced into the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    /// The link that was split. It now ends at the converter's input.
    pub link: LinkId,
    /// The new node.
    pub node: NodeId,
    /// The new link, from the converter's output to the original consumer.
    pub tail: LinkId,
    /// The name the converter was given, e.g. `auto_scale_0`. The reference
    /// names them the same way and scripts grep for it.
    pub name: String,
    /// What the factory asked for.
    pub filter: &'static str,
    /// The arguments the factory asked for, e.g. `sws_flags=bicubic`.
    ///
    /// Carried here because it used to be **dropped**: `Insertion` recorded the
    /// filter name and not its arguments, so `Graph::configure_converting`
    /// rebuilt the `ConverterSpec` with `args: String::new()` and whatever the
    /// factory had chosen was lost. `vaco-filter-graph` did not notice because
    /// its builder re-fetches the arguments from its own factory in the same
    /// crate — a third-party registry supplying its own builder would silently
    /// have lost them.
    pub args: String,
    /// The properties this converter was inserted to fix.
    pub properties: Vec<Property>,
}

/// Solve the plan, inserting converters where a link has no common format.
///
/// See the module documentation for the model, the termination argument and
/// what happens when no assignment exists.
///
/// # Errors
///
/// * [`Error::InvalidData`] — a node's own tied pads share no value, or the
///   plan is structurally malformed. Both are bugs in the caller, not in the
///   user's graph.
/// * [`Error::Unsupported`] — a link has no common format and no converter is
///   available, a class was left entirely unconstrained, or the round bound was
///   hit. `conflicts` is populated in the first case; call [`Conflict::render`]
///   for the message.
pub fn negotiate(
    plan: &mut NegotiationPlan,
    factory: &dyn ConverterFactory,
    mode: AutoConvert,
    conflicts: &mut Vec<Conflict>,
) -> Result<Assignment> {
    conflicts.clear();
    let bound = plan
        .links
        .len()
        .saturating_mul(3)
        .saturating_add(1)
        .min(u32::MAX as usize) as u32;
    let mut inserted = Vec::new();
    let mut round = 0u32;
    loop {
        let outcome = solve_once(plan)?;
        let found = outcome.conflicts;
        if found.is_empty() {
            return Ok(Assignment {
                links: outcome.link_formats,
                inserted,
                rounds: round,
            });
        }
        if mode == AutoConvert::None {
            conflicts.extend(found.into_iter().map(|mut c| {
                c.auto_convert_disabled = true;
                c
            }));
            return Err(Error::Unsupported(
                "no common format on a filter link and auto-conversion is disabled",
            ));
        }
        if round >= bound {
            return Err(Error::Unsupported(
                "format negotiation did not converge; a converter did not fix what it claimed to",
            ));
        }
        let mut repaired = 0usize;
        // Coalesce: all the properties conflicting on one link get one converter.
        let mut by_link: Vec<(LinkId, Vec<Property>)> = Vec::new();
        for c in &found {
            match by_link.iter_mut().find(|(l, _)| *l == c.link) {
                Some((_, props)) => props.push(c.property),
                None => by_link.push((c.link, vec![c.property])),
            }
        }
        for (link, properties) in by_link {
            let Some(ends) = plan.links.get(link.0 as usize).copied() else {
                continue;
            };
            let (Some(up), Some(down)) = (plan.pad_set(ends.src), plan.pad_set(ends.dst)) else {
                continue;
            };
            let (up, down) = (up.clone(), down.clone());
            let Some(spec) = factory.converter(ends.media, &properties, &up, &down) else {
                continue;
            };
            let name = format!("auto_{}_{}", spec.filter, inserted.len());
            let mut formats = spec.formats;
            formats.label.clone_from(&name);
            let (node, tail) = plan.splice(link, formats)?;
            inserted.push(Insertion {
                link,
                node,
                tail,
                name,
                filter: spec.filter,
                args: spec.args,
                properties,
            });
            repaired += 1;
        }
        if repaired == 0 {
            conflicts.extend(found);
            return Err(Error::Unsupported(
                "no common format on a filter link and no converter is available for it",
            ));
        }
        round = round.saturating_add(1);
    }
}

struct Outcome {
    link_formats: Vec<FormatSet>,
    conflicts: Vec<Conflict>,
}

#[expect(
    clippy::too_many_lines,
    reason = "the six steps of plan 16 §1.6.3 read as one procedure; splitting \
              them would hide the ordering, which is the part that has to be right"
)]
fn solve_once(plan: &NegotiationPlan) -> Result<Outcome> {
    let index = plan.pad_index();
    let mut conflicts: Vec<Conflict> = Vec::new();
    // One class table per property. Only properties that any link actually uses
    // are solved, so a pure-video graph never touches channel layouts.
    let mut resolved: Vec<FormatSet> = vec![FormatSet::default(); plan.links.len()];

    for &property in &Property::ALL {
        let mut uf = UnionFind::new(index.total);
        // Per representative: the folded constraint and its provenance.
        let mut state: Vec<Option<(FormatSet, Provenance)>> = vec![None; index.total];
        // Seed every pad slot with its own declaration.
        for (n, node) in plan.nodes.iter().enumerate() {
            let node_id = NodeId(n as u32);
            for (direction, sets) in [
                (Direction::Input, &node.inputs),
                (Direction::Output, &node.outputs),
            ] {
                for (p, set) in sets.iter().enumerate() {
                    let Some(slot) = index.of(PadRef {
                        node: node_id,
                        direction,
                        pad: p as u32,
                    }) else {
                        continue;
                    };
                    let mut prov = Provenance::default();
                    if !set.get(property).is_unconstrained() {
                        prov.record(node_id);
                    }
                    if let Some(cell) = state.get_mut(slot) {
                        *cell = Some((set.clone(), prov));
                    }
                }
            }
        }

        // ---- node-local ties, in node order then tie order ------------------
        for (n, node) in plan.nodes.iter().enumerate() {
            let node_id = NodeId(n as u32);
            for tie in node.ties.iter().filter(|t| t.property == property) {
                let mut pads = tie.pads.iter();
                let Some(&(d0, p0)) = pads.next() else {
                    continue;
                };
                let Some(mut anchor) = index.of(PadRef {
                    node: node_id,
                    direction: d0,
                    pad: p0,
                }) else {
                    continue;
                };
                for &(d, p) in pads {
                    let Some(slot) = index.of(PadRef {
                        node: node_id,
                        direction: d,
                        pad: p,
                    }) else {
                        continue;
                    };
                    match join(&mut uf, &mut state, anchor, slot, property) {
                        Ok(Some(rep)) => anchor = rep,
                        Ok(None) => {}
                        Err(()) => {
                            return Err(Error::InvalidData(
                                "a filter tied two of its own pads that accept no common format",
                            ));
                        }
                    }
                }
            }
        }

        // ---- links, in LinkId order ----------------------------------------
        // The merge that empties a class *is* the conflict. Ordering the merges
        // is what makes which link gets named deterministic (D6).
        for (i, ends) in plan.links.iter().enumerate() {
            if !Property::for_media(ends.media).contains(&property) {
                continue;
            }
            let (Some(a), Some(b)) = (index.of(ends.src), index.of(ends.dst)) else {
                continue;
            };
            match join(&mut uf, &mut state, a, b, property) {
                Ok(_) => {}
                Err(()) => conflicts.push(build_conflict(
                    plan,
                    &index,
                    &mut uf,
                    &state,
                    LinkId(i as u32),
                    *ends,
                    property,
                )),
            }
        }

        if !conflicts.is_empty() {
            continue;
        }

        // ---- pick, and write the answer onto every link ---------------------
        for (i, ends) in plan.links.iter().enumerate() {
            if !Property::for_media(ends.media).contains(&property) {
                continue;
            }
            let Some(slot) = index.of(ends.src) else {
                continue;
            };
            let rep = uf.find(slot);
            let Some(Some((set, _))) = state.get(rep) else {
                continue;
            };
            let picked = pick(set, property);
            if picked.get(property).is_unconstrained() {
                return Err(Error::Unsupported(
                    "format negotiation left a property unconstrained; nothing in the graph said \
                     what it carries",
                ));
            }
            if let Some(out) = resolved.get_mut(i) {
                install(out, &picked, property);
            }
        }
    }

    Ok(Outcome {
        link_formats: resolved,
        conflicts,
    })
}

/// Merge two pad slots for one property, folding their constraints.
///
/// `Err(())` means the merge emptied the class. The union is *not* applied in
/// that case, so a later repair sees the two sides still separate.
fn join(
    uf: &mut UnionFind,
    state: &mut [Option<(FormatSet, Provenance)>],
    a: usize,
    b: usize,
    property: Property,
) -> std::result::Result<Option<usize>, ()> {
    let (ra, rb) = (uf.find(a), uf.find(b));
    if ra == rb {
        return Ok(None);
    }
    let left = state.get(ra).cloned().flatten();
    let right = state.get(rb).cloned().flatten();
    let (Some((ls, lp)), Some((rs, rp))) = (left, right) else {
        return Ok(None);
    };
    let Some(merged) = merge_property(&ls, &rs, property) else {
        return Err(());
    };
    let Some(rep) = uf.union(a, b) else {
        return Ok(None);
    };
    let mut prov = lp;
    prov.absorb(&rp);
    if let Some(cell) = state.get_mut(rep) {
        *cell = Some((merged, prov));
    }
    Ok(Some(rep))
}

/// Intersect two sets on one property only, leaving the others alone.
fn merge_property(a: &FormatSet, b: &FormatSet, property: Property) -> Option<FormatSet> {
    let mut out = a.clone();
    match property {
        Property::PixelFormat => {
            out.pixel_formats =
                merge(a.pixel_formats.as_ref(), b.pixel_formats.as_ref(), property).ok()?;
        }
        Property::SampleFormat => {
            out.sample_formats = merge(
                a.sample_formats.as_ref(),
                b.sample_formats.as_ref(),
                property,
            )
            .ok()?;
        }
        Property::SampleRate => {
            out.sample_rates =
                merge(a.sample_rates.as_ref(), b.sample_rates.as_ref(), property).ok()?;
        }
        Property::ChannelLayout => {
            out.channel_layouts = merge(
                a.channel_layouts.as_ref(),
                b.channel_layouts.as_ref(),
                property,
            )
            .ok()?;
        }
    }
    Some(out)
}

/// Resolve one property of `set` to a single value, keeping the rest as-is.
///
/// The first candidate wins. Because the fold keeps the *first* pad's ordering
/// and pads are folded in `PadRef` order, that is the upstream-most declared
/// preference — which is why a source that lists its native format first keeps
/// it, matching the reference's observed behaviour of never converting when the
/// two sides already agree.
fn pick(set: &FormatSet, property: Property) -> FormatSet {
    let mut out = set.clone();
    match property {
        Property::PixelFormat => out.pixel_formats = first_of(set.pixel_formats.as_ref()),
        Property::SampleFormat => out.sample_formats = first_of(set.sample_formats.as_ref()),
        Property::SampleRate => out.sample_rates = first_of(set.sample_rates.as_ref()),
        Property::ChannelLayout => out.channel_layouts = first_of(set.channel_layouts.as_ref()),
    }
    out
}

fn first_of<T: Clone + PartialEq>(c: Option<&Constraint<T>>) -> Option<Constraint<T>> {
    match c {
        None | Some(Constraint::Any) => c.cloned(),
        Some(other) => other.candidates().first().cloned().map(Constraint::Exact),
    }
}

fn install(target: &mut FormatSet, source: &FormatSet, property: Property) {
    match property {
        Property::PixelFormat => target.pixel_formats.clone_from(&source.pixel_formats),
        Property::SampleFormat => target.sample_formats.clone_from(&source.sample_formats),
        Property::SampleRate => target.sample_rates.clone_from(&source.sample_rates),
        Property::ChannelLayout => target.channel_layouts.clone_from(&source.channel_layouts),
    }
}

fn build_conflict(
    plan: &NegotiationPlan,
    index: &PadIndex,
    uf: &mut UnionFind,
    state: &[Option<(FormatSet, Provenance)>],
    link: LinkId,
    ends: LinkEnds,
    property: Property,
) -> Conflict {
    let side = |uf: &mut UnionFind, pad: PadRef| -> ConflictSide {
        let cell = index
            .of(pad)
            .map(|s| uf.find(s))
            .and_then(|r| state.get(r))
            .and_then(Option::as_ref);
        let (accepts, narrowed_by) = cell.map_or_else(
            || (Vec::new(), Vec::new()),
            |(set, prov)| {
                (
                    set.get(property).describe(),
                    prov.narrowed_by
                        .iter()
                        .map(|&n| plan.label(n).to_owned())
                        .collect(),
                )
            },
        );
        ConflictSide {
            pad,
            label: plan.label(pad.node).to_owned(),
            accepts,
            narrowed_by,
        }
    };
    let upstream = side(uf, ends.src);
    let downstream = side(uf, ends.dst);
    Conflict {
        link,
        property,
        media: ends.media,
        upstream,
        downstream,
        auto_convert_disabled: false,
    }
}

#[cfg(test)]
mod tests;
