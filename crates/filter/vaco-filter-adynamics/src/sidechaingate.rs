//! `sidechaingate` — audio sidechain gate. Same relationship to
//! [`crate::agate`] that [`crate::sidechaincompress`] has to
//! [`crate::acompressor`]: same processor and option table
//! (`agate/sidechaingate`, probed 2026-08-23), sidechain-driven envelope.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, Synced};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Dynamics};

pub const DESC: FilterDesc = FilterDesc {
    name: "sidechaingate",
    description: "audio sidechain gate",
    inputs: common::DUAL_AUDIO_PADS,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

struct SidechainGate {
    dynamics: Dynamics,
}

impl FrameSyncFilter for SidechainGate {
    fn on_event(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&main)?;
        let detect = match event.get(1) {
            Some(sc) => crate::sample::decode(sc)?.4,
            None => channels.clone(),
        };
        self.dynamics.process(&mut channels, &detect);
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &channels,
        )?;
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.dynamics.set_sample_rate(f64::from(*sample_rate));
        }
        Ok(())
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = SidechainGate {
        dynamics: crate::agate::build(req),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Audio, req.instance),
        filter: Box::new(Synced::new(filter)),
    }
}
