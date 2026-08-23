//! Compressed packets.
//!
//! A [`Packet`] is a refcounted byte payload plus timing, stream index, flags
//! and side data. It shares its storage type — [`Buffer`] — with `vaco-frame`'s
//! planes, so the ownership model is one design rather than two: cloning is a
//! refcount bump, writing copies only when shared, and a pooled payload returns
//! to its pool when its last clone drops.
//!
//! # Padding is not optional here
//!
//! Every constructor in this crate allocates `len + `[`BITSTREAM_PADDING`] bytes
//! and keeps the tail zero, so [`Packet::payload_padded`] is free and every
//! parser in the project gets `vaco-bitstream`'s unchecked-body fast path with
//! no per-call cost (plan 11 F9). That padding is worth a measured 11.7% on the
//! short-buffer workload that header parsing actually is.
//!
//! ```
//! use vaco_limits::{Budget, Limits};
//! use vaco_packet::Packet;
//!
//! let mut budget = Budget::new(Limits::strict());
//! let pkt = Packet::from_slice(&mut budget, &[0x00, 0x00, 0x01, 0x67])?;
//! assert_eq!(pkt.payload(), &[0x00, 0x00, 0x01, 0x67]);
//!
//! let padded = pkt.payload_padded().expect("packets always allocate padded");
//! assert_eq!(padded.logical_len(), 4);
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

mod sidedata;

use smallvec::SmallVec;
use vaco_bitstream::Padded;
use vaco_core::{Duration, Error, Result, Rounding, TimeBase, Timestamp};
use vaco_limits::Budget;
use vaco_pool::{BITSTREAM_PADDING, Buffer, BufferPool};

pub use sidedata::PacketSideDataKind;

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
    /// `start`/`end` are sample counts to drop from the front/back of the
    /// decoded output; `skip_reason`/`discard_reason` are the reason byte
    /// that goes with each trim. The reference's own `ffprobe -show_packets`
    /// prints all four (`skip_samples`, `discard_padding`, `skip_reason`,
    /// `discard_reason`) as one block — an MP4 or Matroska stream with only a
    /// leading `CodecDelay` skip and no reason on record reports `0` for
    /// both reason bytes, which is why every producer in this workspace sets
    /// them to `0` today rather than because the fields do not exist.
    SkipSamples {
        start: u32,
        end: u32,
        skip_reason: u8,
        discard_reason: u8,
    },
    /// The PES packet's `stream_id` byte (ITU-T H.222.0 §2.4.3.7 table 2-22),
    /// e.g. `0xe0` for the first video stream, `0xc0` for the first audio
    /// stream. `ffprobe -show_packets` on an MPEG-TS file prints this as its
    /// own `MPEGTS Stream ID` side-data block, one per packet — measured
    /// against `ffmpeg 8.1`, see `vaco-demux-mpegts`'s docs.
    MpegtsStreamId(u8),
    // ... generated from the side-data table
}

impl Packet {
    /// A packet wrapping `data`, with `len` logical bytes and no metadata.
    ///
    /// The low-level entry point. `len` is clamped to the buffer's length, so a
    /// packet can never claim more payload than it owns — the invariant every
    /// accessor below relies on.
    #[must_use]
    pub fn new(data: Buffer, len: usize) -> Self {
        let len = len.min(data.len());
        Self {
            data,
            len,
            stream_index: 0,
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            duration: Duration::ZERO,
            pos: None,
            flags: PacketFlags::empty(),
            side_data: SmallVec::new(),
        }
    }

