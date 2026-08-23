//! A byte-counting `MediaSink` wrapper, for `hls_flags single_file`.
//!
//! `hls_flags single_file` writes every segment into **one** continuous
//! media file, addressed afterwards by `#EXT-X-BYTERANGE`. That means the
//! nested MPEG-TS/fMP4 muxer is constructed exactly **once** for the whole
//! session rather than per segment, and the segment boundaries are just byte
//! offsets recorded while feeding it — but `vaco_format_core::Muxer` has no
//! `position()` method, and by the time a muxer owns its sink there is no
//! outside way to ask it how many bytes it has written. This wrapper is
//! inserted *before* the sink is handed to the nested muxer so the position
//! stays visible from outside.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vaco_core::Result;
use vaco_io::MediaSink;

/// A shared, cheaply-cloned view of a [`CountingSink`]'s position.
#[derive(Debug, Clone, Default)]
pub struct SharedPosition(Arc<AtomicU64>);

impl SharedPosition {
    /// Bytes written (or sought to) so far.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Wraps `inner`, publishing its position through a [`SharedPosition`] handle
/// obtained before ownership of the sink moves into a muxer.
pub struct CountingSink {
    inner: Box<dyn MediaSink>,
    pos: Arc<AtomicU64>,
}

impl CountingSink {
    /// Wrap `inner`; returns the sink and a handle to read its position back.
    #[must_use]
    pub fn new(inner: Box<dyn MediaSink>) -> (Self, SharedPosition) {
        let pos = Arc::new(AtomicU64::new(inner.position()));
        let handle = SharedPosition(pos.clone());
        (Self { inner, pos }, handle)
    }
}

impl core::fmt::Debug for CountingSink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CountingSink")
            .field("pos", &self.pos.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl MediaSink for CountingSink {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.inner.write(buf)?;
        self.pos.store(self.inner.position(), Ordering::Relaxed);
        Ok(())
    }

    fn seek(&mut self, target: u64) -> Result<u64> {
        let at = self.inner.seek(target)?;
        self.pos.store(at, Ordering::Relaxed);
        Ok(at)
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::SharedDynBuf;

    #[test]
    fn position_is_visible_after_the_sink_is_boxed_away() {
        let inner = SharedDynBuf::new();
        let (counting, handle) = CountingSink::new(Box::new(inner));
        let mut boxed: Box<dyn MediaSink> = Box::new(counting);
        assert_eq!(handle.get(), 0);
        boxed.write(&[1, 2, 3, 4]).unwrap();
        assert_eq!(handle.get(), 4);
        boxed.write(&[5, 6]).unwrap();
        assert_eq!(handle.get(), 6);
    }
}
