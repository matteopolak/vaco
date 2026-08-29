//! Fixed encoder configuration and header-packet construction (spec section
//! 4.2): identification, comment, and setup, plus the parameters the
//! per-frame audio encoder ([`crate::encoder`]) needs to stay consistent
//! with whatever this module wrote into the setup header.
//!
//! One configuration for every stream this encoder produces (issue #309):
//! floor type 1 with a single partition class shared by every partition,
//! residue type 1 in a single partition covering the whole spectrum, and a
//! single mode with no block-size switching. See [`crate::enc_codebook`] for
//! why every codebook here is flat/ordered, and the module docs on
//! [`crate::floor1`]/[`crate::residue`] this mirrors for the decode-side
//! shapes these bits must parse back into.
//!
//! `Vaco-Spec-Ref: vorbis-i sections 4.2.2, 4.2.3, 4.2.4 and 5.2`

use crate::bitreader::BitWriterLsb;
use crate::enc_codebook::{flat_code_bits, write_scalar_codebook, write_scalar_vq_codebook};

pub(crate) const IDENT_MAGIC: &[u8] = b"\x01vorbis";
pub(crate) const COMMENT_MAGIC: &[u8] = b"\x03vorbis";
pub(crate) const SETUP_MAGIC: &[u8] = b"\x05vorbis";

/// The block size every stream this encoder produces uses — one mode, no
/// short/long switching (see the module doc). A power of two in
/// `{64..=8192}`, the identification header's own valid range.
pub(crate) const BLOCK_SIZE: u32 = 2048;

/// `BLOCK_SIZE / 2`: both the per-channel spectral vector length
/// ([`crate::floor1::compute_curve`]'s and [`crate::residue::decode`]'s `n`)
/// and the hop between successive analysis windows (50% overlap).
#[allow(
    clippy::integer_division,
    reason = "BLOCK_SIZE is a fixed power of two (2048); the halving is exact"
)]
pub(crate) const HALF: u32 = BLOCK_SIZE / 2;

/// `ilog(HALF - 1)`: the floor1 `range_bits` field such that
/// `1 << RANGE_BITS == HALF` exactly, so the floor curve's fixed second
/// endpoint (spec 7.2.1's `postlist[1] = 1 << range_bits`) lines up with the
/// last spectral line.
const RANGE_BITS: u32 = 10;

/// Floor1 `multiplier` field (1..=4): `2` selects the 128-step quantised dB
/// range (spec 7.2.1's `[1,2,3,4] -> [256,128,86,64]` table).
pub(crate) const FLOOR_MULTIPLIER: u8 = 2;
pub(crate) const FLOOR_RANGE: u32 = 128;

/// Codebook indices, fixed by the order [`build_setup_header`] writes them.
pub(crate) const BOOK_FLOOR: u8 = 0;
pub(crate) const BOOK_CLASS: u8 = 1;
pub(crate) const BOOK_RESIDUE: u8 = 2;

/// Residue scalar-VQ quantiser: `entries` levels spanning
/// `[RESIDUE_MIN, RESIDUE_MIN + (entries-1)*RESIDUE_DELTA]`. Chosen to cover
/// a `floor`-normalised MDCT coefficient's typical range (see
/// [`crate::encoder`]'s quantisation step) at a bit cost this batch's
/// "fixed low-complexity" brief accepts over real entropy coding.
pub(crate) const RESIDUE_ENTRIES: u32 = 32;
pub(crate) const RESIDUE_MIN: f32 = -6.0;
pub(crate) const RESIDUE_DELTA: f32 = 12.0 / (RESIDUE_ENTRIES - 1) as f32;

