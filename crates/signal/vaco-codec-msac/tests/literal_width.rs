use vaco_codec_msac::{Vp8BoolDecoder, Vp9BoolDecoder};

#[test]
fn boolean_engines_keep_the_low_thirty_two_literal_bits() {
    let data = [0x55; 32];
    for width in [0, 31, 32, 33, 65] {
        let mut literal = Vp8BoolDecoder::new(&data);
        let mut bits = Vp8BoolDecoder::new(&data);
        let expected = (0..width).fold(0u32, |v, _| (v << 1) | u32::from(bits.read_flag()));
        assert_eq!(literal.read_literal(width), expected, "VP8 width {width}");
        let mut literal = Vp9BoolDecoder::new(&data);
        let mut bits = Vp9BoolDecoder::new(&data);
        let expected = (0..width).fold(0u32, |v, _| (v << 1) | u32::from(bits.read_bool(128)));
        assert_eq!(literal.read_literal(width), expected, "VP9 width {width}");
    }
}
