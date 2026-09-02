//! Generated assembly of enabled components.
//!
//! # What it is
//!
//! The one place that knows which demuxers, muxers, decoders, filters and
//! protocols this build contains. Everything above it — `vaco-probe`,
//! `vaco-cli`, `vaco-sched` — asks here rather than naming a component crate,
//! which is what keeps the graph a fan-in at layer 6 instead of a mesh.
//!
//! # How it works
//!
//! [`generated`] is written by `cargo xtask gen-registry` from the
//! `vaco-component.toml` fragment each component crate ships (plan 19 §3.4).
//! No agent writes it, so ~120 crates can register themselves with zero
//! contention on a shared working tree. This file is the hand-written half: the
//! types the generated table is expressed in, and the lookups over it.
//!
//! Two properties are load-bearing:
//!
//! * **A descriptor is inspectable without constructing anything.** `ctor` in a
//!   fragment names a `const`/`static` descriptor, never a function, so
//!   `-demuxers`, `-codecs` and `-h demuxer=mp4` can print capabilities without
//!   opening a file or allocating a decoder.
//! * **A disabled component costs nothing.** Every generated row carries the
//!   `#[cfg(feature = …)]` its fragment named, and the dependency edge itself is
//!   `optional = true`, so `--no-default-features` produces a registry with
//!   empty tables and no component crate compiled at all.
//!
//! # How to change it
//!
//! To register a component, write `vaco-component.toml` in **your own crate**
//! and run `cargo xtask gen-registry`. Do not edit [`generated`] or the
//! generated region of `Cargo.toml`; CI re-runs the generator with `--check`
//! and fails on a difference.
//!
//! To add a *kind* — an encoder table, say — the descriptor type has to exist
//! in the trait layer first, and then `KINDS` in `xtask/src/registry.rs` gains a
//! row. Until then such a component still gets a [`Component`] metadata row and
//! a compile-time check that its `ctor` path resolves; see [`Kind::has_table`].
//!
//! # Configuration
//!
//! Cargo features only, and all of them are generated. `default` enables every
//! component whose fragment did not say `default = false`, which is the D4
//! opt-out for anything patent-encumbered.
//!
//! # Dependencies
//!
//! The five `-core` crates that define the descriptor types, `vaco-filter-graph`
//! for the `FilterRegistry` trait [`Filters`] implements, plus one optional
//! path dependency per component crate.

#![forbid(unsafe_code)]

pub mod generated;

use vaco_codec_core::{
    BitstreamFilter, CodecId, CodecParameters, DecoderDesc, EncoderDesc, Parser, ParserDesc,
};
use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::FilterDesc;
use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};
use vaco_format_core::mux::BsfProvider;
use vaco_format_core::{DemuxerDesc, MuxerDesc, ParserProvider, ProbeData};
use vaco_limits::Limits;
use vaco_protocol_core::{ProtocolDesc, ProtocolRegistry};

pub use generated::{
    COMPONENTS, DECODERS, DEMUXERS, ENCODERS, ENCUMBERED_ALL, ENCUMBERED_ENABLED, FILTERS, MUXERS,
    PARSERS, PROTOCOLS,
};
use generated::FILTER_REGISTRIES;

/// What a component is. The vocabulary is frozen in plan 19 §3.4.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum Kind {
    Demuxer,
    Muxer,
    Decoder,
    Encoder,
    Parser,
    Filter,
    FilterDispatch,
    Protocol,
    BitstreamFilter,
}

/// The `kind =` spelling of every variant, in the plan's order. Output order
/// for the listing commands, and the order [`COMPONENTS`] is sorted in.
pub const KIND_NAMES: &[(Kind, &str)] = &[
    (Kind::Demuxer, "demuxer"),
    (Kind::Muxer, "muxer"),
    (Kind::Decoder, "decoder"),
    (Kind::Encoder, "encoder"),
    (Kind::Parser, "parser"),
    (Kind::Filter, "filter"),
    (Kind::FilterDispatch, "filter_dispatch"),
    (Kind::Protocol, "protocol"),
    (Kind::BitstreamFilter, "bitstream_filter"),
];

