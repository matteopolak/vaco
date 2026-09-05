//! Common Encryption (ISO/IEC 23001-7): `pssh`, `schm`/`tenc`, `saiz`/`saio`,
//! `senc`, and `sgpd(seig)`/`sbgp` sample groups.
//!
//! **This module reports encryption; it does not decrypt anything.** That is a
//! deliberate scope boundary a demuxer built on it should keep: surface the
//! scheme, the key id and the system ids so a caller can act on them, and stop
//! there. Nothing here touches sample bytes.
//!
//! # What was measured, and what was not
//!
//! `ffmpeg 8.1`'s `mov` muxer (`-encryption_scheme cenc-aes-ctr -encryption_key
//! … -encryption_kid …`) was used to produce a real CENC file and every byte
//! layout below was read back from it: `schm`'s three fixed fields, `tenc`
//! version 0's `default_isProtected`/`default_Per_Sample_IV_Size`/
//! `default_KID`, `senc`'s `sample_count` plus per-sample IV and (with flags
//! bit 1 set) subsample table, and `saiz`/`saio` pointing at exactly the bytes
//! `senc` itself carries after its own `sample_count` field. `ffprobe`
//! surfaces **none** of it — no encryption tag of any kind on the stream, and
//! decoding the file silently produces garbage frames rather than an error.
//! Reporting this at all is therefore new behaviour, not a reproduction of the
//! reference's own output; `pssh` and `tenc` version 1's `default_crypt_byte_block`/
//! `default_skip_byte_block` were not seen in that file and are transcribed
//! directly from the spec instead.

use vaco_core::{Error, Result};

use crate::boxes::IsoBox;
use crate::fourcc::{FourCc, boxes};
use crate::table::EntryTable;

/// Largest number of `KID`s read from a version-1 `pssh`.
pub const MAX_PSSH_KIDS: usize = 256;

/// `schm` — the encryption scheme and its version (§4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemeType {
    /// e.g. `cenc`, `cbc1`, `cens`, `cbcs`.
    pub scheme_type: FourCc,
    /// 16.16 fixed-point in the box, reported as the raw `u32`.
    pub scheme_version: u32,
}

impl SchemeType {
    /// Parse a `schm` full box.
    #[must_use]
    pub fn parse(schm: &IsoBox<'_>) -> Option<Self> {
        let full = schm.full().ok()?;
        let mut r = full.reader();
        let scheme_type = FourCc(r.bytes(4).try_into().ok()?);
        let scheme_version = r.be32();
        r.check().ok()?;
        Some(Self {
            scheme_type,
            scheme_version,
        })
    }
}

/// A `tenc` or `seig` constant IV, retaining its declared size so an 8-byte
/// value cannot be mistaken for a zero-padded 16-byte value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstantIv {
    /// `default_constant_IV_size` or `constant_IV_size` from the box.
    pub size: u8,
    /// The IV bytes, followed by zeroes only beyond [`Self::size`].
    pub bytes: [u8; 16],
}

/// `tenc` — per-track default encryption parameters (§8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackEncryption {
    /// `default_crypt_byte_block` (version 1 only; 0 on version 0).
    pub crypt_byte_block: u8,
    /// `default_skip_byte_block` (version 1 only; 0 on version 0).
    pub skip_byte_block: u8,
    /// Whether samples of this track are protected by default.
    pub is_protected: bool,
    /// `0` means a per-sample IV is not used and `constant_iv` applies
    /// instead.
    pub per_sample_iv_size: u8,
    /// `default_KID`.
    pub default_kid: [u8; 16],
    /// Present only when `is_protected` and `per_sample_iv_size == 0`.
    pub constant_iv: Option<ConstantIv>,
}

