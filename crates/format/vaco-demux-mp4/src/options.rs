//! Demuxer-level options.
//!
//! Names, types and defaults are **interface facts** (D9) read from the pinned
//! reference with `ffmpeg -h demuxer=mov`. The reference exposes 21; the ones
//! below are the ones this crate acts on. The rest are listed in
//! `docs/format/vaco-demux-mp4.md` under *Not implemented*, so nobody has to
//! guess whether an absent option was forgotten or declined.

/// Options the MP4 demuxer understands.
///
/// A flat set of independent switches rather than a state enum, because that is
/// exactly what the reference's option table is and every one of them is
/// separately settable from the command line.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per reference option; grouping them would break the 1:1 mapping the CLI needs"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Options {
    /// `-ignore_editlist` — do not apply `elst` at all.
    ///
    /// With this set a track reports raw media timestamps: no trim, no empty-
    /// edit delay, no discard flags.
    pub ignore_editlist: bool,
    /// `-ignore_chapters`.
    pub ignore_chapters: bool,
    /// `-use_tfdt` — trust `tfdt` for a fragment's base decode time rather than
    /// accumulating durations.
    pub use_tfdt: bool,
    /// `-enable_drefs` — follow a `dref` entry that points at another file.
    ///
    /// **Refused even when set** (plan 18 §3.1.10): opening a second file
    /// because the first one's bytes asked us to is a file-system read
    /// triggered by content. The option exists so that the refusal is
    /// reportable rather than silent.
    pub enable_drefs: bool,
    /// `-interleaved_read` — emit packets from every track in one DTS-ordered
    /// stream. With this off, each track is drained in turn.
    pub interleaved_read: bool,
    /// `-seek_streams_individually` — after a seek, place every track at its
    /// own nearest preceding sample rather than at the reference track's
    /// position.
    pub seek_streams_individually: bool,
    /// `-max_stts_delta` — a `stts` delta above this is treated as invalid.
    pub max_stts_delta: u32,
    /// `-decryption_key` — the fallback AES-128 key (16 bytes) for a
    /// `cenc`-protected track whose `tenc.default_KID` is not present in
    /// [`Self::decryption_keys`].
    pub decryption_key: Option<[u8; 16]>,
    /// `-decryption_keys` — AES-128 keys selected by `tenc.default_KID`.
    ///
    /// Later entries replace earlier entries with the same KID, matching a
    /// dictionary's last-write-wins behavior. This does not implement `seig`
    /// sample-group key rotation; every sample still uses its track default.
    pub decryption_keys: Vec<DecryptionKey>,
}

/// One Common Encryption key associated with a 16-byte key identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecryptionKey {
    /// The `tenc.default_KID` that selects this key.
    pub kid: [u8; 16],
    /// The AES-128 media key.
    pub key: [u8; 16],
}

impl Mp4Options {
    /// Select a media key for a track's `tenc.default_KID`.
    #[must_use]
    pub fn key_for(&self, kid: &[u8; 16]) -> Option<[u8; 16]> {
        self.decryption_keys
            .iter()
            .rev()
            .find(|entry| &entry.kid == kid)
            .map(|entry| entry.key)
            .or(self.decryption_key)
    }
}

impl Default for Mp4Options {
    fn default() -> Self {
        Self {
            ignore_editlist: false,
            ignore_chapters: false,
            use_tfdt: true,
            enable_drefs: false,
            interleaved_read: true,
            seek_streams_individually: true,
            max_stts_delta: 4_294_487_295,
            decryption_key: None,
            decryption_keys: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DecryptionKey, Mp4Options};

    #[test]
    fn kid_match_precedes_fallback_and_last_duplicate_wins() {
        let kid = [0x11; 16];
        let opts = Mp4Options {
            decryption_key: Some([0xaa; 16]),
            decryption_keys: vec![
                DecryptionKey {
                    kid,
                    key: [0xbb; 16],
                },
                DecryptionKey {
                    kid,
                    key: [0xcc; 16],
                },
            ],
            ..Mp4Options::default()
        };

        assert_eq!(opts.key_for(&kid), Some([0xcc; 16]));
        assert_eq!(opts.key_for(&[0x22; 16]), Some([0xaa; 16]));
    }
}
