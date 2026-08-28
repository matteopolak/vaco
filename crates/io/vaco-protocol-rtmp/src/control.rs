//! The six protocol control messages every RTMP session exchanges,
//! regardless of what AMF/command layer runs on top: Set Chunk Size, Abort
//! Message, Acknowledgement, User Control Message, Window Acknowledgement
//! Size, and Set Peer Bandwidth.
//!
//! All six travel on chunk stream ID 2 with message stream ID 0
//! (`adobe-rtmp-spec-1.0` §5.4).

use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "rtmp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// Chunk stream ID every protocol control message uses.
pub const CONTROL_CSID: u32 = 2;
/// Message stream ID every protocol control message uses.
pub const CONTROL_MESSAGE_STREAM_ID: u32 = 0;

const TYPE_SET_CHUNK_SIZE: u8 = 1;
const TYPE_ABORT: u8 = 2;
const TYPE_ACKNOWLEDGEMENT: u8 = 3;
const TYPE_USER_CONTROL: u8 = 4;
const TYPE_WINDOW_ACK_SIZE: u8 = 5;
const TYPE_SET_PEER_BANDWIDTH: u8 = 6;

/// `Set Peer Bandwidth`'s limit-type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitType {
    /// The peer must not exceed the given window size.
    Hard,
    /// The peer may exceed it only if its own existing limit was already
    /// larger.
    Soft,
    /// Treat as `Hard` if the previous `Set Peer Bandwidth` was `Hard`,
    /// otherwise ignore.
    Dynamic,
}

impl LimitType {
    const fn to_byte(self) -> u8 {
        match self {
            Self::Hard => 0,
            Self::Soft => 1,
            Self::Dynamic => 2,
        }
    }

    const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Hard),
            1 => Some(Self::Soft),
            2 => Some(Self::Dynamic),
            _ => None,
        }
    }
}

/// A `User Control Message`'s event, §5.4.9's `EventType`+`EventData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserControlEvent {
    StreamBegin {
        stream_id: u32,
    },
    StreamEof {
        stream_id: u32,
    },
    StreamDry {
        stream_id: u32,
    },
    SetBufferLength {
        stream_id: u32,
        buffer_length_ms: u32,
    },
    StreamIsRecorded {
        stream_id: u32,
    },
    PingRequest {
        timestamp: u32,
    },
    PingResponse {
        timestamp: u32,
    },
    /// An event type this crate does not name, carried through rather than
    /// dropped — an unrecognised event is not the same fact as a malformed
    /// one.
    Other {
        event_type: u16,
        data: Vec<u8>,
    },
}

impl UserControlEvent {
    fn event_type(&self) -> u16 {
        match self {
            Self::StreamBegin { .. } => 0,
            Self::StreamEof { .. } => 1,
            Self::StreamDry { .. } => 2,
            Self::SetBufferLength { .. } => 3,
            Self::StreamIsRecorded { .. } => 4,
            Self::PingRequest { .. } => 6,
            Self::PingResponse { .. } => 7,
            Self::Other { event_type, .. } => *event_type,
        }
    }

    fn encode_data(&self, out: &mut Vec<u8>) {
        match self {
            Self::StreamBegin { stream_id }
            | Self::StreamEof { stream_id }
            | Self::StreamDry { stream_id }
            | Self::StreamIsRecorded { stream_id } => {
                out.extend_from_slice(&stream_id.to_be_bytes());
            }
            Self::SetBufferLength {
                stream_id,
                buffer_length_ms,
            } => {
                out.extend_from_slice(&stream_id.to_be_bytes());
                out.extend_from_slice(&buffer_length_ms.to_be_bytes());
            }
            Self::PingRequest { timestamp } | Self::PingResponse { timestamp } => {
                out.extend_from_slice(&timestamp.to_be_bytes());
            }
            Self::Other { data, .. } => out.extend_from_slice(data),
        }
    }

