//! Constant-time fixed-width SRP6a arithmetic for `TR-06-2:2024` Annex D.

use core::fmt;

use crypto_bigint::{
    CtEq, CtLt, U2048, U4096,
    modular::{ConstMontyForm, ConstMontyParams},
    zeroize::Zeroize,
};
use vaco_hash::sha2::{Digest, Sha256};

#[cfg(not(target_arch = "wasm32"))]
use crypto_bigint::Random;

crypto_bigint::const_monty_params!(
    AnnexDModulus,
    U2048,
    "AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC3192943DB56050A37329CBB4A099ED8193E0757767A13DD52312AB4B03310DCD7F48A9DA04FD50E8083969EDB767B0CF6095179A163AB3661A05FBD5FAAAE82918A9962F0B93B855F97993EC975EEAA80D740ADBF4FF747359D041D5C33EA71D281E446B14773BCA97B43A23FB801676BD207A436C6481F1D2B9078717461A5B9D32E688F87748544523B524B0D57D5EA77A2775D2ECFA032CFBDBF52FB3786160279004E57AE6AF874E7303CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8E9DBFBB694B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F9E4AFF73"
);

type AnnexDElement = ConstMontyForm<AnnexDModulus, { U2048::LIMBS }>;

/// A semantic failure in the Annex D SRP calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrpError {
    /// Salt is outside Annex D's 4..=255-byte range.
    InvalidSalt,
    /// A or B is zero, outside the group, padded, or too long.
    InvalidPublicValue,
    /// The peer selected an explicit group rather than the allowlisted default.
    UnsupportedGroup,
    /// The entropy provider failed or produced no valid sample in 128 attempts.
    EntropyFailure,
    /// A validator did not match in constant time.
    ProofMismatch,
}

impl fmt::Display for SrpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSalt => "invalid Annex D salt length",
            Self::InvalidPublicValue => "invalid Annex D public value",
            Self::UnsupportedGroup => "only the Annex D default 2048-bit group is supported",
            Self::EntropyFailure => "authentication entropy source failed",
            Self::ProofMismatch => "SRP validator mismatch",
        })
    }
}

impl std::error::Error for SrpError {}

/// A fallible source of secret bytes, injectable for wasm and deterministic tests.
pub trait SecretSource {
    /// Fills every output byte or returns without yielding partial entropy as usable state.
    fn fill_secret(&mut self, output: &mut [u8]) -> Result<(), SrpError>;
}

/// Native operating-system entropy for salts and private SRP exponents.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSecretSource;

#[cfg(not(target_arch = "wasm32"))]
impl SecretSource for SystemSecretSource {
    fn fill_secret(&mut self, output: &mut [u8]) -> Result<(), SrpError> {
        for chunk in output.chunks_mut(256) {
            let mut random = U2048::try_random().map_err(|_| SrpError::EntropyFailure)?;
            let encoded = random.to_be_bytes();
            let source = encoded.get(..chunk.len()).ok_or(SrpError::EntropyFailure)?;
            chunk.copy_from_slice(source);
            random.zeroize();
        }
        Ok(())
    }
}

/// A password database record containing only a salt and SRP verifier.
#[derive(Debug, Clone)]
pub struct VerifierRecord {
    salt: Vec<u8>,
    verifier: U2048,
}

impl VerifierRecord {
    /// Derives a verifier while taking ownership of, then zeroizing, the password bytes.
    pub fn from_password(
        identity: &[u8],
        mut password: Vec<u8>,
        salt: Vec<u8>,
    ) -> Result<Self, SrpError> {
        validate_salt(&salt)?;
        let x = derive_x(identity, &password, &salt);
        password.as_mut_slice().zeroize();
        let verifier = AnnexDElement::new(&U2048::from(2u8)).pow(&x).retrieve();
        Ok(Self { salt, verifier })
    }

    /// Generates a salt through the injected source before deriving the verifier.
    pub fn generate(
        identity: &[u8],
        password: Vec<u8>,
        salt_len: usize,
        source: &mut impl SecretSource,
    ) -> Result<Self, SrpError> {
        if !(4..=255).contains(&salt_len) {
            return Err(SrpError::InvalidSalt);
        }
        let mut salt = vec![0u8; salt_len];
        source.fill_secret(&mut salt)?;
        Self::from_password(identity, password, salt)
    }

