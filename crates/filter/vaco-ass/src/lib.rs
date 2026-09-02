//! ASS/SSA script parsing and override-tag interpretation (plan 16 SS6.3.2,
//! GitHub #487/FT-5.2 and #488/FT-5.3).
//!
//! # Scope
//!
//! This crate ends at [`plan::EventPlan`]: a renderer-agnostic description
//! of styled text runs, position and clip, still in the script's own
//! `PlayResX`/`PlayResY` coordinate space. It renders no pixels — that is
//! `vaco-filter-subtitle`'s job, which scales an `EventPlan` to a real
//! frame and drives `vaco_filter_text::TextRenderer`.
//!
//! [`script::parse`] covers stage (a)'s parsing half: `[Script Info]`,
//! `[V4+ Styles]`/`[V4 Styles]`, `[Events]`. [`plan::plan_event`] covers its
//! tag-interpretation half, the static tag set. See that module's own doc
//! for exactly which tags stage (b) still leaves as "recognised but not
//! animated" and why each one is a stated gap rather than a silent drop.
//!
//! No libass source was read to build this — ISC is Tier A and open to
//! read (`planning/AGENT-CONSTRAINTS.md`'s "clean-room rule is about
//! `FFmpeg`" section), but this crate was built from the informally-published
//! ASS/SSA format documentation and by rendering real `.ass` files and
//! comparing this crate's own parse against them, not by reading libass.
#![forbid(unsafe_code)]

pub mod color;
pub mod plan;
pub mod script;
pub mod style;
pub mod tags;

pub use plan::{EventPlan, ResolvedStyle, TextRun, plan_event};
pub use script::{Event, Script, ScriptInfo, parse};
pub use style::Style;
