use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub keycloak_issuer: String,
    pub keycloak_audience: String,
    pub keycloak_jwks_url: String,
    pub super_admin_role: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// `from_env` over any lookup, so tests never touch the process environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let req = |k: &str| get(k).with_context(|| format!("{k} is not set"));
        let opt = |k: &str, d: &str| get(k).filter(|v| !v.is_empty()).unwrap_or_else(|| d.into());
        let port = match get("PORT").filter(|v| !v.is_empty()) {
            Some(p) => p.parse().with_context(|| "PORT is not a valid port")?,
            None => 8097,
        };
        Ok(Self {
            database_url: req("DATABASE_URL")?,
            port,
            keycloak_issuer: req("KEYCLOAK_ISSUER")?,
            keycloak_audience: opt("KEYCLOAK_AUDIENCE", "strata-configurator"),
            keycloak_jwks_url: req("KEYCLOAK_JWKS_URL")?,
            super_admin_role: opt("SUPER_ADMIN_ROLE", "config-super-admin"),
        })
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
        assert_eq!(c.super_admin_role, "config-super-admin");
    }

    #[test]
    fn database_url_is_required() {
        let err = Config::from_lookup(|k| if k == "DATABASE_URL" { None } else { base(k) })
            .unwrap_err()
            .to_string();
        assert!(err.contains("DATABASE_URL"), "{err}");
    }
}
