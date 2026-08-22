//! Derive macros for `vaco-opts`.
//!
//! Two macros:
//!
//! * `#[derive(Options)]` — projects a struct into an option schema plus an
//!   indexed, type-erased accessor.
//! * `#[derive(OptEnum)]` — turns a fieldless enum into a unit of named
//!   constants.
//!
//! `opt_flags!` is a `macro_rules!` macro and lives in `vaco-opts` itself.
//!
//! Everything the expansion needs is reached through `::vaco_opts::…`, so a
//! consumer that renames the dependency will not compile. That is the usual
//! trade in the ecosystem and is not worth a `crate = "…"` escape hatch until
//! someone actually needs one.

mod attrs;
mod gen_enum;
mod gen_options;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Project a struct into a `vaco_opts::Schema` and a `vaco_opts::Options` impl.
///
/// See the crate-level docs of `vaco-opts` for the attribute grammar.
#[proc_macro_derive(Options, attributes(options, opt))]
pub fn derive_options(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    gen_options::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Turn a fieldless enum into a unit of named constants.
#[proc_macro_derive(OptEnum, attributes(opt_enum, opt_const))]
pub fn derive_opt_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    gen_enum::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[cfg(test)]
mod tests;
