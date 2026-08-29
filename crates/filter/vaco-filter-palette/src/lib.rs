//! T2/T3 palette video filters — `planning/16-filters.md` §4.2's
//! `vaco-filter-palette` row: `palettegen`, `paletteuse`, `latticepal`,
//! `elbg`. Crate did not exist (`crates/filter/` and
//! `planning/ASSIGNMENTS.md` both checked directly).
//!
//! # Real unclaimed remainder of #111 (FT-4.11)
//!
//! `vaco-filter-stack`'s own crate doc (built for the same GitHub issue,
//! #111/FT-4.11) already scoped this: `hstack`/`vstack`/`xstack` (that
//! crate) and `palettegen`/`paletteuse`/`elbg` (this crate) are the real,
//! unclaimed remainder of #111. `latticepal` is not in this pass either —
//! per that same scoping comment, it is not present in the installed
//! `ffmpeg 8.1` reference at all (`ffmpeg -hide_banner -filters | grep
//! lattice` finds nothing), so there is no oracle to measure it against and
//! no reference behaviour to reproduce.
//!
//! # What is implemented
//!
//! All three use a shared, original median-cut colour quantiser
//! ([`quantize`]) — not a transcription of the reference's own quantiser,
//! which is a different, well-documented public algorithm (Heckbert 1982),
//! also implemented from general algorithmic knowledge rather than the
//! reference's source (D6/D7).
//!
//! - [`palettegen`] — accumulates a full-stream colour histogram (8-bit RGB,
//!   alpha ignored) and emits one `16x16` RGBA palette image at end of
//!   stream. `stats_mode=diff`/`single` are parsed but not distinguished
//!   from `full` (this pass always accumulates the whole stream) —
//!   documented, not silently ignored.
//! - [`paletteuse`] — maps each pixel of the main video input to its
//!   nearest colour (plain Euclidean RGB distance, no dithering) in the
//!   palette read from the second input. The reference's default dithering
//!   is `sierra2_4a` (error diffusion); this ships the undithered baseline
//!   only — a real, named simplification, not every `dither=` mode.
//! - [`elbg`] — posterizes a **single frame** to `codebook_length` colours
//!   using the same median-cut quantiser. This is **not** the reference's
//!   actual ELBG (Enhanced Linde–Buzo–Gray) algorithm, which iteratively
//!   refines a codebook via generalized-Lloyd relaxation plus
//!   utility-driven cell splitting over `nb_steps` iterations — median-cut
//!   is a different, simpler, one-shot member of the same "vector
//!   quantisation for posterization" family. `nb_steps`/`seed` are parsed
//!   for option compatibility but do not affect output (median-cut is
//!   deterministic and does not iterate).
//!
//! All three require an addressable, non-hardware, non-palette RGBA input
//! — enforced by requesting an exact `Rgba` pixel format on every relevant
//! pad (see each filter's own `NodeFormats`), so the negotiator inserts a
//! conversion upstream rather than this crate silently misreading another
//! format's byte layout.

#![forbid(unsafe_code)]

pub mod elbg;
pub mod palettegen;
pub mod paletteuse;
pub mod quantize;
pub mod registry;

pub use registry::PaletteRegistry;
