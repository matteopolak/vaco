#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
//! Property: for any block size/frame ratio and any input length,
//! [`BlockDemuxer`] emits packets whose payload bytes concatenate back to
//! exactly the whole-block-aligned prefix of the input, and whose `pts`
//! values are strictly increasing.

use proptest::prelude::*;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational};
use vaco_format_core::DemuxerDesc;
use vaco_format_core::discovery::NoParsers;
use vaco_format_misc_audio::block::{BlockDemuxer, DEFAULT_TARGET_PACKET_BYTES};
use vaco_format_misc_audio::{
    adx, amr, g723, nistsphere, pvf, rawcodec, sbc, svag, tta, vag, wavpack, xa, xwma,
};
use vaco_io::{IoContext, IoOptions, MemorySource};
use vaco_limits::{Budget, Limits};

fn all_descs() -> Vec<DemuxerDesc> {
    vec![
        wavpack::DEMUXER,
        tta::DEMUXER,
        amr::DEMUXER_AMR,
        amr::DEMUXER_AMRNB,
        amr::DEMUXER_AMRWB,
        adx::DEMUXER,
        nistsphere::DEMUXER,
        pvf::DEMUXER,
        g723::DEMUXER,
        sbc::DEMUXER,
        rawcodec::DEMUXER_GSM,
        rawcodec::DEMUXER_SLN,
        rawcodec::DEMUXER_DFPWM,
        rawcodec::DEMUXER_G722,
        rawcodec::DEMUXER_G726,
        rawcodec::DEMUXER_G726LE,
        rawcodec::DEMUXER_G728,
        rawcodec::DEMUXER_G729,
        rawcodec::DEMUXER_APTX,
        rawcodec::DEMUXER_APTX_HD,
        svag::DEMUXER,
        vag::DEMUXER,
        xa::DEMUXER,
        xwma::DEMUXER,
    ]
}

fn build(data: Vec<u8>, bytes_per_block: u32, frames_per_block: u32) -> BlockDemuxer {
    let len = data.len() as u64;
    let src = Box::new(MemorySource::new(data));
    let io = IoContext::new(src, &IoOptions::default()).unwrap();
    let mut stream = vaco_format_core::Stream::new(0, MediaType::Audio, Rational::new(1, 8000));
    stream.params = CodecParameters::audio();
    if let Some(a) = stream.params.audio.as_mut() {
        a.sample_rate = 8000;
    }
    BlockDemuxer::new(
        io,
        stream,
        0,
        Some(len),
        bytes_per_block,
        frames_per_block,
        DEFAULT_TARGET_PACKET_BYTES,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn packets_reconstruct_the_whole_block_aligned_prefix(
        data in proptest::collection::vec(any::<u8>(), 0..2000),
        bytes_per_block in 1u32..40,
        frames_per_block in 1u32..40,
    ) {
        let total_len = data.len();
        let mut d = build(data.clone(), bytes_per_block, frames_per_block);
        let mut budget = Budget::new(Limits::permissive());
        let mut reconstructed = Vec::new();
        let mut last_pts = -1i64;
        loop {
            match d.read_packet(&mut budget) {
                Ok(pkt) => {
                    let pts = pkt.pts.ticks().unwrap_or(0);
                    prop_assert!(pts > last_pts);
                    last_pts = pts;
                    reconstructed.extend_from_slice(pkt.payload());
                }
                Err(Error::Eof) => break,
                Err(e) => prop_assert!(false, "unexpected error {e:?}"),
            }
        }
        #[allow(
            clippy::integer_division,
            reason = "computing the expected whole-block-aligned prefix length for the property check"
        )]
        let whole_blocks_len = (total_len / bytes_per_block as usize) * bytes_per_block as usize;
        prop_assert_eq!(reconstructed.len(), whole_blocks_len);
        prop_assert_eq!(&reconstructed[..], &data[..whole_blocks_len]);
    }

    /// A stand-in for `fuzz/fuzz_targets/misc_audio_demux.rs`'s property,
    /// runnable without `cargo fuzz`: no registered demuxer panics or fails
    /// to terminate on arbitrary bytes. Written because the shared `fuzz`
    /// workspace would not build at the time this crate landed (an
    /// unrelated crate's in-progress edit), so this is the check that could
    /// actually run in this session.
    #[test]
    fn no_demuxer_panics_or_loops_on_arbitrary_bytes(
        data in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        const MAX_PACKETS: u32 = 2000;
        for desc in all_descs() {
            let src = Box::new(MemorySource::new(data.clone()));
            let Ok(mut demux) = (desc.open)(src, &NoParsers) else {
                continue;
            };
            let mut n = 0u32;
            while demux.read_packet().is_ok() {
                n += 1;
                prop_assert!(n < MAX_PACKETS, "{}: read did not terminate", desc.name);
            }
        }
    }
}
