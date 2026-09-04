//! The transcode scheduler: demux → decode → filter → encode → mux.
//!
//! [`PipelineSpec`] declares the graph and [`PipelineSpec::build`] turns it into
//! a [`Pipeline`] state machine. [`Driver`] repeatedly steps that machine.
//! Components arrive already built; this crate neither opens files nor chooses
//! codecs or filtergraphs, so it depends only on framework traits.
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
//! This also runs on `wasm32-unknown-unknown`, where [`Driver::threads`] reports
//! one. Each [`wire::Wire`] is bounded by [`Capacity`] in both items and bytes,
//! and queued media is charged to a `vaco_limits::Budget`. When an output is
//! full its producer is not runnable; downstream-first scheduling therefore
//! propagates backpressure without blocking.
//!
//! On end of stream, a node sends `None` into its component and drains until
//! `Eof` before closing its outputs. This preserves decoder reorder frames,
//! filter windows, and encoder lookahead that would otherwise be lost.

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
