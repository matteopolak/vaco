//! §7.2's reference-frame store (`RefFrameWidth`/`RefFrameHeight`/
//! `FrameStore`) — up to [`tables::NUM_REF_FRAMES`] (8) previously decoded
//! pictures, kept alive by `refresh_frame_flags` and read back by
//! `LAST_FRAME`/`GOLDEN_FRAME`/`ALTREF_FRAME`'s `ref_frame_idx` mapping.
//!
//! Slots are reference-counted (`Arc<Picture>` — `Decoder: Send` rules out
//! `Rc`) rather than copied: the same
//! decoded picture is routinely kept alive in several slots at once (a
//! typical GOP structure refreshes only one or two slots per frame), and a
//! deep per-slot copy on every such refresh would be wasted work for a
//! picture nothing has changed about.

use std::sync::Arc;

use crate::framebuf::Picture;
use crate::tables;

/// One stored reference frame: its pixels plus everything the motion
/// vector scaling process (§8.5.2.3) and block inter-prediction process
/// (§8.5.2.4) need to know about a reference frame that is not necessarily
/// even the same size or subsampling as the frame being decoded.
#[derive(Debug, Clone)]
pub struct RefSlot {
    pub pic: Arc<Picture>,
    pub width: u32,
    pub height: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub bit_depth: u8,
}

/// The 8-slot reference frame store.
#[derive(Debug, Clone, Default)]
pub struct RefFrameStore {
    slots: [Option<RefSlot>; tables::NUM_REF_FRAMES],
}

impl RefFrameStore {
    #[must_use]
    pub fn get(&self, idx: u8) -> Option<&RefSlot> {
        self.slots.get(usize::from(idx)).and_then(Option::as_ref)
    }

    /// §7.2's "for each bit set in `refresh_frame_flags`, that slot's
    /// contents become this frame" — called once per decoded frame
    /// (including a hidden alt-ref) with the freshly-reconstructed
    /// picture, sharing one allocation across every slot the flags select.
    pub fn refresh(&mut self, refresh_frame_flags: u8, slot: &RefSlot) {
        for i in 0..tables::NUM_REF_FRAMES {
            if refresh_frame_flags & (1 << i) != 0
                && let Some(dst) = self.slots.get_mut(i)
            {
                *dst = Some(slot.clone());
            }
        }
    }

    #[must_use]
    pub fn dims(&self, idx: u8) -> Option<(u32, u32)> {
        self.get(idx).map(|s| (s.width, s.height))
    }
}
