//! A minimal ZIP reader: extract one named member, nothing else.
//!
//! # What it is
//!
//! The JVT (H.264/AVC) and JCT-VC (H.265/HEVC) conformance corpora are
//! distributed by their standards bodies as ZIP archives, each holding the
//! bitstream this project actually wants alongside a decoder trace log and/or
//! a reference YUV dump this crate has no use for (see `vaco-media.lock`'s
//! `[[entry]].member` field, which names the one file inside the archive a
//! conformance case should be pointed at). [`extract`] pulls that one member
//! out, verified against its own recorded CRC-32, and nothing else in the
//! archive is ever materialised.
//!
//! # Why this crate writes its own instead of depending on one
//!
//! `miniz_oxide` is already a workspace dependency, but D11 gives every
//! third-party media crate exactly one owner and `vaco-demux-matroska` is
//! already it (`xtask/src/owner_gate.rs`); a second `Cargo.toml` listing it
//! fails `cargo xtask owner-gate`. Shelling out to a system `unzip` would
//! dodge that but trade it for a binary this crate cannot assume is
//! installed (unlike the reference `ffmpeg`/`ffprobe`, which this whole
//! project's differential story already depends on having). A from-scratch
//! reader, written directly from PKWARE's APPNOTE.TXT central-directory
//! layout and RFC 1951 (DEFLATE), owned solely by this crate, is the option
//! that adds no dependency and no external-tool assumption.
//!
//! # How it works
//!
//! ZIP's directory of record is the *central directory*, found by scanning
//! backward from the end of the file for the End Of Central Directory
//! signature. [`extract`] walks it to find `member`'s compression method,
//! sizes and *local header* offset — then reads the local header, because its
//! extra-field length is not guaranteed to match the central directory's, so
//! the real start of the compressed data has to be computed from the local
//! header's own lengths, not assumed. [`inflate::inflate`] implements RFC
//! 1951 directly (stored, fixed-Huffman and dynamic-Huffman blocks); `crc32`
//! is the standard IEEE 802.3 reflected table-based algorithm, generated at
//! call time rather than checked in as a literal table, since it is a public
//! algorithm with exactly one correct table rather than authorial expression
//! to transcribe.
//!
//! # How to change it
//!
//! This reads only what a JVT/JCT-VC conformance ZIP actually contains:
//! method 0 (store) and method 8 (deflate), no ZIP64, no encryption, no
//! multi-disk archives. A member using anything else is a named
//! [`ZipError`], never a silent wrong answer.

use vaco_limits::{Budget, Limits};

mod inflate;

/// Largest ZIP this reader will look at. Every archive this crate's own
/// `vaco-media.lock` names today is a few megabytes; a limit two orders of
/// magnitude above that catches a mistake (the wrong URL, a redirect to an
/// HTML error page) without ever being close for a real conformance archive.
const MAX_ZIP_BYTES: usize = 256 * 1024 * 1024;

/// The largest comment an End Of Central Directory record may declare.
const MAX_EOCD_COMMENT: usize = 65_535;

#[derive(Debug)]
pub enum ZipError {
    /// Not a ZIP file, or truncated, or a structure this reader does not
    /// understand (ZIP64, spanned/multi-disk, encrypted).
    Malformed(&'static str),
    /// The archive has no member by this name.
    MemberNotFound(String),
    /// The central directory names a compression method this reader does not
    /// implement (only 0/store and 8/deflate are).
    UnsupportedMethod { member: String, method: u16 },
    /// The archive exceeded [`MAX_ZIP_BYTES`].
    TooLarge,
    /// The decompressed bytes do not match the entry's own recorded CRC-32 —
    /// a corrupt download or a bug in [`inflate`], never something to paper
    /// over.
    Crc32Mismatch { member: String },
    /// A budget/fuel cap was hit — either a malformed archive or one larger
    /// than any real conformance ZIP this crate names.
    Limit(vaco_limits::LimitError),
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed zip: {why}"),
            Self::MemberNotFound(name) => write!(f, "zip has no member `{name}`"),
            Self::UnsupportedMethod { member, method } => {
                write!(
                    f,
                    "member `{member}` uses unsupported compression method {method}"
                )
            }
            Self::TooLarge => write!(f, "zip exceeds the {MAX_ZIP_BYTES}-byte reader cap"),
            Self::Crc32Mismatch { member } => {
                write!(f, "member `{member}` failed its own CRC-32 check")
            }
            Self::Limit(e) => write!(f, "zip reader budget: {e}"),
        }
    }
}

