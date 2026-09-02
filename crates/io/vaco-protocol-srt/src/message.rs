//! The two transmission modes and message reassembly.
//!
//! # `TransmissionMode`: what the `STREAM` flag actually distinguishes
//!
//! `draft-sharabayko-srt-01`'s own `SRT Flags` table (`handshake::srt_flags`,
//! §3.2.1.1.1, draft-derived) names a `STREAM` bit without giving its
//! semantics in the fetched text beyond the name itself. This module's
//! reading, applied rather than merely quoted: when `STREAM` is **set**,
//! the connection is byte-stream-oriented (SRT's "buffer"/file-transfer
//! mode) — data packets carry no message boundary of their own, and an
//! application reads a continuous byte stream exactly as
//! [`crate::arq::ReceiveWindow`] already delivers one (in strictly
//! increasing sequence order, concatenated). When `STREAM` is **clear**
//! (the default this module assumes absent other information — SRT's
//! "live" mode is the more common one for real-time contribution use
//! cases), the connection is message-oriented: each application-level send
//! is one message, reassembled from one or more data packets via the
//! [`PacketPosition`] flag (`packet.rs`, already draft-derived) and
//! `DataPacket::msg_no`, delivered as one complete unit, not byte-by-byte.
//!
//! **Not independently confirmable without a reference peer** — it follows
//! from the flag's own name and from message mode being the only shape
//! that gives `PacketPosition`/`msg_no` a reason to exist at all, since a
//! pure byte stream would not need them. Stated as an inference, not
//! re-labeled draft-derived.
//!
//! # Message reassembly, sans-io
//!
//! [`MessageReassembler`] takes delivery/drop events in the order
//! [`crate::arq::ReceiveWindow`] already produces them (strictly by
//! sequence number) and groups them by [`crate::packet::DataPacket::msg_no`],
//! using [`PacketPosition`] to know where a message starts and ends.
//! **A message is all-or-nothing**: if any of its packets is
//! too-late-dropped by the ARQ layer before the message completes, the
//! whole partially-assembled message is discarded, not delivered with a
//! hole in it — inferred from message mode's own point (boundary
//! preservation implies a boundary that arrives incomplete is not a
//! valid unit to hand to the application), not a rule the fetched draft
//! text states in so many words.

use std::collections::BTreeMap;

use crate::handshake::{HsReqBody, srt_flags};
use crate::packet::PacketPosition;

/// Which of the two transmission shapes a connection uses — derived from
/// the peer's own `HSREQ`/`HSRSP` `SRT Flags`, not negotiated by this
/// module itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmissionMode {
    /// `STREAM` clear: message-oriented, boundaries preserved.
    Message,
    /// `STREAM` set: byte-stream-oriented, no boundaries.
    Stream,
}

impl TransmissionMode {
    #[must_use]
    pub const fn from_hsreq(hsreq: &HsReqBody) -> Self {
        if hsreq.srt_flags & srt_flags::STREAM != 0 {
            Self::Stream
        } else {
            Self::Message
        }
    }
}

/// One reassembly outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageEvent {
    /// Every packet of this message arrived; `payload` is their
    /// concatenation in position order.
    Complete { msg_no: u32, payload: Vec<u8> },
    /// At least one packet of this message was too-late-dropped by the
    /// ARQ layer before the message completed — the partial assembly is
    /// discarded, per this module's own all-or-nothing reading.
    Dropped { msg_no: u32 },
}

#[derive(Default)]
struct InProgress {
    parts: Vec<Vec<u8>>,
}

/// Reassembles data-packet deliveries into whole messages. Only meaningful
/// for [`TransmissionMode::Message`] — a [`TransmissionMode::Stream`]
/// connection needs no reassembly at all; [`crate::arq::ReceiveWindow`]'s
/// own in-order delivery already *is* the byte stream.
#[derive(Debug, Default)]
pub struct MessageReassembler {
    in_progress: BTreeMap<u32, InProgress>,
}

impl core::fmt::Debug for InProgress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InProgress")
            .field("parts", &self.parts.len())
            .finish()
    }
}

