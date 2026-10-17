//! Keycloak JWT verification (spec section 10.2).
//!
//! Copied from care-bus `crates/carebus-api/src/auth.rs` @ 3b88445 (never a dependency on it): the
//! JWKS cache and the static-key mode. care-bus's static service tokens are dropped: every caller
//! here is a person with a portal-realm token. The static key is RS256 like production, so tests
//! exercise the same algorithm as Keycloak.
//!
//! Keycloak only authenticates (commonpractice's identity model, plan 2026-10-17 §2): from the token
//! this reads `sub`, `preferred_username` and the roles of this app's own client
//! (`resource_access.<KEYCLOAK_AUDIENCE>.roles`), of which only `super_admin` means anything. Realm
//! roles are never read: the shared realm `super_admin` belongs to other apps. Tenancy and tenant
//! roles live in this service's `users` table.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::ApiError;
use crate::state::AppState;

/// The JWKS is cached this long.
const JWKS_TTL: Duration = Duration::from_secs(600);
/// An unknown `kid` re-fetches the JWKS (key rotation) at most this often.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(30);

/// A verified token.
#[derive(Clone, Debug)]
pub struct Claims {
    pub sub: String,
    pub preferred_username: Option<String>,
    /// This app's client roles (`resource_access.<audience>.roles`); realm roles are ignored.
    pub roles: Vec<String>,
}

impl Claims {
    /// The name written to audit events: `preferred_username`, else `sub` (care-bus convention).
    pub fn actor(&self) -> String {
        self.preferred_username
            .clone()
            .unwrap_or_else(|| self.sub.clone())
    }
    pub fn has(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

#[derive(Deserialize)]
struct RawClaims {
    sub: String,
    preferred_username: Option<String>,
    /// Client roles by client id. `realm_access` is deliberately not parsed.
    #[serde(default)]
    resource_access: HashMap<String, ClientRoles>,
}
#[derive(Deserialize, Default)]
struct ClientRoles {
    #[serde(default)]
    roles: Vec<String>,
}

/// Parsed keys by `kid`, plus when we last fetched successfully and when we last tried at all.
#[derive(Default)]
struct JwksCache {
    keys: HashMap<String, DecodingKey>,
    /// Last successful fetch; the cache is fresh for `JWKS_TTL` after it.
    fetched: Option<Instant>,
    /// Last fetch attempt, successful or not. The 30 s floor applies to this, so a Keycloak outage is
    /// hit once per 30 s in total instead of once per request.
    last_attempt: Option<Instant>,
    last_failed: bool,
}

enum Keys {
    Jwks {
        url: String,
        http: reqwest::Client,
        cache: Mutex<JwksCache>,
    },
    Static(DecodingKey),
}

pub struct Verifier {
    issuer: String,
    audience: String,
    keys: Keys,
}

impl Verifier {
    /// Production: RS256 keys from Keycloak's JWKS endpoint (`KEYCLOAK_JWKS_URL`).
    pub fn jwks(issuer: &str, audience: &str, jwks_url: &str, http: reqwest::Client) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            keys: Keys::Jwks {
                url: jwks_url.into(),
                http,
                cache: Mutex::new(JwksCache::default()),
            },
        }
    }

