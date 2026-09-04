//! Per-track sample readers, and the rule that decides which packet is next.
//!
//! # Why samples are produced in batches
//!
//! `vaco-format-isom`'s tables **borrow** the `moov` bytes: `SampleTable<'a>`
//! is a set of `&'a [u8]` slices plus decimated summaries, which is what makes
//! it allocate nothing proportional to the sample count. A `Box<dyn Demuxer>`
//! is `'static`, so the demuxer owns those bytes and cannot also hold a
//! structure that borrows them — safe Rust has no way to express that without
//! a self-referential helper crate (none is in `[workspace.dependencies]`) or
//! leaking the buffer.
//!
//! The resolution is to re-parse the one track's `stbl` per **refill** and take
//! a batch of samples into an owned queue. Parsing is O(samples) — measured at
//! 1.3 ns per sample by that crate's own benchmark — so a fixed batch would be
//! quadratic in the sample count. The batch therefore grows geometrically from
//! [`BATCH_MIN`] to [`BATCH_MAX`], which bounds the number of refills at
//! `log2(BATCH_MAX / BATCH_MIN) + count / BATCH_MAX` and the total re-parse
//! work at a small multiple of one parse.
//!
//! Reported rather than worked around: the natural fix is for
//! `vaco-format-isom` to expose an owned, resumable cursor state (`(index, dts,
//! chunk, within)` are all it needs), which would remove the re-parse
//! completely. See the crate's doc file.

use std::collections::VecDeque;

use vaco_core::{MediaType, Rational};
use vaco_format_isom::IsoBox;
use vaco_format_isom::cenc::{
    SampleEncryption, SampleToSeig, SeigDescriptions, TrackEncryption, sample_grouping_type,
};
use vaco_format_isom::frag::{TrackExtends, TrackFragment};
use vaco_format_isom::stbl::{Sample, SampleTable};

use crate::options::{DecryptionKey, Mp4Options, select_key};

/// Smallest refill batch, in samples.
pub(crate) const BATCH_MIN: u32 = 4096;
/// Largest refill batch, in samples. 128 Ki × 48 B ≈ 6 MiB per track.
pub(crate) const BATCH_MAX: u32 = 128 * 1024;
/// Samples taken from one track fragment per refill.
pub(crate) const FRAG_BATCH: u32 = 16 * 1024;

/// The most consecutive raw-PCM samples this crate ever coalesces into one
/// emitted packet, when [`Reader::raw_pcm`] is set.
///
/// Measured against `ffmpeg 9.0.1`, not assumed: a MOV/ISOBMFF `stsz` table
/// for `pcm_s16le`/`pcm_u8`/`pcm_f32le` audio states one entry **per sample
/// frame** (2/1/4 bytes respectively for those three), and this crate used to
/// emit one packet per entry — 52,920 packets for a 1.2 s, 44.1 kHz mono
/// clip, where the reference emits 52. Varying channel count and sample
/// format (mono/stereo, 8/16/32-bit) while holding duration fixed shows the
/// reference's packet boundary is a constant **sample count**, not a byte or
/// chunk-table target: `duration` is `1024` in every case, while `size`
/// scales with `channels * bytes_per_sample` (1024, 1024×2, 1024×4 bytes for
/// mono-u8, stereo-s16le and mono-f32le respectively) — and it does not
/// follow the source file's own `stsc` grouping either (a fixture whose
/// `stsc` groups 2048 samples per physical chunk still comes back as
/// 1024-sample packets, split in two). `1024` is that constant.
pub(crate) const PCM_GROUP_SAMPLES: u32 = 1024;

/// Hard ceiling on the samples one track will ever be walked for.
///
/// The real bound is the file size — see [`sample_limit`] — and this is the
/// backstop for a source that cannot state one.
pub(crate) const MAX_SAMPLES_PER_TRACK: u32 = 1 << 24;

/// Hard ceiling on the samples one `traf` will ever be walked for.
pub(crate) const MAX_SAMPLES_PER_FRAGMENT: u32 = vaco_format_isom::frag::MAX_SAMPLES_PER_TRAF;

