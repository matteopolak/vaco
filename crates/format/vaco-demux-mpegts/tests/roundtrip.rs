//! Named cases over synthetic transport streams.
//!
//! Every fixture is built in-process by [`TsWriter`], so the tests are
//! hermetic and each one pins exactly one rule. A committed `.ts` file would
//! be both larger and less specific: the whole difficulty of this container is
//! in cases — a wrap, a mid-stream PMT, a PES packet with no declared length,
//! a lost packet — that a recorded file happens to contain or happens not to.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::single_match_else,
    clippy::match_wildcard_for_single_variants,
    clippy::redundant_closure_for_method_calls,
    clippy::unnecessary_cast,
    clippy::cast_possible_wrap,
    clippy::useless_vec,
    clippy::if_same_then_else,
    unused_parens,
    reason = "test code"
)]

use vaco_core::{Error, Timestamp};
use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

// --------------------------------------------------------------- fixtures

const PAT_PID: u16 = 0x0000;
const PMT_PID: u16 = 0x1000;
const VIDEO_PID: u16 = 0x0100;
const AUDIO_PID: u16 = 0x0101;

/// Writes well-formed transport streams.
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

    /// One transport packet. `adaptation` is the field body *after* its own
    /// length byte, or `None` for no adaptation field.
    fn packet(&mut self, pid: u16, pusi: bool, adaptation: Option<&[u8]>, payload: &[u8]) {
        let cc = self.next_cc(pid);
        let afc = if adaptation.is_some() { 3 } else { 1 };
        let mut p = Vec::with_capacity(188);
        p.push(0x47);
        p.push((u8::from(pusi) << 6) | ((pid >> 8) as u8 & 0x1F));
        p.push((pid & 0xFF) as u8);
        p.push((afc << 4) | cc);
        let room = 184;
        let af_len = adaptation.map_or(0, |a| a.len() + 1);
        assert!(af_len + payload.len() <= room, "packet overflows");
        if let Some(a) = adaptation {
            // Stuff the adaptation field out so the payload lands flush at the
            // end, which is what a real muxer does.
            let stuffing = room - payload.len() - a.len() - 1;
            p.push((a.len() + stuffing) as u8);
            p.extend_from_slice(a);
            p.extend(std::iter::repeat_n(0xFFu8, stuffing));
        }
        p.extend_from_slice(payload);
        assert_eq!(p.len(), 188, "packet must be exactly 188 bytes");
        self.out.extend_from_slice(&p);
    }

    fn null_packet(&mut self) {
        self.packet(0x1FFF, false, None, &[0xFF; 184]);
    }

    /// A PSI section, split across as many packets as it needs.
    fn section(&mut self, pid: u16, section: &[u8]) {
        let mut first = true;
        let mut rest = section;
        while !rest.is_empty() {
            let room = if first { 183 } else { 184 };
            let n = rest.len().min(room);
            let mut payload = Vec::new();
            if first {
                payload.push(0u8); // pointer_field
            }
            payload.extend_from_slice(&rest[..n]);
            payload.resize(184, 0xFF);
            self.packet(pid, first, None, &payload);
            rest = &rest[n..];
            first = false;
        }
    }

    /// A PES packet, split across packets, with the timestamps and flags a
    /// real one carries.
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
            // A random-access flag needs a length byte and a flags byte.
            let room = if rai { 182 } else { 184 };
            let n = rest.len().min(room);
            let chunk = &rest[..n];
            // A short chunk is padded by an adaptation field, which is how a
            // real muxer fills the last packet of a PES packet. One spare byte
            // is a zero-length field with no flags byte at all.
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

/// `(stream_type, pid, descriptors)`
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
/// video frame, starting at 90 000 ticks (one second).
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
    .expect("well-formed transport stream")
}

fn drain(d: &mut MpegTsDemuxer) -> Vec<vaco_packet::Packet> {
    let mut v = Vec::new();
    loop {
        match d.read_packet() {
            Ok(p) => v.push(p),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected {e:?}"),
        }
        assert!(v.len() < 100_000, "runaway read");
    }
    v
}

// ------------------------------------------------------------------ tests

