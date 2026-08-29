//! The real `VideoToolbox` decode path: one session, built once from a
//! stream's SPS/PPS, driven synchronously per access unit.
//!
//! # Unsafe surface
//!
//! Every `unsafe` block here is a single FFI call across the `VideoToolbox`/
//! CoreMedia/CoreVideo boundary, each with its own `SAFETY` comment. There
//! are eight call sites: format-description creation, session creation and
//! invalidation, block-buffer creation and data-fill, sample-buffer
//! creation, the decode call itself, and the pixel-buffer lock/unlock pair
//! plus the raw plane-pointer copy in [`VideoToolboxSurface::download`].
//! `RcBlock::new` (the decode callback) and every CoreMedia/CoreVideo getter
//! used (dimensions, plane count, plane geometry) are safe calls in this
//! binding — they carry no memory-safety precondition beyond "the object is
//! valid", which a live `CFRetained`/`&CVPixelBuffer` already guarantees.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    kCMBlockBufferAssureMemoryNowFlag, kCMTimeInvalid,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPlaneCount,
    CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::{VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionSession};

use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_hw_core::{HwAccel, HwDeviceType, HwFrame, HwSurface};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

/// noErr, spelled out because `objc2-video-toolbox` keeps `OSStatus`
/// crate-private (it is a plain `i32` there, so naming the type is
/// unnecessary — every status value here is compared against this).
const NO_ERR: i32 = 0;

/// One `VideoToolbox` decode session for H.264, built once and reused picture
/// to picture, matching how a real `Decoder` implementation would hold it.
#[derive(Debug)]
pub struct VideoToolboxDecoder {
    session: CFRetained<VTDecompressionSession>,
    /// The same format description the session was created from. Every
    /// sample buffer handed to `decode_frame_with_output_handler` must carry
    /// it too — `VideoToolbox` treats a sample buffer with a *different* (or
    /// absent) format description as a format change mid-stream and refuses
    /// it with `kVTFormatDescriptionChangeNotSupportedErr`, which is exactly
    /// what omitting this looked like before this field existed.
    format_description: CFRetained<CMFormatDescription>,
    /// One access unit's slice data, accumulated by
    /// [`decode_slice`](HwAccel::decode_slice) as 4-byte-length-prefixed NAL
    /// units (the AVCC framing `nal_unit_header_length = 4` below commits
    /// this session to) and consumed by
    /// [`end_frame`](HwAccel::end_frame).
    accumulated: Vec<u8>,
}

// SAFETY: Apple documents a VTDecompressionSession as usable from any one
// thread at a time; every method here takes `&mut self`, which already
// enforces single-threaded access to it, so moving the whole decoder to
// another thread between calls is sound.
unsafe impl Send for VideoToolboxDecoder {}

impl VideoToolboxDecoder {
    /// Build a session from a stream's SPS and PPS, exactly as found in
    /// Annex-B (start codes and any trailing padding stripped, emulation
    /// prevention bytes left alone — [`crate::split_annex_b`] produces
    /// units in this shape).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if `VideoToolbox` rejects the parameter sets
    /// (malformed SPS/PPS, or a profile/level this build's `VideoToolbox` does
    /// not implement) or refuses to create a session for them.
    pub fn new(sps: &[u8], pps: &[u8]) -> Result<Self> {
        let format_description = create_h264_format_description(sps, pps)?;

        let mut session_ptr: *mut VTDecompressionSession = std::ptr::null_mut();
        // SAFETY: `format_description` is a just-created, live
        // CMVideoFormatDescription; `session_ptr` is a valid stack
        // out-pointer; the remaining arguments are `None`/null, which the
        // API defines as "let VideoToolbox pick a decoder", "no
        // requirements on the output format" and "call
        // decode_frame_with_output_handler instead of a callback record"
        // respectively.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format_description,
                None,
                None,
                std::ptr::null(),
                NonNull::from(&mut session_ptr),
            )
        };
        if status != NO_ERR {
            return Err(Error::Unsupported(
                "VideoToolbox refused to create a decompression session for this stream",
            ));
        }
        let session_ptr = NonNull::new(session_ptr).ok_or(Error::Unsupported(
            "VideoToolbox reported success but returned no session",
        ))?;
        // SAFETY: `status == noErr` together with a non-null out-pointer is
        // VTDecompressionSessionCreate's documented success case, which
        // hands back a session already carrying a +1 retain count — exactly
        // what `CFRetained::from_raw` takes ownership of.
        let session = unsafe { CFRetained::from_raw(session_ptr) };

        Ok(Self {
            session,
            format_description,
            accumulated: Vec::new(),
        })
    }
}

impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        // SAFETY: `self.session` is a live session for as long as `self`
        // exists; invalidating it before its final `CFRelease` (performed by
        // `CFRetained`'s own `Drop`, immediately after this) is exactly the
        // deterministic-teardown sequence VTDecompressionSessionInvalidate's
        // own doc asks for.
        unsafe { self.session.invalidate() };
    }
}

/// The one decode callback's result: never ran, ran with a real image, or
/// ran and reported failure (no image).
enum Outcome {
    Pending,
    Image(CFRetained<CVPixelBuffer>),
    Failed(i32),
}

impl HwAccel for VideoToolboxDecoder {
    fn device_type(&self) -> HwDeviceType {
        HwDeviceType::VideoToolbox
    }

    fn start_frame(&mut self) -> Result<()> {
        self.accumulated.clear();
        Ok(())
    }

    fn decode_slice(&mut self, data: &[u8]) -> Result<()> {
        // AVCC framing: a 4-byte big-endian length before each NAL unit,
        // matching the `nal_unit_header_length = 4` this session's format
        // description was built with.
        let len = u32::try_from(data.len())
            .map_err(|_| Error::Unsupported("NAL unit too large for a 4-byte AVCC length"))?;
        self.accumulated.extend_from_slice(&len.to_be_bytes());
        self.accumulated.extend_from_slice(data);
        Ok(())
    }

    fn end_frame(&mut self) -> Result<HwFrame> {
        if self.accumulated.is_empty() {
            return Err(Error::Unsupported(
                "end_frame called with no slice data submitted since the last start_frame",
            ));
        }

        let block_buffer = create_block_buffer(&self.accumulated)?;
        let sample_buffer = create_sample_buffer(&block_buffer, &self.format_description)?;

        // `VTDecompressionOutputHandler`'s block type is `dyn Fn(..) + 'static`
        // (the type alias names no lifetime), so the closure below cannot
        // borrow a function-local directly — it owns a clone of this `Rc`
        // instead, which is 'static as a type regardless of where the value
        // it points at happens to live.
        let outcome = std::rc::Rc::new(RefCell::new(Outcome::Pending));
        let outcome_for_block = std::rc::Rc::clone(&outcome);

        let handler = RcBlock::new(
            move |status: i32,
                  _flags: VTDecodeInfoFlags,
                  image: *mut CVImageBuffer,
                  _pts: CMTime,
                  _duration: CMTime| {
                let mut outcome = outcome_for_block.borrow_mut();
                *outcome = match NonNull::new(image) {
                    // SAFETY: a non-null `image` here is a live CVImageBuffer
                    // handed to us for the duration of this callback only
                    // (VTDecompressionOutputCallback's own doc); `retain`
                    // takes our own +1 so it survives past the callback
                    // returning, which is what lets `end_frame` hand it
                    // onward as an `HwFrame`.
                    Some(image) => Outcome::Image(unsafe { CFRetained::retain(image) }),
                    None => Outcome::Failed(status),
                };
            },
        );

        // SAFETY: `sample_buffer` was just built above and is a live,
        // ready (data_ready = true) CMSampleBuffer; `decode_flags` is empty,
        // which VTDecompressionSessionDecodeFrame's own doc guarantees means
        // the output handler has already run by the time this call
        // returns — there is no async lifetime to manage. `handler`'s
        // pointer is valid for the duration of this call, which is the only
        // time it is used.
        let status = unsafe {
            self.session.decode_frame_with_output_handler(
                &sample_buffer,
                VTDecodeFrameFlags::empty(),
                std::ptr::null_mut(),
                RcBlock::as_ptr(&handler),
            )
        };
        self.accumulated.clear();
        // Drop the block (and with it, its clone of `outcome`) now that the
        // synchronous call above is done with it, so the `Rc::try_unwrap`
        // below sees a strong count of exactly one.
        drop(handler);

        if status != NO_ERR {
            return Err(Error::Unsupported(
                "VideoToolbox rejected this access unit's compressed data",
            ));
        }
        let outcome = std::rc::Rc::try_unwrap(outcome).map_or(Outcome::Pending, RefCell::into_inner);
        match outcome {
            Outcome::Image(image) => {
                let (width, height) = pixel_buffer_dimensions(&image);
                Ok(HwFrame::new(
                    PixFmt::VideotoolboxVld,
                    width,
                    height,
                    std::sync::Arc::new(VideoToolboxSurface { image }),
                ))
            }
            Outcome::Failed(status) => {
                let _ = status;
                Err(Error::Unsupported(
                    "VideoToolbox's decode callback reported failure (no image buffer)",
                ))
            }
            Outcome::Pending => Err(Error::Unsupported(
                "VideoToolbox's decode callback never ran, despite a synchronous decode flag set",
            )),
        }
    }
}

