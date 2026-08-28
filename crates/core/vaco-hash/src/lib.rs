#![forbid(unsafe_code)]
//! Checksums and digests, behind one door.
//!
//! # What it is
//!
//! The single owner of `crc`, `md-5`, `sha1` and `sha2` (**D11**: one third-party
//! media crate, one Vaco crate that reaches it). Two components need the same
//! primitives — `vaco-probe`'s `-show_data_hash` and `vaco-mux-hash`'s eight
//! checksum muxers — and before this crate existed they each declared the four
//! dependencies and each defined their own algorithm enum, one spelled
//! `HashAlg` and the other `HashAlgo`, with the same fifteen names and the same
//! labels. `cargo xtask owner-gate` caught the dependency half and
//! `cargo xtask dup-check` would have caught the rest.
//!
//! That mattered more here than duplication usually does. In both places **the
//! checksum IS the printed output**, so two implementations that disagree by a
//! seed or a byte order are a byte-level divergence from the reference by
//! definition — and one of the two is `framemd5`, which the differential
//! harness uses as its own oracle (D6). An oracle with a private copy of the
//! algorithm is not an oracle.
//!
//! # The two things measurement established
//!
//! reference muxers spell "run this hash over these bytes."
//!
//! ## Two families, not one
//!
//! Measured against ffmpeg 8.1 (`ffmpeg -f hash -hash <name> -` and
//! `ffmpeg -f crc`/`-f framecrc` on a one- and two-frame `testsrc`, `LC_ALL=C`
//! — see `docs/format/vaco-mux-hash.md` for the transcripts):
//!
//! - The **generic family** (`hash`, `framehash`, `streamhash`, and — this was
//!   not obvious — `md5`/`framemd5`, which are the generic family with a fixed
//!   algorithm rather than a hand-written one) always uses each algorithm's
//!   ordinary, textbook definition. `-hash adler32` on a whole file gives the
//!   same bytes as `crc`'s dedicated muxer; `-hash crc32` gives the same bytes
//!   as Python's `zlib.crc32`.
//! - The **dedicated `crc`/`framecrc` pair** is its own code, not a thin
//!   wrapper over the generic family, and the two diverge in one specific,
//!   easy-to-miss way: `crc` (whole file) computes ordinary Adler-32 (seed
//!   `a=1, b=0`, RFC 1950) — identical to `-hash adler32` — but `framecrc`
//!   (per packet) seeds Adler-32 with `a=0, b=0` instead. This is not a typo
//!   in this crate; it is what the reference does. `framehash -hash adler32`
//!   on the same packets gives the *standard*-seeded value, proving the zero
//!   seed belongs to `framecrc` specifically and not to "Adler-32 hashed one
//!   packet at a time" in general. See [`ADLER32_FRAME_SEED`].
//!
//! ## Why this list is short
//!
//! The reference's `-hash` accepts fifteen names. Five of them — `murmur3`
//! and the four `RIPEMD*` widths — have no pre-declared pure-Rust crate in
//! `[workspace.dependencies]`, and adding one is a D10 decision this crate is
//! not positioned to make. [`HashAlgo`] names all fifteen and
//! [`HashAlgo::implemented`] says which ten this build can compute. Naming and
//! refusing beats omitting: `-show_data_hash RIPEMD160` printing an ordinary
//! block with the field silently missing is indistinguishable from success,
//! and a differential harness scores it as a pass.
//!
//! Also measured: the reference's own name for SHA-1 is **`sha160`**, not
//! `sha1`; `ffmpeg -f hash -hash sha1 -` refuses the option and `sha160`
//! succeeds. [`HashAlgo::Sha160`] is named to match.
//!
//! # How to change it
//!
//! Adding an algorithm is a D10 dependency decision first and a variant here
//! second. Add the name to [`NAMES`] in the reference's own order — that order
//! is what `-show_data_hash` prints when it rejects a name — then the variant,
//! then arms in [`HashAlgo::label`], [`HashAlgo::hex_len`],
//! [`HashAlgo::digest_hex`] and [`RunningHash`]. The enum is deliberately not
//! `#[non_exhaustive]`: a `match` that silently accepted a new variant without
//! a new hasher arm would produce a wrong digest rather than a compile error.

