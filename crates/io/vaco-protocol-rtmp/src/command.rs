//! NetConnection/NetStream command messages — Adobe RTMP spec's §7, riding
//! on [`crate::amf0`]. Issue #553.
//!
//! **Scope, stated up front.** AMF0 command messages
//! (`message_type_id = 20`) only — AMF3 command messages
//! (`message_type_id = 17`, an extra leading `0x00` byte then AMF3-encoded
//! values) are not built. AMF0 is what `connect()` negotiates by default
//! and what real deployments (`ffmpeg`'s own `rtmpproto.c`-compatible
//! peers included) actually send; nothing in this crate's own flow needs
//! AMF3.
//!
//! A command message's payload is a flat sequence of AMF0 values: the
//! command name (string), the transaction ID (number), a "command
//! object" (an object or [`crate::amf0::Value::Null`]), then zero or more
//! further arguments. [`Command`] models exactly that shape.

use crate::amf0::{self, Value};
use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "rtmp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// One NetConnection/NetStream command, in either direction.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub name: String,
    pub transaction_id: f64,
    pub command_object: Value,
    pub arguments: Vec<Value>,
}

impl Command {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        transaction_id: f64,
        command_object: Value,
        arguments: Vec<Value>,
    ) -> Self {
        Self {
            name: name.into(),
            transaction_id,
            command_object,
            arguments,
        }
    }

    /// Encode as an AMF0 command message payload (the bytes that go in
    /// [`crate::message::RtmpMessage`]'s `payload` with
    /// `message_type_id = 20`).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        amf0::encode(&Value::String(self.name.clone()), &mut out);
        amf0::encode(&Value::Number(self.transaction_id), &mut out);
        amf0::encode(&self.command_object, &mut out);
        for arg in &self.arguments {
            amf0::encode(arg, &mut out);
        }
        out
    }

    /// Decode from an AMF0 command message payload.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if the payload does not start with a
    /// string (the command name) followed by a number (the transaction
    /// ID), or if any value in the sequence is truncated/unrecognised.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut cursor = 0usize;
        let (name_value, consumed) = amf0::decode(
            payload
                .get(cursor..)
                .ok_or_else(|| malformed("command message is empty"))?,
        )?;
        cursor += consumed;
        let Value::String(name) = name_value else {
            return Err(malformed(
                "command message does not start with a string command name",
            ));
        };

        let (id_value, consumed) = amf0::decode(
            payload
                .get(cursor..)
                .ok_or_else(|| malformed("command message has no transaction ID"))?,
        )?;
        cursor += consumed;
        let Value::Number(transaction_id) = id_value else {
            return Err(malformed(
                "command message's transaction ID is not a number",
            ));
        };

        let (command_object, consumed) = amf0::decode(
            payload
                .get(cursor..)
                .ok_or_else(|| malformed("command message has no command object"))?,
        )?;
        cursor += consumed;

        let mut arguments = Vec::new();
        while cursor < payload.len() {
            #[allow(
                clippy::indexing_slicing,
                reason = "cursor < payload.len() checked by the loop condition"
            )]
            let (value, consumed) = amf0::decode(&payload[cursor..])?;
            arguments.push(value);
            cursor += consumed;
        }

        Ok(Self {
            name,
            transaction_id,
            command_object,
            arguments,
        })
    }
}

/// Build the client's `connect` command (§7.2.1.1) — the first command
/// message on `NetConnection`'s own chunk stream (conventionally chunk
/// stream ID 3, message stream ID 0).
#[must_use]
pub fn connect(app: &str, tc_url: &str) -> Command {
    Command::new(
        "connect",
        1.0,
        Value::Object(vec![
            ("app".to_string(), Value::String(app.to_string())),
            ("type".to_string(), Value::String("nonprivate".to_string())),
            ("tcUrl".to_string(), Value::String(tc_url.to_string())),
        ]),
        Vec::new(),
    )
}

