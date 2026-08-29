//! End-to-end oracle: [`TheoraDecoder`] against a real Theora file, decoded
//! here and compared to `ffmpeg`'s own reference decode of the same bytes.
//!
//! # Fixture
//!
//! `fixtures/bear.ogv` — `ffmpeg`'s own FATE test suite
//! (`https://fate-suite.ffmpeg.org/ogg/bear.ogv`), a real 320x180 4:2:0
//! clip encoded by an old `ffmpeg`/`libtheora` (`Lavc52.32.0`), one of the
//! two Ogg/Theora fixtures that suite carries. This crate decodes intra
//! (keyframe) frames only; `bear.ogv` keyframes at packet indices 0, 12 and
//! 24 (every 12th frame, `ffprobe -show_frames` confirms `pict_type=I` at
//! exactly those three and `P` everywhere else) are the ones this test
//! exercises. `fixtures/bear_frame{0,12,24}.yuv` are `ffmpeg -i bear.ogv -f
//! rawvideo -pix_fmt yuv420p`'s own decode of the whole file, sliced to the
//! matching frame index (`ffmpeg` decodes every frame including the
//! P-frames this crate cannot, but a keyframe's own reconstruction never
//! depends on any frame after it, so slicing out frame N from `ffmpeg`'s
//! full decode is exactly `ffmpeg`'s decode of that keyframe).
//!
//! # Result
//!
//! Byte-exact on every plane (Y, U, V separately — see the module doc
//! rationale below) at all three keyframes. A second real file
//! (`ffmpeg`'s FATE suite `ogg/empty_theora_packets.ogv`, 320x240,
//! genuinely different encoder — native `libtheora`, not `ffmpeg`'s own —
//! and a stream that is otherwise almost entirely empty/repeat packets)
//! gave the same result at its own 9 real keyframes; not checked in as a
//! fixture (this test's point is proven with one), but reproducible from
//! the same FATE suite URL.
//!
//! Two real, structural bugs were found and fixed getting to that result,
//! neither of them in the DCT/IDCT/entropy pipeline itself (which is why
//! the luma plane was already byte-exact before either fix — see git
//! history for the fixes' own commit messages for the full account):
//!
//! - `vaco-demux-ogg` never packed Theora's comment and setup header
//!   packets into `Stream::params.extradata` at all — only Vorbis's branch
//!   of `classify_and_emit` did that. Every real Ogg/Theora file was
//!   therefore undecodable through this demuxer before that fix, a
//!   container-side gap, not a bitstream-decode one.
//! - This crate's own `decoder.rs` computed the chroma picture-region crop
//!   by calling `PixelFormat::chroma_blocks(1, 1)`, which returns `(1, 1)`
//!   unchanged for 4:2:0 (that function operates in macro-block units,
//!   where the chroma:luma block-count ratio is always 1:1 — the 2x pixel
//!   subsampling lives in the fixed 8-vs-16-pixels-per-macro-block-edge
//!   convention applied elsewhere). The bug used the coded frame's full
//!   chroma height instead of the correctly-halved one when cropping to
//!   the picture region, which planted the wrong source rows into the
//!   bottom of every chroma plane — invisible on a luma-only or
//!   aggregate-PSNR comparison, exactly the shape of bug this project's
//!   own VP8 encoder work already flagged as a real failure mode. See
//!   `PixelFormat::chroma_subsample`'s doc for the fix.
//!
//! Also found (via reconstructing the loop filter limit table's decode
//! procedure, section 6.4.1 of the spec, which is missing from the
//! published PDF — see `setup.rs`'s module doc): the initial by-analogy
//! guess that its 3-bit prefix followed AC/DC scale's read-plus-one
//! convention was wrong; it is used directly. Wrong by construction, this
//! desynchronised the entire rest of the setup header immediately and was
//! caught the moment a real file was decoded rather than a
//! self-consistent synthetic one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    reason = "test code over a fixed fixture; WIDTH/HEIGHT are always even (Theora's own frame-size constraint), so these divisions are exact"
)]