impl TrackEncryption {
    /// Parse a `tenc` full box.
    #[must_use]
    pub fn parse(tenc: &IsoBox<'_>) -> Option<Self> {
        let full = tenc.full().ok()?;
        if full.version > 1 {
            return None;
        }
        let mut r = full.reader();
        let _reserved = r.u8();
        let (crypt_byte_block, skip_byte_block) = if full.version >= 1 {
            let packed = r.u8();
            (packed >> 4, packed & 0x0F)
        } else {
            let _reserved2 = r.u8();
            (0, 0)
        };
        let is_protected = match r.u8() {
            0 => false,
            1 => true,
            _ => return None,
        };
        let per_sample_iv_size = r.u8();
        if !matches!(per_sample_iv_size, 0 | 8 | 16) {
            return None;
        }
        let default_kid: [u8; 16] = r.bytes(16).try_into().ok()?;
        let constant_iv = if is_protected && per_sample_iv_size == 0 {
            let size = r.u8();
            if !matches!(size, 8 | 16) {
                return None;
            }
            let bytes = r.bytes(usize::from(size));
            if bytes.len() != usize::from(size) {
                return None;
            }
            let mut stored = [0u8; 16];
            stored.get_mut(..usize::from(size))?.copy_from_slice(bytes);
            Some(ConstantIv {
                size,
                bytes: stored,
            })
        } else {
            None
        };
        Some(Self {
            crypt_byte_block,
            skip_byte_block,
            is_protected,
            per_sample_iv_size,
            default_kid,
            constant_iv,
        })
    }
}

/// Maximum `seig` descriptions retained from one `sgpd` box.
pub const MAX_SEIG_DESCRIPTIONS: u32 = 256;

/// Maximum run count retained from one `sbgp` box.
pub const MAX_SAMPLE_GROUP_RUNS: u32 = 1 << 20;

/// Maximum `sgpd` or `sbgp` boxes retained from one container.
pub const MAX_SAMPLE_GROUP_BOXES: usize = 256;

/// The `grouping_type` at the start of an `sgpd` or `sbgp` full-box body.
#[must_use]
pub fn sample_grouping_type(group: &IsoBox<'_>) -> Option<FourCc> {
    let body = group.full().ok()?.body;
    Some(FourCc(body.get(..4)?.try_into().ok()?))
}

/// A deployed version-1 `sgpd` whose entries are Common Encryption `seig`
/// descriptions (ISO/IEC 23001-7 §6).
///
/// Version 1 is intentionally the only accepted grammar here. Version 0 has
/// no entry lengths, while version 2's implicit default-selection semantics
/// are outside the demuxer's current key-rotation slice; callers must refuse
/// those versions rather than guessing at sample keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeigDescriptions {
    /// Descriptions in their 1-based `sgpd` index order.
    pub entries: Vec<TrackEncryption>,
}

impl SeigDescriptions {
    /// Parse a version-1 `sgpd(seig)`.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for another `sgpd` version and
    /// [`Error::InvalidData`] for malformed lengths, fields or excessive
    /// entry counts.
    pub fn parse(sgpd: &IsoBox<'_>) -> Result<Self> {
        let full = sgpd.full()?;
        if full.version != 1 {
            return Err(Error::Unsupported(
                "isom: only version-1 sgpd(seig) descriptions are supported",
            ));
        }
        let mut r = full.reader();
        if r.bytes(4) != b"seig" {
            return Err(Error::InvalidData("isom: sgpd grouping type is not seig"));
        }
        let default_length = r.be32();
        let count = r.be32();
        r.check()
            .map_err(|_| Error::InvalidData("isom: truncated sgpd(seig) header"))?;
        if count > MAX_SEIG_DESCRIPTIONS {
            return Err(Error::LimitExceeded {
                limit: "isom sgpd(seig) entries",
                requested: u64::from(count),
                cap: u64::from(MAX_SEIG_DESCRIPTIONS),
            });
        }
        let mut entries = Vec::new();
        for _ in 0..count {
            let length = if default_length == 0 {
                r.be32()
            } else {
                default_length
            };
            let length = usize::try_from(length)
                .map_err(|_| Error::InvalidData("isom: sgpd(seig) entry length overflow"))?;
            let mut entry = r.sub(length);
            let _reserved = entry.u8();
            let pattern = entry.u8();
            let is_protected = entry.u8();
            let per_sample_iv_size = entry.u8();
            let default_kid: [u8; 16] = entry
                .bytes(16)
                .try_into()
                .map_err(|_| Error::InvalidData("isom: truncated sgpd(seig) KID"))?;
            if is_protected > 1 {
                return Err(Error::InvalidData(
                    "isom: reserved sgpd(seig) isProtected value",
                ));
            }
            if !matches!(per_sample_iv_size, 0 | 8 | 16) {
                return Err(Error::InvalidData(
                    "isom: invalid sgpd(seig) per-sample IV size",
                ));
            }
            let constant_iv = if is_protected == 1 && per_sample_iv_size == 0 {
                let size = entry.u8();
                if !matches!(size, 8 | 16) {
                    return Err(Error::InvalidData(
                        "isom: invalid sgpd(seig) constant IV size",
                    ));
                }
                let size_usize = usize::from(size);
                let iv = entry.bytes(size_usize);
                let mut padded = [0u8; 16];
                let dst = padded.get_mut(..size_usize).ok_or(Error::InvalidData(
                    "isom: invalid sgpd(seig) constant IV size",
                ))?;
                if iv.len() != dst.len() {
                    return Err(Error::InvalidData("isom: truncated sgpd(seig) constant IV"));
                }
                dst.copy_from_slice(iv);
                Some(ConstantIv {
                    size,
                    bytes: padded,
                })
            } else {
                None
            };
            entry
                .check()
                .map_err(|_| Error::InvalidData("isom: truncated sgpd(seig) entry"))?;
            entries.push(TrackEncryption {
                crypt_byte_block: pattern >> 4,
                skip_byte_block: pattern & 0x0f,
                is_protected: is_protected == 1,
                per_sample_iv_size,
                default_kid,
                constant_iv,
            });
        }
        r.check()
            .map_err(|_| Error::InvalidData("isom: truncated sgpd(seig) entries"))?;
        Ok(Self { entries })
    }

