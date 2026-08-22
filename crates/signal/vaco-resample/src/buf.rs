//! The buffer model: what a caller hands in and gets back.
//!
//! Divisions here are by a sample width (1, 2, 4 or 8) or by a frame size the
//! constructor already proved non-zero.
//!
//! The element type is a runtime value, so the public surface is bytes plus a
//! [`SampleFmt`]. Packed and planar are both first-class — forcing one would be
//! a copy, and the whole point of a conversion crate is not to make copies it
//! was not asked for.

#![allow(
    clippy::integer_division,
    reason = "divisors are sample widths or frame sizes already proven non-zero"
)]

use vaco_chlayout::ChannelLayout;
use vaco_core::Error;
use vaco_sampfmt::SampleFmt;

/// The three things that describe one end of a conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub format: SampleFmt,
    pub layout: ChannelLayout,
}

impl AudioSpec {
    /// # Errors
    /// [`Error::InvalidData`] if the layout is structurally invalid, has zero
    /// channels, or the rate is zero.
    pub fn new(sample_rate: u32, format: SampleFmt, layout: ChannelLayout) -> Result<Self, Error> {
        if sample_rate == 0 {
            return Err(Error::InvalidData("sample rate must be non-zero"));
        }
        if layout.channels == 0 || !layout.is_valid() {
            return Err(Error::InvalidData("invalid channel layout"));
        }
        Ok(Self {
            sample_rate,
            format,
            layout,
        })
    }

    #[must_use]
    pub const fn channels(&self) -> u32 {
        self.layout.channels
    }
}

/// A read-only view of audio samples.
///
/// `Packed` is one interleaved block; `Planar` is one block per channel. Which
/// one is legal is decided by [`SampleFmt::is_planar`], and the constructors
/// check it rather than trusting the caller.
#[derive(Clone, Copy, Debug)]
pub enum AudioRef<'a> {
    Packed {
        fmt: SampleFmt,
        channels: u32,
        data: &'a [u8],
    },
    Planar {
        fmt: SampleFmt,
        planes: &'a [&'a [u8]],
    },
}

impl<'a> AudioRef<'a> {
    /// A packed (interleaved) buffer.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `fmt` is planar, `channels` is zero, or `data`
    /// is not a whole number of frames.
    pub fn packed(fmt: SampleFmt, channels: u32, data: &'a [u8]) -> Result<Self, Error> {
        if fmt.is_planar() {
            return Err(Error::InvalidData("planar format given a packed buffer"));
        }
        let frame = frame_bytes(fmt, channels)?;
        if !data.len().is_multiple_of(frame) {
            return Err(Error::InvalidData("packed buffer is not whole frames"));
        }
        Ok(Self::Packed {
            fmt,
            channels,
            data,
        })
    }

    /// A planar buffer, one plane per channel.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `fmt` is packed, there are no planes, or the
    /// planes differ in length or are not a whole number of samples.
    pub fn planar(fmt: SampleFmt, planes: &'a [&'a [u8]]) -> Result<Self, Error> {
        if !fmt.is_planar() {
            return Err(Error::InvalidData("packed format given planar buffers"));
        }
        check_planes(fmt, planes.iter().map(|p| p.len()))?;
        Ok(Self::Planar { fmt, planes })
    }

    #[must_use]
    pub const fn format(&self) -> SampleFmt {
        match *self {
            Self::Packed { fmt, .. } | Self::Planar { fmt, .. } => fmt,
        }
    }

    #[must_use]
    pub fn channels(&self) -> u32 {
        match *self {
            Self::Packed { channels, .. } => channels,
            Self::Planar { planes, .. } => u32::try_from(planes.len()).unwrap_or(u32::MAX),
        }
    }

    /// Samples per channel.
    #[must_use]
    pub fn samples(&self) -> usize {
        match *self {
            Self::Packed {
                fmt,
                channels,
                data,
            } => frame_bytes(fmt, channels).map_or(0, |f| data.len() / f),
            Self::Planar { fmt, planes } => planes
                .first()
                .map_or(0, |p| p.len() / fmt.bytes_per_sample()),
        }
    }

    /// Plane `i`, or `None` when the index is out of range.
    ///
    /// A packed buffer has exactly one plane.
    #[must_use]
    pub fn plane(&self, index: usize) -> Option<&'a [u8]> {
        match *self {
            Self::Packed { data, .. } => (index == 0).then_some(data),
            Self::Planar { planes, .. } => planes.get(index).copied(),
        }
    }

    /// A borrowed view of an audio [`vaco_frame::Frame`]'s planes.
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a video frame or a plane geometry that does
    /// not match the frame's declared format.
    pub fn from_frame_planes(
        fmt: SampleFmt,
        channels: u32,
        planes: &'a [&'a [u8]],
    ) -> Result<Self, Error> {
        if fmt.is_planar() {
            Self::planar(fmt, planes)
        } else {
            let data = planes
                .first()
                .copied()
                .ok_or(Error::InvalidData("packed frame has no plane"))?;
            Self::packed(fmt, channels, data)
        }
    }
}

