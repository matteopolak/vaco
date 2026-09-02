//! Code generation for `#[derive(CliOptionTable)]`.
//!
//! Turns a fieldless enum into a `&'static [vaco_cli_core::table::OptDesc]` --
//! the CLI's own argv-flag descriptors (`-i`, `-c`, `-loglevel`, ...), which
//! are a different flag universe from `vaco_opts::OptFlags`'s AVOption-style
//! encoding/decoding/filtering bits (see `vaco_cli_core::table::ArgFlags`'s
//! own doc for why the two are kept apart -- conflating them once was a real
//! hazard). One variant per option; an alias variant names its canonical
//! target with `alias_of` and inherits whatever it does not override itself,
//! so the two can no longer drift the silent way two independently-typed
//! `o(...)`/`alias(...)` calls could: this macro checks at compile time that
//! every `alias_of` actually names another variant in the same enum, and that
//! no two variants share a name.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, LitStr};

/// Every name `flags(...)` accepts, matching `vaco_cli_core::table::ArgFlags`'s
/// own associated constants exactly -- kept as a fixed list (rather than
/// accepting any identifier) so a typo is a compile error naming the bad
/// token, not a silently-ignored flag.
const ARG_FLAG_NAMES: &[&str] = &[
    "HAS_ARG",
    "GLOBAL",
    "PER_FILE",
    "INPUT",
    "OUTPUT",
    "PER_STREAM",
    "TAKES_SPEC",
    "EXPERT",
    "EXIT",
    "OPENS_INPUT",
    "VIDEO",
    "AUDIO",
    "SUBTITLE",
    "DATA",
    "OPTIONAL_ARG",
];

/// Every `kind = ...` value, matching `vaco_cli_core::value::ValueKind`'s
/// variants exactly.
const VALUE_KINDS: &[&str] = &[
    "None", "Float", "Int", "Int64", "Expr", "Duration", "Rate", "Size", "Color", "Str", "Custom",
];

/// One variant's attributes, before alias resolution.
struct RawVariant {
    ident: syn::Ident,
    span: Span,
    name: String,
    name_span: Span,
    argname: Option<String>,
    flags: Option<Vec<String>>,
    kind: Option<String>,
    help: Option<String>,
    alias_of: Option<String>,
    alias_of_span: Span,
    spec: Option<String>,
}

/// One variant, fully resolved: every field an alias would otherwise share
/// with its target has been copied in, so codegen never has to look sideways.
struct ResolvedVariant {
    name: String,
    argname: Option<String>,
    flags: Vec<String>,
    kind: String,
    help: String,
    alias_of: Option<(String, String)>,
}

pub(crate) fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(CliOptionTable)]` does not support generic parameters",
        ));
    }
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(CliOptionTable)]` only applies to enums",
        ));
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(CliOptionTable)]` needs at least one variant",
        ));
    }

    let mut raw = Vec::new();
    for v in &data.variants {
        if !matches!(v.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                v,
                "`#[derive(CliOptionTable)]` needs a fieldless enum: every variant is one CLI \
                 option",
            ));
        }
        raw.push(parse_variant(v)?);
    }

    let mut seen: Vec<(&str, &syn::Ident)> = Vec::new();
    for v in &raw {
        if let Some((_, prev_ident)) = seen.iter().find(|(n, _)| *n == v.name) {
            return Err(syn::Error::new(
                v.name_span,
                format!(
                    "option name `{}` is already used by `{prev_ident}` -- every option and \
                     alias needs a distinct spelling",
                    v.name
                ),
            ));
        }
        seen.push((&v.name, &v.ident));
    }

    let resolved = raw
        .iter()
        .map(|v| resolve(v, &raw))
        .collect::<syn::Result<Vec<_>>>()?;

    Ok(codegen(input, &resolved))
}

