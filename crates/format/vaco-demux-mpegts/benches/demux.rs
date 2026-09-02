//! The MPEG-TS packet loop (`MpegTsDemuxer::pump`/`read_packet`), the one
//! named hot path in PF-4.5 (#167) this crate had no benchmark for at all —
//! `vaco-format-isom` and `vaco-demux-matroska` both already had one for
//! their own named hot path.
//!
//! The fixture (`simple_file`) is a direct copy of
//! `tests/roundtrip.rs`'s builder of the same name: a two-stream (video +
//! audio) transport stream with a real PAT/PMT and real PES framing, not a
//! synthetic shortcut. It cannot be shared via `use` — `tests/` and
//! `benches/` are separate compilation targets — so it is duplicated here
//! deliberately rather than made `pub` just to avoid the copy.
//!
//! ```text
//! cargo bench -p vaco-demux-mpegts
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unnecessary_cast,
    clippy::single_match_else,
    clippy::vec_init_then_push,
    clippy::useless_vec,
    reason = "bench fixture, mirrors tests/roundtrip.rs's own allow list"
)]

use divan::{Bencher, black_box};
use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

fn main() {
    divan::main();
}

const PAT_PID: u16 = 0x0000;
const PMT_PID: u16 = 0x1000;
const VIDEO_PID: u16 = 0x0100;
const AUDIO_PID: u16 = 0x0101;

struct TsWriter {
    out: Vec<u8>,
    cc: Vec<(u16, u8)>,
}

impl TsWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            cc: Vec::new(),
        }
    }

    fn next_cc(&mut self, pid: u16) -> u8 {
        match self.cc.iter_mut().find(|(p, _)| *p == pid) {
            Some(slot) => {
                slot.1 = (slot.1 + 1) & 0x0F;
                slot.1
            }
            None => {
                self.cc.push((pid, 0));
                0
            }
        }
    }

    fn packet(&mut self, pid: u16, pusi: bool, adaptation: Option<&[u8]>, payload: &[u8]) {
        let cc = self.next_cc(pid);
        let afc = if adaptation.is_some() { 3 } else { 1 };
        let mut p = Vec::new();
        p.push(0x47);
        p.push((u8::from(pusi) << 6) | ((pid >> 8) as u8 & 0x1F));
        p.push((pid & 0xFF) as u8);
        p.push((afc << 4) | cc);
        let room = 184;
        let af_len = adaptation.map_or(0, |a| a.len() + 1);
        assert!(af_len + payload.len() <= room, "packet overflows");
        if let Some(a) = adaptation {
            let stuffing = room - payload.len() - a.len() - 1;
            p.push((a.len() + stuffing) as u8);
            p.extend_from_slice(a);
            p.extend(std::iter::repeat_n(0xFFu8, stuffing));
        }
        p.extend_from_slice(payload);
        assert_eq!(p.len(), 188, "packet must be exactly 188 bytes");
        self.out.extend_from_slice(&p);
    }

    fn section(&mut self, pid: u16, section: &[u8]) {
        let mut first = true;
        let mut rest = section;
        while !rest.is_empty() {
            let room = if first { 183 } else { 184 };
            let n = rest.len().min(room);
            let mut payload = Vec::new();
            if first {
                payload.push(0u8);
            }
            payload.extend_from_slice(&rest[..n]);
            payload.resize(184, 0xFF);
            self.packet(pid, first, None, &payload);
            rest = &rest[n..];
            first = false;
        }
    }

    fn pes(
        &mut self,
        pid: u16,
        stream_id: u8,
        pts: Option<i64>,
        dts: Option<i64>,
        payload: &[u8],
        random_access: bool,
        declare_length: bool,
    ) {
        let mut optional = Vec::new();
        let flags = match (pts, dts) {
            (Some(p), Some(d)) => {
                optional.extend_from_slice(&encode_ts(0b0011, p));
                optional.extend_from_slice(&encode_ts(0b0001, d));
                0xC0
            }
            (Some(p), None) => {
                optional.extend_from_slice(&encode_ts(0b0010, p));
                0x80
            }
            _ => 0x00,
        };
        let mut pes = vec![0x00, 0x00, 0x01, stream_id, 0x00, 0x00];
        pes.push(0x80);
        pes.push(flags);
        pes.push(optional.len() as u8);
        pes.extend_from_slice(&optional);
        pes.extend_from_slice(payload);
        if declare_length {
            let len = (pes.len() - 6) as u16;
            pes[4] = (len >> 8) as u8;
            pes[5] = (len & 0xFF) as u8;
        }

        let mut first = true;
        let mut rest = pes.as_slice();
        while !rest.is_empty() {
            let rai = first && random_access;
            let room = if rai { 182 } else { 184 };
            let n = rest.len().min(room);
            let chunk = &rest[..n];
            let af: Option<Vec<u8>> = if rai {
                Some(vec![0x40])
            } else if n == 184 {
                None
            } else if n == 183 {
                Some(Vec::new())
            } else {
                Some(vec![0x00])
            };
            self.packet(pid, first, af.as_deref(), chunk);
            rest = &rest[n..];
            first = false;
        }
    }
}

