//! The state machine: `step` advances it by one unit of work, and that is the
//! only way it ever advances.
//!
//! # Why a step function
//!
//! D18 requires the workspace to build for `wasm32-unknown-unknown`, which has
//! no threads, and requires parallelism to be optional **at the API level**
//! rather than `#[cfg]`-ed at each call site. A step function satisfies that
//! without a single conditional in the caller: the pipeline is a plain state
//! machine, [`Pipeline::step`] advances it, and a driver is whatever calls
//! `step` — a `while` loop on one thread, or [`Driver`](crate::Driver) handing
//! several units of work to several threads between one plan and one commit.
//! Both drivers run the *same* state machine, so a bug found under one is
//! reproducible under the other and the wasm build is not a second code path
//! that nobody exercises.
//!
//! It also removes a whole class of failure. There is no blocking primitive in
//! this crate: no channel, no mutex, no condvar, no park, no sleep. A full
//! queue does not block its producer, it makes it *unrunnable*, and the
//! scheduler picks someone else. A pipeline that cannot block cannot deadlock;
//! the failure it can have instead is making no progress, which
//! [`Pipeline::step`] reports as [`Finish::Stalled`] with the reason for every
//! node, and which `vaco_limits::ProgressGuard` catches as an error rather than
//! a hang.
//!
//! # Priority is the memory policy
//!
//! Nodes are picked most-downstream-first: muxer, then encoder, then filter,
//! then decoder, then demuxer. A demuxer therefore reads only when nothing
//! downstream can make progress, so the pipeline behaves as a pull system and
//! queues stay shallow *in addition to* being hard-bounded. Ties go to the node
//! that ran least recently, which keeps a multi-output pipeline fair and makes
//! the choice a total order — never a `HashMap` iteration or a thread schedule.

use vaco_codec_core::{CancelToken, Stage};
use vaco_core::{Error, Result, TimeBase};
use vaco_format_core::interleave::{InterleaveQueue, MuxTimestamps};
use vaco_limits::{Budget, ProgressGuard};

use crate::node::{
    CodecWork, DecoderSide, DemuxWork, Done, EncoderSide, FilterWork, Job, MuxWork, PortIn, Ports,
    Work,
};
use crate::spec::{KindSpec, PipelineSpec};
use crate::timing;
use crate::wire::{Capacity, Flow, Wire, WireStats};

/// What one call to [`Pipeline::step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// A node ran. Call `step` again.
    Stepped,
    /// Nothing was runnable. Ask [`Pipeline::classify`] why.
    Idle,
}

/// Why the pipeline stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// Every source read to end of stream, every codec drained, every trailer
    /// written. The normal finish, and the only one that leaves valid outputs.
    Complete,
    /// [`Pipeline::cancel`] was called, or a node failed. Outputs are
    /// incomplete: no trailer was written.
    Cancelled,
    /// Nothing is runnable and nothing is finished. A bug in a component or in
    /// this crate, reported with a per-node diagnosis rather than hung on.
    Stalled(Vec<StallReport>),
}

/// One node that could not make progress, and what it was waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallReport {
    /// The node's index in the pipeline.
    pub node: usize,
    /// Its label, e.g. `"decode 0:1"`.
    pub label: String,
    /// A one-line reason.
    pub reason: &'static str,
    /// Depth of each input wire.
    pub input_depth: Vec<usize>,
    /// Whether each output port had room.
    pub output_room: Vec<bool>,
}

/// Pipeline-wide counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Calls to [`Pipeline::step`] that ran something.
    pub steps: u64,
    /// Items that entered a wire.
    pub pushed: u64,
    /// Items that left one.
    pub popped: u64,
    /// Times a producer was held back by a full wire. The
    /// `send_blocked` signal of plan 12 §7.1's diagnosis table.
    pub stalls: u64,
    /// Peak bytes queued across every wire at once.
    pub peak_queued_bytes: u64,
    /// Per-wire detail, in wire order.
    pub wires: Vec<WireStats>,
}