/// How many samples of a track are worth walking.
///
/// **This is the bound `vaco-format-isom` deliberately left to the demuxer.** A
/// uniform `stsz` is the one declared count in the format with no payload to
/// clamp it against: twelve bytes can legally say `sample_count =
/// 0xFFFF_FFFF`. Nothing allocates for it, but iterating it is a denial of
/// service on a seventy-byte file.
///
/// The bound that is *correct* rather than arbitrary is the source's own size.
/// Distinct samples of one track occupy disjoint byte ranges, and a sample
/// occupies at least one byte, so a file of `n` bytes holds at most `n`
/// samples. A variable `stsz` is already clamped by its own payload; this
/// closes the uniform case with the same kind of argument rather than with a
/// magic number.
#[must_use]
pub(crate) fn sample_limit(declared: u32, source_size: Option<u64>) -> u32 {
    let by_size = source_size
        .and_then(|n| u32::try_from(n.saturating_add(1)).ok())
        .unwrap_or(u32::MAX);
    declared.min(by_size).min(MAX_SAMPLES_PER_TRACK)
}

/// One resolved sample, waiting to be turned into a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pending {
    pub offset: u64,
    pub size: u32,
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub key: bool,
    pub discard: bool,
    /// Leading samples to trim, in time-base ticks — which for an MP4 audio
    /// track are samples, because its time base is `1 / sample_rate`.
    pub skip: u32,
    /// Trailing samples to trim, same units: `ffprobe`'s `discard_padding`.
    /// Measured as 256/512/888/956 on `ffmpeg -c:a aac` files at
    /// 48/32/44.1/22.05 kHz, and it lands on the final sample only. See
    /// [`Reader::push`] for the two container statements it comes from.
    pub skip_end: u32,
    /// This sample's 0-based index within its track's `stbl`, or within its
    /// current `traf` for a fragmented track — what [`Decryptor::decrypt`]
    /// uses to select `senc`'s per-sample record.
    pub index: u32,
}

/// One `senc` record, pre-resolved: the sample's IV widened to a counter
/// block, and its subsample table (empty for full-sample encryption).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SencSample {
    pub key: [u8; 16],
    pub counter: [u8; 16],
    /// `(BytesOfClearData, BytesOfProtectedData)` pairs, in order.
    pub subsamples: Vec<(u16, u32)>,
}

/// Largest subsample table read for one sample. A NAL-structured sample has
/// one entry per NAL unit; 4096 is far past any real slice count and stops a
/// corrupt `subsample_count` from turning into a large allocation.
const MAX_SUBSAMPLES: u16 = 4096;

/// Owned per-track state for decrypting a `cenc`-protected track, built once a
/// usable key and a real `senc` are both in hand — see
/// `Mp4Options::decryption_keys` and the crate doc's *Common Encryption*
/// section. Fragmented tracks replace [`Self::samples`] from each `traf` before
/// that fragment's queue is filled.
///
/// Holds an **owned** copy of `senc`'s records rather than a borrow: a
/// `Reader` outlives any one `Movie::parse` borrow of `self.moov` (the same
/// reason `SampleTable` itself is re-parsed per refill instead of held
/// across calls — see this module's own doc comment). The copy is bounded by
/// `senc`'s own box size, which was already bounded when the whole `moov`
/// payload was read.
#[derive(Debug, Clone)]
pub(crate) struct Decryptor {
    fallback_key: Option<[u8; 16]>,
    keys: Vec<DecryptionKey>,
    defaults: TrackEncryption,
    track_descriptions: Option<SeigDescriptions>,
    pub samples: Vec<SencSample>,
}

impl Decryptor {
    /// Retain the key dictionary, track defaults and any track-level `seig`
    /// descriptions until each fragment supplies its own mapping and `senc`.
    pub(crate) fn fragmented(
        options: &Mp4Options,
        defaults: TrackEncryption,
        descriptions: &[IsoBox<'_>],
    ) -> Result<Self, &'static str> {
        let track_descriptions = unique_descriptions(descriptions)?;
        validate_parameters(&defaults, false)?;
        Ok(Self {
            fallback_key: options.decryption_key,
            keys: options.decryption_keys.clone(),
            defaults,
            track_descriptions,
            samples: Vec::new(),
        })
    }

