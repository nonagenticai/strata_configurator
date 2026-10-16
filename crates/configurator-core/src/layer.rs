//! Layers and scopes (spec section 4.1).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The scope of a layer, ordered from the lowest (base) to the highest (tenant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Base,
    Profile,
    Industry,
    IndustryDept,
    Job,
    Tenant,
}

impl Scope {
    /// Position of the scope in the compile order (base = 0 .. tenant = 5).
    pub fn rank(&self) -> u8 {
        match self {
            Scope::Base => 0,
            Scope::Profile => 1,
            Scope::Industry => 2,
            Scope::IndustryDept => 3,
            Scope::Job => 4,
            Scope::Tenant => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayerError {
    #[error("scope {scope:?} requires key `{key}`")]
    MissingKey { scope: Scope, key: &'static str },
    #[error("scope {scope:?} does not allow key `{key}`")]
    ExtraKey { scope: Scope, key: &'static str },
    #[error("key `{key}` must not be empty")]
    EmptyValue { key: &'static str },
}

/// The layer key every authored object carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerKey {
    pub scope: Scope,
    pub industry: Option<String>,
    pub department: Option<String>,
    pub job: Option<String>,
    pub rule: Option<String>,
    /// `(key, value)` of a `key=value` dimension, e.g. `("channel", "voice")`.
    pub dimension: Option<(String, String)>,
    pub tenant: Option<Uuid>,
}

impl LayerKey {
    /// Enforce the required-keys table of spec section 4.1 exactly: required keys
    /// must be present, optional keys may be, any other key is an error.
    pub fn validate(&self) -> Result<(), LayerError> {
        let (required, optional): (&[&str], &[&str]) = match self.scope {
            Scope::Base => (&[], &[]),
            Scope::Profile => (&["rule"], &[]),
            Scope::Industry => (&["industry"], &[]),
            Scope::IndustryDept => (&["industry", "department"], &[]),
            Scope::Job => (&["job"], &["dimension"]),
            Scope::Tenant => (&["tenant"], &[]),
        };
        let empty = |s: &Option<String>| s.as_deref().is_some_and(str::is_empty);
        let present: [(&'static str, bool, bool); 6] = [
            ("industry", self.industry.is_some(), empty(&self.industry)),
            (
                "department",
                self.department.is_some(),
                empty(&self.department),
            ),
            ("job", self.job.is_some(), empty(&self.job)),
            ("rule", self.rule.is_some(), empty(&self.rule)),
            (
                "dimension",
                self.dimension.is_some(),
                self.dimension
                    .as_ref()
                    .is_some_and(|(k, v)| k.is_empty() || v.is_empty()),
            ),
            ("tenant", self.tenant.is_some(), false),
        ];
        for (key, is_present, is_empty) in present {
            let is_required = required.contains(&key);
            if is_required && !is_present {
                return Err(LayerError::MissingKey {
                    scope: self.scope,
                    key,
                });
            }
            if is_present && !is_required && !optional.contains(&key) {
                return Err(LayerError::ExtraKey {
                    scope: self.scope,
                    key,
                });
            }
            if is_empty {
                return Err(LayerError::EmptyValue { key });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scope: Scope) -> LayerKey {
        LayerKey {
            scope,
            industry: None,
            department: None,
            job: None,
            rule: None,
            dimension: None,
            tenant: None,
        }
    }

    #[test]
    fn layer_tenant_requires_tenant_only() {
        assert!(key(Scope::Tenant).validate().is_err());
        let mut ok = key(Scope::Tenant);
        ok.tenant = Some(Uuid::nil());
        assert_eq!(ok.validate(), Ok(()));
        let mut extra = ok.clone();
        extra.industry = Some("retail".into());
        assert!(extra.validate().is_err());
    }

    #[test]
    fn layer_industry_dept_requires_both() {
        let mut k = key(Scope::IndustryDept);
        assert!(k.validate().is_err());
        k.industry = Some("retail".into());
        assert_eq!(
            k.validate(),
            Err(LayerError::MissingKey {
                scope: Scope::IndustryDept,
                key: "department"
            })
        );
        k.department = Some("faq".into());
        assert_eq!(k.validate(), Ok(()));
    }

    #[test]
    fn layer_extra_key_rejected() {
        let mut k = key(Scope::Base);
        assert_eq!(k.validate(), Ok(()));
        k.industry = Some("retail".into());
        assert_eq!(
            k.validate(),
            Err(LayerError::ExtraKey {
                scope: Scope::Base,
                key: "industry"
            })
        );
    }

    #[test]
    fn scope_rank_is_compile_order() {
        let order = [
            Scope::Base,
            Scope::Profile,
            Scope::Industry,
            Scope::IndustryDept,
            Scope::Job,
            Scope::Tenant,
        ];
        for (i, s) in order.iter().enumerate() {
            assert_eq!(usize::from(s.rank()), i);
        }
    }

    #[test]
    fn job_allows_optional_dimension() {
        let mut k = key(Scope::Job);
        k.job = Some("customer_care_agent".into());
        k.dimension = Some(("channel".into(), "voice".into()));
        assert_eq!(k.validate(), Ok(()));
        k.rule = Some("r".into());
        assert!(k.validate().is_err());
    }
}
