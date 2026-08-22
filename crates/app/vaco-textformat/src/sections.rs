//! The ffprobe section schema.
//!
//! Every row here was transcribed from `ffprobe -sections` on the reference
//! binary (8.1). The dump prints, for each section, four flags plus a
//! `NAME/UNIQUE_NAME` pair; sections whose local name is already unambiguous
//! print the name once. That is the entire authored content of the table, and it
//! is an interface fact (D7): the names are what `-show_entries` accepts on the
//! command line.
//!
//! Two columns are *not* in the dump and were derived by observation:
//!
//! * [`SectionDesc::element_name`] — the child element the `xml` writer emits
//!   for each key/value pair of a [`SectionFlags::VAR_FIELDS`] section, and the
//!   prefix the `compact` writer uses for the same section. `tags` → `tag`,
//!   `side_data` → `side_datum` are observed. See the module note below for the
//!   ones that are not.
//! * [`SectionDesc::default_style`] — whether the `default` writer gives the
//!   section its own `[HEADER]` block or flattens it into the parent as
//!   `PREFIX:key=value`. It follows a rule that reproduces every observed case:
//!   *a section gets a header iff its parent is the root or an array*. The
//!   column is stored explicitly anyway so a future conformance run can diff it
//!   row by row rather than argue about the rule.
//!
//! # Unverified rows
//!
//! `element_name` for the `stream_group` component/piece/block family and for
//! the frame-side-data component/piece family could not be observed: no sample
//! reachable from `lavfi` produces an IAMF stream group or a side-data type with
//! sub-components, and `-show_frames` is v0.2 anyway (D14.4). Those rows carry
//! the section's own local name as a placeholder and are marked
//! `UNVERIFIED_ELEMENT_NAME` below. They affect only the `xml` and `compact`
//! writers, and only for those sections.

use bitflags::bitflags;

bitflags! {
    /// The four flags `ffprobe -sections` prints, in its column order.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct SectionFlags: u8 {
        /// `W` — a wrapper: contains other sections and has no local entries.
        const WRAPPER = 1 << 0;
        /// `A` — an array of elements of the same type.
        const ARRAY = 1 << 1;
        /// `V` — a variable number of fields with variable keys.
        const VAR_FIELDS = 1 << 2;
        /// `T` — the section carries a unique type string.
        const UNIQUE_TYPE = 1 << 3;
    }
}

/// An index into [`SECTIONS`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SectionId(pub u16);

/// How the `default` writer renders a section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DefaultStyle {
    /// Nothing: the section is structural (`root`, and every array).
    Transparent,
    /// `[UPPER(name)]` … `[/UPPER(name)]`.
    Header,
    /// Flattened into the parent as `UPPER(prefix):key=value`.
    Inline,
}

/// One row of the schema.
#[derive(Clone, Copy, Debug)]
pub struct SectionDesc {
    /// Index of this row in [`SECTIONS`]; `SECTIONS[id.0].id == id`.
    pub id: SectionId,
    /// Local name. Used for element names, `[HEADERS]` and flat/ini paths.
    pub name: &'static str,
    /// Globally unique name; equals `name` when unambiguous. Used only by
    /// `-show_entries`, which accepts either spelling.
    pub unique_name: &'static str,
    /// The `-sections` flag column.
    pub flags: SectionFlags,
    /// Children, in `-sections` declaration order.
    pub children: &'static [SectionId],
    /// Element name for a [`SectionFlags::VAR_FIELDS`] section's pairs.
    pub element_name: Option<&'static str>,
    /// `default`-writer rendering.
    pub default_style: DefaultStyle,
}

impl SectionDesc {
    /// The prefix the inline writers use, lowercase for `compact` and uppercased
    /// for `default`. Variable-field sections use their element name (`tag`),
    /// everything else its local name (`disposition`, `flags`).
    #[must_use]
    pub const fn inline_prefix(&self) -> &'static str {
        match self.element_name {
            Some(e) if self.flags.contains(SectionFlags::VAR_FIELDS) => e,
            _ => self.name,
        }
    }

    /// How the `compact`/`csv` writer renders the section.
    ///
    /// Identical to [`SectionDesc::default_style`] except that variable-field
    /// sections are always inlined — which is what makes packet side data read
    /// `side_datum/skip_samples:skip_samples=1024` on the packet's own line,
    /// while the `default` writer gives it a `[SIDE_DATA]` block.
    #[must_use]
    pub const fn compact_style(&self) -> DefaultStyle {
        if self.flags.contains(SectionFlags::VAR_FIELDS) {
            DefaultStyle::Inline
        } else {
            self.default_style
        }
    }

    /// Whether the section contributes a path segment to `flat`/`ini` paths at
    /// the given `hierarchical` setting.
    #[must_use]
    pub const fn in_path(&self, hierarchical: bool) -> bool {
        if self.flags.contains(SectionFlags::WRAPPER) {
            // `root` only.
            return false;
        }
        hierarchical || !self.flags.contains(SectionFlags::ARRAY)
    }
}

