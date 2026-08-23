//! AMF0 (Action Message Format, version 0), enough to read and write the
//! `onMetaData` script tag.
//!
//! Adobe's *Action Message Format — AMF 0* specification. This is a small,
//! recursive tag-length(-ish)-value encoding: each value starts with a
//! one-byte type marker.
//!
//! # Why this crate owns it rather than treating it as a black box
//!
//! `onMetaData`'s `duration`/`width`/`height`/`videocodecid` fields, and the
//! `keyframes` seek-index array some encoders add, are only reachable by
//! actually decoding AMF0 — there is no shortcut. `vaco-mux-flv` reuses
//! [`AmfValue::encode`] to write the same tag back out (D19: one encoder and
//! one decoder for one wire format, not two).
//!
//! # What is decoded
//!
//! Number, Boolean, String, Object, Null, Undefined, ECMA Array, Strict
//! Array, Long String and Date — everything `ffmpeg 8.1`'s own `onMetaData`
//! writer uses. Reference (0x07), XML Document (0x0F), Typed Object (0x10) and
//! AMF3-in-AMF0 switch (0x11) decode as [`Unsupported`](AmfValue::Unsupported)
//! rather than erroring the whole tag — one field this crate cannot interpret
//! should not cost the rest of `onMetaData`.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Recursion depth `decode` will follow into nested objects/arrays.
///
/// AMF0 is self-describing and untrusted; without a cap, a value nesting
/// object-in-object thousands of times deep would blow the call stack before
/// any input-size limit could stop it — a Rust stack overflow is an abort, not
/// a catchable error, so this has to be checked before recursing rather than
/// after.
const MAX_DEPTH: u32 = 32;

/// Object/array/string entries decoded in one call to [`decode`], summed
/// across the whole nested value. Bounds the total work an attacker can cause
/// with a value that individually looks small (each entry still costs real
/// input bytes, but a 64 KiB script tag of one-byte keys is a cheap way to
/// generate tens of thousands of tiny allocations).
const MAX_ITEMS: usize = 1 << 16;

/// A decoded AMF0 value.
#[derive(Debug, Clone, PartialEq)]
pub enum AmfValue {
    Number(f64),
    Boolean(bool),
    String(String),
    /// Object or ECMA array — both are `(key, value)` pairs in this crate;
    /// [`AmfValue::EcmaArray`] is kept as a separate variant only because
    /// `ffmpeg`'s own reader distinguishes them when re-encoding, not because
    /// this crate treats them differently while reading.
    Object(Vec<(String, AmfValue)>),
    EcmaArray(Vec<(String, AmfValue)>),
    StrictArray(Vec<AmfValue>),
    Null,
    Undefined,
    /// Seconds since the Unix epoch times 1000, per the AMF0 `Date` type —
    /// the timezone field the spec also carries is legacy and unused by every
    /// writer this crate has seen, so it is not preserved.
    Date(f64),
    /// A marker this crate recognised but does not interpret (Reference, XML
    /// Document, Typed Object, the AMF3-switch marker). Carries no data,
    /// because none of it is used by anything this crate does.
    Unsupported,
}

impl AmfValue {
    /// A `&str` view, for the common case of reading a metadata key's value.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// An `f64` view — every AMF0 number is a double, including ones that
    /// hold small integers like `width`/`height`.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The `(key, value)` pairs of an `Object` or `EcmaArray`. `None` for
    /// every other variant, including `StrictArray` (which has no keys).
    #[must_use]
    pub fn as_pairs(&self) -> Option<&[(String, AmfValue)]> {
        match self {
            Self::Object(p) | Self::EcmaArray(p) => Some(p),
            _ => None,
        }
    }