    /// RS256 against one fixed public key, for tests. `main` never calls this.
    pub fn static_key(issuer: &str, audience: &str, key: DecodingKey) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            keys: Keys::Static(key),
        }
    }

    pub async fn verify(&self, token: &str) -> Result<Claims, ApiError> {
        let header = decode_header(token).map_err(|_| ApiError::Unauthorized)?;
        let key = match &self.keys {
            Keys::Static(k) => k.clone(),
            Keys::Jwks { url, http, cache } => {
                let kid = header.kid.as_deref().ok_or(ApiError::Unauthorized)?;
                self.jwk(url, http, cache, kid).await?
            }
        };
        let mut v = Validation::new(Algorithm::RS256);
        // jsonwebtoken checks `iss`/`aud` only when present: make them (and `sub`) mandatory, so a
        // portal-realm token minted for another client without an `aud` cannot pass (spec 10.2).
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        v.set_issuer(&[&self.issuer]);
        v.set_audience(&[&self.audience]);
        v.leeway = 5;
        let data = decode::<RawClaims>(token, &key, &v).map_err(|_| ApiError::Unauthorized)?;
        let RawClaims {
            sub,
            preferred_username,
            mut resource_access,
        } = data.claims;
        if sub.is_empty() {
            return Err(ApiError::Unauthorized);
        }
        Ok(Claims {
            sub,
            preferred_username,
            roles: resource_access
                .remove(&self.audience)
                .map(|c| c.roles)
                .unwrap_or_default(),
        })
    }

    /// JWKS cached 10 minutes. A stale cache or an unknown `kid` triggers a fetch (key rotation), but
    /// never more than one attempt per 30 s (failures count), so neither garbage `kid`s nor a Keycloak
    /// outage can turn this service into a hammer. If a fetch fails, keys already held keep verifying
    /// (stale beats down); 503 only when no usable key exists.
    async fn jwk(
        &self,
        url: &str,
        http: &reqwest::Client,
        cache: &Mutex<JwksCache>,
        kid: &str,
    ) -> Result<DecodingKey, ApiError> {
        // The lock is held across the fetch on purpose: concurrent callers wait for one attempt and
        // then reuse its outcome (the floor below stops them from fetching again).
        let mut c = cache.lock().await;
        let fresh = c.fetched.is_some_and(|t| t.elapsed() < JWKS_TTL);
        let known = c.keys.contains_key(kid);
        if !(fresh && known)
            && c.last_attempt
                .is_none_or(|t| t.elapsed() >= JWKS_MIN_REFRESH)
        {
            c.last_attempt = Some(Instant::now());
            match fetch(url, http).await {
                Ok(keys) => {
                    c.keys = keys;
                    c.fetched = Some(Instant::now());
                    c.last_failed = false;
                }
                Err(_) => c.last_failed = true,
            }
        }
        if let Some(k) = c.keys.get(kid) {
            return Ok(k.clone());
        }
        // Unknown kid and the last fetch failed: we cannot tell "rotated" from "forged", so say 503.
        if c.last_failed {
            return Err(keycloak_down());
        }
        Err(ApiError::Unauthorized)
    }
}

fn keycloak_down() -> ApiError {
    ApiError::Unavailable(
        "keycloak_unavailable",
        "the identity provider does not answer".into(),
    )
}

async fn fetch(
    url: &str,
    http: &reqwest::Client,
) -> Result<HashMap<String, DecodingKey>, ApiError> {
    let resp = http
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|_| keycloak_down())?;
    let set: JwkSet = resp.json().await.map_err(|_| keycloak_down())?;
    Ok(set
        .keys
        .iter()
        .filter_map(|j| {
            let kid = j.common.key_id.clone()?;
            Some((kid, DecodingKey::from_jwk(j).ok()?))
        })
        .collect())
}

/// The authenticated person. Missing or invalid token → 401.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The stable identity: matches `users.sub` and is the audit actor.
    pub sub: String,
    /// `preferred_username`, else `sub`: for display only (usernames can change).
    pub actor: String,
    /// Holds the configured super-admin client role (`super_admin` by default) on this app's client.
    pub super_admin: bool,
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let h = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        // The auth scheme is case-insensitive (RFC 7235).
        let (scheme, token) = h.split_once(' ').ok_or(ApiError::Unauthorized)?;
        let token = token.trim();
        if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
            return Err(ApiError::Unauthorized);
        }
        let c = st.auth.verify(token).await?;
        Ok(Caller {
            super_admin: c.has(&st.cfg.super_admin_role),
            actor: c.actor(),
            sub: c.sub,
        })
    }
}

