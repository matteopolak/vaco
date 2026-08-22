//! Fixture construction: enough box *writing* to build a file to parse.
//!
//! This crate does not mux — `vaco-mux-mp4` will — but a parser cannot be
//! tested against tables nobody can write, and a benchmark of the sample-lookup
//! path needs a table with a controlled shape rather than whatever happens to
//! be in a checked-in file. So the writer here exists to serve the reader, and
//! it is deliberately literal: it emits exactly the bytes you describe,
//! including invalid ones, because half the tests are about invalid ones.
//!
//! It is public so that `vaco-demux-mp4`'s tests, the benchmarks and the fuzz
//! targets can share one definition of "an MP4 shaped like this". Nothing in
//! the parse path calls it.

use crate::boxes::{BoxIter, IsoBox};

/// A box: four-byte size, type, payload.
///
/// Panics if the payload is large enough to overflow a 32-bit size — a fixture
/// builder, not a muxer.
#[must_use]
pub fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len().saturating_add(8)).unwrap_or(u32::MAX);
    let mut out = Vec::new();
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A full box: version and 24-bit flags before the payload.
#[must_use]
pub fn fullbx(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![version];
    let f = flags.to_be_bytes();
    body.extend_from_slice(&f[1..4]);
    body.extend_from_slice(payload);
    bx(kind, &body)
}

/// The first box in `data`, for tests that build one box and parse it back.
///
/// Falls back to an empty `free` box rather than panicking, so a fuzz-derived
/// caller cannot be surprised.
#[must_use]
pub fn first_box(data: &[u8]) -> IsoBox<'_> {
    BoxIter::new(data, 0).flatten().next().unwrap_or(IsoBox {
        header: crate::boxes::BoxHeader {
            kind: crate::fourcc::boxes::FREE,
            size: 8,
            header_len: 8,
            usertype: None,
            to_end: false,
        },
        payload: &[],
        offset: 0,
    })
}

