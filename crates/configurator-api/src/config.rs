use anyhow::{Context, Result};

use crate::license_token::TokenKey;

#[derive(Clone)]
pub struct Config {
    /// The runtime connection: the non-bypass app role (`configurator_app`), under RLS.
    pub database_url: String,
    /// The owner connection (migrations, seeder, super-admin platform work); `DATABASE_URL` when unset.
    pub migrate_database_url: Option<String>,
    pub port: u16,
    pub keycloak_issuer: String,
    pub keycloak_audience: String,
    pub keycloak_jwks_url: String,
    pub super_admin_role: String,
    /// The portal-realm admin SA that invites people (W1.1); `None` (invites off) unless all four
    /// `KEYCLOAK_ADMIN_*` settings are set.
    pub keycloak_admin: Option<KeycloakAdminConfig>,
    /// Licence gate 2 (W2.1); off by default.
    pub license_gate: LicenseGateConfig,
    /// The `secure-apps` SA that issues tenant licences (W2.2); `None` (no issuance) unless all four
    /// `LICENSES_ADMIN_*` settings are set.
    pub licenses_admin: Option<LicensesAdminConfig>,
    /// `LICENSE_TOKEN_ENC_KEY`: the AES-256-GCM key the tenant licence token is stored under. Required
    /// (a startup error otherwise) whenever issuance or gate 2 is on, since both need the token.
    pub license_token_key: Option<TokenKey>,
    /// Early-access self-signup (W2.3); off by default.
    pub signup: SignupConfig,
}

/// Early-access self-signup (W2.3), after commonpractice `config.ts` `signupIntakeConfig`,
/// `turnstileConfig` and `signupTelegramConfig`.
#[derive(Clone)]
pub struct SignupConfig {
    /// `EARLY_ACCESS_SIGNUP_ENABLED`: off, every `/api/signup-approvals` route is the unknown-route 404.
    pub enabled: bool,
    /// `SIGNUP_INTAKE_AZP`: the client ids (`azp`) allowed to call the intake.
    pub intake_azp: Vec<String>,
    /// `SIGNUP_INTAKE_ROLE`: the client role (on this app's client) an intake token must hold.
    pub intake_role: String,
    /// `SIGNUP_QUEUE_MAX_PENDING`: pending rows past which the intake refuses (503 `signup_queue_full`).
    pub max_pending: i64,
    /// `TURNSTILE_SECRET`: when set, a relayed Turnstile token is verified.
    pub turnstile_secret: Option<String>,
    pub turnstile_verify_url: String,
    pub turnstile_timeout_ms: u64,
    /// Telegram notices; `None` (off) unless all three `TELEGRAM_*` values are set.
    pub telegram: Option<TelegramConfig>,
    /// `CONFIGURATOR_PUBLIC_URL`: the UI origin the Telegram Approve/Reject buttons open.
    pub public_url: Option<String>,
}

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
    /// The forum topic's `message_thread_id`.
    pub thread_id: i64,
    /// `TELEGRAM_API_BASE`, without a trailing slash (overridable for tests only).
    pub api_base: String,
}

impl std::fmt::Debug for SignupConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignupConfig")
            .field("enabled", &self.enabled)
            .field("intake_azp", &self.intake_azp)
            .field("intake_role", &self.intake_role)
            .field("max_pending", &self.max_pending)
            .field(
                "turnstile_secret",
                &self.turnstile_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("turnstile_verify_url", &self.turnstile_verify_url)
            .field(
                "telegram",
                &self.telegram.as_ref().map(|t| (&t.chat_id, &t.thread_id)),
            )
            .field("public_url", &self.public_url)
            .finish()
    }
}

/// `true`/`1` or `false`/`0`/unset; anything else is an error naming `key`.
fn flag(get: &impl Fn(&str) -> Option<String>, key: &str) -> Result<bool> {
    match get(key).as_deref().map(str::trim) {
        None | Some("" | "false" | "0") => Ok(false),
        Some("true" | "1") => Ok(true),
        Some(v) => anyhow::bail!("{key} must be true or false, not {v:?}"),
    }
}

