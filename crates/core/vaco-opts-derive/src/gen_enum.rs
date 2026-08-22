//! Code generation for `#[derive(OptEnum)]`.
//!
//! The unit mechanism is how `FFmpeg` exposes enum choices generically: a set of
//! named constants grouped under a `unit` string, which several options may
//! share and which `-h` prints beneath each option that references it. A Rust
//! enum is the natural carrier, so this derive turns one into that set.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr};

struct EnumAttrs {
    unit: String,
    base: TokenStream,
}

struct VariantSpec {
    ident: syn::Ident,
    name: String,
    help: String,
}

fn base_tokens(s: &str, span: proc_macro2::Span) -> syn::Result<TokenStream> {
    let v = match s {
        "int" => quote! { ::vaco_opts::OptBase::Int },
        "int64" => quote! { ::vaco_opts::OptBase::Int64 },
        "uint" => quote! { ::vaco_opts::OptBase::UInt },
        "uint64" => quote! { ::vaco_opts::OptBase::UInt64 },
        "flags" => quote! { ::vaco_opts::OptBase::Flags },
        "double" => quote! { ::vaco_opts::OptBase::Double },
        "float" => quote! { ::vaco_opts::OptBase::Float },
        _ => {
            return Err(syn::Error::new(
                span,
                "unknown `base`; expected one of: int, int64, uint, uint64, flags, double, float",
            ));
        }
    };
    Ok(v)
}