    /// Resolve a 1-based group-description index.
    #[must_use]
    pub fn get(&self, index: u32) -> Option<TrackEncryption> {
        usize::try_from(index.checked_sub(1)?)
            .ok()
            .and_then(|index| self.entries.get(index))
            .copied()
    }
}

/// One run from a Common Encryption `sbgp(seig)`, stored by its exclusive
/// cumulative sample end so lookup stays logarithmic without expanding to a
/// sample-sized vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleGroupRun {
    end: u32,
    index: u32,
}

/// A version-0 or version-1 `sbgp(seig)` sample-to-description mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleToSeig {
    runs: Vec<SampleGroupRun>,
    sample_count: u32,
}

impl SampleToSeig {
    /// Parse an `sbgp(seig)` with no nonzero grouping-type parameter.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for another version or a nonzero version-1
    /// parameter, and [`Error::InvalidData`] for malformed or excessive runs.
    pub fn parse(sbgp: &IsoBox<'_>) -> Result<Self> {
        let full = sbgp.full()?;
        if full.version > 1 {
            return Err(Error::Unsupported(
                "isom: only version-0/1 sbgp(seig) mappings are supported",
            ));
        }
        let mut r = full.reader();
        if r.bytes(4) != b"seig" {
            return Err(Error::InvalidData("isom: sbgp grouping type is not seig"));
        }
        if full.version == 1 && r.be32() != 0 {
            return Err(Error::Unsupported(
                "isom: parameterized sbgp(seig) mappings are not supported",
            ));
        }
        let count = r.be32();
        r.check()
            .map_err(|_| Error::InvalidData("isom: truncated sbgp(seig) header"))?;
        if count > MAX_SAMPLE_GROUP_RUNS {
            return Err(Error::LimitExceeded {
                limit: "isom sbgp(seig) runs",
                requested: u64::from(count),
                cap: u64::from(MAX_SAMPLE_GROUP_RUNS),
            });
        }
        let mut runs = Vec::new();
        let mut sample_count = 0u32;
        for _ in 0..count {
            let samples = r.be32();
            let index = r.be32();
            if samples == 0 {
                return Err(Error::InvalidData("isom: zero-length sbgp(seig) run"));
            }
            sample_count = sample_count
                .checked_add(samples)
                .ok_or(Error::InvalidData("isom: sbgp(seig) sample count overflow"))?;
            runs.push(SampleGroupRun {
                end: sample_count,
                index,
            });
        }
        r.check()
            .map_err(|_| Error::InvalidData("isom: truncated sbgp(seig) runs"))?;
        Ok(Self { runs, sample_count })
    }