macro_rules! sections {
    ($(
        $konst:ident = $idx:expr, $name:literal / $unique:literal,
        $flags:expr, $elem:expr, $style:expr, [$($child:ident),* $(,)?]
    );* $(;)?) => {
        impl SectionId {
            $(
                #[doc = concat!("The `", $unique, "` section.")]
                pub const $konst: Self = Self($idx);
            )*
        }

        /// Every section, indexed by [`SectionId`].
        pub static SECTIONS: &[SectionDesc] = &[$(
            SectionDesc {
                id: SectionId($idx),
                name: $name,
                unique_name: $unique,
                flags: $flags,
                children: &[$(SectionId::$child),*],
                element_name: $elem,
                default_style: $style,
            }
        ),*];
    };
}

use DefaultStyle::{Header, Inline, Transparent};

const W: SectionFlags = SectionFlags::WRAPPER;
const A: SectionFlags = SectionFlags::ARRAY;
const V: SectionFlags = SectionFlags::VAR_FIELDS;
const VT: SectionFlags = SectionFlags::VAR_FIELDS.union(SectionFlags::UNIQUE_TYPE);
const N: SectionFlags = SectionFlags::empty();

sections! {
    ROOT = 0, "root" / "root", W, None, Transparent, [
        CHAPTERS, FORMAT, FRAMES, PROGRAMS, STREAM_GROUPS, STREAMS, PACKETS,
        ERROR, PROGRAM_VERSION, LIBRARY_VERSIONS, PIXEL_FORMATS,
    ];

    CHAPTERS = 1, "chapters" / "chapters", A, None, Transparent, [CHAPTER];
    CHAPTER = 2, "chapter" / "chapter", N, None, Header, [CHAPTER_TAGS];
    CHAPTER_TAGS = 3, "tags" / "chapter_tags", V, Some("tag"), Inline, [];

    FORMAT = 4, "format" / "format", N, None, Header, [FORMAT_TAGS];
    FORMAT_TAGS = 5, "tags" / "format_tags", V, Some("tag"), Inline, [];

    FRAMES = 6, "frames" / "frames", A, None, Transparent, [FRAME, SUBTITLE];
    FRAME = 7, "frame" / "frame", N, None, Header,
        [FRAME_TAGS, FRAME_SIDE_DATA_LIST, LOGS];
    FRAME_TAGS = 8, "tags" / "frame_tags", V, Some("tag"), Inline, [];
    FRAME_SIDE_DATA_LIST = 9, "side_data_list" / "frame_side_data_list", A, None,
        Transparent, [FRAME_SIDE_DATA];
    FRAME_SIDE_DATA = 10, "side_data" / "frame_side_data", VT, Some("side_datum"),
        Header, [TIMECODES, FRAME_SIDE_DATA_COMPONENTS];
    TIMECODES = 11, "timecodes" / "timecodes", A, None, Transparent, [TIMECODE];
    TIMECODE = 12, "timecode" / "timecode", N, None, Header, [];
    FRAME_SIDE_DATA_COMPONENTS = 13, "components" / "frame_side_data_components",
        A, None, Transparent, [FRAME_SIDE_DATA_COMPONENT];
    // UNVERIFIED_ELEMENT_NAME
    FRAME_SIDE_DATA_COMPONENT = 14, "component" / "frame_side_data_component",
        VT, Some("component"), Header, [FRAME_SIDE_DATA_PIECES];
    FRAME_SIDE_DATA_PIECES = 15, "pieces" / "frame_side_data_pieces", A, None,
        Transparent, [FRAME_SIDE_DATA_PIECE];
    // UNVERIFIED_ELEMENT_NAME
    FRAME_SIDE_DATA_PIECE = 16, "piece" / "frame_side_data_piece", VT,
        Some("piece"), Header, [];
    LOGS = 17, "logs" / "logs", A, None, Transparent, [LOG];
    LOG = 18, "log" / "log", N, None, Header, [];
    SUBTITLE = 19, "subtitle" / "subtitle", N, None, Header, [];

    PROGRAMS = 20, "programs" / "programs", A, None, Transparent, [PROGRAM];
    PROGRAM = 21, "program" / "program", N, None, Header,
        [PROGRAM_TAGS, PROGRAM_STREAMS];
    PROGRAM_TAGS = 22, "tags" / "program_tags", V, Some("tag"), Inline, [];
    PROGRAM_STREAMS = 23, "streams" / "program_streams", A, None, Transparent,
        [PROGRAM_STREAM];
    PROGRAM_STREAM = 24, "stream" / "program_stream", N, None, Header,
        [PROGRAM_STREAM_DISPOSITION, PROGRAM_STREAM_TAGS];
    PROGRAM_STREAM_DISPOSITION = 25, "disposition" / "program_stream_disposition",
        N, None, Inline, [];
    PROGRAM_STREAM_TAGS = 26, "tags" / "program_stream_tags", V, Some("tag"),
        Inline, [];

    STREAM_GROUPS = 27, "stream_groups" / "stream_groups", A, None, Transparent,
        [STREAM_GROUP];
    STREAM_GROUP = 28, "stream_group" / "stream_group", N, None, Header,
        [STREAM_GROUP_TAGS, STREAM_GROUP_DISPOSITION, STREAM_GROUP_COMPONENTS,
         STREAM_GROUP_STREAMS];
    STREAM_GROUP_TAGS = 29, "tags" / "stream_group_tags", V, Some("tag"), Inline, [];
    STREAM_GROUP_DISPOSITION = 30, "disposition" / "stream_group_disposition", N,
        None, Inline, [];
    STREAM_GROUP_COMPONENTS = 31, "components" / "stream_group_components", A,
        None, Transparent, [STREAM_GROUP_COMPONENT];
    // UNVERIFIED_ELEMENT_NAME
    STREAM_GROUP_COMPONENT = 32, "component" / "stream_group_component", VT,
        Some("component"), Header, [SUBCOMPONENTS, STREAM_GROUP_PIECES];
    SUBCOMPONENTS = 33, "subcomponents" / "subcomponents", A, None, Transparent,
        [SUBCOMPONENT];
    // UNVERIFIED_ELEMENT_NAME
    SUBCOMPONENT = 34, "subcomponent" / "subcomponent", VT, Some("subcomponent"),
        Header, [];
    STREAM_GROUP_PIECES = 35, "pieces" / "stream_group_pieces", A, None,
        Transparent, [STREAM_GROUP_PIECE];
    // UNVERIFIED_ELEMENT_NAME
    STREAM_GROUP_PIECE = 36, "piece" / "stream_group_piece", VT, Some("piece"),
        Header, [SUBPIECES];
    SUBPIECES = 37, "subpieces" / "subpieces", A, None, Transparent, [SUBPIECE];
    // UNVERIFIED_ELEMENT_NAME
    SUBPIECE = 38, "subpiece" / "subpiece", VT, Some("subpiece"), Header, [BLOCKS];
    BLOCKS = 39, "blocks" / "blocks", A, None, Transparent, [BLOCK];
    // UNVERIFIED_ELEMENT_NAME
    BLOCK = 40, "block" / "block", VT, Some("block"), Header, [];
    STREAM_GROUP_STREAMS = 41, "streams" / "stream_group_streams", A, None,
        Transparent, [STREAM_GROUP_STREAM];
    STREAM_GROUP_STREAM = 42, "stream" / "stream_group_stream", N, None, Header,
        [STREAM_GROUP_STREAM_DISPOSITION, STREAM_GROUP_STREAM_TAGS];
    STREAM_GROUP_STREAM_DISPOSITION = 43,
        "disposition" / "stream_group_stream_disposition", N, None, Inline, [];
    STREAM_GROUP_STREAM_TAGS = 44, "tags" / "stream_group_stream_tags", V,
        Some("tag"), Inline, [];

    STREAMS = 45, "streams" / "streams", A, None, Transparent, [STREAM];
    STREAM = 46, "stream" / "stream", N, None, Header,
        [STREAM_DISPOSITION, STREAM_TAGS, STREAM_SIDE_DATA_LIST];
    STREAM_DISPOSITION = 47, "disposition" / "stream_disposition", N, None,
        Inline, [];
    STREAM_TAGS = 48, "tags" / "stream_tags", V, Some("tag"), Inline, [];
    STREAM_SIDE_DATA_LIST = 49, "side_data_list" / "stream_side_data_list", A,
        None, Transparent, [STREAM_SIDE_DATA];
    STREAM_SIDE_DATA = 50, "side_data" / "stream_side_data", VT,
        Some("side_datum"), Header, [];

    PACKETS = 51, "packets" / "packets", A, None, Transparent, [PACKET];
    PACKET = 52, "packet" / "packet", N, None, Header,
        [PACKET_TAGS, PACKET_SIDE_DATA_LIST];
    PACKET_TAGS = 53, "tags" / "packet_tags", V, Some("tag"), Inline, [];
    PACKET_SIDE_DATA_LIST = 54, "side_data_list" / "packet_side_data_list", A,
        None, Transparent, [PACKET_SIDE_DATA];
    PACKET_SIDE_DATA = 55, "side_data" / "packet_side_data", VT,
        Some("side_datum"), Header, [];

    ERROR = 56, "error" / "error", N, None, Header, [];
    PROGRAM_VERSION = 57, "program_version" / "program_version", N, None, Header, [];
    LIBRARY_VERSIONS = 58, "library_versions" / "library_versions", A, None,
        Transparent, [LIBRARY_VERSION];
    LIBRARY_VERSION = 59, "library_version" / "library_version", N, None, Header, [];
    PIXEL_FORMATS = 60, "pixel_formats" / "pixel_formats", A, None, Transparent,
        [PIXEL_FORMAT];
    PIXEL_FORMAT = 61, "pixel_format" / "pixel_format", N, None, Header,
        [PIXEL_FORMAT_FLAGS, PIXEL_FORMAT_COMPONENTS];
    PIXEL_FORMAT_FLAGS = 62, "flags" / "pixel_format_flags", N, None, Inline, [];
    PIXEL_FORMAT_COMPONENTS = 63, "components" / "pixel_format_components", A,
        None, Transparent, [PIXEL_FORMAT_COMPONENT];
    PIXEL_FORMAT_COMPONENT = 64, "component" / "component", N, None, Header, [];
}

