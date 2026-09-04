//! Bounded EAPOL and EAP-SRP-SHA256 framing from `TR-06-2:2024` Annex D.3.

use core::fmt;

/// Resource ceilings applied before an authentication packet is allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticationLimits {
    /// Maximum complete EAPOL packet size.
    pub max_packet_bytes: usize,
    /// Maximum Identity message or username size.
    pub max_identity_bytes: usize,
    /// Maximum configured password size accepted by the state machine.
    pub max_password_bytes: usize,
}

impl Default for AuthenticationLimits {
    fn default() -> Self {
        Self {
            max_packet_bytes: 4096,
            max_identity_bytes: 1024,
            max_password_bytes: 1024,
        }
    }
}

/// A rejected Annex D wire packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EapError {
    /// A configured resource ceiling was exceeded.
    LimitExceeded,
    /// A declared field runs past the received packet.
    Truncated,
    /// The EAPOL version is not Annex D's version 3.
    UnsupportedVersion,
    /// A length is impossible or cannot be represented on the wire.
    InvalidLength,
    /// The EAPOL packet type is reserved and must be silently discarded.
    ReservedEapolType,
    /// The EAP code is reserved and must be silently discarded.
    ReservedEapCode,
    /// A Request type is unsupported and should receive Nak.
    UnsupportedType,
    /// An SRP Request subtype is unsupported and should receive Nak.
    UnsupportedSubtype,
    /// Fields contradict the selected packet kind.
    InvalidMessage,
}

impl fmt::Display for EapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LimitExceeded => "authentication packet exceeds configured limit",
            Self::Truncated => "authentication packet is truncated",
            Self::UnsupportedVersion => "unsupported EAPOL version",
            Self::InvalidLength => "invalid authentication packet length",
            Self::ReservedEapolType => "reserved EAPOL packet type",
            Self::ReservedEapCode => "reserved EAP code",
            Self::UnsupportedType => "unsupported EAP request type",
            Self::UnsupportedSubtype => "unsupported EAP SRP subtype",
            Self::InvalidMessage => "invalid EAP message fields",
        })
    }
}

impl std::error::Error for EapError {}

/// An Annex D EAPOL packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EapolPacket {
    /// Carries a nested EAP packet.
    Eap(EapPacket),
    /// Begins an authentication exchange.
    Start,
    /// Terminates the authenticated relationship.
    Logoff,
}

impl EapolPacket {
    /// Parses one bounded EAPOL packet, ignoring trailing transport bytes.
    pub fn parse(data: &[u8], limits: AuthenticationLimits) -> Result<Self, EapError> {
        if data.len() > limits.max_packet_bytes {
            return Err(EapError::LimitExceeded);
        }
        if byte(data, 0)? != 3 {
            return Err(EapError::UnsupportedVersion);
        }
        let packet_type = byte(data, 1)?;
        let payload_len = usize::from(be_u16(data, 2)?);
        let end = 4usize
            .checked_add(payload_len)
            .ok_or(EapError::InvalidLength)?;
        let payload = data.get(4..end).ok_or(EapError::Truncated)?;
        match packet_type {
            0 => Ok(Self::Eap(EapPacket::parse(payload, limits)?)),
            1 if payload.is_empty() => Ok(Self::Start),
            2 if payload.is_empty() => Ok(Self::Logoff),
            1 | 2 => Err(EapError::InvalidLength),
            _ => Err(EapError::ReservedEapolType),
        }
    }

    /// Serializes with EAPOL version 3 and checked 16-bit lengths.
    pub fn serialize(&self) -> Result<Vec<u8>, EapError> {
        let (packet_type, payload) = match self {
            Self::Eap(packet) => (0u8, packet.serialize()?),
            Self::Start => (1, Vec::new()),
            Self::Logoff => (2, Vec::new()),
        };
        let payload_len = u16::try_from(payload.len()).map_err(|_| EapError::InvalidLength)?;
        let mut out = Vec::with_capacity(4usize.saturating_add(payload.len()));
        out.extend_from_slice(&[3, packet_type]);
        out.extend_from_slice(&payload_len.to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }
}

/// The role encoded in the EAP Code field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EapCode {
    /// Server-originated request.
    Request,
    /// Client-originated response.
    Response,
    /// Authentication or passphrase acknowledgement succeeded.
    Success,
    /// Authentication failed.
    Failure,
}