#![allow(clippy::doc_markdown, reason = "algorithm names are not identifiers")]

use md5::Digest as _;
use vaco_core::{Error, Result};

/// One of the fifteen `-hash` algorithm names the reference accepts.
///
/// Ten are computable here; [`HashAlgo::implemented`] says which. The other
/// five are still named, because a caller that has to reject a name needs to
/// tell the difference between "not a hash" and "a hash this build cannot do",
/// and the reference's rejection message lists all fifteen.
///
/// Deliberately not `#[non_exhaustive]`: the set is closed until a workspace
/// dependency decision reopens it (see the module docs), and a `match` that
/// silently accepted a new variant without a new hasher arm would be a wrong
/// digest, not a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    Md5,
    /// Named but not computable: no pure-Rust crate is pre-declared (D10).
    Murmur3,
    /// Named but not computable: no pure-Rust crate is pre-declared (D10).
    Ripemd128,
    /// Named but not computable: no pure-Rust crate is pre-declared (D10).
    Ripemd160,
    /// Named but not computable: no pure-Rust crate is pre-declared (D10).
    Ripemd256,
    /// Named but not computable: no pure-Rust crate is pre-declared (D10).
    Ripemd320,
    Sha160,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
    /// `SHA-512/224` — a distinct IV, not a truncation of [`HashAlgo::Sha512`].
    Sha512_224,
    /// `SHA-512/256` — a distinct IV, not a truncation of [`HashAlgo::Sha512`].
    Sha512_256,
    /// The ordinary reflected IEEE polynomial (`CRC-32/ISO-HDLC`): init and
    /// xorout all-ones. Confirmed against Python's `zlib.crc32` on the same
    /// bytes — see the crate docs' probe transcript.
    Crc32,
    /// RFC 1950 Adler-32, standard seed (`a=1, b=0`). **Not** what `framecrc`
    /// uses per packet; see the module docs and [`ADLER32_FRAME_SEED`].
    Adler32,
}

impl HashAlgo {
    /// The label the reference prints before `=`.
    ///
    /// Measured, not cased by convention: ten of these are upper case and two
    /// (`murmur3`, `adler32` — the latter is the only one of the ten this
    /// crate implements) are lower case in the reference's own output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Murmur3 => "murmur3",
            Self::Ripemd128 => "RIPEMD128",
            Self::Ripemd160 => "RIPEMD160",
            Self::Ripemd256 => "RIPEMD256",
            Self::Ripemd320 => "RIPEMD320",
            Self::Sha160 => "SHA160",
            Self::Sha224 => "SHA224",
            Self::Sha256 => "SHA256",
            Self::Sha384 => "SHA384",
            Self::Sha512 => "SHA512",
            Self::Sha512_224 => "SHA512/224",
            Self::Sha512_256 => "SHA512/256",
            Self::Crc32 => "CRC32",
            Self::Adler32 => "adler32",
        }
    }

    /// Hex digest length in characters (two per byte), or `None` for one of
    /// the five this build cannot compute.
    #[must_use]
    pub const fn hex_len(self) -> Option<usize> {
        Some(match self {
            Self::Crc32 | Self::Adler32 => 8,
            Self::Md5 => 32,
            Self::Sha160 => 40,
            Self::Sha224 | Self::Sha512_224 => 56,
            Self::Sha256 | Self::Sha512_256 => 64,
            Self::Sha384 => 96,
            Self::Sha512 => 128,
            Self::Murmur3
            | Self::Ripemd128
            | Self::Ripemd160
            | Self::Ripemd256
            | Self::Ripemd320 => return None,
        })
    }

    /// Whether this build can compute it.
    ///
    /// `murmur3` and the four RIPEMD widths have no pre-declared pure-Rust
    /// dependency (D10 makes adding one a reviewed decision), so they are
    /// **named and refused** rather than silently producing a wrong digest or
    /// an empty field. Ten of the fifteen work.
    #[must_use]
    pub const fn implemented(self) -> bool {
        !matches!(
            self,
            Self::Murmur3 | Self::Ripemd128 | Self::Ripemd160 | Self::Ripemd256 | Self::Ripemd320
        )
    }

    /// Look a name up, case-insensitively.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|&(_, a)| a)
    }
}

