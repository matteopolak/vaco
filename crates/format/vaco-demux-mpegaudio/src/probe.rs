//! Detecting an MPEG audio elementary stream.
//!
//! An MPEG audio frame's sync is eleven set bits, which matches a huge amount
//! of non-audio data by chance (a JPEG `APPn` marker is `0xFFEx`, which alone
//! passes the sync test). So this never scores a single matched header: it
//! chains frames at the exact byte stride each header's own bit rate and
//! sample rate imply, and requires several of them to agree, before it will
//! claim anything. Detecting is deliberately stricter than demuxing, which
//! recovers what it can from a stream this would refuse to score.

use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_id3::Id3v2Header;
use vaco_format_mpegaudio::MpegAudioHeader;

/// Consecutive chained frames needed before this claims the reference's own
/// measured score for a real file (`ffprobe -show_format` on
/// `ffmpeg -c:a libmp3lame` output reports `probe_score=51`, with or without
/// a leading `ID3v2` tag).
const STRONG_RUN: u32 = 4;
/// Fewer than this many chained frames is indistinguishable from chance on
/// eleven sync bits and scores nothing.
const MIN_RUN: u32 = 2;
const SCORE_STRONG: ProbeScore = ProbeScore(51);
const SCORE_WEAK: ProbeScore = ProbeScore(24);

/// Bytes an `ID3v2` header could plausibly claim to skip, bounding the
/// distance this walks past a hostile `size` field before giving up on
/// finding a frame sync at all.
const MAX_ID3_SKIP: usize = 16 * 1024 * 1024;

#[must_use]
pub(crate) fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let start = Id3v2Header::parse(data.buf)
        .ok()
        .map(|h| h.total_len())
        .filter(|&len| len < MAX_ID3_SKIP as u64)
        .map_or(0, |len| len as usize);
    match chained_run(data, start) {
        run if run >= STRONG_RUN => SCORE_STRONG,
        run if run >= MIN_RUN => SCORE_WEAK,
        _ => ProbeScore::NONE,
    }
}

/// How many consecutive frames starting at `at` share a version/layer/sample
/// rate and sit at the exact offsets implied by either their fixed bit rate or
/// a confirmed free-format stride.
fn chained_run(data: &ProbeData<'_>, at: usize) -> u32 {
    let Some(first) = read_header(data, at) else {
        return 0;
    };
    if first.frame_len().is_none() {
        return free_format_chained_run(data, at, first);
    }
    let mut pos = at;
    let mut run = 0u32;
    while let Some(h) = read_header(data, pos) {
        if !same_stream(first, h) {
            break;
        }
        run = run.saturating_add(1);
        let Some(len) = h.frame_len() else {
            break;
        };
        if run >= STRONG_RUN {
            break;
        }
        pos = pos.saturating_add(len as usize);
    }
    run
}

fn same_stream(first: MpegAudioHeader, candidate: MpegAudioHeader) -> bool {
    candidate.version == first.version
        && candidate.layer == first.layer
        && candidate.sample_rate_hz() == first.sample_rate_hz()
}

/// Derive a free-format frame's padding-exclusive base length from a later
/// matching header, then require that length to land on at least one further
/// header. The third header is what distinguishes a stride from matching
/// header-shaped bytes inside the first frame's compressed payload.
fn free_format_chained_run(data: &ProbeData<'_>, at: usize, first: MpegAudioHeader) -> u32 {
    let mut candidate_at = at.saturating_add(MpegAudioHeader::LEN);
    let candidate_end = data.len().saturating_sub(MpegAudioHeader::LEN);
    let mut best = 1u32;
    while candidate_at <= candidate_end {
        if let Some(candidate) = read_header(data, candidate_at)
            && same_stream(first, candidate)
            && candidate.bitrate_index == 0
            && let Some(distance) = candidate_at.checked_sub(at)
            && let Some(base_len) = distance.checked_sub(usize::from(first.padding))
            && base_len >= MpegAudioHeader::LEN
        {
            let run = free_format_run_for_base(data, at, first, base_len);
            best = best.max(run);
            if best >= STRONG_RUN {
                return best;
            }
        }
        candidate_at = candidate_at.saturating_add(1);
    }

    if best >= 3 { best } else { 1 }
}

fn free_format_run_for_base(
    data: &ProbeData<'_>,
    at: usize,
    first: MpegAudioHeader,
    base_len: usize,
) -> u32 {
    let mut pos = at;
    let mut run = 0u32;
    while run < STRONG_RUN {
        let Some(header) = read_header(data, pos) else {
            break;
        };
        if !same_stream(first, header) || header.bitrate_index != 0 {
            break;
        }
        run = run.saturating_add(1);
        let Some(frame_len) = base_len.checked_add(usize::from(header.padding)) else {
            break;
        };
        let Some(next_pos) = pos.checked_add(frame_len) else {
            break;
        };
        if next_pos <= pos {
            break;
        }
        pos = next_pos;
    }
    run
}