pub(crate) fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(OptEnum)]` does not support generic parameters",
        ));
    }
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(OptEnum)]` only applies to enums",
        ));
    };

    let Some(attr) = input.attrs.iter().find(|a| a.path().is_ident("opt_enum")) else {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "missing `#[opt_enum(unit = \"…\")]` on the enum",
        ));
    };
    let mut unit: Option<String> = None;
    let mut base: Option<TokenStream> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("unit") {
            unit = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("base") {
            let l: LitStr = meta.value()?.parse()?;
            base = Some(base_tokens(&l.value(), l.span())?);
        } else {
            return Err(meta.error("unknown key in `#[opt_enum(…)]`; expected one of: unit, base"));
        }
        Ok(())
    })?;
    let Some(unit) = unit else {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`#[opt_enum(…)]` requires `unit = \"…\"`",
        ));
    };
    let attrs = EnumAttrs {
        unit,
        base: base.unwrap_or_else(|| quote! { ::vaco_opts::OptBase::Int }),
    };

    let mut variants = Vec::new();
    for v in &data.variants {
        if !matches!(v.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                v,
                "`#[derive(OptEnum)]` needs a fieldless enum: every variant must map to one \
                 integer constant",
            ));
        }
        let mut name: Option<String> = None;
        let mut help = String::new();
        if let Some(a) = v.attrs.iter().find(|a| a.path().is_ident("opt_const")) {
            a.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    name = Some(meta.value()?.parse::<LitStr>()?.value());
                } else if meta.path.is_ident("help") {
                    help = meta.value()?.parse::<LitStr>()?.value();
                } else {
                    return Err(
                        meta.error("unknown key in `#[opt_const(…)]`; expected one of: name, help")
                    );
                }
                Ok(())
            })?;
        }
        variants.push(VariantSpec {
            ident: v.ident.clone(),
            name: name.unwrap_or_else(|| v.ident.to_string().to_lowercase()),
            help,
        });
    }
    if variants.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(OptEnum)]` needs at least one variant",
        ));
    }

    Ok(codegen(input, &attrs, &variants))
}

fn codegen(input: &DeriveInput, attrs: &EnumAttrs, variants: &[VariantSpec]) -> TokenStream {
    let ident = &input.ident;
    let unit = &attrs.unit;
    let base = &attrs.base;

    let consts = variants.iter().map(|v| {
        let vi = &v.ident;
        let name = &v.name;
        let help = &v.help;
        quote! {
            ::vaco_opts::ConstDesc {
                name: #name,
                help: #help,
                unit: #unit,
                value: ::vaco_opts::ConstValue::Int(#ident::#vi as i64),
                flags: ::vaco_opts::OptFlags::NONE,
            }
        }
    });

    let disc_arms = variants.iter().map(|v| {
        let vi = &v.ident;
        quote! { Self::#vi => #ident::#vi as i64 }
    });

    let try_arms = variants.iter().map(|v| {
        let vi = &v.ident;
        quote! {
            if value == (#ident::#vi as i64) {
                return ::core::result::Result::Ok(#ident::#vi);
            }
        }
    });

    quote! {
        #[allow(
            unused_qualifications,
            clippy::all,
            clippy::pedantic,
            clippy::nursery
        )]
        const _: () = {
            impl #ident {
                /// This variant's integer value, as the option system sees it.
                #[allow(dead_code)]
                pub fn opt_discriminant(&self) -> i64 {
                    match self { #(#disc_arms),* }
                }
                /// The unit these constants are grouped under.
                #[allow(dead_code)]
                pub const UNIT: &'static str = #unit;
            }

            impl ::vaco_opts::OptEnumConsts for #ident {
                const CONSTS: &'static [::vaco_opts::ConstDesc] = &[ #(#consts),* ];
            }

            impl ::core::convert::TryFrom<i64> for #ident {
                type Error = ();
                fn try_from(value: i64) -> ::core::result::Result<Self, ()> {
                    #(#try_arms)*
                    ::core::result::Result::Err(())
                }
            }

            impl ::vaco_opts::OptValueKind for #ident {
                const BASE: ::vaco_opts::OptBase = #base;
            }

            impl ::vaco_opts::OptValue for #ident {
                fn parse_into(
                    &mut self,
                    s: &str,
                    ctx: &::vaco_opts::ParseCtx<'_>,
                ) -> ::core::result::Result<(), ::vaco_opts::OptError> {
                    let t = s.trim();
                    // The schema's constants first, then this type's own, so an
                    // enum parses correctly even outside a schema.
                    let from_const = ctx
                        .consts
                        .iter()
                        .chain(<Self as ::vaco_opts::OptEnumConsts>::CONSTS.iter())
                        .find(|c| c.name == t)
                        .and_then(|c| c.value.as_i64());
                    let raw = match from_const {
                        ::core::option::Option::Some(v) => ::core::option::Option::Some(v),
                        ::core::option::Option::None => ::vaco_opts::parse_integer(t)
                            .and_then(|v| i64::try_from(v).ok()),
                    };
                    match raw.and_then(|v| <Self as ::core::convert::TryFrom<i64>>::try_from(v).ok()) {
                        ::core::option::Option::Some(v) => {
                            *self = v;
                            ::core::result::Result::Ok(())
                        }
                        ::core::option::Option::None => {
                            ::core::result::Result::Err(::vaco_opts::OptError::UnknownConst {
                                name: ctx.name.to_owned(),
                                value: s.to_owned(),
                            })
                        }
                    }
                }

                fn serialize(&self, out: &mut ::std::string::String, ctx: &::vaco_opts::SerCtx<'_>) {
                    let v = self.opt_discriminant();
                    let name = ctx
                        .consts
                        .iter()
                        .chain(<Self as ::vaco_opts::OptEnumConsts>::CONSTS.iter())
                        .find(|c| c.value.as_i64() == ::core::option::Option::Some(v))
                        .map(|c| c.name);
                    match name {
                        ::core::option::Option::Some(n) => out.push_str(n),
                        ::core::option::Option::None => out.push_str(&v.to_string()),
                    }
                }

                fn as_f64(&self) -> ::core::option::Option<f64> {
                    ::core::option::Option::Some(self.opt_discriminant() as f64)
                }

                ::vaco_opts::impl_opt_value_common!(#ident);
            }
        };
    }
}
