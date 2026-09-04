//! Movie fragments: `mvex`/`trex`, `moof ▸ traf ▸ tfhd/tfdt/trun/senc`, `sidx`
//! and `mfra ▸ tfra`.
//!
//! ISO/IEC 14496-12 §8.8. A fragmented file replaces the sample tables with a
//! chain of per-fragment run tables, so everything [`crate::stbl`] answers has
//! to be answerable here too — from different bytes, with a different defaults
//! cascade, and with byte addressing that has three cases instead of one.
//!
//! # The defaults cascade
//!
//! A sample's duration, size and flags come from the first of these that states
//! them (§8.8.8.1):
//!
//! 1. the `trun` entry itself, when the corresponding `tr_flags` bit is set;
//! 2. `tfhd`'s `default_sample_*`, when the corresponding `tf_flags` bit is set;
//! 3. `trex`'s `default_sample_*` from `mvex` in the `moov`.
//!
//! plus one exception: the **first** sample of a `trun` takes
//! `first_sample_flags` when `tr_flags & 0x4` is set, which is how a fragment
//! marks its opening keyframe without spending four bytes per sample.
//!
//! # Byte addressing, the part that is usually wrong
//!
//! `trun.data_offset` is relative to a *base*, and the base is chosen by
//! §8.8.7.1 in this order:
//!
//! 1. `tfhd.base_data_offset`, when present;
//! 2. the start of the enclosing `moof`, when `default-base-is-moof` is set;
//! 3. otherwise: the start of the enclosing `moof` for the **first** track
//!    fragment, and the end of the preceding track fragment's data for each one
//!    after it.
//!
//! Measured against `ffprobe 8.1`: a `frag_keyframe+empty_moov` file whose
//! `tfhd` carries `base_data_offset = 1259` (the `moof` position) and whose
//! `trun` carries `data_offset = 516` reports its first packet at `pos=1775`;
//! a `default_base_moof+omit_tfhd_offset` file with `moof` at 769 and
//! `data_offset = 152` reports `pos=921`. Both are reproduced by
//! [`TrackFragment::samples`].
//!
//! `planning/18-formats.md` §3.1.10 says the fall-through case bases on "the
//! start of the previous `mdat`". That is not what 14496-12 §8.8.7.1 says, and
//! it disagrees with case 3 above in any file with more than one `traf`. This
//! crate follows the specification; see the crate doc file for the note.

use vaco_core::{Error, Result};

use crate::boxes::{BoxIter, IsoBox};
use crate::fourcc::boxes;
use crate::table::EntryTable;

/// `tfhd` flag: `base_data_offset` present.
pub const TF_BASE_DATA_OFFSET: u32 = 0x00_0001;
/// `tfhd` flag: `sample_description_index` present.
pub const TF_SAMPLE_DESCRIPTION_INDEX: u32 = 0x00_0002;
/// `tfhd` flag: `default_sample_duration` present.
pub const TF_DEFAULT_SAMPLE_DURATION: u32 = 0x00_0008;
/// `tfhd` flag: `default_sample_size` present.
pub const TF_DEFAULT_SAMPLE_SIZE: u32 = 0x00_0010;
/// `tfhd` flag: `default_sample_flags` present.
pub const TF_DEFAULT_SAMPLE_FLAGS: u32 = 0x00_0020;
/// `tfhd` flag: the fragment has no samples for this track.
pub const TF_DURATION_IS_EMPTY: u32 = 0x01_0000;
/// `tfhd` flag: base the data offset on the enclosing `moof`.
pub const TF_DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;

/// `trun` flag: `data_offset` present.
pub const TR_DATA_OFFSET: u32 = 0x00_0001;
/// `trun` flag: `first_sample_flags` present.
pub const TR_FIRST_SAMPLE_FLAGS: u32 = 0x00_0004;
/// `trun` flag: per-sample duration present.
pub const TR_SAMPLE_DURATION: u32 = 0x00_0100;
/// `trun` flag: per-sample size present.
pub const TR_SAMPLE_SIZE: u32 = 0x00_0200;
/// `trun` flag: per-sample flags present.
pub const TR_SAMPLE_FLAGS: u32 = 0x00_0400;
/// `trun` flag: per-sample composition offset present.
pub const TR_SAMPLE_CTS_OFFSET: u32 = 0x00_0800;

/// Largest number of `trun` boxes kept per track fragment.
///
/// Each costs a small descriptor; the boxes themselves bound the count, but a
/// `moof` made of a million empty `trun`s is cheap to write and this makes the
/// residency explicit.
pub const MAX_RUNS_PER_TRAF: usize = 4096;
/// Largest number of track fragments kept per movie fragment.
pub const MAX_TRAF_PER_MOOF: usize = 1024;
/// Largest number of samples resolved from one track fragment.
///
/// A `trun` whose fields all come from `tfhd`/`trex` carries no per-sample
/// bytes to clamp its declared count against. This cap keeps that valid shape
/// reachable without letting a four-byte count drive an unbounded walk.
pub const MAX_SAMPLES_PER_TRAF: u32 = 1 << 20;
/// Largest number of `tfra` entries kept.
pub const MAX_TFRA_ENTRIES: u32 = 1 << 20;

/// The `sample_flags` word of §8.8.3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SampleFlags(pub u32);

impl SampleFlags {
    /// `is_leading`, two bits.
    #[must_use]
    pub const fn is_leading(self) -> u8 {
        ((self.0 >> 26) & 0x3) as u8
    }

    /// `sample_depends_on`: 1 = not intra, 2 = intra.
    #[must_use]
    pub const fn depends_on(self) -> u8 {
        ((self.0 >> 24) & 0x3) as u8
    }

    /// `sample_is_depended_on`: 1 = others depend on it, 2 = disposable.
    #[must_use]
    pub const fn is_depended_on(self) -> u8 {
        ((self.0 >> 22) & 0x3) as u8
    }

    /// `sample_has_redundancy`.
    #[must_use]
    pub const fn has_redundancy(self) -> u8 {
        ((self.0 >> 20) & 0x3) as u8
    }

