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
use vaco_format_isom::build::{StblSpec, TrackSpec, bx, fullbx, trak};
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

/// [`avc1_stsd`], with one extra extension box appended after `avcC` — for
/// tests that need a `colr` or similar sibling extension on the same entry.
pub fn avc1_stsd_with_extension(extension: &[u8]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]);
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0; 16]);
    entry.extend_from_slice(&160u16.to_be_bytes());
    entry.extend_from_slice(&120u16.to_be_bytes());
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    entry.extend_from_slice(&0u32.to_be_bytes());
    entry.extend_from_slice(&1u16.to_be_bytes());
    let mut name = [0u8; 32];
    name[0] = 4;
    name[1..5].copy_from_slice(b"test");
    entry.extend_from_slice(&name);
    entry.extend_from_slice(&24u16.to_be_bytes());
    entry.extend_from_slice(&0xFFFFu16.to_be_bytes());
    entry.extend_from_slice(&bx(b"avcC", &[1, 0x4d, 0x40, 0x0b]));
    entry.extend_from_slice(extension);
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&bx(b"avc1", &entry));
    vaco_format_isom::build::fullbx(b"stsd", 0, 0, &body)
}

/// A `stsd` holding one `encv` entry: `avc1` wrapped in `sinf ▸ schm(cenc)` /
/// `sinf ▸ schi ▸ tenc`, byte-for-byte the shape read back from a real
/// `ffmpeg 8.1 -encryption_scheme cenc-aes-ctr` file (see
/// `vaco_format_isom::cenc`'s doc comment for the measurement).
pub fn encv_stsd(kid: [u8; 16]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]); // reserved
    entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    entry.extend_from_slice(&[0; 16]); // pre_defined, reserved, pre_defined[3]
    entry.extend_from_slice(&160u16.to_be_bytes());
    entry.extend_from_slice(&120u16.to_be_bytes());
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    entry.extend_from_slice(&0u32.to_be_bytes());
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0u8; 32]); // compressorname
    entry.extend_from_slice(&24u16.to_be_bytes());
    entry.extend_from_slice(&0xFFFFu16.to_be_bytes());

    let frma = bx(b"frma", b"avc1");
    let mut schm_body = b"cenc".to_vec();
    schm_body.extend_from_slice(&1u32.to_be_bytes());
    let schm = vaco_format_isom::build::fullbx(b"schm", 0, 0, &schm_body);
    let mut tenc_body = vec![0u8, 0]; // reserved, reserved
    tenc_body.push(1); // is_protected
    tenc_body.push(8); // per_sample_iv_size
    tenc_body.extend_from_slice(&kid);
    let tenc = vaco_format_isom::build::fullbx(b"tenc", 0, 0, &tenc_body);
    let schi = bx(b"schi", &tenc);
    let mut sinf_body = frma;
    sinf_body.extend_from_slice(&schm);
    sinf_body.extend_from_slice(&schi);
    let sinf = bx(b"sinf", &sinf_body);

    entry.extend_from_slice(&sinf);

    let mut stsd_body = 1u32.to_be_bytes().to_vec();
    stsd_body.extend_from_slice(&bx(b"encv", &entry));
    // A complete box: `StblSpec::stsd_box` writes it verbatim.
    vaco_format_isom::build::fullbx(b"stsd", 0, 0, &stsd_body)
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
            stsd_box: Some(avc1_stsd()),
            stts: vec![(n, delta)],
            stsc: vec![(1, 1, 1)],
            stsz: (0..n).map(|_| size).collect(),
            stco: (0..n)
                .map(|i| u32::try_from(MDAT_PAYLOAD).unwrap_or(0) + i * size)
                .collect(),
            has_stss: false,
            ..StblSpec::default()
        },
        tref: Vec::new(),
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

// ----------------------------------------------------------- fragmented MP4
//
// `fixture`/`fixture_faststart` build a progressive file: samples live in
// `stbl`, addressed by `stco`. A fragmented file has none of that — its
// sample tables arrive with each `moof`, per ISO/IEC 14496-12 §8.8 — so the
// builders below construct that shape directly, byte for byte, the same way
// `vaco_format_isom::build` does for the progressive tables.