/// The floor curve's `x` positions beyond the two fixed endpoints (`0` and
/// `HALF`), one per residue... one per floor1 partition (each partition here
/// has class dimension 1, so this list is also the partition count). Spread
/// roughly geometrically since spectral envelopes vary faster at low
/// frequencies; values are arbitrary beyond being distinct and inside
/// `(0, HALF)`, which [`crate::floor1::Floor1Config::parse_header`]'s
/// duplicate check requires.
pub(crate) const FLOOR_X: &[u32] = &[
    8, 16, 24, 36, 52, 74, 104, 146, 202, 278, 380, 512, 640, 768, 896, 960,
];

fn write_ident_header(w: &mut BitWriterLsb, channels: u8, sample_rate: u32) {
    w.put(0, 32); // vorbis_version
    w.put(u32::from(channels), 8);
    w.put(sample_rate, 32);
    w.put(0, 32); // bitrate_maximum: unset
    w.put(0, 32); // bitrate_nominal: unset
    w.put(0, 32); // bitrate_minimum: unset
    w.put(BLOCK_SIZE.trailing_zeros(), 4); // blocksize_0 exponent
    w.put(BLOCK_SIZE.trailing_zeros(), 4); // blocksize_1 exponent (same: no switching)
    w.put_bool(true); // framing bit
}

const VENDOR: &[u8] = b"vaco vorbis encoder (native, fixed setup, issue #309)";

fn write_comment_header(w: &mut BitWriterLsb) {
    for byte in u32::try_from(VENDOR.len()).unwrap_or(0).to_le_bytes() {
        w.put(u32::from(byte), 8);
    }
    for &b in VENDOR {
        w.put(u32::from(b), 8);
    }
    w.put(0, 32); // user comment list length: none
    w.put_bool(true); // framing bit
}

fn write_setup_header(w: &mut BitWriterLsb) {
    // Codebooks: floor y-value book, residue classbook, residue VQ book.
    w.put(2, 8); // codebook_count - 1 == 2 (three codebooks)
    write_scalar_codebook(w, FLOOR_RANGE);
    write_scalar_codebook(w, 1); // single-entry classbook: classifications == 1
    let residue_bits = flat_code_bits(RESIDUE_ENTRIES);
    write_scalar_vq_codebook(w, RESIDUE_ENTRIES, RESIDUE_MIN, RESIDUE_DELTA, residue_bits);

    // Time-domain transform placeholders: exactly one, required to read 0.
    w.put(0, 6); // time_count - 1 == 0
    w.put(0, 16);

    // Floors: one floor1, `FLOOR_X.len()` partitions, all class 0, dim 1,
    // no subclass cascade, sharing `BOOK_FLOOR`.
    w.put(0, 6); // floor_count - 1 == 0
    w.put(1, 16); // floor type 1
    let partitions = u32::try_from(FLOOR_X.len()).unwrap_or(0);
    w.put(partitions, 5);
    for _ in 0..partitions {
        w.put(0, 4); // partition_class_list[i] == 0
    }
    w.put(0, 3); // class_dimensions[0] - 1 == 0 (dimension 1)
    w.put(0, 2); // class_subclasses[0] == 0 (no cascade, no masterbook read)
    w.put(u32::from(BOOK_FLOOR).saturating_add(1), 8); // the class's one subclass book
    w.put(u32::from(FLOOR_MULTIPLIER).saturating_sub(1), 2);
    w.put(RANGE_BITS, 4);
    for &x in FLOOR_X {
        w.put(x, RANGE_BITS);
    }

    // Residues: one residue type 1, single partition covering [0, HALF),
    // single classification, `BOOK_RESIDUE` active on cascade pass 0 only.
    w.put(0, 6); // residue_count - 1 == 0
    w.put(1, 16); // residue type 1
    w.put(0, 24); // begin
    w.put(HALF, 24); // end
    w.put(HALF.saturating_sub(1), 24); // partition_size - 1 == HALF - 1
    w.put(0, 6); // classifications - 1 == 0 (one classification)
    w.put(u32::from(BOOK_CLASS), 8); // classbook
    w.put(1, 3); // cascade low_bits: bit 0 set (pass 0 active)
    w.put_bool(false); // cascade bitflag: no high bits needed
    w.put(u32::from(BOOK_RESIDUE), 8); // pass 0's book

    // Mappings: one mapping type 0, one submap, no coupling.
    w.put(0, 6); // mapping_count - 1 == 0
    w.put(0, 16); // mapping type 0
    w.put_bool(false); // submaps flag: exactly one submap
    w.put_bool(false); // coupling flag: no channel coupling
    w.put(0, 2); // reserved
    w.put(0, 8); // time placeholder (ignored by decode)
    w.put(0, 8); // floor number: floor 0
    w.put(0, 8); // residue number: residue 0

    // Modes: one mode, long block flag false (uses blocksize_0, which
    // equals blocksize_1 here), mapping 0.
    w.put(0, 6); // mode_count - 1 == 0
    w.put_bool(false); // blockflag
    w.put(0, 16); // windowtype
    w.put(0, 16); // transformtype
    w.put(0, 8); // mapping number

    w.put_bool(true); // framing bit
}

