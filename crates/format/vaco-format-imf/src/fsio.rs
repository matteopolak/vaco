//! `std::fs::File` as a [`vaco_io::RawSource`], so a resolved track file can
//! be handed to [`vaco_demux_mxf::MxfDemuxer::open`] without buffering the
//! whole essence into memory the way `vaco-demux-image2::fsutil::read_file`
//! does for a single frame.
//!
//! A clip-wrapped (OP-Atom) essence file can be an entire programme's worth
//! of video — nothing like image2's one-frame-per-file case — so this
//! module wraps the real file handle and lets [`vaco_io::PeekSource`] supply
//! [`vaco_io::MediaSource`] on top of it, seeking on demand exactly the way
//! a genuine `file://` protocol source would. It is not that protocol crate
//! (`vaco-format-imf` cannot depend on one — see this crate's top-level
//! docs and `package.rs`'s own W2/W3 account for why a local-only demuxer
//! is allowed to reach `std::fs` directly), only the minimum needed to
//! avoid the memory blow-up a whole-file read would cause here.
//!
//! Does not build usefully for `wasm32-unknown-unknown` at runtime, for the
//! same reason `vaco-demux-image2::fsutil` does not: `std::fs` compiles
//! there and every call returns an I/O error, since there is no filesystem
//! behind that target without a host binding this crate does not assume.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use vaco_core::{Error, Result};
use vaco_io::{RawSource, Seekability};

/// A local file, opened for a resolved IMF track file or sibling XML
/// document.
pub struct FileRawSource {
    file: File,
    size: Option<u64>,
}

impl std::fmt::Debug for FileRawSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRawSource")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl FileRawSource {
    /// Open `path`.
    ///
    /// # Errors
    /// Whatever [`std::fs::File::open`] reports.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let file = File::open(path).map_err(Error::Io)?;
        let size = file.metadata().ok().map(|m| m.len());
        Ok(Self { file, size })
    }
}

impl RawSource for FileRawSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            return match self.file.read(buf) {
                Ok(n) => Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(Error::Io(e)),
            };
        }
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        self.file.seek(SeekFrom::Start(pos)).map_err(Error::Io)
    }

    fn size(&self) -> Option<u64> {
        self.size
    }

    fn seekability(&self) -> Seekability {
        Seekability::Cheap
    }
}
