//! AMF0 (Adobe "Action Message Format 0") — the marker-byte-tagged value
//! encoding NetConnection/NetStream command messages are serialised in
//! (`adobe-amf0-spec`, distinct from `adobe-rtmp-spec-1.0` which covers
//! only the handshake/chunk-stream layer built in #552).
//!
//! **Scope, stated up front.** Built: Number (0x00), Boolean (0x01),
//! String/Long String (0x02/0x0C — one Rust type, encode picks the marker
//! by length), Object (0x03), Null (0x05), Undefined (0x06), ECMA Array
//! (0x08), Strict Array (0x0A), Date (0x0B). **Not built:** `MovieClip`
//! (0x04, the spec itself calls this reserved/unused), Reference (0x07,
//! needs an object-identity table nothing here has a use for yet),
//! `RecordSet` (0x0E, deprecated in the spec itself), XML Document (0x0F),
//! Typed Object (0x10), and the AVMPlus/AMF3 switch marker (0x11) — RTMP
//! command messages in practice are AMF0 throughout; nothing in this
//! crate's own NetConnection/NetStream flow needs any of the six missing
//! types.

use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "rtmp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// One AMF0 value. `Object`/`EcmaArray` are `Vec<(String, Value)>` rather
/// than a `HashMap` to preserve wire order — RTMP command objects have a
/// conventional key order (`app`, `flashVer`, `tcUrl`, ...) worth keeping
/// on a round trip, and nothing here needs keyed lookup.
/// `PartialEq` is IEEE-754-correct on `Number`'s `f64`, so a NaN `Number`
/// is never equal to itself — found the hard way by `fuzz/fuzz_targets/
/// rtmp_command.rs`'s first real run, which decoded an arbitrary 8-byte
/// AMF0 Number into a NaN bit pattern and then asserted decoded-value
/// equality across a round trip. The byte-level round trip was correct;
/// the assertion was not NaN-aware. Compare encoded bytes, not `Value`s,
/// when NaN is reachable (any attacker-controlled `Number`).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Boolean(bool),
    String(String),
    Object(Vec<(String, Value)>),
    Null,
    Undefined,
    EcmaArray(Vec<(String, Value)>),
    StrictArray(Vec<Value>),
    /// Milliseconds since the Unix epoch. The spec's own 2-byte timezone
    /// field is always written as 0 (its own documented convention: "This
    /// value is reserved and should always be set to 0.").
    Date(f64),
}

const MARKER_NUMBER: u8 = 0x00;
const MARKER_BOOLEAN: u8 = 0x01;
const MARKER_STRING: u8 = 0x02;
const MARKER_OBJECT: u8 = 0x03;
const MARKER_NULL: u8 = 0x05;
const MARKER_UNDEFINED: u8 = 0x06;
const MARKER_ECMA_ARRAY: u8 = 0x08;
const OBJECT_END: u8 = 0x09;
const MARKER_STRICT_ARRAY: u8 = 0x0A;
const MARKER_DATE: u8 = 0x0B;
const MARKER_LONG_STRING: u8 = 0x0C;

/// Encode one value, appending to `out`.
pub fn encode(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Number(n) => {
            out.push(MARKER_NUMBER);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Value::Boolean(b) => {
            out.push(MARKER_BOOLEAN);
            out.push(u8::from(*b));
        }
        Value::String(s) => encode_string(s, out),
        Value::Object(pairs) => {
            out.push(MARKER_OBJECT);
            encode_pairs(pairs, out);
        }
        Value::Null => out.push(MARKER_NULL),
        Value::Undefined => out.push(MARKER_UNDEFINED),
        Value::EcmaArray(pairs) => {
            out.push(MARKER_ECMA_ARRAY);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "AMF0's own length field is u32; a real command object never approaches u32::MAX pairs"
            )]
            out.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
            encode_pairs(pairs, out);
        }
        Value::StrictArray(items) => {
            out.push(MARKER_STRICT_ARRAY);
            #[allow(clippy::cast_possible_truncation, reason = "see EcmaArray above")]
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            for item in items {
                encode(item, out);
            }
        }
        Value::Date(millis) => {
            out.push(MARKER_DATE);
            out.extend_from_slice(&millis.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
        }
    }
}

fn encode_string(s: &str, out: &mut Vec<u8>) {
    let bytes = s.as_bytes();
    if let Ok(len) = u16::try_from(bytes.len()) {
        out.push(MARKER_STRING);
        out.extend_from_slice(&len.to_be_bytes());
    } else {
        out.push(MARKER_LONG_STRING);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the try_from(u16) above already failed, so len > u16::MAX; u32 is AMF0's own long-string length field width"
        )]
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    }
    out.extend_from_slice(bytes);
}