/// Build the three header packets (spec 4.2.2/4.2.3/4.2.4), each with its
/// `[packet_type][vorbis]` common prefix.
fn header_packets(channels: u8, sample_rate: u32) -> [Vec<u8>; 3] {
    let mut ident = BitWriterLsb::new();
    write_ident_header(&mut ident, channels, sample_rate);
    let mut ident_bytes = IDENT_MAGIC.to_vec();
    ident_bytes.extend_from_slice(&ident.finish());

    let mut comment = BitWriterLsb::new();
    write_comment_header(&mut comment);
    let mut comment_bytes = COMMENT_MAGIC.to_vec();
    comment_bytes.extend_from_slice(&comment.finish());

    let mut setup = BitWriterLsb::new();
    write_setup_header(&mut setup);
    let mut setup_bytes = SETUP_MAGIC.to_vec();
    setup_bytes.extend_from_slice(&setup.finish());

    [ident_bytes, comment_bytes, setup_bytes]
}

/// Xiph-lace the three header packets into the one `extradata` blob a
/// container's codec-parameters channel carries (the exact inverse of
/// [`crate::decoder::split_xiph_headers`], and the shape
/// `vaco-mux-ogg::writer` already expects for Vorbis — see that crate's own
/// doc on why Vorbis needs three packets where Opus/FLAC need one).
#[must_use]
pub(crate) fn build_extradata(channels: u8, sample_rate: u32) -> Vec<u8> {
    let [p0, p1, _p2] = header_packets(channels, sample_rate);
    // 2 header packets, plus the setup packet's own length is implicit
    // (whatever remains): count - 1 == 2 (three headers total).
    let mut out = vec![2u8];
    for p in [&p0, &p1] {
        let mut len = p.len();
        while len >= 255 {
            out.push(255);
            len -= 255;
        }
        out.push(u8::try_from(len).unwrap_or(255));
    }
    let [p0, p1, p2] = header_packets(channels, sample_rate);
    out.extend_from_slice(&p0);
    out.extend_from_slice(&p1);
    out.extend_from_slice(&p2);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::decoder::VorbisDecoder;
    use vaco_codec_core::Decoder;
    use vaco_limits::Limits;

    #[test]
    fn extradata_is_accepted_by_this_crate_s_own_decoder() {
        let extradata = build_extradata(2, 44_100);
        let mut dec = VorbisDecoder::new(Limits::permissive());
        dec.set_extradata(&extradata).unwrap();
    }

    #[test]
    fn extradata_is_accepted_for_mono_and_many_channels() {
        for &ch in &[1u8, 2, 6] {
            let extradata = build_extradata(ch, 48_000);
            let mut dec = VorbisDecoder::new(Limits::permissive());
            dec.set_extradata(&extradata).unwrap();
        }
    }
}
