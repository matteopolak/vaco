//! Byte-boundary scanners for the 37 `*_pipe` splitters.
//!
//! This is framing, not decoding (see the crate's D14.1 note): every function
//! here answers "where does this image end", using only signature bytes,
//! chunk-length fields, or header-declared dimensions — never a pixel value.
//! [`compute_spans`] loads the whole remaining input once (mirroring
//! `vaco-demux-raw::bitstream`'s own worked convention for the same trade-off)
//! and returns a list of non-overlapping, in-order byte ranges, each one
//! packet.
//!
//! # What was measured versus assumed
//!
//! Concatenating three `-f lavfi testsrc` frames through
//! `ffmpeg -f image2pipe -c:v <codec> -` and reading them back with
//! `ffprobe -f <name>_pipe -show_packets` answers a factual question no
//! amount of spec-reading does: **does this splitter support concatenation at
//! all**. Measured on ffmpeg 8.1:
//!
//! | Result | Formats |
//! |---|---|
//! | 3 packets (real per-image framing) | `png`, `jpeg`, `jpegls`, `j2k`, `bmp`, `webp`, `ppm`, `pgm`, `pgmyuv`, `pbm`, `pam`, `pfm`, `qoi`, `xwd`, `hdr`, `xbm` |
//! | **1 packet spanning the whole input**, regardless of how many images were concatenated | `gif`, `tiff`, `sgi`, `dpx`, `exr`, `pcx`, `sunrast` |
//!
//! The second row is not a shortcut this crate took — it is what the
//! reference itself does; `png_pipe` and `gif_pipe` are not the same shape of
//! demuxer. [`ImageFraming::WholeRemaining`] reproduces that measured behaviour
//! exactly (not "unsupported": `-loop`-free single-image use, the overwhelming
//! common case, works identically either way).
//!
//! No encoder exists in this ffmpeg build for `cri`, `dds`, `gem`, `jpegxl`,
//! `jpegxs`, `pgx`, `photocd`, `pictor`, `psd`, `qdraw`, `svg`, or `vbn`, so
//! their concatenation behaviour could not be measured at all (`vbn` gave an
//! inconclusive 2-packets-from-3-images result on the one encode that did not
//! error, and is treated as [`ImageFraming::WholeRemaining`] rather than trusted).
//! `svg` and `xbm`/`xpm`'s sibling text scan are implemented anyway because
//! they follow directly from the format's own text grammar rather than from
//! ffmpeg-specific behaviour; the rest default to [`ImageFraming::WholeRemaining`],
//! which is always at least as correct as a guessed scanner and is called out
//! per-format in `docs/format/vaco-demux-image2.md`.

/// How one pipe splitter finds the end of "this image" in a byte stream.
#[derive(Debug, Clone, Copy)]
pub enum ImageFraming {
    /// No per-image boundary is known: one packet is the entire remaining
    /// input. Correct by construction, and what the reference itself does
    /// for several of these formats (see the module docs).
    WholeRemaining,
    /// PNG: the 8-byte signature, then a chunk walk (4-byte length, 4-byte
    /// type, payload, 4-byte CRC) to `IEND`.
    Png,
    /// A start marker, then a scan for the end marker. `skip_stuffing` engages
    /// the JPEG entropy-coded-segment rule: a `0xFF` immediately followed by
    /// `0x00` is a stuffed literal `0xFF` (not a marker), and `0xFFD0`-`0xFFD7`
    /// are restart markers inside the entropy stream, neither of which ends
    /// the scan. J2K's codestream markers have no stuffing rule, so its
    /// registration leaves this false and gets a plain byte-sequence scan.
    Marker {
        start: [u8; 2],
        end: [u8; 2],
        skip_stuffing: bool,
    },
    /// RIFF container: `"RIFF"` + a 4-byte little-endian size covering
    /// everything after the size field, so the whole chunk is
    /// `8 + size` bytes (rounded up to even, per RIFF's word-alignment rule).
    RiffSized,
    /// `"BM"` + a 4-byte little-endian total file size at offset 2.
    BmpSized,
    /// Netpbm binary family: `P4`/`P5`/`P6`/`P7`/`Pf`/`PF`/`PH`/`Ph`. A text
    /// header (whitespace- and `#`-comment-separated ASCII integers, or PAM's
    /// `KEY value` lines) gives the exact byte length of the binary raster
    /// that follows.
    Netpbm,
    /// PGX (JPEG2000 Part 4 test format): one text header line, then a raster
    /// of `ceil(bits_per_sample / 8) * width * height` bytes.
    Pgx,
    /// QOI: a fixed 14-byte header, then a compressed body ending in the
    /// format's fixed 8-byte end marker (seven `0x00` bytes and a `0x01`).
    Qoi,
    /// X Window Dump: a fixed-layout 100-byte header (25 big-endian `u32`
    /// fields) whose first field is the total header size (header + window
    /// name), followed by a colormap and a pixel array both sized by later
    /// header fields.
    Xwd,
    /// XBM/XPM: a C source fragment. There is no length field; the image ends
    /// at the closing `};` of the pixel array.
    CArrayText,
    /// SVG: XML text. The image ends at the closing `</svg>` tag.
    SvgText,
    /// Radiance (`.hdr`/`.pic`): a text header ending in a blank line and a
    /// resolution line, then `height` new-format-RLE scanlines. Only the
    /// new-format (4-byte-marker) RLE is recognised; anything else falls back
    /// to [`ImageFraming::WholeRemaining`] for that image, which is always safe.
    Radiance,
}