/// Bare key/value pairs (no leading marker, no length prefix) terminated
/// by AMF0's own end marker: a zero-length key followed by `0x09`.
fn encode_pairs(pairs: &[(String, Value)], out: &mut Vec<u8>) {
    for (key, value) in pairs {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an object key is never anywhere near u16::MAX bytes in practice"
        )]
        out.extend_from_slice(&(key.len() as u16).to_be_bytes());
        out.extend_from_slice(key.as_bytes());
        encode(value, out);
    }
    out.extend_from_slice(&0u16.to_be_bytes());
    out.push(OBJECT_END);
}

/// Decode one value from the front of `buf`, returning it and how many
/// bytes were consumed.
///
/// # Errors
/// [`ProtocolError::Malformed`] on truncated input, an unrecognised or
/// deliberately-unsupported marker byte, or a string whose declared
/// length is not valid UTF-8.
pub fn decode(buf: &[u8]) -> Result<(Value, usize)> {
    let marker = *buf
        .first()
        .ok_or_else(|| malformed("AMF0 value is empty"))?;
    let rest = buf.get(1..).unwrap_or(&[]);
    match marker {
        MARKER_NUMBER => {
            let bytes: [u8; 8] = rest
                .get(..8)
                .ok_or_else(|| malformed("AMF0 number is truncated"))?
                .try_into()
                .unwrap_or([0; 8]);
            Ok((Value::Number(f64::from_be_bytes(bytes)), 9))
        }
        MARKER_BOOLEAN => {
            let b = *rest
                .first()
                .ok_or_else(|| malformed("AMF0 boolean is truncated"))?;
            Ok((Value::Boolean(b != 0), 2))
        }
        MARKER_STRING => {
            let (s, consumed) = decode_short_string(rest)?;
            Ok((Value::String(s), 1 + consumed))
        }
        MARKER_LONG_STRING => {
            let (s, consumed) = decode_long_string(rest)?;
            Ok((Value::String(s), 1 + consumed))
        }
        MARKER_OBJECT => {
            let (pairs, consumed) = decode_pairs(rest)?;
            Ok((Value::Object(pairs), 1 + consumed))
        }
        MARKER_NULL => Ok((Value::Null, 1)),
        MARKER_UNDEFINED => Ok((Value::Undefined, 1)),
        MARKER_ECMA_ARRAY => {
            let _count = rest
                .get(..4)
                .ok_or_else(|| malformed("AMF0 ECMA array count is truncated"))?;
            let (pairs, consumed) = decode_pairs(
                rest.get(4..)
                    .ok_or_else(|| malformed("AMF0 ECMA array is truncated"))?,
            )?;
            Ok((Value::EcmaArray(pairs), 1 + 4 + consumed))
        }
        MARKER_STRICT_ARRAY => {
            let count_bytes: [u8; 4] = rest
                .get(..4)
                .ok_or_else(|| malformed("AMF0 strict array count is truncated"))?
                .try_into()
                .unwrap_or([0; 4]);
            let count = u32::from_be_bytes(count_bytes);
            let mut items: Vec<Value> = Vec::new();
            let mut cursor = 4usize;
            for _ in 0..count {
                let (value, consumed) = decode(
                    rest.get(cursor..)
                        .ok_or_else(|| malformed("AMF0 strict array is truncated"))?,
                )?;
                items.push(value);
                cursor += consumed;
            }
            Ok((Value::StrictArray(items), 1 + cursor))
        }
        MARKER_DATE => {
            let bytes: [u8; 8] = rest
                .get(..8)
                .ok_or_else(|| malformed("AMF0 date is truncated"))?
                .try_into()
                .unwrap_or([0; 8]);
            Ok((Value::Date(f64::from_be_bytes(bytes)), 1 + 8 + 2))
        }
        _ => Err(malformed(
            "AMF0 marker byte is unrecognised or unsupported by this crate",
        )),
    }
}

fn decode_short_string(buf: &[u8]) -> Result<(String, usize)> {
    let len_bytes: [u8; 2] = buf
        .get(..2)
        .ok_or_else(|| malformed("AMF0 string length is truncated"))?
        .try_into()
        .unwrap_or([0; 2]);
    let len = usize::from(u16::from_be_bytes(len_bytes));
    let bytes = buf
        .get(2..2 + len)
        .ok_or_else(|| malformed("AMF0 string body is truncated"))?;
    let s = String::from_utf8(bytes.to_vec())
        .map_err(|_| malformed("AMF0 string is not valid UTF-8"))?;
    Ok((s, 2 + len))
}

fn decode_long_string(buf: &[u8]) -> Result<(String, usize)> {
    let len_bytes: [u8; 4] = buf
        .get(..4)
        .ok_or_else(|| malformed("AMF0 long string length is truncated"))?
        .try_into()
        .unwrap_or([0; 4]);
    let len = u32::from_be_bytes(len_bytes) as usize;
    let bytes = buf
        .get(4..4 + len)
        .ok_or_else(|| malformed("AMF0 long string body is truncated"))?;
    let s = String::from_utf8(bytes.to_vec())
        .map_err(|_| malformed("AMF0 long string is not valid UTF-8"))?;
    Ok((s, 4 + len))
}