    /// Number of samples covered by the run table.
    #[must_use]
    pub const fn sample_count(&self) -> u32 {
        self.sample_count
    }

    /// Description index for zero-based `sample`, including `0` for the track
    /// default and fragment-local values starting at `0x10001`.
    #[must_use]
    pub fn index_for(&self, sample: u32) -> Option<u32> {
        let run = self.runs.partition_point(|run| run.end <= sample);
        self.runs.get(run).map(|run| run.index)
    }
}

/// The scheme and default track parameters from `sinf ▸ schm`/`sinf ▸ schi ▸
/// tenc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CencInfo {
    /// From `schm`, when present.
    pub scheme: Option<SchemeType>,
    /// From `schi ▸ tenc`, when present.
    pub track_encryption: Option<TrackEncryption>,
}

impl CencInfo {
    /// Read `schm` and `schi ▸ tenc` out of a `sinf` box.
    #[must_use]
    pub fn from_sinf(sinf: &IsoBox<'_>) -> Self {
        let mut me = Self::default();
        for child in sinf.children() {
            let Ok(child) = child else { continue };
            match child.kind() {
                boxes::SCHM => me.scheme = SchemeType::parse(&child),
                boxes::SCHI => {
                    if let Some(tenc) = child.children().find(boxes::TENC) {
                        me.track_encryption = TrackEncryption::parse(&tenc);
                    }
                }
                _ => {}
            }
        }
        me
    }

    /// Whether either half was actually found.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.scheme.is_none() && self.track_encryption.is_none()
    }
}

/// `pssh` — a DRM system's opaque initialisation data (§8.1).
///
/// May sit under `moov` (progressive) or as a top-level box alongside `moof`
/// (fragmented); the box itself does not say which, so a caller collects it
/// from wherever its own scan reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pssh {
    /// The DRM system this initialisation data is for.
    pub system_id: [u8; 16],
    /// Version 1 only: the key ids this `pssh` concerns.
    pub kids: Vec<[u8; 16]>,
    /// The opaque `Data` field, verbatim.
    pub data: Vec<u8>,
}

impl Pssh {
    /// Parse a `pssh` full box.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] when the payload is too short to be a
    /// full box at all.
    pub fn parse(pssh: &IsoBox<'_>) -> Result<Self> {
        let full = pssh.full()?;
        let mut r = full.reader();
        let system_id: [u8; 16] = r.bytes(16).try_into().unwrap_or([0; 16]);
        let mut kids = Vec::new();
        if full.version >= 1 {
            let count = r.be32();
            let rest = full.body.get(r.pos()..).unwrap_or(&[]);
            let table = EntryTable::new(rest, 16, count.min(MAX_PSSH_KIDS as u32));
            for i in 0..table.len() {
                if let Some(e) = table.entry(i) {
                    kids.push(e.try_into().unwrap_or([0; 16]));
                }
            }
            r.skip((table.len() as usize).saturating_mul(16));
        }
        let data_size = r.be32();
        let data = r.bytes(data_size as usize).to_vec();
        Ok(Self {
            system_id,
            kids,
            data,
        })
    }
}

/// `saiz` — declared size of each sample's auxiliary information (§8.7.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleAuxSizes {
    /// Present only when `flags & 1`.
    pub aux_info_type: Option<(FourCc, u32)>,
    /// `0` means every sample's size is `default_sample_info_size` instead of
    /// being listed individually.
    pub default_sample_info_size: u8,
    /// Declared sample count, already clamped against the payload when sizes
    /// are listed individually.
    pub sample_count: u32,
}

impl SampleAuxSizes {
    /// Parse a `saiz` full box.
    #[must_use]
    pub fn parse(saiz: &IsoBox<'_>) -> Option<Self> {
        let full = saiz.full().ok()?;
        let mut r = full.reader();
        let aux_info_type = if full.flags & 1 != 0 {
            let kind = FourCc(r.bytes(4).try_into().ok()?);
            let param = r.be32();
            Some((kind, param))
        } else {
            None
        };
        let default_sample_info_size = r.u8();
        let declared = r.be32();
        r.check().ok()?;
        let sample_count = if default_sample_info_size == 0 {
            let rest = full.body.get(r.pos()..).unwrap_or(&[]);
            EntryTable::new(rest, 1, declared).len()
        } else {
            declared
        };
        Some(Self {
            aux_info_type,
            default_sample_info_size,
            sample_count,
        })
    }
}