/// One packet's byte range within the buffered input. `end` is exclusive.
pub type Span = (usize, usize);

/// Split `data` into spans per [`ImageFraming`]. Never panics, always terminates:
/// every branch either returns a fixed number of spans or advances strictly
/// past the previous span's end before looping again.
#[must_use]
pub fn compute_spans(framing: ImageFraming, data: &[u8]) -> Vec<Span> {
    match framing {
        ImageFraming::WholeRemaining => whole_remaining(data),
        ImageFraming::Png => png_spans(data),
        ImageFraming::Marker {
            start,
            end,
            skip_stuffing,
        } => marker_spans(data, start, end, skip_stuffing),
        ImageFraming::RiffSized => sized_spans(data, riff_size),
        ImageFraming::BmpSized => sized_spans(data, bmp_size),
        ImageFraming::Netpbm => sized_spans(data, netpbm::size),
        ImageFraming::Pgx => sized_spans(data, pgx_size),
        ImageFraming::Qoi => qoi_spans(data),
        ImageFraming::Xwd => sized_spans(data, xwd_size),
        ImageFraming::CArrayText => carray_spans(data),
        ImageFraming::SvgText => svg_spans(data),
        ImageFraming::Radiance => radiance::spans(data),
    }
}

fn whole_remaining(data: &[u8]) -> Vec<Span> {
    if data.is_empty() {
        Vec::new()
    } else {
        vec![(0, data.len())]
    }
}

/// Drive a `fn(&[u8]) -> Option<total_len>` repeatedly: each call sees the
/// remaining tail, and a returned length becomes one span. Used by every
/// format whose header states its own exact byte length.
fn sized_spans(data: &[u8], size_of: fn(&[u8]) -> Option<usize>) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rest) = data.get(pos..) {
        if rest.is_empty() {
            break;
        }
        let Some(len) = size_of(rest) else { break };
        let len = len.max(1).min(rest.len());
        out.push((pos, pos + len));
        pos += len;
    }
    out
}

// ------------------------------------------------------------------- PNG

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// The 4-byte big-endian length field of the chunk starting at `cursor`.
fn png_chunk_len(rest: &[u8], cursor: usize) -> Option<usize> {
    let len_bytes = rest.get(cursor..cursor + 4)?;
    let len_arr = <[u8; 4]>::try_from(len_bytes).ok()?;
    Some(u32::from_be_bytes(len_arr) as usize)
}

fn png_spans(data: &[u8]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rest) = data.get(pos..) {
        if !rest.starts_with(&PNG_SIGNATURE) {
            break;
        }
        let mut cursor = 8usize;
        let mut found_end = None;
        while let Some(chunk_len) = png_chunk_len(rest, cursor) {
            let Some(kind) = rest.get(cursor + 4..cursor + 8) else {
                break;
            };
            let chunk_end = cursor.saturating_add(12).saturating_add(chunk_len);
            if kind == b"IEND" {
                found_end = Some(chunk_end.min(rest.len()));
                break;
            }
            if chunk_end <= cursor || chunk_end > rest.len() {
                break;
            }
            cursor = chunk_end;
        }
        let end = found_end.unwrap_or(rest.len());
        out.push((pos, pos + end));
        if end == 0 {
            break;
        }
        pos += end;
    }
    out
}