impl Kind {
    /// The fragment spelling, e.g. `"bitstream_filter"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        KIND_NAMES
            .iter()
            .find(|(k, _)| *k == self)
            .map_or("unknown", |(_, n)| *n)
    }

    /// Resolve a fragment spelling.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        KIND_NAMES.iter().find(|(_, n)| *n == name).map(|&(k, _)| k)
    }

    /// Whether this build has a typed descriptor table for the kind.
    ///
    /// False for `BitstreamFilter`, because `vaco-codec-core` defines no
    /// `BitstreamFilterDesc` yet. Components of that kind are listed and their
    /// `ctor` paths are checked at compile time, but there is nothing typed to
    /// hand back. This is a reported gap in the trait layer, not a design
    /// choice — see the crate's doc file.
    ///
    /// `Parser` and `Encoder` were both on this list once, until
    /// `vaco-codec-core` grew [`ParserDesc`](vaco_codec_core::ParserDesc) and
    /// [`EncoderDesc`](vaco_codec_core::EncoderDesc) respectively; both are
    /// now real tables, [`Parsers`] is a real provider, and `vaco-cli`
    /// resolves `-c:v`/`-c:a` through
    /// [`encoder_by_name`]/[`decoder_by_name`] instead of special-casing
    /// `"copy"`.
    #[must_use]
    pub const fn has_table(self) -> bool {
        matches!(
            self,
            Self::Demuxer
                | Self::Muxer
                | Self::Decoder
                | Self::Encoder
                | Self::Parser
                | Self::Filter
                | Self::FilterDispatch
                | Self::Protocol
        )
    }
}

/// One registered component, as its fragment declared it.
///
/// Deliberately separate from the descriptor types: this is the *listing*
/// surface, uniform across kinds, and it exists for every kind including the
/// ones with no descriptor type. `-formats`, `-codecs`, `-demuxers` and friends
/// render exactly these rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Component {
    pub kind: Kind,
    /// Registry name. May be a comma-separated family — the reference reports
    /// `mov,mp4,m4a,3gp,3g2,mj2` as **one** component — and every element is a
    /// valid spelling for `-f`.
    pub name: &'static str,
    pub long_name: Option<&'static str>,
    /// The crate that declared it. Not printed; used by diagnostics and by the
    /// conformance harness's component-intersection normaliser.
    pub krate: &'static str,
    /// The cargo feature gating it, or `None` for an always-on component.
    pub feature: Option<&'static str>,
    /// `video`/`audio`/`subtitle`/`data` for the codec-ish kinds.
    pub media: Option<&'static str>,
    /// The [`CodecId`] name for decoder/encoder/parser.
    pub codec: Option<&'static str>,
    pub extensions: &'static [&'static str],
    pub mime_types: &'static [&'static str],
}

impl Component {
    /// Whether this component answers to `name`, honouring the comma-separated
    /// family spelling.
    #[must_use]
    pub fn matches_name(&self, name: &str) -> bool {
        self.name == name || self.name.split(',').any(|n| n == name)
    }

    /// `media` as a [`MediaType`], when the fragment gave one.
    #[must_use]
    pub fn media_type(&self) -> Option<MediaType> {
        match self.media? {
            "video" => Some(MediaType::Video),
            "audio" => Some(MediaType::Audio),
            "subtitle" => Some(MediaType::Subtitle),
            "data" => Some(MediaType::Data),
            _ => None,
        }
    }

    /// `codec` as a [`CodecId`], when the fragment gave one this build knows.
    #[must_use]
    pub fn codec_id(&self) -> Option<CodecId> {
        CodecId::from_name(self.codec?)
    }
}

// --------------------------------------------------------------- enumeration

/// Every enabled component, in `(kind, name, crate)` order.
///
/// The order is the generator's, not this build's link order, so two builds
/// with the same feature set list components identically — which D6 needs,
/// since the listing commands' output is compared byte for byte.
pub fn components() -> impl Iterator<Item = &'static Component> {
    COMPONENTS.iter()
}