fn read_header(data: &ProbeData<'_>, at: usize) -> Option<MpegAudioHeader> {
    let word = data.rb32(at)?;
    MpegAudioHeader::parse(word)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn cbr_frame(bytes_len: usize) -> Vec<u8> {
        let mut frame = vec![0u8; bytes_len];
        frame[0] = 0xFF;
        frame[1] = 0xFB;
        frame[2] = 0x90;
        frame[3] = 0x00;
        frame
    }

    fn free_format_frame(base_len: usize, padding: bool) -> Vec<u8> {
        let mut header = MpegAudioHeader::parse(0xFFFB_9000).expect("valid header");
        header.bitrate_index = 0;
        header.padding = padding;
        let mut frame = vec![0u8; base_len + usize::from(padding)];
        frame[..MpegAudioHeader::LEN].copy_from_slice(&header.to_bytes());
        frame
    }

    #[test]
    fn four_chained_frames_score_the_measured_reference_value() {
        let header = MpegAudioHeader::parse(0xFFFB_9000).expect("valid header");
        let len = header.frame_len().expect("cbr frame has a length") as usize;
        let mut data = Vec::new();
        for _ in 0..6 {
            data.extend_from_slice(&cbr_frame(len));
        }
        assert_eq!(probe(&ProbeData::new(&data)), SCORE_STRONG);
    }

    #[test]
    fn four_chained_free_format_frames_score_the_measured_reference_value() {
        let mut data = Vec::new();
        for padding in [false, true, false, true, false, false] {
            data.extend_from_slice(&free_format_frame(417, padding));
        }
        assert_eq!(probe(&ProbeData::new(&data)), SCORE_STRONG);
    }

    #[test]
    fn a_matching_free_format_header_inside_payload_does_not_set_the_stride() {
        let mut first = free_format_frame(417, false);
        let header = free_format_frame(417, false);
        first[100..100 + MpegAudioHeader::LEN].copy_from_slice(&header[..MpegAudioHeader::LEN]);

        let mut data = first;
        for _ in 0..5 {
            data.extend_from_slice(&free_format_frame(417, false));
        }
        assert_eq!(probe(&ProbeData::new(&data)), SCORE_STRONG);
    }

    #[test]
    fn one_unconfirmed_free_format_stride_scores_nothing() {
        let mut data = free_format_frame(417, false);
        let next = free_format_frame(417, false);
        data.extend_from_slice(&next);
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::NONE);
    }

    #[test]
    fn a_leading_id3v2_tag_is_skipped_before_scanning() {
        let header = MpegAudioHeader::parse(0xFFFB_9000).expect("valid header");
        let len = header.frame_len().expect("cbr frame has a length") as usize;
        let mut data = vec![0u8; 10];
        data[0..3].copy_from_slice(b"ID3");
        data[3] = 4;
        // A ten-byte tag: synchsafe size field is all zero.
        for _ in 0..6 {
            data.extend_from_slice(&cbr_frame(len));
        }
        assert_eq!(probe(&ProbeData::new(&data)), SCORE_STRONG);
    }

    #[test]
    fn one_incidental_sync_scores_nothing() {
        let mut data = vec![0u8; 512];
        data[100] = 0xFF;
        data[101] = 0xE0;
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::NONE);
    }

    #[test]
    fn prose_scores_nothing() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(200);
        assert_eq!(probe(&ProbeData::new(text.as_bytes())), ProbeScore::NONE);
    }

    #[test]
    fn a_run_of_jpeg_app_markers_does_not_chain() {
        // `0xFFEx` passes the sync test on its own; a real JPEG's marker
        // segments have arbitrary lengths unrelated to any mp3 bit rate, so
        // consecutive markers should not land on further valid headers.
        let mut data = Vec::new();
        for i in 0..8u8 {
            data.push(0xFF);
            data.push(0xE0 | (i & 0x0F));
            data.push(0x00);
            data.push(0x10);
            data.extend_from_slice(&[b'E', b'x', b'i', b'f', 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        assert!(probe(&ProbeData::new(&data)) < SCORE_STRONG);
    }

    #[test]
    fn ac3_sync_never_matches() {
        let mut data = vec![0u8; 64];
        data[0] = 0x0B;
        data[1] = 0x77;
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::NONE);
    }

    #[test]
    fn mpegts_sync_never_matches() {
        let mut data = vec![0u8; 188 * 4];
        for pkt in data.chunks_mut(188) {
            pkt[0] = 0x47;
        }
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::NONE);
    }

    #[test]
    fn empty_input_never_panics() {
        assert_eq!(probe(&ProbeData::new(&[])), ProbeScore::NONE);
    }
}
