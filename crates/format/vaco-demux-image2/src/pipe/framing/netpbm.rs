//! Netpbm binary family: `P4`/`P5`/`P6`/`P7`/`Pf`/`PF`/`PH`/`Ph`.
//!
//! Shared by `pbm_pipe`, `pgm_pipe`, `pgmyuv_pipe`, `ppm_pipe`, `pam_pipe`,
//! `pfm_pipe` and `phm_pipe`. `pgmyuv` is not a distinct on-disk shape: probed
//! directly (`ffmpeg -c:v pgmyuv`), it writes a plain `P5` header whose height
//! already includes the appended chroma rows (4:2:0 chroma makes an 8-row
//! frame a 12-row `P5`), so it needs no separate parsing path here — only its
//! own registration, extension and probe.
//!
//! Every text header in this family is whitespace- and `#`-comment-separated
//! ASCII tokens (PAM's is `KEY value` lines instead), always followed by
//! exactly one whitespace byte and then a binary raster whose length the
//! header fully determines. That is what makes this framing rather than
//! decoding: nothing here reads a sample value, only the three or four
//! integers that say how many bytes follow.

/// The exact byte length of one netpbm image at the start of `rest`, or
/// `None` if `rest` does not start with a magic this module recognises.
///
/// The ASCII variants (`P1`/`P2`/`P3`) have no binary raster to size — pixel
/// values are decimal text, whitespace-separated, with no length field — so
/// they report "the rest of the input", the same honest fallback
/// [`crate::pipe::framing::ImageFraming::WholeRemaining`] uses elsewhere. That is
/// conservative (never chops a later image out of a later text image), and
/// this project has not measured whether the reference concatenates ASCII
/// netpbm files at all: its own encoders write the binary variants.
#[must_use]
pub fn size(rest: &[u8]) -> Option<usize> {
    let magic = rest.get(0..2)?;
    match magic {
        b"P1" | b"P2" | b"P3" => Some(rest.len()),
        b"P4" => sized(rest, 2, Kind::Bitmap),
        b"P5" => sized(rest, 2, Kind::Gray),
        b"P6" => sized(rest, 2, Kind::Rgb),
        b"P7" => pam_sized(rest),
        b"Pf" => float_sized(rest, 1),
        b"PF" => float_sized(rest, 3),
        b"PH" | b"Ph" => half_sized(rest, magic == b"PH", 3),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Bitmap,
    Gray,
    Rgb,
}

/// Read whitespace/`#`-comment-separated ASCII tokens starting at `pos`.
/// Returns `(token, position right after it)`.
struct Tokenizer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    fn next_token(&mut self) -> Option<&'a [u8]> {
        loop {
            let &b = self.data.get(self.pos)?;
            if b == b'#' {
                while self.data.get(self.pos).is_some_and(|&c| c != b'\n') {
                    self.pos += 1;
                }
                continue;
            }
            if b.is_ascii_whitespace() {
                self.pos += 1;
                continue;
            }
            break;
        }
        let start = self.pos;
        while self
            .data
            .get(self.pos)
            .is_some_and(|&c| !c.is_ascii_whitespace())
        {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        self.data.get(start..self.pos)
    }

    fn next_uint(&mut self) -> Option<usize> {
        let tok = self.next_token()?;
        std::str::from_utf8(tok).ok()?.parse().ok()
    }

    /// Exactly one whitespace byte must separate the header from the raster,
    /// per every netpbm variant's own grammar.
    fn skip_single_separator(&mut self) -> Option<()> {
        let &b = self.data.get(self.pos)?;
        if !b.is_ascii_whitespace() {
            return None;
        }
        self.pos += 1;
        Some(())
    }
}

fn sized(rest: &[u8], magic_len: usize, kind: Kind) -> Option<usize> {
    let mut t = Tokenizer::new(rest, magic_len);
    let width = t.next_uint()?;
    let height = t.next_uint()?;
    let (channels, maxval) = match kind {
        Kind::Bitmap => (1, None),
        Kind::Gray => (1, Some(t.next_uint()?)),
        Kind::Rgb => (3, Some(t.next_uint()?)),
    };
    t.skip_single_separator()?;
    let bytes_per_sample = maxval.map_or(0, |m| if m < 256 { 1 } else { 2 });
    let raster = if maxval.is_none() {
        // PBM: one bit per pixel, rows padded to a whole byte.
        width.div_ceil(8).checked_mul(height)?
    } else {
        width
            .checked_mul(height)?
            .checked_mul(channels)?
            .checked_mul(bytes_per_sample)?
    };
    t.pos.checked_add(raster)
}

