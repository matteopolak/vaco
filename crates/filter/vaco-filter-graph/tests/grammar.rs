//! The grammar, pinned against ffmpeg 8.1.
//!
//! Every case here was run through the reference first; the commands are in
//! `docs/filter/vaco-filter-graph.md`. Four of them contradict plan 16 §2.1,
//! and each of those carries the observed output in a comment so the claim can
//! be re-checked rather than believed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    reason = "test code"
)]

use vaco_filter_graph::ast::{Ast, parse};
use vaco_filter_graph::error::ErrorKind;

fn ok(src: &str) -> Ast {
    parse(src).unwrap_or_else(|e| panic!("{src:?} should parse:\n{}", e.render(src)))
}

fn err(src: &str) -> ErrorKind {
    parse(src)
        .err()
        .unwrap_or_else(|| panic!("{src:?} should not parse"))
        .kind
}

fn names(ast: &Ast) -> Vec<String> {
    ast.chains
        .iter()
        .flat_map(|c| c.filters.iter())
        .map(|f| f.name.clone())
        .collect()
}

#[test]
fn a_single_chain_of_filters() {
    let ast = ok("scale=640:480,format=yuv420p");
    assert_eq!(ast.chains.len(), 1);
    assert_eq!(names(&ast), ["scale", "format"]);
    assert_eq!(ast.chains[0].filters[0].args.as_deref(), Some("640:480"));
}

#[test]
fn whitespace_is_skipped_around_the_structure() {
    // ffmpeg -f lavfi -i "color=s=32x32:d=0.04 , hflip"        -> accepted
    // ffmpeg -f lavfi -i "  color=s=32x32:d=0.04,hflip  "      -> accepted
    // ffmpeg -f lavfi -i "color=s=32x32:d=0.04[a] ; [a] hflip" -> accepted
    for src in [
        " scale , null ",
        "scale=1:1[a] ; [ a ] null",
        "\n scale \n , \n null \n",
    ] {
        assert!(parse(src).is_ok(), "{src:?}");
    }
}

#[test]
fn whitespace_before_the_equals_is_fine_but_inside_a_name_is_not() {
    // ffmpeg -f lavfi -i "movie =ab"     -> opens 'ab', so `movie ` was trimmed
    // ffmpeg -f lavfi -i "hflip @x"      -> No such filter: 'hflip '
    assert_eq!(ok("null  =  1").chains[0].filters[0].name, "null");
    assert_eq!(ok("null @x").chains[0].filters[0].name, "null ");
}

#[test]
fn a_trailing_separator_is_accepted_but_a_leading_or_doubled_one_is_not() {
    // This contradicts plan 16 §2.1 rule 4, which says only that an empty
    // filterchain is an error. Measured:
    //   "color=…,hflip;"  -> accepted        "color=…,hflip,"  -> accepted
    //   "color=…,hflip;;" -> No such filter: ''
    //   ";color=…"        -> No such filter: ''
    assert_eq!(names(&ok("null;")), ["null"]);
    assert_eq!(names(&ok("null,")), ["null"]);
    assert_eq!(names(&ok("null , ")), ["null"]);
    assert_eq!(err("null;;"), ErrorKind::EmptyFilterName);
    assert_eq!(err(";null"), ErrorKind::EmptyFilterName);
    assert_eq!(err("null,,null"), ErrorKind::EmptyFilterName);
}

#[test]
fn an_empty_description_is_an_error() {
    // ffmpeg -f lavfi -i "" -> No filters specified in the graph description
    assert_eq!(err(""), ErrorKind::EmptyGraph);
    assert_eq!(err("   "), ErrorKind::EmptyGraph);
}

#[test]
fn labels_are_trimmed_and_may_contain_almost_anything() {
    // ffmpeg -f lavfi -i "color=…[ a ];[ a ]hflip"    -> accepted
    // ffmpeg -f lavfi -i "color=…[a[b];[a[b]hflip"    -> accepted
    let ast = ok("null[ a ];[a]null");
    assert_eq!(ast.chains[0].filters[0].outputs[0].name, "a");
    assert_eq!(
        ok("null[a[b];[a[b]null").chains[0].filters[0].outputs[0].name,
        "a[b"
    );
    assert_eq!(
        ok(r"null[a\]b];[a\]b]null").chains[0].filters[0].outputs[0].name,
        "a]b"
    );
}