use vaco_codec_core::{CodecId, Decoder};
use vaco_codec_theora::TheoraDecoder;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_frame::FrameData;
use vaco_io::MemorySource;
use vaco_limits::Limits;

const WIDTH: usize = 320;
const HEIGHT: usize = 180;
const Y_SIZE: usize = WIDTH * HEIGHT;
const C_SIZE: usize = (WIDTH / 2) * (HEIGHT / 2);

/// Decode every keyframe in `fixtures/bear.ogv`, keyed by packet index.
fn decode_keyframes() -> Vec<(usize, Vec<u8>, Vec<u8>, Vec<u8>)> {
    let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bear.ogv"))
        .expect("reads fixture");
    let mut d = vaco_demux_ogg::OggDemuxer::open(
        Box::new(MemorySource::new(bytes)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("opens");

    let video_idx = d
        .streams()
        .iter()
        .find(|s| s.params.codec_id == Some(CodecId::Theora))
        .map(|s| s.index)
        .expect("has a theora stream");
    let extradata = d
        .streams()
        .iter()
        .find(|s| s.index == video_idx)
        .and_then(|s| s.params.extradata.clone())
        .expect("has extradata");

    let mut dec = TheoraDecoder::new(Limits::permissive());
    dec.set_extradata(&extradata).expect("valid setup header");

    let mut out = Vec::new();
    let mut packet_no = 0usize;
    while let Ok(packet) = d.read_packet() {
        if packet.stream_index != video_idx {
            continue;
        }
        let this_packet = packet_no;
        packet_no += 1;
        if dec.send_packet(Some(&packet)).is_err() {
            continue; // delta frame; out of scope for this crate
        }
        let Ok(frame) = dec.receive_frame() else {
            continue;
        };
        let FrameData::Video { .. } = &frame.data else {
            continue;
        };
        let mut y = Vec::new();
        let mut u = Vec::new();
        let mut v = Vec::new();
        for (pi, dst) in [(0, &mut y), (1, &mut u), (2, &mut v)] {
            let plane = frame.plane(pi).expect("plane exists");
            for row in plane.rows_iter() {
                dst.extend_from_slice(row);
            }
        }
        out.push((this_packet, y, u, v));
    }
    out
}

fn reference_frame(packet_index: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let path = format!(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bear_frame{}.yuv"),
        packet_index
    );
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("reads {path}: {e}"));
    let y = data[..Y_SIZE].to_vec();
    let u = data[Y_SIZE..Y_SIZE + C_SIZE].to_vec();
    let v = data[Y_SIZE + C_SIZE..Y_SIZE + 2 * C_SIZE].to_vec();
    (y, u, v)
}

fn assert_plane_exact(name: &str, packet_index: usize, ours: &[u8], reference: &[u8]) {
    assert_eq!(
        ours.len(),
        reference.len(),
        "frame {packet_index} plane {name}: length mismatch"
    );
    let mut max_diff = 0u8;
    let mut diff_count = 0usize;
    for (&a, &b) in ours.iter().zip(reference) {
        let d = a.abs_diff(b);
        max_diff = max_diff.max(d);
        if d != 0 {
            diff_count += 1;
        }
    }
    assert_eq!(
        diff_count, 0,
        "frame {packet_index} plane {name}: {diff_count} differing pixels, max diff {max_diff}"
    );
}

#[test]
fn keyframes_match_ffmpegs_decode_byte_exact_per_plane() {
    let ours = decode_keyframes();
    assert_eq!(
        ours.iter().map(|(idx, ..)| *idx).collect::<Vec<_>>(),
        vec![0, 12, 24],
        "expected exactly the three known keyframes of bear.ogv"
    );
    for (packet_index, y, u, v) in ours {
        let (ry, ru, rv) = reference_frame(packet_index);
        assert_plane_exact("Y", packet_index, &y, &ry);
        assert_plane_exact("U", packet_index, &u, &ru);
        assert_plane_exact("V", packet_index, &v, &rv);
    }
}
