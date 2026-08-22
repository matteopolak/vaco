//! Code generation for `#[derive(Options)]`.
//!
//! The macro's entire job is to project struct fields into an indexed,
//! type-erased accessor and to lift attributes into a static table. No parsing,
//! serialisation or help formatting is generated: those live once in
//! `vaco-opts` and operate on `&mut dyn OptValue` plus `&OptionDesc`.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Expr, Fields, Lit};

use crate::attrs::{
    ArrayAttrs, ClassAttrs, FieldMode, FieldSpec, OptAttrs, inner_type, parse_class_attrs,
    parse_field,
};

pub(crate) fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(Options)]` does not support generic parameters: the schema lives in a \
             `static`, which cannot mention them",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(Options)]` only applies to structs",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(Options)]` needs a struct with named fields",
        ));
    };

    let class = parse_class_attrs(&input.attrs, &input.ident)?;

    let mut specs = Vec::new();
    let mut errors: Option<syn::Error> = None;
    for f in &named.named {
        match parse_field(f) {
            Ok(s) => specs.push(s),
            Err(e) => match &mut errors {
                Some(acc) => acc.combine(e),
                None => errors = Some(e),
            },
        }
    }
    if let Some(e) = errors {
        return Err(e);
    }
    check_duplicate_names(&specs)?;

    Ok(codegen(input, &class, &specs))
}

fn check_duplicate_names(specs: &[FieldSpec]) -> syn::Result<()> {
    let mut seen: Vec<(String, proc_macro2::Span)> = Vec::new();
    for s in specs {
        let FieldMode::Opt(a) = &s.mode else { continue };
        let primary = a.name.clone().unwrap_or_else(|| s.ident.to_string());
        for n in core::iter::once(primary).chain(a.aliases.iter().cloned()) {
            if seen.iter().any(|(prev, _)| *prev == n) {
                return Err(syn::Error::new(
                    s.span,
                    format!("duplicate option name or alias `{n}`"),
                ));
            }
            seen.push((n, s.span));
        }
    }
    Ok(())
}

struct Projected<'a> {
    id: u16,
    spec: &'a FieldSpec,
    attrs: &'a OptAttrs,
}