    /// Build the complete progressive-track sample state from its `stbl`.
    pub(crate) fn progressive(
        options: &Mp4Options,
        defaults: TrackEncryption,
        table: &SampleTable<'_>,
    ) -> Result<Self, &'static str> {
        let track_descriptions = unique_descriptions(&table.sample_group_descriptions)?;
        let mapping = unique_mapping(&table.sample_to_groups)?;
        let senc = table
            .sample_encryption
            .as_ref()
            .and_then(SampleEncryption::parse)
            .ok_or("mp4: cenc track is missing or has a truncated senc sample-encryption box")?;
        if senc.sample_count != table.sample_count() {
            return Err("mp4: cenc senc sample count does not match the sample table");
        }
        if mapping
            .as_ref()
            .is_some_and(|mapping| mapping.sample_count() != senc.sample_count)
        {
            return Err("mp4: cenc seig sbgp sample count does not match the sample table");
        }
        let mut me = Self {
            fallback_key: options.decryption_key,
            keys: options.decryption_keys.clone(),
            defaults,
            track_descriptions,
            samples: Vec::new(),
        };
        me.samples = me.parse_records(&senc, mapping.as_ref(), None, false)?;
        Ok(me)
    }

    /// Replace the active records with this `traf`'s `senc` table.
    ///
    /// The named error covers a missing box, a count that disagrees with the
    /// fragment, or records that end early. The caller refuses the fragment
    /// before any queued ciphertext can be returned.
    pub(crate) fn replace_fragment(
        &mut self,
        traf: &TrackFragment<'_>,
        sample_count: u32,
    ) -> Result<(), &'static str> {
        let senc_box = traf
            .sample_encryption
            .as_ref()
            .ok_or("mp4: cenc: fragmented traf is missing its senc sample-encryption box")?;
        let senc = vaco_format_isom::cenc::SampleEncryption::parse(senc_box)
            .ok_or("mp4: cenc: fragmented traf has a truncated senc header")?;
        if senc.sample_count != sample_count {
            return Err("mp4: cenc: fragmented traf senc sample count does not match trun");
        }
        let fragment_descriptions = unique_descriptions(&traf.sample_group_descriptions)?;
        let mapping = unique_mapping(&traf.sample_to_groups)?;
        if mapping
            .as_ref()
            .is_some_and(|mapping| mapping.sample_count() != sample_count)
        {
            return Err("mp4: cenc seig fragment sbgp sample count does not match trun");
        }
        self.samples = self.parse_records(
            &senc,
            mapping.as_ref(),
            fragment_descriptions.as_ref(),
            true,
        )?;
        Ok(())
    }

    fn parse_records(
        &self,
        senc: &SampleEncryption<'_>,
        mapping: Option<&SampleToSeig>,
        fragment_descriptions: Option<&SeigDescriptions>,
        fragmented: bool,
    ) -> Result<Vec<SencSample>, &'static str> {
        let mut r = vaco_bitstream::ByteReader::new(senc.records);
        let mut samples = Vec::new();
        for sample in 0..senc.sample_count {
            let group_index = mapping.map_or(0, |mapping| mapping.index_for(sample).unwrap_or(0));
            let parameters = self.parameters(group_index, fragment_descriptions, fragmented)?;
            validate_parameters(&parameters, group_index != 0)?;
            let key = select_key(&self.keys, self.fallback_key, &parameters.default_kid).ok_or(
                if group_index == 0 {
                    "mp4: cenc default KID has no supplied decryption key"
                } else {
                    "mp4: cenc seig KID has no supplied decryption key"
                },
            )?;
            let iv = r.bytes(usize::from(parameters.per_sample_iv_size));
            let mut counter = [0u8; 16];
            let n = iv.len().min(16);
            if let (Some(dst), Some(src)) = (counter.get_mut(..n), iv.get(..n)) {
                dst.copy_from_slice(src);
            }
            let mut subsamples = Vec::new();
            if senc.has_subsamples {
                let count = r.be16();
                if count > MAX_SUBSAMPLES {
                    return Err("mp4: cenc senc subsample count exceeds the supported limit");
                }
                for _ in 0..count {
                    subsamples.push((r.be16(), r.be32()));
                }
            }
            if r.overrun() {
                return Err("mp4: cenc senc sample records are truncated");
            }
            samples.push(SencSample {
                key,
                counter,
                subsamples,
            });
        }
        Ok(samples)
    }

    fn parameters(
        &self,
        index: u32,
        fragment_descriptions: Option<&SeigDescriptions>,
        fragmented: bool,
    ) -> Result<TrackEncryption, &'static str> {
        if index == 0 {
            return Ok(self.defaults);
        }
        if index == 0x1_0000 {
            return Err("mp4: cenc seig group-description index 0x10000 is invalid");
        }
        if index >= 0x1_0001 {
            if !fragmented {
                return Err("mp4: cenc progressive sbgp uses a fragment-local seig index");
            }
            return fragment_descriptions
                .and_then(|descriptions| descriptions.get(index - 0x1_0000))
                .ok_or("mp4: cenc fragment sbgp references a missing seig description");
        }
        self.track_descriptions
            .as_ref()
            .and_then(|descriptions| descriptions.get(index))
            .ok_or("mp4: cenc sbgp references a missing track-level seig description")
    }

    /// Decrypt `payload` in place for sample `index`. `false` when `senc`
    /// declared no record for this sample or the record's subsample table
    /// does not fit the payload — the caller turns that into a reported
    /// error rather than silently handing back ciphertext.
    ///
    /// For a subsample-encrypted sample the protected ranges form **one**
    /// continuous AES-CTR stream (§9.5: the block counter is not reset and a
    /// partial block carries over into the next range), so they are gathered,
    /// decrypted once and scattered back. **Measured** against
    /// `ffmpeg 9.0.1 -decryption_key` on ffmpeg's own `cenc-aes-ctr` output:
    /// every decrypted H.264 and AAC packet matched the clear encode.
    pub(crate) fn decrypt(&self, index: u32, payload: &mut [u8]) -> bool {
        let Some(sample) = usize::try_from(index)
            .ok()
            .and_then(|i| self.samples.get(i))
        else {
            return false;
        };
        if sample.subsamples.is_empty() {
            vaco_crypto::ctr_apply_aes128(&sample.key, &sample.counter, payload);
            return true;
        }
        let mut ranges = Vec::new();
        let mut at = 0usize;
        for &(clear, protected) in &sample.subsamples {
            let start = at.checked_add(usize::from(clear));
            let end = start.and_then(|s| s.checked_add(usize::try_from(protected).ok()?));
            let (Some(start), Some(end)) = (start, end) else {
                return false;
            };
            if end > payload.len() {
                return false;
            }
            ranges.push(start..end);
            at = end;
        }
        let mut protected = Vec::new();
        for r in &ranges {
            if let Some(part) = payload.get(r.clone()) {
                protected.extend_from_slice(part);
            }
        }
        vaco_crypto::ctr_apply_aes128(&sample.key, &sample.counter, &mut protected);
        let mut taken = 0usize;
        for r in &ranges {
            let n = r.len();
            if let (Some(dst), Some(src)) = (
                payload.get_mut(r.clone()),
                protected.get(taken..taken.saturating_add(n)),
            ) {
                dst.copy_from_slice(src);
            }
            taken = taken.saturating_add(n);
        }
        true
    }
}