fn parse_variant(v: &syn::Variant) -> syn::Result<RawVariant> {
    let span = v.span_for_errors();
    let Some(attr) = v.attrs.iter().find(|a| a.path().is_ident("cli")) else {
        return Err(syn::Error::new(
            span,
            format!(
                "variant `{}` has no `#[cli(...)]`; every variant needs `name = \"...\"` plus \
                 either `flags(...)`, `kind = ...` and `help = \"...\"`, or `alias_of = \"...\"`",
                v.ident
            ),
        ));
    };

    let mut name: Option<(String, Span)> = None;
    let mut argname: Option<String> = None;
    let mut flags: Option<Vec<String>> = None;
    let mut kind: Option<String> = None;
    let mut help: Option<String> = None;
    let mut alias_of: Option<(String, Span)> = None;
    let mut spec: Option<String> = None;

    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            let l: LitStr = meta.value()?.parse()?;
            name = Some((l.value(), l.span()));
        } else if meta.path.is_ident("argname") {
            argname = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("help") {
            help = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("alias_of") {
            let l: LitStr = meta.value()?.parse()?;
            alias_of = Some((l.value(), l.span()));
        } else if meta.path.is_ident("spec") {
            spec = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("kind") {
            let ident: syn::Ident = meta.value()?.parse()?;
            let s = ident.to_string();
            if !VALUE_KINDS.contains(&s.as_str()) {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("unknown `kind`; expected one of: {}", VALUE_KINDS.join(", ")),
                ));
            }
            kind = Some(s);
        } else if meta.path.is_ident("flags") {
            let mut fs = Vec::new();
            meta.parse_nested_meta(|m| {
                let Some(id) = m.path.get_ident() else {
                    return Err(m.error("expected a flag name"));
                };
                let s = id.to_string();
                if !ARG_FLAG_NAMES.contains(&s.as_str()) {
                    return Err(syn::Error::new(
                        id.span(),
                        format!(
                            "unknown flag `{s}`; expected one of: {}",
                            ARG_FLAG_NAMES.join(", ")
                        ),
                    ));
                }
                fs.push(s);
                Ok(())
            })?;
            flags = Some(fs);
        } else {
            return Err(meta.error(
                "unknown key in `#[cli(...)]`; expected one of: name, argname, flags, kind, \
                 help, alias_of, spec",
            ));
        }
        Ok(())
    })?;

    let Some((name, name_span)) = name else {
        return Err(syn::Error::new(span, format!("variant `{}` needs `name = \"...\"`", v.ident)));
    };
    if spec.is_some() && alias_of.is_none() {
        return Err(syn::Error::new(
            span,
            format!("variant `{}` sets `spec` without `alias_of`", v.ident),
        ));
    }
    let (alias_of, alias_of_span) = match alias_of {
        Some((t, s)) => (Some(t), s),
        None => (None, span),
    };

    Ok(RawVariant {
        ident: v.ident.clone(),
        span,
        name,
        name_span,
        argname,
        flags,
        kind,
        help,
        alias_of,
        alias_of_span,
        spec,
    })
}

fn resolve(v: &RawVariant, all: &[RawVariant]) -> syn::Result<ResolvedVariant> {
    let Some(target_name) = &v.alias_of else {
        let Some(flags) = v.flags.clone() else {
            return Err(syn::Error::new(
                v.span,
                format!("option `{}` needs `flags(...)` (or `alias_of = \"...\"`)", v.ident),
            ));
        };
        let Some(kind) = v.kind.clone() else {
            return Err(syn::Error::new(
                v.span,
                format!("option `{}` needs `kind = ...` (or `alias_of = \"...\"`)", v.ident),
            ));
        };
        let Some(help) = v.help.clone() else {
            return Err(syn::Error::new(
                v.span,
                format!("option `{}` needs `help = \"...\"` (or `alias_of = \"...\"`)", v.ident),
            ));
        };
        return Ok(ResolvedVariant {
            name: v.name.clone(),
            argname: v.argname.clone(),
            flags,
            kind,
            help,
            alias_of: None,
        });
    };

    let spec = v.spec.clone().unwrap_or_default();
    let is_self = target_name == &v.name;

    // A non-empty `spec` self-alias is a real, meaningful shape: `-b`'s own
    // entry names itself as its `alias_of` target with `spec = "v"`, which is
    // how a bare `-b` (no specifier the user typed) means `-b:v` -- see
    // `ParsedOption::resolved`. An *empty*-spec self-alias resolves
    // identically to having no `alias_of` at all, so it can only be a mistake.
    if is_self && spec.is_empty() {
        return Err(syn::Error::new(
            v.alias_of_span,
            format!(
                "`{}` aliases itself with an empty `spec`, which is a no-op identical to \
                 having no `alias_of` at all -- remove it, or give it the specifier this \
                 option's bare form should imply",
                v.ident
            ),
        ));
    }

    let Some(target) = all.iter().find(|o| &o.name == target_name) else {
        return Err(syn::Error::new(
            v.alias_of_span,
            format!(
                "`alias_of = \"{target_name}\"` does not name another option in this table -- \
                 checked at compile time so this cannot drift silently the way an unchecked \
                 string could"
            ),
        ));
    };
    // A target that is itself a *self*-alias (see above) behaves like a
    // canonical option in every respect that matters here -- it declares its
    // own `flags`/`kind`/`help` rather than inheriting them -- so chaining
    // through it is not the silent-drift hazard a real A -> B -> C chain
    // would be. Only forbid pointing at a target that itself points
    // somewhere *else*.
    if !is_self
        && target
            .alias_of
            .as_ref()
            .is_some_and(|t| t != &target.name)
    {
        return Err(syn::Error::new(
            v.alias_of_span,
            format!(
                "`{}` aliases `{target_name}`, which is itself an alias -- point at the \
                 canonical option instead of chaining aliases",
                v.ident
            ),
        ));
    }

    // A self-alias has nothing else to inherit from -- it must state its own
    // `flags`/`kind`/`help` (and `argname`, if any) explicitly, same as a
    // plain option.
    let (argname, flags, kind, help) = if is_self {
        (
            v.argname.clone(),
            v.flags.clone().ok_or_else(|| {
                syn::Error::new(v.span, format!("option `{}` needs `flags(...)`", v.ident))
            })?,
            v.kind.clone().ok_or_else(|| {
                syn::Error::new(v.span, format!("option `{}` needs `kind = ...`", v.ident))
            })?,
            v.help.clone().ok_or_else(|| {
                syn::Error::new(v.span, format!("option `{}` needs `help = \"...\"`", v.ident))
            })?,
        )
    } else {
        (
            v.argname.clone().or_else(|| target.argname.clone()),
            v.flags.clone().or_else(|| target.flags.clone()).ok_or_else(|| {
                syn::Error::new(v.span, "internal error: canonical option missing `flags`")
            })?,
            v.kind.clone().or_else(|| target.kind.clone()).ok_or_else(|| {
                syn::Error::new(v.span, "internal error: canonical option missing `kind`")
            })?,
            v.help.clone().or_else(|| target.help.clone()).ok_or_else(|| {
                syn::Error::new(v.span, "internal error: canonical option missing `help`")
            })?,
        )
    };

    Ok(ResolvedVariant {
        name: v.name.clone(),
        argname,
        flags,
        kind,
        help,
        alias_of: Some((target.name.clone(), spec)),
    })
}

