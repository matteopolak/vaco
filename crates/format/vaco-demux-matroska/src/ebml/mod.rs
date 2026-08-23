//! The Matroska schema on top of the generic EBML layer.
//!
//! The generic grammar — VINTs, the element header, the in-memory child
//! walker, the streaming header reader, and the open-element stack that
//! implements RFC 8794 section 6.2's unknown-size termination — now lives in
//! [`vaco_format_ebml`] and is re-exported here unchanged, so `vaco-mux-matroska`
//! can share the exact same definitions rather than a second copy (D19). What
//! stays in this module is Matroska-specific: the [`schema`] table (RFC 9559
//! section 5's element tree) and the functions ([`lookup`], [`is_child_of`],
//! [`is_root`]) that read it, plus a [`MatroskaStack`] wrapper that closes over that
//! schema so every existing call site in this crate keeps its exact shape.
//!
//! # Specification
//!
//! RFC 8794 sections 4 (element structure), 5 (element ID and data size
//! VINTs), 6.2 (unknown data size) and 11.2 (the EBML header elements). The
//! Matroska rows of [`schema`] come from RFC 9559 section 5.
//!
//! # The three readers, and why there are three
//!
//! | Reader | Input | Used for |
//! |---|---|---|
//! | [`Slice`] | `&[u8]` already in memory | every bounded master: `Info`, `Tracks`, `Cues`, `Tags`, … |
//! | [`read_header`] | [`IoContext`] | one element header at the current stream position |
//! | [`MatroskaStack`] | — | the open-element stack, which is what makes unknown sizes terminable |
//!
//! Bounded masters are read whole and walked in memory because that is both
//! simpler and faster; the streaming path exists because a `Cluster` may be of
//! unknown size and arbitrarily large, and because a live `WebM` stream is not
//! seekable at all.
//!
//! # Bounds
//!
//! Everything here is driven by attacker-controlled byte counts:
//!
//! * `EBMLMaxIDLength` and `EBMLMaxSizeLength` are capped at [`MAX_ID_LEN`] and
//!   [`MAX_SIZE_LEN`] regardless of what the header declares, and a declaration
//!   *larger* than the cap is rejected rather than clamped.
//! * [`Slice::children`] is a flat iterator; nesting is expressed by the caller
//!   recursing with an explicit `depth` checked against [`MAX_DEPTH`].
//! * [`MatroskaStack`] has a fixed frame ceiling and cannot grow past it.

use vaco_core::Result;
use vaco_io::IoContext;

pub mod schema;

// Re-exported unchanged from the generic crate — see the module docs above.
pub use vaco_format_ebml::{
    Caps, Child, Children, Header, MAX_DEPTH, MAX_ID_LEN, MAX_SIZE_LEN, Size, as_float, as_int,
    as_str, as_uint, read_id, read_signed_vint, read_size, vint_len,
};

/// The EBML value types RFC 8794 section 7 defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    Master,
    UInt,
    Int,
    Float,
    Str,
    Utf8,
    Binary,
    Date,
}

/// One row of the schema.
#[derive(Debug, Clone, Copy)]
pub struct ElementDef {
    pub id: u32,
    pub name: &'static str,
    pub kind: ElementKind,
    /// The ID of the only element this one may appear inside;
    /// [`schema::ROOT`] for a root element and [`schema::GLOBAL`] for a global.
    pub parent: u32,
    /// Whether the element may also appear inside itself, as `SimpleTag` and
    /// `ChapterAtom` do.
    pub recursive: bool,
    /// Whether RFC 8794 section 11.1.6.10's `unknownsizeallowed` is set, which
    /// only `Segment` and `Cluster` have.
    pub unknown_size_ok: bool,
}

/// Look up `id` in the schema.
#[must_use]
pub fn lookup(id: u32) -> Option<&'static ElementDef> {
    schema::ELEMENTS
        .binary_search_by_key(&id, |d| d.id)
        .ok()
        .and_then(|i| schema::ELEMENTS.get(i))
}