fn unique_descriptions(boxes: &[IsoBox<'_>]) -> Result<Option<SeigDescriptions>, &'static str> {
    let mut matching = boxes.iter().filter(|group| {
        sample_grouping_type(group).is_some_and(|kind| kind.as_bytes() == *b"seig")
    });
    let Some(group) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() {
        return Err("mp4: cenc has duplicate sgpd(seig) description boxes");
    }
    SeigDescriptions::parse(group)
        .map(Some)
        .map_err(|_| "mp4: cenc sgpd(seig) is malformed or not supported (requires version 1)")
}

fn unique_mapping(boxes: &[IsoBox<'_>]) -> Result<Option<SampleToSeig>, &'static str> {
    let mut matching = boxes.iter().filter(|group| {
        sample_grouping_type(group).is_some_and(|kind| kind.as_bytes() == *b"seig")
    });
    let Some(group) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() {
        return Err("mp4: cenc has duplicate sbgp(seig) mapping boxes");
    }
    SampleToSeig::parse(group)
        .map(Some)
        .map_err(|_| "mp4: cenc sbgp(seig) is malformed or not supported (requires version 0/1)")
}

fn validate_parameters(parameters: &TrackEncryption, from_seig: bool) -> Result<(), &'static str> {
    if !parameters.is_protected {
        return Err(if from_seig {
            "mp4: cenc clear seig sample groups are not supported"
        } else {
            "mp4: cenc tenc marks the track as unprotected"
        });
    }
    if parameters.crypt_byte_block != 0 || parameters.skip_byte_block != 0 {
        return Err(if from_seig {
            "mp4: cenc seig pattern encryption is not supported"
        } else {
            "mp4: cenc tenc pattern encryption is not supported"
        });
    }
    if parameters.per_sample_iv_size == 0 {
        return Err(if from_seig {
            "mp4: cenc seig constant IV is not supported"
        } else {
            "mp4: cenc tenc constant IV is not supported"
        });
    }
    if !matches!(parameters.per_sample_iv_size, 8 | 16) {
        return Err(if from_seig {
            "mp4: cenc seig per-sample IV size is not supported"
        } else {
            "mp4: cenc tenc per-sample IV size is not supported"
        });
    }
    Ok(())
}

/// One track fragment belonging to one track, located but not yet resolved.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FragEntry {
    /// Index into the demuxer's fragment list.
    pub fragment: usize,
    /// Index of the `traf` inside that `moof`.
    pub traf: usize,
    /// Decode time of its first sample.
    pub start_dts: i64,
    /// Samples it declares, already bounded.
    pub samples: u32,
    /// Offset of its first sample, for the tie-break rule and for seeking.
    pub first_offset: u64,
}

/// Where a track's samples come from.
#[derive(Debug)]
pub(crate) enum Source {
    /// The movie's own sample table.
    Table {
        /// Index into `Movie::tracks`, stable because `Movie::parse` skips a
        /// broken `trak` deterministically.
        slot: usize,
        next: u32,
        limit: u32,
    },
    /// Movie fragments.
    Fragments { entry: usize, next_in_entry: u32 },
    /// A single `covr` image, which has no timeline at all.
    AttachedPic {
        offset: u64,
        size: u32,
        emitted: bool,
    },
    /// One HEIF/AVIF item (ISO/IEC 23008-12): a single coded picture whose
    /// bytes `iloc` places as one or more extents, concatenated in order.
    Item {
        extents: Vec<(u64, u64)>,
        emitted: bool,
    },
}

/// One track's read state.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each names an independent axis a real file can combine — exhaustion, permanent \
              refusal, encryption — not a state machine with excluded combinations"
)]
pub(crate) struct Reader {
    pub stream_index: u32,
    pub time_base: Rational,
    /// The track's media type. Kept whole rather than as an `audio` flag so
    /// the one policy that is neither "audio" nor "not audio" — the trailing
    /// zero-duration `mov_text` sample `Mp4Demuxer::next_packet` skips — can
    /// name the type it was measured on instead of a second, parallel bool.
    pub media_type: MediaType,
    /// `min(0, min(ctts))`, or `cslg`. Applied to DTS only — a D17 deviation
    /// reproduced from the reference; see `vaco-format-isom`'s doc file.
    pub dts_shift: i64,
    /// The edit list's single shift, applied to both PTS and DTS.
    pub edit_shift: i64,
    /// Where the presented timeline starts; samples before it are trimmed.
    pub trim_point: i64,
    /// Where the presented timeline ends; the part of a sample past it is
    /// trimmed. `i64::MAX` when the edit list states no end (no `elst`, or a
    /// `segment_duration` of 0, which §8.6.6.1 defines as "to the end of the
    /// media").
    pub trim_end: i64,
    /// Samples one packet of this track decodes to, taken as the track's most
    /// common `stts` delta — for audio, whose MP4 time base is
    /// `1 / sample_rate`, a duration in ticks *is* a sample count.
    ///
    /// Read from the file rather than from a per-codec frame-size table on
    /// purpose: AAC alone is 1024 or 960 samples depending on the profile,
    /// and doubles again under SBR, so a table would over-trim the variants
    /// it guessed wrong. `stts` states the answer the file itself uses. Zero
    /// for video, and for a fragmented track with no sample table to survey,
    /// which disables the trailing trim rather than guessing.
    pub frame_samples: u32,
    pub source: Source,
    pub entries: Vec<FragEntry>,
    pub queue: VecDeque<Pending>,
    pub batch: u32,
    pub finished: bool,
    /// Permanently unreadable, as opposed to merely exhausted.
    ///
    /// A track whose `dref` points at another file is refused (plan 18
    /// §3.1.10), and a seek must not resurrect it. Keeping the two states in
    /// one `finished` flag did exactly that: `place` clears `finished` because
    /// a seek moves an exhausted reader back into the file, and it cleared the
    /// refusal with it. Found by the `dem_mp4` fuzz target, as "a seek produced
    /// a packet a straight read never did".
    pub blocked: bool,
    /// Why `sinf ▸ schm`/`sinf ▸ schi ▸ tenc` could not produce
    /// [`Reader::decrypt`] — no usable key, an unsupported scheme/grouping, or
    /// malformed auxiliary data.
    ///
    /// Deliberately **not** folded into `blocked`: a blocked track silently
    /// produces no packets forever, which is right for an unreachable `dref`
    /// but wrong here — a caller who asks for packets from a protected track
    /// it cannot decrypt should be told why, not handed an empty stream that
    /// looks the same as a track with nothing in it. `ensure_head` turns this
    /// into an [`vaco_core::Error::Unsupported`] the first time any packet is
    /// requested, once — for every track, so a mixed encrypted/clear file
    /// fails predictably rather than only once the encrypted track's turn in
    /// the interleave happens to come up.
    pub encryption_error: Option<&'static str>,
    /// Set instead of [`Reader::encryption_error`] when usable keys exist. For a
    /// progressive track this holds its sample-table `senc`; for a fragmented
    /// track refill replaces it from the current `traf` before queuing samples.
    pub decrypt: Option<Decryptor>,
    /// This track's codec name starts with `pcm_` and it is not
    /// `Common Encryption`-protected (grouping would break
    /// [`Decryptor::decrypt`]'s one-`senc`-record-per-table-sample indexing,
    /// and raw PCM inside CENC is not a real case this crate has seen to
    /// justify complicating that seam for).
    ///
    /// [`refill_table`] coalesces up to [`PCM_GROUP_SAMPLES`] consecutive,
    /// file-contiguous table entries into one packet when this is set —
    /// see that constant's own doc for why, and its module-level "why
    /// samples are produced in batches" doc for why entries are read in
    /// batches at all.
    pub raw_pcm: bool,
}