/// `saio` — where each sample's auxiliary information starts (§8.7.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleAuxOffsets {
    /// Present only when `flags & 1`.
    pub aux_info_type: Option<(FourCc, u32)>,
    /// Absolute offsets (version 0) or explicitly 64-bit ones (version 1),
    /// widened to `u64` either way.
    pub offsets: Vec<u64>,
}

impl SampleAuxOffsets {
    /// Largest number of offsets read from one `saio`.
    pub const MAX_ENTRIES: u32 = 1 << 20;

    /// Parse a `saio` full box.
    #[must_use]
    pub fn parse(saio: &IsoBox<'_>) -> Option<Self> {
        let full = saio.full().ok()?;
        let mut r = full.reader();
        let aux_info_type = if full.flags & 1 != 0 {
            let kind = FourCc(r.bytes(4).try_into().ok()?);
            let param = r.be32();
            Some((kind, param))
        } else {
            None
        };
        let declared = r.be32();
        r.check().ok()?;
        let stride = if full.version == 0 { 4 } else { 8 };
        let rest = full.body.get(r.pos()..).unwrap_or(&[]);
        let table = EntryTable::new(rest, stride, declared.min(Self::MAX_ENTRIES));
        let mut offsets = Vec::new();
        for i in 0..table.len() {
            let Some(mut e) = table.reader_at(i) else {
                break;
            };
            offsets.push(if full.version == 0 {
                u64::from(e.be32())
            } else {
                e.be64()
            });
        }
        Some(Self {
            aux_info_type,
            offsets,
        })
    }
}

/// `senc` — per-sample IVs and, when `flags & 2`, subsample tables (§7.2).
///
/// `sample_count` and the byte range the per-sample records occupy are always
/// reported; resolving an individual sample's IV additionally needs the
/// track's `per_sample_iv_size` from `tenc`, which lives in a different box —
/// see [`Self::iv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleEncryption<'a> {
    /// Whether each sample also carries a subsample table (`flags & 2`).
    pub has_subsamples: bool,
    /// `sample_count`.
    pub sample_count: u32,
    /// Absolute file offset of the first byte after `sample_count` — where
    /// the per-sample records begin, and what `saio` should point at when
    /// both boxes are present in the same file.
    pub records_offset: u64,
    /// The bytes starting at [`Self::records_offset`], borrowed from the
    /// box's own payload.
    pub records: &'a [u8],
}

impl<'a> SampleEncryption<'a> {
    /// Parse a `senc` full box.
    #[must_use]
    pub fn parse(senc: &IsoBox<'a>) -> Option<Self> {
        let full = senc.full().ok()?;
        let mut r = full.reader();
        let sample_count = r.be32();
        r.check().ok()?;
        let records = full.body.get(r.pos()..).unwrap_or(&[]);
        Some(Self {
            has_subsamples: full.flags & 2 != 0,
            sample_count,
            records_offset: full.offset.saturating_add(r.pos() as u64),
            records,
        })
    }

    /// The `index`-th sample's IV (0-based), `iv_size` bytes, big-endian.
    ///
    /// `None` when [`Self::has_subsamples`] is set: a subsample table gives
    /// each record a variable length (the IV plus a `subsample_count`-sized
    /// table), so indexing sample `n` needs every earlier record's own
    /// `subsample_count` already read — a sequential scan this crate's
    /// decryption path does not implement (see the module doc's *What was
    /// measured* section; full-sample encryption, `has_subsamples == false`,
    /// is the case this crate writes and the case it decrypts).
    #[must_use]
    pub fn iv(&self, index: u32, iv_size: u8) -> Option<&'a [u8]> {
        if self.has_subsamples || iv_size == 0 {
            return None;
        }
        let stride = usize::from(iv_size);
        let start = stride.checked_mul(usize::try_from(index).ok()?)?;
        self.records.get(start..start.checked_add(stride)?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn bx(kind: [u8; 4], full_payload: &[u8]) -> IsoBox<'_> {
        // A full box: version/flags then body, wrapped as if a real box had
        // already stripped its size/type header.
        let header_len = 8u64;
        let total = header_len + full_payload.len() as u64;
        IsoBox {
            offset: 0,
            header: crate::boxes::BoxHeader {
                kind: FourCc::new(&kind),
                header_len,
                size: total,
                usertype: None,
                to_end: false,
            },
            payload: full_payload,
        }
    }

