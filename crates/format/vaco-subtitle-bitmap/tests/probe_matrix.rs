//! Cross-format probe discipline (D17, `planning/AGENT-CONSTRAINTS.md`
//! "Detection and demuxing ask different questions").
//!
//! Every probe in this crate must answer "is this plausibly *my* format?"
//! strictly enough that a sample of any other format in this crate, and a
//! sample of ordinary prose, never outscores the sample's true owner. This is
//! the same shape `vaco-subtitle-text/tests/probe_matrix.rs` uses — the exact
//! test that "caught a real `vplayer`/`stl` collision the day it was
//! written" per this crate's brief.

#![allow(clippy::unwrap_used, reason = "test code")]

use vaco_format_core::probe::ProbeData;

type Probe = fn(&ProbeData<'_>) -> vaco_format_core::probe::ProbeScore;

fn pgs_sample() -> Vec<u8> {
    fn segment(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"PG");
        v.extend_from_slice(&90_000u32.to_be_bytes());
        v.extend_from_slice(&90_000u32.to_be_bytes());
        v.push(kind);
        v.extend_from_slice(&(u16::try_from(payload.len()).unwrap()).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }
    let mut v = segment(0x16, &[1, 2, 3]); // PCS
    v.extend(segment(0x17, &[4, 5])); // WDS
    v.extend(segment(0x14, &[6])); // PDS
    v.extend(segment(0x15, &[7, 8, 9])); // ODS
    v.extend(segment(0x80, &[])); // END
    v
}

fn dvbsub_sample() -> Vec<u8> {
    fn seg(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x0Fu8, kind, 0, 1];
        v.extend_from_slice(&(u16::try_from(payload.len()).unwrap()).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }
    let mut v = seg(0x10, &[1, 2]); // page composition
    v.extend(seg(0x11, &[0, 0, 0, 10, 0, 10])); // region composition
    v.extend(seg(0x80, &[])); // end of display set
    v
}

fn dvbtxt_sample() -> Vec<u8> {
    let mut v = Vec::new();
    for id in [0x02u8, 0x03, 0xFF, 0x02] {
        let mut r = vec![id, 0x2C];
        r.extend(std::iter::repeat_n(0u8, 44));
        v.extend(r);
    }
    v
}

const VOBSUB_IDX_SAMPLE: &[u8] = b"# VobSub index file, v7\n\
size: 720x480\n\
palette: 000000, ffffff, ff0000\n\
\n\
id: en, index: 0\n\
timestamp: 00:00:01:234, filepos: 000000000\n\
timestamp: 00:00:03:456, filepos: 0000004ab\n\
";

const PROSE: &[u8] = b"The quick brown fox jumps over the lazy dog. This is an ordinary\nparagraph of English prose with no timing information in it at all.\nIt spans several lines so that any line-oriented probe sees plenty of\nplausible-looking text to reject.\n";

fn samples() -> Vec<(&'static str, Probe, Vec<u8>)> {
    vec![
        ("sup", vaco_subtitle_bitmap::sup::probe, pgs_sample()),
        (
            "dvbsub",
            vaco_subtitle_bitmap::dvbsub::probe,
            dvbsub_sample(),
        ),
        (
            "dvbtxt",
            vaco_subtitle_bitmap::dvbtxt::probe,
            dvbtxt_sample(),
        ),
        (
            "vobsub",
            vaco_subtitle_bitmap::vobsub::probe,
            VOBSUB_IDX_SAMPLE.to_vec(),
        ),
    ]
}

#[test]
fn every_probe_rejects_plain_prose() {
    for (name, probe, _) in samples() {
        let score = probe(&ProbeData::new(PROSE));
        assert!(
            score.is_none(),
            "{name}'s probe scored {score:?} on plain prose, expected NONE"
        );
    }
}

#[test]
fn every_sample_is_recognised_by_its_own_probe() {
    for (name, probe, sample) in samples() {
        let score = probe(&ProbeData::new(&sample));
        assert!(
            !score.is_none(),
            "{name}'s probe scored NONE on its own sample"
        );
    }
}

#[test]
fn no_probe_outscores_a_samples_true_owner() {
    let all = samples();
    for (owner, _, sample) in &all {
        let data = ProbeData::new(sample);
        let own_score = all
            .iter()
            .find(|(n, _, _)| n == owner)
            .map(|(_, p, _)| p(&data))
            .unwrap();
        for (other, probe, _) in &all {
            if other == owner {
                continue;
            }
            let foreign_score = probe(&data);
            assert!(
                foreign_score < own_score,
                "{other}'s probe scored {foreign_score:?} on {owner}'s sample, \
                 which is not less than {owner}'s own score {own_score:?} — \
                 a real file in this shape could be mis-detected"
            );
        }
    }
}
