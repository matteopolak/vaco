//! Hand-rolled compile-fail suite.
//!
//! `trybuild` is the usual tool for this and plan 11 §6.9 asks for it, but it
//! is not in `[workspace.dependencies]` and this crate may not add one (D10).
//! So instead of compiling bad input and diffing rustc's output, these tests
//! drive the expansion functions directly and assert the message each rejection
//! produces. That covers the same ground — every attribute error has an
//! asserted message — without a new dependency, and it runs in milliseconds
//! rather than seconds.
//!
//! If `trybuild` is ever added to the workspace, keep these: they are a
//! superset of what a `.stderr` file pins, because they assert on the message
//! rather than on rustc's rendering of it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use syn::DeriveInput;

fn options_err(src: &str) -> String {
    let input: DeriveInput = syn::parse_str(src).expect("test input must be valid Rust");
    match crate::gen_options::expand(&input) {
        Ok(_) => panic!("expected an error, but expansion succeeded:\n{src}"),
        Err(e) => e.to_string(),
    }
}

fn options_ok(src: &str) {
    let input: DeriveInput = syn::parse_str(src).expect("test input must be valid Rust");
    if let Err(e) = crate::gen_options::expand(&input) {
        panic!("expected success, got: {e}\n{src}");
    }
}

fn enum_err(src: &str) -> String {
    let input: DeriveInput = syn::parse_str(src).expect("test input must be valid Rust");
    match crate::gen_enum::expand(&input) {
        Ok(_) => panic!("expected an error, but expansion succeeded:\n{src}"),
        Err(e) => e.to_string(),
    }
}

// ------------------------------------------------------------ struct level

#[test]
fn missing_options_attribute() {
    let e = options_err("struct S { }");
    assert!(e.contains("missing `#[options("), "{e}");
}

#[test]
fn options_without_name() {
    let e = options_err("#[options(help = \"x\")] struct S { }");
    assert!(e.contains("requires `name"), "{e}");
}

#[test]
fn unknown_struct_key() {
    let e = options_err("#[options(name = \"s\", bogus = 1)] struct S { }");
    assert!(e.contains("unknown key in `#[options"), "{e}");
}

#[test]
fn generics_rejected() {
    let e = options_err("#[options(name = \"s\")] struct S<T> { x: T }");
    assert!(e.contains("generic parameters"), "{e}");
}

#[test]
fn enum_rejected_by_options() {
    let e = options_err("#[options(name = \"s\")] enum S { A }");
    assert!(e.contains("only applies to structs"), "{e}");
}

#[test]
fn tuple_struct_rejected() {
    let e = options_err("#[options(name = \"s\")] struct S(i32);");
    assert!(e.contains("named fields"), "{e}");
}

// ------------------------------------------------------------- field level

#[test]
fn field_without_opt_attribute() {
    let e = options_err("#[options(name = \"s\")] struct S { a: i32 }");
    assert!(e.contains("has no `#[opt("), "{e}");
}

#[test]
fn missing_help() {
    let e = options_err("#[options(name = \"s\")] struct S { #[opt(name = \"a\")] a: i32 }");
    assert!(e.contains("missing `help"), "{e}");
}

#[test]
fn range_on_a_string() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { #[opt(help = \"h\", range = 0..=1)] a: String }",
    );
    assert!(e.contains("carries no number"), "{e}");
}

#[test]
fn unit_on_a_color() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { #[opt(help = \"h\", unit = \"u\")] a: Rgba }",
    );
    assert!(e.contains("no named constants"), "{e}");
}

#[test]
fn exclusive_range_rejected() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { #[opt(help = \"h\", range = 0..1)] a: i32 }",
    );
    assert!(e.contains("must be inclusive"), "{e}");
}

#[test]
fn array_on_a_non_vec() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { #[opt(help = \"h\", array(sep = '|'))] a: i32 }",
    );
    assert!(e.contains("is not `Vec"), "{e}");
}

#[test]
fn vec_without_array() {
    let e = options_err("#[options(name = \"s\")] struct S { #[opt(help = \"h\")] a: Vec<i32> }");
    assert!(e.contains("must declare `array"), "{e}");
}

#[test]
fn duplicate_name() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { \
         #[opt(name = \"a\", help = \"h\")] x: i32, \
         #[opt(name = \"a\", help = \"h\")] y: i32 }",
    );
    assert!(e.contains("duplicate option name"), "{e}");
}

#[test]
fn duplicate_alias_against_name() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { \
         #[opt(name = \"a\", help = \"h\")] x: i32, \
         #[opt(name = \"b\", alias = \"a\", help = \"h\")] y: i32 }",
    );
    assert!(e.contains("duplicate option name"), "{e}");
}

