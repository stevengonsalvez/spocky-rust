pub mod base64_js;
pub mod channel;
pub mod js_json;
pub mod js_string;

use std::{error::Error, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use crypto_secretbox::{
    Key, Nonce, XSalsa20Poly1305,
    aead::{Aead, KeyInit},
};
use rand_core::{OsRng, RngCore};
use salsa20::{
    Key as SalsaKey,
    cipher::{consts::U10, generic_array::GenericArray},
    hsalsa,
};
use x25519_dalek::{PublicKey, StaticSecret};

pub const KEY_LENGTH: usize = 32;
pub const NONCE_LENGTH: usize = 24;
pub const AUTH_TAG_LENGTH: usize = 16;

pub type SharedKey = [u8; KEY_LENGTH];

#[derive(Clone, Eq, PartialEq)]
pub struct KeyPair {
    pub public_key: [u8; KEY_LENGTH],
    pub secret_key: [u8; KEY_LENGTH],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CryptoError {
    InvalidPublicKeyEncoding,
    InvalidPublicKeyLength,
    InvalidSecretKeyEncoding,
    InvalidSecretKeyLength,
    InvalidPeerPublicKeyLength,
    InvalidPeerPublicKey,
    InvalidSharedKeyLength,
    InvalidNonceLength,
    CiphertextBundleTooShort,
    EncryptionFailed,
    DecryptionFailed,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPublicKeyEncoding => formatter.write_str("Invalid public key encoding"),
            Self::InvalidPublicKeyLength => {
                formatter.write_str("Invalid public key length (expected 32)")
            }
            Self::InvalidSecretKeyEncoding => formatter.write_str("Invalid secret key encoding"),
            Self::InvalidSecretKeyLength => {
                formatter.write_str("Invalid secret key length (expected 32)")
            }
            Self::InvalidPeerPublicKeyLength => {
                formatter.write_str("Invalid peer public key length (expected 32)")
            }
            Self::InvalidPeerPublicKey => formatter.write_str("Invalid peer public key"),
            Self::InvalidSharedKeyLength => {
                formatter.write_str("Invalid shared key length (expected 32)")
            }
            Self::InvalidNonceLength => formatter.write_str("Invalid nonce length (expected 24)"),
            Self::CiphertextBundleTooShort => formatter.write_str("Ciphertext bundle too short"),
            Self::EncryptionFailed => formatter.write_str("Encryption failed"),
            Self::DecryptionFailed => formatter.write_str("Decryption failed"),
        }
    }
}

impl Error for CryptoError {}

#[must_use]
pub fn generate_key_pair() -> KeyPair {
    let mut secret_key = [0_u8; KEY_LENGTH];
    OsRng.fill_bytes(&mut secret_key);
    key_pair_from_secret(secret_key)
}

#[must_use]
pub fn key_pair_from_secret(secret_key: [u8; KEY_LENGTH]) -> KeyPair {
    let secret = StaticSecret::from(secret_key);
    let public_key = PublicKey::from(&secret).to_bytes();
    KeyPair {
        public_key,
        secret_key,
    }
}

/// Exports a 32-byte public key as canonical padded base64.
///
/// # Errors
///
/// Returns an error when the key is not exactly 32 bytes.
pub fn export_public_key(public_key: &[u8]) -> Result<String, CryptoError> {
    require_array::<KEY_LENGTH>(public_key).map_err(|()| CryptoError::InvalidPublicKeyLength)?;
    Ok(STANDARD.encode(public_key))
}

/// Imports a canonical padded base64 public key.
///
/// # Errors
///
/// Returns an error for malformed, noncanonical, or non-32-byte input.
pub fn import_public_key(encoded: &str) -> Result<[u8; KEY_LENGTH], CryptoError> {
    if !encoded.len().is_multiple_of(4) {
        return Err(CryptoError::InvalidPublicKeyEncoding);
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| CryptoError::InvalidPublicKeyEncoding)?;
    if STANDARD.encode(&bytes) != encoded {
        return Err(CryptoError::InvalidPublicKeyEncoding);
    }
    require_array(&bytes).map_err(|()| CryptoError::InvalidPublicKeyLength)
}