#[test]
fn psi_produces_one_program_and_two_streams() {
    let d = open(simple_file(10));
    assert_eq!(d.streams().len(), 2);
    assert_eq!(d.streams()[0].id, Some(i64::from(VIDEO_PID)));
    assert_eq!(d.streams()[1].id, Some(i64::from(AUDIO_PID)));
    assert_eq!(
        d.streams()[0].time_base,
        vaco_core::Rational::new(1, 90_000)
    );
    assert_eq!(d.streams()[1].metadata_get("language"), Some("eng"));
    assert_eq!(d.programs().len(), 1);
    assert_eq!(d.programs()[0].id, 1);
    assert_eq!(d.programs()[0].stream_indices, vec![0, 1]);
    // Fields, not metadata: they used to print as `TAG:pcr_pid=…`.
    assert_eq!(d.programs()[0].pcr_pid, Some(256));
    assert_eq!(d.programs()[0].pmt_pid, Some(PMT_PID));
    assert_eq!(d.programs()[0].program_num, Some(1));
    assert_eq!(d.programs()[0].pmt_version, Some(0));
    assert!(
        !d.programs()[0]
            .metadata
            .iter()
            .any(|(k, _)| k == "pcr_pid" || k == "pmt_pid" || k == "pmt_version"),
        "the three values must not also travel as tags"
    );
}

#[test]
fn every_pes_packet_comes_back_once_with_its_timestamps() {
    let mut d = open(simple_file(10));
    let packets = drain(&mut d);
    let video: Vec<_> = packets.iter().filter(|p| p.stream_index == 0).collect();
    let audio: Vec<_> = packets.iter().filter(|p| p.stream_index == 1).collect();
    assert_eq!(video.len(), 10);
    assert_eq!(audio.len(), 10);
    for (i, p) in video.iter().enumerate() {
        assert_eq!(p.pts.ticks(), Some(90_000 + (i as i64) * 3600));
        assert_eq!(p.dts, p.pts);
        assert_eq!(p.len, 400);
        assert_eq!(p.is_key(), i % 5 == 0);
    }
    // Audio has no `random_access_indicator`; every audio frame is a sync
    // point, which is what the reference reports too.
    assert!(audio.iter().all(|p| p.is_key()));
    assert!(audio.iter().all(|p| p.len == 100));
}

#[test]
fn eof_is_sticky() {
    let mut d = open(simple_file(3));
    let _ = drain(&mut d);
    for _ in 0..5 {
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
}

#[test]
fn a_video_pes_with_no_declared_length_ends_at_the_next_one_and_at_eof() {
    let mut d = open(simple_file(4));
    let packets = drain(&mut d);
    // The fourth video packet has nothing after it, so it exists only because
    // end of input completed it.
    assert_eq!(
        packets.iter().filter(|p| p.stream_index == 0).count(),
        4,
        "the last unbounded PES packet must be emitted at EOF"
    );
}

#[test]
fn start_time_and_duration_are_estimated_because_nothing_declares_them() {
    let d = open(simple_file(25));
    assert_eq!(d.streams()[0].start_time.ticks(), Some(90_000));
    // 25 frames at 3600 ticks: the last frame's own duration is inferred from
    // the previous inter-frame delta, so the end is 90_000 + 25 * 3600.
    let dur = d.duration().expect("estimated");
    assert_eq!(dur.as_micros(), 25 * 3600 * 1_000_000 / 90_000);
    let per_stream = d.streams()[0].duration().expect("per-stream duration");
    assert_eq!(per_stream, dur);
}

#[test]
fn trailing_null_packets_do_not_shorten_the_duration() {
    // The tail-scan retry loop is what makes this work: the last real
    // timestamp is far from the end of the file.
    let mut bytes = simple_file(25);
    let real = open(bytes.clone()).duration();
    let mut w = TsWriter::new();
    for _ in 0..4000 {
        w.null_packet();
    }
    bytes.extend_from_slice(&w.out);
    assert_eq!(open(bytes).duration(), real);
}

#[test]
fn a_file_starting_mid_packet_is_resynchronised() {
    let mut bytes = vec![0x11u8; 77];
    bytes.extend_from_slice(&simple_file(6));
    let mut d = open(bytes);
    assert_eq!(d.streams().len(), 2);
    assert_eq!(drain(&mut d).len(), 12);
}

#[test]
fn a_lost_packet_marks_the_pes_packet_corrupt_rather_than_dropping_it() {
    let bytes = simple_file(6);
    // Remove one middle packet of the second video PES packet. Finding it by
    // PID keeps the test independent of the writer's exact packing.
    let mut out = Vec::new();
    let mut removed = false;
    let mut seen = 0;
    for chunk in bytes.chunks(188) {
        let pid = (u16::from(chunk[1] & 0x1F) << 8) | u16::from(chunk[2]);
        let pusi = chunk[1] & 0x40 != 0;
        if pid == VIDEO_PID && !pusi {
            seen += 1;
            if seen == 4 && !removed {
                removed = true;
                continue;
            }
        }
        out.extend_from_slice(chunk);
    }
    assert!(removed, "fixture must contain a continuation packet");
    let mut d = open(out);
    let packets = drain(&mut d);
    assert!(
        packets
            .iter()
            .any(|p| p.flags.contains(vaco_packet::PacketFlags::CORRUPT)),
        "a continuity gap must be reported, not hidden"
    );
    assert!(d.stats().continuity_gaps >= 1);
}

#[test]
fn a_pmt_with_a_broken_crc_is_ignored_silently() {
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID)]));
    let mut bad = pmt(1, 0, VIDEO_PID, &[(0x1B, VIDEO_PID, Vec::new())]);
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    w.section(PMT_PID, &bad);
    for i in 0..4 {
        w.pes(
            VIDEO_PID,
            0xE0,
            Some(90_000 + i * 3600),
            None,
            &[0; 50],
            true,
            true,
        );
    }
    let d = open(w.out);
    assert_eq!(
        d.streams().len(),
        0,
        "a failing CRC must not create streams"
    );
    assert!(d.stats().crc_failures >= 1);
}