#[test]
fn unknown_field_key() {
    let e = options_err("#[options(name = \"s\")] struct S { #[opt(bogus = 1)] a: i32 }");
    assert!(e.contains("unknown key in `#[opt"), "{e}");
}

#[test]
fn unknown_flag_name() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { #[opt(help = \"h\", flags(bogus))] a: i32 }",
    );
    assert!(e.contains("unknown flag `bogus`"), "{e}");
    assert!(
        e.contains("encoding"),
        "the message must list the valid flags: {e}"
    );
}

#[test]
fn skip_with_other_keys() {
    let e = options_err("#[options(name = \"s\")] struct S { #[opt(skip, help = \"h\")] a: i32 }");
    assert!(e.contains("cannot be combined"), "{e}");
}

#[test]
fn child_with_other_keys() {
    let e = options_err("#[options(name = \"s\")] struct S { #[opt(child, name = \"x\")] a: Sub }");
    assert!(e.contains("cannot be combined"), "{e}");
}

#[test]
fn consts_without_unit() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { #[opt(help = \"h\", consts = &X)] a: i32 }",
    );
    assert!(e.contains("needs a `unit"), "{e}");
}

#[test]
fn unknown_array_key() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { #[opt(help = \"h\", array(bogus = 1))] a: Vec<i32> }",
    );
    assert!(e.contains("unknown key in `array"), "{e}");
}

#[test]
fn multiple_field_errors_are_all_reported() {
    let e = options_err(
        "#[options(name = \"s\")] struct S { \
         #[opt(name = \"a\")] x: i32, \
         #[opt(name = \"b\")] y: i32 }",
    );
    // `syn::Error::combine` keeps both; `to_string` shows the first, but the
    // iterator sees both.
    let input: DeriveInput = syn::parse_str(
        "#[options(name = \"s\")] struct S { \
         #[opt(name = \"a\")] x: i32, \
         #[opt(name = \"b\")] y: i32 }",
    )
    .unwrap();
    let err = crate::gen_options::expand(&input).unwrap_err();
    assert_eq!(err.into_iter().count(), 2, "{e}");
}

// ---------------------------------------------------------------- accepted

#[test]
fn the_worked_example_shape_expands() {
    options_ok(
        "#[options(name = \"SwrContext\", help = \"resampling\")]
         struct R {
             #[opt(name = \"isr\", alias = \"in_sample_rate\", help = \"input rate\",
                   default = 0, range = 0..=2147483647, flags(audio, param))]
             in_sample_rate: i32,
             #[opt(name = \"flags\", help = \"engine flags\", unit = \"swr_flags\",
                   default = SwrFlags::empty(), flags(audio, param))]
             flags: SwrFlags,
             #[opt(name = \"channel_map\", help = \"map\", array(sep = '|', max_len = 64),
                   flags(audio, param))]
             channel_map: Vec<i32>,
             #[opt(child)]
             dither: DitherOptions,
             #[opt(skip)]
             cached: Option<Vec<f32>>,
         }",
    );
}

// ------------------------------------------------------------------ OptEnum

#[test]
fn opt_enum_needs_unit() {
    let e = enum_err("#[opt_enum(base = \"int\")] enum E { A }");
    assert!(e.contains("requires `unit"), "{e}");
}

#[test]
fn opt_enum_missing_attribute() {
    let e = enum_err("enum E { A }");
    assert!(e.contains("missing `#[opt_enum("), "{e}");
}

#[test]
fn opt_enum_rejects_data_variants() {
    let e = enum_err("#[opt_enum(unit = \"u\")] enum E { A(i32) }");
    assert!(e.contains("fieldless enum"), "{e}");
}

#[test]
fn opt_enum_rejects_structs() {
    let e = enum_err("#[opt_enum(unit = \"u\")] struct E { a: i32 }");
    assert!(e.contains("only applies to enums"), "{e}");
}

#[test]
fn opt_enum_unknown_base() {
    let e = enum_err("#[opt_enum(unit = \"u\", base = \"bogus\")] enum E { A }");
    assert!(e.contains("unknown `base`"), "{e}");
}

#[test]
fn opt_enum_unknown_const_key() {
    let e = enum_err("#[opt_enum(unit = \"u\")] enum E { #[opt_const(bogus = 1)] A }");
    assert!(e.contains("unknown key in `#[opt_const"), "{e}");
}

#[test]
fn opt_enum_empty() {
    let e = enum_err("#[opt_enum(unit = \"u\")] enum E { }");
    assert!(e.contains("at least one variant"), "{e}");
}
