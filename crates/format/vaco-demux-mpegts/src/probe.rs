//! Content detection.
//!
//! MPEG-TS has no magic number. It has a *rhythm*: a `0x47` every 188 bytes
//! (or 192, or 204), forever. So the probe counts consecutive strided sync
//! bytes and scores the longest run it finds.
//!
//! # The scores are measured, not derived
//!
//! Probed against ffprobe 8.1 by truncating a muxed `.ts` file to `N` packets
//! and reading `format.probe_score`:
//!
//! | packets present | reference `probe_score` |
//! |---|---|
//! | 1-2 | file rejected outright (no streams) |
//! | 3-10 | **2** |
//! | 11 and up | **50** |
//!
//! Two things follow, and both matter.
//!
//! **`ProbeScore`'s convention table cannot express 50.** `repeating(n)` is
//! `min(100, 25 + 8n)`, which takes the values 33, 41, 49, 57 — it steps over
//! the reference's answer. `EXTENSION` happens to equal 50 but means something
//! else entirely. So [`TS_SCORE_STRONG`] is declared here as a measured
//! constant, and `vaco-format-core`'s table is reported as not covering the
//! self-synchronising case it was written for. See the docs file.
//!
//! **The low-confidence answer is 2, not 25.** That is below
//! `ProbeScore::RETRY`, so a short TS prefix does not ask the probe loop for
//! more data the way a weak guess normally would. Reproduced because
//! `probe_score` is printed and D6 makes it a conformance field.

use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_mpegts_tables::packet::{PacketStride, SYNC_BYTE};

/// The score a confidently-detected transport stream gets.
///
/// **Measured** against ffprobe 8.1, not drawn from `ProbeScore`'s convention
/// table — which has no value here, see the module docs.
pub const TS_SCORE_STRONG: ProbeScore = ProbeScore(50);

/// The score a short but plausible prefix gets. Also measured.
pub const TS_SCORE_WEAK: ProbeScore = ProbeScore(2);

/// Consecutive strided sync bytes needed for [`TS_SCORE_STRONG`].
///
/// Measured: ten packets score 2 and eleven score 50, so the threshold is a
/// run of eleven sync bytes.
pub const STRONG_RUN: u32 = 11;

/// Runs shorter than this are not evidence of anything: `0x47` occurs in
/// ordinary data about once per 256 bytes, so two in a row at one stride
/// happens by chance in a few kilobytes.
pub const MIN_RUN: u32 = 3;

/// The longest strided sync run in `data`, and the stride that produced it.
///
/// Bounded work: the outer scan stops at the first stride, and the inner count
/// stops at `STRONG_RUN` since nothing above it changes the answer. That makes
/// the probe linear in the buffer with a small constant, which matters because
/// it runs on every candidate format for every input.
#[must_use]
pub fn best_run(data: &ProbeData<'_>) -> Option<(PacketStride, usize, u32)> {
    let mut best: Option<(PacketStride, usize, u32)> = None;
    // A packet may start anywhere inside the first stride; past that, any real
    // transport stream would already have shown a sync byte.
    let limit = data.len().min(PacketStride::Rs.stride());
    for at in 0..=limit {
        for stride in PacketStride::ALL {
            let run = run_at(data, at, stride);
            if run >= MIN_RUN && best.is_none_or(|(_, _, b)| run > b) {
                best = Some((stride, at, run));
            }
        }
        if best.is_some_and(|(_, _, r)| r >= STRONG_RUN) {
            break;
        }
    }
    best
}

fn run_at(data: &ProbeData<'_>, at: usize, stride: PacketStride) -> u32 {
    let mut pos = at.saturating_add(stride.prefix());
    let mut n = 0u32;
    while n < STRONG_RUN {
        // `ProbeData::get` reports zero inside the padding window, so a run
        // can never be extended past the end of the real data.
        match data.get(pos) {
            Some(SYNC_BYTE) => n = n.saturating_add(1),
            _ => break,
        }
        pos = match pos.checked_add(stride.stride()) {
            Some(p) => p,
            None => break,
        };
    }
    n
}

