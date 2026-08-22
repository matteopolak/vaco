//! Fixture construction and awkward sources, shared by the integration tests.
//!
//! Box writing comes from `vaco_format_isom::build` so that "an MP4 shaped like
//! this" has one definition across that crate's tests, this crate's tests, the
//! benchmarks and the fuzz targets.

#![allow(
    dead_code,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::panic,
    unreachable_pub,
    reason = "test support code"
)]

use vaco_core::Result;
use vaco_format_isom::build::{StblSpec, TrackSpec, bx, trak};
use vaco_io::{MediaSource, Seekability};

/// Byte offset of the `mdat` payload in every fixture below.
///
/// The media data is written **first** so that chunk offsets are constants a
/// test can write down rather than a function of the `moov`'s size.
pub const MDAT_PAYLOAD: u64 = 20 + 8;

/// `ftyp`, then `mdat` holding `media`, then `moov`.
pub fn fixture(
    movie_timescale: u32,
    movie_duration: u32,
    tracks: &[TrackSpec],
    media: &[u8],
) -> Vec<u8> {
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&512u32.to_be_bytes());
    ftyp.extend_from_slice(b"isom");

    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&movie_timescale.to_be_bytes());
    mvhd.extend_from_slice(&movie_duration.to_be_bytes());
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
    mvhd.extend_from_slice(&[0; 10]);
    for v in vaco_format_isom::fixed::IDENTITY_MATRIX {
        mvhd.extend_from_slice(&v.to_be_bytes());
    }
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&2u32.to_be_bytes());

    let mut moov = vaco_format_isom::build::fullbx(b"mvhd", 0, 0, &mvhd);
    for t in tracks {
        moov.extend_from_slice(&trak(t));
    }

    let mut out = bx(b"ftyp", &ftyp);
    assert_eq!(out.len() as u64 + 8, MDAT_PAYLOAD);
    out.extend_from_slice(&bx(b"mdat", media));
    out.extend_from_slice(&bx(b"moov", &moov));
    out
}

/// `ftyp`, then `moov`, then `mdat` — the `-movflags +faststart` layout, and
/// the only progressive one a source that cannot seek can read.
///
/// `mk` is called twice with the `mdat` payload offset, exactly as a real
/// faststart muxer patches `stco` once the `moov` size is known. The first call
/// only sizes the header, so the offsets it returns are ignored.
pub fn fixture_faststart(
    movie_timescale: u32,
    movie_duration: u32,
    media: &[u8],
    mk: impl Fn(u64) -> Vec<TrackSpec>,
) -> Vec<u8> {
    let probe = header(movie_timescale, movie_duration, &mk(0));
    let base = probe.len() as u64 + 8;
    let mut out = header(movie_timescale, movie_duration, &mk(base));
    assert_eq!(
        out.len() as u64 + 8,
        base,
        "moov size changed between passes"
    );
    out.extend_from_slice(&bx(b"mdat", media));
    out
}

fn header(movie_timescale: u32, movie_duration: u32, tracks: &[TrackSpec]) -> Vec<u8> {
    let whole = fixture(movie_timescale, movie_duration, tracks, &[]);
    // `fixture` writes ftyp, an empty `mdat`, then `moov`; reorder to put the
    // `moov` in front and drop the placeholder `mdat`.
    let mut out = whole.get(..20).unwrap_or_default().to_vec();
    out.extend_from_slice(whole.get(28..).unwrap_or_default());
    out
}

/// A `stsd` holding one `avc1` entry, 160×120, with a four-byte `avcC`.
pub fn avc1_stsd() -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]);
    entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    entry.extend_from_slice(&[0; 16]);
    entry.extend_from_slice(&160u16.to_be_bytes());
    entry.extend_from_slice(&120u16.to_be_bytes());
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    entry.extend_from_slice(&0u32.to_be_bytes());
    entry.extend_from_slice(&1u16.to_be_bytes()); // frame count
    let mut name = [0u8; 32];
    name[0] = 4;
    name[1..5].copy_from_slice(b"test");
    entry.extend_from_slice(&name);
    entry.extend_from_slice(&24u16.to_be_bytes());
    entry.extend_from_slice(&0xFFFFu16.to_be_bytes());
    entry.extend_from_slice(&bx(b"avcC", &[1, 0x4d, 0x40, 0x0b]));
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&bx(b"avc1", &entry));
    vaco_format_isom::build::fullbx(b"stsd", 0, 0, &body)
}

/// A track of `n` fixed-size samples, one per chunk, starting at `MDAT_PAYLOAD`.
pub fn simple_track(track_id: u32, n: u32, size: u32, delta: u32) -> TrackSpec {
    TrackSpec {
        track_id,
        track_duration: 0,
        handler: *b"vide",
        timescale: 12_800,
        media_duration: u64::from(n) * u64::from(delta),
        language: 0x55C4,
        elst: Vec::new(),
        stbl: StblSpec {
            stsd: Some(avc1_stsd()),
            stts: vec![(n, delta)],
            stsc: vec![(1, 1, 1)],
            stsz: (0..n).map(|_| size).collect(),
            stco: (0..n)
                .map(|i| u32::try_from(MDAT_PAYLOAD).unwrap_or(0) + i * size)
                .collect(),
            has_stss: false,
            ..StblSpec::default()
        },
    }
}

/// A source that hands out at most `chunk` bytes per read, and can be made
/// unseekable.
///
/// `vaco-parse-aac`'s fuzzer found that a parser fed in small pieces truncated
/// its input; the lesson is that "works on a `Vec`" proves nothing about how a
/// component behaves on a real transport.
#[derive(Debug)]
pub struct ChunkSource {
    data: Vec<u8>,
    pos: usize,
    chunk: usize,
    seekable: bool,
}

impl ChunkSource {
    pub fn new(data: Vec<u8>, chunk: usize, seekable: bool) -> Self {
        Self {
            data,
            pos: 0,
            chunk: chunk.max(1),
            seekable,
        }
    }
}

impl MediaSource for ChunkSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let want = buf.len().min(self.chunk);
        let end = (self.pos + want).min(self.data.len());
        let n = end.saturating_sub(self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..end]);
        self.pos += n;
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        if !self.seekable {
            return Err(vaco_core::Error::NotSeekable);
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
        let end = (self.pos + len).min(self.data.len());
        Ok(&self.data[self.pos..end])
    }
}