fn encode_ts(prefix: u8, v: i64) -> [u8; 5] {
    let v = v as u64;
    [
        (prefix << 4) | ((((v >> 30) as u8) & 0x07) << 1) | 1,
        ((v >> 22) & 0xFF) as u8,
        (((((v >> 15) & 0x7F) as u8) << 1) | 1) as u8,
        ((v >> 7) & 0xFF) as u8,
        ((((v & 0x7F) as u8) << 1) | 1) as u8,
    ]
}

fn crc32(data: &[u8]) -> u32 {
    vaco_format_mpegts_tables::crc32(data)
}

fn build_section(table_id: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
    let section_length = 5 + body.len() + 4;
    let mut s = vec![
        table_id,
        0xB0 | ((section_length >> 8) as u8 & 0x0F),
        (section_length & 0xFF) as u8,
        (ext >> 8) as u8,
        (ext & 0xFF) as u8,
        0xC1 | (version << 1),
        0,
        0,
    ];
    s.extend_from_slice(body);
    s.extend_from_slice(&crc32(&s).to_be_bytes());
    s
}

fn pat(programs: &[(u16, u16)]) -> Vec<u8> {
    let mut body = Vec::new();
    for &(num, pid) in programs {
        body.extend_from_slice(&num.to_be_bytes());
        body.push(0xE0 | ((pid >> 8) as u8 & 0x1F));
        body.push((pid & 0xFF) as u8);
    }
    build_section(0x00, 1, 0, &body)
}

fn pmt(program: u16, version: u8, pcr_pid: u16, streams: &[(u8, u16, Vec<u8>)]) -> Vec<u8> {
    let mut body = vec![
        0xE0 | ((pcr_pid >> 8) as u8 & 0x1F),
        (pcr_pid & 0xFF) as u8,
        0xF0,
        0x00,
    ];
    for (stream_type, pid, desc) in streams {
        body.push(*stream_type);
        body.push(0xE0 | ((*pid >> 8) as u8 & 0x1F));
        body.push((*pid & 0xFF) as u8);
        body.push(0xF0 | ((desc.len() >> 8) as u8 & 0x0F));
        body.push((desc.len() & 0xFF) as u8);
        body.extend_from_slice(desc);
    }
    build_section(0x02, program, version, &body)
}

/// A two-stream file: `frames` video frames at 25 fps and one audio frame per
/// video frame, starting at 90 000 ticks (one second). Direct copy of
/// `tests/roundtrip.rs::simple_file` — see the module doc for why.
fn simple_file(frames: usize) -> Vec<u8> {
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID)]));
    w.section(
        PMT_PID,
        &pmt(
            1,
            0,
            VIDEO_PID,
            &[
                (0x1B, VIDEO_PID, Vec::new()),
                (0x0F, AUDIO_PID, vec![0x0A, 0x04, b'e', b'n', b'g', 0x00]),
            ],
        ),
    );
    for i in 0..frames {
        let pts = 90_000 + (i as i64) * 3600;
        w.pes(
            VIDEO_PID,
            0xE0,
            Some(pts),
            Some(pts),
            &vec![i as u8; 400],
            i % 5 == 0,
            false,
        );
        w.pes(
            AUDIO_PID,
            0xC0,
            Some(pts),
            None,
            &vec![0xAA; 100],
            true,
            true,
        );
    }
    w.out
}

fn open(bytes: Vec<u8>) -> MpegTsDemuxer {
    MpegTsDemuxer::open(
        Box::new(MemorySource::new(bytes)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("fixture must open")
}

const FRAME_COUNTS: &[usize] = &[50, 500, 2000];

/// Full cost of opening the demuxer (stride detection + PAT/PMT discovery)
/// and draining every packet, for a file of `frames` video+audio frame
/// pairs. This is the whole `pump`/`read_packet` loop end to end, not a
/// microbenchmark of one internal function — the loop is what PF-4.5 names.
#[divan::bench(args = FRAME_COUNTS)]
fn open_and_drain(bencher: Bencher<'_, '_>, frames: usize) {
    let bytes = simple_file(frames);
    bencher
        .counter(divan::counter::ItemsCount::new(frames * 2))
        .bench_local(|| {
            let mut demux = open(black_box(bytes.clone()));
            let mut count = 0usize;
            while demux.read_packet().is_ok() {
                count += 1;
            }
            black_box(count)
        });
}