    /// `sample_padding_value`.
    #[must_use]
    pub const fn padding(self) -> u8 {
        ((self.0 >> 17) & 0x7) as u8
    }

    /// `sample_degradation_priority`.
    #[must_use]
    pub const fn degradation_priority(self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }

    /// Whether the sample may be decoded from cold.
    ///
    /// The file states the *negative* (`sample_is_non_sync_sample`), so this is
    /// its inverse. `sample_depends_on == 2` also declares an intra sample and
    /// is honoured, because encoders disagree about which of the two they set.
    #[must_use]
    pub const fn is_sync(self) -> bool {
        (self.0 & 0x0001_0000) == 0 || self.depends_on() == 2
    }
}

/// `trex` — per-track defaults for every fragment (§8.8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackExtends {
    /// The track these defaults apply to.
    pub track_id: u32,
    /// Default one-based `stsd` index.
    pub default_sample_description_index: u32,
    /// Default sample duration in media ticks.
    pub default_sample_duration: u32,
    /// Default sample size in bytes.
    pub default_sample_size: u32,
    /// Default `sample_flags`.
    pub default_sample_flags: u32,
}

impl TrackExtends {
    /// Parse a `trex` full box.
    #[must_use]
    pub fn parse(full: &crate::boxes::FullBox<'_>) -> Self {
        let mut r = full.reader();
        Self {
            track_id: r.be32(),
            default_sample_description_index: r.be32(),
            default_sample_duration: r.be32(),
            default_sample_size: r.be32(),
            default_sample_flags: r.be32(),
        }
    }
}

/// `tfhd` — the per-fragment overrides (§8.8.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackFragmentHeader {
    /// `track_ID`.
    pub track_id: u32,
    /// `base_data_offset`, when stated.
    pub base_data_offset: Option<u64>,
    /// `sample_description_index`, when stated.
    pub sample_description_index: Option<u32>,
    /// `default_sample_duration`, when stated.
    pub default_sample_duration: Option<u32>,
    /// `default_sample_size`, when stated.
    pub default_sample_size: Option<u32>,
    /// `default_sample_flags`, when stated.
    pub default_sample_flags: Option<u32>,
    /// `duration-is-empty`.
    pub duration_is_empty: bool,
    /// `default-base-is-moof`.
    pub default_base_is_moof: bool,
}

impl TrackFragmentHeader {
    /// Parse a `tfhd` full box.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the declared optional fields do not fit.
    pub fn parse(full: &crate::boxes::FullBox<'_>) -> Result<Self> {
        let f = full.flags;
        let mut r = full.reader();
        let track_id = r.be32();
        let base_data_offset = (f & TF_BASE_DATA_OFFSET != 0).then(|| r.be64());
        let sample_description_index = (f & TF_SAMPLE_DESCRIPTION_INDEX != 0).then(|| r.be32());
        let default_sample_duration = (f & TF_DEFAULT_SAMPLE_DURATION != 0).then(|| r.be32());
        let default_sample_size = (f & TF_DEFAULT_SAMPLE_SIZE != 0).then(|| r.be32());
        let default_sample_flags = (f & TF_DEFAULT_SAMPLE_FLAGS != 0).then(|| r.be32());
        r.check()
            .map_err(|_| Error::InvalidData("isom: truncated tfhd"))?;
        Ok(Self {
            track_id,
            base_data_offset,
            sample_description_index,
            default_sample_duration,
            default_sample_size,
            default_sample_flags,
            duration_is_empty: f & TF_DURATION_IS_EMPTY != 0,
            default_base_is_moof: f & TF_DEFAULT_BASE_IS_MOOF != 0,
        })
    }
}

/// Byte offsets of the optional per-sample fields within one `trun` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TrunLayout {
    duration: Option<usize>,
    size: Option<usize>,
    flags: Option<usize>,
    cts: Option<usize>,
    stride: usize,
}

impl TrunLayout {
    fn from_flags(f: u32) -> Self {
        let mut me = Self::default();
        let mut at = 0usize;
        let mut take = |present: bool| -> Option<usize> {
            if !present {
                return None;
            }
            let here = at;
            at = at.saturating_add(4);
            Some(here)
        };
        me.duration = take(f & TR_SAMPLE_DURATION != 0);
        me.size = take(f & TR_SAMPLE_SIZE != 0);
        me.flags = take(f & TR_SAMPLE_FLAGS != 0);
        me.cts = take(f & TR_SAMPLE_CTS_OFFSET != 0);
        me.stride = at;
        me
    }
}

/// `trun` — one run of samples (§8.8.8).
#[derive(Debug, Clone)]
pub struct TrackRun<'a> {
    /// `data_offset`, relative to the track fragment's base.
    pub data_offset: Option<i32>,
    /// `first_sample_flags`, overriding the first sample's flags only.
    pub first_sample_flags: Option<u32>,
    /// `version`; version 1 makes composition offsets signed.
    pub version: u8,
    sample_count: u32,
    entries: EntryTable<'a>,
    layout: TrunLayout,
}

impl<'a> TrackRun<'a> {
    /// Parse a `trun` full box.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the fixed header does not fit.
    pub fn parse(full: &crate::boxes::FullBox<'a>) -> Result<Self> {
        let f = full.flags;
        let mut r = full.reader();
        let declared = r.be32();
        let data_offset = (f & TR_DATA_OFFSET != 0).then(|| r.be32().cast_signed());
        let first_sample_flags = (f & TR_FIRST_SAMPLE_FLAGS != 0).then(|| r.be32());
        r.check()
            .map_err(|_| Error::InvalidData("isom: truncated trun header"))?;
        let layout = TrunLayout::from_flags(f);
        let rest = full.body.get(r.pos()..).unwrap_or(&[]);
        // A zero-stride run has no entry bytes to clamp against: every field
        // comes from `tfhd`/`trex`, but `sample_count` still states how many
        // samples use those defaults. The enclosing-traf cap supplies the
        // bound that bytes cannot in this one valid shape.
        let entries = if layout.stride == 0 {
            EntryTable::new(&[], 1, 0)
        } else {
            EntryTable::new(rest, layout.stride, declared)
        };
        let sample_count = if layout.stride == 0 {
            declared.min(MAX_SAMPLES_PER_TRAF)
        } else {
            entries.len()
        };
        Ok(Self {
            data_offset,
            first_sample_flags,
            version: full.version,
            sample_count,
            entries,
            layout,
        })
    }

