//! A common numeric domain for filters that do arithmetic on samples.
//!
//! `volume`, `amix`, `amerge`, `channelmap`, `join` and `pan` all need to read
//! and combine sample values regardless of the twelve `SampleFmt` variants a
//! link might have negotiated. Rather than hand-roll six numeric conversions
//! (`u8`/`s16`/`s32`/`s64`/`f32`/`f64`, each packed or planar), this module
//! reuses `vaco_resample::convert::convert` — the crate that already carries
//! the project's *measured* numeric contract (D17: which rounding mode the
//! reference actually uses on each conversion pair) — to decode a frame's
//! planes into planar `f64` and back. That costs an extra copy on filters that
//! are not on anyone's hot path yet; it buys agreement with the one place in
//! the tree that has already done the measurement.

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_resample::buf::{AudioMut, AudioRef};
use vaco_resample::convert;
use vaco_sampfmt::SampleFmt;

/// One channel's samples, decoded to `f64`.
pub(crate) type Channels = SmallVec<[Vec<f64>; 8]>;

/// Decode an audio frame's planes into one `Vec<f64>` per channel.
///
/// # Errors
/// [`Error::InvalidData`] if `frame` is not audio, or its plane layout does
/// not match its declared format.
pub(crate) fn decode(frame: &Frame) -> Result<(SampleFmt, u32, u32, ChannelLayout, Channels)> {
    let FrameData::Audio {
        format,
        sample_rate,
        samples,
        layout,
        ..
    } = &frame.data
    else {
        return Err(Error::InvalidData("expected an audio frame"));
    };
    let (fmt, rate, samples, layout) = (*format, *sample_rate, *samples, layout.clone());
    let channels = layout.channels.max(1);

    let mut src_planes: SmallVec<[&[u8]; 8]> = SmallVec::new();
    for i in 0..frame.plane_count() {
        let Some(p) = frame.plane(i) else { break };
        src_planes.push(p.as_slice());
    }
    let src = AudioRef::from_frame_planes(fmt, channels, &src_planes)
        .map_err(|_| Error::InvalidData("audio plane layout does not match its format"))?;

    let mut bufs: SmallVec<[Vec<u8>; 8]> = (0..channels)
        .map(|_| vec![0u8; (samples as usize).saturating_mul(8)])
        .collect();
    {
        // A plain `Vec` here, not `SmallVec`: `Vec`'s `Drop` impl is
        // `#[may_dangle]`, which is what lets the borrow checker see that
        // dropping this container of `&mut [u8]` borrows does not need them
        // to outlive the container itself. `SmallVec` has no such attribute
        // (it is a stable third-party crate; `dropck_eyepatch` is nightly-only),
        // so the same line with `SmallVec<[&mut [u8]; 8]>` fails to borrow-check.
        let mut plane_refs: Vec<&mut [u8]> = bufs.iter_mut().map(Vec::as_mut_slice).collect();
        let mut dst = AudioMut::planar(SampleFmt::F64P, &mut plane_refs)
            .map_err(|_| Error::InvalidData("could not build an f64 decode buffer"))?;
        convert::convert(src, &mut dst)?;
    }

    let mut out: Channels = SmallVec::new();
    for b in &bufs {
        let mut v = Vec::new();
        for chunk in b.chunks_exact(8) {
            let arr: [u8; 8] = [
                *chunk.first().unwrap_or(&0),
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
                *chunk.get(3).unwrap_or(&0),
                *chunk.get(4).unwrap_or(&0),
                *chunk.get(5).unwrap_or(&0),
                *chunk.get(6).unwrap_or(&0),
                *chunk.get(7).unwrap_or(&0),
            ];
            v.push(f64::from_ne_bytes(arr));
        }
        out.push(v);
    }
    let _ = rate;
    Ok((fmt, rate, samples, layout, out))
}

/// Encode planar `f64` channel data into a freshly pooled frame of `fmt`.
///
/// All channels must carry the same sample count; the shorter of any that
/// disagree wins, which cannot happen for callers that built `data` from
/// [`decode`] and never resized a channel independently of the others.
///
/// # Errors
/// Whatever the pool or the underlying conversion reports.
pub(crate) fn encode(
    pool: &FramePool,
    fmt: SampleFmt,
    layout: ChannelLayout,
    sample_rate: u32,
    data: &Channels,
) -> Result<Frame> {
    let samples = data.iter().map(Vec::len).min().unwrap_or(0);
    let samples_u32 = u32::try_from(samples).unwrap_or(u32::MAX);
    // Captured before `layout` moves into `acquire_audio`: for a *packed*
    // format there is exactly one plane regardless of channel count, so
    // `refs.len()` below (the plane count) is not the channel count — using
    // it as one silently mismatched `AudioMut::packed`'s declared channel
    // count against `src`'s real one, which made `convert::convert` fail and,
    // through `mix()`'s `.ok()?`, made every packed-format mix silently
    // consume its input and produce nothing.
    let out_channels = layout.channels.max(1);
    let mut frame = pool.acquire_audio(fmt, layout, samples_u32, sample_rate)?;

    let mut src_bufs: SmallVec<[Vec<u8>; 8]> = SmallVec::new();
    for ch in data {
        let mut b = Vec::new();
        for s in ch.iter().take(samples) {
            b.extend_from_slice(&s.to_ne_bytes());
        }
        src_bufs.push(b);
    }
    let src_refs: SmallVec<[&[u8]; 8]> = src_bufs.iter().map(Vec::as_slice).collect();
    let src = AudioRef::planar(SampleFmt::F64P, &src_refs)
        .map_err(|_| Error::InvalidData("could not build an f64 encode buffer"))?;

    {
        let mut planes = frame.planes_mut();
        // `Vec`, not `SmallVec` — see the note in the mirror-image decode path.
        let mut refs: Vec<&mut [u8]> = Vec::new();
        for p in &mut planes {
            if let Some(row) = p.row_mut(0) {
                refs.push(row);
            }
        }
        if fmt.is_planar() {
            let mut dst = AudioMut::planar(fmt, &mut refs)
                .map_err(|_| Error::InvalidData("could not build an output audio buffer"))?;
            convert::convert(src, &mut dst)?;
        } else {
            let Some(buf) = refs.into_iter().next() else {
                return Err(Error::InvalidData("packed frame has no plane"));
            };
            let mut dst = AudioMut::packed(fmt, out_channels, buf)
                .map_err(|_| Error::InvalidData("could not build an output audio buffer"))?;
            convert::convert(src, &mut dst)?;
        }
    }
    Ok(frame)
}
