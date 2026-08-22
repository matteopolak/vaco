//! The TOC byte and packet framing. RFC 6716 §3.
//!
//! An Opus packet is **not self-framing**: its length comes from the container
//! (an Ogg segment, a Matroska block, an MP4 sample, an RTP payload). What the
//! packet itself carries is the split of that length into one or more *frames*,
//! and the TOC byte that says how long each frame is in time.
//!
//! Getting this wrong is the classic Opus parser bug: the code-2 and code-3 VBR
//! length fields can claim more bytes than the packet contains, and a parser
//! that subtracts before it checks underflows. Every length here is bounded
//! against what is actually present before it is used.

use arrayvec::ArrayVec;
use vaco_core::{Error, Result};

/// The largest number of frames a packet may carry: 120 ms at 2.5 ms each.
/// RFC 6716 §3.2.5.
pub const MAX_FRAMES: usize = 48;

/// The largest a single frame may be. RFC 6716 §3.2.1.
pub const MAX_FRAME_BYTES: usize = 1275;

/// The longest a packet may last, in 48 kHz samples. RFC 6716 §3.2.5.
pub const MAX_PACKET_SAMPLES: u32 = 5760;

/// Which coder the frame uses. RFC 6716 §3.1, Table 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// SILK only — speech, narrow to wideband.
    SilkOnly,
    /// SILK and CELT together.
    Hybrid,
    /// CELT only — music, and every frame shorter than 10 ms.
    CeltOnly,
}

/// The audio bandwidth the frame codes. RFC 6716 §3.1, Table 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bandwidth {
    /// 4 kHz.
    Narrowband,
    /// 6 kHz.
    Mediumband,
    /// 8 kHz.
    Wideband,
    /// 12 kHz.
    SuperWideband,
    /// 20 kHz.
    Fullband,
}

impl Bandwidth {
    /// The nominal audio bandwidth in Hz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            Self::Narrowband => 4000,
            Self::Mediumband => 6000,
            Self::Wideband => 8000,
            Self::SuperWideband => 12000,
            Self::Fullband => 20000,
        }
    }
}

/// The table-of-contents byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toc(pub u8);

impl Toc {
    /// The five-bit configuration number.
    #[must_use]
    pub const fn config(self) -> u8 {
        self.0 >> 3
    }

    /// The stereo flag: one coded channel or two.
    #[must_use]
    pub const fn is_stereo(self) -> bool {
        self.0 & 0x04 != 0
    }

    /// The two-bit frame-packing code, 0..=3.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.0 & 0x03
    }

    /// Which coder the configuration selects.
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self.config() {
            0..=11 => Mode::SilkOnly,
            12..=15 => Mode::Hybrid,
            _ => Mode::CeltOnly,
        }
    }

    /// The audio bandwidth the configuration selects.
    #[must_use]
    #[expect(
        clippy::match_same_arms,
        reason = "the arms are RFC 6716 Table 2 row for row; merging the ones \
                  that happen to share a bandwidth would make the table \
                  impossible to check against the specification"
    )]
    pub const fn bandwidth(self) -> Bandwidth {
        match self.config() {
            0..=3 => Bandwidth::Narrowband,
            4..=7 => Bandwidth::Mediumband,
            8..=11 => Bandwidth::Wideband,
            12 | 13 => Bandwidth::SuperWideband,
            14 | 15 => Bandwidth::Fullband,
            16..=19 => Bandwidth::Narrowband,
            20..=23 => Bandwidth::Wideband,
            24..=27 => Bandwidth::SuperWideband,
            _ => Bandwidth::Fullband,
        }
    }

    /// One frame's duration in 48 kHz samples.
    ///
    /// The SILK configurations step 10, 20, 40, 60 ms; the hybrid ones only
    /// 10 and 20; the CELT ones 2.5, 5, 10, 20.
    #[must_use]
    pub const fn frame_samples(self) -> u32 {
        let config = self.config();
        match config {
            // SILK: four sizes per bandwidth.
            0..=11 => match config % 4 {
                0 => 480,
                1 => 960,
                2 => 1920,
                _ => 2880,
            },
            // Hybrid: two sizes per bandwidth.
            12..=15 => {
                if config.is_multiple_of(2) {
                    480
                } else {
                    960
                }
            }
            // CELT: four sizes per bandwidth, starting at 2.5 ms.
            _ => match config % 4 {
                0 => 120,
                1 => 240,
                2 => 480,
                _ => 960,
            },
        }
    }

    /// Coded channels: two when the stereo flag is set.
    #[must_use]
    pub const fn coded_channels(self) -> u8 {
        if self.is_stereo() { 2 } else { 1 }
    }
}

