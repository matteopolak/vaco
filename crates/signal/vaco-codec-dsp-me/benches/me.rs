//! Relative cost of the three search patterns at a realistic 8x8
//! inter-prediction block and a ±16 search range.

use vaco_codec_dsp_me::{BlockOrigin, Metric, Displacement, SearchConfig, Searcher};
use vaco_codec_dsp_mecmp::Plane;

const REF_W: usize = 128;
const REF_H: usize = 128;
const BLOCK: BlockOrigin = BlockOrigin {
    x: 40,
    y: 40,
    width: 8,
    height: 8,
};

fn buffers() -> (Vec<u8>, Vec<u8>) {
    let cur: Vec<u8> = (0..REF_W * REF_H).map(|i| (i * 7 % 256) as u8).collect();
    let refb: Vec<u8> = (0..REF_W * REF_H).map(|i| (i * 11 % 256) as u8).collect();
    (cur, refb)
}

fn config() -> SearchConfig {
    SearchConfig {
        metric: Metric::Sad,
        range: 16,
    }
}

#[divan::bench]
fn full_search(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let searcher = Searcher::new();
    let cfg = config();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), REF_W, REF_W, REF_H);
        let r = Plane::new(divan::black_box(&refb), REF_W, REF_W, REF_H);
        searcher.full_search(c, r, BLOCK, &cfg, Displacement::ZERO)
    });
}

#[divan::bench]
fn diamond_search(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let searcher = Searcher::new();
    let cfg = config();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), REF_W, REF_W, REF_H);
        let r = Plane::new(divan::black_box(&refb), REF_W, REF_W, REF_H);
        searcher.diamond_search(c, r, BLOCK, &cfg, Displacement::ZERO)
    });
}

#[divan::bench]
fn three_step_search(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let searcher = Searcher::new();
    let cfg = config();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), REF_W, REF_W, REF_H);
        let r = Plane::new(divan::black_box(&refb), REF_W, REF_W, REF_H);
        searcher.three_step_search(c, r, BLOCK, &cfg, Displacement::ZERO)
    });
}

fn main() {
    divan::main();
}
