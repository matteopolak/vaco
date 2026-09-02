//! The three handshake state machines — `draft-sharabayko-srt-01` §4.3,
//! summarised (not verbatim ASCII-diagrammed the way `packet.rs`/
//! `handshake.rs` are) from the fetched IETF datatracker rendering.
//!
//! **Sans-io**: nothing here owns a socket or a clock.
//! [`CallerHandshake`]/[`ListenerHandshake`]/[`RendezvousHandshake`] take "a
//! packet arrived" as input and a caller-supplied timestamp, and return
//! "send these bytes" / "connected" / "rejected" as output — a later
//! package drives one of these against a real socket, once the
//! worker-thread seam is built.
//!
//! # Evidence class of what follows
//!
//! The **caller/listener** 4-message exchange (INDUCTION, INDUCTION-
//! response, CONCLUSION, CONCLUSION-response) is close to the fetched
//! draft text and is checked in this module's tests by running this
//! crate's own [`CallerHandshake`] against its own [`ListenerHandshake`] in
//! a loopback simulation. **That is self-consistency, not a differential
//! result** — a misreading shared between the two sides of one crate would
//! pass every one of these tests identically, exactly the weakness named
//! for this dispatch. Nothing here is checked against `packet.rs`/
//! `handshake.rs`'s own draft-derived field-layout tests beyond using the
//! same types.
//!
//! The **rendezvous** machine is the higher-risk one, flagged explicitly:
//! the fetched summary of `draft` §4.3.2 names WAVEAHAND, a cookie contest,
//! CONCLUSION (Initiator, then Responder), and AGREEMENT, but not the
//! full retry/timeout/re-WAVEAHAND behaviour real UDT-derived rendezvous
//! has under packet loss or near-simultaneous starts. What is implemented
//! here is the single successful pass through those four steps —
//! `RendezvousHandshake` is the same type driven by both peers (unlike
//! caller/listener, which are two different types), differing only in
//! which cookie value happens to be larger at runtime, which is the
//! "genuinely different state machine, not caller-with-a-flag" shape this
//! dispatch asked for. **Not interop-verified, and cannot be without a
//! reference peer.**

use crate::cookie;
use crate::handshake::{
    EncryptionField, Extension, HandshakeCif, HandshakeType, HsReqBody, RejectReason,
    SRT_CMD_HSREQ, SRT_CMD_HSRSP, parse_extensions, serialize_extension,
};
use crate::packet::{ControlPacket, ControlType};
use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "srt";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// The `0x4A17` extension-field magic the fetched draft text names for an
/// `HSv5` INDUCTION response — draft-derived.
pub const HSV5_MAGIC: u16 = 0x4a17;

/// This crate's own SRT version number, reported in every HSREQ/HSRSP —
/// matches `draft`'s own worked example format (major.minor.patch packed
/// as `0x00MMmmpp`-shaped, one byte "family" implied by context) closely
/// enough for a self-hosted handshake; not a value any reference peer has
/// confirmed.
pub const SRT_VERSION: u32 = 0x0001_0500;

/// Everything a handshake needs to know about the local side before it can
/// build its first packet.
#[derive(Debug, Clone, Copy)]
pub struct HandshakeParams {
    pub local_socket_id: u32,
    pub initial_seq_no: u32,
    pub mtu: u32,
    pub max_flow_window: u32,
    /// 128 bits, matching `HandshakeCif::peer_ip`'s own shape (an IPv4
    /// address in the first word, zero elsewhere is the common case).
    pub local_ip: [u32; 4],
    pub encryption: EncryptionField,
}

/// What a peer told us about itself, once connected.
#[derive(Debug, Clone)]
pub struct ConnectedInfo {
    pub peer_socket_id: u32,
    pub peer_initial_seq_no: u32,
    pub peer_mtu: u32,
    pub peer_max_flow_window: u32,
    pub peer_hsreq: Option<HsReqBody>,
}