#[derive(Debug)]
struct InPort {
    wire: usize,
    /// The base the *consumer* wants items in. Items are rescaled out of the
    /// wire's base into this one exactly once, when they are taken.
    time_base: TimeBase,
    eof_delivered: bool,
}

#[derive(Debug)]
struct NodeMeta {
    label: String,
    priority: u8,
    inputs: Vec<InPort>,
    /// Per output port, every wire fanning out of it.
    outputs: Vec<Vec<usize>>,
    last_run: u64,
}

/// A built pipeline: the state machine a driver steps.
#[derive(Debug)]
pub struct Pipeline {
    nodes: Vec<NodeMeta>,
    /// Parallel to `nodes`. `None` while the node is checked out as a [`Job`].
    work: Vec<Option<Work>>,
    wires: Vec<Wire>,
    budget: Budget,
    cancel: CancelToken,
    stop_reading: bool,
    guard: ProgressGuard,
    steps: u64,
    peak_bytes: u64,
}

impl Pipeline {
    /// The cancellation token every component in this pipeline shares.
    ///
    /// This is `vaco_codec_core::CancelToken` — the one that already exists, so
    /// a decoder's frame tasks and the pipeline stop on the same flag. Cancelling
    /// is an **abort**: no trailer is written and the outputs are not valid
    /// files. For a clean early stop, use [`Pipeline::stop_reading`].
    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Abort. The next [`Pipeline::step`] returns [`Advance::Idle`] and
    /// [`Pipeline::classify`] reports [`Finish::Cancelled`].
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Stop reading input, but finish cleanly.
    ///
    /// Every demuxer closes its output ports at their current position; the
    /// codecs drain their delay, the filters flush, the muxer writes its
    /// trailer, and the result is a valid — shorter — file. This is the shape
    /// `-t`, `-frames`, `-fs` and `-shortest` all need, and the reason
    /// cancellation is two things rather than one.
    pub const fn stop_reading(&mut self) {
        self.stop_reading = true;
    }

    /// Whether a graceful stop has been asked for.
    #[must_use]
    pub const fn is_stopping(&self) -> bool {
        self.stop_reading
    }

    /// Steps taken so far.
    #[must_use]
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// Bytes currently queued across every wire.
    #[must_use]
    pub const fn queued_bytes(&self) -> u64 {
        self.budget.committed()
    }

    /// Counters, gathered on demand.
    #[must_use]
    pub fn stats(&self) -> Stats {
        let wires: Vec<WireStats> = self.wires.iter().map(Wire::stats).collect();
        Stats {
            steps: self.steps,
            pushed: wires.iter().map(|w| w.pushed).sum(),
            popped: wires.iter().map(|w| w.popped).sum(),
            stalls: wires.iter().map(|w| w.stalls).sum(),
            peak_queued_bytes: self.peak_bytes,
            wires,
        }
    }

