//! The gapless trim an MP4 states, and the two independent places it comes
//! from.
//!
//! Both were measured against `ffprobe 9.0.1 -show_packets` on
//! `ffmpeg -c:a aac` files at 22.05/32/44.1/48 kHz, mono and stereo. Every
//! one reported `skip_samples=1024` on the first packet and a
//! `discard_padding` on the last of 956/512/888/256 respectively — and this
//! demuxer emitted the leading half and a hardcoded `end: 0` for the
//! trailing one, so an AAC file decoded 1024 samples of encoder priming and
//! its whole trailing padding into the output.
//!
//! The trailing half is *not* an edit-list fact. On the 48 kHz file the
//! `elst` is `[(96000, 1024, 1.0)]` and the last sample's presentation ends
//! at exactly 96000 — the padding is stated instead by that sample's `stts`
//! delta being 768 against the track's 1024-sample frames.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod common;

use common::{MDAT_PAYLOAD, fixture};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::{Demuxer, FormatOptions, NoParsers};
use vaco_format_isom::build::{StblSpec, TrackSpec};
use vaco_io::{MediaSource, MemorySource};
use vaco_packet::{PacketSideData, PacketSideDataKind};

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn stsd(payload_hex: &str) -> Vec<u8> {
    let payload = from_hex(payload_hex);
    let mut boxed = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    boxed.extend_from_slice(b"stsd");
    boxed.extend_from_slice(&payload);
    boxed
}

/// Real `stsd` payload bytes from `ffmpeg -c:a aac -b:a 96k` at 48 kHz mono:
/// one `mp4a` entry wrapping an `esds` whose `AudioSpecificConfig` is
/// AAC-LC. Used verbatim so the demuxer resolves `CodecId::Aac` — the
/// trailing trim is deliberately gated on the codec having a fixed frame
/// size, and a made-up sample entry would not exercise that gate.
const AAC_STSD: &str = "00000000000000010000005a6d703461000000000000000100000000000000000001001000000000bb80000000000036657364730000000003808080250001000480808017401500000000017b1400017b140580808005118856e500068080800102";

/// Real `stsd` payload bytes from `ffmpeg -c:a alac`, whose
/// `ALACSpecificConfig` states `frame_length = 4096`. ALAC has no fixed
/// frame size as far as `CodecId::fixed_frame_size` is concerned, because
/// its final frame is genuinely short and says so in its own frame header.
const ALAC_STSD: &str = "000000000000000100000048616c6163000000000000000100000000000000000001001000000000ac44000000000024616c616300000000000010000010280a0e01000000002004000ac4400000ac44";

/// `frames` full-length samples of `frame` ticks, then one final sample of
/// `last` ticks — the shape `ffmpeg`'s MP4 muxer writes for an audio track
/// whose encoder padded the tail.
fn audio_track(
    stsd_hex: &str,
    frame: u32,
    frames: u32,
    last: u32,
    elst: Vec<(u64, i64, i16)>,
) -> TrackSpec {
    let count = frames as usize + 1;
    TrackSpec {
        handler: *b"soun",
        timescale: 48_000,
        media_duration: u64::from(frame * frames + last),
        elst,
        stbl: StblSpec {
            stsd_box: Some(stsd(stsd_hex)),
            stts: vec![(frames, frame), (1, last)],
            stsc: vec![(1, count as u32, 1)],
            stsz: vec![16; count],
            stco: vec![u32::try_from(MDAT_PAYLOAD).unwrap()],
            has_stss: false,
            ..StblSpec::default()
        },
        ..TrackSpec::default()
    }
}

fn trims(data: Vec<u8>) -> Vec<(usize, u32, u32)> {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data));
    let mut demux = Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options::default(),
    )
    .expect("open");
    let mut out = Vec::new();
    let mut i = 0;
    while let Ok(p) = demux.read_packet() {
        if let Some(PacketSideData::SkipSamples { start, end, .. }) =
            p.side_data(PacketSideDataKind::SkipSamples)
        {
            out.push((i, *start, *end));
        }
        i += 1;
    }
    out
}

/// The shape of every `ffmpeg -c:a aac` MP4: `elst.media_time` states the
/// encoder priming, a short final `stts` delta states the padding.
#[test]
fn aac_gets_the_leading_priming_and_the_trailing_padding() {
    let track = audio_track(AAC_STSD, 1024, 4, 768, vec![(4096, 1024, 1)]);
    let data = fixture(48_000, 4096, &[track], &[0u8; 16 * 5]);
    assert_eq!(
        trims(data),
        vec![(0, 1024, 0), (4, 0, 256)],
        "1024 in from the edit list, 1024-768=256 off the end from the short final stts"
    );
}

/// The regression that made this gate necessary. ALAC's last frame really is
/// 2184 samples long and its own frame header says so, so trimming the
/// difference against the track's 4096-sample frames deleted 1912 real
/// samples from the end of every `ffmpeg -c:a alac` file — measured as a
/// 1912-sample shortfall against `ffmpeg`'s own decode, where `ffprobe`
/// reports no `discard_padding` at all.
#[test]
fn alac_gets_no_trailing_trim_from_a_short_final_stts() {
    let track = audio_track(ALAC_STSD, 4096, 4, 2184, Vec::new());
    let data = fixture(48_000, 4096, &[track], &[0u8; 16 * 5]);
    assert_eq!(
        trims(data),
        Vec::new(),
        "a codec whose frames are not a fixed size states its own last-frame length"
    );
}

/// A `segment_duration` that stops before the media does is the other way an
/// MP4 states a tail trim, and it must work with no short final `stts` at
/// all: five full 1024-sample frames, an edit that presents only 4096 ticks
/// of them after the 1024-tick priming.
#[test]
fn an_edit_list_that_ends_early_trims_the_tail_on_its_own() {
    let track = audio_track(AAC_STSD, 1024, 5, 1024, vec![(4096, 1024, 1)]);
    let data = fixture(48_000, 4096, &[track], &[0u8; 16 * 6]);
    let got = trims(data);
    assert_eq!(got.first().copied(), Some((0, 1024, 0)));
    assert_eq!(
        got.last().copied(),
        Some((5, 0, 1024)),
        "the final frame lies wholly past the edit's end, so all 1024 of it go"
    );
}

/// No `elst` and no short final sample: nothing to trim, and in particular
/// no side data at all rather than a zero-valued record.
#[test]
fn a_track_with_no_edit_and_no_short_tail_carries_no_trim() {
    let track = audio_track(AAC_STSD, 1024, 4, 1024, Vec::new());
    let data = fixture(48_000, 5120, &[track], &[0u8; 16 * 5]);
    assert_eq!(trims(data), Vec::new());
}
