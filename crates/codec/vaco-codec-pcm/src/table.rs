//! The one table this whole crate is built around.
//!
//! Every `CodecId::Pcm*` variant maps to exactly one [`PcmFormat`] row:
//! how many bytes one sample takes on the wire, how those bytes are laid
//! out ([`WireKind`]), and which in-memory [`SampleFmt`] the decoded frame
//! carries. [`codec::decode`]/[`codec::encode`] are the only two functions
//! that read this table — nothing here has a matching `if`/`match` arm for
//! each codec, because the whole point (plan 15 §4.9) is that there is
//! exactly one conversion routine, parameterised by data.
//!
//! `Vaco-Spec-Ref: itu-t-h262` is not relevant here; there is no registered
//! `provenance/sources.toml` entry for ITU-T G.711 today, so the A-law/mu-law
//! rows cite nothing (`Vaco-Spec-Ref` omitted is the documented "genuinely
//! nothing to cite" case) — the companding formulas below are the standard
//! public G.711 piecewise-linear approximation, derived from first
//! principles rather than transcribed from any implementation.

use vaco_codec_core::CodecId;
use vaco_sampfmt::SampleFmt;

/// How the bytes of one sample are laid out on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireKind {
    /// Two's-complement signed integer.
    SignedInt { big_endian: bool },
    /// Offset-binary unsigned integer (128/32768/... is the zero level).
    UnsignedInt { big_endian: bool },
    /// IEEE 754 binary32/binary64.
    Float { big_endian: bool },
    /// ITU-T G.711 A-law, one byte per sample.
    ALaw,
    /// ITU-T G.711 mu-law, one byte per sample.
    MuLaw,
    /// Acorn/Archimedes VIDC logarithmic PCM, one byte per sample.
    ///
    /// No primary specification is registered in `provenance/sources.toml`
    /// for this format (it is Acorn hardware documentation that was not
    /// locatable). The companding below follows the same sign/exponent/
    /// mantissa shape as A-law, which is a generic, unoriginal numerical
    /// pattern for logarithmic PCM rather than something transcribed from
    /// any implementation — but unlike A-law/mu-law it has not been checked
    /// against any reference sample, so decode is best-effort and flagged as
    /// such in this crate's closing report.
    Vidc,
}

/// One row of the PCM identity table.
#[derive(Debug, Clone, Copy)]
pub struct PcmFormat {
    pub id: CodecId,
    /// Bytes per sample **as stored on the wire**. Distinct from
    /// `decoded.bytes_per_sample()` — `s24le` stores 3 bytes and decodes into
    /// a 4-byte `S32` sample.
    pub container_bytes: u8,
    pub wire: WireKind,
    /// The in-memory format a decoded [`vaco_frame::Frame`] carries.
    pub decoded: SampleFmt,
    /// Whether this crate registers an encoder for this id. `false` only for
    /// [`WireKind::Vidc`] (see its doc) — every format with an attested
    /// companding rule round-trips both ways.
    pub encodable: bool,
}

const fn row(
    id: CodecId,
    container_bytes: u8,
    wire: WireKind,
    decoded: SampleFmt,
    encodable: bool,
) -> PcmFormat {
    PcmFormat {
        id,
        container_bytes,
        wire,
        decoded,
        encodable,
    }
}

/// All 21 `CodecId::Pcm*` variants this build's `vaco-codec-core` declares.
///
/// Matches `vaco-demux-raw::pcm::PCM_FORMATS` one-for-one (same 21 names,
/// same `decoded` mapping) — that crate demuxes these exact 21 raw families
/// and this crate is what actually decodes/encodes the samples once framed.
pub const PCM_FORMATS: &[PcmFormat] = &[
    row(CodecId::PcmAlaw, 1, WireKind::ALaw, SampleFmt::S16, true),
    row(CodecId::PcmMulaw, 1, WireKind::MuLaw, SampleFmt::S16, true),
    row(
        CodecId::PcmS8,
        1,
        WireKind::SignedInt { big_endian: false },
        SampleFmt::U8,
        true,
    ),
    row(
        CodecId::PcmU8,
        1,
        WireKind::UnsignedInt { big_endian: false },
        SampleFmt::U8,
        true,
    ),
    row(
        CodecId::PcmS16le,
        2,
        WireKind::SignedInt { big_endian: false },
        SampleFmt::S16,
        true,
    ),
    row(
        CodecId::PcmS16be,
        2,
        WireKind::SignedInt { big_endian: true },
        SampleFmt::S16,
        true,
    ),
    row(
        CodecId::PcmU16le,
        2,
        WireKind::UnsignedInt { big_endian: false },
        SampleFmt::S16,
        true,
    ),
    row(
        CodecId::PcmU16be,
        2,
        WireKind::UnsignedInt { big_endian: true },
        SampleFmt::S16,
        true,
    ),
    row(
        CodecId::PcmS24le,
        3,
        WireKind::SignedInt { big_endian: false },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmS24be,
        3,
        WireKind::SignedInt { big_endian: true },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmU24le,
        3,
        WireKind::UnsignedInt { big_endian: false },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmU24be,
        3,
        WireKind::UnsignedInt { big_endian: true },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmS32le,
        4,
        WireKind::SignedInt { big_endian: false },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmS32be,
        4,
        WireKind::SignedInt { big_endian: true },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmU32le,
        4,
        WireKind::UnsignedInt { big_endian: false },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmU32be,
        4,
        WireKind::UnsignedInt { big_endian: true },
        SampleFmt::S32,
        true,
    ),
    row(
        CodecId::PcmF32le,
        4,
        WireKind::Float { big_endian: false },
        SampleFmt::F32,
        true,
    ),
    row(
        CodecId::PcmF32be,
        4,
        WireKind::Float { big_endian: true },
        SampleFmt::F32,
        true,
    ),
    row(
        CodecId::PcmF64le,
        8,
        WireKind::Float { big_endian: false },
        SampleFmt::F64,
        true,
    ),
    row(
        CodecId::PcmF64be,
        8,
        WireKind::Float { big_endian: true },
        SampleFmt::F64,
        true,
    ),
    row(CodecId::PcmVidc, 1, WireKind::Vidc, SampleFmt::S16, false),
];

/// Look up a format row by codec identity.
#[must_use]
pub fn format_for(id: CodecId) -> Option<&'static PcmFormat> {
    PCM_FORMATS.iter().find(|f| f.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_exactly_the_21_declared_variants() {
        assert_eq!(PCM_FORMATS.len(), 21);
    }

    #[test]
    fn every_row_resolves_by_its_own_id() {
        for f in PCM_FORMATS {
            assert_eq!(format_for(f.id).map(|r| r.id), Some(f.id));
        }
    }

    #[test]
    fn twenty_of_twenty_one_are_encodable() {
        assert_eq!(PCM_FORMATS.iter().filter(|f| f.encodable).count(), 20);
    }
}