fn table_u32(kind: [u8; 4], version: u8, entries: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        &u32::try_from(entries.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for e in entries {
        body.extend_from_slice(&e.to_be_bytes());
    }
    fullbx(&kind, version, 0, &body)
}

/// A description of one `stbl`, in the terms the tables themselves use.
#[derive(Debug, Clone)]
pub struct StblSpec {
    /// Raw `stsd` payload, written verbatim after the box header.
    pub stsd: Option<Vec<u8>>,
    /// `stts` runs of `(sample_count, sample_delta)`.
    pub stts: Vec<(u32, u32)>,
    /// `ctts` version 0 runs, written unsigned.
    pub ctts_v0: Vec<(u32, i32)>,
    /// `ctts` version 1 runs, written signed.
    pub ctts_v1: Vec<(u32, i32)>,
    /// `ctts` version 0 runs written from raw `u32` values, for the
    /// above-`i32::MAX` case.
    pub ctts_raw_v0: Option<Vec<(u32, u32)>>,
    /// `cslg` as `(shift, least, greatest, start, end)`, written version 1.
    pub cslg: Option<(i64, i64, i64, i64, i64)>,
    /// `stss` entries, one-based as the file stores them.
    pub stss: Vec<u32>,
    /// Whether to emit a `stss` box at all. `false` means "every sample is a
    /// sync sample"; `true` with an empty [`StblSpec::stss`] means "none are".
    pub has_stss: bool,
    /// `stsc` runs of `(first_chunk, samples_per_chunk, description_index)`.
    pub stsc: Vec<(u32, u32, u32)>,
    /// Per-sample sizes for a `sample_size == 0` `stsz`.
    pub stsz: Vec<u32>,
    /// `(sample_size, sample_count)` for a uniform `stsz`.
    pub stsz_uniform: Option<(u32, u32)>,
    /// `(field_size, packed bytes, sample_count)` for a `stz2`.
    pub stz2: Option<(u8, Vec<u8>, u32)>,
    /// 32-bit chunk offsets.
    pub stco: Vec<u32>,
    /// 64-bit chunk offsets; wins over [`StblSpec::stco`].
    pub co64: Option<Vec<u64>>,
    /// `sdtp` per-sample dependency bytes.
    pub sdtp: Option<Vec<u8>>,
}

impl Default for StblSpec {
    fn default() -> Self {
        Self {
            stsd: None,
            stts: Vec::new(),
            ctts_v0: Vec::new(),
            ctts_v1: Vec::new(),
            ctts_raw_v0: None,
            cslg: None,
            stss: Vec::new(),
            // A `stss` is emitted by default because the interesting fixtures
            // have one; `has_stss: false` is how a test asks for its absence.
            has_stss: true,
            stsc: Vec::new(),
            stsz: Vec::new(),
            stsz_uniform: None,
            stz2: None,
            stco: Vec::new(),
            co64: None,
            sdtp: None,
        }
    }
}

/// Serialise a [`StblSpec`] into a `stbl` box.
#[must_use]
pub fn stbl(spec: &StblSpec) -> Vec<u8> {
    let mut body = Vec::new();
    if let Some(sd) = &spec.stsd {
        body.extend_from_slice(&bx(b"stsd", sd));
    }
    {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(spec.stts.len()).unwrap_or(0).to_be_bytes());
        for (c, d) in &spec.stts {
            b.extend_from_slice(&c.to_be_bytes());
            b.extend_from_slice(&d.to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"stts", 0, 0, &b));
    }
    if let Some(raw) = &spec.ctts_raw_v0 {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(raw.len()).unwrap_or(0).to_be_bytes());
        for (c, o) in raw {
            b.extend_from_slice(&c.to_be_bytes());
            b.extend_from_slice(&o.to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"ctts", 0, 0, &b));
    } else if !spec.ctts_v1.is_empty() {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(spec.ctts_v1.len()).unwrap_or(0).to_be_bytes());
        for (c, o) in &spec.ctts_v1 {
            b.extend_from_slice(&c.to_be_bytes());
            b.extend_from_slice(&o.to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"ctts", 1, 0, &b));
    } else if !spec.ctts_v0.is_empty() {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(spec.ctts_v0.len()).unwrap_or(0).to_be_bytes());
        for (c, o) in &spec.ctts_v0 {
            b.extend_from_slice(&c.to_be_bytes());
            b.extend_from_slice(&(*o as u32).to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"ctts", 0, 0, &b));
    }
    if let Some((shift, least, greatest, start, end)) = spec.cslg {
        let mut b = Vec::new();
        for v in [shift, least, greatest, start, end] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"cslg", 1, 0, &b));
    }
    if spec.has_stss {
        body.extend_from_slice(&table_u32(*b"stss", 0, &spec.stss));
    }
    {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(spec.stsc.len()).unwrap_or(0).to_be_bytes());
        for (f, s, d) in &spec.stsc {
            b.extend_from_slice(&f.to_be_bytes());
            b.extend_from_slice(&s.to_be_bytes());
            b.extend_from_slice(&d.to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"stsc", 0, 0, &b));
    }
    if let Some((field_size, data, count)) = &spec.stz2 {
        let mut b = vec![0u8, 0, 0, *field_size];
        b.extend_from_slice(&count.to_be_bytes());
        b.extend_from_slice(data);
        body.extend_from_slice(&fullbx(b"stz2", 0, 0, &b));
    } else if let Some((size, count)) = spec.stsz_uniform {
        let mut b = Vec::new();
        b.extend_from_slice(&size.to_be_bytes());
        b.extend_from_slice(&count.to_be_bytes());
        body.extend_from_slice(&fullbx(b"stsz", 0, 0, &b));
    } else {
        let mut b = vec![0u8, 0, 0, 0];
        b.extend_from_slice(&u32::try_from(spec.stsz.len()).unwrap_or(0).to_be_bytes());
        for s in &spec.stsz {
            b.extend_from_slice(&s.to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"stsz", 0, 0, &b));
    }
    if let Some(wide) = &spec.co64 {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(wide.len()).unwrap_or(0).to_be_bytes());
        for o in wide {
            b.extend_from_slice(&o.to_be_bytes());
        }
        body.extend_from_slice(&fullbx(b"co64", 0, 0, &b));
    } else {
        body.extend_from_slice(&table_u32(*b"stco", 0, &spec.stco));
    }
    if let Some(sd) = &spec.sdtp {
        body.extend_from_slice(&fullbx(b"sdtp", 0, 0, sd));
    }
    bx(b"stbl", &body)
}

/// A description of one track, in the fields the boxes carry.
#[derive(Debug, Clone)]
pub struct TrackSpec {
    /// `tkhd.track_ID`.
    pub track_id: u32,
    /// `tkhd.duration`, in the movie timescale.
    pub track_duration: u64,
    /// `hdlr.handler_type`.
    pub handler: [u8; 4],
    /// `mdhd.timescale`.
    pub timescale: u32,
    /// `mdhd.duration`.
    pub media_duration: u64,
    /// Packed `mdhd` language.
    pub language: u16,
    /// `elst` entries of `(segment_duration, media_time, rate_integer)`.
    pub elst: Vec<(u64, i64, i16)>,
    /// The sample table.
    pub stbl: StblSpec,
}

