//! `-show_data`, `-data_dump_format` and `-show_data_hash`: the packet payload
//! as text.
//!
//! # What it is
//!
//! Three options that add a field to `[PACKET]`. `data` is the payload rendered
//! as an `xxd` hexdump or as base64; `data_hash` is `NAME:hex`. Both are plain
//! strings to every writer, which is why the `json` output carries the newlines
//! as `\n` escapes rather than as structure.
//!
//! # How it works
//!
//! [`xxd`] and [`base64`] build the string; [`HashAlg`] names an algorithm and
//! [`HashAlg::digest`] runs it. Nothing here touches the section machinery —
//! `show::packet` decides *whether* to call it and in what order.
//!
//! # Provenance
//!
//! Every rule below is a run of `ffprobe` 8.1 (Homebrew, arm64 macOS) against a
//! rawvideo file of exactly `n` bytes, which is the cheapest way to control a
//! packet's length precisely:
//!
//! ```sh
//! python3 -c 'open("rawN.gray","wb").write(bytes(range(N)))'
//! ffprobe -v quiet -of json -show_packets -show_data \
//!         -f rawvideo -video_size Nx1 -pixel_format gray rawN.gray
//! ```
//!
//! Run for n ∈ {1,2,3,4,5,15,16,17,31,32,33}, which pins the partial-line
//! padding at every position within a group and at both group boundaries.
//!
//! What that established:
//!
//! * The value **begins with a newline** and every line ends with one, so the
//!   default writer prints `data=` and then an apparently blank line before the
//!   next field. That blank line is the value's own trailing `\n` plus the
//!   writer's.
//! * The ASCII column starts at byte 51 of the line, always. A full line is
//!   `"%08x: "` (10) + eight `"%04x "` groups (40) + one separator space.
//! * A missing byte contributes two spaces, so the group separator survives.
//! * `-data_dump_format base64` replaces the whole thing with the payload
//!   wrapped at **80 base64 characters** per line. The 17-byte file does not
//!   show that; the 5 171-byte packet does.
//! * An unknown dump format is an error: `Unknown data dump format with name
//!   'hex'`, exit 1.
//! * `data` is emitted **before** `data_hash` when both are asked for.

use core::fmt::Write as _;

use vaco_textformat::num::hex_byte;

/// How `-show_data` renders a payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DumpFormat {
    /// `-data_dump_format xxd`, and the default.
    #[default]
    Xxd,
    /// `-data_dump_format base64`.
    Base64,
}

impl DumpFormat {
    /// Parse a `-data_dump_format` value.
    ///
    /// The two names are the reference's own, listed by `ffprobe -h`:
    /// "available formats are: xxd, base64".
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "xxd" => Some(Self::Xxd),
            "base64" => Some(Self::Base64),
            _ => None,
        }
    }

    /// Render `data` for the `data` field.
    #[must_use]
    pub fn render(self, data: &[u8]) -> String {
        match self {
            Self::Xxd => xxd(data),
            Self::Base64 => base64(data),
        }
    }
}

/// Bytes per hexdump line. Not a tunable — it is the observed geometry.
const LINE: usize = 16;

/// The `xxd`-shaped hexdump, exactly as the reference emits it.
///
/// Leading newline, one line per 16 bytes, each line newline-terminated.
#[must_use]
pub fn xxd(data: &[u8]) -> String {
    // 1 + ceil(n/16) * 68: offset 10, hex 41, ascii 16, newline 1.
    let mut out = String::with_capacity(1 + data.len().div_ceil(LINE) * 68);
    out.push('\n');
    for (row, chunk) in data.chunks(LINE).enumerate() {
        let _ = write!(out, "{:08x}: ", row * LINE);
        for i in 0..LINE {
            match chunk.get(i) {
                Some(b) => out.push_str(&hex_byte(*b)),
                // A missing byte still occupies its two columns, which is what
                // keeps the ASCII column at 51 on a short final line.
                None => out.push_str("  "),
            }
            if i % 2 == 1 {
                out.push(' ');
            }
        }
        out.push(' ');
        for b in chunk {
            // `isprint` in the C locale: 0x20 to 0x7e. Space prints as a
            // space, which is visible on the n=33 line and is the reason this
            // is a range test rather than an `is_ascii_graphic` call.
            out.push(if (0x20..=0x7e).contains(b) {
                char::from(*b)
            } else {
                '.'
            });
        }
        out.push('\n');
    }
    out
}

