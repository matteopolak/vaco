//! Demuxer-level options.
//!
//! Names, types and defaults are **interface facts** (D9) read from the pinned
//! reference with `ffmpeg -h demuxer=mov`. The reference exposes 21; the ones
//! below are the ones this crate acts on. The rest are listed in
//! `docs/format/vaco-demux-mp4.md` under *Not implemented*, so nobody has to
//! guess whether an absent option was forgotten or declined.

/// Options the MP4 demuxer understands.
///
/// A flat set of independent switches rather than a state enum, because that is
/// exactly what the reference's option table is and every one of them is
/// separately settable from the command line.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per reference option; grouping them would break the 1:1 mapping the CLI needs"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp4Options {
    /// `-ignore_editlist` — do not apply `elst` at all.
    ///
    /// With this set a track reports raw media timestamps: no trim, no empty-
    /// edit delay, no discard flags.
    pub ignore_editlist: bool,
    /// `-ignore_chapters`.
    pub ignore_chapters: bool,
    /// `-use_tfdt` — trust `tfdt` for a fragment's base decode time rather than
    /// accumulating durations.
    pub use_tfdt: bool,
    /// `-enable_drefs` — follow a `dref` entry that points at another file.
    ///
    /// **Refused even when set** (plan 18 §3.1.10): opening a second file
    /// because the first one's bytes asked us to is a file-system read
    /// triggered by content. The option exists so that the refusal is
    /// reportable rather than silent.
    pub enable_drefs: bool,
    /// `-interleaved_read` — emit packets from every track in one DTS-ordered
    /// stream. With this off, each track is drained in turn.
    pub interleaved_read: bool,
    /// `-seek_streams_individually` — after a seek, place every track at its
    /// own nearest preceding sample rather than at the reference track's
    /// position.
    pub seek_streams_individually: bool,
    /// `-max_stts_delta` — a `stts` delta above this is treated as invalid.
    pub max_stts_delta: u32,
    /// `-decryption_key` — a single AES-128 key (16 bytes) applied to every
    /// `cenc`-protected track, given a real per-sample IV to decrypt with
    /// (`senc`, non-fragmented only — see `docs/format/vaco-demux-mp4.md`).
    ///
    /// The reference's `-decryption_keys` (per-`KID` dictionary) is not
    /// implemented: one key for every protected track is what this option
    /// alone already means, and is enough for the common single-key case.
    pub decryption_key: Option<[u8; 16]>,
}

impl Default for Mp4Options {
    fn default() -> Self {
        Self {
            ignore_editlist: false,
            ignore_chapters: false,
            use_tfdt: true,
            enable_drefs: false,
            interleaved_read: true,
            seek_streams_individually: true,
            max_stts_delta: 4_294_487_295,
            decryption_key: None,
        }
    }
}