impl std::error::Error for ZipError {}

impl From<vaco_limits::LimitError> for ZipError {
    fn from(e: vaco_limits::LimitError) -> Self {
        Self::Limit(e)
    }
}

fn u16_le(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    let a = *s.first()?;
    let c = *s.get(1)?;
    Some(u16::from_le_bytes([a, c]))
}

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    let arr: [u8; 4] = s.try_into().ok()?;
    Some(u32::from_le_bytes(arr))
}

/// One central-directory record this reader cares about.
struct CentralEntry {
    method: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
    name: String,
}

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

/// Find the End Of Central Directory record, scanning backward from the end
/// (its comment field is variable length, so the signature is not at a fixed
/// offset). Returns the byte offset the record starts at.
fn find_eocd(zip: &[u8]) -> Result<usize, ZipError> {
    let scan_from = zip.len().saturating_sub(22 + MAX_EOCD_COMMENT);
    let window = zip
        .get(scan_from..)
        .ok_or(ZipError::Malformed("empty archive"))?;
    // Scan backward: a comment could itself contain four bytes that look like
    // the signature, and the true EOCD is always the *last* match.
    for start in (0..window.len().saturating_sub(21)).rev() {
        if u32_le(window, start) == Some(EOCD_SIG) {
            return Ok(scan_from + start);
        }
    }
    Err(ZipError::Malformed(
        "no End Of Central Directory record found",
    ))
}

/// Parse the central directory into every entry it names.
fn read_central_directory(zip: &[u8], budget: &mut Budget) -> Result<Vec<CentralEntry>, ZipError> {
    let eocd = find_eocd(zip)?;
    let disk_entries = u16_le(zip, eocd + 10).ok_or(ZipError::Malformed("truncated EOCD"))?;
    let cd_size = u32_le(zip, eocd + 12).ok_or(ZipError::Malformed("truncated EOCD"))?;
    let cd_offset = u32_le(zip, eocd + 16).ok_or(ZipError::Malformed("truncated EOCD"))?;
    if cd_offset == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF || disk_entries == 0xFFFF {
        return Err(ZipError::Malformed("ZIP64 is not supported"));
    }

    // Not `budget.alloc`: entries are pushed one at a time as they are
    // parsed (each iteration already pays `consume_fuel`, which bounds the
    // loop), and `CentralEntry` holds a `String` so it is not `Copy` — the
    // shape `Budget::alloc` is for.
    let mut out: Vec<CentralEntry> = Vec::new();
    let mut pos = cd_offset as usize;
    for _ in 0..disk_entries {
        budget.consume_fuel(1)?;
        if u32_le(zip, pos) != Some(CENTRAL_SIG) {
            return Err(ZipError::Malformed(
                "central directory record has the wrong signature",
            ));
        }
        let method =
            u16_le(zip, pos + 10).ok_or(ZipError::Malformed("truncated central record"))?;
        let crc32 = u32_le(zip, pos + 16).ok_or(ZipError::Malformed("truncated central record"))?;
        let compressed_size = u64::from(
            u32_le(zip, pos + 20).ok_or(ZipError::Malformed("truncated central record"))?,
        );
        let uncompressed_size = u64::from(
            u32_le(zip, pos + 24).ok_or(ZipError::Malformed("truncated central record"))?,
        );
        let name_len = usize::from(
            u16_le(zip, pos + 28).ok_or(ZipError::Malformed("truncated central record"))?,
        );
        let extra_len = usize::from(
            u16_le(zip, pos + 30).ok_or(ZipError::Malformed("truncated central record"))?,
        );
        let comment_len = usize::from(
            u16_le(zip, pos + 32).ok_or(ZipError::Malformed("truncated central record"))?,
        );
        let local_header_offset = u64::from(
            u32_le(zip, pos + 42).ok_or(ZipError::Malformed("truncated central record"))?,
        );
        let name_bytes = zip
            .get(pos + 46..pos + 46 + name_len)
            .ok_or(ZipError::Malformed(
                "central record name runs past end of file",
            ))?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        out.push(CentralEntry {
            method,
            crc32,
            compressed_size,
            uncompressed_size,
            local_header_offset,
            name,
        });
        pos = pos
            .checked_add(46 + name_len + extra_len + comment_len)
            .ok_or(ZipError::Malformed("central record length overflow"))?;
    }
    let _ = cd_size;
    Ok(out)
}