/// Client credentials for `strata-configurator-admin` in the portal realm.
#[derive(Clone)]
pub struct KeycloakAdminConfig {
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    /// The realm's admin API, e.g. `https://auth.nonagentic.ai/admin/realms/portal`.
    pub api_url: String,
}

impl std::fmt::Debug for KeycloakAdminConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeycloakAdminConfig")
            .field("token_url", &self.token_url)
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("api_url", &self.api_url)
            .finish()
    }
}

/// Licence gate 2: introspect the tenant's licence on tenant requests (commonpractice
/// `license-gate.ts`).
#[derive(Clone, Debug)]
pub struct LicenseGateConfig {
    pub enabled: bool,
    pub introspect_url: String,
    /// The licences `App.key` a licence must be for; also the app licences are issued for.
    pub app_key: String,
    pub cache_ttl_sec: u64,
}

/// Client credentials for `strata-configurator-licenses-admin` in the `secure-apps` realm.
#[derive(Clone)]
pub struct LicensesAdminConfig {
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    /// The licences admin API base, e.g. `http://licenses.licenses.svc.cluster.local:8544`.
    pub url: String,
    /// The plan new tenants are licensed on.
    pub plan_code: String,
}

impl std::fmt::Debug for LicensesAdminConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LicensesAdminConfig")
            .field("token_url", &self.token_url)
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("url", &self.url)
            .field("plan_code", &self.plan_code)
            .finish()
    }
}