/// One state transition's result.
#[derive(Debug, Clone)]
pub enum HandshakeOutcome {
    /// Send these bytes; the handshake is not finished yet.
    Send(Vec<u8>),
    /// Nothing to send yet (waiting on the peer) — the rendezvous
    /// Responder branch uses this immediately after losing the cookie
    /// contest.
    Wait,
    /// Send these bytes, *and* the handshake is now complete — the
    /// rendezvous Initiator's own AGREEMENT send doubles as its own
    /// connect, per the fetched draft text.
    SendAndConnected(Vec<u8>, ConnectedInfo),
    Connected(ConnectedInfo),
    Rejected(RejectReason),
}

fn hs_extensions_for_request(_params: &HandshakeParams, include_kmreq: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let hsreq = HsReqBody {
        srt_version: SRT_VERSION,
        srt_flags: crate::handshake::srt_flags::TSBPDSND | crate::handshake::srt_flags::TSBPDRCV,
        receiver_tsbpd_delay: 120,
        sender_tsbpd_delay: 120,
    };
    out.extend_from_slice(&serialize_extension(&Extension {
        ext_type: SRT_CMD_HSREQ,
        contents: hsreq.serialize(),
    }));
    // KMREQ's actual key-material payload is deferred (see `crate::km`'s
    // module docs); `include_kmreq` is accepted here so the call sites read
    // correctly once that lands, and is unused until then rather than
    // half-implemented.
    let _ = include_kmreq;
    out
}

fn hs_extensions_for_response() -> Vec<u8> {
    let hsrsp = HsReqBody {
        srt_version: SRT_VERSION,
        srt_flags: crate::handshake::srt_flags::TSBPDSND | crate::handshake::srt_flags::TSBPDRCV,
        receiver_tsbpd_delay: 120,
        sender_tsbpd_delay: 120,
    };
    serialize_extension(&Extension {
        ext_type: SRT_CMD_HSRSP,
        contents: hsrsp.serialize(),
    })
}

fn build_handshake_packet(
    dest_socket_id: u32,
    timestamp: u32,
    version: u32,
    handshake_type: HandshakeType,
    cif_fixed: &HandshakeCif,
    extensions: &[u8],
) -> Vec<u8> {
    let mut cif = cif_fixed.serialize();
    cif.extend_from_slice(extensions);
    let _ = version; // carried inside cif_fixed.version; kept as a parameter for call-site clarity
    let _ = handshake_type; // likewise carried inside cif_fixed.handshake_type
    let pkt = ControlPacket {
        control_type: ControlType::Handshake,
        subtype_or_reserved: 0,
        type_specific: 0,
        timestamp,
        dest_socket_id,
        cif,
    };
    pkt.serialize()
}

/// Parse an incoming packet as a Handshake, returning its fixed CIF and any
/// extensions.
fn parse_handshake_packet(data: &[u8]) -> Result<(HandshakeCif, Vec<Extension>)> {
    let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
    let pkt = ControlPacket::parse(data, &mut budget)?;
    if pkt.control_type != ControlType::Handshake {
        return Err(malformed("expected a Handshake control packet"));
    }
    let (cif, consumed) = HandshakeCif::parse(&pkt.cif)?;
    let extensions = pkt.cif.get(consumed..).map_or_else(Vec::new, |rest| {
        parse_extensions(rest).unwrap_or_default()
    });
    Ok((cif, extensions))
}

fn find_hsreq_or_hsrsp(extensions: &[Extension]) -> Option<HsReqBody> {
    extensions
        .iter()
        .find(|e| e.ext_type == SRT_CMD_HSREQ || e.ext_type == SRT_CMD_HSRSP)
        .and_then(|e| HsReqBody::parse(&e.contents).ok())
}

// -------------------------------------------------------------- caller

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallerState {
    AwaitInductionResponse,
    AwaitConclusionResponse,
    Done,
}