/// Every enabled component of one kind, in name order.
pub fn components_of_kind(kind: Kind) -> impl Iterator<Item = &'static Component> {
    COMPONENTS.iter().filter(move |c| c.kind == kind)
}

/// The [`Component`] row for a descriptor name of a given kind.
#[must_use]
pub fn component(kind: Kind, name: &str) -> Option<&'static Component> {
    COMPONENTS
        .iter()
        .find(|c| c.kind == kind && c.matches_name(name))
}

/// Whether a component of `kind` named `name` is in this build.
#[must_use]
pub fn is_enabled(kind: Kind, name: &str) -> bool {
    component(kind, name).is_some()
}

// ----------------------------------------------------------------- container

/// Every enabled demuxer descriptor. The candidate set for
/// [`vaco_format_core::Probe`].
#[must_use]
pub fn demuxers() -> &'static [&'static DemuxerDesc] {
    DEMUXERS
}

/// `-f <name>`: resolve a demuxer by name or by any element of its family.
#[must_use]
pub fn demuxer_by_name(name: &str) -> Option<&'static DemuxerDesc> {
    DEMUXERS.iter().copied().find(|d| d.matches_name(name))
}

/// Every demuxer whose `extensions` list claims `filename`'s extension.
///
/// An iterator rather than an `Option` because extensions genuinely collide —
/// `.mp4` is claimed by more than one container in a full build — and the
/// caller (probing, or `-f`) decides how to break the tie. Extension matching is
/// a *hint* to the probe engine, never a selection on its own.
pub fn demuxers_for_extension(filename: &str) -> impl Iterator<Item = &'static DemuxerDesc> {
    let filename = filename.to_owned();
    DEMUXERS
        .iter()
        .copied()
        .filter(move |d| d.matches_extension(&filename))
}

/// Every demuxer declaring `mime` in its fragment or its descriptor.
///
/// Both are consulted because [`DemuxerDesc`] carries `mime_types` but
/// [`MuxerDesc`] does not, so the fragment is the only uniform source and the
/// two can in principle disagree. `tests/generated.rs` asserts they do not.
pub fn demuxers_for_mime(mime: &str) -> impl Iterator<Item = &'static DemuxerDesc> {
    let mime = mime.to_owned();
    DEMUXERS.iter().copied().filter(move |d| {
        d.mime_types.contains(&mime.as_str())
            || component(Kind::Demuxer, d.name)
                .is_some_and(|c| c.mime_types.contains(&mime.as_str()))
    })
}

/// Every enabled muxer descriptor.
#[must_use]
pub fn muxers() -> &'static [&'static MuxerDesc] {
    MUXERS
}

/// `-f <name>` on the output side.
#[must_use]
pub fn muxer_by_name(name: &str) -> Option<&'static MuxerDesc> {
    MUXERS.iter().copied().find(|m| m.matches_name(name))
}

/// The muxer whose `extensions` claim `filename`, for output-format guessing.
pub fn muxers_for_extension(filename: &str) -> impl Iterator<Item = &'static MuxerDesc> {
    let filename = filename.to_owned();
    MUXERS.iter().copied().filter(move |m| {
        ProbeData::new(&[])
            .with_filename(&filename)
            .extension_matches(m.extensions)
    })
}

// -------------------------------------------------------------------- codecs

/// Every enabled decoder descriptor.
#[must_use]
pub fn decoders() -> &'static [&'static DecoderDesc] {
    DECODERS
}

/// A decoder by its implementation name, e.g. `"h264"`.
#[must_use]
pub fn decoder_by_name(name: &str) -> Option<&'static DecoderDesc> {
    DECODERS.iter().copied().find(|d| d.name == name)
}

/// The default decoder for a codec: the first enabled implementation in
/// registry order.
///
/// "First in registry order" is a deterministic rule rather than a good one —
/// there is no priority field on [`DecoderDesc`] to rank two implementations of
/// the same codec by. With one implementation per codec, which is where this
/// build is, the distinction does not arise; when a second lands, the
/// descriptor needs a priority and this needs revisiting. Recorded in the doc
/// file as a known gap.
#[must_use]
pub fn decoder_for(codec: CodecId) -> Option<&'static DecoderDesc> {
    DECODERS.iter().copied().find(|d| d.id == codec)
}