/// An optional group of settings: `None` when none is set, every value when all are, an error naming
/// the missing ones when only some are (half a service account is a misconfiguration, not "off").
fn group<const N: usize>(
    get: &impl Fn(&str) -> Option<String>,
    keys: [&str; N],
) -> Result<Option<[String; N]>> {
    let vals = keys.map(|k| get(k).filter(|v| !v.trim().is_empty()));
    if vals.iter().all(Option::is_none) {
        return Ok(None);
    }
    let missing: Vec<&str> = keys
        .iter()
        .zip(&vals)
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| *k)
        .collect();
    if !missing.is_empty() {
        anyhow::bail!("set all of {keys:?} or none: {missing:?} is not set");
    }
    Ok(Some(vals.map(Option::unwrap_or_default)))
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &"[redacted]")
            .field(
                "migrate_database_url",
                &self.migrate_database_url.as_ref().map(|_| "[redacted]"),
            )
            .field("port", &self.port)
            .field("keycloak_issuer", &self.keycloak_issuer)
            .field("keycloak_audience", &self.keycloak_audience)
            .field("keycloak_admin", &self.keycloak_admin)
            .field("license_gate", &self.license_gate)
            .field("licenses_admin", &self.licenses_admin)
            .field("license_token_key", &self.license_token_key)
            .field("signup", &self.signup)
            .finish_non_exhaustive()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// `from_env` over any lookup, so tests never touch the process environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let req = |k: &str| {
            get(k)
                .filter(|v| !v.trim().is_empty())
                .with_context(|| format!("{k} is not set"))
        };
        let opt = |k: &str, d: &str| get(k).filter(|v| !v.is_empty()).unwrap_or_else(|| d.into());
        let port = match get("PORT").filter(|v| !v.is_empty()) {
            Some(p) => p.parse().with_context(|| "PORT is not a valid port")?,
            None => 8097,
        };
        let keycloak_admin = group(
            &get,
            [
                "KEYCLOAK_ADMIN_TOKEN_URL",
                "KEYCLOAK_ADMIN_CLIENT_ID",
                "KEYCLOAK_ADMIN_CLIENT_SECRET",
                "KEYCLOAK_ADMIN_API_URL",
            ],
        )?
        .map(
            |[token_url, client_id, client_secret, api_url]| KeycloakAdminConfig {
                token_url,
                client_id,
                client_secret,
                api_url,
            },
        );
        let enabled = flag(&get, "LICENSE_GATE_2_ENABLED")?;
        let cache_ttl_sec = match get("LICENSE_CACHE_TTL_SEC").filter(|v| !v.is_empty()) {
            Some(v) => v
                .parse::<u64>()
                .with_context(|| "LICENSE_CACHE_TTL_SEC is not a number of seconds")?,
            None => 300,
        };
        let license_gate = LicenseGateConfig {
            enabled,
            introspect_url: opt(
                "LICENSE_INTROSPECT_URL",
                "http://licenses.licenses.svc.cluster.local:8544/license/introspect",
            ),
            app_key: opt("LICENSE_APP_KEY", "strata-configurator"),
            cache_ttl_sec,
        };
        let licenses_admin = group(
            &get,
            [
                "LICENSES_ADMIN_TOKEN_URL",
                "LICENSES_ADMIN_CLIENT_ID",
                "LICENSES_ADMIN_CLIENT_SECRET",
                "LICENSES_ADMIN_URL",
            ],
        )?
        .map(
            |[token_url, client_id, client_secret, url]| LicensesAdminConfig {
                token_url,
                client_id,
                client_secret,
                url,
                plan_code: opt("LICENSE_PLAN_CODE", "standard"),
            },
        );
        let trimmed = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        // Fail loud: a key that is set must be valid whatever else is on, and issuance or gate 2
        // without one is a misconfiguration, not "store the token in clear" or "deny everyone".
        let license_token_key = trimmed("LICENSE_TOKEN_ENC_KEY")
            .map(|v| {
                TokenKey::from_base64(&v)
                    .map_err(|why| anyhow::anyhow!("LICENSE_TOKEN_ENC_KEY {why}"))
            })
            .transpose()?;
        if license_token_key.is_none() && (licenses_admin.is_some() || license_gate.enabled) {
            anyhow::bail!(
                "LICENSE_TOKEN_ENC_KEY is not set: licence issuance and gate 2 store and read the \
                 tenant licence token encrypted under it (base64 of 32 bytes)"
            );
        }
        let max_pending =
            match trimmed("SIGNUP_QUEUE_MAX_PENDING") {
                Some(v) => v.parse::<i64>().ok().filter(|n| *n >= 1).with_context(
                    || "SIGNUP_QUEUE_MAX_PENDING must be a whole number of at least 1",
                )?,
                None => 500,
            };
        let turnstile_timeout_ms = match trimmed("TURNSTILE_TIMEOUT_MS") {
            Some(v) => v
                .parse::<u64>()
                .with_context(|| "TURNSTILE_TIMEOUT_MS is not a number of milliseconds")?,
            None => 5000,
        };
        // As commonpractice: Telegram is off unless the token, chat and forum topic are all set.
        let telegram = match (
            trimmed("TELEGRAM_BOT_TOKEN"),
            trimmed("TELEGRAM_SIGNUP_CHAT_ID"),
            trimmed("TELEGRAM_SIGNUP_THREAD_ID"),
        ) {
            (Some(bot_token), Some(chat_id), Some(thread_id)) => Some(TelegramConfig {
                bot_token,
                chat_id,
                thread_id: thread_id
                    .parse::<i64>()
                    .with_context(|| "TELEGRAM_SIGNUP_THREAD_ID is not a message_thread_id")?,
                api_base: trimmed("TELEGRAM_API_BASE")
                    .unwrap_or_else(|| "https://api.telegram.org".into())
                    .trim_end_matches('/')
                    .to_string(),
            }),
            _ => None,
        };
        let signup = SignupConfig {
            enabled: flag(&get, "EARLY_ACCESS_SIGNUP_ENABLED")?,
            intake_azp: trimmed("SIGNUP_INTAKE_AZP")
                .unwrap_or_else(|| "configurator-signup-intake".into())
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            intake_role: trimmed("SIGNUP_INTAKE_ROLE")
                .unwrap_or_else(|| "configurator-signup-intake".into()),
            max_pending,
            turnstile_secret: trimmed("TURNSTILE_SECRET"),
            turnstile_verify_url: trimmed("TURNSTILE_VERIFY_URL").unwrap_or_else(|| {
                "https://challenges.cloudflare.com/turnstile/v0/siteverify".into()
            }),
            turnstile_timeout_ms,
            telegram,
            public_url: trimmed("CONFIGURATOR_PUBLIC_URL")
                .map(|u| u.trim_end_matches('/').to_string()),
        };
        Ok(Self {
            signup,
            database_url: req("DATABASE_URL")?,
            migrate_database_url: get("MIGRATE_DATABASE_URL").filter(|v| !v.trim().is_empty()),
            port,
            keycloak_issuer: req("KEYCLOAK_ISSUER")?,
            keycloak_audience: opt("KEYCLOAK_AUDIENCE", "strata-configurator"),
            keycloak_jwks_url: req("KEYCLOAK_JWKS_URL")?,
            super_admin_role: opt("SUPER_ADMIN_ROLE", "super_admin"),
            keycloak_admin,
            license_gate,
            licenses_admin,
            license_token_key,
        })
    }

    /// The owner connection: `MIGRATE_DATABASE_URL`, else `DATABASE_URL`.
    pub fn owner_database_url(&self) -> &str {
        self.migrate_database_url
            .as_deref()
            .unwrap_or(&self.database_url)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    fn base(k: &str) -> Option<String> {
        match k {
            "DATABASE_URL" => Some("postgres://u@h/configurator"),
            "KEYCLOAK_ISSUER" => Some("https://issuer.test/realms/portal"),
            "KEYCLOAK_JWKS_URL" => Some("https://issuer.test/certs"),
            _ => None,
        }
        .map(String::from)
    }

    #[test]
    fn defaults_apply() {
        let c = Config::from_lookup(base).unwrap();
        assert_eq!(c.port, 8097);
        assert_eq!(c.keycloak_audience, "strata-configurator");
        assert_eq!(c.super_admin_role, "super_admin");
        assert_eq!(c.migrate_database_url, None);
        assert_eq!(c.owner_database_url(), "postgres://u@h/configurator");
    }

    #[test]
    fn migrate_database_url_is_the_owner_connection() {
        let c = Config::from_lookup(|k| {
            if k == "MIGRATE_DATABASE_URL" {
                Some("postgres://owner@h/configurator".into())
            } else {
                base(k)
            }
        })
        .unwrap();
        assert_eq!(c.database_url, "postgres://u@h/configurator");
        assert_eq!(c.owner_database_url(), "postgres://owner@h/configurator");
        // blank means unset
        let c = Config::from_lookup(|k| {
            if k == "MIGRATE_DATABASE_URL" {
                Some("  ".into())
            } else {
                base(k)
            }
        })
        .unwrap();
        assert_eq!(c.owner_database_url(), "postgres://u@h/configurator");
    }

    #[test]
    fn database_url_is_required() {
        let err = Config::from_lookup(|k| if k == "DATABASE_URL" { None } else { base(k) })
            .unwrap_err()
            .to_string();
        assert!(err.contains("DATABASE_URL"), "{err}");
    }

    #[test]
    fn blank_required_settings_rejected_and_debug_redacts_credentials() {
        for key in ["DATABASE_URL", "KEYCLOAK_ISSUER", "KEYCLOAK_JWKS_URL"] {
            assert!(
                Config::from_lookup(|k| if k == key { Some("  ".into()) } else { base(k) })
                    .is_err()
            );
        }
        let c = Config::from_lookup(|k| match k {
            "DATABASE_URL" => Some("postgres://user:private-password@db/app".into()),
            "MIGRATE_DATABASE_URL" => Some("postgres://owner:owner-secret@db/app".into()),
            _ => base(k),
        })
        .unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("private-password"), "{shown}");
        assert!(!shown.contains("owner-secret"), "{shown}");
    }

    /// `base` plus `extra`.
    fn with(extra: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k: &str| {
            extra
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
                .or_else(|| base(k))
        }
    }

    const KEYCLOAK_ADMIN: [(&str, &str); 4] = [
        (
            "KEYCLOAK_ADMIN_TOKEN_URL",
            "https://auth.test/realms/portal/protocol/openid-connect/token",
        ),
        ("KEYCLOAK_ADMIN_CLIENT_ID", "strata-configurator-admin"),
        ("KEYCLOAK_ADMIN_CLIENT_SECRET", "kc-admin-secret"),
        (
            "KEYCLOAK_ADMIN_API_URL",
            "https://auth.test/admin/realms/portal",
        ),
    ];
    /// A valid `LICENSE_TOKEN_ENC_KEY` (base64 of 32 bytes).
    const ENC_KEY: (&str, &str) = (
        "LICENSE_TOKEN_ENC_KEY",
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
    );
    const LICENSES_ADMIN: [(&str, &str); 5] = [
        (
            "LICENSES_ADMIN_TOKEN_URL",
            "https://auth.test/realms/secure-apps/protocol/openid-connect/token",
        ),
        (
            "LICENSES_ADMIN_CLIENT_ID",
            "strata-configurator-licenses-admin",
        ),
        ("LICENSES_ADMIN_CLIENT_SECRET", "licences-admin-secret"),
        ("LICENSES_ADMIN_URL", "http://licenses.test:8544"),
        ENC_KEY,
    ];

    const BOTH: [(&str, &str); 9] = [
        KEYCLOAK_ADMIN[0],
        KEYCLOAK_ADMIN[1],
        KEYCLOAK_ADMIN[2],
        KEYCLOAK_ADMIN[3],
        LICENSES_ADMIN[0],
        LICENSES_ADMIN[1],
        LICENSES_ADMIN[2],
        LICENSES_ADMIN[3],
        LICENSES_ADMIN[4],
    ];

    #[test]
    fn invites_licence_gate_and_issuance_are_dark_by_default() {
        let c = Config::from_lookup(base).unwrap();
        assert!(c.keycloak_admin.is_none());
        assert!(c.licenses_admin.is_none());
        assert!(!c.license_gate.enabled);
        assert_eq!(
            c.license_gate.introspect_url,
            "http://licenses.licenses.svc.cluster.local:8544/license/introspect"
        );
        assert_eq!(c.license_gate.app_key, "strata-configurator");
        assert_eq!(c.license_gate.cache_ttl_sec, 300);
    }

    #[test]
    fn service_accounts_need_every_setting_and_debug_redacts_their_secrets() {
        let c = Config::from_lookup(with(&KEYCLOAK_ADMIN)).unwrap();
        let k = c.keycloak_admin.as_ref().unwrap();
        assert_eq!(k.client_id, "strata-configurator-admin");
        assert_eq!(k.api_url, "https://auth.test/admin/realms/portal");
        let c = Config::from_lookup(with(&LICENSES_ADMIN)).unwrap();
        let l = c.licenses_admin.as_ref().unwrap();
        assert_eq!(l.url, "http://licenses.test:8544");
        assert_eq!(l.plan_code, "standard");
        let shown = format!("{:?}", Config::from_lookup(with(&BOTH)).unwrap());
        assert!(shown.contains("strata-configurator-admin"), "{shown}");
        assert!(!shown.contains("kc-admin-secret"), "{shown}");
        assert!(!shown.contains("licences-admin-secret"), "{shown}");

        // half a service account is an error naming what is missing, not "off"
        for group in [&KEYCLOAK_ADMIN[..], &LICENSES_ADMIN[..]] {
            let (missing, _) = group[2];
            let partial: Vec<(&str, &str)> = group
                .iter()
                .copied()
                .filter(|(k, _)| *k != missing)
                .collect();
            let err = Config::from_lookup(|k| {
                partial
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
                    .or_else(|| base(k))
            })
            .unwrap_err()
            .to_string();
            assert!(err.contains(missing), "{err}");
        }
    }

    #[test]
    fn licence_gate_settings_parse_strictly() {
        let c = Config::from_lookup(with(&[
            ("LICENSE_GATE_2_ENABLED", "true"),
            (
                "LICENSE_INTROSPECT_URL",
                "http://lic.test/license/introspect",
            ),
            ("LICENSE_APP_KEY", "other-app"),
            ("LICENSE_CACHE_TTL_SEC", "5"),
            ENC_KEY,
        ]))
        .unwrap();
        assert!(c.license_gate.enabled);
        assert_eq!(
            c.license_gate.introspect_url,
            "http://lic.test/license/introspect"
        );
        assert_eq!(c.license_gate.app_key, "other-app");
        assert_eq!(c.license_gate.cache_ttl_sec, 5);
        assert!(
            !Config::from_lookup(with(&[("LICENSE_GATE_2_ENABLED", "0")]))
                .unwrap()
                .license_gate
                .enabled
        );
        for (key, bad) in [
            ("LICENSE_GATE_2_ENABLED", "yes"),
            ("LICENSE_CACHE_TTL_SEC", "five"),
        ] {
            let err = Config::from_lookup(|k| if k == key { Some(bad.into()) } else { base(k) })
                .unwrap_err()
                .to_string();
            assert!(err.contains(key), "{err}");
        }
    }
    #[test]
    fn the_token_key_is_required_with_issuance_or_gate_2_and_must_be_valid() {
        // all dark: no key needed
        let c = Config::from_lookup(base).unwrap();
        assert!(c.license_token_key.is_none());
        // issuance, or gate 2, without the key is a startup error naming it
        let issuance: Vec<(&str, &str)> = LICENSES_ADMIN[..4].to_vec();
        let err = Config::from_lookup(|k| {
            issuance
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
                .or_else(|| base(k))
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("LICENSE_TOKEN_ENC_KEY"), "{err}");
        let err = Config::from_lookup(with(&[("LICENSE_GATE_2_ENABLED", "true")]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("LICENSE_TOKEN_ENC_KEY"), "{err}");
        // with it, both start, and the key never prints
        let c = Config::from_lookup(with(&LICENSES_ADMIN)).unwrap();
        assert!(c.license_token_key.is_some());
        let shown = format!("{c:?}");
        assert!(!shown.contains(ENC_KEY.1), "{shown}");
        // a key that is set must be valid, even with everything else off
        for bad in ["not-base64!", "AAECAwQFBgcICQoLDA0ODw=="] {
            let err = Config::from_lookup(|k| {
                if k == "LICENSE_TOKEN_ENC_KEY" {
                    Some(bad.into())
                } else {
                    base(k)
                }
            })
            .unwrap_err()
            .to_string();
            assert!(err.contains("LICENSE_TOKEN_ENC_KEY"), "{err}");
            assert!(!err.contains(bad), "the value is not echoed: {err}");
        }
    }

    #[test]
    fn early_access_is_dark_by_default_and_parses_strictly() {
        let c = Config::from_lookup(base).unwrap();
        assert!(!c.signup.enabled);
        assert_eq!(c.signup.intake_azp, ["configurator-signup-intake"]);
        assert_eq!(c.signup.intake_role, "configurator-signup-intake");
        assert_eq!(c.signup.max_pending, 500);
        assert!(c.signup.turnstile_secret.is_none());
        assert!(c.signup.telegram.is_none());

        let c = Config::from_lookup(with(&[
            ("EARLY_ACCESS_SIGNUP_ENABLED", "true"),
            ("SIGNUP_INTAKE_AZP", "site, chat ,"),
            ("TURNSTILE_SECRET", "ts-secret"),
            ("TELEGRAM_BOT_TOKEN", "123:bot-secret"),
            ("TELEGRAM_SIGNUP_CHAT_ID", "-100"),
            ("TELEGRAM_SIGNUP_THREAD_ID", "7"),
        ]))
        .unwrap();
        assert!(c.signup.enabled);
        assert_eq!(c.signup.intake_azp, ["site", "chat"]);
        let t = c.signup.telegram.as_ref().unwrap();
        assert_eq!(t.api_base, "https://api.telegram.org");
        let shown = format!("{c:?}");
        assert!(!shown.contains("ts-secret"), "{shown}");
        assert!(!shown.contains("bot-secret"), "{shown}");

        // two of the three Telegram settings: off, as in commonpractice
        let c = Config::from_lookup(with(&[
            ("TELEGRAM_BOT_TOKEN", "123:bot-secret"),
            ("TELEGRAM_SIGNUP_CHAT_ID", "-100"),
        ]))
        .unwrap();
        assert!(c.signup.telegram.is_none());

        for (key, bad) in [
            ("EARLY_ACCESS_SIGNUP_ENABLED", "yes"),
            ("SIGNUP_QUEUE_MAX_PENDING", "0"),
            ("SIGNUP_QUEUE_MAX_PENDING", "many"),
        ] {
            let err = Config::from_lookup(|k| if k == key { Some(bad.into()) } else { base(k) })
                .unwrap_err()
                .to_string();
            assert!(err.contains(key), "{err}");
        }
    }
}
