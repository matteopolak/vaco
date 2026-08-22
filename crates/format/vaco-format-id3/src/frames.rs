//! Frame content: text values, `TXXX`, `COMM`, `APIC`, and the frame-ID →
//! metadata-key table that turns them into the `(key, value)` pairs a
//! container reports.
//!
//! # Scope
//!
//! `ID3v2` defines several dozen frame types (`ID3v2.4.0` §4 alone lists over
//! seventy). This crate decodes the ones that carry metadata a demuxer
//! reports as a stream/format tag, confirmed against `ffprobe` 8.1's own
//! `TAG:<key>` output (`ffmpeg -metadata <key>=<value> -id3v2_version 3|4
//! -c:a mp3 out.mp3`, then `ffprobe -show_entries format_tags`):
//!
//! | Frame | Key | v2.2 alias |
//! |---|---|---|
//! | `TIT2` | `title` | `TT2` |
//! | `TPE1` | `artist` | `TP1` |
//! | `TALB` | `album` | `TAL` |
//! | `TYER` | `date` | `TYE` |
//! | `TDRC` | `date` | — (v2.4 only; `TDRC` is what `TYER`/`TDAT`/`TIME` were folded into, per `ID3v2.4.0` §3.1; not independently probed the way `TYER` was, since ffmpeg's own `-id3v2_version 4` writer already emits `TDRC` for the same `-metadata date=` input and reads it back the same way) |
//! | `TRCK` | `track` | `TRK` |
//! | `TCON` | `genre` | `TCO` |
//! | `TPE2` | `album_artist` | `TP2` |
//! | `TCOM` | `composer` | `TCM` |
//! | `TPOS` | `disc` | `TPA` |
//! | `TPE3` | `performer` | `TP3` |
//! | `TSSE` | `encoder` | — |
//! | `COMM` | `comment` | `COM` |
//! | `TXXX` | *(the frame's own description field)* | `TXX` |
//!
//! `APIC` (`PIC` in v2.2) is not a text key at all — probed, it becomes a
//! second, `attached_pic`-disposition stream in the reference, which is a
//! demuxer-level decision this crate does not make. [`Picture`] exposes the
//! decoded structure instead; see [`Frame::Picture`].
//!
//! **Deliberately not covered**: lyrics (`USLT`), synchronised lyrics
//! (`SYLT`), play counters, private frames, URL frames, and the rest of the
//! `ID3v2.4.0` §4 list. A caller that needs one of these should add it the
//! same way — a probed key mapping and a unit test pinning it — rather than
//! assume the omission is an oversight.
//!
//! **Compressed and encrypted frames are not decoded.** Both require a
//! capability this crate does not have (zlib inflation is unbounded without
//! its own separate `Budget` discipline; encryption additionally requires a
//! key `ID3v2` does not carry) — see [`Frame::Unsupported`].

use vaco_limits::Budget;

use crate::encoding::{self, Encoding};
use crate::frame_header::Id3FrameFlags;

/// One frame's decoded content.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// A plain text-information frame's (`T***`, except `TXXX`) value.
    Text(String),
    /// `TXXX`/`TXX`: a caller-defined key and its value.
    UserText { description: String, value: String },
    /// `COMM`/`COM`: an ISO-639-2 language code (verbatim, not text-decoded
    /// — it is not encoded text per the frame grammar), a short description,
    /// and the comment body.
    Comment {
        language: [u8; 3],
        description: String,
        text: String,
    },
    /// `APIC` (`PIC` in v2.2, whose different layout is not decoded — see
    /// the module docs).
    Picture(Picture),
    /// A frame this crate recognises structurally but does not interpret:
    /// compressed, encrypted, or an image frame in the v2.2 `PIC` layout.
    Unsupported,
    /// Any other frame ID. The raw content is not retained — see the module
    /// docs for what this deliberately excludes.
    Other,
}

/// A decoded `APIC` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// The MIME type string, e.g. `"image/jpeg"`. Always Latin-1 regardless
    /// of the frame's own encoding byte — `ID3v2.4.0` §4.14 fixes it so a
    /// reader never needs the picture's *own* text encoding to find where
    /// the MIME type ends.
    pub mime_type: String,
    /// `ID3v2.4.0` §4.14's picture-type byte: `3` is "Cover (front)", `0` is
    /// "Other", and so on. Not interpreted further here.
    pub picture_type: u8,
    pub description: String,
    /// The image bytes, verbatim. Charged to the caller's `Budget`.
    pub data: Vec<u8>,
}