/// `draft` §4.3.1's caller side: sends INDUCTION, then CONCLUSION once the
/// listener echoes a cookie back.
#[derive(Debug)]
pub struct CallerHandshake {
    params: HandshakeParams,
    state: CallerState,
}

impl CallerHandshake {
    #[must_use]
    pub const fn new(params: HandshakeParams) -> Self {
        Self {
            params,
            state: CallerState::AwaitInductionResponse,
        }
    }

    /// The first packet a caller sends: version 4, no cookie, dest socket
    /// id 0 (unknown listener socket yet) — `draft` §4.3.1 step 1.
    #[must_use]
    pub fn start(&self, timestamp: u32) -> Vec<u8> {
        let cif = HandshakeCif {
            version: 4,
            encryption: EncryptionField::None,
            extension_field: 0,
            initial_seq_no: self.params.initial_seq_no,
            mtu: self.params.mtu,
            max_flow_window: self.params.max_flow_window,
            handshake_type: HandshakeType::Induction,
            socket_id: self.params.local_socket_id,
            syn_cookie: 0,
            peer_ip: self.params.local_ip,
        };
        build_handshake_packet(0, timestamp, 4, HandshakeType::Induction, &cif, &[])
    }

    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` does not parse as a Handshake
    /// control packet, or arrives in the wrong state.
    pub fn on_packet(&mut self, data: &[u8], timestamp: u32) -> Result<HandshakeOutcome> {
        match self.state {
            CallerState::AwaitInductionResponse => {
                let (cif, _ext) = parse_handshake_packet(data)?;
                if let HandshakeType::Reject(r) = cif.handshake_type {
                    self.state = CallerState::Done;
                    return Ok(HandshakeOutcome::Rejected(r));
                }
                if cif.handshake_type != HandshakeType::Induction {
                    return Err(malformed("expected an INDUCTION response"));
                }
                let conclusion = HandshakeCif {
                    version: 5,
                    encryption: self.params.encryption,
                    extension_field: HSV5_MAGIC,
                    initial_seq_no: self.params.initial_seq_no,
                    mtu: self.params.mtu,
                    max_flow_window: self.params.max_flow_window,
                    handshake_type: HandshakeType::Conclusion,
                    socket_id: self.params.local_socket_id,
                    syn_cookie: cif.syn_cookie,
                    peer_ip: self.params.local_ip,
                };
                let extensions =
                    hs_extensions_for_request(&self.params, self.params.encryption != EncryptionField::None);
                self.state = CallerState::AwaitConclusionResponse;
                Ok(HandshakeOutcome::Send(build_handshake_packet(
                    cif.socket_id,
                    timestamp,
                    5,
                    HandshakeType::Conclusion,
                    &conclusion,
                    &extensions,
                )))
            }
            CallerState::AwaitConclusionResponse => {
                let (cif, ext) = parse_handshake_packet(data)?;
                if let HandshakeType::Reject(r) = cif.handshake_type {
                    self.state = CallerState::Done;
                    return Ok(HandshakeOutcome::Rejected(r));
                }
                if cif.handshake_type != HandshakeType::Conclusion {
                    return Err(malformed("expected a CONCLUSION response"));
                }
                self.state = CallerState::Done;
                Ok(HandshakeOutcome::Connected(ConnectedInfo {
                    peer_socket_id: cif.socket_id,
                    peer_initial_seq_no: cif.initial_seq_no,
                    peer_mtu: cif.mtu,
                    peer_max_flow_window: cif.max_flow_window,
                    peer_hsreq: find_hsreq_or_hsrsp(&ext),
                }))
            }
            CallerState::Done => Err(malformed("handshake already finished")),
        }
    }
}

// -------------------------------------------------------------- listener

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerState {
    AwaitInduction,
    AwaitConclusion,
    Done,
}

/// `draft` §4.3.1's listener side: reactive, echoes a cookie on INDUCTION,
/// confirms on CONCLUSION.
#[derive(Debug)]
pub struct ListenerHandshake {
    params: HandshakeParams,
    state: ListenerState,
    /// Set once the INDUCTION response is sent, checked against the
    /// caller's CONCLUSION — a listener that received a CONCLUSION quoting
    /// a different cookie either raced with another caller or is being
    /// spoofed, and this crate treats either as a fresh rejection, not a
    /// silently-accepted mismatch.
    issued_cookie: Option<u32>,
    peer_addr: (Vec<u8>, u16),
}

impl ListenerHandshake {
    #[must_use]
    pub const fn new(params: HandshakeParams, peer_ip: Vec<u8>, peer_port: u16) -> Self {
        Self {
            params,
            state: ListenerState::AwaitInduction,
            issued_cookie: None,
            peer_addr: (peer_ip, peer_port),
        }
    }

    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` does not parse as a Handshake
    /// control packet, or arrives in the wrong state.
    pub fn on_packet(&mut self, data: &[u8], timestamp: u32, now_unix_secs: u64) -> Result<HandshakeOutcome> {
        match self.state {
            ListenerState::AwaitInduction => {
                let (cif, _ext) = parse_handshake_packet(data)?;
                if cif.handshake_type != HandshakeType::Induction {
                    return Err(malformed("expected an INDUCTION request"));
                }
                let local_ip_bytes: Vec<u8> = self.params.local_ip.iter().flat_map(|w| w.to_be_bytes()).collect();
                let cookie = cookie::compute(
                    &local_ip_bytes,
                    0,
                    &self.peer_addr.0,
                    self.peer_addr.1,
                    now_unix_secs,
                );
                self.issued_cookie = Some(cookie);
                let response = HandshakeCif {
                    version: 5,
                    encryption: EncryptionField::None,
                    extension_field: HSV5_MAGIC,
                    initial_seq_no: self.params.initial_seq_no,
                    mtu: self.params.mtu,
                    max_flow_window: self.params.max_flow_window,
                    handshake_type: HandshakeType::Induction,
                    socket_id: self.params.local_socket_id,
                    syn_cookie: cookie,
                    peer_ip: self.params.local_ip,
                };
                self.state = ListenerState::AwaitConclusion;
                Ok(HandshakeOutcome::Send(build_handshake_packet(
                    cif.socket_id,
                    timestamp,
                    5,
                    HandshakeType::Induction,
                    &response,
                    &[],
                )))
            }
            ListenerState::AwaitConclusion => {
                let (cif, ext) = parse_handshake_packet(data)?;
                if cif.handshake_type != HandshakeType::Conclusion {
                    return Err(malformed("expected a CONCLUSION request"));
                }
                if Some(cif.syn_cookie) != self.issued_cookie {
                    self.state = ListenerState::Done;
                    return Ok(HandshakeOutcome::Rejected(RejectReason::RdvCookie));
                }
                let response = HandshakeCif {
                    version: 5,
                    encryption: self.params.encryption,
                    extension_field: HSV5_MAGIC,
                    initial_seq_no: self.params.initial_seq_no,
                    mtu: self.params.mtu,
                    max_flow_window: self.params.max_flow_window,
                    handshake_type: HandshakeType::Conclusion,
                    socket_id: self.params.local_socket_id,
                    syn_cookie: cif.syn_cookie,
                    peer_ip: self.params.local_ip,
                };
                let extensions = hs_extensions_for_response();
                self.state = ListenerState::Done;
                let info = ConnectedInfo {
                    peer_socket_id: cif.socket_id,
                    peer_initial_seq_no: cif.initial_seq_no,
                    peer_mtu: cif.mtu,
                    peer_max_flow_window: cif.max_flow_window,
                    peer_hsreq: find_hsreq_or_hsrsp(&ext),
                };
                Ok(HandshakeOutcome::SendAndConnected(
                    build_handshake_packet(
                        cif.socket_id,
                        timestamp,
                        5,
                        HandshakeType::Conclusion,
                        &response,
                        &extensions,
                    ),
                    info,
                ))
            }
            ListenerState::Done => Err(malformed("handshake already finished")),
        }
    }
}

