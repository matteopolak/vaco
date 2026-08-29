//! What a muxer needs that is not a stream of packets: a title, per-file
//! tags, a chapter list, attachments, and per-stream tags.
//!
//! # Why this exists
//!
//! [`crate::mux::MuxBuilder::add_stream`] carries only [`CodecParameters`], so
//! before this module there was no channel at all for anything Matroska's
//! `Tags`/`Chapters`/`Attachments`, MP4's `udta▸meta▸ilst`/`chpl`, or a plain
//! `ffmetadata` file needed to write. `vaco-mux-matroska`, `vaco-mux-mp4` and
//! `vaco-mux-stream` each reported the same absence independently — see
//! `planning/INTERFACE-GAPS.md` gap 1.
//!
//! # How it works
//!
//! [`MuxMetadata`] is a plain, container-agnostic bundle. A caller fills it in
//! (or leaves it at [`MuxMetadata::default`], which every existing call site
//! effectively does today) and hands it to
//! [`crate::mux::MuxBuilder::with_metadata`]. `MuxBuilder::open` then calls
//! [`crate::Muxer::set_metadata`] once, after `init` and after stream time
//! bases are settled but before the header is written (M30) — the same point
//! a title or a chapter table has to be known by for any container that
//! writes it into its own header structure.
//!
//! [`crate::Muxer::set_metadata`]'s default implementation does nothing, so
//! every muxer written before this module existed keeps compiling and keeps
//! behaving exactly as before: it already dropped this information because
//! there was nowhere to put it, and the default drops it the same way. A
//! muxer that wants to write metadata overrides the method; nothing else about
//! it needs to change.
//!
//! # How to change it
//!
//! Add a field here when a *format-independent* concept is missing — a
//! per-file language, say. A concept specific to one container (MP4's 4-byte
//! `ilst` atom codes, Matroska's `SeekHead`) belongs in that container's own
//! crate, mapped from these fields, not here: see `vaco-mux-matroska`'s module
//! doc for why `SeekHead` in particular is deliberately not modelled as data
//! at all.
//!
//! Chapters reuse [`crate::Chapter`] — the exact type [`crate::Demuxer::chapters`]
//! already returns — rather than a second, mux-only type, specifically so that
//! remuxing (`vaco -i in -c copy out`, copying chapters straight across) needs
//! no conversion step at all.

use crate::{Chapter, Program};
use vaco_core::Disposition;

/// A binary attachment: a font, a cover image, a subtitle sidecar.
///
/// Modelled after Matroska's `AttachedFile`, which is the richest of the
/// containers that write these, but nothing here is Matroska-specific: MP4's
/// cover art is exactly `data` with `mime_type` fixed to an image type and
/// `filename`/`description` unused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MuxAttachment {
    /// A display filename, e.g. `"cover.jpg"`. May be empty.
    pub filename: String,
    /// The IANA media type, e.g. `"image/jpeg"`. Never guessed here — a muxer
    /// that needs one and was not given one is exactly the case that produces
    /// a file no player opens, so this stays whatever the caller stated.
    pub mime_type: String,
    /// Free-text description. Empty means absent, matching every other
    /// optional string in this crate's model (compare
    /// [`crate::options::FormatOptions::dump_separator`]).
    pub description: String,
    pub data: Vec<u8>,
}

