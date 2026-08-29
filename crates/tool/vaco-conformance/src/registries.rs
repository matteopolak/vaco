//! The single, explicit, reviewed list of [`FilterRegistry`]s this crate's
//! reference-gated option tests reach.
//!
//! # Why this exists as its own module
//!
//! `option_consts_gate.rs` (named-constant parsing) and `option_name_gate.rs`
//! (option-name recognition) both need to walk the same set of registries.
//! Two test files hand-copying the same 27-entry list is a drift risk one
//! file's addition and the other's omission would not surface until
//! someone noticed a gate silently not covering a crate it looked like it
//! should — so there is exactly one list, imported by both.
//!
//! # How to change it
//!
//! Adding a filter crate to either gate means adding its registry here —
//! a real, reviewed line, not something that starts being swept just by
//! existing in the tree, the same discipline `filterexec::REGISTRIES` and
//! `vaco-registry`'s own generated table both use. Unlike
//! `filterexec::REGISTRIES`, this list is not limited to crates the
//! in-process frame-execution path can drive: `FilterRegistry::create`
//! alone never builds a `Graph` or touches a frame, so there is no
//! single-input / pixel-format / pad-count constraint to inherit, which is
//! why this list already covers the full registered-filter surface.

use vaco_filter_graph::registry::FilterRegistry;

/// Every filter crate in the tree that exposes a [`FilterRegistry`], tried
/// in this fixed, explicit order.
pub const REGISTRIES: &[(&str, &dyn FilterRegistry)] = &[
    (
        "vaco-filter-aanalysis",
        &vaco_filter_aanalysis::AmeasureRegistry,
    ),
    (
        "vaco-filter-adynamics",
        &vaco_filter_adynamics::DynamicsRegistry,
    ),
    (
        "vaco-filter-aeffects",
        &vaco_filter_aeffects::AeffectsRegistry,
    ),
    ("vaco-filter-aeq", &vaco_filter_aeq::EqRegistry),
    (
        "vaco-filter-analysis",
        &vaco_filter_analysis::registry::AnalysisRegistry,
    ),
    (
        "vaco-filter-artistic",
        &vaco_filter_artistic::ArtisticRegistry,
    ),
    ("vaco-filter-asource", &vaco_filter_asource::AsourceRegistry),
    ("vaco-filter-audio", &vaco_filter_audio::AudioRegistry),
    ("vaco-filter-blur", &vaco_filter_blur::BlurRegistry),
    ("vaco-filter-color", &vaco_filter_color::ColorRegistry),
    (
        "vaco-filter-convolve",
        &vaco_filter_convolve::ConvolveRegistry,
    ),
    (
        "vaco-filter-deinterlace",
        &vaco_filter_deinterlace::DeinterlaceRegistry,
    ),
    ("vaco-filter-denoise", &vaco_filter_denoise::DenoiseRegistry),
    ("vaco-filter-draw-vf", &vaco_filter_draw_vf::DrawVfRegistry),
    (
        "vaco-filter-geometry",
        &vaco_filter_geometry::T2GeometryRegistry,
    ),
    ("vaco-filter-key", &vaco_filter_key::KeyRegistry),
    ("vaco-filter-lut", &vaco_filter_lut::LutRegistry),
    ("vaco-filter-mm", &vaco_filter_mm::MmRegistry),
    ("vaco-filter-motion", &vaco_filter_motion::MotionRegistry),
    ("vaco-filter-overlay", &vaco_filter_overlay::OverlayRegistry),
    ("vaco-filter-palette", &vaco_filter_palette::PaletteRegistry),
    ("vaco-filter-scope", &vaco_filter_scope::ScopeRegistry),
    ("vaco-filter-source", &vaco_filter_source::GeneratorRegistry),
    ("vaco-filter-stack", &vaco_filter_stack::StackRegistry),
    (
        "vaco-filter-temporal",
        &vaco_filter_temporal::TemporalRegistry,
    ),
    (
        "vaco-filter-video-composite",
        &vaco_filter_video_composite::CompositeRegistry,
    ),
    (
        "vaco-filter-video-format",
        &vaco_filter_video_format::FormatRegistry,
    ),
    (
        "vaco-filter-video-geometry",
        &vaco_filter_video_geometry::GeometryRegistry,
    ),
    (
        "vaco-filter-video-source",
        &vaco_filter_video_source::SourceRegistry,
    ),
];