/// 403 `forbidden` unless the caller is the super-admin.
pub fn require_super_admin(c: &Caller) -> Result<(), ApiError> {
    if c.super_admin {
        Ok(())
    } else {
        Err(ApiError::Forbidden("super-admin"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use std::sync::Arc;
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Test keypair generated once per test binary (RSA keygen is slow); nothing secret is committed.
    struct TestKey {
        pem: String,
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

    fn gen_key() -> TestKey {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::traits::PublicKeyParts;
        let k = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        TestKey {
            pem: k
                .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
                .unwrap()
                .to_string(),
            n: b64url(&k.n().to_bytes_be()),
        }
    }

    static KEY_A: LazyLock<TestKey> = LazyLock::new(gen_key);
    static KEY_B: LazyLock<TestKey> = LazyLock::new(gen_key);

    struct Stub {
        url: String,
        hits: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
    }

    const ISS: &str = "https://issuer.test/realms/portal";

    /// A JWKS endpoint serving `jwks`; counts hits; answers 500 while `fail` is set.
    async fn stub_jwks(jwks: serde_json::Value) -> Stub {
        let hits = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        let (h, f) = (hits.clone(), fail.clone());
        let app = axum::Router::new().route(
            "/certs",
            axum::routing::get(move || {
                let (h, f, jwks) = (h.clone(), f.clone(), jwks.clone());
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    if f.load(Ordering::SeqCst) {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom")
                            .into_response();
                    }
                    axum::Json(jwks).into_response()
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/certs", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        Stub { url, hits, fail }
    }

    fn verifier(s: &Stub) -> Verifier {
        Verifier::jwks(ISS, "aud", &s.url, reqwest::Client::new())
    }

    fn empty() -> serde_json::Value {
        serde_json::json!({"keys": []})
    }
    fn jwks_a() -> serde_json::Value {
        serde_json::json!({"keys": [{"kty":"RSA","kid":"k1","use":"sig","alg":"RS256","n":KEY_A.n.as_str(),"e":"AQAB"}]})
    }

    fn exp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 300
    }

    fn rs256(pem: &str, kid: &str, aud: &str) -> String {
        let mut h = jsonwebtoken::Header::new(Algorithm::RS256);
        h.kid = Some(kid.into());
        jsonwebtoken::encode(
            &h,
            &serde_json::json!({"sub":"u-9","preferred_username":"j.devries","exp":exp(),"iss":ISS,"aud":aud,
                "realm_access":{"roles":["realm-only"]},
                "resource_access":{"aud":{"roles":["super_admin"]},"other-client":{"roles":["elsewhere"]}}}),
            &jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn token_with_kid(kid: &str) -> String {
        // The key lookup fails before the signature matters, so any signing key will do.
        let mut h = jsonwebtoken::Header::new(Algorithm::HS256);
        h.kid = Some(kid.into());
        jsonwebtoken::encode(
            &h,
            &serde_json::json!({"sub": "x"}),
            &jsonwebtoken::EncodingKey::from_secret(b"k"),
        )
        .unwrap()
    }

    /// Pretend the last fetch (and attempt) happened `secs` ago.
    async fn age(v: &Verifier, secs: u64) {
        if let Keys::Jwks { cache, .. } = &v.keys {
            let mut c = cache.lock().await;
            let t = Instant::now()
                .checked_sub(Duration::from_secs(secs))
                .unwrap();
            c.fetched = Some(t);
            c.last_attempt = Some(t);
        }
    }

    fn is_503(r: &Result<Claims, ApiError>) -> bool {
        matches!(r, Err(ApiError::Unavailable("keycloak_unavailable", _)))
    }

    #[tokio::test]
    async fn rs256_token_signed_by_the_jwks_key_verifies() {
        let s = stub_jwks(jwks_a()).await;
        let v = verifier(&s);
        let c = v.verify(&rs256(&KEY_A.pem, "k1", "aud")).await.unwrap();
        assert_eq!(c.sub, "u-9");
        assert_eq!(c.actor(), "j.devries");
        // only this client's roles: not the realm's, not another client's
        assert_eq!(c.roles, vec!["super_admin".to_string()]);
        // the parsed key is cached: a second token does not re-fetch
        v.verify(&rs256(&KEY_A.pem, "k1", "aud")).await.unwrap();
        assert_eq!(s.hits.load(Ordering::SeqCst), 1);
        // wrong audience still refused on the production path
        let r = v.verify(&rs256(&KEY_A.pem, "k1", "other")).await;
        assert!(matches!(r, Err(ApiError::Unauthorized)));
    }

    #[tokio::test]
    async fn realm_roles_are_never_read() {
        let s = stub_jwks(jwks_a()).await;
        let v = verifier(&s);
        let mut h = jsonwebtoken::Header::new(Algorithm::RS256);
        h.kid = Some("k1".into());
        let tok = jsonwebtoken::encode(
            &h,
            &serde_json::json!({"sub":"u-1","exp":exp(),"iss":ISS,"aud":"aud",
                "realm_access":{"roles":["super_admin"]}}),
            &jsonwebtoken::EncodingKey::from_rsa_pem(KEY_A.pem.as_bytes()).unwrap(),
        )
        .unwrap();
        let c = v.verify(&tok).await.unwrap();
        assert!(c.roles.is_empty(), "{:?}", c.roles);
        assert!(!c.has("super_admin"));
    }

    #[tokio::test]
    async fn rs256_unknown_kid_or_foreign_signature_is_401() {
        let s = stub_jwks(jwks_a()).await;
        let v = verifier(&s);
        let r = v.verify(&rs256(&KEY_A.pem, "nope", "aud")).await;
        assert!(matches!(r, Err(ApiError::Unauthorized)));
        // right kid, signed by a different keypair
        let r = v.verify(&rs256(&KEY_B.pem, "k1", "aud")).await;
        assert!(matches!(r, Err(ApiError::Unauthorized)));
        // control: the real key is accepted by the same verifier
        assert!(v.verify(&rs256(&KEY_A.pem, "k1", "aud")).await.is_ok());
    }

    #[tokio::test]
    async fn unknown_kid_refreshes_jwks_at_most_once_per_30s() {
        let s = stub_jwks(empty()).await;
        let v = verifier(&s);
        for i in 0..5 {
            let r = v.verify(&token_with_kid(&format!("rotated-{i}"))).await;
            assert!(matches!(r, Err(ApiError::Unauthorized)));
        }
        // 1 initial fetch; the unknown kids right after must not re-fetch
        assert_eq!(s.hits.load(Ordering::SeqCst), 1);
        // 30 s later an unknown kid is allowed to refresh once
        age(&v, 31).await;
        let _ = v.verify(&token_with_kid("rotated-late")).await;
        assert_eq!(s.hits.load(Ordering::SeqCst), 2);
        let _ = v.verify(&token_with_kid("rotated-late-2")).await;
        assert_eq!(s.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn keycloak_outage_is_hit_once_not_once_per_request() {
        let s = stub_jwks(empty()).await;
        let v = Arc::new(verifier(&s));
        v.verify(&token_with_kid("warm")).await.unwrap_err();
        assert_eq!(s.hits.load(Ordering::SeqCst), 1);
        s.fail.store(true, Ordering::SeqCst);
        age(&v, 31).await;
        let tasks: Vec<_> = (0..5)
            .map(|i| {
                let v = v.clone();
                tokio::spawn(async move { v.verify(&token_with_kid(&format!("n{i}"))).await })
            })
            .collect();
        for t in tasks {
            // the cache holds no such kid and the last attempt failed: 503, not 401
            assert!(is_503(&t.await.unwrap()));
        }
        // 1 (warm) + exactly 1 failed attempt shared by all 5 waiters
        assert_eq!(s.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stale_keys_keep_verifying_through_an_outage() {
        let s = stub_jwks(jwks_a()).await;
        let v = verifier(&s);
        v.verify(&rs256(&KEY_A.pem, "k1", "aud")).await.unwrap();
        s.fail.store(true, Ordering::SeqCst);
        age(&v, 601).await; // past the TTL
        for _ in 0..3 {
            let c = v.verify(&rs256(&KEY_A.pem, "k1", "aud")).await.unwrap();
            assert_eq!(c.sub, "u-9");
        }
        // 1 warm-up + 1 failed refresh; the next two requests reuse the failure
        assert_eq!(s.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn keycloak_down_is_503_not_401() {
        let v = Verifier::jwks(
            ISS,
            "aud",
            "http://127.0.0.1:1/certs",
            reqwest::Client::new(),
        );
        assert!(is_503(&v.verify(&token_with_kid("k")).await));
    }
}