impl EapCode {
    const fn wire(self) -> u8 {
        match self {
            Self::Request => 1,
            Self::Response => 2,
            Self::Success => 3,
            Self::Failure => 4,
        }
    }
}

/// The typed data carried by an EAP Request or Response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EapMessage {
    /// Identity prompt or identity bytes, distinguished by [`EapPacket::code`].
    Identity(Vec<u8>),
    /// Nak naming the sole supported EAP method.
    Nak,
    /// Annex D's SHA256-SRP6a method.
    Srp(SrpMessage),
}

/// One typed SRP-SHA256 message from Annex D.3.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrpMessage {
    /// Server name, salt and optional explicit group from Figure 26.
    Challenge {
        /// Unauthenticated display name of the server.
        name: Vec<u8>,
        /// Password verifier salt, always 4..=255 bytes.
        salt: Vec<u8>,
        /// Explicit generator; absent means the default value 2.
        generator: Option<Vec<u8>>,
        /// Explicit prime modulus; absent means the Annex D default.
        modulus: Option<Vec<u8>>,
    },
    /// The client's unpadded public value A.
    ClientKey(Vec<u8>),
    /// The server's unpadded public value B.
    ServerKey(Vec<u8>),
    /// The client's M1 proof and PSK-selection bit.
    ClientValidator {
        use_session_key: bool,
        proof: [u8; 32],
    },
    /// The server's M2 proof and PSK-selection bit.
    ServerValidator {
        use_session_key: bool,
        proof: [u8; 32],
    },
    /// Optional post-authentication request for the current PSK passphrase.
    PassphraseRequest,
    /// Optional encrypted passphrase framing from Figure 32.
    PassphraseResponse {
        use_session_key: bool,
        aes_256: bool,
        encrypted: Vec<u8>,
    },
}

/// A complete EAP packet nested inside EAPOL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EapPacket {
    /// Request/Response/Success/Failure role.
    pub code: EapCode,
    /// Four-message exchange identifier.
    pub identifier: u8,
    /// Present only for Request and Response.
    pub message: Option<EapMessage>,
}

impl EapPacket {
    /// Constructs an EAP Success packet.
    #[must_use]
    pub const fn success(identifier: u8) -> Self {
        Self {
            code: EapCode::Success,
            identifier,
            message: None,
        }
    }

    /// Constructs an EAP Failure packet.
    #[must_use]
    pub const fn failure(identifier: u8) -> Self {
        Self {
            code: EapCode::Failure,
            identifier,
            message: None,
        }
    }

    /// Constructs an Identity Response from opaque username bytes.
    #[must_use]
    pub fn identity_response(identifier: u8, identity: Vec<u8>) -> Self {
        Self {
            code: EapCode::Response,
            identifier,
            message: Some(EapMessage::Identity(identity)),
        }
    }

    fn parse(data: &[u8], limits: AuthenticationLimits) -> Result<Self, EapError> {
        let code = match byte(data, 0)? {
            1 => EapCode::Request,
            2 => EapCode::Response,
            3 => EapCode::Success,
            4 => EapCode::Failure,
            _ => return Err(EapError::ReservedEapCode),
        };
        let identifier = byte(data, 1)?;
        let declared = usize::from(be_u16(data, 2)?);
        if declared < 4 {
            return Err(EapError::InvalidLength);
        }
        let packet = data.get(..declared).ok_or(EapError::Truncated)?;
        if matches!(code, EapCode::Success | EapCode::Failure) {
            if declared != 4 {
                return Err(EapError::InvalidLength);
            }
            return Ok(Self {
                code,
                identifier,
                message: None,
            });
        }
        let body = packet.get(4..).ok_or(EapError::Truncated)?;
        let message = parse_message(code, body, limits)?;
        Ok(Self {
            code,
            identifier,
            message: Some(message),
        })
    }

    fn serialize(&self) -> Result<Vec<u8>, EapError> {
        let data = match (self.code, &self.message) {
            (EapCode::Request | EapCode::Response, Some(message)) => {
                serialize_message(self.code, message)?
            }
            (EapCode::Success | EapCode::Failure, None) => Vec::new(),
            _ => return Err(EapError::InvalidMessage),
        };
        let len = 4usize
            .checked_add(data.len())
            .ok_or(EapError::InvalidLength)?;
        let wire_len = u16::try_from(len).map_err(|_| EapError::InvalidLength)?;
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(&[self.code.wire(), self.identifier]);
        out.extend_from_slice(&wire_len.to_be_bytes());
        out.extend_from_slice(&data);
        Ok(out)
    }
}