/// The `HwSurface` a completed `VideoToolbox` decode produces: a retained
/// `CVPixelBuffer`, downloadable to a real software [`Frame`].
struct VideoToolboxSurface {
    image: CFRetained<CVPixelBuffer>,
}

// SAFETY: `objc2-core-video`'s generated `CVBuffer` opts out of the auto
// Send/Sync every other `CFRetained<T>` gets, which is the conservative
// default for a CF type with no single documented thread-safety rule --- but
// a `CVPixelBuffer` specifically is Apple's own documented exception: safe to
// read from any thread once retained, provided access is through
// `CVPixelBufferLockBaseAddress`, which the only reader here
// (`HwSurface::download`) always goes through with `ReadOnly` and never
// mutates through. No other method on this type exists.
unsafe impl Send for VideoToolboxSurface {}
// SAFETY: see the comment above `impl Send` — the same reasoning covers
// `Sync`: concurrent read-only access through a `&VideoToolboxSurface` can
// only ever reach `CVPixelBufferLockBaseAddress`'s read-only path.
unsafe impl Sync for VideoToolboxSurface {}

impl std::fmt::Debug for VideoToolboxSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoToolboxSurface").finish_non_exhaustive()
    }
}

impl HwSurface for VideoToolboxSurface {
    fn device_type(&self) -> HwDeviceType {
        HwDeviceType::VideoToolbox
    }

    fn download(&self, budget: &mut Budget) -> Result<Frame> {
        let (width, height) = pixel_buffer_dimensions(&self.image);

        // SAFETY: `self.image` is a live CVPixelBuffer for as long as
        // `self` exists; locking for read-only CPU access before touching
        // any plane pointer, and unlocking exactly once on every exit path
        // below, is the contract CVPixelBufferLockBaseAddress's own doc
        // states.
        let lock_status =
            unsafe { CVPixelBufferLockBaseAddress(&self.image, CVPixelBufferLockFlags::ReadOnly) };
        if lock_status != 0 {
            return Err(Error::Unsupported(
                "VideoToolbox refused to lock a decoded pixel buffer for readback",
            ));
        }

        let frame = download_nv12(&self.image, width, height, budget);

        // SAFETY: matches the successful lock above one-for-one, on every
        // path out of this function from here on.
        unsafe { CVPixelBufferUnlockBaseAddress(&self.image, CVPixelBufferLockFlags::ReadOnly) };

        frame
    }
}

