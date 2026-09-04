//! A HEIF file built box by box (ISO/IEC 23008-12): two `jpeg` tiles — one
//! split across two `iloc` extents — a `grid` item whose descriptor lives in
//! `idat` (`construction_method 1`), and a non-hidden thumbnail. What this
//! pins is the demuxer's *shape* for an item file, measured on real files
//! against `ffprobe 9.0.1` (see `docs/format/vaco-demux-mp4.md`): every
//! coded item is one stream with one packet, the grid is a stream group,
//! not a stream, and a seek re-emits the single frame.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use vaco_codec_core::CodecId;
use vaco_core::{Disposition, Rational, Timestamp};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, SeekFlags, SeekTarget, StreamGroupKind, TileGrid};
use vaco_format_isom::build::{bx, fullbx};
use vaco_io::{MediaSource, MemorySource};

const TILE_A: &[u8] = b"tile-a-bytes";
const TILE_B_HEAD: &[u8] = b"tile-b-";
const TILE_B_TAIL: &[u8] = b"second-extent";
const THUMB: &[u8] = b"thumb";

fn infe(id: u16, kind: [u8; 4], name: &str, hidden: bool) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    body.extend_from_slice(&0u16.to_be_bytes());
    body.extend_from_slice(&kind);
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    fullbx(b"infe", 2, u32::from(hidden), &body)
}

fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut body = w.to_be_bytes().to_vec();
    body.extend_from_slice(&h.to_be_bytes());
    fullbx(b"ispe", 0, 0, &body)
}

/// `iloc` version 1, 4-byte offsets and lengths, no base offset.
fn iloc(entries: &[(u16, u16, &[(u32, u32)])]) -> Vec<u8> {
    let mut body = vec![0x44, 0x00];
    body.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for (id, method, extents) in entries {
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&method.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&(extents.len() as u16).to_be_bytes());
        for (off, len) in *extents {
            body.extend_from_slice(&off.to_be_bytes());
            body.extend_from_slice(&len.to_be_bytes());
        }
    }
    fullbx(b"iloc", 1, 0, &body)
}

/// Items: 1 = tile A, 2 = tile B (two extents), 3 = grid (primary, in
/// `idat`), 4 = thumbnail. Returns the file bytes.
fn heif() -> Vec<u8> {
    let ftyp = bx(
        b"ftyp",
        &[b"mif1".as_slice(), &0u32.to_be_bytes(), b"mif1", b"jpeg"].concat(),
    );
    let mut hdlr_body = 0u32.to_be_bytes().to_vec();
    hdlr_body.extend_from_slice(b"pict");
    hdlr_body.extend_from_slice(&[0; 12]);
    hdlr_body.push(0);
    let hdlr = fullbx(b"hdlr", 0, 0, &hdlr_body);
    let pitm = fullbx(b"pitm", 0, 0, &3u16.to_be_bytes());
    let mut iinf_body = 4u16.to_be_bytes().to_vec();
    iinf_body.extend_from_slice(&infe(1, *b"jpeg", "", true));
    iinf_body.extend_from_slice(&infe(2, *b"jpeg", "", true));
    iinf_body.extend_from_slice(&infe(3, *b"grid", "Grid", false));
    iinf_body.extend_from_slice(&infe(4, *b"jpeg", "Thumb", false));
    let iinf = fullbx(b"iinf", 0, 0, &iinf_body);
    let dimg = bx(
        b"dimg",
        &[3u16, 2, 1, 2]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect::<Vec<_>>(),
    );
    let thmb = bx(
        b"thmb",
        &[4u16, 1, 3]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect::<Vec<_>>(),
    );
    let iref = fullbx(b"iref", 0, 0, &[dimg, thmb].concat());
    // ipco: 1 = tile ispe 16x8, 2 = grid ispe 30x8 (cropped from 32x8),
    // 3 = thumb ispe 4x2.
    let ipco = bx(b"ipco", &[ispe(16, 8), ispe(30, 8), ispe(4, 2)].concat());
    let ipma = fullbx(
        b"ipma",
        0,
        0,
        &[
            4u32.to_be_bytes().as_slice(),
            &[0, 1, 1, 1],
            &[0, 2, 1, 1],
            &[0, 3, 1, 2],
            &[0, 4, 1, 3],
        ]
        .concat(),
    );
    let iprp = bx(b"iprp", &[ipco, ipma].concat());
    // ImageGrid: version 0, flags 0, 1 row, 2 columns, output 30x8.
    let grid_desc = [0u8, 0, 0, 1, 0, 30, 0, 8];
    let idat = bx(b"idat", &grid_desc);

    let mdat_payload = [TILE_A, TILE_B_HEAD, THUMB, TILE_B_TAIL].concat();
    let extents = |mdat_off: u32| -> Vec<u8> {
        let a = mdat_off;
        let b_head = a + TILE_A.len() as u32;
        let thumb = b_head + TILE_B_HEAD.len() as u32;
        let b_tail = thumb + THUMB.len() as u32;
        iloc(&[
            (1, 0, &[(a, TILE_A.len() as u32)]),
            (
                2,
                0,
                &[
                    (b_head, TILE_B_HEAD.len() as u32),
                    (b_tail, TILE_B_TAIL.len() as u32),
                ],
            ),
            (3, 1, &[(0, grid_desc.len() as u32)]),
            (4, 0, &[(thumb, THUMB.len() as u32)]),
        ])
    };
    let meta_of = |il: &[u8]| {
        fullbx(
            b"meta",
            0,
            0,
            &[
                hdlr.as_slice(),
                pitm.as_slice(),
                il,
                iinf.as_slice(),
                iref.as_slice(),
                iprp.as_slice(),
                idat.as_slice(),
            ]
            .concat(),
        )
    };
    let probe = extents(0);
    let mdat_off = (ftyp.len() + meta_of(&probe).len() + 8) as u32;
    let meta = meta_of(&extents(mdat_off));
    [ftyp, meta, bx(b"mdat", &mdat_payload)].concat()
}