fn codegen(input: &DeriveInput, class: &ClassAttrs, specs: &[FieldSpec]) -> TokenStream {
    let ident = &input.ident;
    let class_name = &class.name;
    let class_help = &class.help;

    let mut opts: Vec<Projected<'_>> = Vec::new();
    let mut children: Vec<&FieldSpec> = Vec::new();
    for s in specs {
        match &s.mode {
            FieldMode::Opt(a) => {
                let id = u16::try_from(opts.len()).unwrap_or(u16::MAX);
                opts.push(Projected {
                    id,
                    spec: s,
                    attrs: a,
                });
            }
            FieldMode::Child => children.push(s),
            FieldMode::Skip => {}
        }
    }

    let descs = opts.iter().map(desc_expr);

    let child_schemas = children.iter().map(|c| {
        let ty = &c.ty;
        quote! { <#ty as ::vaco_opts::HasSchema>::SCHEMA }
    });

    let slot_arms = opts.iter().map(|p| {
        let id = p.id;
        let f = &p.spec.ident;
        quote! { #id => ::core::option::Option::Some(&self.#f as &dyn ::vaco_opts::OptValue) }
    });
    let slot_mut_arms = opts.iter().map(|p| {
        let id = p.id;
        let f = &p.spec.ident;
        quote! { #id => ::core::option::Option::Some(&mut self.#f as &mut dyn ::vaco_opts::OptValue) }
    });

    let children_fn = if children.is_empty() {
        quote! {}
    } else {
        let refs = children.iter().map(|c| {
            let f = &c.ident;
            quote! { &self.#f as &dyn ::vaco_opts::Options }
        });
        let muts = children.iter().map(|c| {
            let f = &c.ident;
            quote! { &mut self.#f as &mut dyn ::vaco_opts::Options }
        });
        quote! {
            fn children(&self) -> ::std::vec::Vec<&dyn ::vaco_opts::Options> {
                ::std::vec![ #(#refs),* ]
            }
            fn children_mut(&mut self) -> ::std::vec::Vec<&mut dyn ::vaco_opts::Options> {
                ::std::vec![ #(#muts),* ]
            }
        }
    };

    let range_arms: Vec<TokenStream> = opts
        .iter()
        .filter_map(|p| {
            let (lo, hi) = p.attrs.range.as_ref()?;
            let id = p.id;
            let f = &p.spec.ident;
            let name = option_name(p);
            Some(quote! {
                #id => ::vaco_opts::__rt::check_range(&self.#f, #lo, #hi, #name)
            })
        })
        .collect();
    let range_fn = if range_arms.is_empty() {
        quote! {}
    } else {
        quote! {
            fn check_range(&self, id: ::vaco_opts::OptId)
                -> ::core::result::Result<(), ::vaco_opts::OptError>
            {
                match id.0 {
                    #(#range_arms,)*
                    _ => ::core::result::Result::Ok(()),
                }
            }
        }
    };

    let default_impl = if class.no_default {
        quote! {}
    } else {
        let inits = specs.iter().map(|s| {
            let f = &s.ident;
            let explicit = match &s.mode {
                FieldMode::Opt(a) => a.default.as_ref(),
                FieldMode::Child | FieldMode::Skip => None,
            };
            explicit.map_or_else(
                || quote! { #f: ::core::default::Default::default() },
                |e| quote! { #f: #e },
            )
        });
        quote! {
            impl ::core::default::Default for #ident {
                fn default() -> Self {
                    Self { #(#inits),* }
                }
            }
        }
    };

    quote! {
        #[allow(
            non_snake_case,
            unused_qualifications,
            unused_imports,
            clippy::all,
            clippy::pedantic,
            clippy::nursery
        )]
        const _: () = {
            static OPTS: &[::vaco_opts::OptionDesc] = &[ #(#descs,)* ];

            static SCHEMA: ::vaco_opts::Schema = ::vaco_opts::Schema {
                class_name: #class_name,
                class_help: #class_help,
                options: OPTS,
                children: &[ #(#child_schemas),* ],
            };

            impl ::vaco_opts::HasSchema for #ident {
                const SCHEMA: &'static ::vaco_opts::Schema = &SCHEMA;
            }

            impl ::vaco_opts::Options for #ident {
                fn schema(&self) -> &'static ::vaco_opts::Schema { &SCHEMA }

                fn slot(&self, id: ::vaco_opts::OptId)
                    -> ::core::option::Option<&dyn ::vaco_opts::OptValue>
                {
                    match id.0 {
                        #(#slot_arms,)*
                        _ => ::core::option::Option::None,
                    }
                }

                fn slot_mut(&mut self, id: ::vaco_opts::OptId)
                    -> ::core::option::Option<&mut dyn ::vaco_opts::OptValue>
                {
                    match id.0 {
                        #(#slot_mut_arms,)*
                        _ => ::core::option::Option::None,
                    }
                }

                fn as_dyn(&self) -> &dyn ::vaco_opts::Options { self }
                fn as_dyn_mut(&mut self) -> &mut dyn ::vaco_opts::Options { self }

                #children_fn

                fn defaults(&self) -> ::std::boxed::Box<dyn ::vaco_opts::Options> {
                    ::std::boxed::Box::new(<Self as ::core::default::Default>::default())
                }

                #range_fn
            }

            #default_impl
        };
    }
}

fn option_name(p: &Projected<'_>) -> String {
    p.attrs
        .name
        .clone()
        .unwrap_or_else(|| p.spec.ident.to_string())
}

fn desc_expr(p: &Projected<'_>) -> TokenStream {
    let a = p.attrs;
    let ty = &p.spec.ty;
    let name = option_name(p);
    let aliases = &a.aliases;
    let help = a.help.clone().unwrap_or_default();
    let id = p.id;

    let array = match &a.array {
        None => quote! { ::core::option::Option::None },
        Some(ArrayAttrs {
            sep,
            min_len,
            max_len,
        }) => {
            let sep = sep.unwrap_or('|');
            let min = min_len.unwrap_or(0);
            let max = max_len.unwrap_or(u32::MAX);
            quote! {
                ::core::option::Option::Some(::vaco_opts::__rt::array(#sep, #min, #max))
            }
        }
    };

    let flags = flags_expr(&a.flags);

    let unit = a.unit.as_ref().map_or_else(
        || quote! { ::core::option::Option::None },
        |u| quote! { ::core::option::Option::Some(#u) },
    );

    let consts = match (&a.consts, &a.unit) {
        (Some(e), _) => quote! { #e },
        (None, Some(_)) => {
            let inner = inner_type(ty);
            quote! { <#inner as ::vaco_opts::OptEnumConsts>::CONSTS }
        }
        (None, None) => quote! { &[] },
    };

    let range = a.range.as_ref().map_or_else(
        || quote! { ::core::option::Option::None },
        |(lo, hi)| {
            quote! {
                ::core::option::Option::Some(
                    ::vaco_opts::__rt::range_display((#lo) as f64, (#hi) as f64)
                )
            }
        },
    );

    let default_repr = a
        .default_repr
        .clone()
        .or_else(|| a.default.as_ref().and_then(literal_repr))
        .unwrap_or_default();

    quote! {
        ::vaco_opts::OptionDesc {
            name: #name,
            aliases: &[ #(#aliases),* ],
            help: #help,
            kind: ::vaco_opts::OptKind {
                base: <#ty as ::vaco_opts::OptValueKind>::BASE,
                array: #array,
            },
            flags: #flags,
            unit: #unit,
            consts: #consts,
            range: #range,
            default_repr: #default_repr,
            id: ::vaco_opts::OptId(#id),
        }
    }
}

fn flags_expr(names: &[String]) -> TokenStream {
    if names.is_empty() {
        return quote! { ::vaco_opts::OptFlags::NONE };
    }
    let mut out = quote! { ::vaco_opts::OptFlags::NONE };
    for n in names {
        let konst = format_ident!("{}", n.to_uppercase());
        out = quote! { #out.union(::vaco_opts::OptFlags::#konst) };
    }
    out
}

/// Render a literal default the way `-h full` prints it. Non-literal defaults
/// (`SampleFormat::None`, `SwrFlags::empty()`) cannot be rendered at expansion
/// time; `OptionsExt::default_repr` computes those exactly at runtime.
fn literal_repr(e: &Expr) -> Option<String> {
    match e {
        Expr::Lit(l) => match &l.lit {
            Lit::Str(s) => Some(s.value()),
            Lit::Int(i) => Some(i.base10_digits().to_owned()),
            Lit::Float(f) => Some(f.base10_digits().to_owned()),
            Lit::Bool(b) => Some(b.value.to_string()),
            Lit::Char(c) => Some(c.value().to_string()),
            _ => None,
        },
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => {
            literal_repr(&u.expr).map(|s| format!("-{s}"))
        }
        Expr::Group(g) => literal_repr(&g.expr),
        Expr::Paren(p) => literal_repr(&p.expr),
        Expr::Path(p) if p.path.is_ident("None") => Some(String::new()),
        _ => None,
    }
}
