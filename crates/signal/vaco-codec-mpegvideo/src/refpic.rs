//! B-picture display reordering (D-22a/d): the decode-order-to-display-order
//! fix-up every member of this family with bidirectional prediction needs.
//!
//! A B-picture's two references are always already decoded — that is what
//! makes it a B-picture — so the bitstream carries every reference picture
//! *before* the B-pictures that need it, which is *after* those
//! B-pictures' own display position. Every family in this group (H.263 Annex
//! O, MPEG-1/2, MPEG-4 Part 2, and the WMV/VC-1 lineage's own B-frames) uses
//! the identical fix: hold the most-recently-decoded reference picture
//! instead of emitting it immediately, and emit it only once the *next*
//! reference picture is decoded, by which point every B-picture between them
//! has already been decoded and emitted.
//!
//! Extracted and generalised from `vaco-codec-mpeg12`'s `decoder.rs`
//! (`previous`/`recent`/`held` fields), which is why this module's own
//! tests reconstruct that crate's exact I/P/B decode-order sequence as a
//! regression case.

/// Holds the two reference pictures a B-picture reads (`previous`/`recent`)
/// and the one-picture emission delay every reference picture (I or P) goes
/// through before a caller sees it.
///
/// `T` is whatever a family's decoded-picture type is (a `Frame`, or a
/// thin wrapper around one) — this buffer only ever clones and holds it, it
/// never inspects the pixels.
#[derive(Debug, Clone)]
pub struct PictureReorderBuffer<T> {
    /// The reference two pictures back (what a B-picture's forward
    /// prediction reads, and what a P-picture reads is `recent` instead).
    previous: Option<T>,
    /// The most recently *decoded* reference picture.
    recent: Option<T>,
    /// A reference picture decoded but not yet emitted, because the
    /// B-pictures that must come out before it (in display order) have not
    /// all been decoded yet.
    held: Option<T>,
}

impl<T> Default for PictureReorderBuffer<T> {
    fn default() -> Self {
        Self {
            previous: None,
            recent: None,
            held: None,
        }
    }
}

impl<T: Clone> PictureReorderBuffer<T> {
    /// A fresh buffer, as at the start of a sequence or after a flush.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The reference two pictures back, for a B-picture's forward
    /// direction. `None` before the second reference picture of a sequence
    /// has been decoded.
    #[must_use]
    pub fn previous(&self) -> Option<&T> {
        self.previous.as_ref()
    }

    /// The most recently decoded reference picture — what a P-picture reads,
    /// and a B-picture's backward direction.
    #[must_use]
    pub fn recent(&self) -> Option<&T> {
        self.recent.as_ref()
    }

    /// A non-reference picture (a B-picture): decoded and displayable
    /// immediately, since nothing depends on it. Returns it back, for a
    /// caller to hand straight to its own emission queue — this buffer does
    /// not own an output queue itself, only the delay logic reference
    /// pictures need.
    #[must_use]
    pub fn emit_non_reference(picture: T) -> T {
        picture
    }

    /// A reference picture (I or P) has just finished decoding. Returns the
    /// previously-held reference picture, if any, which is now safe to
    /// display — every B-picture between it and `picture` has already been
    /// decoded and emitted by the time a caller reaches this point, since
    /// bitstream order guarantees it.
    ///
    /// After this call, `picture` becomes both `recent` (for the next
    /// picture's prediction) and the newly held picture (for the next call
    /// to emit).
    pub fn push_reference(&mut self, picture: T) -> Option<T> {
        let ready = self.held.replace(picture.clone());
        self.previous = self.recent.replace(picture);
        ready
    }

    /// End of stream (or a discontinuity): the last held reference picture,
    /// if any, is now safe to display — nothing else in the sequence is
    /// coming to depend on it.
    pub fn drain(&mut self) -> Option<T> {
        self.held.take()
    }

    /// Reset to a fresh state, discarding every reference and held picture —
    /// a flush or a sequence boundary.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::PictureReorderBuffer;

    /// Reconstructs the classic `IBBPBB...` decode order (bitstream order
    /// carries `I P B B` for a `I B B P` display sequence) and checks that
    /// pictures come out of the buffer in **decode order for references,
    /// display order overall** — i.e. the emission sequence a caller would
    /// build by combining `push_reference`'s return with
    /// `emit_non_reference` in bitstream order is the display order.
    #[test]
    fn i_p_b_b_reorders_to_display_order() {
        let mut buf: PictureReorderBuffer<&'static str> = PictureReorderBuffer::new();
        let mut emitted = Vec::new();

        // Bitstream order: I(0) P(3) B(1) B(2) P(6) B(4) B(5).
        if let Some(r) = buf.push_reference("I0") {
            emitted.push(r);
        }
        if let Some(r) = buf.push_reference("P3") {
            emitted.push(r);
        }
        emitted.push(PictureReorderBuffer::<&'static str>::emit_non_reference("B1"));
        emitted.push(PictureReorderBuffer::<&'static str>::emit_non_reference("B2"));
        if let Some(r) = buf.push_reference("P6") {
            emitted.push(r);
        }
        emitted.push(PictureReorderBuffer::<&'static str>::emit_non_reference("B4"));
        emitted.push(PictureReorderBuffer::<&'static str>::emit_non_reference("B5"));
        if let Some(r) = buf.drain() {
            emitted.push(r);
        }

        assert_eq!(emitted, vec!["I0", "B1", "B2", "P3", "B4", "B5", "P6"]);
    }

    #[test]
    fn previous_and_recent_track_the_last_two_references() {
        let mut buf: PictureReorderBuffer<u32> = PictureReorderBuffer::new();
        assert_eq!(buf.previous(), None);
        assert_eq!(buf.recent(), None);

        buf.push_reference(1);
        assert_eq!(buf.previous(), None);
        assert_eq!(buf.recent(), Some(&1));

        buf.push_reference(2);
        assert_eq!(buf.previous(), Some(&1));
        assert_eq!(buf.recent(), Some(&2));
    }

    #[test]
    fn drain_with_nothing_held_is_none() {
        let mut buf: PictureReorderBuffer<u32> = PictureReorderBuffer::new();
        assert_eq!(buf.drain(), None);
    }

    #[test]
    fn reset_clears_everything() {
        let mut buf: PictureReorderBuffer<u32> = PictureReorderBuffer::new();
        buf.push_reference(1);
        buf.push_reference(2);
        buf.reset();
        assert_eq!(buf.previous(), None);
        assert_eq!(buf.recent(), None);
        assert_eq!(buf.drain(), None);
    }
}