impl Reader {
    /// Whether this track's samples are audio, which is what the `elst`
    /// trims are expressed against.
    pub(crate) const fn is_audio(&self) -> bool {
        matches!(self.media_type, MediaType::Audio)
    }

    /// The next sample, without consuming it.
    pub(crate) fn head(&self) -> Option<&Pending> {
        self.queue.front()
    }

    /// Grow the batch for the next refill, bounded.
    fn grow(&mut self) {
        self.batch = self.batch.saturating_mul(2).min(BATCH_MAX);
    }

    /// Turn a media-timescale sample into a queue entry.
    fn push(
        &mut self,
        offset: u64,
        size: u32,
        dts: i64,
        cts: i32,
        duration: u32,
        key: bool,
        index: u32,
    ) {
        let dts_out = dts
            .saturating_add(self.dts_shift)
            .saturating_add(self.edit_shift);
        let pts_out = dts
            .saturating_add(i64::from(cts))
            .saturating_add(self.edit_shift);
        let end = pts_out.saturating_add(i64::from(duration));
        let discard = end <= self.trim_point && duration > 0;
        let skip = if self.is_audio() {
            self.trim_point
                .saturating_sub(pts_out)
                .clamp(0, i64::from(duration)) as u32
        } else {
            0
        };
        // The tail the decoder produces but the file never presents.
        //
        // Two container statements say this, and one expression covers both.
        // A sample's *declared* `stts` duration can be shorter than the
        // frame it decodes to — that is where `ffmpeg -c:a aac` puts its
        // trailing encoder padding, measured as a final `stts` delta of 768
        // against 1024-sample frames on a 48 kHz file, matching `ffprobe`'s
        // `discard_padding=256`. Separately, an `elst` can end before the
        // media does. So the presented end is the earlier of "what this
        // sample declares" and "where the edit list stops", and the trim is
        // everything the decoder emits past it.
        let skip_end = if self.is_audio() && self.frame_samples > duration {
            let decoded_end = pts_out.saturating_add(i64::from(self.frame_samples));
            let presented_end = end.min(self.trim_end);
            decoded_end.saturating_sub(presented_end).clamp(
                0,
                i64::from(self.frame_samples).saturating_sub(i64::from(skip)),
            ) as u32
        } else if self.is_audio() {
            end.saturating_sub(self.trim_end)
                .clamp(0, i64::from(duration).saturating_sub(i64::from(skip))) as u32
        } else {
            0
        };
        self.queue.push_back(Pending {
            offset,
            size,
            dts: dts_out,
            pts: pts_out,
            duration,
            key,
            discard,
            skip,
            skip_end,
            index,
        });
    }
}