/// The fifteen names, **in the reference's own order** — which is the order it
/// prints them in when it rejects one ("Known algorithms: MD5 murmur3
/// RIPEMD128 …"), so the order is observable behaviour rather than taste.
pub const NAMES: &[(&str, HashAlgo)] = &[
    ("MD5", HashAlgo::Md5),
    ("murmur3", HashAlgo::Murmur3),
    ("RIPEMD128", HashAlgo::Ripemd128),
    ("RIPEMD160", HashAlgo::Ripemd160),
    ("RIPEMD256", HashAlgo::Ripemd256),
    ("RIPEMD320", HashAlgo::Ripemd320),
    ("SHA160", HashAlgo::Sha160),
    ("SHA224", HashAlgo::Sha224),
    ("SHA256", HashAlgo::Sha256),
    ("SHA512/224", HashAlgo::Sha512_224),
    ("SHA512/256", HashAlgo::Sha512_256),
    ("SHA384", HashAlgo::Sha384),
    ("SHA512", HashAlgo::Sha512),
    ("CRC32", HashAlgo::Crc32),
    ("adler32", HashAlgo::Adler32),
];

impl HashAlgo {
    /// One-shot lower-case hex digest of `data`.
    ///
    /// Used by the per-packet muxers ([`crate::frame`]), which need a fresh
    /// hash per call rather than a running one.
    #[must_use]
    pub fn digest_hex(self, data: &[u8]) -> Option<String> {
        Some(match self {
            Self::Md5 => hex(&md5::Md5::digest(data)),
            Self::Sha160 => hex(&sha1::Sha1::digest(data)),
            Self::Sha224 => hex(&sha2::Sha224::digest(data)),
            Self::Sha256 => hex(&sha2::Sha256::digest(data)),
            Self::Sha384 => hex(&sha2::Sha384::digest(data)),
            Self::Sha512 => hex(&sha2::Sha512::digest(data)),
            Self::Sha512_224 => hex(&sha2::Sha512_224::digest(data)),
            Self::Sha512_256 => hex(&sha2::Sha512_256::digest(data)),
            Self::Crc32 => hex(&crc32(data).to_be_bytes()),
            Self::Adler32 => hex(&adler32_seeded(data, 1, 0).to_be_bytes()),
            Self::Murmur3
            | Self::Ripemd128
            | Self::Ripemd160
            | Self::Ripemd256
            | Self::Ripemd320 => return None,
        })
    }

    /// `LABEL:hex`, the spelling `-show_data_hash` prints.
    #[must_use]
    pub fn labelled_digest(self, data: &[u8]) -> Option<String> {
        Some(format!("{}:{}", self.label(), self.digest_hex(data)?))
    }

    /// A fresh incremental hasher for this algorithm.
    ///
    /// Used by the whole-file and per-stream muxers ([`crate::whole`],
    /// [`crate::stream`]), which fold many packets into one digest without
    /// buffering the concatenation in memory.
    #[must_use]
    pub fn running(self) -> Option<RunningHash> {
        RunningHash::new(self)
    }
}

/// The per-packet seed `framecrc` uses for its Adler-32, measured to differ
/// from the standard `(1, 0)` RFC 1950 seed — see the module docs.
pub const ADLER32_FRAME_SEED: (u32, u32) = (0, 0);

/// The seed the whole-file `crc` muxer and `-hash adler32` both use.
pub const ADLER32_STANDARD_SEED: (u32, u32) = (1, 0);

