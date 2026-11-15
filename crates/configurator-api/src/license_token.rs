//! The tenant licence token at rest (after commonpractice's `tenants.license_token_encrypted`):
//! AES-256-GCM under `LICENSE_TOKEN_ENC_KEY` (base64 of 32 bytes), stored as
//! `v1:` + base64(nonce (12 bytes) || ciphertext || tag (16 bytes)), standard alphabet, padded.
//!
//! The tenant id is the associated data, so a sealed token copied onto another tenant's row does not
//! open. A token that does not open (wrong or rotated key, a corrupt or moved row, no `v1:` prefix) is
//! an error, never "no licence": gate 2 turns it into 402 `license_unreadable`, which needs an
//! operator (commonpractice `LicenseTokenUnreadableError`). There is no plaintext fallback: migration
//! 0006 refuses to run over a row that is not `v1:`.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use uuid::Uuid;

/// The version prefix of a sealed token.
pub const PREFIX: &str = "v1:";
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;

/// `LICENSE_TOKEN_ENC_KEY`, decoded. Never printed.
#[derive(Clone)]
pub struct TokenKey([u8; KEY_LEN]);

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenKey([redacted])")
    }
}

impl TokenKey {
    /// The key from its base64 form (standard alphabet, padded): exactly 32 bytes, else an error that
    /// says what is wrong without echoing the value.
    pub fn from_base64(s: &str) -> Result<Self, String> {
        let raw = STANDARD
            .decode(s.trim())
            .map_err(|_| "is not valid base64".to_string())?;
        let key: [u8; KEY_LEN] = raw
            .as_slice()
            .try_into()
            .map_err(|_| format!("must decode to {KEY_LEN} bytes, not {}", raw.len()))?;
        Ok(Self(key))
    }
}

/// A sealed token could not be opened, or a token could not be sealed. Carries no key or token bytes.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("the stored licence token has no v1: prefix")]
    NotVersioned,
    #[error("the stored licence token is not valid base64")]
    NotBase64,
    #[error("the stored licence token is too short")]
    Truncated,
    #[error("the stored licence token does not decrypt (wrong key, or a corrupt or moved row)")]
    Undecryptable,
    #[error("the decrypted licence token is not UTF-8")]
    NotUtf8,
    #[error("could not seal the licence token")]
    Seal,
}

/// Seals and opens licence tokens under one key.
pub struct TokenCipher {
    key: LessSafeKey,
    rng: SystemRandom,
}

impl TokenCipher {
    pub fn new(key: &TokenKey) -> Self {
        let unbound = UnboundKey::new(&AES_256_GCM, &key.0)
            .expect("a 32-byte key is always a valid AES-256-GCM key");
        Self {
            key: LessSafeKey::new(unbound),
            rng: SystemRandom::new(),
        }
    }