/// Where the compressed data for a local-header entry actually starts —
/// **not** the central directory's own extra-field length, which is not
/// guaranteed to match (APPNOTE.TXT allows the two to differ).
fn local_data_offset(zip: &[u8], local_header_offset: u64) -> Result<usize, ZipError> {
    let at = usize::try_from(local_header_offset)
        .map_err(|_| ZipError::Malformed("local header offset overflow"))?;
    if u32_le(zip, at) != Some(LOCAL_SIG) {
        return Err(ZipError::Malformed(
            "local file header has the wrong signature",
        ));
    }
    let name_len =
        usize::from(u16_le(zip, at + 26).ok_or(ZipError::Malformed("truncated local header"))?);
    let extra_len =
        usize::from(u16_le(zip, at + 28).ok_or(ZipError::Malformed("truncated local header"))?);
    at.checked_add(30 + name_len + extra_len)
        .ok_or(ZipError::Malformed("local header length overflow"))
}

/// The standard IEEE 802.3 CRC-32 (reflected), generated at call time. This
/// is a public algorithm with one correct table, not authorial expression —
/// see the module docs for why it is not checked in as a literal array.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0_u32; 256];
    let mut n = 0;
    while n < 256 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "n < 256 fits in u8 by construction"
        )]
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        if let Some(slot) = table.get_mut(n as usize) {
            *slot = c;
        }
        n += 1;
    }
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        let t = table.get(idx).copied().unwrap_or(0);
        crc = t ^ (crc >> 8);
    }
    !crc
}