/// Fill `reader`'s queue from the movie's sample table.
///
/// Returns whether the reader **advanced** — which is not the same as whether
/// it produced a packet. A batch whose samples all lie outside the file yields
/// nothing and is still progress, because the cursor moved; reporting it as a
/// stall is how a truncated file turns into a spurious "no progress" error
/// several thousand samples before the end of its table. Found by the
/// `dem_mp4` fuzz target on a truncated fragmented file, which read zero
/// packets straight through and a dozen after a seek.
///
/// Borrows are split by the caller:
/// `table` is derived from the demuxer's `moov` field while `reader` comes from
/// its `readers` field, and the two are disjoint.
pub(crate) fn refill_table(
    reader: &mut Reader,
    table: &SampleTable<'_>,
    source_size: Option<u64>,
) -> bool {
    let Source::Table { next, limit, .. } = &mut reader.source else {
        return false;
    };
    let (start, limit) = (*next, *limit);
    if start >= limit {
        reader.finished = true;
        return false;
    }
    let want = reader.batch.min(limit.saturating_sub(start));
    let mut last = start;
    let samples: Vec<Sample> = table
        .cursor_at(start)
        .take_while(|s| s.index < limit)
        .take(want as usize)
        .collect();
    let got = samples.len();
    if reader.raw_pcm {
        // Coalesce runs of contiguous table entries into one packet each,
        // up to `PCM_GROUP_SAMPLES` — see `Reader::raw_pcm`'s doc for why.
        // A run breaks early on a file-offset discontinuity (a chunk
        // boundary against an interleaved track's own bytes) or a sample
        // that does not fit the source, exactly like the ungrouped path
        // below, just applied to a run instead of one entry at a time.
        let mut i = 0usize;
        while let Some(&first) = samples.get(i) {
            last = first.index;
            let first_fits =
                source_size.is_none_or(|n| first.offset.saturating_add(u64::from(first.size)) <= n);
            if !first_fits {
                i += 1;
                continue;
            }
            let mut count = 1u32;
            let mut bytes = first.size;
            let mut duration = first.duration;
            let mut end = first.offset.saturating_add(u64::from(first.size));
            let mut j = i + 1;
            while count < PCM_GROUP_SAMPLES {
                let Some(&s) = samples.get(j) else { break };
                if s.offset != end {
                    break;
                }
                let sample_fits =
                    source_size.is_none_or(|n| end.saturating_add(u64::from(s.size)) <= n);
                if !sample_fits {
                    break;
                }
                end = end.saturating_add(u64::from(s.size));
                bytes = bytes.saturating_add(s.size);
                duration = duration.saturating_add(s.duration);
                count += 1;
                last = s.index;
                j += 1;
            }
            reader.push(
                first.offset,
                bytes,
                first.dts,
                first.cts_offset,
                duration,
                first.is_sync,
                first.index,
            );
            i = j;
        }
    } else {
        for s in &samples {
            let s = *s;
            last = s.index;
            // A sample that does not fit inside the source is not readable. Plan 18
            // §3.1.10: they are dropped, and `nb_frames` still reports the table's
            // count.
            let fits = source_size.is_none_or(|n| s.offset.saturating_add(u64::from(s.size)) <= n);
            if fits {
                reader.push(
                    s.offset,
                    s.size,
                    s.dts,
                    s.cts_offset,
                    s.duration,
                    s.is_sync,
                    s.index,
                );
            }
        }
    }
    // A cursor that yielded less than a full batch has run out: the table
    // cannot resolve any later sample either, so this is the end rather than a
    // gap to walk over one index at a time.
    let exhausted = got < want as usize;
    let advanced = last
        .saturating_add(1)
        .max(start.saturating_add(want))
        .min(limit);
    if let Source::Table { next, .. } = &mut reader.source {
        *next = advanced;
        if exhausted || advanced >= limit {
            reader.finished = true;
        }
    }
    reader.grow();
    true
}

