//! The D11 boundary: the only file in this crate that names `claxon`.
//!
//! `claxon::FlacReader` wants a complete, well-formed native FLAC stream —
//! a `"fLaC"` marker, a `STREAMINFO` metadata block, then frames — but a
//! [`vaco_codec_core::Decoder`] is handed one already-demuxed packet at a
//! time (typically one FLAC frame; Matroska lacing can pack more than one).
//! The adapter here bridges that: wrap the packet's payload behind a
//! synthetic single-block header and hand the whole thing to a *fresh*
//! `FlacReader` per packet. Constructing one is cheap (a 38-byte header
//! parse), and driving its `.samples()` iterator to exhaustion decodes
//! every frame the payload actually contains, which is what makes the
//! multi-frame-per-packet case fall out for free rather than needing its
//! own loop here.
//!
//! If `claxon` is ever swapped for a native decoder, this is the one file
//! that changes.

use std::io::Cursor;

use vaco_core::{Error, Result};

use crate::streaminfo::wrap_as_last_metadata_block;

/// One packet's worth of decoded audio: interleaved samples (channel-major,
/// i.e. `ch0, ch1, ch0, ch1, ...` for stereo) plus the stream properties
/// Claxon read out of the synthetic `STREAMINFO` it was handed.
#[derive(Debug)]
pub struct Decoded {
    pub interleaved: Vec<i32>,
    pub channels: u32,
    pub bits_per_sample: u32,
    pub sample_rate: u32,
}

/// Decode every sample in `payload` (one or more concatenated, complete
/// FLAC frames), given the 34-byte `STREAMINFO` payload that describes the
/// stream.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed frame, [`Error::Unsupported`] for
/// a spec feature Claxon 0.4.3 does not implement (LPC with a negative
/// shift, or an escaped/unencoded Rice partition — see `crate::rice`'s
/// module doc for why this crate's own encoder never produces the
/// latter), and [`Error::UnexpectedEof`] for a truncated payload.
pub fn decode_packet(streaminfo_block: &[u8; 34], payload: &[u8]) -> Result<Decoded> {
    let mut file = Vec::new();
    file.extend_from_slice(b"fLaC");
    file.extend_from_slice(&wrap_as_last_metadata_block(streaminfo_block));
    file.extend_from_slice(payload);

    let cursor = Cursor::new(file);
    let mut reader = claxon::FlacReader::new(cursor).map_err(|e| map_err(&e))?;
    let info = reader.streaminfo();

    let mut interleaved = Vec::new();
    for sample in reader.samples() {
        interleaved.push(sample.map_err(|e| map_err(&e))?);
    }

    Ok(Decoded {
        interleaved,
        channels: info.channels,
        bits_per_sample: info.bits_per_sample,
        sample_rate: info.sample_rate,
    })
}

fn map_err(e: &claxon::Error) -> Error {
    match e {
        claxon::Error::IoError(_) => Error::UnexpectedEof,
        claxon::Error::FormatError(msg) => Error::InvalidData(msg),
        claxon::Error::Unsupported(msg) => Error::Unsupported(msg),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::decode_packet;
    use crate::encoder::FlacEncoder;
    use crate::streaminfo::find_streaminfo_block;
    use vaco_chlayout::ChannelLayout;
    use vaco_codec_core::Encoder;
    use vaco_frame::Frame;
    use vaco_limits::{Budget, Limits};
    use vaco_sampfmt::SampleFmt;

    #[test]
    fn decodes_one_frame_encoded_by_this_crate() {
        let limits = Limits::permissive();
        let mut budget = Budget::new(limits.clone());
        let layout = ChannelLayout::MONO;
        let samples: Vec<i16> = (0..64).map(|i| (i * 37) as i16).collect();
        let mut frame = Frame::alloc_audio(&mut budget, SampleFmt::S16P, layout, 64, 8_000)
            .expect("alloc audio frame");
        {
            let mut planes = frame.planes_mut();
            let plane = planes.first_mut().expect("one plane");
            let row = plane.row_mut(0).expect("row 0");
            for (i, &s) in samples.iter().enumerate() {
                if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                    dst.copy_from_slice(&s.to_ne_bytes());
                }
            }
        }

        let mut enc = FlacEncoder::new(limits);
        enc.send_frame(Some(&frame)).expect("send frame");
        enc.send_frame(None).expect("start drain");
        let packet = enc.receive_packet().expect("one packet");

        let extradata = enc.extradata();
        let block = find_streaminfo_block(&extradata).expect("streaminfo present");
        let decoded = decode_packet(&block, packet.payload()).expect("decode");
        assert_eq!(decoded.channels, 1);
        let got: Vec<i32> = decoded.interleaved;
        let want: Vec<i32> = samples.iter().map(|&s| i32::from(s)).collect();
        assert_eq!(got, want);
    }
}