fn codegen(input: &DeriveInput, variants: &[ResolvedVariant]) -> TokenStream {
    let ident = &input.ident;

    let entries = variants.iter().map(|v| {
        let name = &v.name;
        let help = &v.help;
        let argname_tokens = if let Some(a) = &v.argname {
            quote! { ::core::option::Option::Some(#a) }
        } else {
            quote! { ::core::option::Option::None }
        };
        let flags_tokens = if v.flags.is_empty() {
            quote! { ::vaco_cli_core::table::ArgFlags::NONE }
        } else {
            // `.union(...)` rather than `|`: `BitOr::bitor` is a normal trait
            // method, not `const fn`, and this table is built in a `const`
            // context.
            let flag_idents = v.flags.iter().map(|f| format_ident!("{}", f));
            quote! {
                ::vaco_cli_core::table::ArgFlags::NONE
                #(.union(::vaco_cli_core::table::ArgFlags::#flag_idents))*
            }
        };
        let kind_ident = format_ident!("{}", v.kind);
        let alias_tokens = if let Some((target, spec)) = &v.alias_of {
            quote! { ::core::option::Option::Some((#target, #spec)) }
        } else {
            quote! { ::core::option::Option::None }
        };
        quote! {
            ::vaco_cli_core::table::OptDesc {
                name: #name,
                argname: #argname_tokens,
                flags: #flags_tokens,
                kind: ::vaco_cli_core::table::ValueKind::#kind_ident,
                help: #help,
                alias_of: #alias_tokens,
            }
        }
    });

    quote! {
        #[allow(unused_qualifications, clippy::all, clippy::pedantic, clippy::nursery)]
        const _: () = {
            impl #ident {
                /// This option table, in declaration order. Aliases carry their
                /// resolved (inherited-unless-overridden) `argname`/`flags`/`kind`/
                /// `help`, so this is a plain, self-contained `OptDesc` list --
                /// every consumer keeps working exactly as it did against the old
                /// hand-written array.
                #[allow(dead_code)]
                pub(crate) const OPTIONS: &'static [::vaco_cli_core::table::OptDesc] =
                    &[ #(#entries),* ];
            }
        };
    }
}

trait SpanForErrors {
    fn span_for_errors(&self) -> Span;
}

impl SpanForErrors for syn::Variant {
    fn span_for_errors(&self) -> Span {
        syn::spanned::Spanned::span(self)
    }
}
