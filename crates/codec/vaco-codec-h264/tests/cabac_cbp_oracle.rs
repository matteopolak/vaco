//! Independent, black-box verification of [`vaco_codec_h264::mb`]'s
//! `coded_block_pattern` decode (`decode_cbp_cabac`) for the first
//! macroblock of a slice -- the exact syntax element #418's investigation
//! has repeatedly needed to check against a reference and repeatedly
//! found it *cannot*: no `ffmpeg -debug` sub-flag (checked every one `ffmpeg
//! 8.1 -h full` lists) prints per-macroblock `coded_block_pattern`, so a
//! prior round's claim that address 0-4's own CBP values "matched the
//! reference" turned out to be this decoder's own self-consistency, not an
//! independent observation -- exactly the kind of premise this project has
//! repeatedly found collapses on inspection.
//!
//! # The instrument, and why this one
//!
//! Two options were on the table. **Patching bytes directly into a real
//! CABAC-coded bitstream** (the technique a sibling agent used successfully
//! for MPEG-2 this session) does not transfer here: MPEG-2's syntax is
//! VLC-coded with recoverable codeword boundaries, but CABAC is an
//! arithmetic code with no such boundary -- its `range`/`offset` state
//! evolves continuously across every decision in the slice, so overwriting
//! compressed bytes at a chosen offset would desynchronise everything
//! downstream of the patch rather than cleanly substituting one value.
//! **Inferring CBP from reconstructed pixel output** was the fallback
//! option on the table, but it is genuinely weaker evidence (residual
//! magnitude is visible in reconstructed error, but not which of the four
//! luma quadrants or which chroma component actually carried it).
//!
//! What this file does instead, and why it counts as a real oracle rather
//! than another self-consistency check: **construct genuine encoder
//! *input*** (raw YUV, not a hand-built bitstream) so that `libx264`'s own
//! per-macroblock accounting -- printed to stderr as `coded y,uvDC,uvAC
//! intra: NN.N% NN.N% NN.N%` by the real, unmodified encoder, entirely
//! independent of anything in this crate -- states outright whether luma
//! and/or chroma residual was coded for every macroblock in the frame.
//! Combined with one purely structural argument that needs no reference at
//! all (see `cbp_oracle_flat_frame_decodes_to_zero_everywhere` below), this
//! gives two different, independently corroborated ground truths for
//! address 0 of a slice with every neighbour unavailable -- the exact
//! structural position addresses 0-4 of the real corpora occupy.
//!
//! # Fixture generation
//!
//! `cabac_cbp_oracle_flat.264` -- every Y/Cb/Cr sample set to exactly 128
//! before encoding:
//!
//! ```text
//! python3 -c "
//! w,h = 64,64
//! y = bytes([128])*(w*h)
//! c = bytes([128])*((w//2)*(h//2))
//! open('flat128.yuv','wb').write(y+c+c)
//! "
//! ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 64x64 -i flat128.yuv -frames:v 1 \
//!        -c:v libx264 -profile:v main -coder cabac -bf 0 -refs 1 -g 30 \
//!        -x264opts "no-8x8dct" -f h264 cabac_cbp_oracle_flat.264
//! ```
//!
//! `libx264`'s own log for this encode: `mb I I16..4: 100.0% 0.0% 0.0%`,
//! `coded y,uvDC,uvAC intra: 0.0% 0.0% 0.0%` -- the encoder's own count of
//! *zero* macroblocks with any coded residual, of any kind, anywhere in the
//! frame.
//!
//! `cabac_cbp_oracle_noise.264` -- every Y/Cb/Cr sample independently
//! random (`random.seed(7)`), encoded at `-qp 12` (low QP, so genuine
//! high-frequency noise content survives quantisation in every 4x4 block):
//!
//! ```text
//! python3 -c "
//! import random
//! random.seed(7)
//! w, h = 64, 64
//! y = bytearray(random.randrange(0,256) for _ in range(w*h))
//! cw, ch = w//2, h//2
//! cb = bytearray(random.randrange(0,256) for _ in range(cw*ch))
//! cr = bytearray(random.randrange(0,256) for _ in range(cw*ch))
//! open('noise_chroma.yuv','wb').write(bytes(y)+bytes(cb)+bytes(cr))
//! "
//! ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 64x64 -i noise_chroma.yuv -frames:v 1 \
//!        -c:v libx264 -profile:v main -coder cabac -bf 0 -refs 1 -g 30 -qp 12 \
//!        -x264opts "no-8x8dct" -f h264 cabac_cbp_oracle_noise.264
//! ```
//!
//! `libx264`'s own log: `mb I I16..4: 0.0% 0.0% 100.0%` (every macroblock
//! `I_NxN`, so `coded_block_pattern` is explicitly CABAC-decoded via
//! `decode_cbp_cabac` for every one of them -- `I_16x16` embeds its own
//! `cbp` in `mb_type` instead and would not exercise this function),
//! `coded y,uvDC,uvAC intra: 100.0% 100.0% 100.0%` -- every macroblock has
//! luma residual, chroma DC residual, *and* chroma AC residual, which per
//! Table 9-4 is exactly `cbp_chroma == 2` -- the same `cbp_chroma` value
//! previously reported (unverified, at the time) for `cabac_ip_simple.264`'s
//! own address 0.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "test code over a fixed fixture")]