    #[test]
    fn schm_reads_scheme_type_and_version() {
        let mut body = vec![0, 0, 0, 0]; // version/flags
        body.extend_from_slice(b"cenc");
        body.extend_from_slice(&1u32.to_be_bytes());
        let b = bx(*b"schm", &body);
        let s = SchemeType::parse(&b).unwrap();
        assert_eq!(s.scheme_type, FourCc::new(b"cenc"));
        assert_eq!(s.scheme_version, 1);
    }

    /// Bytes read back from a real `ffmpeg 8.1`
    /// `-encryption_scheme cenc-aes-ctr` file: version 0, `isProtected=1`,
    /// `per_sample_iv_size=8`, `default_KID` ending `…0001`.
    #[test]
    fn tenc_v0_matches_a_real_ffmpeg_file() {
        let hex = "000000000000010800000000000000000000000000000001";
        let raw = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect::<Vec<u8>>();
        let b = bx(*b"tenc", &raw);
        let t = TrackEncryption::parse(&b).unwrap();
        assert!(t.is_protected);
        assert_eq!(t.per_sample_iv_size, 8);
        assert_eq!(t.default_kid[15], 1);
        assert_eq!(&t.default_kid[..15], &[0u8; 15]);
        assert!(t.constant_iv.is_none());
        assert_eq!((t.crypt_byte_block, t.skip_byte_block), (0, 0));
    }

    #[test]
    fn tenc_rejects_a_reserved_is_protected_value() {
        let mut body = vec![0, 0, 0, 0, 0, 0, 2, 8];
        // version/flags, two reserved bytes, is_protected, IV size
        body.extend_from_slice(&[0xAB; 16]);
        assert!(TrackEncryption::parse(&bx(*b"tenc", &body)).is_none());
    }

    #[test]
    fn tenc_rejects_an_unsupported_per_sample_iv_size() {
        let mut body = vec![0, 0, 0, 0, 0, 0, 1, 4];
        // version/flags, two reserved bytes, is_protected, IV size
        body.extend_from_slice(&[0xAB; 16]);
        assert!(TrackEncryption::parse(&bx(*b"tenc", &body)).is_none());
    }

    #[test]
    fn tenc_rejects_an_unknown_version() {
        let mut body = vec![2, 0, 0, 0, 0, 0, 1, 8];
        // version/flags, reserved, pattern, is_protected, IV size
        body.extend_from_slice(&[0xAB; 16]);
        assert!(TrackEncryption::parse(&bx(*b"tenc", &body)).is_none());
    }

    #[test]
    fn tenc_v1_reads_the_byte_block_pattern() {
        let mut body = vec![1, 0, 0, 0]; // version=1, flags=0
        body.push(0); // reserved
        body.push(0x19); // crypt=1, skip=9
        body.push(1); // is_protected
        body.push(0); // per_sample_iv_size = 0 -> constant IV follows
        body.extend_from_slice(&[0xAB; 16]); // default_KID
        body.push(16); // constant_iv_size
        body.extend_from_slice(&[0xCD; 16]);
        let b = bx(*b"tenc", &body);
        let t = TrackEncryption::parse(&b).unwrap();
        assert_eq!((t.crypt_byte_block, t.skip_byte_block), (1, 9));
        assert_eq!(
            t.constant_iv,
            Some(ConstantIv {
                size: 16,
                bytes: [0xCDu8; 16],
            })
        );
    }