    /// An empty packet — a flush marker, or a field to fill in later.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Buffer::empty(), 0)
    }

    /// Allocate a zeroed payload of `len` bytes, plus the bitstream padding.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] if the size overflows or a budget cap is hit.
    pub fn alloc(budget: &mut Budget, len: usize) -> Result<Self> {
        Ok(Self::new(Buffer::alloc_padded(budget, len)?, len))
    }

    /// Copy `data` into a fresh padded payload.
    ///
    /// # Errors
    ///
    /// As [`Packet::alloc`].
    pub fn from_slice(budget: &mut Budget, data: &[u8]) -> Result<Self> {
        Ok(Self::new(
            Buffer::from_slice_padded(budget, data)?,
            data.len(),
        ))
    }

    /// Take a payload from `pool`, which must be a padded size class.
    ///
    /// Build the pool with [`BufferPool::new_padded`] so the padding is part of
    /// the size class and does not fragment the free lists. The payload's
    /// *contents* are whatever the previous user left; only the padding tail is
    /// re-zeroed, which is 64 bytes rather than the whole buffer.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the pool's class is too small to hold `len`
    /// plus the padding, and [`Error::LimitExceeded`] if the pool is at its cap.
    pub fn alloc_pooled(pool: &BufferPool, len: usize) -> Result<Self> {
        let need = len
            .checked_add(BITSTREAM_PADDING)
            .ok_or(Error::InvalidData("packet length overflows"))?;
        if pool.buffer_size() < need {
            return Err(Error::InvalidData(
                "pool size class is too small for this packet",
            ));
        }
        let mut data = pool.get()?;
        // Restore the one region whose contents are load-bearing.
        if let Some(tail) = data.make_mut().get_mut(len..need) {
            tail.fill(0);
        }
        Ok(Self::new(data, len))
    }

    /// Exactly the logical bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        self.data.as_slice().get(..self.len).unwrap_or(&[])
    }

    /// The logical bytes, copying first if the payload is shared.
    ///
    /// Only the payload is exposed, never the padding: a bitstream filter that
    /// rewrites a packet must not be able to dirty the zeros
    /// [`Packet::payload_padded`] promises.
    pub fn payload_mut(&mut self) -> &mut [u8] {
        let len = self.len;
        self.data.make_mut().get_mut(..len).unwrap_or(&mut [])
    }

    /// The padded view (F9).
    ///
    /// `Some` for every packet built by a constructor in this crate: at least
    /// [`BITSTREAM_PADDING`] zero bytes follow the payload, so a bit reader
    /// built from it takes the unchecked body path for the whole buffer.
    /// `None` only if the `data` field was replaced by hand with an unpadded
    /// buffer, in which case the caller falls back to `BitReader::new`.
    #[must_use]
    pub fn payload_padded(&self) -> Option<Padded<'_>> {
        self.data.padded(self.len)
    }

    /// Whether writing the payload would copy.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.data.is_unique()
    }

    /// Pay the copy-on-write cost now rather than inside a loop.
    pub fn make_writable(&mut self) {
        self.data.make_writable();
    }

    /// Shorten the payload, keeping the padding invariant intact.
    ///
    /// Zeroes the bytes between the new end and the old one — which is what
    /// makes the shortened packet still `payload_padded`-able. Copies if shared.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        let old = self.len;
        let end = old.saturating_add(BITSTREAM_PADDING).min(self.data.len());
        if let Some(gap) = self.data.make_mut().get_mut(len..end) {
            gap.fill(0);
        }
        self.len = len;
    }

    /// A new packet carrying `range` of this one's payload, with this packet's
    /// metadata.
    ///
    /// **This copies.** Zero-copy splitting — the MPEG-TS PES, Matroska laced
    /// block and ADTS framing cases — needs a byte offset on `Packet` that the
    /// frozen struct does not have, and would also break the padding invariant
    /// for every sub-packet but the last. See `docs/model/vaco-packet.md`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if `range` is not inside the payload, and
    /// [`Error::LimitExceeded`] if the copy does not fit the budget.
    pub fn sub_packet(&self, budget: &mut Budget, range: std::ops::Range<usize>) -> Result<Self> {
        let src = self
            .payload()
            .get(range)
            .ok_or(Error::InvalidData("sub-packet range outside the payload"))?;
        let mut out = Self::from_slice(budget, src)?;
        out.stream_index = self.stream_index;
        out.pts = self.pts;
        out.dts = self.dts;
        out.duration = self.duration;
        out.pos = self.pos;
        out.flags = self.flags;
        out.side_data.clone_from(&self.side_data);
        Ok(out)
    }

    /// Rescale every timestamp field with one rounding mode.
    ///
    /// `pts` and `dts` must be rescaled together or the stream drifts, which is
    /// why this is one method rather than two call sites. `duration` is not
    /// touched: [`Duration`] is microseconds, not ticks, so it is already
    /// independent of the time base — a deviation from plan 11 §14.1, which
    /// assumed a tick count and a `time_base` field on the packet.
    pub fn rescale_ts(&mut self, from: TimeBase, to: TimeBase, rounding: Rounding) {
        self.pts = self.pts.rescale(from, to, rounding);
        self.dts = self.dts.rescale(from, to, rounding);
    }

    /// Whether this packet is a keyframe.
    #[must_use]
    pub const fn is_key(&self) -> bool {
        self.flags.contains(PacketFlags::KEY)
    }

    /// Whether the payload is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for Packet {
    fn default() -> Self {
        Self::empty()
    }
}
