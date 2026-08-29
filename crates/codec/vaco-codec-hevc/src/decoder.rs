//! [`HevcDecoder`] — the [`Decoder`] this crate builds, not registered (see
//! the crate doc).
//!
//! # Framing this decoder accepts
//!
//! [`HevcDecoder::send_packet`] walks its packet payload as an Annex-B byte
//! stream (`vaco_bitstream::annexb::nal_units`) — the shape a raw `.hevc`
//! elementary stream or an MPEG-TS PES payload carries, and what a real
//! encoder's own in-band VPS/SPS/PPS-before-every-IDR output looks like. A
//! length-prefixed `hvcC` sample (the common MP4 shape) is **not** handled —
//! [`Decoder::set_extradata`] is accepted but does nothing yet, a stated cut
//! rather than a silent one.
use std::collections::HashMap;

use vaco_bitstream::annexb;
use vaco_codec_cabac::CabacDecoder;
use vaco_codec_core::Decoder;
use vaco_core::{Duration, Error, Result, Timestamp};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_hevc::{ChromaFormat, HevcNalHeader, NalUnitType, Pps, SliceHeader, SliceKind, Sps};

use crate::cabac_ctx::ContextBank;
use crate::ctu::{self, Ctx};
use crate::framebuf::{CuGrid, Picture};

/// The HEVC decoder. See the crate doc and module doc for exactly what is
/// and is not implemented.
pub struct HevcDecoder {
    limits: Limits,
    machine: vaco_codec_core::machine::Machine<Frame>,
    sps: HashMap<u8, Sps>,
    pps: HashMap<u8, Pps>,
}

impl std::fmt::Debug for HevcDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HevcDecoder").finish_non_exhaustive()
    }
}

impl HevcDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            machine: vaco_codec_core::machine::Machine::with_capacity(vaco_codec_core::Caps::empty(), 1),
            sps: HashMap::new(),
            pps: HashMap::new(),
        }
    }

    fn handle_nal(&mut self, budget: &mut Budget, ebsp: &[u8], pts: Timestamp, duration: Duration) -> Result<()> {
        let Some(header) = HevcNalHeader::parse(ebsp) else { return Ok(()) };
        if header.nuh_layer_id != 0 {
            return Ok(()); // base layer only.
        }
        let mut scratch = Vec::new();
        let rbsp = annexb::to_rbsp(ebsp, &mut scratch);

        match header.nal_unit_type {
            NalUnitType::SPS_NUT => {
                let sps = Sps::parse(rbsp, budget)?;
                self.sps.insert(sps.id, sps);
            }
            NalUnitType::PPS_NUT => {
                let pps = Pps::parse(rbsp, budget)?;
                self.pps.insert(pps.id, pps);
            }
            t if t.has_slice_header() => {
                let frame = self.decode_slice(budget, header, rbsp, pts, duration)?;
                self.machine.emit(frame);
            }
            _ => {}
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, reason = "one linear scope-check-then-decode sequence")]
    fn decode_slice(
        &mut self,
        budget: &mut Budget,
        header: HevcNalHeader,
        rbsp: &[u8],
        pts: Timestamp,
        duration: Duration,
    ) -> Result<Frame> {
        let pps_id = vaco_parse_hevc::slice::peek_pps_id(rbsp)
            .ok_or(Error::InvalidData("vaco-codec-hevc: slice segment header truncated before pps_id"))?;
        let pps = self
            .pps
            .get(&pps_id)
            .cloned()
            .ok_or(Error::Unsupported("vaco-codec-hevc: referenced PPS not seen yet"))?;
        let sps = self
            .sps
            .get(&pps.sps_id)
            .cloned()
            .ok_or(Error::Unsupported("vaco-codec-hevc: referenced SPS not seen yet"))?;

        check_scope(&sps, &pps)?;

        let mut reader = vaco_bitstream::BitReader::new(rbsp);
        reader.skip(16);
        let hdr = SliceHeader::parse_data(&mut reader, header, &sps, &pps, budget)?;
        reader.check()?;

        if hdr.kind != SliceKind::I {
            return Err(Error::Unsupported("vaco-codec-hevc: only I-slices are decoded"));
        }
        if hdr.dependent {
            return Err(Error::Unsupported("vaco-codec-hevc: dependent slice segments are not supported"));
        }
        if !hdr.first_slice_segment_in_pic {
            return Err(Error::Unsupported("vaco-codec-hevc: multiple slice segments per picture are not supported"));
        }

        reader.align();
        let cabac_data = reader.remaining_bytes();

        let slice_qp = 26 + pps.init_qp_minus26 + hdr.qp_delta;

        let width = usize::try_from(sps.pic_width_in_luma_samples).unwrap_or(0);
        let height = usize::try_from(sps.pic_height_in_luma_samples).unwrap_or(0);
        let mut pic = Picture::new(budget, width, height)?;
        let cu_grid = CuGrid::new(budget, width, height)?;
        let mut walk = Ctx::new(&mut pic, cu_grid, &sps, &pps, slice_qp);

        let mut cabac = CabacDecoder::new(cabac_data);
        let mut ctx = ContextBank::new(i8::try_from(slice_qp.clamp(0, 51)).unwrap_or(0));

        let ctb_size = 1u32 << walk.log2_ctb_size;
        let pic_width_u32 = u32::try_from(walk.pic_width).unwrap_or(0);
        let pic_height_u32 = u32::try_from(walk.pic_height).unwrap_or(0);
        let ctbs_x = pic_width_u32.div_ceil(ctb_size).max(1);
        let ctbs_y = pic_height_u32.div_ceil(ctb_size).max(1);
        let total_ctbs = ctbs_x.saturating_mul(ctbs_y);
        let ctb_size_i = i32::try_from(ctb_size).unwrap_or(0);

        for addr in 0..total_ctbs {
            let col = addr.checked_rem(ctbs_x).unwrap_or(0);
            let row = addr.checked_div(ctbs_x).unwrap_or(0);
            let cx = i32::try_from(col).unwrap_or(0) * ctb_size_i;
            let cy = i32::try_from(row).unwrap_or(0) * ctb_size_i;
            ctu::decode_ctu(&mut cabac, &mut ctx, &mut walk, cx, cy)?;
            let end = cabac.decode_terminate();
            if cabac.malformed() {
                return Err(Error::InvalidData("vaco-codec-hevc: CABAC decode ran past the slice segment data"));
            }
            if end != 0 {
                break;
            }
        }

        let mut frame = pic_to_frame(budget, &sps, &pic)?;
        frame.pts = pts;
        frame.duration = duration;
        frame.flags |= vaco_frame::FrameFlags::KEY;
        Ok(frame)
    }
}

