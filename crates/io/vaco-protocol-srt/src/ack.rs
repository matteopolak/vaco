//! The ACK and NAK control packets' Control Information Fields —
//! `draft-sharabayko-srt-01` §3.2.4 and §3.2.5. `packet.rs`'s own module
//! docs cover the base control frame both sit on top of, including that
//! `Acknowledgement Number`/`Reserved` reuses the base frame's own
//! `subtype_or_reserved`/`type_specific` slot — a separate per-ACK-message
//! counter for `ACKACK` correlation, distinct from the
//! `Last Acknowledged Packet Sequence Number` inside the CIF below. See
//! [`AckCif`] and [`parse_nak_cif`] for each CIF's own layout.

use vaco_protocol_core::{ProtocolError, Result};

use crate::packet::be32;

const SCHEME: &str = "srt";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// `draft` §3.2.4's own recommendation — draft-derived.
pub const LIGHT_ACK_EVERY_N_PACKETS: u32 = 64;
/// `draft` §3.2.4: "A Full ACK control packet is sent every 10 ms" —
/// draft-derived.
pub const FULL_ACK_INTERVAL_MS: u64 = 10;

/// The stats half of an ACK CIF. See [`AckCif`]: the field *shape* is
/// draft-derived, the values are not (no formula is given), so every
/// producer in this crate uses [`AckStats::placeholder`] rather than a
/// hand-tuned guess.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AckStats {
    pub rtt_us: u32,
    pub rtt_variance_us: u32,
    pub available_buffer_size: u32,
    pub packets_receiving_rate: u32,
    pub estimated_link_capacity: u32,
    pub receiving_rate: u32,
}

impl AckStats {
    /// All-zero — see [`AckCif`] for why this is not a guessed formula.
    #[must_use]
    pub const fn placeholder() -> Self {
        Self {
            rtt_us: 0,
            rtt_variance_us: 0,
            available_buffer_size: 0,
            packets_receiving_rate: 0,
            estimated_link_capacity: 0,
            receiving_rate: 0,
        }
    }
}

/// A Full ACK CIF (`draft` §3.2.4, Figure 13), draft-derived layout:
///
/// ```text
/// |            Last Acknowledged Packet Sequence Number           |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                              RTT                              |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                          RTT Variance                         |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                     Available Buffer Size                     |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                     Packets Receiving Rate                    |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                     Estimated Link Capacity                   |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                         Receiving Rate                        |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
///
/// Seven 32-bit words, 28 bytes — the *shape* is draft-derived. **The
/// values this crate computes for RTT/RTTVar/the three rate fields are
/// not** — the draft states the layout but no formula for any of these
/// (no smoothing constant, no sampling window). [`AckStats::placeholder`]
/// returns all-zero rather than a guessed formula: a wrong *layout* is a
/// wire-compatibility bug a peer would reject, while a wrong but
/// plausible-looking *smoothing constant* would look verified when it is
/// not — zero stays honest about what was never measured.
///
/// `draft` §3.2.4 also names Light ACK ("every 64 packets") and Small ACK,
/// for higher data rates, without giving either's own CIF shape in the
/// fetched text — not implemented here; every ACK this module builds is a
/// Full ACK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckCif {
    pub last_ack_seq_no: u32,
    pub stats: AckStats,
}

const ACK_CIF_LEN: usize = 28;