// ------------------------------------------------------------------ Marker

fn marker_spans(data: &[u8], start: [u8; 2], end: [u8; 2], skip_stuffing: bool) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(s) = find2(data, start, pos) {
        let scan_from = s.saturating_add(2);
        let e = if skip_stuffing {
            find_jpeg_eoi(data, end, scan_from)
        } else {
            find2(data, end, scan_from)
        }
        .map_or(data.len(), |e| e.saturating_add(2));
        out.push((s, e));
        if e <= s {
            break;
        }
        pos = e;
    }
    out
}

fn find2(data: &[u8], needle: [u8; 2], from: usize) -> Option<usize> {
    let slice = data.get(from..)?;
    if slice.len() < 2 {
        return None;
    }
    slice.windows(2).position(|w| w == needle).map(|i| i + from)
}

/// Scan for `end` starting at `from`, honouring JPEG's entropy-coded-segment
/// stuffing: `0xFF 0x00` is a literal `0xFF` byte in the entropy stream, and
/// `0xFF 0xD0..=0xD7` is a restart marker, both of which must be skipped
/// rather than mistaken for `end`.
fn find_jpeg_eoi(data: &[u8], end: [u8; 2], from: usize) -> Option<usize> {
    let mut i = from;
    loop {
        let &b0 = data.get(i)?;
        if b0 != 0xFF {
            i += 1;
            continue;
        }
        let &b1 = data.get(i + 1)?;
        if [b0, b1] == end {
            return Some(i);
        }
        if b1 == 0x00 || (0xD0..=0xD7).contains(&b1) {
            i += 2; // stuffed byte or restart marker: not a real boundary
            continue;
        }
        // Some other marker (e.g. a re-inserted DNL, or a second SOS in a
        // progressive scan): not the one we want, keep scanning past it.
        i += 2;
    }
}

// ------------------------------------------------------------------- RIFF

fn riff_size(rest: &[u8]) -> Option<usize> {
    if !rest.starts_with(b"RIFF") {
        return None;
    }
    let size_bytes = rest.get(4..8)?;
    let size_arr = <[u8; 4]>::try_from(size_bytes).ok()?;
    let size = u32::from_le_bytes(size_arr) as usize;
    let total = 8usize.checked_add(size)?;
    total.checked_add(total % 2) // RIFF word-aligns; pad if odd.
}

// -------------------------------------------------------------------- BMP

fn bmp_size(rest: &[u8]) -> Option<usize> {
    if !rest.starts_with(b"BM") {
        return None;
    }
    let size_bytes = rest.get(2..6)?;
    let size_arr = <[u8; 4]>::try_from(size_bytes).ok()?;
    Some(u32::from_le_bytes(size_arr) as usize)
}

// -------------------------------------------------------------------- QOI

const QOI_END_MARKER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];

fn qoi_spans(data: &[u8]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rest) = data.get(pos..) {
        if !rest.starts_with(b"qoif") || rest.len() < 14 {
            break;
        }
        let end = rest
            .windows(QOI_END_MARKER.len())
            .position(|w| w == QOI_END_MARKER)
            .map_or(rest.len(), |i| i + QOI_END_MARKER.len());
        out.push((pos, pos + end));
        if end == 0 {
            break;
        }
        pos += end;
    }
    out
}

// -------------------------------------------------------------------- XWD

/// `<X11/XWDFile.h>`'s `XWDFileHeader`: 25 big-endian `u32` fields, field 0 is
/// this header's own size (header struct + null-terminated window name),
/// field 18 is the colormap entry count (12 bytes each), field 11/12 are bits
/// per pixel and bytes per scanline, field 5 is pixel height. Public,
/// widely-documented X11 header layout; offsets cross-checked directly
/// against `ffmpeg -c:v xwd`'s own output (see the module docs) rather than
/// taken on faith.
fn xwd_size(rest: &[u8]) -> Option<usize> {
    let be = |off: usize| -> Option<usize> {
        let bytes = rest.get(off..off + 4)?;
        let arr = <[u8; 4]>::try_from(bytes).ok()?;
        Some(u32::from_be_bytes(arr) as usize)
    };
    let header_size = be(0)?;
    if header_size < 100 {
        return None; // smaller than the fixed struct: not an XWD header
    }
    let pixmap_height = be(20)?;
    let bytes_per_line = be(48)?;
    let ncolors = be(76)?;
    let pixels = bytes_per_line.checked_mul(pixmap_height)?;
    let colormap = ncolors.checked_mul(12)?;
    header_size.checked_add(colormap)?.checked_add(pixels)
}

