//! Cross-format probe discipline for the ten `Framing::StartCode3` members
//! of the bitstream family (D17, `planning/AGENT-CONSTRAINTS.md` "Detection
//! and demuxing ask different questions"; `planning/CONFORMANCE-FINDINGS.md`
//! finding 3).
//!
//! All ten open with the same `00 00 01` start code, so a probe that only
//! checks *that* a start code is present at the front of the file cannot
//! tell them apart: every one of them scores 51 on every one of the others'
//! samples, and ties break alphabetically — which is exactly how a real
//! H.264 elementary stream ended up reported as `avs2`. This is the
//! automated version of the check the brief asked to run by hand against
//! every pair: every format's own sample must probe as that format, and
//! must not probe as any of the other nine.
//!
//! `crates/format/vaco-subtitle-text/tests/probe_matrix.rs` is the model
//! this is copied from. Two differences from that crate, both because raw
//! elementary streams behave differently from self-describing text formats:
//!
//! * Four of the ten (`h264`, `hevc`, `mpegvideo`, `m4v`) have a verified
//!   start-code *identifier* — see `start_code_identifier` in
//!   `src/bitstream.rs` for exactly what was measured and how. Their
//!   samples carry that identifier and are expected to win on content alone,
//!   with no filename needed.
//! * The other six (`avs2`, `avs3`, `cavsvideo`, `evc`, `vc1`, `vvc`) have no
//!   encoder in this `ffmpeg` 8.1 build, so per the brief they make no
//!   structural claim at all and are only ever recognised by filename
//!   extension. Their samples are given a filename with their own
//!   extension and are expected to win *only* because of it — swap the
//!   extension for a wrong one and every one of them should score `NONE`,
//!   which `unverified_formats_are_extension_only` checks directly.
//!
//! One pair is a disclosed, unavoidable tie rather than a bug:
//! `avs2` and `cavsvideo` both list `avs` as a common extension (measured
//! directly — `ffmpeg -h demuxer=avs2` and `-h demuxer=cavsvideo` both print
//! `Common extensions: ... avs`), so a bare `.avs` file scores 50 on both
//! and nothing in either demuxer's own data disambiguates it. That is a
//! property of the reference, not a gap in this fix, so the "no other probe
//! outscores the owner" check below excludes that one ordered pair by name
//! rather than silently passing by accident.

#![allow(clippy::unwrap_used, reason = "test code")]

use vaco_demux_raw::bitstream::{
    DEMUXER_AVS2, DEMUXER_AVS3, DEMUXER_CAVSVIDEO, DEMUXER_EVC, DEMUXER_H264, DEMUXER_HEVC,
    DEMUXER_M4V, DEMUXER_MPEGVIDEO, DEMUXER_VC1, DEMUXER_VVC,
};
use vaco_format_core::DemuxerDesc;
use vaco_format_core::probe::ProbeData;

/// One format's descriptor, a representative sample, and the filename (if
/// any) it needs to be recognised.
struct Case {
    desc: &'static DemuxerDesc,
    sample: &'static [u8],
    filename: Option<&'static str>,
}

/// The ten `StartCode3` members, each with a sample built the way the doc
/// comment above describes. Byte patterns for `h264`/`hevc`/`mpegvideo`/
/// `m4v` are exactly what was measured (see `start_code_identifier`'s doc
/// table); the other six carry an arbitrary start code (never one of the
/// four verified identifiers) since their probe makes no structural claim
/// and content is not what recognises them.
fn cases() -> Vec<Case> {
    vec![
        Case {
            desc: &DEMUXER_H264,
            // SPS, nal_ref_idc 3, forbidden_zero_bit 0 — measured on
            // `ffmpeg -c:v libx264 -f h264`.
            sample: &[0, 0, 1, 0x67, 0xAA, 0xAA, 0xAA, 0xAA],
            filename: None,
        },
        Case {
            desc: &DEMUXER_HEVC,
            // VPS, type 32 — measured on `ffmpeg -c:v libx265 -f hevc`.
            sample: &[0, 0, 1, 0x40, 0x01, 0xAA, 0xAA, 0xAA],
            filename: None,
        },
        Case {
            desc: &DEMUXER_MPEGVIDEO,
            // sequence_header_code — measured identically on `mpeg1video`
            // and `mpeg2video` encoders muxed with `-f mpeg1video`/`-f
            // mpeg2video`.
            sample: &[0, 0, 1, 0xB3, 0x04, 0x00, 0x40, 0x13],
            filename: None,
        },
        Case {
            desc: &DEMUXER_M4V,
            // visual_object_sequence_start_code — measured on `ffmpeg -c:v
            // mpeg4 -f m4v`.
            sample: &[0, 0, 1, 0xB0, 0x01, 0x00, 0x00, 0x01],
            filename: None,
        },
        Case {
            desc: &DEMUXER_AVS2,
            sample: &[0, 0, 1, 0x01, 0xAA, 0xAA, 0xAA, 0xAA],
            filename: Some("sample.avs2"),
        },
        Case {
            desc: &DEMUXER_AVS3,
            sample: &[0, 0, 1, 0x01, 0xAA, 0xAA, 0xAA, 0xAA],
            filename: Some("sample.avs3"),
        },
        Case {
            desc: &DEMUXER_CAVSVIDEO,
            sample: &[0, 0, 1, 0x01, 0xAA, 0xAA, 0xAA, 0xAA],
            filename: Some("sample.avs"),
        },
        Case {
            desc: &DEMUXER_EVC,
            sample: &[0, 0, 1, 0x01, 0xAA, 0xAA, 0xAA, 0xAA],
            filename: Some("sample.evc"),
        },
        Case {
            desc: &DEMUXER_VC1,
            sample: &[0, 0, 1, 0x01, 0xAA, 0xAA, 0xAA, 0xAA],
            filename: Some("sample.vc1"),
        },
        Case {
            desc: &DEMUXER_VVC,
            sample: &[0, 0, 1, 0x01, 0xAA, 0xAA, 0xAA, 0xAA],
            filename: Some("sample.vvc"),
        },
    ]
}

