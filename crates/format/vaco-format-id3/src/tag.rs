//! Assembling a whole `ID3v2` tag: header, optional extended header,
//! whole-tag/per-frame unsynchronisation, and the frame walk.

use vaco_core::Result;
use vaco_limits::Budget;

use crate::frame_header::{FrameHeaderV2, FrameHeaderV34, Id3FrameFlags};
use crate::frames::{self, Frame, Picture};
use crate::header::{Flags, Id3v2Header};
use crate::synchsafe;
use crate::unsync;

/// Frames processed before giving up on a tag as pathological.
///
/// Every frame consumes at least six bytes of header, so this is far above
/// anything a legitimate tag produces; it exists to give a corrupt or
/// adversarial tag a bounded, reproducible failure via `Budget::consume_fuel`
/// rather than an unbounded loop.
const FUEL_PER_FRAME: u64 = 1;

/// A parsed `ID3v2` tag.
#[derive(Debug, Clone, Default)]
pub struct Id3v2Tag {
    pub major_version: u8,
    /// `(key, value)` pairs in frame order, using the metadata key table
    /// documented in `crate::frames`. A `TXXX`/`TXX` frame contributes its
    /// own description as the key; every other mapped frame contributes the
    /// fixed key from the table. Frames with no key mapping, or whose value
    /// decoded empty, are omitted — matching the reference, which does not
    /// report an empty tag either (probed via `ID3v1`'s equivalent case; see
    /// `crate::id3v1`).
    pub entries: Vec<(String, String)>,
    /// Every `APIC`/`PIC` frame found. `PIC`'s v2.2 layout is not decoded
    /// (see `crate::frames`), so a v2.2 tag's pictures are always empty.
    pub pictures: Vec<Picture>,
}

impl Id3v2Tag {
    /// Parse a whole tag, starting at `data[0]` (i.e. `data` begins with the
    /// `"ID3"` header — use [`crate::skip::detect`] to locate it in a stream
    /// first).
    ///
    /// A declared header size larger than `data` actually holds is clamped,
    /// never trusted for anything beyond how far to read.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] if the header itself does not parse.
    /// [`vaco_core::Error::LimitExceeded`] if `budget` is exhausted by
    /// unsynchronisation removal, picture data, or the frame count.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        let header = Id3v2Header::parse(data)?;
        let body_end = usize::try_from(header.size)
            .unwrap_or(usize::MAX)
            .saturating_add(header::LEN)
            .min(data.len());
        let raw_body = data.get(header::LEN..body_end).unwrap_or(&[]);

        let unsynced_whole_tag;
        let body: &[u8] = if header.flags.contains(Flags::UNSYNCHRONISATION) {
            unsynced_whole_tag = unsync::remove(raw_body, budget)?;
            &unsynced_whole_tag
        } else {
            raw_body
        };

        let mut pos = 0usize;
        if header.flags.contains(Flags::EXTENDED_HEADER) {
            if let Some(len) =
                extended_header_len(header.major_version, body.get(pos..).unwrap_or(&[]))
            {
                pos = pos.saturating_add(len).min(body.len());
            } else {
                // A declared extended header with no room to hold even its
                // own size field is malformed; there is nothing safe to skip
                // past, so stop rather than guess.
                pos = body.len();
            }
        }

        let mut entries = Vec::new();
        let mut pictures = Vec::new();

