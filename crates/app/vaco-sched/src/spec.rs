//! Building a pipeline: the part that has to admit `-map`.
//!
//! # The data structure is a graph, not a list of pairs
//!
//! The reference tool routes any input stream to any number of output streams,
//! each with its own filter chain and encoder, and it will happily send one
//! input stream to two outputs — one re-encoded, one copied — in a single
//! invocation. A `Vec<(input, output)>` cannot express that, and a design that
//! starts there does not generalise; it gets rewritten.
//!
//! So the unit here is a **tap**: a handle to a port that produces something.
//! [`PacketTap`] and [`FrameTap`] are `Copy`, and using one twice is fan-out —
//! the port grows a second wire and each item is cloned onto it. (Cloning is
//! cheap: a `Packet`'s and a `Frame`'s buffers are reference-counted, so the
//! clone copies a pointer, not a picture.) One input stream to five outputs is
//! five calls to [`PipelineSpec::map`] with the same tap.
//!
//! The two tap types are distinct so that wiring a frame producer into a muxer
//! is a compile error rather than a runtime one. That is the entire routing
//! validation: everything else the builder accepts is connectable.
//!
//! # Cycles
//!
//! A tap can only name a node that already exists, so a graph built through
//! this API is acyclic **by construction** rather than by a check. That is the
//! reason `[dec:N]` loopback decoders are not expressible here (see the crate
//! docs): admitting them means adding a back-edge constructor and, with it,
//! the cycle detection and slack-edge sizing that plan 14 §7.6 describes.
//!
//! # Worked shape
//!
//! ```text
//!   in0 ─stream 0─┬─▶ map(out0)                          (stream copy)
//!                 └─▶ decode ─▶ filter ─▶ encode ─▶ map(out1)
//!   in1 ─stream 1───▶ decode ─┘  (a second input into the same graph)
//! ```

use vaco_codec_core::{CodecParameters, Decoder, Encoder};
use vaco_core::{Error, Rational, Result, TimeBase};
use vaco_filter_core::{Graph, NodeId};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Demuxer, Muxer};
use vaco_limits::Limits;

use crate::wire::{Capacity, Flow};

/// A handle to a port that produces compressed packets.
///
/// `Copy`: using the same tap twice is how one input stream reaches two
/// outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketTap {
    pub(crate) node: u32,
    pub(crate) port: u16,
}

/// A handle to a port that produces decoded frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameTap {
    pub(crate) node: u32,
    pub(crate) port: u16,
}

/// A handle to an input file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputRef(pub(crate) u32);

/// A handle to an output file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputRef(pub(crate) u32);

/// One binding of a frame producer to a filter graph's buffer source.
///
/// `time_base` is what the caller passed to
/// [`Graph::set_source_format`](vaco_filter_core::Graph::set_source_format) for
/// that source. It is asked for rather than inferred because `vaco-filter-core`
/// has no getter for a source link's format, and guessing it is how frames
/// arrive in the wrong unit — the errors are small, systematic, and invisible
/// until an hour into a file.
#[derive(Debug, Clone, Copy)]
pub struct SourceBind {
    /// Where the frames come from.
    pub tap: FrameTap,
    /// The graph's buffer-source node they go into.
    pub node: NodeId,
    /// The time base that source link was configured with.
    pub time_base: Rational,
}