/// Everything about a file that is not a stream of packets.
///
/// One bundle rather than four separate `MuxBuilder` parameters: a caller
/// building this from parsed `-metadata`/`-metadata:s:N`/chapter-file options
/// fills it in once and hands it over, and a muxer overriding
/// [`crate::Muxer::set_metadata`] reads exactly the fields its container
/// format can express and ignores the rest — nothing here is mandatory for
/// any container to honour.
#[derive(Debug, Clone, Default)]
pub struct MuxMetadata {
    /// File-level tags: title, artist, encoder, and so on. Order is
    /// preserved and duplicates are allowed, mirroring
    /// [`crate::Demuxer::metadata`] on the read side.
    pub tags: Vec<(String, String)>,
    /// Chapters, in presentation order. Reuses [`Chapter`] — see the module
    /// doc for why.
    pub chapters: Vec<Chapter>,
    /// Attachments, in the order they should be written.
    pub attachments: Vec<MuxAttachment>,
    /// Per-stream tags — language, a per-stream title — indexed by the
    /// stream's position, in the order [`crate::mux::MuxBuilder::add_stream`]
    /// declared it. Shorter than the stream count means "nothing stated for
    /// the rest"; entries past the declared stream count are ignored rather
    /// than rejected, since a caller building this from a flat CLI option list
    /// before every stream exists has no earlier point to be told the count.
    pub stream_tags: Vec<Vec<(String, String)>>,
    /// Per-stream `-disposition`, indexed exactly like [`Self::stream_tags`] —
    /// same shape, same "shorter means unstated" convention, same reason
    /// (a caller building this from argv has no earlier point at which the
    /// real stream count is known). [`Disposition::empty`] at a given index
    /// means either "unstated" or "explicitly cleared" — this type cannot
    /// distinguish the two, which matches [`Self::stream_tags`]'s own
    /// inability to distinguish "no tags" from "not stated" either.
    ///
    /// This is the channel `crate::StreamSpec`'s own doc named as missing
    /// (`planning/INTERFACE-GAPS.md` gap 9's leftover): `add_stream`/
    /// `add_stream_with` run before every `-disposition` occurrence for a
    /// stream is necessarily known (a later specifier can still match an
    /// already-added stream), so disposition cannot ride on `StreamSpec`
    /// the way `time_base` does — it has to arrive with the rest of
    /// [`MuxMetadata`], at [`crate::mux::MuxBuilder::open`], same as tags.
    pub stream_disposition: Vec<Disposition>,
    /// `-program`, one entry per occurrence, in argv order. Reuses
    /// [`Program`] — the exact type [`crate::Demuxer::programs`] already
    /// returns — the same reasoning [`Chapter`] reuse gets in the module
    /// doc: a remux that copies programs across needs no conversion step.
    /// `stream_indices` are **output** stream positions (the same indexing
    /// [`Self::stream_tags`] uses), since a program groups the streams this
    /// file is actually writing, not the input's.
    ///
    /// No muxer overrides [`crate::Muxer::set_metadata`] to read this yet —
    /// it is the same "channel before writer" state `Self::tags` was in
    /// before `vaco-mux-mp4`/`vaco-mux-matroska` picked it up (see the
    /// module doc's history). MPEG-TS/MPEG-PS are the containers with a
    /// native program concept; Matroska and MP4 have none to map this onto.
    pub programs: Vec<Program>,
}

impl MuxMetadata {
    /// Whether every field is empty — the state every muxer session starts in
    /// and the one [`crate::Muxer::set_metadata`]'s default is built to accept
    /// silently.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
            && self.chapters.is_empty()
            && self.attachments.is_empty()
            && self.stream_tags.iter().all(Vec::is_empty)
            && self.stream_disposition.iter().all(|d| d.is_empty())
            && self.programs.is_empty()
    }

    /// Tags declared for `stream_index`, or an empty slice if none were.
    #[must_use]
    pub fn tags_for_stream(&self, stream_index: u32) -> &[(String, String)] {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.stream_tags.get(i))
            .map_or(&[], Vec::as_slice)
    }

    /// The disposition declared for `stream_index`, or [`Disposition::empty`]
    /// if none was — the same "shorter means unstated" reading
    /// [`Self::tags_for_stream`] gives [`Self::stream_tags`].
    #[must_use]
    pub fn disposition_for_stream(&self, stream_index: u32) -> Disposition {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.stream_disposition.get(i))
            .copied()
            .unwrap_or(Disposition::NONE)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::field_reassign_with_default,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        assert!(MuxMetadata::default().is_empty());
    }

    #[test]
    fn any_populated_field_is_not_empty() {
        let mut m = MuxMetadata::default();
        m.tags.push(("title".to_owned(), "x".to_owned()));
        assert!(!m.is_empty());

        let mut m = MuxMetadata::default();
        m.stream_tags = vec![Vec::new(), vec![("language".to_owned(), "eng".to_owned())]];
        assert!(!m.is_empty());
    }

    #[test]
    fn stream_tags_are_indexed_by_position_and_out_of_range_is_empty() {
        let mut m = MuxMetadata::default();
        m.stream_tags = vec![vec![("language".to_owned(), "eng".to_owned())], Vec::new()];
        assert_eq!(
            m.tags_for_stream(0),
            &[("language".to_owned(), "eng".to_owned())]
        );
        assert_eq!(m.tags_for_stream(1), &[] as &[(String, String)]);
        assert_eq!(m.tags_for_stream(5), &[] as &[(String, String)]);
    }

    #[test]
    fn stream_disposition_is_indexed_by_position_and_out_of_range_is_empty() {
        let mut m = MuxMetadata::default();
        m.stream_disposition = vec![Disposition::DEFAULT, Disposition::empty()];
        assert_eq!(m.disposition_for_stream(0), Disposition::DEFAULT);
        assert_eq!(m.disposition_for_stream(1), Disposition::empty());
        assert_eq!(m.disposition_for_stream(5), Disposition::empty());
        assert!(!m.is_empty());
    }

    #[test]
    fn a_stream_disposition_vec_of_only_empty_flags_is_still_empty() {
        let mut m = MuxMetadata::default();
        m.stream_disposition = vec![Disposition::empty(), Disposition::empty()];
        assert!(m.is_empty(), "no flag is actually set, so this carries no real information");
    }

    #[test]
    fn a_declared_program_is_not_empty() {
        let mut m = MuxMetadata::default();
        m.programs.push(Program::new(0));
        assert!(!m.is_empty());
    }
}
