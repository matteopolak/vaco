//! PNG/JPEG/GIF/BMP/TIFF/WebP header parsing against arbitrary bytes.
//!
//! Every format in `vaco-parse-image` reduces to "the whole input is one
//! image" (see the crate doc), so there is no chunking-invariance property
//! to check the way `parse_h264`/`parse_mpegvideo` do — only totality.
//!
//! fuzz-crate: vaco-parse-image
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Parser;
use vaco_limits::Limits;
use vaco_parse_image::ImageParser;
use vaco_parse_image::{bmp, gif, jpeg, png, tiff, webp};

fn drive<H: vaco_parse_image::parser::ImageHeader + 'static>(data: &[u8]) {
    let mut parser = ImageParser::<H>::new(Limits::strict());
    if let Ok((_pkt, used)) = parser.parse(data) {
        assert_eq!(used, data.len(), "the whole input must be consumed");
    }
}

fuzz_target!(|data: &[u8]| {
    drive::<png::Png>(data);
    drive::<jpeg::Jpeg>(data);
    drive::<gif::Gif>(data);
    drive::<bmp::Bmp>(data);
    drive::<tiff::Tiff>(data);
    drive::<webp::Webp>(data);
});