/// A parsed Opus packet: the TOC plus the frames it splits into.
///
/// The frames borrow the input; nothing is copied and nothing is allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusPacket<'a> {
    /// The table-of-contents byte.
    pub toc: Toc,
    /// The compressed frames, in order. A zero-length frame is legal and means
    /// "no data transmitted" (DTX).
    pub frames: ArrayVec<&'a [u8], MAX_FRAMES>,
    /// Padding bytes the packet carries, which carry no audio.
    pub padding: usize,
    /// How many bytes of the input the packet occupied. Equal to the input
    /// length for [`OpusPacket::parse`]; shorter for a self-delimited packet.
    pub len: usize,
}

impl<'a> OpusPacket<'a> {
    /// Parse a packet whose length the container has already established.
    ///
    /// **The caller must have framed the packet already.** Opus carries no
    /// length of its own: `data` must be exactly one packet, no more and no
    /// less. Passing two concatenated packets does not fail — it produces
    /// nonsense — which is why the multi-stream path uses
    /// [`OpusPacket::parse_self_delimited`] instead of guessing.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the framing is inconsistent with the length
    /// the caller supplied, and [`Error::UnexpectedEof`] when the packet is
    /// empty.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        Self::parse_inner(data, false)
    }

    /// Parse a packet in the self-delimiting framing of RFC 6716 Appendix B.
    ///
    /// This is the form every stream but the last uses inside a multi-stream
    /// (mapping family 1, 2 or 255) packet: the length of the final frame is
    /// coded explicitly, so the packet knows where it ends.
    ///
    /// # Errors
    ///
    /// As [`OpusPacket::parse`].
    pub fn parse_self_delimited(data: &'a [u8]) -> Result<Self> {
        Self::parse_inner(data, true)
    }

    /// The packet's duration in 48 kHz samples.
    #[must_use]
    pub fn samples(&self) -> u32 {
        // Bounded by construction: `MAX_FRAMES` frames of at most 2880 samples
        // cannot exceed `MAX_PACKET_SAMPLES` once the frame-count check has run.
        u32::try_from(self.frames.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(self.toc.frame_samples())
    }

    /// Bytes of actual frame data, padding and framing overhead excluded.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.len()).sum()
    }

    fn parse_inner(data: &'a [u8], self_delimited: bool) -> Result<Self> {
        let Some((&first, mut body)) = data.split_first() else {
            return Err(Error::UnexpectedEof);
        };
        let toc = Toc(first);
        let mut frames: ArrayVec<&'a [u8], MAX_FRAMES> = ArrayVec::new();
        let mut padding = 0usize;

        match toc.code() {
            0 => {
                let len = if self_delimited {
                    let (n, rest) = read_frame_length(body)?;
                    body = rest;
                    n
                } else {
                    body.len()
                };
                push_frame(&mut frames, &mut body, len)?;
            }
            1 => {
                let each = if self_delimited {
                    let (n, rest) = read_frame_length(body)?;
                    body = rest;
                    n
                } else {
                    if body.len() % 2 != 0 {
                        return Err(Error::InvalidData(
                            "Opus code 1 packet has an odd payload length",
                        ));
                    }
                    body.len()
                        .checked_div(2)
                        .ok_or(Error::InvalidData("Opus code 1 frame size"))?
                };
                push_frame(&mut frames, &mut body, each)?;
                push_frame(&mut frames, &mut body, each)?;
            }
            2 => {
                let (first_len, rest) = read_frame_length(body)?;
                body = rest;
                let second_len = if self_delimited {
                    let (n, rest) = read_frame_length(body)?;
                    body = rest;
                    n
                } else {
                    body.len().checked_sub(first_len).ok_or(Error::InvalidData(
                        "Opus code 2 first frame overruns the packet",
                    ))?
                };
                push_frame(&mut frames, &mut body, first_len)?;
                push_frame(&mut frames, &mut body, second_len)?;
            }
            _ => {
                let (&header, rest) = body
                    .split_first()
                    .ok_or(Error::InvalidData("Opus code 3 packet has no frame count"))?;
                body = rest;
                let vbr = header & 0x80 != 0;
                let has_padding = header & 0x40 != 0;
                let count = usize::from(header & 0x3f);
                if count == 0 {
                    return Err(Error::InvalidData("Opus code 3 packet declares no frames"));
                }
                let samples = u32::try_from(count)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(toc.frame_samples());
                if count > MAX_FRAMES || samples > MAX_PACKET_SAMPLES {
                    return Err(Error::InvalidData(
                        "Opus code 3 packet is longer than 120 ms",
                    ));
                }

                if has_padding {
                    let (bytes, rest) = read_padding_length(body)?;
                    body = rest;
                    padding = bytes;
                    if !self_delimited {
                        // The packet's length is known, so the padding is a
                        // suffix of it: cut it away before the frame lengths
                        // are worked out from what is left. A self-delimited
                        // packet does not know where it ends until its frames
                        // have been read, so its padding is skipped afterwards.
                        let keep = body.len().checked_sub(padding).ok_or(Error::InvalidData(
                            "Opus code 3 padding overruns the packet",
                        ))?;
                        body = body.get(..keep).unwrap_or_default();
                    }
                }

                if vbr {
                    // `count - 1` explicit lengths; the last frame takes what
                    // is left.
                    let mut lengths: ArrayVec<usize, MAX_FRAMES> = ArrayVec::new();
                    for _ in 1..count {
                        let (n, rest) = read_frame_length(body)?;
                        body = rest;
                        lengths.push(n);
                    }
                    let declared: usize = lengths.iter().sum();
                    let last = if self_delimited {
                        let (n, rest) = read_frame_length(body)?;
                        body = rest;
                        n
                    } else {
                        body.len().checked_sub(declared).ok_or(Error::InvalidData(
                            "Opus code 3 VBR lengths overrun the packet",
                        ))?
                    };
                    for len in lengths {
                        push_frame(&mut frames, &mut body, len)?;
                    }
                    push_frame(&mut frames, &mut body, last)?;
                } else {
                    let each = if self_delimited {
                        let (n, rest) = read_frame_length(body)?;
                        body = rest;
                        n
                    } else {
                        if body.len() % count != 0 {
                            return Err(Error::InvalidData(
                                "Opus code 3 CBR payload does not divide by the frame count",
                            ));
                        }
                        body.len()
                            .checked_div(count)
                            .ok_or(Error::InvalidData("Opus code 3 frame count is zero"))?
                    };
                    for _ in 0..count {
                        push_frame(&mut frames, &mut body, each)?;
                    }
                }
            }
        }

        if self_delimited {
            body = body
                .get(padding..)
                .ok_or(Error::InvalidData("Opus padding overruns the packet"))?;
        } else if !body.is_empty() {
            return Err(Error::InvalidData("Opus packet has trailing bytes"));
        }
        let len = data.len().saturating_sub(body.len());
        Ok(Self {
            toc,
            frames,
            padding,
            len,
        })
    }
}