/// A `moov` for a fragmented file: `mvhd`, one empty-`stbl` `trak` per track
/// (a fragmented track's samples live in `moof`, never in `stbl`), and `mvex`
/// with one `trex` per track — whose presence is what
/// `Movie::is_fragmented` tests for.
pub fn frag_moov(movie_timescale: u32, tracks: &[(u32, [u8; 4])]) -> Vec<u8> {
    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&movie_timescale.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes()); // duration: unknown, as `empty_moov` writes it
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
    mvhd.extend_from_slice(&[0; 10]);
    for v in vaco_format_isom::fixed::IDENTITY_MATRIX {
        mvhd.extend_from_slice(&v.to_be_bytes());
    }
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&(u32::try_from(tracks.len()).unwrap_or(0) + 1).to_be_bytes());

    let mut moov = fullbx(b"mvhd", 0, 0, &mvhd);
    for &(track_id, handler) in tracks {
        let spec = TrackSpec {
            track_id,
            track_duration: 0,
            handler,
            timescale: 1_000,
            media_duration: 0,
            language: 0x55C4,
            elst: Vec::new(),
            stbl: StblSpec {
                stsd_box: Some(avc1_stsd()),
                ..StblSpec::default()
            },
            tref: Vec::new(),
        };
        moov.extend_from_slice(&trak(&spec));
    }
    let mut mvex_body = Vec::new();
    for &(track_id, _) in tracks {
        let mut trex = Vec::new();
        trex.extend_from_slice(&track_id.to_be_bytes());
        trex.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
        trex.extend_from_slice(&0u32.to_be_bytes()); // default_sample_duration
        trex.extend_from_slice(&0u32.to_be_bytes()); // default_sample_size
        trex.extend_from_slice(&0u32.to_be_bytes()); // default_sample_flags
        mvex_body.extend_from_slice(&fullbx(b"trex", 0, 0, &trex));
    }
    moov.extend_from_slice(&bx(b"mvex", &mvex_body));
    moov
}

/// One `moof` (one `mfhd`, one `traf` for `track_id`, using
/// `default-base-is-moof`) immediately followed by the `mdat` holding its
/// samples, each `sizes[i]` bytes of filler at a fixed 1000-tick duration.
///
/// `data_offset` is computed and patched after the box is otherwise
/// complete: changing its *value* never changes any box's *size*, so unlike a
/// muxer with more than one thing to place after the header, one pass plus a
/// four-byte patch is enough here — no faststart-style two-pass needed.
pub fn frag_unit(sequence: u32, track_id: u32, tfdt: u64, sizes: &[u32]) -> Vec<u8> {
    let mfhd = fullbx(b"mfhd", 0, 0, &sequence.to_be_bytes());
    let tfhd = fullbx(b"tfhd", 0, 0x02_0000, &track_id.to_be_bytes());
    let tfdt_box = fullbx(b"tfdt", 1, 0, &tfdt.to_be_bytes());

    let tr_flags: u32 = 0x1 | 0x100 | 0x200 | 0x400; // data_offset, duration, size, flags
    let mut trun_body = Vec::new();
    trun_body.extend_from_slice(&u32::try_from(sizes.len()).unwrap_or(0).to_be_bytes());
    let data_offset_at = trun_body.len();
    trun_body.extend_from_slice(&0i32.to_be_bytes()); // patched below
    for (i, &size) in sizes.iter().enumerate() {
        trun_body.extend_from_slice(&1_000u32.to_be_bytes()); // duration
        trun_body.extend_from_slice(&size.to_be_bytes());
        // depends_on=2 (intra) for the first sample, 1 (not intra) after —
        // `SampleFlags::is_sync` reads either the negative non-sync bit or
        // this field, so this alone is enough to mark one sync sample.
        let flags: u32 = if i == 0 { 0x0200_0000 } else { 0x0101_0000 };
        trun_body.extend_from_slice(&flags.to_be_bytes());
    }
    let trun = fullbx(b"trun", 0, tr_flags, &trun_body);

    let mut traf_body = tfhd;
    traf_body.extend_from_slice(&tfdt_box);
    let trun_pos_in_traf_body = traf_body.len();
    traf_body.extend_from_slice(&trun);
    let traf = bx(b"traf", &traf_body);

    let mut moof_body = mfhd;
    let traf_pos_in_moof_body = moof_body.len();
    moof_body.extend_from_slice(&traf);
    let mut moof = bx(b"moof", &moof_body);

    // Absolute position of the `data_offset` placeholder inside `moof`: its
    // own 8-byte box header, the 4-byte fullbox prefix, then wherever each
    // container placed the next one down.
    let data_offset_pos =
        8 + traf_pos_in_moof_body + 8 + trun_pos_in_traf_body + 8 + 4 + data_offset_at;
    let mdat_header_len = 8u64;
    let data_offset = i32::try_from(moof.len() as u64 + mdat_header_len).unwrap_or(i32::MAX);
    moof[data_offset_pos..data_offset_pos + 4].copy_from_slice(&data_offset.to_be_bytes());

    let mut mdat_payload = Vec::new();
    for &size in sizes {
        mdat_payload.extend(std::iter::repeat_n(0xABu8, size as usize));
    }
    moof.extend_from_slice(&bx(b"mdat", &mdat_payload));
    moof
}