fn parse_message(
    code: EapCode,
    data: &[u8],
    limits: AuthenticationLimits,
) -> Result<EapMessage, EapError> {
    let kind = byte(data, 0)?;
    let body = data.get(1..).ok_or(EapError::Truncated)?;
    match kind {
        1 => {
            if body.len() > limits.max_identity_bytes {
                return Err(EapError::LimitExceeded);
            }
            Ok(EapMessage::Identity(body.to_vec()))
        }
        3 if code == EapCode::Response && body == [0x13] => Ok(EapMessage::Nak),
        3 => Err(EapError::InvalidMessage),
        0x13 => Ok(EapMessage::Srp(parse_srp(code, body)?)),
        _ => Err(EapError::UnsupportedType),
    }
}

fn parse_srp(code: EapCode, data: &[u8]) -> Result<SrpMessage, EapError> {
    let subtype = byte(data, 0)?;
    let body = data.get(1..).ok_or(EapError::Truncated)?;
    match (code, subtype) {
        (EapCode::Request, 1) => parse_challenge(body),
        (EapCode::Response, 1) => Ok(SrpMessage::ClientKey(parse_public(body)?)),
        (EapCode::Request, 2) => Ok(SrpMessage::ServerKey(parse_public(body)?)),
        (EapCode::Response, 2) => parse_validator(body, true),
        (EapCode::Request, 3) => parse_validator(body, false),
        (EapCode::Request, 0x10) if body.is_empty() => Ok(SrpMessage::PassphraseRequest),
        (EapCode::Response, 0x10) => parse_passphrase_response(body),
        (_, 1 | 2 | 3 | 0x10) => Err(EapError::InvalidMessage),
        _ => Err(EapError::UnsupportedSubtype),
    }
}

fn parse_challenge(data: &[u8]) -> Result<SrpMessage, EapError> {
    let name_len = usize::from(byte(data, 0)?);
    let name_end = 1usize
        .checked_add(name_len)
        .ok_or(EapError::InvalidLength)?;
    let name = data.get(1..name_end).ok_or(EapError::Truncated)?.to_vec();
    let salt_len = usize::from(byte(data, name_end)?);
    if salt_len < 4 {
        return Err(EapError::InvalidMessage);
    }
    let salt_start = name_end.checked_add(1).ok_or(EapError::InvalidLength)?;
    let salt_end = salt_start
        .checked_add(salt_len)
        .ok_or(EapError::InvalidLength)?;
    let salt = data
        .get(salt_start..salt_end)
        .ok_or(EapError::Truncated)?
        .to_vec();
    let generator_len = usize::from(byte(data, salt_end)?);
    let generator_start = salt_end.checked_add(1).ok_or(EapError::InvalidLength)?;
    let generator_end = generator_start
        .checked_add(generator_len)
        .ok_or(EapError::InvalidLength)?;
    let generator = if generator_len == 0 {
        None
    } else {
        Some(
            data.get(generator_start..generator_end)
                .ok_or(EapError::Truncated)?
                .to_vec(),
        )
    };
    let modulus_bytes = data.get(generator_end..).ok_or(EapError::Truncated)?;
    let modulus = if modulus_bytes.is_empty() {
        None
    } else {
        Some(modulus_bytes.to_vec())
    };
    Ok(SrpMessage::Challenge {
        name,
        salt,
        generator,
        modulus,
    })
}

fn parse_public(data: &[u8]) -> Result<Vec<u8>, EapError> {
    if data.is_empty() || (data.len() > 1 && data.first().copied() == Some(0)) {
        return Err(EapError::InvalidMessage);
    }
    Ok(data.to_vec())
}

fn parse_validator(data: &[u8], client: bool) -> Result<SrpMessage, EapError> {
    if data.len() != 36 {
        return Err(EapError::InvalidLength);
    }
    let use_session_key = byte(data, 3)? & 1 != 0;
    let proof: [u8; 32] = data
        .get(4..36)
        .ok_or(EapError::Truncated)?
        .try_into()
        .map_err(|_| EapError::InvalidLength)?;
    if client {
        Ok(SrpMessage::ClientValidator {
            use_session_key,
            proof,
        })
    } else {
        Ok(SrpMessage::ServerValidator {
            use_session_key,
            proof,
        })
    }
}

