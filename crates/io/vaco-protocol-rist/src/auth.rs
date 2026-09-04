//! Sans-I/O client and server authentication state machines for Annex D.

use core::{fmt, mem};

use crypto_bigint::zeroize::Zeroize;

use crate::{
    eap::{
        AuthenticationLimits, EapCode, EapError, EapMessage, EapPacket, EapolPacket, SrpMessage,
    },
    gre::{AuthenticationFrame, AuthenticationFrameError},
    srp::{
        ClientEphemeral, ClientEvidence, SecretSource, ServerEphemeral, ServerEvidence, SessionKey,
        SrpError, VerifierRecord, begin_client, begin_server, finish_client, finish_server,
        require_default_group,
    },
};

/// Minimum interval between successful authentication and re-authentication.
pub const MIN_REAUTHENTICATION_INTERVAL_MS: u64 = 60_000;

/// Server behavior when an identity has no verifier record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownIdentityPolicy {
    /// Complete the expensive exchange with a fake record before returning Failure.
    PrivacyPreserving,
    /// Return Failure immediately after the Identity Response.
    FailFast,
}

/// Per-peer limits and retry policy for an Annex D authentication session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationConfig {
    /// Wire and credential allocation limits.
    pub limits: AuthenticationLimits,
    /// Display name carried in the unauthenticated Challenge.
    pub server_name: Vec<u8>,
    /// Milliseconds before a request is retransmitted or a client restarts.
    pub timeout_ms: u64,
    /// Number of byte-identical server retransmissions after the first request.
    pub max_retries: u8,
    /// First identifier in the server's four-message wrapping sequence.
    pub initial_identifier: u8,
    /// Advertise use of the derived key for this peer's outbound RIST traffic.
    pub use_session_key_as_psk: bool,
    /// Whether an unknown identity fails immediately or after fake SRP work.
    pub unknown_identity_policy: UnknownIdentityPolicy,
}

impl Default for AuthenticationConfig {
    fn default() -> Self {
        Self {
            limits: AuthenticationLimits::default(),
            server_name: Vec::new(),
            timeout_ms: 3_000,
            max_retries: 3,
            initial_identifier: 0,
            use_session_key_as_psk: false,
            unknown_identity_policy: UnknownIdentityPolicy::PrivacyPreserving,
        }
    }
}

impl AuthenticationConfig {
    fn validate(&self) -> Result<(), AuthenticationFailure> {
        if self.server_name.len() > usize::from(u8::MAX)
            || self.timeout_ms == 0
            || self.limits.max_packet_bytes < 8
        {
            Err(AuthenticationFailure::InvalidConfiguration)
        } else {
            Ok(())
        }
    }
}

/// Password-verifier lookup supplied by the embedding application.
pub trait VerifierStore {
    /// Returns an owned salt/verifier record, never a cleartext password.
    fn lookup(&self, identity: &[u8]) -> Option<VerifierRecord>;
}

impl<F> VerifierStore for F
where
    F: Fn(&[u8]) -> Option<VerifierRecord>,
{
    fn lookup(&self, identity: &[u8]) -> Option<VerifierRecord> {
        self(identity)
    }
}

/// A terminal or locally detected authentication failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationFailure {
    /// Session construction received impossible limits or an oversized credential.
    InvalidConfiguration,
    /// The peer sent a syntactically valid packet in an invalid state.
    ProtocolViolation,
    /// The fixed Annex D group was not selected.
    UnsupportedGroup,
    /// A public SRP value was zero, non-canonical, or outside the group.
    InvalidPublicValue,
    /// The entropy source failed or could not produce a scalar within its bound.
    EntropyFailure,
    /// A client or server validator did not match.
    ProofMismatch,
    /// No verifier exists for the supplied identity.
    UnknownIdentity,
    /// The configured retry budget was exhausted.
    Timeout,
    /// The peer explicitly logged off.
    LoggedOff,
}

impl fmt::Display for AuthenticationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "invalid authentication configuration",
            Self::ProtocolViolation => "authentication protocol violation",
            Self::UnsupportedGroup => "unsupported SRP group",
            Self::InvalidPublicValue => "invalid SRP public value",
            Self::EntropyFailure => "authentication entropy failure",
            Self::ProofMismatch => "authentication proof mismatch",
            Self::UnknownIdentity => "unknown authentication identity",
            Self::Timeout => "authentication retry budget exhausted",
            Self::LoggedOff => "peer logged off",
        })
    }
}

