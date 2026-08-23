//! `-movflags`, the brand this file writes, and everything else that decides
//! *shape* rather than being derived from the streams themselves.
//!
//! This is not routed through `vaco_format_core::FormatOptions` — that type is
//! the options every container shares, and `movflags` is MP4-specific in the
//! same way `AviMuxer` and `OggMuxer` take their own private construction
//! arguments rather than growing the shared table. [`MovMuxer::with_options`]
//! is the entry point a caller who needs anything beyond the registry's
//! default construction uses.

use vaco_core::TimeBase;

bitflags::bitflags! {
    /// `-movflags`, one bit per flag exactly as the reference names them.
    ///
    /// The *names* are interface facts (D9); the bit values are ours; nothing
    /// outside this crate observes them.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct MovFlags: u32 {
        /// Move `moov` to the front after the fact (§ this crate's `progressive` module).
        const FASTSTART = 1 << 0;
        /// Write `moov` immediately, with empty sample tables, before any
        /// sample arrives. Implied by every fragmented mode this crate has,
        /// since a fragmented file's initial `moov` never carries samples;
        /// kept as a distinct flag because a caller may ask for it explicitly.
        const EMPTY_MOOV = 1 << 1;
        /// Start a new fragment at every packet marked a keyframe.
        const FRAG_KEYFRAME = 1 << 2;
        /// Start a new fragment at every packet.
        const FRAG_EVERY_FRAME = 1 << 3;
        /// `tfhd.base_data_offset` is omitted; `trun.data_offset` is relative
        /// to the start of the enclosing `moof` instead.
        const DEFAULT_BASE_MOOF = 1 << 4;
        /// Omit `tfhd`'s `base_data_offset` field outright (paired with
        /// `default_base_moof` in every file this crate has measured, and
        /// refused on its own — see [`MuxOptions::validate`]).
        const OMIT_TFHD_OFFSET = 1 << 5;
        /// One `moof`+`mdat` pair per track per fragment interval, instead of
        /// one `moof` covering every track.
        const SEPARATE_MOOF = 1 << 6;
        /// DASH-friendly output: implies `default_base_moof`, and buffers the
        /// whole fragment sequence so a `sidx` can be written after `moov`.
        const DASH = 1 << 7;
        /// CMAF-friendly output: the same shape as `dash` for this crate's
        /// purposes (see `docs/format/vaco-mux-mp4.md` for what CMAF
        /// conformance this does *not* attempt: chunk-level `styp`
        /// alignment and CMAF's stricter brand rules are out of scope).
        const CMAF = 1 << 8;
        /// Fragment mode is active at all. Set automatically whenever any of
        /// `frag_keyframe`/`frag_every_frame`/`frag_duration`/`frag_size` is
        /// requested via [`MuxOptions`]; not user-facing on its own, but kept
        /// as a flag so [`MovMuxer`](crate::mux::MovMuxer) has one thing to
        /// check rather than four.
        const FRAGMENTED = 1 << 9;
    }
}

impl MovFlags {
    /// Whether any of this crate's fragmentation triggers is active.
    #[must_use]
    pub const fn is_fragmented(self) -> bool {
        self.contains(Self::FRAGMENTED)
            || self.contains(Self::FRAG_KEYFRAME)
            || self.contains(Self::FRAG_EVERY_FRAME)
    }

    /// Whether `moov` should be written empty, up front.
    ///
    /// True whenever fragmented mode is active at all: this crate's
    /// fragmented `moov` never carries samples, `empty_moov` or not — see the
    /// crate-level docs' *What is deferred* note on the distinction
    /// `ffmpeg`'s buffering makes that this crate does not model.
    #[must_use]
    pub const fn writes_empty_moov(self, fragmented: bool) -> bool {
        fragmented || self.contains(Self::EMPTY_MOOV)
    }
}

/// A brand-variant container profile: the `ftyp` major brand and compatible
/// list, plus which of them (`ipod`/`psp`) constrain codec choice. See
/// `docs/format/vaco-mux-mp4.md` for the exact bytes each was measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Brand {
    #[default]
    Mp4,
    Mov,
    Ipod,
    Ismv,
    F4v,
    Psp,
    ThreeGp,
    ThreeG2,
    Avif,
}

/// One chapter mark: presentation time and title.
#[derive(Debug, Clone)]
pub struct ChapterMark {
    pub start: vaco_core::Timestamp,
    pub time_base: TimeBase,
    pub title: String,
}

/// Cover art to embed in `udta ▸ meta ▸ ilst ▸ covr`.
#[derive(Debug, Clone)]
pub struct CoverArt {
    pub is_png: bool,
    pub data: Vec<u8>,
}

/// Everything this crate needs that is not a stream: the brand, `movflags`,
/// fragmentation thresholds, timestamps and iTunes-style tags.
#[derive(Debug, Clone, Default)]
pub struct MuxOptions {
    pub brand: Brand,
    pub movflags: MovFlags,
    /// Fragment boundary: elapsed time since the fragment started, in the
    /// *primary* track's time base (its first `add_stream` video track, or
    /// its first track if none is video). `None` disables the trigger.
    pub frag_duration: Option<vaco_core::Duration>,
    /// Fragment boundary: accumulated fragment payload bytes. `None` disables
    /// the trigger.
    pub frag_size: Option<u64>,
    /// Unix seconds to stamp into `mvhd`/`tkhd`/`mdhd`. `None` writes `0` —
    /// the value `ffmpeg 8.1` itself writes absent explicit metadata
    /// (measured: `ffmpeg -f lavfi -i testsrc=d=1 -c:v mpeg4 out.mp4` with no
    /// `-metadata creation_time` writes an all-zero `mvhd` timestamp pair).
    pub creation_time_unix: Option<i64>,
    /// Suppresses [`MuxOptions::creation_time_unix`] even when set, mirroring
    /// `-fflags +bitexact` on the reference (`vaco_format_isom::movie`'s own
    /// docs note the same suppression).
    pub bitexact: bool,
    /// iTunes-style tags: `(fourcc key, value)`, e.g. `(*b"\xa9nam", "Title")`.
    pub tags: Vec<([u8; 4], String)>,
    pub cover_art: Option<CoverArt>,
    pub chapters: Vec<ChapterMark>,
}

impl MuxOptions {
    /// Whether this configuration is internally consistent.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::Unsupported`] for `omit_tfhd_offset` without
    /// `default_base_moof` — the file that combination produces has no way
    /// to state a fragment's data offset at all, and every writer this
    /// crate's docs cite pairs the two.
    pub fn validate(&self) -> vaco_core::Result<()> {
        if self.movflags.contains(MovFlags::OMIT_TFHD_OFFSET)
            && !self
                .movflags
                .intersects(MovFlags::DEFAULT_BASE_MOOF | MovFlags::DASH | MovFlags::CMAF)
        {
            return Err(vaco_core::Error::Unsupported(
                "mp4: omit_tfhd_offset needs default_base_moof (or dash/cmaf, which imply it)",
            ));
        }
        Ok(())
    }

    /// The effective `movflags`, with `dash`/`cmaf`'s implications folded in.
    #[must_use]
    pub fn effective_flags(&self) -> MovFlags {
        let mut f = self.movflags;
        if f.intersects(MovFlags::DASH | MovFlags::CMAF) {
            f |= MovFlags::DEFAULT_BASE_MOOF;
        }
        if f.is_fragmented() {
            f |= MovFlags::FRAGMENTED;
        }
        f
    }
}