// -------------------------------------------------------------------- PGX

/// `PG` SP (`ML`|`LM`) SP [`+`|`-`] prec SP width SP height LF, then a raster
/// of `ceil(prec/8) * width * height` bytes. JPEG2000 Part 4's test format;
/// public spec, not exercised against a real encoder (none exists in this
/// ffmpeg build).
fn pgx_size(rest: &[u8]) -> Option<usize> {
    if !rest.starts_with(b"PG") {
        return None;
    }
    let nl = rest.iter().position(|&b| b == b'\n')?;
    let line = std::str::from_utf8(rest.get(..nl)?).ok()?;
    let mut fields = line.split_whitespace();
    let _pg = fields.next()?;
    let _endian = fields.next()?;
    let prec_tok = fields.next()?;
    let prec_tok = prec_tok.trim_start_matches(['+', '-']);
    let prec: usize = prec_tok.parse().ok()?;
    let width: usize = fields.next()?.parse().ok()?;
    let height: usize = fields.next()?.parse().ok()?;
    let bytes_per_sample = prec.div_ceil(8).max(1);
    let raster = bytes_per_sample.checked_mul(width)?.checked_mul(height)?;
    (nl + 1).checked_add(raster)
}

// ------------------------------------------------------------- C-array text

/// XBM/XPM: a C source fragment ending at the pixel array's closing `};`.
/// Byte-oriented (not UTF-8-validated): both formats are ASCII in practice
/// and a malformed file simply fails to find the terminator, falling back to
/// the whole remainder.
fn carray_spans(data: &[u8]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rest) = data.get(pos..) {
        if rest.is_empty() {
            break;
        }
        let end = find_bytes(rest, b"};").map_or(rest.len(), |i| i + 2);
        out.push((pos, pos + end));
        if end == 0 {
            break;
        }
        pos += end;
    }
    out
}

fn find_bytes(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).position(|w| w == needle)
}

// -------------------------------------------------------------------- SVG

fn svg_spans(data: &[u8]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rest) = data.get(pos..) {
        if rest.is_empty() {
            break;
        }
        let end = find_bytes(rest, b"</svg>").map_or(rest.len(), |i| i + b"</svg>".len());
        out.push((pos, pos + end));
        if end == 0 {
            break;
        }
        pos += end;
    }
    out
}

pub mod netpbm;
pub mod radiance;