/// Fill `reader`'s queue from one track fragment.
pub(crate) fn refill_fragment(
    reader: &mut Reader,
    traf: &TrackFragment<'_>,
    base: u64,
    defaults: &TrackExtends,
    source_size: Option<u64>,
) -> bool {
    let Source::Fragments {
        entry,
        next_in_entry,
    } = reader.source
    else {
        return false;
    };
    let Some(e) = reader.entries.get(entry).copied() else {
        return false;
    };
    if next_in_entry >= e.samples {
        return false;
    }
    let want = FRAG_BATCH.min(e.samples.saturating_sub(next_in_entry));
    let resolved: Vec<_> = traf
        .samples(base, e.start_dts, defaults)
        .skip(next_in_entry as usize)
        .take(want as usize)
        .collect();
    for (i, s) in resolved.into_iter().enumerate() {
        let fits = source_size.is_none_or(|n| s.offset.saturating_add(u64::from(s.size)) <= n);
        if fits {
            reader.push(
                s.offset,
                s.size,
                s.dts,
                s.cts_offset,
                s.duration,
                s.is_sync(),
                next_in_entry.saturating_add(u32::try_from(i).unwrap_or(u32::MAX)),
            );
        }
    }
    if let Some(Source::Fragments { next_in_entry, .. }) = Some(&mut reader.source) {
        *next_in_entry = next_in_entry.saturating_add(want);
    }
    true
}