    /// Whether every node has finished.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.work
            .iter()
            .all(|w| w.as_ref().is_some_and(Work::is_done))
    }

    /// Advance by one unit of work.
    ///
    /// # Errors
    ///
    /// Whatever a component reported, or
    /// [`Error::LimitExceeded`](vaco_core::Error::LimitExceeded) when the
    /// pipeline's memory budget or its no-progress guard runs out. A failure
    /// cancels the pipeline, so a caller that ignores the error still stops.
    pub fn step(&mut self) -> Result<Advance> {
        self.drive(1)
    }

    /// Advance by up to `width` units of work, running them one after another.
    ///
    /// The serial driver's `width` is 1. [`Driver`](crate::Driver) uses the same
    /// entry point with the jobs handed to threads instead; see
    /// [`Pipeline::check_out`].
    ///
    /// # Errors
    ///
    /// As [`Pipeline::step`].
    pub fn drive(&mut self, width: usize) -> Result<Advance> {
        if self.cancel.is_cancelled() {
            return Ok(Advance::Idle);
        }
        let mut progressed = self.begin_step();
        let jobs = self.check_out(width.max(1));
        if jobs.is_empty() && !progressed {
            return Ok(Advance::Idle);
        }
        for job in jobs {
            let done = job.run();
            progressed |= done.progressed;
            self.check_in(done)?;
        }
        self.end_step(progressed)?;
        Ok(Advance::Stepped)
    }

    /// Step until nothing is runnable, then say why it stopped.
    ///
    /// # Errors
    ///
    /// As [`Pipeline::step`].
    pub fn run(&mut self) -> Result<Finish> {
        while self.step()? == Advance::Stepped {}
        Ok(self.classify())
    }

    /// Why the pipeline is not running.
    #[must_use]
    pub fn classify(&self) -> Finish {
        if self.cancel.is_cancelled() {
            return Finish::Cancelled;
        }
        if self.is_complete() {
            return Finish::Complete;
        }
        Finish::Stalled(self.blocked())
    }

    /// A diagnosis for every node that has not finished.
    fn blocked(&self) -> Vec<StallReport> {
        let mut out = Vec::new();
        for (i, meta) in self.nodes.iter().enumerate() {
            let Some(Some(work)) = self.work.get(i) else {
                continue;
            };
            if work.is_done() {
                continue;
            }
            let (inputs, room) = self.port_state(i);
            let reason = if !room.iter().all(|r| *r) {
                "every wire out of some output port is full"
            } else if inputs.iter().any(|p| !p.at_eof) {
                "waiting for input that has not arrived"
            } else {
                "the component reported nothing to do with all inputs at end of stream"
            };
            out.push(StallReport {
                node: i,
                label: meta.label.clone(),
                reason,
                input_depth: meta
                    .inputs
                    .iter()
                    .map(|p| self.wires.get(p.wire).map_or(0, Wire::depth))
                    .collect(),
                output_room: room,
            });
        }
        out
    }

    /// Open a step: apply a graceful stop to every demuxer that has not already
    /// finished. Returns whether that itself was progress.
    pub(crate) fn begin_step(&mut self) -> bool {
        if !self.stop_reading {
            return false;
        }
        let mut any = false;
        for i in 0..self.nodes.len() {
            let mut close = Vec::new();
            {
                let Some(Some(Work::Demux(d))) = self.work.get_mut(i) else {
                    continue;
                };
                if d.finished {
                    continue;
                }
                d.stop(&mut close);
            }
            any = true;
            self.close_ports(i, &close);
        }
        any
    }

    /// Close a step: account for it and check the no-progress guard.
    ///
    /// # Errors
    ///
    /// [`vaco_limits::LimitError::NoProgress`] once the pipeline has taken too
    /// many consecutive steps that changed nothing. A livelock is as bad as a
    /// deadlock and much harder to see, so it is an error rather than a spin.
    pub(crate) fn end_step(&mut self, progressed: bool) -> Result<()> {
        self.steps = self.steps.saturating_add(1);
        self.peak_bytes = self.peak_bytes.max(self.budget.committed());
        self.guard.tick(progressed)?;
        Ok(())
    }

    /// What each of a node's ports looks like right now.
    fn port_state(&self, node: usize) -> (Vec<PortIn>, Vec<bool>) {
        let Some(meta) = self.nodes.get(node) else {
            return (Vec::new(), Vec::new());
        };
        let inputs = meta
            .inputs
            .iter()
            .map(|p| {
                let wire = self.wires.get(p.wire);
                PortIn {
                    has_item: wire.is_some_and(|w| !w.is_empty()),
                    at_eof: wire.is_some_and(Wire::at_eof),
                    eof_delivered: p.eof_delivered,
                }
            })
            .collect();
        let room = meta
            .outputs
            .iter()
            .map(|ids| {
                ids.iter()
                    .all(|w| self.wires.get(*w).is_some_and(Wire::has_room))
            })
            .collect();
        (inputs, room)
    }

    /// Detach up to `max` runnable nodes, with their inputs, as [`Job`]s.
    ///
    /// Two jobs can never conflict: each node is detached at most once, each
    /// wire has exactly one producer and one consumer, and the inputs are
    /// *moved* out rather than borrowed. That is what lets the parallel driver
    /// exist without a lock anywhere in this crate.
    pub(crate) fn check_out(&mut self, max: usize) -> Vec<Job> {
        let stop_reading = self.stop_reading;
        let mut chosen: Vec<(u8, u64, usize)> = Vec::new();
        for i in 0..self.nodes.len() {
            let Some(Some(work)) = self.work.get(i) else {
                continue;
            };
            let (inputs, out_room) = self.port_state(i);
            let ports = Ports {
                inputs: &inputs,
                out_room: &out_room,
                stop_reading,
            };
            if work.ready(ports) {
                let Some(meta) = self.nodes.get(i) else {
                    continue;
                };
                chosen.push((meta.priority, meta.last_run, i));
            } else if !out_room.iter().all(|r| *r) && !work.is_done() {
                // Held back by a full wire: the occupancy signal plan 12 §7.1
                // diagnoses a bottleneck from.
                for (port, ok) in out_room.iter().enumerate() {
                    if *ok {
                        continue;
                    }
                    let ids = self
                        .nodes
                        .get(i)
                        .and_then(|m| m.outputs.get(port))
                        .cloned()
                        .unwrap_or_default();
                    for w in ids {
                        if let Some(wire) = self.wires.get_mut(w) {
                            wire.note_stall();
                        }
                    }
                }
            }
        }
        // Most downstream first, then least recently run, then by index. A
        // total order, so the choice cannot vary between runs.
        chosen.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        chosen.truncate(max);
        // Commit order is node order, not priority order, so the sequence of
        // pushes is the same however many threads ran the jobs.
        chosen.sort_unstable_by_key(|c| c.2);

        let mut jobs = Vec::new();
        for (_, _, i) in chosen {
            if let Some(job) = self.detach(i) {
                jobs.push(job);
            }
        }
        jobs
    }

    fn detach(&mut self, node: usize) -> Option<Job> {
        let Self {
            nodes,
            work: works,
            wires,
            budget,
            ..
        } = self;
        let work = works.get_mut(node)?.take()?;
        let batch = work.batch();
        let meta = nodes.get_mut(node)?;
        let mut inputs = Vec::new();
        let mut ended = Vec::new();
        for (port, slot) in meta.inputs.iter_mut().enumerate() {
            let Some(wire) = wires.get_mut(slot.wire) else {
                continue;
            };
            let from = wire.time_base();
            let to = slot.time_base;
            for _ in 0..batch {
                if !work.accepts_input() {
                    break;
                }
                let Some(mut item) = wire.pop(budget) else {
                    break;
                };
                timing::rescale(&mut item, from, to);
                inputs.push((port, item));
            }
            if wire.at_eof() && !slot.eof_delivered {
                slot.eof_delivered = true;
                ended.push((port, timing::rescale_ts(wire.end_pts(), from, to)));
            }
        }
        let all_ended = !meta.inputs.is_empty() && meta.inputs.iter().all(|p| p.eof_delivered);
        Some(Job {
            node,
            work,
            inputs,
            ended,
            all_ended,
        })
    }

    /// Put a finished job back and apply what it produced.
    ///
    /// # Errors
    ///
    /// The component's own failure, or a budget overrun. Either cancels the
    /// pipeline before returning, so a caller cannot accidentally carry on.
    pub(crate) fn check_in(&mut self, done: Done) -> Result<()> {
        let Done {
            node,
            work,
            out,
            close,
            progressed: _,
            error,
        } = done;
        if let Some(slot) = self.work.get_mut(node) {
            *slot = Some(work);
        }
        let steps = self.steps;
        if let Some(meta) = self.nodes.get_mut(node) {
            meta.last_run = steps.saturating_add(1);
        }
        if let Some(e) = error {
            self.cancel.cancel();
            return Err(e);
        }
        for (port, item) in out {
            self.emit(node, port, item)?;
        }
        self.close_ports(node, &close);
        Ok(())
    }

    /// Push one item onto every wire fanning out of `port`.
    fn emit(&mut self, node: usize, port: usize, item: crate::wire::Payload) -> Result<()> {
        let Self {
            nodes,
            wires,
            budget,
            ..
        } = self;
        let Some(ids) = nodes.get(node).and_then(|m| m.outputs.get(port)) else {
            return Ok(());
        };
        let n = ids.len();
        let mut held = Some(item);
        for (k, w) in ids.iter().enumerate() {
            let payload = if k + 1 == n {
                match held.take() {
                    Some(p) => p,
                    None => break,
                }
            } else {
                match held.as_ref() {
                    // Cheap: a Packet's and a Frame's buffers are
                    // reference-counted, so a fan-out clone copies pointers.
                    Some(p) => p.clone(),
                    None => break,
                }
            };
            if let Some(wire) = wires.get_mut(*w) {
                wire.push(payload, budget)?;
            }
        }
        Ok(())
    }

    fn close_ports(&mut self, node: usize, close: &[(usize, vaco_core::Timestamp)]) {
        let Self { nodes, wires, .. } = self;
        for (port, ts) in close {
            let Some(ids) = nodes.get(node).and_then(|m| m.outputs.get(*port)) else {
                continue;
            };
            for w in ids {
                if let Some(wire) = wires.get_mut(*w) {
                    wire.close(*ts);
                }
            }
        }
    }
}