fn decode_pairs(buf: &[u8]) -> Result<(Vec<(String, Value)>, usize)> {
    let mut pairs = Vec::new();
    let mut cursor = 0usize;
    loop {
        let key_len_bytes: [u8; 2] = buf
            .get(cursor..cursor + 2)
            .ok_or_else(|| malformed("AMF0 object key length is truncated"))?
            .try_into()
            .unwrap_or([0; 2]);
        let key_len = usize::from(u16::from_be_bytes(key_len_bytes));
        if key_len == 0 {
            let end_marker = *buf
                .get(cursor + 2)
                .ok_or_else(|| malformed("AMF0 object end marker is truncated"))?;
            if end_marker != OBJECT_END {
                return Err(malformed(
                    "AMF0 object has a zero-length key that is not the end marker",
                ));
            }
            cursor += 3;
            break;
        }
        let key_bytes = buf
            .get(cursor + 2..cursor + 2 + key_len)
            .ok_or_else(|| malformed("AMF0 object key body is truncated"))?;
        let key = String::from_utf8(key_bytes.to_vec())
            .map_err(|_| malformed("AMF0 object key is not valid UTF-8"))?;
        cursor += 2 + key_len;
        let (value, consumed) = decode(
            buf.get(cursor..)
                .ok_or_else(|| malformed("AMF0 object value is truncated"))?,
        )?;
        pairs.push((key, value));
        cursor += consumed;
    }
    Ok((pairs, cursor))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn round_trip(value: &Value) -> Value {
        let mut buf = Vec::new();
        encode(value, &mut buf);
        let (decoded, consumed) = decode(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        decoded
    }

    #[test]
    fn number_round_trips() {
        assert_eq!(round_trip(&Value::Number(3.5)), Value::Number(3.5));
        assert_eq!(round_trip(&Value::Number(0.0)), Value::Number(0.0));
        assert_eq!(round_trip(&Value::Number(-1.0)), Value::Number(-1.0));
    }

    #[test]
    fn boolean_round_trips() {
        assert_eq!(round_trip(&Value::Boolean(true)), Value::Boolean(true));
        assert_eq!(round_trip(&Value::Boolean(false)), Value::Boolean(false));
    }

    #[test]
    fn string_round_trips() {
        assert_eq!(
            round_trip(&Value::String("rtmp://example/live".to_string())),
            Value::String("rtmp://example/live".to_string())
        );
        assert_eq!(
            round_trip(&Value::String(String::new())),
            Value::String(String::new())
        );
    }

    #[test]
    fn null_and_undefined_round_trip() {
        assert_eq!(round_trip(&Value::Null), Value::Null);
        assert_eq!(round_trip(&Value::Undefined), Value::Undefined);
    }

    #[test]
    fn object_round_trips_preserving_key_order() {
        let obj = Value::Object(vec![
            ("app".to_string(), Value::String("live".to_string())),
            (
                "flashVer".to_string(),
                Value::String("FMLE/3.0".to_string()),
            ),
            ("audioSampleAccess".to_string(), Value::Boolean(true)),
        ]);
        assert_eq!(round_trip(&obj), obj);
    }

    #[test]
    fn ecma_array_round_trips() {
        let arr = Value::EcmaArray(vec![
            ("0".to_string(), Value::Number(1.0)),
            ("1".to_string(), Value::Number(2.0)),
        ]);
        assert_eq!(round_trip(&arr), arr);
    }

    #[test]
    fn strict_array_round_trips() {
        let arr = Value::StrictArray(vec![
            Value::Number(1.0),
            Value::String("x".to_string()),
            Value::Null,
        ]);
        assert_eq!(round_trip(&arr), arr);
    }

    #[test]
    fn date_round_trips() {
        assert_eq!(
            round_trip(&Value::Date(1_000_000.0)),
            Value::Date(1_000_000.0)
        );
    }

    #[test]
    fn nested_object_in_object_round_trips() {
        let obj = Value::Object(vec![(
            "level".to_string(),
            Value::Object(vec![(
                "code".to_string(),
                Value::String("NetConnection.Connect.Success".to_string()),
            )]),
        )]);
        assert_eq!(round_trip(&obj), obj);
    }

    #[test]
    fn truncated_input_is_rejected_not_panicking() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[MARKER_NUMBER, 1, 2, 3]).is_err());
        assert!(decode(&[MARKER_STRING, 0, 5, b'h', b'i']).is_err());
        assert!(decode(&[MARKER_OBJECT]).is_err());
    }

    #[test]
    fn a_long_string_marker_is_used_for_strings_over_u16_max() {
        let long = "x".repeat(70_000);
        let value = Value::String(long.clone());
        let mut buf = Vec::new();
        encode(&value, &mut buf);
        assert_eq!(buf[0], MARKER_LONG_STRING);
        assert_eq!(round_trip(&value), Value::String(long));
    }
}
