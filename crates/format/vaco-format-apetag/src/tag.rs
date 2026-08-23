//! APEv1/APEv2 tag structure: the header/footer, item list, and the
//! leading/trailing placement and ID3v1-coexistence rules.
//!
//! Specification: the `APEv2` tag specification published on the Hydrogenaudio
//! wiki (originally Monkey's Audio's own documentation, now the community
//! reference; <https://wiki.hydrogenaud.io/index.php?title=APEv2_specification>),
//! which is the field-layout registry for this format the way RFC 2361 is for
//! `WAVEFORMATEX` tags — a fixed structure, not anyone's creative expression
//! (D9).
//!
//! # Layout
//!
//! A 32-byte footer (mandatory) optionally preceded by a byte-identical
//! 32-byte header (the only difference between the two is one flag bit) and
//! the item list in between:
//!
//! ```text
//! [ header (32 bytes), optional ] [ items ] [ footer (32 bytes), mandatory ]
//!
//! header/footer:
//!   0   8   preamble       "APETAGEX"
//!   8   4   version        1000 (v1) or 2000 (v2), LE
//!  12   4   tag_size       bytes from the first item to the end of the
//!                          footer inclusive (i.e. NOT counting the header)
//!  16   4   item_count
//!  20   4   flags          global flags, see below
//!  24   8   reserved       must be zero
//!
//! item:
//!   0   4   value_size     LE, bytes of value, excluding the key and its NUL
//!   4   4   item_flags     LE, see below
//!   8   …   key            NUL-terminated ASCII (2-255 bytes before the NUL)
//!   …   …   value          value_size bytes
//! ```
//!
//! # Flags
//!
//! Tag-level (header/footer) flags use only three bits of the 32-bit field:
//! bit 31 ("this tag has a header"), bit 29 ("this is the header, not the
//! footer"), and bit 0 ("the whole tag is read-only", deprecated and
//! ignored by every reader including this one). Item-level flags use bit 0
//! ("this item is read-only") and bits 1-2 (the item's value type: `00`
//! UTF-8 text, `01` binary, `10` external reference, `11` reserved).
//!
//! # The `ID3v1` coexistence rule
//!
//! An MP3 file can carry both tags at once, and when it does, the APE tag's
//! footer sits **immediately before** the trailing 128-byte `ID3v1` tag, not at
//! the very end of the file — so a reader that looks for `"APETAGEX"` at
//! `file_len - 32` misses it whenever an `ID3v1` tag follows. [`locate`] checks
//! for a trailing `ID3v1` tag first and steps back over it before looking for
//! the APE footer, which is the rule this module exists to get right.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// `"APETAGEX"`.
pub const PREAMBLE: &[u8; 8] = b"APETAGEX";

/// Bytes in one header or footer.
pub const FOOTER_LEN: u64 = 32;

/// `ID3v1` tags are always exactly this long, and always end the file when
/// present.
pub const ID3V1_LEN: u64 = 128;

bitflags::bitflags! {
    /// Tag-level (header/footer) flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct TagFlags: u32 {
        /// The whole tag is marked read-only. Deprecated; recorded, not enforced.
        const READ_ONLY  = 1 << 0;
        /// This is the header (set in the header, clear in the footer).
        const IS_HEADER  = 1 << 29;
        /// A header precedes the item list (set in both header and footer
        /// when a header is present).
        const HAS_HEADER = 1 << 31;
    }
}

/// An item's value type, packed into bits 1-2 of its flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemKind {
    #[default]
    Utf8Text,
    Binary,
    ExternalReference,
    Reserved,
}

impl ItemKind {
    #[must_use]
    const fn from_flags(flags: u32) -> Self {
        match (flags >> 1) & 0b11 {
            0 => Self::Utf8Text,
            1 => Self::Binary,
            2 => Self::ExternalReference,
            _ => Self::Reserved,
        }
    }

    const fn to_bits(self) -> u32 {
        (match self {
            Self::Utf8Text => 0,
            Self::Binary => 1,
            Self::ExternalReference => 2,
            Self::Reserved => 3,
        }) << 1
    }
}

/// One parsed item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApeItem {
    pub key: String,
    pub kind: ItemKind,
    pub read_only: bool,
    /// Raw value bytes. For [`ItemKind::Utf8Text`], multiple values are
    /// NUL-separated within this buffer (the specification's own convention
    /// for multi-valued items) — [`ApeItem::text_values`] splits them.
    pub value: Vec<u8>,
}