impl std::error::Error for AuthenticationFailure {}

impl From<SrpError> for AuthenticationFailure {
    fn from(error: SrpError) -> Self {
        match error {
            SrpError::InvalidSalt => Self::ProtocolViolation,
            SrpError::InvalidPublicValue => Self::InvalidPublicValue,
            SrpError::UnsupportedGroup => Self::UnsupportedGroup,
            SrpError::EntropyFailure => Self::EntropyFailure,
            SrpError::ProofMismatch => Self::ProofMismatch,
        }
    }
}

/// Output from one deterministic state-machine transition.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticationAction {
    /// Discard the input without changing transport state.
    Ignore,
    /// Send this complete cleartext GRE/EAPOL datagram.
    Send(Vec<u8>),
    /// Authentication completed; optionally send the final response first.
    Authenticated { response: Option<Vec<u8>> },
    /// Authentication failed; optionally send EAP Failure before disconnecting.
    Disconnect {
        response: Option<Vec<u8>>,
        reason: AuthenticationFailure,
    },
}

struct SecretBytes(Vec<u8>);

impl SecretBytes {
    fn clone_secret(&self) -> Vec<u8> {
        self.0.clone()
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.as_mut_slice().zeroize();
    }
}

struct CachedResponse {
    request: CachedRequest,
    response: Vec<u8>,
}

enum CachedRequest {
    Packet(EapPacket),
    Unsupported(Vec<u8>),
}

enum ClientState {
    Waiting,
    AwaitChallenge {
        identifier: u8,
    },
    AwaitServerKey {
        identifier: u8,
        salt: Vec<u8>,
        ephemeral: Box<ClientEphemeral>,
    },
    AwaitServerValidator {
        identifier: u8,
        evidence: ClientEvidence,
    },
}

/// One peer's Annex D client state, with no socket or clock ownership.
pub struct ClientSession<S> {
    config: AuthenticationConfig,
    identity: Vec<u8>,
    password: SecretBytes,
    source: S,
    state: ClientState,
    cached_responses: Vec<CachedResponse>,
    deadline_ms: Option<u64>,
    session_key: Option<SessionKey>,
    peer_uses_session_key_as_psk: bool,
    authenticated: bool,
    last_authenticated_ms: Option<u64>,
}

impl<S> fmt::Debug for ClientSession<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientSession")
            .field("config", &self.config)
            .field("identity", &self.identity)
            .field("password", &"[REDACTED]")
            .field("authenticated", &self.authenticated)
            .field("has_session_key", &self.session_key.is_some())
            .field(
                "peer_uses_session_key_as_psk",
                &self.peer_uses_session_key_as_psk,
            )
            .finish_non_exhaustive()
    }
}

impl<S: SecretSource> ClientSession<S> {
    /// Creates a bounded client. The password is zeroized when the session drops.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticationFailure::InvalidConfiguration`] when a limit is
    /// impossible or the supplied credential exceeds it.
    pub fn new(
        config: AuthenticationConfig,
        identity: Vec<u8>,
        password: Vec<u8>,
        source: S,
    ) -> Result<Self, AuthenticationFailure> {
        config.validate()?;
        if identity.len() > config.limits.max_identity_bytes
            || password.len() > config.limits.max_password_bytes
        {
            return Err(AuthenticationFailure::InvalidConfiguration);
        }
        Ok(Self {
            config,
            identity,
            password: SecretBytes(password),
            source,
            state: ClientState::Waiting,
            cached_responses: Vec::new(),
            deadline_ms: None,
            session_key: None,
            peer_uses_session_key_as_psk: false,
            authenticated: false,
            last_authenticated_ms: None,
        })
    }

    /// Sends EAPOL-Start. Too-early re-authentication requests are ignored.
    pub fn start(&mut self, now_ms: u64) -> AuthenticationAction {
        if !self.reauthentication_allowed(now_ms) {
            return AuthenticationAction::Ignore;
        }
        self.state = ClientState::Waiting;
        self.cached_responses.clear();
        self.deadline_ms = Some(now_ms.saturating_add(self.config.timeout_ms));
        match encode(EapolPacket::Start, self.config.limits) {
            Ok(bytes) => AuthenticationAction::Send(bytes),
            Err(reason) => self.disconnect(None, reason),
        }
    }

