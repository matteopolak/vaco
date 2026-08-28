//! Per-track state: the sample entry, the accumulated samples, and the
//! compression from "one record per sample/chunk" into the run-length tables
//! `stts`/`ctts`/`stsc` actually store.
//!
//! Nothing here writes a box — that is [`vaco_format_isom::writer`]'s job.
//! This module only decides *what the numbers are*.

use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Rational};
use vaco_format_isom::fourcc::FourCc;

use crate::entry::BuiltEntry;

/// One written sample, in the track's own (post-rescale) time base.
#[derive(Debug, Clone, Copy)]
pub struct SampleRecord {
    pub offset: u64,
    pub size: u32,
    pub dts: i64,
    pub cts_offset: i32,
    pub is_sync: bool,
}

/// One finished chunk: its first sample's file offset and how many samples it holds.
#[derive(Debug, Clone, Copy)]
pub struct ChunkRecord {
    pub offset: u64,
    pub sample_count: u32,
}

/// Everything this crate tracks for one stream, from `add_stream` onward.
#[derive(Debug, Clone)]
pub struct TrackState {
    pub track_id: u32,
    pub media: MediaType,
    pub handler: FourCc,
    pub timescale: u32,
    /// `tkhd.volume`: `0x0100` for audio, `0` for anything else.
    pub volume: i16,
    /// Display width/height, 16.16 fixed point; zero for audio.
    pub width: u32,
    pub height: u32,
    pub language: u16,
    pub entry: BuiltEntry,
    pub params: CodecParameters,
    pub samples: Vec<SampleRecord>,
    pub chunks: Vec<ChunkRecord>,
    /// Duration of the last sample, carried forward when a packet's own
    /// duration is unknown — `stts`'s final run needs *a* value, and
    /// repeating the previous delta is what every writer this crate could
    /// probe does when a demuxer's last packet gives none.
    pub last_duration_hint: u32,
    /// Set the first time [`crate::mux::MovMuxer::check_bitstream`] answers
    /// `Insert` for this track, so the second ask in the same chain-building
    /// loop answers `Keep` instead of the same name again. Without this a
    /// `GLOBALHEADER` track with empty extradata asks for `extract_extradata`
    /// forever, since nothing about `CodecParameters` changes between asks —
    /// see `vaco-mux-avi::StreamOut::bsf_decided` for the identical fix and
    /// the `MuxWriter` doc this is answering.
    pub bsf_decided: bool,
}

impl TrackState {
    #[must_use]
    pub fn new(track_id: u32, timescale: u32, entry: BuiltEntry, params: CodecParameters) -> Self {
        let media = entry.media;
        let handler = match media {
            MediaType::Audio => FourCc::new(b"soun"),
            _ => FourCc::new(b"vide"),
        };
        let (width, height) = params
            .video
            .as_ref()
            .map_or((0, 0), |v| ((v.width) << 16, (v.height) << 16));
        let volume = if media == MediaType::Audio { 0x0100 } else { 0 };
        Self {
            track_id,
            media,
            handler,
            timescale,
            volume,
            width,
            height,
            language: vaco_format_isom::lang::PACKED_UND,
            entry,
            params,
            samples: Vec::new(),
            chunks: Vec::new(),
            last_duration_hint: 0,
            bsf_decided: false,
        }
    }

    /// The track's own time base, `1 / timescale`.
    #[must_use]
    pub fn time_base(&self) -> Rational {
        Rational::new(1, i32::try_from(self.timescale.max(1)).unwrap_or(i32::MAX))
    }

    /// Whether every recorded sample is a sync sample — `stss` is omitted
    /// entirely in that case, per §8.6.2's "not present means all sync samples".
    #[must_use]
    pub fn all_sync(&self) -> bool {
        self.samples.iter().all(|s| s.is_sync)
    }

    /// Media duration in this track's own timescale: the last sample's `dts`
    /// plus its duration, taken from the run table so the two never disagree.
    #[must_use]
    pub fn media_duration(&self) -> u64 {
        let runs = self.stts_runs();
        let mut total: u64 = 0;
        for (count, delta) in runs {
            total = total.saturating_add(u64::from(count).saturating_mul(u64::from(delta)));
        }
        total
    }

