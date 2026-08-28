//! Loopback handshake simulations: this crate's own two implementations
//! talking to each other with no network involved.
//!
//! **Self-consistency only** — see `session.rs`'s module docs for why this
//! is real evidence of internal consistency and not evidence of matching
//! `draft-sharabayko-srt-01`: a shared misreading of the draft would pass
//! every test in this file identically. The draft-derived checks live in
//! `packet.rs`/`handshake.rs`'s own unit tests, against hand-built bytes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test code")]

use vaco_protocol_srt::handshake::EncryptionField;
use vaco_protocol_srt::session::{CallerHandshake, HandshakeParams, ListenerHandshake, HandshakeOutcome, RendezvousHandshake};

fn params(socket_id: u32) -> HandshakeParams {
    HandshakeParams {
        local_socket_id: socket_id,
        initial_seq_no: 1000,
        mtu: 1500,
        max_flow_window: 8192,
        local_ip: [0x7f00_0001, 0, 0, 0],
        encryption: EncryptionField::None,
    }
}

#[test]
fn caller_and_listener_reach_connected_in_four_messages() {
    let mut caller = CallerHandshake::new(params(1));
    let mut listener = ListenerHandshake::new(params(2), vec![127, 0, 0, 1], 4000);

    // 1. Caller -> Listener: INDUCTION.
    let induction = caller.start(0);

    // 2. Listener -> Caller: INDUCTION response.
    let listener_induction_reply = match listener.on_packet(&induction, 1, 1_000_000).unwrap() {
        HandshakeOutcome::Send(bytes) => bytes,
        other => panic!("expected Send, got {other:?}"),
    };

    // 3. Caller -> Listener: CONCLUSION.
    let caller_conclusion = match caller.on_packet(&listener_induction_reply, 2).unwrap() {
        HandshakeOutcome::Send(bytes) => bytes,
        other => panic!("expected Send, got {other:?}"),
    };

    // 4. Listener -> Caller: CONCLUSION response, and the listener is
    // connected the moment it sends it.
    let (listener_conclusion_reply, listener_info) =
        match listener.on_packet(&caller_conclusion, 3, 1_000_000).unwrap() {
            HandshakeOutcome::SendAndConnected(bytes, info) => (bytes, info),
            other => panic!("expected SendAndConnected, got {other:?}"),
        };
    assert_eq!(listener_info.peer_socket_id, 1);
    assert_eq!(listener_info.peer_initial_seq_no, 1000);

    // Caller receives the final CONCLUSION and connects too.
    let caller_info = match caller.on_packet(&listener_conclusion_reply, 4).unwrap() {
        HandshakeOutcome::Connected(info) => info,
        other => panic!("expected Connected, got {other:?}"),
    };
    assert_eq!(caller_info.peer_socket_id, 2);
    assert_eq!(caller_info.peer_initial_seq_no, 1000);
    assert!(caller_info.peer_hsreq.is_some(), "listener's HSRSP must be visible to the caller");
}

#[test]
fn rendezvous_reaches_connected_regardless_of_who_wins_the_cookie_contest() {
    // Distinct ports/addresses so the two sides compute different cookies
    // (see `cookie.rs`: a tie is rejected, per the draft's own rule as
    // quoted, which this test is not exercising).
    let mut alice = RendezvousHandshake::new(params(10), &[127, 0, 0, 1], 6001, 6000, 1_000_000);
    let mut bob = RendezvousHandshake::new(params(20), &[127, 0, 0, 1], 6000, 6001, 1_000_000);

    let alice_wave = alice.start(0);
    let bob_wave = bob.start(0);

    let alice_after_bobs_wave = alice.on_packet(&bob_wave, 1).unwrap();
    let bob_after_alices_wave = bob.on_packet(&alice_wave, 1).unwrap();

    // Exactly one side must win and send a CONCLUSION; the other must Wait.
    // Whichever one sent becomes `initiator` here; the other, `responder`.
    let (mut initiator, mut responder, winner_conclusion) =
        match (alice_after_bobs_wave, bob_after_alices_wave) {
            (HandshakeOutcome::Send(bytes), HandshakeOutcome::Wait) => (alice, bob, bytes),
            (HandshakeOutcome::Wait, HandshakeOutcome::Send(bytes)) => (bob, alice, bytes),
            other => panic!("expected exactly one Send and one Wait, got {other:?}"),
        };

    let responder_conclusion = match responder.on_packet(&winner_conclusion, 2).unwrap() {
        HandshakeOutcome::Send(bytes) => bytes,
        other => panic!("expected Send, got {other:?}"),
    };

    let (agreement, initiator_info) = match initiator.on_packet(&responder_conclusion, 3).unwrap() {
        HandshakeOutcome::SendAndConnected(bytes, info) => (bytes, info),
        other => panic!("expected SendAndConnected, got {other:?}"),
    };

    let responder_info = match responder.on_packet(&agreement, 4).unwrap() {
        HandshakeOutcome::Connected(info) => info,
        other => panic!("expected Connected, got {other:?}"),
    };

    assert_eq!(initiator_info.peer_initial_seq_no, 1000);
    assert_eq!(responder_info.peer_initial_seq_no, 1000);
}