    /// Consumes one received GRE datagram and returns the next transport action.
    pub fn on_gre_packet(&mut self, data: &[u8], now_ms: u64) -> AuthenticationAction {
        if let Some(cached) = self.cached_responses.iter().find(|cached| {
            matches!(&cached.request, CachedRequest::Unsupported(request) if request.as_slice() == data)
        })
        {
            return AuthenticationAction::Send(cached.response.clone());
        }

        let frame = match AuthenticationFrame::parse(data, self.config.limits) {
            Ok(frame) => frame,
            Err(AuthenticationFrameError::Eap(
                EapError::UnsupportedType { identifier }
                | EapError::UnsupportedSubtype { identifier },
            )) => return self.send_nak(data, identifier, now_ms),
            Err(_) => return AuthenticationAction::Ignore,
        };
        match frame.packet {
            EapolPacket::Start => AuthenticationAction::Ignore,
            EapolPacket::Logoff => self.disconnect(None, AuthenticationFailure::LoggedOff),
            EapolPacket::Eap(packet) => {
                let begins_exchange = matches!(
                    (&self.state, packet.code, &packet.message),
                    (
                        ClientState::Waiting,
                        EapCode::Request,
                        Some(EapMessage::Identity(_))
                    )
                );
                if !begins_exchange
                    && let Some(cached) = self.cached_responses.iter().find(|cached| {
                        matches!(&cached.request, CachedRequest::Packet(request) if request == &packet)
                    })
                {
                    return AuthenticationAction::Send(cached.response.clone());
                }
                self.on_eap(packet, now_ms)
            }
        }
    }

    /// Restarts with a fresh EAPOL-Start when the current client deadline expires.
    pub fn on_tick(&mut self, now_ms: u64) -> AuthenticationAction {
        let Some(deadline) = self.deadline_ms else {
            return AuthenticationAction::Ignore;
        };
        if now_ms < deadline {
            return AuthenticationAction::Ignore;
        }
        self.state = ClientState::Waiting;
        self.cached_responses.clear();
        self.deadline_ms = Some(now_ms.saturating_add(self.config.timeout_ms));
        match encode(EapolPacket::Start, self.config.limits) {
            Ok(bytes) => AuthenticationAction::Send(bytes),
            Err(reason) => self.disconnect(None, reason),
        }
    }

    /// Whether the mutual validator exchange completed.
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Whether the caller may admit non-authentication GRE traffic for this peer.
    #[must_use]
    pub const fn allows_data(&self) -> bool {
        self.authenticated
    }

    /// Borrows the current session key without making an implicit secret copy.
    #[must_use]
    pub const fn session_key(&self) -> Option<&SessionKey> {
        self.session_key.as_ref()
    }

    /// Whether this client advertised K for traffic it sends to the server.
    #[must_use]
    pub const fn outbound_uses_session_key_as_psk(&self) -> bool {
        self.authenticated && self.config.use_session_key_as_psk
    }

    /// Whether the server advertised K for traffic it sends to this client.
    #[must_use]
    pub const fn inbound_uses_session_key_as_psk(&self) -> bool {
        self.authenticated && self.peer_uses_session_key_as_psk
    }