/// Refuse, up front, every combination this crate does not implement — see
/// the crate doc for the complete, stated list.
fn check_scope(sps: &Sps, pps: &Pps) -> Result<()> {
    let unsupported = |why: &'static str| Err(Error::Unsupported(why));
    if sps.chroma_format != ChromaFormat::Yuv420 {
        return unsupported("vaco-codec-hevc: only 4:2:0 chroma is decoded");
    }
    if sps.bit_depth_luma != 8 || sps.bit_depth_chroma != 8 {
        return unsupported("vaco-codec-hevc: only 8-bit samples are decoded");
    }
    if sps.separate_colour_plane {
        return unsupported("vaco-codec-hevc: separate_colour_plane_flag is not supported");
    }
    if sps.scaling_list_enabled {
        return unsupported("vaco-codec-hevc: custom scaling lists are not supported (flat scaling only)");
    }
    if sps.pcm.is_some() {
        return unsupported("vaco-codec-hevc: I_PCM is not supported");
    }
    if sps.range_extension.is_some() {
        return unsupported("vaco-codec-hevc: SPS range-extension flags are not supported");
    }
    if sps.scc_extension.is_some() {
        return unsupported("vaco-codec-hevc: screen-content-coding extensions are not supported");
    }
    if pps.tiles.is_some() {
        return unsupported("vaco-codec-hevc: tiles are not supported");
    }
    if pps.entropy_coding_sync_enabled {
        return unsupported("vaco-codec-hevc: wavefront parallel processing is not supported");
    }
    if pps.cu_qp_delta_enabled {
        return unsupported("vaco-codec-hevc: per-CU QP delta is not supported (constant slice QP only)");
    }
    if pps.transquant_bypass_enabled {
        return unsupported("vaco-codec-hevc: transquant_bypass is not supported");
    }
    if pps.range_extension.is_some() {
        return unsupported("vaco-codec-hevc: PPS range-extension flags are not supported");
    }
    if pps.scc_extension.is_some() {
        return unsupported("vaco-codec-hevc: screen-content-coding extensions are not supported");
    }
    Ok(())
}

fn pic_to_frame(budget: &mut Budget, sps: &Sps, pic: &Picture) -> Result<Frame> {
    let pix_fmt = vaco_pixfmt::PixFmt::from_name("yuv420p")
        .map_err(|_| Error::InvalidData("vaco-codec-hevc: yuv420p pixel format missing"))?;
    let (width, height) = sps.dimensions().unwrap_or((sps.pic_width_in_luma_samples, sps.pic_height_in_luma_samples));
    let mut frame = Frame::alloc_video(budget, pix_fmt, width, height)?;
    blit(&pic.y, &mut frame, 0, width as usize, height as usize);
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    blit(&pic.cb, &mut frame, 1, cw, ch);
    blit(&pic.cr, &mut frame, 2, cw, ch);
    Ok(frame)
}

fn blit(src: &crate::framebuf::Plane, frame: &mut Frame, plane_index: usize, width: usize, height: usize) {
    let Some(mut dst) = frame.plane_mut(plane_index) else { return };
    for y in 0..height.min(dst.rows()) {
        let Some(row) = dst.row_mut(y) else { continue };
        for x in 0..width.min(row.len()) {
            let v = src.get(x, y);
            if let Some(b) = row.get_mut(x) {
                *b = u8::try_from(v).unwrap_or(0);
            }
        }
    }
}

impl Decoder for HevcDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match self.machine.accept(packet.is_none())? {
            vaco_codec_core::machine::Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            vaco_codec_core::machine::Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                let mut budget = Budget::new(self.limits.clone());
                for ebsp in annexb::nal_units(pkt.payload()) {
                    self.handle_nal(&mut budget, ebsp, pkt.pts, pkt.duration)?;
                }
                Ok(())
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }

    fn set_extradata(&mut self, _extradata: &[u8]) -> Result<()> {
        // `hvcC` (length-prefixed sample framing) is not implemented — see
        // the module doc. Accepted rather than refused so a caller that
        // always calls this (the common shape) is not penalised for a
        // stream this decoder can still handle in-band.
        Ok(())
    }
}