/// Take `len` bytes off the front of `body` as one frame.
fn push_frame<'a>(
    frames: &mut ArrayVec<&'a [u8], MAX_FRAMES>,
    body: &mut &'a [u8],
    len: usize,
) -> Result<()> {
    if len > MAX_FRAME_BYTES {
        return Err(Error::InvalidData("Opus frame exceeds 1275 bytes"));
    }
    let Some(frame) = body.get(..len) else {
        return Err(Error::InvalidData("Opus frame overruns the packet"));
    };
    let Some(rest) = body.get(len..) else {
        return Err(Error::InvalidData("Opus frame overruns the packet"));
    };
    if frames.try_push(frame).is_err() {
        return Err(Error::InvalidData("Opus packet declares too many frames"));
    }
    *body = rest;
    Ok(())
}

/// The one- or two-byte frame length code of RFC 6716 §3.2.1.
fn read_frame_length(body: &[u8]) -> Result<(usize, &[u8])> {
    let Some((&first, rest)) = body.split_first() else {
        return Err(Error::InvalidData("Opus frame length field is missing"));
    };
    if first < 252 {
        return Ok((usize::from(first), rest));
    }
    let Some((&second, rest)) = rest.split_first() else {
        return Err(Error::InvalidData(
            "Opus two-byte frame length field is truncated",
        ));
    };
    Ok((usize::from(second) * 4 + usize::from(first), rest))
}

/// The padding length of RFC 6716 §3.2.5: a run of `255` bytes each worth 254,
/// terminated by a byte below 255 worth its own value.
///
/// The count of *length* bytes is itself part of the padding, which is why the
/// running total adds one per byte read.
fn read_padding_length(body: &[u8]) -> Result<(usize, &[u8])> {
    let mut total = 0usize;
    let mut rest = body;
    loop {
        let Some((&byte, tail)) = rest.split_first() else {
            return Err(Error::InvalidData("Opus padding length is truncated"));
        };
        rest = tail;
        if byte == 255 {
            total = total.saturating_add(254);
            if total > body.len() {
                return Err(Error::InvalidData("Opus padding overruns the packet"));
            }
        } else {
            total = total.saturating_add(usize::from(byte));
            return Ok((total, rest));
        }
    }
}