    /// Samples in the run.
    ///
    /// For a zero-stride run, the bounded declared count whose fields all fall
    /// back to `tfhd`/`trex`; otherwise the count its entry bytes can hold.
    #[must_use]
    pub const fn sample_count(&self) -> u32 {
        self.sample_count
    }

    fn field(&self, i: u32, at: Option<usize>) -> Option<u32> {
        self.entries.get_u32(i, at?)
    }
}

/// `traf` — one track's part of a movie fragment (§8.8.6).
#[derive(Debug, Clone)]
pub struct TrackFragment<'a> {
    /// The `tfhd`.
    pub header: TrackFragmentHeader,
    /// `tfdt.baseMediaDecodeTime`, when present.
    ///
    /// The only reliable way to place a fragment on the timeline;
    /// `default-base-is-moof` and `base_data_offset` govern *byte* addressing
    /// and say nothing about time.
    pub base_media_decode_time: Option<u64>,
    /// The runs, in file order.
    pub runs: Vec<TrackRun<'a>>,
    /// Fragment-local Common Encryption records, when a `senc` child exists.
    pub sample_encryption: Option<IsoBox<'a>>,
    /// Fragment-local `sgpd` sample-group descriptions, retained raw.
    pub sample_group_descriptions: Vec<IsoBox<'a>>,
    /// Fragment-local `sbgp` sample-to-group maps, retained raw.
    pub sample_to_groups: Vec<IsoBox<'a>>,
}

impl<'a> TrackFragment<'a> {
    /// Parse a `traf` container.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed child or a missing `tfhd`.
    pub fn parse(traf: &IsoBox<'a>) -> Result<Self> {
        let mut header = None;
        let mut base_media_decode_time = None;
        let mut runs = Vec::new();
        let mut sample_encryption = None;
        let mut sample_group_descriptions = Vec::new();
        let mut sample_to_groups = Vec::new();
        for child in traf.children() {
            let child = child?;
            match child.kind() {
                boxes::TFHD => header = Some(TrackFragmentHeader::parse(&child.full()?)?),
                boxes::TFDT => {
                    let full = child.full()?;
                    let mut r = full.reader();
                    let v = if full.version == 1 {
                        r.be64()
                    } else {
                        u64::from(r.be32())
                    };
                    if r.check().is_ok() {
                        base_media_decode_time = Some(v);
                    }
                }
                boxes::TRUN if runs.len() < MAX_RUNS_PER_TRAF => {
                    runs.push(TrackRun::parse(&child.full()?)?);
                }
                boxes::SENC if sample_encryption.is_none() => sample_encryption = Some(child),
                boxes::SGPD => {
                    if sample_group_descriptions.len() >= crate::cenc::MAX_SAMPLE_GROUP_BOXES {
                        return Err(Error::LimitExceeded {
                            limit: "isom traf sgpd boxes",
                            requested: sample_group_descriptions.len() as u64 + 1,
                            cap: crate::cenc::MAX_SAMPLE_GROUP_BOXES as u64,
                        });
                    }
                    sample_group_descriptions.push(child);
                }
                boxes::SBGP => {
                    if sample_to_groups.len() >= crate::cenc::MAX_SAMPLE_GROUP_BOXES {
                        return Err(Error::LimitExceeded {
                            limit: "isom traf sbgp boxes",
                            requested: sample_to_groups.len() as u64 + 1,
                            cap: crate::cenc::MAX_SAMPLE_GROUP_BOXES as u64,
                        });
                    }
                    sample_to_groups.push(child);
                }
                _ => {}
            }
        }
        Ok(Self {
            header: header.ok_or(Error::InvalidData("isom: traf without a tfhd"))?,
            base_media_decode_time,
            runs,
            sample_encryption,
            sample_group_descriptions,
            sample_to_groups,
        })
    }

    /// Samples in the fragment, across every run.
    #[must_use]
    pub fn sample_count(&self) -> u64 {
        self.runs
            .iter()
            .fold(0u64, |a, r| a.saturating_add(u64::from(r.sample_count())))
            .min(u64::from(MAX_SAMPLES_PER_TRAF))
    }

    /// Resolve every sample in the fragment.
    ///
    /// `base` is the track fragment's data base, from
    /// [`MovieFragment::track_base`]. `decode_time` is where the fragment
    /// starts on the media timeline — `tfdt` when present, otherwise the
    /// caller's running total, because an fMP4 written without `tfdt` can only
    /// be placed by accumulating.
    #[must_use]
    pub fn samples(
        &self,
        base: u64,
        decode_time: i64,
        defaults: &TrackExtends,
    ) -> FragmentSamples<'_, 'a> {
        FragmentSamples {
            traf: self,
            defaults: *defaults,
            run: 0,
            index_in_run: 0,
            cursor: base,
            base,
            dts: self
                .base_media_decode_time
                .map_or(decode_time, |t| i64::try_from(t).unwrap_or(i64::MAX)),
            started_run: false,
            remaining: MAX_SAMPLES_PER_TRAF,
        }
    }

    /// One past the last byte any of this fragment's samples occupies, which is
    /// the base of the next track fragment when neither offset flag is set.
    #[must_use]
    pub fn data_end(&self, base: u64, defaults: &TrackExtends) -> u64 {
        self.samples(base, 0, defaults).fold(base, |a, s| {
            a.max(s.offset.saturating_add(u64::from(s.size)))
        })
    }
}

/// One resolved fragment sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentSample {
    /// Absolute byte offset.
    pub offset: u64,
    /// Length in bytes.
    pub size: u32,
    /// Duration in media ticks.
    pub duration: u32,
    /// Decode time in media ticks.
    pub dts: i64,
    /// `pts - dts`.
    pub cts_offset: i32,
    /// The resolved `sample_flags`.
    pub flags: SampleFlags,
    /// One-based `stsd` index.
    pub description_index: u32,
}

