#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
//! One comparison loop over every fixture this crate ships, each opened
//! through the exact `DemuxerDesc` a caller would use and checked against
//! `ffprobe 8.1` (`-show_entries stream=sample_rate,channels,duration_ts,
//! time_base -show_entries format=duration`, `-f <name>` for the headerless
//! formats since they have no magic to auto-detect).
//!
//! Two formats' `duration_ts` diverge from ours by *time base*, not by
//! duration: `adx`'s reference reports ticks at 1/250 (one per 32-sample
//! block) and `g726`/`g726le` at a generic 1/90000, where this crate counts
//! samples at `1/sample_rate`. The wall-clock duration this crate computes
//! agrees with the reference in both cases (`0.304 s` and `0.3 s`
//! respectively), so the table below checks duration in microseconds, which
//! is convention-independent, rather than the raw tick count.
//!
//! `aptx`/`aptx_hd` are a second divergence, the other way: the reference's
//! raw demuxer declines to state a duration at all (`N/A`) for those two,
//! while this crate estimates one from the file size divided by the fixed
//! 4:1/6:1 block ratio — the same policy `RawPcmDemuxer` uses for headerless
//! PCM. Recorded rather than "fixed", since matching `N/A` would mean
//! deliberately discarding information this crate can compute exactly.

use std::path::Path;

use vaco_format_core::discovery::NoParsers;
use vaco_format_core::probe::ProbeData;
use vaco_format_core::{Demuxer, DemuxerDesc};
use vaco_io::MediaSource;

struct Row {
    file: &'static str,
    desc: DemuxerDesc,
    sample_rate: u32,
    channels: u32,
    /// `ffprobe -show_entries format=duration`, in microseconds; `None`
    /// where the reference itself reports `N/A`.
    reference_duration_us: Option<i64>,
}

fn open_file(path: &Path) -> Box<dyn MediaSource> {
    let bytes = std::fs::read(path).unwrap();
    Box::new(vaco_io::MemorySource::new(bytes))
}

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

#[test]
fn every_fixture_matches_the_measured_reference_row() {
    use vaco_format_misc_audio::{adx, g723, rawcodec, sbc, tta, wavpack};

    let rows = [
        Row {
            file: "wavpack.wv",
            desc: wavpack::DEMUXER,
            sample_rate: 44_100,
            channels: 2,
            reference_duration_us: Some(300_000),
        },
        Row {
            file: "tta.tta",
            desc: tta::DEMUXER,
            sample_rate: 44_100,
            channels: 2,
            reference_duration_us: Some(300_000),
        },
        Row {
            file: "adx.adx",
            desc: adx::DEMUXER,
            sample_rate: 8000,
            channels: 1,
            reference_duration_us: Some(304_000),
        },
        Row {
            file: "g722.g722",
            desc: rawcodec::DEMUXER_G722,
            sample_rate: 16_000,
            channels: 1,
            reference_duration_us: Some(150_000),
        },
        Row {
            file: "g726.g726",
            desc: rawcodec::DEMUXER_G726,
            sample_rate: 8000,
            channels: 1,
            reference_duration_us: Some(300_000),
        },
        Row {
            file: "g726le.g726le",
            desc: rawcodec::DEMUXER_G726LE,
            sample_rate: 8000,
            channels: 1,
            reference_duration_us: Some(300_000),
        },
        Row {
            file: "aptx.aptx",
            desc: rawcodec::DEMUXER_APTX,
            sample_rate: 48_000,
            channels: 2,
            reference_duration_us: None,
        },
        Row {
            file: "aptx_hd.aptx_hd",
            desc: rawcodec::DEMUXER_APTX_HD,
            sample_rate: 48_000,
            channels: 2,
            reference_duration_us: None,
        },
        Row {
            file: "sbc.sbc",
            desc: sbc::DEMUXER,
            sample_rate: 16_000,
            channels: 1,
            reference_duration_us: None,
        },
        Row {
            file: "g723_1.g723_1",
            desc: g723::DEMUXER,
            sample_rate: 8000,
            channels: 1,
            reference_duration_us: None,
        },
    ];

    let mut failures = Vec::new();
    for row in &rows {
        let path = fixture(row.file);
        let src = open_file(&path);
        let opened = (row.desc.open)(src, &NoParsers);
        let mut demux = match opened {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{}: failed to open: {e:?}", row.file));
                continue;
            }
        };
        let streams = demux.streams();
        let Some(stream) = streams.first() else {
            failures.push(format!("{}: no streams reported", row.file));
            continue;
        };
        let Some(audio) = stream.params.audio.as_ref() else {
            failures.push(format!("{}: not reported as an audio stream", row.file));
            continue;
        };
        if audio.sample_rate != row.sample_rate {
            failures.push(format!(
                "{}: sample_rate {} != reference {}",
                row.file, audio.sample_rate, row.sample_rate
            ));
        }
        let channels = audio.layout.as_ref().map_or(0, |l| l.channels);
        if channels != row.channels {
            failures.push(format!(
                "{}: channels {} != reference {}",
                row.file, channels, row.channels
            ));
        }
        if let Some(want_us) = row.reference_duration_us {
            let got = demux.duration().map(vaco_core::Duration::as_micros);
            let close = got.is_some_and(|g| (g - want_us).abs() <= 1);
            if !close {
                failures.push(format!(
                    "{}: duration {got:?} us != reference {want_us} us",
                    row.file
                ));
            }
        }

        // Drain packets: every fixture must produce at least one, and the
        // format must not error out before EOF.
        let mut packets = 0usize;
        loop {
            match demux.read_packet() {
                Ok(_) => packets += 1,
                Err(vaco_core::Error::Eof) => break,
                Err(e) => {
                    failures.push(format!("{}: read_packet failed: {e:?}", row.file));
                    break;
                }
            }
        }
        if packets == 0 {
            failures.push(format!("{}: produced zero packets", row.file));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The probe for every registered demuxer must not claim a plain text file.
#[test]
fn no_probe_claims_prose() {
    use vaco_format_misc_audio::{adx, amr, nistsphere, pvf, rawcodec, sbc, tta, wavpack};

    let text = ProbeData::new(b"The quick brown fox jumps over the lazy dog. Not media.");
    let probes: &[fn(&ProbeData<'_>) -> vaco_format_core::probe::ProbeScore] = &[
        adx::probe,
        amr::probe_amr,
        nistsphere::probe,
        pvf::probe,
        sbc::probe,
        tta::probe,
        wavpack::probe,
        rawcodec::probe_gsm,
        rawcodec::probe_sln,
        rawcodec::probe_dfpwm,
        rawcodec::probe_g722,
        rawcodec::probe_g726,
        rawcodec::probe_g726le,
        rawcodec::probe_g728,
        rawcodec::probe_g729,
        rawcodec::probe_aptx,
        rawcodec::probe_aptx_hd,
    ];
    for p in probes {
        assert_eq!(p(&text), vaco_format_core::probe::ProbeScore::NONE);
    }
}