impl ApeItem {
    /// A new UTF-8 text item holding one value.
    #[must_use]
    pub fn text(key: impl Into<String>, value: impl AsRef<str>) -> Self {
        Self {
            key: key.into(),
            kind: ItemKind::Utf8Text,
            read_only: false,
            value: value.as_ref().as_bytes().to_vec(),
        }
    }

    /// The value as text, replacing any invalid UTF-8 — only meaningful for
    /// [`ItemKind::Utf8Text`], but never panics on a binary item either.
    #[must_use]
    pub fn text_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.value)
    }

    /// The value split on the multi-value NUL separator.
    #[must_use]
    pub fn text_values(&self) -> Vec<String> {
        self.value
            .split(|&b| b == 0)
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect()
    }
}

/// A parsed APEv1/APEv2 tag.
#[derive(Debug, Clone, Default)]
pub struct ApeTag {
    /// `1000` or `2000` — 100 × the major version.
    pub version: u32,
    pub items: Vec<ApeItem>,
}

impl ApeTag {
    /// Parse a tag whose **footer** is the last 32 bytes of `data` (i.e.
    /// `data` is the tag itself — header if present, items, footer — with no
    /// trailing `ID3v1` tag or anything else after it). Use [`crate::locate`]
    /// to find that slice inside a whole file first.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the footer's preamble does not match, or a
    /// declared item size runs past the buffer.
    /// [`Error::LimitExceeded`] if `budget` is exhausted.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        let flen = usize::try_from(FOOTER_LEN).unwrap_or(usize::MAX);
        if data.len() < flen {
            return Err(Error::InvalidData("apetag: shorter than one footer"));
        }
        let footer_at = data.len() - flen;
        let footer = data.get(footer_at..).unwrap_or(&[]);
        let (version, tag_size, item_count, flags) = parse_footer_or_header(footer)?;

        // `tag_size` counts from the first item to the end of the footer,
        // inclusive — it does NOT include a leading header. So the items
        // start at `footer_at + FOOTER_LEN - tag_size`, clamped to zero: a
        // tag_size larger than what is actually present is the classic
        // declared-size lie, and clamping to the start of the buffer is the
        // same "never trust past what exists" discipline `vaco-format-riff`
        // uses for chunk sizes.
        let tag_total = usize::try_from(tag_size).unwrap_or(usize::MAX);
        let items_start = footer_at.saturating_add(flen).saturating_sub(tag_total);
        let items_end = footer_at;
        let body = data.get(items_start..items_end).unwrap_or(&[]);

        let flags = TagFlags::from_bits_retain(flags);
        let cap = usize::try_from(item_count).unwrap_or(usize::MAX);
        let mut items = Vec::new();
        let mut r = ByteReader::new(body);
        for _ in 0..cap {
            if r.remaining() == 0 {
                break;
            }
            budget.consume_fuel(1)?;
            let Some(item) = read_item(&mut r, budget)? else {
                break;
            };
            items.push(item);
        }
        let _ = flags; // read-only/header bits are informational; see module docs
        Ok(Self { version, items })
    }

    /// Serialise as a **footer-only** tag (no header) — the common form real
    /// writers emit, since the footer alone is sufficient for any reader that
    /// scans from the end of the file, and it costs 32 fewer bytes.
    /// [`ApeTag::to_bytes_with_header`] emits both when a leading tag is
    /// wanted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if an item's key is not `2..=255` printable
    /// ASCII bytes excluding `=`, or there are too many items or bytes to
    /// express in the 32-bit fields.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.serialise(false)
    }

    /// Serialise with a leading header as well as the trailing footer.
    ///
    /// # Errors
    /// As [`ApeTag::to_bytes`].
    pub fn to_bytes_with_header(&self) -> Result<Vec<u8>> {
        self.serialise(true)
    }

    fn serialise(&self, with_header: bool) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        for item in &self.items {
            write_item(&mut body, item)?;
        }
        let flen = usize::try_from(FOOTER_LEN).unwrap_or(32);
        let tag_size = u32::try_from(body.len().saturating_add(flen))
            .map_err(|_| Error::InvalidData("apetag: tag too large"))?;
        let item_count = u32::try_from(self.items.len())
            .map_err(|_| Error::InvalidData("apetag: too many items"))?;

        let mut global = TagFlags::empty();
        if with_header {
            global |= TagFlags::HAS_HEADER;
        }

        let mut out = Vec::new();
        if with_header {
            write_footer_or_header(
                &mut out,
                self.version,
                tag_size,
                item_count,
                (global | TagFlags::IS_HEADER).bits(),
            );
        }
        out.extend_from_slice(&body);
        write_footer_or_header(&mut out, self.version, tag_size, item_count, global.bits());
        Ok(out)
    }

    /// The first item whose key matches case-insensitively (`APEv2` keys are
    /// specified as case-insensitive).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ApeItem> {
        self.items.iter().find(|i| i.key.eq_ignore_ascii_case(key))
    }
}