/// Look a section up by id.
///
/// Infallible for every [`SectionId`] constant; returns the root for an
/// out-of-range id rather than panicking, because ids can in principle arrive
/// from a `-show_entries` parse.
#[must_use]
pub fn desc(id: SectionId) -> &'static SectionDesc {
    SECTIONS.get(id.0 as usize).unwrap_or(&ROOT_FALLBACK)
}

static ROOT_FALLBACK: SectionDesc = SectionDesc {
    id: SectionId(0),
    name: "root",
    unique_name: "root",
    flags: SectionFlags::WRAPPER,
    children: &[],
    element_name: None,
    default_style: DefaultStyle::Transparent,
};

/// Find every section whose local **or** unique name is `name`.
///
/// `-show_entries` matches on either spelling, and a local-name match selects
/// *all* sections carrying it — which is why `-show_entries stream=index` also
/// opens the (usually empty) `programs` and `stream_groups` arrays: `stream` is
/// the local name of `program_stream` and `stream_group_stream` too. Observed.
pub fn by_name(name: &str) -> impl Iterator<Item = &'static SectionDesc> + '_ {
    SECTIONS
        .iter()
        .filter(move |s| s.name == name || s.unique_name == name)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_indices() {
        for (i, s) in SECTIONS.iter().enumerate() {
            assert_eq!(s.id.0 as usize, i, "{} has the wrong id", s.unique_name);
        }
    }

    #[test]
    fn unique_names_are_unique() {
        for (i, a) in SECTIONS.iter().enumerate() {
            for b in SECTIONS.iter().skip(i + 1) {
                assert_ne!(a.unique_name, b.unique_name);
            }
        }
    }

    #[test]
    fn every_section_but_root_is_reachable() {
        let mut seen = vec![false; SECTIONS.len()];
        let mut stack = vec![SectionId::ROOT];
        while let Some(id) = stack.pop() {
            let Some(slot) = seen.get_mut(id.0 as usize) else {
                continue;
            };
            if core::mem::replace(slot, true) {
                continue;
            }
            stack.extend_from_slice(desc(id).children);
        }
        assert!(seen.iter().all(|&b| b));
    }

    #[test]
    fn default_style_follows_the_parent_rule() {
        // Observed rule: a section gets its own `[HEADER]` iff its parent is the
        // root or an array; arrays and the root are transparent.
        for parent in SECTIONS {
            for child in parent.children {
                let c = desc(*child);
                let expect = if c
                    .flags
                    .intersects(SectionFlags::WRAPPER | SectionFlags::ARRAY)
                {
                    DefaultStyle::Transparent
                } else if parent.id == SectionId::ROOT || parent.flags.contains(SectionFlags::ARRAY)
                {
                    DefaultStyle::Header
                } else {
                    DefaultStyle::Inline
                };
                assert_eq!(c.default_style, expect, "{}", c.unique_name);
            }
        }
    }

    #[test]
    fn local_name_lookup_finds_all_streams() {
        let n = by_name("stream").count();
        assert_eq!(n, 3, "stream, program_stream, stream_group_stream");
        assert_eq!(by_name("stream_tags").count(), 1);
    }
}
