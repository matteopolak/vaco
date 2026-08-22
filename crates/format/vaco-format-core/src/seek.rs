//! Seeking.

use vaco_core::Timestamp;

/// Where to seek to.
#[derive(Debug, Clone, Copy)]
pub enum SeekTarget {
    /// To a timestamp on a specific stream, in that stream's time base.
    Timestamp { stream_index: u32, ts: Timestamp },
    /// To a byte offset. Used for formats with no index, and by `-bytes`.
    Byte(u64),
    /// To a frame number, where the format can count frames.
    Frame { stream_index: u32, frame: u64 },
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct SeekFlags: u8 {
        /// Land at or before the target rather than at or after it.
        const BACKWARD = 1 << 0;
        /// Allow landing on a non-keyframe; the caller will decode and discard.
        const ANY      = 1 << 1;
        /// Target is a byte position even for a timestamp-capable format.
        const BYTE     = 1 << 2;
    }
}