/// The base64 form: a leading newline, the payload wrapped at 80 characters,
/// a trailing newline.
///
/// Standard alphabet with `=` padding. **The wrap is real and was missed the
/// first time**: a 17-byte packet produces one short line and looks
/// unwrapped, so the rule only appears on a payload long enough to exceed 80
/// base64 characters. Measured on the 5 171-byte first video packet of
/// `av.mp4` — 87 lines, 86 of them exactly 80 characters and the last 16.
/// A short input is not a small version of a long one.
#[must_use]
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    /// Base64 characters per output line.
    const WRAP: usize = 80;
    let mut out = String::with_capacity(2 + data.len().div_ceil(3) * 4);
    out.push('\n');
    let mut col = 0usize;
    for chunk in data.chunks(3) {
        if col == WRAP {
            out.push('\n');
            col = 0;
        }
        col += 4;
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let sextet = |shift: u32| {
            let i = ((n >> shift) & 0x3f) as usize;
            char::from(ALPHABET.get(i).copied().unwrap_or(b'A'))
        };
        out.push(sextet(18));
        out.push(sextet(12));
        out.push(if chunk.len() > 1 { sextet(6) } else { '=' });
        out.push(if chunk.len() > 2 { sextet(0) } else { '=' });
    }
    out.push('\n');
    out
}

/// A `-show_data_hash` algorithm.
///
/// The reference accepts fifteen names, **case-insensitively**, and prints the
/// canonical spelling from its own table — which is not the spelling you gave
/// it and is not uniformly upper case. Measured:
///
/// ```sh
/// ffprobe -v error -of csv=p=0:nk=1 -show_entries packet=data_hash \
///         -show_data_hash <name> -f rawvideo -video_size 3x1 \
///         -pixel_format gray raw3.gray
/// ```
///
/// | Given | Printed |
/// |---|---|
/// | `md5`, `MD5` | `MD5` |
/// | `crc32`, `CRC32` | `CRC32` |
/// | `ADLER32`, `adler32` | `adler32` — lower case, alone among the fifteen |
/// | `MuRmUr3` | `murmur3` |
/// | `sha1` | **rejected**; the name is `SHA160` |
///
/// `CRC32` is the ordinary reflected IEEE polynomial and `adler32` is the
/// ordinary one — both confirmed against Python's `zlib` on the same three
/// bytes, which is the cheapest independent oracle available.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HashAlg {
    Md5,
    Murmur3,
    Ripemd128,
    Ripemd160,
    Ripemd256,
    Ripemd320,
    Sha160,
    Sha224,
    Sha256,
    Sha512_224,
    Sha512_256,
    Sha384,
    Sha512,
    Crc32,
    Adler32,
}

/// Why a name this build knows but cannot compute is refused outright.
///
/// The alternative is what the first version did: `digest` returns `None`, the
/// field is `Absent::Omit`, and `-show_data_hash RIPEMD160` prints a perfectly
/// ordinary `[PACKET]` block with no `data_hash` line and exits 0. That is the
/// same failure shape as the empty `[PACKET]` section this work replaced —
/// indistinguishable from success, and a differential harness scores it as a
/// pass. Refusing is the ENOSYS pattern `vaco-cli` set.
pub const HASH_UNSUPPORTED: &str = "-show_data_hash: murmur3 and RIPEMD128/160/256/320 are not implemented \u{2014} no pure-Rust crate for them is pre-declared (D10). The other ten names work.";

/// The fifteen names, in the reference's own order — the order it prints them
/// in when it rejects one ("Known algorithms: MD5 murmur3 RIPEMD128 …").
pub const HASH_NAMES: &[(&str, HashAlg)] = &[
    ("MD5", HashAlg::Md5),
    ("murmur3", HashAlg::Murmur3),
    ("RIPEMD128", HashAlg::Ripemd128),
    ("RIPEMD160", HashAlg::Ripemd160),
    ("RIPEMD256", HashAlg::Ripemd256),
    ("RIPEMD320", HashAlg::Ripemd320),
    ("SHA160", HashAlg::Sha160),
    ("SHA224", HashAlg::Sha224),
    ("SHA256", HashAlg::Sha256),
    ("SHA512/224", HashAlg::Sha512_224),
    ("SHA512/256", HashAlg::Sha512_256),
    ("SHA384", HashAlg::Sha384),
    ("SHA512", HashAlg::Sha512),
    ("CRC32", HashAlg::Crc32),
    ("adler32", HashAlg::Adler32),
];