/// Build the server's successful response to `connect` (§7.2.1.1's own
/// worked example names `_result` with a properties object and an
/// information object).
#[must_use]
pub fn connect_result(transaction_id: f64) -> Command {
    Command::new(
        "_result",
        transaction_id,
        Value::Object(vec![
            (
                "fmsVer".to_string(),
                Value::String("VACO/1,0,0,0".to_string()),
            ),
            ("capabilities".to_string(), Value::Number(31.0)),
        ]),
        vec![Value::Object(vec![
            ("level".to_string(), Value::String("status".to_string())),
            (
                "code".to_string(),
                Value::String("NetConnection.Connect.Success".to_string()),
            ),
            (
                "description".to_string(),
                Value::String("Connection succeeded.".to_string()),
            ),
        ])],
    )
}

/// Build the client's `createStream` command (§7.2.1.3): command object
/// is always Null, no further arguments.
#[must_use]
pub fn create_stream(transaction_id: f64) -> Command {
    Command::new("createStream", transaction_id, Value::Null, Vec::new())
}

/// Build the server's response to `createStream`: `_result` with a Null
/// command object and one argument, the new stream ID.
#[must_use]
pub fn create_stream_result(transaction_id: f64, stream_id: f64) -> Command {
    Command::new(
        "_result",
        transaction_id,
        Value::Null,
        vec![Value::Number(stream_id)],
    )
}

/// Build the client's `publish` command (§7.2.2.6): stream name and
/// publish type (`"live"`/`"record"`/`"append"`) as arguments, sent on
/// the stream ID `createStream` returned.
#[must_use]
pub fn publish(stream_name: &str, publish_type: &str) -> Command {
    Command::new(
        "publish",
        0.0,
        Value::Null,
        vec![
            Value::String(stream_name.to_string()),
            Value::String(publish_type.to_string()),
        ],
    )
}

/// Build the server's `onStatus` response to a successful `publish`
/// (§7.2.2.6's own named status code `NetStream.Publish.Start`).
#[must_use]
pub fn on_status_publish_start(stream_name: &str) -> Command {
    Command::new(
        "onStatus",
        0.0,
        Value::Null,
        vec![Value::Object(vec![
            ("level".to_string(), Value::String("status".to_string())),
            (
                "code".to_string(),
                Value::String("NetStream.Publish.Start".to_string()),
            ),
            (
                "description".to_string(),
                Value::String(format!("{stream_name} is now published.")),
            ),
        ])],
    )
}

/// Build the client's `play` command (§7.2.2.1): stream name as the first
/// argument.
#[must_use]
pub fn play(stream_name: &str) -> Command {
    Command::new(
        "play",
        0.0,
        Value::Null,
        vec![Value::String(stream_name.to_string())],
    )
}

/// Build the server's `onStatus` response to a successful `play`
/// (§7.2.2.1's own named status code `NetStream.Play.Start`).
#[must_use]
pub fn on_status_play_start(stream_name: &str) -> Command {
    Command::new(
        "onStatus",
        0.0,
        Value::Null,
        vec![Value::Object(vec![
            ("level".to_string(), Value::String("status".to_string())),
            (
                "code".to_string(),
                Value::String("NetStream.Play.Start".to_string()),
            ),
            (
                "description".to_string(),
                Value::String(format!("Started playing {stream_name}.")),
            ),
        ])],
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn connect_command_round_trips_through_encode_decode() {
        let cmd = connect("live", "rtmp://example.test/live");
        let decoded = Command::decode(&cmd.encode()).unwrap();
        assert_eq!(decoded, cmd);
    }

    #[test]
    fn every_named_command_and_response_round_trips() {
        for cmd in [
            connect("live", "rtmp://x/live"),
            connect_result(1.0),
            create_stream(2.0),
            create_stream_result(2.0, 1.0),
            publish("mystream", "live"),
            on_status_publish_start("mystream"),
            play("mystream"),
            on_status_play_start("mystream"),
        ] {
            let decoded = Command::decode(&cmd.encode()).unwrap();
            assert_eq!(decoded, cmd);
        }
    }

    #[test]
    fn decode_rejects_a_payload_not_starting_with_a_string() {
        let mut buf = Vec::new();
        amf0::encode(&Value::Number(1.0), &mut buf);
        assert!(Command::decode(&buf).is_err());
    }

    #[test]
    fn decode_rejects_an_empty_payload() {
        assert!(Command::decode(&[]).is_err());
    }
}
