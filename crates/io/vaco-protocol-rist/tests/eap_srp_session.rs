#![allow(clippy::panic, clippy::unwrap_used, reason = "test assertion helpers")]

use vaco_protocol_rist::{
    auth::{
        AuthenticationAction, AuthenticationConfig, AuthenticationFailure, ClientSession,
        ServerSession, UnknownIdentityPolicy, VerifierStore,
    },
    eap::{AuthenticationLimits, EapCode, EapMessage, EapPacket, EapolPacket, SrpMessage},
    gre::{AuthenticationFrame, GreHeader, PROTOCOL_TYPE_EAPOL, PROTOCOL_TYPE_IP, RistVersion},
    srp::{SecretSource, SrpError, VerifierRecord},
};

struct FixedSecrets(u8);

impl SecretSource for FixedSecrets {
    fn fill_secret(&mut self, output: &mut [u8]) -> Result<(), SrpError> {
        output.fill(0);
        self.0 = self.0.wrapping_add(1).max(1);
        if let Some(last) = output.last_mut() {
            *last = self.0;
        }
        Ok(())
    }
}

struct FailingSecrets;

impl SecretSource for FailingSecrets {
    fn fill_secret(&mut self, _output: &mut [u8]) -> Result<(), SrpError> {
        Err(SrpError::EntropyFailure)
    }
}

#[derive(Clone)]
struct OneVerifier {
    record: VerifierRecord,
}

impl VerifierStore for OneVerifier {
    fn lookup(&self, identity: &[u8]) -> Option<VerifierRecord> {
        if identity == b"rist" {
            Some(self.record.clone())
        } else {
            None
        }
    }
}

struct NoVerifier;

impl VerifierStore for NoVerifier {
    fn lookup(&self, _identity: &[u8]) -> Option<VerifierRecord> {
        None
    }
}

#[test]
fn default_group_exchange_authenticates_both_peers_and_opens_the_gate() {
    let record = VerifierRecord::from_password(
        b"rist",
        b"mainprofile".to_vec(),
        vec![0x72, 0xf9, 0xd5, 0x38],
    )
    .unwrap();
    let config = AuthenticationConfig {
        server_name: b"example".to_vec(),
        ..AuthenticationConfig::default()
    };
    let mut client = ClientSession::new(
        config.clone(),
        b"rist".to_vec(),
        b"mainprofile".to_vec(),
        FixedSecrets(0),
    )
    .unwrap();
    let mut server = ServerSession::new(config, OneVerifier { record }, FixedSecrets(16)).unwrap();

    assert!(!client.allows_data());
    assert!(!server.allows_data());

    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let identity_response = packet(client.on_gre_packet(&identity, 2));
    let challenge = packet(server.on_gre_packet(&identity_response, 3));
    let client_key = packet(client.on_gre_packet(&challenge, 4));
    let server_key = packet(server.on_gre_packet(&client_key, 5));
    let client_validator = packet(client.on_gre_packet(&server_key, 6));
    let server_validator = packet(server.on_gre_packet(&client_validator, 7));
    let success = authenticated_packet(client.on_gre_packet(&server_validator, 8));
    assert_eq!(
        server.on_gre_packet(&success, 9),
        AuthenticationAction::Authenticated { response: None }
    );

    assert!(client.is_authenticated());
    assert!(server.is_authenticated());
    assert!(client.allows_data());
    assert!(server.allows_data());
    assert_eq!(client.session_key(), server.session_key());
    assert!(!client.outbound_uses_session_key_as_psk());
    assert!(!client.inbound_uses_session_key_as_psk());
    assert!(!server.outbound_uses_session_key_as_psk());
    assert!(!server.inbound_uses_session_key_as_psk());

    for bytes in [
        start,
        identity,
        identity_response,
        challenge,
        client_key,
        server_key,
        client_validator,
        server_validator,
        success,
    ] {
        let frame = AuthenticationFrame::parse(&bytes, AuthenticationLimits::default()).unwrap();
        assert_eq!(frame.header.protocol_type, PROTOCOL_TYPE_EAPOL);
        assert_eq!(frame.header.key_or_nonce, None);
        assert!(!frame.header.h);
    }
}

