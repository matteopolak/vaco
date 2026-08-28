//! Self-consistency: a full PSK-encrypted Main Profile tunnel session,
//! both directions, closing the loop `crate::gre` + `crate::psk` open for
//! -- "a main-profile encrypted session completes... in both directions"
//! (#559's Acc), with the reference-peer half of that Acc replaced by this
//! crate's own two sides agreeing (no reference RIST peer is reachable on
//! this machine; see the crate's own module docs for why).
//!
//! §7's own rule: "The entire payload of the GRE packet, not including
//! the GRE header, shall be encrypted... the GRE header shall be
//! transmitted in the clear." Every scenario below builds exactly that
//! shape: a clear `GreHeader` (`K`=1 carrying the nonce, `S`=1 carrying
//! the sequence number) followed by an encrypted body.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

use vaco_protocol_rist::gre::{
    GreHeader, PROTOCOL_TYPE_IP, ReducedUdpHeader, RistVersion, VSF_ETHERTYPE, VSF_PROTOCOL_TYPE_RIST,
    VSF_SUBTYPE_REDUCED_OVERHEAD, VsfHeader,
};
use vaco_protocol_rist::psk::{self, KeyBits};

const PASSPHRASE: &[u8] = b"Reliable Internet Stream Transport";

/// Build one PSK-encrypted tunnel packet: a clear `GreHeader` (nonce,
/// sequence number) followed by an encrypted body.
fn build_encrypted_packet(nonce: u32, sequence_number: u32, key_bits: KeyBits, body: &[u8]) -> Vec<u8> {
    let header = GreHeader {
        checksum: None,
        key_or_nonce: Some(nonce),
        sequence_number: Some(sequence_number),
        h: key_bits.to_h_bit(),
        rv: RistVersion::V2022,
        protocol_type: VSF_ETHERTYPE,
    };
    let key = psk::derive_key(PASSPHRASE, nonce, key_bits);
    let mut encrypted_body = body.to_vec();
    psk::apply_keystream(&key, sequence_number, &mut encrypted_body).unwrap();

    let mut packet = header.serialize();
    packet.extend_from_slice(&encrypted_body);
    packet
}

/// The receiving side: parse the clear header, derive the same key from
/// the same passphrase and the header's own nonce, decrypt, and hand back
/// the recovered plaintext body.
fn receive_encrypted_packet(packet: &[u8]) -> Vec<u8> {
    let (header, consumed) = GreHeader::parse(packet).unwrap();
    let nonce = header.key_or_nonce.expect("PSK packets carry K=1");
    let sequence_number = header.sequence_number.expect("PSK packets carry S=1");
    let key_bits = KeyBits::from_h_bit(header.h);
    let key = psk::derive_key(PASSPHRASE, nonce, key_bits);
    let mut body = packet.get(consumed..).unwrap().to_vec();
    psk::apply_keystream(&key, sequence_number, &mut body).unwrap();
    body
}

/// A Reduced Overhead Mode body: `VsfHeader` + `ReducedUdpHeader` + an
/// opaque RTP-shaped payload (the payload's own interpretation is
/// `vaco-rtp`'s job, not this crate's tunnelling layer).
fn reduced_overhead_body(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let mut body = VsfHeader {
        protocol_type: VSF_PROTOCOL_TYPE_RIST,
        subtype: VSF_SUBTYPE_REDUCED_OVERHEAD,
    }
    .serialize()
    .to_vec();
    body.extend_from_slice(
        &ReducedUdpHeader {
            source_port: src_port,
            destination_port: dst_port,
        }
        .serialize(),
    );
    body.extend_from_slice(payload);
    body
}

