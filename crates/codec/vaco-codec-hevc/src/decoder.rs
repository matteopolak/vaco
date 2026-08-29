//! [`HevcDecoder`] — the [`Decoder`] this crate registers.
//!
//! # AVCC-equivalent (`hvcC`) vs Annex B
//!
//! MP4/Matroska store length-prefixed samples (`hvcC`); a raw `.hevc`
//! elementary stream or an MPEG-TS PES payload is Annex B. This decoder
//! embeds [`HevcParser`] exactly the way `vaco-codec-h264` embeds
//! `H264Parser` — [`HevcParser::set_extradata`] tells the two framings
//! apart from the `hvcC`/Annex-B extradata shape and remembers which one
//! applies ([`HevcParser::framing`]); [`vaco_format_nalu::units`] (the
//! same low-level, `Framing`-aware NAL-unit iterator both this module and
//! [`HevcParser`] itself are built on) then walks either framing
//! identically. **No `hevc_mp4toannexb` bitstream filter is applied or
//! needed in this path** — that filter's job is muxer-side re-framing on
//! *output*, not decode input; see `vaco-bsf-h2645`'s own module doc.
//!
//! This used to walk every packet as a hardcoded Annex-B byte stream
//! (`vaco_bitstream::annexb::nal_units`) with its own ad-hoc VPS/SPS/PPS
//! `HashMap`s, so `vaco -i real.mp4 -c:v hevc` decoded nothing: MP4 never
//! carries in-band parameter sets, and the decoder never called
//! [`Decoder::set_extradata`]'s actual implementation because it didn't
//! have one — the trait's no-op default swallowed the `hvcC` box
//! silently. That is exactly `vaco-codec-h264`'s own "AVCC vs Annex B"
//! history repeating; the fix is the identical pattern, reusing
//! [`HevcParser`]'s parameter-set bookkeeping rather than duplicating it
//! a second time (D14: this crate must not re-derive what
//! `vaco-parse-hevc` already owns).
//!
//! # Access-unit assembly
//!
//! [`HevcParser::push_access_unit`] is called on every packet
//! ([`send_packet`](Decoder::send_packet)), reusing its own parameter-set
//! bookkeeping and slice-header parse rather than re-deriving either —
//! this decoder only re-scans the same access unit's NAL units afterward
//! (again via [`vaco_format_nalu::units`], not a second parser) to reach
//! the one primary-coded-picture slice's raw bits `push_access_unit`
//! itself does not hand back.

use vaco_codec_cabac::CabacDecoder;
use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_format_nalu::{RbspBuf, units};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_hevc::{ChromaFormat, HevcNalHeader, HevcParser, Pps, SliceHeader, SliceKind, Sps};

use crate::cabac_ctx::ContextBank;
use crate::ctu::{self, Ctx};
use crate::deblock;
use crate::framebuf::{CuGrid, Picture};