/// An incremental digest, fed bytes across many [`RunningHash::update`] calls
/// and consumed once by [`RunningHash::finish_hex`].
///
/// A small hand-written enum rather than `Box<dyn Update>`: every algorithm
/// this crate supports has a fixed-size state (a `u32` pair or a stack-sized
/// hasher struct), so boxing would trade a known-size value for an allocation
/// and a vtable for no benefit — `#![forbid(unsafe_code)]` does not need the
/// indirection either.
pub enum RunningHash {
    Md5(md5::Md5),
    Sha160(sha1::Sha1),
    Sha224(sha2::Sha224),
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
    Sha512_224(sha2::Sha512_224),
    Sha512_256(sha2::Sha512_256),
    Crc32(crc::Digest<'static, u32>),
    /// Adler-32's whole state is just the two running sums.
    Adler32(u32, u32),
}

impl RunningHash {
    #[must_use]
    fn new(algo: HashAlgo) -> Option<Self> {
        Some(match algo {
            HashAlgo::Md5 => Self::Md5(md5::Md5::new()),
            HashAlgo::Sha160 => Self::Sha160(sha1::Sha1::new()),
            HashAlgo::Sha224 => Self::Sha224(sha2::Sha224::new()),
            HashAlgo::Sha256 => Self::Sha256(sha2::Sha256::new()),
            HashAlgo::Sha384 => Self::Sha384(sha2::Sha384::new()),
            HashAlgo::Sha512 => Self::Sha512(sha2::Sha512::new()),
            HashAlgo::Sha512_224 => Self::Sha512_224(sha2::Sha512_224::new()),
            HashAlgo::Sha512_256 => Self::Sha512_256(sha2::Sha512_256::new()),
            HashAlgo::Crc32 => Self::Crc32(CRC32_TABLE.digest()),
            HashAlgo::Adler32 => {
                let (a, b) = ADLER32_STANDARD_SEED;
                Self::Adler32(a, b)
            }
            HashAlgo::Murmur3
            | HashAlgo::Ripemd128
            | HashAlgo::Ripemd160
            | HashAlgo::Ripemd256
            | HashAlgo::Ripemd320 => return None,
        })
    }

    /// A running Adler-32 seeded with `seed` rather than the algorithm's own
    /// default — the escape hatch [`crate::whole`] uses for the `crc` muxer,
    /// which is ordinary Adler-32 under a different label, and [`crate::frame`]
    /// does not: `framecrc` hashes one packet per call, which
    /// [`HashAlgo::digest_hex`] already covers with an explicit seed.
    #[must_use]
    pub const fn adler32_seeded(a0: u32, b0: u32) -> Self {
        Self::Adler32(a0, b0)
    }

    /// Fold `data` into the running digest.
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md5(h) => h.update(data),
            Self::Sha160(h) => h.update(data),
            Self::Sha224(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
            Self::Sha384(h) => h.update(data),
            Self::Sha512(h) => h.update(data),
            Self::Sha512_224(h) => h.update(data),
            Self::Sha512_256(h) => h.update(data),
            Self::Crc32(d) => d.update(data),
            Self::Adler32(a, b) => update_adler32(a, b, data),
        }
    }

    /// Finalise and render as lower-case hex.
    #[must_use]
    pub fn finish_hex(self) -> String {
        match self {
            Self::Md5(h) => hex(&h.finalize()),
            Self::Sha160(h) => hex(&h.finalize()),
            Self::Sha224(h) => hex(&h.finalize()),
            Self::Sha256(h) => hex(&h.finalize()),
            Self::Sha384(h) => hex(&h.finalize()),
            Self::Sha512(h) => hex(&h.finalize()),
            Self::Sha512_224(h) => hex(&h.finalize()),
            Self::Sha512_256(h) => hex(&h.finalize()),
            Self::Crc32(d) => hex(&d.finalize().to_be_bytes()),
            Self::Adler32(a, b) => hex(&((b << 16) | a).to_be_bytes()),
        }
    }