impl AckCif {
    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` is shorter than 28 bytes.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < ACK_CIF_LEN {
            return Err(malformed("ACK CIF shorter than 28 bytes"));
        }
        Ok(Self {
            last_ack_seq_no: be32(data, 0)?,
            stats: AckStats {
                rtt_us: be32(data, 4)?,
                rtt_variance_us: be32(data, 8)?,
                available_buffer_size: be32(data, 12)?,
                packets_receiving_rate: be32(data, 16)?,
                estimated_link_capacity: be32(data, 20)?,
                receiving_rate: be32(data, 24)?,
            },
        })
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.last_ack_seq_no.to_be_bytes());
        out.extend_from_slice(&self.stats.rtt_us.to_be_bytes());
        out.extend_from_slice(&self.stats.rtt_variance_us.to_be_bytes());
        out.extend_from_slice(&self.stats.available_buffer_size.to_be_bytes());
        out.extend_from_slice(&self.stats.packets_receiving_rate.to_be_bytes());
        out.extend_from_slice(&self.stats.estimated_link_capacity.to_be_bytes());
        out.extend_from_slice(&self.stats.receiving_rate.to_be_bytes());
        out
    }
}

/// A NAK CIF (`draft` §3.2.5, Figure 14), draft-derived layout:
///
/// ```text
/// |0|                 Lost packet sequence number                 |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |1|         Range of lost packets from sequence number          |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |0|                    Up to sequence number                    |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
///
/// **Range compression (the second/third lines above) is not implemented
/// here.** The draft does not state how a decoder tells the second word
/// of a range apart from an unrelated single-loss entry other than "the
/// first word's own top bit was 1", unconfirmable without a reference to
/// test against — getting a *compression* optimisation wrong would
/// silently corrupt the loss list a peer acts on, for zero benefit over
/// the always-safe alternative. This function names each lost sequence
/// number as its own single-entry word (top bit `0`): single-entry loss
/// list only.
///
/// # Errors
/// [`ProtocolError::Malformed`] if `data`'s length is not a whole number
/// of 4-byte words. A `1`-flagged (range) entry is accepted on *parse*
/// (a peer using the real reference's own range compression must still be
/// readable) by treating its own word as one more single-loss entry and
/// the word after it as another — safe because both words still name real
/// sequence numbers, even though the range's true, wider meaning is lost
/// (see above for why this crate does not attempt the compressed encoding
/// itself on write).
pub fn parse_nak_cif(data: &[u8]) -> Result<Vec<u32>> {
    if !data.len().is_multiple_of(4) {
        return Err(malformed("NAK CIF length is not a multiple of 4 bytes"));
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        let word = be32(data, pos)?;
        out.push(word & 0x7fff_ffff);
        pos += 4;
    }
    Ok(out)
}

#[must_use]
pub fn serialize_nak_cif(lost: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &seq in lost {
        out.extend_from_slice(&(seq & 0x7fff_ffff).to_be_bytes());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Draft-derived: `draft` §3.2.4's own recommendation and interval.
    #[test]
    fn ack_timing_constants_match_the_draft() {
        assert_eq!(FULL_ACK_INTERVAL_MS, 10);
        assert_eq!(LIGHT_ACK_EVERY_N_PACKETS, 64);
    }

    /// Draft-derived: `draft` §3.2.4 Figure 13's exact field layout, hand
    /// built, not round-tripped.
    #[test]
    fn ack_cif_matches_the_drafts_own_field_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1000u32.to_be_bytes());
        bytes.extend_from_slice(&20_000u32.to_be_bytes()); // rtt
        bytes.extend_from_slice(&5_000u32.to_be_bytes()); // rtt var
        bytes.extend_from_slice(&8192u32.to_be_bytes()); // avail buffer
        bytes.extend_from_slice(&500u32.to_be_bytes()); // packets recv rate
        bytes.extend_from_slice(&1_000_000u32.to_be_bytes()); // est link cap
        bytes.extend_from_slice(&400_000u32.to_be_bytes()); // recv rate

        let cif = AckCif::parse(&bytes).unwrap();
        assert_eq!(cif.last_ack_seq_no, 1000);
        assert_eq!(cif.stats.rtt_us, 20_000);
        assert_eq!(cif.stats.rtt_variance_us, 5_000);
        assert_eq!(cif.stats.available_buffer_size, 8192);
        assert_eq!(cif.stats.packets_receiving_rate, 500);
        assert_eq!(cif.stats.estimated_link_capacity, 1_000_000);
        assert_eq!(cif.stats.receiving_rate, 400_000);
        assert_eq!(cif.serialize(), bytes);
    }

    /// Self-consistency: this module's own single-entry NAK round-trips.
    #[test]
    fn nak_cif_round_trips_single_entries() {
        let lost = vec![5, 6, 9, 100, 0x7fff_ffff];
        let bytes = serialize_nak_cif(&lost);
        let back = parse_nak_cif(&bytes).unwrap();
        assert_eq!(back, lost);
    }

    #[test]
    fn rejects_a_nak_cif_with_a_partial_trailing_word() {
        assert!(parse_nak_cif(&[0, 0, 0]).is_err());
    }
}