#[test]
fn a_stream_whose_codec_has_no_codec_id_is_still_reported() {
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID)]));
    // 0x81 is AC-3 in the ATSC private range; `CodecId` has no variant.
    w.section(
        PMT_PID,
        &pmt(1, 0, 0x1FFF, &[(0x81, AUDIO_PID, Vec::new())]),
    );
    for i in 0..4 {
        w.pes(
            AUDIO_PID,
            0xBD,
            Some(90_000 + i * 2880),
            None,
            &[0; 60],
            true,
            true,
        );
    }
    let d = open(w.out);
    assert_eq!(d.streams().len(), 1);
    let s = &d.streams()[0];
    assert_eq!(s.media_type(), Some(vaco_core::MediaType::Audio));
    assert_eq!(s.params.codec_id, None);
    assert_eq!(s.metadata_get("ts_codec"), Some("ac3"));
}

#[test]
fn a_registration_descriptor_becomes_the_codec_tag() {
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID)]));
    let desc = vec![0x05, 4, b'O', b'p', b'u', b's'];
    w.section(PMT_PID, &pmt(1, 0, 0x1FFF, &[(0x06, AUDIO_PID, desc)]));
    for i in 0..4 {
        w.pes(
            AUDIO_PID,
            0xBD,
            Some(90_000 + i * 1800),
            None,
            &[0; 60],
            true,
            true,
        );
    }
    let d = open(w.out);
    assert_eq!(d.streams()[0].params.codec_tag, Some(*b"Opus"));
    assert_eq!(
        d.streams()[0].params.codec_id,
        Some(vaco_codec_core::CodecId::Opus)
    );
}

#[test]
fn a_thirty_three_bit_wrap_stays_monotonic() {
    const PERIOD: i64 = 1 << 33;
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID)]));
    w.section(
        PMT_PID,
        &pmt(1, 0, VIDEO_PID, &[(0x1B, VIDEO_PID, Vec::new())]),
    );
    // Start a little before the wrap and cross it.
    let base = PERIOD - 10 * 3600;
    for i in 0..25i64 {
        let raw = (base + i * 3600) % PERIOD;
        w.pes(
            VIDEO_PID,
            0xE0,
            Some(raw),
            Some(raw),
            &[0u8; 80],
            i == 0,
            true,
        );
    }
    let mut d = open(w.out);
    let packets = drain(&mut d);
    assert_eq!(packets.len(), 25);
    let ts: Vec<i64> = packets.iter().filter_map(|p| p.pts.ticks()).collect();
    for pair in ts.windows(2) {
        assert_eq!(
            pair[1] - pair[0],
            3600,
            "the wrap must be corrected, not stepped over: {ts:?}"
        );
    }
}