/// Every enabled decoder for a codec, in registry order.
pub fn decoders_for(codec: CodecId) -> impl Iterator<Item = &'static DecoderDesc> {
    DECODERS.iter().copied().filter(move |d| d.id == codec)
}

/// Whether this build can decode `codec`.
#[must_use]
pub fn can_decode(codec: CodecId) -> bool {
    decoder_for(codec).is_some()
}

/// Every enabled encoder descriptor. Mirrors [`decoders`] exactly, now that
/// `EncoderDesc` has a typed table of its own.
#[must_use]
pub fn encoders() -> &'static [&'static EncoderDesc] {
    ENCODERS
}

/// An encoder by its implementation name, e.g. `"qoi"`. This is the lookup
/// `vaco-cli`'s `-c:v`/`-c:a` resolves a name through.
#[must_use]
pub fn encoder_by_name(name: &str) -> Option<&'static EncoderDesc> {
    ENCODERS.iter().copied().find(|e| e.name == name)
}

/// The default encoder for a codec: the first enabled implementation in
/// registry order. Same caveat as [`decoder_for`] — no priority field exists
/// yet to rank two implementations of the same codec.
#[must_use]
pub fn encoder_for(codec: CodecId) -> Option<&'static EncoderDesc> {
    ENCODERS.iter().copied().find(|e| e.id == codec)
}

/// Every enabled encoder for a codec, in registry order.
pub fn encoders_for(codec: CodecId) -> impl Iterator<Item = &'static EncoderDesc> {
    ENCODERS.iter().copied().filter(move |e| e.id == codec)
}

/// Whether this build can encode `codec`.
#[must_use]
pub fn can_encode(codec: CodecId) -> bool {
    encoder_for(codec).is_some()
}

/// Every codec this build has *some* implementation of, in [`CodecId`] order.
///
/// `-codecs` lists the codec identity table, not the implementation table, and
/// annotates each row with what is available for it.
pub fn codecs() -> impl Iterator<Item = CodecId> {
    CodecId::all()
}

// ---------------------------------------------------------- filters, protocols

/// Every enabled filter descriptor.
#[must_use]
pub fn filters() -> &'static [&'static FilterDesc] {
    FILTERS
}

/// A filter by name.
#[must_use]
pub fn filter_by_name(name: &str) -> Option<&'static FilterDesc> {
    FILTERS.iter().copied().find(|f| f.name == name)
}

/// The [`FilterRegistry`] every real `-vf`/`-af`/`-filter_complex`/
/// `-print_graphs` invocation resolves a filter name through.
///
/// Generated from the same fragments as [`FILTERS`], so "listed in
/// `-filters`" and "buildable by name" are one fact instead of two that have
/// to be kept in sync by hand — `vaco-cli` used to hand-maintain a second
/// list of every filter crate's registry (`filterreg.rs`'s own
/// `CliFilterRegistry::REGISTRIES`) and it drifted: `vaco-filter-v360` was
/// registered and listed but had no entry in that list, so `-filters`
/// advertised `v360` and `-vf v360=...` reported "Unknown filter" (GitHub
/// #497). `decoder_for`/`encoder_for` never had this seam because
/// `DecoderDesc`/`EncoderDesc` carry their own `make` function pointer;
/// `FilterDesc` does not (it stays inspectable-without-constructing — see
/// the crate doc), so this type is the filter side's equivalent: the
/// generated `FILTER_REGISTRIES` table plus the linear search
/// `decoder_for`/`encoder_for` don't need, since a filter is looked up by a
/// name a fragment does not otherwise index.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filters;

impl FilterRegistry for Filters {
    fn names(&self) -> Vec<&str> {
        FILTER_REGISTRIES.iter().flat_map(|r| r.names()).collect()
    }