// ------------------------------------------------------------------- build

impl PipelineSpec {
    /// Turn the declaration into a runnable [`Pipeline`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if a tap names a port that does not exist, or if
    /// a wire would carry the wrong kind of item — both unreachable through the
    /// typed tap API, and checked anyway because the alternative is a panic.
    pub fn build(self) -> Result<Pipeline> {
        let Self {
            nodes: specs,
            capacity,
            limits,
            max_input_errors,
        } = self;

        let mut metas: Vec<NodeMeta> = specs
            .iter()
            .map(|s| NodeMeta {
                label: s.label.clone(),
                priority: match s.kind {
                    KindSpec::Mux { .. } => 4,
                    KindSpec::Encode(_) => 3,
                    KindSpec::Filter { .. } => 2,
                    KindSpec::Decode(_) => 1,
                    KindSpec::Demux { .. } => 0,
                },
                inputs: Vec::new(),
                outputs: vec![Vec::new(); s.outputs.len()],
                last_run: 0,
            })
            .collect();

        let mut wires: Vec<Wire> = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            for (tap, consumer_tb) in &spec.inputs {
                let (flow, producer_tb) = specs
                    .get(tap.node as usize)
                    .and_then(|n| n.outputs.get(tap.port as usize))
                    .copied()
                    .ok_or(Error::InvalidData("a tap names a port that does not exist"))?;
                let id = wires.len();
                wires.push(Wire::new(flow, capacity, producer_tb));
                metas
                    .get_mut(tap.node as usize)
                    .and_then(|m| m.outputs.get_mut(tap.port as usize))
                    .ok_or(Error::InvalidData("a tap names a port that does not exist"))?
                    .push(id);
                metas
                    .get_mut(i)
                    .ok_or(Error::InvalidData("node index out of range"))?
                    .inputs
                    .push(InPort {
                        wire: id,
                        time_base: *consumer_tb,
                        eof_delivered: false,
                    });
            }
        }

