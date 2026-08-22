//! The two framings a NAL stream ever arrives in, and one iterator over both.
//!
//! Written from ITU-T H.264 Annex B (byte stream format) and ISO/IEC 14496-15
//! §5.3.3 (length-prefixed samples).

use vaco_bitstream::{ByteReader, annexb};

/// How many bytes a length-prefixed NAL's length field occupies.
///
/// ISO/IEC 14496-15 stores `lengthSizeMinusOne` in two bits, so the encodable
/// widths are 1, 2, 3 and 4 — but the specification reserves 3, and no muxer in
/// the world writes it. A value outside {1, 2, 4} is therefore a malformed
/// configuration record rather than an exotic one, and this type cannot hold it.
///
/// This matches [`vaco_bitstream::avcc::nal_units`], which yields nothing for
/// any other width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LengthSize(u8);

impl LengthSize {
    /// One byte.
    pub const ONE: Self = Self(1);
    /// Two bytes.
    pub const TWO: Self = Self(2);
    /// Four bytes — what every real `avcC` and `hvcC` declares.
    pub const FOUR: Self = Self(4);

    /// A width in bytes, if it is one the format permits.
    #[must_use]
    pub const fn new(bytes: u8) -> Option<Self> {
        match bytes {
            1 | 2 | 4 => Some(Self(bytes)),
            _ => None,
        }
    }

    /// From the two-bit `lengthSizeMinusOne` field of an `avcC` / `hvcC`.
    ///
    /// Returns `None` for the reserved encoding 2 (a three-byte length) and for
    /// anything above 3.
    #[must_use]
    pub const fn from_minus_one(v: u8) -> Option<Self> {
        Self::new(v.wrapping_add(1))
    }

    /// The width in bytes: 1, 2 or 4.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The width as a `usize`, which is what every cursor calculation wants.
    ///
    /// Named `len` because it is a byte count and every call site is arithmetic
    /// over byte counts; there is no `is_empty` because a zero-width length
    /// prefix is not representable.
    #[must_use]
    #[allow(clippy::len_without_is_empty, reason = "a LengthSize is never zero")]
    pub const fn len(self) -> usize {
        self.0 as usize
    }

    /// The largest unit length this width can express.
    #[must_use]
    pub const fn max_unit_len(self) -> u64 {
        match self.0 {
            1 => 0xFF,
            2 => 0xFFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

/// How NAL units are delimited in a buffer.
///
/// The distinction is a container's, not a codec's: MPEG-TS and raw elementary
/// streams carry Annex B, ISO-BMFF and Matroska carry length prefixes, and the
/// same SPS parses identically out of either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// H.264 Annex B: units separated by `00 00 01` or `00 00 00 01`.
    AnnexB,
    /// ISO/IEC 14496-15: each unit preceded by a big-endian length.
    LengthPrefixed(LengthSize),
}

impl Framing {
    /// Length-prefixed framing with a width in bytes, if the width is legal.
    #[must_use]
    pub const fn length_prefixed(bytes: u8) -> Option<Self> {
        match LengthSize::new(bytes) {
            Some(s) => Some(Self::LengthPrefixed(s)),
            None => None,
        }
    }

    /// The length-prefix width, or `None` for Annex B.
    ///
    /// `ffprobe` prints this as `nal_length_size`, and `0` there means Annex B.
    #[must_use]
    pub const fn length_size(self) -> Option<LengthSize> {
        match self {
            Self::AnnexB => None,
            Self::LengthPrefixed(s) => Some(s),
        }
    }

    /// Whether this is the length-prefixed form — `ffprobe`'s `is_avc`.
    #[must_use]
    pub const fn is_length_prefixed(self) -> bool {
        matches!(self, Self::LengthPrefixed(_))
    }
}

/// One NAL unit, located in the buffer it came from.
///
/// `data` is **EBSP**: the bytes exactly as they appear in the stream, with
/// emulation-prevention bytes still in place and the NAL header as its first
/// byte. Use [`RbspBuf`](crate::RbspBuf) to get the RBSP a bit reader can parse.
///
/// The two extra fields are what [`vaco_bitstream::annexb::nal_units`] cannot
/// give and every caller has to recompute otherwise: `offset` is where `data`
/// begins in the source buffer, which a demuxer needs for `Packet::pos` and a
/// parser needs to report consumed bytes; `start_code_len` is what has to be
/// re-emitted when converting framings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nal<'a> {
    /// The unit's bytes, header included, emulation prevention intact.
    pub data: &'a [u8],
    /// Byte offset of `data` within the buffer it was found in.
    pub offset: usize,
    /// Bytes of start code immediately before `offset`: 3 or 4 for Annex B, 0
    /// for length-prefixed (where the prefix width is the framing's).
    pub start_code_len: u8,
}

impl Nal<'_> {
    /// The unit's first byte, which is the NAL header in every H.26x codec.
    #[must_use]
    pub fn header_byte(&self) -> Option<u8> {
        self.data.first().copied()
    }

