//! Attribute declarations consumed by `vaco-vecheck`.
//!
//! The attribute is intentionally code-generation neutral: it documents that a
//! free-function kernel is governed by a checked `vecheck.toml` entry, while
//! keeping the body and its target-feature context exactly as the author wrote
//! them. The checker, rather than macro expansion, obtains LLVM remarks and
//! disassembly from the real compiler invocation.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Declare a free function to be checked by `vaco-vecheck`.
///
/// The corresponding `vecheck.toml` kernel entry is the sole source for the
/// stable contract id and compiler symbol. The function must use
/// `#[inline(always)]`: SIMD kernels need that guarantee for their dispatched
/// target-feature context to reach the loop body.
///
/// ```
/// use vaco_vecheck_macros::must_vectorize;
///
/// #[must_vectorize]
/// #[inline(always)]
/// fn add_one(values: &mut [u32]) {
///     for value in values {
///         *value += 1;
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn must_vectorize(attributes: TokenStream, item: TokenStream) -> TokenStream {
    if !attributes.is_empty() {
        return syn::Error::new_spanned(
            proc_macro2::TokenStream::from(attributes),
            "must_vectorize does not accept arguments; declare the id and symbol in vecheck.toml",
        )
        .into_compile_error()
        .into();
    }
    let function = parse_macro_input!(item as ItemFn);
    if !has_inline_always(&function) {
        return syn::Error::new_spanned(
            &function.sig.ident,
            "must_vectorize functions must declare #[inline(always)]",
        )
        .into_compile_error()
        .into();
    }
    quote!(#function).into()
}

fn has_inline_always(function: &ItemFn) -> bool {
    function.attrs.iter().any(|attribute| {
        attribute.path().is_ident("inline")
            && attribute
                .parse_args_with(|input: syn::parse::ParseStream<'_>| {
                    let ident: syn::Ident = input.parse()?;
                    Ok(ident == "always")
                })
                .is_ok_and(|always| always)
    })
}

#[cfg(test)]
mod tests {
    use super::has_inline_always;
    use syn::parse_quote;

    #[test]
    fn recognizes_inline_always() {
        let function = parse_quote! {
            #[inline(always)]
            fn vector_kernel() {}
        };
        assert!(has_inline_always(&function));
    }

    #[test]
    fn rejects_plain_inline() {
        let function = parse_quote! {
            #[inline]
            fn scalar_helper() {}
        };
        assert!(!has_inline_always(&function));
    }
}