#[test]
fn an_empty_or_unterminated_label_is_an_error() {
    // ffmpeg -f lavfi -i "color=…[]"  -> Bad (empty?) label found in the following: "[]".
    // ffmpeg -f lavfi -i "…null[b"    -> Mismatched '[' found in the following: "[b".
    assert_eq!(err("null[]"), ErrorKind::EmptyLabel);
    assert_eq!(err("null[ ]"), ErrorKind::EmptyLabel);
    assert_eq!(err("null[b"), ErrorKind::UnterminatedLabel);
    assert_eq!(err("[b null"), ErrorKind::UnterminatedLabel);
}

#[test]
fn a_close_bracket_does_not_end_a_filter_name() {
    // ffmpeg -f lavfi -i "hflip]x" -> No such filter: 'hflip]x'
    assert_eq!(ok("hflip]x").chains[0].filters[0].name, "hflip]x");
}

#[test]
fn anything_but_a_separator_after_a_filter_is_trailing_garbage() {
    // ffmpeg -f lavfi -i "color=s=2x2:d=0.04]"          -> Trailing garbage after a filter: ]
    // ffmpeg -f lavfi -i "color=…[a]  [a]null"          -> Trailing garbage after a filter: null
    assert!(matches!(err("null=1]"), ErrorKind::TrailingGarbage(_)));
    assert!(matches!(
        err("null[a]  [a]null"),
        ErrorKind::TrailingGarbage(_)
    ));
}

#[test]
fn an_instance_tag_with_no_filter_name_is_the_same_as_no_name() {
    // Found by `graph_parse` at exec 667 on the input "\t\t\t@", which parsed
    // to a filter whose name was the empty string.
    //   ffmpeg -f lavfi -i "@"   -> No such filter: ''
    //   ffmpeg -f lavfi -i "@x"  -> No such filter: ''
    //   ffmpeg -f lavfi -i "x@"  -> No such filter: 'x'   (an empty *tag* is fine)
    assert_eq!(err("@"), ErrorKind::EmptyFilterName);
    assert_eq!(err("@x"), ErrorKind::EmptyFilterName);
    assert_eq!(err("\t\t\t@"), ErrorKind::EmptyFilterName);
    let f = &ok("x@").chains[0].filters[0];
    assert_eq!((f.name.as_str(), f.instance.as_deref()), ("x", Some("")));
}

#[test]
fn the_instance_tag_splits_at_the_first_at_sign() {
    let f = &ok("scale@big=2:2").chains[0].filters[0];
    assert_eq!(f.name, "scale");
    assert_eq!(f.instance.as_deref(), Some("big"));
    assert_eq!(f.args.as_deref(), Some("2:2"));
    let f = &ok("scale@a@b").chains[0].filters[0];
    assert_eq!(
        (f.name.as_str(), f.instance.as_deref()),
        ("scale", Some("a@b"))
    );
}

#[test]
fn the_auto_instance_name_counts_every_filter_in_the_graph() {
    // This contradicts plan 16 §2.1 rule 5, which says the counter is per
    // filter *name* and the form is `name@N`. Measured:
    //   "color=…,scale=16:16,hflip,scale=8:8"
    //     -> Parsed_color_0 Parsed_scale_1 Parsed_hflip_2 Parsed_scale_3
    //   "color=…,scale@a=16:16,hflip"
    //     -> Parsed_color_0  scale@a  Parsed_hflip_2   (the tag still takes slot 1)
    let ast = ok("null,scale=16:16,hflip,scale=8:8");
    let got: Vec<String> = ast.chains[0]
        .filters
        .iter()
        .enumerate()
        .map(|(i, f)| f.instance_name(i))
        .collect();
    assert_eq!(
        got,
        [
            "Parsed_null_0",
            "Parsed_scale_1",
            "Parsed_hflip_2",
            "Parsed_scale_3"
        ]
    );
    let ast = ok("null,scale@a=16:16,hflip");
    assert_eq!(ast.chains[0].filters[1].instance_name(1), "scale@a");
    assert_eq!(ast.chains[0].filters[2].instance_name(2), "Parsed_hflip_2");
}