    /// `token`, sealed for `tenant` under a fresh random nonce.
    pub fn seal(&self, tenant: Uuid, token: &str) -> Result<String, TokenError> {
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce).map_err(|_| TokenError::Seal)?;
        let mut in_out = token.as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(tenant.as_bytes()),
                &mut in_out,
            )
            .map_err(|_| TokenError::Seal)?;
        let mut blob = Vec::with_capacity(NONCE_LEN + in_out.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&in_out);
        Ok(format!("{PREFIX}{}", STANDARD.encode(blob)))
    }

    /// The token `stored` holds for `tenant`.
    pub fn open(&self, tenant: Uuid, stored: &str) -> Result<String, TokenError> {
        let b64 = stored
            .strip_prefix(PREFIX)
            .ok_or(TokenError::NotVersioned)?;
        let mut blob = STANDARD.decode(b64).map_err(|_| TokenError::NotBase64)?;
        if blob.len() < NONCE_LEN + TAG_LEN {
            return Err(TokenError::Truncated);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&blob[..NONCE_LEN]);
        let plain = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(tenant.as_bytes()),
                &mut blob[NONCE_LEN..],
            )
            .map_err(|_| TokenError::Undecryptable)?;
        String::from_utf8(plain.to_vec()).map_err(|_| TokenError::NotUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> TokenKey {
        TokenKey::from_base64(&STANDARD.encode([byte; KEY_LEN])).unwrap()
    }

    fn tenant() -> Uuid {
        Uuid::parse_str("6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b").unwrap()
    }

    #[test]
    fn round_trip_with_a_fresh_nonce_each_time() {
        let c = TokenCipher::new(&key(7));
        let a = c.seal(tenant(), "lic_tok_abc").unwrap();
        let b = c.seal(tenant(), "lic_tok_abc").unwrap();
        assert!(a.starts_with(PREFIX), "{a}");
        assert!(
            !a.contains("lic_tok_abc"),
            "the token is not stored in clear"
        );
        assert_ne!(a, b, "a fresh nonce per seal");
        assert_eq!(c.open(tenant(), &a).unwrap(), "lic_tok_abc");
        assert_eq!(c.open(tenant(), &b).unwrap(), "lic_tok_abc");
        // the stored shape: v1: + base64(nonce 12 || ciphertext || tag 16)
        let raw = STANDARD.decode(&a[PREFIX.len()..]).unwrap();
        assert_eq!(raw.len(), NONCE_LEN + "lic_tok_abc".len() + TAG_LEN);
    }

    #[test]
    fn the_wrong_key_does_not_open() {
        let sealed = TokenCipher::new(&key(7)).seal(tenant(), "tok").unwrap();
        assert!(matches!(
            TokenCipher::new(&key(8)).open(tenant(), &sealed),
            Err(TokenError::Undecryptable)
        ));
    }

    #[test]
    fn a_token_moved_to_another_tenant_does_not_open() {
        let c = TokenCipher::new(&key(7));
        let sealed = c.seal(tenant(), "tok").unwrap();
        assert!(matches!(
            c.open(Uuid::new_v4(), &sealed),
            Err(TokenError::Undecryptable)
        ));
    }

    #[test]
    fn tampering_is_detected() {
        let c = TokenCipher::new(&key(7));
        let sealed = c.seal(tenant(), "tok").unwrap();
        let mut raw = STANDARD.decode(&sealed[PREFIX.len()..]).unwrap();
        // flip a bit in the ciphertext, in the tag, and in the nonce
        for i in [NONCE_LEN, raw.len() - 1, 0] {
            raw[i] ^= 0x01;
            let tampered = format!("{PREFIX}{}", STANDARD.encode(&raw));
            assert!(
                matches!(c.open(tenant(), &tampered), Err(TokenError::Undecryptable)),
                "byte {i}"
            );
            raw[i] ^= 0x01;
        }
        // and the untouched blob still opens
        let back = format!("{PREFIX}{}", STANDARD.encode(raw));
        assert_eq!(c.open(tenant(), &back).unwrap(), "tok");
    }

    #[test]
    fn malformed_stored_values_are_errors_not_plaintext() {
        let c = TokenCipher::new(&key(7));
        assert!(matches!(
            c.open(tenant(), "tok-plaintext"),
            Err(TokenError::NotVersioned)
        ));
        assert!(matches!(
            c.open(tenant(), "v1:not base64!"),
            Err(TokenError::NotBase64)
        ));
        assert!(matches!(
            c.open(tenant(), &format!("{PREFIX}{}", STANDARD.encode([0u8; 27]))),
            Err(TokenError::Truncated)
        ));
    }

    #[test]
    fn keys_must_be_32_bytes_of_base64_and_never_print() {
        assert!(TokenKey::from_base64(&STANDARD.encode([1u8; 32])).is_ok());
        assert!(TokenKey::from_base64(&format!("  {}\n", STANDARD.encode([1u8; 32]))).is_ok());
        let short = TokenKey::from_base64(&STANDARD.encode([1u8; 16])).unwrap_err();
        assert!(short.contains("32 bytes, not 16"), "{short}");
        assert!(TokenKey::from_base64("%%%").is_err());
        assert!(TokenKey::from_base64("").is_err());
        let shown = format!("{:?}", key(0xAB));
        assert_eq!(shown, "TokenKey([redacted])");
    }
}