impl SourceBind {
    /// Bind `tap` to `node`, taking the time base from the producer.
    ///
    /// Correct whenever the caller configured the source link from the same
    /// stream the tap comes from, which is the usual case.
    #[must_use]
    pub const fn new(tap: FrameTap, node: NodeId, time_base: Rational) -> Self {
        Self {
            tap,
            node,
            time_base,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Tap {
    pub node: u32,
    pub port: u16,
}

impl From<PacketTap> for Tap {
    fn from(t: PacketTap) -> Self {
        Self {
            node: t.node,
            port: t.port,
        }
    }
}

impl From<FrameTap> for Tap {
    fn from(t: FrameTap) -> Self {
        Self {
            node: t.node,
            port: t.port,
        }
    }
}

pub(crate) struct NodeSpec {
    pub label: String,
    pub kind: KindSpec,
    /// Per input port: where it comes from, and the time base the consumer
    /// wants its items in.
    pub inputs: Vec<(Tap, TimeBase)>,
    /// Per output port: what it carries and the base it counts in.
    pub outputs: Vec<(Flow, TimeBase)>,
}

pub(crate) enum KindSpec {
    Demux {
        demuxer: Box<dyn Demuxer>,
        /// Per output port, the demuxer stream index it carries.
        stream_of_port: Vec<u32>,
    },
    Decode(Box<dyn Decoder>),
    Encode(Box<dyn Encoder>),
    Filter {
        graph: Box<Graph>,
        sources: Vec<NodeId>,
        sinks: Vec<NodeId>,
    },
    Mux {
        muxer: Box<dyn Muxer>,
        flags: FormatFlags,
        options: Box<FormatOptions>,
        /// Per input port, the muxer stream index it feeds.
        stream_of_port: Vec<u32>,
    },
}

impl std::fmt::Debug for KindSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Demux { .. } => "Demux",
            Self::Decode(_) => "Decode",
            Self::Encode(_) => "Encode",
            Self::Filter { .. } => "Filter",
            Self::Mux { .. } => "Mux",
        })
    }
}

impl std::fmt::Debug for NodeSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeSpec")
            .field("label", &self.label)
            .field("kind", &self.kind)
            .field("inputs", &self.inputs.len())
            .field("outputs", &self.outputs.len())
            .finish()
    }
}

/// Declares a pipeline: what the inputs are, what the outputs are, and which
/// input stream reaches which output stream by which route.
///
/// Nothing here runs. [`PipelineSpec::build`] turns the declaration into a
/// [`Pipeline`](crate::Pipeline), which is the thing a driver steps.
#[derive(Debug)]
pub struct PipelineSpec {
    pub(crate) nodes: Vec<NodeSpec>,
    pub(crate) capacity: Capacity,
    pub(crate) limits: Limits,
    pub(crate) max_input_errors: u32,
}