#[test]
fn the_sws_prefix_is_recognised_only_at_the_very_start() {
    // ffmpeg -v verbose -f lavfi -i "sws_flags=bicubic;…"   -> auto_scale_0 flags:'bicubic'
    // ffmpeg -f lavfi -i " sws_flags=bicubic;…"             -> also accepted
    // ffmpeg -f lavfi -i "sws_flags =bicubic;…"             -> parsed as a filter
    // ffmpeg -f lavfi -i "…;sws_flags=bicubic"              -> parsed as a filter
    assert_eq!(
        ok("sws_flags=bicubic;null").sws_flags.as_deref(),
        Some("bicubic")
    );
    assert_eq!(
        ok("  sws_flags=bicubic;null").sws_flags.as_deref(),
        Some("bicubic")
    );
    assert_eq!(ok("sws_flags=;null").sws_flags.as_deref(), Some(""));
    let ast = ok("sws_flags =bicubic;null");
    assert!(ast.sws_flags.is_none());
    assert_eq!(names(&ast), ["sws_flags", "null"]);
    let ast = ok("null;sws_flags=bicubic");
    assert!(ast.sws_flags.is_none());
    assert_eq!(names(&ast), ["null", "sws_flags"]);
}

#[test]
fn arguments_split_on_unescaped_colons_and_positionals_must_come_first() {
    // ffmpeg -f lavfi -i "color=…,scale=w=640:480" -> No option name near '480'
    let f = &ok("overlay=10:10:eof_action=pass").chains[0].filters[0];
    let args = f.arguments().unwrap();
    assert_eq!(args.len(), 3);
    assert_eq!(args[0].key, None);
    assert_eq!(args[0].value(), "10");
    assert_eq!(args[2].key.as_deref(), Some("eof_action"));
    assert_eq!(args[2].value(), "pass");

    let f = &ok("scale=w=640:480").chains[0].filters[0];
    assert!(matches!(
        f.arguments().unwrap_err().kind,
        ErrorKind::PositionalAfterNamed(_)
    ));
}

#[test]
fn a_bare_name_has_no_arguments_and_name_equals_has_empty_ones() {
    assert_eq!(ok("null").chains[0].filters[0].args, None);
    assert_eq!(ok("null=").chains[0].filters[0].args.as_deref(), Some(""));
    assert!(
        ok("null=").chains[0].filters[0]
            .arguments()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_list_valued_option_splits_on_the_bar() {
    let f = &ok("format=pix_fmts=yuv420p|yuv422p|nv12").chains[0].filters[0];
    let args = f.arguments().unwrap();
    assert_eq!(args[0].list_values(), ["yuv420p", "yuv422p", "nv12"]);
    // ... and an escaped bar is data, not a separator.
    let f = &ok(r"format=pix_fmts=a\\|b").chains[0].filters[0];
    assert_eq!(f.arguments().unwrap()[0].list_values(), ["a|b"]);
}

#[test]
fn several_chains_joined_by_labels() {
    let ast = ok("[0:v]scale=640:360[small];[small][1:v]overlay=10:10[out]");
    assert_eq!(ast.chains.len(), 2);
    assert_eq!(ast.chains[0].filters[0].inputs[0].name, "0:v");
    assert_eq!(ast.chains[1].filters[0].inputs[1].name, "1:v");
    assert_eq!(ast.chains[1].filters[0].outputs[0].name, "out");
}

#[test]
fn parsing_is_iterative_so_depth_costs_bytes_not_stack() {
    // The obvious hazard in a hand-written parser. There is no recursion here
    // at all, so this is a regression guard rather than a hope.
    let deep = "[".repeat(200_000);
    assert!(parse(&deep).is_err());
    let chain = core::iter::repeat_n("null", 50_000)
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(parse(&chain).map(|a| a.chains[0].filters.len()), Ok(50_000));
    let chains = core::iter::repeat_n("null", 50_000)
        .collect::<Vec<_>>()
        .join(";");
    assert_eq!(parse(&chains).map(|a| a.chains.len()), Ok(50_000));
}

#[test]
fn a_filter_literally_named_sws_flags_survives_printing() {
    // Found by `graph_hostile`: the leading backslash keeps this from being the
    // prefix, so it parses as one filter named `sws_flags`. Printing it without
    // the backslash turned it back into the prefix and an empty graph.
    let src = r"\sws_flags=x|y;";
    let ast = ok(src);
    assert_eq!(names(&ast), ["sws_flags"]);
    assert!(ast.sws_flags.is_none());
    let printed = ast.to_string();
    let again = ok(&printed);
    assert_eq!(again.without_spans(), ast.without_spans(), "{printed:?}");
    assert_eq!(again.to_string(), printed);
}
