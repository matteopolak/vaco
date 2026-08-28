//! Theora decode against arbitrary bytes: frame header, block-level qi
//! decode, DCT token decode, DC prediction, dequantization/IDCT
//! reconstruction, and the loop filter — the whole
//! `TheoraDecoder::send_packet`/`receive_frame` pipeline for a keyframe.
//!
//! The `extradata` (identification/comment/setup headers, Xiph-laced) is a
//! small fixed, hand-built, valid one rather than fuzzed input: building a
//! *correct* setup header is what `[crate::setup::Setup::parse]` itself
//! already has unit tests for, and fuzzing header bytes here would mostly
//! exercise the "reject a malformed header" path rather than the frame
//! decode pipeline this target exists for (the same reasoning
//! `opus_decode`'s fuzz target gives for its own fixed `OpusHead`). The 80
//! DCT token Huffman tables are all the trivial single-entry (0-bit code)
//! table for token 0 (a length-1 EOB run) — enough to make every table
//! decodable, though it means the fuzzed bytes drive frame-header and
//! block-qi decode more than DCT token variety; deepening that is future
//! work, not a correctness gap in what is asserted here.
//!
//! What is asserted beyond "does not panic": a decoded frame's declared
//! dimensions match the identification header's picture region, and its
//! pixel format matches the header's declared subsampling.
//!
//! fuzz-crate: vaco-codec-theora

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_theora::TheoraDecoder;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

/// MSB-first bit writer, matching Theora's own bitpacking convention
/// (section 5).
struct BitWriter {
    bytes: Vec<u8>,
    buf: u32,
    n: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            buf: 0,
            n: 0,
        }
    }

    fn put(&mut self, value: u32, width: u32) {
        for i in (0..width).rev() {
            let bit = (value >> i) & 1;
            self.buf = (self.buf << 1) | bit;
            self.n += 1;
            if self.n == 8 {
                self.bytes.push(u8::try_from(self.buf).unwrap_or(0));
                self.buf = 0;
                self.n = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.buf <<= 8 - self.n;
            self.bytes.push(u8::try_from(self.buf).unwrap_or(0));
        }
        self.bytes
    }
}

/// A minimal, valid identification header body (section 6.2): a 32x32
/// (2x2 macro blocks) 4:2:0 frame, no crop.
fn ident_body() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put(3, 8); // vmaj
    w.put(2, 8); // vmin
    w.put(0, 8); // vrev
    w.put(2, 16); // fmbw
    w.put(2, 16); // fmbh
    w.put(32, 24); // picw
    w.put(32, 24); // pich
    w.put(0, 8); // picx
    w.put(0, 8); // picy
    w.put(30, 32); // frn
    w.put(1, 32); // frd
    w.put(1, 24); // parn
    w.put(1, 24); // pard
    w.put(0, 8); // cs
    w.put(0, 24); // nombr
    w.put(0, 6); // qual
    w.put(0, 5); // kfgshift
    w.put(0, 2); // pf = 4:2:0
    w.put(0, 3); // reserved
    w.finish()
}

/// A minimal, valid setup header body (section 6.4): one base matrix, one
/// quant range per `(qti, pli)` all chained back to a single real
/// definition, and 80 trivial one-entry Huffman tables.
fn setup_body() -> Vec<u8> {
    let mut w = BitWriter::new();

    // Loop filter limits (reconstructed procedure; see `setup` module doc):
    // 1-bit-wide, all zero (loop filter disabled).
    w.put(0, 3);
    for _ in 0..64 {
        w.put(0, 1);
    }

    // AC/DC scale tables: 1-bit-wide, all one.
    w.put(0, 4);
    for _ in 0..64 {
        w.put(1, 1);
    }
    w.put(0, 4);
    for _ in 0..64 {
        w.put(1, 1);
    }

    // One base matrix (all zero coefficients).
    w.put(0, 9); // NBMS - 1 = 0 => NBMS = 1
    for _ in 0..64 {
        w.put(0, 8);
    }

    // Quant ranges: (qti=0, pli=0) defines one range spanning all 63 qi
    // steps; every other (qti, pli) copies from the chain (see
    // `quant::QuantParams::parse`'s doc for the copy-index formula this
    // relies on: 0,1 -> 0,0; 0,2 -> 0,1; 1,0 -> 0,2; 1,1 -> 1,0; 1,2 -> 1,1).
    // ilog(NBMS - 1) == ilog(0) == 0 bits for every base-matrix-index field.
    w.put(62, 6); // ilog(62) == 6 bits: size - 1 = 62 => size = 63
    // (bmi fields are 0 width; nothing to write for them)
    for (qti, _pli) in [(0u32, 1u32), (0, 2), (1, 0), (1, 1), (1, 2)] {
        w.put(0, 1); // NEWQR = 0 (copy)
        if qti > 0 {
            w.put(0, 1); // RPQR = 0 (use the "most recent" chain)
        }
    }

    // 80 Huffman tables, each a single leaf (token 0) at the empty code.
    for _ in 0..80 {
        w.put(1, 1); // ISLEAF
        w.put(0, 5); // TOKEN = 0
    }

    w.finish()
}

fn pack_xiph(headers: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![u8::try_from(headers.len().saturating_sub(1)).unwrap_or(0)];
    for h in headers.iter().take(headers.len().saturating_sub(1)) {
        let mut len = h.len();
        while len >= 255 {
            out.push(255);
            len -= 255;
        }
        out.push(u8::try_from(len).unwrap_or(0));
    }
    for h in headers {
        out.extend_from_slice(h);
    }
    out
}

fn build_extradata() -> Vec<u8> {
    let mut ident = vec![0x80u8];
    ident.extend_from_slice(b"theora");
    ident.extend_from_slice(&ident_body());

    let mut comment = vec![0x81u8];
    comment.extend_from_slice(b"theora");
    comment.extend_from_slice(&[0u8; 8]); // empty vendor string + comment count

    let mut setup = vec![0x82u8];
    setup.extend_from_slice(b"theora");
    setup.extend_from_slice(&setup_body());

    pack_xiph(&[ident, comment, setup])
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 8192 {
        return;
    }

    let mut dec = TheoraDecoder::new(Limits::default());
    if dec.set_extradata(&build_extradata()).is_err() {
        return;
    }

    let mut budget = Budget::new(Limits::default());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };

    for _ in 0..2 {
        if dec.send_packet(Some(&packet)).is_err() {
            continue;
        }
        while let Ok(frame) = dec.receive_frame() {
            let FrameData::Video {
                width,
                height,
                format,
                ..
            } = frame.data
            else {
                continue;
            };
            assert_eq!((width, height), (32, 32), "picture region mismatch");
            assert_eq!(format, PixFmt::Yuv420p, "pixel format mismatch");
        }
    }
});
