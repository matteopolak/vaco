//! Frame-shaped pooling: a free list per plane, keyed by geometry.
//!
//! [`BufferPool`] holds one size class. A frame needs several — luma and chroma
//! are different sizes — and the sizes change when the stream's resolution
//! does. `FramePool` is the thin layer that owns one `BufferPool` per plane and
//! throws them all away when the geometry changes, because keeping the old ones
//! is exactly how a pool becomes a leak on a resolution-switching stream.

use std::sync::Arc;

use parking_lot::Mutex;
use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_pixfmt::PixFmt;
use vaco_pool::{ALIGN, BufferPool, PoolConfig, PoolStats};
use vaco_sampfmt::SampleFmt;

use crate::{Frame, FrameData, Plane};

/// The geometry a cached set of plane pools was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Video(PixFmt, u32, u32),
    Audio(SampleFmt, u32, u32),
}

#[derive(Debug, Default)]
struct State {
    key: Option<Key>,
    pools: SmallVec<[BufferPool; 4]>,
}

/// A pool of frame-shaped buffer sets.
///
/// Cloning shares the pool. It is `Send + Sync`, so one pool serves a whole
/// decode stage.
#[derive(Debug, Clone)]
pub struct FramePool {
    state: Arc<Mutex<State>>,
    config: PoolConfig,
}

impl Default for FramePool {
    fn default() -> Self {
        Self::new(PoolConfig::default())
    }
}

impl FramePool {
    /// A pool with the given per-plane caps.
    #[must_use]
    pub fn new(config: PoolConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            config,
        }
    }

    /// Take a video frame, reusing recycled plane buffers when the geometry
    /// matches the last request.
    ///
    /// Plane contents are **unspecified** — a recycled plane holds the previous
    /// frame's pixels. A decoder overwrites every byte it cares about, and
    /// paying for a 3 MB memset per frame to avoid that is exactly the cost this
    /// crate exists to remove.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a hardware pixel format, and
    /// [`Error::LimitExceeded`] when a plane pool is at its cap.
    pub fn acquire_video(&self, format: PixFmt, width: u32, height: u32) -> Result<Frame> {
        if format.is_hw() {
            return Err(Error::Unsupported(
                "cannot allocate a hardware pixel format",
            ));
        }
        let layout = format.plane_layout(width, height, ALIGN)?;
        let key = Key::Video(format, width, height);
        let pools = self.pools_for(&key, layout.planes, |i| {
            layout.sizes.get(i).copied().unwrap_or(0)
        });

        let mut planes: SmallVec<[Plane; 4]> = SmallVec::new();
        for (i, pool) in pools.iter().enumerate() {
            planes.push(Plane {
                data: pool.get()?,
                stride: layout.strides.get(i).copied().unwrap_or(0),
            });
        }
        Ok(Frame::from_data(FrameData::Video {
            format,
            width,
            height,
            planes,
        }))
    }

    /// Take an audio frame.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] when a plane pool is at its cap, or the size
    /// computation overflows.
    pub fn acquire_audio(
        &self,
        format: SampleFmt,
        layout: ChannelLayout,
        samples: u32,
        sample_rate: u32,
    ) -> Result<Frame> {
        let channels = layout.channels.max(1) as usize;
        let (count, per_plane) = if format.is_planar() {
            (channels, 1usize)
        } else {
            (1usize, channels)
        };
        let bytes = (samples as usize)
            .checked_mul(format.bytes_per_sample())
            .and_then(|b| b.checked_mul(per_plane))
            .ok_or(Error::LimitExceeded {
                limit: "audio_frame_bytes",
                requested: u64::MAX,
                cap: usize::MAX as u64,
            })?;

        let key = Key::Audio(format, layout.channels, samples);
        let pools = self.pools_for(&key, count, |_| bytes);

        let mut planes: SmallVec<[Plane; 8]> = SmallVec::new();
        for pool in &pools {
            planes.push(Plane {
                data: pool.get()?,
                stride: bytes,
            });
        }
        Ok(Frame::from_data(FrameData::Audio {
            format,
            sample_rate,
            samples,
            layout,
            planes,
        }))
    }

    /// The plane pools for `key`, rebuilding them if the geometry changed.
    fn pools_for<F: Fn(usize) -> usize>(
        &self,
        key: &Key,
        count: usize,
        size_of: F,
    ) -> SmallVec<[BufferPool; 4]> {
        let mut st = self.state.lock();
        if st.key.as_ref() != Some(key) || st.pools.len() != count {
            st.pools = (0..count)
                .map(|i| BufferPool::with_config(size_of(i), self.config))
                .collect();
            st.key = Some(key.clone());
        }
        st.pools.clone()
    }

    /// Discard every cached buffer. Call on a geometry change the pool cannot
    /// see coming, such as a codec reinitialising.
    pub fn clear(&self) {
        let mut st = self.state.lock();
        for pool in &st.pools {
            pool.clear();
        }
        st.pools.clear();
        st.key = None;
    }

    /// Counters summed over the current plane pools.
    ///
    /// `allocations` flat while `hits` climbs is what recycling looks like; the
    /// crate's pooling test asserts exactly that.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let st = self.state.lock();
        let mut out = PoolStats::default();
        for pool in &st.pools {
            let s = pool.stats();
            out.allocations = out.allocations.saturating_add(s.allocations);
            out.hits = out.hits.saturating_add(s.hits);
            out.recycled = out.recycled.saturating_add(s.recycled);
            out.live_bytes = out.live_bytes.saturating_add(s.live_bytes);
            out.live_buffers = out.live_buffers.saturating_add(s.live_buffers);
            out.retained_buffers = out.retained_buffers.saturating_add(s.retained_buffers);
        }
        out
    }
}