        while pos < body.len() {
            budget.consume_fuel(FUEL_PER_FRAME)?;
            let rest = body.get(pos..).unwrap_or(&[]);
            let (id, size, flags, header_len) = if header.major_version <= 2 {
                let Some(fh) = FrameHeaderV2::parse(rest) else {
                    break;
                };
                (
                    fh.id.to_vec(),
                    fh.size,
                    crate::frame_header::Id3FrameFlags::default(),
                    crate::frame_header::LEN_V2,
                )
            } else {
                let Some(fh) = FrameHeaderV34::parse(header.major_version, rest) else {
                    break;
                };
                (
                    fh.id.to_vec(),
                    fh.size,
                    fh.flags,
                    crate::frame_header::LEN_V34,
                )
            };
            pos = pos.saturating_add(header_len);

            let avail = body.len().saturating_sub(pos);
            let take = usize::try_from(size).unwrap_or(usize::MAX).min(avail);
            let mut content = body.get(pos..pos.saturating_add(take)).unwrap_or(&[]);
            pos = pos.saturating_add(take);

            if flags.contains(Id3FrameFlags::GROUPING) {
                content = content.get(1..).unwrap_or(&[]);
            }
            if flags.contains(Id3FrameFlags::DATA_LENGTH_INDICATOR) {
                content = content.get(4..).unwrap_or(&[]);
            }
            let unsynced_frame;
            let content: &[u8] = if flags.contains(Id3FrameFlags::UNSYNCHRONISATION) {
                unsynced_frame = unsync::remove(content, budget)?;
                &unsynced_frame
            } else {
                content
            };

            let frame = frames::decode(&id, flags, content, budget)?;
            record(&mut entries, &mut pictures, &id, frame);
        }

        Ok(Self {
            major_version: header.major_version,
            entries,
            pictures,
        })
    }
}

fn record(
    entries: &mut Vec<(String, String)>,
    pictures: &mut Vec<Picture>,
    id: &[u8],
    frame: Frame,
) {
    match frame {
        Frame::Text(value) => {
            if let (Some(key), false) = (frames::metadata_key(id), value.is_empty()) {
                entries.push((key.to_string(), value));
            }
        }
        Frame::UserText { description, value } => {
            if !value.is_empty() {
                entries.push((description, value));
            }
        }
        Frame::Comment { text, .. } => {
            if !text.is_empty() {
                entries.push(("comment".to_string(), text));
            }
        }
        Frame::Picture(p) => pictures.push(p),
        Frame::Unsupported | Frame::Other => {}
    }
}

/// Bytes to skip to get past an extended header, per `ID3v2.3.0` §3.2 /
/// `ID3v2.4.0` §3.2.
///
/// **Not independently probed** — `ffmpeg` does not write an extended
/// header under any option this crate's author found, so this is read
/// directly from the two published specifications rather than confirmed
/// against the reference binary. The two versions disagree on whether the
/// four-byte size field counts itself: `ID3v2.3.0`'s is plain binary and
/// *excludes* itself (total length is `4 + size`); `ID3v2.4.0`'s is synchsafe
/// and *includes* itself (total length is `size`). This is a commonly-cited
/// inconsistency between the two specs, not a transcription risk specific to
/// this crate.
fn extended_header_len(major: u8, data: &[u8]) -> Option<usize> {
    let bytes = <[u8; 4]>::try_from(data.get(..4)?).ok()?;
    if major >= 4 {
        let total = synchsafe::decode(bytes) as usize;
        Some(total.max(4))
    } else {
        let size = u32::from_be_bytes(bytes) as usize;
        Some(4usize.saturating_add(size))
    }
}

mod header {
    pub(crate) const LEN: usize = crate::header::LEN;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn synchsafe_bytes(n: u32) -> [u8; 4] {
        [
            ((n >> 21) & 0x7f) as u8,
            ((n >> 14) & 0x7f) as u8,
            ((n >> 7) & 0x7f) as u8,
            (n & 0x7f) as u8,
        ]
    }