    /// `stts`: `(sample_count, sample_delta)` runs, compressed from
    /// consecutive equal deltas.
    #[must_use]
    pub fn stts_runs(&self) -> Vec<(u32, u32)> {
        if self.samples.is_empty() {
            return Vec::new();
        }
        let mut deltas: Vec<u32> = self
            .samples
            .windows(2)
            .map(|w| {
                let (Some(a), Some(b)) = (w.first(), w.get(1)) else {
                    return 0;
                };
                u32::try_from(b.dts.saturating_sub(a.dts).max(0)).unwrap_or(0)
            })
            .collect();
        deltas.push(self.last_duration_hint);
        compress_runs(&deltas)
    }

    /// `ctts`: `(sample_count, offset)` runs. Empty when every offset is
    /// zero, matching what a demuxer already treats as "no `ctts` box".
    #[must_use]
    pub fn ctts_runs(&self) -> Vec<(u32, i32)> {
        if self.samples.iter().all(|s| s.cts_offset == 0) {
            return Vec::new();
        }
        let offsets: Vec<i32> = self.samples.iter().map(|s| s.cts_offset).collect();
        compress_runs(&offsets)
    }

    /// `stss`: one-based sync-sample numbers, or `None` when every sample is
    /// sync (so the caller omits the box).
    #[must_use]
    pub fn stss_list(&self) -> Option<Vec<u32>> {
        if self.all_sync() {
            return None;
        }
        Some(
            self.samples
                .iter()
                .enumerate()
                .filter(|(_, s)| s.is_sync)
                .map(|(i, _)| u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1))
                .collect(),
        )
    }

    /// `stsz`: one size per sample, in sample order.
    #[must_use]
    pub fn stsz_list(&self) -> Vec<u32> {
        self.samples.iter().map(|s| s.size).collect()
    }

    /// `stco`/`co64`: one file offset per chunk.
    #[must_use]
    pub fn chunk_offset_list(&self) -> Vec<u64> {
        self.chunks.iter().map(|c| c.offset).collect()
    }

    /// The `elst`/`media_time` value this crate can actually produce: the
    /// decode-order-first sample's composition offset (`cts_offset`, clamped
    /// to non-negative), *not* `-dts` of that sample.
    ///
    /// The reference derives `media_time` from the encoder's original,
    /// possibly-negative `dts` (measured, CONFORMANCE-FINDINGS 49: an AAC
    /// track with `pts == dts` throughout, so every `cts_offset` is `0`,
    /// still gets a nonzero `media_time` when its own first `dts` is
    /// negative — a fact `cts_offset` alone cannot see). This crate cannot
    /// reproduce that in general: by the time a packet reaches
    /// [`crate::mux::MovMuxer::write_packet`], whatever normalized its `dts`
    /// upstream (outside this crate — the pipeline the CLI drives, not
    /// `vaco-mux-mp4`) has already shifted decode-order-first `dts` to `0`,
    /// discarding the original negative baseline `-dts` would need.
    /// `cts_offset` is the one piece of that shift's *effect* survives the
    /// normalization unchanged, since a uniform shift to both `pts` and
    /// `dts` cancels out of their difference — and it happens to equal the
    /// true `media_time` exactly whenever the encoder's original
    /// presentation actually starts at `pts == 0`, which every measured case
    /// except encoder-priming audio does. See `presented_duration`'s docs
    /// for the other half of the same gap.
    #[must_use]
    pub fn media_time(&self) -> u32 {
        let v = self.samples.first().map_or(0, |s| s.cts_offset).max(0);
        u32::try_from(v).unwrap_or(u32::MAX)
    }

    /// [`TrackState::media_duration`] minus however far the decode-order-
    /// first sample's `pts` (`dts + cts_offset`) sits below zero — `0`
    /// whenever [`TrackState::media_time`]'s normalized `dts` starts at `0`
    /// and `cts_offset` is non-negative there, which is every case this
    /// crate can currently observe (see that method's docs on why the
    /// upstream `dts` normalization makes this the effective default).
    /// Measured (CONFORMANCE-FINDINGS 49): a reordered video track's
    /// `elst.segment_duration`/`tkhd`/`mvhd` state its **full**, un-adjusted
    /// duration — the reorder delay is not lost time, only reordered time —
    /// while the reference's own encoder-priming audio case (not
    /// reproducible here, see `media_time`) loses exactly its priming delay.
    /// Subtracting `media_time` unconditionally, rather than this
    /// pts-below-zero check, was tried and measured wrong: it shrank the
    /// reordered video track's duration by its `dts` lead-in even though
    /// nothing was actually cut from it.
    #[must_use]
    pub fn presented_duration(&self) -> u64 {
        let pts0 = self
            .samples
            .first()
            .map_or(0, |s| s.dts.saturating_add(i64::from(s.cts_offset)));
        let lead = u64::try_from(pts0.saturating_neg().max(0)).unwrap_or(0);
        self.media_duration().saturating_sub(lead)
    }

    /// `stsc`: `(first_chunk, samples_per_chunk, sample_description_index)`
    /// runs, one-based chunk numbers, compressed from consecutive equal
    /// per-chunk sample counts. `sample_description_index` is always `1` —
    /// this crate never writes a second `stsd` entry for one track.
    #[must_use]
    pub fn stsc_runs(&self) -> Vec<(u32, u32, u32)> {
        let mut out = Vec::new();
        let mut last_count: Option<u32> = None;
        for (i, c) in self.chunks.iter().enumerate() {
            if last_count != Some(c.sample_count) {
                out.push((
                    u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
                    c.sample_count,
                    1u32,
                ));
                last_count = Some(c.sample_count);
            }
        }
        out
    }
}

