//! `ashowinfo` — show textual information for each audio frame.
//!
//! `ffmpeg -h filter=ashowinfo` (2026-08-23): no options at all. What it
//! prints is the interface (`ffmpeg -f lavfi -i "sine=..." -af ashowinfo -f
//! null -`, 2026-08-23):
//!
//! ```text
//! n:0 pts:0 pts_time:0 fmt:s16 channels:1 chlayout:mono rate:8000
//! nb_samples:400 checksum:700D2B79 plane_checksums: [ 700D2B79 ]
//! ```
//!
//! Field *names* are reproduced (D7: CLI/interface names, not expression).
//! The *checksum algorithm* is not: the reference's is unmeasured (no public
//! spec, and D7 forbids reading its source to find out), so this crate logs
//! an FNV-1a hash of each plane's raw bytes instead — same field name and
//! shape, a different number. A documented gap, not a guess dressed up as a
//! match.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "ashowinfo",
    description: "show textual information for each audio frame",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// FNV-1a, 32-bit: a small, dependency-free, well-defined hash. Not a claim
/// about what the reference computes — see the module doc.
fn fnv1a(bytes: &[u8]) -> u32 {
    const OFFSET: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    bytes.iter().fold(OFFSET, |h, &b| (h ^ u32::from(b)).wrapping_mul(PRIME))
}

#[derive(Debug, Clone, Default)]
struct ShowInfo {
    index: u64,
    sample_rate: u32,
    time_base: vaco_core::Rational,
}

impl FrameFilter for ShowInfo {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            time_base,
            ..
        }) = ctx.input_link(0)
        {
            self.sample_rate = *sample_rate;
            self.time_base = *time_base;
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, samples, layout, _channels) = crate::sample::decode(&input)?;
        let pts = input.pts.ticks().unwrap_or(0);
        let pts_time = input.pts.to_seconds(self.time_base).unwrap_or(0.0);

        let mut checksums = Vec::new();
        let mut overall = 0x811c_9dc5u32;
        for i in 0..input.plane_count() {
            let Some(p) = input.plane(i) else { break };
            let bytes = p.as_slice();
            let cs = fnv1a(bytes);
            checksums.push(cs);
            // Combine plane checksums into one "overall" value: XOR-fold, the
            // simplest order-sensitive-enough combiner for a diagnostic
            // field nothing downstream parses.
            overall ^= cs.rotate_left((i as u32 % 31) + 1);
        }
        let checksum_list = checksums
            .iter()
            .map(|c| format!("{c:08X}"))
            .collect::<Vec<_>>()
            .join(" ");

        tracing::info!(
            target: "vaco_filter_ameasure::ashowinfo",
            "n:{} pts:{} pts_time:{:.6} fmt:{} channels:{} chlayout:{} rate:{} \
             nb_samples:{} checksum:{:08X} plane_checksums: [ {} ]",
            self.index,
            pts,
            pts_time,
            fmt,
            layout.channels,
            layout.describe(),
            rate.max(self.sample_rate),
            samples,
            overall,
            checksum_list,
        );
        self.index += 1;
        Ok(FrameOut::One(input))
    }

    fn flush_state(&mut self) {
        self.index = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(ShowInfo::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::fnv1a;

    #[test]
    fn fnv1a_is_deterministic_and_order_sensitive() {
        assert_eq!(fnv1a(b"abc"), fnv1a(b"abc"));
        assert_ne!(fnv1a(b"abc"), fnv1a(b"acb"));
        // Known FNV-1a-32 test vector for the empty string.
        assert_eq!(fnv1a(b""), 0x811c_9dc5);
    }
}