        let mut work = Vec::new();
        for (i, spec) in specs.into_iter().enumerate() {
            let outputs = metas.get(i).map(|m| m.outputs.clone()).unwrap_or_default();
            let inputs_tb: Vec<TimeBase> = metas
                .get(i)
                .map(|m| {
                    m.inputs
                        .iter()
                        .map(|p| wires.get(p.wire).map_or(TimeBase::ZERO, Wire::time_base))
                        .collect()
                })
                .unwrap_or_default();
            work.push(Some(build_work(
                spec,
                &outputs,
                &inputs_tb,
                max_input_errors,
            )));
        }

        Ok(Pipeline {
            nodes: metas,
            work,
            wires,
            budget: Budget::new(limits),
            cancel: CancelToken::new(),
            stop_reading: false,
            guard: ProgressGuard::new(),
            steps: 0,
            peak_bytes: 0,
        })
    }
}

fn build_work(
    spec: crate::spec::NodeSpec,
    outputs: &[Vec<usize>],
    input_time_bases: &[TimeBase],
    max_input_errors: u32,
) -> Work {
    match spec.kind {
        KindSpec::Demux {
            demuxer,
            stream_of_port,
        } => {
            let max = stream_of_port.iter().copied().max().unwrap_or(0) as usize;
            let mut port_for_stream = vec![None; max + 1];
            for (port, stream) in stream_of_port.iter().enumerate() {
                // A port nothing maps has no wire; dropping its packets inside
                // the demuxer is what makes `-map` a selection.
                if outputs.get(port).is_some_and(|w| !w.is_empty())
                    && let Some(slot) = port_for_stream.get_mut(*stream as usize)
                {
                    *slot = Some(port);
                }
            }
            Work::Demux(DemuxWork {
                demuxer,
                port_for_stream,
                last_pts: vec![vaco_core::Timestamp::NONE; stream_of_port.len()],
                finished: false,
                max_errors: max_input_errors,
                errors: 0,
            })
        }
        KindSpec::Decode(decoder) => Work::Decode(CodecWork {
            side: DecoderSide(decoder),
            stage: Stage::Feeding,
            last_pts: vaco_core::Timestamp::NONE,
            stashed: None,
            pending_eof: None,
            end_pts: vaco_core::Timestamp::NONE,
        }),
        KindSpec::Encode(encoder) => Work::Encode(CodecWork {
            side: EncoderSide(encoder),
            stage: Stage::Feeding,
            last_pts: vaco_core::Timestamp::NONE,
            stashed: None,
            pending_eof: None,
            end_pts: vaco_core::Timestamp::NONE,
        }),
        KindSpec::Filter {
            graph,
            sources,
            sinks,
        } => {
            let n = sinks.len();
            Work::Filter(Box::new(FilterWork {
                graph: *graph,
                sources,
                sinks,
                sink_closed: vec![false; n],
                last_pts: vec![vaco_core::Timestamp::NONE; n],
                stashed: Vec::new(),
                pending_eof: Vec::new(),
                pending_output: false,
            }))
        }
        KindSpec::Mux {
            muxer,
            flags,
            options,
            stream_of_port,
        } => {
            let count = stream_of_port
                .iter()
                .copied()
                .max()
                .map_or(0, |m| m as usize + 1);
            // A `notimestamps` container stores no timestamps, so
            // `MuxTimestamps::apply` clears them — and the queue has to be
            // told, or it rejects the very packets that function produced.
            let mut queue = InterleaveQueue::new(count, &options);
            if flags.contains(vaco_format_core::flags::FormatFlags::NOTIMESTAMPS) {
                queue = queue.without_timestamps();
            }
            let mut to_time_base = Vec::new();
            for index in &stream_of_port {
                let tb = PipelineSpec::muxer_time_base(muxer.as_ref(), *index);
                queue.set_time_base(*index, tb);
                to_time_base.push(tb);
            }
            Work::Mux(Box::new(MuxWork {
                ts: MuxTimestamps::new(count, flags, &options),
                queue,
                muxer,
                stream_index: stream_of_port,
                from_time_base: input_time_bases.to_vec(),
                to_time_base,
                header_written: false,
                trailer_written: false,
            }))
        }
    }
}

/// Sanity: a wire's flow and a tap's flow are the same thing, and the builder
/// is what guarantees it.
const _: () = {
    assert!(matches!(Flow::Packets, Flow::Packets));
    assert!(Capacity::DEFAULT.max_items > 0);
};
