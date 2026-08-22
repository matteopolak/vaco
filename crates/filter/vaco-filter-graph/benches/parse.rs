//! Parsing and building cost. A filtergraph is parsed once per run, so this is
//! a guard against pathological input rather than a hot path.

use vaco_filter_graph::{ast, mock};

fn main() {
    divan::main();
}

const SIMPLE: &str = "scale=640:480,format=pix_fmts=yuv420p";
const COMPLEX: &str =
    "[0:v]scale=640:360[small];[small][1:v]overlay=10:10:eof_action=pass[out];[out]null[final]";
const ESCAPED: &str =
    r"drawtext=text=this is a \\\'string\\\'\\: may contain one\, or more\, special characters";

#[divan::bench(args = [SIMPLE, COMPLEX, ESCAPED])]
fn parse(bencher: divan::Bencher<'_, '_>, src: &str) {
    bencher.bench(|| ast::parse(divan::black_box(src)));
}

#[divan::bench]
fn parse_deep_chain(bencher: divan::Bencher<'_, '_>) {
    let src = core::iter::repeat_n("null", 256)
        .collect::<Vec<_>>()
        .join(",");
    bencher.bench(|| ast::parse(divan::black_box(&src)));
}

#[divan::bench]
fn build_chain(bencher: divan::Bencher<'_, '_>) {
    let registry = mock::MockRegistry::new();
    let src = "counter=n=4,invert,dropevery=every=2,null";
    let ast = ast::parse(src).unwrap_or_default();
    bencher.bench(|| vaco_filter_graph::build(divan::black_box(&ast), &registry));
}
