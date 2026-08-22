//! The attribute grammar.
//!
//! Hand-written against `syn` rather than built on `darling` (plan 11 §6.8):
//! the grammar has a repeatable `alias`, a `flags(...)` list, a typed `range`
//! expression and a `default` const expression, and bending a generic
//! attribute-derive crate around that costs more than owning ~400 lines.

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprRange, Field, Ident, LitChar, LitInt, LitStr, Type};

/// Every flag name `flags(...)` accepts, in the order `-h full` prints them.
pub(crate) const FLAG_NAMES: &[&str] = &[
    "encoding",
    "decoding",
    "filtering",
    "video",
    "audio",
    "subtitle",
    "export",
    "readonly",
    "bsf",
    "runtime",
    "deprecated",
    "child_consts",
    "param",
];

/// Every key `#[opt(...)]` accepts.
const OPT_KEYS: &[&str] = &[
    "name",
    "alias",
    "help",
    "default",
    "default_repr",
    "range",
    "unit",
    "consts",
    "flags",
    "array",
    "child",
    "skip",
];

/// Types for which `range = …` is meaningless.
const NON_NUMERIC: &[&str] = &["String", "str", "bool", "Dict", "Binary", "Rgba"];

/// Types for which `unit = …` is meaningless: a unit groups *named numeric
/// constants*, and none of these carries a number.
const NON_UNIT: &[&str] = &["String", "str", "Dict", "Binary", "Rgba", "bool"];

// ------------------------------------------------------------------ struct

#[derive(Debug)]
pub(crate) struct ClassAttrs {
    pub name: String,
    pub help: String,
    /// Suppress the generated `impl Default`, for a type that writes its own.
    pub no_default: bool,
}

pub(crate) fn parse_class_attrs(attrs: &[Attribute], ident: &Ident) -> syn::Result<ClassAttrs> {
    let Some(attr) = attrs.iter().find(|a| a.path().is_ident("options")) else {
        return Err(syn::Error::new(
            ident.span(),
            "missing `#[options(name = \"…\", help = \"…\")]` on the struct",
        ));
    };
    let mut name: Option<String> = None;
    let mut help: Option<String> = None;
    let mut no_default = false;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            name = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("help") {
            help = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("no_default") {
            no_default = true;
        } else {
            return Err(meta
                .error("unknown key in `#[options(…)]`; expected one of: name, help, no_default"));
        }
        Ok(())
    })?;
    let Some(name) = name else {
        return Err(syn::Error::new(
            attr.span(),
            "`#[options(…)]` requires `name = \"…\"`",
        ));
    };
    Ok(ClassAttrs {
        name,
        help: help.unwrap_or_default(),
        no_default,
    })
}

// ------------------------------------------------------------------- field