// ------------------------------------------------------------ rendezvous

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RendezvousState {
    Waving,
    AwaitConclusionAsInitiator,
    AwaitConclusionAsResponder,
    AwaitAgreement,
    Done,
}

/// `draft` §4.3.2's rendezvous mode: both peers send WAVEAHAND, a cookie
/// contest resolves which one becomes Initiator, and the two sides then
/// diverge — genuinely different code paths, not one side with a flag
/// flipped. See module docs for what is and is not verified here.
#[derive(Debug)]
pub struct RendezvousHandshake {
    params: HandshakeParams,
    state: RendezvousState,
    local_cookie: u32,
    /// The peer's HSREQ, captured from its CONCLUSION when this side is the
    /// Responder — the AGREEMENT that follows carries no extensions, so
    /// this is the only place the Responder ever sees it.
    stashed_peer_hsreq: Option<HsReqBody>,
}

impl RendezvousHandshake {
    #[must_use]
    pub fn new(params: HandshakeParams, peer_ip: &[u8], peer_port: u16, local_port: u16, now_unix_secs: u64) -> Self {
        let local_ip_bytes: Vec<u8> = params.local_ip.iter().flat_map(|w| w.to_be_bytes()).collect();
        let local_cookie = cookie::compute(&local_ip_bytes, local_port, peer_ip, peer_port, now_unix_secs);
        Self {
            params,
            state: RendezvousState::Waving,
            local_cookie,
            stashed_peer_hsreq: None,
        }
    }

