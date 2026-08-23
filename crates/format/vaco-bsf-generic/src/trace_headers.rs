//! `trace_headers`: log parsed unit headers, unchanged bitstream.
//!
//! Measured (`ffmpeg 8.1`): `-bsf:v trace_headers` produces byte-identical
//! output to no filter at all on every input tried. Its only effect is
//! diagnostic text on `stderr` — there is no bitstream transform to get
//! right or wrong, so this crate implements the identity half faithfully and
//! does not attempt to reproduce the log line format, which is not an
//! observable this project's conformance harness compares (it checks output
//! bytes and `ffprobe` fields, not a tool's own stderr prose).
//!
//! Restricted to H.264 and HEVC at construction, matching
//! [`crate::filter_units`]: `ffmpeg -h bsf=trace_headers` lists eight
//! supported codecs and this crate only has NAL-unit vocabulary for two of
//! them.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "trace_headers",
    long_name: "Trace headers of NAL units",
    build,
};

struct TraceHeaders;

impl PacketMap for TraceHeaders {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        if let Some(p) = packet {
            out.push_back(p.clone());
        }
        Ok(())
    }
}

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::H264 | CodecId::Hevc) => Ok(Box::new(MappedFilter::new(TraceHeaders))),
        _ => Err(Error::Unsupported(
            "trace_headers: this build only recognises units for h264 and hevc",
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    #[test]
    fn output_is_byte_identical_to_the_input() {
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[0, 0, 0, 1, 0x67, 0x42]).unwrap();
        let params = CodecParameters::video().with_codec(CodecId::H264);
        let mut f = (DESC.build)(&params).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), pkt.payload());
    }

    #[test]
    fn an_unrecognised_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Vp9);
        assert!((DESC.build)(&params).is_err());
    }
}