/// Advance to the next track fragment, if this track has one.
pub(crate) fn advance_fragment(reader: &mut Reader) -> bool {
    let Source::Fragments {
        entry,
        next_in_entry,
    } = &mut reader.source
    else {
        return false;
    };
    let done = reader
        .entries
        .get(*entry)
        .is_none_or(|e| *next_in_entry >= e.samples);
    if !done {
        return true;
    }
    if entry.saturating_add(1) < reader.entries.len() {
        *entry = entry.saturating_add(1);
        *next_in_entry = 0;
        true
    } else {
        false
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_uniform_stsz_is_bounded_by_the_source_size() {
        // Twelve bytes can declare four billion samples; a seventy-eight-byte
        // file holds at most seventy-nine.
        assert_eq!(sample_limit(u32::MAX, Some(78)), 79);
        assert_eq!(sample_limit(4, Some(78)), 4);
        assert_eq!(sample_limit(u32::MAX, None), MAX_SAMPLES_PER_TRACK);
        assert_eq!(
            sample_limit(u32::MAX, Some(u64::MAX)),
            MAX_SAMPLES_PER_TRACK
        );
    }

    fn reader() -> Reader {
        Reader {
            stream_index: 0,
            time_base: Rational::new(1, 1000),
            media_type: MediaType::Audio,
            dts_shift: 0,
            edit_shift: -1024,
            trim_point: 0,
            trim_end: i64::MAX,
            frame_samples: 0,
            source: Source::Table {
                slot: 0,
                next: 0,
                limit: 8,
            },
            entries: Vec::new(),
            queue: VecDeque::new(),
            batch: BATCH_MIN,
            finished: false,
            blocked: false,
            encryption_error: None,
            raw_pcm: false,
            decrypt: None,
        }
    }

    #[test]
    fn a_sample_entirely_before_the_edit_is_discarded_and_trimmed() {
        let mut r = reader();
        r.push(0, 4, 0, 0, 1024, true, 0);
        let p = r.queue.front().copied().unwrap();
        assert_eq!(p.pts, -1024);
        assert_eq!(p.dts, -1024);
        assert!(p.discard);
        assert_eq!(p.skip, 1024);
    }

    #[test]
    fn a_sample_straddling_the_edit_is_trimmed_but_kept() {
        let mut r = reader();
        r.edit_shift = -512;
        r.push(0, 4, 0, 0, 1024, true, 0);
        let p = r.queue.front().copied().unwrap();
        assert_eq!(p.pts, -512);
        assert!(!p.discard);
        assert_eq!(p.skip, 512);
    }

    #[test]
    fn a_video_sample_is_never_given_a_sample_trim() {
        let mut r = reader();
        r.media_type = MediaType::Video;
        r.push(0, 4, 0, 0, 1024, true, 0);
        let p = r.queue.front().copied().unwrap();
        assert!(p.discard);
        assert_eq!(p.skip, 0);
    }

    #[test]
    fn the_composition_offset_moves_pts_but_not_dts() {
        let mut r = reader();
        r.edit_shift = 0;
        r.dts_shift = -512;
        r.push(0, 4, 1024, 256, 512, false, 0);
        let p = r.queue.front().copied().unwrap();
        assert_eq!(p.dts, 512, "dts carries the ctts-derived shift");
        assert_eq!(p.pts, 1280, "pts carries the composition offset");
    }

    #[test]
    fn the_batch_grows_geometrically_and_stops() {
        let mut r = reader();
        for _ in 0..64 {
            r.grow();
        }
        assert_eq!(r.batch, BATCH_MAX);
    }
}