impl Default for PipelineSpec {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineSpec {
    /// An empty specification with default capacities and permissive limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            capacity: Capacity::DEFAULT,
            limits: Limits::permissive(),
            max_input_errors: 64,
        }
    }

    /// Set the bound applied to every wire.
    ///
    /// This is the user-visible capacity that `-thread_queue_size`,
    /// `-max_muxing_queue_size` and `-filter_buffered_frames` all configure in
    /// the reference tool. They are one knob here because per-edge tuning
    /// without per-edge measurement is guessing, and plan 12 §7.1 already says
    /// the numbers come from a sweep.
    #[must_use]
    pub const fn with_capacity(mut self, capacity: Capacity) -> Self {
        self.capacity = capacity;
        self
    }

    /// Set the memory budget every queued packet and frame is charged to.
    ///
    /// The per-wire [`Capacity`] is a soft admission limit; this is the hard
    /// ceiling underneath it. Exceeding it is
    /// [`Error::LimitExceeded`](vaco_core::Error::LimitExceeded), never a stall.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Recoverable demuxer errors tolerated per input before the input is
    /// declared unusable. The simplest form of `-max_error_rate`.
    #[must_use]
    pub const fn with_max_input_errors(mut self, n: u32) -> Self {
        self.max_input_errors = n;
        self
    }

    fn push(&mut self, node: NodeSpec) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(node);
        id
    }

    fn out_of(&self, tap: Tap) -> Result<(Flow, TimeBase)> {
        self.nodes
            .get(tap.node as usize)
            .and_then(|n| n.outputs.get(tap.port as usize))
            .copied()
            .ok_or(Error::InvalidData("a tap names a port that does not exist"))
    }

    /// Add an input file. Every stream the demuxer reports becomes a port.
    ///
    /// Ports for streams nothing maps are created but never wired, and their
    /// packets are dropped inside the demuxer node — which is what makes `-map`
    /// a selection rather than a filter applied later.
    pub fn add_input(&mut self, demuxer: Box<dyn Demuxer>) -> InputRef {
        let outputs: Vec<(Flow, TimeBase)> = demuxer
            .streams()
            .iter()
            .map(|s| (Flow::Packets, s.time_base))
            .collect();
        let stream_of_port: Vec<u32> = demuxer.streams().iter().map(|s| s.index).collect();
        let label = format!("input {}", self.nodes.len());
        let id = self.push(NodeSpec {
            label,
            kind: KindSpec::Demux {
                demuxer,
                stream_of_port,
            },
            inputs: Vec::new(),
            outputs,
        });
        InputRef(id)
    }

    /// The tap for one of an input's streams, addressed by the demuxer's own
    /// stream index — the number `-map 0:3` names.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the input has no such stream.
    pub fn input_stream(&self, input: InputRef, stream_index: u32) -> Result<PacketTap> {
        let node = self
            .nodes
            .get(input.0 as usize)
            .ok_or(Error::InvalidData("no such input"))?;
        let KindSpec::Demux { stream_of_port, .. } = &node.kind else {
            return Err(Error::InvalidData("that handle is not an input"));
        };
        let port = stream_of_port
            .iter()
            .position(|s| *s == stream_index)
            .ok_or(Error::InvalidData("the input has no such stream"))?;
        Ok(PacketTap {
            node: input.0,
            port: port as u16,
        })
    }

    /// How many streams an input has.
    #[must_use]
    pub fn input_stream_count(&self, input: InputRef) -> usize {
        self.nodes
            .get(input.0 as usize)
            .map_or(0, |n| n.outputs.len())
    }

    /// Add an output file, taking the container's flags from the muxer itself.
    ///
    /// This used to pass `FormatFlags::empty()`, which reads as "nothing
    /// special" and is in fact the *strictest* container: `requires_strict_dts`
    /// is `!TS_NONSTRICT`, so an empty set silently opted every caller into
    /// strictly increasing DTS. Asking the muxer is both correct and impossible
    /// to get wrong by omission.
    pub fn add_output(&mut self, muxer: Box<dyn Muxer>) -> OutputRef {
        let flags = muxer.flags();
        self.add_output_with(muxer, flags, FormatOptions::default())
    }

    /// Add an output file, declaring the container's flags and options.
    ///
    /// `flags` are the muxer's own — they decide whether `avoid_negative_ts`
    /// resolves to shifting and whether DTS must strictly increase — and they
    /// come from the container's `MuxerDesc`, not from the caller's preference.
    pub fn add_output_with(
        &mut self,
        muxer: Box<dyn Muxer>,
        flags: FormatFlags,
        options: FormatOptions,
    ) -> OutputRef {
        let label = format!("output {}", self.nodes.len());
        let id = self.push(NodeSpec {
            label,
            kind: KindSpec::Mux {
                muxer,
                flags,
                options: Box::new(options),
                stream_of_port: Vec::new(),
            },
            inputs: Vec::new(),
            outputs: Vec::new(),
        });
        OutputRef(id)
    }

    /// Attach a decoder to a packet producer. Frames come out in the producer's
    /// time base.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the tap does not exist.
    pub fn add_decoder(&mut self, from: PacketTap, decoder: Box<dyn Decoder>) -> Result<FrameTap> {
        let (_, tb) = self.out_of(from.into())?;
        let id = self.push(NodeSpec {
            label: format!("decode {}:{}", from.node, from.port),
            kind: KindSpec::Decode(decoder),
            inputs: vec![(from.into(), tb)],
            outputs: vec![(Flow::Frames, tb)],
        });
        Ok(FrameTap { node: id, port: 0 })
    }

    /// Attach an encoder to a frame producer.
    ///
    /// `time_base` is the encoder's own: frames are rescaled into it on the way
    /// in and its packets come out counted in it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the tap does not exist.
    pub fn add_encoder(
        &mut self,
        from: FrameTap,
        encoder: Box<dyn Encoder>,
        time_base: Rational,
    ) -> Result<PacketTap> {
        self.out_of(from.into())?;
        let id = self.push(NodeSpec {
            label: format!("encode {}:{}", from.node, from.port),
            kind: KindSpec::Encode(encoder),
            inputs: vec![(from.into(), time_base)],
            outputs: vec![(Flow::Packets, time_base)],
        });
        Ok(PacketTap { node: id, port: 0 })
    }

    /// Attach a configured filter graph.
    ///
    /// `graph` must already be built and [`configure`]d: this crate schedules
    /// filter graphs, it does not negotiate them. `sinks` names the graph's
    /// buffer sinks in the order the returned taps should come out in.
    ///
    /// [`configure`]: vaco_filter_core::Graph::configure
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if a bound tap does not exist, or if a named sink
    /// is not a buffer sink of `graph`.
    pub fn add_filter(
        &mut self,
        graph: Graph,
        sources: &[SourceBind],
        sinks: &[NodeId],
    ) -> Result<Vec<FrameTap>> {
        let mut inputs = Vec::new();
        for bind in sources {
            self.out_of(bind.tap.into())?;
            inputs.push((Tap::from(bind.tap), bind.time_base));
        }
        let mut outputs = Vec::new();
        for sink in sinks {
            let tb = crate::node::link_time_base(graph.sink_format(*sink)?);
            outputs.push((Flow::Frames, tb));
        }
        let id = self.push(NodeSpec {
            label: format!("filter {}", self.nodes.len()),
            kind: KindSpec::Filter {
                graph: Box::new(graph),
                sources: sources.iter().map(|b| b.node).collect(),
                sinks: sinks.to_vec(),
            },
            inputs,
            outputs,
        });
        Ok((0..sinks.len())
            .map(|port| FrameTap {
                node: id,
                port: port as u16,
            })
            .collect())
    }

    /// Route a packet producer into an output file. **This is `-map`.**
    ///
    /// Declares the stream on the muxer immediately — containers need every
    /// stream before the header is written — and returns the index the muxer
    /// assigned it. Calling this twice with the same tap and two different
    /// outputs is a fan-out; calling it with a demuxer's tap and no decoder in
    /// between is a stream copy.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the tap or the output does not exist, or if the
    /// tap does not carry packets. [`Error::Unsupported`] from the muxer when
    /// the container cannot carry the codec.
    pub fn map(&mut self, from: PacketTap, to: OutputRef, params: &CodecParameters) -> Result<u32> {
        let (flow, from_tb) = self.out_of(from.into())?;
        if flow != Flow::Packets {
            return Err(Error::InvalidData("only packets can be muxed"));
        }
        let node = self
            .nodes
            .get_mut(to.0 as usize)
            .ok_or(Error::InvalidData("no such output"))?;
        let KindSpec::Mux {
            muxer,
            stream_of_port,
            ..
        } = &mut node.kind
        else {
            return Err(Error::InvalidData("that handle is not an output"));
        };
        let index = muxer.add_stream(params)?;
        stream_of_port.push(index);
        node.inputs.push((from.into(), from_tb));
        Ok(index)
    }

    /// The time base a muxer chose for one of its streams, or
    /// [`TIME_BASE_Q`] when it has no opinion.
    pub(crate) fn muxer_time_base(muxer: &dyn Muxer, index: u32) -> TimeBase {
        muxer.stream_time_base(index).unwrap_or(TIME_BASE_Q)
    }
}
