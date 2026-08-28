//! `showinfo`: log per-packet diagnostics, unchanged bitstream.
//!
//! Measured (`ffmpeg 8.1`, `-bsf:v showinfo` against a real `libx264`
//! elementary stream): output is byte-identical to no filter at all. Its
//! whole effect is one diagnostic line per packet on `stderr` (size, pts/dts,
//! duration, an Adler-32 of the payload) — there is no bitstream transform to
//! get right or wrong, and this project's conformance harness compares output
//! bytes and `ffprobe` fields, never a tool's own stderr prose (see
//! `crate::trace_headers`'s docs, which make the identical call for the
//! identical reason).
//!
//! Unlike `trace_headers`, `ffmpeg -h bsf=showinfo` names no `Supported
//! codecs:` line at all — it takes any packet stream — so this filter is not
//! restricted at construction.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "showinfo",
    long_name: "Show textual information for each packet",
    build,
};

struct ShowInfo;

impl PacketMap for ShowInfo {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        if let Some(p) = packet {
            out.push_back(p.clone());
        }
        Ok(())
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(_params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    Ok(Box::new(MappedFilter::new(ShowInfo)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    #[test]
    fn output_is_byte_identical_to_the_input() {
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, b"anything at all").unwrap();
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"anything at all");
    }

    #[test]
    fn any_codec_is_accepted() {
        for codec in [
            vaco_codec_core::CodecId::H264,
            vaco_codec_core::CodecId::Av1,
            vaco_codec_core::CodecId::Aac,
        ] {
            let params = CodecParameters::video().with_codec(codec);
            assert!((DESC.build)(&params).is_ok());
        }
    }
}