impl FragmentSample {
    /// `dts + cts_offset`, saturating.
    #[must_use]
    pub const fn pts(&self) -> i64 {
        self.dts.saturating_add(self.cts_offset as i64)
    }

    /// Whether decoding may start here.
    #[must_use]
    pub const fn is_sync(&self) -> bool {
        self.flags.is_sync()
    }
}

/// Iterator over a track fragment's samples.
#[derive(Debug, Clone)]
pub struct FragmentSamples<'t, 'a> {
    traf: &'t TrackFragment<'a>,
    defaults: TrackExtends,
    run: usize,
    index_in_run: u32,
    cursor: u64,
    base: u64,
    dts: i64,
    started_run: bool,
    remaining: u32,
}

impl Iterator for FragmentSamples<'_, '_> {
    type Item = FragmentSample;

    fn next(&mut self) -> Option<FragmentSample> {
        if self.remaining == 0 {
            return None;
        }
        loop {
            let run = self.traf.runs.get(self.run)?;
            if !self.started_run {
                // A run with an explicit data offset restarts from the base;
                // one without continues where the previous run's data ended.
                if let Some(d) = run.data_offset {
                    self.cursor = self.base.saturating_add_signed(i64::from(d));
                }
                self.started_run = true;
            }
            if self.index_in_run >= run.sample_count() {
                self.run = self.run.saturating_add(1);
                self.index_in_run = 0;
                self.started_run = false;
                continue;
            }
            let i = self.index_in_run;
            let h = &self.traf.header;
            let duration = run
                .field(i, run.layout.duration)
                .or(h.default_sample_duration)
                .unwrap_or(self.defaults.default_sample_duration);
            let size = run
                .field(i, run.layout.size)
                .or(h.default_sample_size)
                .unwrap_or(self.defaults.default_sample_size);
            let flags = match (i, run.first_sample_flags) {
                (0, Some(f)) => f,
                _ => run
                    .field(i, run.layout.flags)
                    .or(h.default_sample_flags)
                    .unwrap_or(self.defaults.default_sample_flags),
            };
            let cts_offset = run.field(i, run.layout.cts).map_or(0, |v| {
                if run.version == 0 {
                    // Version 0 composition offsets are unsigned; a value above
                    // i32::MAX is clamped rather than wrapped negative.
                    i32::try_from(v).unwrap_or(i32::MAX)
                } else {
                    v.cast_signed()
                }
            });
            let out = FragmentSample {
                offset: self.cursor,
                size,
                duration,
                dts: self.dts,
                cts_offset,
                flags: SampleFlags(flags),
                description_index: h
                    .sample_description_index
                    .unwrap_or(self.defaults.default_sample_description_index),
            };
            self.index_in_run = i.saturating_add(1);
            self.cursor = self.cursor.saturating_add(u64::from(size));
            self.dts = self.dts.saturating_add(i64::from(duration));
            self.remaining = self.remaining.saturating_sub(1);
            return Some(out);
        }
    }
}

/// `moof` — one movie fragment (§8.8.4).
#[derive(Debug, Clone)]
pub struct MovieFragment<'a> {
    /// `mfhd.sequence_number`.
    pub sequence_number: u32,
    /// Absolute file offset of the `moof` box's first byte.
    pub offset: u64,
    /// The track fragments, in file order.
    pub tracks: Vec<TrackFragment<'a>>,
}

impl<'a> MovieFragment<'a> {
    /// Parse a `moof` container.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed child.
    pub fn parse(moof: &IsoBox<'a>) -> Result<Self> {
        let mut sequence_number = 0;
        let mut tracks = Vec::new();
        for child in moof.children() {
            let child = child?;
            match child.kind() {
                boxes::MFHD => {
                    let full = child.full()?;
                    let mut r = full.reader();
                    sequence_number = r.be32();
                }
                boxes::TRAF if tracks.len() < MAX_TRAF_PER_MOOF => {
                    tracks.push(TrackFragment::parse(&child)?);
                }
                _ => {}
            }
        }
        Ok(Self {
            sequence_number,
            offset: moof.offset,
            tracks,
        })
    }

    /// The data base for track fragment `index`, per §8.8.7.1.
    ///
    /// `defaults_for` supplies the `trex` row for a track id, needed because
    /// resolving case 3 means measuring where the preceding fragment's data
    /// ended, which needs its sample sizes, which may come from `trex`.
    #[must_use]
    pub fn track_base<F>(&self, index: usize, mut defaults_for: F) -> Option<u64>
    where
        F: FnMut(u32) -> TrackExtends,
    {
        let traf = self.tracks.get(index)?;
        if let Some(b) = traf.header.base_data_offset {
            return Some(b);
        }
        if traf.header.default_base_is_moof {
            return Some(self.offset);
        }
        // Case 3: the first track fragment bases on the `moof`, and each later
        // one on the end of its predecessor's data.
        let mut base = self.offset;
        for i in 0..index {
            let prev = self.tracks.get(i)?;
            let prev_base = if let Some(b) = prev.header.base_data_offset {
                b
            } else if prev.header.default_base_is_moof {
                self.offset
            } else {
                base
            };
            base = prev.data_end(prev_base, &defaults_for(prev.header.track_id));
        }
        Some(base)
    }
}

/// One `sidx` reference (§8.16.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentReference {
    /// `false` for media, `true` for a nested `sidx`.
    pub is_index: bool,
    /// Bytes the referenced item occupies.
    pub referenced_size: u32,
    /// Duration in the `sidx`'s own timescale.
    pub subsegment_duration: u32,
    /// Whether the subsegment starts with a stream access point.
    pub starts_with_sap: bool,
    /// SAP type, three bits.
    pub sap_type: u8,
    /// SAP delta time.
    pub sap_delta_time: u32,
}