/// The HEVC decoder. See the crate doc and module doc for exactly what is
/// and is not implemented.
pub struct HevcDecoder {
    limits: Limits,
    parser: HevcParser,
    budget: Budget,
    rbsp: RbspBuf,
    machine: vaco_codec_core::machine::Machine<vaco_frame::Frame>,
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
            parser: HevcParser::new(limits.clone()),
            budget: Budget::new(limits.clone()),
            rbsp: RbspBuf::new(),
            machine: vaco_codec_core::machine::Machine::with_capacity(vaco_codec_core::Caps::empty(), 1),
            limits,
        }
    }

    /// Decode one packet (one container sample / access unit) into at most
    /// one frame. `Ok(None)` means the access unit carried no primary-coded
    /// picture (parameter sets/SEI only), which is legal and not an error —
    /// mirrors `H264Decoder::decode_packet`'s own contract exactly.
    fn decode_packet(&mut self, pkt: &Packet) -> Result<Option<vaco_frame::Frame>> {
        let payload = pkt.payload();
        let framing = self.parser.framing();

        // Reuse `HevcParser`'s own access-unit assembly for parameter-set
        // bookkeeping and slice-header parsing — not re-derived here.
        // `info.picture_type` is `None` when this access unit held no VCL
        // slice at all (or its parameter sets are not yet known, which is
        // legal for a stream joined mid-flight).
        let info = self.parser.push_access_unit(payload, framing)?;
        let Some(kind) = info.picture_type else { return Ok(None) };
        if kind != 'I' {
            return Err(Error::Unsupported("vaco-codec-hevc: only I-slices are decoded"));
        }

        // Locate this access unit's one primary-coded-picture slice —
        // `vaco_format_nalu::units` is the same reusable, `Framing`-aware
        // NAL-unit iterator `HevcParser` itself is built on, not a second
        // parser.
        let mut slice_nal: Option<&[u8]> = None;
        let mut slice_count = 0u32;
        for nal in units(payload, framing) {
            let Some(header) = HevcNalHeader::parse(nal.data) else { continue };
            if !header.is_base_layer() {
                continue;
            }
            if header.nal_unit_type.has_slice_header() {
                slice_count += 1;
                if slice_nal.is_none() {
                    slice_nal = Some(nal.data);
                }
            }
        }
        if slice_count > 1 {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: more than one slice segment per picture is not supported",
            ));
        }
        let Some(ebsp) = slice_nal else { return Ok(None) };
        let header = HevcNalHeader::parse(ebsp).ok_or(Error::InvalidData("vaco-codec-hevc: empty NAL unit"))?;

        self.rbsp.fill(ebsp, &mut self.budget)?;
        let rbsp = self.rbsp.as_slice();

        let pps_id = vaco_parse_hevc::slice::peek_pps_id(rbsp)
            .ok_or(Error::InvalidData("vaco-codec-hevc: slice segment header truncated before pps_id"))?;
        // Cloned rather than borrowed: the CTU walk below needs `&mut
        // self.budget` for the rest of this function, which a borrow of
        // `self.parser`'s tables held open across that call would conflict
        // with.
        let (pps, sps) = {
            let (p, s) = self
                .parser
                .parameter_sets()
                .sps_for_pps(pps_id)
                .ok_or(Error::Unsupported("vaco-codec-hevc: referenced PPS/SPS not seen yet"))?;
            (p.clone(), s.clone())
        };

        check_scope(&sps, &pps)?;

        let mut reader = vaco_bitstream::BitReader::new(rbsp);
        reader.skip(16);
        let hdr = SliceHeader::parse_data(&mut reader, header, &sps, &pps, &mut self.budget)?;
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
        let mut pic = Picture::new(&mut self.budget, width, height)?;
        let cu_grid = CuGrid::new(&mut self.budget, width, height)?;
        let mut walk = Ctx::new(
            &mut pic,
            cu_grid,
            &sps,
            &pps,
            slice_qp,
            hdr.deblocking_filter_disabled,
            hdr.beta_offset_div2,
            hdr.tc_offset_div2,
        );

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

        deblock::filter_picture(&mut walk);

        let mut frame = pic_to_frame(&mut self.budget, &sps, &pic)?;
        frame.pts = pkt.pts;
        frame.duration = pkt.duration;
        frame.flags |= vaco_frame::FrameFlags::KEY;
        Ok(Some(frame))
    }
}

/// Refuse, up front, every combination this crate does not implement — see
/// the crate doc for the complete, stated list.
fn check_scope(sps: &Sps, pps: &Pps) -> Result<()> {
    let unsupported = |why: &'static str| Err(Error::Unsupported(why));
    if sps.chroma_format != ChromaFormat::Yuv420 {
        return unsupported("vaco-codec-hevc: only 4:2:0 chroma is decoded");
    }
    if sps.sample_adaptive_offset_enabled {
        // Not merely "SAO's pixel offsets are never applied" (true, and
        // stated in the crate doc) — `sample_adaptive_offset_enabled_flag`
        // also gates §7.3.8.3's optional `sao()` syntax at the start of
        // every CTU (`slice_sao_luma_flag`/`slice_sao_chroma_flag`, in
        // turn read from the slice header). This crate parses neither, so
        // a stream that actually turns SAO on desyncs the entropy decoder
        // from the very first CTU that merges or sets an offset — not a
        // silently-wrong pixel, a `CABAC decode ran past the slice
        // segment data` crash. Refusing by name, matching every other cut
        // in this function, turns that crash into an honest
        // `Error::Unsupported` instead.
        return unsupported("vaco-codec-hevc: SAO is not supported (encode without it, e.g. libx265 no-sao=1)");
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

fn pic_to_frame(budget: &mut Budget, sps: &Sps, pic: &Picture) -> Result<vaco_frame::Frame> {
    let pix_fmt = vaco_pixfmt::PixFmt::from_name("yuv420p")
        .map_err(|_| Error::InvalidData("vaco-codec-hevc: yuv420p pixel format missing"))?;
    let (width, height) = sps.dimensions().unwrap_or((sps.pic_width_in_luma_samples, sps.pic_height_in_luma_samples));
    let mut frame = vaco_frame::Frame::alloc_video(budget, pix_fmt, width, height)?;
    blit(&pic.y, &mut frame, 0, width as usize, height as usize);
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    blit(&pic.cb, &mut frame, 1, cw, ch);
    blit(&pic.cr, &mut frame, 2, cw, ch);
    Ok(frame)
}

fn blit(src: &crate::framebuf::Plane, frame: &mut vaco_frame::Frame, plane_index: usize, width: usize, height: usize) {
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
                match self.decode_packet(pkt) {
                    Ok(Some(frame)) => {
                        self.machine.emit(frame);
                        Ok(())
                    }
                    Ok(None) => Ok(()),
                    Err(e) => Err(e),
                }
            }
        }
    }

    fn receive_frame(&mut self) -> Result<vaco_frame::Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.parser.flush();
        // Release every byte charged to the budget along with the state
        // that held them, mirroring `H264Decoder::flush`'s own precedent.
        self.budget = Budget::new(self.limits.clone());
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.parser.set_extradata(extradata)?;
        Ok(())
    }
}