    fn on_eap(&mut self, packet: EapPacket, now_ms: u64) -> AuthenticationAction {
        if packet.code == EapCode::Failure {
            return self.disconnect(None, AuthenticationFailure::ProofMismatch);
        }

        let request = packet.clone();
        let state = mem::replace(&mut self.state, ClientState::Waiting);
        match (state, packet.code, packet.message) {
            (ClientState::Waiting, EapCode::Request, Some(EapMessage::Identity(_))) => {
                if !self.reauthentication_allowed(now_ms) {
                    return AuthenticationAction::Ignore;
                }
                self.cached_responses.clear();
                let identifier = packet.identifier;
                let response = EapPacket::identity_response(identifier, self.identity.clone());
                self.send_response(
                    request,
                    response,
                    ClientState::AwaitChallenge {
                        identifier: identifier.wrapping_add(1),
                    },
                    now_ms,
                )
            }
            (
                ClientState::AwaitChallenge { identifier },
                EapCode::Request,
                Some(EapMessage::Srp(SrpMessage::Challenge {
                    salt,
                    generator,
                    modulus,
                    ..
                })),
            ) if packet.identifier == identifier => {
                if let Err(error) = require_default_group(generator.as_deref(), modulus.as_deref())
                {
                    return self.failure_response(packet.identifier, error.into());
                }
                let (ephemeral, public) = match begin_client(&mut self.source) {
                    Ok(value) => value,
                    Err(error) => return self.failure_response(packet.identifier, error.into()),
                };
                let response = EapPacket {
                    code: EapCode::Response,
                    identifier,
                    message: Some(EapMessage::Srp(SrpMessage::ClientKey(public))),
                };
                self.send_response(
                    request,
                    response,
                    ClientState::AwaitServerKey {
                        identifier: identifier.wrapping_add(1),
                        salt,
                        ephemeral,
                    },
                    now_ms,
                )
            }
            (
                ClientState::AwaitServerKey {
                    identifier,
                    salt,
                    ephemeral,
                },
                EapCode::Request,
                Some(EapMessage::Srp(SrpMessage::ServerKey(public))),
            ) if packet.identifier == identifier => {
                let evidence = match finish_client(
                    ephemeral,
                    &self.identity,
                    self.password.clone_secret(),
                    &salt,
                    &public,
                ) {
                    Ok(value) => value,
                    Err(error) => return self.failure_response(packet.identifier, error.into()),
                };
                let response = EapPacket {
                    code: EapCode::Response,
                    identifier,
                    message: Some(EapMessage::Srp(SrpMessage::ClientValidator {
                        use_session_key: self.config.use_session_key_as_psk,
                        proof: evidence.m1,
                    })),
                };
                self.send_response(
                    request,
                    response,
                    ClientState::AwaitServerValidator {
                        identifier: identifier.wrapping_add(1),
                        evidence,
                    },
                    now_ms,
                )
            }
            (
                ClientState::AwaitServerValidator {
                    identifier,
                    evidence,
                },
                EapCode::Request,
                Some(EapMessage::Srp(SrpMessage::ServerValidator {
                    use_session_key,
                    proof,
                })),
            ) if packet.identifier == identifier => {
                if let Err(error) = evidence.verify_m2(&proof) {
                    return self.failure_response(packet.identifier, error.into());
                }
                let response_packet = EapPacket::success(identifier);
                let response = match encode(EapolPacket::Eap(response_packet), self.config.limits) {
                    Ok(bytes) => bytes,
                    Err(reason) => return self.disconnect(None, reason),
                };
                self.session_key = Some(evidence.key);
                self.peer_uses_session_key_as_psk = use_session_key;
                self.authenticated = true;
                self.last_authenticated_ms = Some(now_ms);
                self.deadline_ms = None;
                self.cache_response(CachedResponse {
                    request: CachedRequest::Packet(request),
                    response: response.clone(),
                });
                AuthenticationAction::Authenticated {
                    response: Some(response),
                }
            }
            (state, _, _) => {
                self.state = state;
                AuthenticationAction::Ignore
            }
        }
    }

    fn send_response(
        &mut self,
        request: EapPacket,
        packet: EapPacket,
        next_state: ClientState,
        now_ms: u64,
    ) -> AuthenticationAction {
        let response = match encode(EapolPacket::Eap(packet), self.config.limits) {
            Ok(bytes) => bytes,
            Err(reason) => return self.disconnect(None, reason),
        };
        self.state = next_state;
        self.deadline_ms = Some(now_ms.saturating_add(self.config.timeout_ms));
        self.cache_response(CachedResponse {
            request: CachedRequest::Packet(request),
            response: response.clone(),
        });
        AuthenticationAction::Send(response)
    }

    fn send_nak(&mut self, request: &[u8], identifier: u8, now_ms: u64) -> AuthenticationAction {
        let packet = EapPacket {
            code: EapCode::Response,
            identifier,
            message: Some(EapMessage::Nak),
        };
        let response = match encode(EapolPacket::Eap(packet), self.config.limits) {
            Ok(bytes) => bytes,
            Err(reason) => return self.disconnect(None, reason),
        };
        self.cache_response(CachedResponse {
            request: CachedRequest::Unsupported(request.to_vec()),
            response: response.clone(),
        });
        self.deadline_ms = Some(now_ms.saturating_add(self.config.timeout_ms));
        AuthenticationAction::Send(response)
    }

    fn cache_response(&mut self, response: CachedResponse) {
        if self.cached_responses.len() == 4 {
            self.cached_responses.remove(0);
        }
        self.cached_responses.push(response);
    }

