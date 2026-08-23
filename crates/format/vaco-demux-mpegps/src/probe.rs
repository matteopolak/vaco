//! Content detection for MPEG program streams.
//!
//! A program stream has no file magic beyond the pack start code, which is
//! also a legal three-byte sequence (`00 00 01`) inside plenty of other
//! formats' payloads. The signal that actually distinguishes it is a
//! *rhythm*: a file must open with a pack start code, and every other
//! `00 00 01`-prefixed run inside it is one of a small, closed set of
//! `stream_id` values (pack, system header, program end, PSM, padding, a
//! private stream, or an elementary-stream id in `0xC0..=0xEF`). Plain data
//! containing three literal zero bytes followed by a one essentially never
//! keeps producing plausible ids at every subsequent occurrence.
//!
//! # What is and is not measured
//!
//! The byte-level pack/system-header parsing this crate relies on elsewhere
//! is verified against real `ffmpeg -f mpeg`/`-f vob` output (see
//! `pack.rs`). The specific [`ProbeScore`] *values* below are this crate's
//! own content heuristic, not a transcription of the reference's internal
//! probe weights — those are an implementation detail of a specific binary,
//! not a fact about the format. What is guaranteed is the *ordering*: more
//! plausible start codes in a row scores higher, and a single ambiguous
//! match scores under [`ProbeScore::RETRY`] so a container with genuinely
//! stronger evidence wins the format's probe retry loop.

use vaco_format_core::{ProbeData, ProbeScore};

use crate::pack::PACK_START_CODE;
use crate::pes::{
    SID_DSMCC, SID_ECM, SID_EMM, SID_H222_TYPE_E, SID_PADDING, SID_PRIVATE_1, SID_PRIVATE_2,
    SID_PROGRAM_STREAM_DIRECTORY, SID_PROGRAM_STREAM_MAP,
};

/// `MPEG_program_end_code`'s `stream_id` byte.
const SID_PROGRAM_END: u8 = 0xB9;
/// `pack_start_code`'s `stream_id` byte.
const SID_PACK: u8 = 0xBA;
/// `system_header_start_code`'s `stream_id` byte.
const SID_SYSTEM_HEADER: u8 = 0xBB;

/// Plausible start codes found gives [`ProbeScore::MAX`].
pub const STRONG_RUN: u32 = 4;
/// How far into the buffer to scan.
const SCAN_LIMIT: usize = 4096;

/// Whether `id` is a `stream_id` a program stream can legally use.
const fn is_plausible_stream_id(id: u8) -> bool {
    matches!(
        id,
        SID_PROGRAM_END
            | SID_PACK
            | SID_SYSTEM_HEADER
            | SID_PROGRAM_STREAM_MAP
            | SID_PRIVATE_1
            | SID_PADDING
            | SID_PRIVATE_2
            | SID_ECM
            | SID_EMM
            | SID_DSMCC
            | SID_H222_TYPE_E
            | SID_PROGRAM_STREAM_DIRECTORY
    ) || matches!(id, 0xC0..=0xEF)
}

/// Score a buffer as MPEG program stream content.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    const EXT: &[&str] = &["mpg", "mpeg", "m2p", "vob", "vcd"];
    if !data.starts_with(&PACK_START_CODE) {
        return ProbeScore::from_extension(data, EXT);
    }
    let limit = data.len().min(SCAN_LIMIT);
    let mut pos = 0usize;
    let mut plausible = 0u32;
    let mut implausible = 0u32;
    while pos + 4 <= limit {
        if data.matches_at(pos, &[0x00, 0x00, 0x01]) {
            let Some(id) = data.get(pos + 3) else {
                break;
            };
            if is_plausible_stream_id(id) {
                plausible += 1;
            } else {
                implausible += 1;
            }
            pos += 4;
        } else {
            pos += 1;
        }
        if plausible >= STRONG_RUN && implausible == 0 {
            break;
        }
    }
    if implausible > plausible {
        return ProbeScore::from_extension(data, EXT);
    }
    match plausible {
        0 => ProbeScore::from_extension(data, EXT),
        1..=3 => ProbeScore::weak(20 + plausible as u8 * 5),
        _ => ProbeScore::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_ps() -> Vec<u8> {
        // pack, system header (short), video PES x2 — enough plausible start
        // codes to clear STRONG_RUN.
        let mut v = vec![
            0x00, 0x00, 0x01, 0xba, 0x21, 0x00, 0x01, 0x00, 0x01, 0xa1, 0xa1, 0xad,
        ];
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xbb, 0x00, 0x00]);
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x0f]);
        v.extend_from_slice(b"payload");
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xc0, 0x00, 0x00, 0x0f]);
        v.extend_from_slice(b"audio");
        v
    }

    #[test]
    fn a_synthetic_program_stream_scores_max() {
        let buf = synthetic_ps();
        let data = ProbeData::new(&buf);
        assert_eq!(probe(&data), ProbeScore::MAX);
    }

    #[test]
    fn a_bare_pack_header_with_nothing_else_scores_weak() {
        let buf = [
            0x00, 0x00, 0x01, 0xba, 0x21, 0x00, 0x01, 0x00, 0x01, 0xa1, 0xa1, 0xad,
        ];
        let data = ProbeData::new(&buf);
        let score = probe(&data);
        assert!(score.needs_retry(), "{score:?} should be below RETRY");
    }

    #[test]
    fn random_bytes_score_none() {
        let buf = [0u8; 32];
        let data = ProbeData::new(&buf).with_filename("clip.bin");
        assert!(probe(&data).is_none());
    }

    #[test]
    fn extension_alone_is_a_weak_hint() {
        let buf = [0u8; 32];
        let data = ProbeData::new(&buf).with_filename("clip.vob");
        assert!(!probe(&data).is_none());
    }

    #[test]
    fn a_pack_start_code_followed_by_garbage_ids_is_not_max() {
        let mut buf = vec![
            0x00, 0x00, 0x01, 0xba, 0x21, 0x00, 0x01, 0x00, 0x01, 0xa1, 0xa1, 0xad,
        ];
        // Three more "00 00 01 XX" runs with implausible ids.
        for _ in 0..3 {
            buf.extend_from_slice(&[0x00, 0x00, 0x01, 0x05, 0xaa, 0xbb]);
        }
        let data = ProbeData::new(&buf);
        assert_ne!(probe(&data), ProbeScore::MAX);
    }
}
