//! The open-element stack, and with it RFC 8794 section 6.2.
//!
//! The rule a streaming EBML parser turns on: an unknown-size element ends at
//! the first element that is not one of its legal children. Deciding that
//! needs two things — a schema, and knowing what is currently open — and this
//! type is the second. The schema itself is a property of whatever format is
//! built on EBML (Matroska's element tree, say), not of EBML itself, so
//! [`Stack::terminations_for`] takes it as two predicates rather than knowing
//! one.

use vaco_core::{Error, Result};

use crate::element::MAX_DEPTH;

/// One open master element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub id: u32,
    /// One past the last octet of this element's data, when known.
    pub end: Option<u64>,
}

/// The stack of open master elements.
#[derive(Debug, Default, Clone)]
pub struct Stack {
    frames: Vec<Frame>,
}

impl Stack {
    /// Deepest nesting the stack will hold.
    pub const MAX_FRAMES: usize = MAX_DEPTH as usize;

    #[must_use]
    pub const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// Open a master element.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] past [`Stack::MAX_FRAMES`].
    pub fn push(&mut self, id: u32, end: Option<u64>) -> Result<()> {
        if self.frames.len() >= Self::MAX_FRAMES {
            return Err(Error::LimitExceeded {
                limit: "ebml_depth",
                requested: self.frames.len() as u64 + 1,
                cap: Self::MAX_FRAMES as u64,
            });
        }
        self.frames.push(Frame { id, end });
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    #[must_use]
    pub fn top(&self) -> Option<Frame> {
        self.frames.last().copied()
    }

    /// The ID of the innermost open element, or `root` when nothing is open.
    #[must_use]
    pub fn open_id(&self, root: u32) -> u32 {
        self.frames.last().map_or(root, |f| f.id)
    }

    /// The nearest known end above the current position, which bounds any
    /// read.
    #[must_use]
    pub fn bound(&self) -> Option<u64> {
        self.frames.iter().filter_map(|f| f.end).min()
    }

    /// Pop every frame whose known end is at or before `pos`.
    ///
    /// Returns how many were closed.
    pub fn close_finished(&mut self, pos: u64) -> usize {
        let mut n = 0;
        while self
            .frames
            .last()
            .is_some_and(|f| f.end.is_some_and(|e| pos >= e))
        {
            self.frames.pop();
            n += 1;
        }
        n
    }

    /// How many unknown-size frames an element with ID `id` terminates.
    ///
    /// RFC 8794 section 6.2, transcribed directly: walk outward from the
    /// innermost open element; each frame whose children do not admit `id` is
    /// ended by it. Only unknown-size frames may be ended this way — a frame
    /// with a known size ends where its size says it does, so an unexpected ID
    /// inside one is a corrupt child to skip, not a terminator.
    ///
    /// `is_child_of(child, parent)` and `is_root(id)` are the caller's
    /// schema: whether `child` may legally appear directly inside `parent`
    /// (global elements should answer `true` for every `parent`), and whether
    /// `id` is a root-level element that terminates everything however deep.
    ///
    /// Returns `None` when `id` cannot be placed at all, which happens for an
    /// ID the schema does not know, or one that is only legal deeper in the
    /// tree.
    #[must_use]
    pub fn terminations_for(
        &self,
        id: u32,
        root: u32,
        is_child_of: impl Fn(u32, u32) -> bool,
        is_root: impl Fn(u32) -> bool,
    ) -> Option<usize> {
        let mut popped = 0usize;
        loop {
            let idx = self.frames.len().checked_sub(popped)?;
            let parent = if idx == 0 {
                root
            } else {
                self.frames.get(idx - 1)?.id
            };
            if is_child_of(id, parent) {
                return Some(popped);
            }
            // A root element ends everything, however deep.
            if idx == 0 {
                return is_root(id).then_some(popped);
            }
            let frame = self.frames.get(idx - 1)?;
            if frame.end.is_some() {
                // Known size: `id` is not a legal child, but the frame does
                // not end here. The caller skips the element instead.
                return None;
            }
            popped = popped.checked_add(1)?;
        }
    }

    pub fn truncate_by(&mut self, n: usize) {
        let keep = self.frames.len().saturating_sub(n);
        self.frames.truncate(keep);
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    // A tiny two-level schema for exercising the mechanism on its own:
    // ROOT(0) -> A(1) -> B(2), plus a global G(3) legal everywhere, mirroring
    // Matroska's own Segment/Cluster/SimpleBlock/Void shape without pulling in
    // a real schema table.
    const ROOT: u32 = 0;
    const A: u32 = 1;
    const B: u32 = 2;
    const G: u32 = 3;
    /// Legal directly inside `ROOT`, alongside `A`, but not inside `A` — the
    /// mini-schema's answer to Matroska's `Cues` being a `Segment` child but
    /// not a `Cluster` one.
    const C: u32 = 4;

    fn is_child_of(child: u32, parent: u32) -> bool {
        match child {
            B => parent == A,
            A | C => parent == ROOT,
            G => true,
            _ => false,
        }
    }

    fn is_root(id: u32) -> bool {
        id == A
    }

    #[test]
    fn a_sibling_ends_the_open_frame_and_a_root_ends_everything() {
        let mut stack = Stack::new();
        stack.push(A, None).unwrap();
        stack.push(B, None).unwrap();
        assert_eq!(
            stack.terminations_for(B, ROOT, is_child_of, is_root),
            Some(1)
        );
        assert_eq!(
            stack.terminations_for(G, ROOT, is_child_of, is_root),
            Some(0)
        );
        assert_eq!(
            stack.terminations_for(A, ROOT, is_child_of, is_root),
            Some(2)
        );
    }

    #[test]
    fn an_unknown_id_never_ends_anything() {
        let mut stack = Stack::new();
        stack.push(A, None).unwrap();
        assert_eq!(
            stack.terminations_for(0xFF, ROOT, is_child_of, is_root),
            None
        );
    }

    #[test]
    fn a_known_size_frame_is_never_ended_early() {
        let mut stack = Stack::new();
        stack.push(A, Some(1000)).unwrap();
        // `C` is not a legal child of `A`, but `A`'s size says where it ends,
        // so the answer is "skip", not "close".
        assert_eq!(stack.terminations_for(C, ROOT, is_child_of, is_root), None);
    }

    #[test]
    fn frames_close_when_their_end_is_reached() {
        let mut stack = Stack::new();
        stack.push(A, Some(100)).unwrap();
        stack.push(B, Some(50)).unwrap();
        assert_eq!(stack.close_finished(49), 0);
        assert_eq!(stack.close_finished(50), 1);
        assert_eq!(stack.close_finished(100), 1);
        assert!(stack.is_empty());
    }

    #[test]
    fn the_stack_refuses_to_grow_past_its_ceiling() {
        let mut stack = Stack::new();
        for _ in 0..Stack::MAX_FRAMES {
            stack.push(B, None).unwrap();
        }
        assert!(stack.push(B, None).is_err());
    }
}