#[derive(Debug, Default, Clone)]
pub(crate) struct ArrayAttrs {
    pub sep: Option<char>,
    pub min_len: Option<u32>,
    pub max_len: Option<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct OptAttrs {
    pub name: Option<String>,
    pub aliases: Vec<String>,
    pub help: Option<String>,
    pub default: Option<Expr>,
    pub default_repr: Option<String>,
    pub range: Option<(Expr, Expr)>,
    pub unit: Option<String>,
    pub consts: Option<Expr>,
    pub flags: Vec<String>,
    pub array: Option<ArrayAttrs>,
}

#[derive(Debug)]
pub(crate) enum FieldMode {
    /// Not an option at all.
    Skip,
    /// A nested `Options` object.
    Child,
    Opt(Box<OptAttrs>),
}

#[derive(Debug)]
pub(crate) struct FieldSpec {
    pub ident: Ident,
    pub ty: Type,
    pub mode: FieldMode,
    pub span: Span,
}

pub(crate) fn parse_field(field: &Field) -> syn::Result<FieldSpec> {
    let Some(ident) = field.ident.clone() else {
        return Err(syn::Error::new(
            field.span(),
            "`#[derive(Options)]` needs a struct with named fields",
        ));
    };
    let span = field.span();
    let Some(attr) = field.attrs.iter().find(|a| a.path().is_ident("opt")) else {
        return Err(syn::Error::new(
            span,
            format!(
                "field `{ident}` has no `#[opt(…)]`; every field must be declared, \
                 skipped with `#[opt(skip)]`, or nested with `#[opt(child)]`"
            ),
        ));
    };

    let mut a = OptAttrs::default();
    let mut skip = false;
    let mut child = false;
    let mut other_keys = false;

    attr.parse_nested_meta(|meta| {
        let key = meta
            .path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_default();
        match key.as_str() {
            "skip" => skip = true,
            "child" => child = true,
            "name" => {
                other_keys = true;
                a.name = Some(meta.value()?.parse::<LitStr>()?.value());
            }
            "alias" => {
                other_keys = true;
                a.aliases.push(meta.value()?.parse::<LitStr>()?.value());
            }
            "help" => {
                other_keys = true;
                a.help = Some(meta.value()?.parse::<LitStr>()?.value());
            }
            "default" => {
                other_keys = true;
                a.default = Some(meta.value()?.parse::<Expr>()?);
            }
            "default_repr" => {
                other_keys = true;
                a.default_repr = Some(meta.value()?.parse::<LitStr>()?.value());
            }
            "unit" => {
                other_keys = true;
                a.unit = Some(meta.value()?.parse::<LitStr>()?.value());
            }
            "consts" => {
                other_keys = true;
                a.consts = Some(meta.value()?.parse::<Expr>()?);
            }
            "range" => {
                other_keys = true;
                let r: ExprRange = meta.value()?.parse()?;
                let is_inclusive = matches!(r.limits, syn::RangeLimits::Closed(_));
                if !is_inclusive {
                    return Err(syn::Error::new(
                        r.span(),
                        "`range` must be inclusive, written `a..=b`",
                    ));
                }
                let (Some(lo), Some(hi)) = (r.start.clone(), r.end.clone()) else {
                    return Err(syn::Error::new(
                        r.span(),
                        "`range` needs both bounds, written `a..=b`",
                    ));
                };
                a.range = Some((*lo, *hi));
            }
            "flags" => {
                other_keys = true;
                meta.parse_nested_meta(|m| {
                    let Some(id) = m.path.get_ident() else {
                        return Err(m.error("expected a flag name"));
                    };
                    let s = id.to_string();
                    if !FLAG_NAMES.contains(&s.as_str()) {
                        return Err(syn::Error::new(
                            id.span(),
                            format!(
                                "unknown flag `{s}`; expected one of: {}",
                                FLAG_NAMES.join(", ")
                            ),
                        ));
                    }
                    a.flags.push(s);
                    Ok(())
                })?;
            }
            "array" => {
                other_keys = true;
                let mut ar = ArrayAttrs::default();
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("sep") {
                        ar.sep = Some(m.value()?.parse::<LitChar>()?.value());
                    } else if m.path.is_ident("min_len") {
                        ar.min_len = Some(m.value()?.parse::<LitInt>()?.base10_parse()?);
                    } else if m.path.is_ident("max_len") {
                        ar.max_len = Some(m.value()?.parse::<LitInt>()?.base10_parse()?);
                    } else {
                        return Err(m.error(
                            "unknown key in `array(…)`; expected one of: sep, min_len, max_len",
                        ));
                    }
                    Ok(())
                })?;
                a.array = Some(ar);
            }
            _ => {
                return Err(meta.error(format!(
                    "unknown key in `#[opt(…)]`; expected one of: {}",
                    OPT_KEYS.join(", ")
                )));
            }
        }
        Ok(())
    })?;

    if skip && (child || other_keys) {
        return Err(syn::Error::new(
            span,
            "`#[opt(skip)]` cannot be combined with any other key",
        ));
    }
    if child && other_keys {
        return Err(syn::Error::new(
            span,
            "`#[opt(child)]` cannot be combined with any other key",
        ));
    }
    if skip {
        return Ok(FieldSpec {
            ident,
            ty: field.ty.clone(),
            mode: FieldMode::Skip,
            span,
        });
    }
    if child {
        return Ok(FieldSpec {
            ident,
            ty: field.ty.clone(),
            mode: FieldMode::Child,
            span,
        });
    }

    validate_opt(&a, &ident, &field.ty, span)?;
    Ok(FieldSpec {
        ident,
        ty: field.ty.clone(),
        mode: FieldMode::Opt(Box::new(a)),
        span,
    })
}

fn validate_opt(a: &OptAttrs, ident: &Ident, ty: &Type, span: Span) -> syn::Result<()> {
    if a.help.is_none() {
        return Err(syn::Error::new(
            span,
            format!("option `{ident}` is missing `help = \"…\"`"),
        ));
    }
    let vec_shaped = is_vec(ty);
    match (&a.array, vec_shaped) {
        (Some(_), false) => {
            return Err(syn::Error::new(
                span,
                format!("`array(…)` on `{ident}`, whose type is not `Vec<…>`"),
            ));
        }
        (None, true) => {
            return Err(syn::Error::new(
                span,
                format!("field `{ident}` is a `Vec<…>` and must declare `array(…)`"),
            ));
        }
        _ => {}
    }
    let inner = last_ident(inner_type(ty));
    if let Some(name) = inner.as_deref() {
        if a.range.is_some() && NON_NUMERIC.contains(&name) {
            return Err(syn::Error::new(
                span,
                format!("`range = …` on `{ident}`, whose type `{name}` carries no number"),
            ));
        }
        if a.unit.is_some() && NON_UNIT.contains(&name) {
            return Err(syn::Error::new(
                span,
                format!("`unit = …` on `{ident}`, whose type `{name}` has no named constants"),
            ));
        }
    }
    if a.consts.is_some() && a.unit.is_none() {
        return Err(syn::Error::new(
            span,
            format!("`consts = …` on `{ident}` needs a `unit = \"…\"` to group them under"),
        ));
    }
    Ok(())
}

// ------------------------------------------------------------ type helpers

fn path_ident(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(p) => p.path.segments.last(),
        _ => None,
    }
}

pub(crate) fn last_ident(ty: &Type) -> Option<String> {
    path_ident(ty).map(|s| s.ident.to_string())
}

pub(crate) fn is_vec(ty: &Type) -> bool {
    // `Vec<T>` or `Option<Vec<T>>`.
    if last_ident(ty).as_deref() == Some("Vec") {
        return true;
    }
    if last_ident(ty).as_deref() == Some("Option") {
        return generic_arg(ty).is_some_and(is_vec);
    }
    false
}

fn generic_arg(ty: &Type) -> Option<&Type> {
    let seg = path_ident(ty)?;
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|a| match a {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// Strip `Option<…>` and `Vec<…>` wrappers to reach the value type. This is the
/// type whose `OptEnumConsts` supplies a unit's named constants.
pub(crate) fn inner_type(ty: &Type) -> &Type {
    match last_ident(ty).as_deref() {
        Some("Option" | "Vec") => generic_arg(ty).map_or(ty, inner_type),
        _ => ty,
    }
}