    /// As [`RunningHash::finish_hex`], but the raw 32 bits — for the `crc`
    /// muxer's `0x%08x` spelling, which the generic `<ALGO>=<hex>` muxers
    /// don't use.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if this is not a 32-bit algorithm (`Crc32` or
    /// `Adler32`); every caller in this crate only ever builds one of those
    /// two through this path, so the error is a defensive bound, not a
    /// reachable one.
    pub fn finish_u32(self) -> Result<u32> {
        match self {
            Self::Crc32(d) => Ok(d.finalize()),
            Self::Adler32(a, b) => Ok((b << 16) | a),
            _ => Err(Error::InvalidData(
                "finish_u32 called on a non-32-bit running hash",
            )),
        }
    }
}

impl core::fmt::Debug for RunningHash {
    /// Hand-written: several of the wrapped hasher types are not `Debug`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            Self::Md5(_) => "Md5",
            Self::Sha160(_) => "Sha160",
            Self::Sha224(_) => "Sha224",
            Self::Sha256(_) => "Sha256",
            Self::Sha384(_) => "Sha384",
            Self::Sha512(_) => "Sha512",
            Self::Sha512_224(_) => "Sha512_224",
            Self::Sha512_256(_) => "Sha512_256",
            Self::Crc32(_) => "Crc32",
            Self::Adler32(..) => "Adler32",
        };
        f.debug_tuple("RunningHash").field(&name).finish()
    }
}

// A `static`, not a `const`: `Digest<'a, u32>` borrows the table, and a
// `const` re-materialises at every use site with a use-site-local lifetime —
// exactly wrong for a digest a caller holds across many `update` calls. The
// `static` has one fixed address for the process's lifetime.
static CRC32_TABLE: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

/// One-shot CRC-32/ISO-HDLC (the ordinary reflected IEEE polynomial).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    CRC32_TABLE.checksum(data)
}

/// NUT's own CRC-32, for `vaco-format-nut`'s `packet_footer`/
/// `header_checksum` fields.
///
/// The NUT specification says only "Generator polynomial is 0x104C11DB7.
/// Starting value is zero" — enough to name the polynomial but not the bit
/// order, reflection or final XOR, none of which [`crc32`] (CRC-32/ISO-HDLC:
/// reflected, seeded all-ones, xored all-ones) shares. **Measured**, not
/// inferred from the spec text: computing this exact configuration (same
/// generator polynomial as the well-known `CRC-32/MPEG-2`, but seeded with
/// zero instead of `CRC-32/MPEG-2`'s all-ones) over a real `ffmpeg -f nut`
/// main header's payload reproduces that file's own stored
/// `packet_footer` checksum exactly; every other seed/reflection/xorout
/// combination tried did not. `check`/`residue` below were derived from
/// that same measurement (`residue` follows from `crc(message ++
/// crc(message)) == 0`, the standard self-consistency identity for a
/// zero-seeded, non-reflected, non-xored CRC).
///
/// D11: this is still exactly one *crate* owning `crc` (this one) — the
/// `crc` crate this file already depends on generates the table for any
/// [`crc::Algorithm`] parameterisation, so a second named variant here is
/// not a second implementation, only a second (measured, documented)
/// configuration of the one already-approved dependency.
const CRC32_NUT: crc::Algorithm<u32> = crc::Algorithm {
    width: 32,
    poly: 0x04C1_1DB7,
    init: 0,
    refin: false,
    refout: false,
    xorout: 0,
    check: 0x89a1_897f,
    residue: 0,
};

static CRC32_NUT_TABLE: crc::Crc<u32> = crc::Crc::<u32>::new(&CRC32_NUT);

/// One-shot NUT CRC-32. See [`CRC32_NUT`] for what makes this different
/// from [`crc32`].
#[must_use]
pub fn crc32_nut(data: &[u8]) -> u32 {
    CRC32_NUT_TABLE.checksum(data)
}