    #[test]
    fn tenc_preserves_an_eight_byte_constant_iv_without_widening_it() {
        let mut body = vec![1, 0, 0, 0]; // version=1, flags=0
        body.push(0); // reserved
        body.push(0); // no pattern
        body.push(1); // is_protected
        body.push(0); // per_sample_iv_size = 0 -> constant IV follows
        body.extend_from_slice(&[0xAB; 16]); // default_KID
        body.push(8); // constant_iv_size
        body.extend_from_slice(&[0xCD; 8]);
        let b = bx(*b"tenc", &body);
        let t = TrackEncryption::parse(&b).unwrap();
        assert_eq!(
            t.constant_iv,
            Some(ConstantIv {
                size: 8,
                bytes: [
                    0xCD, 0xCD, 0xCD, 0xCD, 0xCD, 0xCD, 0xCD, 0xCD, 0, 0, 0, 0, 0, 0, 0, 0,
                ],
            })
        );
    }

    #[test]
    fn tenc_rejects_a_truncated_constant_iv() {
        let mut body = vec![1, 0, 0, 0]; // version=1, flags=0
        body.push(0); // reserved
        body.push(0); // no pattern
        body.push(1); // is_protected
        body.push(0); // per_sample_iv_size = 0 -> constant IV follows
        body.extend_from_slice(&[0xAB; 16]); // default_KID
        body.push(16); // constant_iv_size
        body.extend_from_slice(&[0xCD; 8]); // truncated constant IV
        let b = bx(*b"tenc", &body);
        assert!(TrackEncryption::parse(&b).is_none());
    }

    #[test]
    fn version_one_seig_and_version_zero_sbgp_resolve_the_exact_runs() {
        let mut sgpd = vec![1, 0, 0, 0];
        sgpd.extend_from_slice(b"seig");
        sgpd.extend_from_slice(&20u32.to_be_bytes());
        sgpd.extend_from_slice(&2u32.to_be_bytes());
        for (kid, iv_size) in [(0x11, 8), (0x22, 16)] {
            sgpd.extend_from_slice(&[0, 0, 1, iv_size]);
            sgpd.extend_from_slice(&[kid; 16]);
        }
        let descriptions = SeigDescriptions::parse(&bx(*b"sgpd", &sgpd)).unwrap();
        assert_eq!(descriptions.get(1).unwrap().default_kid, [0x11; 16]);
        assert_eq!(descriptions.get(2).unwrap().per_sample_iv_size, 16);
        assert!(descriptions.get(0).is_none());
        assert!(descriptions.get(3).is_none());

        let mut sbgp = vec![0, 0, 0, 0];
        sbgp.extend_from_slice(b"seig");
        sbgp.extend_from_slice(&3u32.to_be_bytes());
        for (count, index) in [(2u32, 0u32), (3, 1), (1, 0x1_0001)] {
            sbgp.extend_from_slice(&count.to_be_bytes());
            sbgp.extend_from_slice(&index.to_be_bytes());
        }
        let mapping = SampleToSeig::parse(&bx(*b"sbgp", &sbgp)).unwrap();
        assert_eq!(mapping.sample_count(), 6);
        assert_eq!(
            (0..7).map(|i| mapping.index_for(i)).collect::<Vec<_>>(),
            vec![
                Some(0),
                Some(0),
                Some(1),
                Some(1),
                Some(1),
                Some(0x1_0001),
                None
            ]
        );
    }