/// Exports a 32-byte secret key as padded base64.
///
/// # Errors
///
/// Returns an error when the key is not exactly 32 bytes.
pub fn export_secret_key(secret_key: &[u8]) -> Result<String, CryptoError> {
    require_array::<KEY_LENGTH>(secret_key).map_err(|()| CryptoError::InvalidSecretKeyLength)?;
    Ok(STANDARD.encode(secret_key))
}

/// Imports a padded base64 secret key.
///
/// # Errors
///
/// Returns an error for malformed or non-32-byte input.
pub fn import_secret_key(encoded: &str) -> Result<[u8; KEY_LENGTH], CryptoError> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| CryptoError::InvalidSecretKeyEncoding)?;
    require_array(&bytes).map_err(|()| CryptoError::InvalidSecretKeyLength)
}

/// Derives the `TweetNaCl` `box.before` key from a Curve25519 key pair.
///
/// # Errors
///
/// Returns an error for invalid key lengths or unsupported low-order peers.
pub fn derive_shared_key(
    our_secret_key: &[u8],
    peer_public_key: &[u8],
) -> Result<SharedKey, CryptoError> {
    let secret = require_array(our_secret_key).map_err(|()| CryptoError::InvalidSecretKeyLength)?;
    let peer =
        require_array(peer_public_key).map_err(|()| CryptoError::InvalidPeerPublicKeyLength)?;
    let raw_shared = StaticSecret::from(secret).diffie_hellman(&PublicKey::from(peer));
    if raw_shared.as_bytes().iter().all(|byte| *byte == 0) {
        return Err(CryptoError::InvalidPeerPublicKey);
    }

    let raw_key = SalsaKey::clone_from_slice(raw_shared.as_bytes());
    let zero_input = GenericArray::default();
    Ok(hsalsa::<U10>(&raw_key, &zero_input).into())
}

/// Encrypts bytes with a fresh random 24-byte nonce.
///
/// # Errors
///
/// Returns an error for an invalid shared key or encryption failure.
pub fn encrypt(shared_key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0_u8; NONCE_LENGTH];
    OsRng.fill_bytes(&mut nonce);
    encrypt_with_nonce(shared_key, &nonce, plaintext)
}

/// Encrypts bytes with a caller-provided nonce for deterministic fixtures.
///
/// # Errors
///
/// Returns an error for an invalid key or nonce length, or encryption failure.
pub fn encrypt_with_nonce(
    shared_key: &[u8],
    nonce: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let key = require_array::<KEY_LENGTH>(shared_key)
        .map_err(|()| CryptoError::InvalidSharedKeyLength)?;
    let nonce =
        require_array::<NONCE_LENGTH>(nonce).map_err(|()| CryptoError::InvalidNonceLength)?;
    let cipher = XSalsa20Poly1305::new(Key::from_slice(&key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| CryptoError::EncryptionFailed)?;
    let mut bundle = Vec::with_capacity(NONCE_LENGTH + ciphertext.len());
    bundle.extend_from_slice(&nonce);
    bundle.extend_from_slice(&ciphertext);
    Ok(bundle)
}

/// Decrypts a `[24-byte nonce][MAC-first ciphertext]` bundle.
///
/// # Errors
///
/// Returns an error for an invalid key, truncated bundle, or failed authentication.
pub fn decrypt(shared_key: &[u8], bundle: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let key = require_array::<KEY_LENGTH>(shared_key)
        .map_err(|()| CryptoError::InvalidSharedKeyLength)?;
    if bundle.len() < NONCE_LENGTH {
        return Err(CryptoError::CiphertextBundleTooShort);
    }
    let cipher = XSalsa20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(
            Nonce::from_slice(&bundle[..NONCE_LENGTH]),
            &bundle[NONCE_LENGTH..],
        )
        .map_err(|_| CryptoError::DecryptionFailed)
}

fn require_array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], ()> {
    bytes.try_into().map_err(|_| ())
}