impl Default for TrackSpec {
    fn default() -> Self {
        Self {
            track_id: 1,
            track_duration: 0,
            handler: *b"vide",
            timescale: 12_800,
            media_duration: 0,
            language: 0x55C4,
            elst: Vec::new(),
            stbl: StblSpec::default(),
        }
    }
}

/// Serialise a [`TrackSpec`] into a `trak` box.
#[must_use]
pub fn trak(spec: &TrackSpec) -> Vec<u8> {
    let mut tkhd = Vec::new();
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // creation
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // modification
    tkhd.extend_from_slice(&spec.track_id.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // reserved
    tkhd.extend_from_slice(&(spec.track_duration as u32).to_be_bytes());
    tkhd.extend_from_slice(&[0; 8]); // reserved
    tkhd.extend_from_slice(&0i16.to_be_bytes()); // layer
    tkhd.extend_from_slice(&0i16.to_be_bytes()); // alternate group
    tkhd.extend_from_slice(&0i16.to_be_bytes()); // volume
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for v in crate::fixed::IDENTITY_MATRIX {
        tkhd.extend_from_slice(&v.to_be_bytes());
    }
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // width 16.16
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // height 16.16

    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&spec.timescale.to_be_bytes());
    mdhd.extend_from_slice(&(spec.media_duration as u32).to_be_bytes());
    mdhd.extend_from_slice(&spec.language.to_be_bytes());
    mdhd.extend_from_slice(&0u16.to_be_bytes());

    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&0u32.to_be_bytes());
    hdlr.extend_from_slice(&spec.handler);
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.extend_from_slice(b"Fixture\0");

    let mut minf = Vec::new();
    minf.extend_from_slice(&fullbx(b"vmhd", 0, 1, &[0; 8]));
    minf.extend_from_slice(&stbl(&spec.stbl));

    let mut mdia = Vec::new();
    mdia.extend_from_slice(&fullbx(b"mdhd", 0, 0, &mdhd));
    mdia.extend_from_slice(&fullbx(b"hdlr", 0, 0, &hdlr));
    mdia.extend_from_slice(&bx(b"minf", &minf));

    let mut out = Vec::new();
    out.extend_from_slice(&fullbx(b"tkhd", 0, 3, &tkhd));
    if !spec.elst.is_empty() {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(spec.elst.len()).unwrap_or(0).to_be_bytes());
        for (dur, media, rate) in &spec.elst {
            b.extend_from_slice(&(*dur as u32).to_be_bytes());
            b.extend_from_slice(&(*media as i32).to_be_bytes());
            b.extend_from_slice(&rate.to_be_bytes());
            b.extend_from_slice(&0u16.to_be_bytes());
        }
        out.extend_from_slice(&bx(b"edts", &fullbx(b"elst", 0, 0, &b)));
    }
    out.extend_from_slice(&bx(b"mdia", &mdia));
    bx(b"trak", &out)
}

/// A whole file: `ftyp`, `moov` with `mvhd` and the given tracks, then `mdat`.
#[must_use]
pub fn file(
    major: &[u8; 4],
    movie_timescale: u32,
    movie_duration: u64,
    tracks: &[TrackSpec],
) -> Vec<u8> {
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(major);
    ftyp.extend_from_slice(&512u32.to_be_bytes());
    ftyp.extend_from_slice(major);

    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&movie_timescale.to_be_bytes());
    mvhd.extend_from_slice(&(movie_duration as u32).to_be_bytes());
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume
    mvhd.extend_from_slice(&[0; 10]);
    for v in crate::fixed::IDENTITY_MATRIX {
        mvhd.extend_from_slice(&v.to_be_bytes());
    }
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&2u32.to_be_bytes()); // next track id

    let mut moov = fullbx(b"mvhd", 0, 0, &mvhd);
    for t in tracks {
        moov.extend_from_slice(&trak(t));
    }

    let mut out = bx(b"ftyp", &ftyp);
    out.extend_from_slice(&bx(b"moov", &moov));
    out.extend_from_slice(&bx(b"mdat", &[0; 16]));
    out
}
