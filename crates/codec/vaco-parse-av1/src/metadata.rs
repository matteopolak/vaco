//! `metadata_obu()`, AV1 spec §5.8.1–§5.8.7.
//!
//! Five payload shapes, selected by a `leb128()`-coded `metadata_type`. Only
//! `HDR_CLL`, `HDR_MDCV` and `ITUT_T35` carry anything a `CodecParameters`
//! ever surfaces (mastering display / content light level feed `ColorInfo`
//! consumers, `ITUT_T35` is HDR10+/Dolby Vision dynamic metadata); scalability
//! and timecode are parsed structurally, for completeness and so a
//! `filter_units`-style caller can identify them, but nothing downstream of
//! this crate consumes their fields yet.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::leb::leb128;

/// `metadata_type`, §5.8.1.
pub const METADATA_TYPE_HDR_CLL: u64 = 1;
pub const METADATA_TYPE_HDR_MDCV: u64 = 2;
pub const METADATA_TYPE_SCALABILITY: u64 = 3;
pub const METADATA_TYPE_ITUT_T35: u64 = 4;
pub const METADATA_TYPE_TIMECODE: u64 = 5;

/// `metadata_hdr_cll()`, §5.8.3 — CEA-861.3 content light level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrCll {
    pub max_cll: u16,
    pub max_fall: u16,
}

/// `metadata_hdr_mdcv()`, §5.8.4 — SMPTE ST 2086 mastering display colour
/// volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrMdcv {
    /// `primary_chromaticity_x/y[0..3]`, each a 0.16 fixed-point fraction.
    pub primary_chromaticity: [(u16, u16); 3],
    pub white_point_chromaticity: (u16, u16),
    /// 24.8 fixed-point candela/m².
    pub luminance_max: u32,
    /// 18.14 fixed-point candela/m².
    pub luminance_min: u32,
}

/// `metadata_itut_t35()`, §5.8.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItuT35 {
    pub country_code: u8,
    /// Only present when `country_code == 0xFF`.
    pub country_code_extension_byte: Option<u8>,
    /// `itu_t_t35_payload_bytes` — everything left in the OBU after the
    /// country code(s). The specification leaves its length implicit (it runs
    /// to the end of the OBU), so this is a borrow of the caller's payload
    /// rather than a copy.
    pub payload: Vec<u8>,
}

/// A metadata OBU's payload, decoded so far as this crate goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metadata {
    HdrCll(HdrCll),
    HdrMdcv(HdrMdcv),
    ItuT35(ItuT35),
    /// `metadata_scalability()` or `metadata_timecode()`, or a `metadata_type`
    /// the specification does not assign: kept as the raw type and the bytes
    /// that followed it, un-decoded.
    Other {
        metadata_type: u64,
        data: Vec<u8>,
    },
}

