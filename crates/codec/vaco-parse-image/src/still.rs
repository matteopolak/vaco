//! Header-only stream description for the still-image formats whose header
//! reader lives in their decoder crate: PCX, TGA, SGI, XWD, XBM, QOI,
//! PBM/PGM/PPM/PAM/PFM/PHM and JPEG-LS.
//!
//! # Why these delegate instead of parsing the header again
//!
//! The six formats in this crate's other modules (PNG, JPEG, GIF, BMP, TIFF,
//! WebP) read their own headers, because their decoders live behind a
//! `vaco-codec-*` crate this one deliberately does not name. These thirteen
//! do not have that problem: each decoder already reads exactly the fields
//! `CodecParameters` wants — dimensions and the pixel format it is about to
//! allocate — from a header reader that is one function call away, at the
//! same layer (`layers.toml` permits same-layer edges).
//!
//! Re-deriving them here would put the format's pixel-format table in two
//! places, and the two would then have to agree for the probe output and the
//! decoded frame to match. That is the failure `CLAUDE.md` calls "one source
//! of truth", and the reason a probe can report `rgb24` while the pipeline
//! converts to grey: nine of these formats reported no parameters at all,
//! which the transcode path read as "no opinion" and answered with the
//! encoder's first accepted format.
//!
//! Each `parameters` function reads the same header its own `decode` does, so
//! what is reported here is what the frame will carry, by construction.

use vaco_codec_core::CodecParameters;

use crate::parser::ImageHeader;

