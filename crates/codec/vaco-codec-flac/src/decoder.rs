//! FLAC decode via the `claxon` crate.
//!
//! One packet in, at most one frame out, decoded synchronously — there is
//! no reorder buffer and nothing is ever delayed past the packet that
//! produced it, so a plain [`std::collections::VecDeque`] is enough (the
//! same shape `vaco-codec-mpegaudio`'s decoder uses, rather than
//! [`vaco_codec_core::machine::Machine`]).

use std::collections::VecDeque;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

use crate::claxon_boundary::decode_packet;
use crate::streaminfo::{find_streaminfo_block, guess_from_frame_header, to_block_bytes};

/// A [`vaco_codec_core::Decoder`] over [`Packet`]/[`Frame`]: FLAC decode
/// via the `claxon` crate (the D11 boundary is `crate::claxon_boundary`,
/// the only file that names it).
#[derive(Debug)]
pub struct FlacDecoder {
    limits: Limits,
    pending: VecDeque<Frame>,
    /// The 34-byte `STREAMINFO` payload, once known — from
    /// [`FlacDecoder::set_extradata`], or synthesized from the first
    /// packet's own frame header if extradata never arrives.
    streaminfo: Option<[u8; 34]>,
}

impl FlacDecoder {
    /// A decoder bounding its output frames by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: VecDeque::new(),
            streaminfo: None,
        }
    }
}

impl Decoder for FlacDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else { return Ok(()) };
        let payload = packet.payload();
        if payload.is_empty() {
            return Ok(());
        }

        let streaminfo_block = if let Some(block) = self.streaminfo {
            block
        } else {
            let (sample_rate, channels, bits_per_sample) =
                guess_from_frame_header(payload).unwrap_or((44_100, 2, 16));
            to_block_bytes(sample_rate, channels, bits_per_sample, u16::MAX)
        };

        let decoded = decode_packet(&streaminfo_block, payload)?;
        if decoded.channels == 0 {
            return Err(Error::InvalidData("flac: stream declares zero channels"));
        }
        let per_channel = decoded
            .interleaved
            .len()
            .checked_div(decoded.channels as usize)
            .unwrap_or(0);
        if per_channel == 0 {
            return Ok(());
        }

        let layout = ChannelLayout::default_for(decoded.channels)
            .ok_or(Error::Unsupported("flac: unsupported channel count"))?;
        let format = if decoded.bits_per_sample <= 16 {
            SampleFmt::S16P
        } else {
            SampleFmt::S32P
        };

        let mut budget = Budget::new(self.limits.clone());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            format,
            layout,
            per_channel as u32,
            decoded.sample_rate,
        )?;
        write_channels(
            &mut frame,
            &decoded.interleaved,
            decoded.channels as usize,
            format,
        );
        self.pending.push_back(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(Error::NeedMoreInput)
    }

    fn flush(&mut self) {
        self.pending.clear();
    }

    /// Seed the decoder from the container's `STREAMINFO`.
    ///
    /// Different containers wrap the same 34-byte block differently
    /// (Ogg-FLAC's marker packet, MP4's `dfLa` box, Matroska's native
    /// `CodecPrivate`), so this scans `extradata` for the block by
    /// structure rather than assuming any one envelope — see
    /// [`crate::streaminfo::find_streaminfo_block`]. When nothing is
    /// found, this fails soft: no error, and [`FlacDecoder::send_packet`]
    /// falls back to guessing from the first frame header instead.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if let Some(block) = find_streaminfo_block(extradata) {
            self.streaminfo = Some(block);
        }
        Ok(())
    }
}

/// De-interleave `interleaved` (channel-major: `ch0, ch1, ch0, ch1, ...`)
/// into `frame`'s per-channel planes, encoding each `i32` sample as
/// native-endian bytes at `format`'s width.
fn write_channels(frame: &mut Frame, interleaved: &[i32], channels: usize, format: SampleFmt) {
    let bytes_per_sample = format.bytes_per_sample();
    for (ch, mut plane) in frame.planes_mut().into_iter().enumerate() {
        let Some(row) = plane.row_mut(0) else {
            continue;
        };
        let mut sample_index = 0usize;
        while let Some(src_index) = sample_index
            .checked_mul(channels)
            .and_then(|v| v.checked_add(ch))
        {
            let Some(&value) = interleaved.get(src_index) else {
                break;
            };
            let Some(dst_start) = sample_index.checked_mul(bytes_per_sample) else {
                break;
            };
            let Some(dst_end) = dst_start.checked_add(bytes_per_sample) else {
                break;
            };
            let Some(dst) = row.get_mut(dst_start..dst_end) else {
                break;
            };
            match format {
                SampleFmt::S16P => {
                    let clamped = value.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                    dst.copy_from_slice(&(clamped as i16).to_ne_bytes());
                }
                SampleFmt::S32P => dst.copy_from_slice(&value.to_ne_bytes()),
                _ => {}
            }
            sample_index += 1;
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_wrap,
    reason = "test code"
)]
mod tests {
    use super::FlacDecoder;
    use crate::encoder::FlacEncoder;
    use vaco_chlayout::ChannelLayout;
    use vaco_codec_core::{Decoder, Encoder};
    use vaco_frame::{Frame, FrameData};
    use vaco_limits::{Budget, Limits};
    use vaco_sampfmt::SampleFmt;

    #[test]
    fn decodes_a_packet_this_crate_encoded() {
        let limits = Limits::permissive();
        let mut budget = Budget::new(limits.clone());
        let layout = ChannelLayout::STEREO;
        let n = 200u32;
        let mut frame = Frame::alloc_audio(&mut budget, SampleFmt::S16P, layout, n, 44_100)
            .expect("alloc audio frame");
        {
            let mut planes = frame.planes_mut();
            for (ch, plane) in planes.iter_mut().enumerate() {
                let row = plane.row_mut(0).expect("row 0");
                for i in 0..n as usize {
                    let v = ((i as i32) * (ch as i32 + 1)) as i16;
                    if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                        dst.copy_from_slice(&v.to_ne_bytes());
                    }
                }
            }
        }

        let mut enc = FlacEncoder::new(limits.clone());
        enc.send_frame(Some(&frame)).expect("send");
        enc.send_frame(None).expect("drain start");
        let packet = enc.receive_packet().expect("packet");
        let extradata = enc.extradata();

        let mut dec = FlacDecoder::new(limits);
        dec.set_extradata(&extradata).expect("set extradata");
        dec.send_packet(Some(&packet)).expect("send packet");
        let out = dec.receive_frame().expect("frame");

        let FrameData::Audio {
            samples,
            layout: out_layout,
            ..
        } = out.data
        else {
            panic!("expected an audio frame");
        };
        assert_eq!(samples, n);
        assert_eq!(out_layout.channels, 2);
    }
}
