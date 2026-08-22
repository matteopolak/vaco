//! The filtergraph description language: parsing, link resolution and the
//! auto-conversion policy.
//!
//! This crate turns the string behind `-vf`, `-af` and `-filter_complex` into a
//! wired [`vaco_filter_core::Graph`]. It knows the grammar, the three levels of
//! escaping, how labels connect chains, and which converter repairs which kind
//! of format mismatch. It knows **no filters**: they arrive through
//! [`registry::FilterRegistry`], which is what lets the whole language be
//! tested against `vaco-filter-core`'s mock filters.
//!
//! # The pipeline
//!
//! ```text
//! text ──ast::parse──> Ast ──build::build──> BuiltGraph ──configure──> Graph
//!          grammar,        labels, pad          negotiation and
//!          escaping        resolution           auto-conversion
//! ```
//!
//! # Escaping has three levels and they are not the same level
//!
//! Plan 13 §1b records two occasions on which an agent measured a
//! *filtergraph's* unescaping and attributed it to the parser underneath. This
//! crate **is** that unescaping, so the levels are named rather than blurred:
//!
//! | Level | Who applies it | Stops at | Where |
//! |---|---|---|---|
//! | 3 | the shell | shell metacharacters | not ours |
//! | 2 | the graph scanner | `[` `]` `,` `;` | [`lex::next_token`] with [`lex::StopSet::GRAPH`] |
//! | 1 | the option scanner | `:` then `=` | [`ast::FilterSpec::arguments`] |
//! | 0 | a list-valued option | `\|` | [`ast::Arg::list_values`] |
//!
//! Each level removes one backslash, which is why the canonical rule of thumb
//! is that **each level doubles them**. The graph scanner *does* unescape —
//! measured, and the single most load-bearing fact in this crate:
//!
//! ```sh
//! ffmpeg -f lavfi -i "color=s=32x32:d=0.04,setpts=@\:@" ...
//! #  -> the argument list splits at that colon, so `\:` had already become `:`
//! #     by the time the option layer saw it.
//! ```
//!
//! # Untrusted input
//!
//! A graph description arrives from a command line or a configuration file.
//! Nothing here panics, recurses or allocates unboundedly: the parser is a flat
//! loop over `&str` with no recursion at all, so deeply nested brackets cost
//! bytes rather than stack. `fuzz/fuzz_targets/graph_parse.rs` is the standing
//! proof.

#![forbid(unsafe_code)]

pub mod ast;
pub mod build;
pub mod convert;
pub mod error;
pub mod lex;
pub mod mock;
pub mod registry;
pub mod span;

pub use ast::{Arg, Ast, Chain, FilterSpec, Label, SWS_PREFIX, parse};
pub use build::{BuiltGraph, NodeInfo, OpenPad, build, parse_and_build};
pub use convert::{AUDIO_CONVERTER, DefaultConverters, VIDEO_CONVERTER};
pub use error::{ErrorKind, GraphError};
pub use lex::{Quirk, StopSet};
pub use registry::{FilterRegistry, Instance, Instantiate, pads};
pub use span::Span;