/// Parse a `metadata_obu()` payload — the OBU's bytes with `obu_header()` (and
/// size field, if any) already stripped.
///
/// # Errors
///
/// [`Error::InvalidData`] if the `metadata_type` `leb128()` itself is
/// malformed or the payload is too short for the type it declares, or
/// [`Error::LimitExceeded`] from `budget`.
pub fn parse(payload: &[u8], budget: &mut Budget) -> Result<Metadata> {
    budget.check_metadata_bytes(payload.len() as u64)?;
    let mut r = BitReader::new(payload);
    let (metadata_type, type_bytes) = leb128(&mut r);
    if r.overrun() {
        return Err(Error::InvalidData("metadata_obu: malformed metadata_type"));
    }
    let rest = payload.get(type_bytes as usize..).unwrap_or(&[]);

    Ok(match metadata_type {
        METADATA_TYPE_HDR_CLL => {
            let mut r = BitReader::new(rest);
            let max_cll = r.get(16) as u16;
            let max_fall = r.get(16) as u16;
            r.check()
                .map_err(|_| Error::InvalidData("metadata_hdr_cll: truncated"))?;
            Metadata::HdrCll(HdrCll { max_cll, max_fall })
        }
        METADATA_TYPE_HDR_MDCV => {
            let mut r = BitReader::new(rest);
            let mut primary_chromaticity = [(0u16, 0u16); 3];
            for slot in &mut primary_chromaticity {
                *slot = (r.get(16) as u16, r.get(16) as u16);
            }
            let white_point_chromaticity = (r.get(16) as u16, r.get(16) as u16);
            let luminance_max = r.get(32);
            let luminance_min = r.get(32);
            r.check()
                .map_err(|_| Error::InvalidData("metadata_hdr_mdcv: truncated"))?;
            Metadata::HdrMdcv(HdrMdcv {
                primary_chromaticity,
                white_point_chromaticity,
                luminance_max,
                luminance_min,
            })
        }
        METADATA_TYPE_ITUT_T35 => {
            let Some(&country_code) = rest.first() else {
                return Err(Error::InvalidData("metadata_itut_t35: truncated"));
            };
            let (country_code_extension_byte, payload_start) = if country_code == 0xFF {
                let Some(&ext) = rest.get(1) else {
                    return Err(Error::InvalidData(
                        "metadata_itut_t35: truncated extension byte",
                    ));
                };
                (Some(ext), 2)
            } else {
                (None, 1)
            };
            let payload_bytes = rest.get(payload_start..).unwrap_or(&[]);
            budget.charge(payload_bytes.len() as u64)?;
            Metadata::ItuT35(ItuT35 {
                country_code,
                country_code_extension_byte,
                payload: payload_bytes.to_vec(),
            })
        }
        _ => {
            budget.charge(rest.len() as u64)?;
            Metadata::Other {
                metadata_type,
                data: rest.to_vec(),
            }
        }
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    #[test]
    fn hdr_cll_round_trips_its_two_fields() {
        // type=1, max_cll=1000, max_fall=400
        let mut data = vec![1u8];
        data.extend_from_slice(&1000u16.to_be_bytes());
        data.extend_from_slice(&400u16.to_be_bytes());
        let m = parse(&data, &mut budget()).expect("parses");
        assert_eq!(
            m,
            Metadata::HdrCll(HdrCll {
                max_cll: 1000,
                max_fall: 400
            })
        );
    }

    #[test]
    fn itut_t35_carries_the_country_code_and_the_rest_as_payload() {
        let mut data = vec![4u8, 0xB5]; // type=4 (ITUT_T35), country_code=0xB5 (US)
        data.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let m = parse(&data, &mut budget()).expect("parses");
        match m {
            Metadata::ItuT35(t) => {
                assert_eq!(t.country_code, 0xB5);
                assert_eq!(t.country_code_extension_byte, None);
                assert_eq!(t.payload, vec![0xde, 0xad, 0xbe, 0xef]);
            }
            other => panic!("expected ItuT35, got {other:?}"),
        }
    }

    #[test]
    fn itut_t35_extension_byte_is_read_for_0xff() {
        let data = vec![4u8, 0xFF, 0x26, 1, 2, 3];
        let m = parse(&data, &mut budget()).expect("parses");
        match m {
            Metadata::ItuT35(t) => {
                assert_eq!(t.country_code, 0xFF);
                assert_eq!(t.country_code_extension_byte, Some(0x26));
                assert_eq!(t.payload, vec![1, 2, 3]);
            }
            other => panic!("expected ItuT35, got {other:?}"),
        }
    }

    #[test]
    fn an_unrecognised_type_is_kept_as_raw_bytes() {
        let data = vec![9u8, 1, 2, 3];
        let m = parse(&data, &mut budget()).expect("parses");
        assert_eq!(
            m,
            Metadata::Other {
                metadata_type: 9,
                data: vec![1, 2, 3]
            }
        );
    }

    #[test]
    fn truncation_never_panics() {
        let mut data = vec![2u8]; // HDR_MDCV, which needs 24 bytes
        data.extend_from_slice(&[0u8; 20]);
        for n in 0..=data.len() {
            let _ = parse(&data[..n], &mut budget());
        }
    }
}
