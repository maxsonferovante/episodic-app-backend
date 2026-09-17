//! Pure-Rust Fernet-compatible token crypto (no OpenSSL, musl-safe).
//!
//! The wire format is byte-for-byte identical to
//! [the Fernet spec](https://github.com/fernet/spec), so tokens interoperate
//! with the `fernet` Rust crate and Python `cryptography`:
//!
//! ```text
//! base64url(0x80 || timestamp_be_u64 || iv16 || AES128-CBC(PKCS7) || HMAC-SHA256)
//! ```
//!
//! The 32-byte key splits in half: first 16 bytes sign (HMAC-SHA256), last 16
//! encrypt (AES-128-CBC). Keys are accepted as base64url with or without
//! padding.

use aes::Aes128;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE},
    Engine as _,
};
use cbc::{Decryptor, Encryptor};
use cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type Aes128CbcEnc = Encryptor<Aes128>;
type Aes128CbcDec = Decryptor<Aes128>;
type HmacSha256 = Hmac<Sha256>;

const VERSION: u8 = 0x80;
const KEY_LEN: usize = 32;
const IV_LEN: usize = 16;
const MAC_LEN: usize = 32;
/// Rejects tokens stamped more than this far ahead of now.
const MAX_CLOCK_SKEW_SECS: u64 = 60;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid key: expected 32 base64url-encoded bytes")]
    InvalidKey,
    #[error("invalid token")]
    InvalidToken,
    #[error("token expired")]
    Expired,
    #[error("token timestamp is too far in the future")]
    ClockSkew,
}

/// Decode a base64url (or standard base64) key, tolerating missing padding.
pub fn decode_key(key_b64: &str) -> Result<[u8; KEY_LEN], Error> {
    let raw = b64_decode(key_b64.trim())?;
    raw.try_into().map_err(|_| Error::InvalidKey)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, Error> {
    let mut padded = s.to_string();
    if !padded.len().is_multiple_of(4) {
        padded.extend(std::iter::repeat_n('=', 4 - padded.len() % 4));
    }
    URL_SAFE
        .decode(&padded)
        .or_else(|_| STANDARD.decode(&padded))
        .map_err(|_| Error::InvalidToken)
}

/// Seal `plaintext` with a fresh random IV and current timestamp.
pub fn encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> String {
    use rand::RngCore;
    let mut iv = [0u8; IV_LEN];
    rand::rng().fill_bytes(&mut iv);
    let now = std::time::SystemTime::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_secs())
        .unwrap_or(0);
    encrypt_with(key, plaintext, now, iv)
}

/// Deterministic seal (fixed timestamp + IV) — used by tests against the
/// official spec vector.
pub fn encrypt_with(
    key: &[u8; KEY_LEN],
    plaintext: &[u8],
    timestamp: u64,
    iv: [u8; IV_LEN],
) -> String {
    let ct = Aes128CbcEnc::new_from_slices(&key[16..], &iv)
        .expect("16-byte halves of a 32-byte key and 16-byte iv")
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext);

    let mut data = Vec::with_capacity(1 + 8 + IV_LEN + ct.len() + MAC_LEN);
    data.push(VERSION);
    data.extend_from_slice(&timestamp.to_be_bytes());
    data.extend_from_slice(&iv);
    data.extend_from_slice(&ct);

    let mut mac = HmacSha256::new_from_slice(&key[..16]).expect("fixed-size key");
    mac.update(&data);
    let tag = mac.finalize().into_bytes();
    data.extend_from_slice(&tag);

    URL_SAFE.encode(&data)
}

/// Open a token issued within the last `ttl_secs` seconds.
pub fn decrypt(key: &[u8; KEY_LEN], token: &str, ttl_secs: u64) -> Result<Vec<u8>, Error> {
    let now = std::time::SystemTime::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_secs())
        .map_err(|_| Error::InvalidToken)?;
    decrypt_at(key, token, ttl_secs, now)
}