/// Copy a two-plane (Y, interleaved `CbCr`) `CVPixelBuffer` into a
/// [`PixFmt::Nv12`] [`Frame`], respecting each plane's own stride.
///
/// Assumes the buffer is already locked by the caller — this function only
/// reads.
fn download_nv12(image: &CVPixelBuffer, width: u32, height: u32, budget: &mut Budget) -> Result<Frame> {
    if CVPixelBufferGetPlaneCount(image) != 2 {
        return Err(Error::Unsupported(
            "VideoToolbox produced a pixel buffer shape this crate does not read (expected two NV12 planes)",
        ));
    }

    let mut frame = Frame::alloc_video(budget, PixFmt::Nv12, width, height)?;
    let mut planes = frame.planes_mut();
    for (plane_index, plane) in planes.iter_mut().enumerate() {
        let plane_width = CVPixelBufferGetWidthOfPlane(image, plane_index);
        let plane_height = CVPixelBufferGetHeightOfPlane(image, plane_index);
        let src_stride = CVPixelBufferGetBytesPerRowOfPlane(image, plane_index);
        let base = CVPixelBufferGetBaseAddressOfPlane(image, plane_index);
        let Some(base) = NonNull::new(base) else {
            return Err(Error::Unsupported(
                "VideoToolbox reported a locked pixel buffer with a null plane address",
            ));
        };
        // NV12's second plane is interleaved CbCr, so its row byte count is
        // twice the sample width, not the plane width itself. `Nv12`'s own
        // `vaco-pixfmt` layout already knows this; we only need to copy
        // exactly the bytes the plane actually has, per row.
        let row_bytes = if plane_index == 0 { plane_width } else { plane_width * 2 };
        for row in 0..plane_height.min(plane.rows()) {
            // SAFETY: `base` plus `row * src_stride` stays within the
            // locked pixel buffer's plane, because `row < plane_height` and
            // `src_stride` is VideoToolbox's own reported stride for this
            // plane; `row_bytes <= src_stride` always holds for a
            // conforming NV12 buffer, and the buffer is locked for the
            // duration of this whole function.
            let src_row = unsafe {
                std::slice::from_raw_parts(base.as_ptr().cast::<u8>().add(row * src_stride), row_bytes)
            };
            let Some(dst_row) = plane.row_mut(row) else {
                break;
            };
            let n = src_row.len().min(dst_row.len());
            if let (Some(src), Some(dst)) = (src_row.get(..n), dst_row.get_mut(..n)) {
                dst.copy_from_slice(src);
            }
        }
    }
    drop(planes);
    Ok(frame)
}

fn pixel_buffer_dimensions(image: &CVPixelBuffer) -> (u32, u32) {
    let width = CVPixelBufferGetWidthOfPlane(image, 0).max(1);
    let height = CVPixelBufferGetHeightOfPlane(image, 0).max(1);
    (
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    )
}

fn create_h264_format_description(
    sps: &[u8],
    pps: &[u8],
) -> Result<CFRetained<CMFormatDescription>> {
    if sps.is_empty() || pps.is_empty() {
        return Err(Error::Unsupported(
            "VideoToolbox needs a non-empty SPS and PPS to build a format description",
        ));
    }
    // `CMVideoFormatDescriptionCreateFromH264ParameterSets` takes an array of
    // pointers rather than a slice of slices, so the two NAL units' own
    // buffers have to outlive this call as locals — they do, as `sps`/`pps`,
    // both non-empty per the check above, which is what makes `as_ptr()`
    // non-null here.
    let sps_ptr = NonNull::new(sps.as_ptr().cast_mut())
        .ok_or(Error::Unsupported("unreachable: a non-empty slice's pointer is never null"))?;
    let pps_ptr = NonNull::new(pps.as_ptr().cast_mut())
        .ok_or(Error::Unsupported("unreachable: a non-empty slice's pointer is never null"))?;
    let mut pointers = [sps_ptr, pps_ptr];
    let mut sizes = [sps.len(), pps.len()];
    let Some(pointers_head) = NonNull::new(pointers.as_mut_ptr()) else {
        return Err(Error::Unsupported("unreachable: a local array is never null"));
    };
    let Some(sizes_head) = NonNull::new(sizes.as_mut_ptr()) else {
        return Err(Error::Unsupported("unreachable: a local array is never null"));
    };

    let mut desc_ptr: *const CMFormatDescription = std::ptr::null();
    // SAFETY: `pointers`/`sizes` describe two live, non-empty byte slices
    // (`sps`, `pps`) that outlive this call; `nal_unit_header_length = 4`
    // commits the AVCC framing `decode_slice` above produces; `desc_ptr` is
    // a valid stack out-pointer.
    let status = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            None,
            2,
            pointers_head,
            sizes_head,
            4,
            NonNull::from(&mut desc_ptr),
        )
    };
    if status != NO_ERR {
        return Err(Error::Unsupported(
            "VideoToolbox rejected these SPS/PPS NAL units as a format description",
        ));
    }
    let desc_ptr = NonNull::new(desc_ptr.cast_mut()).ok_or(Error::Unsupported(
        "VideoToolbox reported success but returned no format description",
    ))?;
    // SAFETY: `status == noErr` with a non-null out-pointer is this
    // function's documented success case, handing back a +1-retained
    // CMFormatDescription.
    Ok(unsafe { CFRetained::from_raw(desc_ptr) })
}