    /// Returns the salt bytes carried in the Challenge.
    #[must_use]
    pub fn salt(&self) -> &[u8] {
        &self.salt
    }

    /// Returns the canonical unpadded verifier for persistence.
    #[must_use]
    pub fn verifier_bytes(&self) -> Vec<u8> {
        canonical(&self.verifier)
    }

    /// Restores a validated record from persistent canonical bytes.
    pub fn from_verifier_bytes(salt: Vec<u8>, verifier: &[u8]) -> Result<Self, SrpError> {
        validate_salt(&salt)?;
        let verifier = parse_public::<AnnexDModulus>(verifier)?;
        Ok(Self { salt, verifier })
    }

    pub(crate) fn fake(source: &mut impl SecretSource) -> Result<Self, SrpError> {
        let mut salt = vec![0u8; 32];
        source.fill_secret(&mut salt)?;
        let verifier = sample_scalar::<AnnexDModulus>(source)?;
        Ok(Self { salt, verifier })
    }
}

/// The independent 32-byte `K = SHA256(S)` output of Annex D.
pub struct SessionKey([u8; 32]);

impl SessionKey {
    /// Borrows K for PSK rotation without transferring secret ownership.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionKey([REDACTED])")
    }
}

impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).to_bool()
    }
}

impl Eq for SessionKey {}

impl Drop for SessionKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub(crate) struct ClientEphemeral {
    private: U2048,
    public: U2048,
}

impl Drop for ClientEphemeral {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}

pub(crate) struct ClientEvidence {
    pub key: SessionKey,
    pub m1: [u8; 32],
    pub expected_m2: [u8; 32],
}

impl ClientEvidence {
    pub(crate) fn verify_m2(&self, received: &[u8; 32]) -> Result<(), SrpError> {
        if self.expected_m2.ct_eq(received).to_bool() {
            Ok(())
        } else {
            Err(SrpError::ProofMismatch)
        }
    }
}

pub(crate) struct ServerEphemeral {
    private: U2048,
    client_public: U2048,
    server_public: U2048,
    verifier: U2048,
}

impl Drop for ServerEphemeral {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}

pub(crate) struct ServerEvidence {
    pub key: SessionKey,
    pub m2: [u8; 32],
}

pub(crate) fn require_default_group(
    generator: Option<&[u8]>,
    modulus: Option<&[u8]>,
) -> Result<(), SrpError> {
    if generator.is_none() && modulus.is_none() {
        Ok(())
    } else {
        Err(SrpError::UnsupportedGroup)
    }
}

pub(crate) fn begin_client(
    source: &mut impl SecretSource,
) -> Result<(ClientEphemeral, Vec<u8>), SrpError> {
    let private = sample_scalar::<AnnexDModulus>(source)?;
    let public = pow_g::<AnnexDModulus>(&private);
    let bytes = canonical(&public);
    Ok((ClientEphemeral { private, public }, bytes))
}

pub(crate) fn finish_client(
    ephemeral: ClientEphemeral,
    identity: &[u8],
    mut password: Vec<u8>,
    salt: &[u8],
    server_public: &[u8],
) -> Result<ClientEvidence, SrpError> {
    validate_salt(salt)?;
    let server_public = parse_public::<AnnexDModulus>(server_public)?;
    let x = derive_x(identity, &password, salt);
    password.as_mut_slice().zeroize();
    let k = multiplier::<AnnexDModulus>();
    let u = scrambling(&ephemeral.public, &server_public);
    let gx = ConstMontyForm::<AnnexDModulus, { U2048::LIMBS }>::new(&U2048::from(2u8)).pow(&x);
    let base = AnnexDElement::new(&server_public) - AnnexDElement::new(&k) * gx;
    let ux = u
        .resize::<{ U4096::LIMBS }>()
        .wrapping_mul(&x.resize::<{ U4096::LIMBS }>());
    let exponent = ephemeral
        .private
        .resize::<{ U4096::LIMBS }>()
        .wrapping_add(&ux);
    let mut shared = base.pow(&exponent).retrieve();
    let key_bytes = hash(&[&canonical(&shared)]);
    shared.zeroize();
    let m1 = proof_m1::<AnnexDModulus>(
        identity,
        salt,
        &ephemeral.public,
        &server_public,
        &key_bytes,
    );
    let expected_m2 = hash(&[&canonical(&ephemeral.public), &m1, &key_bytes]);
    Ok(ClientEvidence {
        key: SessionKey(key_bytes),
        m1,
        expected_m2,
    })
}