/// A writable view of audio samples. The mirror of [`AudioRef`].
#[derive(Debug)]
pub enum AudioMut<'a> {
    Packed {
        fmt: SampleFmt,
        channels: u32,
        data: &'a mut [u8],
    },
    Planar {
        fmt: SampleFmt,
        planes: &'a mut [&'a mut [u8]],
    },
}

impl<'a> AudioMut<'a> {
    /// # Errors
    /// As [`AudioRef::packed`].
    pub fn packed(fmt: SampleFmt, channels: u32, data: &'a mut [u8]) -> Result<Self, Error> {
        if fmt.is_planar() {
            return Err(Error::InvalidData("planar format given a packed buffer"));
        }
        let frame = frame_bytes(fmt, channels)?;
        if !data.len().is_multiple_of(frame) {
            return Err(Error::InvalidData("packed buffer is not whole frames"));
        }
        Ok(Self::Packed {
            fmt,
            channels,
            data,
        })
    }

    /// # Errors
    /// As [`AudioRef::planar`].
    pub fn planar(fmt: SampleFmt, planes: &'a mut [&'a mut [u8]]) -> Result<Self, Error> {
        if !fmt.is_planar() {
            return Err(Error::InvalidData("packed format given planar buffers"));
        }
        check_planes(fmt, planes.iter().map(|p| p.len()))?;
        Ok(Self::Planar { fmt, planes })
    }

    #[must_use]
    pub const fn format(&self) -> SampleFmt {
        match *self {
            Self::Packed { fmt, .. } | Self::Planar { fmt, .. } => fmt,
        }
    }

    #[must_use]
    pub fn channels(&self) -> u32 {
        match self {
            Self::Packed { channels, .. } => *channels,
            Self::Planar { planes, .. } => u32::try_from(planes.len()).unwrap_or(u32::MAX),
        }
    }

    /// Capacity in samples per channel.
    #[must_use]
    pub fn samples(&self) -> usize {
        match self {
            Self::Packed {
                fmt,
                channels,
                data,
            } => frame_bytes(*fmt, *channels).map_or(0, |f| data.len() / f),
            Self::Planar { fmt, planes } => planes
                .first()
                .map_or(0, |p| p.len() / fmt.bytes_per_sample()),
        }
    }

    /// Mutable plane `i`.
    pub fn plane_mut(&mut self, index: usize) -> Option<&mut [u8]> {
        match self {
            Self::Packed { data, .. } => {
                if index == 0 {
                    Some(&mut **data)
                } else {
                    None
                }
            }
            Self::Planar { planes, .. } => planes.get_mut(index).map(|p| &mut **p),
        }
    }
}

/// Bytes in one interleaved frame of `channels` channels.
fn frame_bytes(fmt: SampleFmt, channels: u32) -> Result<usize, Error> {
    if channels == 0 {
        return Err(Error::InvalidData("zero channels"));
    }
    fmt.bytes_per_sample()
        .checked_mul(channels as usize)
        .ok_or(Error::InvalidData("frame size overflow"))
}

fn check_planes(fmt: SampleFmt, lens: impl Iterator<Item = usize> + Clone) -> Result<usize, Error> {
    let bps = fmt.bytes_per_sample();
    let mut first: Option<usize> = None;
    let mut count = 0usize;
    for len in lens {
        if !len.is_multiple_of(bps) {
            return Err(Error::InvalidData("plane is not whole samples"));
        }
        match first {
            None => first = Some(len),
            Some(f) if f != len => {
                return Err(Error::InvalidData("planes differ in length"));
            }
            Some(_) => {}
        }
        count += 1;
    }
    if count == 0 {
        return Err(Error::InvalidData("no planes"));
    }
    Ok(count)
}