    fn failure_response(
        &mut self,
        identifier: u8,
        reason: AuthenticationFailure,
    ) -> AuthenticationAction {
        let response = encode(
            EapolPacket::Eap(EapPacket::failure(identifier)),
            self.config.limits,
        )
        .ok();
        self.disconnect(response, reason)
    }

    fn disconnect(
        &mut self,
        response: Option<Vec<u8>>,
        reason: AuthenticationFailure,
    ) -> AuthenticationAction {
        self.state = ClientState::Waiting;
        self.cached_responses.clear();
        self.deadline_ms = None;
        self.session_key = None;
        self.peer_uses_session_key_as_psk = false;
        self.authenticated = false;
        AuthenticationAction::Disconnect { response, reason }
    }

    fn reauthentication_allowed(&self, now_ms: u64) -> bool {
        !self.authenticated
            || self
                .last_authenticated_ms
                .is_none_or(|last| now_ms >= last.saturating_add(MIN_REAUTHENTICATION_INTERVAL_MS))
    }
}

struct PendingRequest {
    bytes: Vec<u8>,
    deadline_ms: u64,
    retries: u8,
}

enum ServerState {
    Waiting,
    AwaitIdentity {
        identifier: u8,
    },
    AwaitClientKey {
        identifier: u8,
        identity: Vec<u8>,
        record: Box<VerifierRecord>,
        unknown_identity: bool,
    },
    AwaitClientValidator {
        identifier: u8,
        identity: Vec<u8>,
        salt: Vec<u8>,
        ephemeral: Box<ServerEphemeral>,
        unknown_identity: bool,
    },
    AwaitSuccess {
        identifier: u8,
        evidence: ServerEvidence,
        client_uses_session_key_as_psk: bool,
    },
}

/// One peer's Annex D server state, with injected verifier storage and entropy.
pub struct ServerSession<S, V> {
    config: AuthenticationConfig,
    source: S,
    verifiers: V,
    state: ServerState,
    pending: Option<PendingRequest>,
    next_identifier: u8,
    session_key: Option<SessionKey>,
    peer_uses_session_key_as_psk: bool,
    authenticated: bool,
    last_authenticated_ms: Option<u64>,
}

impl<S, V> fmt::Debug for ServerSession<S, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerSession")
            .field("config", &self.config)
            .field("next_identifier", &self.next_identifier)
            .field("authenticated", &self.authenticated)
            .field("has_session_key", &self.session_key.is_some())
            .field(
                "peer_uses_session_key_as_psk",
                &self.peer_uses_session_key_as_psk,
            )
            .finish_non_exhaustive()
    }
}

impl<S: SecretSource, V: VerifierStore> ServerSession<S, V> {
    /// Creates one bounded server-side peer session.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticationFailure::InvalidConfiguration`] when the
    /// server name, packet floor, or timeout is invalid.
    pub fn new(
        config: AuthenticationConfig,
        verifiers: V,
        source: S,
    ) -> Result<Self, AuthenticationFailure> {
        config.validate()?;
        let next_identifier = config.initial_identifier;
        Ok(Self {
            config,
            source,
            verifiers,
            state: ServerState::Waiting,
            pending: None,
            next_identifier,
            session_key: None,
            peer_uses_session_key_as_psk: false,
            authenticated: false,
            last_authenticated_ms: None,
        })
    }

    /// Starts an exchange without requiring the optional EAPOL-Start packet.
    pub fn start(&mut self, now_ms: u64) -> AuthenticationAction {
        if !matches!(self.state, ServerState::Waiting) {
            return AuthenticationAction::Ignore;
        }
        self.begin_exchange(now_ms)
    }

    /// Consumes one received GRE datagram and returns the next transport action.
    pub fn on_gre_packet(&mut self, data: &[u8], now_ms: u64) -> AuthenticationAction {
        let Ok(frame) = AuthenticationFrame::parse(data, self.config.limits) else {
            return AuthenticationAction::Ignore;
        };
        match frame.packet {
            EapolPacket::Start => {
                if let Some(pending) = &self.pending {
                    AuthenticationAction::Send(pending.bytes.clone())
                } else if matches!(self.state, ServerState::Waiting) {
                    self.begin_exchange(now_ms)
                } else {
                    AuthenticationAction::Ignore
                }
            }
            EapolPacket::Logoff => self.disconnect(None, AuthenticationFailure::LoggedOff),
            EapolPacket::Eap(packet) => self.on_eap(packet, now_ms),
        }
    }