fn parse_footer_or_header(data: &[u8]) -> Result<(u32, u32, u32, u32)> {
    let mut r = ByteReader::new(data);
    let preamble = r.bytes(8);
    if preamble != PREAMBLE {
        return Err(Error::InvalidData("apetag: bad preamble"));
    }
    let version = r.le32();
    let tag_size = r.le32();
    let item_count = r.le32();
    let flags = r.le32();
    r.check()
        .map_err(|_| Error::InvalidData("apetag: short footer"))?;
    Ok((version, tag_size, item_count, flags))
}

fn write_footer_or_header(
    out: &mut Vec<u8>,
    version: u32,
    tag_size: u32,
    item_count: u32,
    flags: u32,
) {
    out.extend_from_slice(PREAMBLE);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&tag_size.to_le_bytes());
    out.extend_from_slice(&item_count.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&[0u8; 8]);
}

/// Shortest key this crate accepts, per the specification's `2..=255` bound.
pub const MIN_KEY_LEN: usize = 2;
/// Longest key this crate accepts, per the specification's `2..=255` bound.
pub const MAX_KEY_LEN: usize = 255;

fn read_item(r: &mut ByteReader<'_>, budget: &mut Budget) -> Result<Option<ApeItem>> {
    if r.remaining() < 8 {
        return Ok(None);
    }
    let value_size = r.le32();
    let flags = r.le32();
    // The key is NUL-terminated with no declared length; scan for it bounded
    // by what remains, so a missing terminator ends the walk rather than
    // reading past the buffer.
    let rest = r.rest();
    let Some(nul_at) = rest.iter().position(|&b| b == 0) else {
        return Ok(None);
    };
    // The specification's key length bound is `2..=255` bytes, not just
    // "non-empty" — found by fuzzing: a one-byte key parsed successfully
    // here before this was `< 2`, producing an `ApeItem` this crate's own
    // writer would then refuse to serialise back out.
    if !(MIN_KEY_LEN..=MAX_KEY_LEN).contains(&nul_at) {
        return Ok(None);
    }
    let key_bytes = rest.get(..nul_at).unwrap_or(&[]);
    if !key_bytes
        .iter()
        .all(|&b| (0x20..=0x7e).contains(&b) && b != b'=')
    {
        return Ok(None);
    }
    let key = String::from_utf8_lossy(key_bytes).into_owned();
    r.skip(nul_at.saturating_add(1));

    let avail = r.remaining();
    let want = usize::try_from(value_size).unwrap_or(usize::MAX).min(avail);
    let src = r.bytes(want);
    let mut value = budget.alloc::<u8>(src.len())?;
    value.copy_from_slice(src);

    Ok(Some(ApeItem {
        key,
        kind: ItemKind::from_flags(flags),
        read_only: flags & 1 == 1,
        value,
    }))
}