#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn reduced_overhead_session_completes_in_both_directions_aes128() {
    let payload_a_to_b = b"RTP packet from A, seq 100".to_vec();
    let payload_b_to_a = b"RTCP receiver report from B".to_vec();

    // A -> B
    let body = reduced_overhead_body(3000, 3001, &payload_a_to_b);
    let packet = build_encrypted_packet(0x0000_0001, 1, KeyBits::Aes128, &body);
    let recovered = receive_encrypted_packet(&packet);
    assert_eq!(recovered, body);
    let (vsf, consumed) = VsfHeader::parse(&recovered).unwrap();
    assert_eq!(vsf.protocol_type, VSF_PROTOCOL_TYPE_RIST);
    let (udp, consumed2) = ReducedUdpHeader::parse(&recovered[consumed..]).unwrap();
    assert_eq!((udp.source_port, udp.destination_port), (3000, 3001));
    assert_eq!(&recovered[consumed + consumed2..], &payload_a_to_b[..]);

    // B -> A: a different nonce, a different sequence number, the same
    // passphrase -- proving the session is symmetric, not just replaying
    // the same derived key in one direction.
    let body = reduced_overhead_body(3001, 3000, &payload_b_to_a);
    let packet = build_encrypted_packet(0x0000_0002, 1, KeyBits::Aes128, &body);
    let recovered = receive_encrypted_packet(&packet);
    assert_eq!(recovered, body);
}

#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn reduced_overhead_session_completes_in_both_directions_aes256() {
    let payload_a_to_b = vec![0xAAu8; 200];
    let payload_b_to_a = vec![0x55u8; 64];

    let body = reduced_overhead_body(4000, 4001, &payload_a_to_b);
    let packet = build_encrypted_packet(0xCAFE_BABE, 42, KeyBits::Aes256, &body);
    assert_eq!(receive_encrypted_packet(&packet), body);

    let body = reduced_overhead_body(4001, 4000, &payload_b_to_a);
    let packet = build_encrypted_packet(0xF00D_F00D, 43, KeyBits::Aes256, &body);
    assert_eq!(receive_encrypted_packet(&packet), body);
}

#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn full_datagram_mode_payload_round_trips_encrypted() {
    // Full Datagram Mode's body is "a full layer-3 IP packet" -- opaque to
    // this crate either way, so the test payload is just arbitrary bytes
    // standing in for one.
    let fake_ip_packet: Vec<u8> = (0..300u32).map(|i| (i % 256) as u8).collect();
    let header = GreHeader {
        checksum: None,
        key_or_nonce: Some(9),
        sequence_number: Some(5),
        h: false,
        rv: RistVersion::V2022,
        protocol_type: PROTOCOL_TYPE_IP,
    };
    let key = psk::derive_key(PASSPHRASE, 9, KeyBits::Aes128);
    let mut encrypted = fake_ip_packet.clone();
    psk::apply_keystream(&key, 5, &mut encrypted).unwrap();
    let mut packet = header.serialize();
    packet.extend_from_slice(&encrypted);

    let (parsed_header, consumed) = GreHeader::parse(&packet).unwrap();
    assert_eq!(parsed_header.protocol_type, PROTOCOL_TYPE_IP);
    let mut recovered = packet[consumed..].to_vec();
    psk::apply_keystream(&key, 5, &mut recovered).unwrap();
    assert_eq!(recovered, fake_ip_packet);
}

#[test]
fn a_wrong_passphrase_does_not_silently_recover_the_plaintext() {
    let body = reduced_overhead_body(1, 2, b"secret");
    let packet = build_encrypted_packet(1, 1, KeyBits::Aes128, &body);

    let (header, consumed) = GreHeader::parse(&packet).unwrap();
    let wrong_key = psk::derive_key(b"the wrong passphrase entirely", header.key_or_nonce.unwrap(), KeyBits::Aes128);
    let mut wrongly_decrypted = packet[consumed..].to_vec();
    psk::apply_keystream(&wrong_key, header.sequence_number.unwrap(), &mut wrongly_decrypted).unwrap();
    assert_ne!(wrongly_decrypted, body, "a wrong passphrase must not happen to recover the same bytes");
}
