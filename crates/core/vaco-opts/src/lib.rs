//! The `AVOption`/`AVClass` equivalent: typed, introspectable option sets.
//!
//! Every configurable component declares its options once with
//! `#[derive(Options)]` and gets parsing, validation, serialisation, help data
//! and runtime mutation. Unlike a fixed-flag parser, [`Schema`] exposes this
//! metadata from a type without instantiating the component.
//!
//! # API shape
//!
//! [`OptBase`] and [`ArrayDesc`] describe the 21 option kinds; [`OptValue`]
//! supplies parsing and serialisation for field types; [`OptionDesc`] and
//! [`Schema`] hold generated metadata; [`Options`] provides indexed access;
//! [`OptionsExt`] supplies the generic operations.
//!
//! # Example
//!
//! ```
//! use vaco_opts::{Options, OptionsExt};
//! #[derive(Debug, Clone, Options)]
//! #[options(name = "scale", help = "scale the input video")]
//! struct Scale {
//!     #[opt(name = "w", help = "output width", default = 0, range = 0..=8192,
//!           flags(video, filtering))]
//!     width: i32,
//! }
//! let mut s = Scale::default();
//! s.set_from_string("1280", "=", ":").unwrap();
//! assert_eq!(s.width, 1280);
//! ```
//!
//! # Behavioral notes
//!
//! `Dict`, `escape`, `parse::*` and `Rgba` remain here until their layer-1
//! home can provide them without a dependency cycle. `Options::children`
//! returns an owned `Vec`, `check_range` is part of `Options`, and the
//! dyn-compatible [`OptValueKind`] carries the kind constant separately from
//! [`OptValue`]. `Binary` and `VideoRate` are newtypes to avoid conflicting
//! implementations for `Vec<u8>` and `Rational`.

// The derive macro expands to `::vaco_opts::…` paths; this makes those resolve
// inside this crate's own tests and doctests too.
#[allow(unused_extern_crates)]
extern crate self as vaco_opts;

pub mod desc;
pub mod dict;
pub mod error;
pub mod escape;
pub mod flags;
pub mod help;
pub mod kind;
pub mod macros;
pub mod object;
pub mod parse;
pub mod rt;
pub mod value;

pub use desc::{
    ConstDesc, ConstValue, HasSchema, OptEnumConsts, OptId, OptRangeDisplay, OptionDesc, Schema,
    schema_of,
};
pub use dict::{Dict, DictFlags};
pub use error::OptError;
pub use flags::OptFlags;
pub use help::{HelpEntry, help_entries, help_entries_recursive};
pub use kind::{ArrayDesc, OptBase, OptKind};
pub use object::{Options, OptionsExt, SerializeFlags};
pub use parse::{Binary, Rgba, VideoRate};
pub use value::{
    OptValue, OptValueKind, ParseCtx, SerCtx, parse_flag_bits, parse_integer, serialize_flag_bits,
};

/// `#[derive(CliOptionTable)]` — turn a fieldless enum into a
/// `vaco_cli_core::table::OptDesc` argv-flag table. Lives here because this
/// is where the project's derive-macro infrastructure lives, not because the
/// result has anything to do with `Options`/`OptFlags` — see
/// `vaco_cli_core::table::ArgFlags`'s own doc for why the two flag
/// vocabularies are kept apart.
pub use vaco_opts_derive::CliOptionTable;
/// `#[derive(OptEnum)]` — turn a fieldless enum into a unit of named constants.
pub use vaco_opts_derive::OptEnum;
/// `#[derive(Options)]` — project a struct's fields into an option schema.
pub use vaco_opts_derive::Options;

/// Runtime support for macro expansion. Not a stable surface.
#[doc(hidden)]
pub use rt as __rt;