/// Run-length compress a sequence of values that support equality.
fn compress_runs<T: Copy + PartialEq>(values: &[T]) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for &v in values {
        if let Some(last) = out.last_mut()
            && last.1 == v
        {
            last.0 = last.0.saturating_add(1);
        } else {
            out.push((1, v));
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::CodecId;
    use vaco_core::MediaType;

    fn entry() -> BuiltEntry {
        BuiltEntry {
            bytes: vec![0u8; 4],
            media: MediaType::Video,
        }
    }

    fn params() -> CodecParameters {
        CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::H264),
            ..CodecParameters::default()
        }
    }

    fn push(track: &mut TrackState, offset: u64, size: u32, dts: i64, cts: i32, sync: bool) {
        track.samples.push(SampleRecord {
            offset,
            size,
            dts,
            cts_offset: cts,
            is_sync: sync,
        });
    }

    #[test]
    fn stts_compresses_equal_deltas_and_repeats_the_last_one() {
        let mut t = TrackState::new(1, 1000, entry(), params());
        push(&mut t, 0, 10, 0, 0, true);
        push(&mut t, 10, 10, 100, 0, false);
        push(&mut t, 20, 10, 200, 0, false);
        push(&mut t, 30, 10, 350, 0, false); // delta 150, breaks the run
        t.last_duration_hint = 150;
        assert_eq!(t.stts_runs(), vec![(2, 100), (2, 150)]);
        assert_eq!(t.media_duration(), 2 * 100 + 2 * 150);
    }

    #[test]
    fn ctts_is_empty_when_every_offset_is_zero() {
        let mut t = TrackState::new(1, 1000, entry(), params());
        push(&mut t, 0, 10, 0, 0, true);
        push(&mut t, 10, 10, 100, 0, false);
        assert!(t.ctts_runs().is_empty());

        push(&mut t, 20, 10, 200, 33, false);
        assert_eq!(t.ctts_runs(), vec![(2, 0), (1, 33)]);
    }

    #[test]
    fn stss_is_none_when_every_sample_is_sync() {
        let mut t = TrackState::new(1, 1000, entry(), params());
        push(&mut t, 0, 10, 0, 0, true);
        push(&mut t, 10, 10, 100, 0, true);
        assert!(t.stss_list().is_none());

        push(&mut t, 20, 10, 200, 0, false);
        assert_eq!(t.stss_list(), Some(vec![1, 2]));
    }

    #[test]
    fn stsc_runs_compress_consecutive_equal_chunk_sizes() {
        let mut t = TrackState::new(1, 1000, entry(), params());
        t.chunks.push(ChunkRecord {
            offset: 0,
            sample_count: 3,
        });
        t.chunks.push(ChunkRecord {
            offset: 30,
            sample_count: 3,
        });
        t.chunks.push(ChunkRecord {
            offset: 60,
            sample_count: 1,
        });
        assert_eq!(t.stsc_runs(), vec![(1, 3, 1), (3, 1, 1)]);
        assert_eq!(t.chunk_offset_list(), vec![0, 30, 60]);
    }

    #[test]
    fn media_time_is_zero_with_no_reordering_or_priming() {
        let mut t = TrackState::new(1, 1000, entry(), params());
        push(&mut t, 0, 10, 0, 0, true);
        push(&mut t, 10, 10, 100, 0, false);
        t.last_duration_hint = 100;
        assert_eq!(t.media_time(), 0);
        assert_eq!(t.presented_duration(), t.media_duration());
    }

    /// A reordered video track: `dts` starts negative (decode-ahead delay)
    /// but the first sample's `pts` is still `0`, so nothing is actually
    /// missing from what gets presented — `media_time` is nonzero but
    /// `presented_duration` keeps the full span (CONFORMANCE-FINDINGS 49).
    #[test]
    fn reordering_sets_media_time_without_shrinking_the_presented_duration() {
        let mut t = TrackState::new(1, 12800, entry(), params());
        // First sample in decode order: pts = dts + cts_offset = -1024 + 1024 = 0.
        push(&mut t, 0, 10, -1024, 1024, true);
        push(&mut t, 10, 10, -512, 2560, false);
        push(&mut t, 20, 10, 0, 1024, false);
        t.last_duration_hint = 512;
        assert_eq!(t.media_time(), 1024);
        assert_eq!(t.presented_duration(), t.media_duration());
    }

    /// An AAC-style priming case: no reordering at all (`cts_offset` is
    /// always `0`), but the first sample's `dts` (and so its `pts`, since
    /// they are equal) starts negative. `presented_duration` still gets this
    /// right given a genuinely negative `dts` — but `media_time` cannot: it
    /// only ever sees `cts_offset`, which is `0` here, so it reports `0`
    /// where the reference would state `1024`. This is
    /// `TrackState::media_time`'s documented pipeline-normalization gap,
    /// demonstrated directly rather than only asserted in prose
    /// (CONFORMANCE-FINDINGS 49): the real pipeline never actually hands
    /// this crate a negative `dts` (something upstream already normalizes it
    /// to `0` first), so `media_time` degrades to this rather than doing
    /// worse than it does.
    #[test]
    fn media_time_cannot_see_a_priming_delay_that_presented_duration_can() {
        let mut t = TrackState::new(1, 44_100, entry(), params());
        push(&mut t, 0, 10, -1024, 0, true);
        push(&mut t, 10, 10, 0, 0, false);
        push(&mut t, 20, 10, 1024, 0, false);
        t.last_duration_hint = 1024;
        assert_eq!(t.media_time(), 0, "cts_offset alone cannot see a negative dts");
        assert_eq!(t.presented_duration(), t.media_duration() - 1024);
    }

    #[test]
    fn an_audio_track_gets_full_volume_and_a_sound_handler() {
        let mut p = params();
        p.media_type = Some(MediaType::Audio);
        let e = BuiltEntry {
            bytes: vec![0u8; 4],
            media: MediaType::Audio,
        };
        let t = TrackState::new(2, 44_100, e, p);
        assert_eq!(t.volume, 0x0100);
        assert_eq!(t.handler, FourCc::new(b"soun"));
        assert_eq!(t.width, 0);
    }
}