impl HashAlg {
    /// Look a name up, case-insensitively.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        HASH_NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|&(_, a)| a)
    }

    /// The canonical spelling, which is what the field prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
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
            Self::Sha512_224 => "SHA512/224",
            Self::Sha512_256 => "SHA512/256",
            Self::Sha384 => "SHA384",
            Self::Sha512 => "SHA512",
            Self::Crc32 => "CRC32",
            Self::Adler32 => "adler32",
        }
    }

    /// Whether this build can compute it.
    ///
    /// `murmur3` and the four RIPEMD variants have no pre-declared pure-Rust
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

    /// `NAME:hex`, or `None` for an algorithm this build does not have.
    #[must_use]
    pub fn digest(self, data: &[u8]) -> Option<String> {
        use md5::Digest as _;
        let hex = |bytes: &[u8]| -> String {
            let mut s = String::with_capacity(bytes.len() * 2);
            for b in bytes {
                s.push_str(&hex_byte(*b));
            }
            s
        };
        let body = match self {
            Self::Md5 => hex(&md5::Md5::digest(data)),
            Self::Sha160 => hex(&sha1::Sha1::digest(data)),
            Self::Sha224 => hex(&sha2::Sha224::digest(data)),
            Self::Sha256 => hex(&sha2::Sha256::digest(data)),
            Self::Sha512_224 => hex(&sha2::Sha512_224::digest(data)),
            Self::Sha512_256 => hex(&sha2::Sha512_256::digest(data)),
            Self::Sha384 => hex(&sha2::Sha384::digest(data)),
            Self::Sha512 => hex(&sha2::Sha512::digest(data)),
            Self::Crc32 => hex(&crc32(data).to_be_bytes()),
            Self::Adler32 => hex(&adler32(data).to_be_bytes()),
            _ => return None,
        };
        Some(format!("{}:{body}", self.name()))
    }
}

/// CRC-32/ISO-HDLC — the zlib polynomial, reflected, init and xorout all-ones.
fn crc32(data: &[u8]) -> u32 {
    const ALG: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
    ALG.checksum(data)
}