    /// Retransmits cached request bytes or fails after the configured retry budget.
    pub fn on_tick(&mut self, now_ms: u64) -> AuthenticationAction {
        let Some(pending) = &mut self.pending else {
            return AuthenticationAction::Ignore;
        };
        if now_ms < pending.deadline_ms {
            return AuthenticationAction::Ignore;
        }
        if pending.retries < self.config.max_retries {
            pending.retries = pending.retries.saturating_add(1);
            pending.deadline_ms = now_ms.saturating_add(self.config.timeout_ms);
            return AuthenticationAction::Send(pending.bytes.clone());
        }
        self.disconnect(None, AuthenticationFailure::Timeout)
    }

    /// Whether the final client Success was received.
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Whether the caller may admit non-authentication GRE traffic for this peer.
    #[must_use]
    pub const fn allows_data(&self) -> bool {
        self.authenticated
    }

    /// Borrows the current session key without making an implicit secret copy.
    #[must_use]
    pub const fn session_key(&self) -> Option<&SessionKey> {
        self.session_key.as_ref()
    }

    /// Whether this server advertised K for traffic it sends to the client.
    #[must_use]
    pub const fn outbound_uses_session_key_as_psk(&self) -> bool {
        self.authenticated && self.config.use_session_key_as_psk
    }

    /// Whether the client advertised K for traffic it sends to this server.
    #[must_use]
    pub const fn inbound_uses_session_key_as_psk(&self) -> bool {
        self.authenticated && self.peer_uses_session_key_as_psk
    }

    fn begin_exchange(&mut self, now_ms: u64) -> AuthenticationAction {
        if !self.reauthentication_allowed(now_ms) {
            return AuthenticationAction::Ignore;
        }
        let identifier = self.next_identifier;
        self.next_identifier = self.next_identifier.wrapping_add(4);
        let request = EapPacket {
            code: EapCode::Request,
            identifier,
            message: Some(EapMessage::Identity(Vec::new())),
        };
        self.send_request(request, ServerState::AwaitIdentity { identifier }, now_ms)
    }

    fn on_eap(&mut self, packet: EapPacket, now_ms: u64) -> AuthenticationAction {
        if packet.code == EapCode::Failure {
            return self.disconnect(None, AuthenticationFailure::ProofMismatch);
        }
        let state = mem::replace(&mut self.state, ServerState::Waiting);
        match (state, packet.code, packet.message) {
            (
                ServerState::AwaitIdentity { identifier },
                EapCode::Response,
                Some(EapMessage::Identity(identity)),
            ) if packet.identifier == identifier => {
                self.pending = None;
                let (record, unknown_identity) = match self.verifiers.lookup(&identity) {
                    Some(record) => (record, false),
                    None if self.config.unknown_identity_policy
                        == UnknownIdentityPolicy::FailFast =>
                    {
                        return self
                            .failure_response(identifier, AuthenticationFailure::UnknownIdentity);
                    }
                    None => match VerifierRecord::fake(&mut self.source) {
                        Ok(record) => (record, true),
                        Err(error) => return self.disconnect(None, error.into()),
                    },
                };
                let challenge_identifier = identifier.wrapping_add(1);
                let request = EapPacket {
                    code: EapCode::Request,
                    identifier: challenge_identifier,
                    message: Some(EapMessage::Srp(SrpMessage::Challenge {
                        name: self.config.server_name.clone(),
                        salt: record.salt().to_vec(),
                        generator: None,
                        modulus: None,
                    })),
                };
                self.send_request(
                    request,
                    ServerState::AwaitClientKey {
                        identifier: challenge_identifier,
                        identity,
                        record: Box::new(record),
                        unknown_identity,
                    },
                    now_ms,
                )
            }
            (
                ServerState::AwaitClientKey {
                    identifier,
                    identity,
                    record,
                    unknown_identity,
                },
                EapCode::Response,
                Some(EapMessage::Srp(SrpMessage::ClientKey(public))),
            ) if packet.identifier == identifier => {
                self.pending = None;
                let salt = record.salt().to_vec();
                let (ephemeral, public) = match begin_server(record, &public, &mut self.source) {
                    Ok(value) => value,
                    Err(error) => return self.failure_response(identifier, error.into()),
                };
                let server_key_identifier = identifier.wrapping_add(1);
                let request = EapPacket {
                    code: EapCode::Request,
                    identifier: server_key_identifier,
                    message: Some(EapMessage::Srp(SrpMessage::ServerKey(public))),
                };
                self.send_request(
                    request,
                    ServerState::AwaitClientValidator {
                        identifier: server_key_identifier,
                        identity,
                        salt,
                        ephemeral,
                        unknown_identity,
                    },
                    now_ms,
                )
            }
            (
                ServerState::AwaitClientValidator {
                    identifier,
                    identity,
                    salt,
                    ephemeral,
                    unknown_identity,
                },
                EapCode::Response,
                Some(EapMessage::Srp(SrpMessage::ClientValidator {
                    use_session_key,
                    proof,
                })),
            ) if packet.identifier == identifier => {
                self.pending = None;
                let evidence = finish_server(ephemeral, &identity, &salt, &proof);
                if unknown_identity {
                    drop(evidence);
                    return self
                        .failure_response(identifier, AuthenticationFailure::UnknownIdentity);
                }
                let evidence = match evidence {
                    Ok(evidence) => evidence,
                    Err(error) => return self.failure_response(identifier, error.into()),
                };
                let validator_identifier = identifier.wrapping_add(1);
                let request = EapPacket {
                    code: EapCode::Request,
                    identifier: validator_identifier,
                    message: Some(EapMessage::Srp(SrpMessage::ServerValidator {
                        use_session_key: self.config.use_session_key_as_psk,
                        proof: evidence.m2,
                    })),
                };
                self.send_request(
                    request,
                    ServerState::AwaitSuccess {
                        identifier: validator_identifier,
                        evidence,
                        client_uses_session_key_as_psk: use_session_key,
                    },
                    now_ms,
                )
            }
            (
                ServerState::AwaitSuccess {
                    identifier,
                    evidence,
                    client_uses_session_key_as_psk,
                },
                EapCode::Success,
                None,
            ) if packet.identifier == identifier => {
                self.pending = None;
                self.session_key = Some(evidence.key);
                self.peer_uses_session_key_as_psk = client_uses_session_key_as_psk;
                self.authenticated = true;
                self.last_authenticated_ms = Some(now_ms);
                AuthenticationAction::Authenticated { response: None }
            }
            (state, _, _) => {
                self.state = state;
                AuthenticationAction::Ignore
            }
        }
    }