/// One-shot Adler-32 (RFC 1950 §9) from an explicit seed.
///
/// Written out rather than pulled in from a crate: it is nine lines, and a
/// dependency that only ever computed this would be a D10 adoption for no
/// reduction in code (the same call `vaco-probe` made for the same reason).
#[must_use]
pub fn adler32_seeded(data: &[u8], a0: u32, b0: u32) -> u32 {
    let mut a = a0;
    let mut b = b0;
    update_adler32(&mut a, &mut b, data);
    (b << 16) | a
}

fn update_adler32(a: &mut u32, b: &mut u32, data: &[u8]) {
    const BASE: u32 = 65_521;
    for byte in data {
        *a = (*a + u32::from(*byte)) % BASE;
        *b = (*b + *a) % BASE;
    }
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    /// The internationally-documented Adler-32 test vector, standard seed.
    #[test]
    fn adler32_matches_the_rfc_1950_check_value() {
        assert_eq!(adler32_seeded(b"123456789", 1, 0), 0x091E_01DE);
    }

    /// Measured: `framecrc` on ffmpeg 8.1 gives `0x091501dd` for this same
    /// input muxed through the `s8` raw-audio demuxer (see
    /// `docs/format/vaco-mux-hash.md`), which is exactly the zero-seed variant.
    #[test]
    fn the_frame_seed_is_not_the_standard_seed() {
        let (a0, b0) = ADLER32_FRAME_SEED;
        assert_eq!(adler32_seeded(b"123456789", a0, b0), 0x0915_01DD);
        assert_ne!(ADLER32_FRAME_SEED, ADLER32_STANDARD_SEED);
    }

    /// The RFC 4321 / common CRC-32 check value for `"123456789"`.
    #[test]
    fn crc32_matches_the_ieee_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    /// The check value for the same `"123456789"` string under
    /// [`CRC32_NUT`]'s parameters, computed independently (not copied from
    /// [`CRC32_NUT`]'s own `check` field — that would only prove the field
    /// and the table agree with each other, not that either is right).
    #[test]
    fn crc32_nut_matches_its_own_documented_check_value() {
        assert_eq!(crc32_nut(b"123456789"), 0x89A1_897F);
    }

    /// A zero-seeded, non-reflected, non-xored CRC has the property that
    /// appending a message's own (big-endian) checksum to itself and
    /// recomputing gives zero — the identity [`CRC32_NUT`]'s `residue` was
    /// derived from. Checked here independently of that derivation.
    #[test]
    fn crc32_nut_message_plus_its_own_checksum_reduces_to_zero() {
        let msg = b"123456789";
        let sum = crc32_nut(msg);
        let mut extended = msg.to_vec();
        extended.extend_from_slice(&sum.to_be_bytes());
        assert_eq!(crc32_nut(&extended), 0);
    }

    /// The measured value this crate exists to reproduce: 170 real bytes —
    /// `ffmpeg -f nut`'s (8.1) main header packet's payload, from the first
    /// byte after `packet_header` (startcode + `forward_ptr`) to the last
    /// byte before `packet_footer`'s checksum, out of a real two-stream
    /// (mpeg4 video + mp3 audio) capture — against that same file's own
    /// stored `packet_footer` checksum, read directly off the file's bytes.
    /// This is what ruled out every other CRC-32 configuration tried
    /// (reflected, all-ones seed, xor-all-ones, ...): none of them
    /// reproduced this value, only [`CRC32_NUT`]'s parameters did.
    #[test]
    fn crc32_nut_matches_a_real_nut_main_header_checksum() {
        // clippy: this is data, not a magic-number computation.
        #[allow(clippy::unreadable_literal, reason = "raw measured bytes")]
        const MAIN_HEADER_PAYLOAD: [u8; 170] = [
            0x03, 0x02, 0x81, 0xff, 0x7f, 0x02, 0x01, 0x83, 0x90, 0x00, 0x01, 0x82,
            0xf7, 0x00, 0xc0, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xa0,
            0x00, 0x02, 0x01, 0x01, 0x28, 0x01, 0x00, 0x29, 0x00, 0x21, 0x01, 0x9f,
            0x7f, 0x20, 0x02, 0x9f, 0x7f, 0x7b, 0x28, 0x08, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x81, 0xc0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
            0x04, 0x29, 0x00, 0x01, 0x06, 0x00, 0x02, 0x01, 0x00, 0x00, 0x01, 0x01,
            0x08, 0x00, 0x02, 0x01, 0x01, 0x00, 0x01, 0x81, 0xc0, 0x80, 0x80, 0x80,
            0x80, 0x80, 0x80, 0x80, 0x01, 0x00, 0x01, 0x06, 0x91, 0x7f, 0x02, 0x01,
            0x00, 0x00, 0x01, 0x01, 0x08, 0x91, 0x7f, 0x02, 0x01, 0x01, 0x00, 0x01,
            0x81, 0xc0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01, 0x00, 0x21,
            0x08, 0x91, 0x7f, 0x78, 0x01, 0x00, 0x00, 0x78, 0x81, 0xc0, 0x80, 0x80,
            0x80, 0x80, 0x80, 0x80, 0x80, 0x01, 0x04, 0xc0, 0x00, 0x06, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0x06, 0x03, 0x00, 0x00, 0x01, 0x04, 0x00, 0x00,
            0x01, 0xb6, 0x02, 0xff, 0xfa, 0x02, 0xff, 0xfb, 0x02, 0xff, 0xfc, 0x02,
            0xff, 0xfd,
        ];
        assert_eq!(crc32_nut(&MAIN_HEADER_PAYLOAD), 0xAD46_C507);
    }

    #[test]
    fn digest_hex_and_running_agree() {
        let data = b"the quick brown fox";
        for algo in [
            HashAlgo::Md5,
            HashAlgo::Sha160,
            HashAlgo::Sha224,
            HashAlgo::Sha256,
            HashAlgo::Sha384,
            HashAlgo::Sha512,
            HashAlgo::Sha512_224,
            HashAlgo::Sha512_256,
            HashAlgo::Crc32,
            HashAlgo::Adler32,
        ] {
            let one_shot = algo.digest_hex(data).expect("implemented");
            let mut running = algo.running().expect("implemented");
            running.update(&data[..5]);
            running.update(&data[5..]);
            let folded = running.finish_hex();
            assert_eq!(
                one_shot, folded,
                "{algo:?} disagreed between one-shot and running"
            );
            assert_eq!(Some(one_shot.len()), algo.hex_len(), "{algo:?} hex length");
        }
    }

    #[test]
    fn the_five_unimplemented_names_are_named_and_refused_not_omitted() {
        for algo in [
            HashAlgo::Murmur3,
            HashAlgo::Ripemd128,
            HashAlgo::Ripemd160,
            HashAlgo::Ripemd256,
            HashAlgo::Ripemd320,
        ] {
            assert!(!algo.implemented(), "{algo:?}");
            assert!(algo.digest_hex(b"x").is_none(), "{algo:?}");
            assert!(algo.running().is_none(), "{algo:?}");
            assert!(algo.hex_len().is_none(), "{algo:?}");
            // Named, though: a caller refusing one has to be able to say
            // *which* it is refusing, and the reference lists all fifteen.
            assert!(HashAlgo::parse(algo.label()) == Some(algo), "{algo:?}");
        }
    }

    #[test]
    fn every_name_round_trips_through_parse_in_the_reference_order() {
        for (name, algo) in NAMES {
            assert_eq!(HashAlgo::parse(name), Some(*algo), "{name}");
            assert_eq!(algo.label(), *name, "{name}");
        }
        assert_eq!(NAMES.len(), 15);
        // Case-insensitive, because `-hash md5` and `-hash MD5` both work.
        assert_eq!(HashAlgo::parse("md5"), Some(HashAlgo::Md5));
        assert_eq!(HashAlgo::parse("sha1"), None, "the name is sha160");
    }

    #[test]
    fn crc32_running_matches_the_u32_finisher() {
        let mut h = HashAlgo::Crc32.running().expect("implemented");
        h.update(b"123456789");
        assert_eq!(h.finish_u32().unwrap(), 0xCBF4_3926);
    }
}