/// Adler-32, RFC 1950 §9.
///
/// Written out rather than pulled in: it is nine lines, and a dependency that
/// only ever computes this would be a D10 adoption for no reduction in code.
fn adler32(data: &[u8]) -> u32 {
    const BASE: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for byte in data {
        a = (a + u32::from(*byte)) % BASE;
        b = (b + a) % BASE;
    }
    (b << 16) | a
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    /// The bytes every geometry case below was measured with.
    fn ramp(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| u8::try_from(i & 0xff).unwrap_or_default())
            .collect()
    }

    #[test]
    fn the_hexdump_geometry_is_the_measured_one() {
        // Captured verbatim from `ffprobe -of json … -show_data`.
        assert_eq!(
            xxd(&ramp(1)),
            "\n00000000: 00                                       .\n"
        );
        assert_eq!(
            xxd(&ramp(3)),
            "\n00000000: 0001 02                                  ...\n"
        );
        assert_eq!(
            xxd(&ramp(15)),
            "\n00000000: 0001 0203 0405 0607 0809 0a0b 0c0d 0e    ...............\n"
        );
        assert_eq!(
            xxd(&ramp(16)),
            "\n00000000: 0001 0203 0405 0607 0809 0a0b 0c0d 0e0f  ................\n"
        );
        assert_eq!(
            xxd(&ramp(17)),
            "\n00000000: 0001 0203 0405 0607 0809 0a0b 0c0d 0e0f  ................\n\
             00000010: 10                                       .\n"
        );
    }

    #[test]
    fn the_ascii_column_starts_at_byte_51_on_every_line() {
        for n in [1usize, 2, 3, 4, 5, 15, 16, 17, 31, 32, 33] {
            for line in xxd(&ramp(n)).lines().filter(|l| !l.is_empty()) {
                assert!(line.len() >= 51, "n={n}: {line:?}");
                // Byte 50 is the separator space; 51 is the first ASCII cell.
                assert_eq!(line.as_bytes().get(50), Some(&b' '), "n={n}");
            }
        }
    }

    #[test]
    fn byte_0x20_prints_as_a_space_and_0x7f_as_a_dot() {
        // Observed on the n=33 run, whose last byte is 0x20 and renders blank.
        let s = xxd(&[0x20, 0x7e, 0x7f, 0x00]);
        assert!(s.ends_with(" ~..\n"), "{s:?}");
    }

    #[test]
    fn the_dump_is_empty_but_still_a_newline_for_a_zero_length_payload() {
        assert_eq!(xxd(&[]), "\n");
        assert_eq!(base64(&[]), "\n\n");
    }

    #[test]
    fn base64_matches_the_reference_bytes() {
        // `-data_dump_format base64` on the 17-byte ramp.
        assert_eq!(base64(&ramp(17)), "\nAAECAwQFBgcICQoLDA0ODxA=\n");
        assert_eq!(base64(b"a"), "\nYQ==\n");
        assert_eq!(base64(b"ab"), "\nYWI=\n");
        assert_eq!(base64(b"abc"), "\nYWJj\n");
    }

    #[test]
    fn base64_wraps_at_eighty_characters() {
        // Measured: the 5 171-byte first video packet of `av.mp4` renders as
        // 87 lines — 86 of exactly 80 characters and a last one of 16.
        let s = base64(&ramp(5171));
        let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 87);
        assert!(
            lines.iter().take(86).all(|l| l.len() == 80),
            "every line but the last is full"
        );
        assert_eq!(lines.last().map(|l| l.len()), Some(16));
        // 5171 bytes -> ceil(5171/3)*4 = 6896 base64 characters, none lost.
        assert_eq!(lines.iter().map(|l| l.len()).sum::<usize>(), 6896);
    }

    #[test]
    fn the_hash_names_are_case_insensitive_but_print_canonically() {
        assert_eq!(HashAlg::parse("md5"), Some(HashAlg::Md5));
        assert_eq!(HashAlg::parse("MD5"), Some(HashAlg::Md5));
        assert_eq!(HashAlg::parse("MuRmUr3"), Some(HashAlg::Murmur3));
        assert_eq!(
            HashAlg::parse("ADLER32").map(HashAlg::name),
            Some("adler32")
        );
        assert_eq!(HashAlg::parse("crc32").map(HashAlg::name), Some("CRC32"));
        // Observed: `sha1` is rejected. The name is SHA160.
        assert_eq!(HashAlg::parse("sha1"), None);
        assert_eq!(HashAlg::parse("nosuch"), None);
        assert_eq!(HashAlg::parse(""), None);
    }

    #[test]
    fn the_measured_digests_are_reproduced() {
        // `ffprobe … -show_data_hash <alg>` on the three bytes 00 01 02.
        let d = [0u8, 1, 2];
        for (alg, want) in [
            (HashAlg::Md5, "MD5:b95f67f61ebb03619622d798f45fc2d3"),
            (
                HashAlg::Sha160,
                "SHA160:0c7a623fd2bbc05b06423be359e4021d36e721ad",
            ),
            (
                HashAlg::Sha224,
                "SHA224:e615202185aabe2aca924bec29e5a12384f8339eae4e64c9cba9f1da",
            ),
            (
                HashAlg::Sha256,
                "SHA256:ae4b3280e56e2faf83f414a6e3dabe9d5fbe18976544c05fed121accb85b53fc",
            ),
            (
                HashAlg::Sha512_224,
                "SHA512/224:00fec611d324972280d5b8d125bd43dd6ea2515ce38c3b888e613a07",
            ),
            (
                HashAlg::Sha512_256,
                "SHA512/256:daca0762a6678e4e26cb8a893d71d72cf3239e29cc837629590b84625dec14af",
            ),
            (HashAlg::Crc32, "CRC32:0854897f"),
            (HashAlg::Adler32, "adler32:00070004"),
        ] {
            assert_eq!(alg.digest(&d).as_deref(), Some(want), "{}", alg.name());
        }
    }

    #[test]
    fn the_five_unimplemented_algorithms_refuse_rather_than_guess() {
        for alg in [
            HashAlg::Murmur3,
            HashAlg::Ripemd128,
            HashAlg::Ripemd160,
            HashAlg::Ripemd256,
            HashAlg::Ripemd320,
        ] {
            assert!(!alg.implemented(), "{}", alg.name());
            assert_eq!(alg.digest(&[0, 1, 2]), None);
        }
        assert_eq!(
            HASH_NAMES.iter().filter(|(_, a)| a.implemented()).count(),
            10
        );
    }

    #[test]
    fn the_dump_format_names_are_the_two_the_reference_lists() {
        assert_eq!(DumpFormat::parse("xxd"), Some(DumpFormat::Xxd));
        assert_eq!(DumpFormat::parse("base64"), Some(DumpFormat::Base64));
        assert_eq!(DumpFormat::parse("hex"), None);
        assert_eq!(DumpFormat::default(), DumpFormat::Xxd);
    }
}