/// The frame-ID → metadata-key table from the module docs, for `T***` and
/// `COMM`/`COM` frames whose value maps directly to one key. `TXXX`/`TXX`
/// and `APIC`/`PIC` are handled separately because their key (or absence of
/// one) is not a static string.
pub(crate) fn metadata_key(id: &[u8]) -> Option<&'static str> {
    match id {
        b"TIT2" | b"TT2" => Some("title"),
        b"TPE1" | b"TP1" => Some("artist"),
        b"TALB" | b"TAL" => Some("album"),
        b"TYER" | b"TYE" | b"TDRC" => Some("date"),
        b"TRCK" | b"TRK" => Some("track"),
        b"TCON" | b"TCO" => Some("genre"),
        b"TPE2" | b"TP2" => Some("album_artist"),
        b"TCOM" | b"TCM" => Some("composer"),
        b"TPOS" | b"TPA" => Some("disc"),
        b"TPE3" | b"TP3" => Some("performer"),
        b"TSSE" => Some("encoder"),
        _ => None,
    }
}

/// Whether `id` is a plain text-information frame (`T***`, but not the
/// user-defined `TXXX`/`TXX`, which has its own two-field layout).
fn is_plain_text_frame(id: &[u8]) -> bool {
    matches!(id.first(), Some(b'T')) && id != b"TXXX" && id != b"TXX"
}

/// Decode one frame's content, given its (already unsynchronised, already
/// past any group-identifier/data-length-indicator prefix — see
/// `crate::tag`) body.
///
/// `id` is the raw frame identifier (3 bytes for v2.2, 4 for v2.3/v2.4).
/// `budget` charges for the one case that copies input-sized data:
/// `APIC`/`PIC`'s image bytes.
///
/// # Errors
///
/// Only from `budget` running out while copying picture data; malformed
/// frame *content* (a truncated string, a missing terminator) degrades to a
/// best-effort decode rather than an error, matching every real ID3 reader.
pub fn decode(
    id: &[u8],
    flags: Id3FrameFlags,
    body: &[u8],
    budget: &mut Budget,
) -> vaco_core::Result<Frame> {
    if flags.contains(Id3FrameFlags::COMPRESSION) || flags.contains(Id3FrameFlags::ENCRYPTION) {
        return Ok(Frame::Unsupported);
    }
    if id == b"PIC" {
        // v2.2's picture frame uses a 3-byte image-format code instead of a
        // MIME string, and is otherwise laid out differently enough that
        // treating it as APIC would misparse it. Not decoded — see the
        // module docs.
        return Ok(Frame::Unsupported);
    }
    if id == b"APIC" {
        return decode_apic(body, budget).map(Frame::Picture);
    }
    if id == b"COMM" || id == b"COM" {
        return Ok(decode_comm(body));
    }
    if id == b"TXXX" || id == b"TXX" {
        return Ok(decode_txxx(body));
    }
    if is_plain_text_frame(id) {
        return Ok(decode_text(body));
    }
    Ok(Frame::Other)
}

fn decode_text(body: &[u8]) -> Frame {
    let Some((&enc_byte, rest)) = body.split_first() else {
        return Frame::Text(String::new());
    };
    let encoding = Encoding::from_byte(enc_byte).unwrap_or(Encoding::Latin1);
    Frame::Text(encoding::read_to_end(encoding, rest))
}

fn decode_txxx(body: &[u8]) -> Frame {
    let Some((&enc_byte, rest)) = body.split_first() else {
        return Frame::UserText {
            description: String::new(),
            value: String::new(),
        };
    };
    let encoding = Encoding::from_byte(enc_byte).unwrap_or(Encoding::Latin1);
    let (description, rest) = encoding::read_terminated(encoding, rest);
    let value = encoding::read_to_end(encoding, rest);
    Frame::UserText { description, value }
}

fn decode_comm(body: &[u8]) -> Frame {
    let Some((&enc_byte, rest)) = body.split_first() else {
        return Frame::Comment {
            language: [0; 3],
            description: String::new(),
            text: String::new(),
        };
    };
    let encoding = Encoding::from_byte(enc_byte).unwrap_or(Encoding::Latin1);
    let lang_bytes = rest.get(..3).unwrap_or(&[0; 3]);
    let language = <[u8; 3]>::try_from(lang_bytes).unwrap_or([0; 3]);
    let rest = rest.get(3..).unwrap_or(&[]);
    let (description, rest) = encoding::read_terminated(encoding, rest);
    let text = encoding::read_to_end(encoding, rest);
    Frame::Comment {
        language,
        description,
        text,
    }
}