/// Extract `member` from a ZIP archive's raw bytes.
///
/// # Errors
/// See [`ZipError`]. A truncated/malformed archive, an absent member, an
/// unsupported compression method and a CRC-32 mismatch are all distinct,
/// named failures rather than a generic parse error.
pub fn extract(zip: &[u8], member: &str) -> Result<Vec<u8>, ZipError> {
    if zip.len() > MAX_ZIP_BYTES {
        return Err(ZipError::TooLarge);
    }
    let mut budget = Budget::new(Limits::strict());
    let entries = read_central_directory(zip, &mut budget)?;
    let entry = entries
        .iter()
        .find(|e| e.name == member)
        .ok_or_else(|| ZipError::MemberNotFound(member.to_owned()))?;

    let data_start = local_data_offset(zip, entry.local_header_offset)?;
    let compressed_len = usize::try_from(entry.compressed_size)
        .map_err(|_| ZipError::Malformed("compressed size overflow"))?;
    let compressed = zip
        .get(data_start..data_start + compressed_len)
        .ok_or(ZipError::Malformed("member data runs past end of archive"))?;

    let uncompressed_len = usize::try_from(entry.uncompressed_size)
        .map_err(|_| ZipError::Malformed("uncompressed size overflow"))?;

    let out = match entry.method {
        0 => {
            let mut buf = budget.alloc::<u8>(compressed.len())?;
            if let Some(dst) = buf.get_mut(..compressed.len()) {
                dst.copy_from_slice(compressed);
            }
            buf
        }
        8 => inflate::inflate(compressed, uncompressed_len, &mut budget)?,
        other => {
            return Err(ZipError::UnsupportedMethod {
                member: member.to_owned(),
                method: other,
            });
        }
    };

    if crc32(&out) != entry.crc32 {
        return Err(ZipError::Crc32Mismatch {
            member: member.to_owned(),
        });
    }
    Ok(out)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{crc32, extract};

    /// Hand-built ZIP with two stored (method 0) members, so this test needs
    /// no compressor -- just the container format. Verified against Python's
    /// own `zipfile` while writing this (`python3 -c "import zipfile; ..."`),
    /// not shipped as a fixture.
    fn build_stored_zip(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let mut offsets = Vec::new();
        for (name, data) in members {
            offsets.push(out.len() as u32);
            let crc = crc32(data);
            out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
            out.extend_from_slice(&20_u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0_u16.to_le_bytes()); // flags
            out.extend_from_slice(&0_u16.to_le_bytes()); // method: store
            out.extend_from_slice(&0_u16.to_le_bytes()); // time
            out.extend_from_slice(&0_u16.to_le_bytes()); // date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // compressed
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // uncompressed
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes()); // extra len
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
        }
        let cd_start = out.len() as u32;
        for ((name, data), &offset) in members.iter().zip(&offsets) {
            let crc = crc32(data);
            central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
            central.extend_from_slice(&20_u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20_u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0_u16.to_le_bytes()); // flags
            central.extend_from_slice(&0_u16.to_le_bytes()); // method
            central.extend_from_slice(&0_u16.to_le_bytes()); // time
            central.extend_from_slice(&0_u16.to_le_bytes()); // date
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes()); // extra len
            central.extend_from_slice(&0_u16.to_le_bytes()); // comment len
            central.extend_from_slice(&0_u16.to_le_bytes()); // disk number
            central.extend_from_slice(&0_u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        out.extend_from_slice(&central);
        let cd_size = central.len() as u32;
        out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // disk number
        out.extend_from_slice(&0_u16.to_le_bytes()); // disk with CD
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_start.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // comment len
        out
    }

    #[test]
    fn extracts_a_stored_member_by_name() {
        let zip = build_stored_zip(&[("a.txt", b"hello"), ("b/bit.264", b"bitstream bytes here")]);
        let got = extract(&zip, "b/bit.264").expect("extracts");
        assert_eq!(got, b"bitstream bytes here");
    }

    #[test]
    fn a_missing_member_is_a_named_error() {
        let zip = build_stored_zip(&[("a.txt", b"hello")]);
        let err = extract(&zip, "does-not-exist").unwrap_err();
        assert!(matches!(err, super::ZipError::MemberNotFound(_)));
    }

    #[test]
    fn crc32_matches_a_known_vector() {
        // The canonical CRC-32 check value everyone can verify by hand.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_corrupted_member_fails_its_own_crc_check() {
        let mut zip = build_stored_zip(&[("a.bin", b"0123456789")]);
        // Flip a byte inside the stored payload without touching any header
        // or the central directory's own recorded CRC/size, so this is
        // purely a "the bytes on disk do not match what the archive itself
        // claims" case. Locate "0123456789" and corrupt one byte of the
        // *local* copy (the first match — the central directory carries no
        // second copy of the payload, only its metadata).
        if let Some(pos) = zip.windows(10).position(|w| w == b"0123456789") {
            zip[pos] = b'X';
        }
        let err = extract(&zip, "a.bin").unwrap_err();
        assert!(matches!(err, super::ZipError::Crc32Mismatch { .. }));
    }
}