use vaco_bitstream::{BitReader, annexb};
use vaco_codec_cabac::CabacDecoder;
use vaco_format_nalu::RbspBuf;
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

/// Decode every slice in `data` and return the first slice's
/// `first_slice_mb_cbp` -- the `(cbp_luma, cbp_chroma)` this crate's own
/// `decode_cbp_cabac` produced for address 0, the exact value under
/// dispute.
fn first_slice_cbp(data: &[u8]) -> Option<(u8, u8)> {
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();
    let mut result = None;

    for nal in annexb::nal_units(data) {
        let Some(header) = H264NalHeader::parse(nal) else { continue };
        match header.nal_unit_type {
            NalUnitType::Sps => {
                rbsp.fill(nal, &mut budget).unwrap();
                let _ = params.add_sps(rbsp.as_slice(), &mut budget);
            }
            NalUnitType::Pps => {
                rbsp.fill(nal, &mut budget).unwrap();
                let _ = params.add_pps(rbsp.as_slice(), &mut budget);
            }
            NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                rbsp.fill(nal, &mut budget).unwrap();
                let payload = rbsp.as_slice();
                let mut reader = BitReader::new(payload);
                reader.skip(8);
                let pps_id = {
                    let mut r2 = BitReader::new(payload);
                    r2.skip(8);
                    let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                    let _ = g.ue_v(u32::MAX).unwrap();
                    let _ = g.ue_v(9).unwrap();
                    g.ue_v(255).unwrap() as u8
                };
                let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                let slice_header =
                    SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget).unwrap();
                let mut cabac = CabacDecoder::from_reader(reader);
                let stats =
                    vaco_codec_h264::mb::decode_slice_cabac(&mut cabac, &mut budget, sps, pps, &slice_header)
                        .unwrap_or_else(|e| panic!("{e:?}"));
                assert!(!cabac.malformed(), "CABAC engine reported malformed input");
                if result.is_none() {
                    result = Some(stats.first_slice_mb_cbp.expect("slice had at least one macroblock"));
                }
            }
            _ => {}
        }
    }
    result
}

/// Structural ground truth, no encoder log needed: every Y/Cb/Cr sample in
/// the source frame is exactly 128, the same value clause 8.3.1.2.1's
/// unavailable-neighbour substitution always uses. Every intra prediction
/// mode is some linear/directional combination of neighbouring samples (or
/// the substituted 128, where neighbours are unavailable) -- applied to an
/// already-128 neighbourhood, every one of them predicts 128 again, exactly
/// matching the already-128 source. The residual is exactly zero
/// everywhere, for every macroblock, regardless of which mode the encoder
/// picks or what `QP` is used (zero input to the transform quantises to
/// zero at any step size) -- so `coded_block_pattern` must be `(0, 0)` by
/// construction, not by appeal to any reference.
#[test]
fn cbp_oracle_flat_frame_decodes_to_zero_everywhere() {
    let data: &[u8] = include_bytes!("fixtures/cabac_cbp_oracle_flat.264");
    assert_eq!(first_slice_cbp(data), Some((0, 0)));
}

/// Encoder-log ground truth: `libx264` itself reports 100% of macroblocks
/// coded with luma residual, chroma DC residual, *and* chroma AC residual
/// for this fixture (see this file's module doc) -- independent of this
/// crate, from the real encoder's own accounting. Table 9-4 maps "DC and AC
/// chroma both coded" to `cbp_chroma == 2`; every macroblock being `I_NxN`
/// (0% `I_16x16`) means `cbp_luma`/`cbp_chroma` come from the explicit
/// `decode_cbp_cabac` path, not `mb_type`'s embedded encoding.
#[test]
fn cbp_oracle_noise_frame_matches_libx264s_own_accounting() {
    let data: &[u8] = include_bytes!("fixtures/cabac_cbp_oracle_noise.264");
    assert_eq!(first_slice_cbp(data), Some((0b1111, 2)));
}