pub(crate) fn begin_server(
    record: VerifierRecord,
    client_public: &[u8],
    source: &mut impl SecretSource,
) -> Result<(ServerEphemeral, Vec<u8>), SrpError> {
    let client_public = parse_public::<AnnexDModulus>(client_public)?;
    let private = sample_scalar::<AnnexDModulus>(source)?;
    let k = multiplier::<AnnexDModulus>();
    let b = AnnexDElement::new(&k) * AnnexDElement::new(&record.verifier)
        + AnnexDElement::new(&U2048::from(2u8)).pow(&private);
    let server_public = b.retrieve();
    if server_public.ct_eq(&U2048::ZERO).to_bool() {
        return Err(SrpError::InvalidPublicValue);
    }
    let bytes = canonical(&server_public);
    Ok((
        ServerEphemeral {
            private,
            client_public,
            server_public,
            verifier: record.verifier,
        },
        bytes,
    ))
}

pub(crate) fn finish_server(
    ephemeral: ServerEphemeral,
    identity: &[u8],
    salt: &[u8],
    received_m1: &[u8; 32],
) -> Result<ServerEvidence, SrpError> {
    let u = scrambling(&ephemeral.client_public, &ephemeral.server_public);
    let base = AnnexDElement::new(&ephemeral.client_public)
        * AnnexDElement::new(&ephemeral.verifier).pow(&u);
    let mut shared = base.pow(&ephemeral.private).retrieve();
    let key_bytes = hash(&[&canonical(&shared)]);
    shared.zeroize();
    let expected = proof_m1::<AnnexDModulus>(
        identity,
        salt,
        &ephemeral.client_public,
        &ephemeral.server_public,
        &key_bytes,
    );
    if !expected.ct_eq(received_m1).to_bool() {
        return Err(SrpError::ProofMismatch);
    }
    let m2 = hash(&[&canonical(&ephemeral.client_public), &expected, &key_bytes]);
    Ok(ServerEvidence {
        key: SessionKey(key_bytes),
        m2,
    })
}

fn validate_salt(salt: &[u8]) -> Result<(), SrpError> {
    if (4..=255).contains(&salt.len()) {
        Ok(())
    } else {
        Err(SrpError::InvalidSalt)
    }
}

fn sample_scalar<M: ConstMontyParams<{ U2048::LIMBS }>>(
    source: &mut impl SecretSource,
) -> Result<U2048, SrpError> {
    let modulus = *M::PARAMS.modulus().as_ref();
    for _ in 0..128 {
        let mut bytes = [0u8; 256];
        source.fill_secret(&mut bytes)?;
        let value = U2048::from_be_slice(&bytes);
        bytes.zeroize();
        if !value.ct_eq(&U2048::ZERO).to_bool() && value.ct_lt(&modulus).to_bool() {
            return Ok(value);
        }
    }
    Err(SrpError::EntropyFailure)
}

fn parse_public<M: ConstMontyParams<{ U2048::LIMBS }>>(bytes: &[u8]) -> Result<U2048, SrpError> {
    if bytes.is_empty()
        || bytes.len() > 256
        || (bytes.len() > 1 && bytes.first().copied() == Some(0))
    {
        return Err(SrpError::InvalidPublicValue);
    }
    let mut padded = [0u8; 256];
    let start = padded
        .len()
        .checked_sub(bytes.len())
        .ok_or(SrpError::InvalidPublicValue)?;
    padded
        .get_mut(start..)
        .ok_or(SrpError::InvalidPublicValue)?
        .copy_from_slice(bytes);
    let value = U2048::from_be_slice(&padded);
    let modulus = *M::PARAMS.modulus().as_ref();
    if value.ct_eq(&U2048::ZERO).to_bool() || !value.ct_lt(&modulus).to_bool() {
        Err(SrpError::InvalidPublicValue)
    } else {
        Ok(value)
    }
}

fn canonical(value: &U2048) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let mut out: Vec<u8> = bytes
        .iter()
        .copied()
        .skip_while(|byte| *byte == 0)
        .collect();
    if out.is_empty() {
        out.push(0);
    }
    out
}

