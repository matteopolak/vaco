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

/// One reference-frame slot, before its pixels necessarily exist (issue
/// #328). A handle rather than an `Arc<Picture>`: `refresh` below has to run
/// the instant a frame's header is parsed — before that frame's own tile
/// decode and reconstruction happen, possibly on a worker thread, possibly
/// seconds later — so the *next* frame's own header parse can find out what
/// `LAST_FRAME`/`GOLDEN_FRAME`/`ALTREF_FRAME` mean without waiting for this
/// frame's pixels. [`vaco_codec_core::picture::PictureRef`] is exactly a
/// handle to a picture that may still be in production, and cloning one is
/// a refcount bump — the same trade [`RefSlot`]'s own doc explains for
/// `Arc<Picture>`, one level earlier.
#[derive(Debug, Clone)]
pub struct PendingRefSlot {
    pub pic_ref: vaco_codec_core::picture::PictureRef,
    pub width: u32,
    pub height: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub bit_depth: u8,
}

/// The persistent, handle-based mirror of [`RefFrameStore`] a
/// frame-threaded [`crate::decode::Vp9Decoder`] keeps across packets.
/// [`materialize_ref_store`] turns one of these into a real
/// `RefFrameStore` for a frame task that actually needs to read pixels.
#[derive(Debug, Clone, Default)]
pub struct PendingRefStore {
    slots: [Option<PendingRefSlot>; tables::NUM_REF_FRAMES],
}

impl PendingRefStore {
    #[must_use]
    pub fn get(&self, idx: u8) -> Option<&PendingRefSlot> {
        self.slots.get(usize::from(idx)).and_then(Option::as_ref)
    }

    /// Same rule as [`RefFrameStore::refresh`], on handles instead of
    /// pixels.
    pub fn refresh(&mut self, refresh_frame_flags: u8, slot: &PendingRefSlot) {
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

/// Wait for every populated slot of `pending` to finish and copy its pixels
/// into an owned [`Picture`], producing the plain, `Arc<Picture>`-backed
/// [`RefFrameStore`] every existing reconstruction call
/// (`crate::decode::predict_inter_region`/`ref_slot_for`) already reads.
///
/// Several slots routinely alias the same underlying picture (a typical GOP
/// structure refreshes only one or two of the three `ref_frame_idx` targets
/// per frame) — `waiter_decode_index` doubles as the dedup key via each
/// `PictureRef::decode_index`, so that picture's bytes are copied once, not
/// once per slot that happens to point at it.
///
/// # Errors
///
/// Whatever [`vaco_codec_core::picture::PictureRef::wait_rows_for`] reports:
/// the producing task failed, or (checked in debug builds) this waits on a
/// picture that is not earlier in decode order.
pub fn materialize_ref_store(
    pending: &PendingRefStore,
    waiter_decode_index: u64,
    budget: &mut vaco_limits::Budget,
) -> vaco_core::Result<RefFrameStore> {
    let mut cache: std::collections::HashMap<u64, Arc<Picture>> = std::collections::HashMap::new();
    let mut out = RefFrameStore::default();
    for i in 0..tables::NUM_REF_FRAMES {
        let Some(p) = pending.get(u8::try_from(i).unwrap_or(0)) else { continue };
        let pic = if let Some(cached) = cache.get(&p.pic_ref.decode_index()) {
            cached.clone()
        } else {
            let luma_w = usize::try_from(p.width).unwrap_or(0);
            let luma_h = usize::try_from(p.height).unwrap_or(0);
            let chroma_w = luma_w >> u32::from(p.subsampling_x);
            let chroma_h = luma_h >> u32::from(p.subsampling_y);
            let pic = Arc::new(crate::framebuf::materialize(&p.pic_ref, waiter_decode_index, luma_w, luma_h, chroma_w, chroma_h, budget)?);
            cache.insert(p.pic_ref.decode_index(), pic.clone());
            pic
        };
        if let Some(dst) = out.slots.get_mut(i) {
            *dst = Some(RefSlot { pic, width: p.width, height: p.height, subsampling_x: p.subsampling_x, subsampling_y: p.subsampling_y, bit_depth: p.bit_depth });
        }
    }
    Ok(out)
}