    fn create(&self, req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        for r in FILTER_REGISTRIES {
            if r.names().contains(&req.name) {
                return r.create(req);
            }
        }
        Err(format!("Unknown filter '{}'", req.name))
    }
}

/// Every enabled protocol descriptor.
#[must_use]
pub fn protocols() -> &'static [&'static ProtocolDesc] {
    PROTOCOLS
}

/// A [`ProtocolRegistry`] holding every enabled protocol.
///
/// [`ProtocolRegistry`] is a runtime structure rather than a static table
/// because a caller may want a *restricted* one — an HLS playlist opens nested
/// URLs under a whitelist — so this builds a fresh one rather than handing out a
/// shared singleton.
#[must_use]
pub fn protocol_registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    for p in PROTOCOLS {
        r.register(p);
    }
    r
}

// ------------------------------------------------------------------- parsers

/// Every enabled parser descriptor.
#[must_use]
pub fn parsers() -> &'static [&'static ParserDesc] {
    PARSERS
}

/// A parser by its implementation name, e.g. `"h264"`.
#[must_use]
pub fn parser_by_name(name: &str) -> Option<&'static ParserDesc> {
    PARSERS.iter().copied().find(|p| p.name == name)
}

/// The parser descriptor for a codec: the first enabled implementation in
/// registry order.
///
/// Same deterministic-but-arbitrary tie-break as [`decoder_for`], and for the
/// same reason — [`ParserDesc`] has no priority field, and with one
/// implementation per codec the question does not arise.
#[must_use]
pub fn parser_desc_for(codec: CodecId) -> Option<&'static ParserDesc> {
    PARSERS.iter().copied().find(|p| p.handles(codec))
}

/// Whether this build can parse `codec`'s bitstream headers.
#[must_use]
pub fn can_parse(codec: CodecId) -> bool {
    parser_desc_for(codec).is_some()
}

// ------------------------------------------------------------ parser provider

/// The registry's [`ParserProvider`]: how a demuxer gets a bitstream parser
/// without depending on a codec crate (D14.1).
///
/// This is the whole point of the indirection. `vaco-demux-mp4` needs an H.264
/// SPS to report `profile`, `pix_fmt` and `has_b_frames`, and
/// `cargo xtask layer-check` forbids it from depending on `vaco-parse-h264`.
/// So it asks for a parser by [`CodecId`] and this supplies one from the
/// generated table — the demuxer names no codec crate, and a build with
/// `--no-default-features` simply gets `None` and reports what the container
/// itself states.
///
/// # Limits
///
/// A parser reached this way is handed **attacker-controlled bytes on the
/// probe path**, before anything has validated them, so it must be bounded.
/// [`ParserProvider::parser_for`] takes no [`Limits`] — the trait is frozen —
/// so the budget is chosen here, and it is [`Limits::strict`]: the same
/// conservative default [`vaco_format_core::Discovery`] applies to the driver
/// wrapped around the parser, so the two agree without either having to know
/// about the other.
///
/// `Parsers` is deliberately still a **unit struct**. Making it carry a
/// `Limits` field would be a source-breaking change for every existing
/// `&vaco_registry::Parsers`, and nothing needs a different budget yet. When
/// something does, add a second provider that carries one rather than
/// re-shaping this — that keeps the change additive, which is what a registry
/// two binaries depend on is for.
#[derive(Clone, Copy, Default, Debug)]
pub struct Parsers;

impl ParserProvider for Parsers {
    fn parser_for(&self, codec: CodecId) -> Option<Box<dyn Parser>> {
        Some(parser_desc_for(codec)?.build(Limits::strict()))
    }
}

// -------------------------------------------------------- bitstream filters