/// A `sidx` box (ISO/IEC 14496-12 §8.16.3), version 0 (32-bit times).
pub fn sidx(
    reference_id: u32,
    timescale: u32,
    earliest_pts: u32,
    first_offset: u32,
    refs: &[(bool, u32, u32, bool, u8, u32)],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&reference_id.to_be_bytes());
    body.extend_from_slice(&timescale.to_be_bytes());
    body.extend_from_slice(&earliest_pts.to_be_bytes());
    body.extend_from_slice(&first_offset.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes()); // reserved
    body.extend_from_slice(&u16::try_from(refs.len()).unwrap_or(0).to_be_bytes());
    for &(is_index, size, dur, starts_sap, sap_type, sap_delta) in refs {
        let a = (u32::from(is_index) << 31) | (size & 0x7FFF_FFFF);
        body.extend_from_slice(&a.to_be_bytes());
        body.extend_from_slice(&dur.to_be_bytes());
        let c = (u32::from(starts_sap) << 31)
            | (u32::from(sap_type & 0x7) << 28)
            | (sap_delta & 0x0FFF_FFFF);
        body.extend_from_slice(&c.to_be_bytes());
    }
    fullbx(b"sidx", 0, 0, &body)
}

/// `mfra`: one `tfra` per `(track_id, rows)`, each row
/// `(time, moof_offset, traf_number, trun_number, sample_number)`, followed by
/// the `mfro` trailer that states the whole thing's size — the last sixteen
/// bytes of the file the demuxer's trailer reader looks for.
/// `traf_number`/`trun_number`/`sample_number` are written as one byte each
/// (the packed `length_size` fields all zero, meaning length − 1 == 0).
pub fn mfra(entries: &[(u32, &[(u64, u64, u32, u32, u32)])]) -> Vec<u8> {
    let mut mfra_body = Vec::new();
    for &(track_id, rows) in entries {
        let mut tfra_body = Vec::new();
        tfra_body.extend_from_slice(&track_id.to_be_bytes());
        tfra_body.extend_from_slice(&0u32.to_be_bytes()); // all three length_size fields = 0 (length 1)
        tfra_body.extend_from_slice(&u32::try_from(rows.len()).unwrap_or(0).to_be_bytes());
        for &(time, moof_offset, traf_no, trun_no, sample_no) in rows {
            tfra_body.extend_from_slice(&time.to_be_bytes());
            tfra_body.extend_from_slice(&moof_offset.to_be_bytes());
            tfra_body.push(u8::try_from(traf_no).unwrap_or(u8::MAX));
            tfra_body.push(u8::try_from(trun_no).unwrap_or(u8::MAX));
            tfra_body.push(u8::try_from(sample_no).unwrap_or(u8::MAX));
        }
        mfra_body.extend_from_slice(&fullbx(b"tfra", 1, 0, &tfra_body));
    }
    let mfra_box = bx(b"mfra", &mfra_body);
    // §8.8.11: `mfro.size` is the whole `mfra` box's size, including `mfro`
    // itself — sixteen fixed bytes, always.
    let total = u32::try_from(mfra_box.len())
        .unwrap_or(u32::MAX)
        .saturating_add(16);
    let mut out = mfra_box;
    out.extend_from_slice(&fullbx(b"mfro", 0, 0, &total.to_be_bytes()));
    out
}

/// `ftyp`, then `moov`, then each of `units` (already-built `moof+mdat`
/// pairs) in order, then `trailer` (typically an [`mfra`] box) if given.
///
/// A thin assembler rather than something that computes fragment offsets
/// itself: a caller building `mfra`'s rows needs those offsets *before*
/// concatenation, by summing the lengths of `ftyp`+`moov`+preceding units, so
/// it already has them by the time this runs.
pub fn frag_file(moov: &[u8], units: &[Vec<u8>], trailer: Option<Vec<u8>>) -> Vec<u8> {
    let mut ftyp_payload = Vec::new();
    ftyp_payload.extend_from_slice(b"isom");
    ftyp_payload.extend_from_slice(&512u32.to_be_bytes());
    ftyp_payload.extend_from_slice(b"isom");
    let mut out = bx(b"ftyp", &ftyp_payload);
    out.extend_from_slice(&bx(b"moov", moov));
    for u in units {
        out.extend_from_slice(u);
    }
    if let Some(t) = trailer {
        out.extend_from_slice(&t);
    }
    out
}
