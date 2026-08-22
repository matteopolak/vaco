//! Compressed packets.

use smallvec::SmallVec;
use vaco_core::{Duration, Timestamp};
use vaco_pool::Buffer;

/// One compressed unit as read from a container: usually one frame of video or
/// one block of audio.
#[derive(Debug, Clone)]
pub struct Packet {
    pub data: Buffer,
    /// Logical length; `data` may be longer because of bitstream padding.
    pub len: usize,
    pub stream_index: u32,
    /// Presentation timestamp, in the owning stream's time base.
    pub pts: Timestamp,
    /// Decode timestamp. Differs from `pts` whenever the codec reorders frames.
    pub dts: Timestamp,
    pub duration: Duration,
    /// Byte position in the source, when known. Reported by ffprobe.
    pub pos: Option<u64>,
    pub flags: PacketFlags,
    pub side_data: SmallVec<[PacketSideData; 2]>,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PacketFlags: u8 {
        const KEY     = 1 << 0;
        const CORRUPT = 1 << 1;
        const DISCARD = 1 << 2;
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PacketSideData {
    Palette(Buffer),
    NewExtradata(Buffer),
    DisplayMatrix([i32; 9]),
    SkipSamples { start: u32, end: u32 },
    // ... generated from the side-data table
}
