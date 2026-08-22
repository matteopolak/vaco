//! The one rounding story.
//!
//! Every stage boundary is a time-base change, and the errors compound: a
//! 1/1000 demuxer base into a 1/25 filter base into a 1/90000 muxer base loses
//! a little at each hop, and the loss is systematic if any hop truncates. So
//! there is exactly one function per item kind here, they both use
//! [`Rounding::NearestAwayFromZero`], and nothing in this crate rescales any
//! other way.
//!
//! `NearestAwayFromZero` is `vaco_core::Rounding`'s own default and what
//! `vaco_format_core::interleave::MuxTimestamps`'s M1 step already applies, so
//! choosing it here means the muxer-side chain and the pipeline-side hops agree
//! rather than each having an opinion. Its worst-case error is half a tick per
//! hop and it is unbiased, so errors across hops cancel rather than accumulate —
//! which is the property that matters over a two-hour file.

use vaco_core::{Rounding, TimeBase, Timestamp};
use vaco_frame::Frame;
use vaco_packet::Packet;

use crate::wire::Payload;

/// The rounding every rescale in this crate uses.
pub const ROUNDING: Rounding = Rounding::NearestAwayFromZero;

/// Move a frame's timing from `from` into `to`, in place.
///
/// `Frame::time_base` is the frame's own record of what its `pts` is counted
/// in, so it moves with the timestamp. A decoder that leaves it undefined is
/// taken at the stream's word: the frame is assumed to be in `from`.
pub fn rescale_frame(frame: &mut Frame, from: TimeBase, to: TimeBase) {
    if from == to || !from.is_defined() || !to.is_defined() {
        frame.time_base = to;
        return;
    }
    frame.pts = frame.pts.rescale(from, to, ROUNDING);
    frame.time_base = to;
}

/// Move a packet's timing from `from` into `to`, in place.
pub fn rescale_packet(packet: &mut Packet, from: TimeBase, to: TimeBase) {
    if from == to || !from.is_defined() || !to.is_defined() {
        return;
    }
    packet.rescale_ts(from, to, ROUNDING);
}

/// Move whatever it is from `from` into `to`.
pub fn rescale(item: &mut Payload, from: TimeBase, to: TimeBase) {
    match item {
        Payload::Packet(p) => rescale_packet(p, from, to),
        Payload::Frame(f) => rescale_frame(f, from, to),
    }
}

/// Move a bare timestamp — an end-of-stream marker — from `from` into `to`.
#[must_use]
pub fn rescale_ts(ts: Timestamp, from: TimeBase, to: TimeBase) -> Timestamp {
    if from == to || !from.is_defined() || !to.is_defined() {
        return ts;
    }
    ts.rescale(from, to, ROUNDING)
}