    /// Look up a key in an `Object`/`EcmaArray` value, case-sensitively —
    /// AMF0 keys are ordinary UTF-8 strings with no folding convention.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&AmfValue> {
        self.as_pairs()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

// Marker bytes.
const NUMBER: u8 = 0x00;
const BOOLEAN: u8 = 0x01;
const STRING: u8 = 0x02;
const OBJECT: u8 = 0x03;
const NULL: u8 = 0x05;
const UNDEFINED: u8 = 0x06;
const REFERENCE: u8 = 0x07;
const ECMA_ARRAY: u8 = 0x08;
const OBJECT_END: u8 = 0x09;
const STRICT_ARRAY: u8 = 0x0A;
const DATE: u8 = 0x0B;
const LONG_STRING: u8 = 0x0C;
const XML_DOCUMENT: u8 = 0x0F;
const TYPED_OBJECT: u8 = 0x10;
const AVMPLUS: u8 = 0x11;

/// Decode one AMF0 value from the start of `data`.
///
/// # Errors
///
/// [`Error::InvalidData`] on a truncated value or an unrecognised marker
/// byte; [`Error::LimitExceeded`] past [`MAX_DEPTH`] or [`MAX_ITEMS`].
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<(AmfValue, usize)> {
    let mut items = 0usize;
    decode_inner(data, budget, 0, &mut items)
}

fn decode_inner(
    data: &[u8],
    budget: &mut Budget,
    depth: u32,
    items: &mut usize,
) -> Result<(AmfValue, usize)> {
    if depth > MAX_DEPTH {
        return Err(Error::LimitExceeded {
            limit: "amf0_depth",
            requested: u64::from(depth),
            cap: u64::from(MAX_DEPTH),
        });
    }
    *items += 1;
    if *items > MAX_ITEMS {
        return Err(Error::LimitExceeded {
            limit: "amf0_items",
            requested: *items as u64,
            cap: MAX_ITEMS as u64,
        });
    }
    let &marker = data.first().ok_or(Error::UnexpectedEof)?;
    let rest = data.get(1..).unwrap_or(&[]);
    match marker {
        NUMBER => {
            let bytes = rest.get(..8).ok_or(Error::UnexpectedEof)?;
            let n = f64::from_be_bytes(bytes.try_into().unwrap_or([0; 8]));
            Ok((AmfValue::Number(n), 9))
        }
        BOOLEAN => {
            let &b = rest.first().ok_or(Error::UnexpectedEof)?;
            Ok((AmfValue::Boolean(b != 0), 2))
        }
        STRING => {
            let (s, n) = decode_short_string(rest, budget)?;
            Ok((AmfValue::String(s), 1 + n))
        }
        LONG_STRING | XML_DOCUMENT => {
            let (s, n) = decode_long_string(rest, budget)?;
            Ok((AmfValue::String(s), 1 + n))
        }
        NULL => Ok((AmfValue::Null, 1)),
        UNDEFINED => Ok((AmfValue::Undefined, 1)),
        REFERENCE => {
            rest.get(..2).ok_or(Error::UnexpectedEof)?;
            Ok((AmfValue::Unsupported, 3))
        }
        DATE => {
            let bytes = rest.get(..8).ok_or(Error::UnexpectedEof)?;
            let n = f64::from_be_bytes(bytes.try_into().unwrap_or([0; 8]));
            rest.get(8..10).ok_or(Error::UnexpectedEof)?;
            Ok((AmfValue::Date(n), 11))
        }
        OBJECT => {
            let (pairs, n) = decode_pairs(rest, budget, depth, items)?;
            Ok((AmfValue::Object(pairs), 1 + n))
        }
        ECMA_ARRAY => {
            let _declared_count = rest.get(..4).ok_or(Error::UnexpectedEof)?;
            let (pairs, n) = decode_pairs(rest.get(4..).unwrap_or(&[]), budget, depth, items)?;
            Ok((AmfValue::EcmaArray(pairs), 5 + n))
        }
        STRICT_ARRAY => {
            let count_bytes = rest.get(..4).ok_or(Error::UnexpectedEof)?;
            let count = u32::from_be_bytes(count_bytes.try_into().unwrap_or([0; 4]));
            let mut values = Vec::new();
            let mut pos = 4usize;
            for _ in 0..count.min(u32::try_from(MAX_ITEMS).unwrap_or(u32::MAX)) {
                let slice = rest.get(pos..).ok_or(Error::UnexpectedEof)?;
                let (v, n) = decode_inner(slice, budget, depth.saturating_add(1), items)?;
                values.push(v);
                pos = pos.saturating_add(n);
            }
            Ok((AmfValue::StrictArray(values), 1 + pos))
        }
        TYPED_OBJECT => {
            let (_class_name, class_len) = decode_short_string(rest, budget)?;
            let (pairs, n) =
                decode_pairs(rest.get(class_len..).unwrap_or(&[]), budget, depth, items)?;
            Ok((AmfValue::Object(pairs), 1 + class_len + n))
        }
        AVMPLUS => Ok((AmfValue::Unsupported, 1)),
        _ => Err(Error::InvalidData("flv: unrecognised AMF0 marker")),
    }
}

/// Decode `(key, value)` pairs up to the `OBJECT_END` terminator
/// (`0x00 0x00 0x09`), used by both `Object` and `ECMA Array`.
fn decode_pairs(
    data: &[u8],
    budget: &mut Budget,
    depth: u32,
    items: &mut usize,
) -> Result<(Vec<(String, AmfValue)>, usize)> {
    let mut pairs = Vec::new();
    let mut pos = 0usize;
    loop {
        // The terminator is an empty key (u16 length 0) followed by the
        // object-end marker — check for it before trying to decode a key,
        // since an empty string is otherwise a perfectly ordinary key.
        if data.get(pos..pos.saturating_add(3)) == Some(&[0, 0, OBJECT_END]) {
            pos = pos.saturating_add(3);
            break;
        }
        let (key, key_len) = decode_short_string(data.get(pos..).unwrap_or(&[]), budget)?;
        pos = pos.saturating_add(key_len);
        let (value, value_len) = decode_inner(
            data.get(pos..).unwrap_or(&[]),
            budget,
            depth.saturating_add(1),
            items,
        )?;
        pos = pos.saturating_add(value_len);
        if pairs.len() >= MAX_ITEMS {
            return Err(Error::LimitExceeded {
                limit: "amf0_items",
                requested: pairs.len() as u64,
                cap: MAX_ITEMS as u64,
            });
        }
        pairs.push((key, value));
    }
    Ok((pairs, pos))
}

/// A `u16`-length-prefixed UTF-8 string — the key encoding, and the ordinary
/// `String` value encoding.
fn decode_short_string(data: &[u8], budget: &mut Budget) -> Result<(String, usize)> {
    let len_bytes = data.get(..2).ok_or(Error::UnexpectedEof)?;
    let len = usize::from(u16::from_be_bytes(len_bytes.try_into().unwrap_or([0; 2])));
    decode_string_body(data.get(2..).unwrap_or(&[]), len, budget).map(|s| (s, 2 + len))
}

/// A `u32`-length-prefixed UTF-8 string (`LongString`/`XMLDocument`).
fn decode_long_string(data: &[u8], budget: &mut Budget) -> Result<(String, usize)> {
    let len_bytes = data.get(..4).ok_or(Error::UnexpectedEof)?;
    let len = usize::try_from(u32::from_be_bytes(len_bytes.try_into().unwrap_or([0; 4])))
        .unwrap_or(usize::MAX);
    decode_string_body(data.get(4..).unwrap_or(&[]), len, budget).map(|s| (s, 4 + len))
}

fn decode_string_body(data: &[u8], len: usize, budget: &mut Budget) -> Result<String> {
    let bytes = data.get(..len).ok_or(Error::UnexpectedEof)?;
    let mut buf = budget.alloc::<u8>(bytes.len())?;
    buf.copy_from_slice(bytes);
    // A metadata string that is not valid UTF-8 is replaced rather than
    // rejected, the same tolerance `vaco-io`'s own string readers use for
    // container text fields — losing one field's exact bytes is better than
    // losing all of `onMetaData` over it.
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

impl AmfValue {
    /// Encode this value, appending to `out`.
    ///
    /// No error path: every [`AmfValue`] this crate can construct is encodable
    /// (a `String`/key longer than `u32::MAX` bytes is not reachable from any
    /// realistic caller, and is truncated rather than mis-encoded if it ever
    /// is).
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Number(n) => {
                out.push(NUMBER);
                out.extend_from_slice(&n.to_be_bytes());
            }
            Self::Boolean(b) => {
                out.push(BOOLEAN);
                out.push(u8::from(*b));
            }
            Self::String(s) => encode_string(out, s),
            Self::Object(pairs) => {
                out.push(OBJECT);
                encode_pairs(out, pairs);
            }
            Self::EcmaArray(pairs) => {
                out.push(ECMA_ARRAY);
                let n = u32::try_from(pairs.len()).unwrap_or(u32::MAX);
                out.extend_from_slice(&n.to_be_bytes());
                encode_pairs(out, pairs);
            }
            Self::StrictArray(values) => {
                out.push(STRICT_ARRAY);
                let n = u32::try_from(values.len()).unwrap_or(u32::MAX);
                out.extend_from_slice(&n.to_be_bytes());
                for v in values {
                    v.encode(out);
                }
            }
            Self::Null => out.push(NULL),
            Self::Undefined | Self::Unsupported => out.push(UNDEFINED),
            Self::Date(n) => {
                out.push(DATE);
                out.extend_from_slice(&n.to_be_bytes());
                out.extend_from_slice(&[0, 0]);
            }
        }
    }
}