fn open(bytes: Vec<u8>) -> Mp4Demuxer {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options::default(),
    )
    .unwrap()
}

#[test]
fn items_become_one_packet_streams_and_the_grid_a_group() {
    let mut demux = open(heif());
    let streams = demux.streams();
    assert_eq!(
        streams.len(),
        3,
        "two tiles and a thumbnail; the grid is not a stream"
    );
    for (s, (id, w, h)) in streams.iter().zip([(1, 16, 8), (2, 16, 8), (4, 4, 2)]) {
        assert_eq!(s.id, Some(id));
        assert_eq!(s.params.codec_id, Some(CodecId::Jpeg));
        assert_eq!(s.params.codec_tag, Some(*b"jpeg"));
        let v = s.params.video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (w, h));
        assert_eq!(v.sample_aspect_ratio, Rational::ONE);
        assert_eq!(s.time_base, Rational::new(1, 1));
        assert_eq!(s.r_frame_rate, Rational::new(1, 1));
        assert_eq!(s.frame_count, Some(1));
        assert_eq!(s.duration_ts, None);
    }
    assert_eq!(streams[0].disposition, Disposition::DEPENDENT);
    assert_eq!(streams[1].disposition, Disposition::DEPENDENT);
    assert_eq!(streams[2].disposition, Disposition::empty());
    assert!(streams[0].metadata.is_empty());
    assert_eq!(
        streams[2].metadata,
        vec![("title".to_owned(), "Thumb".to_owned())]
    );

    let groups = demux.stream_groups();
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.id, 3);
    assert_eq!(g.stream_indices, vec![0, 1]);
    assert_eq!(g.disposition, Disposition::DEFAULT);
    assert_eq!(g.metadata, vec![("title".to_owned(), "Grid".to_owned())]);
    let expected = TileGrid {
        tile_rows: 1,
        tile_columns: 2,
        coded_width: 32,
        coded_height: 8,
        output_width: 30,
        output_height: 8,
        horizontal_offset: 0,
        vertical_offset: 0,
        tile_offsets: vec![(0, 0), (16, 0)],
    };
    assert!(
        matches!(&g.kind, StreamGroupKind::TileGrid(grid) if *grid == expected),
        "{:?}",
        g.kind
    );

    let mut got = Vec::new();
    while let Ok(p) = demux.read_packet() {
        assert_eq!(p.pts, Timestamp::ZERO);
        assert_eq!(p.dts, Timestamp::ZERO);
        assert!(p.is_key());
        got.push((p.stream_index, p.payload().to_vec()));
    }
    assert_eq!(
        got,
        vec![
            (0, TILE_A.to_vec()),
            (1, [TILE_B_HEAD, TILE_B_TAIL].concat()),
            (2, THUMB.to_vec()),
        ],
        "one packet per item, extents concatenated"
    );

    // A seek lands back on the single frame; the stream is not spent.
    demux
        .seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::ZERO,
            },
            SeekFlags::empty(),
        )
        .unwrap();
    let again = demux.read_packet().unwrap();
    assert_eq!(again.payload(), TILE_A);
}

#[test]
fn a_meta_box_that_is_not_pict_is_still_no_movie() {
    let mut bytes = heif();
    // Flip the handler to `mdta`: a QuickTime metadata `meta`, not items.
    let at = bytes.windows(4).position(|w| w == b"pict").unwrap();
    bytes[at..at + 4].copy_from_slice(b"mdta");
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    assert!(
        Mp4Demuxer::open(
            src,
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options::default()
        )
        .is_err()
    );
}