/// Every registered [`vaco_bsf_core::BsfDesc`], name-sorted duplicates aside.
///
/// A hand-assembled slice rather than a generated typed table: `kind =
/// "bitstream_filter"` has no descriptor type in the trait layer yet
/// ([`Kind::has_table`] says so directly), so `gen-registry` emits a
/// [`Component`] metadata row and a `ctor`-resolution check for each fragment
/// but no `BSF_FILTERS`-shaped static the way [`PARSERS`] gets one. Adding
/// that table means adding a descriptor type to `vaco-codec-core`, the crate
/// [`BitstreamFilter`] itself lives in — the same move that turned `Parser`
/// from an untyped kind into [`PARSERS`], once `vaco-codec-core` grew
/// [`ParserDesc`]. That crate is not this wave's to edit, so this is the
/// scoped, disclosed workaround instead: match on name directly against each
/// `vaco-bsf-*` crate's own `filters()` list, which is exactly what a typed
/// table would let [`Bsfs::open`] do anyway, one layer removed. Recorded as a
/// gap rather than silently living with it — see
/// `planning/INTERFACE-GAPS.md`.
fn bsf_descs() -> impl Iterator<Item = vaco_bsf_core::BsfDesc> {
    vaco_bsf_generic::filters()
        .iter()
        .chain(vaco_bsf_h2645::filters())
        .chain(vaco_bsf_audio::filters())
        .chain(vaco_bsf_av1::filters())
        .chain(vaco_bsf_legacy::filters())
        .chain(vaco_bsf_subtitle::filters())
        .chain(vaco_bsf_vpx::filters())
        .copied()
}

/// The registry's [`BsfProvider`]: how a muxer's `check_bitstream` request
/// (M6) reaches a real filter without the muxer naming a `vaco-bsf-*` crate
/// (D14.1) — the mux-side mirror of [`Parsers`].
///
/// Before this existed every `BsfProvider` in the tree was
/// [`vaco_format_core::mux::NoBsfs`], so `check_bitstream` asking for
/// `extract_extradata` or `h264_mp4toannexb` always failed — the M6 stage ran
/// and had nothing to give it (`planning/INTERFACE-GAPS.md` gap 8's own
/// closing note). This is that filter.
#[derive(Clone, Copy, Default, Debug)]
pub struct Bsfs;

impl BsfProvider for Bsfs {
    fn open(&self, name: &str, params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
        for d in bsf_descs() {
            if d.name == name {
                return (d.build)(params);
            }
        }
        Err(Error::Unsupported(
            "this build registers no bitstream filter by that name",
        ))
    }
}

