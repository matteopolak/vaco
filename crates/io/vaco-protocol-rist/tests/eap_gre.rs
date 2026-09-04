use vaco_protocol_rist::{
    eap::{AuthenticationLimits, EapolPacket},
    gre::{AuthenticationFrame, GreHeader, PROTOCOL_TYPE_EAPOL, RistVersion},
};

#[test]
fn annex_d_eapol_uses_cleartext_gre_protocol_type() {
    let frame = AuthenticationFrame::new(EapolPacket::Start, Some(0x0102_0304));
    let bytes = frame.serialize().unwrap();
    let (header, offset) = GreHeader::parse(&bytes).unwrap();
    assert_eq!(header.protocol_type, PROTOCOL_TYPE_EAPOL);
    assert_eq!(header.sequence_number, Some(0x0102_0304));
    assert_eq!(bytes.get(offset..), Some([3, 1, 0, 0].as_slice()));
    assert_eq!(
        AuthenticationFrame::parse(&bytes, AuthenticationLimits::default()).unwrap(),
        frame
    );
}

#[test]
fn non_eapol_gre_packet_is_not_accepted_as_authentication() {
    let header = GreHeader {
        checksum: None,
        key_or_nonce: None,
        sequence_number: None,
        h: false,
        rv: RistVersion::V2022,
        protocol_type: 0x0800,
    };
    let mut bytes = header.serialize();
    bytes.extend_from_slice(&[3, 1, 0, 0]);
    assert!(AuthenticationFrame::parse(&bytes, AuthenticationLimits::default()).is_err());
}
