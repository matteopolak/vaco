//! MP4 demuxing over a source that hands out bytes a few at a time, and that
//! may refuse to seek.
//!
//! `vaco-parse-aac`'s fuzzer found that a parser fed in small chunks silently
//! truncated its input; the lesson generalises, and a demuxer has strictly more
//! ways to get it wrong because it seeks as well as reads. The invariant this
//! target exists for:
//!
//! > **On a seekable source, the chunk size must not change the packets.**
//!
//! It also drives the non-seekable path, where the demuxer must either work
//! (a `-movflags +faststart` layout, or fragments with a buffered `mdat`) or
//! refuse cleanly — never panic, and never read past the end.
//! fuzz-crate: vaco-demux-mp4

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, Result};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, Seekability};

#[derive(Debug, arbitrary::Arbitrary)]
struct Input<'a> {
    /// One less than the chunk size, so zero means "one byte at a time".
    chunk: u16,
    seekable: bool,
    ignore_editlist: bool,
    use_tfdt: bool,
    data: &'a [u8],
}

/// A source that returns at most `chunk` bytes per read.
struct Chunked {
    data: Vec<u8>,
    pos: usize,
    chunk: usize,
    seekable: bool,
}

impl MediaSource for Chunked {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let want = buf.len().min(self.chunk);
        let end = self.pos.saturating_add(want).min(self.data.len());
        let n = end.saturating_sub(self.pos);
        let (Some(dst), Some(src)) = (buf.get_mut(..n), self.data.get(self.pos..end)) else {
            return Ok(0);
        };
        dst.copy_from_slice(src);
        self.pos = end;
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        if !self.seekable {
            return Err(Error::NotSeekable);
        }
        self.pos = usize::try_from(pos)
            .unwrap_or(usize::MAX)
            .min(self.data.len());
        Ok(self.pos as u64)
    }

    fn position(&self) -> u64 {
        self.pos as u64
    }

    fn size(&self) -> Option<u64> {
        self.seekable.then_some(self.data.len() as u64)
    }

    fn seekability(&self) -> Seekability {
        if self.seekable {
            Seekability::Cheap
        } else {
            Seekability::None
        }
    }

    fn peek(&mut self, len: usize) -> Result<&[u8]> {
        let end = self.pos.saturating_add(len).min(self.data.len());
        Ok(self.data.get(self.pos..end).unwrap_or(&[]))
    }
}

type Row = (u32, Option<i64>, Option<i64>, Option<u64>, usize);

fn read_all(data: &[u8], chunk: usize, seekable: bool, opts: Mp4Options) -> Option<Vec<Row>> {
    let src: Box<dyn MediaSource> = Box::new(Chunked {
        data: data.to_vec(),
        pos: 0,
        chunk: chunk.max(1),
        seekable,
    });
    let mut demux = Mp4Demuxer::open(src, &NoParsers, &FormatOptions::default(), opts).ok()?;
    let mut out = Vec::new();
    for _ in 0..2048 {
        match demux.read_packet() {
            Ok(p) => out.push((
                p.stream_index,
                p.pts.ticks(),
                p.dts.ticks(),
                p.pos,
                p.payload().len(),
            )),
            Err(_) => break,
        }
    }
    Some(out)
}

fuzz_target!(|input: Input<'_>| {
    let opts = Mp4Options {
        ignore_editlist: input.ignore_editlist,
        use_tfdt: input.use_tfdt,
        ..Mp4Options::default()
    };
    let chunk = usize::from(input.chunk).saturating_add(1);

    if input.seekable {
        // The invariant: a seekable source's chunk size is invisible.
        let whole = read_all(input.data, usize::MAX, true, opts.clone());
        let fed = read_all(input.data, chunk, true, opts);
        assert_eq!(
            whole.is_some(),
            fed.is_some(),
            "a chunk size of {chunk} changed whether the file opened"
        );
        if let (Some(a), Some(b)) = (whole, fed) {
            assert_eq!(a, b, "a chunk size of {chunk} changed the packets");
        }
    } else {
        // Forward-only: must not panic, and must not invent bytes.
        if let Some(rows) = read_all(input.data, chunk, false, opts) {
            for (_, _, _, pos, len) in rows {
                let end = pos.unwrap_or(0).saturating_add(len as u64);
                assert!(end <= input.data.len() as u64, "packet past the source");
            }
        }
    }
});