fn derive_x(identity: &[u8], password: &[u8], salt: &[u8]) -> U2048 {
    let inner = hash(&[identity, b":", password]);
    hash_to_uint(&hash(&[salt, &inner]))
}

fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
    }
    digest.finalize().into()
}

fn hash_to_uint(bytes: &[u8; 32]) -> U2048 {
    let mut padded = [0u8; 256];
    if let Some(tail) = padded.get_mut(224..) {
        tail.copy_from_slice(bytes);
    }
    U2048::from_be_slice(&padded)
}

fn pow_g<M: ConstMontyParams<{ U2048::LIMBS }>>(exponent: &U2048) -> U2048 {
    ConstMontyForm::<M, { U2048::LIMBS }>::new(&U2048::from(2u8))
        .pow(exponent)
        .retrieve()
}

fn multiplier<M: ConstMontyParams<{ U2048::LIMBS }>>() -> U2048 {
    let modulus = *M::PARAMS.modulus().as_ref();
    hash_to_uint(&hash(&[&canonical(&modulus), &[2]]))
}

fn scrambling(client_public: &U2048, server_public: &U2048) -> U2048 {
    hash_to_uint(&hash(&[
        &canonical(client_public),
        &canonical(server_public),
    ]))
}

fn proof_m1<M: ConstMontyParams<{ U2048::LIMBS }>>(
    identity: &[u8],
    salt: &[u8],
    client_public: &U2048,
    server_public: &U2048,
    key: &[u8; 32],
) -> [u8; 32] {
    let modulus = *M::PARAMS.modulus().as_ref();
    let n_hash = hash(&[&canonical(&modulus)]);
    let g_hash = hash(&[&[2]]);
    let mut xor = [0u8; 32];
    for ((out, left), right) in xor.iter_mut().zip(n_hash).zip(g_hash) {
        *out = left ^ right;
    }
    let identity_hash = hash(&[identity]);
    hash(&[
        &xor,
        &identity_hash,
        salt,
        &canonical(client_public),
        &canonical(server_public),
        key,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    crypto_bigint::const_monty_params!(
        ExampleModulus,
        U2048,
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000D66AAFE8E245F9AC245A199F62CE61AB8FA90A4D80C71CD2ADFD0B9DA163B29F2A34AFBDB3B1B5D0102559CE63D8B6E86B0AA59C14E79D4AA62D1748E4249DF3"
    );

    #[test]
    fn annex_d9_numeric_vector_matches_every_intermediate() {
        let n = *ExampleModulus::PARAMS.modulus().as_ref();
        let salt = from_hex("72F9D5383B7EB7599FB63028F47475B60A55F313D40E0BE023E026C97C0A2C32");
        let a = uint("138AB4045633AD14961CB1AD0720B1989104151C0708794491113302CCCC27D5");
        let b = uint("ED0D58FF861A1FC75A0829BEA5F1392D2B13AB2B05CBCD6ED1E71AAAD761E856");
        let x = derive_x(b"rist", b"mainprofile", &salt);
        let v = ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&U2048::from(2u8))
            .pow(&x)
            .retrieve();
        let client_public = pow_g::<ExampleModulus>(&a);
        let k = multiplier::<ExampleModulus>();
        let server_public = (ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&k)
            * ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&v)
            + ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&U2048::from(2u8)).pow(&b))
        .retrieve();
        let u = scrambling(&client_public, &server_public);
        let gx = ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&U2048::from(2u8)).pow(&x);
        let base = ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&server_public)
            - ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&k) * gx;
        let ux = u
            .resize::<{ U4096::LIMBS }>()
            .wrapping_mul(&x.resize::<{ U4096::LIMBS }>());
        let exponent = a.resize::<{ U4096::LIMBS }>().wrapping_add(&ux);
        let client_shared = base.pow(&exponent).retrieve();
        let server_shared =
            (ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&client_public)
                * ConstMontyForm::<ExampleModulus, { U2048::LIMBS }>::new(&v).pow(&u))
            .pow(&b)
            .retrieve();
        let key = hash(&[&canonical(&client_shared)]);
        let m1 = proof_m1::<ExampleModulus>(b"rist", &salt, &client_public, &server_public, &key);
        let m2 = hash(&[&canonical(&client_public), &m1, &key]);

        assert_eq!(
            hex(&canonical(&n)),
            "d66aafe8e245f9ac245a199f62ce61ab8fa90a4d80c71cd2adfd0b9da163b29f2a34afbdb3b1b5d0102559ce63d8b6e86b0aa59c14e79d4aa62d1748e4249df3"
        );
        assert_eq!(
            hex(&canonical(&x)),
            "850d72f3946ec76ba4a52097e6df990f88cfb9a40252b7f52bec2e0d20bfe892"
        );
        assert_eq!(
            hex(&canonical(&v)),
            "2e06fea163d6e9ff0fa7ed6c59233389d0dba0c08c0f72f6dad1e2a3d8b92a772f070439d1c11b87fa990d2daf04eb830cc77d61acc4b253297379cd8e6dc3af"
        );
        assert_eq!(
            hex(&canonical(&client_public)),
            "92c4cefb95a1ae2e576a252b19273fd4613f44fda4ac8cc84a089d5740756223943882bad34cb55f35139cddb60e0d19acd2b884cfb27f53c8ea969269abe014"
        );
        assert_eq!(
            hex(&canonical(&k)),
            "890d0ac9e42a7f909d3caa9a0ff115c52a1dc8ded10839ef9583c4e35ea76e78"
        );
        assert_eq!(
            hex(&canonical(&server_public)),
            "858cdc811b5eeaa7f58c12767d309ebd2df1d46f59ef5686052e6511cf853ca4e66910bdbd28cbeae2f2dee7f6bf3756757bd69e88d48c77b5371a82ef52ad84"
        );
        assert_eq!(
            hex(&canonical(&u)),
            "4c53609f8b4f9f6f534df35abfdd760e8eec1117eb01421a66a425c059789a94"
        );
        assert_eq!(client_shared, server_shared);
        assert_eq!(
            hex(&canonical(&client_shared)),
            "beaef3b089be9022135c8798c777c30609546c1c0f305186ef30070677f6ffc221eec9e2bfdd405fea6589a0d8bb54df447187c265218ab3333064587db7f27c"
        );
        assert_eq!(
            hex(&key),
            "d2270ab6b54f80d246e474f8dd76fc7deca3f49fbdf419e082dc989b38608c34"
        );
        assert_eq!(
            hex(&m1),
            "e28147c801bab9c37647c1ff4a29fa720e3f5676434fb85ea9a752cc1f9b1ad4"
        );
        assert_eq!(
            hex(&m2),
            "84f19797916fbdcab1321ca78b575b145b586150248afaa156361b8bcb139b32"
        );
    }

    #[test]
    fn default_group_rejects_noncanonical_public_values_and_custom_groups() {
        let modulus = canonical(AnnexDModulus::PARAMS.modulus().as_ref());
        assert_eq!(
            parse_public::<AnnexDModulus>(&[0]),
            Err(SrpError::InvalidPublicValue)
        );
        assert_eq!(
            parse_public::<AnnexDModulus>(&[0, 1]),
            Err(SrpError::InvalidPublicValue)
        );
        assert_eq!(
            parse_public::<AnnexDModulus>(&modulus),
            Err(SrpError::InvalidPublicValue)
        );
        let mut too_long = vec![1];
        too_long.resize(257, 0);
        assert_eq!(
            parse_public::<AnnexDModulus>(&too_long),
            Err(SrpError::InvalidPublicValue)
        );
        assert_eq!(
            require_default_group(Some(&[2]), None),
            Err(SrpError::UnsupportedGroup)
        );
        assert_eq!(
            require_default_group(None, Some(&modulus)),
            Err(SrpError::UnsupportedGroup)
        );
    }

    fn uint(value: &str) -> U2048 {
        let bytes = from_hex(value);
        let mut padded = [0u8; 256];
        let start = 256usize.saturating_sub(bytes.len());
        if let Some(tail) = padded.get_mut(start..) {
            tail.copy_from_slice(&bytes);
        }
        U2048::from_be_slice(&padded)
    }

    fn from_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let high = digit(pair.first().copied().unwrap_or_default());
                let low = digit(pair.get(1).copied().unwrap_or_default());
                (high << 4) | low
            })
            .collect()
    }

    fn digit(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            b'A'..=b'F' => value - b'A' + 10,
            _ => 0,
        }
    }

    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len().saturating_mul(2));
        for byte in bytes {
            out.push(char::from(DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        out
    }
}