/// One minimal, structurally valid PNG (signature + empty `IHDR` + `IEND`),
/// for tests elsewhere in this crate (`pipe::tests`, the fuzz target's own
/// unit tests) that need real framed bytes without pulling in an image
/// encoder. Not a fixture of anything the reference wrote — just enough
/// structure for [`compute_spans`] to walk.
#[cfg(test)]
pub(crate) fn tests_support_png() -> Vec<u8> {
    let mut v = PNG_SIGNATURE.to_vec();
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&[0u8; 13]);
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(b"IEND");
    v.extend_from_slice(&0u32.to_be_bytes());
    v
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn png_bytes(iend_extra: &[u8]) -> Vec<u8> {
        let mut v = PNG_SIGNATURE.to_vec();
        // IHDR: 13-byte payload.
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&[0u8; 13]);
        v.extend_from_slice(&0u32.to_be_bytes()); // fake crc
        // IEND: zero-length payload.
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(b"IEND");
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(iend_extra);
        v
    }

    #[test]
    fn png_splits_concatenated_images() {
        let mut data = png_bytes(&[]);
        let one_len = data.len();
        data.extend(png_bytes(&[]));
        data.extend(png_bytes(&[]));
        let spans = compute_spans(ImageFraming::Png, &data);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0], (0, one_len));
        assert_eq!(spans[1], (one_len, one_len * 2));
    }

    #[test]
    fn png_with_no_signature_yields_nothing() {
        assert!(compute_spans(ImageFraming::Png, b"not a png").is_empty());
    }

    #[test]
    fn jpeg_marker_skips_stuffed_ff00_and_restart_markers() {
        let framing = ImageFraming::Marker {
            start: [0xFF, 0xD8],
            end: [0xFF, 0xD9],
            skip_stuffing: true,
        };
        // SOI, a stuffed 0xFF00 and a restart marker inside the "entropy"
        // data, then the real EOI.
        let data = [0xFF, 0xD8, 0x00, 0xFF, 0x00, 0xFF, 0xD1, 0xAA, 0xFF, 0xD9];
        let spans = compute_spans(framing, &data);
        assert_eq!(spans, vec![(0, 10)]);
    }

    #[test]
    fn jpeg_marker_splits_three_concatenated_images() {
        let framing = ImageFraming::Marker {
            start: [0xFF, 0xD8],
            end: [0xFF, 0xD9],
            skip_stuffing: true,
        };
        let one = [0xFFu8, 0xD8, 0x11, 0x22, 0xFF, 0xD9];
        let mut data = Vec::new();
        data.extend_from_slice(&one);
        data.extend_from_slice(&one);
        data.extend_from_slice(&one);
        let spans = compute_spans(framing, &data);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0], (0, 6));
        assert_eq!(spans[2], (12, 18));
    }

    #[test]
    fn riff_sized_reads_the_declared_length() {
        let mut data = Vec::from(*b"RIFF");
        data.extend_from_slice(&12u32.to_le_bytes()); // 8 + 12 = 20 total
        data.extend_from_slice(b"WEBPVP8 ");
        data.extend_from_slice(&[0u8; 4]);
        assert_eq!(data.len(), 20);
        let spans = compute_spans(ImageFraming::RiffSized, &data);
        assert_eq!(spans, vec![(0, 20)]);
    }

    #[test]
    fn bmp_sized_reads_the_declared_length() {
        let mut data = Vec::from(*b"BM");
        data.extend_from_slice(&30u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 24]);
        assert_eq!(data.len(), 30);
        let spans = compute_spans(ImageFraming::BmpSized, &data);
        assert_eq!(spans, vec![(0, 30)]);
    }

    #[test]
    fn qoi_finds_the_fixed_end_marker() {
        let mut data = Vec::from(*b"qoif");
        data.extend_from_slice(&8u32.to_be_bytes());
        data.extend_from_slice(&8u32.to_be_bytes());
        data.push(3);
        data.push(0);
        data.extend_from_slice(&[0xAB; 4]); // fake compressed body
        data.extend_from_slice(&QOI_END_MARKER);
        let spans = compute_spans(ImageFraming::Qoi, &data);
        assert_eq!(spans, vec![(0, data.len())]);
    }

    #[test]
    fn carray_text_finds_closing_brace() {
        let data = b"static char x[] = {\n1,2,3\n};static char y[]={4};".to_vec();
        let spans = compute_spans(ImageFraming::CArrayText, &data);
        assert_eq!(spans.len(), 2);
        assert_eq!(
            &data[spans[0].0..spans[0].1],
            b"static char x[] = {\n1,2,3\n};"
        );
    }

    #[test]
    fn svg_text_finds_closing_tag() {
        let data = b"<svg></svg><svg foo=\"1\"></svg>".to_vec();
        let spans = compute_spans(ImageFraming::SvgText, &data);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn whole_remaining_is_one_span_regardless_of_content() {
        let data = b"anything at all, even multiple\x00fake\x00images".to_vec();
        assert_eq!(
            compute_spans(ImageFraming::WholeRemaining, &data),
            vec![(0, data.len())]
        );
        assert!(compute_spans(ImageFraming::WholeRemaining, &[]).is_empty());
    }

    #[test]
    fn every_strategy_terminates_on_empty_input() {
        for framing in [
            ImageFraming::WholeRemaining,
            ImageFraming::Png,
            ImageFraming::Marker {
                start: [0xFF, 0xD8],
                end: [0xFF, 0xD9],
                skip_stuffing: true,
            },
            ImageFraming::RiffSized,
            ImageFraming::BmpSized,
            ImageFraming::Netpbm,
            ImageFraming::Pgx,
            ImageFraming::Qoi,
            ImageFraming::Xwd,
            ImageFraming::CArrayText,
            ImageFraming::SvgText,
            ImageFraming::Radiance,
        ] {
            assert!(compute_spans(framing, &[]).is_empty());
        }
    }
}