#[cfg(test)]
#[allow(clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn bsfs_opens_every_registered_filter_by_name() {
        for d in bsf_descs() {
            // Every filter here refuses at least one codec identity, but
            // `null`/`setts`/`chomp`/`noise`/`dump_extra`/`remove_extra` take
            // any `CodecParameters` at all — this only checks that `open`
            // reaches the right constructor by name, not that every filter
            // accepts a default-constructed stream.
            let outcome = Bsfs.open(d.name, &CodecParameters::default());
            assert!(
                outcome.is_ok() || matches!(outcome, Err(Error::Unsupported(_))),
                "{}: open returned neither a filter nor Unsupported",
                d.name
            );
        }
    }

    #[test]
    fn bsfs_refuses_an_unknown_name() {
        let outcome = Bsfs.open("this_filter_does_not_exist", &CodecParameters::default());
        assert!(matches!(outcome, Err(Error::Unsupported(_))));
    }

    #[test]
    fn every_bsf_component_row_has_a_reachable_filter() {
        for c in components_of_kind(Kind::BitstreamFilter) {
            assert!(
                bsf_descs().any(|d| d.name == c.name),
                "{} is registered but Bsfs cannot open it",
                c.name
            );
        }
    }

    /// Rule F's runtime half: `xtask reachability-check` already asserts
    /// every `filter` component's crate has a `filter_dispatch` one (a
    /// static check over the fragments); this asserts the two generated
    /// tables that produces actually agree at the individual filter-name
    /// level, the same shape `every_bsf_component_row_has_a_reachable_filter`
    /// checks for bitstream filters.
    #[test]
    fn every_filter_component_row_is_reachable_through_filters() {
        let registry = Filters;
        for c in components_of_kind(Kind::Filter) {
            for alias in c.name.split(',') {
                assert!(
                    registry.contains(alias),
                    "{alias} is registered but Filters cannot dispatch it"
                );
            }
        }
    }

    #[test]
    fn kind_names_round_trip() {
        for &(kind, name) in KIND_NAMES {
            assert_eq!(Kind::from_name(name), Some(kind), "{name}");
            assert_eq!(kind.name(), name);
        }
        assert_eq!(Kind::from_name("nonesuch"), None);
    }

    #[test]
    fn components_are_sorted_by_kind_then_name() {
        let mut prev: Option<(usize, &str)> = None;
        for c in components() {
            let rank = KIND_NAMES
                .iter()
                .position(|(k, _)| *k == c.kind)
                .unwrap_or(usize::MAX);
            if let Some(p) = prev {
                assert!(p <= (rank, c.name), "{p:?} then {:?}", (rank, c.name));
            }
            prev = Some((rank, c.name));
        }
    }

    #[test]
    fn every_typed_row_has_a_metadata_row() {
        // The generator emits both from the same fragment, so a mismatch means
        // the two halves drifted — which is the failure mode a generated file is
        // supposed to make impossible.
        for d in demuxers() {
            assert!(
                component(Kind::Demuxer, d.name).is_some(),
                "demuxer {} has no COMPONENTS row",
                d.name
            );
        }
        for m in muxers() {
            assert!(component(Kind::Muxer, m.name).is_some(), "muxer {}", m.name);
        }
        for d in decoders() {
            assert!(
                component(Kind::Decoder, d.name).is_some(),
                "decoder {}",
                d.name
            );
        }
        for f in filters() {
            assert!(
                component(Kind::Filter, f.name).is_some(),
                "filter {}",
                f.name
            );
        }
        for p in protocols() {
            assert!(
                component(Kind::Protocol, p.name).is_some(),
                "protocol {}",
                p.name
            );
        }
    }

    #[test]
    fn fragment_metadata_agrees_with_the_descriptor() {
        // A fragment may omit `extensions`/`long_name`; it may not contradict
        // them. Catching that here rather than at a user's `-formats` output.
        for d in demuxers() {
            let Some(c) = component(Kind::Demuxer, d.name) else {
                continue;
            };
            assert_eq!(c.name, d.name, "name");
            if let Some(l) = c.long_name {
                assert_eq!(l, d.long_name, "{}: long_name", d.name);
            }
            if !c.extensions.is_empty() {
                assert_eq!(c.extensions, d.extensions, "{}: extensions", d.name);
            }
            if !c.mime_types.is_empty() {
                assert_eq!(c.mime_types, d.mime_types, "{}: mime_types", d.name);
            }
        }
    }

    #[test]
    fn every_family_alias_resolves_to_its_own_descriptor() {
        for d in demuxers() {
            for alias in d.name.split(',') {
                let found = demuxer_by_name(alias);
                assert!(found.is_some(), "{alias} does not resolve");
                assert_eq!(found.map(|f| f.name), Some(d.name), "{alias}");
            }
        }
    }

    #[test]
    fn a_kind_without_a_table_is_still_listable() {
        // `Encoder` used to belong on this list too, before `EncoderDesc`
        // existed. Asserting it stayed empty would have pinned an absence
        // this build is actively closing rather than a real invariant, so
        // this asserts the mapping instead: `BitstreamFilter` has no table,
        // `Encoder` does.
        assert!(!Kind::BitstreamFilter.has_table());
        assert!(Kind::Encoder.has_table());
        // Listing must not panic or claim a table exists, whichever it is.
        for kind in [Kind::Encoder, Kind::BitstreamFilter] {
            let _ = components_of_kind(kind).count();
        }
    }

    #[test]
    fn every_registered_encoder_is_a_component_too() {
        for e in encoders() {
            assert!(component(Kind::Encoder, e.name).is_some(), "encoder {}", e.name);
        }
    }

    /// Every registered parser must be reachable by every codec it declares.
    ///
    /// This replaces `the_parser_provider_is_honest_about_being_empty`, which
    /// asserted `parser_for` returned `None` for every codec — true, and the
    /// whole reason `-show_streams` could not report a profile, a pixel format
    /// or a channel count on any container. The assertion is inverted rather
    /// than deleted so that a build which loses the parser table fails here.
    #[test]
    fn every_registered_parser_is_reachable_by_codec() {
        for desc in parsers() {
            for &codec in desc.codecs {
                assert!(
                    Parsers.parser_for(codec).is_some(),
                    "{} declares {} but the provider cannot build one",
                    desc.name,
                    codec.name()
                );
                assert!(can_parse(codec), "{}", codec.name());
                assert_eq!(parser_desc_for(codec).map(|d| d.name), Some(desc.name));
            }
            assert_eq!(parser_by_name(desc.name).map(|d| d.name), Some(desc.name));
        }
        assert!(parser_by_name("nonesuch").is_none());
    }

    /// A codec with no parser still answers, and answers `None` rather than
    /// panicking — the `--no-default-features` shape, and the path a demuxer
    /// takes for any codec this build cannot parse.
    #[test]
    fn a_codec_without_a_parser_gets_none() {
        for codec in codecs() {
            let has = parsers().iter().any(|p| p.handles(codec));
            assert_eq!(Parsers.parser_for(codec).is_some(), has, "{}", codec.name());
        }
    }

    /// A descriptor must be inspectable without building anything, which is
    /// what lets `-parsers` print a table without allocating a parser.
    #[test]
    fn parser_descriptors_agree_with_their_metadata_rows() {
        for desc in parsers() {
            let row = component(Kind::Parser, desc.name)
                .unwrap_or_else(|| panic!("parser {} has no COMPONENTS row", desc.name));
            assert_eq!(row.long_name, Some(desc.long_name), "{}", desc.name);
            assert_eq!(row.media_type(), Some(desc.media_type), "{}", desc.name);
            assert_eq!(
                row.codec_id(),
                desc.codecs.first().copied(),
                "{} fragment `codec` must name the descriptor's first codec",
                desc.name
            );
        }
    }

    #[test]
    fn protocol_registry_holds_every_enabled_protocol() {
        let r = protocol_registry();
        assert_eq!(r.len(), PROTOCOLS.len());
        for p in protocols() {
            assert!(r.find(p.name).is_some(), "{}", p.name);
        }
    }

    #[test]
    fn lookups_on_an_empty_or_populated_registry_do_not_panic() {
        // Every accessor, run whatever the feature set is. The registry is the
        // one crate whose tables can legitimately be empty, and an accessor that
        // only works when something is registered is a latent panic in a
        // `--no-default-features` build.
        assert!(demuxer_by_name("nonesuch").is_none());
        assert!(muxer_by_name("nonesuch").is_none());
        assert!(decoder_by_name("nonesuch").is_none());
        assert!(filter_by_name("nonesuch").is_none());
        assert!(component(Kind::Demuxer, "nonesuch").is_none());
        assert!(!is_enabled(Kind::Muxer, "nonesuch"));
        assert_eq!(demuxers_for_extension("x.nonesuch").count(), 0);
        assert_eq!(demuxers_for_mime("application/nonesuch").count(), 0);
        assert_eq!(muxers_for_extension("x.nonesuch").count(), 0);
        assert_eq!(demuxers().len(), DEMUXERS.len());
        assert_eq!(codecs().count(), CodecId::all().count());
    }

    #[test]
    fn component_media_and_codec_resolve() {
        let c = Component {
            kind: Kind::Decoder,
            name: "h264",
            long_name: None,
            krate: "x",
            feature: None,
            media: Some("video"),
            codec: Some("h264"),
            extensions: &[],
            mime_types: &[],
        };
        assert_eq!(c.media_type(), Some(MediaType::Video));
        assert_eq!(c.codec_id(), Some(CodecId::H264));
        assert!(c.matches_name("h264"));

        let family = Component {
            name: "mov,mp4,m4a",
            media: None,
            codec: None,
            ..c
        };
        assert!(family.matches_name("mp4"));
        assert!(!family.matches_name("mp3"));
        assert_eq!(family.media_type(), None);
        assert_eq!(family.codec_id(), None);
    }
}