/// Whether `id` is legal directly inside `parent`.
///
/// Global elements are legal inside every master (RFC 8794 section 11.1.6.5),
/// and an ID the schema does not know is not legal anywhere — which is what
/// makes it unable to terminate an unknown-size element.
#[must_use]
pub fn is_child_of(id: u32, parent: u32) -> bool {
    match lookup(id) {
        Some(def) => {
            def.parent == schema::GLOBAL
                || def.parent == parent
                || (def.recursive && def.id == parent)
        }
        None => false,
    }
}

/// Whether `id` is a root element, which terminates any open element.
#[must_use]
pub fn is_root(id: u32) -> bool {
    lookup(id).is_some_and(|d| d.parent == schema::ROOT)
}

/// Read one element header at the current position of `io`.
///
/// A thin wrapper over [`vaco_format_ebml::read_header`] so every call site in
/// this crate keeps importing it from `ebml` rather than reaching into the
/// generic crate directly.
///
/// # Errors
///
/// As [`vaco_format_ebml::read_header`].
pub fn read_header(io: &mut IoContext, caps: Caps) -> Result<Option<Header>> {
    vaco_format_ebml::read_header(io, caps)
}

/// A cursor over one master element's data, already in memory.
///
/// Re-exported as a distinct name (rather than `pub use ... as Slice`) only so
/// the doc table above can link to it under this module; the type itself is
/// [`vaco_format_ebml::Slice`] with no behaviour added.
pub type Slice<'a> = vaco_format_ebml::Slice<'a>;

/// The stack of open master elements, closed over the Matroska schema above.
///
/// [`vaco_format_ebml::Stack`] takes the "is this a legal child" question as a
/// pair of closures, since that question belongs to whatever schema sits on
/// top of EBML rather than to EBML itself. Every call site in this crate
/// wants the Matroska answer, so this wrapper supplies [`is_child_of`] and
/// [`is_root`] once and keeps the single-argument `terminations_for(id)` shape
/// the rest of the crate (and the `matroska_ebml` fuzz target) already uses.
#[derive(Debug, Default, Clone)]
pub struct MatroskaStack(vaco_format_ebml::Stack);

impl MatroskaStack {
    /// Deepest nesting the stack will hold. Matroska needs five.
    pub const MAX_FRAMES: usize = vaco_format_ebml::Stack::MAX_FRAMES;

    #[must_use]
    pub fn new() -> Self {
        Self(vaco_format_ebml::Stack::new())
    }

    /// Open a master element.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] past [`MatroskaStack::MAX_FRAMES`].
    pub fn push(&mut self, id: u32, end: Option<u64>) -> Result<()> {
        self.0.push(id, end)
    }

    pub fn pop(&mut self) -> Option<vaco_format_ebml::Frame> {
        self.0.pop()
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.depth()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn top(&self) -> Option<vaco_format_ebml::Frame> {
        self.0.top()
    }

    /// The ID of the innermost open element, or [`schema::ROOT`].
    #[must_use]
    pub fn open_id(&self) -> u32 {
        self.0.open_id(schema::ROOT)
    }

    /// The nearest known end above `pos`, which bounds any read.
    #[must_use]
    pub fn bound(&self) -> Option<u64> {
        self.0.bound()
    }

    /// Pop every frame whose known end is at or before `pos`.
    pub fn close_finished(&mut self, pos: u64) -> usize {
        self.0.close_finished(pos)
    }

    /// How many unknown-size frames an element with ID `id` terminates, per
    /// the Matroska schema. See [`vaco_format_ebml::Stack::terminations_for`].
    #[must_use]
    pub fn terminations_for(&self, id: u32) -> Option<usize> {
        self.0
            .terminations_for(id, schema::ROOT, is_child_of, is_root)
    }

    pub fn truncate_by(&mut self, n: usize) {
        self.0.truncate_by(n);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests;
