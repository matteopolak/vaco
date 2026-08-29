//! `VlcTable::decode`'s linear scan against `VlcTable::decode_with_lut`'s
//! direct lookup, on a table shaped like a real one this crate's users have
//! (H.264's `coeff_token`/`total_zeros` tables run up to 16-bit codewords
//! with dozens of entries; `SYNTHETIC` below is not transcribed from any
//! specification — it is a synthetic table built only to have that same
//! rough shape for benchmarking, so this measurement carries no clean-room
//! or provenance claim about a real codec's table).
//!
//! ```text
//! cargo bench -p vaco-codec-vlc
//! ```

use divan::{Bencher, black_box};
use vaco_bitstream::{BitReader, BitWriter};
use vaco_codec_vlc::{VlcEntry, VlcTable};

fn main() {
    divan::main();
}

/// A synthetic prefix-free code with H.264-`coeff_token`-scale shape: 30
/// entries, lengths from 2 to 16 bits, built as a canonical Huffman code
/// (shorter codes first, each subtree exhausted before the next length) so
/// it is genuinely prefix-free and complete-ish without hand-picking bits.
fn synthetic_table() -> Vec<VlcEntry> {
    let lengths: [u8; 30] = [
        2, 3, 3, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 8, 8, 9, 10, 11, 12, 12, 13, 13, 14, 14,
        15, 16, 16,
    ];
    let mut entries = Vec::new();
    let mut code: u32 = 0;
    let mut prev_len = 0u8;
    for (symbol, &len) in lengths.iter().enumerate() {
        code <<= len - prev_len;
        entries.push(VlcEntry::new(code, len, symbol as u32));
        code += 1;
        prev_len = len;
    }
    entries
}

/// A bitstream built by writing every entry's own codeword back to back
/// `reps` times, so decoding it end to end exercises every codeword length
/// in the table repeatedly rather than always hitting the same one.
fn stream_of(entries: &[VlcEntry], reps: usize) -> Vec<u8> {
    let mut w = BitWriter::new();
    for _ in 0..reps {
        for entry in entries {
            w.put(u32::from(entry.len), entry.code);
        }
    }
    w.align_zero();
    w.finish()
}

const REPS: usize = 2000;

// Both loops below run for a fixed, known-in-advance count
// (`REPS * entries.len()`, exactly how many codewords `stream_of` wrote)
// rather than looping on `decode`'s own `Option` result: the stream's
// trailing zero padding (`align_zero`) is itself a valid codeword for this
// table's shortest entry, so a `while let Some(..)` loop would run forever
// on padding instead of stopping — the same "a reader that pads with zeros
// makes termination unsafe" hazard `BitReader::get` warns about, here on
// `decode`'s own bounded `peek` rather than `get`.

#[divan::bench]
fn scan(bencher: Bencher<'_, '_>) {
    let entries = synthetic_table();
    let table = VlcTable::new(&entries);
    let bytes = stream_of(&entries, REPS);
    let n = REPS * entries.len();
    bencher.counter(divan::counter::ItemsCount::new(n)).bench_local(|| {
        let mut r = BitReader::new(black_box(&bytes));
        let mut count = 0u32;
        for _ in 0..n {
            if let Some(sym) = table.decode(&mut r) {
                count = count.wrapping_add(black_box(sym));
            }
        }
        black_box(count)
    });
}

#[divan::bench]
fn lut(bencher: Bencher<'_, '_>) {
    let entries = synthetic_table();
    let table = VlcTable::new(&entries);
    let lut = table.build_lut();
    let bytes = stream_of(&entries, REPS);
    let n = REPS * entries.len();
    bencher.counter(divan::counter::ItemsCount::new(n)).bench_local(|| {
        let mut r = BitReader::new(black_box(&bytes));
        let mut count = 0u32;
        for _ in 0..n {
            if let Some(sym) = table.decode_with_lut(&mut r, black_box(&lut)) {
                count = count.wrapping_add(black_box(sym));
            }
        }
        black_box(count)
    });
}
