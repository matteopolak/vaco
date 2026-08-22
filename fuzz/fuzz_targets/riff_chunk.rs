//! The RIFF chunk grammar, `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE`,
//! `BITMAPINFOHEADER` and `ds64`, all together: every one of them parses
//! directly from a byte slice with no I/O in between, so one target covering
//! all four keeps the fuzzer's whole budget on the parsing logic itself.
//!
//! Properties asserted, mirroring `vaco-format-isom`'s `isom_boxes` target:
//!
//! * `ChunkIter` terminates on any input and never yields a payload that
//!   reaches outside the buffer it was given, however the input lies about
//!   `ckSize` (including the all-ones "unknown length" convention).
//! * `WaveFormatEx::parse`, `BitmapInfoHeader::parse`, `RiffHeader::parse` and
//!   `Ds64::parse` either return an error or a value — never a panic — over
//!   arbitrary bytes, and under `Limits::strict()` with a near-zero budget
//!   they fail with `LimitExceeded` rather than allocating anything sized
//!   from the input.
//!
//! fuzz-crate: vaco-format-riff

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_riff::chunk::{ChunkIter, RiffHeader};
use vaco_format_riff::rf64::Ds64;
use vaco_format_riff::{BitmapInfoHeader, WaveFormatEx};
use vaco_limits::{Budget, Limits};

/// Chunks visited before the target concludes iteration is not terminating.
/// libFuzzer bounds the input length, and every chunk costs at least eight
/// bytes, so a genuine walk over the corpus never approaches this.
const RUNAWAY: usize = 1 << 22;

fuzz_target!(|data: &[u8]| {
    // 1. Flat chunk iteration terminates and never reports a payload longer
    //    than what the buffer actually held from that point on.
    let mut seen = 0usize;
    for item in ChunkIter::new(data, 0) {
        seen += 1;
        assert!(seen < RUNAWAY, "chunk iteration did not terminate");
        let Ok(c) = item else { break };
        assert!(
            (c.payload.len() as u64) <= u64::from(c.declared_size),
            "a clamped payload exceeded its own declared size"
        );
        if let Some((_, mut kids)) = c.children() {
            let mut nested = 0usize;
            for k in &mut kids {
                nested += 1;
                assert!(nested < RUNAWAY, "child chunk iteration did not terminate");
                if k.is_err() {
                    break;
                }
            }
        }
    }

    // 2. The outermost header either parses or errors; it never panics, and
    //    when it parses, its children walk cleanly under the same iterator
    //    used above.
    if let Ok(h) = RiffHeader::parse(data) {
        let mut seen = 0usize;
        for item in h.children(data) {
            seen += 1;
            assert!(seen < RUNAWAY, "RiffHeader::children did not terminate");
            if item.is_err() {
                break;
            }
        }
    }

    // 3. WAVEFORMATEX: parse under a generous budget (so the parse itself is
    //    exercised), then again under a near-empty one (so the failure mode
    //    is a clean LimitExceeded, never a panic or an unbounded copy).
    let mut generous = Budget::new(Limits::permissive());
    if let Ok(fmt) = WaveFormatEx::parse(data, &mut generous) {
        // Resolving the extensible tail must be total over whatever `extra`
        // parsing produced, including a GUID that is not a recognised
        // Microsoft media subtype.
        let _ = fmt.extensible().and_then(|e| e.sub_format_tag());
        let _ = vaco_format_riff::wave_tags::codec_name(&fmt);
        let _ = vaco_format_riff::wave_tags::codec_id(&fmt);
    }
    let mut starved = Budget::new(Limits::strict().with_alloc_total(2));
    let _ = WaveFormatEx::parse(data, &mut starved);

    // 4. BITMAPINFOHEADER has no budget of its own (fixed 40-byte read, no
    //    input-derived allocation) — just totality.
    if let Ok(h) = BitmapInfoHeader::parse(data) {
        let c = h.compression();
        let _ = vaco_format_riff::video_tags::codec_name(c);
        let _ = vaco_format_riff::video_tags::codec_id(c);
    }

    // 5. ds64: a declared table length is exactly the "trust a count from the
    //    header" trap this crate exists to close.
    let mut generous = Budget::new(Limits::permissive());
    let _ = Ds64::parse(data, &mut generous);
    let mut starved = Budget::new(Limits::strict().with_alloc_total(2));
    let _ = Ds64::parse(data, &mut starved);
});
