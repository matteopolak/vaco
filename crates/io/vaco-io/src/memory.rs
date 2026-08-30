//! An in-memory [`MediaSource`].
//!
//! Three jobs: unit tests, fuzz targets (a fuzzer supplies bytes, not files),
//! and the `data:` URI protocol. The forward-only constructor exists because
//! "does this still work on a pipe?" is a question every probe path has to
//! answer, and an in-memory source that *refuses* to seek makes it testable
//! without an OS pipe.

use vaco_core::{Error, Result};

use crate::{MediaSource, Seekability};

/// A slice of bytes presented as a media source.
#[derive(Debug, Clone)]
pub struct MemorySource {
    data: Vec<u8>,
    pos: usize,
    seekability: Seekability,
}

impl MemorySource {
    /// A fully seekable source, the equivalent of a local file.
    #[must_use]
    pub const fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            pos: 0,
            seekability: Seekability::Cheap,
        }
    }

    /// A source that reports [`Seekability::None`] and refuses [`MediaSource::seek`].
    ///
    /// Bytes are still in memory, so this is not a simulation of latency — it is
    /// a simulation of *capability*, which is the thing probe paths get wrong.
    #[must_use]
    pub const fn forward_only(data: Vec<u8>) -> Self {
        Self {
            data,
            pos: 0,
            seekability: Seekability::None,
        }
    }

    /// A seekable source that reports seeking as expensive, so callers exercise
    /// the read-and-discard path.
    #[must_use]
    pub const fn expensive(data: Vec<u8>) -> Self {
        Self {
            data,
            pos: 0,
            seekability: Seekability::Expensive,
        }
    }

    /// The bytes behind the source.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

impl MediaSource for MemorySource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let Some(rest) = self.data.get(self.pos..) else {
            return Ok(0);
        };
        let n = rest.len().min(buf.len());
        let (Some(src), Some(dst)) = (rest.get(..n), buf.get_mut(..n)) else {
            return Ok(0);
        };
        dst.copy_from_slice(src);
        self.pos += n;
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        if self.seekability == Seekability::None {
            return Err(Error::NotSeekable);
        }
        // Seeking past the end is legal and yields EOF on the next read, which
        // is what a file does: `lseek` past EOF succeeds, `position()` reports
        // where it actually landed, and only a subsequent read discovers there
        // is nothing there. Clamping here would make this source disagree with
        // `FileSource` (whose `seek` is a bare `File::seek(SeekFrom::Start)`,
        // never clamped) about where a demuxer ended up after an identical
        // seek — exactly the "passes on a memory source, fails on a real file"
        // split a test double exists to prevent. `read` and `peek` already
        // treat `pos > data.len()` as EOF via `data.get(pos..)`, so no other
        // change is needed here.
        self.pos = usize::try_from(pos).unwrap_or(usize::MAX);
        Ok(self.pos as u64)
    }

    fn position(&self) -> u64 {
        self.pos as u64
    }

    fn size(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn seekability(&self) -> Seekability {
        self.seekability
    }

    fn peek(&mut self, len: usize) -> Result<&[u8]> {
        let Some(rest) = self.data.get(self.pos..) else {
            return Ok(&[]);
        };
        rest.get(..len.min(rest.len())).ok_or(Error::UnexpectedEof)
    }
}
