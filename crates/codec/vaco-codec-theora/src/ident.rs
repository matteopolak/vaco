//! Identification header (`Vaco-Spec-Ref: theora-spec-20170603 section 6.2`).

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

/// Pixel format (section 6.2, Table 6.4): which chroma planes are
/// subsampled, and in which directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelFormat {
    /// 4:2:0 — both chroma axes halved.
    Yuv420,
    /// 4:2:2 — chroma halved horizontally only.
    Yuv422,
    /// 4:4:4 — chroma unsubsampled.
    Yuv444,
}

impl PixelFormat {
    /// Chroma plane dimensions in blocks, given the luma frame size in
    /// macro blocks. Derived from the `NSBS` table (section 6.2, Table 6.5):
    /// each pixel format's chroma block grid is exactly what makes that
    /// table's super block counts come out right.
    #[must_use]
    pub(crate) const fn chroma_blocks(self, fmbw: u32, fmbh: u32) -> (u32, u32) {
        match self {
            Self::Yuv420 => (fmbw, fmbh),
            Self::Yuv422 => (fmbw, fmbh.saturating_mul(2)),
            Self::Yuv444 => (fmbw.saturating_mul(2), fmbh.saturating_mul(2)),
        }
    }

    /// Pixel-domain chroma subsampling factors `(horizontal, vertical)`:
    /// how many luma pixels correspond to one chroma pixel on each axis.
    ///
    /// This is **not** derivable by calling [`Self::chroma_blocks`] with a
    /// `1x1` macro block grid — that function's `Yuv420` arm always returns
    /// its input unchanged (`(fmbw, fmbh)`), because the block-domain
    /// chroma:luma ratio is 1:1 in macro block units regardless of pixel
    /// format; the 2x pixel subsampling for 4:2:0/4:2:2 is baked into the
    /// fixed 8-vs-16-pixels-per-macro-block-edge convention applied
    /// elsewhere, not into the block *count*. Calling `chroma_blocks(1, 1)`
    /// to get a subsampling factor was a real bug (caught by decoding a
    /// real file and finding the last few rows of every chroma plane
    /// corrupted, section D6/D17): it silently returned `(1, 1)` for every
    /// pixel format including `Yuv420`, so the picture-region crop used the
    /// coded frame's full chroma height/width instead of the correct
    /// halved one, and every row past the true chroma height read padding.
    #[must_use]
    pub(crate) const fn chroma_subsample(self) -> (u32, u32) {
        match self {
            Self::Yuv420 => (2, 2),
            Self::Yuv422 => (2, 1),
            Self::Yuv444 => (1, 1),
        }
    }
}

/// The identification header's fields that frame decode needs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ident {
    pub fmbw: u32,
    pub fmbh: u32,
    pub picw: u32,
    pub pich: u32,
    pub picx: u32,
    pub picy: u32,
    pub pf: PixelFormat,
}