fn parse_passphrase_response(data: &[u8]) -> Result<SrpMessage, EapError> {
    let flags = byte(data, 0)?;
    let use_session_key = flags & 0x80 != 0;
    let aes_256 = flags & 0x40 != 0;
    let encrypted = data.get(1..).ok_or(EapError::Truncated)?.to_vec();
    if use_session_key && !encrypted.is_empty() {
        return Err(EapError::InvalidMessage);
    }
    Ok(SrpMessage::PassphraseResponse {
        use_session_key,
        aes_256,
        encrypted,
    })
}

fn serialize_message(code: EapCode, message: &EapMessage) -> Result<Vec<u8>, EapError> {
    match message {
        EapMessage::Identity(bytes) => {
            let mut out = Vec::with_capacity(bytes.len().saturating_add(1));
            out.push(1);
            out.extend_from_slice(bytes);
            Ok(out)
        }
        EapMessage::Nak if code == EapCode::Response => Ok(vec![3, 0x13]),
        EapMessage::Nak => Err(EapError::InvalidMessage),
        EapMessage::Srp(message) => serialize_srp(code, message),
    }
}

fn serialize_srp(code: EapCode, message: &SrpMessage) -> Result<Vec<u8>, EapError> {
    let (subtype, mut body) = match (code, message) {
        (
            EapCode::Request,
            SrpMessage::Challenge {
                name,
                salt,
                generator,
                modulus,
            },
        ) => (
            1,
            serialize_challenge(name, salt, generator.as_deref(), modulus.as_deref())?,
        ),
        (EapCode::Response, SrpMessage::ClientKey(value)) => (1, serialize_public(value)?),
        (EapCode::Request, SrpMessage::ServerKey(value)) => (2, serialize_public(value)?),
        (
            EapCode::Response,
            SrpMessage::ClientValidator {
                use_session_key,
                proof,
            },
        ) => (2, serialize_validator(*use_session_key, proof)),
        (
            EapCode::Request,
            SrpMessage::ServerValidator {
                use_session_key,
                proof,
            },
        ) => (3, serialize_validator(*use_session_key, proof)),
        (EapCode::Request, SrpMessage::PassphraseRequest) => (0x10, Vec::new()),
        (
            EapCode::Response,
            SrpMessage::PassphraseResponse {
                use_session_key,
                aes_256,
                encrypted,
            },
        ) => {
            if *use_session_key && !encrypted.is_empty() {
                return Err(EapError::InvalidMessage);
            }
            let mut data = Vec::with_capacity(encrypted.len().saturating_add(1));
            data.push((u8::from(*use_session_key) << 7) | (u8::from(*aes_256) << 6));
            data.extend_from_slice(encrypted);
            (0x10, data)
        }
        _ => return Err(EapError::InvalidMessage),
    };
    let mut out = Vec::with_capacity(body.len().saturating_add(2));
    out.extend_from_slice(&[0x13, subtype]);
    out.append(&mut body);
    Ok(out)
}

fn serialize_challenge(
    name: &[u8],
    salt: &[u8],
    generator: Option<&[u8]>,
    modulus: Option<&[u8]>,
) -> Result<Vec<u8>, EapError> {
    let name_len = u8::try_from(name.len()).map_err(|_| EapError::InvalidLength)?;
    if !(4..=255).contains(&salt.len()) {
        return Err(EapError::InvalidMessage);
    }
    let salt_len = u8::try_from(salt.len()).map_err(|_| EapError::InvalidLength)?;
    let generator = generator.unwrap_or_default();
    let generator_len = u8::try_from(generator.len()).map_err(|_| EapError::InvalidLength)?;
    let mut out = Vec::new();
    out.push(name_len);
    out.extend_from_slice(name);
    out.push(salt_len);
    out.extend_from_slice(salt);
    out.push(generator_len);
    out.extend_from_slice(generator);
    if let Some(modulus) = modulus {
        out.extend_from_slice(modulus);
    }
    Ok(out)
}