fn probe(case: &Case) -> vaco_format_core::probe::ProbeScore {
    let data = ProbeData::new(case.sample);
    let data = match case.filename {
        Some(f) => data.with_filename(f),
        None => data,
    };
    (case.desc.probe)(&data)
}

/// The one disclosed, unavoidable tie: `avs2` and `cavsvideo` both claim the
/// bare `.avs` extension in the reference itself, so neither can win over
/// the other on `cavsvideo`'s own sample (which is also a valid `.avs`
/// file, extension-wise, for `avs2`).
fn is_disclosed_tie(owner: &str, other: &str) -> bool {
    matches!(
        (owner, other),
        ("cavsvideo", "avs2") | ("avs2", "cavsvideo")
    )
}

#[test]
fn every_sample_is_recognised_by_its_own_probe() {
    for case in cases() {
        let score = probe(&case);
        assert!(
            !score.is_none(),
            "{}'s probe scored NONE on its own sample",
            case.desc.name
        );
    }
}

#[test]
fn no_probe_outscores_a_samples_true_owner() {
    let all = cases();
    for owner in &all {
        let data = ProbeData::new(owner.sample);
        let data = match owner.filename {
            Some(f) => data.with_filename(f),
            None => data,
        };
        let own_score = (owner.desc.probe)(&data);
        for other in &all {
            if other.desc.name == owner.desc.name {
                continue;
            }
            if is_disclosed_tie(owner.desc.name, other.desc.name) {
                continue;
            }
            let foreign_score = (other.desc.probe)(&data);
            assert!(
                foreign_score < own_score,
                "{}'s probe scored {foreign_score:?} on {}'s sample, which is \
                 not less than {}'s own score {own_score:?} — a real file in \
                 this shape could be mis-detected",
                other.desc.name,
                owner.desc.name,
                owner.desc.name,
            );
        }
    }
}

/// The regression this whole finding is about: a genuine H.264 elementary
/// stream must not lose to any of its nine `StartCode3` siblings, including
/// `avs2` — which is exactly what happened before the identifier check
/// existed, because ties broke alphabetically and `avs2` sorts first.
#[test]
fn h264_beats_every_sibling_including_avs2() {
    let all = cases();
    let h264 = all.iter().find(|c| c.desc.name == "h264").unwrap();
    let data = ProbeData::new(h264.sample);
    let h264_score = (h264.desc.probe)(&data);
    for other in &all {
        if other.desc.name == "h264" {
            continue;
        }
        let foreign_score = (other.desc.probe)(&data);
        assert!(
            foreign_score < h264_score,
            "{} scored {foreign_score:?} on a real H.264 sample, not less \
             than h264's own {h264_score:?}",
            other.desc.name
        );
    }
}

/// The six formats with no encoder in this `ffmpeg` build make no structural
/// claim at all: swap their sample's filename for one with the wrong
/// extension and they must score `NONE`, proving the win above came from the
/// extension and not from the (deliberately non-identifying) start code in
/// the sample bytes.
#[test]
fn unverified_formats_are_extension_only() {
    let unverified = ["avs2", "avs3", "cavsvideo", "evc", "vc1", "vvc"];
    for case in cases()
        .into_iter()
        .filter(|c| unverified.contains(&c.desc.name))
    {
        let data = ProbeData::new(case.sample).with_filename("sample.wrongext");
        let score = (case.desc.probe)(&data);
        assert!(
            score.is_none(),
            "{}'s probe scored {score:?} on its own sample bytes with a \
             non-matching extension — it is claiming structural evidence it \
             was never measured to have",
            case.desc.name
        );
    }
}

/// Plain prose, with no filename at all, must not be claimed by any of the
/// ten — the cheapest possible "definitely not my format" input (per
/// `planning/AGENT-CONSTRAINTS.md` "Detection and demuxing ask different
/// questions").
#[test]
fn every_probe_rejects_plain_prose_with_no_filename() {
    const PROSE: &[u8] = b"The quick brown fox jumps over the lazy dog. This is ordinary English\n\
          prose with no start code and no informative filename anywhere in it.\n";
    for case in cases() {
        let score = (case.desc.probe)(&ProbeData::new(PROSE));
        assert!(
            score.is_none(),
            "{}'s probe scored {score:?} on plain prose with no filename",
            case.desc.name
        );
    }
}