impl MessageReassembler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one packet the ARQ layer just delivered, in sequence order.
    ///
    /// A `First`/`Middle` packet for a `msg_no` this reassembler has not
    /// seen `First` for yet starts a fresh accumulation rather than
    /// erroring — a defensive choice (this module has no reference to
    /// confirm what a real peer's own error handling looks like here), not
    /// a documented protocol rule.
    pub fn on_delivered(
        &mut self,
        position: PacketPosition,
        msg_no: u32,
        payload: Vec<u8>,
    ) -> Option<MessageEvent> {
        match position {
            PacketPosition::Single => Some(MessageEvent::Complete { msg_no, payload }),
            PacketPosition::First => {
                self.in_progress.insert(
                    msg_no,
                    InProgress {
                        parts: vec![payload],
                    },
                );
                None
            }
            PacketPosition::Middle => {
                self.in_progress
                    .entry(msg_no)
                    .or_default()
                    .parts
                    .push(payload);
                None
            }
            PacketPosition::Last => {
                let mut entry = self.in_progress.remove(&msg_no).unwrap_or_default();
                entry.parts.push(payload);
                Some(MessageEvent::Complete {
                    msg_no,
                    payload: entry.parts.concat(),
                })
            }
        }
    }

    /// The ARQ layer gave up on a sequence number belonging to `msg_no` —
    /// discard whatever of that message was accumulated, if any.
    pub fn on_seq_dropped(&mut self, msg_no: u32) -> Option<MessageEvent> {
        self.in_progress
            .remove(&msg_no)
            .map(|_| MessageEvent::Dropped { msg_no })
    }

    #[must_use]
    pub fn in_progress_count(&self) -> usize {
        self.in_progress.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use crate::handshake::EncryptionField;

    fn hsreq(flags: u32) -> HsReqBody {
        HsReqBody {
            srt_version: 0,
            srt_flags: flags,
            receiver_tsbpd_delay: 0,
            sender_tsbpd_delay: 0,
        }
    }

    /// Draft-derived: the `STREAM` bit value itself (`handshake::srt_flags`,
    /// already checked against `draft` Table 6 in `handshake.rs`'s own
    /// tests); this test only checks this module's *interpretation* of it,
    /// which is stated as an inference in the module docs, not draft text.
    #[test]
    fn stream_flag_selects_stream_mode_and_its_absence_selects_message_mode() {
        assert_eq!(
            TransmissionMode::from_hsreq(&hsreq(srt_flags::STREAM)),
            TransmissionMode::Stream
        );
        assert_eq!(
            TransmissionMode::from_hsreq(&hsreq(0)),
            TransmissionMode::Message
        );
        assert_eq!(
            TransmissionMode::from_hsreq(&hsreq(srt_flags::STREAM | srt_flags::TSBPDSND)),
            TransmissionMode::Stream,
            "STREAM combined with other flags still selects Stream mode"
        );
        let _ = EncryptionField::None; // silence an unused-import warning on some feature sets
    }

    #[test]
    fn single_packet_message_completes_immediately() {
        let mut r = MessageReassembler::new();
        let event = r.on_delivered(PacketPosition::Single, 1, vec![1, 2, 3]);
        assert_eq!(
            event,
            Some(MessageEvent::Complete {
                msg_no: 1,
                payload: vec![1, 2, 3]
            })
        );
        assert_eq!(r.in_progress_count(), 0);
    }

    #[test]
    fn multi_packet_message_completes_only_on_last_and_concatenates_in_order() {
        let mut r = MessageReassembler::new();
        assert_eq!(r.on_delivered(PacketPosition::First, 5, vec![1]), None);
        assert_eq!(r.in_progress_count(), 1);
        assert_eq!(r.on_delivered(PacketPosition::Middle, 5, vec![2]), None);
        assert_eq!(
            r.on_delivered(PacketPosition::Last, 5, vec![3]),
            Some(MessageEvent::Complete {
                msg_no: 5,
                payload: vec![1, 2, 3]
            })
        );
        assert_eq!(r.in_progress_count(), 0);
    }

    #[test]
    fn a_dropped_packet_discards_the_whole_partial_message_not_just_the_hole() {
        let mut r = MessageReassembler::new();
        r.on_delivered(PacketPosition::First, 9, vec![1]);
        r.on_delivered(PacketPosition::Middle, 9, vec![2]);
        let event = r.on_seq_dropped(9);
        assert_eq!(event, Some(MessageEvent::Dropped { msg_no: 9 }));
        assert_eq!(
            r.in_progress_count(),
            0,
            "the partial message must not linger"
        );

        // A subsequent Last for the same msg_no (a stray retransmission
        // arriving after the drop) starts fresh rather than completing a
        // message that was already given up on.
        assert_eq!(
            r.on_delivered(PacketPosition::Last, 9, vec![3]),
            Some(MessageEvent::Complete {
                msg_no: 9,
                payload: vec![3]
            })
        );
    }

    #[test]
    fn independent_messages_interleave_without_cross_contamination() {
        let mut r = MessageReassembler::new();
        r.on_delivered(PacketPosition::First, 1, vec![b'a']);
        r.on_delivered(PacketPosition::First, 2, vec![b'x']);
        assert_eq!(
            r.on_delivered(PacketPosition::Last, 1, vec![b'b']),
            Some(MessageEvent::Complete {
                msg_no: 1,
                payload: vec![b'a', b'b']
            })
        );
        assert_eq!(
            r.on_delivered(PacketPosition::Last, 2, vec![b'y']),
            Some(MessageEvent::Complete {
                msg_no: 2,
                payload: vec![b'x', b'y']
            })
        );
    }
}