#[test]
fn a_declared_discontinuity_is_passed_through_untouched() {
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID)]));
    w.section(
        PMT_PID,
        &pmt(1, 0, VIDEO_PID, &[(0x1B, VIDEO_PID, Vec::new())]),
    );
    for i in 0..5i64 {
        let pts = 90_000 + i * 3600;
        w.pes(VIDEO_PID, 0xE0, Some(pts), Some(pts), &[0; 60], true, true);
    }
    // A splice: the adaptation field declares the jump, so the new base is
    // legitimate and must survive into the reported timestamps.
    for i in 0..5i64 {
        let pts = 9_000_000 + i * 3600;
        let mut pes = Vec::new();
        pes.extend_from_slice(&[0x00, 0x00, 0x01, 0xE0]);
        let mut payload = vec![0x80u8, 0x80, 5];
        payload.extend_from_slice(&encode_ts(0b0010, pts));
        payload.extend_from_slice(&[0u8; 60]);
        pes.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        pes.extend_from_slice(&payload);
        let mut af = vec![0x80u8]; // discontinuity_indicator
        af.resize(184 - pes.len() - 1, 0xFF);
        w.packet(VIDEO_PID, true, Some(&af), &pes);
    }
    let mut d = open(w.out);
    let ts: Vec<i64> = drain(&mut d).iter().filter_map(|p| p.pts.ticks()).collect();
    assert_eq!(ts.len(), 10);
    assert_eq!(ts[4], 90_000 + 4 * 3600);
    assert_eq!(ts[5], 9_000_000, "a declared discontinuity is not repaired");
    assert!(d.stats().discontinuities >= 1);
}

#[test]
fn seeking_bisects_and_lands_at_or_before_the_target() {
    let mut d = open(simple_file(200));
    let target = Timestamp::new(90_000 + 150 * 3600);
    d.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: target,
        },
        SeekFlags::BACKWARD,
    )
    .expect("seek");
    let first = d.read_packet().expect("a packet after seeking");
    let got = first.pts.ticks().expect("timestamp");
    assert!(got <= target.ticks().unwrap(), "landed past the target");
    assert!(
        got >= 90_000 + 100 * 3600,
        "landed far too early: {got} for target {target:?}"
    );
}

#[test]
fn seeking_to_the_start_replays_the_whole_file() {
    let mut d = open(simple_file(40));
    let all = drain(&mut d);
    d.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(0),
        },
        SeekFlags::BACKWARD,
    )
    .expect("seek");
    let again = drain(&mut d);
    assert_eq!(again.len(), all.len());
    assert_eq!(again[0].pts, all[0].pts);
}

#[test]
fn a_byte_seek_resynchronises_to_a_packet_boundary() {
    let mut d = open(simple_file(40));
    d.seek(SeekTarget::Byte(5000), SeekFlags::empty())
        .expect("byte seek");
    let p = d.read_packet().expect("a packet");
    assert!(p.pts.is_some());
}

#[test]
fn a_second_program_appears_when_its_pmt_does() {
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID), (2, PMT_PID + 1)]));
    w.section(
        PMT_PID,
        &pmt(1, 0, VIDEO_PID, &[(0x1B, VIDEO_PID, Vec::new())]),
    );
    for i in 0..3i64 {
        w.pes(
            VIDEO_PID,
            0xE0,
            Some(90_000 + i * 3600),
            None,
            &[0; 60],
            true,
            true,
        );
    }
    // The second program's PMT only arrives after some payload, which is the
    // progressive-discovery case MPEG-TS makes ordinary.
    w.section(
        PMT_PID + 1,
        &pmt(2, 0, AUDIO_PID, &[(0x0F, AUDIO_PID, Vec::new())]),
    );
    for i in 0..3i64 {
        w.pes(
            AUDIO_PID,
            0xC0,
            Some(90_000 + i * 2880),
            None,
            &[0; 60],
            true,
            true,
        );
    }
    let d = open(w.out);
    assert_eq!(d.programs().len(), 2);
    assert_eq!(d.streams().len(), 2);
}