fn create_block_buffer(data: &[u8]) -> Result<CFRetained<CMBlockBuffer>> {
    let mut block_ptr: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: passing a null `memory_block` with `kCMBlockBufferAssureMemoryNowFlag`
    // set asks CoreMedia to allocate `data.len()` bytes itself using the
    // default allocator (both allocator arguments `None`); `block_ptr` is a
    // valid stack out-pointer.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            data.len(),
            None,
            std::ptr::null(),
            0,
            data.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block_ptr),
        )
    };
    if status != NO_ERR {
        return Err(Error::Unsupported(
            "VideoToolbox refused to allocate a block buffer for this access unit",
        ));
    }
    let block_ptr = NonNull::new(block_ptr).ok_or(Error::Unsupported(
        "VideoToolbox reported success but returned no block buffer",
    ))?;
    // SAFETY: success case per the call above; hands back a +1-retained
    // CMBlockBuffer.
    let block_buffer = unsafe { CFRetained::from_raw(block_ptr) };

    let Some(source) = NonNull::new(data.as_ptr().cast_mut().cast()) else {
        return Err(Error::Unsupported("unreachable: a live slice is never null"));
    };
    // SAFETY: `source` points at `data`, which is live and at least
    // `data.len()` bytes; `block_buffer` was just allocated above with
    // exactly that capacity (`kCMBlockBufferAssureMemoryNowFlag`), so
    // copying the whole of `data` into it at offset 0 stays in bounds.
    let status = unsafe {
        CMBlockBuffer::replace_data_bytes(source, &block_buffer, 0, data.len())
    };
    if status != NO_ERR {
        return Err(Error::Unsupported(
            "VideoToolbox refused to fill a block buffer with this access unit's bytes",
        ));
    }
    Ok(block_buffer)
}

fn create_sample_buffer(
    block_buffer: &CMBlockBuffer,
    format_description: &CMFormatDescription,
) -> Result<CFRetained<CMSampleBuffer>> {
    // No real timing is known at this layer (there is no `Decoder`
    // integration yet to source a pts/dts from — see the crate doc), so
    // every field is `kCMTimeInvalid`, which CMSampleBuffer's own doc
    // states is the "not available" value for exactly this situation.
    // SAFETY: reading an `extern "C"` static requires `unsafe` per Rust's
    // rules for FFI globals; this one is a plain data constant CoreMedia
    // exports and never mutates.
    let invalid = unsafe { kCMTimeInvalid };
    let timing = CMSampleTimingInfo {
        duration: invalid,
        presentationTimeStamp: invalid,
        decodeTimeStamp: invalid,
    };
    let mut sample_ptr: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: `block_buffer` is a live, fully-populated CMBlockBuffer;
    // `format_description` is the same live CMFormatDescription the session
    // was created from — passing anything else (including `None`) here is
    // what `kVTFormatDescriptionChangeNotSupportedErr` looked like before
    // this parameter existed, since VideoToolbox checks the sample buffer's
    // own format against the session's; `timing`/the single sample-size
    // entry describe exactly the one sample (`num_samples = 1`) it holds;
    // `sample_ptr` is a valid stack out-pointer.
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(block_buffer),
            Some(format_description),
            1,
            1,
            std::ptr::from_ref(&timing),
            0,
            std::ptr::null(),
            NonNull::from(&mut sample_ptr),
        )
    };
    if status != NO_ERR {
        return Err(Error::Unsupported(
            "VideoToolbox refused to wrap this access unit's block buffer as a sample buffer",
        ));
    }
    let sample_ptr = NonNull::new(sample_ptr).ok_or(Error::Unsupported(
        "VideoToolbox reported success but returned no sample buffer",
    ))?;
    // SAFETY: success case per the call above; hands back a +1-retained
    // CMSampleBuffer.
    Ok(unsafe { CFRetained::from_raw(sample_ptr) })
}