    /// Bytes this unit occupies in the source, delimiter included.
    ///
    /// For a length-prefixed unit the caller must add the prefix width itself,
    /// since `start_code_len` is 0 there — the prefix belongs to the framing,
    /// not to the unit.
    #[must_use]
    pub const fn framed_len(&self) -> usize {
        self.start_code_len as usize + self.data.len()
    }

    /// Where this unit ends in the source buffer.
    #[must_use]
    pub const fn end(&self) -> usize {
        self.offset.saturating_add(self.data.len())
    }
}

/// Iterate the NAL units of `buf` under `framing`.
///
/// Both arms terminate on every input: each step advances the cursor past at
/// least one delimiter byte.
///
/// # Annex B
///
/// Agrees exactly with [`vaco_bitstream::annexb::nal_units`] on the yielded
/// slices — empty units skipped, `trailing_zero_8bits` trimmed — and adds the
/// position information. `tests/agreement.rs` asserts the equality, and the
/// `nalu_framing` fuzz target asserts it on arbitrary bytes.
///
/// # Length-prefixed
///
/// Agrees exactly with [`vaco_bitstream::avcc::nal_units`]: a prefix that does
/// not fit, or that declares more bytes than remain, ends the iteration rather
/// than yielding a truncated unit. A truncated length field is a structural
/// error, not a short read.
#[must_use]
pub fn units(buf: &[u8], framing: Framing) -> NalUnits<'_> {
    match framing {
        Framing::AnnexB => NalUnits::AnnexB(AnnexBUnits {
            buf,
            next: annexb::find_start_code(buf, 0).map(|i| i + 3),
        }),
        Framing::LengthPrefixed(size) => NalUnits::Length(LengthUnits {
            reader: ByteReader::new(buf),
            size,
            done: false,
        }),
    }
}

/// The iterator [`units`] returns.
#[derive(Debug, Clone)]
pub enum NalUnits<'a> {
    /// Annex B byte stream.
    AnnexB(AnnexBUnits<'a>),
    /// Length-prefixed sample.
    Length(LengthUnits<'a>),
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = Nal<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::AnnexB(i) => i.next(),
            Self::Length(i) => i.next(),
        }
    }
}

/// Annex-B iteration with positions. Obtained from [`units`].
#[derive(Debug, Clone)]
pub struct AnnexBUnits<'a> {
    buf: &'a [u8],
    /// Start of the current unit's payload, or `None` once exhausted.
    next: Option<usize>,
}

impl<'a> Iterator for AnnexBUnits<'a> {
    type Item = Nal<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let start = self.next?;
            let (end, next) = match annexb::find_start_code(self.buf, start) {
                Some(sc) => (sc, Some(sc + 3)),
                None => (self.buf.len(), None),
            };
            self.next = next;
            let unit = self.buf.get(start..end).unwrap_or(&[]);
            // `trailing_zero_8bits`, which also absorbs the leading zero of a
            // four-byte start code belonging to the *next* unit.
            let trimmed = match unit.iter().rposition(|&b| b != 0) {
                Some(last) => unit.get(..=last).unwrap_or(&[]),
                None => &[],
            };
            if !trimmed.is_empty() {
                // A start code is three bytes; a fourth zero in front of it is
                // the `zero_byte` of Annex B §B.1.1.
                let four = start >= 4 && self.buf.get(start - 4) == Some(&0);
                return Some(Nal {
                    data: trimmed,
                    offset: start,
                    start_code_len: if four { 4 } else { 3 },
                });
            }
            self.next?;
        }
    }
}

/// Length-prefixed iteration with positions. Obtained from [`units`].
#[derive(Debug, Clone)]
pub struct LengthUnits<'a> {
    reader: ByteReader<'a>,
    size: LengthSize,
    done: bool,
}

impl<'a> Iterator for LengthUnits<'a> {
    type Item = Nal<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.reader.remaining() < self.size.len() {
            return None;
        }
        let len = match self.size.get() {
            1 => u32::from(self.reader.u8()),
            2 => u32::from(self.reader.be16()),
            _ => self.reader.be32(),
        } as usize;
        if len == 0 || len > self.reader.remaining() {
            self.done = true;
            return None;
        }
        let offset = self.reader.pos();
        Some(Nal {
            data: self.reader.bytes(len),
            offset,
            start_code_len: 0,
        })
    }
}