#[test]
fn session_key_psk_selection_is_directional() {
    let server_config = AuthenticationConfig {
        use_session_key_as_psk: true,
        ..AuthenticationConfig::default()
    };
    let mut client = make_client(server_config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        server_config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    complete_exchange(&mut client, &mut server, 0);
    assert!(client.outbound_uses_session_key_as_psk());
    assert!(client.inbound_uses_session_key_as_psk());
    assert!(server.outbound_uses_session_key_as_psk());
    assert!(server.inbound_uses_session_key_as_psk());

    let client_config = AuthenticationConfig {
        use_session_key_as_psk: false,
        ..AuthenticationConfig::default()
    };
    let server_config = AuthenticationConfig {
        use_session_key_as_psk: true,
        ..AuthenticationConfig::default()
    };
    let mut client = make_client(client_config, b"mainprofile");
    let mut server = ServerSession::new(
        server_config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    complete_exchange(&mut client, &mut server, 0);
    assert!(!client.outbound_uses_session_key_as_psk());
    assert!(client.inbound_uses_session_key_as_psk());
    assert!(server.outbound_uses_session_key_as_psk());
    assert!(!server.inbound_uses_session_key_as_psk());
}

#[test]
fn wrong_password_sends_failure_and_keeps_both_gates_closed() {
    let config = AuthenticationConfig::default();
    let record = verifier(b"mainprofile");
    let mut client = make_client(config.clone(), b"wrong-password");
    let mut server = ServerSession::new(config, OneVerifier { record }, FixedSecrets(16)).unwrap();

    let client_validator = drive_to_client_validator(&mut client, &mut server, 0);
    let action = server.on_gre_packet(&client_validator, 7);
    let failure = disconnect_packet(action, AuthenticationFailure::ProofMismatch);
    assert_failure(&failure);
    assert!(!client.allows_data());
    assert!(!server.allows_data());
    assert!(server.session_key().is_none());
}

#[test]
fn privacy_mode_unknown_identity_runs_through_server_key_before_failure() {
    let config = AuthenticationConfig::default();
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(config, NoVerifier, FixedSecrets(16)).unwrap();

    let client_validator = drive_to_client_validator(&mut client, &mut server, 0);
    let action = server.on_gre_packet(&client_validator, 7);
    let failure = disconnect_packet(action, AuthenticationFailure::UnknownIdentity);
    assert_failure(&failure);
}

#[test]
fn fail_fast_unknown_identity_stops_after_identity_response() {
    let config = AuthenticationConfig {
        unknown_identity_policy: UnknownIdentityPolicy::FailFast,
        ..AuthenticationConfig::default()
    };
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(config, NoVerifier, FixedSecrets(16)).unwrap();

    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let identity_response = packet(client.on_gre_packet(&identity, 2));
    let failure = disconnect_packet(
        server.on_gre_packet(&identity_response, 3),
        AuthenticationFailure::UnknownIdentity,
    );
    assert_failure(&failure);
}

#[test]
fn zero_public_values_and_wrong_server_validator_fail_closed() {
    let config = AuthenticationConfig::default();
    let record = verifier(b"mainprofile");
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        config.clone(),
        OneVerifier {
            record: record.clone(),
        },
        FixedSecrets(16),
    )
    .unwrap();

    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let identity_response = packet(client.on_gre_packet(&identity, 2));
    let challenge = packet(server.on_gre_packet(&identity_response, 3));
    let _client_key = packet(client.on_gre_packet(&challenge, 4));
    let invalid_client_key = authentication_packet(EapPacket {
        code: EapCode::Response,
        identifier: 1,
        message: Some(EapMessage::Srp(SrpMessage::ClientKey(vec![0]))),
    });
    let failure = disconnect_packet(
        server.on_gre_packet(&invalid_client_key, 5),
        AuthenticationFailure::InvalidPublicValue,
    );
    assert_failure(&failure);

    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(config, OneVerifier { record }, FixedSecrets(16)).unwrap();
    let start = packet(client.start(10));
    let identity = packet(server.on_gre_packet(&start, 11));
    let identity_response = packet(client.on_gre_packet(&identity, 12));
    let challenge = packet(server.on_gre_packet(&identity_response, 13));
    let _client_key = packet(client.on_gre_packet(&challenge, 14));
    let invalid_server_key = authentication_packet(EapPacket {
        code: EapCode::Request,
        identifier: 2,
        message: Some(EapMessage::Srp(SrpMessage::ServerKey(vec![0]))),
    });
    let failure = disconnect_packet(
        client.on_gre_packet(&invalid_server_key, 15),
        AuthenticationFailure::InvalidPublicValue,
    );
    assert_failure(&failure);
}

#[test]
fn entropy_failure_sends_failure_without_opening_the_gate() {
    let config = AuthenticationConfig::default();
    let mut client = ClientSession::new(
        config.clone(),
        b"rist".to_vec(),
        b"mainprofile".to_vec(),
        FailingSecrets,
    )
    .unwrap();
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let identity_response = packet(client.on_gre_packet(&identity, 2));
    let challenge = packet(server.on_gre_packet(&identity_response, 3));
    let failure = disconnect_packet(
        client.on_gre_packet(&challenge, 4),
        AuthenticationFailure::EntropyFailure,
    );
    assert_failure(&failure);
    assert!(!client.allows_data());
    assert!(client.session_key().is_none());
}

#[test]
fn explicit_group_is_rejected_before_private_key_generation() {
    let config = AuthenticationConfig::default();
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let _identity_response = packet(client.on_gre_packet(&identity, 2));
    let explicit = authentication_packet(EapPacket {
        code: EapCode::Request,
        identifier: 1,
        message: Some(EapMessage::Srp(SrpMessage::Challenge {
            name: Vec::new(),
            salt: vec![1, 2, 3, 4],
            generator: Some(vec![2]),
            modulus: None,
        })),
    });
    let failure = disconnect_packet(
        client.on_gre_packet(&explicit, 3),
        AuthenticationFailure::UnsupportedGroup,
    );
    assert_failure(&failure);
}

#[test]
fn wrong_m2_and_logoff_clear_authenticated_state() {
    let config = AuthenticationConfig::default();
    let record = verifier(b"mainprofile");
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        config.clone(),
        OneVerifier {
            record: record.clone(),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let validator = drive_to_server_validator(&mut client, &mut server, 0);
    let mut frame = AuthenticationFrame::parse(&validator, config.limits).unwrap();
    let EapolPacket::Eap(EapPacket {
        message: Some(EapMessage::Srp(SrpMessage::ServerValidator { proof, .. })),
        ..
    }) = &mut frame.packet
    else {
        panic!("expected server validator");
    };
    *proof.first_mut().unwrap() ^= 1;
    let failure = disconnect_packet(
        client.on_gre_packet(&frame.serialize().unwrap(), 8),
        AuthenticationFailure::ProofMismatch,
    );
    assert_failure(&failure);
    assert!(!client.allows_data());

    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(config, OneVerifier { record }, FixedSecrets(16)).unwrap();
    complete_exchange(&mut client, &mut server, 10);
    assert!(client.allows_data());
    assert!(server.allows_data());
    let logoff = AuthenticationFrame::new(EapolPacket::Logoff, None)
        .serialize()
        .unwrap();
    assert_eq!(
        client.on_gre_packet(&logoff, 20),
        AuthenticationAction::Disconnect {
            response: None,
            reason: AuthenticationFailure::LoggedOff,
        }
    );
    assert_eq!(
        server.on_gre_packet(&logoff, 20),
        AuthenticationAction::Disconnect {
            response: None,
            reason: AuthenticationFailure::LoggedOff,
        }
    );
    assert!(!client.allows_data());
    assert!(!server.allows_data());
    assert!(client.session_key().is_none());
    assert!(server.session_key().is_none());
}

#[test]
fn non_authentication_gre_is_ignored_while_gate_is_closed() {
    let config = AuthenticationConfig::default();
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let header = GreHeader {
        checksum: None,
        key_or_nonce: None,
        sequence_number: None,
        h: false,
        rv: RistVersion::V2022,
        protocol_type: PROTOCOL_TYPE_IP,
    };
    let mut data = header.serialize();
    data.extend_from_slice(&[1, 2, 3]);
    assert_eq!(client.on_gre_packet(&data, 0), AuthenticationAction::Ignore);
    assert_eq!(server.on_gre_packet(&data, 0), AuthenticationAction::Ignore);
    assert!(!client.allows_data());
    assert!(!server.allows_data());
}

#[test]
fn unsupported_request_types_and_subtypes_receive_nak() {
    let config = AuthenticationConfig::default();
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut unsupported_type = authentication_packet(EapPacket {
        code: EapCode::Request,
        identifier: 44,
        message: Some(EapMessage::Identity(Vec::new())),
    });
    *unsupported_type.get_mut(12).unwrap() = 99;
    assert_nak(&packet(client.on_gre_packet(&unsupported_type, 0)), 44);

    let mut unsupported_subtype = authentication_packet(EapPacket {
        code: EapCode::Request,
        identifier: 45,
        message: Some(EapMessage::Srp(SrpMessage::Challenge {
            name: Vec::new(),
            salt: vec![1, 2, 3, 4],
            generator: None,
            modulus: None,
        })),
    });
    *unsupported_subtype.get_mut(13).unwrap() = 99;
    assert_nak(&packet(client.on_gre_packet(&unsupported_subtype, 1)), 45);
}

#[test]
fn credential_and_server_name_limits_fail_at_construction() {
    let mut config = AuthenticationConfig::default();
    config.limits.max_identity_bytes = 3;
    assert!(matches!(
        ClientSession::new(
            config.clone(),
            b"rist".to_vec(),
            b"pw".to_vec(),
            FixedSecrets(0),
        ),
        Err(AuthenticationFailure::InvalidConfiguration)
    ));

    config.limits.max_identity_bytes = 4;
    config.limits.max_password_bytes = 1;
    assert!(matches!(
        ClientSession::new(
            config.clone(),
            b"rist".to_vec(),
            b"pw".to_vec(),
            FixedSecrets(0),
        ),
        Err(AuthenticationFailure::InvalidConfiguration)
    ));

    config.server_name = vec![0; 256];
    assert!(matches!(
        ServerSession::new(config, NoVerifier, FixedSecrets(0)),
        Err(AuthenticationFailure::InvalidConfiguration)
    ));
}

#[test]
fn retransmits_are_stable_and_retry_exhaustion_resets_the_server() {
    let config = AuthenticationConfig::default();
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let first = packet(server.start(0));
    for deadline in [3_000, 6_000, 9_000] {
        assert_eq!(packet(server.on_tick(deadline)), first);
    }
    assert_eq!(
        server.on_tick(12_000),
        AuthenticationAction::Disconnect {
            response: None,
            reason: AuthenticationFailure::Timeout,
        }
    );
    assert_eq!(packet(server.start(12_001)), first_with_identifier(4));
}

#[test]
fn four_identifier_windows_wrap_without_overlap() {
    let config = AuthenticationConfig {
        max_retries: 0,
        initial_identifier: 254,
        ..AuthenticationConfig::default()
    };
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    assert_eq!(packet(server.start(0)), first_with_identifier(254));
    assert!(matches!(
        server.on_tick(3_000),
        AuthenticationAction::Disconnect {
            reason: AuthenticationFailure::Timeout,
            ..
        }
    ));
    assert_eq!(packet(server.start(3_001)), first_with_identifier(2));
}

#[test]
fn duplicate_requests_resend_cached_response_and_client_timeout_restarts() {
    let config = AuthenticationConfig::default();
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let first_response = packet(client.on_gre_packet(&identity, 2));
    assert_eq!(packet(client.on_gre_packet(&identity, 3)), first_response);
    let challenge = packet(server.on_gre_packet(&first_response, 4));
    let first_key = packet(client.on_gre_packet(&challenge, 5));
    assert_eq!(packet(client.on_gre_packet(&challenge, 6)), first_key);
    assert_eq!(packet(client.on_tick(3_005)), start);
}

#[test]
fn earlier_requests_replay_their_original_response_after_later_steps() {
    let config = AuthenticationConfig::default();
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let identity_response = packet(client.on_gre_packet(&identity, 2));
    let challenge = packet(server.on_gre_packet(&identity_response, 3));
    let client_key = packet(client.on_gre_packet(&challenge, 4));
    let server_key = packet(server.on_gre_packet(&client_key, 5));
    let client_validator = packet(client.on_gre_packet(&server_key, 6));

    assert_eq!(
        packet(client.on_gre_packet(&identity, 7)),
        identity_response
    );
    assert_eq!(packet(client.on_gre_packet(&challenge, 8)), client_key);
    assert_eq!(
        packet(client.on_gre_packet(&server_key, 9)),
        client_validator
    );
}

#[test]
fn out_of_order_requests_and_mismatched_responses_are_discarded() {
    let config = AuthenticationConfig::default();
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(
        config,
        OneVerifier {
            record: verifier(b"mainprofile"),
        },
        FixedSecrets(16),
    )
    .unwrap();
    let start = packet(client.start(0));
    let identity = packet(server.on_gre_packet(&start, 1));
    let identity_response = packet(client.on_gre_packet(&identity, 2));

    let wrong_identifier =
        authentication_packet(EapPacket::identity_response(99, b"rist".to_vec()));
    assert_eq!(
        server.on_gre_packet(&wrong_identifier, 3),
        AuthenticationAction::Ignore
    );
    assert_eq!(packet(server.on_tick(3_001)), identity);

    let premature_server_key = authentication_packet(EapPacket {
        code: EapCode::Request,
        identifier: 2,
        message: Some(EapMessage::Srp(SrpMessage::ServerKey(vec![2]))),
    });
    assert_eq!(
        client.on_gre_packet(&premature_server_key, 4),
        AuthenticationAction::Ignore
    );
    let challenge = packet(server.on_gre_packet(&identity_response, 5));
    assert!(matches!(
        client.on_gre_packet(&challenge, 6),
        AuthenticationAction::Send(_)
    ));
}

#[test]
fn reauthentication_respects_floor_replaces_key_and_fails_closed() {
    let config = AuthenticationConfig::default();
    let record = verifier(b"mainprofile");
    let mut client = make_client(config.clone(), b"mainprofile");
    let mut server = ServerSession::new(config, OneVerifier { record }, FixedSecrets(16)).unwrap();
    complete_exchange(&mut client, &mut server, 0);
    let old_key = *client.session_key().unwrap().as_bytes();
    assert_eq!(server.start(60_008), AuthenticationAction::Ignore);
    assert!(client.allows_data());
    let identity = packet(server.start(60_009));
    assert!(server.allows_data());
    let identity_response = packet(client.on_gre_packet(&identity, 60_010));
    let challenge = packet(server.on_gre_packet(&identity_response, 60_011));
    let client_key = packet(client.on_gre_packet(&challenge, 60_012));
    let server_key = packet(server.on_gre_packet(&client_key, 60_013));
    let client_validator = packet(client.on_gre_packet(&server_key, 60_014));
    let server_validator = packet(server.on_gre_packet(&client_validator, 60_015));
    let success = authenticated_packet(client.on_gre_packet(&server_validator, 60_016));
    assert_eq!(
        server.on_gre_packet(&success, 60_017),
        AuthenticationAction::Authenticated { response: None }
    );
    assert_ne!(client.session_key().unwrap().as_bytes(), &old_key);
    assert_eq!(client.session_key(), server.session_key());

    let identity = packet(server.start(120_017));
    let identity_response = packet(client.on_gre_packet(&identity, 120_018));
    let challenge = packet(server.on_gre_packet(&identity_response, 120_019));
    let client_key = packet(client.on_gre_packet(&challenge, 120_020));
    let server_key = packet(server.on_gre_packet(&client_key, 120_021));
    let mut client_validator = packet(client.on_gre_packet(&server_key, 120_022));
    let last = client_validator.last_mut().unwrap();
    *last ^= 1;
    let failure = disconnect_packet(
        server.on_gre_packet(&client_validator, 120_023),
        AuthenticationFailure::ProofMismatch,
    );
    assert_failure(&failure);
    assert!(!server.allows_data());
    assert!(server.session_key().is_none());
    assert!(matches!(
        client.on_gre_packet(&failure, 120_024),
        AuthenticationAction::Disconnect {
            reason: AuthenticationFailure::ProofMismatch,
            ..
        }
    ));
    assert!(!client.allows_data());
    assert!(matches!(
        client.start(120_025),
        AuthenticationAction::Send(_)
    ));
    assert!(matches!(
        server.start(120_025),
        AuthenticationAction::Send(_)
    ));
}

fn packet(action: AuthenticationAction) -> Vec<u8> {
    match action {
        AuthenticationAction::Send(bytes) => bytes,
        other => panic!("expected send action, got {other:?}"),
    }
}

fn authenticated_packet(action: AuthenticationAction) -> Vec<u8> {
    match action {
        AuthenticationAction::Authenticated {
            response: Some(bytes),
        } => bytes,
        other => panic!("expected authenticated response, got {other:?}"),
    }
}

fn disconnect_packet(action: AuthenticationAction, expected: AuthenticationFailure) -> Vec<u8> {
    match action {
        AuthenticationAction::Disconnect {
            response: Some(bytes),
            reason,
        } => {
            assert_eq!(reason, expected);
            bytes
        }
        other => panic!("expected disconnect response, got {other:?}"),
    }
}

fn verifier(password: &[u8]) -> VerifierRecord {
    VerifierRecord::from_password(b"rist", password.to_vec(), vec![0x72, 0xf9, 0xd5, 0x38]).unwrap()
}

fn make_client(config: AuthenticationConfig, password: &[u8]) -> ClientSession<FixedSecrets> {
    ClientSession::new(config, b"rist".to_vec(), password.to_vec(), FixedSecrets(0)).unwrap()
}

fn drive_to_client_validator<V: VerifierStore>(
    client: &mut ClientSession<FixedSecrets>,
    server: &mut ServerSession<FixedSecrets, V>,
    now_ms: u64,
) -> Vec<u8> {
    let start = packet(client.start(now_ms));
    let identity = packet(server.on_gre_packet(&start, now_ms + 1));
    let identity_response = packet(client.on_gre_packet(&identity, now_ms + 2));
    let challenge = packet(server.on_gre_packet(&identity_response, now_ms + 3));
    let client_key = packet(client.on_gre_packet(&challenge, now_ms + 4));
    let server_key = packet(server.on_gre_packet(&client_key, now_ms + 5));
    packet(client.on_gre_packet(&server_key, now_ms + 6))
}

fn drive_to_server_validator<V: VerifierStore>(
    client: &mut ClientSession<FixedSecrets>,
    server: &mut ServerSession<FixedSecrets, V>,
    now_ms: u64,
) -> Vec<u8> {
    let client_validator = drive_to_client_validator(client, server, now_ms);
    packet(server.on_gre_packet(&client_validator, now_ms + 7))
}

fn complete_exchange<V: VerifierStore>(
    client: &mut ClientSession<FixedSecrets>,
    server: &mut ServerSession<FixedSecrets, V>,
    now_ms: u64,
) {
    let server_validator = drive_to_server_validator(client, server, now_ms);
    let success = authenticated_packet(client.on_gre_packet(&server_validator, now_ms + 8));
    assert_eq!(
        server.on_gre_packet(&success, now_ms + 9),
        AuthenticationAction::Authenticated { response: None }
    );
}

fn authentication_packet(packet: EapPacket) -> Vec<u8> {
    AuthenticationFrame::new(EapolPacket::Eap(packet), None)
        .serialize()
        .unwrap()
}

fn assert_failure(bytes: &[u8]) {
    let frame = AuthenticationFrame::parse(bytes, AuthenticationLimits::default()).unwrap();
    assert!(matches!(
        frame.packet,
        EapolPacket::Eap(EapPacket {
            code: EapCode::Failure,
            message: None,
            ..
        })
    ));
}

fn assert_nak(bytes: &[u8], identifier: u8) {
    let frame = AuthenticationFrame::parse(bytes, AuthenticationLimits::default()).unwrap();
    assert!(matches!(
        frame.packet,
        EapolPacket::Eap(EapPacket {
            code: EapCode::Response,
            identifier: actual,
            message: Some(EapMessage::Nak),
        }) if actual == identifier
    ));
}

fn first_with_identifier(identifier: u8) -> Vec<u8> {
    authentication_packet(EapPacket {
        code: EapCode::Request,
        identifier,
        message: Some(EapMessage::Identity(Vec::new())),
    })
}
