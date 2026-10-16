//! Test signing keys and token minting, after care-bus's `rs256` test helper @ 3b88445.
//! The RSA keypairs are generated once per test binary, so no private key is committed.
#![allow(dead_code)]
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode};

use super::TEST_ISSUER;

/// The audience the API expects (`KEYCLOAK_AUDIENCE` default).
pub const TEST_AUDIENCE: &str = "strata-configurator";
/// The realm role that makes a caller super-admin (`SUPER_ADMIN_ROLE` default).
pub const SUPER_ADMIN: &str = "config-super-admin";

struct KeyPair {
    enc: EncodingKey,
    /// base64url (no padding) modulus, as a JWKS `n`.
    n: String,
}

fn b64url(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let v = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..=c.len() {
            out.push(T[((v >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn gen_key() -> KeyPair {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::traits::PublicKeyParts;
    let k = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let pem = k.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
    KeyPair {
        enc: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
        n: b64url(&k.n().to_bytes_be()),
    }
}

/// The key the API trusts.
static KEY: LazyLock<KeyPair> = LazyLock::new(gen_key);
/// A key the API has never seen.
static FOREIGN: LazyLock<KeyPair> = LazyLock::new(gen_key);

/// The public half of the trusted key, for `Verifier::static_key`.
pub fn decoding_key() -> DecodingKey {
    DecodingKey::from_rsa_components(&KEY.n, "AQAB").unwrap()
}

fn mint(key: &KeyPair, sub: &str, roles: &[&str]) -> String {
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some("test".into());
    let claims = serde_json::json!({
        "sub": sub, "preferred_username": username(sub), "exp": exp,
        "iss": TEST_ISSUER, "aud": TEST_AUDIENCE,
        "realm_access": {"roles": roles}
    });
    encode(&h, &claims, &key.enc).unwrap()
}

/// The `preferred_username` minted for `sub`, i.e. the audit actor.
pub fn username(sub: &str) -> String {
    format!("{sub}.user")
}

/// A valid RS256 portal-realm token for `sub` with these realm roles.
pub fn token(sub: &str, roles: &[&str]) -> String {
    mint(&KEY, sub, roles)
}

/// A super-admin token for `sub`.
pub fn super_admin(sub: &str) -> String {
    token(sub, &[SUPER_ADMIN])
}

/// A well-formed token signed by a key the API does not trust.
pub fn foreign_token(sub: &str, roles: &[&str]) -> String {
    mint(&FOREIGN, sub, roles)
}
