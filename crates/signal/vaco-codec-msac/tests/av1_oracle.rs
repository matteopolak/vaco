//! Fixed-CDF traces from the installed libaom 3.14.1 static library.
//! Regenerate with `libaom_trace.c`; the reference uses inverse CDFs while
//! this API accepts the ascending CDFs defined in AV1 §8.2.6.

#![allow(
    clippy::integer_division,
    reason = "CDF probability boundaries use exact integer quantization"
)]

use vaco_codec_msac::Av1SymbolDecoder;

#[test]
fn multi_symbol_traces_match_libaom() {
    let data: Vec<u8> = (0u32..512)
        .map(|i| u8::try_from((i * 73 + (i >> 2) * 19 + 0xb4) & 255).unwrap_or(0))
        .collect();
    let expected = include_bytes!("fixtures/av1_libaom_symbols.bin");
    let mut actual = Vec::new();
    for n in [2u32, 3, 4, 7, 16] {
        let mut cdf: Vec<u16> = (1..=n)
            .map(|i| u16::try_from(i * i * 32768 / (n * n)).unwrap_or(0))
            .chain([0])
            .collect();
        let mut decoder = Av1SymbolDecoder::new(&data, true);
        for _ in 0..128 {
            actual.push(u8::try_from(decoder.read_symbol(&mut cdf)).unwrap_or(u8::MAX));
        }
        assert!(!decoder.overrun());
    }
    assert_eq!(actual.len(), 640);
    assert_eq!(actual.as_slice(), expected);
}
