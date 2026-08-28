//! T2 audio dynamics filters: compressor/limiter/gate/expander/sidechain
//! family plus loudness normalisation and measurement.
//!
//! FT-4.8b (GitHub #472), the other of two children FT-4.8 (#56) split into
//! for single-writer ownership — the sibling is `vaco-filter-aeq`
//! (#471).
//!
//! # Scope versus the brief that requested this crate
//!
//! GitHub #472's own text — checked directly rather than trusted from the
//! brief's restatement, per this project's practice after an earlier agent
//! found its epic named a different grouping than its brief claimed — reads
//! "Dynamics: compressor/limiter/gate/expander/sidechain family plus
//! `loudnorm` and `dynaudnorm`." That maps to nine filters counted against
//! `ffmpeg -filters` (2026-08-23): `acompressor`, `alimiter`, `agate`,
//! `compand` and `mcompand` (the "expander" family — `compand`'s own
//! description is literally "Compress or expand audio dynamic range"),
//! `sidechaincompress`, `sidechaingate`, `loudnorm`, `dynaudnorm`. The
//! brief that requested this crate additionally named `speechnorm`,
//! `volumedetect`, `astats`, `silencedetect`, `silenceremove` — five
//! measurement/silence filters #472's own text does not mention. All
//! fourteen are implemented here; see
//! `docs/filter/vaco-filter-adynamics.md` for which are numerically
//! verified against a real property and which are structural.
//!
//! Plus six more (FT-4.13e, GitHub #485, closing epic #58): `acrusher`,
//! `asoftclip`, `apsyclip`, `adynamicequalizer`, `adynamicsmooth`, `adrc` —
//! the remaining `vaco-filter-adynamics`-row filters plan 16 §4.3 lists that
//! had not been registered yet. `acrusher` and `asoftclip` are measured
//! against the reference (see each module's own doc for exactly what);
//! `apsyclip`, `adynamicequalizer` and `adrc`'s non-default path are
//! structural substitutes for algorithms (a psychoacoustic masking model, an
//! undocumented per-bin spectral expression grammar) that black-box probing
//! cannot recover, honestly labelled as such rather than guessed;
//! `adynamicsmooth` implements a real published algorithm (Cytomic's
//! self-modulating dynamic smoothing filter) from its own description.
#![forbid(unsafe_code)]

pub mod acompressor;
pub mod acrusher;
pub mod adrc;
pub mod adynamicequalizer;
pub mod adynamicsmooth;
pub mod agate;
pub mod alimiter;
pub mod apsyclip;
pub mod asoftclip;
pub mod astats;
mod common;
pub mod compand;
pub mod dynaudnorm;
mod engine;
pub mod loudnorm;
pub mod mcompand;
pub mod registry;
mod sample;
pub mod sidechaincompress;
pub mod sidechaingate;
pub mod silencedetect;
pub mod silenceremove;
pub mod speechnorm;
pub mod volumedetect;

pub use registry::DynamicsRegistry;