/// Declare one [`ImageHeader`] that forwards to a decoder crate's own
/// header reader.
macro_rules! delegate {
    ($(#[$meta:meta])* $ty:ident => $read:path) => {
        $(#[$meta])*
        #[derive(Debug)]
        pub struct $ty;

        impl ImageHeader for $ty {
            fn parse(data: &[u8]) -> Option<CodecParameters> {
                $read(data)
            }
        }
    };
}

delegate!(
    /// PCX (`ZSoft` Paintbrush), via `vaco-codec-image-simple`.
    Pcx => vaco_codec_image_simple::parameters_pcx
);
delegate!(
    /// TGA (Truevision), via `vaco-codec-image-simple`.
    Targa => vaco_codec_image_simple::parameters_tga
);
delegate!(
    /// SGI (Silicon Graphics image), via `vaco-codec-image-simple`.
    Sgi => vaco_codec_image_simple::parameters_sgi
);
delegate!(
    /// XWD (X Window Dump), via `vaco-codec-image-simple`.
    Xwd => vaco_codec_image_simple::parameters_xwd
);
delegate!(
    /// XBM (X BitMap), via `vaco-codec-image-simple`.
    Xbm => vaco_codec_image_simple::parameters_xbm
);
delegate!(
    /// QOI (Quite OK Image), via `vaco-codec-qoi`.
    Qoi => vaco_codec_qoi::parameters
);
delegate!(
    /// PBM (`P1`/`P4`), via `vaco-codec-pnm`.
    Pbm => vaco_codec_pnm::parameters_pbm
);
delegate!(
    /// PGM (`P2`/`P5`), via `vaco-codec-pnm`.
    Pgm => vaco_codec_pnm::parameters_pgm
);
delegate!(
    /// PPM (`P3`/`P6`), via `vaco-codec-pnm`.
    Ppm => vaco_codec_pnm::parameters_ppm
);
delegate!(
    /// PAM (`P7`), via `vaco-codec-pnm`.
    Pam => vaco_codec_pnm::parameters_pam
);
delegate!(
    /// PFM (`Pf`/`PF`), via `vaco-codec-pnm`.
    Pfm => vaco_codec_pnm::parameters_pfm
);
delegate!(
    /// PHM (`Ph`/`PH`), via `vaco-codec-pnm`.
    Phm => vaco_codec_pnm::parameters_phm
);
delegate!(
    /// JPEG-LS (ITU-T T.87 `SOF55`), via `vaco-codec-jpegls`.
    JpegLs => vaco_codec_jpegls::parameters
);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_codec_core::CodecId;
    use vaco_pixfmt::PixFmt;

    /// A 3x2 P6 (binary RGB) PPM: colour, and the case a missing pixel
    /// format used to turn grey.
    const PPM_RGB: &[u8] = b"P6\n3 2\n255\n\x80\x00\xff\x00\x00\x00\x00\x00\x00\
\x00\x00\x00\x00\x00\x00\x00\x00\x00";

    #[test]
    fn a_colour_ppm_reports_rgb24_not_grey() {
        let p = Ppm::parse(PPM_RGB).unwrap();
        assert_eq!(p.codec_id, Some(CodecId::Ppm));
        let v = p.video.unwrap();
        assert_eq!((v.width, v.height), (3, 2));
        assert_eq!(v.format, Some(PixFmt::Rgb24));
    }

    #[test]
    fn a_pgm_reports_gray8() {
        let p = Pgm::parse(b"P5\n4 2\n255\n01234567").unwrap();
        let v = p.video.unwrap();
        assert_eq!((v.width, v.height), (4, 2));
        assert_eq!(v.format, Some(PixFmt::Gray8));
    }

    /// `maxval > 255` is the 16-bit branch, a different pixel format from the
    /// same magic — the asymmetry a single-fixture test would miss.
    #[test]
    fn a_sixteen_bit_pgm_reports_gray16be() {
        let p = Pgm::parse(b"P5\n2 1\n65535\n\x00\x01\x00\x02").unwrap();
        assert_eq!(p.video.unwrap().format, Some(PixFmt::Gray16be));
    }

    #[test]
    fn an_xbm_reports_its_defines() {
        let src = b"#define image_width 13\n#define image_height 3\n\
static unsigned char image_bits[] = { 0x00, 0x00 };\n";
        let p = Xbm::parse(src).unwrap();
        assert_eq!(p.codec_id, Some(CodecId::Xbm));
        let v = p.video.unwrap();
        assert_eq!((v.width, v.height), (13, 3));
        assert_eq!(v.format, Some(PixFmt::MonoWhite));
    }

    #[test]
    fn a_qoi_header_reports_its_channel_count() {
        let mut rgb = b"qoif".to_vec();
        rgb.extend_from_slice(&7u32.to_be_bytes());
        rgb.extend_from_slice(&5u32.to_be_bytes());
        rgb.push(3);
        rgb.push(0);
        let v = Qoi::parse(&rgb).unwrap().video.unwrap();
        assert_eq!((v.width, v.height), (7, 5));
        assert_eq!(v.format, Some(PixFmt::Rgb24));

        let mut rgba = rgb.clone();
        rgba[12] = 4;
        assert_eq!(
            Qoi::parse(&rgba).unwrap().video.unwrap().format,
            Some(PixFmt::Rgba)
        );
    }

    #[test]
    fn every_delegate_refuses_a_truncated_header_without_panicking() {
        let inputs: [&[u8]; 4] = [
            PPM_RGB,
            b"qoif\x00\x00\x00\x07\x00\x00\x00\x05\x03\x00",
            b"#define image_width 4\n#define image_height 2\n",
            &[0x0A, 0x05, 0x01, 0x08],
        ];
        for input in inputs {
            for n in 0..input.len() {
                let head = input.get(..n).unwrap();
                let _ = Ppm::parse(head);
                let _ = Qoi::parse(head);
                let _ = Xbm::parse(head);
                let _ = Pcx::parse(head);
                let _ = Sgi::parse(head);
                let _ = Xwd::parse(head);
                let _ = Targa::parse(head);
                let _ = JpegLs::parse(head);
                let _ = Pam::parse(head);
                let _ = Pfm::parse(head);
                let _ = Phm::parse(head);
                let _ = Pbm::parse(head);
                let _ = Pgm::parse(head);
            }
        }
    }
}
