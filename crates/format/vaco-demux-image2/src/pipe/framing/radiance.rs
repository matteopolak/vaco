//! Radiance (`.hdr`/`.pic`) framing: a text header, then new-format-RLE
//! scanlines.
//!
//! Measured against `ffmpeg -c:v hdr`: `#?RADIANCE\n`, more `KEY=value`
//! lines, a blank line, a resolution line (`-Y <height> +X <width>\n`), then
//! `height` scanlines. Each scanline this crate's own encoder writes opens
//! with the new-format RLE marker `02 02 <hi> <lo>` (the big-endian scanline
//! width, valid only for widths in `8..=0x7fff`), followed by four
//! run/literal-coded channel planes (R, G, B, E).
//!
//! # What this does not handle
//!
//! The *old* Radiance formats — flat 4-byte-per-pixel RGBE with no RLE, and
//! the earlier `(1,1,1,n)`-repeat RLE — are a different, undelimited shape
//! that cannot be walked without effectively decoding it, and this crate's
//! probe of the reference produced only the new format. [`spans`] falls back
//! to [`super::ImageFraming::WholeRemaining`]'s behaviour (the rest of
//! the buffer is one image) the moment a scanline does not start with the
//! new-format marker — for a genuinely old-format file that means exactly one
//! packet, which is always a safe answer, just not a maximally split one.

use super::Span;

const NEW_RLE_MARKER: [u8; 2] = [0x02, 0x02];

/// Split `data` into whole-Radiance-image spans.
#[must_use]
pub fn spans(data: &[u8]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rest) = data.get(pos..) {
        if rest.is_empty() || !(rest.starts_with(b"#?RADIANCE") || rest.starts_with(b"#?RGBE")) {
            break;
        }
        let end = one_image_len(rest).unwrap_or(rest.len());
        out.push((pos, pos + end));
        if end == 0 {
            break;
        }
        pos += end;
    }
    out
}

fn one_image_len(rest: &[u8]) -> Option<usize> {
    let (header_end, width, height) = parse_header(rest)?;
    let mut cursor = header_end;
    for _ in 0..height {
        cursor = scanline_end(rest, cursor, width)?;
    }
    Some(cursor)
}

/// Consume `KEY=value` lines up to the blank line, then the resolution line.
/// Returns `(offset just past the resolution line, width, height)`.
fn parse_header(rest: &[u8]) -> Option<(usize, usize, usize)> {
    let mut pos = 0usize;
    loop {
        let line_end = rest.get(pos..)?.iter().position(|&b| b == b'\n')? + pos;
        let line = rest.get(pos..line_end)?;
        pos = line_end + 1;
        if line.is_empty() {
            break; // the blank line separating info lines from the resolution
        }
    }
    let line_end = rest.get(pos..)?.iter().position(|&b| b == b'\n')? + pos;
    let line = std::str::from_utf8(rest.get(pos..line_end)?).ok()?;
    pos = line_end + 1;

    // "-Y <h> +X <w>" is what this crate's probe produced; the other three
    // axis-order permutations are part of the same public format and are
    // accepted too, since nothing here needs to know the image's orientation.
    let mut fields = line.split_whitespace();
    let mut width = None;
    let mut height = None;
    while let (Some(sign_axis), Some(value)) = (fields.next(), fields.next()) {
        let n: usize = value.parse().ok()?;
        match sign_axis.chars().last() {
            Some('X') => width = Some(n),
            Some('Y') => height = Some(n),
            _ => return None,
        }
    }
    Some((pos, width?, height?))
}

/// Walk one new-format-RLE scanline (4-byte marker + 4 run/literal-coded
/// channel planes) and return the offset just past it, or `None` if `cursor`
/// is not the start of a well-formed new-format scanline.
fn scanline_end(rest: &[u8], cursor: usize, width: usize) -> Option<usize> {
    if !(8..=0x7fff).contains(&width) {
        return None;
    }
    let marker = rest.get(cursor..cursor + 4)?;
    if marker.get(0..2) != Some(&NEW_RLE_MARKER[..]) {
        return None;
    }
    let declared_width = (usize::from(*marker.get(2)?) << 8) | usize::from(*marker.get(3)?);
    if declared_width != width {
        return None;
    }
    let mut pos = cursor + 4;
    for _channel in 0..4 {
        let mut produced = 0usize;
        while produced < width {
            let &count = rest.get(pos)?;
            pos += 1;
            if count > 128 {
                let run = usize::from(count - 128);
                let _value = *rest.get(pos)?;
                pos += 1;
                produced += run;
            } else {
                let lit = usize::from(count);
                if lit == 0 {
                    return None; // malformed: a literal run of zero never terminates
                }
                pos = pos.checked_add(lit)?;
                if pos > rest.len() {
                    return None;
                }
                produced += lit;
            }
        }
        if produced != width {
            return None; // over- or under-ran the row: not a well-formed scanline
        }
    }
    Some(pos)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn one_scanline(width: usize, value: u8) -> Vec<u8> {
        let mut row = Vec::new();
        row.extend_from_slice(&NEW_RLE_MARKER);
        row.push((width >> 8) as u8);
        row.push((width & 0xFF) as u8);
        for _ in 0..4 {
            // One run covering the whole row.
            row.push(128 + u8::try_from(width).unwrap());
            row.push(value);
        }
        row
    }

    fn one_image(width: usize, height: usize) -> Vec<u8> {
        let mut v = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n".to_vec();
        v.extend_from_slice(format!("-Y {height} +X {width}\n").as_bytes());
        for _ in 0..height {
            v.extend(one_scanline(width, 0x80));
        }
        v
    }

    #[test]
    fn single_image_length_matches_construction() {
        let img = one_image(8, 4);
        let spans = spans(&img);
        assert_eq!(spans, vec![(0, img.len())]);
    }

    #[test]
    fn splits_three_concatenated_images() {
        let one = one_image(8, 2);
        let mut data = Vec::new();
        data.extend_from_slice(&one);
        data.extend_from_slice(&one);
        data.extend_from_slice(&one);
        let spans = spans(&data);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0], (0, one.len()));
        assert_eq!(spans[2], (one.len() * 2, one.len() * 3));
    }

    #[test]
    fn non_radiance_input_yields_nothing() {
        assert!(spans(b"not radiance at all").is_empty());
    }

    #[test]
    fn old_format_falls_back_to_whole_remaining() {
        // A header claiming one 8x1 scanline, but flat (non-RLE) pixel data:
        // the RLE marker check fails and this falls back to "the rest".
        let mut data = b"#?RADIANCE\n\n".to_vec();
        data.extend_from_slice(b"-Y 1 +X 8\n");
        data.extend_from_slice(&[0x10; 32]); // 8 pixels * 4 bytes, no RLE marker
        let spans = spans(&data);
        assert_eq!(spans, vec![(0, data.len())]);
    }

    #[test]
    fn truncated_header_falls_back_to_whole_remaining_rather_than_panicking() {
        // A signature with no complete header (no blank line, or no
        // resolution line): not a well-formed Radiance image, but not
        // "contribute nothing" either — same WholeRemaining fallback as an
        // old-format scanline, and safe for the same reason.
        let a = b"#?RADIANCE\nFORMAT=x\n".to_vec();
        assert_eq!(spans(&a), vec![(0, a.len())]);
        let b = b"#?RADIANCE\n\n-Y".to_vec();
        assert_eq!(spans(&b), vec![(0, b.len())]);
    }
}