fn decode_apic(body: &[u8], budget: &mut Budget) -> vaco_core::Result<Picture> {
    let Some((&enc_byte, rest)) = body.split_first() else {
        return Ok(Picture {
            mime_type: String::new(),
            picture_type: 0,
            description: String::new(),
            data: Vec::new(),
        });
    };
    let encoding = Encoding::from_byte(enc_byte).unwrap_or(Encoding::Latin1);
    // The MIME type is always Latin-1 (ID3v2.4.0 §4.14), independent of the
    // frame's own encoding byte, which governs only the description.
    let (mime_type, rest) = encoding::read_terminated(Encoding::Latin1, rest);
    let Some((&picture_type, rest)) = rest.split_first() else {
        return Ok(Picture {
            mime_type,
            picture_type: 0,
            description: String::new(),
            data: Vec::new(),
        });
    };
    let (description, rest) = encoding::read_terminated(encoding, rest);
    let mut data = budget.alloc::<u8>(rest.len())?;
    data.copy_from_slice(rest);
    Ok(Picture {
        mime_type,
        picture_type,
        description,
        data,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::permissive())
    }

    #[test]
    fn plain_text_frame_decodes_latin1() {
        let mut body = vec![0x00];
        body.extend_from_slice(b"Hello World");
        let f = decode(b"TIT2", Id3FrameFlags::default(), &body, &mut budget()).unwrap();
        assert_eq!(f, Frame::Text("Hello World".to_string()));
    }

    #[test]
    fn txxx_splits_description_and_value() {
        let mut body = vec![0x00];
        body.extend_from_slice(b"comment\x00A comment here");
        let f = decode(b"TXXX", Id3FrameFlags::default(), &body, &mut budget()).unwrap();
        assert_eq!(
            f,
            Frame::UserText {
                description: "comment".to_string(),
                value: "A comment here".to_string(),
            }
        );
    }

    #[test]
    fn comm_splits_language_description_and_text() {
        let mut body = vec![0x00];
        body.extend_from_slice(b"eng\x00Hello comment");
        let f = decode(b"COMM", Id3FrameFlags::default(), &body, &mut budget()).unwrap();
        assert_eq!(
            f,
            Frame::Comment {
                language: *b"eng",
                description: String::new(),
                text: "Hello comment".to_string(),
            }
        );
    }

    #[test]
    fn apic_round_trips_the_probed_layout() {
        // Byte-for-byte the APIC ffmpeg 8.1 writes for a PNG cover with
        // description "Cover": encoding=0, "image/png\0", type=0,
        // "Cover\0", then the raw PNG bytes.
        let mut body = vec![0x00];
        body.extend_from_slice(b"image/png\x00");
        body.push(0x00);
        body.extend_from_slice(b"Cover\x00");
        body.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let f = decode(b"APIC", Id3FrameFlags::default(), &body, &mut budget()).unwrap();
        let Frame::Picture(pic) = f else {
            panic!("expected a picture");
        };
        assert_eq!(pic.mime_type, "image/png");
        assert_eq!(pic.picture_type, 0);
        assert_eq!(pic.description, "Cover");
        assert_eq!(pic.data, b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn compressed_frames_are_unsupported_not_misdecoded() {
        let flags = Id3FrameFlags::COMPRESSION;
        let f = decode(b"TIT2", flags, &[0x00, 0xAB, 0xCD], &mut budget()).unwrap();
        assert_eq!(f, Frame::Unsupported);
    }

    #[test]
    fn encrypted_frames_are_unsupported_not_misdecoded() {
        let flags = Id3FrameFlags::ENCRYPTION;
        let f = decode(b"TPE1", flags, &[0x00, 0xAB], &mut budget()).unwrap();
        assert_eq!(f, Frame::Unsupported);
    }

    #[test]
    fn v22_picture_is_unsupported_not_misdecoded_as_apic() {
        let f = decode(
            b"PIC",
            Id3FrameFlags::default(),
            &[0x00, b'J', b'P', b'G'],
            &mut budget(),
        )
        .unwrap();
        assert_eq!(f, Frame::Unsupported);
    }

    #[test]
    fn an_empty_body_never_panics() {
        for id in [b"TIT2".as_slice(), b"TXXX", b"COMM", b"APIC", b"XXXX"] {
            assert!(decode(id, Id3FrameFlags::default(), &[], &mut budget()).is_ok());
        }
    }

    #[test]
    fn v22_aliases_map_to_the_same_keys_as_their_v23_names() {
        assert_eq!(metadata_key(b"TT2"), metadata_key(b"TIT2"));
        assert_eq!(metadata_key(b"TP1"), metadata_key(b"TPE1"));
        assert_eq!(metadata_key(b"COM"), None); // COMM/COM is handled specially, not via this table
    }

    #[test]
    fn tyer_and_tdrc_share_the_date_key() {
        assert_eq!(metadata_key(b"TYER"), Some("date"));
        assert_eq!(metadata_key(b"TDRC"), Some("date"));
    }

    #[test]
    fn unmapped_frames_have_no_metadata_key() {
        assert_eq!(metadata_key(b"USLT"), None);
        assert_eq!(metadata_key(b"PRIV"), None);
    }
}
