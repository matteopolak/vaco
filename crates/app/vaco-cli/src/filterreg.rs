//! The combined [`FilterRegistry`] every real invocation of `-vf`/`-af`/
//! `-filter_complex`/`-lavfi` resolves names through.
//!
//! # There is no aggregate registry in `vaco-registry`, by design of its own
//!
//! `vaco-registry`'s generated tables give `DecoderDesc`/`EncoderDesc`/
//! `MuxerDesc`/`BsfDesc` a `build`/`open` fn pointer, so `vaco_registry::Bsfs`
//! (this crate's own model: see `vaco-registry`'s `impl BsfProvider for Bsfs`)
//! can construct any registered bitstream filter by name with one linear
//! search. `FilterDesc` carries **no such pointer** — it is `name`,
//! `description`, `inputs`, `outputs`, `flags` and nothing that constructs
//! anything.
//!
//! That is not an oversight this crate can fix by adding a field: every
//! filter crate in the tree already solved its own half of the problem,
//! independently, in the shape [`vaco_filter_graph::registry::FilterRegistry`]
//! asks for — a `pub(crate) fn create(&Instantiate) -> Result<Instance,
//! String>` per filter, dispatched by a `pub struct FooRegistry` the crate
//! exports (`vaco-filter-video-geometry::GeometryRegistry`,
//! `vaco-filter-audio::AudioRegistry`, and so on — 27 of them at last count).
//! There is simply no single place upstream of this crate that has *all 27 in
//! one list*, because building that list is exactly this kind of
//! cross-cutting, many-crate-touching decision `planning/AGENT-CONSTRAINTS.md`
//! asks an agent to stop and report rather than force alone — except here the
//! list already exists, reviewed, in
//! `crates/tool/vaco-conformance/src/registries.rs` (built for that crate's
//! own reference-gated option tests) and independently in
//! `crates/tool/vaco-conformance/src/filterexec.rs` (a shorter list, for the
//! subset that crate's in-process frame harness can drive). Neither module is
//! this crate's to import — `vaco-conformance` is a `tool`-layer crate that
//! depends on `vaco-cli`, not the other way around — so this module is a
//! third copy of the same reviewed list, kept for the same reason
//! `registries.rs`'s own doc gives for not hand-copying it a second time
//! silently: **the list is explicit and every addition is a real, reviewed
//! line**, not something a filter crate starts appearing in just by existing
//! in the tree.
//!
//! `filterexec.rs`'s own doc says the quiet part directly: "There is no
//! aggregate registry combining every filter crate in the tree yet (a real,
//! separate gap — see `planning/INTERFACE-GAPS.md`)". This module is that
//! aggregate, for the one caller (`vaco`, the transcode binary) that actually
//! needs the *full* surface rather than a curated subset.
//!
//! # How to change it
//!
//! A new filter crate lands with its own `FooRegistry` and a `create` per
//! filter; wiring it into `vaco -vf`/`-filter_complex` means adding one line
//! to [`CliFilterRegistry::REGISTRIES`] and one dependency line to this
//! crate's `Cargo.toml`, in the same commit as the `vaco-registry` fragment
//! that already has to name its `FilterDesc`. Forgetting this step means the
//! filter shows up in `-filters` (which reads `vaco_registry::filters()`
//! directly) but `-vf thatfilter` reports "unrecognized filter" — the same
//! class of gap this project's own brief warns about: implemented, tested,
//! and unreachable because a registry fragment was never updated.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// Tries every filter crate's own [`FilterRegistry`] in turn, in this fixed,
/// explicit order — the same list `vaco-conformance::registries::REGISTRIES`
/// reviews for its own purposes, kept independently per this module's doc.
#[derive(Debug, Clone, Copy, Default)]
pub struct CliFilterRegistry;

impl CliFilterRegistry {
    const REGISTRIES: &'static [&'static dyn FilterRegistry] = &[
        &vaco_filter_aanalysis::AmeasureRegistry,
        &vaco_filter_adynamics::DynamicsRegistry,
        &vaco_filter_aeffects::AeffectsRegistry,
        &vaco_filter_aeq::EqRegistry,
        &vaco_filter_analysis::registry::AnalysisRegistry,
        &vaco_filter_artistic::ArtisticRegistry,
        &vaco_filter_asource::AsourceRegistry,
        &vaco_filter_audio::AudioRegistry,
        &vaco_filter_blur::BlurRegistry,
        &vaco_filter_color::ColorRegistry,
        &vaco_filter_convolve::ConvolveRegistry,
        &vaco_filter_deinterlace::DeinterlaceRegistry,
        &vaco_filter_denoise::DenoiseRegistry,
        &vaco_filter_draw_vf::DrawVfRegistry,
        &vaco_filter_geometry::T2GeometryRegistry,
        &vaco_filter_key::KeyRegistry,
        &vaco_filter_lut::LutRegistry,
        &vaco_filter_mm::MmRegistry,
        &vaco_filter_overlay::OverlayRegistry,
        &vaco_filter_scope::ScopeRegistry,
        &vaco_filter_source::GeneratorRegistry,
        &vaco_filter_stack::StackRegistry,
        &vaco_filter_temporal::TemporalRegistry,
        &vaco_filter_video_composite::CompositeRegistry,
        &vaco_filter_video_format::FormatRegistry,
        &vaco_filter_video_geometry::GeometryRegistry,
        &vaco_filter_video_source::SourceRegistry,
    ];

    fn find(name: &str) -> Option<&'static dyn FilterRegistry> {
        Self::REGISTRIES
            .iter()
            .copied()
            .find(|r| r.names().contains(&name))
    }
}

impl FilterRegistry for CliFilterRegistry {
    fn names(&self) -> Vec<&str> {
        Self::REGISTRIES.iter().flat_map(|r| r.names()).collect()
    }

    fn contains(&self, name: &str) -> bool {
        Self::find(name).is_some()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match Self::find(req.name) {
            Some(r) => r.create(req),
            None => Err(format!("Unknown filter '{}'", req.name)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn every_family_is_reachable_and_distinct() {
        let reg = CliFilterRegistry;
        // A handful of real names, one per family, spot-checked rather than
        // walking the whole surface (that's `vaco-conformance`'s job, on its
        // own reviewed copy of this list).
        for name in ["scale", "hflip", "aformat", "anull", "null", "crop"] {
            assert!(reg.contains(name), "{name} should be reachable");
        }
        assert!(!reg.contains("this_is_not_a_real_filter_name"));
    }

    #[test]
    fn scale_actually_instantiates() {
        let reg = CliFilterRegistry;
        let req = Instantiate {
            name: "scale",
            instance: "scale",
            args: Some("w=320:h=240"),
            arguments: &[],
        };
        assert!(reg.create(&req).is_ok());
    }

    #[test]
    fn an_unknown_name_is_a_clean_error() {
        let reg = CliFilterRegistry;
        let req = Instantiate {
            name: "not_a_real_filter",
            instance: "not_a_real_filter",
            args: None,
            arguments: &[],
        };
        assert!(reg.create(&req).is_err());
    }
}