    fn decode(event_type: u16, data: &[u8]) -> Result<Self> {
        fn u32_be(data: &[u8]) -> Result<u32> {
            <[u8; 4]>::try_from(data)
                .map(u32::from_be_bytes)
                .map_err(|_| malformed("user control event data too short for its type"))
        }
        Ok(match event_type {
            0 => Self::StreamBegin {
                stream_id: u32_be(data)?,
            },
            1 => Self::StreamEof {
                stream_id: u32_be(data)?,
            },
            2 => Self::StreamDry {
                stream_id: u32_be(data)?,
            },
            3 => {
                let Some(sid_bytes) = data.get(..4) else {
                    return Err(malformed("SetBufferLength needs 8 bytes"));
                };
                let Some(len_bytes) = data.get(4..8) else {
                    return Err(malformed("SetBufferLength needs 8 bytes"));
                };
                Self::SetBufferLength {
                    stream_id: u32_be(sid_bytes)?,
                    buffer_length_ms: u32_be(len_bytes)?,
                }
            }
            4 => Self::StreamIsRecorded {
                stream_id: u32_be(data)?,
            },
            6 => Self::PingRequest {
                timestamp: u32_be(data)?,
            },
            7 => Self::PingResponse {
                timestamp: u32_be(data)?,
            },
            other => Self::Other {
                event_type: other,
                data: data.to_vec(),
            },
        })
    }
}

/// One protocol control message (message type IDs 1-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    /// New maximum chunk payload size for chunks sent *by the peer that
    /// sent this*, effective immediately.
    SetChunkSize(u32),
    /// Discard any partially-received message on the named chunk stream.
    Abort {
        chunk_stream_id: u32,
    },
    /// Total bytes received so far, sent after each `Window Acknowledgement
    /// Size` worth of data.
    Acknowledgement {
        sequence_number: u32,
    },
    UserControl(UserControlEvent),
    /// Request an `Acknowledgement` after this many bytes.
    WindowAckSize(u32),
    SetPeerBandwidth {
        window_size: u32,
        limit_type: LimitType,
    },
}

impl ControlMessage {
    /// The message type ID this control message is sent as.
    #[must_use]
    pub const fn message_type_id(&self) -> u8 {
        match self {
            Self::SetChunkSize(_) => TYPE_SET_CHUNK_SIZE,
            Self::Abort { .. } => TYPE_ABORT,
            Self::Acknowledgement { .. } => TYPE_ACKNOWLEDGEMENT,
            Self::UserControl(_) => TYPE_USER_CONTROL,
            Self::WindowAckSize(_) => TYPE_WINDOW_ACK_SIZE,
            Self::SetPeerBandwidth { .. } => TYPE_SET_PEER_BANDWIDTH,
        }
    }