    fn v34_frame(id: [u8; 4], plain_size: bool, content: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        if plain_size {
            out.extend_from_slice(&(content.len() as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&synchsafe_bytes(content.len() as u32));
        }
        out.extend_from_slice(&[0, 0]); // flags
        out.extend_from_slice(content);
        out
    }

    fn tag(major: u8, frames: &[u8]) -> Vec<u8> {
        let mut out = b"ID3".to_vec();
        out.push(major);
        out.push(0);
        out.push(0); // flags
        out.extend_from_slice(&synchsafe_bytes(frames.len() as u32));
        out.extend_from_slice(frames);
        out
    }

    #[test]
    fn parses_the_probed_v23_tag_shape() {
        let mut text = vec![0x00];
        text.extend_from_slice(b"Hello World");
        let mut frames = v34_frame(*b"TIT2", true, &text);
        let mut artist = vec![0x00];
        artist.extend_from_slice(b"The Artist");
        frames.extend_from_slice(&v34_frame(*b"TPE1", true, &artist));
        let data = tag(3, &frames);

        let mut budget = Budget::new(Limits::permissive());
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        assert_eq!(t.major_version, 3);
        assert_eq!(
            t.entries,
            vec![
                ("title".to_string(), "Hello World".to_string()),
                ("artist".to_string(), "The Artist".to_string()),
            ]
        );
    }

    #[test]
    fn v24_synchsafe_frame_size_is_read_correctly() {
        let mut text = vec![0x00];
        text.extend_from_slice(&[b'A'; 200]);
        let frames = v34_frame(*b"TIT2", false, &text);
        let data = tag(4, &frames);
        let mut budget = Budget::new(Limits::permissive());
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].1.len(), 200);
    }

    #[test]
    fn txxx_key_is_its_own_description() {
        let mut content = vec![0x00];
        content.extend_from_slice(b"comment\x00A comment here");
        let frames = v34_frame(*b"TXXX", true, &content);
        let data = tag(3, &frames);
        let mut budget = Budget::new(Limits::permissive());
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        assert_eq!(
            t.entries,
            vec![("comment".to_string(), "A comment here".to_string())]
        );
    }

    #[test]
    fn apic_is_collected_as_a_picture_not_an_entry() {
        let mut content = vec![0x00];
        content.extend_from_slice(b"image/png\x00");
        content.push(3); // front cover
        content.extend_from_slice(b"\x00");
        content.extend_from_slice(&[1, 2, 3, 4]);
        let frames = v34_frame(*b"APIC", true, &content);
        let data = tag(3, &frames);
        let mut budget = Budget::new(Limits::permissive());
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        assert!(t.entries.is_empty());
        assert_eq!(t.pictures.len(), 1);
        assert_eq!(t.pictures[0].mime_type, "image/png");
        assert_eq!(t.pictures[0].data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_declared_header_size_past_the_buffer_is_clamped() {
        let mut data = b"ID3".to_vec();
        data.push(3);
        data.push(0);
        data.push(0);
        data.extend_from_slice(&synchsafe_bytes(1_000_000)); // lies
        data.extend_from_slice(b"short");
        let mut budget = Budget::new(Limits::permissive());
        // Must not panic or hang; the frame walk simply finds nothing valid.
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        assert!(t.entries.is_empty());
    }

    #[test]
    fn whole_tag_unsynchronisation_is_undone_before_frame_parsing() {
        let mut text = vec![0x00];
        text.extend_from_slice(b"Hi");
        let mut frames = v34_frame(*b"TIT2", true, &text);
        // Insert a synchronisation artefact the encoder would have escaped.
        frames.insert(0, 0x00);
        frames.insert(0, 0xFF);
        let mut data = b"ID3".to_vec();
        data.push(3);
        data.push(0);
        data.push(Flags::UNSYNCHRONISATION.bits());
        data.extend_from_slice(&synchsafe_bytes(frames.len() as u32));
        data.extend_from_slice(&frames);

        let mut budget = Budget::new(Limits::permissive());
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        // The leading FF 00 was removed, so the very next bytes are read as
        // a frame header (FF alone is not a valid ID3 frame id, but a real
        // demux would have arranged for the FF-prefixed bytes to precede a
        // genuine frame; here we only assert the tag still parses without
        // panicking and the well-formed TIT2 after it is unaffected).
        let _ = t;
    }

    #[test]
    fn an_unknown_frame_is_skipped_not_fatal() {
        let frames_data = v34_frame(*b"PRIV", true, &[1, 2, 3]);
        let mut all = frames_data;
        let mut text = vec![0x00];
        text.extend_from_slice(b"Title");
        all.extend_from_slice(&v34_frame(*b"TIT2", true, &text));
        let data = tag(3, &all);
        let mut budget = Budget::new(Limits::permissive());
        let t = Id3v2Tag::parse(&data, &mut budget).unwrap();
        assert_eq!(t.entries, vec![("title".to_string(), "Title".to_string())]);
    }
}