/// Open a token as of `now_secs` (Unix time) — the testable core.
pub fn decrypt_at(
    key: &[u8; KEY_LEN],
    token: &str,
    ttl_secs: u64,
    now_secs: u64,
) -> Result<Vec<u8>, Error> {
    // Minimum: version + timestamp + iv + one block + mac.
    let raw = b64_decode(token.trim())?;
    if raw.len() < 1 + 8 + IV_LEN + 16 + MAC_LEN || raw[0] != VERSION {
        return Err(Error::InvalidToken);
    }
    let (head, tag) = raw.split_at(raw.len() - MAC_LEN);

    let mut mac = HmacSha256::new_from_slice(&key[..16]).map_err(|_| Error::InvalidToken)?;
    mac.update(head);
    mac.verify_slice(tag).map_err(|_| Error::InvalidToken)?;

    let ts = u64::from_be_bytes(head[1..9].try_into().map_err(|_| Error::InvalidToken)?);
    if now_secs.saturating_add(MAX_CLOCK_SKEW_SECS) < ts {
        return Err(Error::ClockSkew);
    }
    if ts.saturating_add(ttl_secs) < now_secs {
        return Err(Error::Expired);
    }

    Aes128CbcDec::new_from_slices(&key[16..], &head[9..9 + IV_LEN])
        .map_err(|_| Error::InvalidToken)?
        .decrypt_padded_vec_mut::<Pkcs7>(&head[9 + IV_LEN..])
        .map_err(|_| Error::InvalidToken)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Official Fernet spec vector, cross-generated with Python `cryptography`
    // (key ..., ts 499162800, iv 0..16, plaintext "hello").
    const SPEC_KEY: &str = "cw_0x689RpI-jtRR7oE8h_eQsKImvJapLeSbXpwF4e4=";
    const SPEC_TOKEN: &str = "gAAAAAAdwJ6wAAECAwQFBgcICQoLDA0ODy021cpGVWKZ_eEwCGM4BLLF_5CV9dOPmrhuVUPgJobwOz7JcbmrR64jVmpU4IwqDA==";
    const SPEC_TS: u64 = 499162800;

    fn spec_key() -> [u8; KEY_LEN] {
        decode_key(SPEC_KEY).unwrap()
    }

    #[test]
    fn spec_vector_encrypt_matches() {
        let iv: [u8; IV_LEN] = core::array::from_fn(|i| i as u8);
        assert_eq!(encrypt_with(&spec_key(), b"hello", SPEC_TS, iv), SPEC_TOKEN);
    }

    #[test]
    fn spec_vector_decrypt_roundtrip() {
        let pt = decrypt_at(&spec_key(), SPEC_TOKEN, 60, SPEC_TS + 30).unwrap();
        assert_eq!(pt, b"hello");
    }

    #[test]
    fn round_trip() {
        let key = decode_key(&random_key()).unwrap();
        let token = encrypt(&key, b"EVT#123#epi_456");
        assert_eq!(decrypt(&key, &token, 30 * 24 * 3600).unwrap(), b"EVT#123#epi_456");
    }

    #[test]
    fn tampered_token_rejected() {
        let mut token = SPEC_TOKEN.to_string();
        let last = token.pop().unwrap();
        token.push(if last == 'A' { 'B' } else { 'A' });
        assert_eq!(
            decrypt_at(&spec_key(), &token, 60, SPEC_TS + 30),
            Err(Error::InvalidToken)
        );
    }

    #[test]
    fn expired_token_rejected() {
        assert_eq!(
            decrypt_at(&spec_key(), SPEC_TOKEN, 60, SPEC_TS + 61),
            Err(Error::Expired)
        );
    }

    #[test]
    fn future_token_rejected() {
        assert_eq!(
            decrypt_at(&spec_key(), SPEC_TOKEN, 60, SPEC_TS - 61),
            Err(Error::ClockSkew)
        );
    }

    #[test]
    fn bad_key_rejected() {
        // Valid base64, wrong length.
        assert_eq!(decode_key("aGVsbG8="), Err(Error::InvalidKey));
        // Not base64 at all.
        assert!(decode_key("!!!not-a-key!!!").is_err());
    }

    #[test]
    fn unpadded_key_accepted() {
        let stripped = SPEC_KEY.trim_end_matches('=');
        assert_eq!(decode_key(stripped).unwrap(), spec_key());
    }

    #[cfg(test)]
    fn random_key() -> String {
        use rand::RngCore;
        let mut raw = [0u8; KEY_LEN];
        rand::rng().fill_bytes(&mut raw);
        URL_SAFE.encode(raw)
    }
}