    /// Encode this message's payload (not the chunk framing — see
    /// [`CONTROL_CSID`]/[`CONTROL_MESSAGE_STREAM_ID`] and
    /// [`crate::message::chunk_message`]).
    #[must_use]
    pub fn encode_payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::SetChunkSize(size) => {
                // Top bit reserved (must be zero); the low 31 bits carry
                // the chunk size, per §5.4.1.
                out.extend_from_slice(&(*size & 0x7fff_ffff).to_be_bytes());
            }
            Self::Abort { chunk_stream_id } => {
                out.extend_from_slice(&chunk_stream_id.to_be_bytes());
            }
            Self::Acknowledgement { sequence_number } => {
                out.extend_from_slice(&sequence_number.to_be_bytes());
            }
            Self::UserControl(event) => {
                out.extend_from_slice(&event.event_type().to_be_bytes());
                event.encode_data(&mut out);
            }
            Self::WindowAckSize(size) => {
                out.extend_from_slice(&size.to_be_bytes());
            }
            Self::SetPeerBandwidth {
                window_size,
                limit_type,
            } => {
                out.extend_from_slice(&window_size.to_be_bytes());
                out.push(limit_type.to_byte());
            }
        }
        out
    }

    /// Decode a control message from its message type ID and payload.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if `message_type_id` names a control
    /// message but `payload` does not match its fixed shape.
    ///
    /// # Returns
    /// `Ok(None)` if `message_type_id` is not one of the six control types
    /// (1-6) — not an error, since a chunk stream 2 message with some other
    /// type ID is simply not this crate's concern.
    pub fn decode(message_type_id: u8, payload: &[u8]) -> Result<Option<Self>> {
        fn u32_be(data: &[u8], what: &'static str) -> Result<u32> {
            <[u8; 4]>::try_from(data)
                .map(u32::from_be_bytes)
                .map_err(|_| malformed(what))
        }
        Ok(Some(match message_type_id {
            TYPE_SET_CHUNK_SIZE => {
                let raw = u32_be(payload, "Set Chunk Size needs 4 bytes")?;
                Self::SetChunkSize(raw & 0x7fff_ffff)
            }
            TYPE_ABORT => Self::Abort {
                chunk_stream_id: u32_be(payload, "Abort Message needs 4 bytes")?,
            },
            TYPE_ACKNOWLEDGEMENT => Self::Acknowledgement {
                sequence_number: u32_be(payload, "Acknowledgement needs 4 bytes")?,
            },
            TYPE_USER_CONTROL => {
                let Some(type_bytes) = payload.get(..2) else {
                    return Err(malformed("User Control Message needs at least 2 bytes"));
                };
                let Ok(arr) = <[u8; 2]>::try_from(type_bytes) else {
                    return Err(malformed("User Control Message needs at least 2 bytes"));
                };
                let event_type = u16::from_be_bytes(arr);
                let data = payload.get(2..).unwrap_or(&[]);
                Self::UserControl(UserControlEvent::decode(event_type, data)?)
            }
            TYPE_WINDOW_ACK_SIZE => Self::WindowAckSize(u32_be(
                payload,
                "Window Acknowledgement Size needs 4 bytes",
            )?),
            TYPE_SET_PEER_BANDWIDTH => {
                let Some(window_bytes) = payload.get(..4) else {
                    return Err(malformed("Set Peer Bandwidth needs 5 bytes"));
                };
                let Some(&limit_byte) = payload.get(4) else {
                    return Err(malformed("Set Peer Bandwidth needs 5 bytes"));
                };
                let window_size = u32_be(window_bytes, "Set Peer Bandwidth needs 5 bytes")?;
                let limit_type = LimitType::from_byte(limit_byte).ok_or_else(|| {
                    malformed("Set Peer Bandwidth's limit type must be 0, 1 or 2")
                })?;
                Self::SetPeerBandwidth {
                    window_size,
                    limit_type,
                }
            }
            _ => return Ok(None),
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn round_trip(msg: &ControlMessage) {
        let payload = msg.encode_payload();
        let decoded = ControlMessage::decode(msg.message_type_id(), &payload)
            .unwrap()
            .unwrap();
        assert_eq!(&decoded, msg);
    }

    #[test]
    fn set_chunk_size_round_trips_and_masks_the_reserved_bit() {
        round_trip(&ControlMessage::SetChunkSize(4096));
        let payload = ControlMessage::SetChunkSize(0xffff_ffff).encode_payload();
        assert_eq!(payload, vec![0x7f, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn abort_round_trips() {
        round_trip(&ControlMessage::Abort { chunk_stream_id: 7 });
    }

    #[test]
    fn acknowledgement_round_trips() {
        round_trip(&ControlMessage::Acknowledgement {
            sequence_number: 123_456,
        });
    }

    #[test]
    fn window_ack_size_round_trips() {
        round_trip(&ControlMessage::WindowAckSize(2_500_000));
    }

    #[test]
    fn set_peer_bandwidth_round_trips_each_limit_type() {
        for limit_type in [LimitType::Hard, LimitType::Soft, LimitType::Dynamic] {
            round_trip(&ControlMessage::SetPeerBandwidth {
                window_size: 2_500_000,
                limit_type,
            });
        }
    }

    #[test]
    fn set_peer_bandwidth_rejects_an_unknown_limit_type() {
        let mut payload = 2_500_000u32.to_be_bytes().to_vec();
        payload.push(9);
        assert!(ControlMessage::decode(TYPE_SET_PEER_BANDWIDTH, &payload).is_err());
    }

    #[test]
    fn user_control_named_events_round_trip() {
        round_trip(&ControlMessage::UserControl(
            UserControlEvent::StreamBegin { stream_id: 1 },
        ));
        round_trip(&ControlMessage::UserControl(
            UserControlEvent::SetBufferLength {
                stream_id: 1,
                buffer_length_ms: 3000,
            },
        ));
        round_trip(&ControlMessage::UserControl(
            UserControlEvent::PingRequest { timestamp: 999 },
        ));
        round_trip(&ControlMessage::UserControl(
            UserControlEvent::PingResponse { timestamp: 999 },
        ));
    }

    #[test]
    fn user_control_unknown_event_type_is_carried_through_not_rejected() {
        round_trip(&ControlMessage::UserControl(UserControlEvent::Other {
            event_type: 42,
            data: vec![1, 2, 3, 4, 5],
        }));
    }

    #[test]
    fn non_control_message_type_decodes_to_none() {
        assert_eq!(ControlMessage::decode(20, &[]).unwrap(), None);
    }

    #[test]
    fn short_payload_is_malformed_not_a_panic() {
        assert!(ControlMessage::decode(TYPE_SET_CHUNK_SIZE, &[0, 1]).is_err());
        assert!(ControlMessage::decode(TYPE_USER_CONTROL, &[0]).is_err());
        assert!(ControlMessage::decode(TYPE_SET_PEER_BANDWIDTH, &[0, 0, 0, 0]).is_err());
    }
}