fn pam_sized(rest: &[u8]) -> Option<usize> {
    let mut t = Tokenizer::new(rest, 2);
    let mut width = None;
    let mut height = None;
    let mut depth = None;
    let mut maxval = None;
    loop {
        let key = t.next_token()?;
        if key == b"ENDHDR" {
            break;
        }
        match key {
            b"WIDTH" => width = Some(t.next_uint()?),
            b"HEIGHT" => height = Some(t.next_uint()?),
            b"DEPTH" => depth = Some(t.next_uint()?),
            b"MAXVAL" => maxval = Some(t.next_uint()?),
            b"TUPLTYPE" => {
                let _ = t.next_token()?;
            }
            _ => return None, // unrecognised key: give up rather than guess
        }
    }
    // ENDHDR is followed by exactly one newline, then the raster.
    t.skip_single_separator()?;
    let bytes_per_sample = if maxval? < 256 { 1 } else { 2 };
    let raster = width?
        .checked_mul(height?)?
        .checked_mul(depth?)?
        .checked_mul(bytes_per_sample)?;
    t.pos.checked_add(raster)
}

fn float_sized(rest: &[u8], channels: usize) -> Option<usize> {
    let mut t = Tokenizer::new(rest, 2);
    let width = t.next_uint()?;
    let height = t.next_uint()?;
    let _scale = t.next_token()?; // sign gives endianness; not needed for length
    t.skip_single_separator()?;
    let raster = width
        .checked_mul(height)?
        .checked_mul(channels)?
        .checked_mul(4)?;
    t.pos.checked_add(raster)
}

/// `phm`'s half-precision-float variant: same grammar as PFM, 2 bytes/sample.
/// Not observed against a real encoder (this ffmpeg build has none); modelled
/// directly on PFM, which *is* measured, plus the format's own name.
fn half_sized(rest: &[u8], is_color: bool, color_channels: usize) -> Option<usize> {
    let mut t = Tokenizer::new(rest, 2);
    let width = t.next_uint()?;
    let height = t.next_uint()?;
    let _scale = t.next_token()?;
    t.skip_single_separator()?;
    // `PH` mirrors `PF` (colour); `Ph` mirrors `Pf` (mono) by ffmpeg's own
    // pfm/phm naming convention (lowercase second letter = single-channel).
    let channels = if is_color { color_channels } else { 1 };
    let raster = width
        .checked_mul(height)?
        .checked_mul(channels)?
        .checked_mul(2)?;
    t.pos.checked_add(raster)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn pgm_binary_size() {
        let header = b"P5\n8 8\n255\n";
        let mut data = header.to_vec();
        data.extend(vec![0u8; 64]); // 8x8, one byte per sample
        assert_eq!(size(&data), Some(header.len() + 64));
    }

    #[test]
    fn ppm_binary_size_three_channels() {
        let header = b"P6\n2 2\n255\n";
        let mut data = header.to_vec();
        data.extend(vec![0u8; 2 * 2 * 3]);
        assert_eq!(size(&data), Some(header.len() + 12));
    }

    #[test]
    fn pbm_binary_size_packs_bits() {
        let header = b"P4\n8 2\n"; // 1 byte/row * 2 rows
        let mut data = header.to_vec();
        data.extend(vec![0u8; 2]);
        assert_eq!(size(&data), Some(header.len() + 2));
    }

    #[test]
    fn pam_size_from_key_value_header() {
        let header = b"P7\nWIDTH 2\nHEIGHT 2\nDEPTH 3\nMAXVAL 255\nTUPLTYPE RGB\nENDHDR\n";
        let mut data = header.to_vec();
        data.extend(vec![0u8; 2 * 2 * 3]);
        assert_eq!(size(&data), Some(header.len() + 12));
    }

    #[test]
    fn pfm_color_size_is_four_bytes_per_channel() {
        let header = b"PF\n2 2\n1.000000\n";
        let mut data = header.to_vec();
        data.extend(vec![0u8; 2 * 2 * 3 * 4]);
        assert_eq!(size(&data), Some(header.len() + 48));
    }

    #[test]
    fn ascii_variants_fall_back_to_whole_remaining() {
        let data = b"P2\n2 2\n255\n0 1 2 3\n".to_vec();
        assert_eq!(size(&data), Some(data.len()));
    }

    #[test]
    fn unrecognised_magic_returns_none() {
        assert_eq!(size(b"XY\n1 1\n"), None);
    }

    #[test]
    fn truncated_header_returns_none_without_panicking() {
        assert_eq!(size(b"P6\n8"), None);
        assert_eq!(size(b"P7\nWIDTH 8"), None);
        assert_eq!(size(b""), None);
        assert_eq!(size(b"P"), None);
    }
}