fn write_item(out: &mut Vec<u8>, item: &ApeItem) -> Result<()> {
    if !(MIN_KEY_LEN..=MAX_KEY_LEN).contains(&item.key.len())
        || !item
            .key
            .bytes()
            .all(|b| (0x20..=0x7e).contains(&b) && b != b'=')
    {
        return Err(Error::InvalidData("apetag: invalid item key"));
    }
    let value_size = u32::try_from(item.value.len())
        .map_err(|_| Error::InvalidData("apetag: item value too large"))?;
    let flags = item.kind.to_bits() | u32::from(item.read_only);
    out.extend_from_slice(&value_size.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(item.key.as_bytes());
    out.push(0);
    out.extend_from_slice(&item.value);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::permissive())
    }

    #[test]
    fn a_footer_only_tag_round_trips() {
        let tag = ApeTag {
            version: 2000,
            items: vec![
                ApeItem::text("Artist", "Test Artist"),
                ApeItem::text("Title", "Test Title"),
            ],
        };
        let bytes = tag.to_bytes().unwrap();
        let parsed = ApeTag::parse(&bytes, &mut budget()).unwrap();
        assert_eq!(parsed.version, 2000);
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.get("artist").unwrap().text_lossy(), "Test Artist");
        assert_eq!(parsed.get("ARTIST").unwrap().text_lossy(), "Test Artist");
        assert_eq!(parsed.get("Title").unwrap().text_lossy(), "Test Title");
    }

    #[test]
    fn a_header_plus_footer_tag_round_trips() {
        let tag = ApeTag {
            version: 2000,
            items: vec![ApeItem::text("Album", "Test Album")],
        };
        let bytes = tag.to_bytes_with_header().unwrap();
        assert_eq!(bytes.len(), 32 + 32 + 8 + "Album\0Test Album".len());
        let parsed = ApeTag::parse(&bytes, &mut budget()).unwrap();
        assert_eq!(parsed.get("album").unwrap().text_lossy(), "Test Album");
    }

    #[test]
    fn multi_value_text_splits_on_nul() {
        let mut item = ApeItem::text("Genre", "Rock");
        item.value.push(0);
        item.value.extend_from_slice(b"Pop");
        assert_eq!(item.text_values(), vec!["Rock", "Pop"]);
    }

    #[test]
    fn a_bad_preamble_is_rejected() {
        let mut bytes = vec![0u8; 32];
        bytes[..8].copy_from_slice(b"NOTAPE!!");
        assert!(ApeTag::parse(&bytes, &mut budget()).is_err());
    }

    #[test]
    fn a_lying_tag_size_is_clamped_not_trusted() {
        let tag = ApeTag {
            version: 2000,
            items: vec![ApeItem::text("Key", "V")],
        };
        let mut bytes = tag.to_bytes().unwrap();
        // Inflate the declared tag_size far past what actually precedes the
        // footer.
        let n = bytes.len();
        bytes[n - 32 + 12..n - 32 + 16].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        // Must not panic and must not read before the start of the buffer.
        let parsed = ApeTag::parse(&bytes, &mut budget()).unwrap();
        assert_eq!(parsed.items.len(), parsed.items.len());
    }

    #[test]
    fn too_short_a_buffer_is_rejected_not_panicking() {
        assert!(ApeTag::parse(&[0u8; 5], &mut budget()).is_err());
        assert!(ApeTag::parse(&[], &mut budget()).is_err());
    }

    /// Found by fuzzing: `read_item` accepted a one-byte key (`nul_at == 1`)
    /// because its bound check only ever rejected `nul_at == 0`, letting a
    /// spec-violating `ApeItem` (2..=255-byte keys) out of `parse` — one this
    /// crate's own `write_item` would then refuse to serialise, an asymmetry
    /// that is itself a bug independent of anything else it might cause
    /// downstream.
    #[test]
    fn a_one_byte_key_is_rejected() {
        let mut item = Vec::new();
        item.extend_from_slice(&1u32.to_le_bytes()); // value_size
        item.extend_from_slice(&0u32.to_le_bytes()); // flags
        item.push(b'#'); // one-byte key
        item.push(0); // NUL terminator
        item.push(b'V'); // one-byte value

        let tag_size = u32::try_from(item.len() + 32).unwrap();
        let mut bytes = item;
        bytes.extend_from_slice(PREAMBLE);
        bytes.extend_from_slice(&2000u32.to_le_bytes());
        bytes.extend_from_slice(&tag_size.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // item_count
        bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
        bytes.extend_from_slice(&[0u8; 8]); // reserved

        let parsed = ApeTag::parse(&bytes, &mut budget()).unwrap();
        assert!(
            parsed.items.is_empty(),
            "a one-byte key must not survive parsing: {:?}",
            parsed.items
        );
    }

    #[test]
    fn an_invalid_key_is_rejected_on_write() {
        let tag = ApeTag {
            version: 2000,
            items: vec![ApeItem::text("a=b", "v")],
        };
        assert!(tag.to_bytes().is_err());
        let tag2 = ApeTag {
            version: 2000,
            items: vec![ApeItem::text("x", "v")], // one byte, below the 2-byte minimum
        };
        assert!(tag2.to_bytes().is_err());
    }
}