    fn send_request(
        &mut self,
        packet: EapPacket,
        next_state: ServerState,
        now_ms: u64,
    ) -> AuthenticationAction {
        let bytes = match encode(EapolPacket::Eap(packet), self.config.limits) {
            Ok(bytes) => bytes,
            Err(reason) => return self.disconnect(None, reason),
        };
        self.state = next_state;
        self.pending = Some(PendingRequest {
            bytes: bytes.clone(),
            deadline_ms: now_ms.saturating_add(self.config.timeout_ms),
            retries: 0,
        });
        AuthenticationAction::Send(bytes)
    }

    fn failure_response(
        &mut self,
        identifier: u8,
        reason: AuthenticationFailure,
    ) -> AuthenticationAction {
        let response = encode(
            EapolPacket::Eap(EapPacket::failure(identifier)),
            self.config.limits,
        )
        .ok();
        self.disconnect(response, reason)
    }

    fn disconnect(
        &mut self,
        response: Option<Vec<u8>>,
        reason: AuthenticationFailure,
    ) -> AuthenticationAction {
        self.state = ServerState::Waiting;
        self.pending = None;
        self.session_key = None;
        self.peer_uses_session_key_as_psk = false;
        self.authenticated = false;
        AuthenticationAction::Disconnect { response, reason }
    }

    fn reauthentication_allowed(&self, now_ms: u64) -> bool {
        !self.authenticated
            || self
                .last_authenticated_ms
                .is_none_or(|last| now_ms >= last.saturating_add(MIN_REAUTHENTICATION_INTERVAL_MS))
    }
}

fn encode(
    packet: EapolPacket,
    limits: AuthenticationLimits,
) -> Result<Vec<u8>, AuthenticationFailure> {
    let eapol_len = packet
        .serialize()
        .map_err(|_| AuthenticationFailure::ProtocolViolation)?
        .len();
    if eapol_len > limits.max_packet_bytes {
        return Err(AuthenticationFailure::ProtocolViolation);
    }
    AuthenticationFrame::new(packet, None)
        .serialize()
        .map_err(|_| AuthenticationFailure::ProtocolViolation)
}
