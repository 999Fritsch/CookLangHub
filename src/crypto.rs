//! Encryption for the credentials that the application stores.
//!
//! Forgejo access tokens and the OAuth client secret sit in SQLite. They are
//! encrypted there, so a copy of the database file alone does not give
//! somebody the ability to act as a user.
//!
//! The key comes from the installation session secret through HKDF. Changing
//! the session secret therefore invalidates every stored credential, which
//! signs everybody out. That is the correct behavior for a rotated key.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Separates this key from any other key derived from the same secret.
const KEY_INFO: &[u8] = b"cooklanghub:credential-encryption:v1";
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("cannot derive the encryption key")]
    KeyDerivation,
    #[error("cannot encrypt the value")]
    Encrypt,
    #[error("cannot decrypt the value; the session secret may have changed")]
    Decrypt,
}

/// Encrypts and decrypts short secrets with one installation key.
#[derive(Clone)]
pub struct Cipher {
    inner: ChaCha20Poly1305,
}

// The key must never reach a log line.
impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cipher([redacted])")
    }
}

impl Cipher {
    /// Derive the installation key from the session secret.
    pub fn from_session_secret(secret: &str) -> Result<Self, CryptoError> {
        let hkdf = hkdf::Hkdf::<Sha256>::new(None, secret.as_bytes());
        let mut key = [0u8; 32];
        hkdf.expand(KEY_INFO, &mut key)
            .map_err(|_| CryptoError::KeyDerivation)?;

        let inner =
            ChaCha20Poly1305::new_from_slice(&key).map_err(|_| CryptoError::KeyDerivation)?;
        Ok(Self { inner })
    }

    /// Encrypt a value. The result carries its nonce in the first 12 bytes.
    pub fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>, CryptoError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = self
            .inner
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| CryptoError::Encrypt)?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt a value that [`Cipher::encrypt`] produced.
    pub fn decrypt(&self, stored: &[u8]) -> Result<String, CryptoError> {
        if stored.len() <= NONCE_LEN {
            return Err(CryptoError::Decrypt);
        }
        let (nonce_bytes, ciphertext) = stored.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce_bytes.try_into().map_err(|_| CryptoError::Decrypt)?;

        let plaintext = self
            .inner
            .decrypt(&Nonce::from(nonce), ciphertext)
            .map_err(|_| CryptoError::Decrypt)?;

        String::from_utf8(plaintext).map_err(|_| CryptoError::Decrypt)
    }
}

/// Derive a second secret from the installation session secret.
///
/// A derived secret needs no separate setting and no separate store, and it
/// is the same after a restart, which is what lets a repeated registration
/// find the value that Forgejo already holds. `purpose` separates one
/// derived secret from every other, so learning one gives nothing about the
/// next. Changing the session secret changes all of them.
pub fn derived_secret(session_secret: &str, purpose: &str) -> Result<String, CryptoError> {
    let hkdf = hkdf::Hkdf::<Sha256>::new(None, session_secret.as_bytes());
    let mut out = [0u8; 32];
    hkdf.expand(purpose.as_bytes(), &mut out)
        .map_err(|_| CryptoError::KeyDerivation)?;

    Ok(URL_SAFE_NO_PAD.encode(out))
}

/// Make a random URL-safe token of `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Hash a token for storage. The database keeps the digest, never the token.
pub fn digest(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The PKCE S256 challenge for a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> Cipher {
        Cipher::from_session_secret("a-test-session-secret").unwrap()
    }

    #[test]
    fn a_value_survives_a_round_trip() {
        let c = cipher();
        let stored = c.encrypt("gto_secret_token").unwrap();
        assert_eq!(c.decrypt(&stored).unwrap(), "gto_secret_token");
    }

    #[test]
    fn the_stored_form_does_not_contain_the_plaintext() {
        let stored = cipher().encrypt("gto_secret_token").unwrap();
        let as_text = String::from_utf8_lossy(&stored);
        assert!(!as_text.contains("gto_secret_token"));
    }

    #[test]
    fn the_same_value_encrypts_differently_each_time() {
        let c = cipher();
        assert_ne!(
            c.encrypt("same").unwrap(),
            c.encrypt("same").unwrap(),
            "a repeated nonce would leak that two values are equal"
        );
    }

    #[test]
    fn another_session_secret_cannot_decrypt() {
        let stored = cipher().encrypt("gto_secret_token").unwrap();
        let other = Cipher::from_session_secret("a-different-secret").unwrap();
        assert!(other.decrypt(&stored).is_err());
    }

    #[test]
    fn a_changed_byte_fails_to_decrypt() {
        let c = cipher();
        let mut stored = c.encrypt("gto_secret_token").unwrap();
        let last = stored.len() - 1;
        stored[last] ^= 0x01;
        assert!(c.decrypt(&stored).is_err());
    }

    #[test]
    fn the_debug_output_hides_the_key() {
        assert_eq!(format!("{:?}", cipher()), "Cipher([redacted])");
    }

    #[test]
    fn a_random_token_differs_every_time() {
        assert_ne!(random_token(32), random_token(32));
    }

    #[test]
    fn a_derived_secret_is_the_same_after_a_restart() {
        let first = derived_secret("a-test-session-secret", "webhook").unwrap();
        let second = derived_secret("a-test-session-secret", "webhook").unwrap();
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn each_purpose_gives_its_own_secret() {
        let webhook = derived_secret("a-test-session-secret", "webhook").unwrap();
        let other = derived_secret("a-test-session-secret", "something-else").unwrap();
        assert_ne!(webhook, other);
    }

    #[test]
    fn a_derived_secret_never_contains_the_session_secret() {
        let derived = derived_secret("super-secret-key", "webhook").unwrap();
        assert!(!derived.contains("super-secret-key"));
    }

    #[test]
    fn another_session_secret_derives_another_value() {
        assert_ne!(
            derived_secret("one", "webhook").unwrap(),
            derived_secret("two", "webhook").unwrap()
        );
    }

    #[test]
    fn the_pkce_challenge_matches_the_rfc_7636_example() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