fn encode_key(out: &mut Vec<u8>, key: &str) {
    let bytes = key.as_bytes();
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes.get(..usize::from(len)).unwrap_or(bytes));
}

fn encode_string(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    if let Ok(len) = u16::try_from(bytes.len()) {
        out.push(STRING);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
    } else {
        out.push(LONG_STRING);
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes.get(..len as usize).unwrap_or(bytes));
    }
}

fn encode_pairs(out: &mut Vec<u8>, pairs: &[(String, AmfValue)]) {
    for (k, v) in pairs {
        encode_key(out, k);
        v.encode(out);
    }
    out.extend_from_slice(&[0, 0, OBJECT_END]);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, reason = "test code")]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget::new(vaco_limits::Limits::permissive())
    }

    #[test]
    fn number_round_trips() {
        let mut out = Vec::new();
        AmfValue::Number(3.5).encode(&mut out);
        let (v, n) = decode(&out, &mut budget()).unwrap();
        assert_eq!(v, AmfValue::Number(3.5));
        assert_eq!(n, out.len());
    }

    #[test]
    fn string_round_trips() {
        let mut out = Vec::new();
        AmfValue::String("onMetaData".to_owned()).encode(&mut out);
        let (v, _) = decode(&out, &mut budget()).unwrap();
        assert_eq!(v.as_str(), Some("onMetaData"));
    }

    #[test]
    fn ecma_array_round_trips_and_is_queryable() {
        let pairs = vec![
            ("duration".to_owned(), AmfValue::Number(1.5)),
            ("width".to_owned(), AmfValue::Number(64.0)),
        ];
        let mut out = Vec::new();
        AmfValue::EcmaArray(pairs.clone()).encode(&mut out);
        let (v, n) = decode(&out, &mut budget()).unwrap();
        assert_eq!(n, out.len());
        assert_eq!(v.get("duration").and_then(AmfValue::as_f64), Some(1.5));
        assert_eq!(v.get("width").and_then(AmfValue::as_f64), Some(64.0));
        assert_eq!(v.get("nonesuch"), None);
    }

    #[test]
    fn the_measured_onmetadata_prefix_decodes() {
        // `ffmpeg 8.1`'s FLV muxer, byte for byte: String "onMetaData"
        // followed by an ECMA array whose first entry is "duration".
        let bytes = [
            0x02, 0x00, 0x0a, b'o', b'n', b'M', b'e', b't', b'a', b'D', b'a', b't', b'a', 0x08,
            0x00, 0x00, 0x00, 0x0d, 0x00, 0x08, b'd', b'u', b'r', b'a', b't', b'i', b'o', b'n',
            0x00, 0x3f, 0xf3, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x00, 0x00, 0x09,
        ];
        let (name, n1) = decode(&bytes, &mut budget()).unwrap();
        assert_eq!(name.as_str(), Some("onMetaData"));
        let (meta, _) = decode(bytes.get(n1..).unwrap_or(&[]), &mut budget()).unwrap();
        let duration = meta.get("duration").and_then(AmfValue::as_f64).unwrap();
        assert!((duration - 1.2).abs() < 1e-9);
    }

    #[test]
    fn strict_array_has_no_keys() {
        let mut out = Vec::new();
        AmfValue::StrictArray(vec![AmfValue::Number(1.0), AmfValue::Boolean(true)])
            .encode(&mut out);
        let (v, n) = decode(&out, &mut budget()).unwrap();
        assert_eq!(n, out.len());
        assert!(v.as_pairs().is_none());
        assert_eq!(
            v,
            AmfValue::StrictArray(vec![AmfValue::Number(1.0), AmfValue::Boolean(true)])
        );
    }

    #[test]
    fn deeply_nested_objects_are_rejected_rather_than_overflowing_the_stack() {
        let bytes = vec![OBJECT; (MAX_DEPTH + 10) as usize];
        // No terminator is ever reached; the depth check must fire first.
        assert!(decode(&bytes, &mut budget()).is_err());
    }

    #[test]
    fn a_truncated_value_is_rejected_not_panicking() {
        assert!(decode(&[NUMBER, 1, 2, 3], &mut budget()).is_err());
        assert!(decode(&[STRING, 0, 5, b'h', b'i'], &mut budget()).is_err());
        assert!(decode(&[], &mut budget()).is_err());
    }

    #[test]
    fn an_unrecognised_marker_is_rejected() {
        assert!(decode(&[0xEE], &mut budget()).is_err());
    }
}