fn serialize_public(value: &[u8]) -> Result<Vec<u8>, EapError> {
    if value.is_empty() || (value.len() > 1 && value.first().copied() == Some(0)) {
        return Err(EapError::InvalidMessage);
    }
    Ok(value.to_vec())
}

fn serialize_validator(use_session_key: bool, proof: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(&[0, 0, 0, u8::from(use_session_key)]);
    out.extend_from_slice(proof);
    out
}

fn byte(data: &[u8], at: usize) -> Result<u8, EapError> {
    data.get(at).copied().ok_or(EapError::Truncated)
}

fn be_u16(data: &[u8], at: usize) -> Result<u16, EapError> {
    let end = at.checked_add(2).ok_or(EapError::InvalidLength)?;
    let bytes: [u8; 2] = data
        .get(at..end)
        .ok_or(EapError::Truncated)?
        .try_into()
        .map_err(|_| EapError::Truncated)?;
    Ok(u16::from_be_bytes(bytes))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn literal_eapol_layouts_match_annex_d_figures() {
        assert_eq!(EapolPacket::Start.serialize().unwrap(), [3, 1, 0, 0]);
        assert_eq!(
            EapolPacket::Eap(EapPacket::success(0x42))
                .serialize()
                .unwrap(),
            [3, 0, 0, 4, 3, 0x42, 0, 4]
        );
        assert_eq!(
            EapolPacket::Eap(EapPacket::identity_response(7, b"rist".to_vec()))
                .serialize()
                .unwrap(),
            [3, 0, 0, 9, 2, 7, 0, 9, 1, b'r', b'i', b's', b't']
        );
    }

    #[test]
    fn nested_lengths_and_resource_limits_are_enforced() {
        let limits = AuthenticationLimits::default();
        assert_eq!(
            EapolPacket::parse(&[3, 0, 0, 4, 3, 7, 0, 5], limits),
            Err(EapError::Truncated)
        );
        assert_eq!(
            EapolPacket::parse(&[3, 9, 0, 0], limits),
            Err(EapError::ReservedEapolType)
        );
        assert_eq!(
            EapolPacket::parse(&[3, 0, 0, 4, 9, 0, 0, 4], limits),
            Err(EapError::ReservedEapCode)
        );
        let tiny = AuthenticationLimits {
            max_packet_bytes: 7,
            ..limits
        };
        assert_eq!(
            EapolPacket::parse(&[3, 0, 0, 4, 3, 0, 0, 4], tiny),
            Err(EapError::LimitExceeded)
        );
    }

    #[test]
    fn challenge_salt_and_public_values_are_canonical() {
        let short_salt = EapPacket {
            code: EapCode::Request,
            identifier: 1,
            message: Some(EapMessage::Srp(SrpMessage::Challenge {
                name: Vec::new(),
                salt: vec![1, 2, 3],
                generator: None,
                modulus: None,
            })),
        };
        assert_eq!(
            EapolPacket::Eap(short_salt).serialize(),
            Err(EapError::InvalidMessage)
        );
        let padded = EapPacket {
            code: EapCode::Response,
            identifier: 2,
            message: Some(EapMessage::Srp(SrpMessage::ClientKey(vec![0, 1]))),
        };
        assert_eq!(
            EapolPacket::Eap(padded).serialize(),
            Err(EapError::InvalidMessage)
        );
    }

    #[test]
    fn every_typed_packet_round_trips() {
        let packets = [
            EapolPacket::Start,
            EapolPacket::Logoff,
            EapolPacket::Eap(EapPacket::identity_response(7, b"rist".to_vec())),
            EapolPacket::Eap(EapPacket {
                code: EapCode::Request,
                identifier: 8,
                message: Some(EapMessage::Srp(SrpMessage::Challenge {
                    name: b"server".to_vec(),
                    salt: vec![1, 2, 3, 4],
                    generator: None,
                    modulus: None,
                })),
            }),
            EapolPacket::Eap(EapPacket {
                code: EapCode::Response,
                identifier: 9,
                message: Some(EapMessage::Srp(SrpMessage::ClientValidator {
                    use_session_key: true,
                    proof: [0x55; 32],
                })),
            }),
            EapolPacket::Eap(EapPacket::failure(10)),
        ];
        for packet in packets {
            let bytes = packet.serialize().unwrap();
            assert_eq!(
                EapolPacket::parse(&bytes, AuthenticationLimits::default()).unwrap(),
                packet
            );
        }
    }
}
