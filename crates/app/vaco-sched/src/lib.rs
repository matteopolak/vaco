//! The transcode scheduler: demux → decode → filter → encode → mux, as a graph
//! that something drives.
//!
//! # What it is
//!
//! [`PipelineSpec`] declares what runs — inputs, outputs, and which input
//! stream reaches which output stream by which route. [`PipelineSpec::build`]
//! turns that into a [`Pipeline`], which is a **state machine with a step
//! function**. [`Driver`] is a loop around it. Nothing here opens a file,
//! chooses a codec or parses a filtergraph: components arrive already built, so
//! this crate depends on the framework traits and on no concrete component.
//!
//! ```no_run
//! use vaco_sched::{Driver, PipelineSpec};
//! # fn go(
//! #     demuxer: Box<dyn vaco_format_core::Demuxer>,
//! #     muxer: Box<dyn vaco_format_core::Muxer>,
//! # ) -> vaco_core::Result<()> {
//! let mut spec = PipelineSpec::new();
//! let input = spec.add_input(demuxer);
//! let output = spec.add_output(muxer);
//!
//! // `-map 0:0 -c copy`
//! let video = spec.input_stream(input, 0)?;
//! let params = /* the input stream's parameters */
//! # vaco_codec_core::CodecParameters::new(vaco_core::MediaType::Video);
//! spec.map(video, output, &params)?;
//!
//! let mut pipeline = spec.build()?;
//! let finish = Driver::with_threads(4).run(&mut pipeline)?;
//! # let _ = finish; Ok(()) }
//! ```
//!
//! That source compiles and runs on `wasm32-unknown-unknown`, where
//! `Driver::threads()` reports `1`. See [`driver`] for why.
//!
//! # Backpressure, in one paragraph
//!
//! Every edge is a [`Wire`](wire::Wire) bounded by a [`Capacity`] — items *and*
//! bytes, whichever binds first — and every queued packet and frame is charged
//! to a `vaco_limits::Budget` underneath that. A node whose output has no room
//! is not *runnable*; it does not block, because there is no blocking primitive
//! in this crate. Combined with picking the most downstream runnable node
//! first, a pipeline that reads faster than it writes simply stops reading.
//!
//! # End of stream, in one paragraph
//!
//! A node that has seen end of stream on its inputs sends `None` into its
//! component, drains until the component reports `Eof`, emits everything it got,
//! and only then closes its outputs. A decoder's reorder delay, a filter's
//! window and an encoder's lookahead all come out of that drain. Closing early
//! loses exactly the tail of every output, silently — the classic bug in this
//! shape of code, and the one this crate's `flush` tests are aimed at.
//!
//! # What it does not do yet
//!
//! Named, because a deferred feature that is written down is a decision and one
//! that is not is a surprise. Each says whether the API accommodates it later.
//!
//! | Deferred | Accommodates later? |
//! |---|---|
//! | `-re` (read at native rate) | **Yes.** A rate gate is a predicate on the demuxer node's readiness; `vaco-time`'s `Instant` is already the clock door. No API change. |
//! | `-shortest`, `-shortest_buf_duration` | **Yes.** [`Pipeline::stop_reading`] is the mechanism; what is missing is the policy that decides *when*, plus the pre-mux sync queue that holds frames past a candidate end time. Additive. |
//! | `-fs` (output size limit) | **Yes.** A check in the muxer node after each write, then `stop_reading`. Additive. |
//! | `-t`, `-ss`, `-frames` | **Yes.** Same shape as `-fs`: a per-node predicate plus `stop_reading`. |
//! | `-fps_mode` / `-vsync` (frame duplication and dropping) | **Yes**, but it belongs in a filter, not here — it is a frame-rate conversion, and `vaco-filter-core` already schedules those. |
//! | `-copyts` and start-time offsets | **Yes.** `vaco_format_core::interleave::MuxTimestamps` already implements the offset chain; the pipeline passes options through to it. |
//! | Mid-stream parameter change (`-reinit_filter`, `-drop_changed`) | **Partly.** A `Params` variant on [`wire::Payload`] and a reconfigure path on the filter node. The wire and node contracts hold; [`wire::Payload`] gains a variant, which is a breaking change to a public enum. |
//! | Loopback decoders `[dec:N]` | **No.** The builder is acyclic *by construction* — a tap can only name an existing node — so a back-edge needs a new constructor, and with it plan 14 §7.6's cycle detection and slack-edge sizing. That is a real design addition, not a fill-in. |
//! | Per-edge queue capacities (`-thread_queue_size` vs `-max_muxing_queue_size`) | **Yes.** [`Capacity`] is already per wire; only the builder API says "one for all". |
//! | Thread-count tuning per stage, hardware frame pools | **Yes.** [`Driver`] owns the count and nodes own their components; neither is in the state machine. |
//! | Seek (the pipeline currently runs forward only) | **Yes.** `Wire::reset` and `Graph::flush` exist for it; what is missing is the ordering, which is the same drain-then-reset problem as end of stream. |
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`spec`] | [`PipelineSpec`], the taps, and what `-map` means |
//! | [`pipeline`] | [`Pipeline`], the step function, readiness and the stall diagnosis |
//! | [`driver`] | [`Driver`]: the serial loop and the threaded one |
//! | [`wire`] | bounded queues, [`Capacity`], and the payload |
//! | [`timing`] | the one rounding story |

#![forbid(unsafe_code)]

pub mod driver;
mod node;
pub mod pipeline;
pub mod spec;
pub mod timing;
pub mod wire;

pub use driver::Driver;
pub use pipeline::{Advance, Finish, Pipeline, StallReport, Stats};
pub use spec::{FrameTap, InputRef, OutputRef, PacketTap, PipelineSpec, SourceBind};
pub use wire::{Capacity, Flow, Payload, WireStats};

#[cfg(test)]
mod tests;