/// `sidx` — the segment index (§8.16.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentIndex {
    /// The track this index refers to.
    pub reference_id: u32,
    /// Timescale of [`SegmentIndex::earliest_presentation_time`] and the
    /// reference durations.
    pub timescale: u32,
    /// Earliest presentation time of the first subsegment.
    pub earliest_presentation_time: u64,
    /// Bytes between the end of this box and the first referenced item.
    pub first_offset: u64,
    /// Absolute file offset of the first byte after this `sidx`.
    pub anchor: u64,
    /// The references, in order.
    pub references: Vec<SegmentReference>,
}

impl SegmentIndex {
    /// Parse a `sidx` box. `anchor` is the offset just past the box.
    #[must_use]
    pub fn parse(sidx: &IsoBox<'_>) -> Option<Self> {
        let full = sidx.full().ok()?;
        let mut r = full.reader();
        let reference_id = r.be32();
        let timescale = r.be32();
        let (earliest_presentation_time, first_offset) = if full.version == 0 {
            (u64::from(r.be32()), u64::from(r.be32()))
        } else {
            (r.be64(), r.be64())
        };
        let _reserved = r.be16();
        let declared = r.be16();
        r.check().ok()?;
        let rest = full.body.get(r.pos()..).unwrap_or(&[]);
        // `reference_count` is 16 bits, so it is already bounded at 65 535
        // references; `EntryTable` then clamps it against the payload.
        let table = EntryTable::new(rest, 12, u32::from(declared));
        let mut references = Vec::new();
        for i in 0..table.len() {
            let a = table.get_u32(i, 0)?;
            let b = table.get_u32(i, 4)?;
            let c = table.get_u32(i, 8)?;
            references.push(SegmentReference {
                is_index: a >> 31 == 1,
                referenced_size: a & 0x7FFF_FFFF,
                subsegment_duration: b,
                starts_with_sap: c >> 31 == 1,
                sap_type: ((c >> 28) & 0x7) as u8,
                sap_delta_time: c & 0x0FFF_FFFF,
            });
        }
        Some(Self {
            reference_id,
            timescale,
            earliest_presentation_time,
            first_offset,
            anchor: sidx.offset.saturating_add(sidx.header.size),
            references,
        })
    }

    /// Byte offset and presentation time of each referenced subsegment.
    ///
    /// Offsets accumulate from `anchor + first_offset`; times from
    /// `earliest_presentation_time`. Both saturate rather than wrap, so a
    /// crafted index yields nonsense positions rather than aliasing back onto
    /// real ones.
    pub fn subsegments(&self) -> impl Iterator<Item = (u64, u64, SegmentReference)> + '_ {
        let mut at = self.anchor.saturating_add(self.first_offset);
        let mut time = self.earliest_presentation_time;
        self.references.iter().map(move |r| {
            let here = (at, time, *r);
            at = at.saturating_add(u64::from(r.referenced_size));
            time = time.saturating_add(u64::from(r.subsegment_duration));
            here
        })
    }
}

/// One `tfra` entry (§8.8.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RandomAccessEntry {
    /// Presentation time in the track's media timescale.
    pub time: u64,
    /// Absolute offset of the `moof` containing the sample.
    pub moof_offset: u64,
    /// One-based `traf` number within that `moof`.
    pub traf_number: u32,
    /// One-based `trun` number within that `traf`.
    pub trun_number: u32,
    /// One-based sample number within that `trun`.
    pub sample_number: u32,
}

/// `tfra` — the per-track random access table (§8.8.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackFragmentRandomAccess {
    /// The track this table indexes.
    pub track_id: u32,
    /// The entries, ascending in time.
    pub entries: Vec<RandomAccessEntry>,
}

impl TrackFragmentRandomAccess {
    /// Parse a `tfra` full box.
    #[must_use]
    pub fn parse(tfra: &IsoBox<'_>) -> Option<Self> {
        let full = tfra.full().ok()?;
        let mut r = full.reader();
        let track_id = r.be32();
        let packed = r.be32();
        let declared = r.be32();
        r.check().ok()?;
        // The three "length_size" fields hold `len - 1`, so each is 1..=4.
        let traf_len = (((packed >> 4) & 0x3) as usize).saturating_add(1);
        let trun_len = (((packed >> 2) & 0x3) as usize).saturating_add(1);
        let sample_len = ((packed & 0x3) as usize).saturating_add(1);
        let time_len: usize = if full.version == 1 { 8 } else { 4 };
        let stride = time_len
            .saturating_mul(2)
            .saturating_add(traf_len)
            .saturating_add(trun_len)
            .saturating_add(sample_len);
        let rest = full.body.get(r.pos()..).unwrap_or(&[]);
        let table = EntryTable::new(rest, stride, declared.min(MAX_TFRA_ENTRIES));
        let mut entries = Vec::new();
        for i in 0..table.len() {
            let mut e = table.reader_at(i)?;
            let time = if full.version == 1 {
                e.be64()
            } else {
                u64::from(e.be32())
            };
            let moof_offset = if full.version == 1 {
                e.be64()
            } else {
                u64::from(e.be32())
            };
            let traf_number = read_var(&mut e, traf_len);
            let trun_number = read_var(&mut e, trun_len);
            let sample_number = read_var(&mut e, sample_len);
            if e.overrun() {
                break;
            }
            entries.push(RandomAccessEntry {
                time,
                moof_offset,
                traf_number,
                trun_number,
                sample_number,
            });
        }
        Some(Self { track_id, entries })
    }

    /// The last entry at or before `time`.
    #[must_use]
    pub fn at_or_before(&self, time: u64) -> Option<RandomAccessEntry> {
        let at = self.entries.partition_point(|e| e.time <= time);
        self.entries.get(at.checked_sub(1)?).copied()
    }
}

fn read_var(r: &mut vaco_bitstream::ByteReader<'_>, len: usize) -> u32 {
    let mut v = 0u32;
    for _ in 0..len.min(4) {
        v = (v << 8) | u32::from(r.u8());
    }
    v
}

/// Collect the `trex` rows from a `mvex` container.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed child.
pub fn parse_mvex(mvex: &IsoBox<'_>) -> Result<Vec<TrackExtends>> {
    let mut out = Vec::new();
    for child in mvex.children() {
        let child = child?;
        if child.kind() == boxes::TREX {
            out.push(TrackExtends::parse(&child.full()?));
        }
    }
    Ok(out)
}