#[test]
fn an_sdt_supplies_the_service_name_tags() {
    let mut w = TsWriter::new();
    w.section(PAT_PID, &pat(&[(1, PMT_PID)]));
    w.section(
        PMT_PID,
        &pmt(1, 0, VIDEO_PID, &[(0x1B, VIDEO_PID, Vec::new())]),
    );
    let mut svc = vec![0x48u8, 0, 0x01, 6];
    svc.extend_from_slice(b"FFmpeg");
    svc.push(9);
    svc.extend_from_slice(b"Service01");
    svc[1] = (svc.len() - 2) as u8;
    let mut body = vec![0x00, 0x01, 0xFF, 0x00, 0x01, 0xFF];
    body.push(0x80 | ((svc.len() >> 8) as u8));
    body.push((svc.len() & 0xFF) as u8);
    body.extend_from_slice(&svc);
    w.section(0x0011, &build_section(0x42, 1, 0, &body));
    for i in 0..3i64 {
        w.pes(
            VIDEO_PID,
            0xE0,
            Some(90_000 + i * 3600),
            None,
            &[0; 60],
            true,
            true,
        );
    }
    let d = open(w.out);
    let meta = &d.programs()[0].metadata;
    assert_eq!(
        meta.iter()
            .find(|(k, _)| k == "service_name")
            .map(|(_, v)| v.as_str()),
        Some("Service01")
    );
    assert_eq!(
        meta.iter()
            .find(|(k, _)| k == "service_provider")
            .map(|(_, v)| v.as_str()),
        Some("FFmpeg")
    );
}

#[test]
fn a_192_byte_m2ts_stride_is_detected() {
    let plain = simple_file(8);
    let mut m2ts = Vec::new();
    for chunk in plain.chunks(188) {
        m2ts.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        m2ts.extend_from_slice(chunk);
    }
    let mut d = open(m2ts);
    assert_eq!(d.stride(), vaco_format_mpegts_tables::PacketStride::M2ts);
    assert_eq!(d.streams().len(), 2);
    assert_eq!(drain(&mut d).len(), 16);
}

#[test]
fn a_204_byte_stride_ignores_its_parity_bytes() {
    let plain = simple_file(8);
    let mut rs = Vec::new();
    for chunk in plain.chunks(188) {
        rs.extend_from_slice(chunk);
        rs.extend_from_slice(&[0x5A; 16]);
    }
    let mut d = open(rs);
    assert_eq!(d.stride(), vaco_format_mpegts_tables::PacketStride::Rs);
    assert_eq!(drain(&mut d).len(), 16);
}

#[test]
fn a_strict_limit_refuses_rather_than_allocating() {
    let bytes = simple_file(40);
    let d = MpegTsDemuxer::open_with_limits(
        Box::new(MemorySource::new(bytes)),
        &FormatOptions::default(),
        Limits::tiny(),
    );
    // Either it opens and then fails cleanly on a packet, or it refuses at
    // open. Both are correct; allocating past the ceiling is not.
    match d {
        Ok(mut d) => {
            let mut n = 0;
            while let Ok(_p) = d.read_packet() {
                n += 1;
                assert!(n < 10_000);
            }
        }
        Err(Error::LimitExceeded { .. }) => {}
        Err(e) => panic!("unexpected {e:?}"),
    }
}

#[test]
fn an_input_of_only_null_packets_opens_with_no_streams() {
    let mut w = TsWriter::new();
    for _ in 0..2000 {
        w.null_packet();
    }
    let mut d = open(w.out);
    assert_eq!(d.streams().len(), 0);
    assert!(d.duration().is_none());
    assert!(matches!(d.read_packet(), Err(Error::Eof)));
}

#[test]
fn a_file_with_no_packet_rhythm_is_refused() {
    let bytes = vec![0u8; 4096];
    let r = MpegTsDemuxer::open(
        Box::new(MemorySource::new(bytes)),
        &NoParsers,
        &FormatOptions::default(),
    );
    assert!(matches!(r, Err(Error::InvalidData(_))));
}

#[test]
fn discovery_can_wrap_the_demuxer_and_replays_every_packet() {
    use vaco_format_core::discovery::Discovery;
    let d = open(simple_file(12));
    let mut disc = Discovery::new(d, vaco_demux_mpegts::FLAGS, &FormatOptions::default());
    disc.run(&NoParsers).expect("discovery");
    let mut n = 0;
    while let Ok(_p) = disc.read_packet() {
        n += 1;
        assert!(n < 10_000);
    }
    assert_eq!(n, 24);
}