impl Ident {
    /// Parse the body of an identification header packet, i.e. everything
    /// after the common `\x80theora` prologue (section 6.2, steps 2-23;
    /// step 1's common-header check is the caller's job since it also
    /// distinguishes comment/setup packets by the same prologue).
    pub(crate) fn parse(body: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(body);
        let vmaj = r.get(8);
        let vmin = r.get(8);
        let _vrev = r.get(8);
        if vmaj != 3 || vmin != 2 {
            return Err(Error::Unsupported(
                "theora: only bitstream version 3.2.x is supported",
            ));
        }
        let fmbw = r.get(16);
        let fmbh = r.get(16);
        if fmbw == 0 || fmbh == 0 {
            return Err(Error::InvalidData("theora: zero-sized coded frame"));
        }
        let picw = r.get(24);
        let pich = r.get(24);
        let picx = r.get(8);
        let picy = r.get(8);
        let _frn = r.get(32);
        let _frd = r.get(32);
        let _parn = r.get(24);
        let _pard = r.get(24);
        let _cs = r.get(8);
        let _nombr = r.get(24);
        let _qual = r.get(6);
        let _kfgshift = r.get(5);
        let pf = r.get(2);
        let _reserved = r.get(3);
        r.check()
            .map_err(|_| Error::InvalidData("theora: truncated identification header"))?;

        let pf = match pf {
            0 => PixelFormat::Yuv420,
            2 => PixelFormat::Yuv422,
            3 => PixelFormat::Yuv444,
            _ => {
                return Err(Error::Unsupported(
                    "theora: reserved pixel format value 1",
                ));
            }
        };
        if picw > fmbw.saturating_mul(16) || pich > fmbh.saturating_mul(16) {
            return Err(Error::InvalidData(
                "theora: picture region larger than the coded frame",
            ));
        }
        // A zero picture size is nonsensical to display; treat it as the
        // full frame rather than propagating a zero-area output frame.
        let (picw, pich) = if picw == 0 || pich == 0 {
            (fmbw.saturating_mul(16), fmbh.saturating_mul(16))
        } else {
            (picw, pich)
        };
        Ok(Self {
            fmbw,
            fmbh,
            picw,
            pich,
            picx: u32::from(u8::try_from(picx).unwrap_or(0)),
            picy: u32::from(u8::try_from(picy).unwrap_or(0)),
            pf,
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    fn push_bits(out: &mut Vec<u8>, cur: &mut u8, n: &mut u32, value: u32, width: u32) {
        for i in (0..width).rev() {
            *cur = (*cur << 1) | u8::from((value >> i) & 1 != 0);
            *n += 1;
            if *n == 8 {
                out.push(*cur);
                *cur = 0;
                *n = 0;
            }
        }
    }

    fn sample_ident_body() -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = 0u8;
        let mut n = 0u32;
        push_bits(&mut out, &mut cur, &mut n, 3, 8); // vmaj
        push_bits(&mut out, &mut cur, &mut n, 2, 8); // vmin
        push_bits(&mut out, &mut cur, &mut n, 0, 8); // vrev
        push_bits(&mut out, &mut cur, &mut n, 15, 16); // fmbw
        push_bits(&mut out, &mut cur, &mut n, 3, 16); // fmbh
        push_bits(&mut out, &mut cur, &mut n, 240, 24); // picw
        push_bits(&mut out, &mut cur, &mut n, 48, 24); // pich
        push_bits(&mut out, &mut cur, &mut n, 0, 8); // picx
        push_bits(&mut out, &mut cur, &mut n, 0, 8); // picy
        push_bits(&mut out, &mut cur, &mut n, 30, 32); // frn
        push_bits(&mut out, &mut cur, &mut n, 1, 32); // frd
        push_bits(&mut out, &mut cur, &mut n, 1, 24); // parn
        push_bits(&mut out, &mut cur, &mut n, 1, 24); // pard
        push_bits(&mut out, &mut cur, &mut n, 0, 8); // cs
        push_bits(&mut out, &mut cur, &mut n, 0, 24); // nombr
        push_bits(&mut out, &mut cur, &mut n, 0, 6); // qual
        push_bits(&mut out, &mut cur, &mut n, 0, 5); // kfgshift
        push_bits(&mut out, &mut cur, &mut n, 0, 2); // pf = 4:2:0
        push_bits(&mut out, &mut cur, &mut n, 0, 3); // reserved
        if n > 0 {
            cur <<= 8 - n;
            out.push(cur);
        }
        out
    }

    #[test]
    fn parses_the_worked_spec_example_geometry() {
        let ident = Ident::parse(&sample_ident_body()).unwrap();
        assert_eq!((ident.fmbw, ident.fmbh), (15, 3));
        assert_eq!((ident.picw, ident.pich), (240, 48));
        assert_eq!(ident.pf, PixelFormat::Yuv420);
    }

    #[test]
    fn rejects_zero_frame_size() {
        let mut body = sample_ident_body();
        // Zero out fmbw's 16 bits (bytes 3..5).
        body[3] = 0;
        body[4] = 0;
        assert!(Ident::parse(&body).is_err());
    }
}