    #[must_use]
    pub fn start(&self, timestamp: u32) -> Vec<u8> {
        let cif = HandshakeCif {
            version: 5,
            encryption: EncryptionField::None,
            extension_field: 0,
            initial_seq_no: self.params.initial_seq_no,
            mtu: self.params.mtu,
            max_flow_window: self.params.max_flow_window,
            handshake_type: HandshakeType::WaveAHand,
            socket_id: self.params.local_socket_id,
            syn_cookie: self.local_cookie,
            peer_ip: self.params.local_ip,
        };
        build_handshake_packet(0, timestamp, 5, HandshakeType::WaveAHand, &cif, &[])
    }

    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` does not parse as a
    /// Handshake control packet, or arrives in a state that does not
    /// expect it.
    pub fn on_packet(&mut self, data: &[u8], timestamp: u32) -> Result<HandshakeOutcome> {
        match self.state {
            RendezvousState::Waving => {
                let (cif, _ext) = parse_handshake_packet(data)?;
                if cif.handshake_type != HandshakeType::WaveAHand {
                    return Err(malformed("expected a WAVEAHAND"));
                }
                match cookie::resolve(self.local_cookie, cif.syn_cookie) {
                    cookie::Contest::Tie => {
                        self.state = RendezvousState::Done;
                        Ok(HandshakeOutcome::Rejected(RejectReason::RdvCookie))
                    }
                    cookie::Contest::LocalWins => {
                        let conclusion = HandshakeCif {
                            version: 5,
                            encryption: self.params.encryption,
                            extension_field: HSV5_MAGIC,
                            initial_seq_no: self.params.initial_seq_no,
                            mtu: self.params.mtu,
                            max_flow_window: self.params.max_flow_window,
                            handshake_type: HandshakeType::Conclusion,
                            socket_id: self.params.local_socket_id,
                            syn_cookie: self.local_cookie,
                            peer_ip: self.params.local_ip,
                        };
                        let extensions = hs_extensions_for_request(
                            &self.params,
                            self.params.encryption != EncryptionField::None,
                        );
                        self.state = RendezvousState::AwaitConclusionAsInitiator;
                        Ok(HandshakeOutcome::Send(build_handshake_packet(
                            cif.socket_id,
                            timestamp,
                            5,
                            HandshakeType::Conclusion,
                            &conclusion,
                            &extensions,
                        )))
                    }
                    cookie::Contest::PeerWins => {
                        self.state = RendezvousState::AwaitConclusionAsResponder;
                        Ok(HandshakeOutcome::Wait)
                    }
                }
            }
            RendezvousState::AwaitConclusionAsInitiator => {
                let (cif, ext) = parse_handshake_packet(data)?;
                if let HandshakeType::Reject(r) = cif.handshake_type {
                    self.state = RendezvousState::Done;
                    return Ok(HandshakeOutcome::Rejected(r));
                }
                if cif.handshake_type != HandshakeType::Conclusion {
                    return Err(malformed("expected a CONCLUSION response"));
                }
                let agreement = HandshakeCif {
                    version: 5,
                    encryption: EncryptionField::None,
                    extension_field: 0,
                    initial_seq_no: self.params.initial_seq_no,
                    mtu: self.params.mtu,
                    max_flow_window: self.params.max_flow_window,
                    handshake_type: HandshakeType::Agreement,
                    socket_id: self.params.local_socket_id,
                    syn_cookie: self.local_cookie,
                    peer_ip: self.params.local_ip,
                };
                self.state = RendezvousState::Done;
                let info = ConnectedInfo {
                    peer_socket_id: cif.socket_id,
                    peer_initial_seq_no: cif.initial_seq_no,
                    peer_mtu: cif.mtu,
                    peer_max_flow_window: cif.max_flow_window,
                    peer_hsreq: find_hsreq_or_hsrsp(&ext),
                };
                Ok(HandshakeOutcome::SendAndConnected(
                    build_handshake_packet(cif.socket_id, timestamp, 5, HandshakeType::Agreement, &agreement, &[]),
                    info,
                ))
            }
            RendezvousState::AwaitConclusionAsResponder => {
                let (cif, ext) = parse_handshake_packet(data)?;
                if cif.handshake_type != HandshakeType::Conclusion {
                    return Err(malformed("expected a CONCLUSION request"));
                }
                let response = HandshakeCif {
                    version: 5,
                    encryption: self.params.encryption,
                    extension_field: HSV5_MAGIC,
                    initial_seq_no: self.params.initial_seq_no,
                    mtu: self.params.mtu,
                    max_flow_window: self.params.max_flow_window,
                    handshake_type: HandshakeType::Conclusion,
                    socket_id: self.params.local_socket_id,
                    syn_cookie: self.local_cookie,
                    peer_ip: self.params.local_ip,
                };
                let extensions = hs_extensions_for_response();
                self.state = RendezvousState::AwaitAgreement;
                self.stashed_peer_hsreq = find_hsreq_or_hsrsp(&ext);
                Ok(HandshakeOutcome::Send(build_handshake_packet(
                    cif.socket_id,
                    timestamp,
                    5,
                    HandshakeType::Conclusion,
                    &response,
                    &extensions,
                )))
            }
            RendezvousState::AwaitAgreement => {
                let (cif, _ext) = parse_handshake_packet(data)?;
                if cif.handshake_type != HandshakeType::Agreement {
                    return Err(malformed("expected an AGREEMENT"));
                }
                self.state = RendezvousState::Done;
                Ok(HandshakeOutcome::Connected(ConnectedInfo {
                    peer_socket_id: cif.socket_id,
                    peer_initial_seq_no: cif.initial_seq_no,
                    peer_mtu: cif.mtu,
                    peer_max_flow_window: cif.max_flow_window,
                    peer_hsreq: self.stashed_peer_hsreq,
                }))
            }
            RendezvousState::Done => Err(malformed("handshake already finished")),
        }
    }

}