/// Collect the `tfra` tables from an `mfra` container.
#[must_use]
pub fn parse_mfra(mfra: &IsoBox<'_>) -> Vec<TrackFragmentRandomAccess> {
    let mut out = Vec::new();
    for child in mfra.children().flatten() {
        if child.kind() == boxes::TFRA
            && let Some(t) = TrackFragmentRandomAccess::parse(&child)
        {
            out.push(t);
        }
    }
    out
}

/// Iterate the `moof` boxes in a slice of top-level boxes.
pub fn movie_fragments(iter: BoxIter<'_>) -> impl Iterator<Item = Result<MovieFragment<'_>>> {
    iter.filter_map(|b| match b {
        Ok(b) if b.kind() == boxes::MOOF => Some(MovieFragment::parse(&b)),
        Ok(_) => None,
        Err(e) => Some(Err(e)),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::testutil::{bx, first_box, fullbx};

    fn tfhd(flags: u32, track: u32, values: &[u64]) -> Vec<u8> {
        let mut b = track.to_be_bytes().to_vec();
        let mut it = values.iter();
        if flags & TF_BASE_DATA_OFFSET != 0 {
            b.extend_from_slice(&it.next().copied().unwrap_or(0).to_be_bytes());
        }
        for bit in [
            TF_SAMPLE_DESCRIPTION_INDEX,
            TF_DEFAULT_SAMPLE_DURATION,
            TF_DEFAULT_SAMPLE_SIZE,
            TF_DEFAULT_SAMPLE_FLAGS,
        ] {
            if flags & bit != 0 {
                b.extend_from_slice(&(it.next().copied().unwrap_or(0) as u32).to_be_bytes());
            }
        }
        fullbx(b"tfhd", 0, flags, &b)
    }

    fn trun(
        version: u8,
        flags: u32,
        data_offset: Option<i32>,
        first_flags: Option<u32>,
        rows: &[[u32; 4]],
    ) -> Vec<u8> {
        // The presence bits and the bytes written must agree, or the entry
        // table starts one word late and every size is read from the wrong
        // place. Derive the bits from the arguments so they cannot drift.
        let flags = flags
            | if data_offset.is_some() {
                TR_DATA_OFFSET
            } else {
                0
            }
            | if first_flags.is_some() {
                TR_FIRST_SAMPLE_FLAGS
            } else {
                0
            };
        let mut b = u32::try_from(rows.len()).unwrap().to_be_bytes().to_vec();
        if let Some(d) = data_offset {
            b.extend_from_slice(&d.to_be_bytes());
        }
        if let Some(f) = first_flags {
            b.extend_from_slice(&f.to_be_bytes());
        }
        for row in rows {
            for (i, bit) in [
                TR_SAMPLE_DURATION,
                TR_SAMPLE_SIZE,
                TR_SAMPLE_FLAGS,
                TR_SAMPLE_CTS_OFFSET,
            ]
            .into_iter()
            .enumerate()
            {
                if flags & bit != 0 {
                    b.extend_from_slice(&row[i].to_be_bytes());
                }
            }
        }
        fullbx(b"trun", version, flags, &b)
    }

    /// The measured `frag.mp4`: `tfhd` with `base_data_offset = 1259`,
    /// defaults 512/4822, `trun` with `data_offset = 516` and per-sample size
    /// plus composition offset. `ffprobe` reported `pos=1775`.
    #[test]
    fn the_measured_fragment_places_its_first_sample_at_the_reference_offset() {
        let mut traf_body = tfhd(
            TF_BASE_DATA_OFFSET
                | TF_DEFAULT_SAMPLE_DURATION
                | TF_DEFAULT_SAMPLE_SIZE
                | TF_DEFAULT_SAMPLE_FLAGS,
            1,
            &[1259, 512, 4822, 0x0101_0000],
        );
        traf_body.extend_from_slice(&fullbx(b"tfdt", 1, 0, &0u64.to_be_bytes()));
        traf_body.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE | TR_SAMPLE_CTS_OFFSET,
            Some(516),
            Some(0x0200_0000),
            &[[0, 4822, 0, 1024], [0, 1668, 0, 2048], [0, 1011, 0, 512]],
        ));
        let mut moof_body = fullbx(b"mfhd", 0, 0, &1u32.to_be_bytes());
        moof_body.extend_from_slice(&bx(b"traf", &traf_body));
        let raw = bx(b"moof", &moof_body);
        let moof = MovieFragment::parse(&first_box(&raw)).unwrap();
        assert_eq!(moof.sequence_number, 1);
        let base = moof.track_base(0, |_| TrackExtends::default()).unwrap();
        assert_eq!(base, 1259);
        let s: Vec<_> = moof.tracks[0]
            .samples(base, 0, &TrackExtends::default())
            .collect();
        assert_eq!(s[0].offset, 1775);
        assert_eq!(s[1].offset, 1775 + 4822);
        assert_eq!(s[2].offset, 1775 + 4822 + 1668);
        // pts/dts as ffprobe printed them: pts=1024 dts=0, pts=2560 dts=512.
        assert_eq!((s[0].dts, s[0].pts()), (0, 1024));
        assert_eq!((s[1].dts, s[1].pts()), (512, 2560));
        assert!(s[0].is_sync());
        assert!(!s[1].is_sync());
        assert_eq!(s[0].duration, 512);
    }

    /// The measured `dbm.mp4`: `default-base-is-moof`, no `base_data_offset`,
    /// `moof` at 769, `data_offset = 152`. `ffprobe` reported `pos=921`.
    #[test]
    fn default_base_is_moof_bases_on_the_moof() {
        let mut traf_body = tfhd(
            TF_DEFAULT_BASE_IS_MOOF
                | TF_DEFAULT_SAMPLE_DURATION
                | TF_DEFAULT_SAMPLE_SIZE
                | TF_DEFAULT_SAMPLE_FLAGS,
            1,
            &[1024, 2116, 0x0101_0000],
        );
        traf_body.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE,
            Some(152),
            Some(0x0200_0000),
            &[[0, 2116, 0, 0], [0, 65, 0, 0], [0, 173, 0, 0]],
        ));
        let mut moof_body = fullbx(b"mfhd", 0, 0, &1u32.to_be_bytes());
        moof_body.extend_from_slice(&bx(b"traf", &traf_body));
        // Place the moof at 769 by parsing it at that base offset.
        let raw = bx(b"moof", &moof_body);
        let iter = BoxIter::new(&raw, 769);
        let b = iter.flatten().next().unwrap();
        let moof = MovieFragment::parse(&b).unwrap();
        let base = moof.track_base(0, |_| TrackExtends::default()).unwrap();
        assert_eq!(base, 769);
        let s: Vec<_> = moof.tracks[0]
            .samples(base, 0, &TrackExtends::default())
            .collect();
        assert_eq!(s[0].offset, 921);
        assert_eq!(s[1].offset, 921 + 2116);
        assert_eq!(s[0].duration, 1024);
    }

    #[test]
    fn the_defaults_cascade_runs_trun_then_tfhd_then_trex() {
        let trex = TrackExtends {
            track_id: 1,
            default_sample_description_index: 1,
            default_sample_duration: 100,
            default_sample_size: 10,
            default_sample_flags: 0x0001_0000,
        };
        // tfhd states a size but not a duration; trun states a duration for
        // each sample but no size.
        let mut traf_body = tfhd(TF_DEFAULT_SAMPLE_SIZE, 1, &[42]);
        traf_body.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_DURATION,
            Some(0),
            None,
            &[[7, 0, 0, 0], [9, 0, 0, 0]],
        ));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        let s: Vec<_> = traf.samples(1000, 0, &trex).collect();
        assert_eq!(s[0].duration, 7); // from trun
        assert_eq!(s[0].size, 42); // from tfhd
        assert_eq!(s[0].flags.0, 0x0001_0000); // from trex
        assert_eq!(s[0].description_index, 1); // from trex
        assert_eq!(s[1].duration, 9);
        assert_eq!(s[1].offset, 1042);
        assert_eq!(s[1].dts, 7);
    }

    #[test]
    fn first_sample_flags_apply_to_the_first_sample_only() {
        let mut traf_body = tfhd(
            TF_DEFAULT_SAMPLE_SIZE | TF_DEFAULT_SAMPLE_FLAGS,
            1,
            &[8, 0x0001_0000],
        );
        traf_body.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_FIRST_SAMPLE_FLAGS,
            Some(0),
            Some(0x0200_0000),
            &[[0; 4], [0; 4], [0; 4]],
        ));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        let samples: Vec<_> = traf.samples(0, 0, &TrackExtends::default()).collect();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].flags.0, 0x0200_0000);
        assert_eq!(samples[1].flags.0, 0x0001_0000);
        assert_eq!(samples[2].flags.0, 0x0001_0000);
    }

    #[test]
    fn a_run_without_per_sample_fields_honours_its_bounded_count() {
        // A default-only run has no per-sample bytes to prove its count, so a
        // four-billion declaration is honoured only up to the traf-wide cap.
        let mut traf_body = tfhd(TF_DEFAULT_SAMPLE_SIZE, 1, &[8]);
        let mut b = u32::MAX.to_be_bytes().to_vec();
        b.extend_from_slice(&0i32.to_be_bytes());
        traf_body.extend_from_slice(&fullbx(b"trun", 0, TR_DATA_OFFSET, &b));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        assert_eq!(traf.sample_count(), u64::from(MAX_SAMPLES_PER_TRAF));
        let samples = traf.samples(0, 0, &TrackExtends::default());
        assert_eq!(samples.remaining, MAX_SAMPLES_PER_TRAF);
    }

    #[test]
    fn two_runs_without_a_second_data_offset_continue_from_the_first() {
        let mut traf_body = tfhd(TF_DEFAULT_SAMPLE_DURATION, 1, &[10]);
        traf_body.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE,
            Some(100),
            None,
            &[[0, 20, 0, 0]],
        ));
        traf_body.extend_from_slice(&trun(0, TR_SAMPLE_SIZE, None, None, &[[0, 30, 0, 0]]));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        let s: Vec<_> = traf.samples(1000, 0, &TrackExtends::default()).collect();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].offset, 1100);
        assert_eq!(s[1].offset, 1120);
        assert_eq!(s[1].dts, 10);
    }

    #[test]
    fn a_second_traf_without_an_offset_bases_on_the_first_ones_data_end() {
        let mut a = tfhd(TF_DEFAULT_SAMPLE_DURATION, 1, &[10]);
        a.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE,
            Some(8),
            None,
            &[[0, 40, 0, 0]],
        ));
        let mut b = tfhd(TF_DEFAULT_SAMPLE_DURATION, 2, &[10]);
        b.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE,
            Some(0),
            None,
            &[[0, 5, 0, 0]],
        ));
        let mut moof_body = fullbx(b"mfhd", 0, 0, &1u32.to_be_bytes());
        moof_body.extend_from_slice(&bx(b"traf", &a));
        moof_body.extend_from_slice(&bx(b"traf", &b));
        let raw = bx(b"moof", &moof_body);
        let moof = MovieFragment::parse(&first_box(&raw)).unwrap();
        assert_eq!(moof.track_base(0, |_| TrackExtends::default()), Some(0));
        // First traf's data runs 8..48, so the second bases at 48.
        assert_eq!(moof.track_base(1, |_| TrackExtends::default()), Some(48));
    }

    #[test]
    fn a_negative_data_offset_moves_backwards_from_the_base() {
        let mut traf_body = tfhd(
            TF_BASE_DATA_OFFSET | TF_DEFAULT_SAMPLE_DURATION,
            1,
            &[1000, 1],
        );
        traf_body.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE,
            Some(-100),
            None,
            &[[0, 4, 0, 0]],
        ));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        let s: Vec<_> = traf.samples(1000, 0, &TrackExtends::default()).collect();
        assert_eq!(s[0].offset, 900);
    }

    #[test]
    fn a_traf_without_a_tfhd_is_an_error() {
        let raw = bx(b"traf", &fullbx(b"tfdt", 0, 0, &0u32.to_be_bytes()));
        assert!(TrackFragment::parse(&first_box(&raw)).is_err());
    }

    #[test]
    fn tfdt_version_one_is_sixty_four_bit() {
        let mut traf_body = tfhd(0, 1, &[]);
        traf_body.extend_from_slice(&fullbx(b"tfdt", 1, 0, &0x1_0000_0000u64.to_be_bytes()));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        assert_eq!(traf.base_media_decode_time, Some(0x1_0000_0000));
    }

    #[test]
    fn sample_flags_decode_the_sync_bit_and_its_alias() {
        assert!(SampleFlags(0).is_sync());
        assert!(!SampleFlags(0x0001_0000).is_sync());
        // depends_on == 2 means intra, which overrides a wrongly set non-sync
        // bit — encoders disagree about which they write.
        assert!(SampleFlags(0x0201_0000).is_sync());
        assert_eq!(SampleFlags(0x0200_0000).depends_on(), 2);
        assert_eq!(SampleFlags(0x0101_0000).depends_on(), 1);
        assert_eq!(SampleFlags(0x0000_1234).degradation_priority(), 0x1234);
    }

    #[test]
    fn trun_version_zero_composition_offsets_clamp_instead_of_wrapping() {
        let mut traf_body = tfhd(TF_DEFAULT_SAMPLE_DURATION, 1, &[1]);
        traf_body.extend_from_slice(&trun(
            0,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE | TR_SAMPLE_CTS_OFFSET,
            Some(0),
            None,
            &[[0, 1, 0, 0xFFFF_FFFF]],
        ));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        let s: Vec<_> = traf.samples(0, 0, &TrackExtends::default()).collect();
        assert_eq!(s[0].cts_offset, i32::MAX);
    }

    #[test]
    fn trun_version_one_composition_offsets_are_signed() {
        let mut traf_body = tfhd(TF_DEFAULT_SAMPLE_DURATION, 1, &[1]);
        traf_body.extend_from_slice(&trun(
            1,
            TR_DATA_OFFSET | TR_SAMPLE_SIZE | TR_SAMPLE_CTS_OFFSET,
            Some(0),
            None,
            &[[0, 1, 0, (-512i32) as u32]],
        ));
        let raw = bx(b"traf", &traf_body);
        let traf = TrackFragment::parse(&first_box(&raw)).unwrap();
        let s: Vec<_> = traf.samples(0, 0, &TrackExtends::default()).collect();
        assert_eq!(s[0].cts_offset, -512);
        assert_eq!(s[0].pts(), -512);
    }

    #[test]
    fn a_sidx_accumulates_offsets_and_times() {
        let mut b = 1u32.to_be_bytes().to_vec(); // reference_id
        b.extend_from_slice(&90_000u32.to_be_bytes()); // timescale
        b.extend_from_slice(&0u32.to_be_bytes()); // earliest_presentation_time
        b.extend_from_slice(&0u32.to_be_bytes()); // first_offset
        b.extend_from_slice(&0u16.to_be_bytes()); // reserved
        b.extend_from_slice(&2u16.to_be_bytes()); // reference_count
        for (size, dur) in [(1000u32, 90_000u32), (2000, 45_000)] {
            b.extend_from_slice(&size.to_be_bytes());
            b.extend_from_slice(&dur.to_be_bytes());
            b.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let raw = fullbx(b"sidx", 0, 0, &b);
        let sidx = SegmentIndex::parse(&first_box(&raw)).unwrap();
        assert_eq!(sidx.references.len(), 2);
        assert!(sidx.references[0].starts_with_sap);
        assert_eq!(sidx.references[0].sap_type, 1);
        let subs: Vec<_> = sidx.subsegments().collect();
        assert_eq!(subs[0].0, sidx.anchor);
        assert_eq!(subs[1].0, sidx.anchor + 1000);
        assert_eq!(subs[1].1, 90_000);
    }

    #[test]
    fn a_sidx_reference_count_beyond_the_payload_is_clamped() {
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&u16::MAX.to_be_bytes());
        b.extend_from_slice(&[0; 12]);
        let raw = fullbx(b"sidx", 0, 0, &b);
        let sidx = SegmentIndex::parse(&first_box(&raw)).unwrap();
        assert_eq!(sidx.references.len(), 1);
    }

    #[test]
    fn a_tfra_reads_its_variable_width_fields() {
        // The three `length_size` fields hold (len - 1) in bits 5:4, 3:2 and
        // 1:0 — so traf 1 byte, trun 1 byte, sample 2 bytes.
        let packed = 0b00_00_01u32;
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(&packed.to_be_bytes());
        b.extend_from_slice(&2u32.to_be_bytes());
        for (t, m) in [(0u32, 100u32), (9000, 5000)] {
            b.extend_from_slice(&t.to_be_bytes());
            b.extend_from_slice(&m.to_be_bytes());
            b.push(1);
            b.push(1);
            b.extend_from_slice(&1u16.to_be_bytes());
        }
        let raw = fullbx(b"tfra", 0, 0, &b);
        let tfra = TrackFragmentRandomAccess::parse(&first_box(&raw)).unwrap();
        assert_eq!(tfra.entries.len(), 2);
        assert_eq!(tfra.entries[1].moof_offset, 5000);
        assert_eq!(tfra.entries[1].sample_number, 1);
        assert_eq!(tfra.at_or_before(8999).unwrap().moof_offset, 100);
        assert_eq!(tfra.at_or_before(9000).unwrap().moof_offset, 5000);
        assert!(tfra.at_or_before(0).is_some());
    }

    #[test]
    fn mvex_yields_one_trex_per_track() {
        let mut body = Vec::new();
        for id in 1..=2u32 {
            let mut b = id.to_be_bytes().to_vec();
            b.extend_from_slice(&1u32.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&fullbx(b"trex", 0, 0, &b));
        }
        let raw = bx(b"mvex", &body);
        let trex = parse_mvex(&first_box(&raw)).unwrap();
        assert_eq!(trex.len(), 2);
        assert_eq!(trex[1].track_id, 2);
    }
}