    #[test]
    fn unsupported_or_truncated_seig_tables_are_refused() {
        for version in [0, 2] {
            let mut body = vec![version, 0, 0, 0];
            body.extend_from_slice(b"seig");
            assert!(matches!(
                SeigDescriptions::parse(&bx(*b"sgpd", &body)),
                Err(Error::Unsupported(_))
            ));
        }

        let mut truncated = vec![1, 0, 0, 0];
        truncated.extend_from_slice(b"seig");
        truncated.extend_from_slice(&20u32.to_be_bytes());
        truncated.extend_from_slice(&1u32.to_be_bytes());
        truncated.extend_from_slice(&[0, 0, 1, 8, 0]);
        assert!(SeigDescriptions::parse(&bx(*b"sgpd", &truncated)).is_err());

        let mut parameterized = vec![1, 0, 0, 0];
        parameterized.extend_from_slice(b"seig");
        parameterized.extend_from_slice(&1u32.to_be_bytes());
        parameterized.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(
            SampleToSeig::parse(&bx(*b"sbgp", &parameterized)),
            Err(Error::Unsupported(_))
        ));
    }

    /// `senc` bytes from the same file: `sample_count=10`, subsamples present.
    #[test]
    fn senc_reports_its_shape_without_decoding_records() {
        let mut body = vec![0, 0, 0, 2]; // version 0, flags = 0x000002
        body.extend_from_slice(&10u32.to_be_bytes());
        body.extend_from_slice(&[0u8; 8]); // one (truncated) IV, irrelevant here
        let b = bx(*b"senc", &body);
        let s = SampleEncryption::parse(&b).unwrap();
        assert!(s.has_subsamples);
        assert_eq!(s.sample_count, 10);
        // offset past the box header (8) + version/flags+count (8)
        assert_eq!(s.records_offset, 16);
    }

    #[test]
    fn saiz_and_saio_round_trip_a_declared_count() {
        let mut saiz_body = vec![0, 0, 0, 0]; // flags = 0
        saiz_body.push(0); // default_sample_info_size = 0 -> per-sample list
        saiz_body.extend_from_slice(&3u32.to_be_bytes());
        saiz_body.extend_from_slice(&[22, 16, 16]);
        let saiz = SampleAuxSizes::parse(&bx(*b"saiz", &saiz_body)).unwrap();
        assert_eq!(saiz.sample_count, 3);
        assert_eq!(saiz.default_sample_info_size, 0);

        let mut saio_body = vec![0, 0, 0, 0];
        saio_body.extend_from_slice(&1u32.to_be_bytes());
        saio_body.extend_from_slice(&11819u32.to_be_bytes());
        let saio = SampleAuxOffsets::parse(&bx(*b"saio", &saio_body)).unwrap();
        assert_eq!(saio.offsets, vec![11819]);
    }

    #[test]
    fn pssh_v1_reads_the_kid_list() {
        let mut body = vec![1, 0, 0, 0]; // version 1
        body.extend_from_slice(&[0x11; 16]); // system_id
        body.extend_from_slice(&2u32.to_be_bytes()); // kid count
        body.extend_from_slice(&[0xAA; 16]);
        body.extend_from_slice(&[0xBB; 16]);
        body.extend_from_slice(&4u32.to_be_bytes()); // data size
        body.extend_from_slice(&[1, 2, 3, 4]);
        let p = Pssh::parse(&bx(*b"pssh", &body)).unwrap();
        assert_eq!(p.system_id, [0x11; 16]);
        assert_eq!(p.kids, vec![[0xAA; 16], [0xBB; 16]]);
        assert_eq!(p.data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn senc_iv_indexes_fixed_size_records() {
        let mut body = vec![0, 0, 0, 0]; // version 0, no subsamples
        body.extend_from_slice(&3u32.to_be_bytes()); // sample_count
        body.extend_from_slice(&1u64.to_be_bytes()); // sample 0's IV
        body.extend_from_slice(&2u64.to_be_bytes()); // sample 1's IV
        body.extend_from_slice(&3u64.to_be_bytes()); // sample 2's IV
        let s = SampleEncryption::parse(&bx(*b"senc", &body)).unwrap();
        assert_eq!(s.iv(0, 8), Some(&1u64.to_be_bytes()[..]));
        assert_eq!(s.iv(1, 8), Some(&2u64.to_be_bytes()[..]));
        assert_eq!(s.iv(2, 8), Some(&3u64.to_be_bytes()[..]));
        assert_eq!(s.iv(3, 8), None, "past the declared sample count");
    }

    #[test]
    fn senc_iv_refuses_a_subsample_table() {
        let mut body = vec![0, 0, 0, 2]; // flags = 0x2: has subsamples
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);
        let s = SampleEncryption::parse(&bx(*b"senc", &body)).unwrap();
        assert_eq!(s.iv(0, 8), None);
    }

    #[test]
    fn a_truncated_tenc_never_panics() {
        for n in 0..24 {
            let _ = TrackEncryption::parse(&bx(*b"tenc", &vec![0u8; n]));
        }
    }

    #[test]
    fn a_truncated_pssh_never_panics() {
        for n in 0..40 {
            let _ = Pssh::parse(&bx(*b"pssh", &vec![1u8; n]));
        }
    }
}