/// The `DemuxerDesc::probe` function.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    match best_run(data) {
        Some((_, _, run)) if run >= STRONG_RUN => TS_SCORE_STRONG,
        Some(_) => TS_SCORE_WEAK,
        None => ProbeScore::NONE,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn ts_bytes(n: usize, stride: PacketStride) -> Vec<u8> {
        let mut v = vec![0u8; stride.stride() * n];
        for i in 0..n {
            v[i * stride.stride() + stride.prefix()] = SYNC_BYTE;
        }
        v
    }

    #[test]
    fn eleven_packets_score_fifty_and_ten_score_two() {
        // The measured step in the reference, reproduced exactly.
        let ten = ts_bytes(10, PacketStride::Ts);
        let eleven = ts_bytes(11, PacketStride::Ts);
        assert_eq!(probe(&ProbeData::new(&ten)), TS_SCORE_WEAK);
        assert_eq!(probe(&ProbeData::new(&eleven)), TS_SCORE_STRONG);
    }

    #[test]
    fn an_empty_or_tiny_buffer_scores_nothing() {
        assert_eq!(probe(&ProbeData::new(&[])), ProbeScore::NONE);
        assert_eq!(probe(&ProbeData::new(&[SYNC_BYTE])), ProbeScore::NONE);
        assert_eq!(
            probe(&ProbeData::new(&ts_bytes(2, PacketStride::Ts))),
            ProbeScore::NONE
        );
    }

    #[test]
    fn m2ts_and_rs_strides_are_detected() {
        for stride in [PacketStride::M2ts, PacketStride::Rs] {
            let buf = ts_bytes(12, stride);
            assert_eq!(probe(&ProbeData::new(&buf)), TS_SCORE_STRONG);
            assert_eq!(best_run(&ProbeData::new(&buf)).unwrap().0, stride);
        }
    }

    #[test]
    fn a_stream_starting_mid_packet_is_found() {
        let mut buf = vec![0x11u8; 77];
        buf.extend_from_slice(&ts_bytes(12, PacketStride::Ts));
        let (stride, at, _) = best_run(&ProbeData::new(&buf)).unwrap();
        assert_eq!(stride, PacketStride::Ts);
        assert_eq!(at, 77);
        assert_eq!(probe(&ProbeData::new(&buf)), TS_SCORE_STRONG);
    }

    #[test]
    fn a_broken_run_does_not_reach_the_strong_score() {
        let mut buf = ts_bytes(30, PacketStride::Ts);
        // Break the rhythm every five packets.
        for i in (5..30).step_by(5) {
            buf[i * 188] = 0x00;
        }
        assert_eq!(probe(&ProbeData::new(&buf)), TS_SCORE_WEAK);
    }

    #[test]
    fn ordinary_data_does_not_score() {
        // A megabyte of pseudo-random bytes must not look like a transport
        // stream. Eleven strided hits by chance has probability about
        // 2^-88 per offset.
        let buf: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        assert_ne!(probe(&ProbeData::new(&buf)), TS_SCORE_STRONG);
    }

    #[test]
    fn a_buffer_of_all_sync_bytes_scores_strong() {
        // Degenerate but legal-looking; the reference accepts it too, and the
        // point of the test is that the probe terminates on it.
        let buf = vec![SYNC_BYTE; 4096];
        assert_eq!(probe(&ProbeData::new(&buf)), TS_SCORE_STRONG);
    }

    #[test]
    fn the_probe_only_returns_values_from_its_own_table() {
        // The convention every format crate is asked to assert.
        for len in [0usize, 1, 188, 189, 2048, 4096] {
            let buf = vec![SYNC_BYTE; len];
            let s = probe(&ProbeData::new(&buf));
            assert!(
                s == ProbeScore::NONE || s == TS_SCORE_WEAK || s == TS_SCORE_STRONG,
                "unexpected score {s:?} at len {len}"
            );
        }
    }
}
